// Real, automated regression test for the one JS/Rust IPC-boundary
// translation that has now caused two real bugs (round 22: `engine`
// silently dropped; round 25: `file_path` sent as `filePath`) - see
// wizard-payload.js's own comment for the full root cause. Run with:
//   node --test apps/desktop/src/test-wizard-payload.js
// node:test + node:assert are built into Node itself (stable since Node
// 20) - no new dependency, same technique round 24 already established
// for web/test-magic-link-logic.js.
const test = require("node:test");
const assert = require("node:assert/strict");
const { buildWizardFoldersPayload } = require("./wizard-payload.js");

// The exact field set apps/desktop/src-tauri/src/commands.rs's
// DumpPlanDto/FolderPlanDto declare - asserted against directly so a
// future rename on either side that isn't mirrored here fails loudly,
// rather than only failing at runtime against a real running app.
const RUST_FOLDER_FIELDS = ["path", "dump"].sort();
const RUST_DUMP_FIELDS = ["schema", "file_path", "engine"].sort();

test("buildWizardFoldersPayload", async (t) => {
  await t.test("a folder with a database dump uses exactly Rust's field names", () => {
    const wizardFolders = [
      {
        path: "/home/dev/my-project",
        needsDb: true,
        dump: { schema: "my_app", filePath: "/tmp/localsync/db-exports/abc123.sql", engine: "mysql" },
      },
    ];
    const payload = buildWizardFoldersPayload(wizardFolders);
    assert.equal(payload.length, 1);
    assert.deepEqual(Object.keys(payload[0]).sort(), RUST_FOLDER_FIELDS);
    assert.ok(payload[0].dump, "dump should not be null for a needsDb folder with a real dump");
    assert.deepEqual(Object.keys(payload[0].dump).sort(), RUST_DUMP_FIELDS);
    // The actual round-25 bug: this used to be `filePath` (this app's own
    // internal JS naming convention) instead of `file_path` - Rust's
    // serde_json deserializer is case-sensitive and matches the declared
    // field name exactly, so a stray camelCase key here means a real
    // send fails outright with "missing field 'file_path'".
    assert.equal(payload[0].dump.file_path, "/tmp/localsync/db-exports/abc123.sql");
    assert.equal(payload[0].dump.schema, "my_app");
    assert.equal(payload[0].dump.engine, "mysql");
    assert.equal(payload[0].dump.filePath, undefined, "the outbound object must not carry the internal camelCase key too");
  });

  await t.test("a folder that doesn't need a database sends dump: null, not an empty object", () => {
    const wizardFolders = [{ path: "/home/dev/static-site", needsDb: false, dump: null }];
    const payload = buildWizardFoldersPayload(wizardFolders);
    assert.equal(payload[0].dump, null);
  });

  await t.test("needsDb true but no dump yet still sends null (a genuinely incomplete folder, not a bug)", () => {
    const wizardFolders = [{ path: "/home/dev/mid-wizard", needsDb: true, dump: null }];
    const payload = buildWizardFoldersPayload(wizardFolders);
    assert.equal(payload[0].dump, null);
  });

  await t.test("multiple folders each keep their own independent dump (or lack of one)", () => {
    const wizardFolders = [
      { path: "/a", needsDb: true, dump: { schema: "a_db", filePath: "/tmp/a.sql", engine: "postgres" } },
      { path: "/b", needsDb: false, dump: null },
      { path: "/c", needsDb: true, dump: { schema: "c_db", filePath: "/tmp/c.tar.gz", engine: "mongodb" } },
    ];
    const payload = buildWizardFoldersPayload(wizardFolders);
    assert.equal(payload.length, 3);
    assert.equal(payload[0].dump.file_path, "/tmp/a.sql");
    assert.equal(payload[1].dump, null);
    assert.equal(payload[2].dump.engine, "mongodb");
  });

  await t.test("path is passed through unchanged", () => {
    const wizardFolders = [{ path: "C:\\Users\\dev\\project", needsDb: false, dump: null }];
    const payload = buildWizardFoldersPayload(wizardFolders);
    assert.equal(payload[0].path, "C:\\Users\\dev\\project");
  });

  await t.test("an empty wizardFolders array produces an empty payload", () => {
    assert.deepEqual(buildWizardFoldersPayload([]), []);
  });
});
