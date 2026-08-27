# LocalSync

Send your local project, as it exists right now, straight to a teammate's machine — no cloud, no tunnel to your dev server. They see a diff, review it, and click Run before anything executes, sandboxed in read-only Podman containers with a small seeded dataset. Nothing about the receiving machine syncs back to you.

This is an MVP validating one path end-to-end: **Linux ⇄ Linux**, a **Spring Boot + MySQL** sample project, one flow (share → review → run). Windows/macOS Podman provisioning, other stacks, and anything past the first successful run (access revocation, multi-tester analytics, etc.) are explicitly out of scope for now.

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
```

The trust chain is enforced at the type level, not just by convention: `ls_containers::run_snapshot` only ever accepts a `VerifiedSnapshot`, which only `ls_security::verify` can construct. Nothing runs until a signature check has passed and — in the app — until a human has seen the diff and clicked Run.

### NAT traversal: STUN first, TURN as a real fallback

Connections try direct P2P (via STUN) first. When that's genuinely unreachable — a strict/symmetric NAT, or a firewall that blocks direct traffic entirely — they fall back to relaying through a TURN server, never the other way around. This isn't assumed: `DataChannelConn::connection_path()` reports whether an established connection actually went `Direct` or `Relayed`, by reading the WebRTC stats for the nominated candidate pair, so the claim is checkable in logs and tests, not just believed.

TURN is off by default (STUN-only, matching earlier behavior) and turns on only when all three of `LOCALSYNC_TURN_URL`, `LOCALSYNC_TURN_USERNAME`, `LOCALSYNC_TURN_CREDENTIAL` are set in the process environment. For interactive use, `scripts/start-turn.sh` runs a local [coturn](https://github.com/coturn/coturn) instance as a Podman container (no `apt install` needed) and prints the env vars to export.

**Proof it actually rescues a blocked connection, not just that it's configured**: `cargo test -p ls-net --test nat_fallback` builds two peer containers on a shared Podman network, each locked down with an in-container `iptables` default-deny (via `--cap-add=NET_ADMIN`, which needs no host root) that blocks everything except the TURN server's control port and the signaling server — a genuine network boundary, not a same-box shortcut. Direct connection is impossible; the test asserts both peers report `PATH=Relayed` and that the transferred bytes match byte-for-byte. See the test file's module doc comment for three narrower designs that were tried and rejected along the way, each ruled out by a real experiment rather than assumed. `cargo test -p ls-net --test turn_configured_still_prefers_direct` is the complementary same-LAN proof: TURN configured *and* reachable still doesn't get used when direct works.

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

## What's not verified here

This was built and tested inside WSL2 Ubuntu, which has no display attached, so `cargo tauri dev`/`build` producing an actual visible window — and the review screen actually being legible and correct to a human — hasn't been confirmed visually by an agent; that check is done by hand, on a real machine, using `scripts/demo-review-screen.sh` above. Everything up to that (the app compiling, launching without a panic, and the exact data the screen would render being correct) is verified by `apps/desktop/src-tauri/tests/pipeline_test.rs` and `preload_test.rs`. Windows/macOS support, multi-service stacks beyond app+DB, and anything past a single share→run flow are unbuilt by design — see the MVP scope note above.
