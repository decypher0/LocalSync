-- Schema for the LocalSync demo fixture. Runs automatically via Postgres's
-- docker-entrypoint-initdb.d on first boot of an empty data directory.

CREATE TABLE IF NOT EXISTS notes (
    id    SERIAL PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    body  TEXT
);
