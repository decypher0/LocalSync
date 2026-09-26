// Run with: node --test apps/desktop/src/test-compose-wizard.js
const test = require("node:test");
const assert = require("node:assert/strict");
const CF = require("./compose-wizard.js");

const tool = (t, label, run, art, defArt) => ({ tool: t, label, default_run_command: run, needs_artifact_path: art, default_artifact_path: defArt });
const catalog = {
  runtimes: [
    { runtime: "java", label: "Java", versions: ["17", "21"], build_tools: [tool("maven", "Maven", "java -jar /app/app.jar", true, "target/*.jar"), tool("gradle", "Gradle", "java -jar /app/app.jar", true, "build/libs/*.jar")] },
    { runtime: "node", label: "Node.js", versions: ["18", "20", "22"], build_tools: [tool("npm", "npm", "npm start", false, null), tool("yarn", "Yarn", "yarn start", false, null), tool("pnpm", "pnpm", "pnpm start", false, null)] },
    { runtime: "python", label: "Python", versions: ["3.10", "3.11", "3.12"], build_tools: [tool("pip", "pip", null, false, null)] },
  ],
  databases: [{ kind: "mysql", label: "MySQL", versions: ["8.0", "8.4"], port: 3306 }, { kind: "postgres", label: "PostgreSQL", versions: ["14", "15", "16"], port: 5432 }],
  extras: [{ kind: "redis", label: "Redis", versions: ["6", "7"], port: 6379 }, { kind: "memcached", label: "Memcached", versions: ["1.6"], port: 11211 }],
  db_env_presets: [{ preset: "standard", label: "Standard" }, { preset: "spring", label: "Spring" }, { preset: "none", label: "None" }],
};

test("runtime drives version, tool and defaults", () => {
  const st = CF.newState();
  CF.setRuntime(catalog, st, "java");
  assert.deepEqual([st.runtimeVersion, st.buildTool, st.runCommand, st.artifactPath], ["21", "maven", "java -jar /app/app.jar", "target/*.jar"]);
  CF.setBuildTool(catalog, st, "gradle");
  assert.equal(st.artifactPath, "build/libs/*.jar");
  CF.setRuntime(catalog, st, "node");
  assert.deepEqual([st.runtimeVersion, st.buildTool, st.runCommand, st.artifactPath], ["22", "npm", "npm start", ""]);
  CF.setRuntime(catalog, st, "python");
  assert.equal(st.runCommand, "", "python has no default command");
  CF.setRuntime(catalog, st, "");
  assert.deepEqual([st.runtime, st.runtimeVersion, st.buildTool], ["", "", ""]);
});

test("an edited run command survives version/tool changes; an unedited one is re-defaulted", () => {
  const st = CF.newState();
  CF.setRuntime(catalog, st, "node");
  CF.setBuildTool(catalog, st, "yarn");
  assert.equal(st.runCommand, "yarn start");
  CF.setRunCommand(catalog, st, "node server.js");
  assert.equal(st.runCommandEdited, true);
  st.runtimeVersion = "18";
  CF.setBuildTool(catalog, st, "pnpm");
  assert.equal(st.runCommand, "node server.js");
  // typing back the exact default hands control back to the catalog
  CF.setRunCommand(catalog, st, "pnpm start");
  assert.equal(st.runCommandEdited, false);
  CF.setBuildTool(catalog, st, "npm");
  assert.equal(st.runCommand, "npm start");
});

test("edited artifact path is kept, default one follows the tool", () => {
  const st = CF.newState();
  CF.setRuntime(catalog, st, "java");
  CF.setArtifactPath(catalog, st, "out/app.jar");
  CF.setBuildTool(catalog, st, "gradle");
  assert.equal(st.artifactPath, "out/app.jar");
});

test("port input keeps digits only", () => {
  assert.equal(CF.cleanPortInput("80a8e-0"), "8080");
  assert.equal(CF.cleanPortInput("123456789"), "12345");
});

test("clientErrors: only runtime and port", () => {
  const st = CF.newState();
  assert.deepEqual(CF.clientErrors(st).map((e) => e.field), ["runtime", "port"]);
  CF.setRuntime(catalog, st, "node");
  st.port = "0";
  assert.deepEqual(CF.clientErrors(st).map((e) => e.field), ["port"]);
  st.port = "8080";
  assert.deepEqual(CF.clientErrors(st), []);
  st.port = "70000";
  assert.equal(CF.clientErrors(st).length, 1);
});

test("initServices fills defaults, keeps answers, resets on engine change", () => {
  const st = CF.newState();
  CF.initServices(catalog, st, "mysql");
  assert.deepEqual([st.dbVersion, st.dbEnvPreset], ["8.4", "standard"]);
  assert.deepEqual(st.extras, { redis: { checked: false, version: "7" }, memcached: { checked: false, version: "1.6" } });
  st.dbVersion = "8.0";
  st.extras.redis = { checked: true, version: "6" };
  CF.initServices(catalog, st, "mysql");
  assert.equal(st.dbVersion, "8.0");
  assert.equal(st.extras.redis.version, "6");
  CF.initServices(catalog, st, "postgres");
  assert.equal(st.dbVersion, "16");
  CF.initServices(catalog, st, null);
  assert.equal(st.dbVersion, "");
});

test("buildSpec produces the backend's exact shape", () => {
  const st = CF.newState();
  CF.setRuntime(catalog, st, "java");
  st.port = "8080";
  CF.initServices(catalog, st, "mysql");
  st.extras.redis.checked = true;
  st.env = [{ key: " A ", value: "1" }, { key: "", value: "" }, { key: "", value: "x" }];
  const { spec, map } = CF.buildSpec(catalog, st, { engine: "mysql", database: "shop" });
  assert.deepEqual(spec, {
    runtime: "java", runtime_version: "21", build_tool: "maven", run_command: "java -jar /app/app.jar", port: 8080,
    artifact_path: "target/*.jar",
    database: { engine: "mysql", version: "8.4", database: "shop" },
    db_env_preset: "standard",
    extras: [{ kind: "redis", version: "7" }],
    env: [{ key: "A", value: "1" }, { key: "", value: "x" }],
  });
  assert.deepEqual(map, { extras: ["redis"], env: [0, 2] });
  CF.setRuntime(catalog, st, "node");
  const b = CF.buildSpec(catalog, st, null);
  assert.equal(b.spec.artifact_path, null, "artifact path only for tools that need it");
  assert.equal(b.spec.database, null);
});

test("env rows add/remove", () => {
  const st = CF.newState();
  CF.addEnvRow(st); CF.addEnvRow(st);
  st.env[1].key = "B";
  CF.removeEnvRow(st, 0);
  assert.deepEqual(st.env, [{ key: "B", value: "" }]);
});

test("mapErrors: slots follow the on-screen rows; steps follow the field", () => {
  const st = CF.newState();
  CF.setRuntime(catalog, st, "node");
  CF.initServices(catalog, st, null);
  st.extras.memcached.checked = true;
  st.env = [{ key: "", value: "" }, { key: "", value: "v" }];
  const { map } = CF.buildSpec(catalog, st, null);
  const mapped = CF.mapErrors(
    [{ field: "port", message: "p" }, { field: "extras[0].version", message: "e" }, { field: "env[0].key", message: "k" }, { field: "database.version", message: "d" }, { field: "env", message: "all" }],
    map
  );
  assert.deepEqual(mapped.map((e) => [e.step, e.slot]), [["app", "port"], ["services", "extra.memcached"], ["services", "env.1.key"], ["services", "database.version"], ["services", "env"]]);
  assert.equal(mapped[0].message, "p", "message is passed through verbatim");
  assert.equal(CF.firstErrorStep(mapped), "app");
  assert.equal(CF.firstErrorStep(mapped.slice(1)), "services");
  assert.equal(CF.firstErrorStep([]), null);
});

test("a test run only counts for the answers (and dump) it ran with", () => {
  const st = CF.newState();
  CF.setRuntime(catalog, st, "node");
  st.port = "3000";
  const dump = { schema: "s", file_path: "/x.sql", engine: "mysql" };
  const a = CF.buildSpec(catalog, st, null).spec;
  const key = CF.testKey(a, dump);
  assert.equal(CF.isTestCurrent(key, CF.buildSpec(catalog, st, null).spec, dump), true);
  st.port = "3001";
  assert.equal(CF.isTestCurrent(key, CF.buildSpec(catalog, st, null).spec, dump), false);
  st.port = "3000";
  st.env = [{ key: "A", value: "" }];
  assert.equal(CF.isTestCurrent(key, CF.buildSpec(catalog, st, null).spec, dump), false);
  st.env = [];
  assert.equal(CF.isTestCurrent(key, CF.buildSpec(catalog, st, null).spec, { ...dump, file_path: "/y.sql" }), false);
  assert.equal(CF.isTestCurrent(null, a, dump), false);
});
