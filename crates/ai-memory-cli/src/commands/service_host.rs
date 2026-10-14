//! `ai-memory service-host`: the background host behind the Windows logon
//! entry `ai-memory setup` installs.
//!
//! Windows has no per-user service manager like systemd `--user` or launchd.
//! A real Windows service needs an administrator to install it and, because
//! the data dir lives in the user's profile, the account password to run it
//! (docs/windows.md, Scenario E). A Scheduled Task runs its action inside a
//! job object that kills the server's process tree when the action exits, and
//! shows a console window while it runs. So `setup` writes an `ai-memory`
//! value under the per-user `HKCU\...\CurrentVersion\Run` key instead, which
//! needs no elevation, and that value runs this command at every logon:
//!
//! 1. Without `--supervise`, `service-host` starts itself again detached from
//!    any console (`DETACHED_PROCESS | CREATE_NO_WINDOW`, breaking away from a
//!    job when allowed) and exits, so the logon console closes at once.
//! 2. With `--supervise`, it runs `serve --transport http --enable-web` on the
//!    same data dir and config with a hidden console (`CREATE_NO_WINDOW`), so
//!    neither the server nor a process it starts (the Git CLI fallback, a
//!    provider CLI) opens a window. It waits on the server and restarts it
//!    after a failure, the way `Restart=on-failure` and launchd's `KeepAlive`
//!    do, until the server exits cleanly or keeps failing within seconds of
//!    starting (another server on the port, a broken config).
//!
//! The supervisor records its own pid and the server's in
//! `<data_dir>/service-host.json`, which `setup` reads to tell whether the
//! host runs and `uninstall` reads to stop it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};

use super::user_service::CommandRunner;
use crate::cli::ServiceHostArgs;
use crate::config::Config;

/// The host's state file name, under the data dir.
const STATE_FILE: &str = "service-host.json";

/// Wait before restarting a server that failed.
const RESTART_DELAY: Duration = Duration::from_secs(5);
/// A server that fails sooner than this after starting counts as a quick
/// failure; [`MAX_QUICK_FAILURES`] in a row stop the host.
const QUICK_FAILURE: Duration = Duration::from_secs(10);
const MAX_QUICK_FAILURES: u32 = 5;

/// What the running host recorded about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct HostState {
    pub(crate) supervisor_pid: u32,
    pub(crate) server_pid: Option<u32>,
}

/// Where the host keeps its state for this data dir.
pub(crate) fn state_file(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILE)
}

fn read_state(path: &Path) -> Option<HostState> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_state(path: &Path, state: HostState) -> Result<()> {
    let text = serde_json::to_string(&state)?;
    ai_memory_wiki::write_atomic(path, text.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Whether `pid` is a live ai-memory process. A recorded pid the system has
/// since handed to another program does not count.
fn is_live_ai_memory(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new(),
    );
    system.process(pid).is_some_and(|process| {
        process
            .name()
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with("ai-memory")
    })
}

/// The recorded host when its supervisor is still alive.
pub(crate) fn running(state_file: &Path) -> Option<HostState> {
    read_state(state_file).filter(|state| is_live_ai_memory(state.supervisor_pid))
}

/// Every live pid the host recorded, supervisor first; `uninstall
/// --purge-data` leaves them out of its live-process check because it is
/// about to stop them.
pub(crate) fn live_pids(state_file: &Path) -> Vec<u32> {
    read_state(state_file)
        .map(|state| {
            std::iter::once(state.supervisor_pid)
                .chain(state.server_pid)
                .filter(|pid| is_live_ai_memory(*pid))
                .collect()
        })
        .unwrap_or_default()
}

/// Stop a running host: the supervisor's process tree, then a server left
/// behind by a supervisor that already died. A host that is not running is
/// no error. The state file goes with it.
///
/// # Errors
/// When `taskkill` could not be run, or refused a process that is still
/// alive afterwards.
pub(crate) fn stop(state_file: &Path, runner: &dyn CommandRunner) -> Result<()> {
    for pid in live_pids(state_file) {
        let pid_text = pid.to_string();
        let outcome = runner.run("taskkill", &["/PID", &pid_text, "/T", "/F"])?;
        if !outcome.success && is_live_ai_memory(pid) {
            anyhow::bail!(
                "`taskkill /PID {pid} /T /F` failed: {}",
                outcome.stderr.trim()
            );
        }
    }
    match std::fs::remove_file(state_file) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("removing {}", state_file.display()))
        }
        _ => Ok(()),
    }
}

/// What the supervisor does after the server exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    Stop,
    RestartAfter(Duration),
}

/// Decide after one server run. A clean exit is deliberate and ends the host;
/// a failure restarts it, unless the server has now failed quickly
/// [`MAX_QUICK_FAILURES`] times in a row, which a restart would not fix.
fn after_exit(succeeded: bool, ran: Duration, quick_failures: &mut u32) -> Next {
    if succeeded {
        return Next::Stop;
    }
    if ran < QUICK_FAILURE {
        *quick_failures += 1;
    } else {
        *quick_failures = 1;
    }
    if *quick_failures >= MAX_QUICK_FAILURES {
        Next::Stop
    } else {
        Next::RestartAfter(RESTART_DELAY)
    }
}

/// Run the server again and again per [`after_exit`]. `run_server` starts one
/// server, reports its pid through `started`, waits for it, and returns
/// whether it exited successfully. Returns the last outcome.
fn supervise(
    mut run_server: impl FnMut(&mut dyn FnMut(u32)) -> Result<bool>,
    mut started: impl FnMut(u32),
    mut sleep: impl FnMut(Duration),
    clock: impl Fn() -> Instant,
) -> Result<bool> {
    let mut quick_failures = 0;
    loop {
        let began = clock();
        let succeeded = run_server(&mut started)?;
        match after_exit(succeeded, clock() - began, &mut quick_failures) {
            Next::Stop => return Ok(succeeded),
            Next::RestartAfter(delay) => sleep(delay),
        }
    }
}

/// The global arguments that point a child at this install.
pub(crate) fn install_args(config: &Config, config_path: Option<&Path>) -> Vec<std::ffi::OsString> {
    let mut args = vec![
        "--data-dir".into(),
        config.data_dir.clone().into_os_string(),
    ];
    if let Some(path) = config_path {
        args.push("--config".into());
        args.push(path.as_os_str().to_owned());
    }
    args
}

/// Run `service-host`.
///
/// # Errors
/// On a platform other than Windows, or when the host cannot start itself or
/// the server.
pub async fn run(config: &Config, args: ServiceHostArgs, config_path: Option<&Path>) -> Result<()> {
    if !cfg!(windows) {
        anyhow::bail!(
            "`service-host` is the Windows logon entry's launcher; on this platform `ai-memory \
             setup` installs a systemd user unit or a LaunchAgent instead"
        );
    }
    let exe = std::env::current_exe().context("locating the running ai-memory binary")?;
    let install = install_args(config, config_path);
    if !args.supervise {
        let mut host_args = install.clone();
        host_args.extend(["service-host".into(), "--supervise".into()]);
        return windows::spawn_detached(&exe, &host_args)
            .with_context(|| format!("starting {} service-host --supervise", exe.display()));
    }

    let state_path = state_file(&config.data_dir);
    let own_pid = std::process::id();
    if running(&state_path).is_some_and(|state| state.supervisor_pid != own_pid) {
        // A second logon, or `setup` racing the Run entry: one host serves.
        return Ok(());
    }
    write_state(
        &state_path,
        HostState {
            supervisor_pid: own_pid,
            server_pid: None,
        },
    )?;
    let mut server_args = install;
    server_args.extend(["serve", "--transport", "http", "--enable-web"].map(Into::into));
    let outcome = tokio::task::spawn_blocking(move || {
        supervise(
            |started| windows::run_hidden(&exe, &server_args, started),
            |pid| {
                let state = HostState {
                    supervisor_pid: own_pid,
                    server_pid: Some(pid),
                };
                if let Err(error) = write_state(&state_path, state) {
                    tracing::warn!(error = %format!("{error:#}"), "service-host: recording the server pid");
                }
            },
            std::thread::sleep,
            Instant::now,
        )
        .map(|succeeded| (succeeded, state_path))
    })
    .await
    .context("the service-host supervisor stopped unexpectedly")?;
    let (succeeded, state_path) = outcome?;
    std::fs::remove_file(&state_path).ok();
    if !succeeded {
        tracing::error!(
            "service-host: the server failed {MAX_QUICK_FAILURES} times within {}s of starting; \
             not restarting it. Run `ai-memory serve --transport http` in a terminal to see why.",
            QUICK_FAILURE.as_secs()
        );
    }
    Ok(())
}

#[cfg(windows)]
mod windows {
    use std::ffi::OsString;
    use std::os::windows::process::CommandExt as _;
    use std::path::Path;
    use std::process::{Command, Stdio};

    use anyhow::{Context as _, Result};

    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const ERROR_ACCESS_DENIED: i32 = 5;

    fn command(exe: &Path, args: &[OsString], flags: u32) -> Command {
        let mut command = Command::new(exe);
        command
            .args(args)
            .creation_flags(flags)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    /// Start the supervisor with no console, out of whatever job started the
    /// logon entry when the job allows it, and do not wait for it.
    pub(super) fn spawn_detached(exe: &Path, args: &[OsString]) -> Result<()> {
        let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
        match command(exe, args, flags | CREATE_BREAKAWAY_FROM_JOB).spawn() {
            Ok(_) => Ok(()),
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                command(exe, args, flags)
                    .spawn()
                    .map(|_| ())
                    .map_err(Into::into)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Run the server on a hidden console of its own, which the processes it
    /// starts inherit instead of opening windows, and wait for it.
    pub(super) fn run_hidden(
        exe: &Path,
        args: &[OsString],
        started: &mut dyn FnMut(u32),
    ) -> Result<bool> {
        let mut child = command(exe, args, CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
            .spawn()
            .with_context(|| format!("starting {} serve", exe.display()))?;
        started(child.id());
        Ok(child.wait().context("waiting for the server")?.success())
    }
}

#[cfg(not(windows))]
mod windows {
    use std::ffi::OsString;
    use std::path::Path;

    use anyhow::{Result, bail};

    pub(super) fn spawn_detached(_exe: &Path, _args: &[OsString]) -> Result<()> {
        bail!("service-host runs only on Windows")
    }

    pub(super) fn run_hidden(
        _exe: &Path,
        _args: &[OsString],
        _started: &mut dyn FnMut(u32),
    ) -> Result<bool> {
        bail!("service-host runs only on Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// A clean exit stops the host; failures restart it after the delay; a
    /// server that keeps failing quickly stops it on the fifth, and one that
    /// ran a while before failing starts the count over.
    #[test]
    fn restart_policy_follows_exit_status_and_how_long_it_ran() {
        let mut quick = 0;
        assert_eq!(after_exit(true, Duration::ZERO, &mut quick), Next::Stop);
        for _ in 1..MAX_QUICK_FAILURES {
            assert_eq!(
                after_exit(false, Duration::from_secs(1), &mut quick),
                Next::RestartAfter(RESTART_DELAY)
            );
        }
        assert_eq!(
            after_exit(false, Duration::from_secs(1), &mut quick),
            Next::Stop
        );

        let mut quick = MAX_QUICK_FAILURES - 1;
        assert_eq!(
            after_exit(false, Duration::from_secs(3600), &mut quick),
            Next::RestartAfter(RESTART_DELAY)
        );
        assert_eq!(quick, 1);
    }

    /// The loop restarts a failing server, reports each pid, sleeps between
    /// runs, and stops once the server exits cleanly.
    #[test]
    fn supervise_restarts_until_a_clean_exit() {
        let outcomes = RefCell::new(vec![false, false, true].into_iter());
        let pids = RefCell::new(Vec::new());
        let sleeps = Cell::new(0);
        let next_pid = Cell::new(100);
        let succeeded = supervise(
            |started| {
                next_pid.set(next_pid.get() + 1);
                started(next_pid.get());
                Ok(outcomes.borrow_mut().next().unwrap())
            },
            |pid| pids.borrow_mut().push(pid),
            |delay| {
                assert_eq!(delay, RESTART_DELAY);
                sleeps.set(sleeps.get() + 1);
            },
            Instant::now,
        )
        .unwrap();
        assert!(succeeded);
        assert_eq!(*pids.borrow(), vec![101, 102, 103]);
        assert_eq!(sleeps.get(), 2);
    }

    /// A server that never stays up stops the loop instead of spinning.
    #[test]
    fn supervise_gives_up_on_a_server_that_keeps_failing() {
        let runs = Cell::new(0);
        let succeeded = supervise(
            |_| {
                runs.set(runs.get() + 1);
                Ok(false)
            },
            |_| {},
            |_| {},
            Instant::now,
        )
        .unwrap();
        assert!(!succeeded);
        assert_eq!(runs.get(), MAX_QUICK_FAILURES);
    }

    /// A state file naming no live ai-memory process means no host runs, and
    /// stopping it then runs nothing and still removes the stale file.
    #[test]
    fn a_stale_state_file_is_not_a_running_host() {
        struct NoCalls;
        impl CommandRunner for NoCalls {
            fn run(
                &self,
                program: &str,
                _args: &[&str],
            ) -> Result<super::super::user_service::CommandOutcome> {
                panic!("{program} must not run for a dead host");
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let path = state_file(tmp.path());
        assert_eq!(running(&path), None);
        write_state(
            &path,
            HostState {
                supervisor_pid: u32::MAX - 1,
                server_pid: Some(u32::MAX - 2),
            },
        )
        .unwrap();
        assert_eq!(running(&path), None);
        assert!(live_pids(&path).is_empty());
        stop(&path, &NoCalls).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn children_are_pointed_at_the_same_install() {
        let config = Config {
            data_dir: PathBuf::from("/data"),
            ..Config::default()
        };
        assert_eq!(
            install_args(&config, Some(Path::new("/data/config.toml"))),
            ["--data-dir", "/data", "--config", "/data/config.toml"]
                .map(std::ffi::OsString::from)
                .to_vec()
        );
        assert_eq!(
            install_args(&config, None),
            ["--data-dir", "/data"]
                .map(std::ffi::OsString::from)
                .to_vec()
        );
    }

    #[test]
    fn service_host_refuses_off_windows() {
        if cfg!(windows) {
            return;
        }
        let config = Config::default();
        let error = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(run(&config, ServiceHostArgs { supervise: false }, None))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Windows logon entry"), "{error}");
    }
}
