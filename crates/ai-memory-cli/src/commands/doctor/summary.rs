//! Bounded installation checks shared with the setup summary.

use super::*;

pub(crate) struct SetupDiagnostics {
    pub(crate) text: String,
    pub(crate) action_required: bool,
}

pub(crate) async fn setup_diagnostics(
    config: &Config,
    agents: &[&str],
) -> Result<SetupDiagnostics> {
    use std::fmt::Write as _;
    let report = collect(
        config,
        &crate::cli::DoctorArgs {
            workspace: None,
            project: None,
            since_days: 30,
            json: false,
        },
    )
    .await?;
    let mut text = String::new();
    let mut action_required = false;
    for agent in agents {
        if hooks_need_approval(&report, agent) {
            action_required = true;
            let _ = writeln!(text, "  ! {agent:<14} Hook approval required");
            let _ = writeln!(
                text,
                "    Open Codex, run /hooks, and approve the ai-memory hooks."
            );
        } else {
            let _ = writeln!(
                text,
                "  · {agent:<14} Restart required; capture not yet verified"
            );
        }
        if *agent == "codex"
            && report.codex_hooks.is_none()
            && !report
                .plugins
                .iter()
                .any(|plugin| plugin.hook_trust.is_some())
        {
            text.push_str("    Hook approval status unavailable; review /hooks in Codex.\n");
        }
    }
    if report.marker_capture_problem.is_some() {
        action_required = true;
        text.push_str("  ! Invalid capture settings. Fix the marker and run ai-memory doctor.\n");
    }
    if let Some(problem) = &report.capture_coverage_problem {
        action_required = true;
        let _ = writeln!(text, "  ! Capture coverage unavailable: {}", problem.reason);
    }
    if report.capture_owner_active {
        action_required = true;
        text.push_str(
            "  ! External capture owner active; check delivery from the external producer.\n",
        );
    }
    // Keep actionable diagnostics from optional integrations visible, without
    // making pre-install session history look like a failed new installation.
    text.push_str(&render_transport_health(&report));
    text.push_str(&render_plugins(&report.plugins));
    text.push_str(&render_muse(report.muse.as_ref()));
    text.push_str(&super::super::agentmemory_residue::render(
        &report.agentmemory_residue,
    ));
    Ok(SetupDiagnostics {
        text,
        action_required,
    })
}
