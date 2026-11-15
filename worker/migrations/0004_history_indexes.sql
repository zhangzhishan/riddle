CREATE INDEX IF NOT EXISTS messages_history_index
  ON messages(created_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS refinements_history_index
  ON refinements(created_at DESC, id DESC);
