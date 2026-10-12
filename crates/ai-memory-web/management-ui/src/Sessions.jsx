import React, { useEffect, useState } from "react";
import { request, scopedQuery } from "./api.js";
import { Modal, secondary, danger, card, formatTimestamp } from "./ui.jsx";

export function Sessions({ me, run, busy }) {
  const [projects, setProjects] = useState([]);
  const [projectIndex, setProjectIndex] = useState("");
  const [page, setPage] = useState({ sessions: [], page_size: 0 });
  const { sessions, page_size: pageSize } = page;
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
    setPage(data);
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
            disabled={busy}
            value={projectIndex}
            onChange={(e) => {
              if (e.target.value === projectIndex) return;
              setPage({ sessions: [], page_size: 0 });
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
                <td className="pr-4 text-muted">
                  {formatTimestamp(s.started_at)}
                </td>
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
          onClick={() => setOffset(Math.max(0, offset - pageSize))}
        >
          Previous
        </button>
        <button
          className={secondary}
          disabled={busy || !pageSize || sessions.length < pageSize}
          onClick={() => setOffset(offset + pageSize)}
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
                  const offset = Math.max(0, detail.offset - detail.page_size);
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
              disabled={
                busy || detail.offset + detail.page_size >= detail.total
              }
              onClick={() =>
                run(async () => {
                  const offset = detail.offset + detail.page_size;
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
              [
                "Handoffs unlinked in other projects",
                "collateral_handoffs_denulled",
              ],
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
