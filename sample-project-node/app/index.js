// LocalSync demo fixture: a minimal notes API. One entity, two endpoints,
// no auth, no ORM — on purpose. This is a fixture, not a template.

const express = require("express");
const { Pool } = require("pg");

// pg reads PGHOST/PGUSER/PGPASSWORD/PGDATABASE/PGPORT itself — no need to
// wire them through a config layer.
const pool = new Pool();

const app = express();
app.use(express.json());

app.get("/health", (req, res) => {
  res.status(200).json({ status: "UP" });
});

app.get("/api/notes", async (req, res) => {
  const result = await pool.query(
    "SELECT id, title, body FROM notes ORDER BY id"
  );
  res.json(result.rows);
});

app.post("/api/notes", async (req, res) => {
  const { title, body } = req.body ?? {};
  const result = await pool.query(
    "INSERT INTO notes (title, body) VALUES ($1, $2) RETURNING id, title, body",
    [title, body]
  );
  res.status(201).json(result.rows[0]);
});

const port = process.env.PORT || 8080;
app.listen(port, () => {
  console.log(`sample-app-node listening on ${port}`);
});
