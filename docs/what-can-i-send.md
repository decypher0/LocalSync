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

The project in git with at least one commit. LocalSync sends what's committed
(plus uncommitted edits to files git already tracks), so every send - with or
without a `docker-compose.yml` - needs a commit to start from.

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
