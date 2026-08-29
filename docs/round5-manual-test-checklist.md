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
