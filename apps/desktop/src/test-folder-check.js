// Run with: node --test apps/desktop/src/test-folder-check.js
const test = require("node:test");
const assert = require("node:assert/strict");
const { FolderCheck: FC, GIT_MISSING_TEXT } = require("./folder-check.js");

// The shape of the real compose_catalog (only what the folder check reads).
const catalog = {
  runtimes: [
    { runtime: "java", label: "Java", build_tools: [{ tool: "maven", label: "Maven" }, { tool: "gradle", label: "Gradle" }] },
    { runtime: "node", label: "Node.js", build_tools: [{ tool: "npm", label: "npm" }] },
    { runtime: "python", label: "Python", build_tools: [{ tool: "pip", label: "pip (requirements.txt)" }] },
    { runtime: "go", label: "Go", build_tools: [{ tool: "go", label: "Go modules (go build)" }] },
  ],
  databases: [{ kind: "mysql", label: "MySQL" }, { kind: "postgres", label: "PostgreSQL" }, { kind: "mongodb", label: "MongoDB" }],
};
const info = (o) => ({ has_compose: false, is_git_repo: true, has_commits: true, git_missing: false, detected_stack: "unknown", ...o });
const chip = (o) => FC.chip({ info: info(o) }, catalog);

test("a supported stack in git gets a green chip named from the catalog", () => {
  assert.deepEqual(chip({ detected_stack: "python" }), { kind: "ok", text: "Python project detected" });
  assert.equal(chip({ detected_stack: "java-maven" }).text, "Java (Maven) project detected");
  assert.equal(chip({ detected_stack: "java-gradle" }).text, "Java (Gradle) project detected");
  assert.equal(chip({ detected_stack: "node" }).text, "Node.js project detected");
  assert.equal(chip({ detected_stack: "go" }).text, "Go project detected");
  assert.equal(chip({ detected_stack: "compose", has_compose: true }).text, "Docker Compose project detected");
});

test("an unknown stack in git needs no chip", () => {
  assert.equal(chip({}).kind, "none");
});

test("a folder not in git, or with no commits, offers to set it up", () => {
  const c = chip({ is_git_repo: false, has_commits: false, detected_stack: "python" });
  assert.equal(c.kind, "git");
  assert.equal(c.text, "Not tracked by git yet");
  assert.equal(c.action, true);
  assert.equal(c.note, "Adds a hidden .git folder and one commit; your files aren't changed.");
  assert.equal(chip({ has_commits: false }).text, "No git commits yet");
  // Even with a compose file of its own: the snapshot is built from a commit.
  assert.equal(chip({ is_git_repo: false, has_commits: false, has_compose: true, detected_stack: "compose" }).kind, "git");
});

test("set-up progress and a failed set-up show on the chip, and clear on retry", () => {
  const base = info({ is_git_repo: false, has_commits: false });
  assert.equal(FC.chip({ info: base, busy: true }, catalog).busy, true);
  assert.equal(FC.chip({ info: base, setupError: "read-only" }, catalog).error, "read-only");
  assert.equal(FC.chip({ info: base, busy: true, setupError: null }, catalog).error, "");
});

test("a stack the catalog has no runtime for is 'not supported yet'", () => {
  assert.deepEqual(chip({ detected_stack: "rust" }), { kind: "bad", text: "Not supported yet: Rust" });
  assert.equal(chip({ detected_stack: "dotnet", is_git_repo: false }).text, "Not supported yet: .NET", "no point offering git for it");
  // Supported is whatever the catalog says: a Rust runtime added there turns it green.
  const withRust = { ...catalog, runtimes: [...catalog.runtimes, { runtime: "rust", label: "Rust", build_tools: [] }] };
  assert.equal(FC.chip({ info: info({ detected_stack: "rust" }) }, withRust).kind, "ok");
  // A Java build tool the catalog doesn't offer.
  const mavenOnly = { ...catalog, runtimes: [{ runtime: "java", label: "Java", build_tools: [{ tool: "maven", label: "Maven" }] }] };
  assert.equal(FC.chip({ info: info({ detected_stack: "java-gradle" }) }, mavenOnly).kind, "bad");
});

test("pending, failed checks and missing git", () => {
  assert.equal(FC.chip(null, catalog).kind, "pending");
  assert.equal(FC.chip({ info: null, error: null }, catalog).kind, "pending");
  assert.deepEqual(FC.chip({ error: "boom" }, catalog), { kind: "bad", text: "Couldn't check this folder: boom" });
  assert.deepEqual(chip({ git_missing: true, is_git_repo: false }), { kind: "bad", text: GIT_MISSING_TEXT });
  // No catalog (it failed to load): no stack verdict, the git chip still works.
  assert.equal(FC.chip({ info: info({ detected_stack: "rust" }) }, null).kind, "none");
  assert.equal(FC.chip({ info: info({ detected_stack: "rust", has_commits: false }) }, null).kind, "git");
});

test("the guide is built from the catalog", () => {
  const [works, notYet, needs] = FC.guideLines(catalog);
  assert.equal(works, "Works: web apps and APIs that listen on a port, in Java, Node.js, Python or Go, optionally with MySQL, PostgreSQL or MongoDB.");
  assert.equal(notYet, "Not supported yet: mobile apps, Rust, PHP, Ruby, .NET, static-only sites.");
  assert.match(needs, /git with at least one commit \(LocalSync can set that up/);
  assert.match(needs, /docker-compose\.yml/);
  const grown = { runtimes: [...catalog.runtimes, { runtime: "php", label: "PHP", build_tools: [] }], databases: [{ label: "MySQL" }] };
  const [w2, n2] = FC.guideLines(grown);
  assert.equal(w2, "Works: web apps and APIs that listen on a port, in Java, Node.js, Python, Go or PHP, optionally with MySQL.");
  assert.equal(n2, "Not supported yet: mobile apps, Rust, Ruby, .NET, static-only sites.");
});

test("Next's message for a folder with nothing committed", () => {
  assert.equal(FC.notInGitError("this folder"), `LocalSync sends what's committed in git, and this folder has nothing committed yet. Use "Set up automatically" under it.`);
});
