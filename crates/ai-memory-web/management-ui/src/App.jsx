import { useEffect, useState } from "react";
import { request } from "./api.js";
import { Login, Password } from "./auth.jsx";
import { ThemePicker } from "./theme.jsx";
import { Sessions } from "./Sessions.jsx";
import { Users } from "./Users.jsx";
import { Keys } from "./Keys.jsx";
import { Modal, primary, secondary } from "./ui.jsx";
import logoUrl from "./assets/logo.webp";

export function App() {
  const [me, setMe] = useState(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [pending, setPending] = useState(0);
  const busy = pending > 0;
  const [tab, setTab] = useState("Sessions");
  const [secret, setSecret] = useState(null);
  const updateIdentity = (identity) => {
    setMe(identity);
    setTab("Sessions");
    setSecret(null);
  };
  const refresh = async () => {
    try {
      const data = await request("/auth/me");
      updateIdentity(data.username ? data : null);
    } catch (e) {
      if (e.status !== 401) setError(e.message);
      updateIdentity(null);
    } finally {
      setLoading(false);
    }
  };
  useEffect(() => {
    refresh();
  }, []);
  const run = async (fn) => {
    setPending((count) => count + 1);
    setError("");
    try {
      await fn();
    } catch (e) {
      setError(e.message);
      if (e.status === 401) {
        updateIdentity(null);
      }
    } finally {
      setPending((count) => count - 1);
    }
  };
  const admin = me?.capabilities?.user_management === true;
  return (
    <div className="console-shell min-h-screen">
      <header className="console-header border-b border-line bg-surface/70">
        <div className="mx-auto flex max-w-6xl flex-wrap items-center justify-between gap-4 px-6 py-5">
          <a href="./" className="brand flex items-center gap-3">
            <img src={logoUrl} alt="" className="h-10 w-10" />
            <span className="brand-name">ai-memory</span>{" "}
            <span className="brand-section ml-3 text-sm font-normal text-muted">
              Management
            </span>
          </a>
          <div className="flex flex-wrap items-center gap-4">
            <ThemePicker />
            {me && (
              <div className="flex items-center gap-4 text-sm">
                <span>
                  {me.name || me.username}
                  <span className="role-badge ml-3 rounded-full border border-line bg-tint px-3 py-1 text-muted">
                    {admin ? "Admin" : "User"}
                  </span>
                </span>
                <button
                  className={secondary}
                  disabled={busy}
                  onClick={() =>
                    run(async () => {
                      await request("/auth/logout", { method: "POST" });
                      updateIdentity(null);
                    })
                  }
                >
                  Sign out
                </button>
              </div>
            )}
          </div>
        </div>
      </header>
      <main
        className={`console-main mx-auto max-w-6xl px-6 py-8 ${!me ? "signed-out" : ""}`}
      >
        {error && (
          <div
            role="alert"
            className="mb-6 rounded-lg border border-danger/25 bg-danger-soft p-4 text-danger"
          >
            {error}
          </div>
        )}
        {loading ? (
          <p role="status">Loading…</p>
        ) : !me ? (
          <Login run={run} refresh={refresh} busy={busy} />
        ) : me.must_change_password ? (
          <Password run={run} refresh={refresh} busy={busy} required />
        ) : (
          <>
            <nav
              aria-label="Management"
              className="console-nav mb-8 flex flex-wrap gap-2"
            >
              {[
                "Sessions",
                ...(admin ? ["Users"] : []),
                "MCP connection",
                "Password",
              ].map((item) => (
                <button
                  key={item}
                  className={`nav-tab ${tab === item ? "is-active" : ""}`}
                  aria-current={tab === item ? "page" : undefined}
                  onClick={() => {
                    setTab(item);
                    setError("");
                    setSecret(null);
                  }}
                >
                  {item}
                </button>
              ))}
            </nav>
            {tab === "Sessions" && (
              <Sessions key={me.username} me={me} run={run} busy={busy} />
            )}
            {tab === "Users" && admin && (
              <Users
                me={me}
                refresh={refresh}
                run={run}
                busy={busy}
                setSecret={setSecret}
              />
            )}
            {tab === "MCP connection" && (
              <Keys run={run} busy={busy} setSecret={setSecret} />
            )}
            {tab === "Password" && (
              <Password run={run} refresh={refresh} busy={busy} />
            )}
          </>
        )}
      </main>
      {secret && (
        <Modal title={secret.title} onClose={() => setSecret(null)}>
          <p className="mb-4 text-sm text-muted">
            Shown once. Save it now; closing this dialog clears it from the
            console.
          </p>
          <pre className="overflow-auto rounded-lg bg-tint p-4 text-sm whitespace-pre-wrap break-all">
            {secret.value}
          </pre>
          <button className={`${primary} mt-5`} onClick={() => setSecret(null)}>
            I saved it
          </button>
        </Modal>
      )}
    </div>
  );
}
