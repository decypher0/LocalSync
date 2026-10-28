# LocalSync

**Hand a teammate your running local project — code, containers and database — in a few minutes, without deploying anything.**

[![Release](https://img.shields.io/github/v/release/decypher0/LocalSync?include_prereleases&label=release)](https://github.com/decypher0/LocalSync/releases)
[![CI](https://github.com/decypher0/LocalSync/actions/workflows/ci.yml/badge.svg)](https://github.com/decypher0/LocalSync/actions/workflows/ci.yml)
[![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-informational)](#install)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

LocalSync packages a snapshot of your project (the files git tracks, your uncommitted edits, a database dump) and sends it straight to a teammate's computer. There it runs in Podman containers, isolated from their system and from your machine. You keep coding; what they run doesn't change until you push an update.

**[Install](#install) · [Quick start](#quick-start) · [What can I send?](docs/what-can-i-send.md) · [Troubleshooting](#troubleshooting) · [Contributing](CONTRIBUTING.md)**

---

## Why

- **Testing against a real backend usually means waiting** for a deploy or a shared staging server.
- **Tunnels (ngrok and similar) expose your live dev process.** Every edit you make hits whoever is testing.
- **Tunnels don't carry the database.** Many apps can't even boot without their seed data.
- **Not everyone has a server**, and nobody should need a terminal to share a project.

LocalSync sends a point-in-time copy instead: signed, reviewed by the receiver before it runs, and complete with its database.

## Features

- **Snapshots, not a live tunnel.** Later edits on your side never affect what the receiver is running.
- **The receiver decides.** Nothing runs until they've reviewed the incoming changes and clicked **Run**.
- **Sandboxed.** Everything runs in Podman containers with a memory limit; nothing is installed on the receiver's OS.
- **Database included.** A wizard detects connection details from your project's config, lets you browse tables, and dumps MySQL, PostgreSQL or MongoDB.
- **No Docker setup needed.** Projects without a `docker-compose.yml` get one generated (Java, Node.js, Python, Go).
- **Three ways to connect:** local network (zero setup), your own relay server, or your Google Drive.
- **Updates, not resends.** Push an update to one person; they review it, apply it, and can roll back.
- **Magic links**, auto-update, light and dark themes.

## Install

Download the latest build from the **[Releases page](https://github.com/decypher0/LocalSync/releases)**.

| OS | File | Notes |
|---|---|---|
| Windows 10/11 | `.exe` installer | SmartScreen may warn: **More info → Run anyway**. |
| macOS | `.dmg` | Gatekeeper blocks a normal double-click: right-click the app → **Open**. |
| Linux | `.deb` or `.AppImage` | `sudo dpkg -i LocalSync_*.deb`, or `chmod +x LocalSync_*.AppImage && ./LocalSync_*.AppImage` |

> Builds are not code-signed yet, so the warnings above are expected.

### What each side needs

| | Sender | Receiver |
|---|---|---|
| **Git** ([git-scm.com](https://git-scm.com)) | Yes — the project must have at least one commit | No |
| **Podman** | Only for the optional compatibility check | Yes — LocalSync offers to install it on first launch (on Windows this enables WSL and needs one restart) |
| **Google account** | Only for Cloud drop | Only for Cloud drop |

When Windows asks whether LocalSync may use the network, choose **Allow** on private networks.

## Quick start

### Sending

1. Open LocalSync, click **New Session**, give it a name and choose **Send a project**.
2. Pick your project folder (or several). A folder that isn't a git repository gets a **Set up automatically** button.
3. If the project has no `docker-compose.yml`, answer a few questions (language, run command, port, database). LocalSync generates the container setup; nothing is written into your folder.
4. If the project uses a database, choose what to include in the dump.
5. Choose how to connect and click **Send**. Share the code or magic link with your teammate.

After you change your code, open the session and click **Push update**. LocalSync shows what changed since the last send and checks that the update still builds before sending it.

### Receiving

1. Click the magic link, or open LocalSync, click **New Session**, choose **Receive a project** and enter the code.
2. Review the files and settings that came in.
3. Click **Run**. The app and its database start in containers on your machine; the session page shows the address to open.
4. When the sender pushes an update, it lands on the same session for you to review. Click **Apply & restart** (or **Run** if it was stopped), and **Roll back to previous version** if the new one fails.

### Choosing how to connect

| Mode | Use when | Setup |
|---|---|---|
| **Local network** | Both computers are on the same Wi-Fi or LAN | None |
| **Remote relay** | You're on different networks | Someone runs the small relay server — see [self-hosting a relay](docs/self-hosting-relay.md) |
| **Cloud drop** | The computers can't reach each other, or the receiver isn't online yet | Both link a Google account — see [Cloud drop](docs/cloud-drop.md) |

Project data always goes directly between the two computers (or through the sender's own Drive with Cloud drop). A relay only passes the connection handshake.

## What can I send?

Web apps and APIs in **Java (Maven/Gradle), Node.js, Python or Go**, optionally with a **MySQL, PostgreSQL or MongoDB** database. Projects with their own `docker-compose.yml` run as-is. Mobile apps, Rust, PHP, Ruby and .NET projects, and static-only sites aren't supported yet.

Files that look like secrets (`.env*`, `*.pem`, `*.key`, `id_rsa*`, …) are **left out** unless you tick them. New files over 25 MB aren't sent.

Details, including config files that point outside the project: **[docs/what-can-i-send.md](docs/what-can-i-send.md)**.

## Security and privacy

- Every snapshot is signed, and the receiver verifies the signature before anything runs.
- Nothing runs until the receiver reviews it and clicks **Run**.
- Containers run under a fixed sandbox policy with a 1 GB memory limit.
- Sharing is one-way: nothing can flow back to the sender's machine.
- Cloud drop shares the file with one specific Google account, never with "anyone with the link".
- There is no LocalSync server and no account. Nothing is collected about you.

Found a security problem? Please report it privately through [GitHub security advisories](https://github.com/decypher0/LocalSync/security/advisories/new) rather than a public issue.

## Troubleshooting

- **Send or Receive hangs on a local network:** check that the firewall allows LocalSync (Windows: Defender Firewall inbound rules; Linux: `ufw`), and that both computers are on the same network.
- **Every Run fails with a memory-limit error on Windows:** see [docs/troubleshooting.md](docs/troubleshooting.md) (a known WSL 3.0.1 issue).
- **"The other device runs an older LocalSync":** update LocalSync on both computers.
- **Logs** to attach to a bug report:
  - Windows: `%APPDATA%\localsync\logs\`
  - macOS: `~/Library/Application Support/localsync/logs/`
  - Linux: `~/.local/share/localsync/logs/`

  `send.log` covers sending and receiving, `provisioning.log` covers Podman setup.

Still stuck? [Open an issue](https://github.com/decypher0/LocalSync/issues/new) with your OS, the LocalSync version and the relevant log lines.

## Documentation

| Topic | Doc |
|---|---|
| Supported projects, external files, how updates work | [what-can-i-send.md](docs/what-can-i-send.md) |
| Sending through Google Drive | [cloud-drop.md](docs/cloud-drop.md) |
| Running your own relay | [self-hosting-relay.md](docs/self-hosting-relay.md) |
| Known problems | [troubleshooting.md](docs/troubleshooting.md) |
| Building from source, tests, project layout | [CONTRIBUTING.md](CONTRIBUTING.md) |
| Google OAuth client for Cloud drop (self-builds) | [google-drive-setup.md](docs/google-drive-setup.md) |
| Code signing and update signing (maintainers) | [code-signing.md](docs/code-signing.md), [auto-update-signing.md](docs/auto-update-signing.md) |
| Building on macOS | [macos-build.md](docs/macos-build.md) |

## Contributing

Bug reports, fixes and docs improvements are welcome. Start with **[CONTRIBUTING.md](CONTRIBUTING.md)**: it covers setting up the toolchain, running the app and tests, and what a pull request needs.

## License

LocalSync is dual-licensed under either of

- [MIT License](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in LocalSync, as defined in the Apache-2.0 license, is dual-licensed as above, without any additional terms or conditions.
