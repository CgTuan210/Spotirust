-- Spotirust Local Music Library Persistence (Migration 0002)
-- Persists scanned local audio files with size/mtime fingerprints so startup loads instantly
-- from SQLite without rescanning entire music directories on every launch.

CREATE TABLE IF NOT EXISTS local_tracks (
    path TEXT PRIMARY KEY NOT NULL,
    file_size INTEGER NOT NULL DEFAULT 0,
    mtime_secs INTEGER NOT NULL DEFAULT 0,
    title TEXT NOT NULL,
    artist TEXT NOT NULL,
    album TEXT NOT NULL,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    artwork_url TEXT,
    track_json TEXT NOT NULL,
    scanned_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_local_tracks_title ON local_tracks(title);
CREATE INDEX IF NOT EXISTS idx_local_tracks_artist ON local_tracks(artist);
