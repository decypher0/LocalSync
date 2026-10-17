// Setup's "Files outside the project" (#wiz-externals, in the compose
// wizard's Services step): settings in the project's config files that point
// at a file or folder elsewhere on this computer (backend: externals.rs,
// detect_external_refs / describe_external). Ticked ones are sent along,
// mounted in the app's container, and LocalSync's copy of the config file is
// changed to the in-container path.
//
// The folder entry (wizardFolders[0]) keeps:
//   externalItems - [{ ref, selected, manual }] for this UI
//   externals     - the ticked, sendable refs: exactly FolderPlanDto.externals
// Loaded by a plain <script> before app.js; the pure part is tested by
// test-externals-step.js under Node.

const ExternalsStep = (() => {
  // Must match externals.rs SIZE_WARN_BYTES / NOT_SUPPORTED_YET.
  const SIZE_WARN_BYTES = 50 * 1024 * 1024;
  const NOT_SUPPORTED_YET =
    "Not supported yet: changing paths inside XML files, values split over several lines, and projects with their own docker-compose.yml.";

  const refKey = (r) => `${r.config_file}\n${r.key}\n${r.path}`;
  const sendable = (r) => r.status === "outside_exists";
  const defaultSelected = (r) => sendable(r) && r.size_bytes <= SIZE_WARN_BYTES;

  function formatSize(n) {
    if (n > SIZE_WARN_BYTES) return "over 50 MB";
    if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
    if (n >= 1024) return `${Math.round(n / 1024)} KB`;
    return `${n} bytes`;
  }

  /** Freshly detected refs -> items, keeping earlier ticks and hand-added rows. */
  function merge(refs, prevItems) {
    const prev = new Map((prevItems || []).map((i) => [refKey(i.ref), i.selected]));
    const items = refs.map((ref) => ({ ref, selected: prev.has(refKey(ref)) ? prev.get(refKey(ref)) && sendable(ref) : defaultSelected(ref), manual: false }));
    for (const i of prevItems || []) if (i.manual && !items.some((x) => refKey(x.ref) === refKey(i.ref))) items.push(i);
    return items;
  }

  /** What is sent: FolderPlanDto.externals. */
  const selectedRefs = (items) => (items || []).filter((i) => i.selected && sendable(i.ref)).map((i) => i.ref);

  /** Everything one row shows. */
  function describe(item) {
    const r = item.ref;
    const where = r.config_file ? `${r.config_file}${r.line ? `, line ${r.line}` : ""}` : "added by hand";
    const out = { key: r.key, where, path: r.path, selectable: sendable(r), size: null, note: null, warn: null };
    if (r.status === "inside") {
      out.note = `Inside the project. A relative path would work on any computer: ${r.suggested_relative}`;
    } else if (r.status === "outside_missing") {
      out.warn = "Not on this computer, so it can't be included.";
    } else {
      out.size = formatSize(r.size_bytes);
      if (r.size_bytes > SIZE_WARN_BYTES) out.warn = "Over 50 MB. It is copied into every send, so it's not ticked by default.";
      if (!r.config_file) out.note = `Your app will find it at ${r.container_path}.`;
      else if (r.rewritable) out.note = `Your app will find it at ${r.container_path} (LocalSync changes ${r.key} in its own copy of ${r.config_file}).`;
      else out.note = `Mounted at ${r.container_path}, but ${r.config_file} isn't changed: LocalSync can't change XML files yet.`;
    }
    return out;
  }

  /** Rows shown as a list, and the paths missing here (grouped, they're often other machines' paths). */
  function groups(items) {
    const list = items.filter((i) => i.ref.status !== "outside_missing");
    const missing = items.filter((i) => i.ref.status === "outside_missing");
    return { list, missing };
  }

  // ---------- DOM (browser only) ----------

  function el(tag, cls, text) {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = text;
    return e;
  }

  function row(item, onToggle) {
    const d = describe(item);
    const li = el("li", "ext-row");
    const head = el("div", "ext-head");
    if (d.selectable) {
      const label = el("label");
      const cb = el("input");
      cb.type = "checkbox";
      cb.checked = item.selected;
      cb.addEventListener("change", () => onToggle(item, cb.checked));
      label.append(cb, document.createTextNode(" "), el("code", null, d.key));
      head.append(label);
    } else {
      head.append(el("code", null, d.key));
    }
    head.append(el("span", "hint-inline", d.where));
    li.append(head, el("div", "mono ext-path", d.size ? `${d.path} (${d.size})` : d.path));
    if (d.note) li.append(el("p", "hint", d.note));
    if (d.warn) li.append(el("p", "warn", d.warn));
    return li;
  }

  /**
   * Renders into `container`. deps = { invoke, open, onChange }: Tauri's
   * invoke and dialog open, and a callback after the selection changes.
   */
  function render(container, folder, deps) {
    container.innerHTML = "";
    const items = folder.externalItems || [];
    folder.externals = selectedRefs(items);
    const changed = () => {
      folder.externals = selectedRefs(folder.externalItems);
      if (deps.onChange) deps.onChange();
    };
    const onToggle = (item, on) => {
      item.selected = on;
      changed();
      render(container, folder, deps);
    };

    container.append(el("h4", null, "Files outside the project"));
    container.append(
      el("p", "hint", "Settings in this project that point at a file or folder elsewhere on this computer. Ticked ones are sent along and put in the app's container; your own files are never changed.")
    );
    if (folder.externalsError) container.append(el("p", "error", folder.externalsError));
    const { list, missing } = groups(items);
    if (folder.externalsScanning) container.append(el("p", "hint-inline", "Looking through the config files…"));
    else if (!items.length && !folder.externalsError) container.append(el("p", "hint", "No settings point outside the project."));
    if (list.length) {
      const ul = el("ul", "cw-rows ext-list");
      for (const i of list) ul.append(row(i, onToggle));
      container.append(ul);
    }
    if (missing.length) {
      const det = el("details", "ext-missing");
      det.append(el("summary", null, `${missing.length} ${missing.length === 1 ? "setting points" : "settings point"} at a path that isn't on this computer`));
      const ul = el("ul", "cw-rows ext-list");
      for (const i of missing) ul.append(row(i, onToggle));
      det.append(ul);
      container.append(det);
    }

    // "Add a file or folder...": the setting's key, then the file or folder.
    const add = el("div", "ext-add");
    const key = el("input");
    key.type = "text";
    key.id = "ext-add-key";
    key.placeholder = "Setting that points at it, e.g. company.configuration.filepath";
    key.setAttribute("aria-label", "Setting (key) that points at the file or folder");
    const err = el("p", "error");
    const pick = async (directory) => {
      err.textContent = "";
      if (!key.value.trim()) {
        err.textContent = "Enter the setting (key) that points at it first.";
        return;
      }
      const path = await deps.open({ directory, multiple: false });
      if (!path) return;
      try {
        const ref = await deps.invoke("describe_external", { folderPath: folder.path, key: key.value.trim(), path });
        folder.externalItems = [...(folder.externalItems || []).filter((i) => refKey(i.ref) !== refKey(ref)), { ref, selected: sendable(ref), manual: true }];
        changed();
        render(container, folder, deps);
      } catch (e) {
        err.textContent = String(e);
      }
    };
    const fileBtn = el("button", "ghost-btn", "Add a file…");
    fileBtn.type = "button";
    fileBtn.addEventListener("click", () => pick(false));
    const dirBtn = el("button", "ghost-btn", "Add a folder…");
    dirBtn.type = "button";
    dirBtn.addEventListener("click", () => pick(true));
    add.append(key, fileBtn, dirBtn);
    container.append(add, err);
    container.append(el("p", "hint", NOT_SUPPORTED_YET));
  }

  /** Scans the folder once (again when the folder changes), then renders. */
  async function show(container, folder, deps) {
    if (folder.externalsScannedFor === folder.path) return render(container, folder, deps);
    folder.externalsScannedFor = folder.path;
    folder.externalsScanning = true;
    folder.externalsError = null;
    render(container, folder, deps);
    try {
      folder.externalItems = merge(await deps.invoke("detect_external_refs", { folderPath: folder.path }), folder.externalItems);
    } catch (e) {
      folder.externalsError = `Couldn't look through the config files: ${e}`;
      folder.externalsScannedFor = null;
    }
    folder.externalsScanning = false;
    render(container, folder, deps);
  }

  return { SIZE_WARN_BYTES, NOT_SUPPORTED_YET, formatSize, merge, selectedRefs, describe, groups, render, show };
})();

if (typeof module !== "undefined" && module.exports) {
  module.exports = ExternalsStep;
}
