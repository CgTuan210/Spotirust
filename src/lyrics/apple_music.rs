//! Honest `AppleMusicLyricsProvider` architecture reflecting verified official Apple Music API
//! boundaries (November 2026).
//!
//! ## Verified Technical Status (`docs/APPLE_MUSIC_LYRICS_RESEARCH.md`)
//! - **Official Apple Music API (`api.music.apple.com/v1`) & MusicKit:**
//!   Exposes catalog lookup (`GET /v1/catalog/{storefront}/songs?filter[isrc]=...`) and
//!   `Songs.Attributes.hasLyrics: boolean`, **but provides NO official public endpoint to fetch
//!   lyric text or word-level TTML timestamps** (confirmed open Apple Feedback `FB25664234`).
//! - **Private / Internal Endpoints (`amp-api.music.apple.com/.../syllable-lyrics`):**
//!   Require Apple's first-party web bearer token and a user `media-user-token` session cookie
//!   scraped from `music.apple.com`, plus origin spoofing. Spotirust **strictly refuses** to call
//!   private endpoints, scrape cookies/tokens, or bundle unauthorized third-party relays.
//!
//! Therefore, `AppleMusicLyricsProvider`:
//! 1. Implements the decoupled `LyricsProvider` trait cleanly so future official Apple API
//!    support can be wired in without touching `PlaybackClock`, `matcher`, or the karaoke renderer.
//! 2. Honestly reports `is_available() == false` and returns `LyricsError::UpstreamUnsupported`
//!    instead of faking Apple Music lyrics or silently serving LRCLIB under an "Apple Music" label.
//! 3. Provides a compliant catalog-metadata helper (`OfficialAppleCatalogClient`) limited strictly
//!    to Apple's documented `api.music.apple.com/v1` endpoints when a maintainer supplies an official
//!    MusicKit Developer Token.

use crate::lyrics::provider::{
    FetchedLyrics, LyricsCandidate, LyricsError, LyricsProvider, LyricsSourceKind, TrackMetadata,
};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;

/// Honest upstream status of the Apple Music lyrics provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppleMusicProviderStatus {
    /// Verified Nov 2026: Apple Music API / MusicKit exposes `hasLyrics: bool` only and provides
    /// no official public endpoint for lyric text or TTML (`FB25664234`).
    BlockedNoPublicLyricsApi,
}

impl AppleMusicProviderStatus {
    pub fn summary(self) -> &'static str {
        match self {
            Self::BlockedNoPublicLyricsApi => {
                "Unavailable — Apple Music API / MusicKit provides no official public lyrics text or TTML endpoint (hasLyrics boolean only; FB25664234). Spotirust does not use private amp-api endpoints or scrape cookies. Use local .ttml files or Automatic/LRCLIB."
            }
        }
    }

    pub fn short_badge(self) -> &'static str {
        match self {
            Self::BlockedNoPublicLyricsApi => "Blocked by Apple API (No public lyrics endpoint)",
        }
    }
}

/// Official Apple Music catalog song attributes returned by documented
/// `GET https://api.music.apple.com/v1/catalog/{storefront}/songs` (metadata only — zero lyrics text).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficialAppleCatalogSong {
    pub id: String,
    pub name: String,
    pub artist_name: String,
    #[serde(default)]
    pub album_name: Option<String>,
    #[serde(default)]
    pub duration_in_millis: Option<u64>,
    #[serde(default)]
    pub isrc: Option<String>,
    #[serde(default)]
    pub content_rating: Option<String>,
    /// Publicly documented boolean indicating only whether Apple's first-party app has lyrics.
    #[serde(default)]
    pub has_lyrics: bool,
}

#[derive(Debug, Deserialize)]
struct AppleCatalogResponse {
    #[serde(default)]
    data: Vec<AppleCatalogSongResource>,
}

#[derive(Debug, Deserialize)]
struct AppleCatalogSongResource {
    id: String,
    attributes: AppleCatalogSongAttributes,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppleCatalogSongAttributes {
    name: String,
    artist_name: String,
    #[serde(default)]
    album_name: Option<String>,
    #[serde(default)]
    duration_in_millis: Option<u64>,
    #[serde(default)]
    isrc: Option<String>,
    #[serde(default)]
    content_rating: Option<String>,
    #[serde(default)]
    has_lyrics: bool,
}

/// Compliant client strictly restricted to Apple's documented public catalog API
/// (`https://api.music.apple.com/v1/catalog/{storefront}/songs?filter[isrc]=...`).
///
/// Requires a legitimate Apple Developer Program ES256 MusicKit Developer Token.
/// Never calls private `amp-api.music.apple.com` endpoints and never accepts `media-user-token` cookies.
#[derive(Clone)]
pub struct OfficialAppleCatalogClient {
    http: reqwest::Client,
    base_url: String,
    storefront: String,
    developer_token: Option<String>,
}

impl OfficialAppleCatalogClient {
    pub fn new(developer_token: Option<String>, storefront: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "Spotirust/{} (https://github.com/CgTuan210/Spotirust)",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: "https://api.music.apple.com".to_string(),
            storefront: storefront.into(),
            developer_token: developer_token
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty()),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn has_developer_token(&self) -> bool {
        self.developer_token.is_some()
    }

    /// Look up official catalog metadata (including `hasLyrics` boolean) by ISRC.
    /// Does NOT return lyric text because Apple's official API provides none.
    pub async fn lookup_by_isrc(
        &self,
        isrc: &str,
    ) -> Result<Vec<OfficialAppleCatalogSong>, LyricsError> {
        let Some(token) = &self.developer_token else {
            return Err(LyricsError::UpstreamUnsupported(
                "Official Apple Music Developer Token (ES256 JWT) is not configured.".to_string(),
            ));
        };
        let cleaned = isrc.trim();
        if cleaned.is_empty() {
            return Ok(Vec::new());
        }

        let url = format!("{}/v1/catalog/{}/songs", self.base_url, self.storefront);
        let resp = self
            .http
            .get(url)
            .bearer_auth(token)
            .query(&[("filter[isrc]", cleaned)])
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    LyricsError::Timeout
                } else {
                    LyricsError::Network(e.to_string())
                }
            })?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(LyricsError::RateLimited {
                retry_after_secs: 60,
            });
        }
        if !resp.status().is_success() {
            return Err(LyricsError::Network(format!(
                "Apple Music catalog API returned HTTP {}",
                resp.status()
            )));
        }

        let payload: AppleCatalogResponse = resp
            .json()
            .await
            .map_err(|e| LyricsError::Parse(e.to_string()))?;

        Ok(payload
            .data
            .into_iter()
            .map(|r| OfficialAppleCatalogSong {
                id: r.id,
                name: r.attributes.name,
                artist_name: r.attributes.artist_name,
                album_name: r.attributes.album_name,
                duration_in_millis: r.attributes.duration_in_millis,
                isrc: r.attributes.isrc,
                content_rating: r.attributes.content_rating,
                has_lyrics: r.attributes.has_lyrics,
            })
            .collect())
    }
}

/// Decoupled Apple Music `LyricsProvider` implementation.
///
/// Honestly reports `BlockedNoPublicLyricsApi` until Apple publishes an official third-party
/// lyrics/TTML API compliant with desktop Rust clients. Never calls unauthorized endpoints.
#[derive(Clone)]
pub struct AppleMusicLyricsProvider {
    status: AppleMusicProviderStatus,
    catalog: OfficialAppleCatalogClient,
}

impl Default for AppleMusicLyricsProvider {
    fn default() -> Self {
        Self::new(None, "us")
    }
}

impl AppleMusicLyricsProvider {
    pub fn new(developer_token: Option<String>, storefront: impl Into<String>) -> Self {
        Self {
            status: AppleMusicProviderStatus::BlockedNoPublicLyricsApi,
            catalog: OfficialAppleCatalogClient::new(developer_token, storefront),
        }
    }

    pub fn status(&self) -> AppleMusicProviderStatus {
        self.status
    }

    pub fn catalog_client(&self) -> &OfficialAppleCatalogClient {
        &self.catalog
    }
}

#[async_trait]
impl LyricsProvider for AppleMusicLyricsProvider {
    fn name(&self) -> &'static str {
        "Apple Music"
    }

    fn source_kind(&self) -> LyricsSourceKind {
        LyricsSourceKind::AppleMusic
    }

    fn is_available(&self) -> bool {
        false
    }

    fn unavailable_reason(&self) -> Option<&'static str> {
        Some(self.status.summary())
    }

    async fn search(&self, _track: &TrackMetadata) -> Result<Vec<LyricsCandidate>, LyricsError> {
        Err(LyricsError::UpstreamUnsupported(
            self.status.summary().to_string(),
        ))
    }

    async fn fetch(&self, _candidate: &LyricsCandidate) -> Result<FetchedLyrics, LyricsError> {
        Err(LyricsError::UpstreamUnsupported(
            self.status.summary().to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn apple_music_provider_honestly_reports_upstream_api_block() {
        let provider = AppleMusicLyricsProvider::default();
        assert_eq!(provider.name(), "Apple Music");
        assert_eq!(provider.source_kind(), LyricsSourceKind::AppleMusic);
        assert!(!provider.is_available());
        assert!(
            provider
                .unavailable_reason()
                .unwrap()
                .contains("FB25664234")
        );

        let meta = TrackMetadata {
            track_id: "sp_1".into(),
            uri: "spotify:track:sp_1".into(),
            title: "Starlight".into(),
            artist: "Aurora".into(),
            artists: vec!["Aurora".into()],
            album: "Nightfall".into(),
            duration_ms: 210_000,
            isrc: Some("USRC12345678".into()),
            explicit: false,
            local_path: None,
        };

        let res = provider.search(&meta).await;
        assert!(matches!(res, Err(LyricsError::UpstreamUnsupported(_))));
    }
}
