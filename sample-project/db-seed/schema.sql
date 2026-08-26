-- Schema for the LocalSync demo fixture. Runs automatically via MySQL's
-- docker-entrypoint-initdb.d on first boot of an empty data volume.

CREATE TABLE IF NOT EXISTS notes (
    id    BIGINT AUTO_INCREMENT PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    body  TEXT
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
