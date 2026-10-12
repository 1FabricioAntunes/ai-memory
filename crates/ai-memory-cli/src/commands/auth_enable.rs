//! `ai-memory auth enable` — require a bearer token on this install's server.
//!
//! The opt-in for installs that predate the new-install default: it does what
//! `init` does on a fresh data dir, then re-points the wiring that already
//! exists at the token. Idempotent, so a second run changes nothing:
//!
//! 1. reuse `[auth].bearer_token_file`'s token, or mint
//!    `<data_dir>/auth-tokens/default` (`0600`);
//! 2. record `bearer_token_file` in `config.toml`, keeping its comments;
//! 3. give the hooks the token through `auth-token` / `auth-header`, the files
//!    every hook front door reads, unless another credential is stored there;
//! 4. re-register Claude Code's `ai-memory` MCP entry, when it points at this
//!    server, through `install-mcp`: it becomes the `mcp-bridge` stdio entry,
//!    which reads the token file itself.
//!
//! The token is never printed and never written into a harness config.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use crate::cli::{AuthEnableArgs, InstallMcpArgs, McpClient};
use crate::commands::install_mcp::{self, mcp_server_url_from_base};
use crate::config::Config;
use crate::local_auth::{self, HookCredential};
use crate::server_profiles::{self, ProfileName};

/// What happened to Claude Code's MCP registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaudeWiring {
    /// `--no-mcp`, or no Claude Code config to look at.
    Skipped,
    /// No `ai-memory` entry is registered.
    NotRegistered,
    /// The entry targets another server; it is left alone.
    OtherServer(String),
    /// The entry was re-registered as the stdio bridge.
    Rewired,
}

/// The result of one `auth enable`.
#[derive(Debug)]
pub(crate) struct EnableOutcome {
    /// A bearer was already supplied inline (`AI_MEMORY_AUTH_TOKEN` or
    /// `[auth].bearer_token`); nothing was changed.
    pub(crate) already_inline: bool,
    pub(crate) token_path: PathBuf,
    pub(crate) token_created: bool,
    pub(crate) config_path: PathBuf,
    pub(crate) config_changed: bool,
    pub(crate) hooks: Option<HookCredential>,
    pub(crate) claude: ClaudeWiring,
}

/// Run `auth enable`.
///
/// # Errors
/// Fails when a server profile named `default` would share the token file,
/// when `servers.toml` or `config.toml` cannot be parsed, or on a failed write.
pub fn run(config: &Config, args: &AuthEnableArgs, config_path: Option<&Path>) -> Result<()> {
    let claude_config = if args.no_mcp {
        None
    } else {
        install_mcp::mcp_config_path(McpClient::ClaudeCode).ok()
    };
    let outcome = enable(config, config_path, claude_config.as_deref())?;
    print_outcome(&outcome);
    Ok(())
}

pub(crate) fn enable(
    config: &Config,
    config_path: Option<&Path>,
    claude_config: Option<&Path>,
) -> Result<EnableOutcome> {
    let data_dir = &config.data_dir;
    let config_path = config_path.map_or_else(|| data_dir.join("config.toml"), PathBuf::from);
    let token_path = config.auth.token_file_path().map_or_else(
        || server_profiles::root_token_path(data_dir),
        Path::to_path_buf,
    );

    if config.auth.renderable_bearer_token().is_some() {
        return Ok(EnableOutcome {
            already_inline: true,
            token_path,
            token_created: false,
            config_path,
            config_changed: false,
            hooks: None,
            claude: ClaudeWiring::Skipped,
        });
    }

    refuse_a_profile_sharing_the_token(data_dir, &token_path)?;
    let minted = local_auth::provision_root_token(&token_path)?;
    let config_changed = if config.auth.bearer_token_file.is_none() {
        record_token_file(&config_path)?
    } else {
        false
    };
    let hooks = local_auth::seed_hook_credential(data_dir, &minted.token)?;

    let mut enabled = config.clone();
    enabled
        .auth
        .adopt_token_file(minted.path.clone(), minted.token.clone());
    let claude = match claude_config {
        Some(path) => rewire_claude_code(&enabled, path)?,
        None => ClaudeWiring::Skipped,
    };

    Ok(EnableOutcome {
        already_inline: false,
        token_path: minted.path,
        token_created: minted.created,
        config_path,
        config_changed,
        hooks: Some(hooks),
        claude,
    })
}

/// A registered server profile named `default` reads `auth-tokens/default`
/// too: the hook would then present this server's root bearer to that
/// profile's server.
fn refuse_a_profile_sharing_the_token(data_dir: &Path, token_path: &Path) -> Result<()> {
    let Some(name) = ProfileName::parse(server_profiles::ROOT_TOKEN_NAME) else {
        return Ok(());
    };
    if !server_profiles::profile_shares_root_token(data_dir, &name, Some(token_path)) {
        return Ok(());
    }
    if server_profiles::root_token_name_is_registered(data_dir)
        .context("reading servers.toml to check the `default` profile name")?
    {
        bail!(
            "a server profile named `default` is registered, and its token file is the one \
             `auth enable` would use for this server's root bearer. Re-register that profile \
             under another name (`ai-memory server add <name> ...`, then \
             `ai-memory server remove default`) and run `ai-memory auth enable` again."
        );
    }
    Ok(())
}

/// Set `[auth].bearer_token_file` in the config file, keeping every comment
/// and other key. Creates the file (owner-only) when the install never had
/// one. Returns whether the file changed.
fn record_token_file(config_path: &Path) -> Result<bool> {
    let existing = match std::fs::read_to_string(config_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", config_path.display()));
        }
    };
    let mut doc: toml_edit::DocumentMut = existing
        .as_deref()
        .unwrap_or_default()
        .parse()
        .with_context(|| format!("parsing {}", config_path.display()))?;
    let auth = doc
        .entry("auth")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_like_mut()
        .with_context(|| format!("`auth` in {} is not a table", config_path.display()))?;
    if auth.get("bearer_token_file").is_some() {
        return Ok(false);
    }
    auth.insert(
        "bearer_token_file",
        toml_edit::value(server_profiles::ROOT_TOKEN_RELATIVE),
    );

    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(config_path)
            .ok()
            .map(|meta| meta.permissions().mode())
    };
    if let Some(parent) = config_path.parent() {
        crate::commands::path_util::create_private_dir(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    // The atomic write's temp file is owner-only; an existing config keeps
    // the mode it had (a packaged one may be group-readable on purpose).
    ai_memory_wiki::write_atomic(config_path, doc.to_string().as_bytes())
        .with_context(|| format!("writing {}", config_path.display()))?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(mode))
            .with_context(|| format!("restoring the mode of {}", config_path.display()))?;
    }
    Ok(true)
}

/// The URL an existing Claude Code `ai-memory` entry targets: an HTTP entry's
/// `url`, or the bridge's `--server-url`.
fn claude_entry_url(path: &Path) -> Result<Option<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let root: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let Some(entry) = root.get("mcpServers").and_then(|s| s.get("ai-memory")) else {
        return Ok(None);
    };
    if let Some(url) = entry.get("url").and_then(serde_json::Value::as_str) {
        return Ok(Some(url.to_owned()));
    }
    let args: Vec<&str> = entry
        .get("args")
        .and_then(serde_json::Value::as_array)
        .map(|args| args.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default();
    Ok(args
        .iter()
        .position(|arg| *arg == "--server-url")
        .and_then(|i| args.get(i + 1))
        .map(|url| (*url).to_owned()))
}

fn rewire_claude_code(config: &Config, path: &Path) -> Result<ClaudeWiring> {
    let Some(url) = claude_entry_url(path)? else {
        return Ok(ClaudeWiring::NotRegistered);
    };
    // Only an entry for this install's server is re-pointed; one for a remote
    // server has its own credential and must keep its destination.
    let ours = mcp_server_url_from_base(&config.server_url);
    let without_flavor = url.split('?').next().unwrap_or(&url);
    if mcp_server_url_from_base(without_flavor) != ours {
        return Ok(ClaudeWiring::OtherServer(url));
    }
    install_mcp::run(
        config,
        InstallMcpArgs {
            client: McpClient::ClaudeCode,
            server_url: Some(ours),
            name: "ai-memory".to_owned(),
            auth_token: None,
            apply: true,
            config_file: Some(path.to_path_buf()),
            session_aware: true,
            flavor: None,
            token_env_var: false,
        },
    )?;
    Ok(ClaudeWiring::Rewired)
}

fn print_outcome(outcome: &EnableOutcome) {
    if outcome.already_inline {
        println!(
            "auth: already required by AI_MEMORY_AUTH_TOKEN or [auth].bearer_token; nothing changed."
        );
        return;
    }
    println!(
        "auth: bearer token {} at {} (owner-only)",
        if outcome.token_created {
            "generated"
        } else {
            "kept"
        },
        outcome.token_path.display()
    );
    println!(
        "config: {} {}",
        if outcome.config_changed {
            "recorded [auth].bearer_token_file in"
        } else {
            "already names the token file:"
        },
        outcome.config_path.display()
    );
    match outcome.hooks {
        Some(HookCredential::Seeded) => {
            println!("hooks: given the token through the hook credential files");
        }
        Some(HookCredential::Current) => println!("hooks: already use the token"),
        Some(HookCredential::KeptOther) => println!(
            "hooks: another credential is stored for them and was kept; re-run \
             `ai-memory install-hooks --agent <agent> --apply` to switch them to this token"
        ),
        None => {}
    }
    match &outcome.claude {
        ClaudeWiring::Rewired => {
            println!(
                "claude-code: MCP entry now runs `ai-memory mcp-bridge`, which reads the token file"
            );
        }
        ClaudeWiring::OtherServer(url) => {
            println!("claude-code: MCP entry targets {url}, another server; left unchanged");
        }
        ClaudeWiring::NotRegistered | ClaudeWiring::Skipped => {}
    }
    println!(
        "Other MCP clients: re-run `ai-memory install-mcp --client <client> --apply`. Codex and \
         Prime Agent then read the token from $AI_MEMORY_AUTH_TOKEN; clients that only take a \
         literal header are refused rather than given the token."
    );
    println!("Restart the ai-memory server for it to require the token.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_in(data_dir: &Path) -> Config {
        Config::load_with_runtime_env(
            None,
            Some(data_dir.to_path_buf()),
            crate::config::RuntimeEnv::default(),
        )
        .unwrap()
    }

    /// The pre-default config `init` wrote: a token pepper and comments, no
    /// bearer.
    fn pre_a1_install(data_dir: &Path) {
        std::fs::create_dir_all(data_dir).unwrap();
        std::fs::write(
            data_dir.join("config.toml"),
            "# operator comment\nlog_level = \"info\"\n\n[auth]\n# keep me\nsecure_cookie = false\ntoken_pepper = \"pp\"\n",
        )
        .unwrap();
    }

    fn claude_json(home: &Path, url: &str) -> PathBuf {
        let path = home.join(".claude.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "mcpServers": {
                    "ai-memory": {"type": "http", "url": url},
                    "other": {"type": "http", "url": "https://other.example/mcp"}
                }
            })
            .to_string(),
        )
        .unwrap();
        path
    }

    /// Every file under `root`, read as text where it is text.
    fn files_under(root: &Path) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push((path, text));
                }
            }
        }
        out
    }

    /// `auth enable` on a pre-default install: token minted, the key recorded with
    /// the operator's comments intact, the hooks given the token, Claude Code
    /// re-registered as the bridge. Run twice, the second run changes nothing,
    /// and the token appears in no harness config.
    #[test]
    fn enable_twice_is_idempotent_and_leaks_the_token_into_no_harness_config() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        pre_a1_install(&data_dir);
        let claude = claude_json(&home, "http://127.0.0.1:49374/mcp");

        let first = enable(&config_in(&data_dir), None, Some(&claude)).unwrap();
        assert!(first.token_created);
        assert!(first.config_changed);
        assert_eq!(first.hooks, Some(HookCredential::Seeded));
        assert_eq!(first.claude, ClaudeWiring::Rewired);

        let token = crate::config::read_trimmed_secret(&first.token_path).unwrap();
        let config_text = std::fs::read_to_string(data_dir.join("config.toml")).unwrap();
        assert!(config_text.contains("# operator comment"), "{config_text}");
        assert!(config_text.contains("# keep me"), "{config_text}");
        assert!(
            config_text.contains("token_pepper = \"pp\""),
            "{config_text}"
        );
        assert!(
            config_text.contains("bearer_token_file = \"auth-tokens/default\""),
            "{config_text}"
        );
        let claude_after_first = std::fs::read_to_string(&claude).unwrap();
        let entry: serde_json::Value = serde_json::from_str(&claude_after_first).unwrap();
        let entry = &entry["mcpServers"]["ai-memory"];
        assert_eq!(entry["type"], "stdio");
        assert_eq!(entry["command"], "ai-memory");
        assert!(entry.get("env").is_none(), "no token env: {entry}");
        assert_eq!(
            config_in(&data_dir).auth.bearer_token.as_deref(),
            Some(token.as_str())
        );

        let second = enable(&config_in(&data_dir), None, Some(&claude)).unwrap();
        assert!(!second.token_created);
        assert!(!second.config_changed);
        assert_eq!(second.hooks, Some(HookCredential::Current));
        assert_eq!(second.claude, ClaudeWiring::Rewired);
        assert_eq!(
            crate::config::read_trimmed_secret(&second.token_path),
            Some(token.clone())
        );
        assert_eq!(
            std::fs::read_to_string(data_dir.join("config.toml")).unwrap(),
            config_text
        );
        assert_eq!(
            std::fs::read_to_string(&claude).unwrap(),
            claude_after_first
        );

        for (path, text) in files_under(&home) {
            assert!(
                !text.contains(&token),
                "token leaked into {}",
                path.display()
            );
        }
        for (path, text) in files_under(&data_dir) {
            let allowed = path == first.token_path
                || path == crate::config::hook_auth_token_path_in(&data_dir)
                || path == crate::config::hook_auth_header_path_in(&data_dir);
            assert!(
                allowed || !text.contains(&token),
                "token copied to an unexpected file {}",
                path.display()
            );
        }
    }

    /// Control: an entry for a remote server keeps its destination.
    #[test]
    fn enable_leaves_a_claude_entry_for_another_server_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        pre_a1_install(&data_dir);
        let claude = claude_json(tmp.path(), "https://team.example/mcp");
        let before = std::fs::read_to_string(&claude).unwrap();

        let outcome = enable(&config_in(&data_dir), None, Some(&claude)).unwrap();
        assert_eq!(
            outcome.claude,
            ClaudeWiring::OtherServer("https://team.example/mcp".into())
        );
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), before);
    }

    /// A registered profile named `default` would read the same file as the
    /// root bearer and present it to its own server: refused before anything
    /// is written.
    #[test]
    fn enable_refuses_while_a_default_profile_would_share_the_token() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        pre_a1_install(&data_dir);
        let default = ProfileName::parse("default").unwrap();
        server_profiles::add(
            &data_dir,
            &default,
            "https://team.example",
            &[],
            Some("team"),
        )
        .unwrap();

        let err = enable(&config_in(&data_dir), None, None).unwrap_err();
        assert!(format!("{err:#}").contains("named `default`"), "{err:#}");
        assert_eq!(
            server_profiles::read_token(&data_dir, &default).as_deref(),
            Some("team"),
            "the profile's own token is untouched"
        );
        assert!(
            !std::fs::read_to_string(data_dir.join("config.toml"))
                .unwrap()
                .contains("bearer_token_file")
        );

        // Control: another profile name does not conflict.
        server_profiles::remove(&data_dir, &default).unwrap();
        let team = ProfileName::parse("team").unwrap();
        server_profiles::add(&data_dir, &team, "https://team.example", &[], Some("team")).unwrap();
        assert!(enable(&config_in(&data_dir), None, None).is_ok());
    }

    /// An inline bearer is already enforcement; `auth enable` adds nothing.
    #[test]
    fn enable_with_an_inline_bearer_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        pre_a1_install(&data_dir);
        let mut config = config_in(&data_dir);
        config.auth.bearer_token = Some("inline".into());

        let outcome = enable(&config, None, None).unwrap();
        assert!(outcome.already_inline);
        assert!(!server_profiles::root_token_path(&data_dir).exists());
        assert!(crate::config::read_hook_auth_token(&data_dir).is_none());
    }

    /// An install that never had a config file gets one, owner-only.
    #[test]
    fn enable_without_a_config_file_creates_one() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        enable(&config_in(&data_dir), None, None).unwrap();
        let loaded = config_in(&data_dir);
        assert!(loaded.auth.bearer_token.is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(data_dir.join("config.toml"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
