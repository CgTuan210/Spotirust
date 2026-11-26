//! SQLite persistence layer with versioned migrations, WAL concurrency, and typed repositories.
//!
//! Stores user profile metadata, recently played history, cached tracks/albums/playlists,
//! lyrics cache, application settings, and playback preferences.
//! OAuth tokens and secrets are NEVER stored in SQLite.

use crate::core::config::UserSettings;
use crate::core::models::{Album, AudioBackendKind, Playlist, RepeatMode, Track, UserProfile};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thiserror::Error;

const MIGRATION_0001: &str = include_str!("../../migrations/0001_initial.sql");
const MIGRATION_0002: &str = include_str!("../../migrations/0002_local_library.sql");
const MIGRATION_0003: &str = include_str!("../../migrations/0003_lyrics_attribution.sql");

/// Versioned database migrations executed in strict ascending order.
const MIGRATIONS: &[(i64, &str)] = &[
    (1, MIGRATION_0001),
    (2, MIGRATION_0002),
    (3, MIGRATION_0003),
];

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("database lock poisoned")]
    LockPoisoned,
}

/// Persisted local audio file metadata with size/mtime fingerprint for incremental rescans.
#[derive(Debug, Clone)]
pub struct PersistedLocalTrack {
    pub path: String,
    pub file_size: u64,
    pub mtime_secs: i64,
    pub track: Track,
}

/// Persisted playback preferences restored across restarts.
#[derive(Debug, Clone)]
pub struct PlaybackPreferences {
    pub volume: f32,
    pub shuffle: bool,
    pub repeat_mode: RepeatMode,
    pub backend_kind: AudioBackendKind,
    pub last_track: Option<Track>,
    pub last_position_ms: u64,
    pub last_queue: Vec<Track>,
}

impl Default for PlaybackPreferences {
    fn default() -> Self {
        Self {
            volume: 0.8,
            shuffle: false,
            repeat_mode: RepeatMode::Off,
            backend_kind: AudioBackendKind::LocalRodio,
            last_track: None,
            last_position_ms: 0,
            last_queue: Vec::new(),
        }
    }
}

/// Cached lyrics payload stored in SQLite.
#[derive(Debug, Clone)]
pub struct CachedLyricsRecord {
    pub cache_key: String,
    pub track_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
    pub format: String,
    pub source: String,
    pub raw_content: Option<String>,
    pub offset_ms: i64,
    pub instrumental: bool,
    pub fetched_at_ms: i64,
    pub expires_at_ms: i64,
    pub attribution_json: Option<String>,
}

/// Summary statistics for the Settings / Diagnostics panel.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    pub cached_tracks: usize,
    pub local_tracks: usize,
    pub cached_albums: usize,
    pub cached_playlists: usize,
    pub cached_lyrics: usize,
    pub recently_played: usize,
    pub schema_version: i64,
}

/// Thread-safe handle to Spotirust's SQLite database.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Open or create the on-disk SQLite database and apply pending migrations.
    pub fn open(path: &Path) -> Result<Self, DatabaseError> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.configure_and_migrate()?;
        Ok(db)
    }

    /// Open an isolated in-memory SQLite database (ideal for automated tests).
    pub fn open_in_memory() -> Result<Self, DatabaseError> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.configure_and_migrate()?;
        Ok(db)
    }

    fn with_conn<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T, DatabaseError>,
    ) -> Result<T, DatabaseError> {
        let mut guard = self.conn.lock().map_err(|_| DatabaseError::LockPoisoned)?;
        f(&mut guard)
    }

    fn configure_and_migrate(&self) -> Result<(), DatabaseError> {
        self.with_conn(|conn| {
            conn.execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA foreign_keys = ON;
                 PRAGMA busy_timeout = 5000;
                 CREATE TABLE IF NOT EXISTS schema_migrations (
                     version INTEGER PRIMARY KEY NOT NULL,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )?;

            for &(version, sql) in MIGRATIONS {
                let already_applied: Option<i64> = conn
                    .query_row(
                        "SELECT version FROM schema_migrations WHERE version = ?1",
                        params![version],
                        |row| row.get(0),
                    )
                    .optional()?;

                if already_applied.is_none() {
                    let tx = conn.transaction()?;
                    tx.execute_batch(sql)?;
                    tx.execute(
                        "INSERT INTO schema_migrations (version, applied_at_ms) VALUES (?1, ?2)",
                        params![version, now_ms()],
                    )?;
                    tx.commit()?;
                    tracing::info!(version, "Applied SQLite migration");
                }
            }
            Ok(())
        })
    }

    pub fn schema_version(&self) -> Result<i64, DatabaseError> {
        self.with_conn(|conn| {
            let ver: Option<i64> = conn
                .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
                    r.get(0)
                })
                .optional()?
                .flatten();
            Ok(ver.unwrap_or(0))
        })
    }

    // -------------------------------------------------------------------------
    // User profile (metadata only — never tokens)
    // -------------------------------------------------------------------------

    pub fn save_user_profile(&self, profile: &UserProfile) -> Result<(), DatabaseError> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM user_profile", [])?;
            conn.execute(
                "INSERT INTO user_profile (id, account_id, display_name, avatar_url, uri, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    profile.id,
                    profile.account_id,
                    profile.display_name,
                    profile.avatar_url,
                    profile.uri,
                    now_ms()
                ],
            )?;
            Ok(())
        })
    }

    pub fn load_user_profile(&self) -> Result<Option<UserProfile>, DatabaseError> {
        self.with_conn(|conn| {
            let row = conn
                .query_row(
                    "SELECT id, account_id, display_name, avatar_url, uri FROM user_profile LIMIT 1",
                    [],
                    |r| {
                        Ok(UserProfile {
                            id: r.get(0)?,
                            account_id: r.get(1)?,
                            display_name: r.get(2)?,
                            avatar_url: r.get(3)?,
                            uri: r.get(4)?,
                        })
                    },
                )
                .optional()?;
            Ok(row)
        })
    }

    pub fn delete_user_profile(&self) -> Result<(), DatabaseError> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM user_profile", [])?;
            Ok(())
        })
    }

    // -------------------------------------------------------------------------
    // Tracks, Albums, Playlists, Recently Played
    // -------------------------------------------------------------------------

    pub fn upsert_tracks(&self, tracks: &[Track]) -> Result<(), DatabaseError> {
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            let ts = now_ms();
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO cached_tracks
                     (id, uri, title, artists_json, album_id, album_name, duration_ms, isrc, artwork_url, explicit, liked, local_path, cached_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT(id) DO UPDATE SET
                        uri=excluded.uri,
                        title=excluded.title,
                        artists_json=excluded.artists_json,
                        album_id=excluded.album_id,
                        album_name=excluded.album_name,
                        duration_ms=excluded.duration_ms,
                        isrc=COALESCE(excluded.isrc, cached_tracks.isrc),
                        artwork_url=COALESCE(excluded.artwork_url, cached_tracks.artwork_url),
                        explicit=excluded.explicit,
                        liked=excluded.liked,
                        local_path=COALESCE(excluded.local_path, cached_tracks.local_path),
                        cached_at_ms=excluded.cached_at_ms",
                )?;
                for track in tracks {
                    let artists_json = serde_json::to_string(&track.artists)?;
                    let local_path = track
                        .local_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string());
                    stmt.execute(params![
                        track.id,
                        track.uri,
                        track.title,
                        artists_json,
                        track.album_id,
                        track.album_name,
                        track.duration_ms as i64,
                        track.isrc,
                        track.artwork_url,
                        i32::from(track.explicit),
                        i32::from(track.liked),
                        local_path,
                        ts
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn load_liked_tracks(&self, limit: usize) -> Result<Vec<Track>, DatabaseError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, uri, title, artists_json, album_id, album_name, duration_ms, isrc, artwork_url, explicit, liked, local_path
                 FROM cached_tracks WHERE liked = 1 ORDER BY cached_at_ms DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], map_track_row)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn set_track_liked(&self, track_id: &str, liked: bool) -> Result<(), DatabaseError> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE cached_tracks SET liked = ?1, cached_at_ms = ?2 WHERE id = ?3",
                params![i32::from(liked), now_ms(), track_id],
            )?;
            Ok(())
        })
    }

    pub fn upsert_albums(&self, albums: &[Album]) -> Result<(), DatabaseError> {
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            let ts = now_ms();
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO cached_albums
                     (id, uri, name, artists_json, release_date, total_tracks, artwork_url, cached_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(id) DO UPDATE SET
                        uri=excluded.uri,
                        name=excluded.name,
                        artists_json=excluded.artists_json,
                        release_date=excluded.release_date,
                        total_tracks=excluded.total_tracks,
                        artwork_url=excluded.artwork_url,
                        cached_at_ms=excluded.cached_at_ms",
                )?;
                for album in albums {
                    let artists_json = serde_json::to_string(&album.artists)?;
                    stmt.execute(params![
                        album.id,
                        album.uri,
                        album.name,
                        artists_json,
                        album.release_date,
                        i64::from(album.total_tracks),
                        album.artwork_url,
                        ts
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn load_cached_albums(&self, limit: usize) -> Result<Vec<Album>, DatabaseError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, uri, name, artists_json, release_date, total_tracks, artwork_url
                 FROM cached_albums ORDER BY cached_at_ms DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], |r| {
                let artists_json: String = r.get(3)?;
                let artists: Vec<String> = serde_json::from_str(&artists_json).unwrap_or_default();
                Ok(Album {
                    id: r.get(0)?,
                    uri: r.get(1)?,
                    name: r.get(2)?,
                    artists,
                    release_date: r.get(4)?,
                    total_tracks: r.get::<_, i64>(5)? as u32,
                    artwork_url: r.get(6)?,
                    accent_rgb: None,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn upsert_playlists(&self, playlists: &[Playlist]) -> Result<(), DatabaseError> {
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            let ts = now_ms();
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO cached_playlists
                     (id, uri, name, description, owner_name, owner_id, snapshot_id, total_tracks, artwork_url, is_collaborative, is_public, cached_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                     ON CONFLICT(id) DO UPDATE SET
                        uri=excluded.uri,
                        name=excluded.name,
                        description=excluded.description,
                        owner_name=excluded.owner_name,
                        owner_id=excluded.owner_id,
                        snapshot_id=excluded.snapshot_id,
                        total_tracks=excluded.total_tracks,
                        artwork_url=excluded.artwork_url,
                        is_collaborative=excluded.is_collaborative,
                        is_public=excluded.is_public,
                        cached_at_ms=excluded.cached_at_ms",
                )?;
                for pl in playlists {
                    stmt.execute(params![
                        pl.id,
                        pl.uri,
                        pl.name,
                        pl.description,
                        pl.owner_name,
                        pl.owner_id,
                        pl.snapshot_id,
                        i64::from(pl.total_tracks),
                        pl.artwork_url,
                        i32::from(pl.collaborative),
                        pl.public.map(i32::from),
                        ts
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn load_cached_playlists(&self, limit: usize) -> Result<Vec<Playlist>, DatabaseError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, uri, name, description, owner_name, owner_id, snapshot_id, total_tracks, artwork_url, is_collaborative, is_public
                 FROM cached_playlists ORDER BY cached_at_ms DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], |r| {
                Ok(Playlist {
                    id: r.get(0)?,
                    uri: r.get(1)?,
                    name: r.get(2)?,
                    description: r.get(3)?,
                    owner_name: r.get(4)?,
                    owner_id: r.get(5)?,
                    snapshot_id: r.get(6)?,
                    total_tracks: r.get::<_, i64>(7)? as u32,
                    artwork_url: r.get(8)?,
                    collaborative: r.get::<_, i32>(9)? != 0,
                    public: r.get::<_, Option<i32>>(10)?.map(|v| v != 0),
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn record_recently_played(
        &self,
        track: &Track,
        context_uri: Option<&str>,
    ) -> Result<(), DatabaseError> {
        let track_json = serde_json::to_string(track)?;
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO recently_played (track_id, played_at_ms, context_uri, track_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![track.id, now_ms(), context_uri, track_json],
            )?;
            // Cap history size to 200 recent plays
            conn.execute(
                "DELETE FROM recently_played WHERE id NOT IN (
                    SELECT id FROM recently_played ORDER BY played_at_ms DESC LIMIT 200
                 )",
                [],
            )?;
            Ok(())
        })
    }

    pub fn load_recently_played(&self, limit: usize) -> Result<Vec<Track>, DatabaseError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT track_json FROM recently_played ORDER BY played_at_ms DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], |r| r.get::<_, String>(0))?;
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for row in rows {
                let raw = row?;
                if let Ok(track) = serde_json::from_str::<Track>(&raw)
                    && seen.insert(track.id.clone())
                {
                    out.push(track);
                }
            }
            Ok(out)
        })
    }

    // -------------------------------------------------------------------------
    // Lyrics cache
    // -------------------------------------------------------------------------

    pub fn save_lyrics_cache(&self, record: &CachedLyricsRecord) -> Result<(), DatabaseError> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO lyrics_cache
                 (cache_key, track_id, title, artist, album, duration_ms, format, source, raw_content, offset_ms, instrumental, fetched_at_ms, expires_at_ms, attribution_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT(cache_key) DO UPDATE SET
                    track_id=excluded.track_id,
                    title=excluded.title,
                    artist=excluded.artist,
                    album=excluded.album,
                    duration_ms=excluded.duration_ms,
                    format=excluded.format,
                    source=excluded.source,
                    raw_content=excluded.raw_content,
                    offset_ms=excluded.offset_ms,
                    instrumental=excluded.instrumental,
                    fetched_at_ms=excluded.fetched_at_ms,
                    expires_at_ms=excluded.expires_at_ms,
                    attribution_json=excluded.attribution_json",
                params![
                    record.cache_key,
                    record.track_id,
                    record.title,
                    record.artist,
                    record.album,
                    record.duration_ms.map(|d| d as i64),
                    record.format,
                    record.source,
                    record.raw_content,
                    record.offset_ms,
                    i32::from(record.instrumental),
                    record.fetched_at_ms,
                    record.expires_at_ms,
                    record.attribution_json
                ],
            )?;
            Ok(())
        })
    }

    pub fn load_lyrics_cache(
        &self,
        cache_key: &str,
    ) -> Result<Option<CachedLyricsRecord>, DatabaseError> {
        let now = now_ms();
        self.with_conn(|conn| {
            let rec = conn
                .query_row(
                    "SELECT cache_key, track_id, title, artist, album, duration_ms, format, source, raw_content, offset_ms, instrumental, fetched_at_ms, expires_at_ms, attribution_json
                     FROM lyrics_cache WHERE cache_key = ?1 AND expires_at_ms > ?2",
                    params![cache_key, now],
                    |r| {
                        Ok(CachedLyricsRecord {
                            cache_key: r.get(0)?,
                            track_id: r.get(1)?,
                            title: r.get(2)?,
                            artist: r.get(3)?,
                            album: r.get(4)?,
                            duration_ms: r.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u64),
                            format: r.get(6)?,
                            source: r.get(7)?,
                            raw_content: r.get(8)?,
                            offset_ms: r.get(9)?,
                            instrumental: r.get::<_, i32>(10)? != 0,
                            fetched_at_ms: r.get(11)?,
                            expires_at_ms: r.get(12)?,
                            attribution_json: r.get(13)?,
                        })
                    },
                )
                .optional()?;
            Ok(rec)
        })
    }

    pub fn update_lyrics_offset(
        &self,
        cache_key: &str,
        offset_ms: i64,
    ) -> Result<(), DatabaseError> {
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE lyrics_cache SET offset_ms = ?1 WHERE cache_key = ?2",
                params![offset_ms, cache_key],
            )?;
            Ok(())
        })
    }

    // -------------------------------------------------------------------------
    // Settings & Playback Preferences
    // -------------------------------------------------------------------------

    pub fn save_settings(&self, settings: &UserSettings) -> Result<(), DatabaseError> {
        let json = serde_json::to_string(settings)?;
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO app_settings (key, value_json, updated_at_ms)
                 VALUES ('user_settings', ?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json, updated_at_ms=excluded.updated_at_ms",
                params![json, now_ms()],
            )?;
            Ok(())
        })
    }

    pub fn load_settings(&self) -> Result<UserSettings, DatabaseError> {
        self.with_conn(|conn| {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT value_json FROM app_settings WHERE key = 'user_settings'",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            match raw {
                Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
                None => Ok(UserSettings::default()),
            }
        })
    }

    pub fn save_playback_preferences(
        &self,
        prefs: &PlaybackPreferences,
    ) -> Result<(), DatabaseError> {
        let last_track_json = prefs
            .last_track
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let last_queue_json = serde_json::to_string(&prefs.last_queue)?;
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE playback_preferences
                 SET volume = ?1, shuffle = ?2, repeat_mode = ?3, backend_kind = ?4,
                     last_track_json = ?5, last_position_ms = ?6, last_queue_json = ?7, updated_at_ms = ?8
                 WHERE id = 1",
                params![
                    f64::from(prefs.volume.clamp(0.0, 1.0)),
                    i32::from(prefs.shuffle),
                    prefs.repeat_mode.as_str(),
                    prefs.backend_kind.as_str(),
                    last_track_json,
                    prefs.last_position_ms as i64,
                    last_queue_json,
                    now_ms()
                ],
            )?;
            Ok(())
        })
    }

    pub fn load_playback_preferences(&self) -> Result<PlaybackPreferences, DatabaseError> {
        self.with_conn(|conn| {
            let row = conn
                .query_row(
                    "SELECT volume, shuffle, repeat_mode, backend_kind, last_track_json, last_position_ms, last_queue_json
                     FROM playback_preferences WHERE id = 1",
                    [],
                    |r| {
                        let volume: f64 = r.get(0)?;
                        let shuffle: i32 = r.get(1)?;
                        let repeat_raw: String = r.get(2)?;
                        let backend_raw: String = r.get(3)?;
                        let last_track_raw: Option<String> = r.get(4)?;
                        let last_position_ms: i64 = r.get(5)?;
                        let last_queue_raw: String = r.get(6)?;
                        Ok(PlaybackPreferences {
                            volume: (volume as f32).clamp(0.0, 1.0),
                            shuffle: shuffle != 0,
                            repeat_mode: RepeatMode::from_db_str(&repeat_raw),
                            backend_kind: AudioBackendKind::from_db_str(&backend_raw),
                            last_track: last_track_raw
                                .and_then(|s| serde_json::from_str::<Track>(&s).ok()),
                            last_position_ms: last_position_ms.max(0) as u64,
                            last_queue: serde_json::from_str::<Vec<Track>>(&last_queue_raw)
                                .unwrap_or_default(),
                        })
                    },
                )
                .optional()?;
            Ok(row.unwrap_or_default())
        })
    }

    // -------------------------------------------------------------------------
    // Local Music Library Persistence (instant startup without full rescans)
    // -------------------------------------------------------------------------

    pub fn upsert_local_tracks(
        &self,
        entries: &[PersistedLocalTrack],
    ) -> Result<(), DatabaseError> {
        if entries.is_empty() {
            return Ok(());
        }
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            let ts = now_ms();
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO local_tracks
                     (path, file_size, mtime_secs, title, artist, album, duration_ms, artwork_url, track_json, scanned_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                     ON CONFLICT(path) DO UPDATE SET
                        file_size=excluded.file_size,
                        mtime_secs=excluded.mtime_secs,
                        title=excluded.title,
                        artist=excluded.artist,
                        album=excluded.album,
                        duration_ms=excluded.duration_ms,
                        artwork_url=excluded.artwork_url,
                        track_json=excluded.track_json,
                        scanned_at_ms=excluded.scanned_at_ms",
                )?;
                for entry in entries {
                    let track_json = serde_json::to_string(&entry.track)?;
                    stmt.execute(params![
                        entry.path,
                        entry.file_size as i64,
                        entry.mtime_secs,
                        entry.track.title,
                        entry.track.artist_display(),
                        entry.track.album_name,
                        entry.track.duration_ms as i64,
                        entry.track.artwork_url,
                        track_json,
                        ts
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn load_local_tracks(&self) -> Result<Vec<Track>, DatabaseError> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT track_json FROM local_tracks ORDER BY LOWER(title) ASC, LOWER(artist) ASC",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                let raw = row?;
                if let Ok(track) = serde_json::from_str::<Track>(&raw) {
                    out.push(track);
                }
            }
            Ok(out)
        })
    }

    pub fn load_local_track_fingerprints(
        &self,
    ) -> Result<std::collections::HashMap<String, PersistedLocalTrack>, DatabaseError> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT path, file_size, mtime_secs, track_json FROM local_tracks")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?.max(0) as u64,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            let mut map = std::collections::HashMap::new();
            for row in rows {
                let (path, file_size, mtime_secs, raw) = row?;
                if let Ok(track) = serde_json::from_str::<Track>(&raw) {
                    map.insert(
                        path.clone(),
                        PersistedLocalTrack {
                            path,
                            file_size,
                            mtime_secs,
                            track,
                        },
                    );
                }
            }
            Ok(map)
        })
    }

    /// Remove persisted local tracks whose paths are absent from `keep_paths` (e.g. deleted files
    /// or removed folders during an explicit Rescan).
    pub fn prune_local_tracks_outside(
        &self,
        keep_paths: &std::collections::HashSet<String>,
    ) -> Result<usize, DatabaseError> {
        self.with_conn(|conn| {
            let existing: Vec<String> = {
                let mut stmt = conn.prepare("SELECT path FROM local_tracks")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                out
            };

            let tx = conn.transaction()?;
            let mut removed = 0usize;
            for path in existing {
                if !keep_paths.contains(&path) {
                    removed +=
                        tx.execute("DELETE FROM local_tracks WHERE path = ?1", params![path])?;
                }
            }
            tx.commit()?;
            Ok(removed)
        })
    }

    /// Remove persisted local tracks belonging to a specific directory prefix.
    pub fn remove_local_tracks_in_dir(&self, dir: &Path) -> Result<usize, DatabaseError> {
        let dir_str = dir.to_string_lossy().to_string();
        self.with_conn(|conn| {
            let existing: Vec<String> = {
                let mut stmt = conn.prepare("SELECT path FROM local_tracks")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r?);
                }
                out
            };
            let tx = conn.transaction()?;
            let mut removed = 0usize;
            for path in existing {
                if Path::new(&path).starts_with(&dir_str) {
                    removed +=
                        tx.execute("DELETE FROM local_tracks WHERE path = ?1", params![path])?;
                }
            }
            tx.commit()?;
            Ok(removed)
        })
    }

    // -------------------------------------------------------------------------
    // Cache management & application reset
    // -------------------------------------------------------------------------

    pub fn cache_stats(&self) -> Result<CacheStats, DatabaseError> {
        self.with_conn(|conn| {
            let count = |table: &str| -> Result<usize, rusqlite::Error> {
                let sql = format!("SELECT COUNT(*) FROM {table}");
                conn.query_row(&sql, [], |r| r.get::<_, i64>(0))
                    .map(|v| v.max(0) as usize)
            };
            let schema_version: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            Ok(CacheStats {
                cached_tracks: count("cached_tracks")?,
                local_tracks: count("local_tracks")?,
                cached_albums: count("cached_albums")?,
                cached_playlists: count("cached_playlists")?,
                cached_lyrics: count("lyrics_cache")?,
                recently_played: count("recently_played")?,
                schema_version,
            })
        })
    }

    /// Clears only cached lyrics entries while preserving catalog cache, artwork, settings, and local library.
    pub fn clear_lyrics_cache(&self) -> Result<usize, DatabaseError> {
        self.with_conn(|conn| {
            let removed = conn.execute("DELETE FROM lyrics_cache", [])?;
            Ok(removed)
        })
    }

    /// Clears cached catalog metadata and cached lyrics while preserving settings, local library, and session profile.
    pub fn clear_cache(&self) -> Result<usize, DatabaseError> {
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            let mut removed = 0usize;
            for table in [
                "cached_tracks",
                "cached_albums",
                "cached_playlists",
                "lyrics_cache",
            ] {
                let sql = format!("DELETE FROM {table}");
                removed += tx.execute(&sql, [])?;
            }
            tx.commit()?;
            Ok(removed)
        })
    }

    /// Wipes all user data, caches, local library index, history, settings, and playback preferences.
    pub fn reset_application(&self) -> Result<(), DatabaseError> {
        self.with_conn(|conn| {
            let tx = conn.transaction()?;
            tx.execute_batch(
                "DELETE FROM user_profile;
                 DELETE FROM cached_tracks;
                 DELETE FROM local_tracks;
                 DELETE FROM cached_albums;
                 DELETE FROM cached_playlists;
                 DELETE FROM recently_played;
                 DELETE FROM lyrics_cache;
                 DELETE FROM app_settings;
                 UPDATE playback_preferences
                 SET volume = 0.8, shuffle = 0, repeat_mode = 'off', backend_kind = 'local_rodio',
                     last_track_json = NULL, last_position_ms = 0, last_queue_json = '[]', updated_at_ms = 0
                 WHERE id = 1;",
            )?;
            tx.commit()?;
            Ok(())
        })
    }
}

fn map_track_row(r: &rusqlite::Row<'_>) -> Result<Track, rusqlite::Error> {
    let artists_json: String = r.get(3)?;
    let artists: Vec<String> = serde_json::from_str(&artists_json).unwrap_or_default();
    let local_path: Option<String> = r.get(11)?;
    Ok(Track {
        id: r.get(0)?,
        uri: r.get(1)?,
        title: r.get(2)?,
        artists,
        album_id: r.get(4)?,
        album_name: r.get(5)?,
        duration_ms: r.get::<_, i64>(6)?.max(0) as u64,
        isrc: r.get(7)?,
        artwork_url: r.get(8)?,
        explicit: r.get::<_, i32>(9)? != 0,
        liked: r.get::<_, i32>(10)? != 0,
        local_path: local_path.map(std::path::PathBuf::from),
        accent_rgb: None,
    })
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}
