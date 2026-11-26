//! Real AMLL TTML DataBase Provider (`https://github.com/amll-dev/amll-ttml-db`).
//!
//! ## License & Terms Verification (Nov 2026)
//! - Repository: `https://github.com/amll-dev/amll-ttml-db`
//! - License: **CC0-1.0** for community-authored hand-timed word-level TTML files.
//! - The repository explicitly prohibits bulk-uploading raw Apple Music TTML dumps; submissions
//!   are hand-timed and manually reviewed by community curators.
//! - Provides direct O(1) per-platform Spotify ID indexing at:
//!   `https://raw.githubusercontent.com/amll-dev/amll-ttml-db/refs/heads/main/spotify-lyrics/{spotify_id}.ttml`
//! - Recommends displaying a repository attribution link and/or TTML author credit (`ttmlAuthorGithubLogin`).

use crate::lyrics::matcher::{TrackMatchQuery, evaluate_candidate};
use crate::lyrics::model::{LyricsAttribution, LyricsFormat};
use crate::lyrics::provider::{
    CandidatePayload, FetchedLyrics, LyricsCandidate, LyricsError, LyricsProvider,
    LyricsSourceKind, TrackMetadata, parse_retry_after,
};
use crate::lyrics::ttml::parse_ttml;
use crate::lyrics::unison::extract_spotify_track_id;
use async_trait::async_trait;
use reqwest::StatusCode;
use std::time::Duration;

const AMLL_DEFAULT_BASE_URL: &str =
    "https://raw.githubusercontent.com/amll-dev/amll-ttml-db/refs/heads/main";
const AMLL_REPO_URL: &str = "https://github.com/amll-dev/amll-ttml-db";
const AMLL_DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Community hand-timed word-level TTML provider backed by `amll-dev/amll-ttml-db` (CC0-1.0).
#[derive(Clone)]
pub struct AmllTtmlDbProvider {
    http: reqwest::Client,
    base_url: String,
}

impl Default for AmllTtmlDbProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AmllTtmlDbProvider {
    pub fn new() -> Self {
        Self::with_timeout(AMLL_DEFAULT_TIMEOUT)
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
            base_url: AMLL_DEFAULT_BASE_URL.to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }
}

#[async_trait]
impl LyricsProvider for AmllTtmlDbProvider {
    fn name(&self) -> &'static str {
        "AMLL TTML DB"
    }

    fn source_kind(&self) -> LyricsSourceKind {
        LyricsSourceKind::AmllTtmlDb
    }

    fn supports_word_sync(&self) -> bool {
        true
    }

    async fn search(&self, track: &TrackMetadata) -> Result<Vec<LyricsCandidate>, LyricsError> {
        // Direct O(1) Spotify ID lookup — skip non-Spotify/local tracks with zero network calls!
        let Some(spotify_id) = extract_spotify_track_id(track) else {
            return Ok(Vec::new());
        };

        let url = format!("{}/spotify-lyrics/{spotify_id}.ttml", self.base_url);
        let resp = self.http.get(&url).send().await.map_err(|err| {
            if err.is_timeout() {
                LyricsError::Timeout
            } else {
                LyricsError::Network(err.to_string())
            }
        })?;

        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
            return Err(LyricsError::RateLimited {
                retry_after_secs: parse_retry_after(&resp, 30),
            });
        }
        if !status.is_success() {
            return Err(LyricsError::Network(format!("AMLL TTML DB HTTP {status}")));
        }

        let raw_xml = resp
            .text()
            .await
            .map_err(|e| LyricsError::Network(e.to_string()))?;
        if raw_xml.trim().is_empty() {
            return Ok(Vec::new());
        }

        // Parse & validate TTML structure upfront so malformed files never mask fallback providers
        let parsed =
            parse_ttml(&raw_xml, "AMLL TTML DB").map_err(|e| LyricsError::Parse(e.to_string()))?;

        let has_word_sync = parsed.has_word_timing();
        let has_line_sync = parsed.format.is_time_synced();
        let instrumental = parsed.instrumental;
        let parsed_format = parsed.format;

        // Build attribution preserving any `ttmlAuthorGithubLogin` extracted by `parse_ttml`
        let mut attribution = parsed.attribution.map(|b| *b).unwrap_or_default();
        attribution.provider_label = "Lyrics from AMLL TTML DB (CC0-1.0)".to_string();
        attribution.source_url = Some(AMLL_REPO_URL.to_string());

        let cand_title = parsed.title.unwrap_or_else(|| track.title.clone());
        let cand_artist = parsed.artist.unwrap_or_else(|| track.artist.clone());
        let cand_album = parsed.album.or_else(|| {
            if track.album.is_empty() {
                None
            } else {
                Some(track.album.clone())
            }
        });
        let cand_isrc = parsed.isrc.or_else(|| track.isrc.clone());

        let mut cand = LyricsCandidate {
            id: format!("amll:spotify:{spotify_id}"),
            provider_name: self.name().to_string(),
            source_kind: LyricsSourceKind::AmllTtmlDb,
            title: cand_title,
            artist: cand_artist,
            album: cand_album,
            duration_ms: Some(track.duration_ms),
            isrc: cand_isrc,
            has_word_sync,
            has_line_sync,
            instrumental,
            format_hint: parsed_format,
            match_score: 0.0,
            match_reason: String::new(),
            payload: CandidatePayload::InlineRaw {
                format: parsed_format,
                raw_content: raw_xml,
                source_label: "AMLL TTML DB".to_string(),
                attribution: Some(attribution),
            },
        };

        // Run through strict matcher to catch rare wrong/reassigned ID submissions
        let query = TrackMatchQuery::from(track);
        if let Some(scored) = evaluate_candidate(&query, &cand.to_matcher_candidate(), 0) {
            cand.match_score = scored.score;
            cand.match_reason = format!("Spotify ID index + {}", scored.reason);
            Ok(vec![cand])
        } else {
            tracing::info!(
                spotify_id,
                track = %track.title,
                "AMLL TTML DB entry rejected by strict metadata/variant matcher"
            );
            Ok(Vec::new())
        }
    }

    async fn fetch(&self, candidate: &LyricsCandidate) -> Result<FetchedLyrics, LyricsError> {
        let CandidatePayload::InlineRaw {
            raw_content,
            source_label,
            attribution,
            ..
        } = &candidate.payload
        else {
            return Err(LyricsError::NotFound);
        };

        let mut doc =
            parse_ttml(raw_content, source_label).map_err(|e| LyricsError::Parse(e.to_string()))?;
        if let Some(attr) = attribution {
            doc.attribution = Some(Box::new(attr.clone()));
        } else {
            let attr = doc
                .attribution
                .get_or_insert_with(|| Box::new(LyricsAttribution::default()));
            attr.provider_label = "Lyrics from AMLL TTML DB (CC0-1.0)".to_string();
            attr.source_url = Some(AMLL_REPO_URL.to_string());
        }
        if doc.format == LyricsFormat::PlainText {
            return Err(LyricsError::NotFound);
        }

        Ok(FetchedLyrics {
            document: doc,
            raw_content: Some(raw_content.clone()),
        })
    }
}
