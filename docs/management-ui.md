# Management console

The optional React/Tailwind console gives a shared server's operators a browser
interface for user accounts, captured sessions, and MCP API keys. It reuses the
engine's human authentication and SQLite storage. It does not introduce another
identity provider, credential format, or database.

## Enable it

Build the console into the binary, then enable it when starting the HTTP server:

```bash
cargo build --locked --release -p ai-memory-cli --features management-ui
./target/release/ai-memory serve --transport http --enable-management-ui
```

The equivalent runtime setting is `AI_MEMORY_ENABLE_MANAGEMENT_UI=true`.
The console follows the [ai-memory website](https://aimemorybr.netlify.app/)
visual style: light and dark surfaces, amber controls, and locally served Instrument Sans,
Bricolage Grotesque, and Commit Mono fonts. Font notices are bundled under
`management-ui/licenses/` and served at `/manage/assets/font-licenses.txt`.
The theme follows your system by default. The header offers System, Light, and Dark;
explicit choices are saved in your browser.
The password form requires at least 12 characters, counting Unicode code points;
the existing engine password policy and authentication endpoints are unchanged.

Both switches are off by default. A binary built without `management-ui` has
neither the console assets nor its management endpoints. A feature-enabled
binary with the runtime switch off exposes neither surface. The existing CLI,
MCP tools, auth endpoints, and wiki browser keep their existing behavior.

Open `/manage` on the server. With `AI_MEMORY_BASE_PATH=/memory`, open
`/memory/manage`. The console has its own mount and does not require
`--enable-web` or `--enable-api`. If the wiki browser is also enabled, its
`--web-slug` must differ from `/manage`.

For a Docker source build:

```bash
docker build -f docker/Dockerfile \
  --build-arg AI_MEMORY_FEATURES=management-ui -t ai-memory-management .
```

Set `AI_MEMORY_ENABLE_MANAGEMENT_UI=true` when running that image. The standard
release image is unchanged; it does not include this optional feature.

## Authentication and roles

Use the existing [human password bootstrap and recovery](install.md#human-password-bootstrap-and-recovery)
to create the initial root operator. Password verification uses the engine's
RustCrypto Argon2id implementation. Browser sessions and CSRF hashes live in the
same SQLite database as user identities and native API credentials.

The UI labels the existing `root` role **Admin** and the existing `user` role
**User**. There is no third role or change to machine-key authority.

- **Admins** add users, reset passwords, change roles, disable or enable
  accounts, issue and revoke users' API keys, inspect sessions, and purge any
  session. The existing last-enabled-root safeguard remains in force.
- **Users** inspect their own and shared legacy sessions, delete sessions with
  their exact recorded owner, change their password, and create or revoke their
  own MCP API keys. They cannot reach the admin operations.

New users receive a temporary password shown once and must change it before
using the console. New API keys are also shown once. Neither is saved to browser
storage. Disabling an account revokes browser access; its existing machine keys
remain active until revoked separately, as in the existing CLI.

Browser cookies cannot authenticate MCP or lifecycle hooks. Configure each agent
with the remote `/mcp` endpoint and a personal `aim_` API key from **MCP
connection**. The same user can use that key for lifecycle hooks. Native keys
remain User-level even when their owner is an Admin.

The CLI already provides the corresponding administration commands:
`ai-memory user add-human/list/reset-password/disable/enable/patch` and
`ai-memory api-key add/list/rotate/revoke`. The console uses their existing
backend operations.

## Public deployment

Use the project's [HTTPS reverse-proxy deployment](https-via-proxy.md).
A VPN is optional. Terminate HTTPS at Caddy or another supported proxy, keep the
engine's port reachable only by that proxy, configure `AI_MEMORY_ALLOWED_HOSTS`
for the public hostname, and set `AI_MEMORY_AUTH__SECURE_COOKIE=true`.

Keep the existing root bearer for machine administration; use personal API keys
for remote agents. Do not expose an anonymous engine or use
`--allow-insecure-no-auth`. The console does not add TLS termination or change
the server's existing public-bind safeguards.

Browser mutations require both a valid session and CSRF token. Console data
responses use `Cache-Control: private, no-store`. The static shell contains no
user data and refuses framing. Project access is checked through the existing
scope resolver; pages remain shared within an authorized project.

## Session deletion

The console previews deletion counts and requires an explicit confirmation.
A preview never writes files, tombstones, or audit events. Confirmation runs the
existing wiki mutation path, admission webhooks, and pre/post git checkpoints.

A session purge removes its captured observations, authored handoffs, generated
session-page versions, and associated review records. It preserves maintained
knowledge pages and later manual versions of a session page. Accepted handoffs
belong to their original author and are preserved. Git history, backups, and
native harness transcripts can still contain earlier content; this operation
is not an erasure guarantee.

A regular user's purge checks exact ownership and current project write access
again in the SQLite deletion transaction. Ownerless legacy sessions are shared
for reading but require an Admin to delete. A session whose deletion would
cascade into another project also requires an Admin; the preview reports that
collateral effect. The existing purge tombstone prevents hook replay from
recreating a deleted session.

## Management endpoints

All `/management/v1/*` routes require a human browser session. Machine bearers
are refused on this surface, including the root bearer. Password-change-required
sessions cannot use it. Mutations also require the existing CSRF cookie/header
pair. Existing `/admin/*` routes retain their existing dual-auth and root gates.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/management/v1/projects` | List readable projects. |
| GET | `/management/v1/sessions?workspace=…&project=…&offset=…` | List up to 50 visible sessions; Admins see all owners. |
| GET | `/management/v1/sessions/{id}/observations?workspace=…&project=…&offset=…` | Read up to 50 scoped events after an owner check. |
| POST | `/management/v1/sessions/{id}/purge` | Preview with `dry_run: true`, or delete with `confirm: true`; body includes `workspace` and `project`. |
| GET / POST | `/management/v1/credentials` | List own keys / create an own key with a `label`. |
| POST | `/management/v1/credentials/{id}/revoke` | Revoke an own key. |

## Frontend development and checks

Sources and vendored production assets live in
`crates/ai-memory-web/management-ui`. Cargo embeds the production assets only
when the feature is enabled; ordinary Rust builds do not need Node or npm.

```bash
npm ci --prefix crates/ai-memory-web/management-ui --ignore-scripts
npm run build --prefix crates/ai-memory-web/management-ui
npm test --prefix crates/ai-memory-web/management-ui
npm run format:check --prefix crates/ai-memory-web/management-ui
cargo t --features ai-memory-cli/management-ui
```

Commit refreshed `dist/` assets with source changes. CI rebuilds them and checks
for drift, runs the ownership/security tests, and drives the real console in
Chromium against a temporary feature-enabled server:

```bash
cargo build --locked -p ai-memory-cli --features management-ui
npm exec --prefix crates/ai-memory-web/management-ui -- playwright install chromium
npm run test:e2e --prefix crates/ai-memory-web/management-ui
```

The browser test covers a root-created user, forced password change, personal
MCP-key issuance, authenticated machine identity, hook capture, session
inspection/deletion, subpath hosting, and absence of console routes when the
runtime switch is off. It creates and removes its own temporary data directory.
