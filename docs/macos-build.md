# Building the macOS `.dmg`

Tauri's macOS bundler only runs on macOS — there's no cross-compile path from
Windows/Linux to a signed `.app`/`.dmg`, and this repo's CI/dev environment
has no macOS machine to build or test on. So the `.dmg` has to be produced by
a developer on their own Mac. Steps:

1. **Install Rust**: <https://rustup.rs> (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`).
2. **Install Xcode Command Line Tools** (provides the macOS SDK/linker Tauri
   needs): `xcode-select --install`.
3. **Install Node.js** (any current LTS) if not already present — needed for
   the `@tauri-apps/cli` dev dependency already in `apps/desktop/package.json`.
4. From the repo root:
   ```sh
   cd apps/desktop
   npm install
   npm run tauri build -- --bundles dmg
   ```
   (`npm run tauri` is the `tauri` script already defined in
   `apps/desktop/package.json`, which invokes the JS `@tauri-apps/cli`; `--`
   passes `--bundles dmg` through to it. If you'd rather use the Rust-native
   CLI instead, `cargo install tauri-cli --version "^2"` then
   `cargo tauri build --bundles dmg` from `apps/desktop/src-tauri` does the
   same thing.)

## Where the `.dmg` lands

`target/release/bundle/dmg/LocalSync_0.1.0_<arch>.dmg`, at the **workspace
root** — not nested under `apps/desktop/src-tauri/`, even though that's where
you ran the build from. `apps/desktop/src-tauri` is a member of this repo's
Cargo workspace (see the root `Cargo.toml`), and Cargo always places every
member's build output under the workspace root's single shared `target/`
directory, never under the member's own path (confirmed the hard way: this
directory doesn't have its own `target/` at all — round 15's release
pipeline first got this wrong too, searching
`apps/desktop/src-tauri/target/...` and finding nothing). `<arch>` is
`aarch64` on Apple Silicon or `x64` on Intel (matching the
`productName`/`version` in `apps/desktop/src-tauri/tauri.conf.json`, which is
`LocalSync` / `0.1.0` as of this writing). Building on Apple Silicon produces
an Apple Silicon-only `.dmg` unless you pass
`--target universal-apple-darwin` (requires both Rust targets installed via
`rustup target add aarch64-apple-darwin x86_64-apple-darwin`) for a universal
binary that runs on both.

## Unsigned build note

Without an Apple Developer ID and `codesign`/notarization set up, the
resulting app is unsigned — macOS Gatekeeper will refuse to open it normally
on another Mac (right-click → Open bypasses this for local testing on the
machine that built it, or the machine of anyone willing to click through the
warning). Setting up signing/notarization is out of scope here; see Tauri's
own macOS bundling docs if that's needed later.
