// Run with: node --test apps/desktop/src/test-changes-panel.js
const test = require("node:test");
const assert = require("node:assert/strict");
const { ChangesPanel: CP } = require("./changes-panel.js");

// The shape project_changes returns (PushStatus fields flattened in).
const changes = (o) => ({ last_device_key: "dev-a", last_device_name: "Alice", changed: true, folders: [], ...o });
const folder = (o) => ({ path: "/w/app", files: [], dump_changed: false, not_sent: false, base_gone: false, ...o });
const file = (path, change_type, o) => ({ path, change_type, new_file: false, included: true, secret: false, ...o });

test("lists each change, with a tick box only for new files", () => {
  const m = CP.model(
    changes({
      folders: [
        folder({
          files: [
            file(".env", "added", { new_file: true, included: false, secret: true }),
            file("notes.md", "added", { new_file: true }),
            file("old.txt", "deleted"),
            file("src/app.rs", "modified"),
          ],
        }),
      ],
    })
  );
  assert.equal(m.title, "Changes since the last send to Alice");
  assert.equal(m.message, "");
  assert.equal(m.folders.length, 1);
  assert.equal(m.folders[0].name, "", "one folder needs no folder heading");
  assert.deepEqual(
    m.folders[0].lines.map((l) => [l.path, l.kind, l.tickable, l.checked]),
    [
      [".env", "Added", true, false],
      ["notes.md", "Added", true, true],
      ["old.txt", "Deleted", false, true],
      ["src/app.rs", "Modified", false, true],
    ]
  );
  const h = CP.html(m);
  assert.match(h, /data-change-file="\.env" data-folder="\/w\/app" >/, "the secret-looking file starts unticked");
  assert.match(h, /data-change-file="notes\.md" data-folder="\/w\/app" checked>/);
  assert.match(h, /may hold secrets, tick to send/);
  assert.doesNotMatch(h, /data-change-file="old\.txt"/, "a tracked file's change has no tick box");
});

test("nothing changed, never sent, a new commit with no file changes, and a re-exported dump", () => {
  assert.equal(CP.model(changes({ changed: false, folders: [folder({})] })).message, "No changes since the last send.");
  const fresh = CP.model({ last_device_key: null, last_device_name: null, changed: false, folders: [folder({ not_sent: true, files: [file("a.txt", "added", { new_file: true })] })] });
  assert.equal(fresh.message, "Not sent to this device yet.");
  assert.equal(fresh.title, "Changes since the last send");
  assert.equal(fresh.folders[0].lines[0].tickable, true, "new files can be chosen before the first send");
  assert.deepEqual(fresh.folders[0].notes, [], "said once, not per folder");
  assert.equal(CP.model(changes({ folders: [folder({})] })).message, "Committed since the last send - no file changes.");
  const dump = CP.model(changes({ folders: [folder({ dump_changed: true })] }));
  assert.equal(dump.message, "");
  assert.deepEqual(dump.folders[0].notes, ["Database dump exported again."]);
});

test("a last-sent version gone from git, and a folder this device never got", () => {
  const m = CP.model(
    changes({
      folders: [folder({ path: "C:\\work\\api", base_gone: true }), folder({ path: "/w/web", not_sent: true, files: [file("x.js", "added", { new_file: true })] })],
    })
  );
  assert.deepEqual(m.folders.map((f) => f.name), ["api", "web"], "several folders are headed by their names");
  assert.match(m.folders[0].notes[0], /no longer in git - Push update sends this folder in full/);
  assert.deepEqual(m.folders[1].notes, ["Not sent to this device yet."]);
});

test("a long list is cut with a count", () => {
  const files = Array.from({ length: CP.MAX_LINES + 3 }, (_, i) => file(`f${i}.txt`, "added"));
  const m = CP.model(changes({ folders: [folder({ files })] }));
  assert.equal(m.folders[0].lines.length, CP.MAX_LINES);
  assert.equal(m.folders[0].more, 3);
  assert.match(CP.html(m), /and 3 more files/);
});

test("pending and failed checks say so, and names are escaped", () => {
  assert.equal(CP.model({ pending: true }).message, "Checking for changes…");
  assert.equal(CP.model({ error: "git broke" }).message, "Couldn't check for changes: git broke");
  const h = CP.html(CP.model(changes({ folders: [folder({ files: [file("<b>.txt", "added", { new_file: true })] })] })), "Couldn't change what is sent: nope");
  assert.match(h, /&lt;b&gt;\.txt/);
  assert.doesNotMatch(h, /<b>\.txt/);
  assert.match(h, /<p class="lsv-error">Couldn&#39;t change what is sent: nope<\/p>/);
});
