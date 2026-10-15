/// The native hook-trust protocol is simulated; storage, MCP and HTTP run
/// through the real executable. The operator's Codex is never started.
#[tokio::test]
async fn setup_names_pending_codex_trust_and_keeps_the_first_run_neutral() {
    use std::os::unix::fs::PermissionsExt as _;
    let sandbox = Sandbox::new();
    let service = Service::new(&sandbox.root);
    let hooks = sandbox.home.join(".codex/hooks.json");
    let reply = json!({"jsonrpc":"2.0", "id":2, "result":{"data":[{"hooks":[{
        "eventName":"sessionStart", "trustStatus":"untrusted", "sourcePath":hooks,
        "command":"ai-memory hook --event session-start --agent codex"
    }]}]}}).to_string();
    let fake = service.bin.join("codex");
    std::fs::write(&fake, format!(
        "#!/bin/sh\nwhile IFS= read -r line; do\ncase \"$line\" in\n*'\"id\":2'*) printf '%s\\n' '{}' ;;\nesac\ndone\n", reply
    )).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let port = crate::e2e_support::free_port();
    let out = setup(&sandbox, &service, port, &["--yes", "--agent", "codex", "--agent", "claude-code"]);
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Installation complete · Action required"), "{stdout}");
    assert!(stdout.contains("Hook approval required"), "{stdout}");
    assert!(stdout.contains("run /hooks"), "{stdout}");
    assert!(!stdout.contains("install-hooks --agent codex --apply"), "{stdout}");
    assert!(!stdout.contains("ran here but nothing was captured"), "{stdout}");
    assert!(!stdout.contains("Every harness"), "{stdout}");
    assert!(!stdout.contains("\u{1b}["), "piped setup must be plain text");
}

#[test]
fn failed_connection_never_claims_installation_complete() {
    let sandbox = Sandbox::new();
    let port = crate::e2e_support::free_port();
    let out = sandbox.cmd(Some(port)).args(["setup", "--yes", "--no-service", "--agent", "claude-code"]).output().unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Installation incomplete"), "{stdout}");
    assert!(stdout.contains("Resolve the problems below"), "{stdout}");
    assert!(!stdout.contains("Installation complete"), "{stdout}");
}

#[test]
fn setup_keeps_files_backups_and_actions_in_grouped_agent_reports() {
    use std::os::unix::fs::PermissionsExt as _;
    let sandbox = Sandbox::new();
    let service = Service::new(&sandbox.root);
    let fake = service.bin.join("codex");
    std::fs::write(&fake, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let existing = [".claude.json", ".claude/settings.json", ".codex/hooks.json", ".gemini/config/mcp_config.json"];
    for file in existing {
        let path = sandbox.home.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}").unwrap();
    }
    std::fs::write(sandbox.home.join(".codex/config.toml"), "").unwrap();
    let port = crate::e2e_support::free_port();
    let out = setup(&sandbox, &service, port, &["--yes", "--agent", "claude-code", "--agent", "codex", "--agent", "open-code", "--agent", "antigravity-cli"]);
    assert!(out.status.success(), "{}", text(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let compact = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    for file in existing.into_iter().chain([".codex/config.toml", ".config/opencode/opencode.json", ".config/opencode/plugins/ai-memory.ts", ".gemini/config/hooks.json"]) {
        assert!(stdout.contains(&format!("~/{file}")), "{file}: {}", text(&out));
        assert!(sandbox.home.join(file).exists(), "{file}");
    }
    assert_eq!(compact.matches("backup written next to it").count(), 5, "{stdout}");
    assert_eq!(stdout.matches("capture-policy capability").count(), 0, "{stdout}");
    assert_eq!(stdout.matches("Capture-policy capability").count(), 1, "{stdout}");
    for information in ["denylist", "staged 18 hook script(s)", "staged 14 hook script(s)", "staged 8 hook script(s)", "$AI_MEMORY_AUTH_TOKEN", "Hooks need review", "Trust all and continue", "trust individually", "codex --dangerously-bypass-hook-trust", "review hook scripts first", "OpenCode auto-loads plugins", "one execution loop, not the conversation", "ai-memory finalize-session --agent antigravity-cli", "final summary and handoff", "opt-in SessionEnd consolidation"] {
        assert!(compact.contains(information), "{information}: {}", text(&out));
    }
    assert!(stdout.contains("AGENT NEXT STEPS"), "{stdout}");
    assert!(!stdout.contains('\u{1b}'), "redirected setup must remain plain: {stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("hook script(s)"), "{stderr}");
    assert!(!stderr.contains("capture-policy capability"), "{stderr}");
    let token = std::fs::read_to_string(sandbox.data_dir().join("auth-tokens/default")).unwrap();
    assert!(!text(&out).contains(token.trim()));
}
