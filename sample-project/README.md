# LocalSync sample project

This is LocalSync's reference demo fixture: a small, deliberately boring
Spring Boot + MySQL app. Every MVP demo path (P2P snapshot transfer, sandboxed
Podman run on the receiving side) runs against this project. It is not a
showcase app — it's a toy notes API, intentionally minimal.

## Structure

- `app/` — Spring Boot 3 app (Maven), a notes REST API
- `db-seed/` — `schema.sql` + `seed.sql`, run automatically by the MySQL
  container on first boot of an empty data volume
- `docker-compose.yml` — `app` + `mysql` services

## Run it standalone

This works on its own, independent of LocalSync:

```sh
cd sample-project
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
