import { test } from "node:test";
import assert from "node:assert/strict";
import { csrfCookie, scopedQuery, request, serverEndpoint } from "./api.js";

test("scopes encode names separately, including slashes, plus signs and ampersands", () => {
  const q = new URLSearchParams(
    scopedQuery({ workspace_name: "a/b", project_name: "x+y&z" }, 50),
  );
  assert.equal(q.get("workspace"), "a/b");
  assert.equal(q.get("project"), "x+y&z");
  assert.equal(q.get("offset"), "50");
});
test("cookie lookup cannot accept another cookie whose value contains the CSRF name", () => {
  assert.equal(
    csrfCookie("other=ai_memory_csrf=forged; ai_memory_csrf=real"),
    "real",
  );
  assert.equal(csrfCookie("ai_memory_csrf_extra=forged"), "");
});
test("mutations send same-origin session cookies and CSRF under the mount prefix", async () => {
  globalThis.document = {
    cookie: "ai_memory_csrf=proof",
    querySelector: () => ({ content: "/memory" }),
  };
  globalThis.fetch = async (url, options) => {
    assert.equal(url, "/memory/management/v1/test");
    assert.equal(options.credentials, "include");
    assert.equal(options.headers["X-CSRF-Token"], "proof");
    assert.equal(options.body, '{"confirm":true}');
    return { ok: true, json: async () => ({ done: true }) };
  };
  assert.deepEqual(
    await request("/management/v1/test", {
      method: "POST",
      body: { confirm: true },
    }),
    { done: true },
  );
});

test("MCP endpoint uses the same configured mount as API requests", () => {
  globalThis.window = { location: { origin: "https://memory.example" } };
  globalThis.document = { querySelector: () => ({ content: "/team/memory" }) };
  assert.equal(
    serverEndpoint("/mcp"),
    "https://memory.example/team/memory/mcp",
  );
  globalThis.document.querySelector = () => null;
  assert.equal(serverEndpoint("/mcp"), "https://memory.example/mcp");
});
