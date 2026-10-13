//! Hook events launched by an ai-memory marketplace plugin (#1166).
//!
//! The plugin's launcher (`plugins/<harness>/scripts/ai-memory-hook`) hands
//! every lifecycle event to `ai-memory hook` with `AI_MEMORY_HOOK_ORIGIN` set
//! to `<agent>-plugin`. Everything else about the event (capture policy,
//! marker scope, server profiles, spooling) is the ordinary native hook path;
//! this module only decides whether a plugin-launched event is dropped before
//! that path starts:
//!
//! - **De-duplication.** A harness runs a plugin's hook and an identical
//!   settings-file hook separately, so a user who installed both would be
//!   captured twice. The settings copy wins (it carries flags the plugin does
//!   not, such as `--capture-assistant` or a baked `--project-strategy`); the
//!   plugin copy of an event the settings hooks already deliver is dropped.
//! - **Folderless sessions.** A session with no project folder (no `cwd`, or a
//!   Claude desktop scratch workspace) is not captured through the plugin.
//!
//! An environment variable rather than a flag keeps an older binary working
//! with a newer plugin: it ignores the variable and only skips these checks.

use std::path::{Component, Path, PathBuf};

use super::install_hooks;
use super::render_shared::CLAUDE_CODE_EVENTS;

/// The environment variable the plugin launcher sets.
pub(crate) const HOOK_ORIGIN_ENV: &str = "AI_MEMORY_HOOK_ORIGIN";

/// Which plugin launched an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginOrigin {
    /// The Claude Code plugin (`plugins/claude-code`).
    ClaudeCode,
    /// The Codex plugin (`plugins/codex`).
    Codex,
}

impl PluginOrigin {
    /// The origin `lookup` reports, if any. Unknown values are not a plugin:
    /// the event takes the ordinary path.
    pub(crate) fn from_env(mut lookup: impl FnMut(&str) -> Option<String>) -> Option<Self> {
        match lookup(HOOK_ORIGIN_ENV)?.trim() {
            "claude-code-plugin" => Some(Self::ClaudeCode),
            "codex-plugin" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// Whether a plugin-launched event must be dropped before capture. `cwd` is
/// the payload's working directory; `settings_files` are the files whose
/// ai-memory hooks outrank the plugin (see [`settings_files_for`]).
pub(crate) fn drops_plugin_event(
    origin: PluginOrigin,
    event: &str,
    cwd: Option<&str>,
    settings_files: &[PathBuf],
) -> bool {
    if is_folderless(cwd) {
        return true;
    }
    let Some(name) = settings_event_name(origin, event) else {
        return false;
    };
    settings_files
        .iter()
        .any(|path| install_hooks::settings_file_carries_ai_memory_hook_for_event(path, name))
}

/// The settings files that outrank the plugin for `origin`.
///
/// Claude Code: the user `settings.json` (honouring `CLAUDE_CONFIG_DIR`) and
/// the checkout's `.claude/settings.local.json`, the two files
/// `install-hooks` writes. The checkout is `CLAUDE_PROJECT_DIR` when Claude
/// set it, else the payload's `cwd`.
///
/// Codex: none. Codex silently skips a settings hook the user has not
/// trusted, and nothing at hook time can tell a trusted one from a skipped
/// one, so dropping the plugin copy could lose the event entirely. `setup`
/// and `doctor` handle a double Codex install instead.
pub(crate) fn settings_files_for(
    origin: PluginOrigin,
    cwd: Option<&str>,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> Vec<PathBuf> {
    match origin {
        PluginOrigin::Codex => Vec::new(),
        PluginOrigin::ClaudeCode => {
            let mut files = Vec::with_capacity(2);
            let config_dir = lookup("CLAUDE_CONFIG_DIR").map(std::ffi::OsString::from);
            if let Ok(user) = install_hooks::claude_settings_path_in(config_dir) {
                files.push(user);
            }
            let project = lookup("CLAUDE_PROJECT_DIR")
                .filter(|dir| !dir.trim().is_empty())
                .or_else(|| cwd.map(str::to_owned));
            if let Some(project) = project {
                files.push(install_hooks::project_claude_settings_local(Path::new(
                    &project,
                )));
            }
            files
        }
    }
}

/// The settings-file event name for a wire event (`session-start` →
/// `SessionStart`).
fn settings_event_name(origin: PluginOrigin, event: &str) -> Option<&'static str> {
    match origin {
        PluginOrigin::ClaudeCode => CLAUDE_CODE_EVENTS
            .iter()
            .find(|(_, script)| script.strip_suffix(".sh") == Some(event))
            .map(|(name, _)| *name),
        PluginOrigin::Codex => None,
    }
}

/// A session with no project folder: no `cwd` at all, or one inside a Claude
/// desktop scratch workspace (`<Claude app dir>/scratch-workspaces/...`,
/// created for a Code-tab session started without a folder). Checked
/// lexically: canonicalizing diverges from the raw runtime path on macOS and
/// Windows.
fn is_folderless(cwd: Option<&str>) -> bool {
    let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
        return true;
    };
    let normalized = cwd.replace('\\', "/");
    let components: Vec<&str> = Path::new(&normalized)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    components
        .windows(2)
        .any(|pair| pair[0].eq_ignore_ascii_case("claude") && pair[1] == "scratch-workspaces")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl FnMut(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    const SETTINGS_HOOK: &str = r#"{"hooks":{"SessionStart":[{"matcher":"","hooks":[{"type":"command","command":"/usr/bin/ai-memory hook --event session-start --agent claude-code --server-url http://127.0.0.1:49374"}]}]}}"#;

    #[test]
    fn origin_parses_only_known_plugins() {
        assert_eq!(
            PluginOrigin::from_env(lookup(&[(HOOK_ORIGIN_ENV, "claude-code-plugin")])),
            Some(PluginOrigin::ClaudeCode)
        );
        assert_eq!(
            PluginOrigin::from_env(lookup(&[(HOOK_ORIGIN_ENV, " codex-plugin\n")])),
            Some(PluginOrigin::Codex)
        );
        for other in ["", "claude-code", "cursor-plugin", "settings"] {
            assert_eq!(
                PluginOrigin::from_env(lookup(&[(HOOK_ORIGIN_ENV, other)])),
                None,
                "{other:?}"
            );
        }
        assert_eq!(PluginOrigin::from_env(lookup(&[])), None);
    }

    /// Adversarial: with an ai-memory settings hook for the same event the
    /// plugin copy is dropped. Controls: another event, a third-party hook,
    /// and no settings file at all keep the plugin copy.
    #[test]
    fn plugin_copy_is_dropped_only_when_settings_deliver_the_same_event() {
        let temp = tempfile::tempdir().unwrap();
        let settings = temp.path().join("settings.json");
        std::fs::write(&settings, SETTINGS_HOOK).unwrap();
        let cwd = Some("/work/repo");
        let files = vec![settings.clone()];

        assert!(drops_plugin_event(
            PluginOrigin::ClaudeCode,
            "session-start",
            cwd,
            &files
        ));
        assert!(
            !drops_plugin_event(PluginOrigin::ClaudeCode, "stop", cwd, &files),
            "an event the settings hooks do not wire stays with the plugin"
        );
        assert!(!drops_plugin_event(
            PluginOrigin::ClaudeCode,
            "session-start",
            cwd,
            &[temp.path().join("missing.json")]
        ));

        std::fs::write(
            &settings,
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"node other-tool.mjs"}]}]}}"#,
        )
        .unwrap();
        assert!(!drops_plugin_event(
            PluginOrigin::ClaudeCode,
            "session-start",
            cwd,
            &files
        ));

        std::fs::write(&settings, "{ not json").unwrap();
        assert!(!drops_plugin_event(
            PluginOrigin::ClaudeCode,
            "session-start",
            cwd,
            &files
        ));
    }

    /// Codex settings hooks never suppress the plugin: an untrusted one would
    /// be skipped by Codex and the event lost.
    #[test]
    fn codex_plugin_events_are_never_deduplicated_against_settings() {
        let temp = tempfile::tempdir().unwrap();
        let settings = temp.path().join("hooks.json");
        std::fs::write(&settings, SETTINGS_HOOK).unwrap();
        assert!(!drops_plugin_event(
            PluginOrigin::Codex,
            "session-start",
            Some("/work/repo"),
            &[settings]
        ));
        assert!(settings_files_for(PluginOrigin::Codex, Some("/w"), lookup(&[])).is_empty());
    }

    #[test]
    fn settings_files_honour_claude_config_dir_and_project_dir() {
        let files = settings_files_for(
            PluginOrigin::ClaudeCode,
            Some("/payload/cwd"),
            lookup(&[
                ("CLAUDE_CONFIG_DIR", "/relocated/claude"),
                ("CLAUDE_PROJECT_DIR", "/the/project"),
            ]),
        );
        assert_eq!(
            files[0],
            Path::new("/relocated/claude").join("settings.json")
        );
        assert!(
            files[1].ends_with(Path::new(".claude").join("settings.local.json")),
            "{files:?}"
        );
        assert!(
            files[1].starts_with("/the/project"),
            "CLAUDE_PROJECT_DIR outranks the payload cwd: {files:?}"
        );

        let files = settings_files_for(
            PluginOrigin::ClaudeCode,
            Some("/payload/cwd"),
            lookup(&[("CLAUDE_PROJECT_DIR", " ")]),
        );
        assert!(files[1].starts_with("/payload/cwd"), "{files:?}");
    }

    #[test]
    fn folderless_sessions_are_dropped() {
        for cwd in [
            None,
            Some(""),
            Some("  "),
            Some("/home/me/.config/Claude/scratch-workspaces/acct/org/scratch-2026-10-13-ab12"),
            Some("/Users/me/Library/Application Support/Claude/scratch-workspaces/a/b/scratch-1"),
            Some(r"C:\Users\me\AppData\Roaming\Claude\scratch-workspaces\a\b\scratch-1"),
        ] {
            assert!(
                drops_plugin_event(PluginOrigin::ClaudeCode, "user-prompt-submit", cwd, &[]),
                "{cwd:?}"
            );
        }
        for cwd in [
            "/home/me/src/claude",
            "/home/me/src/scratch-workspaces",
            "/home/me/Claude/projects/app",
        ] {
            assert!(
                !drops_plugin_event(
                    PluginOrigin::ClaudeCode,
                    "user-prompt-submit",
                    Some(cwd),
                    &[]
                ),
                "{cwd}"
            );
        }
    }
}
