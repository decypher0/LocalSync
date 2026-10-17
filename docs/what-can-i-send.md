# What can I send?

The same short guide appears in the app above **Add folder(s)…** in Setup,
built from LocalSync's own list of supported setups, so the two agree.

## Works

Web apps and APIs that listen on a port, in **Java (Maven or Gradle),
Node.js, Python or Go** - optionally with a **MySQL, PostgreSQL or MongoDB**
database, whose data travels with the project as a dump.

A project with its own `docker-compose.yml` is run with that file, as-is.
For one without, Setup asks a few questions (language, build tool, run
command, port, database) and creates the run instructions for you - nothing is
written into your folder.

## Not supported yet

Mobile apps; Rust, PHP, Ruby and .NET projects; static-only sites.

## Needs

The project in git with at least one commit. LocalSync sends what's committed,
plus uncommitted edits, deletions and new files that `.gitignore` doesn't
exclude - so every send, with or without a `docker-compose.yml`, needs a
commit to start from. Your git index, branches and files are never changed.

New files that look like secrets (`.env*`, `*.env`, `*.pem`, `*.key`,
`*.p12`, `id_rsa*`, `credentials*`, `secrets*`) are left out until you tick
them in **Changes since the last send** on the session page, where you can also
untick any other new file.

A folder that isn't a git repository yet (for example a GitHub "Download ZIP"
folder) shows **Set up automatically** under it in Setup. That adds a hidden
`.git` folder and one commit; your files aren't changed. If the folder has no
`.gitignore`, build and dependency folders (`node_modules`, `target`, `build`,
`dist`, virtualenvs, IDE folders, logs) are excluded through git's own
`.git/info/exclude` instead of a file in your project. Git must be installed
(git-scm.com).

## What each folder shows in Setup

- **"<Stack> project detected"** - recognised from its files (`pom.xml`,
  `build.gradle`, `package.json`, `requirements.txt` / `pyproject.toml`,
  `go.mod`, `docker-compose.yml`).
- **"Not tracked by git yet"** / **"No git commits yet"** - with **Set up
  automatically**.
- **"Not supported yet: <stack>"** - for the stacks listed above.

Whether a particular program runs well in a container is up to the project;
LocalSync doesn't try to judge that.

## Files outside the project

Some projects read a file that lives outside their folder - for example
`company.configuration.filepath=C:/Users/you/Projects/configuration/company_configuration.xml`
in `application.properties`, listing tenants. That file isn't in git, so it
would never reach the other computer.

In Setup (the services step), **Files outside the project** lists every
active setting in `*.properties`, `*.yml` / `*.yaml`, `.env` files and simple
XML whose value is an absolute path (`C:\...`, `C:/...`, `/home`, `/Users`,
`/opt`, `/var`, `/etc`, `/srv`, `/mnt`, `/usr`, `file:` URLs). Commented-out
lines are ignored. Each one shows the setting, the file, the path and its
size:

- a file or folder that exists on this computer is ticked to travel with the
  project (anything over 50 MB starts unticked, with a warning - it is copied
  into every send);
- a path inside the project suggests a relative path instead;
- paths that don't exist here are listed separately; they can't be sent.

**Add a file or folder...** adds one by hand, with the setting that points
at it.

A ticked file travels in the snapshot under `external/`. On the receiver it
is mounted into the app's container at `/localsync/external/<id>/`, and the
setting is changed to that path in LocalSync's own copy of the config, before
the build (so a Java `src/main/resources` file is right inside the jar). Your
file and your folder are never changed.

Not supported yet: changing the path inside XML files (the file is still
mounted - Setup shows where), values split over several lines, and projects
with their own `docker-compose.yml`. A project that uses one database per
tenant still sends only the one dump chosen in Setup.

## Sending an update

The session page lists **Changes since the last send**: per folder, the files
added, modified and deleted since the last send to that device, and whether
its database dump was exported again. It refreshes every 5 seconds and when
you come back to the window. **Push update** is enabled whenever that list
isn't empty; before a device has received anything it says "Not sent to this
device yet". An edited file from outside the project counts too, shown as
`external: <name> (modified)`.

**Push update** and **Send again** first check the update ("Checking this
update..."): the setup still applies (settings valid, dump file and build
file still there), and no changed config file now points at a new path
outside the project. When build or config files changed (`pom.xml`,
`build.gradle`, `package.json`, `requirements.txt`, `pyproject.toml`,
`go.mod`, `Dockerfile`, `docker-compose.yml`, `*.properties`, `*.yml`, XML
under `resources/` or `WEB-INF/`) - or a file from outside the project - the
project is also built and started in Podman here, on a spare port with a
throwaway database, exactly as it will be sent. A pass says "Compatible -
built and started in N s" and sends. A failure shows the last log lines with
**Fix and re-check** and **Send anyway**. Without Podman only the quick
checks run, and the page says so. A project with its own
`docker-compose.yml` gets the quick checks only.

Before pushing, LocalSync asks the receiver which version it holds. If it
holds a different one (or no longer has the session), the whole project is
sent instead and the result says "Sent in full (receiver had a different
version)".

On the receiver, an update from the same sender for the same project lands
on the session it already has, as **Update to review** - nothing has to be
switched on first. After the review:

- a running session offers **Apply & restart**: the old version stops, the
  new one is built and started, and LocalSync waits for it to be healthy;
- a stopped session offers **Run**.

Each update carries the files from outside the project again, so the new
version always runs with the sender's current copy. The previous version is
kept until the new one is healthy. If it fails, the
page shows the end of its log and **Roll back to previous version**.

If the sender's database dump is unchanged, the receiver's database (and
anything added to it locally) is kept. If it changed, LocalSync asks once:
**Keep my data** (default) or **Replace with sender's database** (local data
is lost).
