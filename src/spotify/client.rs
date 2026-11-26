//! Core Spotify Web API HTTP client with bearer refresh, 429 Retry-After / QUOTA_EXCEEDED
//! handling, bounded ETag response caching, and 2026 DTO deserialization.

use crate::auth::session::SessionManager;
use crate::core::models::{Album, Artist, ConnectDevice, Playlist, Track, UserProfile};
use reqwest::header::{AUTHORIZATION, ETAG, IF_NONE_MATCH, RETRY_AFTER};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::RwLock;

pub const SPOTIFY_API_BASE: &str = "https://api.spotify.com/v1";
const MAX_ETAG_CACHE_ENTRIES: usize = 128;

#[derive(Debug, Error)]
pub enum SpotifyApiError {
    #[error("not authenticated with Spotify")]
    Unauthenticated,
    #[error("Spotify Premium is required for this playback action")]
    PremiumRequired,
    #[error("Spotify refused this action (403 Forbidden): {0}")]
    Forbidden(String),
    #[error("no active Spotify Connect playback device found")]
    NoActiveDevice,
    #[error("operation is not supported by Spotify Web API: {0}")]
    Unsupported(String),
    #[error("Spotify rate limit / quota exceeded (retry after {retry_after_secs}s){}", reason.as_ref().map(|r| format!(": {r}")).unwrap_or_default())]
    RateLimited {
        retry_after_secs: u64,
        reason: Option<String>,
    },
    #[error("Spotify API error ({status}): {message}")]
    Api { status: u16, message: String },
    #[error("network error communicating with Spotify: {0}")]
    Network(String),
    #[error("failed to parse Spotify response: {0}")]
    Deserialize(String),
}

#[derive(Clone)]
struct CachedHttpBody {
    etag: String,
    body: String,
    cached_at: Instant,
}

/// Thread-safe Spotify Web API client backed by `reqwest 0.13` (`rustls`).
#[derive(Clone)]
pub struct SpotifyClient {
    http: reqwest::Client,
    session: SessionManager,
    base_url: String,
    etag_cache: Arc<RwLock<HashMap<String, CachedHttpBody>>>,
    /// Shared HTTP 429 `Retry-After` cooldown deadline preventing API spam while rate-limited.
    rate_limit_until: Arc<RwLock<Option<Instant>>>,
}

impl SpotifyClient {
    /// Create a `SpotifyClient` sharing the underlying pooled `reqwest::Client` owned by `session`
    /// (avoiding duplicate TLS and connection pools).
    pub fn new(session: SessionManager) -> Self {
        let http = session.http_client();

        Self {
            http,
            session,
            base_url: SPOTIFY_API_BASE.to_string(),
            etag_cache: Arc::new(RwLock::new(HashMap::new())),
            rate_limit_until: Arc::new(RwLock::new(None)),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn session(&self) -> &SessionManager {
        &self.session
    }

    /// Returns the remaining HTTP 429 cooldown duration if the client is currently backing off.
    pub async fn rate_limit_remaining(&self) -> Option<Duration> {
        let until = *self.rate_limit_until.read().await;
        until.and_then(|deadline| deadline.checked_duration_since(Instant::now()))
    }

    /// Perform an authenticated GET request with conditional `If-None-Match` ETag caching.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, SpotifyApiError> {
        let raw = self
            .request_raw(Method::GET, path, query, None, true)
            .await?;
        serde_json::from_str(&raw).map_err(|e| SpotifyApiError::Deserialize(e.to_string()))
    }

    /// Perform an authenticated GET request that may return `204 No Content` (e.g. player state).
    pub async fn get_optional_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Option<T>, SpotifyApiError> {
        let raw = self
            .request_raw(Method::GET, path, query, None, false)
            .await?;
        if raw.trim().is_empty() {
            return Ok(None);
        }
        let parsed =
            serde_json::from_str(&raw).map_err(|e| SpotifyApiError::Deserialize(e.to_string()))?;
        Ok(Some(parsed))
    }

    /// Perform an authenticated mutation (`PUT`, `POST`, `DELETE`) with optional JSON payload.
    pub async fn send_empty(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        json_body: Option<serde_json::Value>,
    ) -> Result<(), SpotifyApiError> {
        let _ = self
            .request_raw(method, path, query, json_body, false)
            .await?;
        Ok(())
    }

    async fn request_raw(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        json_body: Option<serde_json::Value>,
        use_etag: bool,
    ) -> Result<String, SpotifyApiError> {
        // Strictly honor an active HTTP 429 `Retry-After` window before hitting the network.
        if let Some(remaining) = self.rate_limit_remaining().await {
            return Err(SpotifyApiError::RateLimited {
                retry_after_secs: remaining.as_secs().max(1),
                reason: Some("Rate-limit cooldown active".to_string()),
            });
        }

        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.base_url, path)
        };

        let cache_key = if use_etag && method == Method::GET {
            Some(build_cache_key(&url, query))
        } else {
            None
        };

        let mut token = self
            .session
            .valid_access_token()
            .await
            .map_err(|_| SpotifyApiError::Unauthenticated)?;

        for attempt in 0..2 {
            let mut builder = self
                .http
                .request(method.clone(), &url)
                .header(AUTHORIZATION, format!("Bearer {token}"));

            if !query.is_empty() {
                builder = builder.query(query);
            }
            if let Some(body) = &json_body {
                builder = builder.json(body);
            }
            if let Some(key) = &cache_key
                && let Some(entry) = self.etag_cache.read().await.get(key)
            {
                builder = builder.header(IF_NONE_MATCH, entry.etag.as_str());
            }

            let response = builder
                .send()
                .await
                .map_err(|e| SpotifyApiError::Network(e.to_string()))?;

            let status = response.status();

            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                tracing::info!(
                    "Spotify API returned 401; attempting single-flight OAuth token refresh"
                );
                token = self
                    .session
                    .force_refresh_if_stale(Some(&token))
                    .await
                    .map_err(|_| SpotifyApiError::Unauthenticated)?;
                continue;
            }

            if status == StatusCode::NOT_MODIFIED
                && let Some(key) = &cache_key
                && let Some(entry) = self.etag_cache.read().await.get(key)
            {
                return Ok(entry.body.clone());
            }

            if status == StatusCode::NO_CONTENT {
                return Ok(String::new());
            }

            if status == StatusCode::TOO_MANY_REQUESTS {
                let retry_after_secs = response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .unwrap_or(5)
                    .clamp(1, 300);
                *self.rate_limit_until.write().await =
                    Some(Instant::now() + Duration::from_secs(retry_after_secs));
                let body_text = response.text().await.unwrap_or_default();
                let parsed_err = parse_spotify_error(&body_text);
                tracing::warn!(
                    retry_after_secs,
                    reason = ?parsed_err.reason,
                    "Spotify API returned 429 Too Many Requests; activating cooldown"
                );
                return Err(SpotifyApiError::RateLimited {
                    retry_after_secs,
                    reason: parsed_err.reason,
                });
            }

            let etag = response
                .headers()
                .get(ETAG)
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string);

            let body_text = response
                .text()
                .await
                .map_err(|e| SpotifyApiError::Network(e.to_string()))?;

            if !status.is_success() {
                let parsed = parse_spotify_error(&body_text);
                if parsed.reason.as_deref() == Some("PREMIUM_REQUIRED") {
                    return Err(SpotifyApiError::PremiumRequired);
                }
                if parsed.reason.as_deref() == Some("NO_ACTIVE_DEVICE") {
                    return Err(SpotifyApiError::NoActiveDevice);
                }
                if status == StatusCode::FORBIDDEN {
                    return Err(SpotifyApiError::Forbidden(parsed.message));
                }
                if matches!(
                    status,
                    StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
                ) {
                    return Err(SpotifyApiError::Unsupported(parsed.message));
                }
                return Err(SpotifyApiError::Api {
                    status: status.as_u16(),
                    message: parsed.message,
                });
            }

            if let (Some(key), Some(etag_val)) = (cache_key, etag) {
                let mut cache = self.etag_cache.write().await;
                if cache.len() >= MAX_ETAG_CACHE_ENTRIES
                    && let Some(oldest_key) = cache
                        .iter()
                        .min_by_key(|(_, v)| v.cached_at)
                        .map(|(k, _)| k.clone())
                {
                    cache.remove(&oldest_key);
                }
                cache.insert(
                    key,
                    CachedHttpBody {
                        etag: etag_val,
                        body: body_text.clone(),
                        cached_at: Instant::now(),
                    },
                );
            }

            return Ok(body_text);
        }

        Err(SpotifyApiError::Unauthenticated)
    }

    pub async fn clear_http_cache(&self) {
        self.etag_cache.write().await.clear();
        *self.rate_limit_until.write().await = None;
    }
}

fn build_cache_key(url: &str, query: &[(&str, String)]) -> String {
    let mut key = url.to_string();
    if !query.is_empty() {
        key.push('?');
        for (i, (k, v)) in query.iter().enumerate() {
            if i > 0 {
                key.push('&');
            }
            key.push_str(k);
            key.push('=');
            key.push_str(v);
        }
    }
    key
}

#[derive(Deserialize)]
struct SpotifyErrorEnvelope {
    error: Option<SpotifyErrorBody>,
}

#[derive(Deserialize)]
struct SpotifyErrorBody {
    message: Option<String>,
    reason: Option<String>,
}

struct ParsedSpotifyError {
    message: String,
    reason: Option<String>,
}

fn parse_spotify_error(body: &str) -> ParsedSpotifyError {
    if let Ok(env) = serde_json::from_str::<SpotifyErrorEnvelope>(body)
        && let Some(err) = env.error
    {
        return ParsedSpotifyError {
            message: err
                .message
                .unwrap_or_else(|| "Spotify API error".to_string()),
            reason: err.reason,
        };
    }
    ParsedSpotifyError {
        message: if body.trim().is_empty() {
            "Unexpected Spotify API error".to_string()
        } else {
            body.chars().take(240).collect()
        },
        reason: None,
    }
}

// =============================================================================
// Shared Spotify Web API DTOs (compatible with Feb–Jul 2026 & Extended Quota)
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct ApiImage {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct ApiUserProfile {
    pub id: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub images: Vec<ApiImage>,
    #[serde(default)]
    pub uri: Option<String>,
}

impl From<ApiUserProfile> for UserProfile {
    fn from(u: ApiUserProfile) -> Self {
        let display = u
            .display_name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| u.id.clone());
        Self {
            id: u.id,
            account_id: u.account_id,
            display_name: display,
            avatar_url: u.images.into_iter().next().map(|i| i.url),
            uri: u.uri,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ApiArtistSimple {
    pub id: Option<String>,
    pub name: String,
    pub uri: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApiArtistFull {
    pub id: String,
    pub name: String,
    pub uri: String,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub images: Vec<ApiImage>,
}

impl From<ApiArtistFull> for Artist {
    fn from(a: ApiArtistFull) -> Self {
        Self {
            id: a.id,
            uri: a.uri,
            name: a.name,
            genres: a.genres,
            image_url: a.images.into_iter().next().map(|i| i.url),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ApiAlbum {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub artists: Vec<ApiArtistSimple>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub total_tracks: u32,
    #[serde(default)]
    pub images: Vec<ApiImage>,
}

impl From<ApiAlbum> for Album {
    fn from(a: ApiAlbum) -> Self {
        Self {
            id: a.id,
            uri: a.uri,
            name: a.name,
            artists: a.artists.into_iter().map(|ar| ar.name).collect(),
            release_date: a.release_date,
            total_tracks: a.total_tracks,
            artwork_url: a.images.into_iter().next().map(|i| i.url),
            accent_rgb: None,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct ApiExternalIds {
    #[serde(default)]
    pub isrc: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApiTrack {
    pub id: Option<String>,
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub artists: Vec<ApiArtistSimple>,
    #[serde(default)]
    pub album: Option<ApiAlbum>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub external_ids: Option<ApiExternalIds>,
    #[serde(default)]
    pub explicit: bool,
}

impl ApiTrack {
    pub fn into_track(self, fallback_album: Option<&Album>, liked: bool) -> Track {
        let id = self.id.unwrap_or_else(|| self.uri.clone());
        let (album_id, album_name, artwork_url) = match self.album {
            Some(alb) => (
                Some(alb.id),
                alb.name,
                alb.images.into_iter().next().map(|i| i.url),
            ),
            None => match fallback_album {
                Some(alb) => (
                    Some(alb.id.clone()),
                    alb.name.clone(),
                    alb.artwork_url.clone(),
                ),
                None => (None, String::new(), None),
            },
        };
        Track {
            id,
            uri: self.uri,
            title: self.name,
            artists: self.artists.into_iter().map(|a| a.name).collect(),
            album_id,
            album_name,
            duration_ms: self.duration_ms,
            isrc: self.external_ids.and_then(|e| e.isrc),
            artwork_url,
            explicit: self.explicit,
            liked,
            local_path: None,
            accent_rgb: None,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct ApiPlaylistOwner {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ApiPlaylistItemsRef {
    #[serde(default)]
    pub total: u32,
}

#[derive(Debug, Deserialize)]
pub struct ApiPlaylist {
    pub id: String,
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub owner: ApiPlaylistOwner,
    #[serde(default)]
    pub snapshot_id: String,
    /// Feb 2026 renamed `tracks` -> `items`; accept both cleanly.
    #[serde(default, alias = "tracks")]
    pub items: Option<ApiPlaylistItemsRef>,
    #[serde(default)]
    pub images: Option<Vec<ApiImage>>,
    #[serde(default)]
    pub collaborative: bool,
    #[serde(default)]
    pub public: Option<bool>,
}

impl From<ApiPlaylist> for Playlist {
    fn from(p: ApiPlaylist) -> Self {
        let owner_name = p
            .owner
            .display_name
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| p.owner.id.clone());
        Self {
            id: p.id,
            uri: p.uri,
            name: p.name,
            description: p.description.unwrap_or_default(),
            owner_name,
            owner_id: p.owner.id,
            snapshot_id: p.snapshot_id,
            total_tracks: p.items.map(|i| i.total).unwrap_or(0),
            artwork_url: p
                .images
                .and_then(|imgs| imgs.into_iter().next())
                .map(|i| i.url),
            collaborative: p.collaborative,
            public: p.public,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ApiPaging<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub total: u32,
}

#[derive(Debug, Deserialize)]
pub struct ApiDevice {
    pub id: Option<String>,
    pub name: String,
    #[serde(rename = "type", default)]
    pub device_type: String,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_restricted: bool,
    #[serde(default)]
    pub volume_percent: Option<u8>,
}

impl From<ApiDevice> for ConnectDevice {
    fn from(d: ApiDevice) -> Self {
        Self {
            id: d.id,
            name: d.name,
            device_type: d.device_type,
            is_active: d.is_active,
            is_restricted: d.is_restricted,
            volume_percent: d.volume_percent,
        }
    }
}
