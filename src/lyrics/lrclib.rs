//! Upgraded Public LRCLIB REST API Provider (`https://lrclib.net/docs`).
//!
//! ## Features & Compliance (Nov 2026)
//! - Free, open community lyrics API requiring no API key (optional `SPOTIRUST_LRCLIB_APP_ID`
//!   header supported for higher rate-limit tiers).
//! - **Word-Level Timing Support:** Reads LRCLIB's CC0 1.0 `lyricsfile` YAML v1.0 field first
//!   (`hasWordSync: true` -> `LyricsFormat::LyricsfileWord`) before falling back to Enhanced LRC,
//!   Standard LRC (`syncedLyrics`), and Plain lyrics (`plainLyrics`).
//! - Uses lightweight `GET /api/v2/search` (metadata + availability flags without full lyric bodies)
//!   paired with `GET /api/get/{id}`, falling back transparently to `GET /api/search` when a mirror
//!   or mock does not implement v2.
//! - Honors `Retry-After` on BOTH `429 Too Many Requests` and `503 Service Unavailable`.

use crate::lyrics::lrc::{parse_lrc, parse_plain_lyrics};
use crate::lyrics::lyricsfile::parse_lyricsfile;
use crate::lyrics::matcher::{TrackMatchQuery, evaluate_candidate};
use crate::lyrics::model::{LyricsAttribution, LyricsDocument, LyricsFormat};
use crate::lyrics::provider::{
    CandidatePayload, FetchedLyrics, LyricsCandidate, LyricsError, LyricsProvider,
    LyricsSourceKind, TrackMetadata, parse_retry_after,
};
use crate::lyrics::ttml::parse_ttml;
use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Deserialize;
use std::time::Duration;

const LRCLIB_DEFAULT_BASE_URL: &str = "https://lrclib.net";
const LRCLIB_DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrclibRecord {
    #[serde(default)]
    id: Option<u64>,
    track_name: String,
    artist_name: String,
    #[serde(default)]
    album_name: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    has_word_sync: bool,
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    lyricsfile: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrclibV2SearchItem {
    id: u64,
    track_name: String,
    artist_name: String,
    #[serde(default)]
    album_name: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    has_word_sync: bool,
    #[serde(default)]
    has_synced_lyrics: bool,
    #[serde(default)]
    has_plain_lyrics: bool,
    #[serde(default)]
    has_lyricsfile: bool,
}

/// Public LRCLIB API client supporting word-synced `Lyricsfile` YAML, LRC, plain text,
/// lightweight `/api/v2/search` + `/api/get/{id}`, and respectful `Retry-After` handling.
#[derive(Clone)]
pub struct LrclibProvider {
    http: reqwest::Client,
    base_url: String,
    app_id: Option<String>,
}

impl Default for LrclibProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl LrclibProvider {
    pub fn new() -> Self {
        Self::with_timeout(LRCLIB_DEFAULT_TIMEOUT)
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
        let app_id = std::env::var("SPOTIRUST_LRCLIB_APP_ID")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Self {
            http,
            base_url: LRCLIB_DEFAULT_BASE_URL.to_string(),
            app_id,
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    fn apply_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(app_id) = &self.app_id {
            req.header("Lrclib-App-Id", app_id)
        } else {
            req
        }
    }

    fn attribution() -> LyricsAttribution {
        LyricsAttribution {
            provider_label: "Lyrics from LRCLIB".to_string(),
            source_url: Some("https://lrclib.net".to_string()),
            submitter_name: None,
            submitter_url: None,
            copyright: None,
            disclaimer: None,
        }
    }

    fn record_to_candidate(rec: LrclibRecord, idx: usize) -> Option<LyricsCandidate> {
        let dur_ms = rec
            .duration
            .filter(|d| *d > 0.0)
            .map(|d| (d * 1000.0).round() as u64);
        let id = rec
            .id
            .map(|n| format!("lrclib:{n}"))
            .unwrap_or_else(|| format!("lrclib:idx:{idx}"));

        if rec.instrumental {
            return Some(LyricsCandidate {
                id,
                provider_name: "LRCLIB Public API".to_string(),
                source_kind: LyricsSourceKind::Lrclib,
                title: rec.track_name,
                artist: rec.artist_name,
                album: rec.album_name,
                duration_ms: dur_ms,
                isrc: None,
                has_word_sync: false,
                has_line_sync: false,
                instrumental: true,
                format_hint: LyricsFormat::PlainText,
                match_score: 0.0,
                match_reason: String::new(),
                payload: CandidatePayload::Instrumental {
                    source_label: "LRCLIB (Instrumental)".to_string(),
                },
            });
        }

        // Priority 1: Word-synced CC0 `lyricsfile` YAML v1.0
        if let Some(lf) = rec.lyricsfile.as_deref().filter(|s| !s.trim().is_empty())
            && (rec.has_word_sync || lf.contains("words:"))
            && let Ok(parsed) = parse_lyricsfile(lf, "LRCLIB (Lyricsfile)")
            && parsed.has_word_timing()
        {
            return Some(LyricsCandidate {
                id,
                provider_name: "LRCLIB Public API".to_string(),
                source_kind: LyricsSourceKind::Lrclib,
                title: rec.track_name,
                artist: rec.artist_name,
                album: rec.album_name,
                duration_ms: dur_ms,
                isrc: None,
                has_word_sync: true,
                has_line_sync: true,
                instrumental: false,
                format_hint: LyricsFormat::LyricsfileWord,
                match_score: 0.0,
                match_reason: String::new(),
                payload: CandidatePayload::InlineRaw {
                    format: LyricsFormat::LyricsfileWord,
                    raw_content: lf.to_string(),
                    source_label: "LRCLIB (Lyricsfile)".to_string(),
                    attribution: Some(Self::attribution()),
                },
            });
        }

        // Priority 2: Synced LRC (Enhanced or Standard)
        if let Some(synced) = rec.synced_lyrics.filter(|s| !s.trim().is_empty()) {
            let word_sync = synced.contains('<') && synced.contains('>');
            let fmt = if word_sync {
                LyricsFormat::EnhancedLrc
            } else {
                LyricsFormat::StandardLrc
            };
            return Some(LyricsCandidate {
                id,
                provider_name: "LRCLIB Public API".to_string(),
                source_kind: LyricsSourceKind::Lrclib,
                title: rec.track_name,
                artist: rec.artist_name,
                album: rec.album_name,
                duration_ms: dur_ms,
                isrc: None,
                has_word_sync: word_sync,
                has_line_sync: true,
                instrumental: false,
                format_hint: fmt,
                match_score: 0.0,
                match_reason: String::new(),
                payload: CandidatePayload::InlineRaw {
                    format: fmt,
                    raw_content: synced,
                    source_label: "LRCLIB".to_string(),
                    attribution: Some(Self::attribution()),
                },
            });
        }

        // Priority 3: Line-level Lyricsfile (if synced_lyrics omitted)
        if let Some(lf) = rec.lyricsfile.filter(|s| !s.trim().is_empty())
            && let Ok(parsed) = parse_lyricsfile(&lf, "LRCLIB (Lyricsfile)")
        {
            return Some(LyricsCandidate {
                id,
                provider_name: "LRCLIB Public API".to_string(),
                source_kind: LyricsSourceKind::Lrclib,
                title: rec.track_name,
                artist: rec.artist_name,
                album: rec.album_name,
                duration_ms: dur_ms,
                isrc: None,
                has_word_sync: parsed.has_word_timing(),
                has_line_sync: parsed.format.is_time_synced(),
                instrumental: parsed.instrumental,
                format_hint: parsed.format,
                match_score: 0.0,
                match_reason: String::new(),
                payload: CandidatePayload::InlineRaw {
                    format: parsed.format,
                    raw_content: lf,
                    source_label: "LRCLIB".to_string(),
                    attribution: Some(Self::attribution()),
                },
            });
        }

        // Priority 4: Plain lyrics (NEVER fabricated timestamps)
        if let Some(plain) = rec.plain_lyrics.filter(|s| !s.trim().is_empty()) {
            return Some(LyricsCandidate {
                id,
                provider_name: "LRCLIB Public API".to_string(),
                source_kind: LyricsSourceKind::Lrclib,
                title: rec.track_name,
                artist: rec.artist_name,
                album: rec.album_name,
                duration_ms: dur_ms,
                isrc: None,
                has_word_sync: false,
                has_line_sync: false,
                instrumental: false,
                format_hint: LyricsFormat::PlainText,
                match_score: 0.0,
                match_reason: String::new(),
                payload: CandidatePayload::InlineRaw {
                    format: LyricsFormat::PlainText,
                    raw_content: plain,
                    source_label: "LRCLIB (Plain)".to_string(),
                    attribution: Some(Self::attribution()),
                },
            });
        }

        None
    }

    fn v2_item_to_candidate(item: LrclibV2SearchItem) -> Option<LyricsCandidate> {
        let has_any = item.instrumental
            || item.has_word_sync
            || item.has_synced_lyrics
            || item.has_lyricsfile
            || item.has_plain_lyrics;
        if !has_any {
            return None;
        }
        let dur_ms = item
            .duration
            .filter(|d| *d > 0.0)
            .map(|d| (d * 1000.0).round() as u64);
        let fmt_hint = if item.has_word_sync {
            LyricsFormat::LyricsfileWord
        } else if item.has_synced_lyrics {
            LyricsFormat::StandardLrc
        } else {
            LyricsFormat::PlainText
        };
        Some(LyricsCandidate {
            id: format!("lrclib:{}", item.id),
            provider_name: "LRCLIB Public API".to_string(),
            source_kind: LyricsSourceKind::Lrclib,
            title: item.track_name,
            artist: item.artist_name,
            album: item.album_name,
            duration_ms: dur_ms,
            isrc: None,
            has_word_sync: item.has_word_sync,
            has_line_sync: item.has_word_sync || item.has_synced_lyrics,
            instrumental: item.instrumental,
            format_hint: fmt_hint,
            match_score: 0.0,
            match_reason: String::new(),
            payload: if item.instrumental {
                CandidatePayload::Instrumental {
                    source_label: "LRCLIB (Instrumental)".to_string(),
                }
            } else {
                CandidatePayload::ExternalId(item.id.to_string())
            },
        })
    }

    fn parse_record_to_fetched(rec: LrclibRecord) -> Result<FetchedLyrics, LyricsError> {
        let cand = Self::record_to_candidate(rec, 0).ok_or(LyricsError::NotFound)?;
        Self::materialize_inline(&cand)
    }

    fn materialize_inline(candidate: &LyricsCandidate) -> Result<FetchedLyrics, LyricsError> {
        match &candidate.payload {
            CandidatePayload::Instrumental { source_label } => Ok(FetchedLyrics {
                document: LyricsDocument {
                    lines: Vec::new(),
                    offset_ms: 0,
                    format: LyricsFormat::PlainText,
                    source: source_label.clone(),
                    instrumental: true,
                    title: Some(candidate.title.clone()),
                    artist: Some(candidate.artist.clone()),
                    album: candidate.album.clone(),
                    isrc: candidate.isrc.clone(),
                    attribution: Some(Box::new(Self::attribution())),
                },
                raw_content: None,
            }),
            CandidatePayload::InlineRaw {
                format,
                raw_content,
                source_label,
                attribution,
            } => {
                let mut doc = match format {
                    LyricsFormat::LyricsfileWord | LyricsFormat::LyricsfileLine => {
                        parse_lyricsfile(raw_content, source_label)
                            .map_err(|e| LyricsError::Parse(e.to_string()))?
                    }
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
                doc.attribution = Some(Box::new(
                    attribution.clone().unwrap_or_else(Self::attribution),
                ));
                Ok(FetchedLyrics {
                    document: doc,
                    raw_content: Some(raw_content.clone()),
                })
            }
            _ => Err(LyricsError::NotFound),
        }
    }
}

#[async_trait]
impl LyricsProvider for LrclibProvider {
    fn name(&self) -> &'static str {
        "LRCLIB Public API"
    }

    fn source_kind(&self) -> LyricsSourceKind {
        LyricsSourceKind::Lrclib
    }

    fn supports_word_sync(&self) -> bool {
        true
    }

    async fn search(&self, track: &TrackMetadata) -> Result<Vec<LyricsCandidate>, LyricsError> {
        if track.title.trim().is_empty() || track.artist.trim().is_empty() {
            return Ok(Vec::new());
        }

        let query_meta = TrackMatchQuery::from(track);

        // Step 1: Exact signature endpoint `/api/get`
        let mut get_params = vec![
            ("track_name", track.title.clone()),
            ("artist_name", track.artist.clone()),
        ];
        if !track.album.is_empty() {
            get_params.push(("album_name", track.album.clone()));
        }
        if track.duration_ms >= 1_000 {
            let duration_secs = (track.duration_ms / 1000).clamp(1, 3600);
            get_params.push(("duration", duration_secs.to_string()));
        }

        let get_resp = self
            .apply_headers(
                self.http
                    .get(format!("{}/api/get", self.base_url))
                    .query(&get_params),
            )
            .send()
            .await
            .map_err(map_reqwest_err)?;

        if matches!(
            get_resp.status(),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&get_resp, 30),
            });
        }
        if get_resp.status().is_success()
            && let Ok(rec) = get_resp.json::<LrclibRecord>().await
            && let Some(mut cand) = Self::record_to_candidate(rec, 0)
            && let Some(scored) = evaluate_candidate(&query_meta, &cand.to_matcher_candidate(), 0)
        {
            cand.match_score = scored.score;
            cand.match_reason = scored.reason;
            return Ok(vec![cand]);
        }

        // Step 2: Lightweight `/api/v2/search` (metadata + `hasWordSync` flags without full bodies)
        let search_params = vec![
            ("track_name", track.title.clone()),
            ("artist_name", track.artist.clone()),
        ];
        let v2_resp = self
            .apply_headers(
                self.http
                    .get(format!("{}/api/v2/search", self.base_url))
                    .query(&search_params),
            )
            .send()
            .await
            .map_err(map_reqwest_err)?;

        if matches!(
            v2_resp.status(),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&v2_resp, 30),
            });
        }
        if v2_resp.status().is_success()
            && let Ok(items) = v2_resp.json::<Vec<LrclibV2SearchItem>>().await
        {
            let candidates: Vec<LyricsCandidate> = items
                .into_iter()
                .filter_map(Self::v2_item_to_candidate)
                .collect();
            return Ok(candidates);
        }

        // Step 3: Transparent fallback to `/api/search` (v1) if v2 isn't available on mirror/mock
        let v1_resp = self
            .apply_headers(
                self.http
                    .get(format!("{}/api/search", self.base_url))
                    .query(&search_params),
            )
            .send()
            .await
            .map_err(map_reqwest_err)?;

        if matches!(
            v1_resp.status(),
            StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
        ) {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&v1_resp, 30),
            });
        }
        if !v1_resp.status().is_success() {
            return Ok(Vec::new());
        }

        let records: Vec<LrclibRecord> = v1_resp
            .json()
            .await
            .map_err(|e| LyricsError::Parse(e.to_string()))?;

        let candidates = records
            .into_iter()
            .enumerate()
            .filter_map(|(idx, rec)| Self::record_to_candidate(rec, idx))
            .collect();
        Ok(candidates)
    }

    async fn fetch(&self, candidate: &LyricsCandidate) -> Result<FetchedLyrics, LyricsError> {
        match &candidate.payload {
            CandidatePayload::ExternalId(id) => {
                let resp = self
                    .apply_headers(self.http.get(format!("{}/api/get/{id}", self.base_url)))
                    .send()
                    .await
                    .map_err(map_reqwest_err)?;

                if matches!(
                    resp.status(),
                    StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
                ) {
                    return Err(LyricsError::RateLimited {
                        retry_after_secs: parse_retry_after(&resp, 30),
                    });
                }
                if resp.status() == StatusCode::NOT_FOUND {
                    return Err(LyricsError::NotFound);
                }
                if !resp.status().is_success() {
                    return Err(LyricsError::Network(format!(
                        "LRCLIB /api/get/{id} HTTP {}",
                        resp.status()
                    )));
                }
                let rec: LrclibRecord = resp
                    .json()
                    .await
                    .map_err(|e| LyricsError::Parse(e.to_string()))?;
                Self::parse_record_to_fetched(rec)
            }
            _ => Self::materialize_inline(candidate),
        }
    }
}

fn map_reqwest_err(err: reqwest::Error) -> LyricsError {
    if err.is_timeout() {
        LyricsError::Timeout
    } else {
        LyricsError::Network(err.to_string())
    }
}
