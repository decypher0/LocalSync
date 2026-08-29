# LocalSync

Send your local project, as it exists right now, straight to a teammate's machine — no cloud, no tunnel to your dev server. They see a diff, review it, and click Run before anything executes, sandboxed in read-only Podman containers with a small seeded dataset. Nothing about the receiving machine syncs back to you.

This is an MVP validating one path end-to-end: **Linux ⇄ Linux**, one flow (share → review → run) — plus Windows/macOS Podman provisioning (round 5) so the receiver side isn't Linux-only, and a second reference stack (round 6, below) proving the pipeline isn't secretly specific to the first one. Code-signing/notarization, an auto-updater, and anything past the first successful run (access revocation, multi-tester analytics, etc.) are explicitly out of scope for now.

## How it fits together

```
crates/
  ls-snapshot/    sender: git-aware diff + bundle + manifest + ed25519 signing
  ls-net/         WebRTC data-channel transport (dumb pipe — moves bytes only)
  ls-security/    receiver: signature verification, diff summary, sandbox policy
  ls-containers/  receiver: podman-compose orchestration, cache-aware DB volume
apps/
  signaling-server/  minimal WebSocket relay for WebRTC offer/answer/ICE only —
                      never sees project code or app traffic
  desktop/            Tauri app wiring the four crates together + the UI
sample-project/       Spring Boot + MySQL reference app used by the demo
sample-project-node/  Express + PostgreSQL reference app — proves the pipeline
                       generalizes beyond the first stack (round 6)
```

The trust chain is enforced at the type level, not just by convention: `ls_containers::run_snapshot` only ever accepts a `VerifiedSnapshot`, which only `ls_security::verify` can construct. Nothing runs until a signature check has passed and — in the app — until a human has seen the diff and clicked Run.

### NAT traversal: STUN first, TURN as a real fallback

Connections try direct P2P (via STUN) first. When that's genuinely unreachable — a strict/symmetric NAT, or a firewall that blocks direct traffic entirely — they fall back to relaying through a TURN server, never the other way around. This isn't assumed: `DataChannelConn::connection_path()` reports whether an established connection actually went `Direct` or `Relayed`, by reading the WebRTC stats for the nominated candidate pair, so the claim is checkable in logs and tests, not just believed.

TURN is off by default (STUN-only, matching earlier behavior) and turns on only when all three of `LOCALSYNC_TURN_URL`, `LOCALSYNC_TURN_USERNAME`, `LOCALSYNC_TURN_CREDENTIAL` are set in the process environment. For interactive use, `scripts/start-turn.sh` runs a local [coturn](https://github.com/coturn/coturn) instance as a Podman container (no `apt install` needed) and prints the env vars to export.

**Proof it actually rescues a blocked connection, not just that it's configured**: `cargo test -p ls-net --test nat_fallback` builds two peer containers on a shared Podman network, each locked down with an in-container `iptables` default-deny (via `--cap-add=NET_ADMIN`, which needs no host root) that blocks everything except the TURN server's control port and the signaling server — a genuine network boundary, not a same-box shortcut. Direct connection is impossible; the test asserts both peers report `PATH=Relayed` and that the transferred bytes match byte-for-byte, at a payload size (20,000 bytes) matching a real snapshot's actual scale, not an artificially small one. See the test file's module doc comment for three narrower designs that were tried and rejected along the way, each ruled out by a real experiment rather than assumed. `cargo test -p ls-net --test turn_configured_still_prefers_direct` is the complementary same-LAN proof: TURN configured *and* reachable still doesn't get used when direct works.

### The transport used to stall above ~4KB — fixed, not worked around

Early NAT-fallback testing only used a 4096-byte payload because larger transfers between two separate OS processes (not same-process tasks) reproducibly stalled forever. Root-caused with `RUST_LOG=webrtc_sctp=trace` against real two-process transfers: `webrtc-sctp`'s outbound queue is unbounded and never applies backpressure on its own, so `send_payload`'s original tight per-chunk loop could hand hundreds of KB to the SCTP layer in microseconds — a burst that provoked real UDP packet loss. Separately, `dc.send()` completing (or even the local `buffered_amount` reaching zero) only proves "handed to the local queue", not "the peer has it" — testing showed even a sender's own last-chunk acknowledgment wasn't reliable proof for tail traffic. Both are fixed in `crates/ls-net/src/lib.rs`: real flow control (`send_payload` waits for `buffered_amount()` to drop below a ceiling before queuing more) plus an explicit completion ack from the receiver that the sender waits to see before returning. This was our bug, not an upstream limitation — no chunking workaround was needed. `crates/ls-net/tests/nat_peer_process_stress.rs` proves it: two real OS processes, 10 consecutive runs at 3,000,000 bytes (well past a real snapshot's ~18.5KB), byte-exact every time.

Caching is intentionally not custom-built: Podman's own image-layer cache handles "second build is fast" for free, and the MySQL data volume is named deterministically from a hash of the seed data, so a second snapshot of an unchanged project reuses it instead of reseeding. First run of the sample project cold: ~5 minutes. Same project resent unchanged: ~20 seconds.

## Prerequisites (Linux, or WSL2 Ubuntu on Windows)

```
sudo apt update
sudo apt install -y build-essential curl wget file pkg-config \
    libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
    librsvg2-dev libssl-dev libxdo-dev \
    podman podman-compose uidmap slirp4netns
```
(this is also `scripts/setup-linux-deps.sh` — run it yourself, it needs your `sudo` password interactively)

Plus a Rust toolchain (`https://rustup.rs`) and Node 18+ for the signaling server.

If you're on Windows: install WSL2 (`wsl --install`), run the above inside your Ubuntu distro, and give WSL2 more than its ~7.6GB default memory cap if you're compiling and running containers at the same time — see `.wslconfig` in your Windows user profile (`memory=12GB` was enough here; the Maven+MySQL+container load under the default cap caused real timing flakiness).

## Running the tests

```
cargo test --workspace                              # all four crates
cargo test -p localsync-desktop -- --nocapture       # full pipeline against sample-project
```

The pipeline test does the real thing: bundles `sample-project/`, signs it, round-trips it through JSON exactly as the wire format does, verifies it, reads the diff, brings it up under Podman, confirms `GET /health` responds, tears it down, then resends the *same unchanged* project and asserts the DB volume was reused (`db_cache_hit == true`).

## Running the demo yourself

Two instances of the app on one machine stand in for sender + receiver — no second physical machine needed.

```
scripts/demo.sh
```

This starts the signaling server and two app instances (each with its own `LOCALSYNC_DATA_DIR` so they don't collide). Then, by hand:

1. **Sender window** → Send tab → project folder: the absolute path to `sample-project/` in this repo → generate a room code → Send.
2. **Receiver window** → Receive tab → paste the same room code → Receive.
3. Review the diff (every file shows as "added" for a first send) → Run.
4. The session panel shows the service ports and whether the DB volume was a cache hit. Hit the app's port: `curl http://localhost:8080/api/notes` should return the seeded notes.
5. Repeat steps 1–4 without changing `sample-project/` — the second run should show a cache hit and come up noticeably faster.

### Jumping straight to the review screen

The diff-review/consent screen normally only appears after a live P2P receive. To check it looks right on a real display without going through Send/Receive first:

```
scripts/demo-review-screen.sh
```

This bundles `sample-project/` into a real signed snapshot, then launches the app with `LOCALSYNC_PRELOAD_SNAPSHOT` pointing at it — on startup the app runs that snapshot through the exact same verify+diff path a real receive does (see `apps/desktop/src-tauri/src/main.rs`) and emits it straight to the review screen. It's the same `renderReview()` code path a real receive uses, not a mockup, and Run still works normally from there if you want to go further than just looking.

## Real two-machine, real-UI evidence

`evidence/round4-two-machine/` has the write-up and screenshots of a full share → receive → review → run pass through the *actual* Tauri UI (not the preload shortcut) between two independent WSL2 instances — click-driven via `xdotool`, screenshotted via `scrot` through WSLg. Read that directory's `README.md` first: it's upfront that these two instances share a kernel/VM (Windows Home has no Hyper-V, so true separate hardware wasn't available), and it documents a WSLg display issue that blocked the final "running" screenshot specifically — the functional result is captured instead as real `curl` output against the containers the UI flow actually started (`GET /health` → `200 {"status":"UP"}`, `GET /api/notes` → the real seeded rows), cross-checked against the same commit hash shown in the review screenshot.

## Windows / macOS Podman provisioning (round 5)

The receiver side no longer requires Linux: `ls_containers::run_snapshot` now calls `ensure_podman_ready()` as its first step (`crates/ls-containers/src/provisioning.rs`), which on Windows/macOS actually provisions Podman — installing it (`winget`/`brew`) and initializing/starting its VM (`podman machine init`/`start`, WSL2-backed on Windows, QEMU/AppleHV-backed on macOS) — rather than just checking it's already there, the way the Linux path (unchanged) still does.

**This environment cannot prove this the way rounds 1–4 proved the Linux path**: no macOS access exists anywhere in this build pipeline at all, and while a real Windows host *was* available for this round (unlike WSL2, which only ever produces Linux binaries), fully proving multi-machine behavior is explicitly out of this round's budget — see the definition-of-done note below and `docs/round5-manual-test-checklist.md`.

- **Windows** (`crates/ls-containers/src/provisioning/windows_impl.rs`): implemented *and verified for real* on a Windows 11 Home machine — every command in it was actually run during development, including a full clean provisioning pass (`cargo test -p ls-containers windows_provisioning_end_to_end -- --ignored`). Real bugs it had to work around, not hypothetical: a freshly-`winget`/`pip`-installed binary isn't visible on `PATH` until refreshed from the registry; `wsl.exe`'s captured output is BOM-prefixed UTF-16LE, not UTF-8; the Windows Podman installer doesn't bundle `podman-compose` (installed via `pip` instead); `podman machine start` can report success while WSL2's own session plumbing is still wedged from a prior unclean shutdown (one bounded `wsl --shutdown` + restart retry fixes it). WSL2/Hyper-V-disabled failure text is matched against Microsoft's documented error code where possible; anything else surfaces its raw error rather than a guessed diagnosis.
- **macOS** (`crates/ls-containers/src/provisioning/macos_impl.rs`): **written but entirely unverified** — the file says so at the top. No Mac hardware, VM, or SDK exists in this environment, so `#[cfg(target_os = "macos")]` code here has never once been compiled, let alone run. Written by close analogy to the Windows path and by cross-referencing Podman's and Homebrew's real documentation. First real test happens on a developer's own Mac.

**Structured logging**: every provisioning step — not just the final result — is appended to `provisioning.log` under the OS's standard app-data directory (`%APPDATA%\localsync\logs\` on Windows, `~/Library/Application Support/localsync/logs/` on macOS, `~/.local/share/localsync/logs/` on Linux), with the OS/version detected, the exact command run, and its real exit code/output. See `docs/round5-manual-test-checklist.md` for exactly what to copy back if something breaks.

**Builds**: an unsigned Windows installer was produced and confirmed on disk this round (`LocalSync_0.1.0_x64-setup.exe`, NSIS, ~7.25MB) — SmartScreen will warn since it's unsigned, that's expected. A `.dmg` cannot be produced here (Tauri's macOS bundling only works when built on macOS); `docs/macos-build.md` has the steps for a developer to produce one on their own Mac. Update path: no auto-updater — Tauri's NSIS bundler already replaces a prior install of the same product in place, so bumping `version` in `apps/desktop/src-tauri/tauri.conf.json` and rebuilding is the whole process.

## A second stack, to prove the pipeline generalizes (round 6)

Everything through round 5 was only ever proven against one stack (Spring Boot + MySQL) — leaving open whether the snapshot/manifest format and container orchestration actually generalize, or just happen to work because of accidental Spring-Boot/MySQL-specific assumptions. `sample-project-node/` (Express + PostgreSQL, same structural shape as `sample-project/` — `app/` build context, `db-seed/` at the root, its own standalone `docker-compose.yml`) answers that.

**`ls-snapshot`'s dependency-hash/diff logic needed zero changes.** Its lockfile detection was already a priority-tier fallback (`pom.xml` → `build.gradle*` → `package-lock.json`) searched across each compose service's own build-context subdirectory, built that way from round 1 — `crates/ls-snapshot/src/hash.rs`'s `falls_back_to_package_lock_json_in_a_service_build_dir` test proves `app/package-lock.json` hashes correctly through the exact same, unmodified function Maven projects use.

**`ls-containers` had one real, confirmed hardcoded assumption**: `mysql_service_names` detected the database service by checking for the literal string `"mysql"` in its image — for `sample-project-node`'s `postgres` service, that matched nothing, which would have silently disabled the deterministic cache-volume mechanism entirely for any non-MySQL database. Fixed by generalizing to `database_service_names`, matching against a small explicit keyword list (`mysql`, `mariadb`, `postgres`, `postgresql`) instead of one hardcoded engine — not a second parallel function bolted on next to the first. Nothing else in `ls-containers` (image canonicalization, build-context rewriting, volume pinning) turned out to be MySQL-specific; those already operated on YAML structure generically.

**Cache-reuse proof, same style as round 1, now for both stacks:**

| Stack | Cold run | Cached run |
|---|---|---|
| Spring Boot + MySQL (`sample-project`, unmodified, re-verified after the fix) | 342.6s | 14.0s |
| Express + PostgreSQL (`sample-project-node`) | 107.3s | 14.9s |

`apps/desktop/src-tauri/tests/pipeline_node_test.rs` is the new Postgres proof, mirroring `pipeline_test.rs`'s exact structure (create → verify → diff → run → stop → resend the same unchanged snapshot → run again, asserting `db_cache_hit == true` the second time). `pipeline_test.rs` itself was not touched and still passes.

`crates/ls-net`, `crates/ls-security`, and TURN/signaling/consent-gate code were untouched this round by design (`git diff --stat -- crates/ls-net crates/ls-security` is empty) — that code was under active manual two-machine testing outside this session while this round's work happened.

## Real bugs found in real testing, fixed (round 7)

A real manual two-machine test (Windows 11 + Kali Linux, real WiFi, real UI) surfaced a genuine bug: the Linux sender stalled indefinitely at the bundling step and never produced a WebRTC offer, so the Windows receiver correctly timed out having received nothing. Root-caused and fixed:

- **`git_bytes`** (every git subprocess `create_snapshot` shells out to, in `crates/ls-snapshot/src/bundle.rs`) left stdin inherited from the parent — a GUI process with no terminal — and had no timeout at all, so any blocking git invocation would hang the whole Send flow forever with zero feedback. Now: stdin explicitly closed, `--no-pager` passed defensively, and a real 30s timeout per call, with stdout/stderr drained concurrently rather than after (draining only after `wait()` returns would deadlock the same way on a large `git archive` output exceeding the OS pipe buffer — the same class of bug, caught before it shipped). The exact hang couldn't be reproduced on our own test hardware, so this is a genuine, verified hardening pass against the most likely cause, not a confirmed-exact repro — the new logging below is what closes that gap if it recurs.
- **`send.log`**, same location convention as round 5's `provisioning.log`, now covers the whole Send flow end to end (bundling/each git command/signing/signaling connect/offer/ICE gathering/transfer) via the standard `log` crate facade — see `docs/round5-manual-test-checklist.md`'s round 7 addendum for exactly what to copy back.
- **STUN was already fine** — confirmed the default (`stun.l.google.com:19302`, unconditional since round 1) is not test-harness-specific and wasn't the cause.
- **A native folder picker** on the Send tab, via Tauri's dialog plugin — no more typing an absolute path from memory.

Proven with a new test exercising the real Tauri command layer (`tauri::test::mock_app()` calling `commands::share_snapshot`/`receive_snapshot` directly — not the lower-level crate functions `pipeline_test.rs` already covered), run against **both** sample projects to confirm the fix isn't coupled to one stack: `apps/desktop/src-tauri/tests/send_flow_test.rs`. `pipeline_test.rs` and `pipeline_node_test.rs` were not touched and both still pass — independently re-verified, cache-reuse timings intact (MySQL: 313.1s → 18.3s; Postgres: 88.7s → 17.5s).

## Embedded signaling, a real Linux picker fix, gitignore-aware bundling, firewall automation (round 8)

Driven by friction from the first genuinely successful two-machine test: it worked, but only after manually starting `apps/signaling-server` on both ends and typing its LAN address by hand, the Linux Browse button did nothing, and neither firewall was configured for anyone reading this cold.

- **No more separate signaling process.** `crates/ls-net/src/discovery.rs` adds `host_ephemeral_relay()` (binds `0.0.0.0:0`, replicates `apps/signaling-server/index.js`'s pairing/relay/queue/disconnect protocol byte-for-byte so `signaling.rs`'s existing client works against it unmodified), `detect_lan_ip()` (the classic UDP-`connect()`-to-a-public-address trick — a routing-table lookup, no real traffic sent), and `encode_room_code`/`decode_room_code` (`IP + port + room id` packed into a fixed-width 14-character base62 string). Two new, purely-additive Tauri commands (`start_send_session`, `decode_room_code`) derive the values the *existing, unmodified* `share_snapshot`/`receive_snapshot` commands already take — every instance is symmetric now, any app can send or receive with zero setup difference. The old "Signaling server URL" field moved to Settings as an optional override, not the normal path. Proven same-box end-to-end, including a real payload transfer through nothing but a room code: `crates/ls-net/tests/embedded_relay_test.rs`.
- **The Linux "Browse…" picker's actual root cause**, found by reading the exact pinned `rfd`/`tauri-plugin-dialog` source rather than guessing: the default `gtk3` backend drives the file picker through a second, privately-spawned GTK thread (`GtkGlobalThread`, calling `gtk_init_check`/`gtk_main_iteration` on its own thread) independent of the GTK main loop Tauri's own webview already owns on the real main thread — the same class of bug Tauri has open issues about (`tauri-apps/tauri#11312`, "GTK may only be used from the main thread"). Fixed by switching `apps/desktop/src-tauri/Cargo.toml` to the `xdg-portal` feature instead of `gtk3`: the picker now goes out-of-process, over D-Bus, to the `xdg-desktop-portal` service — there's no second in-process GTK loop for this bug to occur in. Confirmed via the dependency graph (`ashpd`/`zbus` now compiled in place of the private-GTK-thread code path) and a clean full-workspace rebuild; a live interactive click-through confirmation was not obtainable in this round's sandbox (no real desktop environment, and the same WSLg display issues `evidence/round4-two-machine/README.md` already documents) — see `docs/round5-manual-test-checklist.md`'s round 8 addendum for what to confirm on real hardware.
- **Gitignore-aware bundling.** `crates/ls-snapshot` already relied on `git`'s own tracking to skip properly-ignored files by construction; the gap was a project that *accidentally committed* `node_modules`/`.git`/`target`/`build`/etc. without a `.gitignore` at all. A small denylist (`NOISE_DIR_NAMES` in `bundle.rs`) now filters those out at every point they could otherwise leak into a snapshot — the git-archive tar stream, the diff-stat totals, and the review diff itself. Proven against a real accidental-commit scenario: a genuine `npm install` inside a fresh copy of `sample-project-node/`, force-committed with no `.gitignore`, confirmed to produce a snapshot with zero `node_modules` entries anywhere.
- **Windows Firewall rule is automatic now.** An NSIS install hook (`apps/desktop/src-tauri/windows/hooks.nsh`, wired in via `tauri.conf.json`'s `bundle.windows.nsis.installerHooks`) runs `netsh advfirewall firewall add rule` for the installed binary at install time, using the elevation the installer already requires — and removes the rule again on uninstall. Fire-and-forget: a `netsh` failure doesn't abort the install, it just means the original manual-workaround friction returns.
- **Linux `ufw` gets a real warning instead of a silent hang.** `main.rs` checks `ufw status` at startup (`.ok()?` if `ufw` isn't installed at all — not an error) and, if active, both logs and shows a startup banner explaining the dynamic port may need to be allowed through.

All of rounds 1–7's existing tests were re-run unmodified and still pass: `pipeline_test.rs`, `pipeline_node_test.rs`, `preload_test.rs`, `send_flow_test.rs` (both stacks), the full `ls-net` suite (`transfer`, `nat_fallback`, `nat_peer_process_stress`, `turn_configured_still_prefers_direct`, plus the new `embedded_relay_test`), `ls-snapshot` (12/12, including the new accidental-commit case), `ls-security`, and `ls-containers`.

## What's not verified here

Multi-service stacks beyond app+DB and anything past a single share→run flow are unbuilt by design — see the MVP scope note above. Windows/macOS provisioning code exists now (above) but real-machine proof beyond this round's single verified Windows pass is deliberately deferred to `docs/round5-manual-test-checklist.md`, run by a human on real hardware — not simulated here, by design, per that round's explicit budget rule. A minor, separately-tracked finding: `crates/ls-net`'s 30-second `CONNECT_TIMEOUT` was seen to trip once in 7 back-to-back `nat_fallback` test runs under heavy host contention (multiple container lifecycles in quick succession) — not the transport bug that round fixed (it failed before any transfer began), not reproduced outside of rapid repeated automated testing, and not yet addressed. Round 8's Linux file-picker fix (above) is verified by source inspection and a clean build, not by a live click — no real desktop environment was available this round to confirm a picker dialog actually appears; that's the one round-8 item still deferred to `docs/round5-manual-test-checklist.md`. The Windows NSIS firewall hook is verified by config/macro-name correctness against Tauri's documented schema, not by installing the built package and inspecting Windows Defender Firewall's rule list — also deferred to that checklist.
