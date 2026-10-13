# Design proposal: ai-memory as marketplace plugins (#1166)

**Status: proposal for 3.0, research done 2026-10-13. Not implemented.**
Builds on RFC #1166, the feedback comments on it, and the
[`docs/desktop/`](desktop/) notes (especially
[`verification-2026-10.md`](desktop/verification-2026-10.md) and
[`extensions.md`](desktop/extensions.md)).

## Summary

Ship ai-memory as a plugin in each coding app's plugin system, starting with
Claude Code. The plugin stays thin. It holds manifests, the routing skills,
an MCP entry that runs `ai-memory mcp-bridge`, and one small POSIX launcher
that hands every lifecycle event to the installed binary's native
`ai-memory hook` command. The server and the binary are still installed
separately. The plugin never captures through the curl script bundle, so
capture policy (`ignore_paths`, allowlist mode, server profiles) stays
enforced client-side exactly as it is for `install-hooks` today. When the
binary is missing, the plugin captures nothing, prints one line of guidance at
session start, and exits 0.

The marketplace lives in this repository. Catalog files sit at the repository
root and pin each plugin to the release tag, so `/plugin marketplace add
akitaonrails/ai-memory` only ever installs released plugin content.

Recommended 3.0 scope: the Claude Code plugin (CLI and the desktop Code tab),
its marketplace, CI validation, a sandboxed end-to-end test, and the
`setup`/`doctor`/`upgrade` integration. Codex follows in 3.0 if the trust
question below resolves cleanly. Cursor, Cowork capture, Antigravity and
Gemini come later.

## How this was checked

Two kinds of evidence, and the doc says which one backs each claim:

- **Docs.** Official documentation fetched on 2026-10-13. URLs are listed in
  [Sources](#sources).
- **Local runs.** Experiments on 2026-10-13 with Claude Code 2.1.296, Codex
  CLI 0.162.1, Gemini CLI 0.63.0 and `cursor-agent` 2026.10.01, each in a
  scratch config directory (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`). The
  operator's own agent configuration was not touched. A prototype Claude
  plugin with the manifests from this doc was validated, installed into the
  sandbox, and driven with `claude -p`.

What the local runs showed:

1. `claude plugin validate --strict <dir>` checks more than JSON syntax. It
   failed on an unknown hook event (`hooks.PostToolUze: unknown hook event;
   entry ignored at runtime`), on an unknown manifest key, and on a
   marketplace without `description`. Exit codes are usable in CI.
2. A sandboxed install works offline: `claude plugin marketplace add <path>`,
   `claude plugin install ai-memory@ai-memory`, `claude plugin details`.
3. **Plugin hooks fire in `claude -p` without any login.** With no
   credentials, `claude -p "say ok"` printed `Not logged in` but still ran
   the plugin's `SessionStart`, `UserPromptSubmit` and `SessionEnd` hooks.
   Through the prototype launcher and the 2.6.6 native binary, all three
   events landed in a scratch spool (`SessionStart` 38-63 ms including a
   failed handoff fetch, `UserPromptSubmit` 8-12 ms). The final run used the
   exact Claude manifests in this doc, loaded with `--plugin-dir`. CI can
   therefore test real hook wiring without an API key. Tool events need a
   model turn and cannot be driven this way.
4. **Shell-form hooks reject `${user_config.*}`.** Claude Code refused the
   hook with "references ${user_config.*} in a shell-form command. … Use exec
   form instead … or read $CLAUDE_PLUGIN_OPTION_<KEY> from the hook's
   environment."
5. **Option defaults are not applied in hooks.** With an option left unset,
   an exec-form hook referencing it failed ("Plugin option "server_url" isn't
   set"), and `CLAUDE_PLUGIN_OPTION_SERVER_URL` was absent from the hook
   environment. The same unset option *was* substituted with its default in
   the MCP server's `args`. Hooks must therefore never depend on an option
   being set; the launcher supplies its own defaults.
6. `CLAUDE_PLUGIN_OPTION_<KEY>` is exported to hook processes (shell and exec
   form) once the user sets the option. It is not exported to MCP servers.
7. The plugin's stdio MCP server receives `CLAUDE_CODE_SESSION_ID`, so the
   session-aware `ai-memory mcp-bridge` works unchanged. The server is named
   `plugin:ai-memory:ai-memory` and its tools are
   `mcp__plugin_ai-memory_ai-memory__<tool>`. Nothing in the codebase matches
   on the `mcp__ai-memory__` prefix, so the rename is harmless.
8. An exec-form hook whose binary is missing fails with a non-blocking
   "Executable not found in $PATH" error on every event.
9. **The Docker host wrapper runs `ai-memory hook` in a container.**
   `bin/ai-memory` sends `mcp-bridge`, `install-hooks` and a few other
   commands to the native host client, but not `hook`. A launcher that found
   the wrapper on `PATH` paid about 280 ms per event (a `docker run`), and
   inside the container `127.0.0.1` is the container. The native client
   took about 5 ms per event. The wrapper has to route `hook` to the native
   client before a plugin can rely on `PATH` (Phase 1, item 2).
10. Codex 0.162.1 accepted a plugin in the portable layout (root
    `plugin.json`, `mcp.json`, `hooks/hooks.json`) from a local
    `.agents/plugins/marketplace.json`: `codex plugin marketplace add` and
    `codex plugin add ai-memory@ai-memory` both succeeded in a scratch
    `CODEX_HOME`.

## Per-app capability table

Docs fetched 2026-10-13 unless marked otherwise. "Local" means confirmed in
the runs above.

| App | Hooks in a plugin | MCP in a plugin | Skills | Config and secrets | Updates | Distribution | Review / submission |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **Claude Code** (CLI, desktop Code tab) | Yes, `hooks/hooks.json`; merged with user/project hooks; no per-hook approval prompt (install warning and workspace-trust rules only) | Yes, `.mcp.json` or `mcpServers`; stdio/http/sse/ws | Yes, `skills/` (namespaced `/ai-memory:<skill>`) | `userConfig`; `sensitive: true` goes to the OS credential store; `${user_config.KEY}` in MCP config and exec-form hook args; `CLAUDE_PLUGIN_OPTION_<KEY>` in hook env (local: only when set) | `version` pins users; third-party marketplaces have background auto-update **off** by default; `claude plugin update` | Own marketplace (`/plugin marketplace add owner/repo`), directory portal (`<name>@synced`), community marketplace | Directory portal needs a paid plan, README, license, no credentials in files; `claude-plugins-official` takes no portal submissions |
| **Claude Cowork** | Yes (loads) | Local stdio only "when the Cowork session runs on your computer"; remote listed as a connector | Yes | `${user_config.*}` in MCP "Ignored when a referenced option has no default; Cowork doesn't prompt" | Account-synced | Customize > Plugins, zip upload | Same as above; top-level `bin/` makes the plugin uninstallable |
| **Claude Chat tab** | Ignored | Remote only | Yes | Remote URL with `${user_config.*}` ignored | Account-synced | Customize > Plugins | Same |
| **Codex** (CLI + desktop) | Yes, `hooks/hooks.json` or `extensions."com.openai".hooks`; per-hook hash trust: "Installing or enabling a plugin doesn't automatically trust its hooks" | Yes, portable `mcp.json` (`type` required) or legacy `.mcp.json` | Yes | MCP auth types only; no documented per-plugin user settings | `codex plugin marketplace upgrade`; no documented auto-update | `.agents/plugins/marketplace.json` in a repo or `~/.agents/plugins/`; legacy `.claude-plugin/marketplace.json` | Public directory: "Plugins containing lifecycle hooks aren't eligible"; MCP must be public HTTPS |
| **Cursor** | Only in the Cursor Plugin format (`.cursor-plugin/plugin.json` + `hooks/hooks.json`); an Agent Plugin (root `plugin.json`) carries skills and MCP only | Yes, `mcp.json`; expands `${CURSOR_PLUGIN_ROOT}` and `${CLAUDE_PLUGIN_ROOT}` (not `${PLUGIN_ROOT}`) in MCP fields | Yes | `variables` schema, values entered in the dashboard, used as `${VAR}` in `mcp.json` | GitHub import auto-refresh (≤10 min) or manual Refresh | cursor.com/marketplace, team marketplaces, `.cursor-plugin/marketplace.json` at a repo root | "Every plugin is manually reviewed before it's listed"; no documented approval step for plugin hooks |
| **Gemini CLI** | Yes, `hooks/hooks.json` in the extension (timeouts in ms) | Yes, `gemini-extension.json` `mcpServers` | Yes | `settings[]` with `envVar`; `sensitive: true` uses the system keychain | Git HEAD or release tag; `gemini extensions update` | `gemini extensions install <git url>`; gallery crawls repos with topic `gemini-cli-extension` and a root manifest | Automated validation. Note: "Gemini CLI was replaced by Antigravity CLI on June 18th, 2026" for unpaid and Google One users |
| **Antigravity** | Yes, root `hooks.json`, documented for the CLI only; no SessionEnd or prompt event | Yes, `mcp_config.json` (`serverUrl`, not `url`) | Yes | Not documented | "automatically updated" (2.0) | `agy plugin install <name>@antigravity-plugins-official`, GitHub URL, local dir | Not documented; no plugin-root variable documented |
| **Copilot CLI** | Yes | Yes | Yes | Not documented | `copilot plugin update` | marketplace repos; also reads `.claude-plugin/plugin.json` and `.claude-plugin/marketplace.json` as fallbacks | Not documented |
| **Factory Droid** | Yes, Claude-style `hooks/hooks.json`, `${DROID_PLUGIN_ROOT}` or `${CLAUDE_PLUGIN_ROOT}` | Yes, `mcp.json` (no secrets) | Yes | Not documented | `droid plugin marketplace update` | Reads `.factory-plugin/marketplace.json`, falls back to `.claude-plugin/marketplace.json` | Not documented |
| **OpenCode** | Code-level plugins (JS/TS events), not a hooks file | From config, not the plugin | — | Config | npm via Bun at startup | npm or local | None. ai-memory already generates this plugin (`install-hooks --agent opencode`) |
| **Kimi Code** | No (plugins are tool bundles) | No | — | `config_file`/`inject` | Reinstall | Local, zip, git | None |
| **Zed** | No | MCP extensions, "We plan to deprecate MCP server extensions in favor of the official MCP registry" | — | — | Registry | Extension registry | Registry PR |

Corrections to the table in RFC #1166:

- **Codex.** Plugins with hooks cannot go in the public directory, so a
  capturing Codex plugin ships through our own marketplace file. Installing
  it still leaves every hook untrusted until the user approves it in `/hooks`.
- **Cursor.** Hooks require the `.cursor-plugin/` format. An Agent Plugin, or a
  `cursor://` MCP deeplink, adds no hooks and so does not trigger the #721
  de-duplication.
- **Antigravity** can bundle MCP (`mcp_config.json`).
- **Gemini CLI** has been replaced by Antigravity CLI for most free users, so
  it ranks low.
- **Claude.** The anthropic-official listing is out of reach in practice.
  Besides having no submission route, its review prompt
  (`anthropics/claude-plugins-official/.github/policy/prompt.md`, read
  2026-10-13) fails any plugin whose `UserPromptSubmit`, `PreToolUse` or
  `PostToolUse` hook "runs without a project-relevance gate". Capture hooks
  are ungated by design. The self-hosted marketplace is the channel; the
  directory portal is optional (see open questions).

## Design

### 1. What the plugin is, and what it is not

- **Manifests, skills, one launcher script.** No Node or Python, no
  compiled binary, no first-run download. This keeps the Rust-only core rule,
  passes the Claude directory checklist's "binary that isn't an image or
  font" hold, and avoids the patterns that made other memory plugins fragile
  (claude-mem's Bun bootstrap, cognee's 120 s blocking venv install,
  hindsight's unpinned `uvx …@latest`).
- **No top-level `bin/`.** Claude refuses to install such a plugin in Chat and
  Cowork. The launcher lives in `scripts/`.
- **The binary is installed once, separately** (install.sh, Homebrew, AUR,
  RPM, Docker wrapper, the macOS app). The plugin assumes it and says so
  plainly when it is missing.
- **The native hook is the only capture path.** The plugin never ships or
  calls `hooks/<agent>/*.sh`. Those scripts POST with curl and sit outside
  capture-policy v1 (row 11h in `security-boundaries.md`). If the plugin
  fell back to them when the binary is missing, `ignore_paths` and allowlist
  mode would be offered and silently not enforced. Without the binary the
  plugin captures nothing.

### 2. The launcher

One POSIX `sh` script, `scripts/ai-memory-hook`, shared by every plugin and
called as `ai-memory-hook <agent> <event>`. It:

1. picks the server URL: the plugin option (`CLAUDE_PLUGIN_OPTION_SERVER_URL`
   on Claude), else `AI_MEMORY_HOOK_URL`, else `http://127.0.0.1:49374`. It
   cannot rely on `userConfig` defaults, which hooks do not receive (local
   finding 5);
2. finds the binary: the `ai_memory_bin` option if set, else `command -v
   ai-memory`, else the install locations a GUI-launched app may not have on
   `PATH` (`$HOME/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`,
   `/usr/bin`);
3. if nothing is found: reads and discards stdin, prints
   `{"systemMessage": "ai-memory: the ai-memory binary was not found, so this
   session is not captured. Install it and run ai-memory setup: <url>"}` on
   `session-start` and `{}` on every other event, and exits 0. This is the
   pattern Anthropic's own `claude-security` plugin uses for a missing
   Python;
4. otherwise runs `exec "$bin" hook --event <event> --agent <agent>
   --server-url <url> [--data-dir <dir>]` with `AI_MEMORY_HOOK_ORIGIN=<agent>-plugin`
   in the environment. The subcommand comes first so the Docker wrapper can
   intercept it.

The prototype took about 2 ms per call on the missing-binary path. The script
is short enough to read in full, which matters because plugin hooks run with
the user's full permissions.

Why a script and not exec form straight to `ai-memory`:

- exec form gives a raw "Executable not found" error on every tool call when
  the binary is missing (local finding 8);
- it cannot reach binaries that are off a GUI app's `PATH`;
- exec form with `${user_config.server_url}` fails outright when the option
  is unset (finding 5).

The script's cost is Windows. Claude Code runs shell-form hooks through Git
Bash on Windows, so the launcher works there when Git Bash is installed, and
fails without it (Claude then falls back to PowerShell). Codex has a
per-hook `commandWindows` override that can call `ai-memory.exe` directly.
See open question 4.

### 3. Changes to the binary (small, all in existing modules)

- **`hook`: origin-aware de-duplication.** Claude runs a plugin's hook and an
  identical settings-file hook separately ("A plugin's or skill's copy of the
  same handler stays separate"). A user with `install-hooks` already applied
  who adds the plugin would be captured twice. When
  `AI_MEMORY_HOOK_ORIGIN=claude-plugin` (read through the injectable lookup,
  invariant 1) and the Claude settings files already carry ai-memory hooks
  (`settings_file_carries_ai_memory_hooks` on the user file, honouring
  `CLAUDE_CONFIG_DIR`, and the repository's `.claude/settings.local.json`),
  the plugin copy is dropped and prints `{}`. This mirrors
  `is_redundant_cursor_copy` (#721). Settings hooks win because they carry
  flags the plugin does not (`--capture-assistant`, a baked
  `--project-strategy`). An env variable rather than a flag keeps an older
  binary working with a newer plugin; it just skips the de-duplication.
- **`hook` and `mcp-bridge`: an empty `--server-url` means unset.** This lets
  `.mcp.json` pass `${user_config.server_url}` while a user who has a
  configured `server_url` keeps it.
- **Docker wrapper: route `hook` to the native host client**, as it already
  does for `mcp-bridge` (local finding 9). Without this, a Docker-wrapper
  user's plugin hooks run in a container, over budget and pointed at the
  wrong loopback.
- **`setup`, `install-hooks`, `install-skills`:** detect an enabled
  ai-memory Claude plugin (`installed_plugins.json` plus `enabledPlugins` in
  the user settings) and stop writing the parallel Claude settings hooks,
  skills and MCP entry, printing what was skipped. `--force` keeps today's
  behaviour. `setup --server-url` writes the plugin's options through
  `claude plugin configure ai-memory@ai-memory --values-stdin` instead of
  editing Claude's files.
- **`doctor`:** report the plugin (version, enabled, options), version skew
  against the binary, duplicate wiring (plugin plus settings hooks, plugin
  skills plus installed skills), and a Docker wrapper on `PATH` without the
  routing fix. For Codex, extend `codex_hook_trust.rs` to plugin hooks.
- **`upgrade`:** when the plugin is installed, run `claude plugin
  marketplace update ai-memory` and `claude plugin update
  ai-memory@ai-memory` after replacing the binary, because background
  auto-update is off for third-party marketplaces. This keeps binary and
  plugin in step without asking users to opt into auto-update.
- **`uninstall`:** offer `claude plugin uninstall ai-memory@ai-memory`.

None of this touches the MCP tool surface, so the tool count stays at 23.

### 4. Configuration surface

Zero required settings. The defaults reproduce a local `ai-memory setup`
install.

| Knob | Claude plugin | Default | Notes |
| --- | --- | --- | --- |
| Server URL | `userConfig.server_url` (string) | `http://127.0.0.1:49374` | Read by the launcher from `CLAUDE_PLUGIN_OPTION_SERVER_URL`; MCP gets `${user_config.server_url}` |
| Binary path | `userConfig.ai_memory_bin` (string) | `ai-memory` | For installs off `PATH` |
| Data dir | `userConfig.data_dir` (string) | empty | Only for a non-default data dir |
| Token | **none in the plugin** | — | Hooks read `<data_dir>/auth-token`, the OIDC `auth.json` or a server profile; `mcp-bridge` reads the bearer `Config::load` resolves. Same files `setup`, `install-hooks --auth-token` and `auth login` already write |
| Project override, capture mode, `ignore_paths`, briefing, server routing | `.ai-memory.toml` marker, persisted capture mode, `servers.toml` profiles | as today | The native hook already enforces all of these. Nothing is duplicated into plugin settings |

Keeping the token out of the plugin keeps one credential store for hooks, MCP
and the CLI. It also avoids a pattern Anthropic's directory scan flags: an
Arroway source comment records an earlier build being flagged for reading a
token from the environment. A `sensitive` token option is open question 6.

Cowork: an unset option in an MCP entry loads there only if the option has a
default, and Cowork never prompts. Every option above has a default.

### 5. Trust and security

- **Claude.** No per-hook approval for plugin hooks; installing the plugin is
  the trust decision, plus workspace trust in interactive sessions. Plugin
  hooks run outside Claude's sandbox with full user permissions. The README
  and the marketplace description must say exactly what runs and what is
  sent: lifecycle payloads, sanitized by the server, to the configured
  ai-memory server only; no telemetry. `disableAllHooks` disables plugin
  hooks too, and `allowManagedHooksOnly` blocks them unless the plugin is
  force-enabled in managed settings. `doctor` reports both.
- **Codex.** Every plugin hook stays untrusted until approved in `/hooks`,
  and a changed handler is skipped silently. Install docs must say this, and
  `doctor` must detect untrusted or modified plugin hooks. Hook commands are
  kept byte-identical across releases (they reference `${PLUGIN_ROOT}`, never
  a versioned path), so trust can survive upgrades. Whether Codex hashes the
  handler before or after variable expansion is not verified (open question
  2).
- **No secrets in plugin files.** CI greps the plugin tree for bearer-shaped
  strings, and `claude plugin validate` already warns on credential-looking
  header values and on non-loopback `http://` URLs.
- **Capture policy parity.** Every event goes through the native hook, which
  runs capture policy before spool and network (rows 11b, 11g, 11k). The
  launcher has no capture logic of its own.

**Third front door.** Shell hooks, the native client and the generated
TypeScript integrations are the existing three places marker, scope and
capture logic lives, and fixes have had to cover all three (#661, #668). A
plugin is a fourth entry point. It stays safe only if it adds no logic of its
own and the following stays true, each item backed by a test (see
[Tests](#tests)):

1. a plugin-launched event with an `ignore_paths` match never reaches the
   spool;
2. under allowlist capture mode, a repository without a marker emits nothing;
3. marker `workspace`/`project` and `servers.toml` routing apply to
   plugin-launched events exactly as to settings-launched ones;
4. a plugin copy and a settings copy of the same event produce one
   observation, and the plugin copy is the one dropped;
5. with no binary, nothing is sent anywhere and the agent is not blocked.

Implementation adds a row to `docs/security-boundaries.md` for the plugin
front door, citing these tests, in the same change.

### 6. Other harnesses reading our Claude files

Copilot CLI reads `.claude-plugin/plugin.json` and
`.claude-plugin/marketplace.json` as fallbacks, and Droid falls back to
`.claude-plugin/marketplace.json`. Codex also falls back to it when no
`.agents/plugins/marketplace.json` exists. A user who points one of those at
our repository would install the Claude plugin and send events tagged
`claude-code` in a payload dialect the server does not expect. Two guards:

- the launcher recognizes the other harnesses' markers (`DROID_PLUGIN_ROOT`,
  Codex's `PLUGIN_ROOT`, Cursor's `CURSOR_VERSION`). It hands the event to
  the matching ai-memory agent when one exists, and otherwise prints `{}` and
  exits;
- we ship a native catalog for each harness we support, so the fallback is
  never used.

The server's `agent_from_payload` relabelling remains the last line of
defence.

## Repository layout

Recommendation: keep the plugins and the marketplace catalogs **in this
repository**, with catalog entries pinned to the release tag.

```
.claude-plugin/marketplace.json        Claude catalog (also read by Droid)
.agents/plugins/marketplace.json       Codex catalog
.cursor-plugin/marketplace.json        Cursor catalog (phase 3)
plugins/
  README.md                            what each plugin runs and sends
  claude-code/
    .claude-plugin/plugin.json
    hooks/hooks.json
    .mcp.json
    scripts/ai-memory-hook             generated copy of the shared launcher
    skills/<six routing skills>/SKILL.md
    skills/status/SKILL.md
    README.md  LICENSE
  codex/
    plugin.json  mcp.json  hooks/hooks.json  scripts/  skills/
  cursor/
    .cursor-plugin/plugin.json  mcp.json  hooks/hooks.json  scripts/  skills/
```

Each harness gets its own directory: event names, the `--agent` value and
the MCP flags (`--no-session-id` for everyone except Claude) all differ, and
Claude strips manifest keys it does not know while still loading
`hooks/hooks.json`. That rules out a shared hooks file in one directory
(Arroway and claude-mem keep separately named hooks files for the same
reason). Skills are copies generated from the same source, not symlinks;
symlinks break zip uploads and Windows checkouts.

Every file under `plugins/` is **generated** by Rust code from the sources
that already feed `install-hooks` and `install-skills`: `CLAUDE_CODE_EVENTS`,
`CODEX_EVENTS` and `CURSOR_EVENTS` in `render_shared.rs`, and the
`MANAGED_SKILLS` in `ai-memory-core::routing_skills`. A CLI suite test fails
when the committed tree differs from the renderer's output. That is how
plugin installs and CLI installs avoid drifting, the RFC's stated goal. The
renderer is a hidden `ai-memory render-plugins --out <dir>` subcommand with
two callers, the drift test and `bin/release`. A golden-file test with an
environment toggle would also work; the subcommand is easier to run by hand.

**Why in this repository:**

- Plugin files change in the same PR as the Rust code and tests they depend
  on, under the same CI and review.
- Claude (`git-subdir` with `ref`) and Codex (`git-subdir` with `ref` or
  `sha`) can pin each catalog entry to `vX.Y.Z`. Unreleased edits on `main`
  never reach users, and the catalog on `main` always points at the newest
  release. Cognee's Codex catalog already points back into its own repo this
  way.
- `bin/release` stamps every version and `ref` in the release commit, so the
  tag carries a catalog that points at itself. Nothing is pushed after the
  release and no cross-repository token is needed.
- The repository is small enough to serve as a marketplace: 1,011 tracked
  files and a 31 MiB pack, against the directory checklist's limits of
  10,000 files and 50 MiB. Both CLIs offer `--sparse` for marketplace adds.

**Why not a separate `ai-memory-plugins` repository (yet):** it adds a
release step that needs a write token for another repository, a second place
to review, and a drift risk. It becomes worth it for formats that need a
manifest at the repository root: the Gemini gallery (`gemini-extension.json`
at the root), `agy plugin install <github url>`, and possibly a directory
submission whose reviewer clones the whole repository. If that happens,
the release workflow can publish `plugins/<harness>/` into it as generated
output, the same way Arroway's public repo mirrors a private source.

The Cursor catalog cannot pin a ref as far as the docs say. Cursor imports a
repository and refreshes it. Phase 3 has to verify whether a branch can be
chosen (a `plugins-release` branch moved by the release workflow would solve
it) before Cursor ships.

## Manifests

These are the files the renderer would produce for 3.0.0. The Claude ones
passed `claude plugin validate --strict` in the prototype.

### Phase 1: Claude Code

`.claude-plugin/marketplace.json` (repository root):

```json
{
  "name": "ai-memory",
  "owner": { "name": "AkitaOnRails", "url": "https://github.com/akitaonrails" },
  "description": "ai-memory plugins: lifecycle capture and memory tools for coding agents, backed by your own ai-memory server.",
  "plugins": [
    {
      "name": "ai-memory",
      "description": "Long-term memory across sessions and harnesses.",
      "source": {
        "source": "git-subdir",
        "url": "https://github.com/akitaonrails/ai-memory.git",
        "path": "plugins/claude-code",
        "ref": "v3.0.0"
      }
    }
  ]
}
```

`plugins/claude-code/.claude-plugin/plugin.json`:

```json
{
  "name": "ai-memory",
  "displayName": "ai-memory",
  "version": "3.0.0",
  "description": "Long-term memory across sessions and harnesses. Hands each lifecycle event to the installed ai-memory binary, which applies your capture exclusions and sends a sanitized record only to your ai-memory server (default http://127.0.0.1:49374). Adds the ai-memory MCP tools. No telemetry. Requires the ai-memory binary.",
  "author": { "name": "AkitaOnRails", "url": "https://github.com/akitaonrails" },
  "homepage": "https://github.com/akitaonrails/ai-memory",
  "repository": "https://github.com/akitaonrails/ai-memory",
  "license": "MIT",
  "keywords": ["memory", "mcp", "hooks", "handoff", "context"],
  "userConfig": {
    "server_url": {
      "type": "string",
      "title": "ai-memory server URL",
      "description": "Base URL of the ai-memory server. Keep the default for a local install.",
      "default": "http://127.0.0.1:49374"
    },
    "ai_memory_bin": {
      "type": "string",
      "title": "ai-memory executable",
      "description": "Path to the ai-memory binary, only needed when it is not on PATH.",
      "default": "ai-memory"
    },
    "data_dir": {
      "type": "string",
      "title": "ai-memory data directory",
      "description": "Only for an install that uses a non-default data directory.",
      "default": ""
    }
  }
}
```

`plugins/claude-code/hooks/hooks.json`, the same nine events and empty
matchers `install-hooks` writes today:

```json
{
  "description": "ai-memory lifecycle capture through the installed ai-memory binary",
  "hooks": {
    "SessionStart":     [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/ai-memory-hook claude-code session-start" }] }],
    "UserPromptSubmit": [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/ai-memory-hook claude-code user-prompt-submit" }] }],
    "PreToolUse":       [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/ai-memory-hook claude-code pre-tool-use" }] }],
    "PostToolUse":      [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/ai-memory-hook claude-code post-tool-use" }] }],
    "PreCompact":       [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/ai-memory-hook claude-code pre-compact" }] }],
    "Stop":             [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/ai-memory-hook claude-code stop" }] }],
    "SessionEnd":       [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/ai-memory-hook claude-code session-end" }] }],
    "SubagentStart":    [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/ai-memory-hook claude-code subagent-start" }] }],
    "SubagentStop":     [{ "matcher": "", "hooks": [{ "type": "command", "command": "\"${CLAUDE_PLUGIN_ROOT}\"/scripts/ai-memory-hook claude-code subagent-stop" }] }]
  }
}
```

No `timeout` is set, matching `install-hooks`. The native hook returns after
a local spool write. `SessionEnd` has a 1.5 s budget in Claude, and a
plugin's own `timeout` cannot raise it ("Timeouts set on plugin-provided
hooks don't raise the budget"); the native `session-end` already fits it,
because it hands the drain to a detached process. `async: true` on the tool
events is worth measuring later, but it changes how Claude reports output, so
3.0 keeps parity with the settings hooks.

`plugins/claude-code/.mcp.json`:

```json
{
  "mcpServers": {
    "ai-memory": {
      "command": "${user_config.ai_memory_bin}",
      "args": ["mcp-bridge", "--server-url", "${user_config.server_url}"]
    }
  }
}
```

The session-aware bridge (no `--no-session-id`), the same one `install-mcp
--client claude-code` registers; the plugin MCP process receives
`CLAUDE_CODE_SESSION_ID` (local finding 7). No token: the bridge
authenticates with the bearer `Config::load` resolves. `.mcp.json` takes the
binary from an option rather than the launcher because a stdio MCP `command`
must be a real executable on Windows.

`plugins/claude-code/skills/` holds the six managed routing skills plus a
`status` skill with `disable-model-invocation: true`, invoked as
`/ai-memory:status`. It runs `ai-memory doctor` for the plugin's agent and
summarizes the result: binary found, server reachable, duplicate wiring, spool
backlog, and for Codex untrusted hooks. The skill only reports. Fixes
stay with the user.

The six routing skills duplicate what `install-skills` puts in
`~/.claude/skills`, and two copies of each description would sit in context.
`setup` and `install-skills` skip the Claude target when the plugin is
enabled (section 3).

### Phase 2: Codex

Portable layout, which Codex 0.162.1 accepted locally (finding 10).

`.agents/plugins/marketplace.json`:

```json
{
  "name": "ai-memory",
  "interface": { "displayName": "ai-memory" },
  "plugins": [
    {
      "name": "ai-memory",
      "source": {
        "source": "git-subdir",
        "url": "https://github.com/akitaonrails/ai-memory.git",
        "path": "plugins/codex",
        "ref": "v3.0.0"
      },
      "policy": { "installation": "AVAILABLE", "authentication": "ON_INSTALL" },
      "category": "Productivity"
    }
  ]
}
```

`plugins/codex/plugin.json`:

```json
{
  "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
  "name": "ai-memory",
  "version": "3.0.0",
  "description": "Long-term memory across sessions and harnesses, backed by the installed ai-memory binary and your ai-memory server. No telemetry."
}
```

`plugins/codex/mcp.json`:

```json
{
  "$schema": "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
  "mcpServers": {
    "ai-memory": { "type": "stdio", "command": "ai-memory", "args": ["mcp-bridge", "--no-session-id"] }
  }
}
```

`plugins/codex/hooks/hooks.json` (default discovery; seven `CODEX_EVENTS`):

```json
{
  "hooks": {
    "SessionStart": [{ "hooks": [{ "type": "command",
      "command": "sh \"${PLUGIN_ROOT}/scripts/ai-memory-hook\" codex session-start",
      "commandWindows": "ai-memory hook --event session-start --agent codex --server-url http://127.0.0.1:49374" }] }]
  }
}
```

The remaining events follow the same shape. `SessionEnd` stays within Codex's
1 s default (3 s maximum). Open questions for Codex: which `authentication`
policy value suits a plugin with no auth of its own, whether omitting
`matcher` is valid for every event, and whether a hook trusted once stays
trusted after a plugin upgrade (open question 2). The catalog
`ref` is stamped by `bin/release` exactly like Claude's.

### Phase 3: Cursor

`.cursor-plugin/plugin.json` plus Cursor's camelCase events from
`CURSOR_EVENTS` (`sessionStart`, `sessionEnd`, `beforeSubmitPrompt`,
`preToolUse`, `postToolUse`, `postToolUseFailure`, `preCompact`, `stop`):

```json
{ "name": "ai-memory", "version": "3.0.0",
  "description": "Long-term memory across sessions and harnesses, backed by the installed ai-memory binary. No telemetry.",
  "author": { "name": "AkitaOnRails" }, "license": "MIT" }
```

```json
{ "hooks": { "sessionStart": [ { "command": "sh \"${CURSOR_PLUGIN_ROOT}/scripts/ai-memory-hook\" cursor session-start" } ] } }
```

```json
{ "mcpServers": { "ai-memory": { "command": "ai-memory", "args": ["mcp-bridge", "--no-session-id"] } } }
```

Not verified, and must be settled before Cursor ships:

- whether `${CURSOR_PLUGIN_ROOT}` expands in hook commands (the docs only
  promise it for MCP fields);
- what a relative `./scripts/…` resolves against;
- whether the plugin `hooks.json` needs `"version": 1`;
- whether Cursor's third-party import also runs hooks from Claude *plugins*,
  or only from Claude settings files.

The #721 de-duplication has to learn about the Cursor plugin. Today
`cursor_native_hooks_installed` looks only at `~/.cursor/hooks.json`.

### Later: Antigravity, Gemini CLI, Copilot CLI, Droid

- **Antigravity.** It documents no plugin-root variable, so a hook cannot
  find a bundled launcher. Its hook would have to call `ai-memory` from
  `PATH`, which brings back the GUI `PATH` problem. Install by GitHub URL
  probably needs a root manifest. The per-turn `invocationNum` issue
  (verification notes) should be fixed first.
- **Gemini CLI.** Needs a root `gemini-extension.json` for the gallery and for
  install-by-URL. Low priority given the move to Antigravity.
- **Copilot CLI and Droid.** Both read Claude-style layouts and have Claude-style
  hooks. Each needs its own directory and catalog (`.github/plugin/`,
  `.factory-plugin/`) with its own `--agent`. The launcher guard in section 6
  covers the meantime.

## Automation

### On pull requests (`.github/workflows/plugins.yml`, path-filtered)

Paths: `plugins/**`, the three catalog files, `render_shared.rs`,
`ai-memory-core/src/routing_skills/**`, `bin/ai-memory`, the workflow itself.

1. **Drift and versions.** `cargo nextest run -p ai-memory-cli -E
   'test(/plugin/)'`: the committed `plugins/` tree equals the renderer
   output, every manifest version equals `workspace.package.version`, every
   catalog `ref` is `v<that version>`, and the plugin tree has no
   bearer-shaped strings.
2. **Claude validation.** Install a pinned Claude Code CLI (as a CI tool,
   not a build dependency), then `claude plugin validate --strict .` (marketplace)
   and `claude plugin validate --strict plugins/claude-code`.
3. **Launcher tests.** `shellcheck -s sh` plus `tests/hooks/test_plugin_launcher.sh`:
   missing binary (exit 0, `systemMessage` only on session-start, stdin
   drained), option precedence, the Docker wrapper on `PATH`, the
   foreign-harness guard, and a timing bound.
4. **Sandboxed Claude end-to-end.** Build the binary, start `ai-memory serve`
   on a scratch data dir and a free port, then:

   ```bash
   export CLAUDE_CONFIG_DIR="$RUNNER_TEMP/claude"
   mkdir -p "$CLAUDE_CONFIG_DIR"
   # Options for a --plugin-dir plugin live under the id "ai-memory@inline".
   jq -n --arg url "$URL" --arg bin "$PWD/target/debug/ai-memory" --arg dd "$RUNNER_TEMP/data" \
     '{pluginConfigs: {"ai-memory@inline": {options: {server_url: $url, ai_memory_bin: $bin, data_dir: $dd}}}}' \
     > "$CLAUDE_CONFIG_DIR/settings.json"
   cd "$FIXTURE_REPO"
   env -u ANTHROPIC_API_KEY claude -p "hi" --plugin-dir "$GITHUB_WORKSPACE/plugins/claude-code" || true
   ```

   Then assert through `/api/v1` or `ai-memory search` that the server holds
   `session-start`, `user-prompt-submit` and `session-end` observations for
   agent `claude-code` in the project the fixture's marker names. This works
   without credentials (local finding 3). `--plugin-dir` loads the PR's files
   directly; the catalog itself pins a tag that does not exist yet, so a
   second step only runs `claude plugin marketplace add "$GITHUB_WORKSPACE"`
   and checks that the catalog loads. Scrub `AI_MEMORY_*` and managed-run
   variables first: in the prototype run they leaked a `managed_run`
   parameter into the spooled URL.
5. **Front-door parity.** Tool events cannot be driven without a model, so
   policy parity is tested one level down. Pipe recorded Claude payloads
   through the launcher into the real binary and assert that an
   `ignore_paths` match never reaches the spool, that allowlist mode without
   a marker emits nothing, that marker and `servers.toml` routing apply, and
   that a plugin copy is dropped when settings hooks exist. These are the
   adversarial tests the security-boundary row cites.
6. **Codex smoke.** In a scratch `CODEX_HOME`: `codex plugin marketplace add`
   on the checkout, then `codex plugin add ai-memory@ai-memory`, and assert
   the cached plugin root contains the expected files. Optionally, drive `codex
   app-server` with `hooks/list` (the method used in the verification notes)
   to assert the hooks are discovered and reported `untrusted`.
7. **Windows.** Add `plugins/**` to `windows.yml`'s path filter once the
   Windows story is decided.

### On release

- `bin/release <version>` stamps `version` in every plugin manifest and
  catalog entry and sets every catalog `ref` to `v<version>`, in the same
  commit that bumps `Cargo.toml`.
- `release.yml`'s `validate-version` job also checks those values against
  the tag.
- The `github-release` job attaches `ai-memory-claude-plugin.zip` (the
  `plugins/claude-code` tree) and its `.sha256`, for **Customize > Plugins >
  Upload plugin** in the desktop app and Cowork.
- Nothing else is published. The catalogs on `main` point at the new tag as
  soon as the release commit lands. Existing users get the update through
  `ai-memory upgrade` (section 3), `claude plugin update`, or auto-update if
  they turned it on.
- Add "plugin catalogs point at the new tag; `claude plugin validate` passes
  on the tag" to the release checklist next to the Homebrew tap step.

`claude plugin tag` creates `{name}--v{version}` tags. We already tag
`vX.Y.Z`, and our catalogs point at that tag, so it is not needed.

## Tests

Mapped to the boundary inventory, all at integration level:

| Test | Proves | Where |
| --- | --- | --- |
| Committed `plugins/` equals renderer output; versions and refs match the workspace | No drift between CLI and plugin installs | CLI `tests/suite/plugins.rs` |
| Launcher, missing binary | Fail-open, guidance once, nothing sent | `tests/hooks/test_plugin_launcher.sh` |
| `ignore_paths` and allowlist through the launcher | Capture policy parity (front door 1, 2) | CLI suite, real binary, recorded payloads |
| Marker and server-profile routing through the launcher | Scope parity (front door 3) | CLI suite, beside `marker_scope.rs` and `server_profiles.rs` |
| Plugin copy plus settings copy gives one observation; control: plugin alone delivers | De-duplication (front door 4), adversarial with a control | `hook.rs` unit test plus CLI suite |
| Docker wrapper routes `hook` to the native client | Wrapper fix | `scripts/check-native-packaging.sh` |
| Sandboxed Claude install fires hooks into a scratch server | Real wiring, no API key | `plugins.yml` |

## Phased plan for 3.0

Estimates are engineer-days for one person familiar with the codebase.

| Phase | Work | Estimate |
| --- | --- | --- |
| **0. Verification spikes** | Codex: does trust survive a plugin version bump? (two installs, `hooks/list` on each). Claude Desktop Code tab on macOS: what `PATH` do plugin hooks get? Cowork on macOS/Windows: do plugin hooks run on the host and reach `127.0.0.1:49374`? Claude Code on Windows with and without Git Bash. Cursor: the four unknowns above | 1-1.5 |
| **1. Claude Code plugin** | Renderer and hidden `render-plugins`; launcher; `plugins/claude-code`; root catalog; `hook` origin de-duplication; empty `--server-url` handling; Docker wrapper `hook` routing; `setup`/`install-*`/`doctor`/`upgrade`/`uninstall` integration; drift, launcher, parity and de-duplication tests; `docs/install.md` and `docs/mcp-install.md` sections, `plugins/README.md`, CHANGELOG `Added`, security-boundaries row | 4-5 |
| **2. CI and release automation** | `plugins.yml` (validate, launcher, sandboxed e2e, Codex smoke); `bin/release` stamping; `validate-version` extension; release zip asset | 1.5-2 |
| **3. Codex plugin** | `plugins/codex`, `.agents/plugins/marketplace.json`, `commandWindows`, trust guidance in docs, `doctor` plugin-hook trust check | 2 |
| **4. Cursor plugin** (3.0 stretch, else 3.1) | Depends on the phase 0 answers; #721 detection of the Cursor plugin; branch strategy for the Cursor import | 2-3 |
| **Later** | Cowork capture if phase 0 shows hooks run in the VM; Antigravity; Gemini; Copilot CLI and Droid catalogs; a directory submission | — |

Phases 1 to 3 come to roughly 9 to 10.5 days. This is additive work, so it targets
`release/3.0`, not `main`.

## Open questions for the maintainer

1. **Directory listing.** The self-hosted marketplace covers Claude Code and
   (by zip upload) Cowork. Do we also want the claude.ai directory portal,
   which needs a paid plan, a README describing every network call, and
   possibly review of the whole repository? An allowlist-capture variant
   (events only from repositories with an `.ai-memory.toml` marker) would
   arguably satisfy the "project-relevance gate" rule. That would be a
   second plugin with a different default, so it is not proposed for 3.0.
2. **Codex trust across upgrades.** If Codex hashes hook commands after
   expanding `${PLUGIN_ROOT}`, every plugin update needs a fresh `/hooks`
   approval, and an untrusted hook is skipped silently. Is that acceptable
   with `doctor` and `/ai-memory:status` flagging it, or should Codex stay on
   `install-hooks` (one trust per command change) until Codex changes?
3. **Cowork.** If phase 0 shows Cowork hooks run inside the VM, where the
   binary is absent and the host loopback may be unreachable, should the
   plugin say "Cowork is not captured" at session start, or stay silent
   there? The `systemMessage` would otherwise appear on every Cowork session.
   The #878 report that "each message ends and reopens the session" also
   means one session-start (and handoff check) per message.
4. **Windows.** Accept "Git Bash required" for Claude plugin hooks on Windows
   (Claude Code already prefers Git Bash for hooks), or add a PowerShell twin
   of the launcher? A hook entry cannot switch by OS in Claude, though Codex's
   `commandWindows` can.
5. **Which wiring wins.** Proposed: settings hooks win at runtime, and
   `setup` stops writing them once the plugin is enabled. The alternative is
   to make the plugin the default Claude Code install path in `setup` and
   migrate existing settings hooks. That is cleaner long-term but changes
   behaviour for every current user.
6. **Token option.** Add a `sensitive` `auth_token` option, kept in the OS
   credential store, for remote-server users who never run the CLI's auth
   commands? It would need the hook to accept a token from the environment,
   a new input on the trust boundary. Proposed: no for 3.0.
7. **Marketplace and plugin names.** `ai-memory@ai-memory` reads a little
   odd, but both names are free of the reserved prefixes, and changing a
   plugin name later needs a `renames` entry. Keep it?
8. **Folderless sessions** (Code tab scratch workspaces, Cursor's empty
   window) still land in `default/scratch`. The plugin does not change that.
   It stays a separate decision (`desktop/plan.md` phase 2).

## What we borrowed and what we avoided

From the memory tools surveyed (claude-mem, supermemory, mem0, cognee,
hindsight, Arroway, engram) and Anthropic's own plugins:

- **Borrowed:**
  - one dependency-free hook entry point per host;
  - fail open with `{"systemMessage": …}` once when a runtime is missing
    (Anthropic's `claude-security`, `security-guidance`);
  - generated per-host directories with a drift check in CI (mem0's
    `build.py --check`);
  - a clean-profile `marketplace add` + `install --json` smoke test (cognee);
  - a version-consistency check across manifests (cognee, engram);
  - a description that states what is sent and that there is no telemetry.
- **Avoided:**
  - first-run installs inside hooks (claude-mem's Bun bootstrap, cognee's
    120 s blocking venv install, hindsight's unpinned `uvx …@latest`);
  - hooks that scan the plugin cache for the newest version (claude-mem);
  - tokens in plugin-directory or repository-local config files (claude-mem
    Cowork, supermemory);
  - blocking hooks with long timeouts (hindsight's 45 s recall, Arroway's
    700 s `Stop`);
  - default-on identified telemetry (mem0);
  - a top-level `bin/`.

## Appendix: the prototype launcher

The script used in the local runs. The shipped version adds the
foreign-harness guard from section 6, per-harness option variables, and the
`shellcheck` and shell tests listed under Automation.

```sh
#!/bin/sh
# ai-memory plugin hook launcher. Usage: ai-memory-hook <agent> <event>
# Resolves the installed ai-memory binary and hands the event to the native
# `ai-memory hook` command, which owns capture policy, spooling and routing.
# Never blocks the agent: a missing binary drains stdin and exits 0.
agent="$1"
event="$2"
url="${CLAUDE_PLUGIN_OPTION_SERVER_URL:-${AI_MEMORY_HOOK_URL:-http://127.0.0.1:49374}}"
bin="${CLAUDE_PLUGIN_OPTION_AI_MEMORY_BIN:-}"
if [ -z "$bin" ] || [ "$bin" = "ai-memory" ]; then
    bin=""
    if command -v ai-memory >/dev/null 2>&1; then
        bin=$(command -v ai-memory)
    else
        for candidate in "$HOME/.local/bin/ai-memory" /opt/homebrew/bin/ai-memory \
            /usr/local/bin/ai-memory /usr/bin/ai-memory; do
            if [ -x "$candidate" ]; then bin="$candidate"; break; fi
        done
    fi
fi
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
    cat >/dev/null
    if [ "$event" = "session-start" ]; then
        printf '%s\n' '{"systemMessage":"ai-memory: the ai-memory binary was not found, so this session is not captured. Install it and run `ai-memory setup`: https://github.com/akitaonrails/ai-memory#install"}'
    else
        printf '{}\n'
    fi
    exit 0
fi
set -- --event "$event" --agent "$agent" --server-url "$url"
if [ -n "${CLAUDE_PLUGIN_OPTION_DATA_DIR:-}" ]; then
    set -- --data-dir "$CLAUDE_PLUGIN_OPTION_DATA_DIR" "$@"
fi
AI_MEMORY_HOOK_ORIGIN="$agent-plugin" exec "$bin" hook "$@"
```

## Sources

All fetched 2026-10-13.

Claude:
- Plugin manifest reference: <https://code.claude.com/docs/en/plugins/manifest-reference>
- Plugin components (hooks, MCP, `bin/`, data dir): <https://code.claude.com/docs/en/plugins/components>
- Hooks reference (shell vs exec form, Windows shells, SessionEnd budget, workspace trust, deduplication): <https://code.claude.com/docs/en/hooks>
- MCP (plugin servers, variable expansion, `headersHelper`): <https://code.claude.com/docs/en/mcp>
- Marketplace reference (sources, `strict`, versions): <https://code.claude.com/docs/en/plugins/marketplace-reference>
- Host a marketplace (auto-update off by default): <https://code.claude.com/docs/en/plugins/host-marketplace>
- CLI reference (`validate`, `configure`, `tag`, `details`): <https://code.claude.com/docs/en/plugins/cli-reference>
- Loading and local testing: <https://code.claude.com/docs/en/plugins/loading>
- Publish and Anthropic marketplaces: <https://code.claude.com/docs/en/plugins/publish>, <https://code.claude.com/docs/en/plugins/anthropic-marketplaces>
- Plugin security: <https://code.claude.com/docs/en/plugins/security>
- Plugin evals: <https://code.claude.com/docs/en/plugin-evals>
- Settings (`disableAllHooks`, `allowManagedHooksOnly`): <https://code.claude.com/docs/en/settings-reference>
- Platform support (Chat, Cowork, Code): <https://claude.com/docs/plugins/platform-support>
- Pre-submission checklist and submission: <https://claude.com/docs/plugins/pre-submission-checklist>, <https://claude.com/docs/plugins/submit>
- Desktop extensions and Cowork VM note: <https://claude.com/docs/third-party/claude-desktop/extensions>
- Official marketplace review prompt: `anthropics/claude-plugins-official`, `.github/policy/prompt.md`

Codex:
- Build plugins: <https://developers.openai.com/plugins/build/plugins> (same content at <https://developers.openai.com/codex/plugins/build>)
- Hooks and plugin hook trust: <https://learn.chatgpt.com/docs/hooks> (redirect target of <https://developers.openai.com/codex/hooks>)

Cursor:
- Plugins: <https://cursor.com/docs/plugins>
- Plugin reference: <https://cursor.com/docs/reference/plugins>
- Hooks: <https://cursor.com/docs/hooks>
- Third-party hooks: <https://cursor.com/docs/reference/third-party-hooks>

Others:
- Gemini CLI extensions: <https://geminicli.com/docs/extensions/>, <https://geminicli.com/docs/extensions/reference/>, <https://geminicli.com/docs/extensions/releasing/>, <https://geminicli.com/docs/hooks/>
- Antigravity: <https://antigravity.google/docs/plugins.md>, <https://antigravity.google/docs/hooks.md>, <https://antigravity.google/docs/mcp.md>
- Copilot CLI plugin reference: <https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-plugin-reference>
- Factory Droid plugins: <https://docs.factory.com/cli/configuration/plugins.md>
- OpenCode plugins: <https://opencode.ai/docs/plugins/>
- Kimi Code plugins and hooks: <https://moonshotai.github.io/kimi-cli/en/customization/plugins.md>, <https://moonshotai.github.io/kimi-cli/en/customization/hooks.md>
- Agent Plugins 1.0.0: <https://agent-plugins.org/specification>
- Zed MCP extensions: <https://zed.dev/docs/extensions/mcp-extensions>

Example repositories read: `thedotmack/claude-mem`,
`supermemoryai/claude-supermemory`, `mem0ai/mem0` (`integrations/`),
`topoteretes/cognee-integrations`, `vectorize-io/hindsight`,
`oaleviola/arroway-plugin`, `anthropics/claude-plugins-official`,
`anthropics/claude-code` (`plugins/`), `Gentleman-Programming/engram`.
