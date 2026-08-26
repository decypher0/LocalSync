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

## What's not verified here

This was built and tested inside WSL2 Ubuntu, which has no display attached, so `cargo tauri dev`/`build` producing an actual visible window hasn't been confirmed visually — only that it compiles clean and the full command pipeline behind every button works end-to-end (see `apps/desktop/src-tauri/tests/pipeline_test.rs`). TURN relay fallback for strict/symmetric NATs is stubbed but not implemented (STUN-only is enough for same-LAN peers, which is what this demo needs). Windows/macOS support, multi-service stacks beyond app+DB, and anything past a single share→run flow are unbuilt by design — see the MVP scope note above.
