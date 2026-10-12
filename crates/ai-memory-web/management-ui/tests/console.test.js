import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile, access } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawn } from "node:child_process";
import { createServer } from "node:net";
import { randomUUID } from "node:crypto";
import { chromium } from "playwright";

const binary =
  process.env.AI_MEMORY_TEST_BINARY ||
  resolve(import.meta.dirname, "../../../../target/debug/ai-memory");
async function freePort() {
  const server = createServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}
async function exit(child) {
  if (child.exitCode !== null || !child.pid) return;
  const ended = new Promise((resolve) => child.once("exit", resolve));
  child.kill("SIGTERM");
  const timer = setTimeout(() => child.kill("SIGKILL"), 12_000);
  await ended;
  clearTimeout(timer);
}

test(
  "real console: root creates a user; user changes password, creates an MCP key and deletes an owned session",
  { timeout: 90_000 },
  async () => {
    await access(binary);
    const dir = await mkdtemp(resolve(tmpdir(), "ai-memory-console-"));
    const port = await freePort();
    const base = `http://127.0.0.1:${port}/memory`;
    const env = Object.fromEntries(
      Object.entries(process.env).filter(
        ([key]) => !key.startsWith("AI_MEMORY_"),
      ),
    );
    Object.assign(env, {
      AI_MEMORY_AUTH_TOKEN: "console-test-root-bearer",
      AI_MEMORY_AUTH__ROOT_USERNAME: "root",
      AI_MEMORY_AUTH__INITIAL_ROOT_PASSWORD: "console-root-password",
      AI_MEMORY_AUTH__TOKEN_PEPPER: "console-test-pepper",
      AI_MEMORY_AUTH__SECURE_COOKIE: "false",
    });
    let server, browser;
    const config = resolve(dir, "config.toml");
    await writeFile(config, "[auth]\nsecure_cookie = false\n");
    try {
      server = spawn(
        binary,
        [
          "--config",
          config,
          "--data-dir",
          dir,
          "serve",
          "--transport",
          "http",
          "--bind",
          `127.0.0.1:${port}`,
          "--base-path",
          "/memory",
          "--enable-management-ui",
          "--no-watcher",
        ],
        { env, stdio: ["ignore", "pipe", "pipe"] },
      );
      let diagnostics = "";
      server.stderr.on("data", (chunk) => {
        diagnostics = (diagnostics + chunk).slice(-8000);
      });
      server.stdout.resume();
      for (let attempt = 0; attempt < 200; attempt++) {
        if (server.exitCode !== null)
          throw new Error(`Server failed to start: ${diagnostics}`);
        const ready = await fetch(`${base}/healthz`)
          .then((response) => response.ok)
          .catch(() => false);
        if (ready) break;
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
      browser = await chromium.launch({ headless: true });
      const admin = await browser.newPage({ colorScheme: "dark" });
      const errors = [];
      admin.on("pageerror", (error) => errors.push(error.message));
      await admin.goto(`${base}/manage`);
      const theme = admin.getByRole("group", { name: "Color theme" });
      const scheme = () =>
        admin.evaluate(
          () => getComputedStyle(document.documentElement).colorScheme,
        );
      assert.equal(
        await theme
          .getByRole("button", { name: "System", exact: true })
          .getAttribute("aria-pressed"),
        "true",
      );
      assert.equal(await scheme(), "dark");
      await admin.emulateMedia({ colorScheme: "light" });
      assert.equal(await scheme(), "light");
      await theme.getByRole("button", { name: "Dark", exact: true }).click();
      assert.equal(await scheme(), "dark");
      await admin.reload();
      assert.equal(await scheme(), "dark", "explicit theme survives a reload");
      await theme.getByRole("button", { name: "Light", exact: true }).click();
      await admin.emulateMedia({ colorScheme: "dark" });
      assert.equal(
        await scheme(),
        "light",
        "explicit light overrides the system",
      );
      await theme.getByRole("button", { name: "System", exact: true }).click();
      assert.equal(await scheme(), "dark");
      await admin.reload();
      assert.equal(
        await theme
          .getByRole("button", { name: "System", exact: true })
          .getAttribute("aria-pressed"),
        "true",
      );
      await admin.getByLabel("Username", { exact: true }).fill("root");
      await admin
        .getByLabel("Password", { exact: true })
        .fill("console-root-password");
      await admin.getByRole("button", { name: "Sign in", exact: true }).click();
      await admin.getByLabel("Current password").fill("console-root-password");
      const newPasswordInput = admin.getByLabel(
        "New password (at least 12 characters)",
      );
      await newPasswordInput.fill("short");
      assert.equal(
        await newPasswordInput.evaluate((input) => input.checkValidity()),
        false,
      );
      await newPasswordInput.fill("😊".repeat(6));
      assert.equal(
        await newPasswordInput.evaluate((input) => input.checkValidity()),
        false,
        "six surrogate pairs are six characters, not twelve",
      );
      await admin
        .getByLabel("New password (at least 12 characters)")
        .fill("console-root-changed-password");
      await admin
        .getByLabel("Confirm new password")
        .fill("console-root-changed-password");
      await admin
        .getByRole("button", { name: "Change password", exact: true })
        .click();
      await admin.getByRole("button", { name: "Users", exact: true }).click();
      await admin.getByLabel("Username", { exact: true }).fill("alice");
      await admin.getByLabel("Display name").fill("Alice");
      await admin.getByRole("button", { name: "Add user" }).click();
      const dialog = admin.getByRole("dialog");
      await dialog.waitFor();
      const temporaryPassword = await dialog.locator("pre").innerText();
      await dialog.getByRole("button", { name: "I saved it" }).click();
      await admin.getByRole("row").filter({ hasText: "alice" }).waitFor();
      const user = await browser.newPage();
      user.on("pageerror", (error) => errors.push(error.message));
      await user.goto(`${base}/manage/`);
      await user.getByLabel("Username", { exact: true }).fill("alice");
      await user
        .getByLabel("Password", { exact: true })
        .fill(temporaryPassword);
      await user.getByRole("button", { name: "Sign in", exact: true }).click();
      await user.getByLabel("Current password").fill(temporaryPassword);
      await user
        .getByLabel("New password (at least 12 characters)")
        .fill("alice-browser-password");
      await user
        .getByLabel("Confirm new password")
        .fill("alice-browser-password");
      await user
        .getByRole("button", { name: "Change password", exact: true })
        .click();
      await user
        .getByRole("button", { name: "MCP connection", exact: true })
        .click();
      assert.equal(
        await user.getByRole("button", { name: "Users", exact: true }).count(),
        0,
      );
      await user.getByLabel("Key label").fill("test-agent");
      await user.getByRole("button", { name: "Create API key" }).click();
      const keyDialog = user.getByRole("dialog");
      await keyDialog.waitFor();
      const key = await keyDialog.locator("pre").innerText();
      assert.match(key, /^aim_/);
      await keyDialog.getByRole("button", { name: "I saved it" }).click();
      const identity = await fetch(`${base}/identity`, {
        headers: {
          authorization: `Bearer ${key}`,
          "x-ai-memory-client": "console-test",
        },
      });
      assert.equal(identity.status, 200);
      assert.equal((await identity.json()).operator, "user:alice");
      const sid = randomUUID();
      const hook = async (event, extra = {}) => {
        const response = await fetch(
          `${base}/hook?event=${event}&agent=codex&workspace=default&project=browser-demo`,
          {
            method: "POST",
            headers: {
              authorization: `Bearer ${key}`,
              "content-type": "application/json",
              "x-ai-memory-client": "console-test",
            },
            body: JSON.stringify({ session_id: sid, ...extra }),
          },
        );
        assert.equal(response.status, 202);
      };
      await hook("session-start");
      await hook("user-prompt", {
        prompt: "A browser test session with an owned prompt.",
      });
      await hook("session-end");
      await user.getByRole("button", { name: "Sessions", exact: true }).click();
      await user
        .getByLabel("Project", { exact: true })
        .selectOption({ label: "default / browser-demo" });
      const row = user.getByRole("row").filter({ hasText: sid.slice(0, 8) });
      await row.waitFor();
      await row.getByRole("button", { name: "Inspect" }).click();
      await user
        .getByRole("dialog")
        .getByText("A browser test session with an owned prompt.", {
          exact: true,
        })
        .waitFor();
      await user
        .getByRole("dialog")
        .getByRole("button", { name: "Close dialog" })
        .click();
      await row.getByRole("button", { name: "Delete", exact: true }).click();
      await user.getByRole("dialog").getByRole("checkbox").check();
      await user
        .getByRole("dialog")
        .getByRole("button", { name: "Delete session", exact: true })
        .click();
      await user
        .getByRole("status")
        .filter({ hasText: "Session deleted." })
        .waitFor();
      assert.equal(await row.count(), 0);
      assert.deepEqual(errors, []);
      // The same feature-enabled binary must expose no console unless explicitly enabled.
      await browser.close();
      browser = null;
      await exit(server);
      server = spawn(
        binary,
        [
          "--config",
          config,
          "--data-dir",
          dir,
          "serve",
          "--transport",
          "http",
          "--bind",
          `127.0.0.1:${port}`,
          "--base-path",
          "/memory",
          "--no-watcher",
        ],
        { env, stdio: "ignore" },
      );
      for (let attempt = 0; attempt < 100; attempt++) {
        if (
          await fetch(`${base}/healthz`)
            .then((r) => r.ok)
            .catch(() => false)
        )
          break;
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
      assert.equal((await fetch(`${base}/manage`)).status, 404);
      assert.equal((await fetch(`${base}/management/v1/projects`)).status, 404);
    } finally {
      if (browser) await browser.close();
      if (server) await exit(server);
      await rm(dir, { recursive: true, force: true });
    }
  },
);
