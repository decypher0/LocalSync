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

### Before you start: get a real Client ID

Follow `docs/google-drive-setup.md` start to finish first — a Google Cloud
project, the Drive API enabled, an OAuth consent screen in "Testing" status
with **two of your own Google accounts** added as test users (one to act as
sender, one as receiver), and a Desktop-app-type OAuth Client ID. Set
`GOOGLE_OAUTH_CLIENT_ID` to that value before launching either app instance.
Without it, Settings → **Link Google account** fails immediately with a
message pointing back at that doc — confirm that's what you see if you
launch without setting it, as a sanity check that the failure path itself is
honest before you set the real value.

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
   machines.** On the sender: check the **Cloud drop** box on the Send
   wizard's final step, pick a retention option, click **Send** — confirm a
   room code appears and a real file lands in your Google Drive (check
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
