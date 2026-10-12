//! `ai-memory project init` — create a checkout-local scope marker.
//!
//! This command deliberately stays local: it chooses a directory, suggests a
//! scope from the checkout's Git metadata, then creates `.ai-memory.toml`.
//! It does not create a server-side project or contact the configured server.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{self, IsTerminal as _, Write as _};
use std::path::Path;

use ai_memory_core::MARKER_FILENAME;
use anyhow::{Context, Result, bail};

use crate::cli::ProjectInitArgs;
use crate::config::DEFAULT_WORKSPACE;

const FALLBACK_PROJECT: &str = "project";

#[derive(Debug, PartialEq, Eq)]
struct Suggestions {
    workspace: String,
    project: String,
}

/// Run the interactive marker initializer.
///
/// # Errors
/// Returns an error outside an interactive terminal, when the current
/// directory or marker cannot be inspected, or when the marker cannot be
/// created without replacing an existing file.
pub fn run(_: ProjectInitArgs) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("`ai-memory project init` needs an interactive terminal");
    }
    let cwd = std::env::current_dir().context("reading the current directory")?;
    initialize(&cwd, &mut io::stdin().lock(), &mut io::stdout().lock())
}

fn initialize(cwd: &Path, input: &mut impl io::BufRead, output: &mut impl io::Write) -> Result<()> {
    let repo_root = ai_memory_consolidate::discover_repo_root(cwd).ok();
    let directory = repo_root.as_deref().unwrap_or(cwd);
    let marker = directory.join(MARKER_FILENAME);
    if marker
        .try_exists()
        .with_context(|| format!("checking {}", marker.display()))?
    {
        bail!(
            "{} already exists; refusing to overwrite it",
            marker.display()
        );
    }

    let suggestions = suggestions_for(
        directory,
        repo_root.is_some(),
        preferred_remote_identity(cwd).as_deref(),
    );
    let workspace = prompt_scope_value("Workspace", &suggestions.workspace, input, output)?;
    let project = prompt_scope_value("Project", &suggestions.project, input, output)?;
    let body = render_marker(&workspace, &project);
    write_new_marker(&marker, &body)?;
    writeln!(output, "Created {}.", marker.display()).context("reporting created marker")?;
    Ok(())
}

fn preferred_remote_identity(cwd: &Path) -> Option<String> {
    let (upstream, origin) = ai_memory_consolidate::read_identity_remotes(cwd);
    normalized_remote(upstream.as_deref(), origin.as_deref())
}

fn normalized_remote(upstream: Option<&str>, origin: Option<&str>) -> Option<String> {
    [upstream, origin]
        .into_iter()
        .flatten()
        .find_map(ai_memory_core::repository_identity::normalize_remote_url)
}

fn suggestions_for(directory: &Path, is_git_checkout: bool, remote: Option<&str>) -> Suggestions {
    if let Some(remote) = remote
        && let Some(suggestions) = suggestions_from_remote(remote)
    {
        return suggestions;
    }

    Suggestions {
        workspace: if is_git_checkout {
            DEFAULT_WORKSPACE.to_owned()
        } else {
            directory
                .parent()
                .and_then(path_basename)
                .unwrap_or_else(|| DEFAULT_WORKSPACE.to_owned())
        },
        project: path_basename(directory).unwrap_or_else(|| FALLBACK_PROJECT.to_owned()),
    }
}

fn suggestions_from_remote(remote: &str) -> Option<Suggestions> {
    let segments: Vec<&str> = remote
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let project = segments.last()?.to_string();
    let workspace = if segments.len() >= 3 {
        segments[segments.len() - 2].to_owned()
    } else {
        DEFAULT_WORKSPACE.to_owned()
    };
    Some(Suggestions { workspace, project })
}

fn path_basename(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
}

fn prompt_scope_value(
    label: &str,
    suggestion: &str,
    input: &mut impl io::BufRead,
    output: &mut impl io::Write,
) -> Result<String> {
    loop {
        write!(output, "{label} [{suggestion}]: ").context("prompting for marker scope")?;
        output.flush().context("flushing marker scope prompt")?;
        let mut line = String::new();
        if input
            .read_line(&mut line)
            .context("reading marker scope input")?
            == 0
        {
            bail!("aborted (no input)");
        }
        let value = if line.trim().is_empty() {
            suggestion
        } else {
            line.trim()
        };
        if valid_scope_value(value) {
            return Ok(value.to_owned());
        }
        writeln!(
            output,
            "Enter a non-empty workspace or project name without '/'."
        )
        .context("reporting invalid marker scope")?;
    }
}

fn valid_scope_value(value: &str) -> bool {
    !value.is_empty() && !value.contains('/')
}

fn render_marker(workspace: &str, project: &str) -> String {
    let mut document = toml_edit::DocumentMut::new();
    document["workspace"] = toml_edit::value(workspace);
    document["project"] = toml_edit::value(project);
    document["recall"] = toml_edit::Item::Table(toml_edit::Table::new());
    document["recall"]["default_global"] = toml_edit::value(false);
    document["profile"] = toml_edit::Item::Table(toml_edit::Table::new());
    document["profile"]["contribute"] = toml_edit::value(false);
    document["profile"]["consume"] = toml_edit::value(false);
    document["briefing"] = toml_edit::Item::Table(toml_edit::Table::new());
    document["briefing"]["inject_on_session_start"] = toml_edit::value(true);
    document["briefing"]["max_chars"] = toml_edit::value(4_000);
    document.to_string()
}

fn write_new_marker(path: &Path, body: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(body.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::process::Command;

    use super::*;

    #[test]
    fn upstream_wins_over_origin_when_suggesting_scope() {
        let remote = normalized_remote(
            Some("git@git.example.test:acme/platform/api.git"),
            Some("git@github.com:personal/api.git"),
        )
        .unwrap();
        assert_eq!(remote, "git.example.test/acme/platform/api");
        assert_eq!(
            suggestions_from_remote(&remote),
            Some(Suggestions {
                workspace: "platform".to_owned(),
                project: "api".to_owned(),
            })
        );
    }

    #[test]
    fn origin_is_used_when_upstream_is_not_a_remote_identity() {
        assert_eq!(
            normalized_remote(Some("../parent"), Some("https://github.com/acme/api.git")),
            Some("github.com/acme/api".to_owned())
        );
    }

    #[test]
    fn ownerless_remote_uses_default_workspace() {
        assert_eq!(
            suggestions_from_remote("github.com/api"),
            Some(Suggestions {
                workspace: "default".to_owned(),
                project: "api".to_owned(),
            })
        );
    }

    #[test]
    fn directories_without_git_use_parent_and_current_name() {
        let dir = Path::new("/work/team/service");
        assert_eq!(
            suggestions_for(dir, false, None),
            Suggestions {
                workspace: "team".to_owned(),
                project: "service".to_owned(),
            }
        );
    }

    #[test]
    fn git_checkout_without_remote_uses_default_and_root_name() {
        let dir = Path::new("/work/team/service");
        assert_eq!(
            suggestions_for(dir, true, None),
            Suggestions {
                workspace: "default".to_owned(),
                project: "service".to_owned(),
            }
        );
    }

    #[test]
    fn root_fallbacks_are_stable() {
        assert_eq!(
            suggestions_for(Path::new("/"), false, None),
            Suggestions {
                workspace: "default".to_owned(),
                project: "project".to_owned(),
            }
        );
    }

    #[test]
    fn marker_body_matches_the_interactive_template() {
        assert_eq!(
            render_marker("acme", "api"),
            "workspace = \"acme\"\nproject = \"api\"\n\n[recall]\ndefault_global = false\n\n[profile]\ncontribute = false\nconsume = false\n\n[briefing]\ninject_on_session_start = true\nmax_chars = 4000\n"
        );
    }

    #[test]
    fn initialize_writes_a_new_marker_from_entered_values() {
        let temp = tempfile::tempdir().unwrap();
        let mut input = Cursor::new(b"team\napi\n");
        let mut output = Vec::new();

        initialize(temp.path(), &mut input, &mut output).unwrap();

        assert_eq!(
            std::fs::read_to_string(temp.path().join(MARKER_FILENAME)).unwrap(),
            render_marker("team", "api")
        );
        assert!(String::from_utf8(output).unwrap().contains("Created "));
    }

    #[test]
    fn initialize_accepts_the_suggested_values_on_empty_input() {
        let temp = tempfile::tempdir().unwrap();
        let suggestions = suggestions_for(temp.path(), false, None);
        let mut input = Cursor::new(b"\n\n");
        let mut output = Vec::new();

        initialize(temp.path(), &mut input, &mut output).unwrap();

        assert_eq!(
            std::fs::read_to_string(temp.path().join(MARKER_FILENAME)).unwrap(),
            render_marker(&suggestions.workspace, &suggestions.project)
        );
    }

    #[test]
    fn initialize_from_a_worktree_descendant_writes_at_that_worktree_root() {
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().join("main");
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["config", "user.email", "fixture@example.test"]);
        git(&main, &["config", "user.name", "Fixture"]);
        std::fs::write(main.join("README.md"), "fixture\n").unwrap();
        git(&main, &["add", "README.md"]);
        git(&main, &["commit", "--no-gpg-sign", "-qm", "initial"]);

        let worktree = temp.path().join("linked");
        git(
            &main,
            &["worktree", "add", "-q", worktree.to_str().unwrap(), "HEAD"],
        );
        let nested = worktree.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let mut input = Cursor::new(b"team\napi\n");
        let mut output = Vec::new();

        initialize(&nested, &mut input, &mut output).unwrap();

        assert!(worktree.join(MARKER_FILENAME).is_file());
        assert!(!main.join(MARKER_FILENAME).exists());
    }

    #[test]
    fn initialize_reprompts_for_slash_values() {
        let temp = tempfile::tempdir().unwrap();
        let mut input = Cursor::new(b"team/api\nteam\napi/service\napi\n");
        let mut output = Vec::new();

        initialize(temp.path(), &mut input, &mut output).unwrap();

        assert_eq!(
            std::fs::read_to_string(temp.path().join(MARKER_FILENAME)).unwrap(),
            render_marker("team", "api")
        );
        assert_eq!(
            String::from_utf8(output)
                .unwrap()
                .matches("without '/'.")
                .count(),
            2
        );
    }

    #[test]
    fn initialize_refuses_to_overwrite_an_existing_marker() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join(MARKER_FILENAME);
        std::fs::write(&marker, "workspace = \"original\"\n").unwrap();
        let mut input = Cursor::new(b"team\napi\n");
        let mut output = Vec::new();

        let error = initialize(temp.path(), &mut input, &mut output).unwrap_err();

        assert!(error.to_string().contains("refusing to overwrite"));
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "workspace = \"original\"\n"
        );
        assert!(output.is_empty());
    }

    #[test]
    fn initialize_aborts_on_end_of_input_without_creating_a_marker() {
        let temp = tempfile::tempdir().unwrap();
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = initialize(temp.path(), &mut input, &mut output).unwrap_err();

        assert_eq!(error.to_string(), "aborted (no input)");
        assert!(!temp.path().join(MARKER_FILENAME).exists());
    }

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("running git {args:?}: {error}"));
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
