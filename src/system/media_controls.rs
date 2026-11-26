//! Cross-platform OS media control bridge decoupled from `AudioEngine`.
//!
//! Platform implementation status:
//! - **Linux:** Native **MPRIS2** D-Bus server (`org.mpris.MediaPlayer2.spotirust`) powered by
//!   `zbus 5.19` (MIT). Exposes `org.mpris.MediaPlayer2` and `org.mpris.MediaPlayer2.Player`
//!   with live metadata, playback status, position, volume, and transport commands.
//!   Degrades cleanly when no D-Bus session bus is active (e.g. headless CI/SSH).
//! - **Windows (SMTC) & macOS (MPNowPlayingInfoCenter):** Not yet implemented in this release;
//!   reported honestly as `SystemMediaStatus::UnsupportedPlatform` (zero fake stubs).

use crate::audio::engine::{PlaybackSnapshot, PlaybackStatus};
use crate::core::models::RepeatMode;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

/// High-level media commands emitted by OS media keys / desktop panels (MPRIS2, etc.).
#[derive(Debug, Clone, PartialEq)]
pub enum SystemMediaCommand {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Relative seek delta in microseconds (positive = forward, negative = backward).
    SeekOffsetUs(i64),
    /// Absolute seek position in microseconds.
    SetPositionUs(u64),
    /// Target volume in `0.0..=1.0`.
    SetVolume(f64),
}

/// Honest runtime status of the OS media controls integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemMediaStatus {
    /// Active native OS integration.
    Active {
        backend: String,
        bus_name: Option<String>,
    },
    /// Backend is supported on this OS, but the runtime session daemon is unavailable.
    Unavailable { reason: String },
    /// Platform integration is not yet implemented on this OS.
    UnsupportedPlatform { platform: &'static str },
}

impl SystemMediaStatus {
    pub fn display_label(&self) -> String {
        match self {
            Self::Active { backend, bus_name } => match bus_name {
                Some(name) => format!("Active · {backend} ({name})"),
                None => format!("Active · {backend}"),
            },
            Self::Unavailable { reason } => format!("Unavailable ({reason})"),
            Self::UnsupportedPlatform { platform } => {
                format!("Not yet implemented on {platform} (keyboard shortcuts still active)")
            }
        }
    }
}

/// Non-blocking handle owned by the backend coordinator to publish `PlaybackSnapshot` updates
/// to the OS media session without coupling to `AudioEngine`.
#[derive(Clone)]
pub struct SystemMediaBridge {
    status: Arc<RwLock<SystemMediaStatus>>,
    snapshot_tx: Option<mpsc::UnboundedSender<PlaybackSnapshot>>,
}

impl SystemMediaBridge {
    /// Start the platform media control service on the active Tokio runtime.
    ///
    /// Commands received from the OS (e.g., MPRIS2 Play/Pause/Next/Previous/Seek/Volume)
    /// are sent to `cmd_tx`.
    pub fn start(
        cmd_tx: mpsc::UnboundedSender<SystemMediaCommand>,
        artwork_cache_dir: PathBuf,
        bus_name_override: Option<String>,
    ) -> Self {
        #[cfg(target_os = "linux")]
        {
            linux_mpris::start(cmd_tx, artwork_cache_dir, bus_name_override)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (cmd_tx, artwork_cache_dir, bus_name_override);
            Self {
                status: Arc::new(RwLock::new(SystemMediaStatus::UnsupportedPlatform {
                    platform: std::env::consts::OS,
                })),
                snapshot_tx: None,
            }
        }
    }

    /// Publish the latest playback snapshot to OS media controls (lock-free, never blocks).
    pub fn update_snapshot(&self, snapshot: &PlaybackSnapshot) {
        if let Some(tx) = &self.snapshot_tx {
            let _ = tx.send(snapshot.clone());
        }
    }

    /// Return current honest integration status for Settings / Diagnostics.
    pub fn status(&self) -> SystemMediaStatus {
        self.status
            .read()
            .map(|g| g.clone())
            .unwrap_or(SystemMediaStatus::Unavailable {
                reason: "status lock poisoned".to_string(),
            })
    }
}

/// Convert `PlaybackStatus` to the MPRIS2 `PlaybackStatus` string vocabulary.
pub fn mpris_playback_status(status: &PlaybackStatus) -> &'static str {
    match status {
        PlaybackStatus::Playing => "Playing",
        PlaybackStatus::Paused => "Paused",
        PlaybackStatus::Stopped
        | PlaybackStatus::Loading
        | PlaybackStatus::Reconnecting
        | PlaybackStatus::Error(_) => "Stopped",
    }
}

/// Convert `RepeatMode` to the MPRIS2 `LoopStatus` string vocabulary.
pub fn mpris_loop_status(repeat: RepeatMode) -> &'static str {
    match repeat {
        RepeatMode::Off => "None",
        RepeatMode::Track => "Track",
        RepeatMode::Context => "Playlist",
    }
}

/// Build a valid D-Bus `ObjectPath` string for `mpris:trackid`.
pub fn sanitize_mpris_track_path(track_id: &str) -> String {
    let mut sanitized = String::with_capacity(track_id.len());
    for ch in track_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() {
        "/org/mpris/MediaPlayer2/TrackList/NoTrack".to_string()
    } else {
        format!("/org/spotirust/track/{sanitized}")
    }
}

/// Resolve a `Track.artwork_url` into a standard `http(s)://` or `file://` URI for MPRIS2 `mpris:artUrl`.
pub fn resolve_mpris_art_url(
    artwork_url: Option<&str>,
    artwork_cache_dir: &std::path::Path,
) -> Option<String> {
    let raw = artwork_url?.trim();
    if raw.starts_with("https://") || raw.starts_with("http://") || raw.starts_with("file://") {
        return Some(raw.to_string());
    }
    if let Some(hex) = raw.strip_prefix("localart://") {
        let path = artwork_cache_dir.join(format!("localart_{hex}.img"));
        if path.is_file() {
            return Some(format!("file://{}", path.to_string_lossy()));
        }
    }
    None
}

#[cfg(target_os = "linux")]
mod linux_mpris {
    use super::*;
    use std::collections::HashMap;
    use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

    pub const DEFAULT_BUS_NAME: &str = "org.mpris.MediaPlayer2.spotirust";
    pub const MPRIS_OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";

    struct MprisRoot;

    #[zbus::interface(name = "org.mpris.MediaPlayer2")]
    impl MprisRoot {
        async fn raise(&self) {}
        async fn quit(&self) {}

        #[zbus(property)]
        async fn can_quit(&self) -> bool {
            false
        }

        #[zbus(property)]
        async fn can_raise(&self) -> bool {
            false
        }

        #[zbus(property)]
        async fn has_track_list(&self) -> bool {
            false
        }

        #[zbus(property)]
        async fn identity(&self) -> &'static str {
            "Spotirust"
        }

        #[zbus(property)]
        async fn desktop_entry(&self) -> &'static str {
            "spotirust"
        }

        #[zbus(property)]
        async fn supported_uri_schemes(&self) -> Vec<String> {
            vec!["file".to_string(), "spotify".to_string()]
        }

        #[zbus(property)]
        async fn supported_mime_types(&self) -> Vec<String> {
            vec![
                "audio/mpeg".to_string(),
                "audio/flac".to_string(),
                "audio/ogg".to_string(),
                "audio/wav".to_string(),
            ]
        }
    }

    struct MprisPlayer {
        cmd_tx: mpsc::UnboundedSender<SystemMediaCommand>,
        snapshot: Arc<RwLock<PlaybackSnapshot>>,
        artwork_cache_dir: PathBuf,
    }

    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl MprisPlayer {
        async fn next(&self) {
            let _ = self.cmd_tx.send(SystemMediaCommand::Next);
        }

        async fn previous(&self) {
            let _ = self.cmd_tx.send(SystemMediaCommand::Previous);
        }

        async fn pause(&self) {
            let _ = self.cmd_tx.send(SystemMediaCommand::Pause);
        }

        async fn play_pause(&self) {
            let _ = self.cmd_tx.send(SystemMediaCommand::PlayPause);
        }

        async fn stop(&self) {
            let _ = self.cmd_tx.send(SystemMediaCommand::Stop);
        }

        async fn play(&self) {
            let _ = self.cmd_tx.send(SystemMediaCommand::Play);
        }

        async fn seek(&self, offset: i64) {
            let _ = self.cmd_tx.send(SystemMediaCommand::SeekOffsetUs(offset));
        }

        async fn set_position(&self, _track_id: OwnedObjectPath, position: i64) {
            if position >= 0 {
                let _ = self
                    .cmd_tx
                    .send(SystemMediaCommand::SetPositionUs(position as u64));
            }
        }

        async fn open_uri(&self, _uri: String) {}

        #[zbus(property)]
        async fn playback_status(&self) -> String {
            let guard = self.snapshot.read().ok();
            let status = guard
                .as_ref()
                .map(|s| mpris_playback_status(&s.status))
                .unwrap_or("Stopped");
            status.to_string()
        }

        #[zbus(property)]
        async fn loop_status(&self) -> String {
            let guard = self.snapshot.read().ok();
            let repeat = guard.as_ref().map(|s| s.repeat).unwrap_or(RepeatMode::Off);
            mpris_loop_status(repeat).to_string()
        }

        #[zbus(property)]
        async fn rate(&self) -> f64 {
            1.0
        }

        #[zbus(property)]
        async fn shuffle(&self) -> bool {
            self.snapshot.read().map(|s| s.shuffle).unwrap_or(false)
        }

        #[zbus(property)]
        async fn metadata(&self) -> HashMap<String, OwnedValue> {
            let snap = self.snapshot.read().map(|s| s.clone()).unwrap_or_default();
            build_metadata_map(&snap, &self.artwork_cache_dir)
        }

        #[zbus(property)]
        async fn volume(&self) -> f64 {
            self.snapshot
                .read()
                .map(|s| f64::from(s.volume.clamp(0.0, 1.0)))
                .unwrap_or(0.8)
        }

        #[zbus(property)]
        async fn set_volume(&self, value: f64) {
            let clamped = value.clamp(0.0, 1.0);
            let _ = self.cmd_tx.send(SystemMediaCommand::SetVolume(clamped));
        }

        #[zbus(property)]
        async fn position(&self) -> i64 {
            self.snapshot
                .read()
                .map(|s| s.position.current_ms().saturating_mul(1_000) as i64)
                .unwrap_or(0)
        }

        #[zbus(property)]
        async fn minimum_rate(&self) -> f64 {
            1.0
        }

        #[zbus(property)]
        async fn maximum_rate(&self) -> f64 {
            1.0
        }

        #[zbus(property)]
        async fn can_go_next(&self) -> bool {
            true
        }

        #[zbus(property)]
        async fn can_go_previous(&self) -> bool {
            true
        }

        #[zbus(property)]
        async fn can_play(&self) -> bool {
            true
        }

        #[zbus(property)]
        async fn can_pause(&self) -> bool {
            true
        }

        #[zbus(property)]
        async fn can_seek(&self) -> bool {
            true
        }

        #[zbus(property)]
        async fn can_control(&self) -> bool {
            true
        }
    }

    fn build_metadata_map(
        snap: &PlaybackSnapshot,
        artwork_cache_dir: &std::path::Path,
    ) -> HashMap<String, OwnedValue> {
        let mut map: HashMap<String, OwnedValue> = HashMap::new();
        let Some(track) = &snap.current_track else {
            if let Ok(path) = ObjectPath::try_from("/org/mpris/MediaPlayer2/TrackList/NoTrack")
                && let Ok(val) = OwnedValue::try_from(Value::from(path))
            {
                map.insert("mpris:trackid".to_string(), val);
            }
            return map;
        };

        let path_str = sanitize_mpris_track_path(&track.id);
        if let Ok(path) = ObjectPath::try_from(path_str.as_str())
            && let Ok(val) = OwnedValue::try_from(Value::from(path))
        {
            map.insert("mpris:trackid".to_string(), val);
        }

        if let Ok(val) = OwnedValue::try_from(Value::from(track.title.as_str())) {
            map.insert("xesam:title".to_string(), val);
        }
        if !track.artists.is_empty()
            && let Ok(val) = OwnedValue::try_from(Value::from(track.artists.clone()))
        {
            map.insert("xesam:artist".to_string(), val);
        }
        if !track.album_name.is_empty()
            && let Ok(val) = OwnedValue::try_from(Value::from(track.album_name.as_str()))
        {
            map.insert("xesam:album".to_string(), val);
        }
        let duration_ms = if snap.position.duration_ms > 0 {
            snap.position.duration_ms
        } else {
            track.duration_ms
        };
        if duration_ms > 0 {
            let length_us = duration_ms.saturating_mul(1_000) as i64;
            map.insert("mpris:length".to_string(), OwnedValue::from(length_us));
        }
        if let Some(art_url) =
            resolve_mpris_art_url(track.artwork_url.as_deref(), artwork_cache_dir)
            && let Ok(val) = OwnedValue::try_from(Value::from(art_url.as_str()))
        {
            map.insert("mpris:artUrl".to_string(), val);
        }
        map
    }

    pub fn start(
        cmd_tx: mpsc::UnboundedSender<SystemMediaCommand>,
        artwork_cache_dir: PathBuf,
        bus_name_override: Option<String>,
    ) -> SystemMediaBridge {
        let status = Arc::new(RwLock::new(SystemMediaStatus::Unavailable {
            reason: "initializing Linux MPRIS2 D-Bus session".to_string(),
        }));
        let (snap_tx, mut snap_rx) = mpsc::unbounded_channel::<PlaybackSnapshot>();
        let shared_snapshot = Arc::new(RwLock::new(PlaybackSnapshot::default()));

        let status_bg = Arc::clone(&status);
        let snapshot_bg = Arc::clone(&shared_snapshot);

        tokio::spawn(async move {
            let conn = match zbus::Connection::session().await {
                Ok(c) => c,
                Err(err) => {
                    tracing::info!(
                        error = %err,
                        "Linux D-Bus session bus unavailable; MPRIS2 media controls disabled"
                    );
                    if let Ok(mut g) = status_bg.write() {
                        *g = SystemMediaStatus::Unavailable {
                            reason: format!("no D-Bus session bus ({err})"),
                        };
                    }
                    return;
                }
            };

            let player = MprisPlayer {
                cmd_tx,
                snapshot: Arc::clone(&snapshot_bg),
                artwork_cache_dir,
            };

            if let Err(err) = conn.object_server().at(MPRIS_OBJECT_PATH, MprisRoot).await {
                tracing::warn!(error = %err, "Failed registering MPRIS2 root interface");
                if let Ok(mut g) = status_bg.write() {
                    *g = SystemMediaStatus::Unavailable {
                        reason: err.to_string(),
                    };
                }
                return;
            }
            if let Err(err) = conn.object_server().at(MPRIS_OBJECT_PATH, player).await {
                tracing::warn!(error = %err, "Failed registering MPRIS2 Player interface");
                if let Ok(mut g) = status_bg.write() {
                    *g = SystemMediaStatus::Unavailable {
                        reason: err.to_string(),
                    };
                }
                return;
            }

            // Acquire standard MPRIS2 bus name (fall back to PID-qualified name if another instance runs)
            let primary_name = bus_name_override.unwrap_or_else(|| DEFAULT_BUS_NAME.to_string());
            let acquired_name = match conn.request_name(primary_name.as_str()).await {
                Ok(_) => primary_name,
                Err(_) => {
                    let instance_name =
                        format!("{DEFAULT_BUS_NAME}.instance{}", std::process::id());
                    match conn.request_name(instance_name.as_str()).await {
                        Ok(_) => instance_name,
                        Err(err) => {
                            tracing::warn!(error = %err, "Failed claiming MPRIS2 D-Bus name");
                            if let Ok(mut g) = status_bg.write() {
                                *g = SystemMediaStatus::Unavailable {
                                    reason: err.to_string(),
                                };
                            }
                            return;
                        }
                    }
                }
            };

            tracing::info!(bus_name = %acquired_name, "Linux MPRIS2 D-Bus server active");
            if let Ok(mut g) = status_bg.write() {
                *g = SystemMediaStatus::Active {
                    backend: "Linux MPRIS2 (D-Bus)".to_string(),
                    bus_name: Some(acquired_name),
                };
            }

            let iface_ref = conn
                .object_server()
                .interface::<_, MprisPlayer>(MPRIS_OBJECT_PATH)
                .await
                .ok();

            let mut last_status = String::new();
            let mut last_track_id: Option<String> = None;
            let mut last_volume: i32 = -1;
            let mut last_shuffle: Option<bool> = None;
            let mut last_repeat: Option<RepeatMode> = None;

            while let Some(snap) = snap_rx.recv().await {
                if let Ok(mut g) = snapshot_bg.write() {
                    *g = snap.clone();
                }

                let Some(iface) = &iface_ref else {
                    continue;
                };
                let emitter = iface.signal_emitter();
                let player_ref = iface.get().await;

                let next_status = mpris_playback_status(&snap.status);
                if next_status != last_status {
                    last_status = next_status.to_string();
                    let _ = player_ref.playback_status_changed(emitter).await;
                }

                let next_track_id = snap.current_track.as_ref().map(|t| t.id.clone());
                if next_track_id != last_track_id {
                    last_track_id = next_track_id;
                    let _ = player_ref.metadata_changed(emitter).await;
                }

                let next_vol_pct = (snap.volume.clamp(0.0, 1.0) * 100.0).round() as i32;
                if next_vol_pct != last_volume {
                    last_volume = next_vol_pct;
                    let _ = player_ref.volume_changed(emitter).await;
                }

                if last_shuffle != Some(snap.shuffle) {
                    last_shuffle = Some(snap.shuffle);
                    let _ = player_ref.shuffle_changed(emitter).await;
                }

                if last_repeat != Some(snap.repeat) {
                    last_repeat = Some(snap.repeat);
                    let _ = player_ref.loop_status_changed(emitter).await;
                }
            }
        });

        SystemMediaBridge {
            status,
            snapshot_tx: Some(snap_tx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_track_id_for_dbus_object_path() {
        assert_eq!(
            sanitize_mpris_track_path("spotify:track:4cOdK2wGLETKBW3PvgPWqT"),
            "/org/spotirust/track/spotify_track_4cOdK2wGLETKBW3PvgPWqT"
        );
        assert_eq!(
            sanitize_mpris_track_path(""),
            "/org/mpris/MediaPlayer2/TrackList/NoTrack"
        );
    }

    #[test]
    fn maps_playback_and_loop_statuses_to_mpris_spec() {
        assert_eq!(mpris_playback_status(&PlaybackStatus::Playing), "Playing");
        assert_eq!(mpris_playback_status(&PlaybackStatus::Paused), "Paused");
        assert_eq!(mpris_playback_status(&PlaybackStatus::Stopped), "Stopped");
        assert_eq!(mpris_playback_status(&PlaybackStatus::Loading), "Stopped");
        assert_eq!(mpris_loop_status(RepeatMode::Off), "None");
        assert_eq!(mpris_loop_status(RepeatMode::Track), "Track");
        assert_eq!(mpris_loop_status(RepeatMode::Context), "Playlist");
    }
}
