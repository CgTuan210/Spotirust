//! Real Unison Crowdsourced Lyrics Provider (`https://unison.betterlyrics.org`).
//!
//! ## License & Terms Verification (Nov 2026)
//! - Repository / Spec: `https://github.com/better-lyrics/unison`
//! - Read API (`GET /lyrics`, `GET /lyrics/{id}`, `GET /lyrics/search`) is free without an API key
//!   for individuals, non-profits, and **open-source projects under OSI-approved licenses
//!   (Spotirust = MIT)** where Unison lyrics remain free to all users. Commercial use is prohibited.
//! - **Mandatory Attribution (effective by Feb 7, 2027):** Clients displaying Unison lyrics MUST
//!   display `"Lyrics from Unison"` linked to `https://unison.betterlyrics.org/song/<videoId>` AND
//!   the submitter's display name linked to `https://unison.betterlyrics.org/curator/<keyId>`.
//!   Spotirust persists this in `LyricsDocument.attribution` (SQLite-cached) and renders it visibly
//!   in the lyrics footer.
//! - Supports `ETag` / `If-None-Match` conditional caching, `429 RATE_LIMITED`, and `503 LOOKUP_PENDING`.

use crate::lyrics::lrc::{parse_lrc, parse_plain_lyrics};
use crate::lyrics::matcher::{TrackMatchQuery, evaluate_candidate};
use crate::lyrics::model::{LyricsAttribution, LyricsFormat};
use crate::lyrics::provider::{
    CandidatePayload, FetchedLyrics, LyricsCandidate, LyricsError, LyricsProvider,
    LyricsSourceKind, TrackMetadata, parse_retry_after,
};
use crate::lyrics::ttml::parse_ttml;
use async_trait::async_trait;
use reqwest::StatusCode;
use reqwest::header::{ETAG, IF_NONE_MATCH};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const UNISON_DEFAULT_BASE_URL: &str = "https://unison.betterlyrics.org";
const UNISON_DEFAULT_TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnisonSubmitter {
    key_id: String,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnisonSingleEnvelope {
    success: bool,
    #[serde(default)]
    data: Option<UnisonLyricsData>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnisonSearchEnvelope {
    success: bool,
    #[serde(default)]
    data: Vec<UnisonSearchItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnisonLyricsData {
    id: u64,
    #[serde(default)]
    video_id: Option<String>,
    song: String,
    artist: String,
    #[serde(default)]
    album: Option<String>,
    #[serde(default)]
    isrc: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    lyrics: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    sync_type: Option<String>,
    #[serde(default)]
    confidence: Option<String>,
    #[serde(default)]
    matched_by: Option<String>,
    #[serde(default)]
    submitter: Option<UnisonSubmitter>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnisonSearchItem {
    id: u64,
    #[serde(default)]
    video_id: Option<String>,
    song: String,
    artist: String,
    #[serde(default)]
    album: Option<String>,
    #[serde(default)]
    isrc: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    sync_type: Option<String>,
    #[serde(default)]
    confidence: Option<String>,
}

#[derive(Clone)]
struct CachedEtagEntry {
    etag: String,
    data: UnisonLyricsData,
}

/// Real Unison API provider serving community-curated word-level TTML, LRC, and plain lyrics
/// under Unison's open-source MIT-permitted read terms.
#[derive(Clone)]
pub struct UnisonProvider {
    http: reqwest::Client,
    base_url: String,
    etag_cache: Arc<Mutex<HashMap<String, CachedEtagEntry>>>,
}

impl Default for UnisonProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UnisonProvider {
    pub fn new() -> Self {
        Self::with_timeout(UNISON_DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "Spotirust/{} (https://github.com/CgTuan210/Spotirust)",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: UNISON_DEFAULT_BASE_URL.to_string(),
            etag_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Perform a single `GET /lyrics` query with optional `If-None-Match` ETag revalidation.
    async fn fetch_direct_lookup(
        &self,
        cache_key: &str,
        params: &[(&str, String)],
    ) -> Result<Option<UnisonLyricsData>, LyricsError> {
        let url = format!("{}/lyrics", self.base_url);
        let prev_etag = self
            .etag_cache
            .lock()
            .ok()
            .and_then(|m| m.get(cache_key).map(|e| e.etag.clone()));

        let mut req = self.http.get(&url).query(params);
        if let Some(etag) = &prev_etag {
            req = req.header(IF_NONE_MATCH, etag);
        }

        let resp = req.send().await.map_err(map_reqwest_err)?;
        let status = resp.status();

        if status == StatusCode::NOT_MODIFIED {
            return Ok(self
                .etag_cache
                .lock()
                .ok()
                .and_then(|m| m.get(cache_key).map(|e| e.data.clone())));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&resp, 30),
            });
        }
        if status == StatusCode::SERVICE_UNAVAILABLE {
            // Unison returns 503 LOOKUP_PENDING when resolving an unindexed Spotify ID asynchronously.
            // Treat as a soft miss on this lookup strategy so subsequent metadata lookup can still run.
            let body = resp.text().await.unwrap_or_default();
            if body.contains("LOOKUP_PENDING") {
                tracing::debug!(cache_key, "Unison Spotify ID lookup pending (503)");
                return Ok(None);
            }
            return Err(LyricsError::RateLimited {
                retry_after_secs: 15,
            });
        }
        if status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(LyricsError::Network(format!("Unison HTTP {status}")));
        }

        let new_etag = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        let envelope: UnisonSingleEnvelope = resp
            .json()
            .await
            .map_err(|e| LyricsError::Parse(e.to_string()))?;

        if !envelope.success {
            if envelope.code.as_deref() == Some("RATE_LIMITED") {
                return Err(LyricsError::RateLimited {
                    retry_after_secs: 30,
                });
            }
            if envelope.code.as_deref() == Some("NOT_FOUND") {
                return Ok(None);
            }
            return Err(LyricsError::Network(envelope.error.unwrap_or_else(|| {
                "Unison API returned unsuccessful response".to_string()
            })));
        }

        if let Some(data) = envelope.data {
            if let Some(etag) = new_etag
                && let Ok(mut map) = self.etag_cache.lock()
            {
                if map.len() > 256 {
                    map.clear();
                }
                map.insert(
                    cache_key.to_string(),
                    CachedEtagEntry {
                        etag,
                        data: data.clone(),
                    },
                );
            }
            return Ok(Some(data));
        }

        Ok(None)
    }

    async fn fetch_by_id(&self, id: &str) -> Result<UnisonLyricsData, LyricsError> {
        let url = format!("{}/lyrics/{id}", self.base_url);
        let resp = self.http.get(&url).send().await.map_err(map_reqwest_err)?;
        let status = resp.status();

        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&resp, 30),
            });
        }
        if status == StatusCode::SERVICE_UNAVAILABLE {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&resp, 15),
            });
        }
        if status == StatusCode::NOT_FOUND {
            return Err(LyricsError::NotFound);
        }
        if !status.is_success() {
            return Err(LyricsError::Network(format!("Unison HTTP {status}")));
        }

        let envelope: UnisonSingleEnvelope = resp
            .json()
            .await
            .map_err(|e| LyricsError::Parse(e.to_string()))?;
        if !envelope.success {
            return Err(LyricsError::NotFound);
        }
        envelope.data.ok_or(LyricsError::NotFound)
    }

    fn build_attribution(data: &UnisonLyricsData) -> LyricsAttribution {
        let source_url = data
            .video_id
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .map(|vid| format!("https://unison.betterlyrics.org/song/{vid}"))
            .or_else(|| Some("https://unison.betterlyrics.org".to_string()));

        let (submitter_name, submitter_url) = match &data.submitter {
            Some(sub) => {
                let name = sub
                    .display_name
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        let short: String = sub.key_id.chars().take(10).collect();
                        format!("curator:{short}")
                    });
                let url = format!("https://unison.betterlyrics.org/curator/{}", sub.key_id);
                (Some(name), Some(url))
            }
            None => (None, None),
        };

        let disclaimer = if data.matched_by.as_deref() == Some("linked_video") {
            Some(
                "Matched via linked YouTube Music video — use ±50ms offset if timing differs slightly from Spotify."
                    .to_string(),
            )
        } else {
            None
        };

        LyricsAttribution {
            provider_label: "Lyrics from Unison".to_string(),
            source_url,
            submitter_name,
            submitter_url,
            copyright: None,
            disclaimer,
        }
    }

    fn detect_format(format: Option<&str>, sync_type: Option<&str>, raw: &str) -> LyricsFormat {
        let fmt = format.unwrap_or("").to_ascii_lowercase();
        let sync = sync_type.unwrap_or("").to_ascii_lowercase();

        if fmt == "ttml" || raw.trim_start().starts_with('<') {
            if sync == "linesync" {
                LyricsFormat::TtmlLine
            } else {
                LyricsFormat::Ttml
            }
        } else if fmt == "lrc" {
            if sync == "richsync" || (raw.contains('<') && raw.contains('>')) {
                LyricsFormat::EnhancedLrc
            } else {
                LyricsFormat::StandardLrc
            }
        } else if sync == "plain" || fmt == "plain" {
            LyricsFormat::PlainText
        } else if raw.contains('[') && raw.contains(']') {
            LyricsFormat::StandardLrc
        } else {
            LyricsFormat::PlainText
        }
    }

    fn data_to_candidate(data: UnisonLyricsData) -> Option<LyricsCandidate> {
        let raw = data.lyrics.as_deref()?.trim();
        if raw.is_empty() {
            return None;
        }
        let fmt = Self::detect_format(data.format.as_deref(), data.sync_type.as_deref(), raw);
        let attribution = Self::build_attribution(&data);
        let dur_ms = data
            .duration
            .filter(|d| *d > 0.0)
            .map(|d| (d * 1000.0).round() as u64);
        let source_label = match data.confidence.as_deref() {
            Some("high") => "Unison".to_string(),
            Some("medium") => "Unison (Community)".to_string(),
            Some("low") => "Unison (Unverified)".to_string(),
            _ => "Unison".to_string(),
        };

        Some(LyricsCandidate {
            id: format!("unison:{}", data.id),
            provider_name: "Unison".to_string(),
            source_kind: LyricsSourceKind::Unison,
            title: data.song,
            artist: data.artist,
            album: data.album,
            duration_ms: dur_ms,
            isrc: data.isrc,
            has_word_sync: fmt.is_word_synced(),
            has_line_sync: fmt.is_time_synced(),
            instrumental: false,
            format_hint: fmt,
            match_score: 0.0,
            match_reason: String::new(),
            payload: CandidatePayload::InlineRaw {
                format: fmt,
                raw_content: raw.to_string(),
                source_label,
                attribution: Some(attribution),
            },
        })
    }

    fn parse_data_into_document(data: &UnisonLyricsData) -> Result<FetchedLyrics, LyricsError> {
        let raw = data
            .lyrics
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(LyricsError::NotFound)?;

        let fmt = Self::detect_format(data.format.as_deref(), data.sync_type.as_deref(), raw);
        let source_label = "Unison";
        let mut doc = match fmt {
            LyricsFormat::Ttml | LyricsFormat::TtmlLine => {
                parse_ttml(raw, source_label).map_err(|e| LyricsError::Parse(e.to_string()))?
            }
            LyricsFormat::EnhancedLrc | LyricsFormat::StandardLrc => {
                parse_lrc(raw, source_label).map_err(|e| LyricsError::Parse(e.to_string()))?
            }
            _ => parse_plain_lyrics(raw, "Unison (Plain)"),
        };

        if doc.title.is_none() {
            doc.title = Some(data.song.clone());
        }
        if doc.artist.is_none() {
            doc.artist = Some(data.artist.clone());
        }
        if doc.album.is_none() {
            doc.album = data.album.clone();
        }
        if doc.isrc.is_none() {
            doc.isrc = data.isrc.clone();
        }
        doc.attribution = Some(Box::new(Self::build_attribution(data)));

        Ok(FetchedLyrics {
            document: doc,
            raw_content: Some(raw.to_string()),
        })
    }
}

#[async_trait]
impl LyricsProvider for UnisonProvider {
    fn name(&self) -> &'static str {
        "Unison"
    }

    fn source_kind(&self) -> LyricsSourceKind {
        LyricsSourceKind::Unison
    }

    fn supports_word_sync(&self) -> bool {
        true
    }

    async fn search(&self, track: &TrackMetadata) -> Result<Vec<LyricsCandidate>, LyricsError> {
        if track.title.trim().is_empty() && track.isrc.is_none() {
            return Ok(Vec::new());
        }

        let query_meta = TrackMatchQuery::from(track);

        // 1. Highest-precision lookup: ISRC exact lookup
        if let Some(isrc) = track
            .isrc
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            && let Some(data) = self
                .fetch_direct_lookup(&format!("isrc:{isrc}"), &[("isrc", isrc.to_string())])
                .await?
            && let Some(mut cand) = Self::data_to_candidate(data)
        {
            // Ensure ISRC is populated when the server matched by ISRC
            if cand.isrc.is_none() {
                cand.isrc = Some(isrc.to_string());
            }
            if let Some(scored) = evaluate_candidate(&query_meta, &cand.to_matcher_candidate(), 0) {
                cand.match_score = scored.score;
                cand.match_reason = scored.reason;
                return Ok(vec![cand]);
            }
        }

        // 2. Direct Spotify Track ID lookup (when playing a Spotify track)
        if let Some(sp_id) = extract_spotify_track_id(track)
            && let Some(data) = self
                .fetch_direct_lookup(
                    &format!("spotify:{sp_id}"),
                    &[("spotify", sp_id.to_string())],
                )
                .await?
            && let Some(mut cand) = Self::data_to_candidate(data)
            && let Some(scored) = evaluate_candidate(&query_meta, &cand.to_matcher_candidate(), 0)
        {
            cand.match_score = scored.score;
            cand.match_reason = scored.reason;
            return Ok(vec![cand]);
        }

        // 3. Direct song + artist (+ album + duration) lookup
        if !track.title.trim().is_empty() && !track.artist.trim().is_empty() {
            let mut params = vec![
                ("song", track.title.clone()),
                ("artist", track.artist.clone()),
            ];
            if !track.album.trim().is_empty() {
                params.push(("album", track.album.clone()));
            }
            if track.duration_ms >= 1_000 {
                let dur_secs = (track.duration_ms / 1000).clamp(1, 3600);
                params.push(("duration", dur_secs.to_string()));
            }
            let key = format!("meta:{}::{}", track.artist, track.title);
            if let Some(data) = self.fetch_direct_lookup(&key, &params).await?
                && let Some(mut cand) = Self::data_to_candidate(data)
                && let Some(scored) =
                    evaluate_candidate(&query_meta, &cand.to_matcher_candidate(), 0)
            {
                cand.match_score = scored.score;
                cand.match_reason = scored.reason;
                return Ok(vec![cand]);
            }
        }

        // 4. Fallback ranked search (`GET /lyrics/search?q=...`) filtered strictly through matcher.rs
        let q = if track.artist.trim().is_empty() {
            track.title.trim().to_string()
        } else {
            format!("{} {}", track.title.trim(), track.artist.trim())
        };
        if q.is_empty() {
            return Ok(Vec::new());
        }

        let resp = self
            .http
            .get(format!("{}/lyrics/search", self.base_url))
            .query(&[("q", q)])
            .send()
            .await
            .map_err(map_reqwest_err)?;

        let status = resp.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&resp, 30),
            });
        }
        if status == StatusCode::SERVICE_UNAVAILABLE {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&resp, 15),
            });
        }
        if !status.is_success() {
            return Ok(Vec::new());
        }

        let envelope: UnisonSearchEnvelope = resp
            .json()
            .await
            .map_err(|e| LyricsError::Parse(e.to_string()))?;
        if !envelope.success {
            return Ok(Vec::new());
        }

        let candidates = envelope
            .data
            .into_iter()
            .map(|item| {
                let fmt =
                    Self::detect_format(item.format.as_deref(), item.sync_type.as_deref(), "");
                let dur_ms = item
                    .duration
                    .filter(|d| *d > 0.0)
                    .map(|d| (d * 1000.0).round() as u64);
                let _ = (&item.video_id, &item.confidence);
                LyricsCandidate {
                    id: format!("unison:{}", item.id),
                    provider_name: self.name().to_string(),
                    source_kind: LyricsSourceKind::Unison,
                    title: item.song,
                    artist: item.artist,
                    album: item.album,
                    duration_ms: dur_ms,
                    isrc: item.isrc,
                    has_word_sync: fmt.is_word_synced(),
                    has_line_sync: fmt.is_time_synced(),
                    instrumental: false,
                    format_hint: fmt,
                    match_score: 0.0,
                    match_reason: String::new(),
                    payload: CandidatePayload::ExternalId(item.id.to_string()),
                }
            })
            .collect();

        Ok(candidates)
    }

    async fn fetch(&self, candidate: &LyricsCandidate) -> Result<FetchedLyrics, LyricsError> {
        match &candidate.payload {
            CandidatePayload::InlineRaw {
                format,
                raw_content,
                source_label,
                attribution,
            } => {
                let mut doc = match format {
                    LyricsFormat::Ttml | LyricsFormat::TtmlLine => {
                        parse_ttml(raw_content, source_label)
                            .map_err(|e| LyricsError::Parse(e.to_string()))?
                    }
                    LyricsFormat::EnhancedLrc | LyricsFormat::StandardLrc => {
                        parse_lrc(raw_content, source_label)
                            .map_err(|e| LyricsError::Parse(e.to_string()))?
                    }
                    _ => parse_plain_lyrics(raw_content, source_label),
                };
                if doc.title.is_none() {
                    doc.title = Some(candidate.title.clone());
                }
                if doc.artist.is_none() {
                    doc.artist = Some(candidate.artist.clone());
                }
                if doc.album.is_none() {
                    doc.album = candidate.album.clone();
                }
                if doc.isrc.is_none() {
                    doc.isrc = candidate.isrc.clone();
                }
                if let Some(attr) = attribution {
                    doc.attribution = Some(Box::new(attr.clone()));
                }
                Ok(FetchedLyrics {
                    document: doc,
                    raw_content: Some(raw_content.clone()),
                })
            }
            CandidatePayload::ExternalId(id) => {
                let data = self.fetch_by_id(id).await?;
                Self::parse_data_into_document(&data)
            }
            _ => Err(LyricsError::NotFound),
        }
    }
}

/// Extract a canonical 22-character Spotify Base62 track ID from `TrackMetadata`, if present.
pub(crate) fn extract_spotify_track_id(track: &TrackMetadata) -> Option<&str> {
    if let Some(id) = track.uri.strip_prefix("spotify:track:")
        && is_valid_spotify_id(id)
    {
        return Some(id);
    }
    if !track.uri.starts_with("spotirust:")
        && !track.uri.starts_with("local:")
        && is_valid_spotify_id(&track.track_id)
    {
        return Some(&track.track_id);
    }
    None
}

fn is_valid_spotify_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn map_reqwest_err(err: reqwest::Error) -> LyricsError {
    if err.is_timeout() {
        LyricsError::Timeout
    } else {
        LyricsError::Network(err.to_string())
    }
}
