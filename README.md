# LocalSync

**Share a running local project — code, containers, and database — with a teammate in minutes, without deploying anywhere.**

[![Release](https://img.shields.io/github/v/release/decypher0/LocalSync?include_prereleases&label=release)](https://github.com/decypher0/LocalSync/releases)
[![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-informational)](#-download--install)
[![License](https://img.shields.io/badge/license-TBD-lightgrey)](#-license)
[![Build](https://github.com/decypher0/LocalSync/actions/workflows/release.yml/badge.svg)](https://github.com/decypher0/LocalSync/actions)

LocalSync lets a developer package their current local project — source, containers, and database state — and hand it directly to a teammate's machine, where it runs in an isolated, read-only sandbox. No shared staging server, no tunnel to your live dev process, no waiting for a deploy pipeline.

**[Download](#-download--install) · [How it works](#-how-it-works) · [Features](#-features) · [Getting started](#-getting-started) · [Build from source](#-build-from-source) · [Contributing](#-contributing)**

---

## 🧩 The problem

- **Testing a UI against a real backend API usually means waiting** — for a deploy, a shared staging environment, or someone else's schedule.
- **Tunneling tools (ngrok and similar) share your live dev process directly** — the moment you touch your code, whoever's testing feels it too, and there's no isolation between "what I'm actively changing" and "what I'm showing someone."
- **Tunnels alone don't solve the database problem.** A real application — especially a multi-tenant one, or one spanning several services and databases — often needs its actual seed data (default tenants, roles, parent records) just to boot. No developer can safely guess which rows are "needed" and which aren't, and a project's data can genuinely run into gigabytes.
- **Not everyone has a server to route through**, and self-hosting a relay isn't something every small team or solo developer wants to maintain.
- **Setting all of this up shouldn't require a terminal.** A GUI-first workflow, with sane defaults, matters as much as the underlying transport.

## ✅ Features

- **Snapshot-based sharing, not a live tunnel.** Your project is diffed (`.gitignore`-aware), signed, and packaged as a point-in-time snapshot — editing your code afterward never affects what the receiver is running.
- **A real consent gate, enforced at the type level.** Nothing runs on the receiver's machine until they've reviewed the incoming diff and explicitly clicked Run — this isn't just a UI convention, it's structurally impossible to bypass in the code.
- **Read-only, sandboxed execution via containers**, using Podman — no dependency ever gets installed to the receiver's actual OS.
- **Cross-platform**, natively — Windows, macOS, and Linux, from one codebase.
- **Three ways to connect**, chosen per send:
  - **Local network** — zero setup, for teammates on the same Wi-Fi/LAN.
  - **Remote relay** — point at any self-hosted relay for cross-network use without a central server this project runs for you.
  - **Cloud drop** — share via your own Google Drive; the sender stays in full control of who's been granted access, using Drive's own real sharing permissions, not a public link.
- **A guided database wizard**, not a black box. Auto-detects connection details from your project's own config (e.g. Spring Boot's `application.yml`), lets you browse the real live schema before deciding what to include, supports MySQL/PostgreSQL/MongoDB, and handles projects with multiple databases across multiple folders.
- **Fast on repeat sends.** Dependency layers, container images, and database dumps are all cached — a second send of a mostly-unchanged project is dramatically faster than the first.
- **Multi-person sessions.** A sender can have several people connected at once, push an update to one specific person, and receivers can request a fresh copy without waiting to be pushed to.
- **Magic links.** Share a real `https://` link — it opens straight into the app if it's installed, or walks a new teammate through installing it if it's not.
- **A real update path and modern UI** — auto-update, light/dark themes, and a guided step-by-step flow for sharing and receiving.

## 🖥️ How it works

1. **Send** — pick a project folder (or several), choose how to connect, and optionally walk through the database wizard if your project needs one.
2. LocalSync diffs, signs, and packages a snapshot — your live code is never touched or exposed directly.
3. **Receive** — the other person gets a room code or magic link, reviews exactly what's changed, and clicks Run.
4. Containers spin up locally on their machine — app and database included — read-only and disposable.

## 📥 Download & Install

Grab the latest build from the **[Releases page](https://github.com/decypher0/LocalSync/releases)**.

> Builds are currently unsigned while the project doesn't yet have a paid code-signing certificate. Your OS will warn you the first time — this is expected, not a sign of a compromised download.

**Windows:** run the `.exe` installer. If SmartScreen shows *"Windows protected your PC,"* click **More info → Run anyway**.

**macOS:** open the `.dmg`. Since it's unsigned, Gatekeeper will block a normal double-click — right-click the app → **Open**, and confirm in the dialog that appears.

**Linux:** install the `.deb` (`sudo dpkg -i LocalSync_*.deb`) or run the AppImage directly (`chmod +x LocalSync_*.AppImage && ./LocalSync_*.AppImage`).

## 🚀 Getting started

1. Open LocalSync on both machines.
2. On the sender's side, click **Send**, choose a connectivity mode, and pick a project folder.
3. If your project uses a database, the wizard will walk you through it — auto-detecting connection details where it can, and letting you browse real tables before deciding what to include.
4. Share the generated code or magic link with your teammate.
5. On the receiver's side, click **Receive**, enter the code (or just click the link), review the diff, and click **Run**.

## 🛠️ Build from source

**Prerequisites:**
- [Rust](https://rustup.rs) (stable toolchain)
- [Node.js](https://nodejs.org) (current LTS)
- [Podman](https://podman.io)
- Platform build tools: Visual Studio Build Tools (Windows, MSVC toolchain), Xcode Command Line Tools (macOS), or your distro's GTK/WebKit dev packages (Linux — see `scripts/setup-linux-deps.sh`)

```bash
git clone https://github.com/decypher0/LocalSync.git
cd LocalSync/apps/desktop
npm install
npm run tauri build
```

Run the test suite:

```bash
cargo test --workspace
```

Two sample projects (`sample-project/`, a Spring Boot + MySQL app, and `sample-project-node/`, an Express + PostgreSQL app) are included for trying out the full send/receive flow locally without needing a real project on hand.

### Optional: enabling Cloud drop, code-signing, and auto-update

- Cloud drop (Google Drive) mode needs a Google OAuth Client ID you register yourself — see [`docs/google-drive-setup.md`](docs/google-drive-setup.md).
- Producing signed, warning-free installers needs a real code-signing certificate (Windows) and Apple Developer Program enrollment (macOS) — see [`docs/code-signing.md`](docs/code-signing.md).
- Shipping update-capable installers (so "Check for updates" can find something) needs a locally-generated signing key added as a repository secret — see [`docs/auto-update-signing.md`](docs/auto-update-signing.md).

None of these are required to build or use the app — all three are optional, and everything works without them beyond the warnings/limitations described in each doc.

## 🔒 Security model

- Every snapshot is signed; the receiver's app verifies the signature before anything can run.
- Execution is read-only and sandboxed per snapshot — nothing installs to the receiver's host system.
- The trust model is strictly one-way: a sender shares with a receiver, never the other way around. Pull requests and multi-receiver sessions are signaling only — no code or file changes can flow back to the sender.
- Cloud drop mode uses Google Drive's own real, per-account sharing permissions — not a public "anyone with the link" file — so the sender always knows exactly who has access.

## 🤝 Contributing

Issues and pull requests are welcome. If you're picking this up for the first time, `docs/` has setup guides for the pieces that need external credentials (Google Drive, code-signing, auto-update signing), and `docs/round5-manual-test-checklist.md` has the accumulated manual-testing notes for the parts that need a real screen to verify.

## 📄 License

*A license has not yet been chosen for this project.* If you're the maintainer, add a `LICENSE` file and update this section before treating this repo as fully open source — without one, default copyright law applies and others technically don't have permission to use, modify, or redistribute this code, however open the intent.
