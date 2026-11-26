//! Official Spotify Connect remote playback controller controlling active Spotify Connect targets
//! (official Spotify desktop/mobile apps, hardware speakers, or Spotify Soloist) via `/me/player/*`.
//!
//! Honest design guarantees:
//! - Spotify Connect Web API does NOT stream audio bytes to Spotirust; it remotely controls an
//!   active external Connect device.
//! - Requires an authenticated Spotify session (`user-modify-playback-state`, `user-read-playback-state`)
//!   and a **Spotify Premium** account (`403 PREMIUM_REQUIRED` on Free accounts).
//! - NEVER optimistically claims `is_playing = true` before the remote device confirms state via
//!   `GET /me/player`.

use crate::audio::engine::{AudioEngine, AudioError, PlaybackPosition};
use crate::core::models::{RepeatMode, Track};
use crate::spotify::client::{ApiDevice, ApiTrack, SpotifyApiError, SpotifyClient};
use async_trait::async_trait;
use reqwest::Method;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Debug, Deserialize)]
pub struct ApiPlaybackState {
    pub is_playing: bool,
    #[serde(default)]
    pub progress_ms: Option<u64>,
    #[serde(default)]
    pub shuffle_state: bool,
    #[serde(default)]
    pub repeat_state: String,
    #[serde(default)]
    pub device: Option<ApiDevice>,
    #[serde(default)]
    pub item: Option<ApiTrack>,
}

#[derive(Debug, Deserialize)]
struct ApiQueueResponse {
    #[serde(default)]
    currently_playing: Option<ApiTrack>,
    #[serde(default)]
    queue: Vec<ApiTrack>,
}

/// Confirmed remote state returned by `GET /me/player`, including measured round-trip latency (`rtt`)
/// so `PlaybackClock` can compensate half-RTT polling delay accurately.
#[derive(Debug, Clone)]
pub struct RemotePlaybackState {
    pub track: Option<Track>,
    pub position: PlaybackPosition,
    pub progress_ms: u64,
    pub duration_ms: u64,
    pub rtt: Duration,
    pub is_playing: bool,
    pub shuffle_state: bool,
    pub repeat_state: RepeatMode,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub device_type: Option<String>,
    pub volume_percent: Option<u8>,
}

/// Official Spotify Web API Connect playback controller with automatic network retry
/// and strict remote-state confirmation.
#[derive(Clone)]
pub struct SpotifyConnectEngine {
    client: SpotifyClient,
    last_position: Arc<RwLock<PlaybackPosition>>,
    active_device_id: Arc<RwLock<Option<String>>>,
    active_device_name: Arc<RwLock<Option<String>>>,
    active_device_type: Arc<RwLock<Option<String>>>,
}

impl SpotifyConnectEngine {
    pub fn new(client: SpotifyClient) -> Self {
        Self {
            client,
            last_position: Arc::new(RwLock::new(PlaybackPosition::default())),
            active_device_id: Arc::new(RwLock::new(None)),
            active_device_name: Arc::new(RwLock::new(None)),
            active_device_type: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn is_authenticated(&self) -> bool {
        self.client.session().is_authenticated().await
    }

    pub async fn rate_limit_remaining(&self) -> Option<Duration> {
        self.client.rate_limit_remaining().await
    }

    pub async fn active_device_name(&self) -> Option<String> {
        self.active_device_name.read().await.clone()
    }

    pub async fn active_device_type(&self) -> Option<String> {
        self.active_device_type.read().await.clone()
    }

    /// Clear cached device metadata (e.g. on logout or device disconnect).
    pub async fn clear_device_state(&self) {
        *self.active_device_id.write().await = None;
        *self.active_device_name.write().await = None;
        *self.active_device_type.write().await = None;
        *self.last_position.write().await = PlaybackPosition::default();
    }

    /// Transfer active playback to a specific Spotify Connect device ID (`PUT /me/player`).
    pub async fn transfer_device(
        &self,
        device_id: &str,
        start_playing: bool,
    ) -> Result<(), AudioError> {
        let body = serde_json::json!({
            "device_ids": [device_id],
            "play": start_playing
        });
        self.send_with_retry(Method::PUT, "/me/player", &[], Some(body))
            .await?;
        *self.active_device_id.write().await = Some(device_id.to_string());
        Ok(())
    }

    /// Start playing one or more Spotify track URIs with an optional offset URI (`PUT /me/player/play`).
    pub async fn play_uris(
        &self,
        uris: &[String],
        offset_uri: Option<&str>,
    ) -> Result<(), AudioError> {
        if uris.is_empty() {
            return Err(AudioError::InvalidTrack(
                "no Spotify URIs provided for playback".to_string(),
            ));
        }
        let mut query = Vec::new();
        if let Some(id) = self.active_device_id.read().await.clone() {
            query.push(("device_id", id));
        }
        let body = if let Some(offset) = offset_uri {
            serde_json::json!({
                "uris": uris,
                "offset": { "uri": offset }
            })
        } else {
            serde_json::json!({ "uris": uris })
        };
        self.send_with_retry(Method::PUT, "/me/player/play", &query, Some(body))
            .await
    }

    /// Add a Spotify URI to the active Connect device's playback queue (`POST /me/player/queue`).
    pub async fn add_to_queue_remote(&self, track_uri: &str) -> Result<(), AudioError> {
        let mut query = vec![("uri", track_uri.to_string())];
        if let Some(id) = self.active_device_id.read().await.clone() {
            query.push(("device_id", id));
        }
        self.send_with_retry(Method::POST, "/me/player/queue", &query, None)
            .await
    }

    /// Fetch the active Connect device's current track and upcoming queue (`GET /me/player/queue`).
    pub async fn fetch_remote_queue(&self) -> Result<(Option<Track>, Vec<Track>), AudioError> {
        let resp: Option<ApiQueueResponse> = self
            .client
            .get_optional_json("/me/player/queue", &[])
            .await
            .map_err(map_api_err)?;

        let Some(q) = resp else {
            return Ok((None, Vec::new()));
        };
        let current = q.currently_playing.map(|t| t.into_track(None, false));
        let upcoming = q
            .queue
            .into_iter()
            .map(|t| t.into_track(None, false))
            .collect();
        Ok((current, upcoming))
    }

    /// Poll live Spotify Connect playback state (`GET /me/player`) to verify device state
    /// and measure HTTP round-trip latency (`rtt`) for `PlaybackClock` drift compensation.
    pub async fn sync_remote_state(&self) -> Result<Option<RemotePlaybackState>, AudioError> {
        let started_at = Instant::now();
        let state: Option<ApiPlaybackState> = self
            .client
            .get_optional_json("/me/player", &[])
            .await
            .map_err(map_api_err)?;
        let rtt = started_at.elapsed();

        let Some(s) = state else {
            let mut pos = self.last_position.write().await;
            pos.is_playing = false;
            pos.stale = true;
            return Ok(None);
        };

        let (device_id, device_name, device_type, volume_percent) = match &s.device {
            Some(dev) => {
                *self.active_device_id.write().await = dev.id.clone();
                *self.active_device_name.write().await = Some(dev.name.clone());
                *self.active_device_type.write().await = Some(dev.device_type.clone());
                (
                    dev.id.clone(),
                    Some(dev.name.clone()),
                    Some(dev.device_type.clone()),
                    dev.volume_percent,
                )
            }
            None => (None, None, None, None),
        };

        let track = s.item.map(|t| t.into_track(None, false));
        let duration_ms = track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
        let progress_ms = s.progress_ms.unwrap_or(0);
        let pos = PlaybackPosition::new(progress_ms, duration_ms, s.is_playing);
        *self.last_position.write().await = pos.clone();

        Ok(Some(RemotePlaybackState {
            track,
            position: pos,
            progress_ms,
            duration_ms,
            rtt,
            is_playing: s.is_playing,
            shuffle_state: s.shuffle_state,
            repeat_state: RepeatMode::from_db_str(&s.repeat_state),
            device_id,
            device_name,
            device_type,
            volume_percent,
        }))
    }

    /// Query `GET /me/player` after issuing a remote command to confirm the device's real state.
    /// Performs one brief follow-up poll (150ms) if the expected play/pause state hasn't settled yet.
    pub async fn confirm_remote_state(
        &self,
        expected_playing: Option<bool>,
    ) -> Result<Option<RemotePlaybackState>, AudioError> {
        let first = self.sync_remote_state().await?;
        let matches = match (&first, expected_playing) {
            (Some(s), Some(exp)) => s.is_playing == exp,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if matches {
            return Ok(first);
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.sync_remote_state().await
    }

    pub async fn set_shuffle_remote(&self, enabled: bool) -> Result<(), AudioError> {
        let mut query = vec![("state", enabled.to_string())];
        if let Some(id) = self.active_device_id.read().await.clone() {
            query.push(("device_id", id));
        }
        self.send_with_retry(Method::PUT, "/me/player/shuffle", &query, None)
            .await
    }

    pub async fn set_repeat_remote(&self, repeat: RepeatMode) -> Result<(), AudioError> {
        let mut query = vec![("state", repeat.as_str().to_string())];
        if let Some(id) = self.active_device_id.read().await.clone() {
            query.push(("device_id", id));
        }
        self.send_with_retry(Method::PUT, "/me/player/repeat", &query, None)
            .await
    }

    async fn send_with_retry(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<serde_json::Value>,
    ) -> Result<(), AudioError> {
        let mut delay_ms = 300u64;
        for attempt in 0..3 {
            match self
                .client
                .send_empty(method.clone(), path, query, body.clone())
                .await
            {
                Ok(()) => return Ok(()),
                Err(SpotifyApiError::Network(msg)) if attempt < 2 => {
                    tracing::warn!(
                        attempt = attempt + 1,
                        error = %msg,
                        "Transient network failure contacting Spotify Connect; retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms *= 2;
                }
                Err(err) => return Err(map_api_err(err)),
            }
        }
        Err(AudioError::CommandFailed(
            "failed contacting Spotify Connect after retries".to_string(),
        ))
    }
}

#[async_trait]
impl AudioEngine for SpotifyConnectEngine {
    async fn play(&self, track_id: &str) -> Result<(), AudioError> {
        let uri = if track_id.starts_with("spotify:") {
            track_id.to_string()
        } else {
            format!("spotify:track:{track_id}")
        };
        self.play_uris(&[uri], None).await
    }

    async fn pause(&self) -> Result<(), AudioError> {
        let mut query = Vec::new();
        if let Some(id) = self.active_device_id.read().await.clone() {
            query.push(("device_id", id));
        }
        self.send_with_retry(Method::PUT, "/me/player/pause", &query, None)
            .await
    }

    async fn resume(&self) -> Result<(), AudioError> {
        let mut query = Vec::new();
        if let Some(id) = self.active_device_id.read().await.clone() {
            query.push(("device_id", id));
        }
        self.send_with_retry(Method::PUT, "/me/player/play", &query, None)
            .await
    }

    async fn seek(&self, position_ms: u64) -> Result<(), AudioError> {
        let mut query = vec![("position_ms", position_ms.to_string())];
        if let Some(id) = self.active_device_id.read().await.clone() {
            query.push(("device_id", id));
        }
        self.send_with_retry(Method::PUT, "/me/player/seek", &query, None)
            .await
    }

    async fn set_volume(&self, volume: f32) -> Result<(), AudioError> {
        let pct = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
        let mut query = vec![("volume_percent", pct.to_string())];
        if let Some(id) = self.active_device_id.read().await.clone() {
            query.push(("device_id", id));
        }
        self.send_with_retry(Method::PUT, "/me/player/volume", &query, None)
            .await
    }

    async fn next(&self) -> Result<(), AudioError> {
        let mut query = Vec::new();
        if let Some(id) = self.active_device_id.read().await.clone() {
            query.push(("device_id", id));
        }
        self.send_with_retry(Method::POST, "/me/player/next", &query, None)
            .await
    }

    async fn previous(&self) -> Result<(), AudioError> {
        let mut query = Vec::new();
        if let Some(id) = self.active_device_id.read().await.clone() {
            query.push(("device_id", id));
        }
        self.send_with_retry(Method::POST, "/me/player/previous", &query, None)
            .await
    }

    async fn get_position(&self) -> PlaybackPosition {
        self.last_position.read().await.clone()
    }
}

fn map_api_err(err: SpotifyApiError) -> AudioError {
    match err {
        SpotifyApiError::PremiumRequired => AudioError::PremiumRequired,
        SpotifyApiError::NoActiveDevice => AudioError::NoActiveDevice,
        SpotifyApiError::Unauthenticated => AudioError::Unauthenticated,
        SpotifyApiError::RateLimited {
            retry_after_secs, ..
        } => AudioError::RateLimited { retry_after_secs },
        other => AudioError::CommandFailed(other.to_string()),
    }
}
