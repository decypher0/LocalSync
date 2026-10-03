// Page navigation: Home -> New Session (Name, Mode, Setup, Transfer) -> one
// session page per kind (sender/receiver) and state (running/stopped).
//
// Loaded after app.js. It owns no transfer or run logic: the existing send
// wizard, receive form, transfers list, devices list and review panel are
// MOVED into these views (their ids and handlers keep working), the page's
// action buttons press the existing (hidden) buttons, and a few app.js
// functions are wrapped so the views follow what the session model does.
(() => {
  "use strict";
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  const $id = (id) => document.getElementById(id);

  const V = {
    view: "home", // home | new-name | new-mode | new-setup | new-transfer | session
    session: null, // the session object the session page shows (by reference: ids can change)
    flow: null, // the New Session wizard: { name, mode, phase, sessionId?, again? }
    layout: null, // which session-page skeleton is built ("send-running", ...)
    homeItems: [], // last list_sessions result
    closeArmed: null, // session id whose "Close session" is waiting for a second click
  };

  // Banners (firewall, app update) go above the views, in the flow - never over them.
  const banners = document.createElement("div");
  banners.id = "lsv-banners";
  banners.append($id("firewall-banner"), $id("app-update-banner"));
  $id("app-views").prepend(banners);

  const holding = document.createElement("div");
  holding.id = "lsv-holding";
  holding.hidden = true;
  document.body.appendChild(holding);
  document.body.classList.add("lsv-on");

  // ---------- small helpers ----------
  function timeAgo(iso) {
    if (!iso) return "";
    const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
    if (s < 60) return "just now";
    if (s < 3600) return `${Math.round(s / 60)} min ago`;
    if (s < 86400) return `${Math.round(s / 3600)} hour${Math.round(s / 3600) === 1 ? "" : "s"} ago`;
    const d = Math.round(s / 86400);
    return d === 1 ? "yesterday" : `${d} days ago`;
  }
  const displayName = (s) => s.displayName || s.title || "Untitled session";
  // A device key is a discovered device's persistent id, or "dev-<room code>".
  const shortDeviceId = (key) => (key || "").replace(/^dev-/, "").slice(0, 8) || "—";
  const sendIsRunning = (s) => s.transfers.some((t) => t.status === "connecting" || t.status === "active");
  function receiveState(s) {
    if (s.status === "connecting" || s.status === "active") return "receiving";
    if (s.status === "reviewing") return "reviewing";
    if (s.status === "running") return "running";
    return "stopped";
  }
  const icon = {
    send: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M7 17 17 7M9 7h8v8"/></svg>',
    receive: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M17 7 7 17M15 17H7V9"/></svg>',
    chevron: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 6l6 6-6 6"/></svg>',
    sendAgain: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 2L11 13M22 2l-7 20-4-9-9-4 20-7z"/></svg>',
    save: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 21H5a2 2 0 01-2-2V5a2 2 0 012-2h11l5 5v11a2 2 0 01-2 2z"/><path d="M17 21v-8H7v8M7 3v5h8"/></svg>',
    down: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 5v14M19 12l-7 7-7-7"/></svg>',
    up: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 19V5M5 12l7-7 7 7"/></svg>',
    stop: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><rect x="6" y="6" width="12" height="12" rx="2"/></svg>',
    play: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 4l14 8-14 8z"/></svg>',
    box: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 8l-9-5-9 5v8l9 5 9-5z"/><path d="M3 8l9 5 9-5M12 13v8"/></svg>',
  };
  const pill = (state) => {
    const text = { running: "Running", stopped: "Stopped", receiving: "Receiving", reviewing: "Needs review", connecting: "Waiting", failed: "Transfer failed" }[state] || state;
    const cls = state === "running" ? "lsv-pill-green" : state === "stopped" ? "lsv-pill-gray" : state === "failed" ? "lsv-pill-red" : "lsv-pill-blue";
    return `<span class="lsv-pill ${cls}"><span class="lsv-dot"></span>${text}</span>`;
  };
  const row = (label, value) =>
    `<div class="lsv-row"><span class="lsv-label">${escapeHtml(label)}</span><span class="lsv-val">${escapeHtml(value ?? "—")}</span></div>`;

  // ---------- router ----------
  function navigate(view, session) {
    V.view = view;
    if (session !== undefined) V.session = session;
    if (view !== "session") syncNearbyBrowsing();
    for (const el of document.querySelectorAll("#app-views .lsv-view")) el.classList.add("hidden");
    if (view === "home") {
      $id("view-home").classList.remove("hidden");
      renderHome();
    } else if (view === "session") {
      $id("view-session").classList.remove("hidden");
      V.layout = null;
      renderSessionPage();
    } else {
      $id("view-new").classList.remove("hidden");
      for (const p of ["name", "mode", "setup", "transfer"]) $id(`new-${p}-panel`).classList.toggle("hidden", view !== `new-${p}`);
      renderSteps(view.slice(4));
    }
    window.scrollTo(0, 0);
  }
  window.lsvNavigate = navigate;

  function renderSteps(current) {
    const order = ["name", "mode", "setup", "transfer"];
    const at = order.indexOf(current);
    for (const li of document.querySelectorAll(".lsv-steps li[data-step]")) {
      const i = order.indexOf(li.dataset.step);
      li.classList.toggle("is-current", i === at);
      li.classList.toggle("is-done", i < at);
      li.querySelector(".lsv-step-dot").textContent = i < at ? "✓" : String(i + 1);
      if (i === at) li.setAttribute("aria-current", "step");
      else li.removeAttribute("aria-current");
    }
    const lines = document.querySelectorAll(".lsv-steps .lsv-step-line");
    lines.forEach((l, i) => l.classList.toggle("is-done", i < at));
    $id("new-name-echo").textContent = V.flow && V.flow.name && current !== "name" ? `"${V.flow.name}"` : "";
  }

  // ---------- Home ----------
  async function renderHome() {
    let items = [];
    try {
      items = await invoke("list_sessions");
      $id("home-error").classList.add("hidden");
    } catch (err) {
      $id("home-error").textContent = `Couldn't load your sessions: ${err}`;
      $id("home-error").classList.remove("hidden");
    }
    V.homeItems = items || [];
    // What only this window knows: live sends, and receives still in flight.
    const byId = new Map(V.homeItems.map((i) => [i.id, { ...i }]));
    for (const s of sessions.values()) {
      const known = byId.get(s.id);
      if (s.kind === "send") {
        if (known) known.running = sendIsRunning(s);
      } else if (!known) {
        byId.set(s.id, { id: s.id, kind: "receive", name: displayName(s), running: s.status === "running", inFlight: receiveState(s) === "receiving", updated_at: s.startedAt });
      } else if (s.status === "running") {
        known.running = true;
      }
    }
    const list = [...byId.values()].sort((a, b) => (b.updated_at || "").localeCompare(a.updated_at || ""));
    $id("home-empty").classList.toggle("hidden", list.length > 0);
    $id("home-list").innerHTML = list
      .map((i) => {
        const state = i.inFlight ? "receiving" : i.running ? "running" : "stopped";
        return `
        <button type="button" class="lsv-card" data-open="${escapeHtml(i.id)}" data-kind="${escapeHtml(i.kind)}">
          <span class="lsv-card-icon ${i.running ? "is-live" : ""}">${icon[i.kind === "send" ? "send" : "receive"]}</span>
          <span class="lsv-card-main">
            <span class="lsv-card-name">${escapeHtml(i.name)}</span>
            <span class="lsv-card-sub"><span>${i.kind === "send" ? "Sending" : "Received"}</span><span class="lsv-sep">•</span>${pill(state)}</span>
          </span>
          <span class="lsv-card-time">${i.running ? "Updated" : "Saved"} ${escapeHtml(timeAgo(i.updated_at))}</span>
          <span class="lsv-card-chevron" aria-hidden="true">${icon.chevron}</span>
        </button>`;
      })
      .join("");
  }

  $id("home-list").addEventListener("click", async (e) => {
    const card = e.target.closest("[data-open]");
    if (!card) return;
    const id = card.dataset.open;
    const item = V.homeItems.find((i) => i.id === id);
    try {
      let s = sessions.get(id);
      if (!s) {
        if (card.dataset.kind === "send") await openSavedSession(id);
        else await openSavedReceivedSession(id);
        s = sessions.get(id);
      }
      if (!s) return;
      if (item && s.kind === "receive") s.displayName = item.name;
      setActiveSession(s.id);
      navigate("session", s);
    } catch (err) {
      $id("home-error").textContent = String(err);
      $id("home-error").classList.remove("hidden");
    }
  });

  $id("home-new-btn").addEventListener("click", () => {
    V.flow = { name: "", mode: null, phase: "name" };
    $id("new-name-input").value = "";
    $id("new-name-error").classList.add("hidden");
    navigate("new-name");
    $id("new-name-input").focus();
  });

  setInterval(() => {
    if (V.view === "home" && !document.hidden) renderHome();
  }, 10000);

  // ---------- New Session: Name ----------
  $id("new-name-form").addEventListener("submit", (e) => {
    e.preventDefault();
    const name = $id("new-name-input").value.trim();
    if (!name) {
      $id("new-name-error").textContent = "Give the session a name - it's how you'll find it later.";
      $id("new-name-error").classList.remove("hidden");
      $id("new-name-input").focus();
      return;
    }
    V.flow.name = name;
    navigate("new-mode");
    renderModeCards();
  });

  // ---------- New Session: Mode ----------
  function renderModeCards() {
    for (const card of document.querySelectorAll(".lsv-mode-card")) {
      const on = V.flow.mode === card.dataset.mode;
      card.classList.toggle("is-selected", on);
      card.setAttribute("aria-checked", on ? "true" : "false");
    }
  }
  for (const card of document.querySelectorAll(".lsv-mode-card")) {
    card.addEventListener("click", () => {
      V.flow.mode = card.dataset.mode;
      $id("new-mode-error").classList.add("hidden");
      renderModeCards();
    });
  }
  $id("new-mode-continue").addEventListener("click", () => {
    if (!V.flow.mode) {
      $id("new-mode-error").textContent = "Choose Send or Receive.";
      $id("new-mode-error").classList.remove("hidden");
      return;
    }
    if (V.flow.mode === "send") startSendSetup();
    else startReceiveSetup();
  });

  // ---------- New Session: Setup + Transfer (send) ----------
  // The existing send wizard, shown inline. Its Step 1 (transfer mode +
  // target) is this flow's Transfer step, so Setup starts at the folders.
  const wizardModal = document.querySelector("#send-wizard-overlay .wizard-modal");
  function placeWizard(slotId) {
    $id(slotId).appendChild(wizardModal);
    $id("send-wizard-overlay").classList.add("hidden"); // the overlay shell stays empty
  }
  // Parks the wizard out of sight (so a later receive flow never shows it).
  function parkWizard() {
    holding.appendChild(wizardModal);
  }
  // Which Transfer area shows: the send wizard's step, or the receive modes.
  function showTransferArea(mode) {
    $id("new-transfer-slot").classList.toggle("hidden", mode !== "send");
    $id("new-transfer-receive").classList.toggle("hidden", mode !== "receive");
  }

  function startSendSetup() {
    V.flow.phase = "setup";
    placeWizard("new-setup-slot");
    $id("new-setup-slot").classList.remove("hidden");
    $id("new-setup-receive").classList.add("hidden");
    openSendWizard();
    $id("send-wizard-overlay").classList.add("hidden");
    showWizardStep("wiz-step-folders");
    navigate("new-setup");
  }

  async function finishSendSetup() {
    const err = $id("wiz-folders-error");
    if (wizardFolders.length === 0) {
      showWizardStep("wiz-step-folders");
      err.textContent = "Select at least one project folder.";
      return;
    }
    if (composeActive && !composeTestIsCurrent()) {
      origShowWizardStep("wiz-step-compose-review");
      return;
    }
    try {
      const session = await sessionForFolders(buildWizardFoldersPayload(wizardFolders), V.flow.name);
      resetSendWizard();
      startSendTransfer(session, false);
    } catch (e) {
      origShowWizardStep("wiz-step-folders");
      err.textContent = String(e);
    }
  }

  // Transfer: the existing "send this session to a device" step (mode +
  // target, Send). Also what "Send again" on the session page uses.
  function startSendTransfer(session, again) {
    V.flow = V.flow && !again ? { ...V.flow, phase: "transfer", sessionId: session.id } : { name: session.title, mode: "send", phase: "transfer", sessionId: session.id, again };
    placeWizard("new-transfer-slot");
    showTransferArea("send");
    openSendWizard({ sessionId: session.id });
    $id("send-wizard-overlay").classList.add("hidden");
    navigate("new-transfer");
  }

  const origShowWizardStep = window.showWizardStep;
  window.showWizardStep = function (id) {
    if (V.flow && V.flow.mode === "send" && V.flow.phase === "setup") {
      if (id === "wiz-step-mode") {
        // "Back" from the folders step: back to choosing Send/Receive.
        navigate("new-mode");
        renderModeCards();
        return;
      }
      if (id === "wiz-step-ready") {
        finishSendSetup();
        return;
      }
    }
    origShowWizardStep(id);
  };

  const origCloseSendWizard = window.closeSendWizard;
  window.closeSendWizard = function () {
    origCloseSendWizard();
    parkWizard();
    // The wizard's own Cancel while it's a step of this flow = leave the flow.
    if (V.flow && (V.view === "new-setup" || V.view === "new-transfer") && !V.sendingNow) {
      const back = V.flow.sessionId && sessions.get(V.flow.sessionId);
      V.flow = null;
      if (back) navigate("session", back);
      else navigate("home");
    }
  };

  const origStartTransfer = window.startTransfer;
  window.startTransfer = function (session, spec, opts) {
    V.sendingNow = true;
    try {
      const tr = origStartTransfer(session, spec, opts);
      if (V.flow && V.flow.phase === "transfer") {
        V.flow = null;
        navigate("session", session);
      }
      return tr;
    } finally {
      V.sendingNow = false;
    }
  };
  // The Transfer step's Send closes the wizard before starting the transfer.
  $id("wiz-mode-next-btn").addEventListener("click", () => (V.sendingNow = true), true);
  $id("wiz-mode-next-btn").addEventListener("click", () => setTimeout(() => (V.sendingNow = false), 0));

  // ---------- New Session: Setup + Transfer (receive) ----------
  const discoverWrap = $id("discoverability-wrap");
  const receiveForm = $id("receive-idle");
  function startReceiveSetup() {
    V.flow.phase = "setup";
    parkWizard();
    $id("new-setup-slot").classList.add("hidden");
    $id("new-setup-receive").classList.remove("hidden");
    $id("new-work-dir").value = $id("work-dir").value || "/tmp/localsync-work";
    navigate("new-setup");
  }
  $id("new-setup-receive-continue").addEventListener("click", () => {
    const dir = $id("new-work-dir").value.trim() || "/tmp/localsync-work";
    $id("work-dir").value = dir;
    $id("resume-work-dir").value = dir;
    V.flow.phase = "transfer";
    V.flow.workDir = dir;
    parkWizard();
    $id("ntr-code-slot").appendChild(receiveForm);
    $id("ntr-local-slot").appendChild(discoverWrap);
    $id("ntr-relay-url").value = $id("relay-url").value || modeStore.url || "";
    showTransferArea("receive");
    applyReceiveMode();
    navigate("new-transfer");
    $id("receive-room-code").focus();
  });

  // The receive modes drive the existing receive form: Cloud drop is its
  // "this is a Cloud drop code" branch; Remote relay needs the relay URL the
  // Receive handler reads from Settings.
  const RECEIVE_MODE_HINT = {
    local: "Paste the code the sender shared, or turn on discoverability below so a sender on this network can pick this device directly.",
    remote: "For a sender on a different network: both of you use the same relay server.",
    cloud: "The sender uploaded the project to Google Drive. Paste their code; nothing downloads until they approve your linked account.",
  };
  function receiveMode() {
    const on = document.querySelector('input[name="ntr-mode"]:checked');
    return on ? on.value : "local";
  }
  function applyReceiveMode() {
    const mode = receiveMode();
    $id("ntr-local").classList.toggle("hidden", mode !== "local");
    $id("ntr-remote").classList.toggle("hidden", mode !== "remote");
    $id("ntr-mode-hint").textContent = RECEIVE_MODE_HINT[mode];
    const cloud = $id("cloud-drop-receive-toggle");
    if (cloud.checked !== (mode === "cloud")) {
      cloud.checked = mode === "cloud";
      cloud.dispatchEvent(new Event("change"));
    }
  }
  for (const r of document.querySelectorAll('input[name="ntr-mode"]')) r.addEventListener("change", applyReceiveMode);
  $id("ntr-relay-url").addEventListener("input", () => {
    const url = $id("ntr-relay-url").value.trim();
    $id("relay-url").value = url; // what the Receive handler reads
    modeStore.url = url; // and remembered, like Settings does
  });

  $id("new-back-btn").addEventListener("click", () => {
    const v = V.view;
    if (v === "new-name") {
      V.flow = null;
      navigate("home");
    } else if (v === "new-mode") navigate("new-name");
    else if (v === "new-setup") {
      if (V.flow.mode === "send") origCloseSendWizard();
      navigate("new-mode");
      renderModeCards();
    } else if (v === "new-transfer") {
      if (V.flow.mode === "send") {
        // The session exists (and is saved) by now: its page is the way back.
        const s = V.flow.sessionId && sessions.get(V.flow.sessionId);
        origCloseSendWizard();
        V.flow = null;
        if (s) navigate("session", s);
        else navigate("home");
      } else {
        V.flow.phase = "setup";
        navigate("new-setup");
      }
    }
  });

  // ---------- Cloud drop needs the app's Google sign-in client ----------
  // It's set when the app is built/launched (not something a user can enter),
  // so without it both mode pickers show Cloud drop greyed out, with why,
  // instead of letting a transfer start and fail on it.
  const CLOUD_DROP_OFF =
    "Cloud drop isn't available in this build of LocalSync: it needs a Google sign-in client that is set up when the app is built - it can't be turned on from Settings.";
  invoke("cloud_drop_available")
    .then((available) => {
      if (available) return;
      for (const input of [$id("wiz-mode-cloud"), document.querySelector('input[name="ntr-mode"][value="cloud"]')]) {
        if (!input) continue;
        input.disabled = true;
        const label = input.closest("label");
        label.title = CLOUD_DROP_OFF;
        label.classList.add("is-unavailable");
        label.insertAdjacentHTML("beforeend", ` <span class="lsv-unavailable">(not available in this build)</span>`);
      }
    })
    .catch(() => {});

  // ---------- following the session model ----------
  const origAddSession = window.addSession;
  window.addSession = function (session) {
    origAddSession(session);
    if (session.kind !== "receive") return;
    if (V.flow && V.flow.mode === "receive" && V.flow.phase === "transfer") {
      // The receive this flow started: named as the person named it.
      session.displayName = V.flow.name;
      V.flow.pending = session;
      navigate("session", session);
    } else if (!V.flow) {
      // A receive that arrived on its own (accepted nearby device, a pushed update).
      navigate("session", session);
    }
  };

  const origApplyReview = window.applyReviewInfoToSession;
  window.applyReviewInfoToSession = function (session, info) {
    origApplyReview(session, info);
    if (V.flow && V.flow.pending === session) {
      V.flow = null;
      if (info.received_session_id) {
        invoke("rename_session", { sessionId: info.received_session_id, name: session.displayName }).catch((e) =>
          console.warn("couldn't name the received session:", e)
        );
      }
    }
  };

  const origRenderActive = window.renderActiveSession;
  let pageRefresh = null;
  window.renderActiveSession = function () {
    origRenderActive();
    if (V.view !== "session") return;
    renderSessionPage();
    // app.js sometimes redraws before clearing session.busy (e.g. right
    // after a Run), so look again once the current task has finished.
    if (!pageRefresh) pageRefresh = setTimeout(() => ((pageRefresh = null), V.view === "session" && renderSessionPage()), 0);
  };

  // A failed Run updates only the old panel's elements, never redraws:
  // redraw the page whenever a Run finishes, whichever way it went.
  const origRunReceive = window.runReceiveSession;
  window.runReceiveSession = async function (...args) {
    // It marks the session as starting before its first await: show
    // "Starting…" and the live log now, not once the whole project is up.
    const run = origRunReceive(...args);
    if (V.view === "session") {
      renderSessionPage();
      const logs = $id("sp-logs-panel");
      if (logs) logs.scrollIntoView({ block: "nearest" });
    }
    try {
      return await run;
    } finally {
      if (V.view === "session") renderSessionPage();
    }
  };

  const origRenderTabs = window.renderSessionTabs;
  let homeRefresh = null;
  window.renderSessionTabs = function () {
    origRenderTabs();
    if (V.view === "home" && !homeRefresh) homeRefresh = setTimeout(() => ((homeRefresh = null), renderHome()), 300);
  };

  // Live run log lines for the receiver page (app.js appends them to the
  // session; this just redraws the panel after it has).
  listen("run-progress", (evt) => {
    const s = V.session;
    if (V.view !== "session" || !s || evt.payload.session_id !== s.snapshotId) return;
    setTimeout(renderLogs, 0);
  });

  // A receiver page keeps its running/stopped state honest: asks the
  // backend (which asks Podman) every 15 s while it's on screen.
  setInterval(async () => {
    const s = V.session;
    if (V.view !== "session" || document.hidden || !s || s.kind !== "receive" || s.busy || s.runInProgress || s.stopping || s.armBusy) return;
    if (!s.snapshotId || receiveState(s) === "receiving" || receiveState(s) === "reviewing") return;
    try {
      const view = await invoke("open_saved_received_session", { sessionId: s.id });
      if (view.running && s.status !== "running") {
        s.status = "running";
        s.servicePorts = view.service_ports || s.servicePorts;
        s.dbCacheHit = view.db_cache_hit;
      } else if (!view.running && s.status === "running") {
        s.status = "done";
      } else return;
      renderSessionPage();
    } catch (_) {
      /* not saved yet / gone: leave the page as it is */
    }
  }, 15000);

  // ---------- session page ----------
  // Panels that already exist in app.js's DOM and are kept up to date by it.
  const reuse = {
    transfers: $id("send-transfers"),
    transfersEmpty: $id("send-transfers-empty"),
    devices: $id("send-devices-list"),
    devicesEmpty: $id("send-devices-empty"),
    review: $id("review-panel"),
    pushStatus: $id("send-push-status"),
  };

  function pageState(s) {
    if (s.kind === "send") {
      if (sendIsRunning(s)) return "running";
      // A transfer that failed or whose code expired stays visible (error + Retry).
      const last = s.transfers[s.transfers.length - 1];
      return last && (last.status === "error" || last.status === "expired") ? "failed" : "stopped";
    }
    return receiveState(s);
  }

  function renderSessionPage() {
    const s = V.session;
    if (!s) return navigate("home");
    // Existing buttons act on the active session.
    if (activeSessionId !== s.id && sessions.has(s.id)) activeSessionId = s.id;
    const state = pageState(s);
    const layout = `${s.kind}-${state}`;
    if (V.layout !== layout) {
      for (const el of Object.values(reuse)) holding.appendChild(el);
      $id("sp-left").innerHTML = "";
      $id("sp-right").innerHTML = "";
      (s.kind === "send" ? buildSend : buildReceive)(state);
      V.layout = layout;
    }
    renderHeader(s, state);
    if (s.kind === "send" && (!V.pushStatus || V.pushStatus.sessionId !== s.id)) {
      V.pushStatus = { sessionId: s.id, pending: true };
      refreshPushStatus();
    }
    (s.kind === "send" ? updateSend : updateReceive)(s, state);
    syncNearbyBrowsing();
  }

  function renderHeader(s, state) {
    const name = displayName(s);
    const avatar = $id("sp-avatar");
    avatar.textContent = (name.trim()[0] || "?").toUpperCase();
    avatar.className = `lsv-avatar ${state === "running" ? (s.kind === "send" ? "is-send" : "is-receive") : ""}`;
    $id("sp-name").textContent = name;
    if (s.kind === "send") {
      $id("sp-subtitle").textContent = "Sending";
    } else {
      const from = s.recognizedPeer ? ` from ${s.recognizedPeer.name}` : "";
      const when = s.lastReceivedAt || s.startedAt;
      $id("sp-subtitle").textContent = `Received${from}${when ? ` · ${timeAgo(when)}` : ""}`;
    }
    $id("sp-status").outerHTML = pill(state).replace('class="lsv-pill', 'id="sp-status" class="lsv-pill');
    let meta = "";
    if (s.kind === "send") meta = state === "running" ? `Session started ${timeAgo(s.startedAt)}` : `Saved ${timeAgo(s.startedAt)}`;
    else if (state === "running" && s.servicePorts) meta = s.servicePorts.map(([, p]) => `localhost:${p.split(":")[0]}`).join(" · ");
    $id("sp-meta").textContent = meta;
  }

  function panel(title, bodyId, extraClass = "") {
    return `<section class="lsv-panel ${extraClass}"><div class="lsv-panel-title">${title}</div><div id="${bodyId}"></div></section>`;
  }
  function actionsPanel(buttons, note, extraClass = "") {
    return `<section class="lsv-panel lsv-actions ${extraClass}"><div class="lsv-panel-title">Actions</div>${buttons}</section>${
      note ? `<div class="lsv-grow"></div><p class="lsv-note">${note}</p>` : ""
    }`;
  }
  const btn = (id, label, ic, cls = "") => `<button id="${id}" type="button" class="lsv-btn ${cls}">${ic || ""}<span>${label}</span></button>`;

  // ----- sender -----
  function buildSend(state) {
    const left = $id("sp-left");
    const right = $id("sp-right");
    left.innerHTML = panel("Project", "sp-project");
    if (state === "running") {
      left.insertAdjacentHTML(
        "beforeend",
        `<section class="lsv-panel lsv-fill"><div class="lsv-panel-title">Transfer method</div>
          <div id="sp-method" class="lsv-segments" role="list"></div>
          <div id="sp-transfers-slot" class="lsv-scroll"></div></section>`
      );
      $id("sp-transfers-slot").append(reuse.transfers, reuse.transfersEmpty);
      right.innerHTML =
        `<section class="lsv-panel lsv-fill"><div class="lsv-panel-title">Devices</div><div id="sp-devices-slot" class="lsv-scroll"></div></section>` +
        actionsPanel(
          btn("sp-send-again", "Send again", icon.sendAgain, "lsv-btn-primary") +
            btn("sp-save", "Saved automatically", icon.save) +
            btn("sp-pull", "Pull from receiver", icon.down) +
            btn("sp-push", "Push update", icon.up) +
            btn("sp-close", "Close session", "", "lsv-btn-danger"),
          "",
          "is-send"
        );
      $id("sp-devices-slot").insertAdjacentHTML(
        "beforeend",
        `<div id="sp-live-devices" class="lsv-live-devices"></div>
         <div class="lsv-subhead">On this network</div>
         <div id="sp-nearby" class="lsv-live-devices"><p class="lsv-muted lsv-small">Looking for discoverable devices…</p></div>`
      );
      $id("sp-nearby").addEventListener("click", (e) => {
        const b = e.target.closest("[data-send-nearby]");
        const device = b && V.nearby.find((d) => d.fullname === b.dataset.sendNearby);
        if (device) startTransfer(V.session, { kind: "device", device }, { deviceName: device.nickname });
      });
      $id("sp-devices-slot").append(reuse.devices, reuse.devicesEmpty);
      $id("sp-save").disabled = true;
      $id("sp-pull").disabled = true;
      $id("sp-pull").title = "Not available yet - LocalSync can't pull changes back from a receiver.";
    } else if (state === "failed") {
      left.insertAdjacentHTML(
        "beforeend",
        `<section class="lsv-panel lsv-fill"><div class="lsv-panel-title">Transfers</div><div id="sp-transfers-slot" class="lsv-scroll"></div></section>`
      );
      $id("sp-transfers-slot").append(reuse.transfers);
      // The failed one is the newest, at the end: show it.
      requestAnimationFrame(() => { const slot = $id("sp-transfers-slot"); if (slot) slot.scrollTop = slot.scrollHeight; });
      right.innerHTML =
        panel("Last known device", "sp-last-device") +
        actionsPanel(
          btn("sp-send-again", "Send again", icon.sendAgain, "lsv-btn-primary") + btn("sp-push", "Push update", icon.up) + btn("sp-close", "Close session", "", "lsv-btn-danger"),
          "",
          "is-send"
        );
    } else {
      left.insertAdjacentHTML(
        "beforeend",
        `<section class="lsv-panel lsv-fill lsv-placeholder"><span class="lsv-ph-icon">${icon.up}</span>
          <div class="lsv-ph-title">No active transfer</div>
          <div id="sp-ph-text" class="lsv-ph-text">Send again to generate a fresh connection code.</div></section>`
      );
      right.innerHTML =
        panel("Last known device", "sp-last-device") +
        actionsPanel(
          btn("sp-send-again", "Send again", icon.sendAgain, "lsv-btn-primary") + btn("sp-push", "Push update", icon.up) + btn("sp-close", "Close session", "", "lsv-btn-danger"),
          "Push update sends only what changed since the last send, to the last device - reconnecting the same way Send again does.",
          "is-send"
        );
    }
    $id("sp-send-again").addEventListener("click", () => startSendTransfer(V.session, true));
    $id("sp-push").addEventListener("click", pushUpdate);
    // The push flow's progress ("Looking for the devices on the network…").
    $id("sp-push").after(reuse.pushStatus);
    $id("sp-close").addEventListener("click", closeCurrent);
  }

  function updateSend(s, state) {
    const a = s.artifact;
    const commit = a && a.snapshot_id ? a.snapshot_id.split("@").pop() : null;
    const dumps = (s.folders || []).filter((f) => f.dump).map((f) => `${f.dump.engine} · ${f.dump.schema}`);
    const lastSent = s.devices.map((d) => d.marker && d.marker.sent_at).filter(Boolean).sort().pop();
    $id("sp-project").innerHTML =
      row("Local path", (s.folders || []).map((f) => f.path).join(", ") || "—") +
      row("Commit", commit ? shortCommit(commit) : "—") +
      row("Database", dumps.join(", ") || "None") +
      (state === "running" ? row("Snapshot size", a ? formatBytes(a.size_bytes) : "Preparing…") : row("Last sent", lastSent ? timeAgo(lastSent) : "Not sent yet"));
    if (state === "running") {
      const tr = [...s.transfers].reverse().find((t) => t.status === "connecting" || t.status === "active") || s.transfers[s.transfers.length - 1];
      const method = !tr ? null : tr.spec.kind === "cloud" ? "cloud" : tr.spec.kind === "code" && tr.spec.mode === "remote" ? "relay" : "lan";
      $id("sp-method").innerHTML = [
        ["lan", "P2P (LAN)"],
        ["relay", "Relay"],
        ["cloud", "Cloud drop"],
      ]
        .map(([k, label]) => `<span role="listitem" class="lsv-segment ${k === method ? "is-on" : ""}" ${k === method ? 'aria-current="true"' : ""}>${label}</span>`)
        .join("");
      const live = s.transfers.filter((t) => t.status === "connecting" || t.status === "active");
      $id("sp-live-devices").innerHTML =
        live
          .map((t) => {
            const name = t.deviceName || (t.spec.kind === "cloud" ? "Cloud drop" : "Device via code");
            const status = t.status === "active" ? "connected · receiving now" : t.receiverJoined ? "connected" : "waiting for it to connect";
            return `<div class="lsv-device ${t.status === "active" || t.receiverJoined ? "is-live" : ""}"><span class="lsv-device-dot"></span><span><span class="lsv-device-name">${escapeHtml(name)}</span><span class="lsv-device-sub">${status}</span></span></div>`;
          })
          .join("") + `<div class="lsv-device is-waiting"><span class="lsv-device-dot"></span><span class="lsv-device-name">Waiting for another device…</span></div>`;
    } else {
      const last = [...s.devices].filter((d) => d.marker).sort((x, y) => (x.marker.sent_at || "").localeCompare(y.marker.sent_at || "")).pop();
      $id("sp-last-device").innerHTML = last
        ? row("Name", last.name) +
          row("Id", shortDeviceId(last.key)) +
          row("Last sent", `${new Date(last.marker.sent_at).toLocaleString()} (${timeAgo(last.marker.sent_at)})`) +
          row("Transfer speed", last.bytes_per_sec ? `${formatBytes(last.bytes_per_sec)}/s` : "—")
        : `<p class="lsv-muted">Not sent to any device yet.</p>`;
      if (state === "failed") return void updatePushButtons(s);
      $id("sp-ph-text").textContent = last
        ? `The last transfer to this device finished ${timeAgo(last.marker.sent_at)}. Send again to generate a fresh connection code.`
        : "This session hasn't been sent yet. Send again to generate a connection code.";
    }
    updatePushButtons(s);
  }

  // Push update needs a previous recipient AND a project that changed since
  // the last send to it (project_push_status compares the folders' current
  // git commits with what that device received).
  function updatePushButtons(s) {
    const st = V.pushStatus && V.pushStatus.sessionId === s.id ? V.pushStatus : { pending: true };
    let reason = "";
    if (st.pending) reason = "Checking for changes…";
    else if (!st.last_device_key) reason = "No previous recipient on record - send this project to a device first.";
    else if (!st.changed) reason = `No changes to push since the last send to ${st.last_device_name}.`;
    $id("sp-push").disabled = s.busy || !!reason;
    $id("sp-push").title = reason || `Send what changed since the last send to ${st.last_device_name}.`;
    $id("sp-send-again").disabled = s.busy;
  }

  async function refreshPushStatus() {
    const s = V.session;
    if (V.view !== "session" || !s || s.kind !== "send") return;
    let st;
    try {
      st = await invoke("project_push_status", { sessionId: s.id });
    } catch (_) {
      st = { last_device_key: null, changed: false };
    }
    V.pushStatus = { ...st, sessionId: s.id };
    if (V.view === "session" && V.session === s && $id("sp-push")) updatePushButtons(s);
  }

  function pushUpdate() {
    const s = V.session;
    const key = V.pushStatus && V.pushStatus.last_device_key;
    if (!key) return;
    s.pushSelection ||= new Set();
    // Only the diff, to the last-known device: pushUpdateToDevices finds it on
    // the network or falls back to a fresh code, like Send again.
    pushUpdateToDevices(s, [key]).finally(refreshPushStatus);
  }

  // LAN discovery for the running sender page's Devices panel.
  V.nearby = [];
  let nearbyTimer = null;
  async function pollNearby() {
    const box = $id("sp-nearby");
    if (!box) return;
    try {
      V.nearby = await invoke("list_nearby_devices");
    } catch (err) {
      box.innerHTML = `<p class="lsv-muted lsv-small">Couldn't list nearby devices: ${escapeHtml(String(err))}</p>`;
      return;
    }
    box.innerHTML = V.nearby.length
      ? V.nearby
          .map(
            (d) => `<div class="lsv-device is-live"><span class="lsv-device-dot"></span><span class="lsv-grow"><span class="lsv-device-name">${escapeHtml(d.nickname)}</span><span class="lsv-device-sub">discoverable on this network</span></span>
              <button type="button" class="lsv-btn lsv-btn-small" data-send-nearby="${escapeHtml(d.fullname)}">Send</button></div>`
          )
          .join("")
      : `<p class="lsv-muted lsv-small">No discoverable devices yet. A device shows up here once it turns on "Make this device discoverable" - a connection code always works too.</p>`;
  }
  function syncNearbyBrowsing() {
    const want = V.view === "session" && V.layout === "send-running";
    if (want && !nearbyTimer) {
      invoke("start_discovery_browsing").catch(() => {});
      pollNearby();
      nearbyTimer = setInterval(pollNearby, 2500);
    } else if (!want && nearbyTimer) {
      clearInterval(nearbyTimer);
      nearbyTimer = null;
      invoke("stop_discovery_browsing").catch(() => {});
    }
  }
  // A commit made while the page is open enables Push update within 10 s.
  setInterval(() => {
    if (V.view === "session" && !document.hidden && V.session && V.session.kind === "send") refreshPushStatus();
  }, 10000);
  window.addEventListener("focus", refreshPushStatus);

  // ----- receiver -----
  function buildReceive(state) {
    const left = $id("sp-left");
    const right = $id("sp-right");
    left.innerHTML = panel("Project", "sp-project", "lsv-shrink");
    if (state === "running") {
      left.insertAdjacentHTML("beforeend", logsPanel(true));
      right.innerHTML =
        panel("Service", "sp-service") +
        actionsPanel(
          btn("sp-stop", "Stop", icon.stop, "lsv-btn-danger") +
            `<p id="sp-stop-error" class="lsv-error hidden"></p>` +
            btn("sp-arm", "Pull update", icon.down) +
            armedPanel() +
            btn("sp-save", "Saved automatically", icon.save) +
            btn("sp-close", "Close session", "", "lsv-btn-danger")
        );
      $id("sp-save").disabled = true;
      $id("sp-stop").addEventListener("click", stopCurrent);
    } else if (state === "reviewing") {
      left.insertAdjacentHTML("beforeend", `<div id="sp-review-slot" class="lsv-review"></div>`);
      $id("sp-review-slot").appendChild(reuse.review);
      left.insertAdjacentHTML("beforeend", logsPanel(false));
      right.innerHTML = actionsPanel(
        btn("sp-run", "Run", icon.play, "lsv-btn-primary") +
          `<p id="sp-run-error" class="lsv-error hidden"></p><button id="sp-fix-setup" type="button" class="lsv-btn hidden">Fix setup</button>` +
          btn("sp-reject", "Reject", "") +
          btn("sp-close", "Close session", "", "lsv-btn-danger"),
        "Nothing from this project runs until you press Run, after reviewing what changed."
      );
      $id("sp-run").addEventListener("click", runCurrent);
      $id("sp-fix-setup").addEventListener("click", () => openSetup());
      $id("sp-reject").addEventListener("click", () => pressExisting("reject-btn"));
    } else if (state === "receiving") {
      left.insertAdjacentHTML(
        "beforeend",
        `<section class="lsv-panel lsv-fill lsv-placeholder"><span class="lsv-ph-icon">${icon.down}</span>
          <div id="sp-ph-title" class="lsv-ph-title">Waiting for the sender…</div><div id="sp-ph-text" class="lsv-ph-text"></div></section>`
      );
      right.innerHTML = actionsPanel(btn("sp-close", "Close session", "", "lsv-btn-danger"));
    } else {
      left.insertAdjacentHTML(
        "beforeend",
        `<section id="sp-stopped-ph" class="lsv-panel lsv-fill lsv-placeholder"><span class="lsv-ph-icon">${icon.box}</span>
          <div class="lsv-ph-title">Containers aren't running</div>
          <div class="lsv-ph-text">Press Run to bring this project back up with the database exactly as it was saved.</div>
          <p id="sp-run-error" class="lsv-error hidden"></p>
          <button id="sp-fix-setup" type="button" class="lsv-btn lsv-btn-inline hidden">Fix setup</button></section>` + logsPanel(false)
      );
      right.innerHTML =
        panel("Last run", "sp-last-run") +
        actionsPanel(
          btn("sp-run", "Run", icon.play, "lsv-btn-primary") + btn("sp-arm", "Receive update", icon.down) + armedPanel() + btn("sp-close", "Close session", "", "lsv-btn-danger"),
          "A stopped session only shows what you can actually do right now - run it again, pull the latest update, or remove it."
        );
      $id("sp-run").addEventListener("click", runCurrent);
      $id("sp-fix-setup").addEventListener("click", () => openSetup());
    }
    const arm = $id("sp-arm");
    if (arm) {
      arm.addEventListener("click", toggleArm);
      $id("sp-arm-receive").addEventListener("click", receiveUpdateByCode);
      $id("sp-arm-code").addEventListener("keydown", (e) => e.key === "Enter" && receiveUpdateByCode());
      $id("sp-arm-discoverable").addEventListener("click", () => {
        const t = $id("discoverable-toggle");
        if (!t.checked && !t.disabled) {
          t.checked = true;
          t.dispatchEvent(new Event("change"));
        }
        setTimeout(() => V.view === "session" && renderSessionPage(), 600);
      });
    }
    $id("sp-close").addEventListener("click", closeCurrent);
  }

  // Shown under "Pull update" while the session waits for the sender's next
  // push: it arrives on its own from a nearby sender (discoverable), or by
  // the code the sender's Push update shows.
  function armedPanel() {
    return `<div id="sp-armed" class="lsv-armed hidden">
      <p id="sp-armed-text" class="lsv-small"></p>
      <button id="sp-arm-discoverable" type="button" class="lsv-btn lsv-btn-small hidden">Make this device discoverable</button>
      <label class="lsv-field">Or paste the code from the sender's Push update
        <span class="lsv-inline-row"><input id="sp-arm-code" class="lsv-input" type="text" placeholder="e.g. 4XzmFsM6ggcAJ8" autocomplete="off" />
        <button id="sp-arm-receive" type="button" class="lsv-btn lsv-btn-small lsv-btn-primary">Receive update</button></span>
      </label>
    </div><p id="sp-arm-error" class="lsv-error hidden"></p>`;
  }

  async function toggleArm() {
    const s = V.session;
    s.armError = "";
    s.armBusy = true;
    renderSessionPage();
    try {
      await invoke(s.armed ? "disarm_received_session_for_update" : "arm_received_session_for_update", { sessionId: s.id });
      s.armed = !s.armed;
    } catch (err) {
      s.armError = `Couldn't ${s.armed ? "cancel waiting for" : "get ready for"} an update: ${err}`;
    } finally {
      s.armBusy = false;
      if (V.session === s && V.view === "session") renderSessionPage();
    }
  }

  // The update by code: received straight onto this session (it's armed, so
  // the backend files the push under it) and shown here for review.
  async function receiveUpdateByCode() {
    const s = V.session;
    const code = $id("sp-arm-code").value.trim();
    s.armError = code ? "" : "Paste the code the sender's Push update shows.";
    if (!code) return renderSessionPage();
    s.armBusy = true;
    renderSessionPage();
    try {
      const decoded = await invoke("decode_room_code", { code, relayUrl: relayUrl() || null });
      const info = await invoke("receive_snapshot", { roomCode: decoded.room_id, signalingUrl: decoded.signaling_url });
      if (info.received_session_id && info.received_session_id !== s.id) {
        // Not this project/sender: it became a session of its own.
        startReceiveSessionFromInfo(info, info.received_session_id);
        s.armError = "That code was for a different project or sender, so it opened as its own session.";
      } else {
        applyReviewInfoToSession(s, info);
        renderSessionTabs();
      }
    } catch (err) {
      s.armError = `Couldn't receive the update: ${err}`;
    } finally {
      s.armBusy = false;
      if (V.session === s && V.view === "session") renderSessionPage();
    }
  }

  // Stop shows "Stopping…" the moment it's clicked, and "Run" only once the
  // containers are really down.
  async function stopCurrent() {
    const s = V.session;
    if (s.stopping) return;
    s.stopping = true;
    s.stopError = "";
    renderSessionPage();
    try {
      await invoke("stop_received_session", { sessionId: s.id });
      s.status = "done";
      s.endedAt = new Date().toISOString();
      renderSessionTabs();
    } catch (err) {
      s.stopError = `Couldn't stop it: ${err}`;
    } finally {
      s.stopping = false;
      if (V.session === s && V.view === "session") renderSessionPage();
    }
  }

  function logsPanel(live) {
    return `<section id="sp-logs-panel" class="lsv-panel lsv-fill ${live ? "" : "hidden"}">
      <div class="lsv-panel-title lsv-title-row"><span>Logs</span><span id="sp-logs-live" class="lsv-live"><span class="lsv-dot"></span> live</span></div>
      <pre id="sp-logs" class="lsv-logs" tabindex="0" aria-label="Run log"></pre></section>`;
  }

  function renderLogs() {
    const s = V.session;
    const box = $id("sp-logs");
    if (!s || !box) return;
    const wrap = $id("sp-logs-panel");
    // Live while starting/running; on a stopped page only to explain a failed Run.
    const show = receiveState(s) === "running" || s.runInProgress || (!!s.runErrorText && !!s.runLogText);
    wrap.classList.toggle("hidden", !show);
    $id("sp-logs-live").classList.toggle("hidden", !s.runInProgress && receiveState(s) !== "running");
    const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 24;
    box.textContent =
      s.runLogText ||
      (receiveState(s) === "running"
        ? "No log for this run - it was started before this window opened. Stop and Run again to watch it start."
        : "Starting…");
    if (atBottom) box.scrollTop = box.scrollHeight;
  }

  function updateReceive(s, state) {
    const m = s.manifest;
    const dumps = m && m.database_dumps && m.database_dumps.length ? m.database_dumps.map((d) => `${d.engine} · ${d.schema}`).join(", ") : m ? "None" : "—";
    $id("sp-project").innerHTML =
      row("Received from", s.recognizedPeer ? s.recognizedPeer.name : "Unknown sender") +
      row("Project", s.title || "—") +
      row("Commit", s.gitCommit || (m && m.git_commit) ? shortCommit(s.gitCommit || m.git_commit) : "—") +
      row("Database", dumps) +
      row("Saved locally", s.workDir ? `Yes · ${s.workDir}` : "Yes");
    if (state === "running") {
      // Published ports are "host:container"; a bare one has no fixed host port.
      const ports = (s.servicePorts || []).filter(([, p]) => p.includes(":"));
      const services = new Set((s.manifest && s.manifest.services ? s.manifest.services.map((x) => x.name) : []).concat(ports.map(([svc]) => svc)));
      const internal = [...services].filter((svc) => !ports.some(([name]) => name === svc));
      $id("sp-service").innerHTML =
        (ports.length
          ? ports
              .map(([svc, p]) => {
                const [host, container] = p.split(":");
                const url = `http://localhost:${host}`;
                return `<div class="lsv-row"><span class="lsv-label">${escapeHtml(svc)}</span><span class="lsv-val lsv-url-row">
                  <a href="#" class="lsv-url" data-open-url="${escapeHtml(url)}" title="Open in your browser">${escapeHtml(url)}</a>
                  <button type="button" class="lsv-btn lsv-btn-small" data-copy-url="${escapeHtml(url)}">Copy</button>
                  <span class="lsv-muted lsv-small">→ ${escapeHtml(container)}</span></span></div>`;
              })
              .join("")
          : `<p class="lsv-muted">This project publishes no ports to your computer, so there's no address to open.</p>`) +
        (internal.length ? row("Inside the project only", internal.join(", ")) : "") +
        row("Database cache", s.dbCacheHit === true ? "Reused" : s.dbCacheHit === false ? "Seeded fresh" : "Unknown (reopened)");
      $id("sp-stop").disabled = s.busy || s.stopping;
      $id("sp-stop").querySelector("span").textContent = s.stopping ? "Stopping…" : "Stop";
      $id("sp-stop-error").textContent = s.stopError || "";
      $id("sp-stop-error").classList.toggle("hidden", !s.stopError);
    } else if (state === "receiving") {
      const pct = s.progressTotal ? Math.round((s.progressBytes / s.progressTotal) * 100) : null;
      $id("sp-ph-title").textContent = s.status === "active" ? "Receiving…" : "Waiting for the sender…";
      $id("sp-ph-text").textContent = s.status === "active" && s.progressTotal ? `${formatBytes(s.progressBytes)} of ${formatBytes(s.progressTotal)} (${pct}%)` : "Share-code receives wait here until the sender starts the transfer.";
    } else if (state === "stopped") {
      const failed = !!s.runErrorText;
      $id("sp-last-run").innerHTML = row("Stopped", s.endedAt ? timeAgo(s.endedAt) : "—") + row("Exit reason", failed ? "Failed to start" : s.endedAt ? "Stopped" : s.status === "error" ? "Receive failed" : "—");
      const errEl = $id("sp-run-error");
      errEl.textContent = s.runErrorText || (s.status === "error" ? s.errorText : "");
      errEl.classList.toggle("hidden", !errEl.textContent);
      $id("sp-fix-setup").classList.toggle("hidden", !SetupWizard.isPodmanNotReady(s.runErrorText));
      $id("sp-stopped-ph").classList.toggle("hidden", s.runInProgress);
      $id("sp-run").disabled = s.busy || s.runInProgress || s.status === "error";
      $id("sp-run").querySelector("span").textContent = s.runInProgress ? "Starting…" : "Run";
    }
    if (state === "reviewing") {
      const errEl = $id("sp-run-error");
      errEl.textContent = s.runErrorText || "";
      errEl.classList.toggle("hidden", !s.runErrorText);
      $id("sp-fix-setup").classList.toggle("hidden", !SetupWizard.isPodmanNotReady(s.runErrorText));
      $id("sp-run").disabled = s.busy || s.runInProgress;
      $id("sp-run").querySelector("span").textContent = s.runInProgress ? "Starting…" : "Run";
    }
    const arm = $id("sp-arm");
    if (arm) {
      const discoverable = $id("discoverable-toggle").checked;
      arm.disabled = s.busy || s.armBusy || s.stopping;
      arm.querySelector("span").textContent = s.armed ? "Stop waiting for an update" : state === "running" ? "Pull update" : "Receive update";
      arm.title = s.armed ? "" : "Get ready for the sender's next Push update - it lands on this session, for review before it runs.";
      $id("sp-armed").classList.toggle("hidden", !s.armed);
      $id("sp-armed-text").textContent =
        `Waiting for ${s.recognizedPeer ? s.recognizedPeer.name : "the sender"}'s next Push update. ` +
        (discoverable
          ? "This device is discoverable, so a sender nearby can push it straight here."
          : "This device isn't discoverable, so the update needs the code the sender's Push update shows - or make it discoverable.");
      $id("sp-arm-discoverable").classList.toggle("hidden", discoverable);
      $id("sp-arm-receive").disabled = !!s.armBusy;
      $id("sp-arm-receive").textContent = s.armBusy && s.armed ? "Receiving…" : "Receive update";
      $id("sp-arm-error").textContent = s.armError || "";
      $id("sp-arm-error").classList.toggle("hidden", !s.armError);
    }
    renderLogs();
  }

  function runCurrent() {
    const s = V.session;
    if (!$id("work-dir").value.trim()) $id("work-dir").value = s.workDir || "/tmp/localsync-work";
    if (!$id("resume-work-dir").value.trim()) $id("resume-work-dir").value = s.workDir || $id("work-dir").value;
    pressExisting(s.status === "reviewing" ? "run-btn" : "resume-run-btn");
  }

  function pressExisting(id) {
    // Refresh the existing controls first: they may still carry a stale
    // busy-disabled state from app.js's last redraw.
    origRenderActive();
    const el = $id(id);
    if (el && !el.disabled) el.click();
  }

  async function closeCurrent() {
    const s = V.session;
    const button = $id("sp-close");
    if (V.closeArmed !== s.id) {
      V.closeArmed = s.id;
      button.querySelector("span").textContent =
        s.kind === "receive" && s.status === "running" ? "Click again to stop and close" : "Click again to close";
      setTimeout(() => {
        if (V.closeArmed === s.id && $id("sp-close")) {
          V.closeArmed = null;
          $id("sp-close").querySelector("span").textContent = "Close session";
        }
      }, 4000);
      return;
    }
    V.closeArmed = null;
    button.disabled = true;
    try {
      // A receive still in flight has no stored session yet - just drop it.
      if (!(s.kind === "receive" && receiveState(s) === "receiving")) await invoke("close_session", { sessionId: s.id });
      if (s.progressUnlisten) s.progressUnlisten();
      for (const tr of s.transfers || []) for (const u of tr.unlisten || []) u();
      sessions.delete(s.id);
      if (activeSessionId === s.id) activeSessionId = null;
      origRenderTabs();
      V.session = null;
      navigate("home");
    } catch (err) {
      button.disabled = false;
      button.querySelector("span").textContent = "Close session";
      const note = document.createElement("p");
      note.className = "lsv-error";
      note.textContent = String(err);
      button.after(note);
    }
  }

  $id("session-back-btn").addEventListener("click", () => navigate("home"));
  $id("view-session").addEventListener("click", (e) => {
    const open = e.target.closest("[data-open-url]");
    if (open) {
      e.preventDefault();
      invoke("open_local_url", { url: open.dataset.openUrl }).catch((err) => console.warn("couldn't open", err));
      return;
    }
    const copy = e.target.closest("[data-copy-url]");
    if (copy) {
      writeClipboardText(copy.dataset.copyUrl).then(
        () => ((copy.textContent = "Copied"), setTimeout(() => (copy.textContent = "Copy"), 1500)),
        () => (copy.textContent = "Couldn't copy")
      );
    }
  });

  navigate("home");
})();
