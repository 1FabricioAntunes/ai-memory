# Self-service MCP API keys

Build with `--features self-service-keys` and start the HTTP server with
`--enable-self-service-keys` (`AI_MEMORY_ENABLE_SELF_SERVICE_KEYS=true`). Both
switches default to off. Existing human browser sessions may GET or POST
`/management/v1/credentials` and POST `/management/v1/credentials/{id}/revoke`.
Machine bearers and password-change-required sessions cannot use these routes;
mutations require the existing CSRF cookie/header pair.

POST a `label` to create a key for the authenticated immutable user id. The
server returns the `aim_` key once and stores only its peppered hash. Configure
`[auth] token_pepper` as for the existing native-key administration commands.
Revocation refuses another user's credential id. Listing returns metadata and
previews, never token material. Responses use `Cache-Control: private, no-store`.

Use the key as an Authorization: Bearer credential for `/mcp` and lifecycle
hooks. It remains User-level even when its owner is an Admin. Disabling browser
access does not revoke machine keys; revoke them separately. No identity
provider, password implementation, database, or schema is added.

Run `cargo t -p ai-memory-mcp --features self-service-keys` for the adversarial
anonymous, machine, CSRF, pending-password, foreign-id, and legitimate owner tests.
