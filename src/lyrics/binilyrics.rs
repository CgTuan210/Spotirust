//! BiniLyrics Word-Timing Provider (`https://lyrics-api.binimum.org`).
//!
//! Resolves word-by-word and line-synced TTML lyrics via BiniLyrics' search API:
//! 1. Exact ISRC lookup (`GET /?isrc=<ISRC>`) when available.
//! 2. Metadata search (`GET /?track=<title>&artist=<artist>&album=<album>&duration=<secs>`),
//!    with an automatic album-free retry if the album-scoped query returns empty.
//! 3. Strict SSRF host allowlist on returned `lyricsUrl` (`lrc.red`, `lyrics-api.binimum.org`,
//!    `binimum.org` over HTTPS; or the configured custom `base_url` authority in tests).
//! 4. Parses the fetched TTML via `crate::lyrics::ttml::parse_ttml`, preserving syllable/word
//!    boundaries, background vocals, translations, romanization, and honest plain fallback when
//!    the document carries `lrc:timing="None"` or lacks timestamps.

use crate::lyrics::lrc::{parse_lrc, parse_plain_lyrics};
use crate::lyrics::model::{LyricsAttribution, LyricsFormat};
use crate::lyrics::provider::{
    CandidatePayload, FetchedLyrics, LyricsCandidate, LyricsError, LyricsProvider,
    LyricsSourceKind, TrackMetadata, parse_retry_after,
};
use crate::lyrics::ttml::parse_ttml;
use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Deserialize;
use std::time::Duration;

const BINILYRICS_DEFAULT_BASE_URL: &str = "https://lyrics-api.binimum.org";
const BINILYRICS_DEFAULT_TIMEOUT: Duration = Duration::from_secs(6);
const ALLOWED_PRODUCTION_HOSTS: &[&str] = &["lrc.red", "lyrics-api.binimum.org", "binimum.org"];

#[derive(Debug, Clone, Deserialize)]
struct BiniSearchEnvelope {
    #[serde(default)]
    results: Vec<BiniSearchResult>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BiniSearchResult {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    track_name: String,
    #[serde(default)]
    artist_name: String,
    #[serde(default)]
    album_name: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    isrc: Option<String>,
    #[serde(default, alias = "lyricsUrl", alias = "lyrics_url")]
    lyrics_url: Option<String>,
    #[serde(default)]
    timing_type: Option<String>,
}

/// Real BiniLyrics provider serving TTML word-by-word, line-synced, and plain lyrics.
#[derive(Clone)]
pub struct BiniLyricsProvider {
    http: reqwest::Client,
    base_url: String,
    custom_allowed_origin: Option<(String, String, Option<u16>)>,
}

impl Default for BiniLyricsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl BiniLyricsProvider {
    pub fn new() -> Self {
        Self::with_timeout(BINILYRICS_DEFAULT_TIMEOUT)
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
            base_url: BINILYRICS_DEFAULT_BASE_URL.to_string(),
            custom_allowed_origin: None,
        }
    }

    /// Override base URL (used by offline unit/integration mock servers).
    /// Also permits fetching `lyricsUrl` served from the same mock origin.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        let trimmed = base_url.into().trim_end_matches('/').to_string();
        self.custom_allowed_origin = parse_origin(&trimmed);
        self.base_url = trimmed;
        self
    }

    /// Validate that a provider-supplied `lyricsUrl` points strictly to an allowlisted
    /// HTTPS lyrics host (or the explicitly injected mock origin during tests) to prevent SSRF.
    pub fn is_allowed_lyrics_url(&self, raw_url: &str) -> bool {
        let Some((scheme, host, port)) = parse_origin(raw_url.trim()) else {
            return false;
        };
        if let Some((allowed_scheme, allowed_host, allowed_port)) = &self.custom_allowed_origin
            && scheme == *allowed_scheme
            && host == *allowed_host
            && port == *allowed_port
        {
            return true;
        }
        scheme == "https" && ALLOWED_PRODUCTION_HOSTS.contains(&host.as_str())
    }

    async fn query_search(
        &self,
        params: &[(&str, String)],
    ) -> Result<Vec<BiniSearchResult>, LyricsError> {
        let url = format!("{}/", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(params)
            .send()
            .await
            .map_err(map_reqwest_err)?;

        let status = resp.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&resp, 30),
            });
        }
        if status == StatusCode::NOT_FOUND
            || status == StatusCode::UNAUTHORIZED
            || status == StatusCode::BAD_REQUEST
        {
            return Ok(Vec::new());
        }
        if status == StatusCode::SERVICE_UNAVAILABLE {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&resp, 15),
            });
        }
        if !status.is_success() {
            return Err(LyricsError::Network(format!("BiniLyrics HTTP {status}")));
        }

        let envelope: BiniSearchEnvelope = resp
            .json()
            .await
            .map_err(|e| LyricsError::Parse(format!("BiniLyrics search JSON: {e}")))?;

        let _ = &envelope.source;
        Ok(envelope.results)
    }

    fn to_candidate(&self, item: BiniSearchResult, idx: usize) -> Option<LyricsCandidate> {
        let raw_url = item.lyrics_url?.trim().to_string();
        if raw_url.is_empty() || !self.is_allowed_lyrics_url(&raw_url) {
            tracing::warn!(
                lyrics_url = %raw_url,
                "Rejected BiniLyrics candidate with non-allowlisted lyricsUrl"
            );
            return None;
        }

        let (has_word_sync, has_line_sync, format_hint) =
            classify_timing_type(item.timing_type.as_deref());
        let duration_ms = item
            .duration
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|secs| (secs * 1000.0).round() as u64);

        let id = item
            .id
            .filter(|s| !s.trim().is_empty())
            .or_else(|| item.isrc.clone().filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| format!("res_{idx}"));

        Some(LyricsCandidate {
            id: format!("binilyrics:{id}"),
            provider_name: self.name().to_string(),
            source_kind: self.source_kind(),
            title: item.track_name,
            artist: item.artist_name,
            album: item.album_name.filter(|s| !s.trim().is_empty()),
            duration_ms,
            isrc: item.isrc.filter(|s| !s.trim().is_empty()),
            has_word_sync,
            has_line_sync,
            instrumental: false,
            format_hint,
            match_score: 0.0,
            match_reason: "BiniLyrics catalog search".to_string(),
            payload: CandidatePayload::ExternalId(raw_url),
        })
    }
}

#[async_trait]
impl LyricsProvider for BiniLyricsProvider {
    fn name(&self) -> &'static str {
        "BiniLyrics"
    }

    fn source_kind(&self) -> LyricsSourceKind {
        LyricsSourceKind::BiniLyrics
    }

    fn supports_word_sync(&self) -> bool {
        true
    }

    fn supports_line_sync(&self) -> bool {
        true
    }

    async fn search(&self, track: &TrackMetadata) -> Result<Vec<LyricsCandidate>, LyricsError> {
        let mut raw_results = Vec::new();

        // 1. Exact ISRC lookup when available (fastest & highest match certainty)
        if let Some(isrc) = track
            .isrc
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match self.query_search(&[("isrc", isrc.to_string())]).await {
                Ok(found) if !found.is_empty() => raw_results = found,
                Ok(_) => {}
                Err(err @ LyricsError::RateLimited { .. }) => return Err(err),
                Err(err @ LyricsError::Timeout) => return Err(err),
                Err(err) => {
                    tracing::debug!(error = %err, "BiniLyrics ISRC lookup failed; falling back to metadata search");
                }
            }
        }

        // 2. Metadata lookup (title + artist + optional album + rounded duration in seconds)
        let title = track.title.trim();
        let artist = track.artist.trim();
        if raw_results.is_empty() && !title.is_empty() && !artist.is_empty() {
            let mut params = vec![("track", title.to_string()), ("artist", artist.to_string())];
            let album = track.album.trim();
            if !album.is_empty() {
                params.push(("album", album.to_string()));
            }
            if track.duration_ms > 0 {
                let secs = ((track.duration_ms as f64) / 1000.0).round() as u64;
                params.push(("duration", secs.to_string()));
            }

            raw_results = self.query_search(&params).await?;

            // 3. Retry once without album filter (compilations/singles often carry a different album name)
            if raw_results.is_empty() && !album.is_empty() {
                let mut fallback_params =
                    vec![("track", title.to_string()), ("artist", artist.to_string())];
                if track.duration_ms > 0 {
                    let secs = ((track.duration_ms as f64) / 1000.0).round() as u64;
                    fallback_params.push(("duration", secs.to_string()));
                }
                raw_results = self.query_search(&fallback_params).await?;
            }
        }

        let mut candidates = Vec::new();
        for (idx, item) in raw_results.into_iter().enumerate() {
            if let Some(cand) = self.to_candidate(item, idx) {
                candidates.push(cand);
            }
        }
        Ok(candidates)
    }

    async fn fetch(&self, candidate: &LyricsCandidate) -> Result<FetchedLyrics, LyricsError> {
        let CandidatePayload::ExternalId(lyrics_url) = &candidate.payload else {
            return Err(LyricsError::NotFound);
        };

        if !self.is_allowed_lyrics_url(lyrics_url) {
            return Err(LyricsError::Network(
                "Blocked non-allowlisted BiniLyrics lyricsUrl".to_string(),
            ));
        }

        let resp = self
            .http
            .get(lyrics_url)
            .send()
            .await
            .map_err(map_reqwest_err)?;

        let status = resp.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&resp, 30),
            });
        }
        if status == StatusCode::NOT_FOUND || status == StatusCode::UNAUTHORIZED {
            return Err(LyricsError::NotFound);
        }
        if status == StatusCode::SERVICE_UNAVAILABLE {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&resp, 15),
            });
        }
        if !status.is_success() {
            return Err(LyricsError::Network(format!(
                "BiniLyrics lyrics fetch HTTP {status}"
            )));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| LyricsError::Network(e.to_string()))?;
        let trimmed = body.trim();
        if trimmed.is_empty() {
            return Err(LyricsError::NotFound);
        }

        let source_label = "BiniLyrics";
        let mut doc = if trimmed.starts_with('<') {
            parse_ttml(trimmed, source_label).map_err(|e| LyricsError::Parse(e.to_string()))?
        } else {
            parse_lrc(trimmed, source_label)
                .unwrap_or_else(|_| parse_plain_lyrics(trimmed, source_label))
        };

        if doc.title.is_none() && !candidate.title.is_empty() {
            doc.title = Some(candidate.title.clone());
        }
        if doc.artist.is_none() && !candidate.artist.is_empty() {
            doc.artist = Some(candidate.artist.clone());
        }
        if doc.album.is_none() {
            doc.album = candidate.album.clone();
        }
        if doc.isrc.is_none() {
            doc.isrc = candidate.isrc.clone();
        }
        doc.attribution = Some(Box::new(LyricsAttribution {
            provider_label: "BiniLyrics".to_string(),
            source_url: Some("https://binimum.org".to_string()),
            submitter_name: None,
            submitter_url: None,
            copyright: None,
            disclaimer: None,
        }));

        Ok(FetchedLyrics {
            document: doc,
            raw_content: Some(body),
        })
    }
}

fn classify_timing_type(timing_type: Option<&str>) -> (bool, bool, LyricsFormat) {
    match timing_type
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("word" | "syllable" | "richsync" | "karaoke") => (true, true, LyricsFormat::Ttml),
        Some("line" | "linesync" | "synced") => (false, true, LyricsFormat::TtmlLine),
        Some("none" | "plain" | "unsynced" | "static") => (false, false, LyricsFormat::PlainText),
        // When omitted by the catalog, optimistically hint TTML and let `parse_ttml` determine actual tier.
        _ => (true, true, LyricsFormat::Ttml),
    }
}

fn parse_origin(raw: &str) -> Option<(String, String, Option<u16>)> {
    let parsed = reqwest::Url::parse(raw).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    Some((
        parsed.scheme().to_ascii_lowercase(),
        host,
        parsed.port_or_known_default(),
    ))
}

fn map_reqwest_err(err: reqwest::Error) -> LyricsError {
    if err.is_timeout() {
        LyricsError::Timeout
    } else {
        LyricsError::Network(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssrf_allowlist_permits_only_known_https_hosts_and_mock_origin() {
        let prod = BiniLyricsProvider::new();
        assert!(prod.is_allowed_lyrics_url("https://lrc.red/s/GBAAW9400140.ttml"));
        assert!(prod.is_allowed_lyrics_url("https://lyrics-api.binimum.org/lyrics/123.ttml"));
        assert!(prod.is_allowed_lyrics_url("https://binimum.org/s/123.ttml"));

        // Plain HTTP or arbitrary third-party / metadata endpoints MUST be blocked in production
        assert!(!prod.is_allowed_lyrics_url("http://lrc.red/s/GBAAW9400140.ttml"));
        assert!(!prod.is_allowed_lyrics_url("https://evil.example/fake.ttml"));
        assert!(!prod.is_allowed_lyrics_url("http://169.254.169.254/latest/meta-data/"));
        assert!(!prod.is_allowed_lyrics_url("http://127.0.0.1:8080/internal"));
        assert!(!prod.is_allowed_lyrics_url("file:///etc/passwd"));

        // Mock server origin is allowed ONLY when explicitly injected via with_base_url
        let mocked = BiniLyricsProvider::new().with_base_url("http://127.0.0.1:19001");
        assert!(mocked.is_allowed_lyrics_url("http://127.0.0.1:19001/s/demo.ttml"));
        assert!(!mocked.is_allowed_lyrics_url("http://127.0.0.1:19002/s/other_port.ttml"));
        assert!(!mocked.is_allowed_lyrics_url("https://evil.example/fake.ttml"));
    }

    #[test]
    fn classifies_binilyrics_timing_types_accurately() {
        assert_eq!(
            classify_timing_type(Some("word")),
            (true, true, LyricsFormat::Ttml)
        );
        assert_eq!(
            classify_timing_type(Some("line")),
            (false, true, LyricsFormat::TtmlLine)
        );
        assert_eq!(
            classify_timing_type(Some("none")),
            (false, false, LyricsFormat::PlainText)
        );
    }
}
