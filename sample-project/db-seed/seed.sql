-- Small sample dataset, not a production dump — just enough rows to prove
-- GET /api/notes reads real data.

INSERT INTO notes (title, body) VALUES
    ('Welcome to LocalSync', 'This note was seeded on first boot — proves the DB read path works.'),
    ('Grocery list', 'Milk, eggs, bread, coffee.'),
    ('Meeting notes', 'Discussed the Q3 roadmap and the P2P sync protocol.'),
    ('Recipe: pancakes', 'Flour, eggs, milk, a pinch of salt. Mix and fry.'),
    ('Book to read', 'Designing Data-Intensive Applications, by Martin Kleppmann.'),
    ('Bug to fix', 'Container health check flakes on a cold start.'),
    ('Demo reminder', 'Show the P2P snapshot transfer before the sandbox spins up.');
