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
