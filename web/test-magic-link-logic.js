// Real, automated tests for the fallback page's pure logic - run with:
//   node --test web/test-magic-link-logic.js
// No test framework dependency: node:test + node:assert are built into
// Node itself (stable since Node 20), matching this whole page's own "no
// build step" constraint. Only the pure functions exported from app.js are
// covered here (see that file's own comment on why it's split this way) -
// the actual scheme-handoff timing/visibility heuristic and the real fetch
// against GitHub's API need a real browser and real network to observe,
// per this round's own hard budget rule; that's deferred to a human's
// real-hardware click-through, documented in
// docs/round5-manual-test-checklist.md's round 24 addendum.
const test = require("node:test");
const assert = require("node:assert/strict");
const { parseCode, detectOS, pickInstallerAssets, buildSchemeUrl } = require("./app.js");

test("parseCode", async (t) => {
  await t.test("reads a real code from the query string", () => {
    assert.equal(parseCode("?code=ABCD1234"), "ABCD1234");
  });
  await t.test("trims surrounding whitespace", () => {
    assert.equal(parseCode("?code=%20ABCD1234%20"), "ABCD1234");
  });
  await t.test("returns null when there's no code param at all", () => {
    assert.equal(parseCode("?other=thing"), null);
  });
  await t.test("returns null for an empty code value", () => {
    assert.equal(parseCode("?code="), null);
  });
  await t.test("returns null for an empty/missing search string", () => {
    assert.equal(parseCode(""), null);
    assert.equal(parseCode(undefined), null);
  });
  await t.test("ignores unrelated params alongside a real code", () => {
    assert.equal(parseCode("?utm_source=share&code=XYZ999"), "XYZ999");
  });
});

test("detectOS", async (t) => {
  const WINDOWS_UA =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
  const MACOS_UA =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.1 Safari/605.1.15";
  const LINUX_UA = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
  const ANDROID_UA =
    "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36";

  await t.test("recognizes Windows", () => {
    assert.equal(detectOS(WINDOWS_UA), "windows");
  });
  await t.test("recognizes macOS", () => {
    assert.equal(detectOS(MACOS_UA), "macos");
  });
  await t.test("recognizes Linux", () => {
    assert.equal(detectOS(LINUX_UA), "linux");
  });
  await t.test("does not misclassify Android as Linux (real overlap in the raw UA string)", () => {
    assert.equal(detectOS(ANDROID_UA), "unknown");
  });
  await t.test("falls back to unknown for an empty/missing user agent", () => {
    assert.equal(detectOS(""), "unknown");
    assert.equal(detectOS(undefined), "unknown");
  });
  await t.test("falls back to unknown for a UA matching none of the three", () => {
    assert.equal(detectOS("SomeExoticBrowser/1.0"), "unknown");
  });
});

test("pickInstallerAssets", async (t) => {
  const assets = [
    { name: "LocalSync_0.3.0_x64-setup.exe", browser_download_url: "https://example.com/win.exe" },
    { name: "LocalSync_0.3.0_x64-setup.exe.sig", browser_download_url: "https://example.com/win.exe.sig" },
    { name: "LocalSync_0.3.0_aarch64.dmg", browser_download_url: "https://example.com/mac.dmg" },
    { name: "LocalSync_0.3.0_amd64.deb", browser_download_url: "https://example.com/linux.deb" },
    { name: "LocalSync_0.3.0_amd64.AppImage", browser_download_url: "https://example.com/linux.AppImage" },
    { name: "latest.json", browser_download_url: "https://example.com/latest.json" },
  ];

  await t.test("finds the Windows .exe, ignoring its own .sig", () => {
    const picked = pickInstallerAssets(assets, "windows");
    assert.equal(picked.length, 1);
    assert.equal(picked[0].url, "https://example.com/win.exe");
  });
  await t.test("finds the macOS .dmg", () => {
    const picked = pickInstallerAssets(assets, "macos");
    assert.equal(picked.length, 1);
    assert.equal(picked[0].url, "https://example.com/mac.dmg");
  });
  await t.test("finds both real Linux options, .deb before AppImage", () => {
    const picked = pickInstallerAssets(assets, "linux");
    assert.equal(picked.length, 2);
    assert.equal(picked[0].url, "https://example.com/linux.deb");
    assert.equal(picked[1].url, "https://example.com/linux.AppImage");
  });
  await t.test("returns nothing for an OS this release has no build for", () => {
    const winOnly = [{ name: "app-setup.exe", browser_download_url: "https://example.com/win.exe" }];
    assert.deepEqual(pickInstallerAssets(winOnly, "macos"), []);
    assert.deepEqual(pickInstallerAssets(winOnly, "linux"), []);
  });
  await t.test("returns nothing for 'unknown', on purpose - no OS to pick for", () => {
    assert.deepEqual(pickInstallerAssets(assets, "unknown"), []);
  });
  await t.test("handles a missing/malformed assets array without throwing", () => {
    assert.deepEqual(pickInstallerAssets(undefined, "windows"), []);
    assert.deepEqual(pickInstallerAssets(null, "linux"), []);
    assert.deepEqual(pickInstallerAssets([{ name: null }], "windows"), []);
  });
});

test("buildSchemeUrl", async (t) => {
  await t.test("builds the expected localsync:// URL", () => {
    assert.equal(buildSchemeUrl("ABCD1234"), "localsync://receive?code=ABCD1234");
  });
  await t.test("percent-encodes characters that would otherwise break the query string", () => {
    assert.equal(buildSchemeUrl("a b&c"), "localsync://receive?code=a%20b%26c");
  });
});
