import { useEffect, useState } from "react";
import { request, serverEndpoint } from "./api.js";
import { Field, Modal, primary, danger, card, formatTimestamp } from "./ui.jsx";

export function Keys({ run, busy, setSecret }) {
  const [keys, setKeys] = useState([]);
  const [revoke, setRevoke] = useState(null);
  const load = async () =>
    setKeys((await request("/management/v1/credentials")).credentials);
  useEffect(() => {
    run(load);
  }, []);
  const endpoint = serverEndpoint("/mcp");
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
                  Created {formatTimestamp(key.created_at)} ·{" "}
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
