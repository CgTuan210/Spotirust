//! SQLite-backed lyrics cache with deterministic normalized keys, TTL expiry,
//! attribution persistence, and lossless re-parsing of TTML, Lyricsfile YAML,
//! Musixmatch RichSync JSON, Enhanced/Standard LRC, and Plain text.

use crate::core::models::Track;
use crate::lyrics::lrc::{parse_lrc, parse_plain_lyrics};
use crate::lyrics::lyricsfile::parse_lyricsfile;
use crate::lyrics::matcher::normalize_text;
use crate::lyrics::model::{LyricsAttribution, LyricsDocument, LyricsFormat};
use crate::lyrics::musixmatch::parse_musixmatch_richsync;
use crate::lyrics::ttml::parse_ttml;
use crate::storage::database::{CachedLyricsRecord, Database, now_ms};

const HIT_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000; // 30 days
const MISS_TTL_MS: i64 = 6 * 60 * 60 * 1000; // 6 hours

/// Deterministic cache key combining ISRC (if available) or normalized artist + title + duration bucket.
pub fn build_lyrics_cache_key(track: &Track) -> String {
    if let Some(isrc) = track.isrc.as_deref().filter(|s| !s.trim().is_empty()) {
        return format!("isrc:{}", isrc.trim().to_ascii_uppercase());
    }
    let artist = normalize_text(track.primary_artist());
    let title = normalize_text(&track.title);
    let dur_bucket = track.duration_ms / 2_000;
    format!("meta:{artist}::{title}::{dur_bucket}")
}

/// High-level lyrics cache backed by Spotirust's SQLite database.
#[derive(Clone)]
pub struct LyricsCache {
    db: Database,
}

impl LyricsCache {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Look up a cached lyrics entry and re-parse its stored raw content losslessly.
    /// Returns `Some(Ok(doc))` on hit, `Some(Err(reason))` on negative cache hit, or `None` on miss.
    pub fn get(&self, track: &Track) -> Option<Result<LyricsDocument, String>> {
        let key = build_lyrics_cache_key(track);
        let record = self.db.load_lyrics_cache(&key).ok().flatten()?;
        let attribution: Option<Box<LyricsAttribution>> = record
            .attribution_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<LyricsAttribution>(s).ok())
            .map(Box::new);

        if record.instrumental {
            return Some(Ok(LyricsDocument {
                lines: Vec::new(),
                offset_ms: record.offset_ms,
                format: LyricsFormat::PlainText,
                source: record.source,
                instrumental: true,
                title: Some(record.title),
                artist: Some(record.artist),
                album: record.album,
                isrc: track.isrc.clone(),
                attribution,
            }));
        }

        let Some(raw) = record.raw_content else {
            return Some(Err("No lyrics cached for this track".to_string()));
        };

        let parsed = match LyricsFormat::from_db_str(&record.format) {
            LyricsFormat::Ttml | LyricsFormat::TtmlLine => {
                parse_ttml(&raw, record.source.clone()).ok()
            }
            LyricsFormat::LyricsfileWord | LyricsFormat::LyricsfileLine => {
                parse_lyricsfile(&raw, record.source.clone()).ok()
            }
            LyricsFormat::MusixmatchRichSync => {
                parse_musixmatch_richsync(&raw, record.source.clone()).ok()
            }
            LyricsFormat::EnhancedLrc | LyricsFormat::StandardLrc => {
                parse_lrc(&raw, record.source.clone()).ok()
            }
            LyricsFormat::PlainText => Some(parse_plain_lyrics(&raw, record.source.clone())),
        };

        parsed.map(|mut doc| {
            doc.offset_ms = record.offset_ms;
            if doc.title.is_none() {
                doc.title = Some(record.title);
            }
            if doc.artist.is_none() {
                doc.artist = Some(record.artist);
            }
            if doc.album.is_none() {
                doc.album = record.album;
            }
            if attribution.is_some() {
                doc.attribution = attribution;
            }
            Ok(doc)
        })
    }

    /// Store raw lyrics payload (TTML, Lyricsfile YAML, RichSync JSON, LRC, or plain text) in SQLite.
    #[allow(clippy::too_many_arguments)]
    pub fn put_raw(
        &self,
        track: &Track,
        format: LyricsFormat,
        source: &str,
        raw_content: Option<String>,
        offset_ms: i64,
        instrumental: bool,
        attribution: Option<&LyricsAttribution>,
    ) {
        let now = now_ms();
        let ttl = if raw_content.is_some() || instrumental {
            HIT_TTL_MS
        } else {
            MISS_TTL_MS
        };
        let attribution_json = attribution.and_then(|a| serde_json::to_string(a).ok());
        let record = CachedLyricsRecord {
            cache_key: build_lyrics_cache_key(track),
            track_id: Some(track.id.clone()),
            title: track.title.clone(),
            artist: track.primary_artist().to_string(),
            album: if track.album_name.is_empty() {
                None
            } else {
                Some(track.album_name.clone())
            },
            duration_ms: Some(track.duration_ms),
            format: format.as_str().to_string(),
            source: source.to_string(),
            raw_content,
            offset_ms,
            instrumental,
            fetched_at_ms: now,
            expires_at_ms: now + ttl,
            attribution_json,
        };
        if let Err(err) = self.db.save_lyrics_cache(&record) {
            tracing::warn!(error = %err, "Failed saving lyrics entry to SQLite cache");
        }
    }

    pub fn update_offset(&self, track: &Track, offset_ms: i64) {
        let key = build_lyrics_cache_key(track);
        let _ = self.db.update_lyrics_offset(&key, offset_ms);
    }

    /// Remove all cached lyrics entries without touching tracks, playlists, or artwork.
    pub fn clear(&self) -> usize {
        self.db.clear_lyrics_cache().unwrap_or(0)
    }
}
