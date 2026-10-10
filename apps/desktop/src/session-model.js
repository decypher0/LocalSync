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

// A RECEIVE session mirrors the backend's `ReceivedSessionView`
// (id, title, sender_pubkey_hex, snapshot_id, git_commit, work_dir,
// created_at, last_received_at, saved, armed, running). Unlike a send, it
// starts life from a plain receive (no backend-owned view at all - see
// newSession in app.js) and only ever meets this shape once it's been saved
// and is being reopened, or a fresh receive is later saved. These two
// helpers are the receive-side counterpart of pickView/applyView above.

/** The fields the backend owns for a saved receive session - replaced wholesale, never merged. */
function pickReceivedView(view) {
  return {
    title: view.title,
    senderPubkeyHex: view.sender_pubkey_hex,
    snapshotId: view.snapshot_id,
    gitCommit: view.git_commit,
    workDir: view.work_dir,
    lastReceivedAt: view.last_received_at,
    saved: view.saved,
    armed: view.armed,
  };
}

/**
 * Builds a session tab from a *reopened* saved receive session. This is
 * "resume", not "review again" - the diff was already accepted before, so
 * there is no manifest/diff to hold, only what's needed to show the resume
 * panel and let Run/Stop and the arm toggle work. `reportedRunning` is kept
 * separate from `status` because the view has no runnable/stoppable id for
 * an already-running session (see app.js's own note on this gap) - it only
 * ever drives a hint in the UI, never which panel renders.
 */
function receivedSessionFromView(view) {
  return {
    id: view.id,
    kind: "receive",
    status: "resuming",
    startedAt: view.created_at,
    endedAt: null,
    errorText: "",
    resultText: "",
    busy: false,
    progressUnlisten: null,
    progressBytes: 0,
    progressTotal: 0,
    manifest: null,
    diff: null,
    recognizedPeer: null,
    runningSessionId: null,
    servicePorts: null,
    dbCacheHit: null,
    runInProgress: false,
    runLogText: "",
    runErrorText: "",
    // A saved session was, by definition, accepted and run at least once -
    // otherwise there'd have been nothing worth saving.
    hasRunBefore: true,
    rejected: false,
    reportedRunning: view.running,
    ...pickReceivedView(view),
  };
}

function applyReceivedView(session, view) {
  Object.assign(session, pickReceivedView(view));
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

/** The device a send session last went to (newest marker), or null. */
function lastSentDevice(devices) {
  return (
    (devices || [])
      .filter((d) => d.marker && d.marker.sent_at)
      .sort((x, y) => x.marker.sent_at.localeCompare(y.marker.sent_at))
      .pop() || null
  );
}

// ---------- Cloud drop ----------

// ls_clouddrop::oauth::SIGN_IN_EXPIRED_MESSAGE starts with this: Google
// refused the saved sign-in (invalid_grant), and the stale token was deleted.
const GOOGLE_SIGN_IN_EXPIRED = "Your Google sign-in expired.";
const isGoogleSignInExpired = (text) => String(text || "").startsWith(GOOGLE_SIGN_IN_EXPIRED);
// commands::CLOUD_DROP_NOT_LINKED - what the receiver's pre-flight says without one.
const CLOUD_DROP_NOT_LINKED = "Cloud drop needs a Google account so the sender can share the file with you.";

/**
 * What the receive form's "Link Google account" panel says for a pre-flight
 * error, or null when the error isn't fixed by linking (shown as a plain error).
 */
function cloudLinkNote(err) {
  const text = String(err || "");
  if (isGoogleSignInExpired(text)) return text;
  if (text === CLOUD_DROP_NOT_LINKED) return "No Google account is linked on this computer yet. Link one and the request goes ahead with the code you entered.";
  return null;
}

/**
 * The sender's Cloud drop transfer card follows these events:
 * joined (code entered) -> requested (their access request) -> granted |
 * declined -> downloaded; failed / ended can arrive at any point.
 */
function applyCloudEvent(tr, event, data = {}) {
  switch (event) {
    case "joined":
      if (tr.cloudStep === "waiting") tr.cloudStep = "joined";
      tr.receiverJoined = true;
      break;
    case "requested":
      tr.accessRequest = { email: data.email, busy: false, error: "" };
      tr.cloudStep = "requested";
      break;
    case "granted":
      tr.accessRequest = null;
      tr.cloudStep = "approved";
      tr.grantedEmail = data.email;
      break;
    case "declined":
      tr.accessRequest = null;
      tr.cloudStep = "declined";
      tr.declinedEmail = data.email;
      tr.status = "done";
      break;
    case "downloaded":
      tr.cloudStep = "downloaded";
      tr.status = "done";
      break;
    case "failed":
      tr.status = "error";
      tr.errorText = data.message;
      break;
    case "ended":
      // The receiver's control channel closed. After a grant that's the
      // normal end; before one, they left without getting the file.
      if (tr.status !== "waiting") break;
      if (tr.cloudStep === "approved") {
        tr.status = "done";
      } else {
        tr.status = "error";
        tr.errorText = "The receiver disconnected before getting access. Retry for a new code (the file is still in your Drive).";
      }
      break;
  }
  return tr;
}

/**
 * A received project's Run error as the receiver should read it. A project
 * sent without run instructions (ls-containers: "arrived without run
 * instructions", followed by a raw file-not-found cause) becomes one line
 * saying what the sender has to do; anything else is shown as it is.
 */
function plainRunError(text) {
  const t = String(text || "");
  return t.includes("arrived without run instructions")
    ? "This project arrived without run instructions. Ask the sender to finish Setup for it in LocalSync and send it again."
    : t;
}

if (typeof module !== "undefined" && module.exports) {
  module.exports = {
    lastSentDevice,
    isGoogleSignInExpired,
    CLOUD_DROP_NOT_LINKED,
    cloudLinkNote,
    applyCloudEvent,
    plainRunError,
    pickView,
    sendSessionFromView,
    applyView,
    displayStatus,
    sameFolderPlan,
    deviceKeyFor,
    pickReceivedView,
    receivedSessionFromView,
    applyReceivedView,
  };
}
