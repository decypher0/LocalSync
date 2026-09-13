# Auto-containerization: a deferred capability, not yet built

**Status as of round 30: not started. This document exists so the idea doesn't have to be rediscovered from scratch — nothing described here exists in the codebase yet.**

## The gap this would close

LocalSync currently requires a project to already have its own `docker-compose.yml` for the receiver to be able to click Run — `ls_containers::run_snapshot` reads that file directly and has no fallback if it's missing (see round 30's fix to that error message, in `crates/ls-containers/src/lib.rs`, for exactly what a developer sees today when a project has none).

That's a real, meaningful limitation: plenty of real projects a developer might want to share — a plain Node/Express app, a Python/Flask app, a Spring Boot app run via `mvn spring-boot:run` rather than its own Docker setup — were never containerized at all, because their developer never needed to until the moment they wanted to hand it to someone else with LocalSync. Right now, that developer has to go write a `Dockerfile` and `docker-compose.yml` by hand before LocalSync can help them at all — friction that undercuts a good chunk of what makes LocalSync useful in the first place (instant, zero-deploy sharing).

## What this round explicitly did NOT do

Round 30 fixed a real, separate bug (a single-folder wizard send failing to include an *existing* `docker-compose.yml` correctly — a packaging bug, not a missing-file problem) and improved the error message for a project that genuinely has no compose file at all. Neither of those is auto-generation. This document describes a capability that still needs its own dedicated round of real design and implementation work — deliberately not attempted here, per that round's own hard budget rule.

## What the capability would actually need to do

1. **Detect the project's framework/language from files already in the repo**, cheaply and without executing any of the project's own code — the same spirit as the existing database-wizard auto-detection (round 17's Spring Boot `application.yml` sniffing) applied to the app service itself instead of just its database connection. Concretely, at minimum:
   - `package.json` → Node.js (check `"scripts".start`/`"main"` for the actual entry point and port; a `package-lock.json`/`yarn.lock`/`pnpm-lock.yaml` alongside it hints at the package manager to use in the generated Dockerfile).
   - `pom.xml` → Java/Maven (Spring Boot specifically if `spring-boot-starter-*` shows up as a dependency — matters for picking a sane default port/health-check convention).
   - `requirements.txt` / `pyproject.toml` → Python (and ideally which web framework — Flask/Django/FastAPI have different default ports and run commands).
   - Treat an unrecognized project as a real "can't auto-detect" case with a clear message, not a silent wrong guess — matching this project's own established preference (see round 18/22's own error-handling work) for a real, actionable failure over one that quietly does the wrong thing.
2. **Generate a reasonable default `Dockerfile`** for the detected framework — a real multi-stage or otherwise sane build (not just "napkin-sketch `FROM node` with no `.dockerignore` awareness"), since this file becomes part of what the receiver's machine actually builds and runs.
3. **Generate a `docker-compose.yml`** that combines:
   - The detected app service (built from the generated Dockerfile).
   - Whatever database service the Send wizard already collected (rounds 17/18/22's own `ConnectionDetails`/`DumpPlanDto` already carry engine + connection info — this would reuse that, not re-invent a second database-description format).
4. **Respect the existing security policy layer** (`ls_security::default_policy()`, already applied to *any* compose file — generated or provided — via `compose::apply_policy` in `ls_containers`) rather than trying to bypass or duplicate it for generated files specifically.
5. **Never silently overwrite a project's own partial setup** — e.g., a project with a `Dockerfile` but no `docker-compose.yml` (or vice versa) is a real, different case from "nothing at all," and generating on top of an existing file without the developer's explicit confirmation would be a real, separate class of bug to design around carefully, not an edge case to handle as an afterthought.

## Why this is real, separate scope (not a quick add-on)

- Framework detection done honestly (not just "if package.json exists, assume Express on port 3000") needs real per-framework knowledge and real test projects per framework to prove against — likely at least one disposable sample project per supported framework, mirroring how `sample-project`/`sample-project-node` already exist for the two currently-supported stacks.
- A generated Dockerfile that doesn't actually build/run correctly is worse than no Dockerfile at all — it would need the same "prove it round-trips for real, not just that code compiles" evidence bar every other feature in this project has held to (see `crates/ls-dbsource`'s own real-disposable-database testing as the model to follow).
- Deciding how much to auto-detect vs. ask the developer to confirm/adjust (a review step before the generated files are trusted, similar in spirit to the Receive-side diff review this whole project is built around) is a real design question, not just an implementation detail.

## Suggested shape for the round that eventually builds this

A dedicated round, not a goal bolted onto an unrelated one — this needs its own root-cause-style investigation into what "reasonable default" actually means per framework (probably starting with just Node.js and Python, the two most common cases for a project with no existing containerization at all), its own new sample project(s) to prove against, and its own explicit hard-budget-rule framing about what's in and out of scope for a first pass (e.g., "detect and generate for Node/Python only, clear error for anything else" is a legitimate, honestly-scoped first version — not a compromise to apologize for).
