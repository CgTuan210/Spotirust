-- Spotirust Initial SQLite Schema (Migration 0001)
-- NOTE: OAuth tokens and sensitive credentials are NEVER stored in SQLite.
-- They are stored exclusively in the operating system's secure credential vault.

CREATE TABLE IF NOT EXISTS user_profile (
    id TEXT PRIMARY KEY NOT NULL,
    account_id TEXT,
    display_name TEXT NOT NULL,
    avatar_url TEXT,
    uri TEXT,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS cached_tracks (
    id TEXT PRIMARY KEY NOT NULL,
    uri TEXT NOT NULL,
    title TEXT NOT NULL,
    artists_json TEXT NOT NULL,
    album_id TEXT,
    album_name TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    isrc TEXT,
    artwork_url TEXT,
    explicit INTEGER NOT NULL DEFAULT 0,
    liked INTEGER NOT NULL DEFAULT 0,
    local_path TEXT,
    cached_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_cached_tracks_title ON cached_tracks(title);
CREATE INDEX IF NOT EXISTS idx_cached_tracks_isrc ON cached_tracks(isrc);

CREATE TABLE IF NOT EXISTS cached_albums (
    id TEXT PRIMARY KEY NOT NULL,
    uri TEXT NOT NULL,
    name TEXT NOT NULL,
    artists_json TEXT NOT NULL,
    release_date TEXT,
    total_tracks INTEGER NOT NULL DEFAULT 0,
    artwork_url TEXT,
    cached_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS cached_playlists (
    id TEXT PRIMARY KEY NOT NULL,
    uri TEXT NOT NULL,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    owner_name TEXT NOT NULL DEFAULT '',
    owner_id TEXT NOT NULL DEFAULT '',
    snapshot_id TEXT NOT NULL DEFAULT '',
    total_tracks INTEGER NOT NULL DEFAULT 0,
    artwork_url TEXT,
    is_collaborative INTEGER NOT NULL DEFAULT 0,
    is_public INTEGER,
    cached_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS recently_played (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    track_id TEXT NOT NULL,
    played_at_ms INTEGER NOT NULL,
    context_uri TEXT,
    track_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_recently_played_time ON recently_played(played_at_ms DESC);

CREATE TABLE IF NOT EXISTS lyrics_cache (
    cache_key TEXT PRIMARY KEY NOT NULL,
    track_id TEXT,
    title TEXT NOT NULL,
    artist TEXT NOT NULL,
    album TEXT,
    duration_ms INTEGER,
    format TEXT NOT NULL,
    source TEXT NOT NULL,
    raw_content TEXT,
    offset_ms INTEGER NOT NULL DEFAULT 0,
    instrumental INTEGER NOT NULL DEFAULT 0,
    fetched_at_ms INTEGER NOT NULL,
    expires_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_lyrics_cache_track_id ON lyrics_cache(track_id);

CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS playback_preferences (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    volume REAL NOT NULL DEFAULT 0.8,
    shuffle INTEGER NOT NULL DEFAULT 0,
    repeat_mode TEXT NOT NULL DEFAULT 'off',
    backend_kind TEXT NOT NULL DEFAULT 'auto',
    last_track_json TEXT,
    last_position_ms INTEGER NOT NULL DEFAULT 0,
    last_queue_json TEXT NOT NULL DEFAULT '[]',
    updated_at_ms INTEGER NOT NULL
);

INSERT OR IGNORE INTO playback_preferences (id, volume, shuffle, repeat_mode, backend_kind, last_position_ms, last_queue_json, updated_at_ms)
VALUES (1, 0.8, 0, 'off', 'auto', 0, '[]', 0);
