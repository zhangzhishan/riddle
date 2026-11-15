ALTER TABLE refinements
  ADD COLUMN mode TEXT NOT NULL DEFAULT 'refine'
  CHECK (mode IN ('refine', 'redraw'));
