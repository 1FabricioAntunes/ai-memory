//! The setup plan and final result share the terminal's visual vocabulary.

use super::*;
use crate::terminal::{Terminal, Tone};

pub(super) fn plan(plan: &SetupPlan, terminal: &Terminal) -> String {
    let mut out = terminal.header(
        "SETUP / connect your coding agents",
        env!("CARGO_PKG_VERSION"),
    );
    let names: Vec<_> = plan
        .agents
        .iter()
        .map(|agent| agent.agent.kind().as_str())
        .collect();
    out.push_str(&terminal.field(
        "Agents",
        &if names.is_empty() {
            "None detected".to_owned()
        } else {
            names.join(" · ")
        },
    ));
    out.push_str(&terminal.field("Storage", &plan.data_dir.display().to_string()));
    out.push_str(&terminal.field("Config", &plan.config_path.display().to_string()));
    out.push_str(&terminal.section("Your plan"));
    out.push_str(&terminal.document(&format!(
        "  01  {}\n",
        if plan.fresh {
            "Initialize storage and protected authentication"
        } else {
            "Keep existing data and configuration"
        }
    )));
    match &plan.service {
        ServicePlan::Install(unit) => out.push_str(&terminal.document(&format!(
            "  02  Install or start a {}\n",
            unit.manager.describe()
        ))),
        ServicePlan::Skip(reason) => {
            out.push_str(&terminal.document(&format!("  02  Service: {reason}\n")))
        }
    }
    for agent in &plan.agents {
        out.push_str(&terminal.document(&format!(
            "  03  Connect {}{}{}\n",
            agent.agent.kind().as_str(),
            if plan.detected { " (detected)" } else { "" },
            if agent.plugin {
                " through its existing plugin"
            } else {
                " with MCP, hooks and memory skills"
            }
        )));
    }
    out.push_str(&terminal.document("  04  Verify server and memory API connections\n"));
    if plan.agents.is_empty() {
        out.push_str(&terminal.notice(
            Tone::Warn,
            "NO AGENT DETECTED",
            "Name one with --agent <name>.",
        ));
    }
    out.push_str(&terminal.section("Capture"));
    out.push_str(&terminal.document("  Session hooks record prompts and supported tool activity under your policy.\n  No model API key is required in the default mode.\n\n"));
    out
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finish(
    terminal: &Terminal,
    title: &str,
    tone: Tone,
    diagnostics: Option<&str>,
    problems: bool,
    config: &Config,
    endpoint: &ServerEndpoint,
    config_path: &Path,
) -> String {
    let mut out = terminal.notice(
        tone,
        title,
        diagnostics.unwrap_or("Connection checks did not finish. Review the problems below."),
    );
    out.push_str(&terminal.section("Next"));
    if problems {
        out.push_str(
            &terminal.document("  Resolve the problems below, then re-run ai-memory setup.\n"),
        );
    } else {
        out.push_str(&terminal.document("  1  Restart your selected agents to load the integration.\n  2  Start a session in a project, then run ai-memory doctor there.\n"));
    }
    out.push_str(&terminal.document("  Installation checks do not verify session capture.\n"));
    out.push_str(&terminal.section("Your installation"));
    out.push_str(&terminal.field("Dashboard", &endpoint.build_url("/web")));
    if let Some(file) = config.auth.token_file.as_ref().filter(|file| file.loaded) {
        out.push_str(&terminal.field(
            "Sign in",
            "Any username · use the contents of the token file as your password",
        ));
        out.push_str(&terminal.field("Token file", &file.path.display().to_string()));
    }
    out.push_str(&terminal.field("Diagnostics", "ai-memory doctor"));
    out.push_str(&terminal.field("Data", &config.data_dir.display().to_string()));
    out.push_str(&terminal.field("Config", &config_path.display().to_string()));
    if config.auth.bearer_token.is_none() {
        out.push_str(&terminal.notice(
            Tone::Warn,
            "AUTHENTICATION DISABLED",
            "Run ai-memory auth enable to require a token.",
        ));
    } else {
        out.push_str(&terminal.field("Auth", "Bearer token configured · never printed"));
    }
    out
}
