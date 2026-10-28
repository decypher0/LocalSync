# Contributing to LocalSync

Thanks for helping. This guide covers setting up a development machine, how the code is laid out, how to run the app and its tests, and what a pull request needs.

- **Found a bug?** [Open an issue](https://github.com/decypher0/LocalSync/issues/new) with your OS, the LocalSync version, what you did, what happened, and the relevant lines from `send.log` / `provisioning.log` (locations are in the [README](README.md#troubleshooting)).
- **Security problem?** Don't open a public issue. Report it privately through [GitHub security advisories](https://github.com/decypher0/LocalSync/security/advisories/new).
- **Bigger change or new feature?** Open an issue first so we can agree on the approach before you spend time on it.

## Development setup

### Prerequisites

| Tool | Version | Used for |
|---|---|---|
| [Rust](https://rustup.rs) | stable | Everything in `crates/` and the app backend |
| [Node.js](https://nodejs.org) | 20 or later | Tauri CLI, frontend tests, signaling server |
| [Git](https://git-scm.com) | any recent | Snapshots are built from git |
| [Podman](https://podman.io) + `podman-compose` | recent | Running snapshots; the container tests skip themselves without it |

Platform build tools:

- **Windows:** Visual Studio Build Tools with the "Desktop development with C++" workload (MSVC). WebView2 ships with Windows 10/11.
- **macOS:** Xcode Command Line Tools (`xcode-select --install`). See [docs/macos-build.md](docs/macos-build.md).
- **Linux:** the GTK/WebKit development packages — run `bash scripts/setup-linux-deps.sh` (Debian/Ubuntu, needs sudo).

### Clone and build

```bash
git clone https://github.com/decypher0/LocalSync.git
cd LocalSync
cargo build --workspace
```

### Run the app

```bash
cargo run -p localsync-desktop
```

The frontend is plain HTML/CSS/JS in `apps/desktop/src/` with no bundler; it's embedded into the binary, so rerun `cargo run` after editing it. `npm run tauri dev` from `apps/desktop/` works too and rebuilds when files change.

To try a send and a receive on one machine, run two instances with separate data directories:

```bash
LOCALSYNC_DATA_DIR=/tmp/ls-sender cargo run -p localsync-desktop
LOCALSYNC_DATA_DIR=/tmp/ls-receiver cargo run -p localsync-desktop
```

Then send `sample-project/` (Spring Boot + MySQL) or `sample-project-node/` (Express + PostgreSQL) from one window to the other over **Local network**.

### Build installers

```bash
cd apps/desktop
npm install
npm run tauri build
```

Output lands in `target/release/bundle/`. Release builds for all three platforms are produced by `.github/workflows/release.yml`; signing is optional and covered in [docs/code-signing.md](docs/code-signing.md) and [docs/auto-update-signing.md](docs/auto-update-signing.md).

## Project layout

```
apps/
  desktop/
    src/               Frontend: index.html, app.js, views.js, ... and test-*.js
    src-tauri/         Tauri backend: commands, session models, wizards
      tests/           End-to-end tests of the app's flows
  signaling-server/    Optional self-hosted relay (Node, WebSocket)
crates/
  ls-snapshot/         Builds, bundles and signs a snapshot from a git project
  ls-net/              WebRTC transfer, embedded relay, mDNS discovery
  ls-security/         Signature checks, known peers, container policy, diffs
  ls-containers/       Podman setup and repair, running snapshots, health checks
  ls-composegen/       Generates docker-compose.yml + Dockerfile for plain projects
  ls-dbsource/         Detects database settings and exports MySQL/Postgres/Mongo dumps
  ls-clouddrop/        Google Drive transport and OAuth
web/                   Static magic-link landing page (GitHub Pages)
docs/                  User and maintainer docs
sample-project*/       Small apps for trying the full flow
scripts/               Linux setup and demo helpers
```

Each crate's `lib.rs` starts with a doc comment explaining what it owns and why — read it before changing the crate.

## Tests

Run what CI runs before opening a pull request:

```bash
# The network tests start the signaling server and the NAT example
(cd apps/signaling-server && npm ci)
cargo build --example nat_peer -p ls-net

cargo test --workspace
cargo clippy --workspace --all-targets

# Frontend tests (plain Node, no dependencies)
cd apps/desktop/src
for f in test-*.js; do node --test "$f"; done
```

Notes:

- Tests that need Podman, `podman-compose` or a real database **skip themselves with a printed reason** when those aren't installed. Run them locally when you change `ls-containers`, `ls-dbsource` or the run/update flows.
- Live database tests in `crates/ls-dbsource/tests/` start disposable databases in Podman.
- Some behavior (firewalls, OS file pickers, magic links, two real machines) can't be tested automatically. [docs/round5-manual-test-checklist.md](docs/round5-manual-test-checklist.md) is the historical log of those manual checks.

### Useful environment variables

| Variable | Effect |
|---|---|
| `LOCALSYNC_DATA_DIR` | Where the app keeps its sessions, keys and logs (use one per instance) |
| `LOCALSYNC_TURN_URL`, `LOCALSYNC_TURN_USERNAME`, `LOCALSYNC_TURN_CREDENTIAL` | A TURN server for connections that can't go direct |
| `LOCALSYNC_HEALTH_TIMEOUT_SECS`, `LOCALSYNC_DB_START_TIMEOUT_SECS`, `LOCALSYNC_TESTRUN_TIMEOUT_SECS` | Override container start and health-check timeouts |
| `GOOGLE_OAUTH_CLIENT_ID`, `GOOGLE_OAUTH_CLIENT_SECRET` (build time) | Enable Cloud drop in your build — see [docs/google-drive-setup.md](docs/google-drive-setup.md) |

## Pull requests

1. Branch from `main` (`feature/<topic>` or `fix/<topic>`).
2. Keep a pull request to one change. Unrelated cleanups go in their own PR.
3. Add or update a test for the behavior you changed. Bug fixes should come with a test that fails without the fix.
4. Make sure `cargo test`, `cargo clippy` and the frontend tests pass. CI runs them on every pull request.
5. Update the docs when behavior a user sees changes (`README.md`, `docs/what-can-i-send.md`, ...).
6. In the description, say what changed, why, and how you tested it — including any manual checks on a real machine.

### Commit messages

Write the subject as a sentence describing the behavior after the change, the way the history already reads:

```
New files over 25 MB are listed as too large and not sent
```

rather than `fix bug` or `update bundle.rs`. Add a body when the why isn't obvious.

### Code style

- Rust: no new `cargo clippy` warnings. The codebase isn't `cargo fmt`-formatted, so don't reformat whole files; match the surrounding code.
- JavaScript: plain browser JS, no build step and no new npm dependencies in the frontend. Match the style of the file you're editing.
- Prefer clear, actionable error messages that tell the user what to do next — LocalSync's users often aren't the developers of the project they're running.
- Don't add a dependency for something a few lines of code can do.

## License

By contributing, you agree that your contributions are dual-licensed under the [MIT License](LICENSE-MIT) and the [Apache License 2.0](LICENSE-APACHE), as described in the [README](README.md#license), without any additional terms or conditions.
