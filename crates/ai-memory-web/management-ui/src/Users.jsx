import { useEffect, useState } from "react";
import { request } from "./api.js";
import { Field, Modal, primary, secondary, danger, card } from "./ui.jsx";
import { MAX_USERNAME_LENGTH } from "./policy.js";

export function Users({ me, refresh, run, busy, setSecret }) {
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
        <Field
          label="Username"
          name="username"
          required
          maxLength={MAX_USERNAME_LENGTH}
        />
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
                if (
                  action.type === "role" &&
                  action.user.username === me.username
                ) {
                  await refresh();
                } else {
                  await load();
                }
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
