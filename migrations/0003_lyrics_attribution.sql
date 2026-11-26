-- Spotirust SQLite Schema Migration 0003
-- Persist structured lyrics provider attribution (e.g., Unison mandatory song/curator links,
-- AMLL TTML DB contributor credit, Musixmatch copyright notice) alongside cached lyrics.

ALTER TABLE lyrics_cache ADD COLUMN attribution_json TEXT;
