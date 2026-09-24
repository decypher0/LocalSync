// Vanilla JS, no bundler: Tauri serves this directory as-is
// (`build.frontendDist` in tauri.conf.json) and `app.withGlobalTauri` is set
// so `window.__TAURI__` is available without an npm dependency. Plugin
// namespaces (dialog/updater/process below) are real objects here at this
// point in execution - verified directly with a real running app rather
// than assumed, since they're non-enumerable properties (don't show up in
// `Object.keys(window.__TAURI__)`, which only lists the core API - app,
// core, event, ... - misleadingly suggesting they're missing if you check
// that way instead of a direct property/typeof check).
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open } = window.__TAURI__.dialog;
const { check: checkForUpdate } = window.__TAURI__.updater;
const { relaunch } = window.__TAURI__.process;
const { writeText: writeClipboardText } = window.__TAURI__.clipboardManager;

const $ = (id) => document.getElementById(id);

// ---------- preload (dev shortcut for LOCALSYNC_PRELOAD_SNAPSHOT) ----------
// Fires the same IncomingSnapshotInfo shape receive_snapshot resolves with,
// so this reuses startReceiveSessionFromInfo verbatim — the review screen a
// developer sees this way is the same code path a real P2P receive draws,
// not a separate mockup. See scripts/demo-review-screen.sh.
listen("preload-review", (evt) => startReceiveSessionFromInfo(evt.payload));

// ---------- firewall warning (Linux ufw active at startup) ----------
listen("firewall-warning", (evt) => {
  $("firewall-banner").textContent = evt.payload;
  $("firewall-banner").classList.remove("hidden");
});

// ---------- round 24: magic-link deep-link handoff (localsync://receive?code=...) ----------
// Only ever pre-fills the Receive tab's own code input and switches to it -
// reuses the exact existing code-entry path rather than a parallel one, and
// never itself calls receive_snapshot. Accepting a P2P connection always
// still needs the same explicit Receive click a person typing the code by
// hand would make.
function handleDeepLinkUrls(urls) {
  if (!urls) return;
  for (const raw of urls) {
    let url;
    try {
      url = new URL(raw);
    } catch {
      continue; // not a parseable URL at all - ignore rather than throw
    }
    const code = url.searchParams.get("code");
    if (!code) continue;
    switchToTab("receive");
    $("receive-room-code").value = code;
    $("receive-start-error").textContent = "";
    break; // only one link is ever meaningful per launch/event
  }
}

// Two separate entry points, matching how the plugin itself splits this:
// getCurrent() covers "this process was just launched by clicking a link"
// (a fresh Windows/Linux process's own CLI argument, or macOS's equivalent -
// both already parsed into plugin state by the time this JS runs); onOpenUrl
// covers "a link was clicked again while this process is already running"
// (macOS's native re-open event, or a second Windows/Linux process
// redirected here by tauri-plugin-single-instance's "deep-link" feature -
// see main.rs for why that plugin exists at all).
window.__TAURI__.deepLink
  .getCurrent()
  .then((urls) => handleDeepLinkUrls(urls))
  .catch((err) => console.error("deep-link getCurrent failed:", err));
window.__TAURI__.deepLink.onOpenUrl((urls) => handleDeepLinkUrls(urls));

// ---------- round 16: auto-update (checking is silent; installing is never) ----------
// pendingUpdate holds the real Update object check() returned - only
// downloadAndInstall() (called from the Install button below, never
// automatically) actually fetches or applies anything. Checking alone -
// on launch, or via the Settings button - never downloads/installs by
// itself, matching the same explicit-consent shape every other
// state-changing action in this app already holds to (Run, Reject, Push
// update, ...).
let pendingUpdate = null;

async function runUpdateCheck(reportStatus) {
  try {
    const update = await checkForUpdate();
    if (update) {
      pendingUpdate = update;
      $("update-banner-text").textContent = `Update available: v${update.version}`;
      $("app-update-banner").classList.remove("hidden");
      if (reportStatus) $("check-updates-status").textContent = `v${update.version} available.`;
    } else {
      pendingUpdate = null;
      if (reportStatus) $("check-updates-status").textContent = "You're up to date.";
    }
  } catch (err) {
    // Quiet on the launch check (no endpoint configured yet, offline, a
    // dev build with no matching release, etc. shouldn't nag on startup) -
    // only surfaced when the user explicitly asked, via the Settings button.
    if (reportStatus) $("check-updates-status").textContent = String(err);
  }
}

// Launch check: a few seconds after startup, not the very first thing
// competing with initial render, and quiet either way (see reportStatus
// above) - if one's found, the banner appears; if not, or if it fails,
// nothing interrupts anyone.
setTimeout(() => runUpdateCheck(false), 3000);

$("check-updates-btn").addEventListener("click", () => {
  $("check-updates-status").textContent = "Checking…";
  runUpdateCheck(true);
});

$("update-dismiss-btn").addEventListener("click", () => {
  $("app-update-banner").classList.add("hidden");
});

$("update-install-btn").addEventListener("click", async () => {
  if (!pendingUpdate) return;
  $("update-install-btn").disabled = true;
  $("update-dismiss-btn").disabled = true;
  $("update-error").textContent = "";
  $("update-progress-wrap").classList.remove("hidden");
  let total = 0;
  let downloaded = 0;
  try {
    // The one call in this whole flow that actually fetches/applies
    // anything - only reachable from this explicit click.
    await pendingUpdate.downloadAndInstall((event) => {
      switch (event.event) {
        case "Started":
          total = event.data.contentLength || 0;
          $("update-progress-label").textContent = "Downloading update…";
          break;
        case "Progress":
          downloaded += event.data.chunkLength;
          setProgress("update-progress-bar", total ? (downloaded / total) * 100 : 0);
          $("update-progress-label").textContent = `Downloading… ${formatBytes(downloaded)}${total ? ` / ${formatBytes(total)}` : ""}`;
          break;
        case "Finished":
          setProgress("update-progress-bar", 100);
          $("update-progress-label").textContent = "Installing…";
          break;
      }
    });
    $("update-progress-label").textContent = "Update installed — restarting…";
    await relaunch();
  } catch (err) {
    $("update-error").textContent = String(err);
    $("update-install-btn").disabled = false;
    $("update-dismiss-btn").disabled = false;
  }
});

// ---------- pull requests (sender-side: a connected receiver asked "anything new?") ----------
// Registered once, globally — a request can arrive on any tab. Rendered as a
// dismissible row per pending peer_id, since more than one receiver could ask
// at once; removed once Accept/Decline is handled.
listen("pull-request", (evt) => {
  const peerId = evt.payload.peer_id;
  const existing = [...$("pull-requests").children].find((el) => el.dataset.peer === peerId);
  if (existing) return; // already showing a pending request for this peer
  const div = document.createElement("div");
  div.dataset.peer = peerId;
  div.className = "peer-banner";
  div.innerHTML = `
    <span>Pull request from <strong>${escapeHtml(peerId)}</strong></span>
    <span class="inline-row">
      <button class="ghost-btn accept-btn" type="button"><svg class="icon"><use href="#icon-check"></use></svg> Accept</button>
      <button class="ghost-btn decline-btn" type="button"><svg class="icon"><use href="#icon-x"></use></svg> Decline</button>
    </span>
    <p class="error"></p>
  `;
  const errorEl = div.querySelector(".error");
  const respond = async (accept) => {
    div.querySelectorAll("button").forEach((b) => (b.disabled = true));
    try {
      await invoke("respond_to_pull_request", { peerId, accept });
      div.remove();
      if (accept) refreshReceivers();
    } catch (err) {
      errorEl.textContent = String(err);
      div.querySelectorAll("button").forEach((b) => (b.disabled = false));
    }
  };
  div.querySelector(".accept-btn").addEventListener("click", () => respond(true));
  div.querySelector(".decline-btn").addEventListener("click", () => respond(false));
  $("pull-requests").appendChild(div);
});

// ---------- round 23: cloud-access requests (sender-side: a receiver asked
// to be granted Drive access) - same rendering/dedup pattern as the
// pull-request banner above, reusing the same container since both are
// "an incoming request from a connected peer, shown until acted on". ----------
listen("cloud-access-request", (evt) => {
  const { peer_id: peerId, google_email: googleEmail } = evt.payload;
  const existing = [...$("pull-requests").children].find((el) => el.dataset.peer === peerId);
  if (existing) return;
  const div = document.createElement("div");
  div.dataset.peer = peerId;
  div.className = "peer-banner";
  div.innerHTML = `
    <span>Cloud drop access request from <strong>${escapeHtml(googleEmail)}</strong></span>
    <span class="inline-row">
      <button class="ghost-btn accept-btn" type="button">Grant access</button>
      <button class="ghost-btn decline-btn" type="button">Decline</button>
    </span>
    <p class="error"></p>
  `;
  const errorEl = div.querySelector(".error");
  const respond = async (accept) => {
    div.querySelectorAll("button").forEach((b) => (b.disabled = true));
    try {
      await invoke("respond_to_cloud_access_request", { peerId, accept });
      div.remove();
    } catch (err) {
      errorEl.textContent = String(err);
      div.querySelectorAll("button").forEach((b) => (b.disabled = false));
    }
  };
  div.querySelector(".accept-btn").addEventListener("click", () => respond(true));
  div.querySelector(".decline-btn").addEventListener("click", () => respond(false));
  $("pull-requests").appendChild(div);
});

// ---------- snapshot updated (receiver-side: sender pushed a fresh snapshot) ----------
// Same payload shape receive_snapshot resolves with, so this reuses
// applyReviewInfoToSession verbatim - a pushed update goes through the
// exact same diff-review-then-Run/Reject screen as a normal receive.
// Round 29: attaches to whichever receive session most recently connected
// to a sender (lastReceiveSessionId) rather than a single global "the
// receive" - matches the backend's own single-outgoing-connection model
// (see lastReceiveSessionId's own doc comment above) rather than pretending
// this app can already receive pushed updates from more than one sender at
// once. Falls back to a fresh tab if that session is gone (e.g. closed, or
// this is a restart) so the update is never silently dropped.
listen("snapshot-updated", (evt) => {
  const session = lastReceiveSessionId && sessions.get(lastReceiveSessionId);
  if (!session) {
    startReceiveSessionFromInfo(evt.payload);
    return;
  }
  applyReviewInfoToSession(session, evt.payload);
  renderSessionTabs();
  if (session.id === activeSessionId) renderActiveSession();
});

// ---------- settings ----------
$("settings-toggle").addEventListener("click", () => {
  $("settings-panel").classList.toggle("hidden");
});
$("data-dir-display").value = "(read at launch; not editable here)";

// ---------- theme: system default, with a persisted manual override ----------
// Three real states, not a boolean: "system" (default) tracks
// prefers-color-scheme live, for as long as the developer never overrides
// it; "light"/"dark" are explicit, persisted choices that win regardless
// of the OS setting. The actual color values for each theme live entirely
// in styles.css's :root/[data-theme] blocks - this only ever decides which
// one applies, never touches a color itself.
const THEME_KEY = "localsync.theme";

function applyTheme(choice) {
  if (choice === "light" || choice === "dark") {
    document.documentElement.dataset.theme = choice;
  } else {
    delete document.documentElement.dataset.theme;
  }
}

function currentThemeChoice() {
  const saved = localStorage.getItem(THEME_KEY);
  return saved === "light" || saved === "dark" ? saved : "system";
}

// index.html's own inline bootstrap script already applied a saved
// light/dark override before first paint (avoiding a flash of the wrong
// theme) - this just brings the radios themselves in sync with it, and
// re-applies for the "system" case too (a no-op today since the
// bootstrap script only ever sets light/dark, but keeps this function the
// single source of truth rather than splitting theme-application logic
// across two files).
const savedTheme = currentThemeChoice();
$(`theme-${savedTheme}`).checked = true;
applyTheme(savedTheme);

// Shared by Settings' own radios and the round 29 app menu's Theme submenu -
// one source of truth for "choosing a theme", not two divergent code paths
// that happen to agree today.
function setThemeChoice(choice) {
  $(`theme-${choice}`).checked = true;
  localStorage.setItem(THEME_KEY, choice);
  applyTheme(choice);
}

["system", "light", "dark"].forEach((choice) => {
  $(`theme-${choice}`).addEventListener("change", () => setThemeChoice(choice));
});

// ---------- round 29 goal B2: real application-level menu ----------
// One handler for every item build_menu (main.rs) creates - Quit is
// handled directly in Rust (see main.rs's own comment) and never reaches
// here. Every other item just reuses whatever existing, already-correct UI
// already does the same thing (Settings' own theme radios, the Settings
// panel, the Check-for-updates button) rather than duplicating that logic.
listen("menu-action", (evt) => {
  switch (evt.payload) {
    case "menu-check-updates":
      $("settings-panel").classList.remove("hidden");
      $("check-updates-status").textContent = "Checking…";
      runUpdateCheck(true);
      break;
    case "menu-settings":
      $("settings-panel").classList.remove("hidden");
      break;
    case "menu-theme-system":
    case "menu-theme-light":
    case "menu-theme-dark":
      setThemeChoice(evt.payload.slice("menu-theme-".length));
      break;
    case "menu-session-history":
      openSessionHistory();
      break;
  }
});

// ---------- round 29 goal B2: session history (persisted, unlike the
// this-run-only session tabs above) ----------
async function openSessionHistory() {
  $("settings-panel").classList.add("hidden");
  $("session-history-panel").classList.remove("hidden");
  $("session-history-error").textContent = "";
  try {
    renderSessionHistoryList(await invoke("load_session_history"));
  } catch (err) {
    $("session-history-error").textContent = String(err);
  }
}

function renderSessionHistoryList(entries) {
  const list = $("session-history-list");
  list.innerHTML = "";
  // Newest first - a long-lived install's oldest entries are the least
  // relevant to see first.
  const sorted = [...entries].sort((a, b) => new Date(b.started_at) - new Date(a.started_at));
  $("session-history-empty").classList.toggle("hidden", sorted.length > 0);
  for (const entry of sorted) {
    const li = document.createElement("li");
    const kindIcon = entry.kind === "send" ? "icon-send" : "icon-download";
    const when = entry.ended_at ? `${entry.started_at} → ${entry.ended_at}` : `${entry.started_at} (in progress)`;
    li.innerHTML = `
      <svg class="icon"><use href="#${kindIcon}"></use></svg>
      <span>${escapeHtml(entry.title)}</span>
      <span class="hint-inline">${escapeHtml(when)}</span>
    `;
    list.appendChild(li);
  }
}

$("session-history-close-btn").addEventListener("click", () => {
  $("session-history-panel").classList.add("hidden");
});

// ---------- round 23: linked Google account (Cloud drop) ----------
async function refreshGoogleAccountStatus() {
  try {
    const linked = await invoke("google_account_status");
    $("google-account-status").textContent = linked ? `Linked as ${linked.email}` : "Not linked.";
    $("unlink-google-btn").classList.toggle("hidden", !linked);
  } catch (err) {
    $("google-account-status").textContent = String(err);
  }
}
refreshGoogleAccountStatus();

$("link-google-btn").addEventListener("click", async () => {
  $("google-account-error").textContent = "";
  $("link-google-btn").disabled = true;
  $("google-account-status").textContent = "Opening your browser for Google sign-in…";
  try {
    // Blocks until the browser redirect completes (or times out) - see
    // commands::link_google_account. The system browser opens itself; there's
    // nothing further to do here until this resolves.
    await invoke("link_google_account");
    await refreshGoogleAccountStatus();
  } catch (err) {
    $("google-account-error").textContent = String(err);
    await refreshGoogleAccountStatus();
  } finally {
    $("link-google-btn").disabled = false;
  }
});

$("unlink-google-btn").addEventListener("click", async () => {
  $("google-account-error").textContent = "";
  try {
    await invoke("unlink_google_account");
    await refreshGoogleAccountStatus();
  } catch (err) {
    $("google-account-error").textContent = String(err);
  }
});

// ---------- relay mode (persisted in localStorage — set once, survives restarts) ----------
const MODE_KEY = "localsync.relayMode";
const RELAY_URL_KEY = "localsync.relayUrl";

function relayMode() {
  return $("mode-remote").checked ? "remote" : "local";
}

function relayUrl() {
  return $("relay-url").value.trim();
}

function updateModeUi() {
  $("relay-url-wrap").classList.toggle("hidden", relayMode() !== "remote");
}

$("mode-local").addEventListener("change", () => {
  updateModeUi();
  localStorage.setItem(MODE_KEY, relayMode());
});
$("mode-remote").addEventListener("change", () => {
  updateModeUi();
  localStorage.setItem(MODE_KEY, relayMode());
});
$("relay-url").addEventListener("input", () => {
  localStorage.setItem(RELAY_URL_KEY, relayUrl());
});

// Restore persisted mode/URL on load.
if (localStorage.getItem(MODE_KEY) === "remote") $("mode-remote").checked = true;
$("relay-url").value = localStorage.getItem(RELAY_URL_KEY) || "";
updateModeUi();

// ---------- round 37: device name (one identity, used both when this
// device announces itself as discoverable and when it's the one initiating
// a connection to a discovered device) ----------
const DEVICE_NAME_KEY = "localsync.deviceName";

function deviceName() {
  return $("device-name").value.trim();
}
// Saved name if someone already chose one, else the OS hostname - so the
// field is never empty and discoverability works with zero setup.
async function fillDefaultDeviceName() {
  try {
    $("device-name").value = await invoke("default_device_name");
  } catch (err) {
    console.error("default_device_name failed:", err);
  }
}
$("device-name").value = localStorage.getItem(DEVICE_NAME_KEY) || "";
if (!deviceName()) fillDefaultDeviceName();
$("device-name").addEventListener("input", () => {
  localStorage.setItem(DEVICE_NAME_KEY, deviceName());
});
// "change" fires on blur/Enter, not every keystroke - so an edit made while
// discoverable is already on re-announces once, with the final name.
$("device-name").addEventListener("change", async () => {
  if (!deviceName()) {
    localStorage.removeItem(DEVICE_NAME_KEY);
    await fillDefaultDeviceName();
  }
  if ($("discoverable-toggle").checked) $("discoverable-toggle").dispatchEvent(new Event("change"));
});

// ---------- round 37 goal 1: opt-in discoverability (receiving side) ----------
$("discoverable-toggle").addEventListener("change", async () => {
  const enabled = $("discoverable-toggle").checked;
  $("discoverable-error").textContent = "";
  $("discoverable-status").textContent = enabled ? "Turning on…" : "";
  $("discoverable-toggle").disabled = true;
  try {
    await invoke("set_discoverable", { enabled, nickname: deviceName() });
    $("discoverable-status").textContent = enabled ? `Discoverable as "${deviceName()}".` : "";
  } catch (err) {
    $("discoverable-error").textContent = String(err);
    $("discoverable-status").textContent = "";
    $("discoverable-toggle").checked = false; // the backend call failed - never show "on" for a toggle that isn't
  } finally {
    $("discoverable-toggle").disabled = false;
  }
});

// ---------- round 37 goal 3: incoming connection request (receiving side) ----------
// Same rendering/dedup pattern as the pull-request/cloud-access-request
// banners above (reusing #pull-requests, not a new container) - "an
// incoming request from a peer, shown until acted on" is exactly what this
// is too. This is the actual consent gate discovery adds: nothing is ever
// received without this Accept explicitly happening first.
listen("connection-request", (evt) => {
  const peerId = evt.payload.peer_id;
  const existing = [...$("pull-requests").children].find((el) => el.dataset.peer === peerId);
  if (existing) return;
  const div = document.createElement("div");
  div.dataset.peer = peerId;
  div.className = "peer-banner";
  div.innerHTML = `
    <span><strong>${escapeHtml(evt.payload.sender_name)}</strong> wants to send you a project (found via Local network discovery)</span>
    <span class="inline-row">
      <button class="ghost-btn accept-btn" type="button"><svg class="icon"><use href="#icon-check"></use></svg> Accept</button>
      <button class="ghost-btn decline-btn" type="button"><svg class="icon"><use href="#icon-x"></use></svg> Decline</button>
    </span>
    <p class="error"></p>
  `;
  const errorEl = div.querySelector(".error");
  // ponytail: on Accept, this awaits the *entire* transfer before resolving
  // (respond_to_connection_request receives the whole payload before
  // returning - see its own doc comment) - so unlike a normal receive,
  // there's no session tab/progress bar until it's already done; the
  // banner's buttons just stay disabled for the duration. Acceptable for a
  // discovery-initiated receive (this round's actual scope is finding and
  // selecting a peer, not this path's progress UI specifically); upgrade
  // path if a large discovery-initiated project makes this feel stuck: split
  // accept-and-start-transfer from completion, same as receive_snapshot's
  // own two-phase (progress event, then resolve) shape.
  const respond = async (accept) => {
    div.querySelectorAll("button").forEach((b) => (b.disabled = true));
    try {
      const info = await invoke("respond_to_connection_request", { peerId, accept });
      div.remove();
      if (info) startReceiveSessionFromInfo(info);
    } catch (err) {
      errorEl.textContent = String(err);
      div.querySelectorAll("button").forEach((b) => (b.disabled = false));
    }
  };
  div.querySelector(".accept-btn").addEventListener("click", () => respond(true));
  div.querySelector(".decline-btn").addEventListener("click", () => respond(false));
  $("pull-requests").appendChild(div);
});

// ---------- round 37 goal 2: live nearby-devices list (sending side) ----------
//
// Browsing only runs while it's actually useful: while the Send wizard's
// mode step is showing *and* Local network is the selected mode - started/
// stopped from showWizardStep/the mode radios' change handlers below, and
// always stopped on wizard close/cancel, so an idle wizard never leaves a
// browse daemon running. `wizardSelectedDevice` is the one piece of state
// that changes what the final Send button actually does - see send-btn's
// own handler further down.
let wizardSelectedDevice = null;
let nearbyDevicesPollInterval = null;

async function startNearbyDevicesBrowsing() {
  $("wiz-nearby-wrap").classList.remove("hidden");
  $("wiz-nearby-empty").textContent = "Looking for discoverable devices on this network…";
  $("wiz-nearby-empty").classList.remove("hidden");
  $("wiz-nearby-list").classList.add("hidden");
  try {
    await invoke("start_discovery_browsing");
  } catch (err) {
    $("wiz-nearby-empty").textContent = `Couldn't start device discovery: ${err}`;
    return;
  }
  clearInterval(nearbyDevicesPollInterval);
  refreshNearbyDevices();
  nearbyDevicesPollInterval = setInterval(refreshNearbyDevices, 2000);
}

function stopNearbyDevicesBrowsing() {
  clearInterval(nearbyDevicesPollInterval);
  nearbyDevicesPollInterval = null;
  $("wiz-nearby-wrap").classList.add("hidden");
  invoke("stop_discovery_browsing").catch((err) => console.error("stop_discovery_browsing failed:", err));
}

async function refreshNearbyDevices() {
  let devices;
  try {
    devices = await invoke("list_nearby_devices");
  } catch (err) {
    $("wiz-nearby-empty").textContent = `Couldn't list nearby devices: ${err}`;
    $("wiz-nearby-empty").classList.remove("hidden");
    $("wiz-nearby-list").classList.add("hidden");
    return;
  }
  // A device that was selected but has since gone away (e.g. its
  // discoverability was turned off) needs its own, more specific message -
  // silently falling back to manual code entry without saying why would
  // look like the click simply didn't work.
  if (wizardSelectedDevice && !devices.some((d) => d.fullname === wizardSelectedDevice.fullname)) {
    deselectNearbyDevice();
    $("wiz-nearby-empty").textContent = "That device is no longer discoverable — pick another, or enter a code manually.";
  }
  renderNearbyDevicesList(devices);
}

function renderNearbyDevicesList(devices) {
  const empty = $("wiz-nearby-empty");
  const list = $("wiz-nearby-list");
  if (devices.length === 0) {
    // Goal 3: a clear, non-alarming empty state - looking for devices is
    // the normal first few seconds of every browse, and "found nothing" is
    // an expected, ordinary outcome on networks that block multicast, not
    // a failure this UI should imply.
    if (!empty.textContent.startsWith("That device")) {
      empty.textContent = "No devices found yet — you can still enter a code manually on the final step.";
    }
    empty.classList.remove("hidden");
    list.classList.add("hidden");
    return;
  }
  empty.classList.add("hidden");
  list.classList.remove("hidden");
  list.innerHTML = "";
  for (const d of devices) {
    const li = document.createElement("li");
    const isSelected = wizardSelectedDevice?.fullname === d.fullname;
    li.innerHTML = `
      <span class="inline-row receivers-header">
        <span><svg class="icon"><use href="#icon-wifi"></use></svg> ${escapeHtml(d.nickname)}</span>
        <button class="${isSelected ? "primary-btn" : "ghost-btn"} select-nearby-btn" type="button" data-fullname="${escapeHtml(d.fullname)}">
          ${isSelected ? "Selected" : "Select"}
        </button>
      </span>
    `;
    list.appendChild(li);
  }
}

$("wiz-nearby-list").addEventListener("click", async (e) => {
  const btn = e.target.closest(".select-nearby-btn");
  if (!btn) return;
  let devices;
  try {
    devices = await invoke("list_nearby_devices");
  } catch {
    return;
  }
  const device = devices.find((d) => d.fullname === btn.dataset.fullname);
  if (device) selectNearbyDevice(device);
});

function selectNearbyDevice(device) {
  wizardSelectedDevice = device;
  $("wiz-nearby-selected").classList.remove("hidden");
  $("wiz-nearby-selected-name").textContent = device.nickname;
  refreshNearbyDevices();
}

function deselectNearbyDevice() {
  wizardSelectedDevice = null;
  $("wiz-nearby-selected").classList.add("hidden");
  refreshNearbyDevices();
}
$("wiz-nearby-deselect-btn").addEventListener("click", deselectNearbyDevice);

// Round 20 goal 3: the Send wizard's own transfer-mode step - a separate
// radio group from Settings' (Receive still reads Settings' via
// relayMode()/relayUrl() above; that flow isn't part of this round's
// scope), but backed by the exact same localStorage keys, so choosing a
// mode here updates the one real shared default rather than creating a
// second, divergent setting - Settings and the wizard just become two
// surfaces onto the same underlying choice.
// "cloud" is a third, mutually exclusive choice here (moved onto this step
// from its old Step 4 checkbox - see wiz-cloud-drop-wrap in index.html) but
// is deliberately NOT one of the two values `MODE_KEY` ever persists: that
// key is also read by Settings/Receive's own relayMode(), which only ever
// expects "local"/"remote" back. Cloud drop still needs a real signaling
// mode under the hood for its brief identity-request/response handshake
// (see commands::start_cloud_drop_session's own `mode` parameter) - this
// always hands it "local", the same zero-configuration default Local
// network itself is. Picking Cloud drop only *hides* Local/Remote's own
// selection state for the rest of this wizard visit, it never overwrites it.
function wizRelayMode() {
  if ($("wiz-mode-cloud").checked) return "cloud";
  return $("wiz-mode-remote").checked ? "remote" : "local";
}
function wizRelayUrl() {
  return $("wiz-relay-url").value.trim();
}
function updateWizModeUi() {
  const mode = wizRelayMode();
  $("wiz-relay-url-wrap").classList.toggle("hidden", mode !== "remote");
  $("wiz-cloud-drop-wrap").classList.toggle("hidden", mode !== "cloud");
  if (mode === "cloud") refreshWizGoogleAccountStatus();

  // Round 37 goal 2: scoped to Local network only, per this round's own
  // goal - Remote relay/Cloud drop already work across networks LAN
  // discovery can't reach. A device selected while on Local network stops
  // meaning anything once the mode changes away from it.
  if (mode === "local") {
    startNearbyDevicesBrowsing();
  } else {
    stopNearbyDevicesBrowsing();
    deselectNearbyDevice();
  }
}
// Called every time step 1 is (re)entered, so it always reflects the most
// recent choice - made here, or made in Settings since the wizard was last
// opened. Cloud drop is never restored from storage (see wizRelayMode's own
// comment) - it always starts unchecked, falling back to whichever of
// Local/Remote was last persisted.
function syncWizModeFromStorage() {
  $("wiz-mode-cloud").checked = false;
  $("wiz-mode-remote").checked = localStorage.getItem(MODE_KEY) === "remote";
  $("wiz-mode-local").checked = !$("wiz-mode-remote").checked;
  $("wiz-relay-url").value = localStorage.getItem(RELAY_URL_KEY) || "";
  updateWizModeUi();
}
$("wiz-mode-local").addEventListener("change", () => {
  updateWizModeUi();
  localStorage.setItem(MODE_KEY, wizRelayMode());
});
$("wiz-mode-remote").addEventListener("change", () => {
  updateWizModeUi();
  localStorage.setItem(MODE_KEY, wizRelayMode());
});
$("wiz-mode-cloud").addEventListener("change", () => {
  // Deliberately not persisted to MODE_KEY - see wizRelayMode's comment.
  updateWizModeUi();
});
$("wiz-relay-url").addEventListener("input", () => {
  localStorage.setItem(RELAY_URL_KEY, wizRelayUrl());
});

// ---------- Cloud drop's Step-1 linked-Google-account check ----------
// Adapts the exact same Settings-panel flow (refreshGoogleAccountStatus/
// link-google-btn above) to Step 1's own DOM ids, so someone who picks
// Cloud drop without having linked an account yet can do so right there
// instead of being sent off to Settings and back.
async function refreshWizGoogleAccountStatus() {
  $("wiz-google-account-error").textContent = "";
  try {
    const linked = await invoke("google_account_status");
    $("wiz-google-account-status").textContent = linked ? `Linked as ${linked.email}` : "Not linked yet.";
    $("wiz-link-google-btn").classList.toggle("hidden", !!linked);
  } catch (err) {
    $("wiz-google-account-status").textContent = String(err);
  }
}
$("wiz-link-google-btn").addEventListener("click", async () => {
  $("wiz-google-account-error").textContent = "";
  $("wiz-link-google-btn").disabled = true;
  $("wiz-google-account-status").textContent = "Opening your browser for Google sign-in…";
  try {
    await invoke("link_google_account");
    await refreshWizGoogleAccountStatus();
    await refreshGoogleAccountStatus(); // keep Settings' own status in sync too
  } catch (err) {
    $("wiz-google-account-error").textContent = String(err);
    await refreshWizGoogleAccountStatus();
  } finally {
    $("wiz-link-google-btn").disabled = false;
  }
});

$("wiz-mode-next-btn").addEventListener("click", () => {
  if (wizRelayMode() === "remote" && !wizRelayUrl()) {
    $("wiz-mode-error").textContent = "Remote relay URL is required for Remote relay mode.";
    return;
  }
  $("wiz-mode-error").textContent = "";
  showWizardStep("wiz-step-folders");
});

// ---------- tabs ----------
// Extracted so round 24's deep-link handler can switch to Receive the same
// way a real click does, rather than duplicating this in two places.
function switchToTab(name) {
  document.querySelectorAll(".tab-btn").forEach((b) => b.classList.remove("active"));
  document.querySelectorAll(".tab-panel").forEach((p) => p.classList.remove("active"));
  document.querySelector(`.tab-btn[data-tab="${name}"]`).classList.add("active");
  $(`tab-${name}`).classList.add("active");
  // Round 29: clicking a top-level Send/Receive nav tab always means "I
  // want to start something new" - deselect whatever session tab was
  // showing (its own state keeps running untouched in the background;
  // this only changes what's currently *visible*).
  setActiveSession(null);
  // Show "Previously connected" the moment someone looks at the Send tab,
  // not only after they've just sent something or clicked Refresh by hand.
  if (name === "send") refreshReceivers();
}

document.querySelectorAll(".tab-btn").forEach((btn) => {
  btn.addEventListener("click", () => switchToTab(btn.dataset.tab));
});

// ---------- round 29 goal A2/B1: the multi-session model ----------
//
// Root cause of "starting a second, different send while one is already
// active doesn't work" (round 28/29's own investigation, see
// tests/concurrent_multi_session_test.rs for the proof the *backend* was
// never the problem - two genuinely concurrent `share_snapshot` calls,
// spawned before either is awaited, already worked correctly): this file
// used to track "the current send" and "the current receive" as a small
// handful of bare module-level variables (`currentRoomCode`,
// `unlistenSendProgress`, `currentSnapshotId`, ...) and a single shared set
// of DOM elements - starting a second send before the first finished simply
// overwrote all of it, silently losing the first session's own progress/
// room-code display even though its actual backend transfer kept running
// untouched underneath.
//
// Fix: every session (a send in flight, or a receive in flight/held/
// running) is now a real object in this `sessions` Map, keyed by the same
// room code / snapshot id the backend already uses to key its own
// `connected_receivers`/`verified`/`sessions` maps - no new identifier
// scheme invented. `activeSessionId` decides which *one* session's data is
// currently rendered into the shared detail viewport (round 29 goal B1 -
// "one tab per session", terminal-multiplexer style: every other session
// keeps its own state exactly as it was, whether or not it's the one
// currently visible.
const sessions = new Map();
let activeSessionId = null;

/// A session's shape - not a class, just the one object literal every
/// creation site below fills in the same way. `kind` is "send" or
/// "receive"; everything past `status` is only ever read/written by that
/// kind's own code paths (a send session's `snapshotId`/`manifest`/etc.
/// simply stay at their initial `null`, and vice versa).
function newSession(kind, id, title) {
  return {
    id,
    kind,
    title,
    status: "connecting", // connecting | active | reviewing | running | done | error | expired
    startedAt: new Date().toISOString(),
    endedAt: null,
    errorText: "",
    resultText: "",
    busy: false, // an action button (Run/Reject/Stop/Ask for update/Retry) is mid-flight
    // ---- send-only ----
    roomCode: null,
    codeExpiresAt: null,
    progressUnlisten: null,
    progressBytes: 0,
    progressTotal: 0,
    folders: [], // [{ path, dump: {schema, engine} | null }]
    cloudDrop: false,
    // Round 30 goal A: the exact folder/database plan (Rust-shaped payload,
    // not the display-only `folders` above - see buildWizardFoldersPayload)
    // and transfer settings this send was started with, kept around so
    // Retry (retrySendSession) can re-issue a fresh room code and re-run
    // share_snapshot_wizard on this same session/tab - without sending the
    // person back through folder selection or the database wizard for a
    // project that's already fully configured. null for a cloud-drop
    // session (it resolves in one shot; nothing left in "expired" for it
    // to retry) and for anything created before this existed.
    retryFolders: null,
    retryMode: null,
    retryUrl: null,
    // ---- receive-only ----
    snapshotId: null,
    senderPubkeyHex: null,
    manifest: null,
    diff: null,
    recognizedPeer: null,
    runningSessionId: null,
    servicePorts: null,
    dbCacheHit: null,
    runInProgress: false,
    runLogText: "",
    runErrorText: "",
  };
}

function activeSession() {
  return activeSessionId ? sessions.get(activeSessionId) : null;
}

/// Fire-and-forget, same reasoning as everything else in this app that
/// persists something the user doesn't need to wait on (e.g. `remember_peer`)
/// - a failure to record history is worth logging, never worth blocking or
/// erroring the actual session over.
function recordSessionHistory(session) {
  // SessionHistoryEntry (session_history.rs) has no #[serde(rename_all)],
  // so its JSON field names are exactly its Rust field names - snake_case,
  // not the camelCase invoke() otherwise auto-converts top-level command
  // *argument* names to. This nested struct's own fields get no such
  // conversion, so they're spelled out snake_case here to match.
  invoke("record_session_history_entry", {
    entry: {
      id: session.id,
      kind: session.kind,
      title: session.title,
      started_at: session.startedAt,
      ended_at: session.endedAt,
    },
  }).catch((err) => console.error("record_session_history_entry failed:", err));
}

function addSession(session) {
  sessions.set(session.id, session);
  recordSessionHistory(session);
  renderSessionTabs();
  setActiveSession(session.id);
}

/// Marks a session finished (successfully or not) without removing its tab
/// - the person can still look at a completed/failed session's detail until
/// they explicitly close it (closeSessionTab below). Safe to call more than
/// once (e.g. an error path and a later cleanup both reaching for it).
function endSession(session, status) {
  if (session.endedAt) return;
  session.status = status;
  session.endedAt = new Date().toISOString();
  if (session.progressUnlisten) {
    session.progressUnlisten();
    session.progressUnlisten = null;
  }
  recordSessionHistory(session);
  renderSessionTabs();
  if (session.id === activeSessionId) renderActiveSession();
}

function closeSessionTab(id) {
  const session = sessions.get(id);
  if (session && session.progressUnlisten) session.progressUnlisten();
  sessions.delete(id);
  if (activeSessionId === id) {
    setActiveSession(null);
  } else {
    renderSessionTabs();
  }
}

const SESSION_STATUS_ICON = {
  connecting: "icon-wifi",
  active: "icon-send",
  reviewing: "icon-shield-alert",
  running: "icon-play",
  done: "icon-check",
  error: "icon-circle-x",
  expired: "icon-refresh-cw",
};

function renderSessionTabs() {
  const bar = $("session-tabs-bar");
  const list = $("session-tabs-list");
  bar.classList.toggle("hidden", sessions.size === 0);
  list.innerHTML = "";
  for (const session of sessions.values()) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "session-tab-btn" + (session.id === activeSessionId ? " active" : "");
    btn.dataset.sessionId = session.id;
    const kindIcon = session.kind === "send" ? "icon-send" : "icon-download";
    const statusIcon = SESSION_STATUS_ICON[session.status] || "icon-wifi";
    btn.innerHTML = `
      <svg class="icon"><use href="#${kindIcon}"></use></svg>
      <span class="session-tab-title">${escapeHtml(session.title)}</span>
      <svg class="icon session-tab-status session-tab-status-${session.status}"><use href="#${statusIcon}"></use></svg>
      <span class="session-tab-close" data-close-session="${session.id}" title="Close tab">
        <svg class="icon"><use href="#icon-x"></use></svg>
      </span>
    `;
    list.appendChild(btn);
  }
}

$("session-tabs-list").addEventListener("click", (e) => {
  const closeTarget = e.target.closest("[data-close-session]");
  if (closeTarget) {
    closeSessionTab(closeTarget.dataset.closeSession);
    return;
  }
  const tabBtn = e.target.closest(".session-tab-btn");
  if (tabBtn) setActiveSession(tabBtn.dataset.sessionId);
});

/// The single entry point for "which session's data is currently on
/// screen." `null` means no session selected - the plain Send/Receive
/// compose UI (start-send-wizard-btn / the receive-idle form) is what
/// shows in that case, exactly as it always has.
function setActiveSession(id) {
  activeSessionId = id;
  renderSessionTabs();
  renderActiveSession();
}

function renderActiveSession() {
  const session = activeSession();
  const viewport = $("session-detail-viewport");
  $("session-detail-popover").classList.add("hidden");

  if (!session) {
    viewport.classList.add("hidden");
    return;
  }
  viewport.classList.remove("hidden");
  $("session-detail-title").textContent = session.title;
  const statusText = {
    connecting: "Connecting…",
    active: "Active",
    reviewing: "Awaiting review",
    running: "Running",
    done: "Done",
    error: "Error",
    expired: "Expired",
  }[session.status] || session.status;
  $("session-detail-status").textContent = statusText;

  $("session-send-view").classList.toggle("hidden", session.kind !== "send");
  $("session-receive-view").classList.toggle("hidden", session.kind !== "receive");

  if (session.kind === "send") {
    renderSendSessionDetail(session);
  } else {
    renderReceiveSessionDetail(session);
  }
}

$("session-detail-close-btn").addEventListener("click", () => {
  if (activeSessionId) closeSessionTab(activeSessionId);
});

// ---------- round 29 goal B3: per-session detail popover ----------
$("session-detail-info-btn").addEventListener("click", async () => {
  const session = activeSession();
  const popover = $("session-detail-popover");
  if (!session) return;
  popover.classList.toggle("hidden");
  if (popover.classList.contains("hidden")) return;

  const peopleWrap = $("session-detail-people-wrap");
  const foldersWrap = $("session-detail-folders-wrap");
  const dbWrap = $("session-detail-db-wrap");
  peopleWrap.classList.add("hidden");
  foldersWrap.classList.add("hidden");
  dbWrap.classList.add("hidden");

  if (session.kind === "send") {
    // Connected people: session.id (== info.room_id for a normal send - see
    // its own creation site's comment) is the peer_id the backend roster
    // already keys on, *not* the user-facing session.roomCode - filter the
    // full roster down to just this one rather than adding a new,
    // session-scoped backend command for what's already exposed. A Cloud
    // drop session has no roster entry here at all (it uses a separate,
    // per-round-23 map) - this simply (and correctly) finds nothing for it.
    try {
      const roster = await invoke("list_connected_receivers");
      const mine = roster.filter((r) => r.peer_id === session.id);
      if (mine.length > 0) {
        peopleWrap.classList.remove("hidden");
        $("session-detail-people-list").innerHTML = mine
          .map((r) => `<li><span class="mono">${escapeHtml(r.peer_id)}</span> <span class="hint-inline">connected ${escapeHtml(r.connected_at)}</span></li>`)
          .join("");
      }
    } catch (err) {
      console.error("list_connected_receivers failed:", err);
    }

    if (session.folders.length > 0) {
      foldersWrap.classList.remove("hidden");
      $("session-detail-folders-list").innerHTML = session.folders
        .map((f) => `<li>${escapeHtml(wizFolderLabel(f.path))}${f.dump ? ` <span class="hint-inline">(${escapeHtml(f.dump.engine)}, schema ${escapeHtml(f.dump.schema)})</span>` : ""}</li>`)
        .join("");
    }
    const dbFolders = session.folders.filter((f) => f.dump);
    if (dbFolders.length > 0) {
      dbWrap.classList.remove("hidden");
      $("session-detail-db-list").innerHTML = dbFolders
        .map((f) => `<li>${escapeHtml(wizFolderLabel(f.path))}: <strong>${escapeHtml(f.dump.engine)}</strong>, schema <span class="mono">${escapeHtml(f.dump.schema)}</span></li>`)
        .join("");
    }
  } else {
    if (session.manifest) {
      foldersWrap.classList.remove("hidden");
      const folderNames = (session.manifest.folders && session.manifest.folders.length > 0)
        ? session.manifest.folders.map((f) => f.path || f)
        : [session.manifest.project_name];
      $("session-detail-folders-list").innerHTML = folderNames.map((f) => `<li>${escapeHtml(String(f))}</li>`).join("");

      const dumps = session.manifest.database_dumps || [];
      if (dumps.length > 0) {
        dbWrap.classList.remove("hidden");
        $("session-detail-db-list").innerHTML = dumps
          .map((d) => `<li><strong>${escapeHtml(d.engine || "mysql")}</strong>, schema <span class="mono">${escapeHtml(d.schema)}</span></li>`)
          .join("");
      }
    }
  }

  const anyShown = !peopleWrap.classList.contains("hidden") || !foldersWrap.classList.contains("hidden") || !dbWrap.classList.contains("hidden");
  $("session-detail-empty").classList.toggle("hidden", anyShown);
});

// ---------- send: round 17/22 database-source wizard ----------
//
// wizardFolders is the one source of truth for the whole wizard: each entry
// is { path, needsDb, details, sourceLabel, dump, detected } where `dump`,
// once set, is either { schema, filePath, engine } from a developer-
// supplied file or from a real export — share_snapshot_wizard treats both
// identically, so nothing downstream needs to know which one it was.
// `detected` caches the one auto-detection attempt per folder (`null` if
// none found) so it's never re-run once known.
//
// Round 22 fixed the step order to match round 17's own original design:
// the "do you already have a dump file?" question is asked immediately
// after (cheap, local, no-network) auto-detection, for every folder,
// detected or not - a live connection is only ever attempted afterward, on
// the "no, I need to fetch live data" path. It also added: real schema
// browsing after a successful connection (never trusting a typed/detected
// database name blindly), and an optional shared-database mode for multi-
// folder sends (wizardSharedMode/wizardSharedGroupIndices/
// wizardSharedResolved below).
let wizardFolders = [];
let wizardFolderIndex = 0;
let wizardDbSubState = null; // tracks the per-folder sub-panel for Back
let wizardSharedMode = false;
let wizardSharedGroupIndices = [];
let wizardSharedResolved = null; // { details, sourceLabel, dump } once resolved

function wizFolderLabel(path) {
  return path.split(/[\\/]/).filter(Boolean).pop() || path;
}

// Round 20 goal 4: coarse-grained phases for the modal's "Step X of N"
// header - the per-folder database sub-flow (detect/manual/dump/schemas/
// tables) has a genuinely variable number of screens depending on what's
// detected and which branch the developer takes, so it's shown as one
// numbered phase ("Database setup") rather than pretending to a false,
// ever-changing step count within it.
const WIZARD_PHASES = [
  { step: "wiz-step-mode", label: "Transfer mode" },
  { step: "wiz-step-folders", label: "Project folder(s)" },
  { step: "wiz-step-needs-db", label: "Database setup" },
  { step: "wiz-step-shared-db", label: "Database setup" },
  { step: "wiz-step-db-folder", label: "Database setup" },
  { step: "wiz-step-ready", label: "Ready to send" },
];
const WIZARD_PHASE_COUNT = new Set(WIZARD_PHASES.map((p) => p.label)).size;

function updateWizardProgress(id) {
  const entry = WIZARD_PHASES.find((p) => p.step === id);
  if (!entry) return;
  const phaseNumber = new Set(WIZARD_PHASES.slice(0, WIZARD_PHASES.indexOf(entry) + 1).map((p) => p.label)).size;
  $("wizard-progress-label").textContent = `Step ${phaseNumber} of ${WIZARD_PHASE_COUNT}: ${entry.label}`;
}

function showWizardStep(id) {
  document.querySelectorAll("#send-wizard .wizard-step").forEach((el) => el.classList.add("hidden"));
  $(id).classList.remove("hidden");
  updateWizardProgress(id);
}

// ---------- round 20 goal 4: the wizard as a real modal overlay ----------

function openSendWizard() {
  resetSendWizard();
  $("send-wizard-overlay").classList.remove("hidden");
  syncWizModeFromStorage();
  showWizardStep("wiz-step-mode");
}

function closeSendWizard() {
  $("send-wizard-overlay").classList.add("hidden");
  stopNearbyDevicesBrowsing();
}

$("start-send-wizard-btn").addEventListener("click", openSendWizard);

$("wizard-cancel-btn").addEventListener("click", () => {
  closeSendWizard();
  resetSendWizard();
});

function hideAllDbSubPanels() {
  ["wiz-db-detecting", "wiz-db-ask-has-dump", "wiz-db-pick-dump", "wiz-db-manual", "wiz-db-connecting", "wiz-db-schemas", "wiz-db-tables"].forEach(
    (id) => $(id).classList.add("hidden")
  );
  $("wiz-db-connect-error").textContent = "";
}

function renderWizardFolderList() {
  const ul = $("wiz-folder-list");
  ul.innerHTML = "";
  for (const [i, f] of wizardFolders.entries()) {
    const li = document.createElement("li");
    li.innerHTML = `<span class="mono">${escapeHtml(f.path)}</span>`;
    const removeBtn = document.createElement("button");
    removeBtn.className = "ghost-btn remove-folder-btn";
    removeBtn.type = "button";
    // innerHTML is safe here: the icon markup is a fixed literal, nothing
    // from f.path (already escaped above via escapeHtml) ever reaches it.
    removeBtn.innerHTML = '<svg class="icon"><use href="#icon-x"></use></svg> Remove';
    removeBtn.addEventListener("click", () => {
      wizardFolders.splice(i, 1);
      renderWizardFolderList();
    });
    li.appendChild(removeBtn);
    ul.appendChild(li);
  }
}

$("wiz-add-folders-btn").addEventListener("click", async () => {
  const dirs = await open({ directory: true, multiple: true });
  if (!dirs) return;
  const picked = Array.isArray(dirs) ? dirs : [dirs];
  for (const p of picked) {
    if (!wizardFolders.some((f) => f.path === p)) {
      wizardFolders.push({ path: p, needsDb: null, details: null, sourceLabel: null, dump: null, detected: undefined });
    }
  }
  renderWizardFolderList();
});

$("wiz-folders-back-btn").addEventListener("click", () => showWizardStep("wiz-step-mode"));

$("wiz-folders-next-btn").addEventListener("click", () => {
  if (wizardFolders.length === 0) {
    $("wiz-folders-error").textContent = "Select at least one project folder.";
    return;
  }
  $("wiz-folders-error").textContent = "";
  showWizardStep("wiz-step-needs-db");
});

$("wiz-needs-db-back-btn").addEventListener("click", () => showWizardStep("wiz-step-folders"));

$("wiz-needs-db-no-btn").addEventListener("click", () => {
  for (const f of wizardFolders) f.needsDb = false;
  renderWizardReadyStep();
});

$("wiz-needs-db-yes-btn").addEventListener("click", () => {
  for (const f of wizardFolders) f.needsDb = true;
  wizardSharedMode = false;
  wizardSharedResolved = null;
  if (wizardFolders.length > 1) {
    $("wiz-shared-db-note").textContent = "";
    showWizardStep("wiz-step-shared-db");
  } else {
    wizardFolderIndex = 0;
    advanceDbWizard();
  }
});

// ---------- round 22 goal 6: shared-database question (multi-folder only) ----------

$("wiz-shared-db-back-btn").addEventListener("click", () => showWizardStep("wiz-step-needs-db"));

$("wiz-shared-db-no-btn").addEventListener("click", () => {
  wizardSharedMode = false;
  wizardFolderIndex = 0;
  advanceDbWizard();
});

$("wiz-shared-db-yes-btn").addEventListener("click", async () => {
  $("wiz-shared-db-note").textContent = "Checking each folder's own config…";
  // Real, cheap, local detection for every folder up front - this is what
  // lets a genuine mismatch (two folders whose own config clearly points
  // at different databases) be caught automatically rather than assumed
  // away, per this goal's own "only fall back to per-folder entry for
  // folders where auto-detection reveals genuinely different connection
  // details" requirement.
  const detections = [];
  for (const f of wizardFolders) {
    try {
      detections.push(await invoke("detect_db_connection", { folderPath: f.path }));
    } catch {
      detections.push(null);
    }
  }
  const signature = (d) => (d ? JSON.stringify(d.details) : null);
  const distinctSignatures = new Set(detections.filter(Boolean).map(signature));

  wizardFolders.forEach((f, i) => (f.detected = detections[i]));

  if (distinctSignatures.size > 1) {
    $("wiz-shared-db-note").textContent =
      "Auto-detection found different connection details across folders, so each is being set up separately instead.";
    wizardSharedMode = false;
  } else {
    wizardSharedMode = true;
    wizardSharedGroupIndices = wizardFolders.map((_, i) => i);
  }
  wizardFolderIndex = 0;
  advanceDbWizard();
});

// ---------- per-folder database detection / dump-or-fetch / schema browsing ----------

async function advanceDbWizard() {
  if (wizardFolderIndex >= wizardFolders.length) {
    renderWizardReadyStep();
    return;
  }
  const folder = wizardFolders[wizardFolderIndex];

  // Round 22 goal 6: once the shared group's one connection/dump decision
  // is resolved, apply it to every remaining folder in the group silently
  // instead of asking again.
  if (wizardSharedMode && wizardSharedResolved && wizardSharedGroupIndices.includes(wizardFolderIndex)) {
    folder.details = wizardSharedResolved.details;
    folder.sourceLabel = wizardSharedResolved.sourceLabel;
    folder.dump = wizardSharedResolved.dump;
    wizardFolderIndex += 1;
    advanceDbWizard();
    return;
  }

  showWizardStep("wiz-step-db-folder");
  hideAllDbSubPanels();
  $("wiz-db-folder-title").textContent = wizardSharedMode ? "Shared database" : wizFolderLabel(folder.path);
  $("wiz-db-folder-progress").textContent = wizardSharedMode
    ? "Applies to every selected folder"
    : `Folder ${wizardFolderIndex + 1} of ${wizardFolders.length}`;
  wizardDbSubState = "detecting";
  $("wiz-db-detecting").classList.remove("hidden");

  if (folder.detected === undefined) {
    try {
      folder.detected = await invoke("detect_db_connection", { folderPath: folder.path });
    } catch {
      // Detection is pure local file parsing - a thrown error here means
      // something unexpected (e.g. an unreadable path), not "no config
      // found". Either way, manual entry is always the safe fallback.
      folder.detected = null;
    }
  }

  hideAllDbSubPanels();
  showAskHasDump(folder);
}

// Round 22 goal 4: the *first* real decision for every folder, detected or
// not - shown before any live connection is ever attempted. Round 18's
// per-engine defaults (used below) mean a developer picking Postgres or
// MongoDB from a dropdown never has to remember (or leave wrong) another
// engine's standard port.
const DEFAULT_PORT_BY_ENGINE = { mysql: 3306, postgres: 5432, mongodb: 27017 };

function dbNounFor(engine) {
  return engine === "mongodb" ? "collections" : "tables";
}

function showAskHasDump(folder) {
  wizardDbSubState = "ask";
  const detected = folder.detected;
  if (detected) {
    folder.details = detected.details;
    folder.sourceLabel = detected.source_file;
    $("wiz-db-detected-source").textContent = detected.source_file;
    $("wiz-db-detected-engine").textContent = detected.details.engine;
    $("wiz-db-detected-host").textContent = detected.details.host;
    $("wiz-db-detected-port").textContent = String(detected.details.port);
    $("wiz-db-detected-database").textContent = detected.details.database;
    $("wiz-db-detected-username").textContent = detected.details.username;
    $("wiz-db-detected-info").classList.remove("hidden");
    $("wiz-db-engine-picker-inline").classList.add("hidden");
    $("wiz-ask-edit-btn").classList.remove("hidden");
  } else {
    $("wiz-db-detected-info").classList.add("hidden");
    $("wiz-db-engine-picker-inline").classList.remove("hidden");
    $("wiz-intro-engine").value = folder.details?.engine || "mysql";
    // Nothing was detected, so there's nothing yet to "edit" - "No, connect
    // and export" is what leads to manual entry on this path.
    $("wiz-ask-edit-btn").classList.add("hidden");
  }
  $("wiz-db-ask-has-dump").classList.remove("hidden");
}

$("wiz-ask-edit-btn").addEventListener("click", () => {
  hideAllDbSubPanels();
  showManualEntry(wizardFolders[wizardFolderIndex]);
});

// ---------- "Yes, I have a dump file": schema + file only, no connection ----------

$("wiz-has-dump-yes-btn").addEventListener("click", () => {
  const folder = wizardFolders[wizardFolderIndex];
  if (!folder.details) {
    // Nothing detected and no manual entry done yet - the inline engine
    // picker (goal 7) is the only thing we actually need to know before
    // filtering the file dialog below.
    const engine = $("wiz-intro-engine").value;
    folder.details = { engine, host: "", port: DEFAULT_PORT_BY_ENGINE[engine], database: "", username: "", password: "" };
  }
  hideAllDbSubPanels();
  wizardDbSubState = "pick-dump";
  $("wiz-dump-schema").value = folder.details.database || "";
  $("wiz-dump-file-path").value = "";
  $("wiz-dump-database-error").textContent = "";
  $("wiz-dump-error").textContent = "";
  $("wiz-db-pick-dump").classList.remove("hidden");
});

// Round 22 goal 7: filtered by the engine already known at this point (the
// dropdown above, or a real detection/manual entry) - mysql/postgres dumps
// this app (and mysqldump/pg_dump generally) produce are plain .sql text;
// MongoDB's own mongodump has no single-file default output at all
// (a directory of .bson files) *unless* run with --archive, and this
// project's own round-18 mongo export (crates/ls-dbsource/src/engines/
// mongo.rs) tars+gzips that directory into one .tar.gz - so a developer's
// own supplied dump is expected in one of those two real shapes, not
// guessed at.
const DUMP_FILE_FILTERS = {
  mysql: [{ name: "SQL dump", extensions: ["sql"] }],
  postgres: [{ name: "SQL dump", extensions: ["sql"] }],
  mongodb: [{ name: "MongoDB dump archive", extensions: ["gz", "tar", "archive"] }],
};

$("wiz-dump-browse-btn").addEventListener("click", async () => {
  const folder = wizardFolders[wizardFolderIndex];
  const engine = folder.details?.engine || "mysql";
  const file = await open({ directory: false, multiple: false, filters: DUMP_FILE_FILTERS[engine] });
  if (file) $("wiz-dump-file-path").value = file;
});

$("wiz-dump-confirm-btn").addEventListener("click", () => {
  const folder = wizardFolders[wizardFolderIndex];
  const schema = $("wiz-dump-schema").value.trim();
  const filePath = $("wiz-dump-file-path").value.trim();
  $("wiz-dump-database-error").textContent = "";
  $("wiz-dump-error").textContent = "";
  // Round 22 goal 2: contextual placement - the schema-name problem shows
  // right under that specific field, not folded into one generic message.
  if (!schema) {
    $("wiz-dump-database-error").textContent = "A schema/database name is required.";
    return;
  }
  if (!filePath) {
    $("wiz-dump-error").textContent = "Select a dump file.";
    return;
  }
  folder.details = { ...folder.details, database: schema };
  const dump = { schema, filePath, engine: folder.details.engine };
  folder.dump = dump;
  finishFolderOrGroup(folder, dump);
});

// ---------- "No, connect and export": manual entry only when nothing detected ----------

$("wiz-has-dump-no-btn").addEventListener("click", async () => {
  const folder = wizardFolders[wizardFolderIndex];
  hideAllDbSubPanels();
  if (folder.detected) {
    await connectAndBrowseSchemas(folder);
  } else {
    if (!folder.details) {
      const engine = $("wiz-intro-engine").value;
      folder.details = { engine, host: "", port: DEFAULT_PORT_BY_ENGINE[engine], database: "", username: "", password: "" };
    }
    showManualEntry(folder);
  }
});

$("wiz-manual-engine").addEventListener("change", () => {
  const engine = $("wiz-manual-engine").value;
  const portField = $("wiz-manual-port");
  // Only overwrite if it's still at some *other* engine's default - never
  // clobber a port the developer already typed on purpose.
  if (Object.values(DEFAULT_PORT_BY_ENGINE).includes(parseInt(portField.value, 10))) {
    portField.value = DEFAULT_PORT_BY_ENGINE[engine];
  }
});

function showManualEntry(folder) {
  wizardDbSubState = "manual";
  $("wiz-manual-engine").value = folder.details?.engine || "mysql";
  $("wiz-manual-host").value = folder.details?.host || "";
  $("wiz-manual-port").value = folder.details?.port || DEFAULT_PORT_BY_ENGINE[$("wiz-manual-engine").value];
  $("wiz-manual-database").value = folder.details?.database || "";
  $("wiz-manual-username").value = folder.details?.username || "";
  $("wiz-manual-password").value = folder.details?.password || "";
  $("wiz-manual-status").textContent = "";
  $("wiz-manual-error").textContent = "";
  $("wiz-manual-database-error").textContent = "";
  $("wiz-db-manual").classList.remove("hidden");
}

$("wiz-manual-continue-btn").addEventListener("click", async () => {
  const folder = wizardFolders[wizardFolderIndex];
  const details = {
    engine: $("wiz-manual-engine").value,
    host: $("wiz-manual-host").value.trim(),
    port: parseInt($("wiz-manual-port").value, 10) || DEFAULT_PORT_BY_ENGINE[$("wiz-manual-engine").value],
    database: $("wiz-manual-database").value.trim(),
    username: $("wiz-manual-username").value.trim(),
    password: $("wiz-manual-password").value,
  };
  $("wiz-manual-database-error").textContent = "";
  $("wiz-manual-error").textContent = "";
  if (!details.host || !details.username) {
    $("wiz-manual-error").textContent = "Host and username are required.";
    return;
  }
  $("wiz-manual-status").textContent = "Connecting…";
  $("wiz-manual-continue-btn").disabled = true;
  try {
    // Round 22 goal 3: list_db_schemas both proves the connection genuinely
    // works (real host/port/credentials round trip) *and* returns the real
    // schema list in one step - deliberately not test_db_connection (which
    // pins to details.database and would block progress on exactly the
    // typo'd-database-name case this goal exists to fix).
    const schemas = await invoke("list_db_schemas", { details });
    folder.details = details;
    folder.sourceLabel = "Manually entered";
    $("wiz-manual-status").textContent = "";
    hideAllDbSubPanels();
    showSchemaList(folder, schemas);
  } catch (err) {
    $("wiz-manual-status").textContent = "";
    // Round 22 goal 2: a database/schema-not-found failure is shown right
    // under that field specifically, not just a generic form-level error -
    // everything else (bad host, wrong password, connection refused) stays
    // a form-level error since no single field is specifically "wrong".
    const message = String(err);
    if (/database|schema/i.test(message) && /not found|does not exist|unknown/i.test(message)) {
      $("wiz-manual-database-error").textContent = message;
    } else {
      $("wiz-manual-error").textContent = message;
    }
  } finally {
    $("wiz-manual-continue-btn").disabled = false;
  }
});

async function connectAndBrowseSchemas(folder) {
  wizardDbSubState = "connecting";
  $("wiz-db-connecting").classList.remove("hidden");
  try {
    const schemas = await invoke("list_db_schemas", { details: folder.details });
    hideAllDbSubPanels();
    showSchemaList(folder, schemas);
  } catch (err) {
    hideAllDbSubPanels();
    wizardDbSubState = "ask";
    $("wiz-db-connect-error").textContent = String(err);
    showAskHasDump(folder);
  }
}

// ---------- round 22 goal 3: real schema/database browsing ----------

function showSchemaList(folder, schemas) {
  wizardDbSubState = "schemas";
  const ul = $("wiz-schemas-list");
  ul.innerHTML = "";
  const typedName = folder.details.database;
  for (const name of schemas) {
    const li = document.createElement("li");
    const label = document.createElement("label");
    const radio = document.createElement("input");
    radio.type = "radio";
    radio.name = "wiz-schema-choice";
    radio.value = name;
    if (name === typedName) radio.checked = true;
    label.appendChild(radio);
    label.appendChild(document.createTextNode(` ${name}`));
    li.appendChild(label);
    ul.appendChild(li);
  }
  // The typed/detected name might not be a real one (that's the whole
  // point of showing this list instead of trusting it) - default to the
  // first real result so Continue always has a valid choice, but nothing
  // is silently assumed to be right.
  if (!schemas.includes(typedName) && schemas.length > 0) {
    ul.querySelector("input[type=radio]").checked = true;
  }
  $("wiz-schemas-error").textContent = schemas.length === 0 ? "No databases were found on this server." : "";
  $("wiz-db-schemas").classList.remove("hidden");
}

$("wiz-schemas-continue-btn").addEventListener("click", async () => {
  const folder = wizardFolders[wizardFolderIndex];
  const chosen = $("wiz-schemas-list").querySelector("input[type=radio]:checked");
  if (!chosen) {
    $("wiz-schemas-error").textContent = "Select a database/schema to continue.";
    return;
  }
  folder.details = { ...folder.details, database: chosen.value };
  hideAllDbSubPanels();
  await loadTablesForExport(folder);
});

// ---------- round 22 goal 5: clearer table/collection selection ----------

async function loadTablesForExport(folder) {
  wizardDbSubState = "tables";
  try {
    const tables = await invoke("list_db_tables", { details: folder.details });
    const noun = dbNounFor(folder.details.engine);
    $("wiz-tables-database").textContent = folder.details.database;
    $("wiz-tables-noun").textContent = noun;
    $("wiz-tables-noun-2").textContent = noun;
    renderTablesList(tables);
    $("wiz-export-status").textContent = "";
    $("wiz-tables-error").textContent = "";
    $("wiz-db-tables").classList.remove("hidden");
  } catch (err) {
    $("wiz-schemas-error").textContent = String(err);
    $("wiz-db-schemas").classList.remove("hidden");
  }
}

function renderTablesList(tables) {
  const ul = $("wiz-tables-list");
  ul.innerHTML = "";
  for (const t of tables) {
    const li = document.createElement("li");
    li.className = "db-table-row";
    const label = document.createElement("label");
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = true;
    cb.dataset.table = t.name;
    label.appendChild(cb);
    const nameSpan = document.createElement("span");
    nameSpan.className = "db-table-name mono";
    nameSpan.textContent = t.name;
    label.appendChild(nameSpan);
    li.appendChild(label);
    const countSpan = document.createElement("span");
    countSpan.className = "hint-inline";
    countSpan.textContent = t.approx_row_count == null ? "" : `~${t.approx_row_count} rows`;
    li.appendChild(countSpan);
    ul.appendChild(li);
  }
}

$("wiz-tables-select-all-btn").addEventListener("click", () => {
  $("wiz-tables-list")
    .querySelectorAll("input[type=checkbox]")
    .forEach((cb) => (cb.checked = true));
});
$("wiz-tables-select-none-btn").addEventListener("click", () => {
  $("wiz-tables-list")
    .querySelectorAll("input[type=checkbox]")
    .forEach((cb) => (cb.checked = false));
});

$("wiz-export-btn").addEventListener("click", async () => {
  const folder = wizardFolders[wizardFolderIndex];
  const checked = Array.from($("wiz-tables-list").querySelectorAll("input[type=checkbox]:checked")).map(
    (cb) => cb.dataset.table
  );
  if (checked.length === 0) {
    $("wiz-tables-error").textContent = `Select at least one ${dbNounFor(folder.details.engine).replace(/s$/, "")}.`;
    return;
  }
  $("wiz-tables-error").textContent = "";
  $("wiz-export-status").textContent = `Exporting full ${dbNounFor(folder.details.engine)} content…`;
  $("wiz-export-btn").disabled = true;
  try {
    const result = await invoke("export_db_tables", { details: folder.details, tables: checked });
    const dump = { schema: folder.details.database, filePath: result.file_path, engine: folder.details.engine };
    folder.dump = dump;
    $("wiz-export-status").textContent = `Exported ${formatBytes(result.size_bytes)}.`;
    finishFolderOrGroup(folder, dump);
  } catch (err) {
    $("wiz-export-status").textContent = "";
    $("wiz-tables-error").textContent = String(err);
  } finally {
    $("wiz-export-btn").disabled = false;
  }
});

// Round 22 goal 6: the one place both "resolution paths" (a supplied dump
// file, or a fresh export) converge - if this is the shared group's first
// folder being resolved, remember the result so every other folder in the
// group is filled in automatically (see advanceDbWizard's own shortcut).
function finishFolderOrGroup(folder, dump) {
  if (wizardSharedMode && !wizardSharedResolved) {
    wizardSharedResolved = { details: folder.details, sourceLabel: folder.sourceLabel, dump };
  }
  wizardFolderIndex += 1;
  advanceDbWizard();
}

$("wiz-db-back-btn").addEventListener("click", () => {
  if (wizardFolderIndex === 0) {
    showWizardStep(wizardFolders.length > 1 ? "wiz-step-shared-db" : "wiz-step-needs-db");
    return;
  }
  wizardFolderIndex -= 1;
  advanceDbWizard();
});

// ---------- final step: summary + real Send ----------

function renderWizardReadyStep() {
  const ul = $("wiz-ready-summary");
  ul.innerHTML = "";
  for (const f of wizardFolders) {
    const li = document.createElement("li");
    let status;
    if (!f.needsDb) status = "no database";
    else if (f.dump) status = `database: ${escapeHtml(f.dump.schema)} (${escapeHtml(f.dump.filePath)})`;
    else status = "database: none selected";
    li.innerHTML = `<span class="mono">${escapeHtml(wizFolderLabel(f.path))}</span> — ${status}`;
    ul.appendChild(li);
  }
  showWizardStep("wiz-step-ready");
}

$("wiz-ready-back-btn").addEventListener("click", () => {
  const anyDb = wizardFolders.some((f) => f.needsDb);
  if (!anyDb) {
    showWizardStep("wiz-step-needs-db");
    return;
  }
  if (wizardSharedMode) {
    // Re-open the shared group's own flow for editing, rather than
    // silently bouncing straight back here via advanceDbWizard's own
    // already-resolved shortcut.
    wizardSharedResolved = null;
    wizardFolderIndex = wizardSharedGroupIndices[0] ?? 0;
  } else {
    wizardFolderIndex = wizardFolders.length - 1;
  }
  advanceDbWizard();
});

// Round 24: where the magic-link fallback page (web/) is actually hosted -
// see .github/workflows/pages.yml and the README's "Magic link" section
// for how it gets there and what one-time manual repo setting this URL
// depends on (GitHub Pages' project-page URL shape, derived from the repo
// owner/name, not something this app can discover at runtime).
const MAGIC_LINK_BASE_URL = "https://decypher0.github.io/LocalSync/";

function buildMagicLink(roomCode) {
  return `${MAGIC_LINK_BASE_URL}?code=${encodeURIComponent(roomCode)}`;
}

// Round 29: was a single `codeExpiryInterval` tied to "the one send on
// screen" (round 12). Now a session's own `codeExpiresAt` (an absolute
// timestamp, set once when its room code is issued) is the source of
// truth, and this one interval - running for the app's whole lifetime,
// not per-session - just re-renders whichever session happens to be
// active right now. Cheap: it's a no-op unless the active session is a
// send still waiting on a peer.
function renderSendCodeExpiry(session) {
  const el = $("send-code-expiry");
  if (!session.roomCode || !session.codeExpiresAt || session.status === "expired") {
    el.textContent = "";
    return;
  }
  const remaining = Math.round((session.codeExpiresAt - Date.now()) / 1000);
  if (remaining <= 0) {
    el.textContent = "Code expired.";
    el.className = "hint-inline error-inline";
  } else {
    const m = Math.floor(remaining / 60);
    const s = remaining % 60;
    el.textContent = `Expires in ${m}:${String(s).padStart(2, "0")} — share it before then.`;
    el.className = "hint-inline";
  }
}

// Round 30 goal A: real, found-in-testing bug - a sender whose code expired
// (or who hit "Start a new send…" for the same project after a failure) got
// a brand-new session/tab instead of this one being reused, so the same
// conceptual send piled up as duplicate tabs. Root cause: nothing ever
// flipped a stalled "connecting" session to a distinct terminal status once
// its code timed out - it just sat there still *looking* connecting/healthy
// forever (only this tick's countdown text, visible only while it happened
// to be the active tab, ever said otherwise), so there was nothing for a
// person to "retry" - "Start a new send…" was the only button that seemed
// to do anything, and it always builds a fresh session because it has no
// notion of "this is the same project as that other tab."
//
// Fix: this sweep (not just the active-session special case above) marks
// every still-"connecting" send session whose code has timed out as
// "expired" - a real status with its own tab icon/color (see
// SESSION_STATUS_ICON/styles.css) - and renderSendSessionDetail's own Retry
// button (wired to retrySendSession below) re-issues a fresh code on that
// *same* session object/tab, so retrying never creates a second tab for the
// same send again.
setInterval(() => {
  let anyExpired = false;
  for (const s of sessions.values()) {
    if (s.kind === "send" && s.status === "connecting" && s.codeExpiresAt && Date.now() >= s.codeExpiresAt) {
      s.status = "expired";
      anyExpired = true;
    }
  }
  if (anyExpired) renderSessionTabs();
  const session = activeSession();
  if (!session || session.kind !== "send") return;
  if (anyExpired) renderActiveSession();
  else renderSendCodeExpiry(session);
}, 1000);

// Round 29 goal B1: paints the shared send-detail DOM from one session
// object - called whenever that session is the one currently selected
// (right after creating/updating it, and from setActiveSession's own
// renderActiveSession). Every other session's data stays untouched in its
// own object until it's selected.
function renderSendSessionDetail(session) {
  $("send-code-wrap").classList.toggle("hidden", !session.roomCode);
  $("send-room-code-display").textContent = session.roomCode || "";
  renderSendCodeExpiry(session);

  const showProgress = !session.cloudDrop && (session.status === "active" || (session.status === "done" && session.progressTotal > 0));
  $("send-progress-wrap").classList.toggle("hidden", !showProgress);
  if (showProgress) {
    setProgress("send-progress-bar", session.progressTotal ? (session.progressBytes / session.progressTotal) * 100 : 0);
    $("send-progress-label").textContent =
      session.status === "done" ? "Sent." : `Sending… ${formatBytes(session.progressBytes)} / ${formatBytes(session.progressTotal)}`;
  }

  $("send-result").textContent = session.resultText || "";
  $("send-error").textContent = session.errorText || "";

  // Round 30 goal A: Retry - only offered where it can actually do
  // something (a send that has somewhere to retry to, i.e. not Cloud drop,
  // which either already finished or never became a session) and hidden
  // once the same session has since gone on to something else, e.g. right
  // after a Retry itself resolves into "active"/"done".
  const canRetry = !session.cloudDrop && !!session.retryFolders && (session.status === "expired" || session.status === "error");
  $("send-retry-wrap").classList.toggle("hidden", !canRetry);
  if (canRetry) {
    $("send-retry-btn").disabled = session.busy;
    $("send-retry-status").textContent = session.busy ? "Getting a new code…" : "";
  }
}

// Round 24: distinct from each other on purpose - a teammate who already
// has LocalSync installed only needs the bare code (unchanged behavior,
// just given a real button instead of relying on the code display's own
// user-select:all); someone who doesn't has nothing useful to do with a
// bare code until they've installed the app, which is exactly what the
// magic link's fallback page (web/) walks them through.
async function copyToClipboard(text, statusIfOk) {
  try {
    await writeClipboardText(text);
    $("copy-status").textContent = statusIfOk;
  } catch (err) {
    $("copy-status").textContent = `Couldn't copy automatically (${err}) — select the code above and copy it manually.`;
  }
}
$("copy-code-btn").addEventListener("click", () => {
  const roomCode = activeSession()?.roomCode;
  if (roomCode) copyToClipboard(roomCode, "Code copied.");
});
$("copy-link-btn").addEventListener("click", () => {
  const roomCode = activeSession()?.roomCode;
  if (roomCode) copyToClipboard(buildMagicLink(roomCode), "Link copied.");
});

// Resets all wizard state, for the next send after this one finishes (or
// after a failure/cancel the developer wants to redo from scratch). Doesn't
// itself decide which step to show - openSendWizard (the only place that
// reveals the modal) always does that explicitly right after calling this.
function resetSendWizard() {
  wizardFolders = [];
  wizardFolderIndex = 0;
  wizardSharedMode = false;
  wizardSharedGroupIndices = [];
  wizardSharedResolved = null;
  wizardSelectedDevice = null;
  $("wiz-nearby-selected").classList.add("hidden");
  renderWizardFolderList();
  $("wiz-folders-error").textContent = "";
  $("wiz-send-error").textContent = "";
}

// ---------- round 23: Cloud drop retention picker ----------
// Cloud drop itself (the toggle that used to live here as a Step-4
// checkbox) moved to Step 1 - see wiz-mode-cloud/wiz-cloud-drop-wrap - and
// its retention sub-controls (also now rendered on Step 1, inside
// wiz-cloud-drop-wrap) are still wired up the same way, right here.
$("retention-custom").addEventListener("change", () => {
  $("retention-custom-wrap").classList.toggle("hidden", !$("retention-custom").checked);
});
document.querySelectorAll('input[name="cloud-retention"]').forEach((r) =>
  r.addEventListener("change", () => $("retention-custom-wrap").classList.toggle("hidden", r.value !== "custom" || !r.checked))
);

/// Builds the `RetentionChoiceDto` `start_cloud_drop_session` expects.
/// 24h and custom both collapse to `After { after_rfc3339 }` — they're the
/// same case on the Rust side (see `ls_clouddrop::retention::Retention`'s
/// doc comment), just different ways of picking the instant.
function retentionChoiceDto() {
  if ($("retention-24h").checked) {
    const at = new Date(Date.now() + 24 * 60 * 60 * 1000);
    return { kind: "after", afterRfc3339: at.toISOString() };
  }
  if ($("retention-custom").checked) {
    const raw = $("retention-custom-datetime").value;
    if (!raw) throw new Error("Pick a date/time for the custom retention option.");
    return { kind: "after", afterRfc3339: new Date(raw).toISOString() };
  }
  return { kind: "deleteAfterDownload" };
}

// Round 29 goal A2/B1: every click creates its own session object instead
// of overwriting one shared set of globals/DOM - this is the actual fix for
// "starting a second, different send while one is already active doesn't
// work" (see tests/concurrent_multi_session_test.rs for the backend-side
// proof this was always safe to do). The button is only disabled for the
// brief `start_send_session`/`start_cloud_drop_session` round trip; once a
// session exists it's freed again, so a second Send click can start a
// genuinely concurrent session while the first's transfer is still running.
$("send-btn").addEventListener("click", async () => {
  $("wiz-send-error").textContent = "";

  if (wizardFolders.length === 0) {
    $("wiz-send-error").textContent = "Select at least one project folder.";
    return;
  }

  // Round 20 goal 3: the mode chosen explicitly in the wizard's own first
  // step, not Settings' (that toggle now only matters for Receive) - see
  // wizRelayMode()'s own doc comment for why these are deliberately
  // separate accessors onto the same underlying persisted default.
  const mode = wizRelayMode();
  const url = wizRelayUrl();
  if (mode === "remote" && !url) {
    $("wiz-send-error").textContent = "Remote relay URL is required for Remote relay mode.";
    return;
  }
  const isCloudDrop = mode === "cloud";

  const folderLabels = wizardFolders.map((f) => wizFolderLabel(f.path));
  const sessionFolders = wizardFolders.map((f) => ({ path: f.path, dump: f.dump ? { schema: f.dump.schema, engine: f.dump.engine } : null }));
  // resetSendWizard() below reassigns the module-level `wizardFolders` to a
  // fresh empty array (so the wizard can be reopened for another session
  // right away) - this keeps a reference to the array actually used by
  // *this* send, which buildWizardFoldersPayload still needs afterward.
  const foldersForPayload = wizardFolders;

  $("send-btn").disabled = true;

  // Round 23: Cloud drop bundles+uploads a single project (see
  // commands::start_cloud_drop_session's doc comment on why only
  // wizardFolders[0] - same documented boundary push_update/pull-requests
  // already have for a wizard-originated send) and hosts the same kind of
  // room code, but never opens a bulk-transfer channel - the whole thing
  // resolves once the upload is done, there's no separate progress phase.
  if (isCloudDrop) {
    try {
      const retention = retentionChoiceDto();
      // Cloud drop's own signaling handshake always uses "local" (see
      // wizRelayMode's comment - "cloud" isn't a mode start_cloud_drop_session
      // itself understands, and there's no remaining UI here to pick Remote
      // relay for it specifically now that this is one exclusive Step-1
      // choice instead of an add-on to Local/Remote).
      const info = await invoke("start_cloud_drop_session", {
        mode: "local",
        relayUrl: null,
        projectPath: foldersForPayload[0].path,
        retention,
      });
      // Round 20 goal 4: same reason the non-Cloud-drop path below closes
      // the modal on success - the room code renders in the session tab's
      // detail viewport, behind the modal's backdrop, and would be
      // invisible if the wizard stayed open.
      closeSendWizard();
      const session = newSession("send", info.room_code, folderLabels[0]);
      session.roomCode = info.room_code;
      session.cloudDrop = true;
      session.folders = [sessionFolders[0]];
      session.resultText = `Uploaded to Drive as ${info.file_id}. Waiting for the receiver to request access…`;
      addSession(session);
      endSession(session, "done");
      resetSendWizard();
    } catch (err) {
      $("wiz-send-error").textContent = String(err);
    } finally {
      $("send-btn").disabled = false;
    }
    return;
  }

  // Round 37 goal 2: a device selected from the nearby-devices list -
  // connects directly to that device's already-hosted relay instead of
  // hosting a fresh one and showing a room code (there's nothing to show;
  // the recipient isn't pasting anything). require_accept:true is what
  // makes this safe - see share_snapshot_wizard's own doc comment and
  // ls_net::ControlMessage::ConnectionRequest for the actual gate.
  if (mode === "local" && wizardSelectedDevice) {
    const device = wizardSelectedDevice;
    // Session id is a fresh value per attempt, deliberately *not*
    // device.room_id - that's the discoverable device's own standing id,
    // unchanged across repeat sends to it, so reusing it as the session/tab
    // key would collide with a later send to the same device. The backend's
    // actual share-progress correlation id (see share_snapshot_wizard) IS
    // device.room_id though - matched against directly below, not against
    // session.id.
    const session = newSession("send", `discover-${device.room_id}-${Date.now()}`, folderLabels.join(", "));
    session.folders = sessionFolders;
    closeSendWizard();
    addSession(session);
    resetSendWizard();
    $("send-btn").disabled = false;

    session.progressUnlisten = await listen("share-progress", (evt) => {
      if (evt.payload.session_id !== device.room_id) return;
      const wasConnecting = session.status === "connecting";
      session.status = "active";
      session.progressBytes = evt.payload.bytes;
      session.progressTotal = evt.payload.total;
      if (session.id !== activeSessionId) return;
      if (wasConnecting) renderActiveSession();
      else renderSendSessionDetail(session);
    });

    try {
      const folders = buildWizardFoldersPayload(foldersForPayload);
      const snapshotId = await invoke("share_snapshot_wizard", {
        folders,
        roomCode: device.room_id,
        signalingUrl: `ws://${device.host}:${device.port}`,
        requireAccept: true,
        senderName: deviceName() || "Someone",
      });
      session.resultText = `Sent as ${snapshotId}`;
      endSession(session, "done");
      refreshReceivers();
    } catch (err) {
      session.errorText = String(err);
      endSession(session, "error");
    }
    return;
  }

  const session = newSession("send", null, folderLabels.join(", "));
  session.folders = sessionFolders;
  // Round 30 goal A: kept so Retry (retrySendSession, below) can redo just
  // this part later without the person going back through folder selection
  // or the database wizard - see this session field's own comment in
  // newSession.
  session.retryFolders = foldersForPayload;
  session.retryMode = mode;
  session.retryUrl = url;

  try {
    await performSendAttempt(session, mode, url, foldersForPayload, (info) => {
      // Round 29: keyed by info.room_id, not info.room_code - the backend's
      // own share_snapshot_wizard/share_snapshot take a `room_code`
      // *parameter* that's actually always called with room_id (see
      // start_send_session's own doc comment: "room_code is just what's
      // shown to the user... calls share_snapshot(..., room_id, ...)"), and
      // that's the value baked into both the "share-progress" event's
      // session_id and connected_receivers' peer_id.
      session.id = info.room_id;
      // Round 20 goal 4: the modal's job ends once there's a real room code
      // to show - close it now so the code/progress below render in the
      // session tab, exactly where they always have.
      closeSendWizard();
      addSession(session);
      resetSendWizard();
      $("send-btn").disabled = false; // free to start another send now - this one keeps running in its own tab
    });
  } catch (err) {
    // performSendAttempt only ever rethrows here when it failed before a
    // session/tab existed to show the error on (start_send_session itself)
    // - anything past that point is handled inside it instead
    // (endSession("error") on that same session). The wizard is still open,
    // so the error belongs there.
    $("wiz-send-error").textContent = String(err);
    $("send-btn").disabled = false;
  }
});

// Round 30 goal A: the one real attempt loop behind both the initial Send
// click above and Retry (retrySendSession) below - issuing a room code,
// waiting for a peer, and reporting the result all work identically either
// way, the only difference is whether `session` is brand new or already an
// existing tab being redone in place. `onRoomIdKnown` is where the two
// callers differ: the initial send turns a not-yet-a-tab session into a
// real one (closes the wizard, calls addSession); retry instead re-keys the
// *same*, already-open tab under the fresh room id, exactly once
// start_send_session hands one back - see retrySendSession's own comment
// for why that in-place re-keying is what actually fixes the duplicate-tab
// bug this round found.
async function performSendAttempt(session, mode, url, foldersForPayload, onRoomIdKnown) {
  let idAssigned = false;
  try {
    // "local": hosts an embedded relay + derives a LAN-IP-encoded room code
    // (unchanged round-8 behavior). "remote": a relay is already running
    // elsewhere (see README) - only a bare room id is generated, and it IS
    // the whole paste-able code, since both apps already share the relay URL.
    const info = await invoke("start_send_session", { mode, relayUrl: mode === "remote" ? url : null });
    session.roomCode = info.room_code;
    session.codeExpiresAt = Date.now() + info.code_expires_in_seconds * 1000;
    session.status = "connecting";
    session.errorText = "";
    session.resultText = "";
    session.progressBytes = 0;
    session.progressTotal = 0;
    session.endedAt = null; // clears a prior attempt's terminal state, if any, so endSession() below isn't a no-op
    onRoomIdKnown(info);
    idAssigned = true;

    // Round 29: filtered by session_id (now threaded through every Progress
    // emit in commands.rs) so concurrent sends' identically-named
    // "share-progress" events never cross-update the wrong session. Any
    // listener from a previous attempt on this same session (a retry) is
    // torn down first - it's watching for a session_id (the old room id)
    // that will never be emitted again.
    if (session.progressUnlisten) session.progressUnlisten();
    session.progressUnlisten = await listen("share-progress", (evt) => {
      if (evt.payload.session_id !== session.id) return;
      const wasConnecting = session.status === "connecting";
      session.status = "active";
      session.codeExpiresAt = null; // a peer connected - the code did its job
      session.progressBytes = evt.payload.bytes;
      session.progressTotal = evt.payload.total;
      if (session.id !== activeSessionId) return;
      // Full header+body re-render only for the one-time "connecting" ->
      // "active" flip (updates the tab's status text/icon); every later
      // tick on an already-active session only needs the progress bar.
      if (wasConnecting) renderActiveSession();
      else renderSendSessionDetail(session);
    });

    // Round 22 found `engine` silently dropped here; round 25 found
    // `filePath` sent instead of the `file_path` Rust's DumpPlanDto
    // actually declares - see wizard-payload.js's own comment for the full
    // root cause. Extracted into its own file specifically so this exact
    // translation step - the one part of the whole wizard flow no Rust
    // test can reach, since every one of them calls
    // commands::share_snapshot_wizard directly - finally has a real,
    // automated regression test (test-wizard-payload.js).
    const folders = buildWizardFoldersPayload(foldersForPayload);
    const snapshotId = await invoke("share_snapshot_wizard", {
      folders,
      roomCode: info.room_id,
      signalingUrl: info.signaling_url,
      // Round 37: only a discovery-initiated send (picking a device from
      // the nearby-devices list - see sendToNearbyDevice) sets these to
      // require the recipient's explicit Accept/Reject before anything is
      // sent. A manually-entered room code doesn't need it - clicking
      // Receive is already that person's own consent to this connection.
      requireAccept: false,
      senderName: "",
    });
    session.resultText = `Sent as ${snapshotId}`;
    endSession(session, "done");
    refreshReceivers(); // this send may have just added a new roster entry
  } catch (err) {
    // No session/tab exists yet (start_send_session itself failed, before
    // onRoomIdKnown ran) - let the caller decide where to show this: the
    // still-open wizard for a first send, or the same tab being retried for
    // a retry (see the initial send-btn handler and retrySendSession).
    if (!idAssigned) throw err;
    session.endedAt = null; // a retry redoing an already-"error"/"expired" session, see endSession's guard
    session.errorText = String(err);
    endSession(session, "error");
  }
}

// Round 30 goal A: the actual fix for "Start a new send…" spawning a
// duplicate tab when retrying a failed/expired send for the same project -
// this re-runs performSendAttempt on the *same* session object this button
// already lives on, re-keying it (below) to whatever fresh room id the new
// code uses instead of ever creating a second session/tab. Offered only
// where renderSendSessionDetail's own canRetry check allows it (an expired
// or failed non-Cloud-drop send that still has its original folder plan).
async function retrySendSession(session) {
  if (session.busy || !session.retryFolders) return;
  const previousId = session.id;
  session.busy = true;
  if (session.id === activeSessionId) renderActiveSession();
  try {
    await performSendAttempt(session, session.retryMode, session.retryUrl, session.retryFolders, (info) => {
      // Re-key this same tab in place under the new room id - the actual
      // fix: no new entry is ever added to `sessions`, so retrying never
      // shows up as a second tab for the same send.
      if (sessions.has(previousId)) sessions.delete(previousId);
      session.id = info.room_id;
      sessions.set(session.id, session);
      if (activeSessionId === previousId) activeSessionId = session.id;
      recordSessionHistory(session);
      renderSessionTabs();
    });
  } catch (err) {
    // A retry that fails again on this same, still-"error"/"expired"
    // session would otherwise hit endSession's own already-ended guard
    // (endedAt was already set by whatever the *previous* attempt ended
    // with) and silently no-op instead of recording this attempt's own
    // failure - clear it first so this failure is the one that sticks.
    session.endedAt = null;
    session.errorText = String(err);
    endSession(session, "error");
  } finally {
    session.busy = false;
    if (session.id === activeSessionId) renderActiveSession();
  }
}

$("send-retry-btn").addEventListener("click", () => {
  const session = activeSession();
  if (session) retrySendSession(session);
});

// ---------- connected receivers roster (sender-side) ----------
//
// Round 20 goal 2: a real, confirmed-broken-by-direct-use bug - clicking
// Refresh was wired to a real handler calling a real command (nothing was
// actually missing), but gave zero visible feedback when the result was
// unchanged from before (the common case: usually zero or the same
// receivers), which reads exactly like "the button does nothing". `reportStatus`
// mirrors the same silent-vs-explicit pattern round 16's own
// runUpdateCheck already uses for the same reason.
async function refreshReceivers(reportStatus) {
  $("receivers-error").textContent = "";
  if (reportStatus) $("receivers-refresh-status").textContent = "Refreshing…";
  try {
    const list = await invoke("list_connected_receivers");
    $("receivers-wrap").classList.remove("hidden");
    const ul = $("receivers-list");
    ul.innerHTML = "";
    for (const r of list) {
      const li = document.createElement("li");
      li.innerHTML = `
        <span class="mono">${escapeHtml(r.peer_id)}</span>
        <span class="hint-inline">connected ${escapeHtml(r.connected_at)}</span>
        <button class="ghost-btn push-btn" type="button" data-peer="${escapeHtml(r.peer_id)}"><svg class="icon"><use href="#icon-send"></use></svg> Push update</button>
        <span class="hint push-status"></span>
      `;
      ul.appendChild(li);
    }
    if (reportStatus) {
      $("receivers-refresh-status").textContent =
        list.length === 0 ? "No receivers connected." : `${list.length} connected.`;
    }
  } catch (err) {
    $("receivers-error").textContent = String(err);
    if (reportStatus) $("receivers-refresh-status").textContent = "";
  }
}
$("receivers-refresh-btn").addEventListener("click", () => refreshReceivers(true));

// Delegated so newly-rendered rows don't need their own listener wiring.
$("receivers-list").addEventListener("click", async (e) => {
  const btn = e.target.closest(".push-btn");
  if (!btn) return;
  const peerId = btn.dataset.peer;
  const status = btn.nextElementSibling;
  btn.disabled = true;
  status.textContent = "";
  status.className = "hint push-status";
  try {
    const snapshotId = await invoke("push_update", { peerId });
    status.textContent = `Pushed ${snapshotId}`;
    status.className = "result push-status";
  } catch (err) {
    status.textContent = String(err);
    status.className = "error push-status";
  } finally {
    btn.disabled = false;
  }
});

// ---------- receive ----------
// Round 29: the receiver-side counterpart to send-btn's session model
// above. `lastReceiveSessionId` exists only for `snapshot-updated` (a
// pushed update arriving on the one background `state.outgoing_conn` -
// still a real, documented singleton on the Rust side, see
// commands.rs::listen_for_pushed_updates's doc comment; this round's fix
// is scoped to concurrent *sends*, per the actual bug report) - it has no
// room-code/snapshot-id of its own to key off, so this just remembers
// which receive tab most recently connected to a sender, matching the
// backend's own single-upstream-connection model rather than inventing a
// multi-upstream one the backend doesn't have.
let lastReceiveSessionId = null;

function applyReviewInfoToSession(session, info) {
  session.snapshotId = info.snapshot_id;
  session.senderPubkeyHex = info.sender_pubkey_hex;
  session.manifest = info.manifest;
  session.diff = info.diff;
  session.recognizedPeer = info.recognized_peer;
  session.title = info.manifest.project_name;
  session.status = "reviewing";
  session.errorText = "";
}

// Round 29 goal B1: paints the shared receive-detail DOM from one session
// object, the receive-side counterpart to renderSendSessionDetail. Ephemeral
// per-button status text (ask-update/reject/run/stop) is always cleared on
// entry - it belongs to whichever DOM node is on screen right now, and must
// never bleed a different session's leftover text onto this one.
function renderReceiveSessionDetail(session) {
  $("ask-update-status").textContent = "";
  $("reject-error").textContent = "";
  $("stop-error").textContent = "";
  $("run-btn").disabled = session.busy;
  $("reject-btn").disabled = session.busy;
  $("ask-update-btn").disabled = session.busy;
  $("stop-btn").disabled = session.busy;

  const showProgress = session.status === "active";
  $("receive-progress-wrap").classList.toggle("hidden", !showProgress);
  if (showProgress) {
    setProgress("receive-progress-bar", session.progressTotal ? (session.progressBytes / session.progressTotal) * 100 : 0);
    $("receive-progress-label").textContent = `Receiving… ${formatBytes(session.progressBytes)} / ${formatBytes(session.progressTotal)}`;
  }
  $("receive-error").textContent = session.status === "error" ? session.errorText : "";

  $("review-panel").classList.toggle("hidden", session.status !== "reviewing");
  $("session-panel").classList.toggle("hidden", session.status !== "running");

  if (session.status === "reviewing") renderReviewPanel(session);
  if (session.status === "running") renderRunningPanel(session);
}

function renderReviewPanel(session) {
  const m = session.manifest;
  $("m-project").textContent = m.project_name;
  $("m-commit").textContent = m.git_commit;
  $("m-parent").textContent = m.git_parent_commit || "(none — initial snapshot)";
  $("m-services").textContent = m.services.map((s) => `${s.name} (${s.image_or_build})`).join(", ") || "none";

  // Identity recognition is purely informational - it never affects what's
  // shown below or what Run/Reject do. See commands::finalize_received_snapshot.
  $("peer-remember-done").classList.add("hidden");
  $("peer-remember-error").textContent = "";
  $("peer-remember-name").value = "";
  if (session.recognizedPeer) {
    $("peer-recognized").classList.remove("hidden");
    $("peer-new").classList.add("hidden");
    $("peer-recognized-name").textContent = session.recognizedPeer.name;
    $("peer-recognized-since").textContent = `(first seen ${session.recognizedPeer.first_seen})`;
  } else {
    $("peer-recognized").classList.add("hidden");
    $("peer-new").classList.remove("hidden");
  }

  const diff = session.diff;
  const fileCount = diff.entries.length;
  $("diff-totals").innerHTML =
    `<span class="file-count">${fileCount} file${fileCount === 1 ? "" : "s"} changed</span>` +
    `<span class="ins-del">+${diff.total_insertions} / -${diff.total_deletions}</span>`;

  const body = $("diff-body");
  body.innerHTML = "";

  // Group by directory (everything before the last "/" in the path) so a
  // real project with files spread across many dirs doesn't render as one
  // flat wall. Files with no "/" (repo-root files) get their own bucket.
  const groups = new Map();
  for (const entry of diff.entries) {
    const slash = entry.path.lastIndexOf("/");
    const dir = slash === -1 ? "(root)" : entry.path.slice(0, slash);
    if (!groups.has(dir)) groups.set(dir, []);
    groups.get(dir).push(entry);
  }
  const dirs = [...groups.keys()].sort((a, b) =>
    a === "(root)" ? -1 : b === "(root)" ? 1 : a.localeCompare(b)
  );

  // Small diffs (<=12 files total): open everything, nothing to hide.
  // Larger diffs: only auto-collapse the directories that are themselves
  // large (>5 files) — small groups stay open since they're cheap to scan.
  const openAll = fileCount <= 12;

  for (const dir of dirs) {
    const entries = groups.get(dir);
    const details = document.createElement("details");
    details.className = "diff-group";
    details.open = openAll || entries.length <= 5;

    const summary = document.createElement("summary");
    summary.innerHTML =
      `<span class="diff-group-name">${escapeHtml(dir)}</span>` +
      `<span class="diff-group-count">${entries.length} file${entries.length === 1 ? "" : "s"}</span>`;
    details.appendChild(summary);

    const table = document.createElement("table");
    table.className = "diff-table";
    table.innerHTML = "<thead><tr><th>File</th><th>Change</th><th>+</th><th>-</th></tr></thead><tbody></tbody>";
    const tbody = table.querySelector("tbody");
    for (const entry of entries) {
      // Show the path relative to its group heading — the directory is
      // already shown once, in the summary.
      const name = dir === "(root)" ? entry.path : entry.path.slice(dir.length + 1);
      const tr = document.createElement("tr");
      tr.innerHTML = `
        <td class="path">${escapeHtml(name)}</td>
        <td><span class="badge ${entry.change_type}">${entry.change_type}</span></td>
        <td class="ins">+${entry.insertions}</td>
        <td class="del">-${entry.deletions}</td>
      `;
      tbody.appendChild(tr);
    }
    details.appendChild(table);
    body.appendChild(details);
  }

  // Round 29: restores whatever this session's own in-flight/failed Run
  // attempt looked like, so switching tabs away mid-Run and back doesn't
  // lose the log or silently drop the error.
  $("run-error").textContent = session.runErrorText || "";
  const showRunProgress = session.runInProgress || !!session.runErrorText;
  $("run-progress-wrap").classList.toggle("hidden", !showRunProgress);
  if (showRunProgress) {
    $("run-log").textContent = session.runLogText || "";
    document.querySelector("#run-progress-wrap .spinner")?.classList.toggle("hidden", !session.runInProgress);
    $("run-progress-label").textContent = session.runInProgress ? "Starting containers…" : "Failed — see details below.";
  }
}

function renderRunningPanel(session) {
  $("s-project").textContent = session.manifest.project_name;
  const cacheEl = $("s-cache");
  if (session.dbCacheHit) {
    cacheEl.textContent = "Hit — reused seeded volume (fast start)";
    cacheEl.className = "v cache-hit";
  } else {
    cacheEl.textContent = "Miss — cold start, seeded from scratch";
    cacheEl.className = "v cache-cold";
  }

  const list = $("s-ports");
  list.innerHTML = "";
  for (const [service, ports] of session.servicePorts) {
    const hostPort = ports.split(":")[0];
    const li = document.createElement("li");
    li.innerHTML = `${escapeHtml(service)} — <a href="#" data-url="http://localhost:${hostPort}">http://localhost:${hostPort}</a> (${escapeHtml(ports)})`;
    list.appendChild(li);
  }
}

/// Creates a session straight from an already-resolved IncomingSnapshotInfo -
/// used by the dev preload shortcut and by a pushed update that arrives with
/// no known prior session to attach to (e.g. after a restart).
function startReceiveSessionFromInfo(info) {
  const session = newSession("receive", info.snapshot_id, info.manifest.project_name);
  applyReviewInfoToSession(session, info);
  addSession(session);
  lastReceiveSessionId = session.id;
  return session;
}

$("cloud-drop-receive-toggle").addEventListener("change", () => {
  $("cloud-drop-receive-hint").classList.toggle("hidden", !$("cloud-drop-receive-toggle").checked);
});

// Round 29 goal A2/B1: every click gets its own session/tab instead of
// overwriting one shared set of globals/DOM, the same fix as send-btn
// above. receive-btn is never disabled across the whole flow - once the
// room code is typed and cleared, there's nothing left for a second click
// to race against; two concurrent receives just become two tabs.
$("receive-btn").addEventListener("click", async () => {
  const roomCode = $("receive-room-code").value.trim();
  $("receive-start-error").textContent = "";
  if (!roomCode) {
    $("receive-start-error").textContent = "Room code is required.";
    return;
  }

  // Round 25 fix: a receiver never picks a connection mode - mode
  // selection is a *sender* decision (Settings' relayMode()/relayUrl()
  // exist for Send's own remote-mode default, and the wizard has its own
  // separate choice on top of that; neither describes what a given pasted
  // code actually needs). decode_room_code itself now figures that out
  // from the code's own shape - see its doc comment. relayUrl() is passed
  // through unconditionally: harmless if the code turns out to be
  // local-shaped (never consulted in that case), and exactly what's
  // needed if it turns out to be remote-shaped, without a blocking
  // upfront guess about which one it'll be.
  const url = relayUrl();

  // Round 23: waits for the sender's Accept/Reject over the control
  // channel, then downloads straight from Drive - no `receive-progress`
  // events fire for this path (there's no P2P bulk transfer to report
  // progress on). The tab appears immediately (before the sender has even
  // responded) so this wait doesn't block starting anything else.
  if ($("cloud-drop-receive-toggle").checked) {
    const session = newSession("receive", `clouddrop-${roomCode}-${Date.now()}`, "Cloud drop request");
    addSession(session);
    $("receive-room-code").value = "";
    try {
      const outcome = await invoke("request_cloud_drop_access", { code: roomCode, relayUrl: url || null });
      if (!outcome.accepted) {
        session.errorText = "The sender declined this request.";
        endSession(session, "error");
      } else {
        applyReviewInfoToSession(session, outcome.info);
        renderSessionTabs();
        if (session.id === activeSessionId) renderActiveSession();
      }
    } catch (err) {
      session.errorText = String(err);
      endSession(session, "error");
    }
    return;
  }

  let session = null;
  try {
    const decoded = await invoke("decode_room_code", { code: roomCode, relayUrl: url || null });
    session = newSession("receive", decoded.room_id, `Receiving (${roomCode})`);
    addSession(session);
    $("receive-room-code").value = "";

    // Round 29: filtered by session_id, same reasoning as share-progress
    // above - concurrent receives never cross-update each other's tab.
    session.progressUnlisten = await listen("receive-progress", (evt) => {
      if (evt.payload.session_id !== session.id) return;
      const wasConnecting = session.status === "connecting";
      session.status = "active";
      session.progressBytes = evt.payload.bytes;
      session.progressTotal = evt.payload.total;
      if (session.id !== activeSessionId) return;
      if (wasConnecting) renderActiveSession();
      else renderReceiveSessionDetail(session);
    });

    lastReceiveSessionId = session.id;
    // This only verifies + diffs. Nothing from the snapshot executes until
    // the user reviews it below and clicks Run.
    const info = await invoke("receive_snapshot", { roomCode: decoded.room_id, signalingUrl: decoded.signaling_url });
    applyReviewInfoToSession(session, info);
    renderSessionTabs();
    if (session.id === activeSessionId) renderActiveSession();
  } catch (err) {
    if (session) {
      session.errorText = String(err);
      endSession(session, "error");
    } else {
      // Failed before a session existed (decode_room_code itself) - the
      // idle form's own error, since there's no tab to attach it to.
      $("receive-start-error").textContent = String(err);
    }
  }
});

$("peer-remember-btn").addEventListener("click", async () => {
  $("peer-remember-error").textContent = "";
  const session = activeSession();
  const name = $("peer-remember-name").value.trim();
  if (!name || !session || !session.senderPubkeyHex) return;
  $("peer-remember-btn").disabled = true;
  try {
    await invoke("remember_peer", { pubkeyHex: session.senderPubkeyHex, name });
    $("peer-remember-done").classList.remove("hidden");
  } catch (err) {
    $("peer-remember-done").classList.add("hidden");
    $("peer-remember-error").textContent = String(err);
  } finally {
    $("peer-remember-btn").disabled = false;
  }
});

$("ask-update-btn").addEventListener("click", async () => {
  const status = $("ask-update-status");
  status.textContent = "";
  status.className = "hint";
  const session = activeSession();
  if (!session) return;
  session.busy = true;
  $("ask-update-btn").disabled = true;
  try {
    await invoke("send_pull_request");
    status.textContent = "Request sent.";
  } catch (err) {
    status.textContent = String(err);
    status.className = "error";
  } finally {
    session.busy = false;
    $("ask-update-btn").disabled = false;
  }
});

// Connection-level "no" - discards the held snapshot without ever running
// it, independent of (and no shortcut past) the Run button's own gating.
$("reject-btn").addEventListener("click", async () => {
  $("reject-error").textContent = "";
  const session = activeSession();
  if (!session || !session.snapshotId) return;
  session.busy = true;
  $("reject-btn").disabled = true;
  try {
    await invoke("reject_snapshot", { snapshotId: session.snapshotId });
    endSession(session, "done"); // also re-renders the active view
  } catch (err) {
    $("reject-error").textContent = String(err);
  } finally {
    session.busy = false;
    $("reject-btn").disabled = false;
  }
});

// Round 21: the chevron direction itself communicates expanded/collapsed,
// same as the label text always did - kept as innerHTML (fixed literals
// only, nothing dynamic ever reaches this button) rather than duplicating
// two full icon+label strings at every call site.
function setDetailsToggleExpanded(expanded) {
  $("run-details-toggle").innerHTML = expanded
    ? '<svg class="icon"><use href="#icon-chevron-up"></use></svg> Hide details'
    : '<svg class="icon"><use href="#icon-chevron-down"></use></svg> Show details';
  $("run-details-toggle").setAttribute("aria-expanded", String(expanded));
}

// Collapsed by default — toggling only shows/hides the log already
// accumulated in #run-log, doesn't (re)fetch anything.
$("run-details-toggle").addEventListener("click", () => {
  const expanded = !$("run-log").classList.contains("hidden");
  $("run-log").classList.toggle("hidden");
  setDetailsToggleExpanded(!expanded);
});

$("run-btn").addEventListener("click", async () => {
  const session = activeSession();
  const workDir = $("work-dir").value.trim();
  if (!session || !session.snapshotId || !workDir) return;

  session.busy = true;
  session.runInProgress = true;
  session.runLogText = "";
  session.runErrorText = "";
  $("run-btn").disabled = true;
  $("run-error").textContent = "";
  $("run-progress-wrap").classList.remove("hidden");
  $("run-progress-label").textContent = "Starting containers…";
  document.querySelector("#run-progress-wrap .spinner")?.classList.remove("hidden");
  // Fresh per attempt — retrying after a fixed environment problem
  // shouldn't show last attempt's log lines glued onto this one.
  $("run-log").textContent = "";
  $("run-log").classList.add("hidden");
  setDetailsToggleExpanded(false);

  // Registered before invoke so no early line from the backend's tailer is
  // missed. Round 29: keyed by snapshot_id (see
  // commands::tail_provisioning_log) - filtered the same way as the other
  // progress events, though note the underlying provisioning log file
  // itself is still one shared file across all Run attempts (documented,
  // out-of-scope limitation - see commands.rs).
  const unlistenRunProgress = await listen("run-progress", (evt) => {
    if (evt.payload.session_id !== session.snapshotId) return;
    session.runLogText += (session.runLogText ? "\n" : "") + evt.payload.line;
    if (session.id !== activeSessionId) return;
    const log = $("run-log");
    log.textContent = session.runLogText;
    log.scrollTop = log.scrollHeight;
  });

  try {
    // The one call in this app that executes received code — only reachable
    // from this explicit click, after the diff above has been shown.
    const runInfo = await invoke("run_snapshot", { snapshotId: session.snapshotId, workDir });
    session.runningSessionId = runInfo.session_id;
    session.servicePorts = runInfo.service_ports;
    session.dbCacheHit = runInfo.db_cache_hit;
    session.status = "running";
    session.runInProgress = false;
    recordSessionHistory(session);
    renderSessionTabs();
    if (session.id === activeSessionId) renderActiveSession();
  } catch (err) {
    session.runInProgress = false;
    session.runErrorText = String(err);
    if (session.id === activeSessionId) {
      $("run-error").textContent = session.runErrorText;
      $("run-progress-label").textContent = "Failed — see details below.";
      document.querySelector("#run-progress-wrap .spinner")?.classList.add("hidden");
    }
    // Round 12: deliberately does NOT hide #run-progress-wrap here - the
    // streamed log content is most useful right after a real failure,
    // since it likely explains why. Left visible until the next Run
    // attempt clears it at the top of this handler.
  } finally {
    session.busy = false;
    unlistenRunProgress();
    if (session.id === activeSessionId) $("run-btn").disabled = false;
  }
});

$("stop-btn").addEventListener("click", async () => {
  $("stop-error").textContent = "";
  const session = activeSession();
  if (!session || !session.runningSessionId) return;
  session.busy = true;
  $("stop-btn").disabled = true;
  try {
    await invoke("stop_session", { sessionId: session.runningSessionId });
    endSession(session, "done"); // also re-renders the active view
  } catch (err) {
    $("stop-error").textContent = String(err);
  } finally {
    session.busy = false;
    $("stop-btn").disabled = false;
  }
});

// ---------- helpers ----------
function setProgress(id, pct) {
  $(id).style.width = `${Math.max(0, Math.min(100, pct))}%`;
}

function formatBytes(n) {
  if (!n && n !== 0) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MB`;
}

function escapeHtml(s) {
  const div = document.createElement("div");
  div.textContent = s;
  return div.innerHTML;
}
