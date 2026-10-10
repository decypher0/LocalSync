// The folder step's per-folder check: one status chip under each added
// folder (what kind of project it looks like, whether it's in git yet, with
// "Set up automatically" when it isn't), and the "What can I send?" guide.
// Loaded by a plain <script> before app.js; the pure part (chip, guideLines)
// is tested by test-folder-check.js under Node.
//
// What counts as supported is never listed here: a detected stack is looked
// up in the backend's compose_catalog (app.js's `composeCatalog`), so adding a
// runtime there turns its chip green and drops it from "Not supported yet".

// Must match ls_snapshot::GIT_MISSING.
const GIT_MISSING_TEXT = "Git isn't installed - LocalSync needs it to package your project. Install it from git-scm.com and restart LocalSync.";

const FolderCheck = (() => {
  const INIT_NOTE = "Adds a hidden .git folder and one commit; your files aren't changed.";
  // Names for the stacks inspect_project can detect that have no catalog
  // runtime; a name only, the catalog decides whether it's supported.
  const OTHER_STACKS = { rust: "Rust", php: "PHP", ruby: "Ruby", dotnet: ".NET" };

  const joinOr = (xs) => (xs.length > 1 ? `${xs.slice(0, -1).join(", ")} or ${xs[xs.length - 1]}` : xs.join(""));

  /** inspect_project's detected_stack ("java-maven", "node", ...) against the catalog: null when there's nothing to say. */
  function stackInfo(catalog, stack) {
    if (stack === "compose") return { supported: true, label: "Docker Compose" };
    if (!catalog || !stack || stack === "unknown") return null;
    const [runtime, tool] = stack.split("-");
    const r = catalog.runtimes.find((x) => x.runtime === runtime);
    const t = r && tool ? r.build_tools.find((b) => b.tool === tool) : null;
    if (r && (!tool || t)) return { supported: true, label: t ? `${r.label} (${t.label})` : r.label };
    return { supported: false, label: OTHER_STACKS[runtime] || stack };
  }

  /**
   * The one chip for a folder. `check` = { info, error, busy, setupError }
   * (info = inspect_project's result). kind: pending | ok | git | bad | none.
   */
  function chip(check, catalog) {
    if (!check || (!check.info && !check.error)) return { kind: "pending", text: "Checking this folder…" };
    if (check.error) return { kind: "bad", text: `Couldn't check this folder: ${check.error}` };
    const info = check.info;
    if (info.git_missing) return { kind: "bad", text: GIT_MISSING_TEXT };
    const stack = stackInfo(catalog, info.detected_stack);
    if (stack && !stack.supported) return { kind: "bad", text: `Not supported yet: ${stack.label}` };
    if (!info.is_git_repo || !info.has_commits) {
      return {
        kind: "git",
        text: info.is_git_repo ? "No git commits yet" : "Not tracked by git yet",
        action: true,
        busy: !!check.busy,
        note: INIT_NOTE,
        error: check.setupError || "",
      };
    }
    if (stack) return { kind: "ok", text: `${stack.label} project detected` };
    return { kind: "none", text: "" };
  }

  /** The guide's three lines, from the catalog. */
  function guideLines(catalog) {
    const runtimes = catalog.runtimes.map((r) => r.label);
    const dbs = catalog.databases.map((d) => d.label);
    const notYet = Object.keys(OTHER_STACKS).filter((k) => !catalog.runtimes.some((r) => r.runtime === k)).map((k) => OTHER_STACKS[k]);
    return [
      `Works: web apps and APIs that listen on a port, in ${joinOr(runtimes)}` + (dbs.length ? `, optionally with ${joinOr(dbs)}.` : "."),
      `Not supported yet: mobile apps, ${[...notYet, "static-only sites"].join(", ")}.`,
      "Needs: the project in git with at least one commit (LocalSync can set that up for you). A project's own docker-compose.yml is used as-is.",
    ];
  }

  /** Next's message when a folder has nothing committed. */
  const notInGitError = (who) => `LocalSync sends what's committed in git, and ${who} has nothing committed yet. Use "Set up automatically" under it.`;

  // ---------- DOM (app.js's folder list) ----------

  async function ensureCatalog() {
    if (composeCatalog) return;
    try {
      composeCatalog = await invoke("compose_catalog");
    } catch (_) {
      // No catalog = no stack chips or guide; the git chip still works.
    }
  }

  function renderGuide() {
    const box = document.getElementById("wiz-send-guide-lines");
    if (!box || !composeCatalog || box.childElementCount) return;
    for (const line of guideLines(composeCatalog)) {
      const p = document.createElement("p");
      p.textContent = line;
      box.appendChild(p);
    }
  }

  function paint(f) {
    const el = f.checkEl;
    if (!el) return;
    const c = chip(f.check, composeCatalog);
    el.className = `folder-check folder-check-${c.kind}`;
    el.hidden = c.kind === "none";
    el.textContent = "";
    const text = document.createElement("span");
    text.className = "folder-check-text";
    text.textContent = c.text;
    el.appendChild(text);
    if (!c.action) return;
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "ghost-btn folder-check-setup";
    btn.textContent = c.busy ? "Setting up…" : "Set up automatically";
    btn.disabled = c.busy;
    btn.addEventListener("click", () => setUp(f));
    el.appendChild(btn);
    const note = document.createElement("p");
    note.className = "hint folder-check-note";
    note.textContent = c.note;
    el.appendChild(note);
    if (c.error) {
      const err = document.createElement("p");
      err.className = "error folder-check-error";
      err.textContent = c.error;
      el.appendChild(err);
    }
  }

  async function check(f) {
    f.check = { info: null, error: null, busy: false, setupError: null };
    paint(f);
    await ensureCatalog();
    renderGuide();
    try {
      f.check.info = await invoke("inspect_project", { folderPath: f.path });
    } catch (err) {
      f.check.error = String(err);
    }
    paint(f);
  }

  async function setUp(f) {
    f.check.busy = true;
    f.check.setupError = null;
    paint(f);
    try {
      await invoke("init_project_repo", { folderPath: f.path });
    } catch (err) {
      f.check.busy = false;
      f.check.setupError = String(err);
      paint(f);
      return;
    }
    await check(f);
    if (wizardFolders.includes(f)) document.getElementById("wiz-folders-next-btn").click();
  }

  /** Adds the chip under a folder's row; the first time a folder is shown, checks it. */
  function renderRow(li, f) {
    f.checkEl = document.createElement("div");
    li.appendChild(f.checkEl);
    if (f.check) paint(f);
    else check(f);
  }

  /** Next inspected the folder again: keep its chip in step. */
  function update(f, info) {
    f.check = { info, error: null, busy: false, setupError: null };
    paint(f);
  }

  return { chip, guideLines, stackInfo, notInGitError, INIT_NOTE, renderRow, renderGuide, ensureCatalog, update };
})();

if (typeof module !== "undefined" && module.exports) {
  module.exports = { FolderCheck, GIT_MISSING_TEXT };
}
