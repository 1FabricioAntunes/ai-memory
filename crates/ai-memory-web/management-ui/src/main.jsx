import React, { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { request, scopedQuery } from "./api.js";
import "./style.css";
import logoUrl from "./assets/logo.webp";

const primary = "button-primary";
const secondary = "button-secondary";
const danger = "button-danger";
const card = "panel";
const date = (value) =>
  value == null
    ? "—"
    : new Date(
        typeof value === "number" ? value / 1000 : value,
      ).toLocaleString();

const themeStorageKey = "ai-memory-management-theme";
function savedTheme() {
  try {
    const value = localStorage.getItem(themeStorageKey);
    return ["light", "dark"].includes(value) ? value : "system";
  } catch {
    return "system";
  }
}
document.documentElement.dataset.theme = savedTheme();

function ThemePicker() {
  const [theme, setTheme] = useState(savedTheme);
  const chooseTheme = (value) => {
    setTheme(value);
    document.documentElement.dataset.theme = value;
    try {
      if (value === "system") localStorage.removeItem(themeStorageKey);
      else localStorage.setItem(themeStorageKey, value);
    } catch {
      // Theme selection still works when browser storage is unavailable.
    }
  };
  return (
    <div role="group" aria-label="Color theme" className="theme-picker">
      {["system", "light", "dark"].map((value) => (
        <button
          key={value}
          type="button"
          aria-pressed={theme === value}
          onClick={() => chooseTheme(value)}
        >
          {value[0].toUpperCase() + value.slice(1)}
        </button>
      ))}
    </div>
  );
}

function App() {
  const [me, setMe] = useState(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [pending, setPending] = useState(0);
  const busy = pending > 0;
  const [tab, setTab] = useState("Sessions");
  const [secret, setSecret] = useState(null);
  const refresh = async () => {
    try {
      const data = await request("/auth/me");
      setMe(data.username ? data : null);
    } catch (e) {
      if (e.status !== 401) setError(e.message);
      setMe(null);
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
        setSecret(null);
        setMe(null);
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
                      setMe(null);
                      setSecret(null);
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
            {tab === "Sessions" && <Sessions me={me} run={run} busy={busy} />}
            {tab === "Users" && admin && (
              <Users run={run} busy={busy} setSecret={setSecret} />
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

function Field({ label, ...props }) {
  return (
    <label className="block text-sm">
      <span className="mb-2 block text-body">{label}</span>
      <input className="w-full" {...props} />
    </label>
  );
}
function Login({ run, refresh, busy }) {
  return (
    <section className={`${card} login-panel mx-auto max-w-md`}>
      <h1 className="section-heading mb-2 text-2xl font-semibold">Sign in</h1>
      <p className="mb-6 text-sm text-muted">
        Use your ai-memory username and password.
      </p>
      <form
        className="space-y-4"
        onSubmit={(e) => {
          e.preventDefault();
          const form = e.currentTarget;
          const data = new FormData(form);
          run(async () => {
            await request("/auth/login", {
              method: "POST",
              body: {
                username: data.get("username"),
                password: data.get("password"),
              },
            });
            form.reset();
            await refresh();
          });
        }}
      >
        <Field
          label="Username"
          name="username"
          autoComplete="username"
          required
        />
        <Field
          label="Password"
          name="password"
          type="password"
          autoComplete="current-password"
          required
        />
        <button className={primary} disabled={busy}>
          Sign in
        </button>
      </form>
    </section>
  );
}
function validatePasswordInput(event) {
  const input = event.currentTarget;
  input.setCustomValidity(
    Array.from(input.value).length < 12 ? "Use at least 12 characters." : "",
  );
}

function Password({ run, refresh, busy, required }) {
  return (
    <section className={`${card} max-w-lg`}>
      <h1 className="section-heading mb-2 text-2xl font-semibold">
        Change password
      </h1>
      <p className="mb-6 text-sm text-muted">
        {required
          ? "Choose a new password before using the console."
          : "Changing your password signs out your other browser sessions."}
      </p>
      <form
        className="space-y-4"
        onSubmit={(e) => {
          e.preventDefault();
          const form = e.currentTarget;
          const data = new FormData(form);
          run(async () => {
            await request("/auth/password", {
              method: "POST",
              body: {
                current_password: data.get("current"),
                new_password: data.get("new"),
                new_password_confirmation: data.get("confirmation"),
              },
            });
            form.reset();
            await refresh();
          });
        }}
      >
        <Field
          label="Current password"
          name="current"
          type="password"
          autoComplete="current-password"
          required
        />
        <Field
          label="New password (at least 12 characters)"
          name="new"
          minLength={12}
          onInput={validatePasswordInput}
          type="password"
          autoComplete="new-password"
          required
        />
        <Field
          label="Confirm new password"
          name="confirmation"
          minLength={12}
          onInput={validatePasswordInput}
          type="password"
          autoComplete="new-password"
          required
        />
        <button className={primary} disabled={busy}>
          Change password
        </button>
      </form>
    </section>
  );
}
function Modal({ title, children, onClose }) {
  const ref = React.useRef(null);
  useEffect(() => {
    ref.current.showModal();
  }, []);
  return (
    <dialog
      ref={ref}
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      className="console-dialog m-auto w-[min(95vw,42rem)] rounded-[1.25rem] border border-line bg-surface p-6 text-ink backdrop:bg-ink/25"
    >
      <div className="mb-5 flex items-center justify-between gap-4">
        <h2 className="text-xl font-semibold">{title}</h2>
        <button
          aria-label="Close dialog"
          className={secondary}
          onClick={onClose}
        >
          Close
        </button>
      </div>
      {children}
    </dialog>
  );
}
function Sessions({ me, run, busy }) {
  const [projects, setProjects] = useState([]);
  const [projectIndex, setProjectIndex] = useState("");
  const [sessions, setSessions] = useState([]);
  const [offset, setOffset] = useState(0);
  const [detail, setDetail] = useState(null);
  const [deletion, setDeletion] = useState(null);
  const [confirmed, setConfirmed] = useState(false);
  const [notice, setNotice] = useState("");
  const admin = me.capabilities.admin;
  const project = projects[Number(projectIndex)];
  useEffect(() => {
    run(async () => {
      const data = await request("/management/v1/projects");
      setProjects(data.projects);
      if (data.projects.length) setProjectIndex("0");
    });
  }, []);
  const load = async (p = project, start = offset, signal) => {
    if (!p) return;
    const data = await request(
      `/management/v1/sessions?${scopedQuery(p, start)}`,
      { signal },
    );
    setSessions(data.sessions);
  };
  useEffect(() => {
    const controller = new AbortController();
    if (project)
      run(async () => {
        try {
          await load(project, offset, controller.signal);
        } catch (e) {
          if (e.name !== "AbortError") throw e;
        }
      });
    return () => controller.abort();
  }, [projectIndex, offset, projects]);
  const purgeBody = {
    workspace: project?.workspace_name,
    project: project?.project_name,
  };
  return (
    <section>
      <div className="mb-6 flex flex-wrap items-end justify-between gap-4">
        <div>
          <h1 className="section-heading text-2xl font-semibold">
            {admin ? "All sessions" : "Your sessions"}
          </h1>
          <p className="mt-2 text-sm text-muted">
            {admin
              ? "Inspect and manage sessions across users."
              : "Your sessions and shared legacy sessions. Only sessions you own can be deleted."}
          </p>
        </div>
        <label className="text-sm">
          Project
          <select
            className="ml-3 max-w-full"
            value={projectIndex}
            onChange={(e) => {
              setProjectIndex(e.target.value);
              setOffset(0);
              setNotice("");
            }}
            aria-label="Project"
          >
            {projects.map((p, i) => (
              <option
                key={JSON.stringify([p.workspace_name, p.project_name])}
                value={i}
              >
                {p.workspace_name} / {p.project_name}
              </option>
            ))}
          </select>
        </label>
      </div>
      {notice && (
        <p
          role="status"
          className="mb-4 rounded-lg bg-success-soft p-4 text-success"
        >
          {notice}
        </p>
      )}
      <div className={`${card} overflow-x-auto`}>
        <table className="w-full text-left text-sm">
          <thead className="text-muted">
            <tr>
              {[
                "Session",
                "Owner",
                "Started",
                "State",
                "Events",
                "Actions",
              ].map((x) => (
                <th key={x} className="pb-4 pr-5 font-medium">
                  {x}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {sessions.map((s) => (
              <tr key={s.session_id} className="border-t border-line">
                <td className="py-4 pr-4">
                  <div className="font-medium">{s.agent_kind}</div>
                  <div
                    className="mt-1 font-mono text-xs text-muted"
                    title={s.session_id}
                  >
                    {s.session_id.slice(0, 8)}
                  </div>
                </td>
                <td className="pr-4 text-muted">
                  {s.actor_user?.replace(/^user:/, "") || "Shared"}
                </td>
                <td className="pr-4 text-muted">{date(s.started_at)}</td>
                <td className="pr-4">{s.ended_at ? "Ended" : "Open"}</td>
                <td className="pr-4">{s.observation_count}</td>
                <td className="py-4">
                  <div className="flex gap-2">
                    <button
                      className={secondary}
                      disabled={busy}
                      onClick={() =>
                        run(async () => {
                          const data = await request(
                            `/management/v1/sessions/${s.session_id}/observations?${scopedQuery(project)}`,
                          );
                          setDetail({ session: s, ...data, offset: 0 });
                        })
                      }
                    >
                      Inspect
                    </button>
                    {(admin || s.actor_user === `user:${me.username}`) && (
                      <button
                        className={danger}
                        disabled={busy}
                        onClick={() =>
                          run(async () => {
                            const preview = await request(
                              `/management/v1/sessions/${s.session_id}/purge`,
                              {
                                method: "POST",
                                body: { ...purgeBody, dry_run: true },
                              },
                            );
                            setConfirmed(false);
                            setDeletion({ session: s, preview });
                          })
                        }
                      >
                        Delete
                      </button>
                    )}
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {!sessions.length && (
          <p className="py-8 text-center text-muted">
            {projects.length
              ? "No sessions in this project."
              : "No accessible projects."}
          </p>
        )}
      </div>
      <div className="mt-4 flex justify-between">
        <button
          className={secondary}
          disabled={busy || offset === 0}
          onClick={() => setOffset(Math.max(0, offset - 50))}
        >
          Previous
        </button>
        <button
          className={secondary}
          disabled={busy || sessions.length < 50}
          onClick={() => setOffset(offset + 50)}
        >
          Next
        </button>
      </div>
      {detail && (
        <Modal
          title={`${detail.session.agent_kind} session`}
          onClose={() => setDetail(null)}
        >
          <p className="mb-4 break-all font-mono text-xs text-muted">
            {detail.session.session_id}
          </p>
          <div className="max-h-[55vh] space-y-3 overflow-y-auto">
            {detail.records.map((record, i) => (
              <article key={record.id || i} className="rounded-lg bg-tint p-4">
                <p className="mb-2 text-sm font-medium text-success">
                  {record.kind} · {record.title}
                </p>
                <pre className="whitespace-pre-wrap break-words text-xs text-body">
                  {record.body}
                </pre>
              </article>
            ))}
          </div>
          <div className="mt-4 flex justify-between">
            <button
              className={secondary}
              disabled={busy || detail.offset === 0}
              onClick={() =>
                run(async () => {
                  const offset = Math.max(0, detail.offset - 50);
                  const data = await request(
                    `/management/v1/sessions/${detail.session.session_id}/observations?${scopedQuery(project, offset)}`,
                  );
                  setDetail({ ...detail, ...data, offset });
                })
              }
            >
              Previous events
            </button>
            <button
              className={secondary}
              disabled={busy || detail.offset + 50 >= detail.total}
              onClick={() =>
                run(async () => {
                  const offset = detail.offset + 50;
                  const data = await request(
                    `/management/v1/sessions/${detail.session.session_id}/observations?${scopedQuery(project, offset)}`,
                  );
                  setDetail({ ...detail, ...data, offset });
                })
              }
            >
              Next events
            </button>
          </div>
        </Modal>
      )}
      {deletion && (
        <Modal
          title="Delete session"
          onClose={() => !busy && setDeletion(null)}
        >
          <p className="mb-4 text-sm text-body">
            This removes the captured events, authored handoffs, generated
            session-page versions, and review records. Shared knowledge pages
            are preserved. Git history and backups can retain earlier content.
          </p>
          <dl className="grid grid-cols-2 gap-3 rounded-lg bg-tint p-4 text-sm">
            {[
              ["Observations", "observations_deleted"],
              ["Handoffs", "handoffs_deleted"],
              ["Page versions", "pages_deleted"],
              ["Review runs", "auto_improve_runs_deleted"],
              ["Events in other projects", "collateral_observations_deleted"],
            ].map(([label, key]) => (
              <React.Fragment key={key}>
                <dt className="text-muted">{label}</dt>
                <dd>{deletion.preview[key]}</dd>
              </React.Fragment>
            ))}
          </dl>
          <label className="mt-5 flex gap-3 text-sm">
            <input
              type="checkbox"
              checked={confirmed}
              onChange={(e) => setConfirmed(e.target.checked)}
            />
            I understand and want to delete this session.
          </label>
          <button
            className={`${danger} mt-5`}
            disabled={busy || !confirmed}
            onClick={() =>
              run(async () => {
                const result = await request(
                  `/management/v1/sessions/${deletion.session.session_id}/purge`,
                  { method: "POST", body: { ...purgeBody, confirm: true } },
                );
                setDeletion(null);
                setNotice(
                  result.files_failed.length
                    ? "Database deletion completed, but some wiki files could not be removed. Ask an admin to check the server logs."
                    : "Session deleted.",
                );
                await load();
              })
            }
          >
            Delete session
          </button>
        </Modal>
      )}
    </section>
  );
}

function Users({ run, busy, setSecret }) {
  const [users, setUsers] = useState([]);
  const [keys, setKeys] = useState([]);
  const [action, setAction] = useState(null);
  const load = async () => {
    const [users, keys] = await Promise.all([
      request("/admin/users"),
      request("/admin/api-credentials"),
    ]);
    setUsers(users.users);
    setKeys(keys.credentials);
  };
  useEffect(() => {
    run(load);
  }, []);
  return (
    <section>
      <h1 className="section-heading mb-2 text-2xl font-semibold">Users</h1>
      <p className="mb-6 text-sm text-muted">
        Add operators, reset passwords, and manage access. Admins use the
        existing root role.
      </p>
      <form
        className={`${card} mb-6 grid gap-4 md:grid-cols-4`}
        onSubmit={(e) => {
          e.preventDefault();
          const form = e.currentTarget;
          const f = new FormData(form);
          run(async () => {
            const data = await request("/admin/human-users", {
              method: "POST",
              body: {
                username: f.get("username"),
                name: f.get("name") || null,
                role: f.get("role"),
              },
            });
            form.reset();
            setSecret({
              title: `Temporary password for ${data.user.username}`,
              value: data.temporary_password,
            });
            await load();
          });
        }}
      >
        <Field label="Username" name="username" required maxLength={64} />
        <Field label="Display name" name="name" />
        <label className="text-sm">
          <span className="mb-2 block text-body">Role</span>
          <select name="role" className="w-full">
            <option value="user">User</option>
            <option value="root">Admin</option>
          </select>
        </label>
        <button className={`${primary} self-end`} disabled={busy}>
          Add user
        </button>
      </form>
      <div className={`${card} overflow-x-auto`}>
        <table className="w-full text-left text-sm">
          <thead className="text-muted">
            <tr>
              <th className="pb-4">User</th>
              <th>Role</th>
              <th>Status</th>
              <th>Actions</th>
            </tr>
          </thead>
          <tbody>
            {users.map((user) => (
              <tr key={user.id} className="border-t border-line">
                <td className="py-4">
                  <div>{user.username}</div>
                  <div className="text-xs text-muted">{user.name}</div>
                </td>
                <td>{user.role === "root" ? "Admin" : "User"}</td>
                <td>
                  {user.disabled_at
                    ? "Disabled"
                    : user.must_change_password
                      ? "Password change required"
                      : "Active"}
                </td>
                <td>
                  <div className="flex flex-wrap gap-2">
                    <button
                      className={secondary}
                      disabled={busy}
                      onClick={() =>
                        setAction({
                          user,
                          type: "reset-password",
                          label: "Reset password",
                        })
                      }
                    >
                      Reset password
                    </button>
                    <button
                      className={secondary}
                      disabled={busy}
                      onClick={() =>
                        setAction({
                          user,
                          type: "role",
                          label:
                            user.role === "root" ? "Make user" : "Make admin",
                        })
                      }
                    >
                      {user.role === "root" ? "Make user" : "Make admin"}
                    </button>
                    <button
                      className={user.disabled_at ? secondary : danger}
                      disabled={busy}
                      onClick={() =>
                        setAction({
                          user,
                          type: user.disabled_at ? "enable" : "disable",
                          label: user.disabled_at
                            ? "Enable user"
                            : "Disable user",
                        })
                      }
                    >
                      {user.disabled_at ? "Enable" : "Disable"}
                    </button>
                    <button
                      className={secondary}
                      disabled={busy}
                      onClick={() =>
                        setAction({ user, type: "key", label: "Issue MCP key" })
                      }
                    >
                      Issue MCP key
                    </button>
                    <button
                      className={secondary}
                      disabled={busy}
                      onClick={() =>
                        setAction({ user, type: "keys", label: "MCP keys" })
                      }
                    >
                      Manage keys
                    </button>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {action?.type === "keys" && (
        <Modal
          title={`MCP keys: ${action.user.username}`}
          onClose={() => setAction(null)}
        >
          <div className="space-y-3">
            {keys
              .filter((key) => key.user_id === action.user.id)
              .map((key) => (
                <div
                  key={key.id}
                  className="flex items-center justify-between gap-3 rounded-lg bg-tint p-3"
                >
                  <span>
                    {key.label} · {key.revoked_at ? "Revoked" : "Active"}
                  </span>
                  <button
                    className={danger}
                    disabled={busy || !!key.revoked_at}
                    onClick={() =>
                      setAction({
                        ...action,
                        type: "revoke-key",
                        label: "Revoke MCP key",
                        key,
                      })
                    }
                  >
                    Revoke
                  </button>
                </div>
              ))}
            {!keys.some((key) => key.user_id === action.user.id) && (
              <p>No API keys.</p>
            )}
          </div>
        </Modal>
      )}
      {action && action.type !== "keys" && (
        <Modal
          title={`${action.label}: ${action.user.username}`}
          onClose={() => setAction(null)}
        >
          <p className="mb-5 text-sm text-muted">
            {action.type === "disable"
              ? "Disabling removes browser access. Existing MCP API keys remain active until revoked separately."
              : action.type === "key"
                ? "The new key authenticates this user to MCP and lifecycle hooks. It never grants admin privileges."
                : "Confirm this account change."}
          </p>
          <button
            className={primary}
            disabled={busy}
            onClick={() =>
              run(async () => {
                const username = encodeURIComponent(action.user.username);
                if (action.type === "revoke-key")
                  await request(
                    `/admin/api-credentials/${action.key.id}/revoke`,
                    { method: "POST", body: {} },
                  );
                else if (action.type === "role")
                  await request(`/admin/users/${username}`, {
                    method: "PATCH",
                    body: {
                      role: action.user.role === "root" ? "user" : "root",
                    },
                  });
                else if (action.type === "key") {
                  const data = await request("/admin/api-credentials", {
                    method: "POST",
                    body: {
                      username: action.user.username,
                      label: "console-issued",
                    },
                  });
                  setSecret({
                    title: `MCP key for ${action.user.username}`,
                    value: data.token,
                  });
                } else {
                  const data = await request(
                    `/admin/users/${username}/${action.type}`,
                    { method: "POST", body: {} },
                  );
                  if (data.temporary_password)
                    setSecret({
                      title: `Temporary password for ${action.user.username}`,
                      value: data.temporary_password,
                    });
                }
                setAction(null);
                await load();
              })
            }
          >
            Confirm
          </button>
        </Modal>
      )}
    </section>
  );
}

function Keys({ run, busy, setSecret }) {
  const [keys, setKeys] = useState([]);
  const [revoke, setRevoke] = useState(null);
  const load = async () =>
    setKeys((await request("/management/v1/credentials")).credentials);
  useEffect(() => {
    run(load);
  }, []);
  const base =
    document.querySelector('meta[name="ai-memory-base-path"]')?.content || "";
  const endpoint = `${window.location.origin}${base}/mcp`;
  return (
    <section>
      <h1 className="section-heading mb-2 text-2xl font-semibold">
        MCP connection
      </h1>
      <p className="mb-6 text-sm text-muted">
        Use a personal API key to connect your coding agent. Your browser
        password and session cookie cannot authenticate MCP.
      </p>
      <div className={`${card} mb-6`}>
        <p className="mb-2 text-sm text-muted">Remote MCP endpoint</p>
        <code className="break-all text-success">{endpoint}</code>
        <p className="mt-4 text-sm text-muted">
          Set this URL in your MCP client and send the key as an Authorization:
          Bearer header. Configure lifecycle hooks with the server URL and the
          same key.
        </p>
      </div>
      <form
        className={`${card} mb-6 flex flex-wrap items-end gap-4`}
        onSubmit={(e) => {
          e.preventDefault();
          const form = e.currentTarget;
          const f = new FormData(form);
          run(async () => {
            const data = await request("/management/v1/credentials", {
              method: "POST",
              body: { label: f.get("label") },
            });
            form.reset();
            setSecret({ title: "Your new MCP key", value: data.token });
            await load();
          });
        }}
      >
        <Field
          label="Key label"
          name="label"
          required
          maxLength={128}
          placeholder="Laptop · Codex"
        />
        <button className={primary} disabled={busy}>
          Create API key
        </button>
      </form>
      <div className={card}>
        {keys.length ? (
          keys.map((key) => (
            <div
              key={key.id}
              className="flex flex-wrap items-center justify-between gap-4 border-b border-line py-4 last:border-0"
            >
              <div>
                <p>
                  {key.label}
                  <span className="ml-3 font-mono text-xs text-muted">
                    {key.preview}
                  </span>
                </p>
                <p className="mt-1 text-xs text-muted">
                  Created {date(key.created_at)} ·{" "}
                  {key.revoked_at ? "Revoked" : "Active"}
                </p>
              </div>
              <button
                className={danger}
                disabled={busy || !!key.revoked_at}
                onClick={() => setRevoke(key)}
              >
                Revoke
              </button>
            </div>
          ))
        ) : (
          <p className="text-muted">No API keys yet.</p>
        )}
      </div>
      {revoke && (
        <Modal title={`Revoke ${revoke.label}`} onClose={() => setRevoke(null)}>
          <p className="mb-5 text-sm text-muted">
            Clients using this key will lose access. Your password and browser
            sessions stay active.
          </p>
          <button
            className={danger}
            disabled={busy}
            onClick={() =>
              run(async () => {
                await request(
                  `/management/v1/credentials/${revoke.id}/revoke`,
                  { method: "POST", body: {} },
                );
                setRevoke(null);
                await load();
              })
            }
          >
            Revoke key
          </button>
        </Modal>
      )}
    </section>
  );
}

createRoot(document.getElementById("root")).render(<App />);
