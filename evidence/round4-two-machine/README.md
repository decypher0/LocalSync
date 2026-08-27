# Round 4 — real two-machine, real-UI end-to-end evidence

## Isolation level (read this before the rest)

The "two machines" are two independent WSL2 distro instances: `Ubuntu` (sender) and
`LocalSync-Receiver` (receiver — created via `wsl --export`/`--import` from `Ubuntu`,
same toolchain already present, default user fixed to `decypher`). They have separate
filesystems, users, network namespaces, and Podman instances — but they run inside the
**same underlying kernel/VM** (both report hostname `DESKTOP-MBALNRU`; `hostnamectl`
labels the chassis `container`) and share the **same WSLg display** (`DISPLAY=:0`).

This is the explicitly-approved fallback, not an attempt to pass off container-level
separation as "real machines": true separate hardware/VMs aren't available in this
environment (Windows 11 **Home** has no Hyper-V, and WSL2's architecture runs all
distros in one shared lightweight VM). The coordinator confirmed this empirically
before proceeding and got explicit sign-off to use this as "the most realistic option
available."

## What actually happened

1. `sample-project/` was copied into `~/demo-sample-project` inside the `Ubuntu`
   instance and `git init`'d there (it isn't its own repo in the monorepo).
2. Signaling server (`apps/signaling-server/index.js`, port 9090) run on the Windows
   host, reachable from both WSL2 instances via the WSL gateway IP.
3. `localsync-desktop` launched in both instances (`LOCALSYNC_DATA_DIR` set to a
   distinct path in each), driven via `xdotool` + screenshotted via `scrot`, both
   through the shared WSLg `DISPLAY=:0`.
4. Real flow driven through the actual UI: project path + room code entered on the
   sender, Send clicked; same room code entered on the receiver, Receive clicked;
   review screen inspected; Run clicked.
5. Podman (inside the receiver instance, under its own `LOCALSYNC_DATA_DIR`-redirected
   storage root) built and started `mysql` + `app` containers from the received,
   verified snapshot.

## Screenshots

- **`32-sender-fresh.png`** — share initiated/completed on the sender: "Sent as
  `demo-sample-project@ee4b9e1307cac9b8bd264069075f2873a78ddba2`".
- **`34-receiver-review-full.png`** — the real review/consent screen on the receiver:
  same commit hash, real manifest (`app`/`mysql` services), and a real diff table for
  all 12 actual files in the snapshot (`+314/-0`) — genuinely rendered from a
  P2P-received, verified snapshot, not a mockup.
- **`40-receiver-native-click.png`** — confirms the Run click registered (button shows
  "Starting containers...").
- **Running/session-panel screenshot: not captured.** After the containers came up,
  the shared WSLg compositor stopped producing anything but black frames for every
  screenshot attempt (`43`–`46` in this directory are all black, including a fresh
  X11 test after forcing a window resize/move repaint) — confirmed as a WSLg
  session issue, not an app crash (`localsync-desktop`, `WebKitWebProcess`, and
  `WebKitNetworkProcess` were all still alive and responsive throughout). A full
  `wsl --shutdown` likely would have fixed the compositor, but would also have killed
  the running containers being verified, so it wasn't done. The functional proof this
  screenshot would have shown is captured instead in the `/health` output below,
  independently confirmed against the real running containers.

## Functional proof (the part a screenshot can't fake)

From inside the `LocalSync-Receiver` instance, against the containers the real UI
flow actually started:

```
$ podman ps -a
CONTAINER ID  IMAGE                                                     COMMAND  STATUS                        PORTS                   NAMES
c7db3e5d3797  docker.io/library/mysql:8.0                               mysqld   Up (starting)                                         localsync-demo-sample-project-ee4b9e1307ca_mysql_1
97b7aaf3cafe  localhost/localsync-demo-sample-project-ee4b9e1307ca_app:latest    Up                            0.0.0.0:8080->8080/tcp  localsync-demo-sample-project-ee4b9e1307ca_app_1

$ curl -s -w "\nHTTP %{http_code}\n" http://localhost:8080/health
{"status":"UP"}
HTTP 200

$ curl -s -w "\nHTTP %{http_code}\n" http://localhost:8080/api/notes
[{"id":1,"title":"Welcome to LocalSync", ...}, ... 7 rows total ...]
HTTP 200
```

(mysql's own healthcheck shows "starting" indefinitely rather than "healthy" —
cosmetic; the app connected and served real seeded data regardless, same as
round 1's standalone verification.)

Container/volume names (`localsync-demo-sample-project-ee4b9e1307ca_*`,
`localsync-db-f61aa5c299018609cbbdfeab79ae2d15cc789ccc389381ed3b2012d1a8cd3016`)
match the manifest's project name and commit shown in the review screenshot above —
this is the same run, not a separately-triggered one.
