//! Unified `PlaybackController` implementing `AudioEngine` with strict mode separation:
//! 1. **Spotify Soloist Managed Engine** (`AudioBackendKind::SpotifySoloist`):
//!    Controls Spotirust's managed local Spotify Soloist headless daemon via official local WebSocket
//!    API (`--ws 127.0.0.1:0`), allowing Spotify playback even after closing the official Spotify app.
//! 2. **Spotify Connect Remote Controller Mode** (`AudioBackendKind::SpotifyConnect`):
//!    Controls an active external Spotify Connect device via official `/me/player/*` endpoints.
//!    NEVER sets `PlaybackStatus::Playing` before the target Connect device confirms playback via `GET /me/player`.
//! 3. **Local Music Player Mode** (`AudioBackendKind::LocalRodio`):
//!    Plays real local audio files (`MP3`, `FLAC`, `OGG`, `WAV`) natively via `rodio 0.21`.
//!    NEVER fakes a moving progress bar when no audio file or OS audio output device is active.
//! 4. **Optional Librespot 0.8 Backend** (`AudioBackendKind::Librespot`, behind `--features librespot-backend`):
//!    Honest status reporting (blocked for third-party OAuth tokens since Aug 2026, `librespot#1737`).

use crate::audio::clock::{ClockSyncOutcome, PlaybackClock};
use crate::audio::connect::{RemotePlaybackState, SpotifyConnectEngine};
use crate::audio::engine::{
    AudioEngine, AudioError, PlaybackPosition, PlaybackSnapshot, PlaybackStatus,
};
use crate::audio::events::{PlaybackEvent, PlaybackEventBus};
#[cfg(feature = "librespot-backend")]
use crate::audio::librespot_engine::LibrespotEngine;
use crate::audio::local_rodio::LocalRodioPlayer;
use crate::audio::queue::PlaybackQueue;
use crate::audio::soloist::{SoloistEngine, SoloistEvent, SoloistStatus};
use crate::core::models::{AudioBackendKind, RepeatMode, Track};
use crate::spotify::client::SpotifyClient;
use crate::storage::database::{Database, PlaybackPreferences};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Minimum interval between background `GET /me/player` polls in Spotify Connect mode
/// (2.5s = 24 req/min max, well inside Spotify rate limits).
const CONNECT_POLL_INTERVAL: Duration = Duration::from_millis(2_500);

struct InnerPlaybackState {
    queue: PlaybackQueue,
    status: PlaybackStatus,
    clock: PlaybackClock,
    volume: f32,
    preferred_backend: AudioBackendKind,
    active_backend: AudioBackendKind,
    active_device_name: Option<String>,
    active_device_type: Option<String>,
    soloist_owns_queue: bool,
    last_connect_poll: Instant,
}

/// High-level playback coordinator shared between the Tokio runtime and UI commands.
#[derive(Clone)]
pub struct PlaybackController {
    state: Arc<RwLock<InnerPlaybackState>>,
    events: PlaybackEventBus,
    connect: Option<SpotifyConnectEngine>,
    soloist: SoloistEngine,
    local_player: LocalRodioPlayer,
    #[cfg(feature = "librespot-backend")]
    librespot: LibrespotEngine,
    db: Option<Database>,
}

impl PlaybackController {
    pub fn new(
        spotify_client: Option<SpotifyClient>,
        db: Option<Database>,
        events: PlaybackEventBus,
    ) -> Self {
        let fallback_root = std::env::temp_dir().join("spotirust-soloist-ephemeral");
        let soloist = SoloistEngine::new(
            fallback_root.join("bin"),
            fallback_root.join("data"),
            fallback_root.join("cache"),
        );
        Self::with_soloist(spotify_client, db, events, soloist)
    }

    pub fn with_soloist(
        spotify_client: Option<SpotifyClient>,
        db: Option<Database>,
        events: PlaybackEventBus,
        soloist: SoloistEngine,
    ) -> Self {
        let restored = db
            .as_ref()
            .and_then(|d| d.load_playback_preferences().ok())
            .unwrap_or_default();

        let mut queue = PlaybackQueue::new();
        queue.shuffle = restored.shuffle;
        queue.repeat = restored.repeat_mode;

        let is_restorable = |t: &Track| -> bool {
            if t.uri.starts_with("spotirust:sample:") {
                return true;
            }
            match &t.local_path {
                Some(p) => p.is_file(),
                None => true,
            }
        };

        let valid_queue: Vec<Track> = restored
            .last_queue
            .into_iter()
            .filter(&is_restorable)
            .collect();
        let valid_last_track = restored.last_track.filter(&is_restorable);

        if !valid_queue.is_empty() {
            let selected = valid_last_track.as_ref().map(|t| t.id.as_str());
            queue.set_context(valid_queue, selected);
        } else if let Some(track) = valid_last_track {
            queue.enqueue(track);
        }

        let initial_duration = queue.current().map(|t| t.duration_ms).unwrap_or(0);
        let restored_pos_ms = if initial_duration > 0 {
            restored.last_position_ms.min(initial_duration)
        } else {
            restored.last_position_ms
        };
        let mut clock = PlaybackClock::new();
        if let Some(cur) = queue.current() {
            clock.on_track_change(
                Some(cur.id.clone()),
                initial_duration,
                restored_pos_ms,
                false,
            );
        }
        let initial_status = if queue.current().is_some() && restored_pos_ms > 0 {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Stopped
        };

        let connect = spotify_client.map(SpotifyConnectEngine::new);
        let local_player = LocalRodioPlayer::spawn();
        #[cfg(feature = "librespot-backend")]
        let librespot = LibrespotEngine::new(events.clone());

        let state = InnerPlaybackState {
            queue,
            status: initial_status,
            clock,
            volume: restored.volume.clamp(0.0, 1.0),
            preferred_backend: restored.backend_kind,
            active_backend: restored.backend_kind,
            active_device_name: None,
            active_device_type: None,
            soloist_owns_queue: true,
            last_connect_poll: Instant::now() - Duration::from_secs(10),
        };

        Self {
            state: Arc::new(RwLock::new(state)),
            events,
            connect,
            soloist,
            local_player,
            #[cfg(feature = "librespot-backend")]
            librespot,
            db,
        }
    }

    /// Lightweight offline constructor ideal for deterministic unit & integration tests.
    pub fn for_testing() -> Self {
        Self::new(None, None, PlaybackEventBus::default())
    }

    pub fn for_testing_with_soloist(soloist_root: PathBuf) -> Self {
        let soloist = SoloistEngine::new(
            soloist_root.join("bin"),
            soloist_root.join("data"),
            soloist_root.join("cache"),
        );
        Self::with_soloist(None, None, PlaybackEventBus::default(), soloist)
    }

    pub fn soloist(&self) -> &SoloistEngine {
        &self.soloist
    }

    pub fn event_bus(&self) -> &PlaybackEventBus {
        &self.events
    }

    /// Spawn the real-time event consumer bridging Spotify Soloist's local WebSocket stream
    /// into `PlaybackController`, `PlaybackClock`, and UI/MPRIS snapshots (zero HTTP polling).
    pub fn spawn_soloist_event_listener(&self) -> tokio::task::JoinHandle<()> {
        let ctrl = self.clone();
        let mut rx = self.soloist.subscribe();
        tokio::spawn(async move {
            while let Ok(evt) = rx.recv().await {
                ctrl.handle_soloist_event(evt).await;
            }
        })
    }

    async fn handle_soloist_event(&self, evt: SoloistEvent) {
        match evt {
            SoloistEvent::PlaybackState(st) => {
                let device_name = self.soloist.device_name().await;
                let track = st.item.as_ref().and_then(|i| i.into_track());
                let dur_ms = track.as_ref().map(|t| t.duration_ms).unwrap_or(0);
                let est_ms = st.position.estimated_ms_now();
                let is_playing = st.status.eq_ignore_ascii_case("playing");
                let is_buffering = st.status.eq_ignore_ascii_case("buffering");

                let mut track_changed: Option<Track> = None;
                let mut should_emit = false;
                {
                    let mut guard = self.state.write().await;
                    let take_over = guard.active_backend == AudioBackendKind::SpotifySoloist
                        || guard.preferred_backend == AudioBackendKind::SpotifySoloist
                        || (st.is_active && is_playing);
                    if take_over {
                        if st.is_active && is_playing && self.local_player.has_active_sink() {
                            self.local_player.pause();
                        }
                        guard.active_backend = AudioBackendKind::SpotifySoloist;
                        let track_id = track.as_ref().map(|t| t.id.as_str());
                        guard.clock.reconcile_remote(
                            track_id,
                            est_ms,
                            dur_ms,
                            is_playing,
                            Duration::ZERO,
                        );
                        guard.status = if is_playing {
                            PlaybackStatus::Playing
                        } else if is_buffering {
                            PlaybackStatus::Loading
                        } else if track.is_some() && !st.status.eq_ignore_ascii_case("idle") {
                            PlaybackStatus::Paused
                        } else {
                            PlaybackStatus::Stopped
                        };
                        guard.active_device_name = Some(device_name);
                        guard.active_device_type = Some("Soloist".to_string());
                        guard.volume = (f32::from(st.volume) / 100.0).clamp(0.0, 1.0);
                        guard.queue.shuffle = st.options.shuffle;
                        guard.queue.repeat = RepeatMode::from_db_str(&st.options.repeat);

                        if let Some(rt) = &track {
                            let cur_id = guard.queue.current().map(|t| t.id.as_str());
                            if cur_id != Some(rt.id.as_str()) {
                                let found_idx = guard
                                    .queue
                                    .items_in_playback_order()
                                    .iter()
                                    .position(|t| t.id == rt.id || t.uri == rt.uri);
                                if let Some(idx) = found_idx {
                                    guard.queue.jump_to(idx);
                                } else {
                                    guard.queue.set_context(vec![rt.clone()], Some(&rt.id));
                                }
                                track_changed = Some(rt.clone());
                            }
                        }
                        should_emit = true;
                    }
                }
                if let Some(ref t) = track_changed {
                    if is_playing && let Some(db) = &self.db {
                        let _ = db.record_recently_played(t, None);
                    }
                    self.events
                        .publish(PlaybackEvent::TrackChanged(Some(t.clone())));
                }
                if should_emit {
                    self.emit_snapshot().await;
                }
            }
            SoloistEvent::TrackChanged(opt_track) => {
                let mut changed_track = None;
                let mut emit = false;
                {
                    let mut guard = self.state.write().await;
                    if (guard.active_backend == AudioBackendKind::SpotifySoloist
                        || guard.preferred_backend == AudioBackendKind::SpotifySoloist)
                        && let Some(rt) = opt_track
                    {
                        let cur_id = guard.queue.current().map(|t| t.id.as_str());
                        if cur_id != Some(rt.id.as_str()) {
                            let found_idx = guard
                                .queue
                                .items_in_playback_order()
                                .iter()
                                .position(|t| t.id == rt.id || t.uri == rt.uri);
                            if let Some(idx) = found_idx {
                                guard.queue.jump_to(idx);
                            } else {
                                guard.queue.set_context(vec![rt.clone()], Some(&rt.id));
                            }
                            let is_playing = guard.status.is_playing();
                            guard.clock.on_track_change(
                                Some(rt.id.clone()),
                                rt.duration_ms,
                                0,
                                is_playing,
                            );
                            changed_track = Some(rt);
                        }
                        emit = true;
                    }
                }
                if let Some(t) = changed_track {
                    if let Some(db) = &self.db {
                        let _ = db.record_recently_played(&t, None);
                    }
                    self.events.publish(PlaybackEvent::TrackChanged(Some(t)));
                }
                if emit {
                    self.emit_snapshot().await;
                }
            }
            SoloistEvent::PlaybackStatusChanged(status_str) => {
                let mut emit = false;
                {
                    let mut guard = self.state.write().await;
                    if guard.active_backend == AudioBackendKind::SpotifySoloist
                        || guard.preferred_backend == AudioBackendKind::SpotifySoloist
                    {
                        if status_str.eq_ignore_ascii_case("playing") {
                            guard.active_backend = AudioBackendKind::SpotifySoloist;
                            let cur_id = guard.queue.current().map(|t| t.id.clone());
                            let pos_ms = guard.clock.position_ms();
                            let dur = guard.clock.duration_ms();
                            guard.clock.reconcile_remote(
                                cur_id.as_deref(),
                                pos_ms,
                                dur,
                                true,
                                Duration::ZERO,
                            );
                            guard.status = PlaybackStatus::Playing;
                        } else if status_str.eq_ignore_ascii_case("paused") {
                            guard.clock.on_pause(None);
                            guard.status = PlaybackStatus::Paused;
                        } else if status_str.eq_ignore_ascii_case("buffering") {
                            guard.status = PlaybackStatus::Loading;
                        } else if status_str.eq_ignore_ascii_case("idle") {
                            guard.clock.on_pause(None);
                            guard.status = PlaybackStatus::Stopped;
                        }
                        emit = true;
                    }
                }
                if emit {
                    self.emit_snapshot().await;
                }
            }
            SoloistEvent::PositionSync(pos) => {
                let est_ms = pos.estimated_ms_now();
                let advancing = pos.speed > 0.0;
                let mut emit = false;
                {
                    let mut guard = self.state.write().await;
                    if guard.active_backend == AudioBackendKind::SpotifySoloist {
                        let cur_id = guard.queue.current().map(|t| t.id.clone());
                        let dur = guard.clock.duration_ms();
                        if advancing && (pos.speed - guard.clock.playback_speed()).abs() > 0.01 {
                            guard.clock.set_playback_speed_at(pos.speed, Instant::now());
                        }
                        guard.clock.reconcile_remote(
                            cur_id.as_deref(),
                            est_ms,
                            dur,
                            advancing,
                            Duration::ZERO,
                        );
                        if advancing {
                            guard.status = PlaybackStatus::Playing;
                        }
                        emit = true;
                    }
                }
                if emit {
                    self.emit_snapshot().await;
                }
            }
            SoloistEvent::VolumeChanged(vol) => {
                let clamped = (f32::from(vol) / 100.0).clamp(0.0, 1.0);
                let mut emit = false;
                {
                    let mut guard = self.state.write().await;
                    if guard.active_backend == AudioBackendKind::SpotifySoloist {
                        guard.volume = clamped;
                        emit = true;
                    }
                }
                if emit {
                    self.events
                        .publish(PlaybackEvent::VolumeChanged { volume: clamped });
                    self.emit_snapshot().await;
                }
            }
            SoloistEvent::OptionsChanged(opts) => {
                let mut emit = false;
                {
                    let mut guard = self.state.write().await;
                    if guard.active_backend == AudioBackendKind::SpotifySoloist {
                        guard.queue.shuffle = opts.shuffle;
                        guard.queue.repeat = RepeatMode::from_db_str(&opts.repeat);
                        emit = true;
                    }
                }
                if emit {
                    self.emit_queue_and_persist().await;
                    self.emit_snapshot().await;
                }
            }
            SoloistEvent::QueueChanged { upcoming } => {
                let mut emit = false;
                {
                    let mut guard = self.state.write().await;
                    if guard.active_backend == AudioBackendKind::SpotifySoloist {
                        let mut combined = Vec::new();
                        let cur_id = guard.queue.current().map(|t| t.id.clone());
                        if let Some(cur) = guard.queue.current().cloned() {
                            combined.push(cur);
                        }
                        combined.extend(upcoming);
                        if !combined.is_empty() {
                            guard.queue.set_context(combined, cur_id.as_deref());
                            emit = true;
                        }
                    }
                }
                if emit {
                    self.emit_queue_and_persist().await;
                }
            }
            SoloistEvent::DeviceChanged {
                is_active,
                device_name,
            } => {
                let mut emit = false;
                {
                    let mut guard = self.state.write().await;
                    if guard.active_backend == AudioBackendKind::SpotifySoloist {
                        if !device_name.trim().is_empty() {
                            guard.active_device_name = Some(device_name);
                            guard.active_device_type = Some("Soloist".to_string());
                        }
                        if !is_active && guard.status.is_playing() {
                            guard.clock.on_pause(None);
                            guard.status = PlaybackStatus::Paused;
                        }
                        emit = true;
                    }
                }
                if emit {
                    self.emit_snapshot().await;
                }
            }
            SoloistEvent::RuntimeChanged(snap) => {
                let mut emit = false;
                {
                    let mut guard = self.state.write().await;
                    if guard.active_backend == AudioBackendKind::SpotifySoloist {
                        match &snap.status {
                            SoloistStatus::Connected { device_name, .. } => {
                                guard.active_device_name = Some(device_name.clone());
                                guard.active_device_type = Some("Soloist".to_string());
                                emit = true;
                            }
                            SoloistStatus::Stopped => {
                                if guard.status.is_playing() {
                                    guard.clock.on_disconnected();
                                    guard.status = PlaybackStatus::Stopped;
                                }
                                guard.active_device_name = None;
                                guard.active_device_type = None;
                                emit = true;
                            }
                            SoloistStatus::Expired { message }
                            | SoloistStatus::Error { message } => {
                                guard.clock.on_disconnected();
                                guard.status = PlaybackStatus::Error(message.clone());
                                emit = true;
                            }
                            _ => {}
                        }
                    }
                }
                if emit {
                    self.emit_snapshot().await;
                }
            }
            SoloistEvent::ApiError(msg) => {
                self.events
                    .publish(PlaybackEvent::Error(format!("Spotify Soloist: {msg}")));
            }
        }
    }

    pub async fn snapshot(&self) -> PlaybackSnapshot {
        let guard = self.state.read().await;
        let position = match guard.active_backend {
            AudioBackendKind::LocalRodio if self.local_player.has_active_sink() => {
                self.local_player.position()
            }
            _ => guard.clock.to_playback_position(),
        };
        PlaybackSnapshot {
            current_track: guard.queue.current().cloned(),
            status: guard.status.clone(),
            position,
            volume: guard.volume,
            shuffle: guard.queue.shuffle,
            repeat: guard.queue.repeat,
            backend: guard.active_backend,
            preferred_backend: guard.preferred_backend,
            active_device_name: guard.active_device_name.clone(),
            active_device_type: guard.active_device_type.clone(),
        }
    }

    /// Switch the user's explicit operating mode between Spotify Soloist, Spotify Connect Remote,
    /// and Local Player.
    pub async fn set_preferred_backend(&self, kind: AudioBackendKind) {
        if kind != AudioBackendKind::LocalRodio && self.local_player.has_active_sink() {
            self.local_player.pause();
        }
        {
            let mut guard = self.state.write().await;
            guard.preferred_backend = kind;
            guard.active_backend = kind;
            match kind {
                AudioBackendKind::LocalRodio => {
                    guard.active_device_name = None;
                    guard.active_device_type = None;
                    if !self.local_player.has_active_sink() {
                        guard.status = PlaybackStatus::Stopped;
                        guard.clock.on_pause(None);
                    }
                }
                AudioBackendKind::SpotifySoloist => {
                    guard.status = PlaybackStatus::Stopped;
                    guard.clock.on_pause(None);
                }
                AudioBackendKind::SpotifyConnect | AudioBackendKind::Librespot => {
                    guard.status = PlaybackStatus::Stopped;
                    guard.clock.on_pause(None);
                }
            }
        }

        if kind == AudioBackendKind::SpotifySoloist && self.soloist.is_ready().await {
            let name = self.soloist.device_name().await;
            {
                let mut guard = self.state.write().await;
                guard.active_device_name = Some(name);
                guard.active_device_type = Some("Soloist".to_string());
            }
            let _ = self.soloist.request_state().await;
            let _ = self.soloist.request_queue(50).await;
        }

        self.persist_preferences().await;
        self.emit_snapshot().await;

        if kind == AudioBackendKind::SpotifyConnect {
            let _ = self.sync_connect_now().await;
        }
    }

    /// Clear remote Spotify Connect state on user logout without losing local player readiness.
    pub async fn on_logout(&self) {
        if let Some(connect) = &self.connect {
            connect.clear_device_state().await;
        }
        let mut guard = self.state.write().await;
        guard.active_device_name = None;
        guard.active_device_type = None;
        if guard.active_backend == AudioBackendKind::SpotifyConnect {
            guard.status = PlaybackStatus::Stopped;
            guard.clock.reset_stopped();
            guard.active_backend = AudioBackendKind::LocalRodio;
        }
        drop(guard);
        self.emit_snapshot().await;
    }

    /// Immediately query `GET /me/player` and `GET /me/player/queue` on an authenticated Connect session.
    pub async fn sync_connect_now(&self) -> Result<(), AudioError> {
        let Some(connect) = &self.connect else {
            return Err(AudioError::Unauthenticated);
        };
        if !connect.is_authenticated().await {
            return Err(AudioError::Unauthenticated);
        }
        {
            let mut guard = self.state.write().await;
            guard.last_connect_poll = Instant::now();
        }

        let remote = match connect.sync_remote_state().await {
            Ok(r) => r,
            Err(err) => {
                let became_stale = {
                    let mut guard = self.state.write().await;
                    let stale = guard.clock.on_poll_failure();
                    if stale && guard.active_backend == AudioBackendKind::SpotifyConnect {
                        guard.status = PlaybackStatus::Reconnecting;
                    }
                    stale
                };
                if became_stale {
                    self.emit_snapshot().await;
                }
                return Err(err);
            }
        };
        let mut track_changed: Option<Track> = None;

        {
            let mut guard = self.state.write().await;
            if let Some(ref st) = remote {
                guard.active_backend = AudioBackendKind::SpotifyConnect;
                track_changed = apply_remote_state(&mut guard, st);
            } else if guard.active_backend == AudioBackendKind::SpotifyConnect {
                guard.active_device_name = None;
                guard.active_device_type = None;
                guard.status = PlaybackStatus::Stopped;
                guard.clock.on_disconnected();
            }
        }

        // Sync remote upcoming queue when Connect has an active session
        if remote.is_some()
            && let Ok((cur, upcoming)) = connect.fetch_remote_queue().await
        {
            let mut combined = Vec::new();
            let cur_id = cur.as_ref().map(|t| t.id.clone());
            if let Some(c) = cur {
                combined.push(c);
            }
            combined.extend(upcoming);
            if !combined.is_empty() {
                let mut guard = self.state.write().await;
                guard.queue.set_context(combined, cur_id.as_deref());
                drop(guard);
                self.emit_queue_and_persist().await;
            }
        }

        if let Some(track) = track_changed {
            self.events
                .publish(PlaybackEvent::TrackChanged(Some(track)));
        }
        self.emit_snapshot().await;
        Ok(())
    }

    pub async fn transfer_connect_device(&self, device_id: &str) -> Result<(), AudioError> {
        let Some(connect) = &self.connect else {
            return Err(AudioError::Unauthenticated);
        };
        if !connect.is_authenticated().await {
            return Err(AudioError::Unauthenticated);
        }

        // Pause local rodio if transferring to Spotify Connect
        self.local_player.stop();
        {
            let mut guard = self.state.write().await;
            guard.preferred_backend = AudioBackendKind::SpotifyConnect;
            guard.active_backend = AudioBackendKind::SpotifyConnect;
            guard.status = PlaybackStatus::Loading;
            guard.clock.on_pause(None);
        }
        self.emit_snapshot().await;

        connect.transfer_device(device_id, true).await?;
        let confirmed = connect.confirm_remote_state(Some(true)).await?;
        {
            let mut guard = self.state.write().await;
            if let Some(st) = confirmed {
                apply_remote_state(&mut guard, &st);
            } else {
                guard.status = PlaybackStatus::Paused;
                guard.clock.on_pause(None);
            }
        }
        self.emit_snapshot().await;
        Ok(())
    }

    /// Play a specific track within an optional queue context (playlist, album, search results, local folder).
    pub async fn play_track_in_context(
        &self,
        track: Track,
        context: Vec<Track>,
    ) -> Result<(), AudioError> {
        let tracks = if context.is_empty() {
            vec![track.clone()]
        } else if context.iter().any(|t| t.id == track.id) {
            context
        } else {
            let mut v = vec![track.clone()];
            v.extend(context);
            v
        };

        {
            let mut guard = self.state.write().await;
            guard.soloist_owns_queue = tracks.len() <= 1;
            guard.queue.set_context(tracks, Some(&track.id));
        }
        self.start_current_track().await
    }

    pub async fn add_to_queue(&self, track: Track) -> Result<(), AudioError> {
        let active_backend = self.state.read().await.active_backend;
        if active_backend == AudioBackendKind::SpotifySoloist && track.uri.starts_with("spotify:") {
            self.soloist.add_to_queue(&track.uri).await?;
            let _ = self.soloist.request_queue(50).await;
            return Ok(());
        }
        if active_backend == AudioBackendKind::SpotifyConnect
            && track.uri.starts_with("spotify:")
            && let Some(connect) = &self.connect
        {
            if !connect.is_authenticated().await {
                return Err(AudioError::Unauthenticated);
            }
            connect.add_to_queue_remote(&track.uri).await?;
            let _ = self.sync_connect_now().await;
            return Ok(());
        }

        {
            let mut guard = self.state.write().await;
            guard.queue.enqueue(track);
        }
        self.emit_queue_and_persist().await;
        Ok(())
    }

    pub async fn play_queue_index(&self, index: usize) -> Result<(), AudioError> {
        let has_track = {
            let mut guard = self.state.write().await;
            guard.queue.jump_to(index).is_some()
        };
        if has_track {
            self.start_current_track().await
        } else {
            Ok(())
        }
    }

    pub async fn remove_queue_index(&self, index: usize) {
        {
            let mut guard = self.state.write().await;
            guard.queue.remove_at(index);
        }
        self.emit_queue_and_persist().await;
    }

    /// Play a list of tracks immediately with shuffle enabled.
    pub async fn shuffle_play_tracks(&self, tracks: Vec<Track>) -> Result<(), AudioError> {
        if tracks.is_empty() {
            return Ok(());
        }
        {
            let mut guard = self.state.write().await;
            guard.queue.set_context(tracks, None);
            guard.queue.set_shuffle(true);
            let _ = guard.queue.jump_to(0);
        }
        self.start_current_track().await
    }

    /// Stop active playback cleanly (used by MPRIS2 Stop command).
    pub async fn stop_playback(&self) {
        let backend = self.state.read().await.active_backend;
        match backend {
            AudioBackendKind::LocalRodio => {
                self.local_player.stop();
            }
            AudioBackendKind::SpotifySoloist => {
                if self.soloist.is_ready().await {
                    let _ = self.soloist.pause().await;
                }
            }
            AudioBackendKind::SpotifyConnect => {
                if let Some(connect) = &self.connect
                    && connect.is_authenticated().await
                {
                    let _ = connect.pause().await;
                }
            }
            AudioBackendKind::Librespot => {
                #[cfg(feature = "librespot-backend")]
                {
                    let _ = self.librespot.pause().await;
                }
            }
        }
        {
            let mut guard = self.state.write().await;
            guard.status = PlaybackStatus::Stopped;
            let cur_id = guard.queue.current().map(|t| t.id.clone());
            let dur = guard.clock.duration_ms();
            guard.clock.on_track_change(cur_id, dur, 0, false);
        }
        self.persist_preferences().await;
        self.emit_snapshot().await;
    }

    pub async fn clear_queue(&self) {
        self.local_player.stop();
        {
            let mut guard = self.state.write().await;
            guard.queue.clear();
            guard.status = PlaybackStatus::Stopped;
            guard.clock.reset_stopped();
        }
        self.emit_queue_and_persist().await;
        self.emit_snapshot().await;
    }

    pub async fn toggle_play_pause(&self) -> Result<(), AudioError> {
        let (status, active_backend, has_track) = {
            let guard = self.state.read().await;
            (
                guard.status.clone(),
                guard.active_backend,
                guard.queue.current().is_some(),
            )
        };
        match status {
            PlaybackStatus::Playing => self.pause().await,
            PlaybackStatus::Paused => self.resume().await,
            PlaybackStatus::Loading => Ok(()),
            _ => {
                if has_track {
                    self.start_current_track().await
                } else if matches!(
                    active_backend,
                    AudioBackendKind::SpotifyConnect | AudioBackendKind::SpotifySoloist
                ) {
                    self.resume().await
                } else {
                    Ok(())
                }
            }
        }
    }

    pub async fn toggle_shuffle(&self) {
        let (enabled, backend) = {
            let mut guard = self.state.write().await;
            let next = !guard.queue.shuffle;
            let backend = guard.active_backend;
            if !matches!(
                backend,
                AudioBackendKind::SpotifyConnect | AudioBackendKind::SpotifySoloist
            ) {
                guard.queue.set_shuffle(next);
            }
            (next, backend)
        };
        match backend {
            AudioBackendKind::SpotifySoloist => match self.soloist.set_shuffle(enabled).await {
                Ok(()) => {
                    let mut guard = self.state.write().await;
                    guard.queue.set_shuffle(enabled);
                }
                Err(err) => {
                    self.events.publish(PlaybackEvent::Error(err.to_string()));
                }
            },
            AudioBackendKind::SpotifyConnect => {
                if let Some(connect) = &self.connect {
                    match connect.set_shuffle_remote(enabled).await {
                        Ok(()) => {
                            let mut guard = self.state.write().await;
                            guard.queue.set_shuffle(enabled);
                        }
                        Err(err) => {
                            self.events.publish(PlaybackEvent::Error(err.to_string()));
                        }
                    }
                }
            }
            _ => {}
        }
        self.emit_queue_and_persist().await;
        self.emit_snapshot().await;
    }

    pub async fn cycle_repeat(&self) {
        let (repeat, backend) = {
            let mut guard = self.state.write().await;
            let next = guard.queue.repeat.cycle();
            let backend = guard.active_backend;
            if !matches!(
                backend,
                AudioBackendKind::SpotifyConnect | AudioBackendKind::SpotifySoloist
            ) {
                guard.queue.set_repeat(next);
            }
            (next, backend)
        };
        match backend {
            AudioBackendKind::SpotifySoloist => match self.soloist.set_repeat(repeat).await {
                Ok(()) => {
                    let mut guard = self.state.write().await;
                    guard.queue.set_repeat(repeat);
                }
                Err(err) => {
                    self.events.publish(PlaybackEvent::Error(err.to_string()));
                }
            },
            AudioBackendKind::SpotifyConnect => {
                if let Some(connect) = &self.connect {
                    match connect.set_repeat_remote(repeat).await {
                        Ok(()) => {
                            let mut guard = self.state.write().await;
                            guard.queue.set_repeat(repeat);
                        }
                        Err(err) => {
                            self.events.publish(PlaybackEvent::Error(err.to_string()));
                        }
                    }
                }
            }
            _ => {}
        }
        self.emit_queue_and_persist().await;
        self.emit_snapshot().await;
    }

    /// Periodic background tick:
    /// - Local Rodio mode: detects natural sink completion (`sink.empty()`) and auto-advances queue.
    /// - Spotify Connect mode: polls `GET /me/player` at most once every 2.5 seconds when authenticated,
    ///   reconciles drift via `PlaybackClock`, honors HTTP 429 `Retry-After` cooldowns without spamming,
    ///   marks clock stale on repeated poll failures/timeouts, and detects device disconnection mid-playback.
    /// - Spotify Soloist mode: event-driven over local WebSocket (zero polling cost).
    pub async fn tick_background(&self) {
        let (active_backend, preferred_backend, is_playing, should_poll_connect) = {
            let guard = self.state.read().await;
            let poll_due = guard.last_connect_poll.elapsed() >= CONNECT_POLL_INTERVAL;
            (
                guard.active_backend,
                guard.preferred_backend,
                guard.status.is_playing(),
                poll_due,
            )
        };

        // 1. Local Rodio natural end-of-track check (zero network cost)
        if active_backend == AudioBackendKind::LocalRodio && is_playing {
            let local_pos = self.local_player.position();
            let reached_end = self.local_player.is_finished()
                || (local_pos.duration_ms > 0 && local_pos.current_ms() >= local_pos.duration_ms);

            if reached_end {
                let next_exists = {
                    let mut guard = self.state.write().await;
                    guard.queue.advance_on_end().is_some()
                };
                if next_exists {
                    let _ = self.start_current_track().await;
                } else {
                    self.local_player.stop();
                    let mut guard = self.state.write().await;
                    guard.status = PlaybackStatus::Stopped;
                    let cur_id = guard.queue.current().map(|t| t.id.clone());
                    guard.clock.on_track_change(
                        cur_id,
                        local_pos.duration_ms,
                        local_pos.duration_ms,
                        false,
                    );
                    drop(guard);
                    self.emit_snapshot().await;
                }
            }
            return;
        }

        // 2. Spotify Connect throttled state sync (only when authenticated & in Connect mode)
        if should_poll_connect
            && (active_backend == AudioBackendKind::SpotifyConnect
                || preferred_backend == AudioBackendKind::SpotifyConnect)
            && let Some(connect) = &self.connect
            && connect.is_authenticated().await
        {
            // Never poll Spotify while an HTTP 429 Retry-After cooldown is active,
            // but freeze/mark stale if confirmation age exceeds POLL_STALE_TIMEOUT_MS.
            if connect.rate_limit_remaining().await.is_some() {
                let became_stale = {
                    let mut guard = self.state.write().await;
                    let was_stale = guard.clock.is_stale();
                    let now_stale = guard.clock.check_stale_at(Instant::now());
                    if !was_stale && now_stale {
                        guard.status = PlaybackStatus::Reconnecting;
                        true
                    } else {
                        false
                    }
                };
                if became_stale {
                    self.emit_snapshot().await;
                }
                return;
            }
            {
                let mut guard = self.state.write().await;
                guard.last_connect_poll = Instant::now();
            }
            match connect.sync_remote_state().await {
                Ok(remote_opt) => {
                    let mut track_changed: Option<Track> = None;
                    let mut disconnected_device: Option<String> = None;
                    let mut changed = false;
                    {
                        let mut guard = self.state.write().await;
                        if let Some(st) = remote_opt {
                            let prev_playing = guard.status.is_playing();
                            let prev_device = guard.active_device_name.clone();
                            let prev_track = guard.queue.current().map(|t| t.id.clone());
                            let prev_stale = guard.clock.is_stale();
                            track_changed = apply_remote_state(&mut guard, &st);
                            let new_track = guard.queue.current().map(|t| t.id.clone());
                            changed = prev_playing != st.is_playing
                                || prev_device != guard.active_device_name
                                || prev_track != new_track
                                || prev_stale
                                || st.is_playing;
                        } else if guard.active_backend == AudioBackendKind::SpotifyConnect
                            && (guard.status.is_playing()
                                || guard.active_device_name.is_some()
                                || !guard.clock.is_stale())
                        {
                            disconnected_device = guard.active_device_name.take();
                            guard.active_device_type = None;
                            guard.status = PlaybackStatus::Stopped;
                            guard.clock.on_disconnected();
                            changed = true;
                        }
                    }
                    if let Some(dev_name) = disconnected_device {
                        connect.clear_device_state().await;
                        self.events.publish(PlaybackEvent::Error(format!(
                            "Spotify Connect device '{dev_name}' disconnected or became inactive. Select another device to resume."
                        )));
                    }
                    if let Some(track) = track_changed {
                        self.events
                            .publish(PlaybackEvent::TrackChanged(Some(track)));
                        if let Ok((cur, upcoming)) = connect.fetch_remote_queue().await {
                            let mut combined = Vec::new();
                            let cur_id = cur.as_ref().map(|t| t.id.clone());
                            if let Some(c) = cur {
                                combined.push(c);
                            }
                            combined.extend(upcoming);
                            if !combined.is_empty() {
                                let mut guard = self.state.write().await;
                                guard.queue.set_context(combined, cur_id.as_deref());
                                drop(guard);
                                self.emit_queue_and_persist().await;
                            }
                        }
                    }
                    if changed {
                        self.emit_snapshot().await;
                    }
                }
                Err(AudioError::RateLimited { retry_after_secs }) => {
                    let became_stale = {
                        let mut guard = self.state.write().await;
                        let stale = guard.clock.on_poll_failure();
                        if stale {
                            guard.status = PlaybackStatus::Reconnecting;
                        }
                        stale
                    };
                    if became_stale {
                        self.emit_snapshot().await;
                    }
                    self.events.publish(PlaybackEvent::Error(format!(
                        "Spotify Connect rate-limited (HTTP 429). Pausing status sync for {retry_after_secs}s."
                    )));
                }
                Err(_) => {
                    let became_stale = {
                        let mut guard = self.state.write().await;
                        let stale = guard.clock.on_poll_failure();
                        if stale && guard.active_backend == AudioBackendKind::SpotifyConnect {
                            guard.status = PlaybackStatus::Reconnecting;
                        }
                        stale
                    };
                    if became_stale {
                        self.emit_snapshot().await;
                    }
                }
            }
        }
    }

    async fn resolve_backend_for_track(&self, track: &Track) -> AudioBackendKind {
        if track.local_path.is_some() || track.uri.starts_with("spotirust:sample:") {
            return AudioBackendKind::LocalRodio;
        }
        let pref = self.state.read().await.preferred_backend;
        match pref {
            AudioBackendKind::SpotifySoloist => AudioBackendKind::SpotifySoloist,
            AudioBackendKind::Librespot => AudioBackendKind::Librespot,
            AudioBackendKind::SpotifyConnect => AudioBackendKind::SpotifyConnect,
            AudioBackendKind::LocalRodio => {
                if self.soloist.is_ready().await {
                    AudioBackendKind::SpotifySoloist
                } else {
                    AudioBackendKind::SpotifyConnect
                }
            }
        }
    }

    async fn start_current_track(&self) -> Result<(), AudioError> {
        let (track, volume, context_uris) = {
            let guard = self.state.read().await;
            let Some(track) = guard.queue.current().cloned() else {
                return Ok(());
            };
            let uris: Vec<String> = guard
                .queue
                .items_in_playback_order()
                .into_iter()
                .filter(|t| t.uri.starts_with("spotify:track:"))
                .take(50)
                .map(|t| t.uri)
                .collect();
            (track, guard.volume, uris)
        };

        self.local_player.stop();
        let backend = self.resolve_backend_for_track(&track).await;

        {
            let mut guard = self.state.write().await;
            guard.active_backend = backend;
            guard.status = PlaybackStatus::Loading;
            guard
                .clock
                .on_track_change(Some(track.id.clone()), track.duration_ms, 0, false);
        }
        self.emit_snapshot().await;

        match backend {
            AudioBackendKind::LocalRodio => {
                let Some(path) = track.local_path.clone() else {
                    let err = AudioError::InvalidTrack(
                        "Local audio file path is missing for this track".to_string(),
                    );
                    self.record_error(&err).await;
                    return Err(err);
                };

                let player = self.local_player.clone();
                let play_res = tokio::task::spawn_blocking(move || player.play_file(path, volume))
                    .await
                    .map_err(|e| AudioError::BackendUnavailable(e.to_string()))?;

                match play_res {
                    Ok(actual_duration) => {
                        let dur = if actual_duration > 0 {
                            actual_duration
                        } else {
                            track.duration_ms
                        };
                        {
                            let mut guard = self.state.write().await;
                            guard.status = PlaybackStatus::Playing;
                            guard
                                .clock
                                .on_track_change(Some(track.id.clone()), dur, 0, true);
                            guard.active_device_name = None;
                            guard.active_device_type = None;
                        }
                        if let Some(db) = &self.db {
                            let _ = db.record_recently_played(&track, None);
                        }
                        self.events
                            .publish(PlaybackEvent::TrackChanged(Some(track)));
                        self.emit_queue_and_persist().await;
                        self.emit_snapshot().await;
                        Ok(())
                    }
                    Err(err) => {
                        self.record_error(&err).await;
                        Err(err)
                    }
                }
            }
            AudioBackendKind::SpotifySoloist => {
                if let Err(err) = self.soloist.play(&track.uri).await {
                    self.record_error(&err).await;
                    return Err(err);
                }
                let dev_name = self.soloist.device_name().await;
                {
                    let mut guard = self.state.write().await;
                    guard.active_device_name = Some(dev_name);
                    guard.active_device_type = Some("Soloist".to_string());
                }
                self.events
                    .publish(PlaybackEvent::TrackChanged(Some(track)));
                self.emit_queue_and_persist().await;
                self.emit_snapshot().await;
                // Soloist confirms actual Playing status asynchronously over local WebSocket
                // (`playback_state` / `playback_changed` / `position_sync`).
                Ok(())
            }
            AudioBackendKind::SpotifyConnect => {
                let Some(connect) = &self.connect else {
                    let err = AudioError::Unauthenticated;
                    self.record_error(&err).await;
                    return Err(err);
                };
                if !connect.is_authenticated().await {
                    let err = AudioError::Unauthenticated;
                    self.record_error(&err).await;
                    return Err(err);
                }

                let send_res = if context_uris.len() > 1 && context_uris.contains(&track.uri) {
                    connect.play_uris(&context_uris, Some(&track.uri)).await
                } else {
                    connect.play(&track.uri).await
                };

                if let Err(err) = send_res {
                    self.record_error(&err).await;
                    return Err(err);
                }

                // Strictly verify the remote Connect device actually confirmed playback
                match connect.confirm_remote_state(Some(true)).await {
                    Ok(Some(remote)) => {
                        let confirmed_playing = remote.is_playing;
                        {
                            let mut guard = self.state.write().await;
                            guard.last_connect_poll = Instant::now();
                            apply_remote_state(&mut guard, &remote);
                        }
                        if confirmed_playing {
                            if let Some(db) = &self.db {
                                let _ = db.record_recently_played(&track, None);
                            }
                            self.events
                                .publish(PlaybackEvent::TrackChanged(Some(track)));
                            self.emit_queue_and_persist().await;
                            self.emit_snapshot().await;
                            Ok(())
                        } else {
                            let err = AudioError::CommandFailed(
                                "Spotify Connect device accepted the request but did not confirm active playback"
                                    .to_string(),
                            );
                            self.events.publish(PlaybackEvent::Error(err.to_string()));
                            self.emit_snapshot().await;
                            Err(err)
                        }
                    }
                    Ok(None) => {
                        let err = AudioError::NoActiveDevice;
                        self.record_error(&err).await;
                        Err(err)
                    }
                    Err(err) => {
                        self.record_error(&err).await;
                        Err(err)
                    }
                }
            }
            AudioBackendKind::Librespot => {
                #[cfg(feature = "librespot-backend")]
                {
                    if !self.librespot.is_connected().await {
                        let err = AudioError::BackendUnavailable(
                            "Librespot direct playback is blocked for third-party OAuth tokens since Aug 2026 (librespot#1737). Use Spotify Soloist, Spotify Connect, or Local Player instead."
                                .to_string(),
                        );
                        self.record_error(&err).await;
                        return Err(err);
                    }
                    if let Err(err) = self.librespot.play(&track.uri).await {
                        self.record_error(&err).await;
                        return Err(err);
                    }
                    // Librespot updates Playing asynchronously via PlayerEvent::Playing
                    self.emit_snapshot().await;
                    Ok(())
                }
                #[cfg(not(feature = "librespot-backend"))]
                {
                    let err = AudioError::BackendUnavailable(
                        "Librespot is not compiled in this build (and third-party OAuth login5 is blocked since Aug 2026, librespot#1737). Use Spotify Soloist, Spotify Connect, or Local Player."
                            .to_string(),
                    );
                    self.record_error(&err).await;
                    Err(err)
                }
            }
        }
    }

    async fn record_error(&self, err: &AudioError) {
        {
            let mut guard = self.state.write().await;
            guard.status = PlaybackStatus::Error(err.to_string());
            guard.clock.on_pause(None);
        }
        self.events.publish(PlaybackEvent::Error(err.to_string()));
        self.emit_snapshot().await;
    }

    async fn emit_snapshot(&self) {
        let snap = self.snapshot().await;
        self.events.publish(PlaybackEvent::Snapshot(snap));
    }

    async fn emit_queue_and_persist(&self) {
        let (items, current_index, shuffle, repeat) = {
            let guard = self.state.read().await;
            (
                guard.queue.items_in_playback_order(),
                guard.queue.current_index(),
                guard.queue.shuffle,
                guard.queue.repeat,
            )
        };
        self.events.publish(PlaybackEvent::QueueChanged {
            items,
            current_index,
            shuffle,
            repeat,
        });
        self.persist_preferences().await;
    }

    pub async fn persist_preferences(&self) {
        let Some(db) = &self.db else {
            return;
        };
        let prefs = {
            let guard = self.state.read().await;
            let pos_ms = if guard.active_backend == AudioBackendKind::LocalRodio
                && self.local_player.has_active_sink()
            {
                self.local_player.position().current_ms()
            } else {
                guard.clock.position_ms()
            };
            PlaybackPreferences {
                volume: guard.volume,
                shuffle: guard.queue.shuffle,
                repeat_mode: guard.queue.repeat,
                backend_kind: guard.preferred_backend,
                last_track: guard.queue.current().cloned(),
                last_position_ms: pos_ms,
                last_queue: guard
                    .queue
                    .items_in_playback_order()
                    .into_iter()
                    .take(50)
                    .collect(),
            }
        };
        let _ = db.save_playback_preferences(&prefs);
    }
}

/// Applies confirmed remote Spotify Connect state into `InnerPlaybackState` via `PlaybackClock`
/// drift reconciliation (deadband / soft blend / hard snap / track reset).
/// Returns `Some(track)` if the remotely playing track changed.
fn apply_remote_state(
    guard: &mut InnerPlaybackState,
    remote: &RemotePlaybackState,
) -> Option<Track> {
    let remote_track_id = remote.track.as_ref().map(|t| t.id.as_str());
    let outcome = guard.clock.reconcile_remote(
        remote_track_id,
        remote.progress_ms,
        remote.duration_ms,
        remote.is_playing,
        remote.rtt,
    );
    match outcome {
        ClockSyncOutcome::SoftCorrected { delta_ms } => {
            tracing::debug!(
                delta_ms,
                rtt_ms = remote.rtt.as_millis() as u64,
                "Spotify Connect soft-blending playback drift"
            );
        }
        ClockSyncOutcome::HardSnapped { delta_ms } => {
            tracing::debug!(
                delta_ms,
                rtt_ms = remote.rtt.as_millis() as u64,
                "Spotify Connect hard-snapped playback position"
            );
        }
        _ => {}
    }

    guard.status = if remote.is_playing {
        PlaybackStatus::Playing
    } else if remote.track.is_some() {
        PlaybackStatus::Paused
    } else {
        PlaybackStatus::Stopped
    };
    guard.active_device_name = remote.device_name.clone();
    guard.active_device_type = remote.device_type.clone();
    guard.queue.shuffle = remote.shuffle_state;
    guard.queue.repeat = remote.repeat_state;
    if let Some(pct) = remote.volume_percent {
        guard.volume = (f32::from(pct) / 100.0).clamp(0.0, 1.0);
    }

    if let Some(rt) = &remote.track {
        let cur_id = guard.queue.current().map(|t| t.id.as_str());
        if cur_id != Some(rt.id.as_str()) {
            // Select existing queue item if present; otherwise place remote track at current slot
            let found_idx = guard
                .queue
                .items_in_playback_order()
                .iter()
                .position(|t| t.id == rt.id || t.uri == rt.uri);
            if let Some(idx) = found_idx {
                guard.queue.jump_to(idx);
            } else {
                guard.queue.set_context(vec![rt.clone()], Some(&rt.id));
            }
            return Some(rt.clone());
        }
    }
    None
}

#[async_trait]
impl AudioEngine for PlaybackController {
    async fn play(&self, track_id: &str) -> Result<(), AudioError> {
        let existing = {
            let mut guard = self.state.write().await;
            let found = guard
                .queue
                .items_in_playback_order()
                .iter()
                .position(|t| t.id == track_id || t.uri == track_id);
            if let Some(idx) = found {
                guard.queue.jump_to(idx).cloned()
            } else {
                None
            }
        };

        if existing.is_some() {
            return self.start_current_track().await;
        }

        let synthetic = Track {
            id: track_id.to_string(),
            uri: if track_id.starts_with("spotify:") {
                track_id.to_string()
            } else {
                format!("spotify:track:{track_id}")
            },
            title: track_id.to_string(),
            artists: Vec::new(),
            album_id: None,
            album_name: String::new(),
            duration_ms: 0,
            isrc: None,
            artwork_url: None,
            explicit: false,
            liked: false,
            local_path: None,
            accent_rgb: None,
        };
        self.play_track_in_context(synthetic, Vec::new()).await
    }

    async fn pause(&self) -> Result<(), AudioError> {
        let backend = self.state.read().await.active_backend;
        match backend {
            AudioBackendKind::LocalRodio => {
                self.local_player.pause();
                let pos = self.local_player.position();
                {
                    let mut guard = self.state.write().await;
                    let cur_id = guard.queue.current().map(|t| t.id.clone());
                    guard
                        .clock
                        .on_track_change(cur_id, pos.duration_ms, pos.position_ms, false);
                    guard.status = PlaybackStatus::Paused;
                }
                self.persist_preferences().await;
                self.emit_snapshot().await;
                Ok(())
            }
            AudioBackendKind::SpotifySoloist => {
                if let Err(err) = self.soloist.pause().await {
                    self.record_error(&err).await;
                    return Err(err);
                }
                {
                    let mut guard = self.state.write().await;
                    guard.clock.on_pause(None);
                    guard.status = PlaybackStatus::Paused;
                }
                self.persist_preferences().await;
                self.emit_snapshot().await;
                Ok(())
            }
            AudioBackendKind::SpotifyConnect => {
                let Some(connect) = &self.connect else {
                    return Err(AudioError::Unauthenticated);
                };
                if !connect.is_authenticated().await {
                    return Err(AudioError::Unauthenticated);
                }
                if let Err(err) = connect.pause().await {
                    self.record_error(&err).await;
                    return Err(err);
                }
                if let Ok(Some(remote)) = connect.confirm_remote_state(Some(false)).await {
                    let mut guard = self.state.write().await;
                    apply_remote_state(&mut guard, &remote);
                } else {
                    let mut guard = self.state.write().await;
                    guard.clock.on_pause(None);
                    guard.status = PlaybackStatus::Paused;
                }
                self.persist_preferences().await;
                self.emit_snapshot().await;
                Ok(())
            }
            AudioBackendKind::Librespot => {
                #[cfg(feature = "librespot-backend")]
                {
                    self.librespot.pause().await?;
                }
                let mut guard = self.state.write().await;
                guard.clock.on_pause(None);
                guard.status = PlaybackStatus::Paused;
                drop(guard);
                self.emit_snapshot().await;
                Ok(())
            }
        }
    }

    async fn resume(&self) -> Result<(), AudioError> {
        let (backend, saved_pos_ms, has_local_track) = {
            let guard = self.state.read().await;
            (
                guard.active_backend,
                guard.clock.position_ms(),
                guard
                    .queue
                    .current()
                    .and_then(|t| t.local_path.as_ref())
                    .is_some(),
            )
        };

        match backend {
            AudioBackendKind::LocalRodio => {
                if self.local_player.has_active_sink() {
                    if let Err(err) = self.local_player.resume() {
                        self.record_error(&err).await;
                        return Err(err);
                    }
                    let pos = self.local_player.position();
                    {
                        let mut guard = self.state.write().await;
                        let cur_id = guard.queue.current().map(|t| t.id.clone());
                        guard.clock.on_track_change(
                            cur_id,
                            pos.duration_ms,
                            pos.position_ms,
                            pos.is_playing,
                        );
                        guard.status = PlaybackStatus::Playing;
                    }
                    self.emit_snapshot().await;
                    Ok(())
                } else if has_local_track {
                    // Restored paused local track after restart: reload file & seek cleanly
                    self.start_current_track().await?;
                    if saved_pos_ms > 0 {
                        let _ = self.seek(saved_pos_ms).await;
                    }
                    Ok(())
                } else {
                    let err = AudioError::InvalidTrack(
                        "No local audio track loaded to resume".to_string(),
                    );
                    self.record_error(&err).await;
                    Err(err)
                }
            }
            AudioBackendKind::SpotifySoloist => {
                {
                    let mut guard = self.state.write().await;
                    guard.status = PlaybackStatus::Loading;
                    guard.clock.on_pause(None);
                }
                self.emit_snapshot().await;
                if let Err(err) = self.soloist.resume().await {
                    self.record_error(&err).await;
                    return Err(err);
                }
                Ok(())
            }
            AudioBackendKind::SpotifyConnect => {
                let Some(connect) = &self.connect else {
                    let err = AudioError::Unauthenticated;
                    self.record_error(&err).await;
                    return Err(err);
                };
                if !connect.is_authenticated().await {
                    let err = AudioError::Unauthenticated;
                    self.record_error(&err).await;
                    return Err(err);
                }
                {
                    let mut guard = self.state.write().await;
                    guard.status = PlaybackStatus::Loading;
                    guard.clock.on_pause(None);
                }
                self.emit_snapshot().await;

                if let Err(err) = connect.resume().await {
                    self.record_error(&err).await;
                    return Err(err);
                }
                match connect.confirm_remote_state(Some(true)).await {
                    Ok(Some(remote)) => {
                        let playing = remote.is_playing;
                        {
                            let mut guard = self.state.write().await;
                            apply_remote_state(&mut guard, &remote);
                        }
                        self.emit_snapshot().await;
                        if playing {
                            Ok(())
                        } else {
                            let err = AudioError::CommandFailed(
                                "Spotify Connect device did not confirm resume".to_string(),
                            );
                            self.events.publish(PlaybackEvent::Error(err.to_string()));
                            Err(err)
                        }
                    }
                    Ok(None) => {
                        let err = AudioError::NoActiveDevice;
                        self.record_error(&err).await;
                        Err(err)
                    }
                    Err(err) => {
                        self.record_error(&err).await;
                        Err(err)
                    }
                }
            }
            AudioBackendKind::Librespot => {
                #[cfg(feature = "librespot-backend")]
                {
                    self.librespot.resume().await?;
                    Ok(())
                }
                #[cfg(not(feature = "librespot-backend"))]
                {
                    Err(AudioError::BackendUnavailable(
                        "Librespot is not enabled in this build".to_string(),
                    ))
                }
            }
        }
    }

    async fn seek(&self, position_ms: u64) -> Result<(), AudioError> {
        let (backend, duration) = {
            let guard = self.state.read().await;
            (guard.active_backend, guard.clock.duration_ms())
        };
        let clamped = if duration > 0 {
            position_ms.min(duration)
        } else {
            position_ms
        };

        match backend {
            AudioBackendKind::LocalRodio => {
                if let Err(err) = self.local_player.seek(clamped) {
                    self.record_error(&err).await;
                    return Err(err);
                }
                let pos = self.local_player.position();
                {
                    let mut guard = self.state.write().await;
                    let cur_id = guard.queue.current().map(|t| t.id.clone());
                    guard.clock.on_track_change(
                        cur_id,
                        pos.duration_ms,
                        pos.position_ms,
                        pos.is_playing,
                    );
                }
            }
            AudioBackendKind::SpotifySoloist => {
                if let Err(err) = self.soloist.seek(clamped).await {
                    self.record_error(&err).await;
                    return Err(err);
                }
                let mut guard = self.state.write().await;
                guard.clock.on_seek(clamped);
            }
            AudioBackendKind::SpotifyConnect => {
                let Some(connect) = &self.connect else {
                    return Err(AudioError::Unauthenticated);
                };
                if !connect.is_authenticated().await {
                    return Err(AudioError::Unauthenticated);
                }
                if let Err(err) = connect.seek(clamped).await {
                    self.record_error(&err).await;
                    return Err(err);
                }
                // Reset clock anchor immediately on seek (zero smoothing from pre-seek position!)
                {
                    let mut guard = self.state.write().await;
                    guard.clock.on_seek(clamped);
                }
                if let Ok(Some(remote)) = connect.confirm_remote_state(None).await {
                    let mut guard = self.state.write().await;
                    apply_remote_state(&mut guard, &remote);
                }
            }
            AudioBackendKind::Librespot => {
                #[cfg(feature = "librespot-backend")]
                self.librespot.seek(clamped).await?;
            }
        }

        self.events.publish(PlaybackEvent::Seeked {
            position_ms: clamped,
        });
        self.emit_snapshot().await;
        Ok(())
    }

    async fn set_volume(&self, volume: f32) -> Result<(), AudioError> {
        let clamped = volume.clamp(0.0, 1.0);
        let backend = self.state.read().await.active_backend;
        match backend {
            AudioBackendKind::LocalRodio => self.local_player.set_volume(clamped),
            AudioBackendKind::SpotifySoloist => {
                if let Err(err) = self.soloist.set_volume(clamped).await {
                    self.events.publish(PlaybackEvent::Error(err.to_string()));
                    return Err(err);
                }
            }
            AudioBackendKind::SpotifyConnect => {
                let Some(connect) = &self.connect else {
                    return Err(AudioError::Unauthenticated);
                };
                if !connect.is_authenticated().await {
                    return Err(AudioError::Unauthenticated);
                }
                if let Err(err) = connect.set_volume(clamped).await {
                    self.events.publish(PlaybackEvent::Error(err.to_string()));
                    return Err(err);
                }
            }
            AudioBackendKind::Librespot => {
                #[cfg(feature = "librespot-backend")]
                self.librespot.set_volume(clamped).await?;
            }
        }
        {
            let mut guard = self.state.write().await;
            guard.volume = clamped;
        }
        self.events
            .publish(PlaybackEvent::VolumeChanged { volume: clamped });
        self.persist_preferences().await;
        self.emit_snapshot().await;
        Ok(())
    }

    async fn next(&self) -> Result<(), AudioError> {
        let backend = self.state.read().await.active_backend;
        if backend == AudioBackendKind::SpotifyConnect {
            let Some(connect) = &self.connect else {
                return Err(AudioError::Unauthenticated);
            };
            if !connect.is_authenticated().await {
                return Err(AudioError::Unauthenticated);
            }
            if let Err(err) = connect.next().await {
                self.record_error(&err).await;
                return Err(err);
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
            return self.sync_connect_now().await;
        }

        if backend == AudioBackendKind::SpotifySoloist {
            let has_local_context_next = {
                let mut guard = self.state.write().await;
                if !guard.soloist_owns_queue && guard.queue.items_in_playback_order().len() > 1 {
                    let prev_repeat = guard.queue.repeat;
                    if prev_repeat == RepeatMode::Track {
                        guard.queue.repeat = RepeatMode::Off;
                    }
                    let moved = guard.queue.next_track().is_some();
                    guard.queue.repeat = prev_repeat;
                    moved
                } else {
                    false
                }
            };
            if has_local_context_next {
                return self.start_current_track().await;
            }
            if let Err(err) = self.soloist.next().await {
                self.record_error(&err).await;
                return Err(err);
            }
            return Ok(());
        }

        let has_next = {
            let mut guard = self.state.write().await;
            guard.queue.next_track().is_some()
        };
        if has_next {
            self.start_current_track().await
        } else {
            self.pause().await
        }
    }

    async fn previous(&self) -> Result<(), AudioError> {
        let pos_ms = self.get_position().await.current_ms();
        // Standard desktop player UX: restart current track if > 3 seconds into playback
        if pos_ms > 3_000 {
            return self.seek(0).await;
        }

        let backend = self.state.read().await.active_backend;
        if backend == AudioBackendKind::SpotifyConnect {
            let Some(connect) = &self.connect else {
                return Err(AudioError::Unauthenticated);
            };
            if !connect.is_authenticated().await {
                return Err(AudioError::Unauthenticated);
            }
            if let Err(err) = connect.previous().await {
                self.record_error(&err).await;
                return Err(err);
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
            return self.sync_connect_now().await;
        }

        if backend == AudioBackendKind::SpotifySoloist {
            let has_local_context_prev = {
                let mut guard = self.state.write().await;
                if !guard.soloist_owns_queue && guard.queue.items_in_playback_order().len() > 1 {
                    guard.queue.previous_track().is_some()
                } else {
                    false
                }
            };
            if has_local_context_prev {
                return self.start_current_track().await;
            }
            if let Err(err) = self.soloist.previous().await {
                self.record_error(&err).await;
                return Err(err);
            }
            return Ok(());
        }

        let has_prev = {
            let mut guard = self.state.write().await;
            guard.queue.previous_track().is_some()
        };
        if has_prev {
            self.start_current_track().await
        } else {
            self.seek(0).await
        }
    }

    async fn get_position(&self) -> PlaybackPosition {
        let guard = self.state.read().await;
        match guard.active_backend {
            AudioBackendKind::LocalRodio if self.local_player.has_active_sink() => {
                self.local_player.position()
            }
            _ => guard.clock.to_playback_position(),
        }
    }
}
