export function csrfCookie(cookie) {
  const entry = cookie
    .split(";")
    .map((part) => part.trim())
    .find((part) => part.startsWith("ai_memory_csrf="));
  return entry ? entry.slice("ai_memory_csrf=".length) : "";
}

export function scopedQuery(project, offset = 0) {
  return new URLSearchParams({
    workspace: project.workspace_name,
    project: project.project_name,
    offset: String(offset),
  }).toString();
}

export function serverBasePath() {
  return (
    document.querySelector('meta[name="ai-memory-base-path"]')?.content || ""
  );
}

export function serverEndpoint(path) {
  return `${window.location.origin}${serverBasePath()}${path}`;
}

export async function request(path, { method = "GET", body, signal } = {}) {
  const headers = { Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  if (method !== "GET") headers["X-CSRF-Token"] = csrfCookie(document.cookie);
  const response = await fetch(serverBasePath() + path, {
    method,
    headers,
    credentials: "include",
    cache: "no-store",
    body: body === undefined ? undefined : JSON.stringify(body),
    signal,
  });
  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    const error = new Error(
      data.error || `Request failed (${response.status})`,
    );
    error.status = response.status;
    throw error;
  }
  return data;
}
