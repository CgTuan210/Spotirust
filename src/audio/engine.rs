//! Core `AudioEngine` async trait, lock-free-friendly `PlaybackPosition`, and playback snapshots.

use crate::core::models::{AudioBackendKind, RepeatMode, Track};
use async_trait::async_trait;
use std::time::Instant;
use thiserror::Error;

/// Playback lifecycle status.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PlaybackStatus {
    #[default]
    Stopped,
    Loading,
    Playing,
    Paused,
    Reconnecting,
    Error(String),
}

impl PlaybackStatus {
    pub fn is_playing(&self) -> bool {
        matches!(self, Self::Playing)
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Stopped => "Stopped",
            Self::Loading => "Loading…",
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::Reconnecting => "Reconnecting…",
            Self::Error(_) => "Error",
        }
    }
}

/// High-precision playback clock snapshot anchored to a monotonic `Instant`.
///
/// Allows the karaoke renderer and progress bar to compute exact millisecond positions
/// (including smooth soft-drift blending) between backend updates with **zero heap allocations**
/// and **zero drift** on seek/pause/disconnect.
#[derive(Debug, Clone)]
pub struct PlaybackPosition {
    /// Anchor position in milliseconds at `synced_at`.
    pub position_ms: u64,
    /// Total duration of the current track in milliseconds.
    pub duration_ms: u64,
    /// Whether the clock is currently advancing.
    pub is_playing: bool,
    /// Monotonic timestamp when `position_ms` was measured.
    pub synced_at: Instant,
    /// Playback speed multiplier (default `1.0`).
    pub playback_speed: f64,
    /// Optional residual visual offset (ms) blended smoothly toward `0.0` for moderate Connect drift.
    pub drift_offset_ms: f64,
    /// Monotonic start instant of the active soft-drift blend, if any.
    pub drift_blend_start: Option<Instant>,
    /// Total duration (ms) of the soft-drift blend.
    pub drift_blend_ms: u64,
    /// True when remote polling failed/timed out or Spotify Connect disconnected.
    pub stale: bool,
}

impl Default for PlaybackPosition {
    fn default() -> Self {
        Self {
            position_ms: 0,
            duration_ms: 0,
            is_playing: false,
            synced_at: Instant::now(),
            playback_speed: 1.0,
            drift_offset_ms: 0.0,
            drift_blend_start: None,
            drift_blend_ms: 0,
            stale: false,
        }
    }
}

impl PlaybackPosition {
    pub fn new(position_ms: u64, duration_ms: u64, is_playing: bool) -> Self {
        Self {
            position_ms: position_ms.min(duration_ms.max(position_ms)),
            duration_ms,
            is_playing,
            synced_at: Instant::now(),
            playback_speed: 1.0,
            drift_offset_ms: 0.0,
            drift_blend_start: None,
            drift_blend_ms: 0,
            stale: false,
        }
    }

    /// Construct a snapshot directly from `PlaybackClock` state (preserving active drift blend).
    #[allow(clippy::too_many_arguments)]
    pub fn from_clock_parts(
        position_ms: u64,
        duration_ms: u64,
        is_playing: bool,
        synced_at: Instant,
        playback_speed: f64,
        drift_offset_ms: f64,
        drift_blend_start: Option<Instant>,
        drift_blend_ms: u64,
        stale: bool,
    ) -> Self {
        Self {
            position_ms,
            duration_ms,
            is_playing: is_playing && !stale,
            synced_at,
            playback_speed,
            drift_offset_ms,
            drift_blend_start,
            drift_blend_ms,
            stale,
        }
    }

    #[inline]
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// Computes the current playback timestamp in milliseconds at a caller-supplied monotonic `Instant`
    /// with zero heap allocations.
    #[inline]
    pub fn current_ms_at(&self, now: Instant) -> u64 {
        if !self.is_playing || self.stale {
            return if self.duration_ms > 0 {
                self.position_ms.min(self.duration_ms)
            } else {
                self.position_ms
            };
        }
        let elapsed_ms = now.saturating_duration_since(self.synced_at).as_secs_f64()
            * 1000.0
            * self.playback_speed;
        let remaining_drift = match self.drift_blend_start {
            Some(start) if self.drift_blend_ms > 0 && self.drift_offset_ms.abs() > 0.001 => {
                let blend_elapsed = now.saturating_duration_since(start).as_secs_f64() * 1000.0;
                let progress = (blend_elapsed / self.drift_blend_ms as f64).clamp(0.0, 1.0);
                self.drift_offset_ms * (1.0 - progress)
            }
            _ => 0.0,
        };
        let raw = (self.position_ms as f64 + elapsed_ms + remaining_drift)
            .round()
            .max(0.0) as u64;
        if self.duration_ms > 0 {
            raw.min(self.duration_ms)
        } else {
            raw
        }
    }

    /// Computes the current playback timestamp in milliseconds without allocations.
    #[inline]
    pub fn current_ms(&self) -> u64 {
        self.current_ms_at(Instant::now())
    }

    /// Normalized `0.0..=1.0` progress ratio.
    #[inline]
    pub fn progress_ratio(&self) -> f32 {
        if self.duration_ms == 0 {
            return 0.0;
        }
        (self.current_ms() as f64 / self.duration_ms as f64).clamp(0.0, 1.0) as f32
    }
}

/// Snapshot pushed from the audio subsystem to the UI whenever playback state changes.
#[derive(Debug, Clone)]
pub struct PlaybackSnapshot {
    pub current_track: Option<Track>,
    pub status: PlaybackStatus,
    pub position: PlaybackPosition,
    pub volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    /// Currently active execution backend for the current track / remote session.
    pub backend: AudioBackendKind,
    /// User's selected operating mode (`SpotifyConnect` vs `LocalRodio` vs `Librespot`).
    pub preferred_backend: AudioBackendKind,
    pub active_device_name: Option<String>,
    pub active_device_type: Option<String>,
}

impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            current_track: None,
            status: PlaybackStatus::Stopped,
            position: PlaybackPosition::default(),
            volume: 0.8,
            shuffle: false,
            repeat: RepeatMode::Off,
            backend: AudioBackendKind::LocalRodio,
            preferred_backend: AudioBackendKind::LocalRodio,
            active_device_name: None,
            active_device_type: None,
        }
    }
}

/// Errors produced by audio backends.
#[derive(Debug, Error, Clone)]
pub enum AudioError {
    #[error("Spotify Premium is required for remote playback control or streaming")]
    PremiumRequired,
    #[error(
        "no active Spotify Connect device available; open Spotify or Spotify Soloist on a device first"
    )]
    NoActiveDevice,
    #[error("authentication required for Spotify playback")]
    Unauthenticated,
    #[error("Spotify API rate-limited; cooling down for {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("audio backend unavailable: {0}")]
    BackendUnavailable(String),
    #[error("local audio file error: {0}")]
    LocalIo(String),
    #[error("invalid track URI or identifier: {0}")]
    InvalidTrack(String),
    #[error("playback command failed: {0}")]
    CommandFailed(String),
}

/// Unified asynchronous audio engine contract implemented by all playback backends.
#[async_trait]
pub trait AudioEngine: Send + Sync {
    async fn play(&self, track_id: &str) -> Result<(), AudioError>;
    async fn pause(&self) -> Result<(), AudioError>;
    async fn resume(&self) -> Result<(), AudioError>;
    async fn seek(&self, position_ms: u64) -> Result<(), AudioError>;
    async fn set_volume(&self, volume: f32) -> Result<(), AudioError>;
    async fn next(&self) -> Result<(), AudioError>;
    async fn previous(&self) -> Result<(), AudioError>;
    async fn get_position(&self) -> PlaybackPosition;
}
