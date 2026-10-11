-- npx wrangler d1 execute gamefix --remote --file schema.sql
CREATE TABLE IF NOT EXISTS reports (
  id INTEGER PRIMARY KEY,
  -- The report's id as gamefix showed it ("K7Q2-9XMB"); row numbers are reused.
  ref TEXT NOT NULL,
  received TEXT NOT NULL DEFAULT (datetime('now')),
  -- A random id gamefix makes once per install: rate limits, and telling reports apart.
  install TEXT NOT NULL,
  version TEXT NOT NULL,
  message TEXT NOT NULL,
  -- How the person would like an answer, if at all (free text, never used to send anything).
  contact TEXT,
  -- The game's name as gamefix gave it.
  game TEXT,
  -- One line: the distribution, graphics card makers, Steam's runtime.
  system TEXT,
  -- `gamefix check`'s report on the game.
  report TEXT,
  -- What the last check, setup or undo of the game said.
  last TEXT,
  -- What gamefix changed for the game (its record).
  record TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS reports_ref ON reports (ref);
CREATE INDEX IF NOT EXISTS reports_install ON reports (install, received);
