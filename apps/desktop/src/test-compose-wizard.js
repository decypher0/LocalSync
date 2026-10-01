// Run with: node --test apps/desktop/src/test-compose-wizard.js
const test = require("node:test");
const assert = require("node:assert/strict");
const CF = require("./compose-wizard.js");

const tool = (t, label, run, art, defArt) => ({ tool: t, label, default_run_command: run, needs_artifact_path: art, default_artifact_path: defArt });
const javaTool = (t, label, defArt, defWar) => ({ ...tool(t, label, "java -jar /app/app.jar", true, defArt), default_war_path: defWar, war_run_command: "localsync-tomcat" });
const catalog = {
  java_packagings: [{ packaging: "jar", label: "Runnable jar" }, { packaging: "war", label: "WAR on Tomcat" }],
  tomcat_versions: [{ version: "9.0", label: "Tomcat 9", java_versions: ["8", "11", "17", "21"] }, { version: "10.1", label: "Tomcat 10.1", java_versions: ["11", "17", "21"] }],
  runtimes: [
    { runtime: "java", label: "Java", versions: ["8", "11", "17", "21"], build_tools: [javaTool("maven", "Maven", "target/*.jar", "target/*.war"), javaTool("gradle", "Gradle", "build/libs/*.jar", "build/libs/*.war")] },
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

test("multi-module Maven: warns until the build-output path points into a module", () => {
  const mods = ["core", "web"];
  const w = CF.mavenModulesWarning(mods, "maven", "target/*.jar");
  assert.match(w, /multi-module Maven project \(modules: core, web\)/);
  assert.match(w, /core\/target\/\*\.jar/);
  assert.ok(CF.mavenModulesWarning(mods, "maven", "") !== null);
  assert.equal(CF.mavenModulesWarning(mods, "maven", "web/target/*.jar"), null);
  assert.equal(CF.mavenModulesWarning(mods, "maven", "./core/target/app.jar"), null);
  assert.ok(CF.mavenModulesWarning(mods, "maven", "corex/target/*.jar") !== null, "a prefix of a name is not the module");
  assert.equal(CF.mavenModulesWarning(mods, "gradle", "target/*.jar"), null, "Maven only");
  assert.equal(CF.mavenModulesWarning([], "maven", "target/*.jar"), null, "single-module project");
  assert.equal(CF.mavenModulesWarning(undefined, "maven", "target/*.jar"), null);
  assert.match(CF.mavenModulesWarning(["a", "b", "c", "d", "e", "f"], "maven", ""), /a, b, c, d, e, \.\.\./);
});

test("WAR packaging: defaults, Tomcat choice per Java version, and the spec", () => {
  const st = CF.newState();
  CF.setRuntime(catalog, st, "java");
  assert.equal(st.packaging, "jar", "a project not detected as a WAR starts as a jar");
  const jarSpec = CF.buildSpec(catalog, { ...st, port: "8080" }, null).spec;
  assert.ok(!("java_packaging" in jarSpec) && !("tomcat_version" in jarSpec), "a jar spec keeps its old shape");

  CF.setPackaging(catalog, st, "war");
  assert.deepEqual([st.artifactPath, st.runCommand, st.tomcatVersion], ["target/*.war", "localsync-tomcat", "10.1"], "newest Tomcat that fits Java 21");
  CF.setBuildTool(catalog, st, "gradle");
  assert.equal(st.artifactPath, "build/libs/*.war");
  CF.setRuntimeVersion(catalog, st, "8");
  assert.equal(st.tomcatVersion, "9.0", "Java 8 has no Tomcat 10.1, so it moves to 9.0");
  assert.deepEqual(CF.tomcatOptions(catalog, "8").map((t) => t.version), ["9.0"]);
  CF.setRuntimeVersion(catalog, st, "17");
  assert.equal(st.tomcatVersion, "9.0", "a still-valid choice is kept");
  st.port = "8080";
  const spec = CF.buildSpec(catalog, st, null).spec;
  assert.equal(spec.java_packaging, "war");
  assert.equal(spec.tomcat_version, "9.0");
  assert.equal(spec.artifact_path, "build/libs/*.war");

  CF.setPackaging(catalog, st, "jar");
  assert.deepEqual([st.artifactPath, st.runCommand], ["build/libs/*.jar", "java -jar /app/app.jar"], "back to jar defaults");
});

test("WAR packaging: a pom detected as a WAR defaults to Tomcat with the suggested version; edits are kept", () => {
  const st = CF.newState();
  st.detectedWar = true;
  st.suggestedTomcat = "9.0";
  CF.setRuntime(catalog, st, "java");
  assert.deepEqual([st.packaging, st.tomcatVersion, st.artifactPath, st.runCommand], ["war", "9.0", "target/*.war", "localsync-tomcat"]);
  CF.setArtifactPath(catalog, st, "web/target/web.war");
  CF.setPackaging(catalog, st, "jar");
  assert.equal(st.artifactPath, "web/target/web.war", "an edited path survives a packaging change");
  CF.setRuntime(catalog, st, "node");
  assert.equal(CF.isWar(st), false, "WAR is Java only");
  assert.ok(!("java_packaging" in CF.buildSpec(catalog, { ...st, port: "3000" }, null).spec));
});

test("multi-module warning example matches the packaging", () => {
  assert.match(CF.mavenModulesWarning(["web"], "maven", "target/*.war", true), /web\/target\/\*\.war/);
  assert.match(CF.mavenModulesWarning(["web"], "maven", "target/*.jar", false), /web\/target\/\*\.jar/);
});
