# LocalSync sample project (Node)

This is LocalSync's second reference demo fixture: a small, deliberately
boring Express + Postgres app. It exists to prove LocalSync's snapshot/
container pipeline isn't Spring-Boot/MySQL-specific — `sample-project/`
proved the pipeline once against Java/Maven/MySQL, this proves it again
against a different runtime, package manager, and database engine. It is
not a showcase app — it's a toy notes API, intentionally minimal.

## Structure

- `app/` — Express app (Node), a notes REST API
- `db-seed/` — `schema.sql` + `seed.sql`, run automatically by the Postgres
  container on first boot of an empty data directory
- `docker-compose.yml` — `app` + `postgres` services

## Run it standalone

This works on its own, independent of LocalSync:

```sh
cd sample-project-node
docker compose up   # or: podman-compose up
```

Then, once it's up:

```sh
curl localhost:8080/health
curl localhost:8080/api/notes
curl -X POST localhost:8080/api/notes \
  -H "Content-Type: application/json" \
  -d '{"title":"hi","body":"from curl"}'
```

## Notes

- One entity (`Note`), two endpoints, no auth, no migrations framework —
  on purpose. This is a fixture, not a template to build a real app from.
- DB credentials in `docker-compose.yml` are demo-only and thrown away with
  `docker compose down -v`.
