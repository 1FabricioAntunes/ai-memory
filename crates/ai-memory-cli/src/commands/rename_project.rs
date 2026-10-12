//! `ai-memory rename-project` — thin HTTP client for project rename.

use anyhow::Result;

use crate::cli::RenameProjectArgs;
use crate::config::Config;
use crate::http_client::{ServerEndpoint, post_json};

/// Run the `rename-project` subcommand.
///
/// Resolves the current project name (auto-derived from the git repo root
/// when `--from` is omitted) -- or, when `--project-id` is given, skips name
/// resolution and renames that id directly, which is the only way to reach a
/// legacy row whose name is shadowed by another project's `legacy_name` --
/// sends the rename request to the server, then prints a human-readable
/// confirmation line.
///
/// # Errors
/// Returns an error when the server is unreachable or returns a non-2xx
/// response (e.g. 404 workspace/project not found, 422 name taken or invalid).
pub async fn run(config: &Config, args: RenameProjectArgs) -> Result<()> {
    let endpoint = ServerEndpoint::from_config_resolving_auth(config).await;
    // Rename happens inside one workspace, so the marker resolves it the same
    // way it does for every other command. `--to` stays literal: it names the
    // new project, it does not select a scope.
    let (workspace, from, project_id) = match args.project_id {
        Some(id) => (
            super::resolve_workspace(config, args.workspace.as_deref())?,
            None,
            Some(id),
        ),
        None => {
            let (ws, from) =
                super::resolve_scope(config, args.workspace.as_deref(), args.from.as_deref())?;
            (ws, Some(from), None)
        }
    };
    // What the report and the client-local rekey name as the source. With
    // `--project-id` this is the id itself: the old name is only known to the
    // server, and a wrong name in the local rekey would miss the link it was
    // supposed to move.
    let target_label = match (&from, &project_id) {
        (Some(from), _) => from.clone(),
        (None, Some(id)) => id.clone(),
        (None, None) => unreachable!("clap requires one of --from/--project-id"),
    };
    let body = serde_json::json!({
        "workspace": workspace,
        "from": from,
        "project_id": project_id,
        "to": args.to,
    });
    let summary: serde_json::Value = post_json(&endpoint, "/admin/rename-project", &body).await?;
    let pages = summary["pages"].as_u64().unwrap_or(0);
    if let Some(warning) = summary["manifest_warning"].as_str() {
        eprintln!("ai-memory: warning: {warning}");
    }
    if let Err(error) = super::project_registry::rekey_scope(
        config,
        &endpoint,
        &workspace,
        &target_label,
        &workspace,
        &args.to,
    ) {
        eprintln!(
            "ai-memory: project renamed, but the client-local checkout link could not be updated ({error:#})"
        );
    }
    println!(
        "Renamed {}/{} → {}/{} ({} pages now under the new name).",
        workspace, target_label, workspace, args.to, pages
    );
    Ok(())
}
