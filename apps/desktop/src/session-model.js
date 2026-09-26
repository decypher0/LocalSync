// The pure parts of the session model, kept out of app.js so they can be
// tested from Node (same split as wizard-payload.js): no DOM, no Tauri.
//
// A SEND session is a project-scoped workspace that mirrors the backend's
// `ProjectSessionView` (project_session.rs owns the real data). Its
// `transfers` are short-lived and exist only on this side.

/** The fields the backend owns - replaced wholesale, never merged. */
function pickView(view) {
  return { title: view.title, folders: view.folders, artifact: view.artifact, devices: view.devices, saved: view.saved };
}

function sendSessionFromView(view) {
  return {
    id: view.id,
    kind: "send",
    startedAt: view.created_at,
    busy: false,
    transfers: [],
    pushSelection: new Set(),
    ...pickView(view),
  };
}

function applyView(session, view) {
  Object.assign(session, pickView(view));
}

/**
 * The one status a tab shows. A send session has no status of its own - it
 * is derived from its transfers, so there is nothing to keep in sync.
 */
function displayStatus(session) {
  if (session.kind !== "send") return session.status;
  const live = session.transfers.filter((t) => t.status === "connecting" || t.status === "active");
  if (live.some((t) => t.status === "active")) return "active";
  if (live.length > 0) return "connecting";
  const last = session.transfers[session.transfers.length - 1];
  if (last && (last.status === "error" || last.status === "expired")) return last.status;
  return "ready";
}

/** Two folder plans are the same workspace if they name the same folders with the same database plan. */
function sameFolderPlan(a, b) {
  const norm = (folders) =>
    JSON.stringify(
      folders
        .map((f) => ({ path: f.path, dump: f.dump ? { schema: f.dump.schema, file_path: f.dump.file_path, engine: f.dump.engine } : null }))
        .sort((x, y) => x.path.localeCompare(y.path))
    );
  return norm(a) === norm(b);
}

/**
 * The key a session files a discovered device under: its persistent id when
 * it announces one, so it's recognized as the same device next time -
 * otherwise its name, the best identity a device without one has.
 */
function deviceKeyFor(device) {
  return device.device_id || `name:${device.nickname}`;
}

if (typeof module !== "undefined" && module.exports) {
  module.exports = { pickView, sendSessionFromView, applyView, displayStatus, sameFolderPlan, deviceKeyFor };
}
