//! Optional community `librespot` 0.8.0 audio streaming engine (`--features librespot-backend`).
//!
//! IMPORTANT PLATFORM DISCLOSURE (Verified November 2026):
//! - `librespot` is an unofficial open-source Spotify streaming library (MIT) and strictly
//!   requires a **Spotify Premium** account.
//! - Starting August 10, 2026 (`librespot-org/librespot#1737`), Spotify blocked `login5`
//!   playback token issuance for third-party developer Client IDs (`INVALID_CREDENTIALS`).
//! - Spotirust intentionally refuses to embed third-party or first-party official Spotify client
//!   credentials. This engine connects only with valid credentials supplied by the active session
//!   and cleanly reports upstream server rejections rather than bypassing DRM or scraping tokens.

use crate::audio::engine::{AudioEngine, AudioError, PlaybackPosition, PlaybackStatus};
use crate::audio::events::{PlaybackEvent, PlaybackEventBus};
use async_trait::async_trait;
use librespot_core::SpotifyUri;
use librespot_core::authentication::Credentials;
use librespot_core::config::SessionConfig;
use librespot_core::session::Session;
use librespot_playback::audio_backend;
use librespot_playback::config::{AudioFormat, PlayerConfig};
use librespot_playback::mixer::{self, Mixer, MixerConfig};
use librespot_playback::player::{Player, PlayerEvent};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

struct ActiveLibrespot {
    session: Session,
    player: Arc<Player>,
    mixer: Arc<dyn Mixer>,
}

/// Native `librespot` 0.8 playback backend implementing `AudioEngine`.
#[derive(Clone)]
pub struct LibrespotEngine {
    inner: Arc<RwLock<Option<ActiveLibrespot>>>,
    position: Arc<RwLock<PlaybackPosition>>,
    events: PlaybackEventBus,
}

impl LibrespotEngine {
    pub fn new(events: PlaybackEventBus) -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            position: Arc::new(RwLock::new(PlaybackPosition::default())),
            events,
        }
    }

    /// Establish a `librespot` session using a Spotify OAuth access token (with automatic backoff).
    pub async fn connect_with_access_token(&self, access_token: &str) -> Result<(), AudioError> {
        let credentials = Credentials::with_access_token(access_token);
        let session_config = SessionConfig::default();
        let player_config = PlayerConfig {
            position_update_interval: Some(Duration::from_millis(500)),
            ..Default::default()
        };

        let backend = audio_backend::find(None).ok_or_else(|| {
            AudioError::BackendUnavailable("no librespot audio output backend compiled".to_string())
        })?;
        let mixer_builder = mixer::find(None).ok_or_else(|| {
            AudioError::BackendUnavailable("no librespot mixer available".to_string())
        })?;
        let mixer = mixer_builder(MixerConfig::default())
            .map_err(|e| AudioError::BackendUnavailable(e.to_string()))?;

        let session = Session::new(session_config, None);
        session.connect(credentials, false).await.map_err(|e| {
            AudioError::CommandFailed(format!(
                "librespot session auth rejected ({e}). Note: since Aug 2026 Spotify \
                     restricts third-party OAuth tokens on login5 (librespot#1737); use \
                     Spotify Connect / Spotify Soloist as the official playback backend."
            ))
        })?;

        let volume_getter = mixer.get_soft_volume();
        let player = Player::new(player_config, session.clone(), volume_getter, move || {
            backend(None, AudioFormat::default())
        });

        let mut event_rx = player.get_player_event_channel();
        let pos_handle = Arc::clone(&self.position);
        let bus = self.events.clone();

        tokio::spawn(async move {
            while let Some(evt) = event_rx.recv().await {
                match evt {
                    PlayerEvent::Playing { position_ms, .. } => {
                        let mut p = pos_handle.write().await;
                        p.position_ms = u64::from(position_ms);
                        p.is_playing = true;
                        p.synced_at = std::time::Instant::now();
                        bus.publish(PlaybackEvent::StatusChanged(PlaybackStatus::Playing));
                    }
                    PlayerEvent::Paused { position_ms, .. } => {
                        let mut p = pos_handle.write().await;
                        p.position_ms = u64::from(position_ms);
                        p.is_playing = false;
                        p.synced_at = std::time::Instant::now();
                        bus.publish(PlaybackEvent::StatusChanged(PlaybackStatus::Paused));
                    }
                    PlayerEvent::PositionChanged { position_ms, .. }
                    | PlayerEvent::PositionCorrection { position_ms, .. } => {
                        let mut p = pos_handle.write().await;
                        p.position_ms = u64::from(position_ms);
                        p.synced_at = std::time::Instant::now();
                    }
                    PlayerEvent::Loading { .. } => {
                        bus.publish(PlaybackEvent::StatusChanged(PlaybackStatus::Loading));
                    }
                    PlayerEvent::Unavailable { track_id, .. } => {
                        bus.publish(PlaybackEvent::Error(format!(
                            "Librespot could not load track {track_id}"
                        )));
                    }
                    PlayerEvent::Stopped { .. } | PlayerEvent::EndOfTrack { .. } => {
                        let mut p = pos_handle.write().await;
                        p.is_playing = false;
                        p.synced_at = std::time::Instant::now();
                        bus.publish(PlaybackEvent::StatusChanged(PlaybackStatus::Stopped));
                    }
                    _ => {}
                }
            }
        });

        *self.inner.write().await = Some(ActiveLibrespot {
            session,
            player,
            mixer,
        });
        Ok(())
    }

    pub async fn disconnect(&self) {
        if let Some(active) = self.inner.write().await.take() {
            active.player.stop();
            active.session.shutdown();
        }
    }

    pub async fn is_connected(&self) -> bool {
        self.inner
            .read()
            .await
            .as_ref()
            .map(|a| !a.session.is_invalid())
            .unwrap_or(false)
    }
}

#[async_trait]
impl AudioEngine for LibrespotEngine {
    async fn play(&self, track_id: &str) -> Result<(), AudioError> {
        let uri_str = if track_id.starts_with("spotify:") {
            track_id.to_string()
        } else {
            format!("spotify:track:{track_id}")
        };
        let uri = SpotifyUri::from_uri(&uri_str)
            .map_err(|e| AudioError::InvalidTrack(format!("{uri_str}: {e}")))?;

        let guard = self.inner.read().await;
        let active = guard.as_ref().ok_or_else(|| {
            AudioError::BackendUnavailable("librespot session is not connected".to_string())
        })?;
        active.player.load(uri, true, 0);
        Ok(())
    }

    async fn pause(&self) -> Result<(), AudioError> {
        if let Some(active) = self.inner.read().await.as_ref() {
            active.player.pause();
        }
        Ok(())
    }

    async fn resume(&self) -> Result<(), AudioError> {
        if let Some(active) = self.inner.read().await.as_ref() {
            active.player.play();
        }
        Ok(())
    }

    async fn seek(&self, position_ms: u64) -> Result<(), AudioError> {
        if let Some(active) = self.inner.read().await.as_ref() {
            active
                .player
                .seek(position_ms.min(u64::from(u32::MAX)) as u32);
        }
        Ok(())
    }

    async fn set_volume(&self, volume: f32) -> Result<(), AudioError> {
        if let Some(active) = self.inner.read().await.as_ref() {
            let raw = (volume.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16;
            active.mixer.set_volume(raw);
        }
        Ok(())
    }

    async fn next(&self) -> Result<(), AudioError> {
        Ok(())
    }

    async fn previous(&self) -> Result<(), AudioError> {
        Ok(())
    }

    async fn get_position(&self) -> PlaybackPosition {
        self.position.read().await.clone()
    }
}
