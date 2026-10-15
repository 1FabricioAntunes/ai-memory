//! Group the installers' existing reports. Unknown messages are retained in
//! full, and only identical policy reports are combined across agents.

use std::collections::BTreeMap;

use super::*;
use crate::commands::install_output::{self, Message};
use crate::terminal::{Terminal, Tone};

#[derive(Default)]
pub(super) struct AgentReport {
    messages: Vec<(&'static str, Message)>,
}

impl AgentReport {
    pub(super) fn plugin(&mut self, agent: AgentChoice) {
        self.message(
            "Plugin",
            format!(
                "plugin: {} is enabled; leaving the settings-file hooks, MCP entry and skills \
             alone (run `ai-memory install-hooks --agent {} --apply` to add them anyway)",
                agent_plugins::PLUGIN_ID,
                agent.kind().as_str()
            ),
        );
    }

    pub(super) fn collect<T>(&mut self, phase: &'static str, operation: impl FnOnce() -> T) -> T {
        let (result, messages) = install_output::collect(operation);
        self.messages
            .extend(messages.into_iter().map(|message| (phase, message)));
        result
    }

    pub(super) fn message(&mut self, phase: &'static str, text: String) {
        self.messages.push((
            phase,
            Message {
                text,
                stderr: false,
            },
        ));
    }
}

#[derive(Default)]
struct Policies {
    modes: BTreeMap<String, Vec<String>>,
    capabilities: BTreeMap<String, Vec<String>>,
}

impl Policies {
    fn accept(&mut self, text: &str, agent: &str) -> bool {
        let group = if let Some(mode) = text.strip_prefix("capture mode: ") {
            self.modes.entry(mode.to_owned()).or_default()
        } else if let Some(capability) = text.strip_prefix("[ai-memory] capture-policy capability ")
        {
            self.capabilities.entry(capability.to_owned()).or_default()
        } else {
            return false;
        };
        if !group.iter().any(|name| name == agent) {
            group.push(agent.to_owned());
        }
        true
    }

    fn render(&self, terminal: &Terminal) -> String {
        if self.modes.is_empty() && self.capabilities.is_empty() {
            return String::new();
        }
        let mut out = terminal.section("Capture policy");
        for (mode, agents) in &self.modes {
            out.push_str(&terminal.field("Mode", &format!("{mode} · {}", agents.join(" · "))));
        }
        for (capability, agents) in &self.capabilities {
            let text = capability.replace("this selected integration", &agents.join(", "));
            out.push_str(&terminal.document(&format!("  Capture-policy capability {text}\n")));
        }
        out
    }
}

fn name(agent: AgentChoice) -> &'static str {
    match agent {
        AgentChoice::ClaudeCode => "Claude Code",
        AgentChoice::Codex => "Codex",
        AgentChoice::OpenCode => "OpenCode",
        AgentChoice::OpenCode2 => "OpenCode 2",
        AgentChoice::AntigravityCli => "Antigravity CLI",
        _ => agent.kind().as_str(),
    }
}

/// File messages start with a path so the shared terminal can abbreviate the
/// home directory. Preserve the original outcome, backup note and script count.
fn file_detail(text: &str) -> Option<String> {
    let text = text.strip_prefix("✓ ")?;
    if let Some((description, path)) = text.split_once(" → ") {
        return Some(format!("{path} · {description}"));
    }
    let (verb, rest) = text.split_once(' ')?;
    if !matches!(verb, "created" | "updated" | "no-op") {
        return None;
    }
    let (path, note) = rest.rsplit_once(" (")?;
    Some(format!("{path} · {verb} · {}", note.strip_suffix(')')?))
}

fn render_agent(
    terminal: &Terminal,
    plan: AgentPlan,
    report: AgentReport,
    failed: bool,
    policies: &mut Policies,
) -> (String, String) {
    let agent = name(plan.agent);
    let mut details = format!(
        "\n  {}\n",
        terminal.ink(
            &format!(
                "{} {agent}{}",
                if failed { "✗" } else { "✓" },
                if failed { " · incomplete" } else { "" }
            ),
            if failed { Tone::Bad } else { Tone::Good },
        )
    );
    let mut notes = String::new();
    // A print! call may emit a partial line; join adjacent chunks from each
    // phase/channel before classifying so no instruction or warning is split.
    let mut chunks: Vec<(&str, bool, String)> = Vec::new();
    for (phase, message) in report.messages {
        if let Some((last_phase, last_stderr, text)) = chunks.last_mut()
            && *last_phase == phase
            && *last_stderr == message.stderr
        {
            text.push_str(&message.text);
        } else {
            chunks.push((phase, message.stderr, message.text));
        }
    }
    for (phase, stderr, text) in chunks {
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if policies.accept(line, agent) {
                continue;
            }
            if let Some(detail) = file_detail(line) {
                let label = if phase == "Hooks" && line.contains("plugin file") {
                    "Plugin"
                } else {
                    phase
                };
                details.push_str(&terminal.field(label, &detail));
            } else if phase == "Skills"
                || phase == "Plugin"
                || line == "Keeping the existing session-aware entry"
            {
                details.push_str(&terminal.field(phase, line));
            } else if stderr {
                details.push_str(&terminal.notice(Tone::Warn, &format!("{agent} / {phase}"), line));
            } else {
                // Retain unrecognized stdout verbatim as agent guidance.
                notes.push_str(&terminal.document(&format!("    {line}\n")));
            }
        }
    }
    if !notes.is_empty() {
        let (marker, tone) = if plan.agent == AgentChoice::Codex {
            ("!", Tone::Warn)
        } else {
            ("·", Tone::Accent)
        };
        notes = format!(
            "\n  {}\n{notes}",
            terminal.ink(&format!("{marker} {agent}"), tone)
        );
    }
    (details, notes)
}

pub(super) fn run(config: &Config, agents: &[AgentPlan], terminal: &Terminal) -> Vec<String> {
    let mut skills_done = BTreeSet::new();
    let mut policies = Policies::default();
    let mut notes = String::new();
    let mut problems = Vec::new();
    if !agents.is_empty() {
        print!("{}", terminal.section("Connect"));
    }
    for plan in agents {
        let mut report = AgentReport::default();
        let errors = wire_agent(config, *plan, &mut skills_done, &mut report);
        let (details, guidance) =
            render_agent(terminal, *plan, report, !errors.is_empty(), &mut policies);
        print!("{details}");
        notes.push_str(&guidance);
        problems.extend(
            errors
                .into_iter()
                .map(|error| format!("{}: {error}", plan.agent.kind().as_str())),
        );
    }
    print!("{}", policies.render(terminal));
    if !notes.is_empty() {
        print!("{}{notes}", terminal.section("Agent next steps"));
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_unknown_warnings_and_future_guidance_remain_visible() {
        let terminal =
            Terminal::stdout(&crate::config::RuntimeEnv::with_process_env_for_tests(&[(
                "NO_COLOR", "1",
            )]));
        let mut report = AgentReport::default();
        report.collect("Hooks", || {
            install_output::eprintln!("[ai-memory] selected shell/PowerShell compatibility path does not enforce capture-policy v1");
            install_output::eprintln!("Future installer warning: preserve this remedy");
            install_output::print!("Future instruction ");
            install_output::println!("with an important command");
        });
        let (details, notes) = render_agent(
            &terminal,
            AgentPlan::new(AgentChoice::Codex),
            report,
            true,
            &mut Policies::default(),
        );
        assert!(details.contains("✗ Codex · incomplete"), "{details}");
        assert!(!details.contains("✓ Codex"), "{details}");
        let warning = details.replace("\n  │ ", " ");
        assert!(
            warning.contains("does not enforce capture-policy v1"),
            "{details}"
        );
        assert!(details.contains("preserve this remedy"), "{details}");
        assert!(
            notes.contains("Future instruction with an important command"),
            "{notes}"
        );
        assert!(!format!("{details}{notes}").contains('\u{1b}'));
    }

    #[test]
    fn policy_groups_keep_agent_scope_and_never_hide_unsupported_capture() {
        let mut policies = Policies::default();
        let supported = "[ai-memory] capture-policy capability v1 enforced by this selected integration; re-run --apply to refresh existing installs.";
        assert!(policies.accept(supported, "Codex"));
        assert!(policies.accept(supported, "Claude Code"));
        assert!(policies.accept("capture mode: allowlist", "Codex"));
        assert!(policies.accept("capture mode: denylist", "Claude Code"));
        assert!(!policies.accept("[ai-memory] selected shell/PowerShell compatibility path does not enforce capture-policy v1", "Other"));
        assert_eq!(policies.capabilities.len(), 1);
        assert_eq!(policies.modes["allowlist"], ["Codex"]);
        assert_eq!(policies.modes["denylist"], ["Claude Code"]);
    }

    #[test]
    fn file_details_preserve_backups_noops_and_paths_with_spaces() {
        assert_eq!(
            file_detail("✓ updated /home/a b/config.json (backup written next to it)"),
            Some("/home/a b/config.json · updated · backup written next to it".to_owned())
        );
        assert_eq!(
            file_detail("✓ no-op /home/a/config.json (already up to date)"),
            Some("/home/a/config.json · no-op · already up to date".to_owned())
        );
        assert_eq!(
            file_detail("✓ staged 14 hook script(s) → /home/a/hooks"),
            Some("/home/a/hooks · staged 14 hook script(s)".to_owned())
        );
        assert_eq!(file_detail("Some future installer instruction"), None);
    }
}
