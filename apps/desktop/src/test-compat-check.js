// Run with: node --test apps/desktop/src/test-compat-check.js
const test = require("node:test");
const assert = require("node:assert/strict");

// A minimal Tauri + DOM, enough for gate()/slot().
let nextReport = null;
const progress = [];
global.window = {
  __TAURI__: {
    core: { invoke: async (cmd, args) => { assert.equal(cmd, "check_update_compat"); assert.ok(args.sessionId); return typeof nextReport === "function" ? nextReport() : nextReport; } },
    event: { listen: (name, cb) => { assert.equal(name, "compose-test-progress"); progress.push(cb); } },
  },
};
let clickHandler = null;
const el = { style: {}, innerHTML: "", hidden: true, classList: { toggle: (_c, on) => (el.hidden = on) }, addEventListener: (_t, cb) => (clickHandler = cb) };
global.document = { createElement: () => el };
const click = (what) => clickHandler({ target: { closest: () => ({ dataset: { compat: what } }) } });

const { CompatCheck: CC } = require("./compat-check.js");

const report = (o) => ({ ok: true, problems: [], notes: [], test: null, tree_key: "t1", ...o });
const failedBuild = report({
  ok: false,
  problems: ["The app stopped with exit code 1 right after starting."],
  test: { ok: false, error: "The app stopped...", output_tail: Array.from({ length: 30 }, (_, i) => `line ${i}`).join("\n") + "\nSyntaxError: invalid syntax", secs: 40, cached: false },
});

test("compatible states say how they were checked", () => {
  assert.equal(CC.view({ kind: "done", report: report({ test: { ok: true, secs: 34, cached: false, output_tail: "" } }) }).title, "Compatible - built and started in 34 s");
  assert.equal(CC.view({ kind: "done", report: report({ test: { ok: true, secs: 0, cached: true } }) }).title, "Compatible - already built and started for this version");
  const quick = CC.view({ kind: "done", report: report({ notes: ["Podman isn't available, so only the quick checks ran."] }) });
  assert.equal(quick.title, "Compatible - quick checks passed");
  assert.deepEqual(quick.notes, ["Podman isn't available, so only the quick checks ran."]);
  assert.deepEqual(quick.actions, []);
});

test("a failure shows the reason, the last log lines and both buttons", () => {
  const v = CC.view({ kind: "done", report: failedBuild });
  assert.equal(v.kind, "fail");
  assert.deepEqual(v.problems, failedBuild.problems);
  assert.equal(v.log.split("\n").length, 12);
  assert.ok(v.log.endsWith("SyntaxError: invalid syntax"));
  assert.deepEqual(v.actions, ["recheck", "anyway"]);
  const h = CC.html(v);
  assert.ok(h.includes("Fix and re-check") && h.includes("Send anyway") && h.includes("the other device may not be able to run it."));
  // A quick-check failure has no log.
  assert.equal(CC.view({ kind: "done", report: report({ ok: false, problems: ["The database dump /x.sql is missing."] }) }).log, "");
});

test("html escapes what the project printed", () => {
  const h = CC.html(CC.view({ kind: "done", report: report({ ok: false, problems: ["<b>x</b>"], test: { ok: false, output_tail: "<script>", secs: 1 } }) }));
  assert.ok(!h.includes("<b>x</b>") && !h.includes("<script>") && h.includes("&lt;script&gt;"));
});

test("a failing build blocks the send; Send anyway sends", async () => {
  let sent = 0;
  nextReport = failedBuild;
  await CC.gate("s1", () => sent++);
  assert.equal(sent, 0, "a failed check must not send");
  assert.equal(el.hidden, false);
  assert.ok(el.innerHTML.includes("Not compatible") && el.innerHTML.includes("SyntaxError"));
  click("anyway");
  assert.equal(sent, 1);
  assert.equal(el.hidden, true, "the failure is gone once sent anyway");
});

test("Fix and re-check runs again and sends once it passes; progress shows while checking", async () => {
  let sent = 0;
  nextReport = failedBuild;
  await CC.gate("s1", () => sent++);
  let release;
  nextReport = () => new Promise((r) => (release = r));
  click("recheck");
  assert.ok(el.innerHTML.includes("Checking this update..."));
  assert.ok(!el.innerHTML.includes("Not compatible"), "no stale failure while re-checking");
  progress.forEach((cb) => cb({ payload: { line: "Building and starting the containers" } }));
  assert.ok(el.innerHTML.includes("Building and starting the containers"));
  release(report({ test: { ok: true, secs: 12, cached: false, output_tail: "" } }));
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(sent, 1);
  assert.ok(el.innerHTML.includes("Compatible - built and started in 12 s"));
});

test("a check for one session isn't shown on another's page", async () => {
  nextReport = failedBuild;
  await CC.gate("s1", () => {});
  CC.slot("s2");
  assert.equal(el.hidden, true);
});

test("a backend error is a failure with the same two ways out", async () => {
  let sent = 0;
  nextReport = () => Promise.reject("no open session with id s9");
  await CC.gate("s9", () => sent++);
  assert.equal(sent, 0);
  assert.ok(el.innerHTML.includes("no open session with id s9") && el.innerHTML.includes("Send anyway"));
});
