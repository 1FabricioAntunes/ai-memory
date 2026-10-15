//! Installer output is unchanged when run directly. Setup collects the same
//! messages during synchronous wiring so it can group them without redirecting
//! process-wide stdout/stderr or changing any installer result.

use std::cell::RefCell;
use std::fmt::Arguments;
use std::path::Path;

use crate::cli::{InstallMcpArgs, McpClient};

use super::apply_shared::ApplyOutcome;

pub(super) struct Message {
    pub text: String,
    pub stderr: bool,
}

thread_local! {
    static MESSAGES: RefCell<Option<Vec<Message>>> = const { RefCell::new(None) };
}

/// Synchronous and thread-local: other commands/threads keep their normal
/// output. Restoring the previous sink on drop also covers errors and panics.
pub(super) fn collect<T>(operation: impl FnOnce() -> T) -> (T, Vec<Message>) {
    struct Restore(Option<Vec<Message>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            MESSAGES.with(|slot| slot.replace(self.0.take()));
        }
    }
    let _restore = Restore(MESSAGES.with(|slot| slot.replace(Some(Vec::new()))));
    let result = operation();
    let messages = MESSAGES.with(|slot| slot.take().unwrap_or_default());
    (result, messages)
}

pub(super) fn emit(stderr: bool, args: Arguments<'_>) {
    MESSAGES.with(|slot| {
        if let Some(messages) = slot.borrow_mut().as_mut() {
            messages.push(Message {
                text: args.to_string(),
                stderr,
            });
        } else if stderr {
            std::eprint!("{args}");
        } else {
            std::print!("{args}");
        }
    });
}

macro_rules! print {
    ($($arg:tt)*) => { $crate::commands::install_output::emit(false, format_args!($($arg)*)) };
}
macro_rules! println {
    () => { $crate::commands::install_output::emit(false, format_args!("\n")) };
    ($($arg:tt)*) => { $crate::commands::install_output::emit(false, format_args!("{}\n", format_args!($($arg)*))) };
}
macro_rules! eprintln {
    ($($arg:tt)*) => { $crate::commands::install_output::emit(true, format_args!("{}\n", format_args!($($arg)*))) };
}
pub(crate) use {eprintln, print, println};

pub(super) fn mcp_applied(
    args: &InstallMcpArgs,
    path: &Path,
    outcome: ApplyOutcome,
    token_env: &str,
) {
    println!(
        "✓ {} {} ({})",
        outcome.verb(),
        path.display(),
        match outcome {
            ApplyOutcome::Created => "new file",
            ApplyOutcome::Updated => "backup written next to it",
            ApplyOutcome::NoOp => "already up to date",
        }
    );
    if matches!(args.client, McpClient::PrimeAgent)
        && (args.auth_token.is_some() || args.token_env_var)
    {
        println!(
            "  Prime Agent reads the token from ${token_env}; export it where you launch prime-agent."
        );
    }
    if matches!(args.client, McpClient::Droid) && (args.auth_token.is_some() || args.token_env_var)
    {
        println!(
            "  Droid expands ${{{token_env}}} in the Authorization header; export it where you launch droid, or the server fails to connect."
        );
    }
    if matches!(args.client, McpClient::Codex) && args.token_env_var {
        println!("  Codex reads the token from ${token_env}; export it where you launch codex.");
    }
}

pub(super) fn capture_policy_capability(supported: bool) {
    if supported {
        eprintln!(
            "[ai-memory] capture-policy capability v1 enforced by this selected integration; re-run --apply to refresh existing installs."
        );
    } else {
        eprintln!(
            "[ai-memory] selected shell/PowerShell compatibility path does not enforce capture-policy v1; use a native platform selection or generated integration."
        );
    }
}

pub(super) fn codex_trust_hint() {
    println!();
    println!("Codex requires explicit trust for new hooks. Next time you start `codex`:");
    println!("  → the TUI will surface 'Hooks need review' for each new event");
    println!("  → choose 'Trust all and continue' (or trust individually)");
    println!("To bypass the prompt for automated installs, start with");
    println!("`codex --dangerously-bypass-hook-trust` (review hook scripts first).");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_collection_preserves_channels_and_outer_messages() {
        let (_, outer) = collect(|| {
            println!("outer start");
            let (result, inner) = collect(|| {
                print!("file ");
                println!("updated");
                eprintln!("warning");
                Err::<(), _>("failed")
            });
            assert_eq!(result, Err("failed"));
            assert_eq!(inner[0].text, "file ");
            assert_eq!(inner[1].text, "updated\n");
            assert_eq!(inner[2].text, "warning\n");
            assert!(inner[2].stderr);
            println!("outer end");
        });
        assert_eq!(outer.len(), 2);
        assert_eq!(outer[1].text, "outer end\n");
        assert!(!outer[1].stderr);
        assert!(MESSAGES.with(|slot| slot.borrow().is_none()));
    }

    #[test]
    fn panic_restores_the_previous_sink_and_threads_are_independent() {
        let (_, outer) = collect(|| {
            let panic = std::panic::catch_unwind(|| collect(|| panic!("installer panic")));
            assert!(panic.is_err());
            let other = std::thread::spawn(|| collect(|| println!("other thread")))
                .join()
                .unwrap();
            assert_eq!(other.1.len(), 1);
            println!("restored");
        });
        assert_eq!(outer.len(), 1);
        assert_eq!(outer[0].text, "restored\n");
    }
}
