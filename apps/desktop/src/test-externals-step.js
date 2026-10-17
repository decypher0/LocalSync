// Run with: node --test apps/desktop/src/test-externals-step.js
const test = require("node:test");
const assert = require("node:assert/strict");
const X = require("./externals-step.js");

// The shape detect_external_refs returns (externals.rs ExternalRef).
const ref = (o) => ({
  id: "tenants-xml-1a2b3c",
  key: "company.configuration.filepath",
  config_file: "src/main/resources/application.properties",
  value: "C:/Users/Some One/configuration/tenants.xml",
  path: "C:/Users/Some One/configuration/tenants.xml",
  kind: "file",
  status: "outside_exists",
  size_bytes: 1631,
  line: 13,
  container_path: "/localsync/external/tenants-xml-1a2b3c/tenants.xml",
  rewritable: true,
  ...o,
});
const big = ref({ key: "mediafiles.base.path", path: "C:/media", kind: "dir", size_bytes: X.SIZE_WARN_BYTES + 1 });
const missing = ref({ key: "old.path", path: "/usr/local/artigem/x.xml", status: "outside_missing", size_bytes: 0 });
const inside = ref({ key: "inner", path: "C:/proj/conf/x.xml", status: "inside", suggested_relative: "conf/x.xml" });

test("existing files outside the project are ticked; big, missing and inside ones aren't", () => {
  const items = X.merge([ref(), big, missing, inside]);
  assert.deepEqual(items.map((i) => i.selected), [true, false, false, false]);
  assert.deepEqual(X.selectedRefs(items).map((r) => r.key), ["company.configuration.filepath"]);
});

test("a big folder can still be ticked by hand and is then sent", () => {
  const items = X.merge([ref(), big]);
  items[1].selected = true;
  assert.equal(X.selectedRefs(items).length, 2);
});

test("a missing path is never sent, even if marked selected", () => {
  assert.deepEqual(X.selectedRefs([{ ref: missing, selected: true }]), []);
});

test("re-scanning keeps earlier choices and hand-added rows", () => {
  const first = X.merge([ref(), big]);
  first[0].selected = false;
  const manual = { ref: ref({ key: "hand.key", config_file: "", path: "C:/elsewhere/a.txt" }), selected: true, manual: true };
  const again = X.merge([ref(), big], [...first, manual]);
  assert.deepEqual(again.map((i) => [i.ref.key, i.selected]), [
    ["company.configuration.filepath", false],
    ["mediafiles.base.path", false],
    ["hand.key", true],
  ]);
});

test("each row says where the setting is, the size, and where the app finds it", () => {
  const d = X.describe({ ref: ref(), selected: true });
  assert.equal(d.where, "src/main/resources/application.properties, line 13");
  assert.equal(d.size, "2 KB");
  assert.match(d.note, /\/localsync\/external\/tenants-xml-1a2b3c\/tenants\.xml/);
  assert.equal(d.warn, null);
  assert.equal(X.describe({ ref: big }).warn, "Over 50 MB. It is copied into every send, so it's not ticked by default.");
  assert.equal(X.describe({ ref: big }).size, "over 50 MB");
  assert.equal(X.describe({ ref: missing }).warn, "Not on this computer, so it can't be included.");
  assert.equal(X.describe({ ref: missing }).selectable, false);
  assert.match(X.describe({ ref: inside }).note, /conf\/x\.xml/);
  assert.match(X.describe({ ref: ref({ config_file: "conf/app.xml", rewritable: false }) }).note, /can't change XML files yet/);
  assert.equal(X.describe({ ref: ref({ config_file: "", line: 0 }) }).where, "added by hand");
});

test("missing paths are grouped apart from the list", () => {
  const g = X.groups(X.merge([ref(), missing, inside]));
  assert.deepEqual(g.list.map((i) => i.ref.key), ["company.configuration.filepath", "inner"]);
  assert.deepEqual(g.missing.map((i) => i.ref.key), ["old.path"]);
});

test("sizes read plainly", () => {
  assert.equal(X.formatSize(12), "12 bytes");
  assert.equal(X.formatSize(5 * 1024 * 1024), "5.0 MB");
});

test("the payload builder sends exactly the ticked refs", () => {
  const { buildWizardFoldersPayload } = require("./wizard-payload.js");
  const folder = { path: "/p", needsDb: false, compose: { runtime: "python" }, externals: X.selectedRefs(X.merge([ref(), big])) };
  const [out] = buildWizardFoldersPayload([folder]);
  assert.deepEqual(out.externals.map((r) => r.key), ["company.configuration.filepath"]);
  assert.equal(buildWizardFoldersPayload([{ path: "/q", needsDb: false }])[0].externals, undefined, "absent when there are none");
});
