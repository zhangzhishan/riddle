CREATE TABLE IF NOT EXISTS refinements (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  device_id TEXT NOT NULL,
  refinement_key TEXT NOT NULL,
  input_key TEXT NOT NULL UNIQUE,
  output_key TEXT NOT NULL UNIQUE,
  model TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(device_id, refinement_key)
);

CREATE INDEX IF NOT EXISTS refinements_created_index
  ON refinements(id DESC);
