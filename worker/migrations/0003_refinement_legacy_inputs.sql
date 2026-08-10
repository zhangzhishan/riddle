ALTER TABLE refinements
  ADD COLUMN input_available INTEGER NOT NULL DEFAULT 1
  CHECK (input_available IN (0, 1));
