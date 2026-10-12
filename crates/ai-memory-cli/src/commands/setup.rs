//! `ai-memory setup` — a native install in one command.
//!
//! A composition of the commands a native install otherwise runs by hand, in
//! order, through their own library functions:
//!
//! 1. `init`, which on a fresh data dir also mints the root bearer token into
//!    `<data_dir>/auth-tokens/default` and records `[auth] bearer_token_file`;
//! 2. a per-user service running `serve` on that install
//!    ([`super::user_service`]): a systemd `--user` unit or a launchd
//!    LaunchAgent, rendered from the packaged templates. Windows has none;
//! 3. for each agent (named with `--agent`, else detected from its session
//!    store or MCP config under the home, the way `doctor` finds them):
//!    `install-mcp --apply`, `install-hooks --apply` and global
//!    `install-skills`;
//! 4. a self-check that writes nothing: `/healthz`, a `memory_status` call
//!    over MCP with the install's credential, and `doctor`.
//!
//! The token is never printed or passed on a command line. The installers keep
//! it out of every harness config (hooks read the credential files `init`
//! seeds; MCP clients go through `mcp-bridge` or an env-var reference), as
//! they do when run by hand.
//!
//! Every step is idempotent, so a second `setup` changes nothing. Without
//! `--yes` the plan is shown first and confirmed on a terminal; with no
//! terminal to ask on, `setup` changes nothing and exits non-zero.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ai_memory_workstream::{ManagedHarness, OpenCodeDialect, native_store_root};
use anyhow::{Context as _, Result, bail};

use super::user_service::{self, CommandRunner, ServiceChange, ServiceManager, ServiceUnit};
use super::{doctor, init, install_hooks, install_mcp, install_skills, run_autowire};
use crate::cli::{
    AgentChoice, DoctorArgs, HookInstallScope, InitArgs, InstallHooksArgs, InstallMcpArgs,
    InstallSkillsAgent, InstallSkillsArgs, InstallSkillsScope, McpClient, SetupArgs,
};
use crate::config::Config;
use crate::http_client::{ServerEndpoint, ServerProbe, probe_server};

/// How long `setup` waits for a service it just started to answer.
const SERVICE_READY_TIMEOUT: Duration = Duration::from_secs(30);

/// The MCP entry name every installer writes by default.
const ENTRY_NAME: &str = "ai-memory";

/// Harnesses that share a native store with another one, so finding the store
/// cannot tell them apart: OpenCode 2 shares OpenCode's, and the Kiro CLI v3
/// engine's store contains the v2 engine's. Wiring both would write two
/// dialects into one config, so only the stable one is detected; the other is
/// wired when named with `--agent`.
const EXPLICIT_ONLY: &[ManagedHarness] = &[ManagedHarness::OpenCode2, ManagedHarness::KiroV3];

/// What `setup` will do.
#[derive(Debug)]
struct SetupPlan {
    data_dir: PathBuf,
    config_path: PathBuf,
    /// No config file yet: `init` writes one (and mints the token when the
    /// data dir has no database either).
    fresh: bool,
    service: ServicePlan,
    agents: Vec<AgentPlan>,
    /// Agents came from detection, not `--agent`.
    detected: bool,
}

#[derive(Debug)]
enum ServicePlan {
    Install(ServiceUnit),
    Skip(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentPlan {
    agent: AgentChoice,
    mcp: Option<McpClient>,
    skills: InstallSkillsAgent,
}

impl AgentPlan {
    fn new(agent: AgentChoice) -> Self {
        Self {
            agent,
            mcp: install_hooks::mcp_client_for_agent(agent),
            skills: skills_for(agent),
        }
    }
}

/// The skill directory family an agent reads; agents without one of their
/// own read the cross-client `~/.agents/skills`.
fn skills_for(agent: AgentChoice) -> InstallSkillsAgent {
    match agent {
        AgentChoice::ClaudeCode => InstallSkillsAgent::ClaudeCode,
        AgentChoice::Grok => InstallSkillsAgent::Grok,
        AgentChoice::CopilotCli => InstallSkillsAgent::CopilotCli,
        AgentChoice::Devin => InstallSkillsAgent::Devin,
        AgentChoice::Hermes => InstallSkillsAgent::Hermes,
        _ => InstallSkillsAgent::Agents,
    }
}

/// The OpenCode major version an agent names outright, so neither installer
/// probes `opencode --version` and picks one dialect for both.
fn opencode_dialect(agent: AgentChoice) -> Option<OpenCodeDialect> {
    match agent {
        AgentChoice::OpenCode => Some(OpenCodeDialect::V1),
        AgentChoice::OpenCode2 => Some(OpenCodeDialect::V2),
        _ => None,
    }
}

/// Run `setup`.
///
/// # Errors
/// When the plan cannot be built, the operator declines it (or there is no
/// terminal to ask on and no `--yes`), or any step fails; the other steps
/// still run and the error counts the failures.
pub async fn run(config: &Config, args: SetupArgs, config_path: Option<&Path>) -> Result<()> {
    let captured = config.runtime_env.process_env().to_vec();
    let env = move |name: &str| -> Option<OsString> {
        captured
            .iter()
            .find(|(key, _)| key.as_os_str() == name)
            .map(|(_, value)| value.clone())
    };
    let home = super::run::native_home(config).context("locating your home directory")?;
    let exe = std::env::current_exe().context("locating the running ai-memory binary")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);

    let written_config = init_config_path(config, config_path);
    let agents = if args.agents.is_empty() {
        detect_agents(
            &home,
            &|harness| doctor::relocated_session_dir_with(harness, &env),
            &|client| install_mcp::mcp_config_path_with(client, &env).ok(),
        )
    } else {
        dedup(args.agents.iter().copied())
    };
    let service = if args.no_service {
        ServicePlan::Skip("skipped (--no-service)".to_owned())
    } else {
        let spec = user_service::ServiceSpec {
            exe,
            data_dir: config.data_dir.clone(),
            config_path: written_config.clone(),
            home: home.clone(),
            xdg_config_home: env("XDG_CONFIG_HOME").map(PathBuf::from),
        };
        let reachable = matches!(
            probe_server(&ServerEndpoint::from_config_resolving_auth(config).await).await,
            ServerProbe::Reachable
        );
        plan_service(
            ServiceManager::current(),
            &spec,
            &config.server_url,
            reachable,
        )
    };
    let plan = SetupPlan {
        data_dir: config.data_dir.clone(),
        fresh: !written_config.exists(),
        config_path: written_config,
        service,
        agents: agents.into_iter().map(AgentPlan::new).collect(),
        detected: args.agents.is_empty(),
    };

    print_plan(&plan);
    if !args.yes {
        confirm()?;
    }
    execute(
        config,
        config_path,
        plan,
        &user_service::SystemRunner,
        SERVICE_READY_TIMEOUT,
    )
    .await
}

fn init_config_path(config: &Config, explicit: Option<&Path>) -> PathBuf {
    explicit.map_or_else(|| config.data_dir.join("config.toml"), Path::to_path_buf)
}

fn dedup(agents: impl Iterator<Item = AgentChoice>) -> Vec<AgentChoice> {
    let mut seen = Vec::new();
    for agent in agents {
        if !seen.contains(&agent) {
            seen.push(agent);
        }
    }
    seen
}

/// The agents to wire when none was named: every harness `doctor` scans whose
/// session store exists, or whose MCP config file does, under `home`.
fn detect_agents(
    home: &Path,
    session_dir_for: &dyn Fn(ManagedHarness) -> Option<PathBuf>,
    mcp_config_for: &dyn Fn(McpClient) -> Option<PathBuf>,
) -> Vec<AgentChoice> {
    let found = doctor::SCANNED_HARNESSES
        .iter()
        .copied()
        .filter(|harness| !EXPLICIT_ONLY.contains(harness))
        .filter_map(|harness| {
            let agent = run_autowire::agent_choice_for_harness(harness)?;
            let store = native_store_root(harness, home, session_dir_for(harness).as_deref());
            let config = install_hooks::mcp_client_for_agent(agent)
                .and_then(mcp_config_for)
                .is_some_and(|path| path.is_file());
            (store.is_dir() || config).then_some(agent)
        });
    dedup(found)
}

/// Decide the service step. A server already answering with no unit of ours
/// in place is left in charge (Docker, a packaged unit, a foreground
/// `serve`): a second server could not bind the port anyway.
fn plan_service(
    manager: Option<ServiceManager>,
    spec: &user_service::ServiceSpec,
    server_url: &str,
    reachable: bool,
) -> ServicePlan {
    let Some(manager) = manager else {
        return ServicePlan::Skip(
            "not supported on this platform (Windows has no service step); start the server \
             with `ai-memory serve --transport http` and see docs/windows.md"
                .to_owned(),
        );
    };
    if !super::hook_spool::is_loopback_url(server_url) {
        return ServicePlan::Skip(format!(
            "the configured server {server_url} is not on this machine; no local service"
        ));
    }
    match user_service::render(manager, spec) {
        Ok(unit) if reachable && !unit.path.exists() => ServicePlan::Skip(format!(
            "a server already answers at {server_url}; leaving it in charge (pass --no-service \
             to skip this check)"
        )),
        Ok(unit) => ServicePlan::Install(unit),
        Err(error) => ServicePlan::Skip(format!("cannot render the service: {error:#}")),
    }
}

fn print_plan(plan: &SetupPlan) {
    println!("ai-memory setup will:");
    if plan.fresh {
        println!(
            "  - create the data dir {} and write {} (with a new root bearer token in \
             auth-tokens/default when the data dir is new)",
            plan.data_dir.display(),
            plan.config_path.display()
        );
    } else {
        println!(
            "  - keep the existing install at {} ({})",
            plan.data_dir.display(),
            plan.config_path.display()
        );
    }
    match &plan.service {
        ServicePlan::Install(unit) => println!(
            "  - install and start a {} at {}",
            unit.manager.describe(),
            unit.path.display()
        ),
        ServicePlan::Skip(reason) => println!("  - service: {reason}"),
    }
    if plan.agents.is_empty() {
        println!(
            "  - wire no agent: none {}; name one with --agent <name>",
            if plan.detected {
                "was detected"
            } else {
                "was named"
            }
        );
    }
    for agent in &plan.agents {
        let name = agent.agent.kind().as_str();
        let mcp = if agent.mcp.is_some() {
            "MCP + hooks"
        } else {
            "hooks"
        };
        println!(
            "  - wire {name}{}: {mcp} + skills",
            if plan.detected { " (detected)" } else { "" }
        );
    }
    println!("  - check /healthz, memory_status and doctor (read-only)");
}

/// Ask on a terminal; refuse without one.
fn confirm() -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "nothing was changed: there is no terminal to confirm this plan on. Re-run \
             `ai-memory setup --yes` to apply it."
        );
    }
    print!("Proceed? [y/N] ");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .context("reading the answer")?;
    if matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes") {
        Ok(())
    } else {
        bail!("nothing was changed")
    }
}

/// Apply the plan. Every step runs even when an earlier one failed, so the
/// report names every problem at once.
async fn execute(
    config: &Config,
    config_path: Option<&Path>,
    plan: SetupPlan,
    runner: &dyn CommandRunner,
    ready_timeout: Duration,
) -> Result<()> {
    let mut problems: Vec<String> = Vec::new();

    println!("\n== init");
    let outcome = init::initialize(config, &InitArgs { force: false }, config_path)?;
    let mut config = config.clone();
    if let Some(token) = outcome.root_token
        && config.auth.bearer_token.is_none()
    {
        config.auth.adopt_token_file(token.path, token.token);
    }
    println!(
        "config: {} {}",
        if outcome.config_written {
            "wrote"
        } else {
            "kept"
        },
        outcome.config_path.display()
    );

    println!("\n== service");
    let mut wait_for_server = false;
    match &plan.service {
        ServicePlan::Skip(reason) => println!("service: {reason}"),
        ServicePlan::Install(unit) => match user_service::install(unit, runner) {
            Ok(change) => {
                wait_for_server = change != ServiceChange::Unchanged;
                println!(
                    "service: {} {}",
                    match change {
                        ServiceChange::Installed => "installed and started",
                        ServiceChange::Updated => "updated and restarted",
                        ServiceChange::Started => "started",
                        ServiceChange::Unchanged => "already running from",
                    },
                    unit.path.display()
                );
            }
            Err(error) => problems.push(format!(
                "service: {error:#}. Start the server yourself with `ai-memory serve --transport \
                 http`, or re-run with --no-service when it runs some other way."
            )),
        },
    }
    let endpoint = ServerEndpoint::from_config_resolving_auth(&config).await;
    if wait_for_server && !wait_until_reachable(&endpoint, ready_timeout).await {
        problems.push(format!(
            "service: the server did not answer at {} within {}s",
            endpoint.url,
            ready_timeout.as_secs()
        ));
    }

    let mut skills_done = BTreeSet::new();
    for agent in &plan.agents {
        println!("\n== {}", agent.agent.kind().as_str());
        for problem in wire_agent(&config, *agent, &mut skills_done) {
            problems.push(format!("{}: {problem}", agent.agent.kind().as_str()));
        }
    }

    println!("\n== self-check");
    for problem in self_check(&config, &endpoint).await {
        problems.push(format!("self-check: {problem}"));
    }

    println!();
    if config.auth.bearer_token.is_none() {
        println!("auth: none (loopback); run `ai-memory auth enable` to require a bearer token");
    } else {
        println!("auth: bearer token (owner-only file; never printed)");
    }
    if problems.is_empty() {
        println!(
            "ai-memory is set up. Restart your agents so they load the new hooks and MCP entry."
        );
        return Ok(());
    }
    for problem in &problems {
        eprintln!("problem: {problem}");
    }
    bail!(
        "setup finished with {} problem(s); fix them and re-run `ai-memory setup`",
        problems.len()
    )
}

async fn wait_until_reachable(endpoint: &ServerEndpoint, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if matches!(probe_server(endpoint).await, ServerProbe::Reachable) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// `install-mcp`, `install-hooks` and `install-skills` for one agent, MCP
/// first so an OpenCode major-version move has one entry before the hook
/// installer reads it (the order `ai-memory run` auto-wire uses). Returns the
/// failures.
fn wire_agent(config: &Config, plan: AgentPlan, skills_done: &mut BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    let dialect = opencode_dialect(plan.agent);
    if let Some(client) = plan.mcp {
        if install_mcp::existing_entry_is_session_aware(client, None, ENTRY_NAME) {
            println!("mcp: keeping the existing session-aware entry");
        } else {
            let args = InstallMcpArgs {
                client,
                server_url: None,
                name: ENTRY_NAME.to_owned(),
                auth_token: None,
                apply: true,
                config_file: None,
                session_aware: false,
                flavor: None,
                token_env_var: false,
                stdio_bridge: false,
            };
            if let Err(error) = install_mcp::run_with_opencode_dialect(config, args, dialect) {
                problems.push(format!("install-mcp: {error:#}"));
            }
        }
    }
    let hooks = InstallHooksArgs {
        agent: plan.agent,
        hooks_dir: None,
        server_url: None,
        auth_token: None,
        as_user: None,
        apply: true,
        config_file: None,
        scope: HookInstallScope::Global,
        project_strategy: None,
        capture_assistant: false,
        capture_mode: None,
        no_capture_prompts: false,
        capture_prompts: false,
        profile: None,
    };
    if let Err(error) =
        install_hooks::run_with_opencode_dialect_and_mcp_path(config, hooks, dialect, None)
    {
        problems.push(format!("install-hooks: {error:#}"));
    }
    let skills_key = format!("{:?}", plan.skills);
    if skills_done.insert(skills_key) {
        let args = InstallSkillsArgs {
            scope: InstallSkillsScope::Global,
            agent: plan.skills,
            target_dir: None,
            print: false,
            force: false,
        };
        if let Err(error) = install_skills::run(config, args) {
            problems.push(format!("install-skills: {error:#}"));
        }
    }
    problems
}

/// `/healthz`, `memory_status` over MCP, and `doctor`; none of them writes a
/// file. Returns the failures.
async fn self_check(config: &Config, endpoint: &ServerEndpoint) -> Vec<String> {
    let mut problems = Vec::new();
    let healthz = endpoint.build_url("/healthz");
    match reqwest::Client::new()
        .get(&healthz)
        .timeout(Duration::from_secs(5))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => println!("healthz: ok ({healthz})"),
        Ok(response) => problems.push(format!("{healthz} answered {}", response.status())),
        Err(error) => {
            problems.push(format!("{healthz} did not answer: {error}"));
            // Nothing else can pass without a server.
            return problems;
        }
    }
    match memory_status(config).await {
        Ok(()) => println!("memory_status: ok"),
        Err(error) => problems.push(format!("memory_status: {error:#}")),
    }
    let doctor = DoctorArgs {
        workspace: None,
        project: None,
        since_days: 30,
        json: false,
    };
    if let Err(error) = doctor::run(config, doctor).await {
        problems.push(format!("doctor: {error:#}"));
    }
    problems
}

/// One `memory_status` call over the server's MCP endpoint, authenticated the
/// way `mcp-bridge` authenticates, so a refused credential shows here rather
/// than in the agent's first session.
async fn memory_status(config: &Config) -> Result<()> {
    use rmcp::ServiceExt as _;
    use rmcp::model::CallToolRequestParams;
    use rmcp::transport::StreamableHttpClientTransport;

    let url = install_mcp::mcp_server_url_from_base(&config.server_url);
    let transport = StreamableHttpClientTransport::from_config(super::mcp_bridge::upstream_config(
        &url,
        None,
        config.auth.bearer_token.as_deref(),
    )?);
    let client = ().serve(transport).await.with_context(|| format!("connecting to {url}"))?;
    let result = client
        .peer()
        .call_tool(CallToolRequestParams::new("memory_status"))
        .await;
    client.cancel().await.ok();
    let result = result.context("calling memory_status")?;
    if result.is_error == Some(true) {
        bail!("memory_status reported an error");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(home: &Path) -> user_service::ServiceSpec {
        user_service::ServiceSpec {
            exe: PathBuf::from("/opt/ai-memory/ai-memory"),
            data_dir: home.join("data"),
            config_path: home.join("data").join("config.toml"),
            home: home.to_path_buf(),
            xdg_config_home: None,
        }
    }

    #[test]
    fn detection_finds_harness_stores_and_mcp_configs_under_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // Claude Code by its session store, Codex by its MCP config only.
        std::fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        let codex_config = home.join("codex-config.toml");
        std::fs::write(&codex_config, "").unwrap();
        let mcp = |client: McpClient| (client == McpClient::Codex).then(|| codex_config.clone());
        let found = detect_agents(home, &|_| None, &mcp);
        assert_eq!(found, vec![AgentChoice::ClaudeCode, AgentChoice::Codex]);
    }

    /// Control: an empty home detects nothing, and a named-but-missing MCP
    /// config path does not count as present.
    #[test]
    fn an_empty_home_detects_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope.json");
        let found = detect_agents(tmp.path(), &|_| None, &|_| Some(missing.clone()));
        assert!(found.is_empty(), "{found:?}");
    }

    /// OpenCode 2 shares OpenCode's store; detection must not wire both
    /// dialects into the one config file.
    #[test]
    fn a_shared_store_detects_only_the_stable_harness() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join(".local/share/opencode")).unwrap();
        std::fs::create_dir_all(home.join(".kiro/sessions/cli")).unwrap();
        let found = detect_agents(home, &|_| None, &|_| None);
        assert_eq!(found, vec![AgentChoice::OpenCode, AgentChoice::KiroCli]);
    }

    /// A relocated store (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, …) is looked for
    /// where the relocation points, not under the home.
    #[test]
    fn detection_follows_a_relocated_store() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("relocated-claude");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let relocate = |harness: ManagedHarness| {
            (harness == ManagedHarness::Claude).then(|| elsewhere.clone())
        };
        let found = detect_agents(&tmp.path().join("home"), &relocate, &|_| None);
        assert_eq!(found, vec![AgentChoice::ClaudeCode]);
    }

    #[test]
    fn skills_go_to_each_agents_own_directory_or_the_shared_one() {
        assert_eq!(
            skills_for(AgentChoice::ClaudeCode),
            InstallSkillsAgent::ClaudeCode
        );
        assert_eq!(skills_for(AgentChoice::Grok), InstallSkillsAgent::Grok);
        assert_eq!(
            skills_for(AgentChoice::CopilotCli),
            InstallSkillsAgent::CopilotCli
        );
        assert_eq!(skills_for(AgentChoice::Codex), InstallSkillsAgent::Agents);
        assert_eq!(
            skills_for(AgentChoice::KimiCode),
            InstallSkillsAgent::Agents
        );
    }

    #[test]
    fn named_agents_are_deduplicated_in_order() {
        assert_eq!(
            dedup(
                [
                    AgentChoice::Codex,
                    AgentChoice::ClaudeCode,
                    AgentChoice::Codex
                ]
                .into_iter()
            ),
            vec![AgentChoice::Codex, AgentChoice::ClaudeCode]
        );
    }

    #[test]
    fn windows_has_no_service_step_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let ServicePlan::Skip(reason) =
            plan_service(None, &spec(tmp.path()), "http://127.0.0.1:49374", false)
        else {
            panic!("expected a skip");
        };
        assert!(reason.contains("Windows"), "{reason}");
    }

    #[test]
    fn a_remote_server_gets_no_local_service() {
        let tmp = tempfile::tempdir().unwrap();
        let plan = plan_service(
            Some(ServiceManager::SystemdUser),
            &spec(tmp.path()),
            "https://memory.example.com",
            true,
        );
        assert!(
            matches!(plan, ServicePlan::Skip(reason) if reason.contains("not on this machine"))
        );
    }

    /// A server that already answers with no unit of ours in place is not
    /// ours to replace; once our unit exists, a running server is the one it
    /// started and the unit is managed as usual.
    // Windows skips the service step before this decision.
    #[cfg(unix)]
    #[test]
    fn an_answering_server_without_our_unit_is_left_in_charge() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = spec(tmp.path());
        let url = "http://127.0.0.1:49374";
        let plan = plan_service(Some(ServiceManager::SystemdUser), &spec, url, true);
        assert!(matches!(plan, ServicePlan::Skip(reason) if reason.contains("already answers")));

        let ServicePlan::Install(unit) =
            plan_service(Some(ServiceManager::SystemdUser), &spec, url, false)
        else {
            panic!("expected an install when nothing answers");
        };
        std::fs::create_dir_all(unit.path.parent().unwrap()).unwrap();
        std::fs::write(&unit.path, &unit.contents).unwrap();
        assert!(matches!(
            plan_service(Some(ServiceManager::SystemdUser), &spec, url, true),
            ServicePlan::Install(_)
        ));
    }
}
