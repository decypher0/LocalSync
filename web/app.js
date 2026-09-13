// LocalSync magic-link fallback page — plain HTML/CSS/JS, no build step, no
// backend, no database. Hosted as a static GitHub Pages site (see
// .github/workflows/pages.yml and README's "Magic link" section).
//
// Split into two halves on purpose: pure functions with no browser globals
// at the top (parseCode/detectOS/pickInstallerAssets/buildSchemeUrl),
// testable directly from plain Node via the CommonJS export at the bottom
// (see test-magic-link-logic.js, run with
// `node --test web/test-magic-link-logic.js`); the actual
// DOM/fetch/timer orchestration below that only ever runs in a real
// browser. This isn't a framework convention — it's the simplest way to
// get real, automated coverage on the logic that's actually worth testing
// without adding a bundler or test-framework dependency this project
// otherwise has no need for.

const GITHUB_OWNER = "decypher0";
const GITHUB_REPO = "LocalSync";
const SCHEME = "localsync";
// Real technique this round's own brief specifies: try the custom-scheme
// handoff, then use a short timeout combined with a visibility/blur
// heuristic to guess whether it worked. 1750ms sits in the specified
// 1.5-2s window - long enough that a slow OS handoff (antivirus scanning
// the protocol handler, a cold-starting app) still has a real chance to
// blur this tab before the timer fires, short enough that a person whose
// browser really has no handler installed isn't left staring at "trying to
// open the app..." for long.
const HANDOFF_TIMEOUT_MS = 1750;

// ---------------------------------------------------------------------
// Pure logic - no `window`/`document`/`navigator`/`fetch` in this section.
// ---------------------------------------------------------------------

/**
 * Reads `?code=...` from a URL's search string (e.g. `location.search`).
 * Returns the decoded code, or null if there isn't one / it's empty.
 */
function parseCode(search) {
  const params = new URLSearchParams(search || "");
  const code = params.get("code");
  return code && code.trim() ? code.trim() : null;
}

/**
 * Best-effort OS detection from a `navigator.userAgent` string. This is
 * genuinely limited, not just as a disclaimer: a user agent can be spoofed
 * or blank, and at least one common real-world case is known to actively
 * mislead this exact kind of check — iPadOS has, since iPadOS 13, defaulted
 * to a desktop Safari user agent that claims "Macintosh", indistinguishable
 * here from a real Mac. This function does not attempt to work around that
 * (doing so needs signals beyond userAgent, like navigator.maxTouchPoints,
 * which the fallback page doesn't otherwise need) - it's a hint to
 * pre-select the right download for the common case, not a guarantee, and
 * the page always shows every OS's option regardless of what this returns.
 */
function detectOS(userAgent) {
  const ua = (userAgent || "").toLowerCase();
  if (ua.includes("windows")) return "windows";
  // Must be checked before "linux" - Android's own user agent string
  // contains "Linux" (it IS a Linux kernel), and there is no Android build
  // of LocalSync for this check to usefully point at.
  if (ua.includes("android")) return "unknown";
  if (ua.includes("linux")) return "linux";
  if (ua.includes("mac os x") || ua.includes("macintosh")) return "macos";
  return "unknown";
}

/**
 * Given GitHub's own `release.assets` array (from
 * GET /repos/{owner}/{repo}/releases/latest) and a detected OS, returns the
 * matching download(s) as `{ label, url }` — an empty array if this
 * release genuinely has nothing for that OS (e.g. a build that failed for
 * one platform; better to show nothing than a wrong link). Linux
 * deliberately returns up to two entries (.deb then AppImage, matching the
 * order the main README's own install instructions already use) since
 * this project ships both and there's no single "the" Linux installer.
 */
function pickInstallerAssets(assets, os) {
  const list = Array.isArray(assets) ? assets : [];
  const byExt = (ext) => list.find((a) => a && typeof a.name === "string" && a.name.toLowerCase().endsWith(ext));

  if (os === "windows") {
    const exe = byExt(".exe");
    return exe ? [{ label: "Download for Windows (.exe)", url: exe.browser_download_url }] : [];
  }
  if (os === "macos") {
    const dmg = byExt(".dmg");
    return dmg ? [{ label: "Download for macOS (.dmg)", url: dmg.browser_download_url }] : [];
  }
  if (os === "linux") {
    const out = [];
    const deb = byExt(".deb");
    const appImage = byExt(".appimage");
    if (deb) out.push({ label: "Download for Linux (.deb)", url: deb.browser_download_url });
    if (appImage) out.push({ label: "Download for Linux (AppImage)", url: appImage.browser_download_url });
    return out;
  }
  return [];
}

/**
 * The scheme URL a successful handoff would open. Exported mainly so the
 * test suite can assert on it without duplicating the string template.
 */
function buildSchemeUrl(code) {
  return `${SCHEME}://receive?code=${encodeURIComponent(code)}`;
}

// ---------------------------------------------------------------------
// Browser orchestration - only runs with a real `document`/`window`.
// ---------------------------------------------------------------------

function init() {
  const code = parseCode(window.location.search);
  const statusEl = document.getElementById("status");
  const codeBoxEl = document.getElementById("code-box");
  const codeTextEl = document.getElementById("code-text");
  const copyBtn = document.getElementById("copy-code-btn");
  const copyStatusEl = document.getElementById("copy-status");
  const downloadsEl = document.getElementById("downloads");
  const uaNoteEl = document.getElementById("ua-note");
  const errorEl = document.getElementById("error");

  if (!code) {
    statusEl.textContent = "This page is meant to be opened from a LocalSync share link — no code was found in the URL.";
    codeBoxEl.hidden = true;
    return;
  }

  codeTextEl.textContent = code;
  copyBtn.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(code);
      copyStatusEl.textContent = "Copied.";
    } catch {
      copyStatusEl.textContent = "Couldn't copy automatically — select the code above and copy it manually.";
    }
  });

  // ---- Step 1: attempt the custom-scheme handoff ----
  let handoffLikelySucceeded = false;
  const markHandoffSucceeded = () => {
    handoffLikelySucceeded = true;
  };
  // Either signal is treated as "the OS switched away to the installed
  // app" - a real visibilitychange to "hidden" (most browsers) or a bare
  // window blur (some configurations fire only this one). Whichever fires
  // first wins; the other is harmless once handoffLikelySucceeded is true.
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") markHandoffSucceeded();
  });
  window.addEventListener("blur", markHandoffSucceeded);

  statusEl.textContent = "Opening LocalSync…";
  window.location.href = buildSchemeUrl(code);

  setTimeout(() => {
    if (handoffLikelySucceeded) return; // assume it worked; nothing more to do here
    statusEl.textContent = "LocalSync doesn't seem to be installed yet — download it below, then paste the code in.";
    renderDownloads();
  }, HANDOFF_TIMEOUT_MS);

  // ---- Step 2: fallback UI - OS-aware install guidance ----
  async function renderDownloads() {
    const os = detectOS(navigator.userAgent);
    uaNoteEl.textContent =
      "Best guess from your browser — it isn't always right (this page always shows every option below regardless).";

    try {
      const res = await fetch(`https://api.github.com/repos/${GITHUB_OWNER}/${GITHUB_REPO}/releases/latest`, {
        headers: { Accept: "application/vnd.github+json" },
      });
      if (!res.ok) {
        throw new Error(`GitHub API returned ${res.status}`);
      }
      const release = await res.json();
      const assets = release.assets || [];

      const allOs = ["windows", "macos", "linux"];
      // Detected OS's own download(s) first and visually first, but every
      // OS's option is always rendered - a wrong guess should never hide
      // the real download for whoever's actually reading this.
      const ordered = [os, ...allOs.filter((o) => o !== os)];
      downloadsEl.innerHTML = "";
      let anyRendered = false;
      for (const candidate of ordered) {
        for (const { label, url } of pickInstallerAssets(assets, candidate)) {
          anyRendered = true;
          const a = document.createElement("a");
          a.className = "download-btn" + (candidate === os ? " download-btn-primary" : "");
          a.href = url;
          a.textContent = label;
          downloadsEl.appendChild(a);
        }
      }
      if (!anyRendered) {
        errorEl.textContent = "The latest release doesn't list a download for any platform yet.";
      }
    } catch (err) {
      errorEl.textContent =
        "Couldn't reach GitHub to look up the latest download (" +
        String(err && err.message ? err.message : err) +
        "). You can browse releases directly instead.";
      const a = document.createElement("a");
      a.className = "download-btn";
      a.href = `https://github.com/${GITHUB_OWNER}/${GITHUB_REPO}/releases/latest`;
      a.textContent = "Open the Releases page";
      downloadsEl.appendChild(a);
    }
  }
}

if (typeof document !== "undefined") {
  init();
}

if (typeof module !== "undefined" && module.exports) {
  module.exports = { parseCode, detectOS, pickInstallerAssets, buildSchemeUrl };
}
