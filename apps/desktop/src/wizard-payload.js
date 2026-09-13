// Round 25: the exact shape share_snapshot_wizard's `folders` argument must
// have, pulled out of app.js into its own file for one reason - app.js
// itself touches `window`/`document` from its very first executable line
// (const { invoke } = window.__TAURI__.core;) and throughout, so it can't
// be safely `require()`d from a plain Node test the way round 24's
// web/app.js can (that file is deliberately structured with an init()
// guard for exactly this reason). This file has no such problem: it's pure
// data shaping, so it's the one piece worth a real regression test on its
// own.
//
// Why this needs its own test at all: this exact translation step has now
// been the site of two real bugs that reached this far into the project
// before anything caught them - round 22 found `engine` silently dropped,
// round 25 found `filePath` sent instead of `file_path` (Tauri's
// invoke bridge converts a command's own top-level argument names between
// camelCase and snake_case automatically, but does NOT do that recursively
// for nested struct fields like DumpPlanDto's - those deserialize via
// plain serde_json against the exact declared Rust field name). Both bugs
// were invisible to every existing Rust test because those all call
// commands::share_snapshot_wizard directly, bypassing this JS
// reconstruction step entirely - the one place an actual person clicking
// Send goes through that no automated test did until now.
//
// Loaded via a plain <script> tag in index.html, before app.js - same
// vanilla, no-bundler, global-scope convention every other script in this
// app already uses, not a module import.

/**
 * Builds the `folders` argument for the `share_snapshot_wizard` Tauri
 * command from the wizard's own internal folder state. Must match
 * apps/desktop/src-tauri/src/commands.rs's `FolderPlanDto`/`DumpPlanDto`
 * field names exactly (snake_case `file_path`) - this function's own
 * output shape, not wizardFolders' internal one (which stays camelCase
 * `filePath`, this app's normal JS convention), is what actually crosses
 * the IPC boundary.
 */
function buildWizardFoldersPayload(wizardFolders) {
  return wizardFolders.map((f) => ({
    path: f.path,
    dump: f.needsDb && f.dump ? { schema: f.dump.schema, file_path: f.dump.filePath, engine: f.dump.engine } : null,
  }));
}

if (typeof module !== "undefined" && module.exports) {
  module.exports = { buildWizardFoldersPayload };
}
