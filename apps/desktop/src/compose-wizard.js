// The pure form logic of the compose wizard (a project with no
// docker-compose.yml is described through a few dropdowns, and the backend
// generates and test-runs the compose file). No DOM, no Tauri: kept out of
// app.js so Node can test it (same split as session-model.js). Loaded by a
// plain <script> before app.js.
//
// Every option, default and pre-fill comes from the `catalog` argument (the
// backend's `compose_catalog` result) - nothing here lists a runtime, a
// version, a tool or a command. The two free-text things are the run command
// and the environment rows.
//
// Form state (`st`):
//   { runtime, runtimeVersion, buildTool, runCommand, runCommandEdited,
//     artifactPath, artifactPathEdited, port,           // app setup
//     dbEngine, dbVersion, dbEnvPreset,                 // services
//     extras: { <kind>: { checked, version } },
//     env: [{ key, value }] }

const ComposeForm = (() => {
  const newest = (versions) => (versions.length ? versions[versions.length - 1] : ""); // catalog: "Newest last"

  const runtimeInfo = (catalog, runtime) => catalog.runtimes.find((r) => r.runtime === runtime) || null;
  const toolInfo = (catalog, runtime, tool) => {
    const r = runtimeInfo(catalog, runtime);
    return (r && r.build_tools.find((t) => t.tool === tool)) || null;
  };

  function newState() {
    return {
      runtime: "", runtimeVersion: "", buildTool: "",
      runCommand: "", runCommandEdited: false,
      artifactPath: "", artifactPathEdited: false,
      port: "",
      dbEngine: "", dbVersion: "", dbEnvPreset: "",
      extras: {}, env: [],
    };
  }

  /** Pre-fills the run command / artifact path from the chosen tool, except where the person has edited them. */
  function applyDefaults(catalog, st) {
    const tool = toolInfo(catalog, st.runtime, st.buildTool);
    if (!st.runCommandEdited) st.runCommand = (tool && tool.default_run_command) || "";
    if (!st.artifactPathEdited) st.artifactPath = (tool && tool.needs_artifact_path && tool.default_artifact_path) || "";
  }

  /** Changing runtime resets version and tool to that runtime's own (newest version, first tool). "" clears all. */
  function setRuntime(catalog, st, runtime) {
    const r = runtimeInfo(catalog, runtime);
    st.runtime = r ? r.runtime : "";
    st.runtimeVersion = r ? newest(r.versions) : "";
    st.buildTool = r && r.build_tools.length ? r.build_tools[0].tool : "";
    applyDefaults(catalog, st);
  }

  function setBuildTool(catalog, st, tool) {
    st.buildTool = tool;
    applyDefaults(catalog, st);
  }

  /** Typing in the run command: it counts as edited only if it differs from what the tool would pre-fill. */
  function setRunCommand(catalog, st, value) {
    const tool = toolInfo(catalog, st.runtime, st.buildTool);
    st.runCommand = value;
    st.runCommandEdited = value !== ((tool && tool.default_run_command) || "");
  }

  function setArtifactPath(catalog, st, value) {
    const tool = toolInfo(catalog, st.runtime, st.buildTool);
    st.artifactPath = value;
    st.artifactPathEdited = value !== ((tool && tool.needs_artifact_path && tool.default_artifact_path) || "");
  }

  /** Port input keeps digits only (a number input still lets "e" and "-" through). */
  const cleanPortInput = (s) => String(s).replace(/\D/g, "").slice(0, 5);

  /**
   * Fills in the services-step defaults the first time (or when the database
   * engine changed since): newest catalog version of the DB, first preset,
   * every extra unchecked at its newest version. Existing answers are kept.
   */
  function initServices(catalog, st, engine) {
    const dbs = engine ? catalog.databases.find((d) => d.kind === engine) : null;
    if (!dbs) {
      st.dbEngine = "";
      st.dbVersion = "";
    } else if (st.dbEngine !== engine || !dbs.versions.includes(st.dbVersion)) {
      st.dbEngine = engine;
      st.dbVersion = newest(dbs.versions);
    }
    if (!catalog.db_env_presets.some((p) => p.preset === st.dbEnvPreset)) {
      st.dbEnvPreset = catalog.db_env_presets.length ? catalog.db_env_presets[0].preset : "";
    }
    for (const e of catalog.extras) {
      const cur = st.extras[e.kind];
      if (!cur) st.extras[e.kind] = { checked: false, version: newest(e.versions) };
      else if (!e.versions.includes(cur.version)) cur.version = newest(e.versions);
    }
  }

  const addEnvRow = (st) => st.env.push({ key: "", value: "" });
  const removeEnvRow = (st, i) => st.env.splice(i, 1);

  /**
   * Trivial checks that cannot disagree with the backend: a runtime must be
   * chosen and the port must be a real port number. (Everything else,
   * including blank env keys and the run command, is the backend's call.)
   */
  function clientErrors(st) {
    const errs = [];
    if (!st.runtime) errs.push({ field: "runtime", message: "Choose the runtime your app runs on." });
    const p = st.port === "" ? NaN : Number(st.port);
    if (!Number.isInteger(p) || p < 1 || p > 65535) {
      errs.push({ field: "port", message: "Enter the port your app listens on (a number from 1 to 65535)." });
    }
    return errs;
  }

  /**
   * Builds the ComposeSpec the backend takes. `db` = { engine, database } from
   * the database step, or null. Fully blank env rows are dropped; the
   * returned `map` remembers which extra / env row each spec index came from
   * so backend errors like `env[1].key` can be shown beside the right row.
   */
  function buildSpec(catalog, st, db) {
    const tool = toolInfo(catalog, st.runtime, st.buildTool);
    const extraKinds = catalog.extras.map((e) => e.kind).filter((k) => st.extras[k] && st.extras[k].checked);
    const envRows = [];
    st.env.forEach((r, i) => {
      if (r.key.trim() !== "" || r.value !== "") envRows.push(i);
    });
    const spec = {
      runtime: st.runtime,
      runtime_version: st.runtimeVersion,
      build_tool: st.buildTool,
      run_command: st.runCommand.trim(),
      port: Number(st.port),
      artifact_path: tool && tool.needs_artifact_path ? st.artifactPath.trim() : null,
      database: db ? { engine: db.engine, version: st.dbVersion, database: db.database } : null,
      db_env_preset: st.dbEnvPreset || (catalog.db_env_presets[0] || {}).preset || "",
      extras: extraKinds.map((k) => ({ kind: k, version: st.extras[k].version })),
      env: envRows.map((i) => ({ key: st.env[i].key.trim(), value: st.env[i].value })),
    };
    return { spec, map: { extras: extraKinds, env: envRows } };
  }

  /** Which wizard step owns a backend field name. */
  const fieldStep = (field) => (/^(database|extras|env)(\W|$)/.test(field) ? "services" : "app");
  const STEP_ORDER = ["app", "services"];

  /**
   * Turns backend `[{field, message}]` into `[{field, message, step, slot}]`:
   * `slot` names the on-screen place to show it - `extras[1].version` becomes
   * `extra.<kind>`, `env[1].key` becomes `env.<row>.key` (the row on screen,
   * since blank rows were dropped from the spec) - other fields keep their name.
   */
  function mapErrors(errors, map) {
    return errors.map((e) => {
      let slot = e.field;
      let m = /^extras\[(\d+)\]/.exec(e.field);
      if (m) slot = `extra.${map.extras[Number(m[1])]}`;
      m = /^env\[(\d+)\]\.(key|value)$/.exec(e.field);
      if (m) slot = `env.${map.env[Number(m[1])]}.${m[2]}`;
      return { field: e.field, message: e.message, step: fieldStep(e.field), slot };
    });
  }

  /** The earliest step (in wizard order) that has an error, or null. */
  const firstErrorStep = (mapped) => STEP_ORDER.find((s) => mapped.some((e) => e.step === s)) || null;

  /** What a test run vouches for: the spec plus the database dump it ran with. Any change makes it stale. */
  const testKey = (spec, dump) => JSON.stringify({ spec, dump: dump ? { schema: dump.schema, file_path: dump.file_path, engine: dump.engine } : null });
  const isTestCurrent = (testedKey, spec, dump) => testedKey !== null && testedKey === testKey(spec, dump);

  return {
    newState, setRuntime, setBuildTool, setRunCommand, setArtifactPath, applyDefaults, cleanPortInput,
    initServices, addEnvRow, removeEnvRow, clientErrors, buildSpec, mapErrors, firstErrorStep, fieldStep,
    testKey, isTestCurrent, runtimeInfo, toolInfo, newest,
  };
})();

if (typeof module !== "undefined" && module.exports) {
  module.exports = ComposeForm;
}
