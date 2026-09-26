# Round 5 manual test checklist (Windows / macOS)

This is a test **you** run on your own hardware — nothing in this repo's automated
tests can prove Windows/macOS Podman provisioning works, because the build/CI
environment that produced this code has neither a real Windows GUI session nor any
macOS access at all. Everything up to "does it compile and run once" was checked;
whether it actually provisions Podman cleanly on your machine is what this checklist
is for.

## 1. Install the build

- **Windows**: run the `.exe` (NSIS installer) from `apps/desktop/src-tauri/target/release/bundle/nsis/` (or wherever your build produced it — see the build agent's report / repo state for the exact path). It's unsigned, so Windows SmartScreen will warn on first run ("Windows protected your PC") — click **More info → Run anyway**.
- **macOS**: run the `.dmg` you built yourself following `docs/macos-build.md` (this repo's build pipeline cannot produce one). It's unsigned/un-notarized, so Gatekeeper will block it on first open — right-click the app → **Open**, confirm in the dialog that appears (or `xattr -d com.apple.quarantine /Applications/LocalSync.app` if the right-click option doesn't appear).

## 2. Launch — does it open without crashing?

Just open the app. You should see the LocalSync window with **Send** / **Receive**
tabs. If it doesn't open, or opens and immediately closes, that's a launch-level
failure — there's no provisioning log for this yet (provisioning only starts once you
try to *run* something), so capture whatever your OS shows (Windows: Event Viewer →
Windows Logs → Application, filter for `localsync-desktop`; macOS: `Console.app`,
search for `localsync-desktop`) and report that.

## 3. Trigger the run flow and let provisioning kick in

Easiest path: get a snapshot of `sample-project/` onto this machine and load it via
the review-screen shortcut rather than setting up a second sender machine —

```
LOCALSYNC_PRELOAD_SNAPSHOT=<path to a snapshot.json> ./localsync-desktop
```

(generate that file elsewhere with `cargo run --example make_snapshot -p ls-snapshot -- <sample-project-copy> snapshot.json` — see the main README's "Jumping straight to the review screen" section — and copy just the `.json` file over; or do a real Send/Receive between this machine and a Linux sender if you'd rather test the full P2P path too, both are valid).

Review the diff, click **Run**. This is the moment Podman provisioning actually
happens — the button will sit on "Starting containers…" for longer than usual on a
machine that's never had Podman before, since it's now installing Podman and/or
initializing its VM in the background, not just starting an already-running one.

## 4. What to note

- **Provisions cleanly**: the button eventually clears and you land on the running-session screen with service ports listed. Good — go to step 6.
- **Clear error shown**: the app surfaces an error message (in the UI's error text under the Run button) instead of hanging. Note the exact text.
- **Hangs indefinitely**: no error, no progress, "Starting containers…" never resolves. This is the worst case to report — note how long you waited before giving up.

In all three cases, **check the log file next** (step 5) before reporting — the UI
message alone often won't have the detail needed to fix it.

## 5. The log file — what to copy back

Location:
- **Windows**: `%APPDATA%\localsync\logs\provisioning.log`
- **macOS**: `~/Library/Application Support/localsync/logs/provisioning.log`

Every provisioning attempt appends to this file — it's not cleared between runs, so
if you're retrying, the file will have multiple attempts stacked up. **Copy
everything from the last line containing `provisioning check starting` onward** —
that's the beginning of your most recent attempt. Each line is timestamped
(`[info]`/`[error]`) and names the actual step and command involved (e.g. "checking
`podman --version`", "running `winget install ...`", "`podman machine init` failed:
\<real error text\>") — paste the real lines, not a paraphrase, since the exact
command output is usually what actually explains the failure.

## 6. What "success" looks like

Once the run flow completes, the app's session screen shows a `localhost` port for
the `app` service. Open a terminal and:

```
curl http://localhost:<port>/health
```

Success: `{"status":"UP"}`, HTTP 200. That means Podman was fully provisioned *and*
the sample Spring Boot + MySQL containers actually came up and are serving traffic —
the real end-to-end result, not just "the button stopped spinning."

A **partial failure worth reporting even if `/health` eventually works**: anything
that took noticeably long, needed a manual retry, or logged an `[error]` line along
the way even though it self-recovered — those are exactly the rough edges this round
can't have caught without real hardware.

## What to send back

1. Which OS/version (and Windows edition, if Windows).
2. What happened at step 4 (clean / clear error / hang).
3. The copied log section from step 5.
4. The `curl` result from step 6 (or "never got that far").

That's enough for both of us to know exactly where it broke without needing to
reproduce your machine.

---

## Round 7 addendum: retesting the real Send flow (Windows ⇄ Linux)

This directly follows up the real two-machine test that found the sender stalling
indefinitely at the bundling step, with the receiver eventually timing out having
never received an offer. Here's what changed and what to check on retry.

### What changed

- **`git_bytes` (the function behind every git call `create_snapshot` makes) is now
  hardened**: stdin is explicitly closed (a subprocess can no longer block waiting on
  input from a GUI app with no terminal), `--no-pager` is passed defensively, and every
  git call now has a real 30-second timeout instead of none. If bundling ever blocks
  again, it will fail with a clear, specific error after 30s — not hang forever. (We
  could not reproduce the original hang on our own test hardware; this is a real,
  verified hardening pass against the most likely cause, not a confirmed-exact fix —
  if it happens again, the log below will show exactly which git command it's stuck on
  this time, which we didn't have before.)
- **A new `send.log`**, same convention as `provisioning.log` (below), covering the
  *whole* Send flow: bundling (each git command + timing), signing, connecting to the
  signaling server, offer/answer creation, ICE gathering, data channel open, and
  transfer progress. If Send stalls or fails again, this is the file to check first.
- **A "Browse…" button** next to the project-path field on the Send tab — no more
  typing an absolute path by hand.
- STUN was already a real, always-available public server (`stun.l.google.com:19302`,
  unconditional since round 1) — confirmed this is *not* what caused the original
  failure, so no change was needed there.

### What to redo

1. Update both machines to this build.
2. Same setup as before: signaling server reachable from both machines. The Windows
   installer now adds the inbound TCP/UDP firewall rule itself at install time (NSIS
   post-install hook running `netsh advfirewall firewall add rule ...`, removed again
   on uninstall) — no manual `New-NetFirewallRule` step needed any more. On Linux, if
   `ufw` is active the app logs a warning to `send.log` and shows a banner at startup;
   allow LocalSync through `ufw` if Send/Receive hangs (the port is dynamic — chosen
   when you click Send, not fixed).
3. **Use the new Browse button** instead of typing the path, on whichever side is
   sending.
4. Click Send. If it stalls again, note how long you waited, then check the log
   (below) *on the sending machine* rather than just the receiving one's timeout
   message — that's where the useful detail will be this time.

### The new log file

Same location convention as `provisioning.log`, different file:
- **Windows**: `%APPDATA%\localsync\logs\send.log`
- **Linux**: `~/.local/share/localsync/logs/send.log`
- **macOS**: `~/Library/Application Support/localsync/logs/send.log`

Copy everything from the last `share_snapshot: starting` (sender) or
`receive_snapshot: starting` (receiver) line onward. A healthy run's log looks like:
bundling started → each git command with its timing → bundling done → signing →
connecting to signaling → offer created → ICE gathering complete → offer sent → data
channel open → transfer progress → done. **Whichever of those lines is the *last* one
in the log is where it actually stopped** — that's the single most useful thing to
report back, more useful than the error text alone.

### What "success" looks like this time

Same as step 6 above (`curl .../health` → `200 {"status":"UP"}`) — but now reachable
from *either* stack: if you're testing with `sample-project-node/` instead of
`sample-project/`, the equivalent check is `GET /api/notes` returning the seeded rows
on whatever port the session screen shows.

---

## Round 8 addendum: no more manual signaling server, and a real Linux picker fix

### What changed

- **Signaling is embedded now.** Send no longer needs `apps/signaling-server`
  running as a separate process anywhere — clicking **Send** hosts an ephemeral
  relay inside the app itself and shows you one short code (e.g.
  `0007Y2N5wKp2mQ`) that packs your LAN IP, the relay's port, and a room id.
  Paste that single code into the receiver's **Room code** field — nothing
  else to type. Same-box round-trip through this exact path is covered by
  `crates/ls-net/tests/embedded_relay_test.rs`; real cross-machine reachability
  is still what *this* checklist is for. The old "Signaling server URL" field
  still exists under **Settings**, now optional — leave it blank for the
  normal flow above; set it only if you want to point both sides at a
  manually run server instead (e.g. relaying through a box neither machine's
  LAN can reach directly).
- **Windows Firewall rule is automatic.** The NSIS installer now adds the
  inbound TCP/UDP rule for the installed binary at install time (and removes
  it on uninstall) — no manual `New-NetFirewallRule` step. Not re-verified by
  installing on real Windows hardware this round (that's what this checklist
  is for) — if Send/Receive still can't connect, check Windows Defender
  Firewall's inbound rules for "LocalSync" as the first thing to confirm.
- **Linux `ufw` gets a real warning.** If `ufw` is active, the app now shows a
  banner at startup and logs it to `send.log` — the port is dynamic (chosen
  when you click Send), so there's no fixed rule to add in advance; allow
  LocalSync through `ufw` (or disable it for the test) if the relay never
  gets reached.
- **The Linux "Browse…" picker's actual root cause** (silently doing nothing
  when clicked, since round 7): confirmed by reading the exact pinned
  `rfd`/`tauri-plugin-dialog` source — the default Linux backend
  (`gtk3`) drives the file picker through a *second*, privately-spawned GTK
  thread that's independent of the GTK main loop Tauri's webview already
  owns on the real main thread. Most desktop Linux setups tolerate this;
  it's the same class of bug Tauri itself has open issues about
  (`tauri-apps/tauri#11312`, "GTK may only be used from the main thread").
  Fixed by switching to the `xdg-portal` backend instead (asks the
  out-of-process `xdg-desktop-portal` D-Bus service for the picker — no
  second in-process GTK loop, so this class of bug can't happen). Requires
  `xdg-desktop-portal` plus a backend for your desktop (`xdg-desktop-portal-gtk`
  for GNOME/generic, `-kde` for KDE, etc.) — standard on any mainstream
  Linux desktop, but **please confirm Browse actually opens a picker** on
  your real machine as part of this retest; a from-scratch sandbox with no
  desktop environment at all couldn't give a clean interactive confirmation
  this round the way real hardware can.
- **Bundling now skips noise directories** (`node_modules`, `.git`, `target`,
  `build`, `dist`, `__pycache__`, `.venv`, `venv`, `vendor`, `.next`, `.nuxt`)
  even if they were accidentally committed without a `.gitignore` — nothing
  to check by hand here, just fewer surprises in the diff you review before
  clicking Run.

### What to redo

1. Update both machines to this build.
2. On the sender: click **Send**, pick the project with **Browse…**, click
   **Send** again. Confirm a picker dialog actually opens for Browse (this is
   the one item this round couldn't verify without real hardware — see above).
3. Copy the short code shown after Send starts. On the receiver: paste it
   into **Room code**, click **Receive**. Nothing else to configure on either
   side unless you're deliberately using the Settings override.
4. If it stalls, check `send.log` on **the sending machine** as before — same
   file, same convention (`share_snapshot: starting` onward).

### What "success" looks like

Same as above: the review screen appears on the receiver, click Run, then
`curl .../health` (or `GET /api/notes` for the Node stack) succeeds.

---

## Round 9 addendum: retry after a failed Run, the details toggle, the diff view

### What changed

- **A failed Run no longer eats the held snapshot.** If Run fails for an
  environment reason (Podman not found, a bad work directory, a port
  already in use, ...) the snapshot stays held — fix whatever broke and
  click **Run** again on the same review screen, no need to redo Send/Receive.
- **Run now shows a spinner by default**, with a **Show details ▾** toggle
  that reveals a small scrollable terminal-style log streaming the real
  provisioning output live (the same content that's always gone to
  `provisioning.log` — this just also shows it in the app). Collapsed and
  cleared fresh on every Run click.
- **The diff-review screen now groups files by directory** (native
  collapsible sections, one per directory) with a "N files changed" count
  up top, instead of one flat table — larger directories inside a big diff
  start collapsed, everything else starts open.

### What to redo

1. Trigger a Run failure on purpose — easiest way: temporarily rename/move
   `podman` off `PATH`, or point **Work directory** at a path that can't be
   created (e.g. a path through an existing file). Confirm the error shows
   and the review screen is still there with **Run** still clickable — not
   forced back to Send/Receive.
2. Fix whatever you broke, click **Run** again on the *same* review screen.
   Confirm it succeeds without a fresh Send/Receive.
3. Click **Run** on a normal, working snapshot and click **Show details**
   while it's provisioning — confirm real log lines stream in (not a static
   placeholder), and that the log is empty again on the next Run attempt.
4. On the receiver, look at the diff-review screen for a snapshot with files
   in multiple directories (either sample project works) — confirm it's
   grouped by directory with an expand/collapse per group, not one flat list.

### What "success" looks like

Same as the rest of this document — the point of this round's changes is
resilience/clarity around the existing flow, not a new end state to check.

---

## Round 10 addendum: remote relay mode, and known-peer pairing

### What changed

- **Settings now has a real "Connection mode" choice**: **Local network**
  (default — round 8's behavior, unchanged) or **Remote relay**, which shows
  a "Remote relay URL" field. Set once, it's saved (survives restarts) — no
  more re-typing a manual signaling URL every time.
- **Remote relay mode is for peers on different networks**, where a LAN IP
  in the room code is useless. Both apps point at the same self-hosted
  `apps/signaling-server` instance (see the README's new self-hosting
  section) — room codes are still one short paste-able string, just without
  an IP baked in.
- **The review screen now shows who's sending.** A recognized returning
  sender shows "Recognized peer: `<name>`"; a first-time sender shows "New
  sender" with a "Remember as…" field to name and save them for next time.
  **This is informational only** — it changes nothing about what you need to
  do before Run. A new **Reject** button next to Run discards the received
  snapshot immediately without running it, for either case.

### What to redo

1. **Remote relay, same-network stand-in**: on two machines (or two app
   instances on one machine, different `LOCALSYNC_DATA_DIR`), self-host
   `apps/signaling-server` per the README, put its address into both apps'
   Settings → Remote relay, do a normal Send/Receive/Run. Confirm the room
   code is one short string (no manual URL typed anywhere except the
   one-time relay setup), and that it works exactly like Local network mode
   otherwise. If you have access to two genuinely different networks, that's
   the real test this round exists for — a relay reachable from both sides
   (a public VPS, or one side's router port-forwarded) is what proves the
   original "share with anyone, anywhere" case actually works again.
2. Switch back to **Local network** mode and confirm Send/Receive still work
   exactly as in round 8/9 — this must be unaffected.
3. **Peer pairing**: do a Receive from a sender for the first time — confirm
   the review screen says "New sender", type a name, click Save, confirm it
   confirms saved. Do another Receive from the *same* sender (same machine's
   `~/.localsync/identity.key` — don't regenerate it) — confirm this time it
   shows "Recognized peer: `<the name you saved>`".
4. On a recognized-peer receive, confirm the diff/Run/Reject buttons behave
   completely normally — recognizing a peer must never pre-fill, skip, or
   auto-click anything. Try **Reject** once on a snapshot you don't intend to
   run — confirm it clears back to the idle Receive screen without touching
   Podman at all.

### What "success" looks like

Same as the rest of this document for the actual Run flow. The new-this-round
checks are about the *setup and recognition* steps behaving as described,
not a new curl/health-check target.

---

## Round 11 addendum: multiple receivers, targeted push, pull requests

### What changed

- **A sender's connection to each receiver now stays open** after the
  initial Send, instead of closing once the transfer finishes. This is what
  makes everything below possible without a fresh room code each time.
- **Multiple receivers at once.** The Send tab now shows a "Connected
  receivers" list — every receiver who's received from you and is still
  connected, each with its own **Push update** button.
- **Targeted push**: clicking **Push update** next to one specific receiver
  bundles your project's *current* state and sends it to *that receiver
  only* — the others don't see anything.
- **Pull requests**: on the Receive tab, after a successful Receive, an
  **Ask for update** button asks the sender "got anything new?". The sender
  sees a banner (can appear on any tab) with **Accept**/**Decline** — Accept
  triggers the same bundle-and-send Push does; Decline does nothing further.
  A pushed/pulled update shows up on the receiver's review screen exactly
  like a fresh Receive — same diff, same Run/Reject buttons, nothing
  auto-runs.

### What to redo

1. From one sender, Send to **two different receivers** (two app instances,
   two room codes). Confirm both show up in the sender's "Connected
   receivers" list at once.
2. Click **Push update** next to just one of them. Confirm only that
   receiver's review screen updates (a brief banner, then the diff refreshes)
   — the other receiver should see nothing.
3. On the *other* receiver, click **Ask for update**. Confirm the sender
   sees a pull-request banner naming that receiver, click **Accept**, and
   confirm that receiver's review screen updates the same way step 2's did.
4. Try **Decline** once on a pull request — confirm nothing happens on the
   receiver's side (no update, no error, just... nothing).
5. On any update received this way (pushed or pulled), confirm Run/Reject
   work exactly as before — a pushed update is never auto-run.

### What "success" looks like

Same as the rest of this document for the actual Run flow — this round is
about the *push/pull mechanics and targeting* working as described.

---

## Round 12 addendum: the room code no longer expires mid-handoff, peer pairing is reachable, Linux setup is more resilient

### What changed

- **The room code lasts 5 minutes, not 30 seconds**, and now shows a visible
  countdown ("Expires in 4:32 — share it before then.") next to the code
  instead of a silent timer you only discover by hitting a failure. It
  disappears once a receiver actually connects; if it hits zero first, it
  says so plainly ("Code expired — click Send again for a new one.").
- **The Send tab now has a "Previously connected" section** at the top,
  above the fresh-code flow — anyone still connected from earlier this app
  session, with a **Push update** button that reuses that connection
  directly (no new code to share). Labelled "still connected this session"
  deliberately — it's round 11's live-connection roster, not a persisted
  cross-restart history (a receiver has no identity of its own for this app
  to remember that way; see round 10's *sender*-recognition on the Receive
  tab for the actual identity-based pairing, now shown more prominently at
  the very top of the review screen).
- **`scripts/setup-linux-deps.sh` now has a real fallback if `podman-compose`
  isn't installable via apt** (some distros/releases don't carry it) — tries
  `pipx install podman-compose` instead, and tells you to open a new
  terminal if the freshly-installed binary isn't on this shell's `PATH` yet.

### What to redo

1. Click **Send**, watch the countdown appear next to the room code. Wait
   past it on purpose once (or check the error text) to confirm it says
   "expired" clearly rather than a bare connection-failure message.
2. Send to a receiver, then — **without generating a new code** — go back to
   the Send tab and confirm that receiver shows up under **Previously
   connected**. Click **Push update** and confirm the receiver gets it.
3. Receive from the same sender twice: the first time should show "New
   sender", the second time (after saving a name the first time) should show
   "Recognized peer: `<name>`" at the very top of the review screen, above
   everything else.
4. If you're setting up a fresh Linux machine and `podman-compose` isn't in
   your distro's apt repos, confirm `scripts/setup-linux-deps.sh` falls back
   to `pipx` automatically rather than just failing.

### What "success" looks like

Same as the rest of this document — this round is about resilience/clarity
around flows that already existed, not a new end state to check.

---

## Round 13 addendum: no more Windows console windows, a details panel that actually shows something

**This round could not visually confirm either fix — there is no real display
in the environment that built it.** Everything below is real, but real at the
code/process level (a subprocess actually spawns with the right flag; a real
`podman-compose` process's stdout is actually captured into the log a live
view tails) — not "we saw it and it looked right." That confirmation is
squarely what this checklist entry is for.

### What changed, and why (root causes, not guesses)

- **Every subprocess spawn in this codebase now goes through a small
  `CREATE_NO_WINDOW`-setting helper on Windows** (`podman`, `podman-compose`,
  `git`, `winget`, `wsl.exe`, `reg.exe` — audited, not assumed to be just
  one call site) instead of a bare `Command::new`. Verified: the Windows-only
  provisioning module (`windows_impl.rs`) was actually compiled for real on a
  native Windows host as part of this round (it's `#[cfg(target_os =
  "windows")]`-gated, so WSL2 alone can't even compile it) — a real build
  check, not just "the diff looks right."
- **Root cause of the Linux "Show details" panel showing (almost) nothing**:
  round 9's live-tailer only ever watched `ensure_podman_ready()`'s own
  preflight-check logging — on Linux that's a handful of near-instant lines
  ("podman found on PATH", ...), then total silence for the rest of a real
  Run, because the actual slow part (`podman-compose up` pulling/building
  images) was never logged anywhere at all. Fixed: `podman-compose`'s real
  stdout/stderr now streams into the same log file live, line by line, as it
  runs — the exact data source the existing tailer already watches, so no
  separate plumbing was needed on the UI side once this was fixed.
- **A second, independent bug found while investigating**: the details panel
  was being hidden the instant a Run finished — *including on failure*,
  which is exactly the moment its content (now real) would matter most.
  Fixed: it now stays visible after a failure (clearing only when you start
  a fresh Run attempt), and only auto-hides on success, where the screen has
  already moved on to the running-session view anyway.

### What to check on real hardware

1. **Windows**: receive a project and click Run. Watch closely during
   provisioning and container bring-up — **no terminal/console window should
   flash or appear at any point**, however briefly.
2. **Either platform**: click Run, then **Show details** while it's still
   provisioning. Confirm real, moving text appears — not a static/empty box
   — and that it looks like actual `podman-compose` output (image
   pulls, container names, etc.), not just a couple of static lines that
   stop.
3. **Trigger a Run failure on purpose** (same trick as the round 9 addendum
   above — rename `podman-compose` off `PATH` temporarily, or similar).
   Confirm the details panel **stays visible with its content intact** after
   the error appears, instead of vanishing the instant it fails.

### What "success" looks like

Step 1: genuinely nothing flashes on screen, the whole time. Steps 2–3: the
panel shows real, live-updating content during a run, and that content is
still there to read after a failure — not just before you'd sworn you saw
something.

---

## Round 14 addendum: real macOS verification via CI (no Mac needed to check)

Unlike every other addendum in this file, this one doesn't need real hardware
you're sitting in front of — it needs you to click a button on GitHub and
read the result.

### What to do

1. Go to the repo's **Actions** tab on GitHub → **macOS build & verification**
   → **Run workflow** (it's `workflow_dispatch`-only on purpose, see the
   workflow file's own comment — it doesn't run automatically, so this step
   is required before anything below exists to check).
2. Wait for both jobs to finish (`build-and-test` and
   `podman-provisioning-investigation` run independently and don't block
   each other).

### What to check

- **`build-and-test`**: green means the workspace actually compiled and its
  tests actually ran on real macOS. Open the job's own step summary for the
  real `cargo test --workspace` pass/fail breakdown (the `cargo-test-macos-log`
  artifact has the full raw output if a summary line isn't enough). Download
  the `LocalSync-macos-dmg` artifact and confirm it's a real `.dmg` — you
  don't need a Mac to check the file exists and has a real size; opening/
  installing it does need one.
- **`podman-provisioning-investigation`**: read its step summary regardless
  of whether the job shows green or red — **red here is an expected, honest
  possible outcome**, not a bug in this round's work. It means Podman
  couldn't fully provision inside GitHub's macOS runner (most likely a
  nested-virtualization restriction — check the `sysctl kern.hv_support`
  output in the first step's log for the real diagnostic signal), which is
  itself the real answer to a real, previously-unknown question. Green means
  a real container genuinely ran (`podman run --rm hello-world` succeeded).

### What "success" looks like

Note: "success" here doesn't mean "both jobs are green." It means you can
now state, for the first time, a **real, evidence-based answer** — for both
"does LocalSync build and pass its tests on real macOS" and "does Podman
provisioning work inside macOS CI" — instead of the "should work by analogy"
this project has had since round 5. If either investigation was blocked,
that itself is the useful, honest outcome to record here.

---

## Round 15 addendum: the release pipeline, and the new Download & Install steps

Also doesn't need you sitting at a specific machine to check the pipeline
itself — but the *install steps* genuinely do, on all three OSes, since
they're brand new and aimed at a first-time, non-technical user for the
first time in this project.

### What to do

1. Go to the repo's **Actions** tab → **Release** → **Run workflow** (or
   push a tag: `git tag v0.1.0 && git push origin v0.1.0` — either
   triggers it). Wait for all four build/release jobs to finish.
2. Confirm a new entry appears on the **Releases** page with three files
   attached: a Windows `.exe`, a macOS `.dmg`, and both a Linux `.deb` and
   `.AppImage`.

### What to check

- **On a real Windows machine you haven't already set up for development**:
  download the `.exe` from the Release, run it, confirm SmartScreen shows
  up and "More info" → "Run anyway" actually gets you through it, and that
  the app installs and opens.
- **On a real Mac**: download the `.dmg`, confirm the drag-to-Applications
  step works as described, and that double-clicking normally really does
  fail first (confirming the warning text in the README is accurate) before
  right-click → Open succeeds.
- **On a real Linux machine** (ideally one you haven't already installed
  build dependencies on): try both the `.deb` and the AppImage per the
  README's steps, including on a distro that doesn't ship `libfuse2` by
  default if you have one handy, to confirm that specific guidance is
  accurate too.
- Confirm the version-naming scheme reads sensibly on the Releases page
  either way you triggered it (a real `vX.Y.Z` tag, or the generated
  `local-<date>-<sha>` name from a manual run) and that re-running a manual
  build doesn't silently clobber a previous one's assets.

### What "success" looks like

A person who has never seen this project, following only the README's
**Download & Install** section (not this checklist, not anything
developer-facing), ends up with a running LocalSync on their machine,
without ever feeling like something was broken versus merely unsigned.

---

## Round 16 addendum: real auto-update, and code-signing infrastructure

Real hardware is the only way to check the actual update-and-restart
click-through (nothing in this project's build/test environment has a
display reliable enough for that final step — see the round's own commit
for exactly what *could* be verified without one, which is most of it).

### What to do

1. Push a `v*` tag (or run the Release workflow manually) once
   `TAURI_SIGNING_PRIVATE_KEY`/`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` are
   added as repository secrets (see the values generated this round —
   ask whoever ran round 16 for them, or generate a fresh pair yourself
   with `npx tauri signer generate` and update `tauri.conf.json`'s
   `plugins.updater.pubkey` to match if you do).
2. Confirm the Release page gets a real `latest.json` asset attached
   alongside the installers, and that its `platforms` object has real
   (non-empty) `signature` values for `windows-x86_64`, `linux-x86_64`,
   and `darwin-aarch64`.
3. Install that release's build on a real machine, then intentionally
   publish a *newer* one (bump `tauri.conf.json`'s `version` field first —
   the updater compares real semver, so two same-version builds
   correctly won't show an update between them). Open the installed
   app: within a few seconds, the "Update available" banner should
   appear on its own, unprompted but not auto-installing anything.
4. Click **Install update**, confirm real download progress shows, and
   that the app actually relaunches on the new version afterward.
5. Click **Check for updates** manually (in Settings) when already on
   the latest version — confirm it says "up to date" rather than
   silently doing nothing.

### Code-signing (part B) - only once you have real credentials

See `docs/code-signing.md` for exactly what to obtain and which GitHub
secrets to add. Once added:

1. Re-run the release workflow, confirm the `build-windows`/`build-macos`
   logs show "Imported certificate..." rather than "No ... secret
   configured".
2. On the built installer, verify the signature for real (not just "the
   log said it worked") — `docs/code-signing.md`'s last section has the
   exact `codesign`/`spctl`/right-click-Properties steps for each OS.
3. Confirm SmartScreen/Gatekeeper's warning either disappears entirely
   (EV cert / notarized build) or changes to show your real
   organization name instead of "Unknown Publisher" (OV cert, before
   SmartScreen reputation has built up).

### What "success" looks like

Someone with an older installed build, doing nothing except opening the
app, sees a real, real update offered — reviews it, clicks once, and
ends up on the new version without ever touching a terminal or
re-downloading anything by hand.

---

## Round 17 addendum: guided database-source wizard for Send

Round 17 replaces the Send tab's single-folder input with a real,
multi-step wizard, and adds a new database-dump path into the snapshot
manifest/payload. Everything below is verified in this build environment
via real automated tests (multi-folder bundling, a real disposable local
MariaDB instance, real connect/list/export, a real round-trip reimport,
and the full command-layer flow via `wizard_send_flow_test.rs`) — what's
deferred to real hardware is purely the *visual* click-through (does the
wizard's UI actually render and step through correctly in a real running
app), the same category of gap every prior round's UI work has had.

### What changed

- **Multi-folder Send.** The Send tab's "Browse…" now opens a real
  multi-select directory picker; selected folders show in a removable
  list before moving on.
- **The wizard flow.** "Does this project need database access?" (asked
  once) → per-folder detection (Spring Boot's `application.properties`/
  `.yml`) → dump-already-exists vs. connect-and-export → manual entry
  when detection fails, feeding into the same subsequent flow → a final
  summary before the real Send.
- **Real live database browsing.** When exporting, the wizard shows the
  actual tables in the developer's own database (with approximate row
  counts) before asking which to include — never a blind guess. Export
  is always the full table content, never sampled.
- **Manifest extension.** `Manifest.folders` and `Manifest.database_dumps`
  are new, additive fields (old manifests deserialize fine without them;
  old code reading a new manifest ignores them) carrying the per-folder
  breakdown and `{folder, schema, dump_file, hash}` for each packaged
  dump.
- **A real bug found and fixed in the same area**: round 16's own
  auto-update banner reused the id `update-banner`, silently colliding
  with an older Receive-tab element of the same id (round 11's "a new
  update just arrived from the sender" notification) — `document.
  getElementById` was silently resolving to the wrong element for one of
  the two. Renamed round 16's banner to `app-update-banner`; the Receive
  tab's own push-update notification is unaffected but now actually
  correct again.

### What to do

1. On the Send tab, click **Add folder(s)…** and select two or more
   independent project folders in one picker (e.g. copies of this repo's
   `sample-project`/`sample-project-node` fixtures, or your own real
   multi-folder Spring Boot setup) — confirm they all show in the list
   with working **Remove** buttons.
2. Click **Next**, answer **Yes** to "Does this project need database
   access?" — confirm the wizard walks each folder one at a time,
   showing "Folder N of M".
3. Point one folder at a real local MySQL/MariaDB with a Spring Boot
   `application.properties`/`.yml` already configured — confirm the
   wizard auto-detects and displays the real host/port/database/username
   before asking anything.
4. Choose **No, connect and export** — confirm a real table list appears
   (with row-count estimates), select a subset, click **Export selected
   tables**, and confirm a real size is reported.
5. On a second folder, delete/rename its config so detection fails —
   confirm manual entry is offered, and that submitting it re-attempts a
   real connection before continuing into the same "do you have a dump"
   question.
6. On a third folder, answer **Yes, I have a dump file** and browse to
   an existing `.sql` file directly — confirm no connection is attempted
   for that folder.
7. Reach the final **Ready to send** summary, confirm it correctly
   labels each folder's outcome (no database / supplied dump / exported
   dump), then click **Send** and complete a real receive on the other
   side — confirm the diff review shows entries from every folder,
   correctly prefixed by folder name.

### What changed, and why (root causes, not guesses)

`ls_security::diff_summary` originally hard-errored ("payload does not
contain diff_stat.json") for any snapshot without a single top-level
`diff_stat.json` — which is exactly what a multi-folder snapshot produces
(each folder ships its own, at `<folder>/diff_stat.json`). Without
fixing this, `receive_snapshot` itself would fail outright for every
multi-folder send, before a human ever saw a review screen — found and
fixed as part of this round's own work, verified by a real end-to-end
test (`wizard_send_flow_test.rs`) that would fail immediately if this
regressed.

A second real, environment-specific finding (not a code bug, but worth
recording exactly like the pasta-segfault and podman-storage-path issues
documented earlier): this sandbox's `mariadbd` binary runs under an
AppArmor profile that denies *any* process — including its own direct
parent — from delivering it a signal at all (confirmed via `dmesg`'s
audit log). A disposable test server's `Child::kill()` fails silently
there, and the matching `Child::wait()` then hangs forever. Both new
live-database test files stop their disposable server with a real SQL
`SHUTDOWN` instead, which isn't a signal and isn't subject to that
mediation — real hardware without this specific AppArmor confinement
would never have hit this at all, but the fix is correct and harmless
either way.

### What "success" looks like

A developer with several raw, uncontainerized project folders — some
with a database, some without, some with an existing dump, some needing
a fresh export — can go from "select folders" to "sent" without ever
leaving the guided flow or being asked to decide anything about a
database blind. The receiver can review the resulting diff, correctly
broken out per folder, exactly as informatively as a single-folder send
always could. Building and running whatever was received — especially
several raw, non-containerized folders with no `docker-compose.yml`
between them — is explicitly not yet solved; that's the next round's job.

---

## Round 18 addendum: multi-engine DB support, a real connection bug fix, installer terminal flashing

Round 18 fixes three real problems a developer hit on the first live
click-through of round 17's wizard. Two of the three (multi-engine
support, the connection-test bug) are proven here with real automated
tests against real local database servers — a real MariaDB, a real
PostgreSQL 18, and a real MongoDB 7 all connected to, listed, exported
from, and round-tripped through a restore in this environment. The
third (installer terminal flashing) is a Windows-visual behavior this
sandbox cannot observe directly; it's fixed with confidence from a
code-level NSIS review, but **needs a real click-through on real Windows
hardware to actually watch the install and confirm no console window
flashes**, same as every other Windows-visual claim in this project.

### What changed

- **Real multi-engine support.** The Engine field is a real dropdown
  (MySQL/MariaDB, PostgreSQL, MongoDB), each with genuinely working
  connect/list/export logic — PostgreSQL via the real `postgres` crate
  and the real `pg_dump` binary, MongoDB via the real `mongodb` driver
  and the real `mongodump`/`mongorestore` binaries. Picking an engine
  also updates the suggested default port (3306/5432/27017).
- **The manifest now records engine per dump** (`DatabaseDumpEntry.engine`,
  additive/backward-compatible — an old round-17 manifest without it
  defaults to "mysql", which is correct since round 17 never produced
  anything else). The dump file's extension follows the engine too:
  `.sql` for MySQL/PostgreSQL, `.tar.gz` for MongoDB (a tarred directory
  of `mongodump`'s own BSON output — restoring it means untarring, then
  a directory-mode `mongorestore`, never a SQL-style restore).
- **A real, reproduced connection bug, found and fixed.** A literal
  `host: "localhost"` could fail to connect at all, on both Windows and
  Linux, even with correct credentials — root-caused (not assumed) by
  reproducing it against a real local MariaDB in this sandbox: this
  sandbox's `localhost` resolves to the IPv6 loopback (`::1`) only,
  while the server listened on the IPv4 loopback only, so the very
  first TCP connect attempt was refused before any auth ever happened.
  Windows ships the same `::1 localhost` default, which is why the bug
  showed up identically on both platforms despite being an OS/DNS
  resolution issue, not a credentials one. Fixed by substituting the
  unambiguous `127.0.0.1` for a literal "localhost" before connecting,
  for all three engines.
- **Real underlying errors now reach the UI.** The command layer was
  converting every `ls-dbsource` error with `.to_string()`, which for an
  `anyhow::Error` only shows the outermost "failed to connect to ..."
  wrapper and silently drops the real reason (connection refused, wrong
  password, unknown database, ...). Now uses the full error chain
  (`{:#}`), verified directly against real failures of each kind.
- **NSIS install-time terminal flashing fixed.** Round 8's Windows
  Firewall install hook (`apps/desktop/src-tauri/windows/hooks.nsh`) ran
  `netsh.exe` via plain NSIS `ExecWait`, which visibly flashes a console
  window — switched to `nsExec::ExecToLog` (NSIS's own bundled plugin,
  no extra download), which runs it hidden and pipes its output into the
  installer's own detail log instead.

### What to check (real hardware needed for the installer fix specifically)

1. **Installer terminal flashing (Windows, real hardware required)**: run
   a fresh install of the built `.exe`. Watch closely during the install
   (and again during an uninstall) — confirm **no console/terminal window
   ever flashes on screen**, even briefly, while the firewall rules are
   being added/removed. Then confirm the firewall rules were still
   actually created: Windows Defender Firewall → Advanced Settings →
   Inbound Rules → look for "LocalSync" (TCP and UDP).
2. **Multi-engine wizard, on real hardware**: for each of MySQL,
   PostgreSQL, and MongoDB, point the wizard's manual-entry form at a
   real local instance (or a Spring Boot project's real
   `application.properties` for the MySQL case, same as round 17), using
   the literal host value `localhost` specifically (not `127.0.0.1`) —
   confirm the connection succeeds and the real table/collection list
   appears with plausible row/document counts.
3. **Error surfacing**: deliberately get a connection test wrong (bad
   password, a database/collection that doesn't exist, wrong port) for
   each engine — confirm the wizard shows the *real* reason (e.g.
   "Access denied for user", "Unknown database", "password
   authentication failed"), not a bare "failed to connect".
4. **A real MongoDB round trip on real hardware**: export a couple of
   collections, then (once a future round adds restore) confirm the
   `mongorestore <dir>/dump` directory-mode restore documented in
   `crates/ls-dbsource/src/engines/mongo.rs` actually works against the
   real exported archive, not just the same-box proof this round already
   has via `cargo test`.

### What "success" looks like

A developer picks whichever of the three real engines their project
actually uses, types in connection details exactly the way they always
would (including plain `localhost`), and it just works — no cryptic
generic failure, no silent fallback to MySQL's logic for an engine that
was never really implemented. And nobody sees a flashing console window
appear and disappear while LocalSync installs.

---

## Round 22 addendum: wizard flow order, error placement, schema browsing, multi-folder

Round 22 fixes real problems found using round 17/18's wizard against
genuine local setups: the step order didn't match round 17's own design
intent, errors were generic and mis-placed, and a typed database name was
trusted without ever showing what's actually on the server. The logic/
ordering fixes (goals 1, 2, 4, 6) are proven with real tests against real
local MySQL/PostgreSQL/MongoDB instances in this sandbox; the two
presentation goals (3, 5) are implemented and same-box-verified (the
right real data reaches the display layer), but **only a real click-
through on real hardware confirms they actually look right**.

### What changed

- **Dump-file question now comes first** (goal 4): for every folder,
  detected or not, "do you already have a dump file?" is asked
  immediately after (cheap, local) auto-detection — a live connection is
  now only ever attempted on the "no, I need to fetch live data" path.
  Previously, manual entry tested the connection *before* this question
  could even be asked, forcing a working connection just to say "I have a
  dump."
- **Real schema browsing after a successful connection** (goal 3): a new
  `list_db_schemas` command (and each engine's own `list_databases`,
  proven against real MariaDB/PostgreSQL/MongoDB instances) lists what's
  really on the server — connecting without requiring the typed/detected
  database name to already be correct — and the developer picks from that
  real list (pre-selecting the typed name if it's actually there) before
  anything else happens. Manual entry's own "Test connection & continue"
  now uses this same command instead of a database-pinned connection
  test, so a typo'd database name no longer blocks getting to the real
  list that would let you fix it.
- **Contextual error placement** (goal 2): a database/schema-not-found
  failure during manual entry now shows directly under the Database
  field, not just as a generic trailing message.
- **Clearer missing-client-tool errors** (goal 1): a missing `pg_dump`/
  `mongodump` binary now names itself and points at
  `scripts/setup-linux-deps.sh` (Linux) or the right install command
  (macOS) — that script now also documents installing both. MySQL/
  MariaDB needs no external client tool at all (pure-Rust driver), so
  there's nothing to install for it.
- **Improved table/collection selection display** (goal 5): a real
  checkbox card list with Select all/none and each row's approximate
  count, instead of a plain unstyled list.
- **Shared-database question for multi-folder sends** (goal 6): selecting
  more than one folder now asks once whether they share one database —
  if yes, auto-detection runs across all of them first, and if it finds
  genuinely different setups, falls back to per-folder entry
  automatically (with a clear note explaining why) rather than silently
  guessing.
- **Dump-file picker now filters by engine** (goal 7): `.sql` for MySQL/
  PostgreSQL, `.gz`/`.tar`/`.archive` for MongoDB (matching this
  project's own round-18 mongodump export, which tars+gzips a directory
  dump into one file) — the engine is now always known (via detection,
  manual entry, or an inline picker when neither has happened yet) before
  the file dialog ever opens.
- **A real, separate bug found and fixed while integrating this round**:
  the final Send step was reconstructing each folder's dump payload for
  the `share_snapshot_wizard` IPC call without its `engine` field (a
  required field on the Rust side since round 18) — every database-
  attached send would have failed at the IPC boundary. Missed by round
  18's own tests because they call the Tauri command directly, bypassing
  this exact JS reconstruction step.

### What to check on real hardware

1. **The reordered flow itself**: for a folder with no auto-detected
   config, confirm you're asked "do you already have a dump file?"
   *before* any connection fields are shown — answering "yes" should
   never require you to fill in host/port/credentials at all.
2. **Schema browsing, visually**: after a successful connection, confirm
   the real list of databases/schemas actually renders as a picker (not
   just that the right data reached the wizard — this sandbox can't see
   the rendered UI). Try it with a typo'd database name in manual entry —
   confirm you reach the real list instead of being blocked.
3. **Table/collection selection display**: confirm the new checkbox card
   list, Select all/Select none, and row counts actually render and work
   by clicking, for a table list long enough to scroll.
4. **Multi-folder shared-database question**: select 2+ folders with a
   real shared database, confirm the wizard asks once and reuses the
   connection; then try 2+ folders whose own configs genuinely point at
   different databases, and confirm the automatic per-folder fallback
   (with its explanatory note) actually happens and looks right, not just
   that it behaves right.
5. **File picker filtering**: confirm the OS-native file dialog actually
   restricts to `.sql` (MySQL/PostgreSQL) or `.gz`/`.tar`/`.archive`
   (MongoDB) when browsing for an existing dump — this is OS/dialog
   behavior this sandbox cannot render or click.
6. **Missing-tool error, for real**: on a machine without `pg_dump` or
   `mongodump` installed, confirm export shows the new actionable message
   pointing at `scripts/setup-linux-deps.sh` (or run the script itself and
   confirm the install commands it prints actually work).

### What "success" looks like

A developer with an existing dump for one folder never has to fight
through a connection form to say so. A developer fetching live data sees
the real state of the server — real schemas, real tables, real counts —
at every decision point, never a value they typed being silently trusted.
Multiple folders sharing one database is set up once, not N times. And
every error, wherever it appears, says something a person could actually
act on.

---

## Round 20 addendum: layout, functional audit, flow reorder

Round 20 fixes three real problems found by actually using the app: the
whole UI was left-aligned with no centering or max-width, the "Refresh"
button next to "Previously connected" did nothing when clicked, and Send
started with folder selection instead of letting the developer choose a
transfer mode first. This round is structural/functional, not a visual
redesign (that's round 21). **This is a frontend-only round — no Rust
files changed** — verified via `git diff main --stat -- '*.rs'
Cargo.toml Cargo.lock` returning empty, and rounds 1–19's existing
`cargo build --workspace` / test suites were re-run to confirm the
baseline still holds. JS changes were verified via `node --check` (no
syntax errors), a DOM-id cross-reference script (every `$("...")`/
`getElementById("...")` call in `app.js` resolves to a real id in
`index.html`, no duplicate ids), and an HTML tag-balance count — this
sandbox has no way to actually render the app or simulate clicks, so
none of this substitutes for a real click-through.

**A real discrepancy found while starting this round**: the build prompt
referenced a third transfer mode, "Cloud drop," from "round 19." A full
search of the codebase and entire git history (all branches/tags) found
no trace of either — neither exists. Raised directly; the developer
chose to proceed with only the two transfer modes that actually exist
(Local network, Remote relay) for this round's mode-selection step,
deferring Cloud drop to its own future round if and when it's actually
built. The wizard's first step therefore offers exactly two choices, not
three.

### What changed

- **Centered, bounded layout** (goal 1): all body content now sits
  inside a `.app-shell` container (max-width ~880px, centered, with
  consistent side padding); previously full-bleed elements (`.topbar`,
  `.settings-panel`) got compensating negative margins so they still
  read as full-bleed *within* the shell rather than being visibly
  inset. Applies to Send, Receive, and Settings alike since they all
  share the same shell.
- **Refresh button fixed** (goal 2): code review found this was *not* a
  missing- or broken-handler bug — the click handler and its backend
  command were both wired correctly and did re-fetch the roster. The
  real problem is a UX feedback gap: refreshing to the same (often
  empty) result looks identical to doing nothing. Fixed the same way
  round 16 fixed the identical class of problem for update checks
  (`runUpdateCheck(reportStatus)`): `refreshReceivers` now takes a
  `reportStatus` flag, showing "Refreshing…" then a real count (or "No
  receivers connected.") only when a person clicks the button — the
  three existing silent/automatic call sites (after accepting a pull
  request, on tab switch, after a successful send) are unchanged and
  stay quiet.
- **Full button audit** (goal 2): every `addEventListener` call in
  `app.js` was reviewed against every interactive control in
  `index.html`. The Refresh button above was the only genuinely broken
  one found; everything else was already correctly wired.
- **Transfer mode chosen first** (goal 3): the Send wizard's first step
  is now an explicit Local network / Remote relay choice (with the
  relay URL field appearing only when Remote relay is picked), backed
  by the same `localStorage` keys Settings' own toggle already used, so
  the two stay in sync without either depending on the other's DOM
  elements. Folder selection and the full round-17/22 database wizard
  proceed unchanged after this step, regardless of which mode was
  picked; only the transport step at the very end differs.
- **Guided pop-up wizard** (goal 4): folder selection through the full
  database wizard now renders inside a modal overlay (dimmed backdrop,
  `role="dialog" aria-modal="true"`, `max-height: 88vh` with internal
  scroll so the variable-length per-folder DB sub-flow never overflows
  the viewport) instead of flat inline page content. The header shows
  "Step X of N: <phase>" progress — the per-folder DB sub-flow's several
  fine-grained screens are grouped under one "Database setup" phase
  since its real screen count varies by folder/branch taken, so a fake
  precise step count isn't shown. A Cancel button and a Back button on
  every step (including a new Back on the folder-selection step, back to
  mode choice) let the developer leave or step back at any point. The
  modal closes automatically once a send actually succeeds, so the room
  code renders on the main Send tab exactly where it always has.
- **A real gap found and fixed while integrating goal 4**: closing the
  modal only on success meant a failed send (from `start_send_session`
  or `share_snapshot_wizard`) left the modal open while the existing
  error message was written only to `#send-error`, an element on the
  main Send tab page — invisible behind the modal's backdrop at exactly
  the moment it mattered. Added a dedicated `#wiz-send-error` element
  inside the wizard's final step and the `catch` block now writes the
  error to both elements unconditionally (whichever one is actually
  visible depends on whether the modal has closed by that point; writing
  to both is simpler than branching and costs nothing since the hidden
  one is never seen).

### What to check on real hardware

1. **Layout, visually**: confirm the app actually looks centered and
   bounded (not just structurally correct in the DOM) at a range of
   window widths, including narrow ones — this sandbox has no display to
   render against.
2. **Every button, by clicking it**: this round's audit was done entirely
   through code review (no working synthetic input exists in this
   sandbox); do a real click-through of every button/toggle across Send,
   Receive, and Settings and confirm each does what it should, not only
   that the Refresh fix above works.
3. **Refresh, specifically**: click Refresh with zero receivers connected
   and confirm "No receivers connected." now appears (previously: visibly
   nothing happened); click it again with receivers connected and confirm
   the count updates.
4. **Mode-first flow**: start a new send and confirm the very first
   screen is the Local/Remote relay choice, before any folder picker
   appears; confirm choosing Remote relay reveals the URL field and
   blocks Next until it's filled in; confirm the choice persists (via
   Settings' toggle) across restarts.
5. **The modal wizard itself**: confirm it actually renders as a
   dimmed overlay on top of the app (not inline page content), that the
   step counter updates sensibly as you move through mode → folders →
   database setup → ready, that Back/Cancel/Next all work at every step,
   and that a long per-folder DB sub-flow (e.g. picking from a big table
   list) scrolls inside the modal rather than overflowing the window.
6. **The failed-send error, specifically**: trigger a real send failure
   while the wizard modal is still open (e.g. an invalid relay URL that
   passes client-side validation but fails at connect time) and confirm
   the error is now visible inside the modal, not silently swallowed
   behind it.

### What "success" looks like

The app looks and feels like one coherent, intentional piece of software
rather than an unstyled left-aligned document with a few dead buttons.
Every click does something visible, including a repeat click that finds
nothing new. Starting a send asks "how" before it asks "what," and the
whole folder-through-database-setup journey reads as one guided sequence
in its own space — including when it fails partway through.

## Round 21 addendum: iconography, theming, and modern visual design

Round 21 is a genuine visual redesign, not new functionality — a lightweight
icon system (real Lucide SVG source, vendored inline, not a CDN dependency
this offline desktop app can't rely on), a real light/dark theme system with
a persisted manual override, and a design-token pass (spacing, typography,
radius, elevation) applied across every screen including every wizard step
from rounds 17–20. **This sandbox has no display** — everything below is
verified through code review, real WCAG contrast-ratio computation for the
new color tokens, and the same DOM-id/tag-balance/syntax checks every prior
UI round has used, never by actually looking at the app. Whether it actually
looks good is explicitly the developer's own call — see "What to check on
real hardware" below.

### What changed

- **Icon system** (goal 1): 28 icons, real Lucide SVG source (ISC-licensed,
  https://lucide.dev, extracted via the published `lucide-static` npm
  package — not hand-drawn or guessed), vendored as inline `<symbol>` defs
  at the top of `index.html` rather than a cross-file sprite or CDN
  script. Inline was a deliberate choice over a separate `icons.svg` file:
  a same-document `<use href="#icon-x">` behaves identically across
  WebView2 (Windows), WebKitGTK (Linux), and WKWebView (macOS), where a
  cross-file `<use>` pointing at an external SVG is a known source of
  inconsistent behavior across exactly that engine spread — and this is
  an offline desktop app, so a CDN-hosted icon font was never on the
  table. Every icon uses `stroke="currentColor"`, so it always matches
  its surrounding text color with zero icon-specific color rules, in
  either theme. Applied to every button across Send, Receive, Settings,
  and all wizard steps, plus the peer-recognized/peer-new status banners
  (previously a bare ✓/? text character) — kept as icon+label everywhere
  except a small number of genuinely self-explanatory case (e.g. the
  "These details are wrong — edit manually" link), per the round's own
  instruction to prefer clarity over icon-only minimalism.
- **Light and dark themes** (goal 2): every color in the stylesheet is
  now a CSS custom property (verified: the only remaining literal hex
  values in `styles.css` are the token definitions themselves, `#fff` for
  fixed white badge/icon-circle text, and the run log's deliberately
  theme-independent terminal colors — unchanged from before this round,
  and called out in its own comment). A new Settings control offers three
  real states — System / Light / Dark, not a single on/off toggle —
  persisted to `localStorage` and applied synchronously by a small inline
  script in `index.html`'s `<head>` (before `app.js` itself loads, at the
  end of `<body>`) so an explicit override never flashes the OS's theme
  for one frame first. The dark palette is not the light one inverted:
  four colors (the accent/danger/added/modified tones, used as plain text
  read directly against the page background — links, result/error
  messages, diff insertion/deletion coloring) get their own dark-specific
  values, each checked to really reach WCAG AA's 4.5:1 contrast ratio
  against the real dark background by computing it, not eyeballing it;
  the same four colors used as background fills under fixed white text
  (badges, the peer-status icon circles) are deliberately left unchanged
  between themes, since that usage already had known-good contrast in
  both.
- **A real design-token system** (goal 3): a spacing scale
  (`--space-1`…`--space-7`, 4px-based), a typography scale (`--font-size-
  xs`…`--font-size-xl`, `--weight-regular`…`--weight-bold`), radius
  tokens, and elevation tokens (`--shadow-sm/md/lg`, themed separately —
  dark mode's shadows are darker/more opaque, since a light-mode shadow
  value reads as almost invisible against a dark background). Applied
  throughout the entire stylesheet, not just new rules — every screen,
  including every step of the rounds 17–20 wizard (transfer-mode choice,
  folder selection, the full per-folder database sub-flow, the final
  summary). A few real, previously-missing pieces of visual consistency
  were also fixed along the way: `<select>` and `input[type=number]`/
  `input[type=password]` elements had **no styling at all** before this
  round (native browser appearance, inconsistent with the styled
  `input[type=text]` fields right next to them) — now share the same
  rule. A visible focus ring (`:focus-visible`, using the same
  theme-aware `--focus-ring` token) was added for every interactive
  element, since a real desktop app gets used with the keyboard and the
  three WebViews' own default focus indicators don't look or behave the
  same way. Hover states were added to every button variant and to the
  wizard's table/schema list rows, all previously static.

### What to check on real hardware

1. **Icons, at a glance**: confirm the 28 icons actually render (not
   broken `<use>` references — code-verified that every reference
   resolves to a real symbol, but only a real render confirms the SVGs
   themselves paint correctly) and that each one reads clearly at its
   small on-screen size paired with its label.
2. **Both themes, on every screen**: switch System → Light → Dark → System
   again in Settings and confirm each actually applies immediately, with
   no flash of the wrong theme on a fresh launch after setting an explicit
   override. Check contrast specifically on: result/error text, the diff
   insertion/deletion coloring, and links (`ports-list` addresses, the
   "Show details" toggle) — these are the values this round tuned
   specifically for dark-mode legibility and are worth a real look, not
   just the badges/buttons that were left unchanged on purpose.
3. **Every wizard step, in both themes**: step through the full send
   wizard (mode → folders → needs-db → shared-db → the per-folder
   database sub-flow → ready) in both Light and Dark, confirming spacing,
   borders, and the modal's own elevation (shadow) all read as one
   coherent design, not just the main Send/Receive tabs.
4. **Hover and focus states**: confirm buttons, table/schema rows, and
   tabs show a visible hover change, and that Tab-ing through the app
   (keyboard only, no mouse) shows a clear focus ring on whatever's
   focused at every step.
5. **Overall visual quality** — genuinely a call only a person looking at
   a real screen can make: does the icon set read as "modern and
   intentional" rather than random or mismatched; is the spacing rhythm
   actually comfortable; do both themes feel considered rather than one
   being an afterthought. This round implemented a real, internally
   consistent system — it does not, and cannot from this sandbox, verify
   that the result is genuinely good-looking.

### What "success" looks like

The app has a real icon vocabulary, a genuine light/dark theme a person
can pick and keep across restarts, and a visual language — spacing,
type, elevation — that's the same system on every screen instead of
whatever felt right when that screen was originally built. Whether it
actually *looks* good is a judgment call this sandbox is structurally
unable to make; that call belongs to the developer, on a real screen.

---

## Round 23 addendum: Cloud drop (Google Drive) — needs a real OAuth Client ID and two real Google accounts

This is the round with the widest gap between what's automated and what needs
your own hands: nothing here can exercise a real Google OAuth consent screen,
a real cross-account Drive permission grant, or (crucially) tell you which of
the two documented `expirationTime` behaviors your own accounts actually
produce. Everything that *could* be proven without those — PKCE against RFC
7636's own known vector, a real end-to-end OAuth loopback flow against a
simulated browser redirect, every Drive/token HTTP call against a real local
mock server, the retention decision table, and the reject-path control
protocol over real `ls_net` connections — already has 43 passing automated
tests (41 in `ls-clouddrop`, 2 in the new `cloud_drop_protocol_test.rs`); this
checklist is for the rest.

### Before you start: get a real Client ID and Client Secret

Follow `docs/google-drive-setup.md` start to finish first — a Google Cloud
project, the Drive API enabled, an OAuth consent screen in "Testing" status
with **two of your own Google accounts** added as test users (one to act as
sender, one as receiver), and a Desktop-app-type OAuth Client ID. Set both
`GOOGLE_OAUTH_CLIENT_ID` **and** `GOOGLE_OAUTH_CLIENT_SECRET` (round 31 -
see that round's own addendum below for why the secret is needed at all
despite this being a PKCE flow) to the two values from that Client ID's
Credentials page entry, before launching either app instance. Without
either one, Settings → **Link Google account** fails immediately with a
message naming exactly which variable is missing — confirm that's what you
see if you launch with one or both unset, as a sanity check that the
failure path itself is honest before you set the real values.

### What to check

1. **Linking, for real.** With `GOOGLE_OAUTH_CLIENT_ID` set, click **Link
   Google account** in Settings. Confirm your real default browser opens to
   a real Google consent screen listing the scopes from
   `docs/google-drive-setup.md` (with a "Google hasn't verified this app"
   warning — expected while in Testing status; click **Continue** since
   you're signed in as a test user). After approving, confirm the browser
   tab shows a plain confirmation page and Settings now shows your real
   linked email.
2. **A full send/receive/accept cycle, two real accounts, ideally two real
   machines.** On the sender: pick the **Cloud drop** radio on the Send
   wizard's first step (transfer mode), pick a retention option, click
   through to **Send** — confirm a room code appears and a real file lands
   in your Google Drive (check
   drive.google.com yourself; look for a "LocalSync Cloud Drop" folder).
   On the receiver (linked to your *second* test account): check **This is
   a Cloud drop code**, paste the code, click **Receive** — confirm the
   sender sees a real "Cloud drop access request" banner naming the
   receiver's actual email. Click **Grant access** — confirm the receiver's
   Receive tab proceeds straight into the normal diff-review screen
   (Run/Reject exactly as any other transport; nothing auto-runs).
3. **Reject, and confirm it independently — not just via the app.** Send
   again, this time click **Decline** on the incoming request. Then check
   **drive.google.com on the sender's account** (not the app) and confirm
   the uploaded file's Share dialog shows no one else has access at all —
   this independent, Drive-native confirmation is the actual point of this
   round's whole access-grant design, and the one thing no automated test
   in this sandbox could ever demonstrate.
4. **Retention, for real.** Send with "Delete once downloaded" and confirm
   (after the receiver downloads, then relaunching the sender app) the file
   disappears from Drive. Separately, send with "Delete after 24 hours" (or
   pick a near-future custom time to avoid an actual day's wait) and confirm
   it's gone after that time passes and the sender app has been relaunched
   at least once past it.
5. **The `expirationTime` question itself, if you have access to a Google
   Workspace account.** Grant access from a Workspace account with a
   non-"delete after download" retention set, then check that permission's
   real `expirationTime` field via the Drive API or a script — confirming
   whether it was actually honored. This is a genuinely open, two-way
   question this round's code was built to be correct either way for, not
   one it could resolve itself: report back whichever way it goes.

### What "success" looks like

A developer with a linked Google account can send a project to a specific
named colleague — not "anyone with the link" — who has to have their own
account explicitly approved before they can download anything, and who
lands in the exact same review-before-Run screen as any other transport.
Declining leaves a real, independently-checkable trace of "no access
granted" on Drive itself. And whichever retention option was chosen, the
file is actually gone from Drive by the time it's supposed to be — checked
on Drive directly, not just trusted from the app's own UI.

---

## Round 24 addendum: magic link (custom URL scheme + static fallback page)

Round 24 adds a real `https://` link on top of the existing room code: click it with LocalSync installed and it opens straight to a pre-filled Receive tab; click it without LocalSync installed and a static GitHub Pages site walks you through installing it, with your code ready to paste in afterward. **This is the round most dependent on real-hardware confirmation of any so far** — everything about whether a custom URL scheme actually reaches an installed app, or correctly falls back when it doesn't, is OS/browser behavior this sandbox cannot observe or simulate at all.

### What was code-verified (not real-hardware — see below for that)

- The Rust side builds cleanly with `tauri-plugin-deep-link`,
  `tauri-plugin-single-instance` (with its `deep-link` feature), and
  `tauri-plugin-clipboard-manager` all registered — confirmed via a real
  `cargo build -p localsync-desktop`, and `cargo tree -p localsync-desktop`
  was used to directly confirm the `deep-link` feature really pulled
  `tauri-plugin-deep-link` in as a dependency of
  `tauri-plugin-single-instance`, not just declared in Cargo.toml.
- `capabilities/default.json` was updated with the exact permissions each
  new plugin's own manifest requires (`deep-link:default`,
  `clipboard-manager:allow-write-text`) — found by reading each plugin's
  real `permissions/default.toml` from its downloaded crate source, not
  guessed; a missing permission here would fail silently at runtime
  (a rejected `invoke`), not at compile time, so this was checked
  deliberately rather than assumed to be unnecessary.
- The fallback page's real logic — `parseCode`, `detectOS`,
  `pickInstallerAssets`, and the exact `localsync://...` URL it builds —
  has 24 passing automated tests (`node --test
  web/test-magic-link-logic.js`, Node's own built-in test runner, no new
  dependency), covering real edge cases: Android's user agent containing
  the substring "Linux" (and this not being misread as a Linux desktop
  build to offer), a release with no build for a given OS, a missing/empty
  code parameter, and characters in a code that need percent-encoding for
  the URL to be valid.
- `.github/workflows/pages.yml` was validated with `actionlint` (zero
  findings, alongside every other workflow in this repo) and runs the same
  test file as a real deploy gate.
- The exact GitHub API field names used (`assets[].name`,
  `assets[].browser_download_url`) were confirmed against a real
  `GET /repos/.../releases/latest` response from a real public repo with
  real release assets, not assumed from memory of the API's shape.

### What to check on real hardware

1. **Install the app, then click a real magic link** (generate one from
   Send's new "Copy link" button, paste it into a browser on the same
   machine): confirm it opens LocalSync directly, switches to the Receive
   tab, and the code is already filled in — you should only need to click
   Receive, never retype anything.
2. **Click a second magic link while LocalSync is already running**
   (Windows and Linux specifically — this is the exact case
   `tauri-plugin-single-instance`'s `deep-link` feature exists for):
   confirm it brings the *existing* window to the front with the new
   code filled in, rather than opening a second LocalSync window/process.
3. **Click a magic link with LocalSync *not* installed**: confirm the
   browser lands on the static fallback page (once GitHub Pages is
   actually enabled and deployed — see below), that it shows your real
   code with a working copy button, and that it highlights the right
   installer for the machine you're using (then confirm the *other*
   OSes' downloads are still visible, just less prominent — a wrong OS
   guess should never hide the real option).
4. **Confirm the fallback page's own real download links work**: click
   through to an actual installer from the page and confirm the URL
   GitHub's API returned really downloads the file it claims to, for
   whichever OS you're testing on.
5. **The one-time GitHub Pages setup**: in this repo's Settings → Pages,
   confirm "Source" is set to "GitHub Actions" (see this round's README
   section for why this can't be automated), then confirm a push to
   `web/` (or a manual `workflow_dispatch` run of "Deploy magic-link page
   to GitHub Pages") actually publishes the page and that
   `https://decypher0.github.io/LocalSync/?code=test` loads.
6. **Copy code vs. Copy link**: after starting a send, confirm both new
   buttons work, copy genuinely different things (the bare code vs. the
   full `https://...?code=...` link), and that pasting each works where
   you'd expect (the bare code into LocalSync's own Receive field, the
   link into a browser or a chat message to someone else).

### What "success" looks like

Someone who already has LocalSync installed clicks a link and lands
straight in a pre-filled Receive tab — no copy-pasting a code by hand.
Someone who doesn't have it yet clicks the same kind of link, lands on a
real page that tells them what to download for their machine and holds
onto their code until they're ready to paste it in. Neither path needed
a backend, a database, or a hardcoded download URL that would go stale
the next time a release ships.

## Round 25 addendum: critical send/receive bugs + layout regression

Round 25 fixes three real, blocking bugs found in the first genuine
cross-machine test since rounds 20-24 landed: a bad IPC argument name
that broke every database-attached Send outright, a receive-side bug
that made the receiver's own unrelated Settings toggle block valid
codes, and an investigation into a reported layout regression that
turned out not to be reproducible as a source or plain-build bug.

### What changed, and why (root causes, not guesses)

- **Goal 1 - the `share_snapshot_wizard` crash**: `apps/desktop/src/app.js`
  was sending the wizard's dump payload with a `filePath` key (this
  app's own internal JS naming convention) instead of the `file_path`
  Rust's `DumpPlanDto` actually declares. Tauri's `invoke` bridge
  converts a command's own top-level argument names between camelCase
  and snake_case automatically, but does **not** do that recursively for
  nested struct fields - those deserialize via plain `serde_json` against
  the exact declared name. Confirmed via `git log -S"filePath"`: this
  exact bug has existed, unnoticed, since round 17 - every automated test
  calls `commands::share_snapshot_wizard` directly, bypassing this exact
  JS reconstruction step, so nothing ever exercised the real IPC
  boundary until an actual database-attached Send did. Fixed, and - since
  this is now the *second* real bug found at this exact translation step
  (round 22 found `engine` silently dropped here) - pulled the payload
  construction into its own file (`apps/desktop/src/wizard-payload.js`)
  specifically so it's testable from plain Node (`app.js` itself touches
  `window`/`document` from its first line, so it can't be `require()`d
  directly); `test-wizard-payload.js` has 7 passing tests, including one
  that directly asserts the outbound key is `file_path` and that the
  internal `filePath` key never leaks into the outbound object - proven
  to actually catch this exact class of regression by temporarily
  reintroducing the bug and confirming the test fails, then restoring the
  fix and confirming it passes again.
- **Goal 2 - receivers no longer pick a connection mode**: a receiver
  pasting a Local-network code was shown "Remote relay URL is required in
  Settings for Remote relay mode" whenever their own Settings mode toggle
  (meant for choosing a *sender*'s default, unrelated to what a given
  pasted code actually needs) happened to be on "Remote relay" - blocking
  a perfectly valid code before `decode_room_code` was ever even called.
  Root cause: `decode_room_code` took a caller-supplied `mode` argument
  that the frontend read from that same Settings toggle. Fixed at the
  source of truth: local-mode codes are always exactly 14 base62
  characters (the packed LAN-IP/port/room-id string), remote-mode codes
  are always exactly the bare 4-character id `generate_room_id()`
  produces - the two shapes never collide, so `decode_room_code` no
  longer takes a `mode` parameter at all, trying a local-shaped decode
  first and only falling back to treating the code as a remote-mode bare
  id (which does still need a separately-known relay URL - that's real,
  unavoidable information a short id can't encode on its own, not a mode
  pick) if that fails. `request_cloud_drop_access` (round 23) had the
  identical bug and got the identical fix. Proven with a new Rust test,
  `decode_room_code_tells_local_and_remote_codes_apart_with_no_mode_argument`,
  plus the existing `local_mode_still_produces_the_old_encoded_code` now
  explicitly passing `None` for `relay_url` - not just an empty string -
  to prove zero remote-relay configuration is required or consulted for
  a real local-mode code.
- **Goal 3 - the layout regression**: investigated directly rather than
  guessed at. Confirmed the *source* is correct - `.app-shell`, every
  design token, and all 30 icon `<symbol>`s from rounds 20/21 are present
  and correct in the current `main`. Then tested whether a plain local
  build actually re-embeds frontend changes, since Tauri bakes
  `apps/desktop/src/*` into the compiled binary via `generate_context!()`
  rather than serving it from a separate dist folder: three real,
  reproducible tests in this sandbox - editing only `styles.css` (a
  modified file) triggered a real recompile, adding a brand-new,
  previously-unreferenced file also triggered one, and a same-inputs
  rebuild afterward was a genuine no-op (ruling out "it always
  recompiles regardless" as a false explanation for the first two
  results). **This means the regression is not reproducible as either a
  source bug or a plain local-build bug.** The one remaining,
  unproven-but-plausible mechanism this sandbox can't fully exercise:
  the release pipeline's `Swatinem/rust-cache` restores a cached
  `target/` keyed on `Cargo.lock`/`Cargo.toml`, not on frontend content -
  a cache restore's file mtimes don't necessarily land in the same
  relative order a normal edit-then-rebuild would, a real (if unproven
  here) risk class for any mtime-sensitive staleness check. Hardened
  `build.rs` defensively against exactly that with an explicit
  `cargo:rerun-if-changed=../src` - costs nothing (the tests above already
  passed without it) and closes the one gap this investigation couldn't
  fully exercise outside a real CI run. The most likely real-world
  explanation for what was actually observed: a previously-installed
  build, or a `tauri dev` process left running from before rounds 20/21,
  being tested instead of a freshly rebuilt/reinstalled one - this
  project's dev mode has no live frontend reload (no `devUrl`
  configured), so a long-running dev session genuinely would keep
  showing whatever the frontend looked like when it was last started.

### What to check on real hardware

1. **Goal 1, for real**: attach a real database to a folder in the Send
   wizard (any of the three engines) and confirm Send actually completes
   instead of failing immediately with a `missing field 'file_path'`
   error - the exact crash reported.
2. **Goal 2, for real**: on the receiving machine, set Settings' own
   relay mode to "Remote relay" (with or without a URL filled in) and
   then paste a code from a sender who used **Local network** mode -
   confirm it connects successfully with no error about a relay URL,
   proving the receiver's own Settings no longer affects a Local-mode
   code at all. Then do the reverse - paste a genuine Remote-relay code
   with no relay URL configured anywhere - and confirm you now get a
   clear message asking for one, rather than either a wrong error or a
   silent failure.
3. **Goal 3, for real**: after pulling this round's changes, do a
   genuinely clean rebuild (quit any running LocalSync/`tauri dev`
   process first, then rebuild and reinstall/relaunch from scratch) and
   confirm the centered layout, icons, and theme system from rounds
   20/21 actually appear. If they still don't, that's real, valuable
   information this sandbox couldn't produce on its own - worth checking
   specifically whether it reproduces from a *freshly triggered* release
   pipeline run (not a cached one) versus a local build, to help isolate
   whether the `Swatinem/rust-cache` risk this round hardened against is
   the actual mechanism.

### What "success" looks like

Sending a database-attached project no longer crashes. Receiving a
Local-network code never asks about a Remote relay, regardless of
whatever the receiver's own Settings happen to say. And whatever's
actually served by a real install reflects the real, current source -
confirmed as far as this sandbox can reach, with a concrete next check
for the one part it couldn't fully verify itself.

## Round 26 addendum: real release publishing (magic-link + auto-update both depend on it)

Two apparently separate bugs — the magic-link page's "GitHub API returned
404" and the app's own "could not fetch a valid release JSON" — turned out
to share a root cause investigated and confirmed directly against this
repo's real GitHub state (via the public REST API), not assumed: this
project's only real Release existed with real assets, but GitHub's
`/releases/latest` alias structurally excludes prereleases, and every
release this workflow has ever produced via `workflow_dispatch` is
deliberately marked prerelease. The fix publishes a second, fixed `latest`
tag on every run that both consumers now address directly, sidestepping
that alias entirely — confirmed against GitHub's own REST API docs (not
guessed) that a tag-based lookup has no such exclusion. Separately, no
`latest.json` had ever been generated at all, because `TAURI_SIGNING_PRIVATE_KEY`
has never been added as a repository secret — a real, independent gap,
not fixed by the tag change above, and not fixable without the
maintainer's own action (see `docs/auto-update-signing.md`, new this
round).

### What's already confirmed, without needing to trigger anything

- The repo's real Releases were queried directly (`GET /repos/decypher0/LocalSync/releases`
  and `/releases/latest`) - confirmed the 404 and confirmed a real,
  non-draft release with real installer assets already existed, just
  marked `prerelease: true`, before writing any fix.
- `pickInstallerAssets`/`parseCode`/`detectOS`/`buildSchemeUrl` (the
  fallback page's pure logic) are unchanged and all 24 of their existing
  tests still pass - only the URL the page fetches from changed.
- The new `release.yml` step's YAML structure and the exact `make_latest`/
  tag-lookup semantics it relies on were confirmed against GitHub's own
  REST API documentation before being written, not assumed from memory.

### What to check once you actually trigger the release workflow

1. **Run the Release workflow** (Actions tab → Run workflow, or push a
   `v*` tag). Confirm the new "Publish/update the rolling 'latest' release"
   step succeeds, and that a release tagged exactly `latest` now exists on
   the Releases page (separate from the versioned one) with the same
   installer assets attached.
2. **Confirm the alias fix, for real**: `curl -s https://api.github.com/repos/decypher0/LocalSync/releases/tags/latest`
   should return a real release object (not 404) even though it's a
   `workflow_dispatch` run. `curl -sI https://github.com/decypher0/LocalSync/releases/download/latest/<some-asset-name>`
   should redirect to a real download, not 404.
3. **The magic-link page, for real**: open `https://decypher0.github.io/LocalSync/?code=test`
   (after re-deploying `web/`, if it hasn't picked up this round's `app.js`
   change yet) with LocalSync not installed, and confirm real OS-specific
   download links render instead of the "couldn't reach GitHub" fallback
   message.
4. **Auto-update, only after adding `TAURI_SIGNING_PRIVATE_KEY`** (see
   `docs/auto-update-signing.md` for exactly how): re-run the release
   workflow, confirm the "Generate latest.json" step now prints
   `generated=true`, and confirm `curl -s https://api.github.com/repos/decypher0/LocalSync/releases/tags/latest | jq '.assets[].name'`
   lists `latest.json`. Install an older build on real hardware and confirm
   Settings → **Check for updates** now reports a real update rather than
   an error - the actual install-and-restart click-through is still the
   same real-hardware-only gap round 16's own scope note already
   documented, unchanged by this round.

### What "success" looks like

Both consumers resolve a real, current release without needing anyone to
have ever pushed a real `vX.Y.Z` tag - a plain `workflow_dispatch` dev
build is enough for the magic-link page and (once the signing secret
exists) the in-app updater to both find real data, every single run, not
just the lucky first time someone tags a release.

---

## Round 27 addendum: magic-link on real Windows and real Linux

Real testing on real hardware found two distinct bugs the same round's
own investigation confirmed have two distinct root causes - not one fix.
Both are genuinely platform-behavioral, so both still need a real
click-through on real hardware to fully confirm; what's below is exactly
what this sandbox could and couldn't check on its own.

### What changed, and what's already confirmed without real hardware

- **Windows (false "not installed")**: the install-detection heuristic
  (`visibilitychange`/`blur` within a fixed timeout) is confirmed, via
  real research into how every implementation of this technique works
  across browsers, to have no fully reliable signal on any of them - this
  was never fixable by picking the "right" timeout number alone. Fixed
  two ways: the timeout itself moved from 1750ms to 2500ms (the most
  commonly referenced reference implementation for this exact technique
  defaults to 2000ms and documents raising it for a slower-starting app),
  and - the real fix - the page now keeps listening after the fallback UI
  renders and corrects its own message the moment a late success signal
  arrives, rather than leaving a confidently-wrong "doesn't seem to be
  installed" on screen next to an app that's visibly already open. The
  initial fallback message itself was also softened to acknowledge it can
  still be wrong at that exact moment, not assert a negative outright.
  `web/test-magic-link-logic.js`'s 24 existing tests (all pure-function,
  untouched by this change) still pass; this specific fix lives in
  browser/timer orchestration code this project's own test split has
  never covered with automation - see "What to check" below.
- **Linux (deep link doesn't reach the app at all)**: root-caused to a
  real, confirmed, currently-open upstream Tauri bug
  (`tauri-apps/tauri#16014`) - the bundler's default `.desktop` template's
  `Exec=` line has no `%u`/`%U` field code, so per the Desktop Entry
  Specification itself, a launcher invokes the app with *no arguments at
  all* when a link is clicked, regardless of whether the URL scheme is
  otherwise correctly registered. A custom `.desktop` template
  (`apps/desktop/src-tauri/linux/main.desktop`, wired via
  `bundle.linux.deb.desktopTemplate`) adds `%u` and a hardcoded
  `MimeType=x-scheme-handler/localsync;` line (confirmed the default
  template's own `mime_type` template variable isn't populated from this
  project's deep-link scheme config - only from an unrelated
  file-association feature this project doesn't use). New
  `postInstallScript`/`postRemoveScript` entries run
  `update-desktop-database` after install/removal, so the OS's cached
  MIME index actually picks up the change immediately rather than waiting
  on an unrelated future trigger (another package install, a reboot, ...).
  All three new files were validated for real in this sandbox, not just
  reviewed: a real `.deb` was actually built in this project's own WSL2
  environment (`npm run tauri build -- --bundles deb`) - which itself
  caught a real bug before this round shipped it (the header comment
  originally described the default template's own Handlebars syntax
  using literal double-brace mustaches, which the bundler's template
  engine tried to parse as real template code and failed on; fixed by
  rewording the comment, confirmed by a second successful build). The
  built `.deb`'s contents were then extracted (`dpkg-deb -e`/`-x`) and
  inspected directly: the rendered `.desktop` file shows the real
  `Exec=localsync-desktop %u` and `MimeType=x-scheme-handler/localsync;`
  lines exactly as intended, and both `postinst`/`postrm` scripts are
  present with real `rwxr-xr-x` executable permissions and clean LF-only
  line endings (`file` reports plain "POSIX shell script, ASCII text
  executable" - no CRLF flag). The `.gitattributes` addition this round
  is what makes that last part true: a CRLF-corrupted shebang line
  silently breaks a maintainer script's execution on Linux, and this was
  confirmed to be a real risk on this project's own Windows-checked-out
  clone before the fix (git flagged the exact line-ending conversion on
  first `git add`), not a hypothetical one. Not run in this sandbox (no
  passwordless `sudo` available to install it): `desktop-file-validate`
  itself, for an independent, spec-conformance-focused second opinion
  beyond a successful real build and manual inspection - worth running
  once on a real machine (`desktop-file-validate LocalSync.desktop` after
  extracting it) alongside the real-hardware checks below.

### What to check on real hardware

1. **Windows, the actual contradiction**: click a real magic link with
   LocalSync already installed. Confirm the browser tab's message never
   ends up asserting "doesn't seem to be installed" while the app is
   simultaneously visibly open on screen - if the app opens slower than
   2500ms, confirm the page instead shows "Looks like LocalSync just
   opened" once it catches up, rather than staying wrong indefinitely.
2. **Windows, genuinely not installed**: click a magic link on a machine
   that has never had LocalSync, and confirm the softened message
   ("If LocalSync just opened, you're all set — otherwise...") still
   reads clearly and the real download options still render below it.
3. **Linux, the actual deep link**: install the `.deb` on a real Linux
   desktop (GNOME and KDE both worth trying, if you have access to both -
   MIME/URL-scheme handling is desktop-environment-specific in practice
   even though the underlying mechanism is a shared freedesktop.org
   standard), then click a real magic link. Confirm LocalSync actually
   launches (or focuses, if already running - see round 24's
   single-instance handling) with the Receive tab pre-filled with the
   real code, not the default Send screen.
4. **Linux, the registration itself, independently of clicking a link**:
   after installing the `.deb`, run `xdg-mime query default
   x-scheme-handler/localsync` (or check a browser's own "always open
   these types of links" settings) and confirm LocalSync is listed as the
   real, registered handler - this is the direct, independent
   confirmation that installation itself (not just a lucky click) did the
   right thing.
5. **Linux, uninstall**: remove the `.deb` and confirm
   `xdg-mime query default x-scheme-handler/localsync` no longer points
   at LocalSync (or reports nothing) - proving `postRemoveScript` actually
   ran and the stale registration didn't linger.

### What "success" looks like

On Windows, the fallback page never contradicts what the person is
actually looking at - it's either right the first time, or corrects
itself the moment it learns better, and never confidently claims "not
installed" when the app is open. On Linux, clicking a magic link does
exactly what it already does on Windows and macOS: launches or focuses
the app with the real code already in the Receive tab, no manual copy-paste
required - and installing/removing the package is what actually turns
that capability on and off, not a step someone has to discover and run
by hand.

## Round 30 addendum: root-cause "snapshot payload has no docker-compose.yml"

Round 30 root-caused a real Run-time crash from real cross-machine testing (Windows → Linux, Local network, 168MB payload, database wizard used successfully, failed on Run with `snapshot payload has no docker-compose.yml`) - and confirmed it as a genuine packaging/unpacking mismatch, not a project that actually lacked a compose file.

### What changed, and why (root cause confirmed, not guessed)

- **The actual bug**: `ls_snapshot::create_snapshot_multi` (used by
  `share_snapshot_wizard` - the only Send path since round 20, for every
  send, even a single folder) nests every folder's files under its own
  `manifest.folders[i].name` label unconditionally - there was never a
  special case for exactly one folder. `ls_containers::run_snapshot`,
  meanwhile, always looked for `docker-compose.yml` at the unpacked
  payload's own top level. The result: **any** wizard-based single-folder
  send with a real, perfectly correct `docker-compose.yml` failed this
  way - the file was never missing, just nested one directory deeper than
  `run_snapshot` ever looked.
- **Why nothing caught this until now**: the only two tests that ever
  call `ls_containers::run_snapshot` at all
  (`pipeline_test.rs`/`pipeline_node_test.rs`) use the older, pre-round-17
  single-folder `ls_snapshot::create_snapshot`, which never nests
  anything - they never exercised the wizard's own packaging path all the
  way through to a real Run. A new test,
  `a_single_folder_wizard_send_can_actually_be_run` in
  `wizard_send_flow_test.rs`, closes that gap: it drives the exact real
  path a click-through does (`share_snapshot_wizard` ->
  `receive_snapshot` -> `run_snapshot` -> `stop_session`) with one folder
  and a real, minimal `docker-compose.yml` (busybox's own httpd, not a
  real app image - fast, no slow Maven/MySQL pull needed since this
  test's job is proving the file survives packaging, not re-proving a
  real app build; nginx:alpine was tried first and rejected for a real
  reason - it doesn't tolerate this project's own sandbox policy,
  `read_only` rootfs with only `/tmp` mounted writable, without its own
  cache directories - confirmed by directly reproducing the exact
  failure with plain `podman-compose` outside the test entirely). Verified this
  test actually has teeth: temporarily reverted the fix and confirmed the
  test fails with the *exact* reported error string, then restored the
  fix and confirmed it passes again.
- **The fix**: `run_snapshot` now checks `manifest.folders` - if it holds
  exactly one entry (a round-17+ wizard send with one folder), the real
  compose root is `<unpacked payload>/<that folder's label>`, not the
  payload's own top level; a multi-folder manifest (2+, or the older
  empty-`folders` single-folder path) keeps the exact previous behavior
  unchanged. `RunningSession.compose_dir` (used by `stop_session` too) now
  consistently holds this same, correct directory, so teardown looks in
  the same place `up` did.
- **The error message, for the case that's still genuinely possible**: a
  project with truly no `docker-compose.yml` at all (a real, separate,
  already-known-and-deferred gap - see `docs/auto-containerization.md`)
  now gets a clear, actionable message instead of a bare internal-looking
  string: it names the actual problem and the concrete next step (add a
  `docker-compose.yml`, then send again), rather than reading like an
  internal error a developer would have to guess the meaning of.
- **Deferred, on purpose, per this round's own hard budget rule**:
  auto-generating a Dockerfile/compose file for a project that was never
  containerized at all is real, separate scope - `docs/auto-
  containerization.md` describes what that capability would need to do
  and why it deserves its own dedicated round, without building any of it
  here.

### What to check on real hardware

1. **The actual reported scenario, for real**: send a real, single-folder
   project that has its own working `docker-compose.yml` through the
   wizard (with or without the database wizard also being used) and
   confirm Run now succeeds - this exact combination is what failed
   before this round's fix.
2. **A genuinely uncontainerized project**: send a real folder with no
   `docker-compose.yml` at all and confirm Run now shows the new, clear,
   actionable message (naming the real problem and suggesting adding a
   compose file) rather than the old bare "snapshot payload has no
   docker-compose.yml" string.
3. **Multi-folder sends are unaffected**: send 2+ independent folders
   through the wizard (round 17's own original multi-folder case) and
   confirm the review/diff experience is unchanged - this round
   deliberately did not attempt to make a genuinely multi-folder send
   runnable, only fixed the single-folder case.

### What "success" looks like

A project with a real `docker-compose.yml`, sent through the one Send
flow this app actually has today, runs on the receiver's machine -
exactly as a developer sending their own real project would expect,
regardless of whether they also used the database wizard. A project with
no compose file at all gets told clearly what's missing and what to do
about it, instead of a string that reads like something broke inside
LocalSync itself. And the real, separate work of auto-generating a
compose setup for an uncontainerized project is written down clearly
enough to pick up later, not rediscovered from scratch.

## Round 31 addendum: OAuth token exchange now needs a client secret too

Real testing against a live, correctly-configured Desktop-app OAuth client
found round 23's "PKCE means no client secret needed" claim wrong: Google's
real token endpoint rejected the exchange outright with `400 Bad Request:
invalid_request - client_secret is missing`. Investigated before fixing -
confirmed against Google's own docs and multiple independent real-world
reports of the identical error against Google specifically (not a fluke of
one misconfigured client) - see `crates/ls-clouddrop/src/oauth.rs`'s own
module doc comment and `docs/google-drive-setup.md` for the full
explanation. `GOOGLE_OAUTH_CLIENT_SECRET` is now a second required
environment variable alongside `GOOGLE_OAUTH_CLIENT_ID` - see this
checklist's own round 23 addendum above, updated to mention both.

### What's already confirmed, without needing real hardware

- All 40 of `ls-clouddrop`'s existing tests still pass with the new
  required `client_secret` field threaded through every `OAuthConfig`
  construction site.
- Two existing wiremock-backed tests - one for the authorization-code
  exchange (`run_oauth_flow_completes_end_to_end_against_a_simulated_browser_redirect`),
  one for the refresh exchange (`refresh_access_token_sends_expected_request_and_parses_response`) -
  were strengthened with an explicit `client_secret=...` body assertion:
  if the real request built by this code ever omitted it, the mock
  wouldn't match and these tests would fail with a connection/response
  error, not silently pass.
- `cargo build --workspace` and `cargo test --workspace` both still pass
  in full after this change - no other crate was touched.

### What to check on real hardware

1. **The actual failure this round fixes**: with only `GOOGLE_OAUTH_CLIENT_ID`
   set (not the secret), confirm Settings → **Link Google account** fails
   fast with a clear message naming `GOOGLE_OAUTH_CLIENT_SECRET`
   specifically, rather than opening a browser toward a request Google
   would reject anyway.
2. **The real fix**: set both `GOOGLE_OAUTH_CLIENT_ID` and
   `GOOGLE_OAUTH_CLIENT_SECRET` (see `docs/google-drive-setup.md`) and
   confirm the full link flow this checklist's round 23 addendum already
   describes now completes successfully against a real Desktop-app OAuth
   client - the exact scenario that failed before this round's fix.
3. **Refresh, specifically**: if you can wait for (or force) a stored
   token to near its expiry, confirm `ensure_valid_access_token`'s real
   refresh call against Google's live endpoint succeeds too, not just the
   initial exchange - the fix applies to both, but only the initial
   exchange was the one real testing actually hit first.

### What "success" looks like

Linking a Google account works end to end against a real Desktop-app OAuth
client without any client-secret-related error - the exact failure this
round exists to fix - and continues working across a real token refresh,
not just the first exchange.

## Round 28+29 addendum: concurrent multi-session fix, tabbed layout, app menu, session history, per-session details

Real testing found that starting a second, different send while one send
was already active didn't work. Investigated before fixing, not assumed: a
new test (`tests/concurrent_multi_session_test.rs`) spawns two genuinely
concurrent `share_snapshot`/`receive_snapshot` pairs via `tokio::spawn`
*before* awaiting either, then `tokio::join!`s all four - proving the
Rust/`AppState`/`ls-net` layer (round 11's multi-receiver sessions) already
supported this correctly. The real bug was 100% in `app.js`'s frontend
state: a small set of bare module-level variables (`currentRoomCode`,
`unlistenSendProgress`, `currentSnapshotId`, ...) and shared DOM elements
that a second send/receive simply overwrote, silently losing the first
session's own display even though its backend transfer kept running
untouched underneath. Fixed by giving every session (a send in flight, or a
receive in flight/held/running) a real object in a `sessions` Map, and
adding a `session_id` field to the `share-progress`/`receive-progress`/
`run-progress` events (previously unidentified, which would have made even
a correct frontend model unable to tell concurrent sessions' events apart).
On top of that fix: a tab per active session (terminal-multiplexer style), a
real application menu (Check for updates / Settings / Theme / Session
history), a session-history panel persisted via the OS app-data directory
(survives restart, unlike the session tabs themselves), and a per-session
details popover showing connected people / folders / database info for
whichever session is selected.

Two narrower, pre-existing limitations were deliberately left as-is this
round, since the reported bug and this round's own proof test are both
specifically about the *send* side: `AppState.outgoing_conn` (the
receiver's single "ask for update" pull-request target) is still a
singleton, and `ls_containers::ProvisioningLog` is still one shared log
file across all Run attempts. Both are called out in `commands.rs` and
`main.rs` comments as known, out-of-scope boundaries, not silently
unaddressed gaps.

### What's already confirmed, without needing real hardware

- `two_concurrent_sends_different_projects_different_receivers` (new)
  passes: two different projects, sent concurrently to two different
  receivers on the same box, each receiver gets the correct project's
  manifest (never swapped), and `list_connected_receivers` correctly
  attributes both roster entries.
- `cargo build --workspace` and `cargo test --workspace` both pass in
  full (WSL2 - the established authoritative environment for this crate's
  tests, since its test binaries crash on native Windows for an unrelated,
  pre-existing reason - see this checklist's own round 26 notes).
- `session_history.rs`'s 5 unit tests pass: missing-file-is-empty,
  upsert-adds, upsert-with-same-id-replaces-not-duplicates,
  different-ids-both-persist, and the 200-entry cap correctly drops the
  oldest.
- `web/test-magic-link-logic.js` (24 tests) and
  `apps/desktop/src/test-wizard-payload.js` (7 tests) both still pass
  unmodified - this round's frontend changes didn't touch either's own
  pure-logic code path.
- `index.html`'s restructuring (moving the send/review/run panels into the
  new shared session-detail viewport) was checked for balanced, correctly
  nested tags with a real HTML parser, not just visual inspection.

### What to check on real hardware

1. **The actual reported scenario, for real**: start a send, then - while
   its room code/progress is still showing - start a second, different
   send (a different project, to a different or the same receiving
   machine). Confirm both now genuinely run side by side, each in its own
   tab, with correct independent progress/room codes - this exact scenario
   is what failed before this round's fix.
2. **Tabbed layout, generally**: with 2-3 sessions active at once (a mix of
   sends and receives), confirm switching tabs shows each session's own
   correct state (room code, progress, review/run screen) with nothing
   bleeding from a different tab - including mid-transfer and mid-Run tab
   switches, and closing a finished session's tab.
3. **Application menu**: confirm all four items work - Check for updates
   (opens Settings and runs the same check the button does), Settings
   (opens the existing panel), Theme → System/Light/Dark (matches the
   Settings radios exactly, including which one is already selected), and
   View → Session history.
4. **Session history persistence**: run a few sends/receives, open Session
   history from the menu and confirm they're listed, then fully quit and
   relaunch the app and confirm the same entries are still there - this is
   the round's own explicit requirement that this not be an in-memory-only
   list.
5. **Per-session details popover**: for an active multi-receiver send,
   confirm the popover lists every currently-connected person, the
   folder(s) involved, and (if a database wizard was used) the engine and
   schema name(s) for that specific session - and that a different
   session's popover shows its own, different data.
6. **Visual/interaction polish, generally**: this round's UI was built with
   round 21's existing design tokens, but real layout/spacing/interaction
   quality (tab overflow with many sessions, popover positioning near
   window edges, etc.) needs a developer's own hands-on look - it was not
   validated in a real browser/webview this round.

### What "success" looks like

Two or more independent sends and/or receives - different projects,
different peers, mixed send/receive - genuinely run at the same time, each
visible in its own tab with correct, non-bleeding state, backed by a passing
same-box concurrency test rather than an assumption. The application menu,
theme switching, session history, and per-session details all work as
real, functioning features on top of that confirmed backend capability, not
a cosmetic layer sitting on an unconfirmed one.

## Round 33 addendum: AppImage download flakiness + missing macOS updater signature

Round 33 fixes two separate, real release-pipeline problems - a flaky
external download that failed two real release runs in a row, and a
macOS updater signature that never materialized despite the signing key
being confirmed present. This entire round is CI/workflow logic - no
application code changed, so rounds 1-25's own tests are unaffected by
construction, not just unmodified. Both fixes needed a real `workflow_dispatch`
run to fully confirm (this sandbox has no `gh`/token access to trigger
one or pull raw job logs - see below for exactly what that means for
what's verified here).

### What changed, and why (root cause confirmed against real source, not guessed)

- **The macOS `.sig` bug (goal 4) - the real find**: `--bundles dmg`
  (the flag `build-macos-dmg/action.yml` has always built with) never
  actually attempts the updater bundle at all, regardless of
  `createUpdaterArtifacts` or whether a real signing key is present -
  confirmed directly against `tauri-bundler`'s own source
  (`src/bundle.rs`): the updater step only runs when the *requested*
  bundle targets include `PackageType::MacOsBundle` (CLI short name
  `"app"`) specifically, not `Dmg` - even though `dmg` already builds an
  `.app` as its own internal dependency. The bundler's own log line for
  this exact misconfiguration even names the fix directly: "The bundler
  was configured to create updater artifacts but no updater-enabled
  targets were built. Please enable one of these targets: app, appimage,
  msi, nsis." Neither of this round's own two suspected causes (a stale
  path assumption in the locate step, or a missing Apple code-signing
  certificate) was the actual issue - the locate step's path pattern was
  already correct (confirmed against `updater_bundle.rs`'s own output
  path construction), and code-signing is entirely unrelated to whether
  the updater archive gets built at all.
- **The fix**: `--bundles dmg` -> `--bundles dmg,app` in the one shared
  composite action both `release.yml` and `macos.yml` already use for
  this - costs nothing extra to build (the `.app` was already being built
  as `dmg`'s own dependency either way; the only change is Tauri now also
  counts it as an explicitly-requested target instead of deleting it
  afterward). Because `macos.yml` never previously exercised the updater
  step at all (this exact bug made it a no-op there too, harmlessly, this
  whole time), it never needed its own "is a signing key present" check -
  now that the updater step is a real possibility again, that workflow
  gained the same check `release.yml`'s three build jobs already use, so
  its own (currently always-absent) key stays a clean skip instead of a
  new hard failure.
- **The AppImage download flake (goals 1-3)**: confirmed directly against
  `tauri-bundler`'s own source
  (`src/bundle/linux/appimage/linuxdeploy.rs`) that the AppRun/linuxdeploy
  download is a single, one-shot HTTP GET with no retry or timeout
  configuration of its own, and that it caches into
  `dirs::cache_dir()/tauri` - confirmed against that function's own Linux
  implementation to resolve to `~/.cache/tauri` on GitHub's runners
  (`$XDG_CACHE_HOME` isn't set there) - skipping the download entirely
  whenever a file is already there. Added: a `actions/cache` step for
  that exact directory, keyed on the real, pinned `@tauri-apps/cli`
  version read from the checked-in `package-lock.json` (not guessed) so a
  future CLI bump can't silently reuse stale tools from a different
  bundler version; a retry-with-backoff loop (3 attempts) around the
  build step for the cache-miss case, verified with a real, isolated
  script simulating a transient failure (succeeds on retry), a persistent
  failure (exhausts retries), and the happy path (no retries needed) -
  all three behaved exactly as intended; and a fallback to a `.deb`-only
  build if every attempt still fails, rather than failing the whole job -
  matching how this project already treats macOS's own unsigned/
  non-notarized case as a real, degraded release rather than no release
  at all. The locate step's own hard-fail-if-missing check now applies
  only to the `.deb` (confirmed, from the two real recent failures, to
  succeed independently of the AppImage step every time) - a missing
  AppImage is logged and skipped, not a new job failure.

### What's verified here vs. what needs a real trigger

Everything above is confirmed against Tauri's own real bundler source
code (not assumed from documentation or the round's own hypotheses) and
against the workflow files' own real logic - `actionlint` is clean on
every workflow file, and the retry/fallback control flow was tested with
a real, isolated shell script standing in for the three scenarios that
matter (transient failure, persistent failure, happy path). What this
sandbox could not do: pull the actual raw job logs from the two real
failed runs (no `gh` CLI or GitHub token available here - the round's own
quoted log details, exact filenames and the exact HTTP 504, were taken as
real, reliable evidence of what those runs actually hit, and cross-checked
against Tauri's source rather than re-derived from scratch), or trigger a
real `workflow_dispatch` run to watch the fixes work end to end - that's
this round's one remaining, real-hardware-equivalent gap.

### What to check on real hardware (a real `workflow_dispatch` run)

1. **The AppImage cache, across two runs**: trigger the Release workflow
   twice in a row. The first run's "Cache Tauri's AppImage helper
   binaries" step should report a cache miss (or hit, if a prior run
   already populated it); the second run should show a cache hit, and its
   own "Build the .deb and AppImage" step's log should show no
   `github.com/tauri-apps/binary-releases` download at all - `linuxdeploy`
   already existing (skipped) confirms the caching actually works, not
   just that the job succeeded.
2. **The retry+fallback path, if you can force it**: harder to trigger on
   demand (it needs the real GitHub outage to still be happening) - if a
   run does hit the AppImage step failing, confirm the log shows the
   retry attempts and backoff, and that the job still succeeds overall
   with a `.deb`-only Linux release rather than failing outright.
3. **The macOS `.sig`, for real**: with `TAURI_SIGNING_PRIVATE_KEY`
   configured as a repository secret, trigger a real release run and
   confirm the macOS job's "Locate the updater .sig (if produced)" step
   now actually finds both a `.app.tar.gz` and a `.app.tar.gz.sig` -
   previously it only ever found "(none)" for both, regardless of the key.
4. **`latest.json`, finally**: once all three platforms (Windows, Linux,
   macOS) produce a real `.sig` in the same run, confirm the `release`
   job's "Generate latest.json" step reports `generated=true` and the
   resulting file actually lists all three platforms
   (`windows-x86_64`/`linux-x86_64`/`darwin-aarch64`) with real signatures
   - this has never actually happened in a real run before this round,
   per the macOS side of it never having a signature to include.
5. **`macos.yml` still runs clean without a key**: since it now has a
   real (if currently always-failing-the-presence-check) path through the
   updater logic for the first time, trigger it once and confirm it still
   completes successfully end-to-end with no signing key configured -
   exactly as it always has, just via a path that's now actually being
   exercised instead of silently skipped by the old `--bundles dmg` bug.

## Round 34 addendum: Local network Cloud-drop connection hang, duplicate send tabs, Cloud drop moved to Step 1

Real testing on an actual LAN found "Local network" mode's room code
expiring with the receiver never connecting - the exact "does the core
value proposition even work" bug this round treated as the top priority.
Root-caused directly against the code (not assumed): rounds 25
(`decode_room_code`) and 28 (multi-session state) were both confirmed
correct via new tests that exercise the real, previously-untested
production path (a real detected LAN IP, not `127.0.0.1` - every prior
test explicitly substituted that). The actual bug found and fixed is in
**Cloud drop's own use of Local-network signaling**:
`start_cloud_drop_session` connected using `room_code` (the 14-character
display string a human pastes) instead of `room_id` (the 4-character
value a real receiver's `decode_room_code` actually extracts and connects
with) - the two only happen to be equal in "remote" mode, which is why
this was invisible everywhere except local mode. A sender joined a relay
room no receiver could ever reach, and sat waiting until the full 300s
timeout elapsed - exactly the reported symptom.

Honest caveat: the plain (non-Cloud-drop) Send flow was independently
verified correct with a real, passing, real-LAN-IP test
(`local_relay_mode_test.rs`) - if a real tester hits this same symptom
*without* ever touching Cloud drop, that would mean a second, still-
unfound bug; flag it immediately if so.

Also this round: a failed/expired send tab no longer spawns a duplicate
tab on retry (a real in-place "Retry" action was added, reusing the
already-packaged snapshot), and Cloud drop moved from a Step 4 checkbox
to a genuine third Step 1 transfer-mode radio (confirmed via git history
that the round 32 that was originally supposed to do this never actually
happened - no such commit exists anywhere in this repo).

### What to check on real hardware

1. **The actual Local-network fix**: two real machines (or two real
   processes on one machine, each pointed at the other's real LAN IP -
   not loopback), sender picks Local network + Cloud drop, generates a
   code, confirm the receiver actually connects and the upload completes
   well before the code's countdown reaches zero.
2. **Plain Local-network send, for real** (this is the case the code
   review couldn't fully rule out as the tester's exact original path):
   sender picks Local network (no Cloud drop), confirm a full send/receive
   cycle completes on a real LAN between two real machines, not just this
   sandbox's same-box test.
3. **Retry in place**: start a send, let its code expire (or force a
   failure), confirm the same tab turns "Expired" (not silently still
   looking active) rather than a second tab appearing, click **Retry**,
   confirm it gets a new code without re-visiting folder selection or the
   database wizard, and that the retry actually completes a real transfer.
4. **Cloud drop at Step 1**: confirm the wizard's first step now shows
   Local network / Remote relay / Cloud drop as three real radio options,
   selecting Cloud drop shows the linked-account status and retention
   picker right there, and Step 4 no longer has any Cloud-drop UI.
5. **Round 28/29 presence**: confirmed directly from git history, not
   just assumed - `git log --oneline main` shows "Round 28+29" merged via
   PR #12, already part of `main` before this round started.

## Round 35 addendum: dump-file out-of-memory fix + auto-update investigation

Real testing with an actual project's real (large) database dump hit
`reading dump file ...: out of memory`. Root-caused to the exact line the
error text pointed at (`apps/desktop/src-tauri/src/commands.rs`'s
`share_snapshot_wizard`, `std::fs::read(&dump.file_path)`) - but the real
problem was worse than one full-buffer read: `create_snapshot_multi`
(`crates/ls-snapshot/src/lib.rs`) then `.clone()`d that same buffer again
before hashing and tar-appending it, meaning a real dump could have two
full copies of itself alive in memory simultaneously, on top of whatever
the OS itself needed to service the read.

**The fix**: `PendingDump.dump_bytes: Vec<u8>` became `PendingDump.source:
DumpSource` (`Bytes(Vec<u8>)` for already-in-memory content, `FilePath
(PathBuf)` for a dump still on disk). The command layer no longer reads
the file at all - it just stats the path (failing fast on a bad one) and
hands `create_snapshot_multi` a `FilePath`. Hashing (`sha256_hex_of_file`)
and tar-appending (`append_dump_source`) both stream the file in bounded
64KB/256KB chunks instead of materializing it whole. Proven with a real
synthetic-dump test measuring actual peak RSS (`VmHWM`): a 500MB dump
streamed via `FilePath` peaks at ~5.8MB RSS, versus ~505MB for the same
file forced through the old `Bytes`-shaped path - i.e. genuinely bounded,
not just smaller. Round 34's retry-in-place feature does still re-read
(and re-hash) the dump file from scratch on every retry - deliberately
left as-is, since a single streamed pass is now cheap/bounded rather than
something that risks OOM or meaningfully slows a retry down.

**Auto-update**: reported as "not working" with no specifics. Investigated
from scratch rather than assuming it was a repeat of round 33's
now-fixed macOS-signature gap - confirmed the update-check/install code,
Tauri config, and plugin wiring are all correct, and that the real,
currently-published `latest.json` genuinely has valid signatures for all
three platforms (round 33's fix is confirmed working in production, not
just in CI). The actual cause: `tauri.conf.json`'s (and `Cargo.toml`'s/
`package.json`'s) `version` field had been `"0.1.0"` in every single build
this project has ever produced - since the updater compares the running
app's version against `latest.json`'s version and they were always
identical, `checkForUpdate()` correctly reported "no update available"
every single time, which is indistinguishable from "broken" to anyone
testing it. Fixed by bumping all three to `"0.2.0"` - this is what
actually needs to ship as a real release before an update round-trip can
be observed at all.

### What to check on real hardware

1. **Large real dump, for real**: send a project with an actual
   large-ish database dump (the kind that previously produced the OOM)
   over Local network, and confirm it completes without an out-of-memory
   error - watch the sending process's real memory usage (Task
   Manager/Activity Monitor/`top`) and confirm it does not climb anywhere
   near the dump file's own size.
2. **The update round-trip, for the first time ever**: install the
   previous (`0.1.0`) build, then publish a real `0.2.0` release, and
   confirm **Check for updates** now genuinely reports an update is
   available, downloads it, and relaunches into the new version - this
   specific round-trip has never actually been possible to observe before
   this round, since no version bump had ever existed.

## Round 37 addendum: Local-network peer discovery (mDNS)

New capability, scoped to Local network mode only (Remote relay/Cloud drop
already work across networks LAN discovery can't reach): a receiver can
opt in to being discoverable, and a sender picks them from a live
nearby-devices list instead of exchanging a room code. Uses `mdns-sd`
(actively-maintained, pure-Rust mDNS) rather than a hand-rolled UDP
broadcast protocol.

This does not change the trust model. Discovery only replaces the manual
code-copy-paste step with a click - the discoverable device still sees an
explicit "X wants to send you a project" request (a new
`ControlMessage::ConnectionRequest`/`ConnectionResponse` exchange over the
same control channel round 11's pull-requests already use) and must
Accept before anything is sent, and the diff-review-then-Run gate from
round 1 is completely unchanged.

### What's already confirmed, without needing real hardware

- A new same-box test (`crates/ls-net/tests/mdns_discovery_test.rs`)
  proves announce + browse actually find each other over real multicast
  sockets on this machine (not mocked) - passes both natively on Windows
  and in WSL2, each in under a second.
- `cargo build --workspace` and `cargo test --workspace` both pass in full
  (WSL2, the established authoritative environment for this crate's
  tests). This surfaced and fixed a real, pre-existing exhaustive `match`
  over `ControlMessage` in `pull_request_no_payload_test.rs` (round 11's
  own deliberate "a new variant must be consciously handled here" guard
  rail) - it did exactly its job, catching that the two new variants
  needed an explicit decision about payload-carrying (neither can carry
  one, same as every existing variant).
- `index.html`'s restructuring was checked for balanced, correctly nested
  tags with a real HTML parser, and every `$(id)` reference in `app.js`
  was checked to resolve to a real element - not just visual inspection.
- `web/test-magic-link-logic.js` and
  `apps/desktop/src/test-wizard-payload.js` both still pass unmodified -
  this round's changes didn't touch either's own pure-logic code path.

### What to check on real hardware

1. **The actual new capability, across two real machines**: on device A,
   confirm the pre-filled device name (defaults to the OS hostname; edit it if you like) and turn on "Make this device
   discoverable on Local network" (Receive tab). On device B, open the
   Send wizard with Local network selected and confirm device A appears
   in the nearby-devices list within a few seconds, with the name device A
   set. Select it, pick a project, and send - confirm device A sees the
   incoming connection-request banner naming device B, and that nothing
   is received until Accept is clicked.
2. **Manual code entry still works, unmodified**: with discoverability off
   (or a device not selected), confirm a normal room-code send/receive on
   Local network still works exactly as it always has - discovery is
   additive, never a replacement.
3. **A network that blocks multicast**: test on a network known or
   suspected to restrict multicast traffic (corporate/guest Wi-Fi with
   client isolation is the common real-world case; some VPN
   configurations too). Confirm the nearby-devices list shows the "no
   devices found - you can still enter a code manually" state rather than
   looking broken, hanging, or implying something failed, and that manual
   code entry still works normally on that same network.
4. **Reject at the connection-request stage**: confirm clicking Decline on
   the incoming connection-request banner cleanly fails the sender's send
   attempt (a clear "declined" error, not a hang or a generic-looking
   failure) and that nothing was received on the declining device.
5. **Discoverability toggle off mid-session**: turn discoverability off
   while genuinely idle (no pending request) and confirm the device stops
   appearing in another machine's nearby-devices list within a few
   seconds. Toggling off is not expected to gracefully interrupt a
   request that's already showing its Accept/Decline banner on screen -
   that in-flight request can still be answered normally.
6. **Multiple discoverable devices at once**: with two or more machines
   simultaneously discoverable on the same network, confirm all of them
   show up in a third machine's nearby-devices list, each with its own
   correct name, and that selecting one connects to the right device.

## Dump-unpack failure investigation: Run failed with "failed to unpack .../db-dumps/<folder>/<schema>.sql"

Reported as a regression from round 36's zstd change ("compressed on send,
not decompressed on receive"). Investigated by reproducing rather than
assuming, and **that theory did not hold**: every path speaks zstd (bundle,
merge, diff, unpack), and a project sent through the real wizard command
with a real SQL dump now has a test that carries it all the way through
**Run** and checks the unpacked dump is byte-identical
(`a_wizard_send_with_a_real_sql_dump_can_actually_be_run`, 64 MB by default,
`LS_DUMP_TEST_MB` scales it). Extraction is also proven byte-exact at 400 MB
and when unpacked twice into the same directory (what a retried Run does),
and `tar` encodes >8 GiB sizes correctly.

What *was* provably wrong: the message shown was only tar's outer wrapper
(`failed to unpack <file>`); the real reason (disk full, permission denied,
corrupt stream) is nested underneath and `run_snapshot` discarded it with
`e.to_string()`. Fixed: the full cause now reaches the UI, with a plain-
language hint for disk-full (notes that `/tmp` is RAM-backed on some Linux
systems), permission-denied, and truncated/corrupt data.

### What to check on real hardware

1. **Re-run the failing project** (the `xusom-admin` send). If Run still
   fails, the error now says *why* - that line is the actual root cause; send
   it along. If it succeeds, the earlier failure was environmental.
2. **Free space at the work directory**, if the message mentions a full
   disk: `df -h /tmp` (the UI's default work directory is
   `/tmp/localsync-work`). Point the Work directory field at a real disk
   with room for the project *and* its uncompressed dump.

## Session-model refactor: one project session, ephemeral connections

Sending is now built on a **project session** (`project_session.rs`,
`session_commands.rs`): a persistent workspace for one project holding its
folders + database plan, the built artifact, and a history of every device it
has been sent to with **each device's own last-received marker**. A
connection is never held open - every transfer connects, sends, disconnects.
Pushing an update opens the session, picks devices from its history, and for
each connects fresh and sends a snapshot whose reviewed diff is against *that
device's* marker. Saving is opt-in ("Save this session?"), stored in the
round-29 history file. This replaced the separate tab/retry/mode/roster logic
from rounds 8, 10, 11, 12, 25, 28 and 34.

### What's already confirmed, without needing real hardware

- Same-box transfers (`project_session_flow_test.rs`): a session is created
  once and its artifact reused across a failed attempt + retry and a second
  device (build count stays 1); each device keeps its own marker; a push to A
  diffs against A's marker, not B's, and moves only A's; an up-to-date device
  is not connected to at all; nothing is left registered as an open
  connection; a never-saved session leaves nothing behind while a saved one
  reopens whole.
- The model, persistence and device-id logic have their own unit tests
  (`project_session`, `session_history`, `session_commands`), and the pure
  frontend logic is tested in `test-session-model.js`.
- The real `index.html` + `app.js` were driven in headless Chrome against a
  mocked backend (52 checks): first send, second device on the same tab,
  retry in place, push update, discovered-device consent path, save/discard
  prompt on tab close and on quit, reopening a saved session.

### What to check on real hardware

1. **Push to a discoverable device**: send a project to a second machine
   (with "Make this device discoverable" on), commit a change, open the
   session tab, tick that device and **Push update**. Confirm the receiver is
   asked to accept, and its review screen's diff shows only what changed
   since *its* last version - and that the bytes on the wire are still the
   whole project (there is no wire-level delta; see below).
2. **Two devices at different versions**: send v1 to A, commit, send v2 to B,
   commit again, then push to both. Each should review a diff relative to its
   own last version.
3. **Push to a device that isn't discoverable**: it should get a fresh code
   from this end (with the note saying why), not fail.
4. **Retry** after letting a code expire: the same transfer card gets a new
   code; no second card or tab; no rebuild pause.
5. **Save / discard**: close an unsaved session tab and quit the app with one
   open - both should ask, with the one-line explanation. "Don't save" must
   leave nothing in the saved-sessions list after a restart; "Save" must
   bring it back (folders, database plan, devices) via the menu's saved
   sessions. **Quit-time prompt only** - confirm the window actually waits
   for the answer on Windows, macOS and Linux (uses the window close-request
   hook and a new `core:window:allow-destroy` permission).

### Known gaps, flagged rather than papered over

- **No wire-level delta.** The snapshot payload is always the full
  `git archive HEAD`; a device's marker changes the *diff shown for review*
  (`diff_stat.json`/`diff.patch`), not how many bytes are sent. A real delta
  needs a new payload kind and a receiver that keeps the previous version to
  apply it onto - new transfer-protocol work, out of scope here.
- **Receiver-initiated "Ask for update" / pull requests are gone** from the
  UI: they need the sender to hold a connection open, which is exactly what
  the new model removes.
- **Devices reached by pasted code have no identity.** Each such send is
  filed as its own device ("Device via code"); only a discoverable device
  (which announces a persistent id) is recognized across sends.
- **Cloud drop stays outside the model**: it uploads the first folder itself
  and records no device or marker.
- **Legacy backend commands remain** (`share_snapshot_wizard`,
  `push_update`, roster, pull requests): unused by the UI, kept because
  existing tests exercise them - see the note at the top of `commands.rs`.

## Receiver-side session model: persistent received Sessions, arm-for-update, run/stop with zero connection

Follow-up to the sender-side session-model refactor above - a received
project is now its own persistent Session (`ReceivedSession`, mirroring
`ProjectSession`), not just an in-memory `state.verified` entry lost the
moment the app closes. Built as two agents in parallel against a fixed
contract (backend Rust, frontend JS); confirmed to actually match up during
integration - one real gap was found and fixed (the saved-sessions history
list was reading `entry.project` unconditionally, so a saved *receive*
entry's detail line and Open/Delete buttons never rendered - it needed
`entry.received` for `kind === "receive"`), plus one further real bug
(a `session-update-available` push landing on a closed/saved armed session
opened a fallback tab keyed by `snapshot_id`, not the real backend session
id, so a later Run/Save on that tab would have failed with "no open
received session" - fixed by threading the real `session_id` through).

### What changed

- **Receive → run → stop → session persists**: receiving now creates a
  `ReceivedSession` (title, sender identity, last snapshot, work dir,
  compose dir) the moment a payload is verified — independent of any
  connection. Run/Stop (`run_received_session`/the existing, unmodified
  `stop_session`) work against it with zero connection involved.
- **Re-running after a restart needs no reconnect**: `run_received_session`
  uses the already-unpacked, already-policy-rewritten project directory
  directly (`ls_containers::run_existing`) when nothing is left in memory,
  instead of requiring the original signed snapshot again. `db_cache_hit`
  is honestly reported as `false` (unknown) in this path, since the
  manifest needed to compute the real seed-hash-keyed volume name isn't
  persisted — the actual volume reuse is unaffected either way, only this
  one reporting field.
- **Update-ready is explicit, per-session arming** (`arm_received_session_
  for_update`/`disarm...`), not passive: a push only lands on an existing
  session if it was explicitly armed first, matched by sender identity +
  project title. This is real, structural: a session received via a pasted
  code (no persistent device identity) can be armed, but nothing will ever
  match it unless the sender's device also has a stable identity - i.e. the
  sender found this receiver via Local-network discovery. The UI states
  this plainly next to the toggle rather than leaving it to be discovered
  the hard way.
- **Save/discard reuses the exact sender-side mechanism** - same window
  close-request hook, same `core:window:allow-destroy` permission, same
  opt-in-only persistence file (`session-history.json`, `received` field
  alongside `project`). The same cross-platform verification gap the
  sender side already flagged applies here too: not yet click-tested on a
  real Windows/macOS/Linux window, only that the build accepts the
  permission and the logic is unit/integration-tested.
- **A `snapshot_id` collision is real and handled**: two unarmed pushes of
  an unchanged project produce the identical `snapshot_id`, but must become
  two independent sessions (a user can legitimately receive the same
  version twice). `ReceivedSession.id` is a fresh id, never the snapshot
  id - confirmed by a test that deliberately reuses a `snapshot_id` across
  two unarmed pushes and checks two distinct sessions result.

### What to check on real hardware

1. **Full lifecycle, no connection**: receive a project, review, Run,
   confirm it's live, quit the app (or just close the tab and choose
   Save), reopen the app, reopen the saved session from history, click Run
   again - confirm it comes back up without any network activity at all.
2. **Arm → push → same tab updates**: on the receiver, turn on Local-network
   discoverability, receive once from a discovering sender, arm that
   session for update, then have the sender push again to the same device -
   confirm the *same* tab shows the new diff for review (not a second tab),
   and that Run is still a separate, explicit click (never auto-applied).
3. **Arm with a pasted-code receive**: arm a session that was received via
   a pasted code (no discoverable identity involved) and confirm the UI's
   own hint about this being unlikely to ever match a real push is honest
   in practice - a resend from the same sender should NOT land on it
   automatically.
4. **Save/discard prompt on real windows**: closing a receive tab (and
   quitting the app with one open) should show the same save/discard
   dialog the send side already has, on Windows, macOS, and Linux - the
   specific gap this and the sender-side round both flagged as unverified.
5. **History list**: after saving both a send and a receive session,
   confirm the history panel shows the right detail line and working
   Open/Delete buttons for both kinds - this exact rendering path had a
   real bug (reading the wrong field for a receive entry) caught and fixed
   during this round's integration, worth a real look to confirm the fix
   holds up visually, not just in code.
