//! Main `SpotirustApp` implementing `eframe::App` and coordinating the Tokio async runtime,
//! OAuth session lifecycle, Spotify Web API, audio playback controller, lyrics service,
//! artwork cache, SQLite local library, and OS media controls (Linux MPRIS2).

use crate::audio::engine::AudioEngine;
use crate::audio::events::{PlaybackEvent, PlaybackEventBus};
use crate::audio::local_rodio::{
    LocalScanReport, import_local_files_into_db, scan_local_library_into_db,
};
use crate::audio::playback::PlaybackController;
use crate::audio::soloist::{SoloistEngine, SoloistEvent};
use crate::auth::session::SessionManager;
use crate::core::config::AppConfig;
use crate::core::events::{AppCommand, AppEvent};
use crate::core::models::{AudioBackendKind, Route};
use crate::core::plugins::{BuiltinSessionMetricsPlugin, PluginManifest, PluginRegistry};
use crate::core::state::{AppState, AuthUiState, LyricsUiState, SideDrawer};
use crate::lyrics::cache::LyricsCache;
use crate::lyrics::provider::LyricsService;
use crate::spotify::client::SpotifyClient;
use crate::spotify::library::LibraryService;
use crate::spotify::playlists::PlaylistService;
use crate::spotify::search::SearchService;
use crate::storage::artwork::ArtworkService;
use crate::storage::credentials::{CredentialStore, ResilientCredentialStore};
use crate::storage::database::{CacheStats, Database, PlaybackPreferences};
use crate::system::media_controls::{SystemMediaBridge, SystemMediaCommand};
use crate::ui::components::artwork::ArtworkTextureCache;
use crate::ui::lyrics::{KaraokeRenderer, next_karaoke_repaint_delay};
use crate::ui::theme::ThemeManager;
use egui::{CornerRadius, Frame, Margin, Panel, Stroke};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Root native desktop application state owned by `eframe`.
pub struct SpotirustApp {
    state: AppState,
    theme_mgr: ThemeManager,
    artwork_cache: ArtworkTextureCache,
    karaoke_renderer: KaraokeRenderer,
    cmd_tx: mpsc::UnboundedSender<AppCommand>,
    evt_rx: mpsc::UnboundedReceiver<AppEvent>,
    db: Database,
    soloist: SoloistEngine,
    credential_backend: String,
    cached_stats: CacheStats,
    plugin_manifests: Vec<PluginManifest>,
    pending_auth_url: Option<String>,
    /// Keep the Tokio runtime alive for the full lifetime of the desktop window.
    _runtime: tokio::runtime::Runtime,
}

impl SpotirustApp {
    /// Bootstrap persistence, plugins, themes, Tokio runtime, and background workers.
    pub fn bootstrap(cc: &eframe::CreationContext<'_>, config: AppConfig) -> anyhow::Result<Self> {
        config.ensure_dirs()?;

        let db = Database::open(&config.db_path)?;
        let settings = db.load_settings().unwrap_or_default();
        let cached_stats = db.cache_stats().unwrap_or_default();

        let mut plugins = PluginRegistry::new();
        plugins.register(Arc::new(BuiltinSessionMetricsPlugin));
        let plugin_manifests = plugins.manifests();
        let plugin_themes = plugins.all_contributed_themes();
        let plugins = Arc::new(plugins);

        let mut theme_mgr = ThemeManager::new(&config.themes_dir, plugin_themes);
        theme_mgr.set_active(&settings.theme_id);
        theme_mgr.active().apply_to_egui(&cc.egui_ctx);

        let credentials = Arc::new(ResilientCredentialStore::new());
        let credential_backend = credentials.backend_name().to_string();

        let session = SessionManager::new(
            credentials,
            config.spotify_client_id.clone(),
            config.oauth_redirect_port,
            config.oauth_redirect_path.clone(),
        )
        .with_redirect_uri_error(config.oauth_redirect_uri_error.clone());
        let client_id_configured = session.has_client_id();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("spotirust-async")
            .enable_all()
            .build()?;

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<AppCommand>();
        let (evt_tx, evt_rx) = mpsc::unbounded_channel::<AppEvent>();

        let mut state = AppState::new(settings.clone(), client_id_configured, config.portable_mode);
        let soundcheck_dir = config.cache_dir.join("soundcheck");
        state.showcase_tracks = crate::core::models::bundled_showcase_tracks_with_paths(
            Some(soundcheck_dir.join("midnight_compiler.wav")),
            Some(soundcheck_dir.join("velocity_pulse.wav")),
        );

        // Pre-populate UI immediately from local SQLite cache (instant startup — no folder rescan!)
        state.liked_tracks = db.load_liked_tracks(100).unwrap_or_default();
        state.recently_played = db.load_recently_played(30).unwrap_or_default();
        state.playlists = db.load_cached_playlists(50).unwrap_or_default();
        state.albums = db.load_cached_albums(50).unwrap_or_default();
        state.local_tracks = db.load_local_tracks().unwrap_or_default();

        let spotify_client = SpotifyClient::new(session.clone());
        let event_bus = PlaybackEventBus::default();
        let soloist = SoloistEngine::new(
            config.soloist_bin_dir.clone(),
            config.soloist_data_dir.clone(),
            config.soloist_cache_dir.clone(),
        );
        let playback = PlaybackController::with_soloist(
            Some(spotify_client.clone()),
            Some(db.clone()),
            event_bus.clone(),
            soloist.clone(),
        );

        let initial_soloist_key = crate::storage::credentials::load_soloist_api_key()
            .or_else(crate::audio::soloist::resolve_env_soloist_key);
        state.soloist.api_key_configured = initial_soloist_key.is_some();

        let default_lyrics_dir = config.lyrics_dir.clone();
        let mut lyrics_dirs = vec![default_lyrics_dir.clone()];
        lyrics_dirs.extend(settings.local_lyrics_dirs.clone());
        let initial_chain_config = settings.lyrics_chain_config();
        let initial_mxm_key = crate::storage::credentials::load_musixmatch_api_key()
            .or_else(crate::lyrics::musixmatch::resolve_env_musixmatch_key);
        let mut lyrics_service = LyricsService::new(LyricsCache::new(db.clone()), lyrics_dirs)
            .with_chain_config(initial_chain_config.clone());
        lyrics_service.set_musixmatch_api_key(initial_mxm_key);
        state.musixmatch_key_configured = lyrics_service.has_musixmatch_api_key();
        let artwork_service = ArtworkService::new(config.artwork_cache_dir.clone());

        let egui_ctx = cc.egui_ctx.clone();
        spawn_backend_coordinator(
            &runtime,
            egui_ctx,
            cmd_rx,
            evt_tx,
            session,
            spotify_client,
            playback,
            lyrics_service,
            artwork_service,
            db.clone(),
            plugins,
            settings.lyrics_source,
            initial_chain_config,
            settings.local_music_dirs.clone(),
            default_lyrics_dir,
            config.cache_dir.clone(),
            config.artwork_cache_dir.clone(),
            initial_soloist_key,
            settings.soloist_device_name.clone(),
            settings.soloist_custom_binary_path.clone(),
            settings.soloist_auto_start,
        );

        Ok(Self {
            state,
            theme_mgr,
            artwork_cache: ArtworkTextureCache::default(),
            karaoke_renderer: KaraokeRenderer::default(),
            cmd_tx,
            evt_rx,
            db,
            soloist,
            credential_backend,
            cached_stats,
            plugin_manifests,
            pending_auth_url: None,
            _runtime: runtime,
        })
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        use crate::core::state::LoadState;

        while let Ok(evt) = self.evt_rx.try_recv() {
            match evt {
                AppEvent::AuthOpeningBrowser { authorize_url } => {
                    self.pending_auth_url = Some(authorize_url.clone());
                    self.state.auth = AuthUiState::OpeningBrowser { authorize_url };
                }
                AppEvent::AuthWaitingForCallback { authorize_url } => {
                    self.pending_auth_url = Some(authorize_url.clone());
                    self.state.auth = AuthUiState::WaitingForAuthorization { authorize_url };
                }
                AppEvent::AuthExchangingToken => {
                    self.state.auth = AuthUiState::ExchangingToken;
                }
                AppEvent::AuthRefreshing => {
                    self.state.auth = AuthUiState::Refreshing;
                }
                AppEvent::AuthSucceeded { profile } => {
                    self.pending_auth_url = None;
                    self.state
                        .set_notice(format!("Signed in as {}", profile.display_name), false);
                    self.state.auth = AuthUiState::Authenticated(profile);
                    let _ = self.cmd_tx.send(AppCommand::RefreshLibrary);
                    let _ = self.cmd_tx.send(AppCommand::RefreshDevices);
                    let _ = self.cmd_tx.send(AppCommand::SyncConnectState);
                }
                AppEvent::AuthFailed {
                    message,
                    authorize_url,
                } => {
                    if let Some(u) = &authorize_url {
                        self.pending_auth_url = Some(u.clone());
                    }
                    self.state.auth = AuthUiState::Failed {
                        message: message.clone(),
                        authorize_url,
                    };
                    self.state.set_notice(message, true);
                }
                AppEvent::SessionExpired { message } => {
                    self.pending_auth_url = None;
                    self.state.auth = AuthUiState::SessionExpired {
                        message: message.clone(),
                    };
                    self.state.liked_tracks.clear();
                    self.state.top_tracks.clear();
                    self.state.playlists.clear();
                    self.state.albums.clear();
                    self.state.artists.clear();
                    self.state.playlist_tracks.clear();
                    self.state.album_tracks.clear();
                    self.state.artist_albums.clear();
                    self.state.devices.clear();
                    self.state.set_notice(message, true);
                }
                AppEvent::AuthLoggedOut => {
                    self.pending_auth_url = None;
                    self.state.auth = AuthUiState::SignedOut;
                    self.state.liked_tracks.clear();
                    self.state.top_tracks.clear();
                    self.state.playlists.clear();
                    self.state.albums.clear();
                    self.state.artists.clear();
                    self.state.playlist_tracks.clear();
                    self.state.album_tracks.clear();
                    self.state.artist_albums.clear();
                    self.state.devices.clear();
                    self.state.set_notice(
                        "Signed out cleanly. Credentials removed from OS vault.",
                        false,
                    );
                }
                AppEvent::LibraryLoaded {
                    liked_tracks,
                    recently_played,
                    top_tracks,
                    playlists,
                    albums,
                    artists,
                } => {
                    self.state.liked_tracks = liked_tracks;
                    if !recently_played.is_empty() {
                        self.state.recently_played = recently_played;
                    }
                    self.state.top_tracks = top_tracks;
                    self.state.playlists = playlists;
                    self.state.albums = albums;
                    self.state.artists = artists;
                    self.cached_stats = self.db.cache_stats().unwrap_or_default();
                }
                AppEvent::SearchCompleted(results) => {
                    let key = results.query.to_lowercase();
                    self.state.search_cache.insert(key, results.clone());
                    // Guard against out-of-order stale search responses overwriting newer queries
                    if results
                        .query
                        .eq_ignore_ascii_case(self.state.last_dispatched_query.trim())
                    {
                        self.state.search_results = Some(results);
                        self.state.search_loading = false;
                        self.state.search_error = None;
                    }
                }
                AppEvent::SearchFailed { query, error } => {
                    if query.eq_ignore_ascii_case(self.state.last_dispatched_query.trim()) {
                        self.state.search_loading = false;
                        self.state.search_error = Some(error);
                    }
                }
                AppEvent::PlaylistTracksLoaded {
                    playlist_id,
                    tracks,
                } => {
                    self.state
                        .playlist_tracks
                        .insert(playlist_id, LoadState::Loaded(tracks));
                }
                AppEvent::PlaylistTracksFailed { playlist_id, error } => {
                    self.state
                        .playlist_tracks
                        .insert(playlist_id, LoadState::Failed(error));
                }
                AppEvent::AlbumTracksLoaded { album_id, tracks } => {
                    self.state
                        .album_tracks
                        .insert(album_id, LoadState::Loaded(tracks));
                }
                AppEvent::AlbumTracksFailed { album_id, error } => {
                    self.state
                        .album_tracks
                        .insert(album_id, LoadState::Failed(error));
                }
                AppEvent::ArtistAlbumsLoaded { artist_id, albums } => {
                    self.state
                        .artist_albums
                        .insert(artist_id, LoadState::Loaded(albums));
                }
                AppEvent::ArtistAlbumsFailed { artist_id, error } => {
                    self.state
                        .artist_albums
                        .insert(artist_id, LoadState::Failed(error));
                }
                AppEvent::TrackLikeUpdated { mut track, liked } => {
                    track.liked = liked;
                    let track_id = track.id.clone();

                    if let Some(cur) = self.state.playback.current_track.as_mut()
                        && cur.id == track_id
                    {
                        cur.liked = liked;
                    }

                    // Keep Liked Songs collection synchronized immediately
                    if liked {
                        if let Some(existing) = self
                            .state
                            .liked_tracks
                            .iter_mut()
                            .find(|t| t.id == track_id)
                        {
                            existing.liked = true;
                        } else {
                            self.state.liked_tracks.insert(0, track);
                        }
                    } else {
                        self.state.liked_tracks.retain(|t| t.id != track_id);
                    }

                    for list in [
                        &mut self.state.recently_played,
                        &mut self.state.top_tracks,
                        &mut self.state.showcase_tracks,
                        &mut self.state.local_tracks,
                        &mut self.state.queue_items,
                    ] {
                        for t in list.iter_mut() {
                            if t.id == track_id {
                                t.liked = liked;
                            }
                        }
                    }
                    if let Some(res) = self.state.search_results.as_mut() {
                        for t in &mut res.tracks {
                            if t.id == track_id {
                                t.liked = liked;
                            }
                        }
                    }
                    for load in self.state.playlist_tracks.values_mut() {
                        if let LoadState::Loaded(tracks) = load {
                            for t in tracks.iter_mut() {
                                if t.id == track_id {
                                    t.liked = liked;
                                }
                            }
                        }
                    }
                    for load in self.state.album_tracks.values_mut() {
                        if let LoadState::Loaded(tracks) = load {
                            for t in tracks.iter_mut() {
                                if t.id == track_id {
                                    t.liked = liked;
                                }
                            }
                        }
                    }
                }
                AppEvent::DevicesLoaded(devices) => {
                    self.state.devices = devices;
                }
                AppEvent::PlaybackUpdated(snapshot) => {
                    let new_track = snapshot.current_track.clone();
                    self.state.playback = snapshot;
                    if let Some(track) = new_track
                        && self.state.last_lyrics_track_id.as_deref() != Some(track.id.as_str())
                    {
                        self.state.last_lyrics_track_id = Some(track.id.clone());
                        // Immediately clear prior track's lyrics & layout cache so stale lyrics never flash
                        self.state.lyrics = LyricsUiState::Loading {
                            track_id: track.id.clone(),
                        };
                        self.karaoke_renderer.reset();
                        self.state.lyrics_auto_scroll = true;
                        self.state.last_scrolled_line_idx = None;
                        self.state.show_lyrics_import = false;
                        let _ = self.cmd_tx.send(AppCommand::FetchLyrics {
                            track,
                            force_refresh: false,
                        });
                    }
                }
                AppEvent::SoloistStatusUpdated(snap) => {
                    let newly_paired = !self.state.soloist.logged_in && snap.logged_in;
                    self.state.soloist = snap;
                    if newly_paired {
                        self.state.set_notice(
                            format!(
                                "Spotify Soloist '{}' paired & ready! You can now close the official Spotify app.",
                                self.state.soloist.device_name
                            ),
                            false,
                        );
                    }
                }
                AppEvent::SoloistCustomBinaryUpdated(path) => {
                    self.state.settings.soloist_custom_binary_path = path;
                }
                AppEvent::QueueUpdated {
                    items,
                    current_index,
                    shuffle,
                    repeat,
                } => {
                    self.state.queue_items = items;
                    self.state.queue_index = current_index;
                    self.state.shuffle = shuffle;
                    self.state.repeat = repeat;
                }
                AppEvent::LocalScanStarted => {
                    self.state.local_scan_in_progress = true;
                }
                AppEvent::LocalTracksScanned {
                    tracks,
                    added_folder,
                } => {
                    self.state.local_scan_in_progress = false;
                    self.state.local_tracks = tracks;
                    if let Some(folder) = added_folder
                        && !self.state.settings.local_music_dirs.contains(&folder)
                    {
                        self.state.settings.local_music_dirs.push(folder);
                        let _ = self.db.save_settings(&self.state.settings);
                    }
                    self.cached_stats = self.db.cache_stats().unwrap_or_default();
                }
                AppEvent::SystemMediaStatusUpdated(status) => {
                    self.state.system_media_status = status;
                }
                AppEvent::LyricsLoading { track_id } => {
                    if self.is_current_track_id(&track_id) {
                        self.state.lyrics = LyricsUiState::Loading { track_id };
                        self.karaoke_renderer.reset();
                    }
                }
                AppEvent::LyricsLoaded { track_id, document } => {
                    // Guard against slow async lyrics fetch completing after switching tracks
                    if self.is_current_track_id(&track_id) {
                        let revision = self.state.next_lyrics_revision();
                        self.state.lyrics = LyricsUiState::Loaded {
                            track_id,
                            document: Arc::new(document),
                            revision,
                        };
                        self.state.last_scrolled_line_idx = None;
                        self.cached_stats = self.db.cache_stats().unwrap_or_default();
                    }
                }
                AppEvent::LyricsAmbiguous {
                    track_id,
                    provider_name,
                    candidates,
                } => {
                    if self.is_current_track_id(&track_id) {
                        self.state.lyrics = LyricsUiState::Ambiguous {
                            track_id,
                            provider_name,
                            candidates,
                        };
                        self.karaoke_renderer.reset();
                    }
                }
                AppEvent::LyricsUnavailable { track_id, reason } => {
                    if self.is_current_track_id(&track_id) {
                        self.state.lyrics = LyricsUiState::Unavailable { track_id, reason };
                        self.karaoke_renderer.reset();
                    }
                }
                AppEvent::LyricsCacheCleared { freed_entries } => {
                    self.cached_stats = self.db.cache_stats().unwrap_or_default();
                    self.state.set_notice(
                        format!(
                            "Lyrics cache cleared ({freed_entries} cached lyrics entries removed)."
                        ),
                        false,
                    );
                }
                AppEvent::ArtworkLoaded {
                    url,
                    width,
                    height,
                    rgba,
                    dominant_rgb,
                } => {
                    self.artwork_cache
                        .insert(ctx, url, width, height, rgba, dominant_rgb);
                }
                AppEvent::ArtworkFailed { url } => {
                    self.artwork_cache.mark_failed(&url);
                }
                AppEvent::MusixmatchKeyUpdated { configured } => {
                    self.state.musixmatch_key_configured = configured;
                }
                AppEvent::StatusBanner { message, is_error } => {
                    self.state.set_notice(message, is_error);
                }
                AppEvent::CacheCleared { freed_entries } => {
                    self.artwork_cache.clear();
                    self.state.search_cache.clear();
                    self.cached_stats = self.db.cache_stats().unwrap_or_default();
                    self.state.set_notice(
                        format!("Cache cleared ({freed_entries} cached entries removed)."),
                        false,
                    );
                }
                AppEvent::ApplicationReset => {
                    self.artwork_cache.clear();
                    self.karaoke_renderer.reset();
                    self.state.search_cache.clear();
                    self.state.auth = AuthUiState::SignedOut;
                    self.state.liked_tracks.clear();
                    self.state.recently_played.clear();
                    self.state.top_tracks.clear();
                    self.state.playlists.clear();
                    self.state.albums.clear();
                    self.state.artists.clear();
                    self.state.local_tracks.clear();
                    self.state.playlist_tracks.clear();
                    self.state.album_tracks.clear();
                    self.state.artist_albums.clear();
                    self.state.settings = Default::default();
                    self.state.musixmatch_key_configured =
                        crate::lyrics::musixmatch::resolve_env_musixmatch_key().is_some();
                    self.state.musixmatch_key_input.clear();
                    self.state.soloist_key_input.clear();
                    self.theme_mgr.set_active(&self.state.settings.theme_id);
                    self.theme_mgr.active().apply_to_egui(ctx);
                    self.cached_stats = self.db.cache_stats().unwrap_or_default();
                    self.state
                        .set_notice("Application reset to default state.", false);
                }
            }
        }
    }

    fn is_current_track_id(&self, track_id: &str) -> bool {
        self.state
            .playback
            .current_track
            .as_ref()
            .map(|t| t.id.as_str() == track_id)
            .unwrap_or(false)
    }

    fn dispatch_commands(&mut self, commands: Vec<AppCommand>) {
        use crate::core::state::LoadState;

        for cmd in commands {
            match &cmd {
                AppCommand::OpenPendingLoginUrl => {
                    if let Some(url) = self.pending_auth_url.clone() {
                        std::thread::spawn(move || {
                            if let Err(err) = open::that(&url) {
                                tracing::warn!(error = %err, "Failed opening Spotify login URL in browser");
                            }
                        });
                    }
                    continue;
                }
                AppCommand::OpenExternalUrl { url } => {
                    if url.starts_with("https://") {
                        let url_owned = url.clone();
                        std::thread::spawn(move || {
                            if let Err(err) = open::that(&url_owned) {
                                tracing::warn!(error = %err, "Failed opening external URL in browser");
                            }
                        });
                    }
                    continue;
                }
                AppCommand::SetLyricsOffset { offset_ms } => {
                    let next_rev = self.state.next_lyrics_revision();
                    if let LyricsUiState::Loaded {
                        document, revision, ..
                    } = &mut self.state.lyrics
                    {
                        Arc::make_mut(document).offset_ms = *offset_ms;
                        *revision = next_rev;
                    }
                }
                AppCommand::LoadPlaylistTracks { playlist_id } => {
                    self.state
                        .playlist_tracks
                        .insert(playlist_id.clone(), LoadState::Loading);
                }
                AppCommand::LoadAlbumTracks { album_id } => {
                    self.state
                        .album_tracks
                        .insert(album_id.clone(), LoadState::Loading);
                }
                AppCommand::LoadArtistAlbums { artist_id } => {
                    self.state
                        .artist_albums
                        .insert(artist_id.clone(), LoadState::Loading);
                }
                _ => {}
            }
            let _ = self.cmd_tx.send(cmd);
        }
    }
}

impl eframe::App for SpotirustApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events(ctx);
        self.state.expire_old_notice();

        // Dispatch debounced catalog search if ready
        if let Some(query) = self.state.poll_debounced_search() {
            if matches!(self.state.auth, AuthUiState::Authenticated(_)) {
                let _ = self.cmd_tx.send(AppCommand::SearchCatalog { query });
            } else {
                self.state.search_loading = false;
            }
        }

        // Event-driven repaint scheduling: 0% CPU when idle, ~4 Hz outside active karaoke words,
        // and smooth ~30 FPS + exact word-boundary wakes only during active word sweeps.
        let is_playing = self.state.playback.status.is_playing();
        let lyrics_visible = self.state.current_route == Route::NowPlaying
            || self.state.side_drawer == SideDrawer::Lyrics;
        let audio_ms = self
            .state
            .seek_drag_ms
            .unwrap_or_else(|| self.state.playback.position.current_ms());

        if self.state.transition_progress() < 1.0 {
            ctx.request_repaint();
        } else if let Some(delay) = next_karaoke_repaint_delay(
            &self.state.lyrics,
            audio_ms,
            is_playing,
            lyrics_visible,
            self.state.settings.reduce_motion,
        ) {
            ctx.request_repaint_after(delay);
        } else if self.state.search_dirty_since.is_some() || self.state.local_scan_in_progress {
            ctx.request_repaint_after(Duration::from_millis(80));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mut commands = Vec::new();
        let theme = self.theme_mgr.active().clone();
        let desired_anim_time = if self.state.settings.reduce_motion {
            0.0
        } else {
            0.14
        };
        if (ui.style().animation_time - desired_anim_time).abs() > 0.001 {
            ui.ctx()
                .global_style_mut(|s| s.animation_time = desired_anim_time);
        }

        // Global keyboard media shortcuts when no text input holds keyboard focus
        if !ui.ctx().egui_wants_keyboard_input() {
            let (space, ctrl_right, ctrl_left, right, left, ctrl_up, ctrl_down) = ui.input(|i| {
                let ctrl = i.modifiers.ctrl || i.modifiers.command;
                (
                    i.key_pressed(egui::Key::Space),
                    ctrl && i.key_pressed(egui::Key::ArrowRight),
                    ctrl && i.key_pressed(egui::Key::ArrowLeft),
                    !ctrl && i.key_pressed(egui::Key::ArrowRight),
                    !ctrl && i.key_pressed(egui::Key::ArrowLeft),
                    ctrl && i.key_pressed(egui::Key::ArrowUp),
                    ctrl && i.key_pressed(egui::Key::ArrowDown),
                )
            });
            if space {
                commands.push(AppCommand::TogglePlayPause);
            } else if ctrl_right {
                commands.push(AppCommand::NextTrack);
            } else if ctrl_left {
                commands.push(AppCommand::PreviousTrack);
            } else if right && self.state.playback.position.duration_ms > 0 {
                let next_ms = self
                    .state
                    .playback
                    .position
                    .current_ms()
                    .saturating_add(5_000)
                    .min(self.state.playback.position.duration_ms);
                commands.push(AppCommand::SeekTo {
                    position_ms: next_ms,
                });
            } else if left && self.state.playback.position.duration_ms > 0 {
                let prev_ms = self
                    .state
                    .playback
                    .position
                    .current_ms()
                    .saturating_sub(5_000);
                commands.push(AppCommand::SeekTo {
                    position_ms: prev_ms,
                });
            } else if ctrl_up {
                let vol = (self.state.playback.volume + 0.05).clamp(0.0, 1.0);
                self.state.playback.volume = vol;
                commands.push(AppCommand::SetVolume { volume: vol });
            } else if ctrl_down {
                let vol = (self.state.playback.volume - 0.05).clamp(0.0, 1.0);
                self.state.playback.volume = vol;
                commands.push(AppCommand::SetVolume { volume: vol });
            }
        }

        // 1. Floating Liquid Glass Bottom Player Dock (iOS 26 / macOS Tahoe)
        let dock_canvas_bg = if self.state.current_route == Route::NowPlaying {
            if theme.dark {
                egui::Color32::from_rgb(10, 11, 14)
            } else {
                theme.bg_base
            }
        } else {
            theme.bg_base
        };
        Panel::bottom("spotirust_player_bar")
            .resizable(false)
            .exact_size(98.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(dock_canvas_bg)
                    .stroke(Stroke::NONE)
                    .inner_margin(Margin {
                        left: 14,
                        right: 14,
                        top: 6,
                        bottom: 12,
                    }),
            )
            .show(ui, |ui| {
                crate::ui::player::render_player_bar(
                    ui,
                    &mut self.state,
                    &theme,
                    &mut self.artwork_cache,
                    &mut commands,
                );
            });

        // 2. Floating Rounded Glass Left Navigation Sidebar (hidden in Fullscreen Now Playing)
        if self.state.current_route != Route::NowPlaying {
            Panel::left("spotirust_sidebar")
                .resizable(true)
                .default_size(262.0)
                .size_range(220.0..=320.0)
                .show_separator_line(false)
                .frame(
                    Frame::new()
                        .fill(theme.bg_base)
                        .stroke(Stroke::NONE)
                        .inner_margin(Margin {
                            left: 12,
                            right: 4,
                            top: 12,
                            bottom: 4,
                        }),
                )
                .show(ui, |ui| {
                    let sheet_rect = ui.available_rect_before_wrap();
                    crate::ui::theme::paint_liquid_glass(
                        ui.painter(),
                        sheet_rect,
                        20,
                        theme.glass_fill(),
                        None,
                        &theme,
                        false,
                    );
                    let inner_rect = sheet_rect.shrink2(egui::vec2(10.0, 12.0));
                    let mut inner_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(inner_rect)
                            .layout(egui::Layout::top_down(egui::Align::LEFT)),
                    );
                    inner_ui.set_clip_rect(inner_rect.intersect(ui.clip_rect()));
                    crate::ui::sidebar::render_sidebar(
                        &mut inner_ui,
                        &mut self.state,
                        &theme,
                        &mut commands,
                    );
                });
        }

        // 3. Optional Floating Glass Right Utility Drawer (Compact Lyrics or Queue)
        if self.state.current_route != Route::NowPlaying
            && self.state.side_drawer != SideDrawer::None
        {
            Panel::right("spotirust_right_drawer")
                .resizable(true)
                .default_size(346.0)
                .size_range(282.0..=468.0)
                .show_separator_line(false)
                .frame(
                    Frame::new()
                        .fill(theme.bg_base)
                        .stroke(Stroke::NONE)
                        .inner_margin(Margin {
                            left: 4,
                            right: 12,
                            top: 12,
                            bottom: 4,
                        }),
                )
                .show(ui, |ui| {
                    let sheet_rect = ui.available_rect_before_wrap();
                    crate::ui::theme::paint_liquid_glass(
                        ui.painter(),
                        sheet_rect,
                        20,
                        theme.glass_fill(),
                        None,
                        &theme,
                        false,
                    );
                    let inner_rect = sheet_rect.shrink(14.0);
                    let mut inner_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(inner_rect)
                            .layout(egui::Layout::top_down(egui::Align::LEFT)),
                    );
                    inner_ui.set_clip_rect(inner_rect.intersect(ui.clip_rect()));
                    match self.state.side_drawer {
                        SideDrawer::Lyrics => crate::ui::lyrics::render_lyrics_drawer(
                            &mut inner_ui,
                            &mut self.state,
                            &theme,
                            &mut self.karaoke_renderer,
                            &mut commands,
                        ),
                        SideDrawer::Queue => crate::ui::player::render_queue_drawer(
                            &mut inner_ui,
                            &mut self.state,
                            &theme,
                            &mut commands,
                        ),
                        SideDrawer::None => {}
                    }
                });
        }

        // 4. Floating Glass Capsules Top Navigation Bar (borderless over canvas)
        let topbar_bg = if self.state.current_route == Route::NowPlaying {
            if theme.dark {
                egui::Color32::from_rgb(10, 11, 14)
            } else {
                theme.bg_base
            }
        } else {
            theme.bg_base
        };
        Panel::top("spotirust_topbar")
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(topbar_bg)
                    .stroke(Stroke::NONE)
                    .inner_margin(Margin {
                        left: 22,
                        right: 22,
                        top: 12,
                        bottom: 6,
                    }),
            )
            .show(ui, |ui| {
                crate::ui::topbar::render_topbar(ui, &mut self.state, &theme, &mut commands);
            });

        // 5. Main Content Area
        let central_frame = if self.state.current_route == Route::NowPlaying {
            Frame::new().fill(theme.bg_base).inner_margin(Margin::ZERO)
        } else {
            Frame::new()
                .fill(theme.bg_base)
                .inner_margin(Margin::symmetric(28, 12))
                .corner_radius(CornerRadius::ZERO)
        };

        egui::CentralPanel::default()
            .frame(central_frame)
            .show(ui, |ui| {
                let route = self.state.current_route.clone();
                match route {
                    Route::Home => crate::ui::home::render_home(
                        ui,
                        &mut self.state,
                        &theme,
                        &mut self.artwork_cache,
                        &mut commands,
                    ),
                    Route::Search => crate::ui::search::render_search(
                        ui,
                        &mut self.state,
                        &theme,
                        &mut self.artwork_cache,
                        &mut commands,
                    ),
                    Route::Library => crate::ui::library::render_library_overview(
                        ui,
                        &mut self.state,
                        &theme,
                        &mut self.artwork_cache,
                        &mut commands,
                    ),
                    Route::LikedSongs => {
                        let tracks = self.state.liked_tracks.clone();
                        crate::ui::library::render_track_list_page(
                            ui,
                            "Liked Songs",
                            &format!("{} tracks saved in your library", tracks.len()),
                            &tracks,
                            &self.state,
                            &theme,
                            &mut self.artwork_cache,
                            &mut commands,
                        );
                    }
                    Route::RecentlyPlayed => {
                        let tracks = self.state.recently_played.clone();
                        crate::ui::library::render_track_list_page(
                            ui,
                            "Recently Played",
                            "Synced from Spotify & local playback history",
                            &tracks,
                            &self.state,
                            &theme,
                            &mut self.artwork_cache,
                            &mut commands,
                        );
                    }
                    Route::Playlists => crate::ui::library::render_playlists_page(
                        ui,
                        &mut self.state,
                        &theme,
                        &mut self.artwork_cache,
                        &mut commands,
                    ),
                    Route::Albums => crate::ui::library::render_albums_page(
                        ui,
                        &mut self.state,
                        &theme,
                        &mut self.artwork_cache,
                        &mut commands,
                    ),
                    Route::Artists => crate::ui::library::render_artists_page(
                        ui,
                        &mut self.state,
                        &theme,
                        &mut self.artwork_cache,
                        &mut commands,
                    ),
                    Route::LocalFiles => crate::ui::library::render_local_files_page(
                        ui,
                        &mut self.state,
                        &theme,
                        &mut self.artwork_cache,
                        &mut commands,
                    ),
                    Route::PlaylistDetail(id) => crate::ui::library::render_playlist_detail(
                        ui,
                        &id,
                        &self.state,
                        &theme,
                        &mut self.artwork_cache,
                        &mut commands,
                    ),
                    Route::AlbumDetail(id) => crate::ui::library::render_album_detail(
                        ui,
                        &id,
                        &self.state,
                        &theme,
                        &mut self.artwork_cache,
                        &mut commands,
                    ),
                    Route::ArtistDetail(id) => crate::ui::library::render_artist_detail(
                        ui,
                        &id,
                        &mut self.state,
                        &theme,
                        &mut self.artwork_cache,
                        &mut commands,
                    ),
                    Route::NowPlaying => crate::ui::lyrics::render_now_playing_fullscreen(
                        ui,
                        &mut self.state,
                        &theme,
                        &mut self.artwork_cache,
                        &mut self.karaoke_renderer,
                        &mut commands,
                    ),
                    Route::Settings => crate::ui::settings::render_settings(
                        ui,
                        &mut self.state,
                        &mut self.theme_mgr,
                        &self.credential_backend,
                        &self.cached_stats,
                        &self.plugin_manifests,
                        &mut commands,
                    ),
                }
            });

        self.dispatch_commands(commands);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Gracefully terminate any managed Spotify Soloist child daemon so no orphan remains
        self.soloist.stop_blocking();

        // Persist live playback position, track, and queue on exit so restart resumes cleanly
        let prefs = PlaybackPreferences {
            volume: self.state.playback.volume,
            shuffle: self.state.shuffle,
            repeat_mode: self.state.repeat,
            backend_kind: self.state.playback.preferred_backend,
            last_track: self.state.playback.current_track.clone(),
            last_position_ms: self.state.playback.position.current_ms(),
            last_queue: self.state.queue_items.iter().take(50).cloned().collect(),
        };
        let _ = self.db.save_playback_preferences(&prefs);
    }
}

/// Spawn the asynchronous backend tasks on the Tokio runtime so the GUI thread stays lock-free.
#[allow(clippy::too_many_arguments)]
fn spawn_backend_coordinator(
    runtime: &tokio::runtime::Runtime,
    egui_ctx: egui::Context,
    mut cmd_rx: mpsc::UnboundedReceiver<AppCommand>,
    evt_tx: mpsc::UnboundedSender<AppEvent>,
    session: SessionManager,
    spotify_client: SpotifyClient,
    playback: PlaybackController,
    mut lyrics_service: LyricsService,
    artwork_service: ArtworkService,
    db: Database,
    plugins: Arc<PluginRegistry>,
    initial_lyrics_source: crate::core::config::LyricsSourcePreference,
    initial_chain_config: crate::core::config::LyricsProviderChainConfig,
    initial_music_dirs: Vec<PathBuf>,
    default_lyrics_dir: PathBuf,
    cache_dir: PathBuf,
    artwork_cache_dir: PathBuf,
    initial_soloist_key: Option<String>,
    initial_soloist_device_name: String,
    initial_soloist_custom_binary: Option<PathBuf>,
    initial_soloist_auto_start: bool,
) {
    let library_svc = LibraryService::new(spotify_client.clone());
    let playlist_svc = PlaylistService::new(spotify_client.clone());
    let search_svc = SearchService::new(spotify_client.clone());
    let auth_service =
        crate::auth::service::SpotifyAuthService::new(session.clone(), library_svc.clone());
    let soloist = playback.soloist().clone();
    let shared_http = session.http_client();

    let notify_ui = {
        let tx = evt_tx.clone();
        let ctx = egui_ctx.clone();
        move |event: AppEvent| {
            let _ = tx.send(event);
            ctx.request_repaint();
        }
    };

    // 0. Start decoupled OS Media Controls bridge (Linux MPRIS2 D-Bus; honest unsupported status elsewhere)
    let (media_cmd_tx, mut media_cmd_rx) = mpsc::unbounded_channel::<SystemMediaCommand>();
    let media_bridge = {
        let _enter = runtime.enter();
        SystemMediaBridge::start(media_cmd_tx, artwork_cache_dir.clone(), None)
    };
    {
        let _enter = runtime.enter();
        std::mem::drop(playback.spawn_soloist_event_listener());
    }
    // 0b. Forward Spotify Soloist runtime status updates directly to the UI thread
    {
        let mut sol_rx = soloist.subscribe();
        let notify = notify_ui.clone();
        runtime.spawn(async move {
            while let Ok(evt) = sol_rx.recv().await {
                if let SoloistEvent::RuntimeChanged(snap) = evt {
                    notify(AppEvent::SoloistStatusUpdated(snap));
                }
            }
        });
    }
    {
        let pb = playback.clone();
        runtime.spawn(async move {
            while let Some(mcmd) = media_cmd_rx.recv().await {
                match mcmd {
                    SystemMediaCommand::Play => {
                        let snap = pb.snapshot().await;
                        if !snap.status.is_playing() {
                            let _ = pb.toggle_play_pause().await;
                        }
                    }
                    SystemMediaCommand::Pause => {
                        let _ = pb.pause().await;
                    }
                    SystemMediaCommand::PlayPause => {
                        let _ = pb.toggle_play_pause().await;
                    }
                    SystemMediaCommand::Stop => {
                        pb.stop_playback().await;
                    }
                    SystemMediaCommand::Next => {
                        let _ = pb.next().await;
                    }
                    SystemMediaCommand::Previous => {
                        let _ = pb.previous().await;
                    }
                    SystemMediaCommand::SeekOffsetUs(delta_us) => {
                        let pos = pb.get_position().await;
                        let cur_ms = pos.current_ms() as i64;
                        let delta_ms = delta_us / 1_000;
                        let target = (cur_ms + delta_ms).max(0) as u64;
                        let _ = pb.seek(target).await;
                    }
                    SystemMediaCommand::SetPositionUs(pos_us) => {
                        let _ = pb.seek(pos_us / 1_000).await;
                    }
                    SystemMediaCommand::SetVolume(vol) => {
                        let _ = pb.set_volume(vol as f32).await;
                    }
                }
            }
        });
    }

    // 1. Forward playback bus events to UI, OS media controls, & plugin hooks
    {
        let mut bus_rx = playback.event_bus().subscribe();
        let notify = notify_ui.clone();
        let plugins_ref = Arc::clone(&plugins);
        let media_ref = media_bridge.clone();
        runtime.spawn(async move {
            while let Ok(evt) = bus_rx.recv().await {
                match evt {
                    PlaybackEvent::Snapshot(snap) => {
                        media_ref.update_snapshot(&snap);
                        notify(AppEvent::PlaybackUpdated(snap));
                    }
                    PlaybackEvent::TrackChanged(Some(track)) => {
                        plugins_ref.notify_track_changed(&track);
                    }
                    PlaybackEvent::StatusChanged(status) => {
                        plugins_ref.notify_playback_state(status.is_playing());
                    }
                    PlaybackEvent::QueueChanged {
                        items,
                        current_index,
                        shuffle,
                        repeat,
                    } => {
                        notify(AppEvent::QueueUpdated {
                            items,
                            current_index,
                            shuffle,
                            repeat,
                        });
                    }
                    PlaybackEvent::Error(msg) => {
                        notify(AppEvent::StatusBanner {
                            message: msg,
                            is_error: true,
                        });
                    }
                    _ => {}
                }
            }
        });
    }

    // 2. Background playback tick (250ms local Rodio end-of-track check; 2.5s throttled Connect poll)
    {
        let pb = playback.clone();
        runtime.spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            loop {
                interval.tick().await;
                pb.tick_background().await;
            }
        });
    }

    // 3. Startup Soundcheck WAV preparation, Soloist auto-probe/start, session restoration & OS media status
    //    (Local music library is already loaded instantly from SQLite — no full disk rescan on boot!)
    {
        let auth_boot = auth_service.clone();
        let db_boot = db.clone();
        let pb_boot = playback.clone();
        let sol_boot = soloist.clone();
        let notify = notify_ui.clone();
        let wav_cache_dir = cache_dir.clone();
        let media_boot = media_bridge.clone();
        let boot_sol_key = initial_soloist_key.clone();
        let boot_sol_name = initial_soloist_device_name.clone();
        let boot_sol_bin = initial_soloist_custom_binary.clone();
        runtime.spawn(async move {
            // Prepare real royalty-free WAV Soundcheck files asynchronously (zero startup stall)
            let _ = tokio::task::spawn_blocking(move || {
                if let Err(err) = crate::audio::local_rodio::ensure_soundcheck_wavs(&wav_cache_dir)
                {
                    tracing::warn!(error = %err, "Failed preparing Soundcheck WAV files");
                }
            })
            .await;

            // Emit restored playback snapshot immediately
            let snap = pb_boot.snapshot().await;
            media_boot.update_snapshot(&snap);
            notify(AppEvent::PlaybackUpdated(snap));

            // Probe installed Spotify Soloist binary & auto-start when configured
            let sol_snap = sol_boot
                .refresh_installation_status(
                    boot_sol_bin.as_deref(),
                    &boot_sol_name,
                    boot_sol_key.is_some(),
                )
                .await;
            if initial_soloist_auto_start
                && let (Some(bin), Some(key)) = (sol_snap.binary_path, boot_sol_key)
                && let Err(err) = sol_boot.start_managed(bin, boot_sol_name, key).await
            {
                tracing::warn!(error = %err, "Failed auto-starting Spotify Soloist on boot");
            }

            // Report OS media controls status once D-Bus/platform initialization settles
            tokio::time::sleep(Duration::from_millis(120)).await;
            notify(AppEvent::SystemMediaStatusUpdated(
                media_boot.status().display_label(),
            ));

            // Restore persisted OAuth session from OS keychain if available
            if auth_boot.session().has_persisted_session() {
                notify(AppEvent::AuthRefreshing);
            }
            let cached_profile = db_boot.load_user_profile().ok().flatten();
            match auth_boot.restore_on_startup(cached_profile).await {
                Ok(Some(profile)) => {
                    let _ = db_boot.save_user_profile(&profile);
                    notify(AppEvent::AuthSucceeded { profile });
                }
                Ok(None) => {}
                Err(err) => {
                    if err.is_session_expired() {
                        let _ = db_boot.delete_user_profile();
                        notify(AppEvent::SessionExpired {
                            message: "Your saved Spotify session expired or was revoked. Please log in again."
                                .to_string(),
                        });
                    } else {
                        notify(AppEvent::AuthFailed {
                            message: format!("Saved session could not be refreshed: {err}"),
                            authorize_url: None,
                        });
                    }
                }
            }
        });
    }

    // 3b. Listen for runtime session expiration / revocation during active API usage
    {
        let mut expired_rx = session.subscribe_session_expired();
        let auth_exp = auth_service.clone();
        let pb_exp = playback.clone();
        let db_exp = db.clone();
        let sc_exp = spotify_client.clone();
        let sr_exp = search_svc.clone();
        let notify = notify_ui.clone();
        runtime.spawn(async move {
            while let Ok(message) = expired_rx.recv().await {
                auth_exp.mark_session_expired(message.clone()).await;
                pb_exp.on_logout().await;
                let _ = db_exp.delete_user_profile();
                sc_exp.clear_http_cache().await;
                sr_exp.clear_cache().await;
                notify(AppEvent::SessionExpired { message });
            }
        });
    }

    // 4. Main UI command dispatch loop
    runtime.spawn(async move {
        let mut lyrics_source = initial_lyrics_source;
        let mut chain_config = initial_chain_config;
        let mut music_dirs = initial_music_dirs;
        let mut soloist_api_key = initial_soloist_key;
        let mut soloist_device_name = initial_soloist_device_name;
        let mut soloist_custom_binary = initial_soloist_custom_binary;
        let mut active_auth_task: Option<tokio::task::JoinHandle<()>> = None;
        let mut active_lyrics_task: Option<tokio::task::JoinHandle<()>> = None;

        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                AppCommand::StartSpotifyLogin => {
                    // Cancel any prior pending OAuth callback listener before starting a new one
                    // to avoid port conflicts (`EADDRINUSE`) or leaked tasks on Retry Login.
                    if let Some(prev) = active_auth_task.take() {
                        prev.abort();
                    }

                    let auth = auth_service.clone();
                    let db_c = db.clone();
                    let notify = notify_ui.clone();
                    active_auth_task = Some(tokio::spawn(async move {
                        use crate::auth::session::AuthProgressStage;
                        let mut last_url: Option<String> = None;
                        let login_res = auth
                            .spotify_login(true, Duration::from_secs(180), |stage| match stage {
                                AuthProgressStage::OpeningBrowser { authorize_url } => {
                                    last_url = Some(authorize_url.clone());
                                    notify(AppEvent::AuthOpeningBrowser { authorize_url });
                                }
                                AuthProgressStage::WaitingForAuthorization { authorize_url } => {
                                    last_url = Some(authorize_url.clone());
                                    notify(AppEvent::AuthWaitingForCallback { authorize_url });
                                }
                                AuthProgressStage::ExchangingToken => {
                                    notify(AppEvent::AuthExchangingToken);
                                }
                            })
                            .await;
                        match login_res {
                            Ok(profile) => {
                                let _ = db_c.save_user_profile(&profile);
                                notify(AppEvent::AuthSucceeded { profile });
                            }
                            Err(err) => notify(AppEvent::AuthFailed {
                                message: err.to_string(),
                                authorize_url: last_url,
                            }),
                        }
                    }));
                }
                AppCommand::RefreshSpotifySession => {
                    let auth = auth_service.clone();
                    let db_c = db.clone();
                    let notify = notify_ui.clone();
                    notify(AppEvent::AuthRefreshing);
                    tokio::spawn(async move {
                        match auth.spotify_refresh_session().await {
                            Ok(Some(profile)) => {
                                let _ = db_c.save_user_profile(&profile);
                                notify(AppEvent::AuthSucceeded { profile });
                            }
                            Ok(None) => {
                                notify(AppEvent::AuthLoggedOut);
                            }
                            Err(err) => {
                                if err.is_session_expired() {
                                    let _ = db_c.delete_user_profile();
                                    notify(AppEvent::SessionExpired {
                                        message: "Your Spotify session expired or was revoked. Please log in again."
                                            .to_string(),
                                    });
                                } else {
                                    notify(AppEvent::AuthFailed {
                                        message: format!("Session refresh failed: {err}"),
                                        authorize_url: None,
                                    });
                                }
                            }
                        }
                    });
                }
                AppCommand::Logout => {
                    if let Some(prev) = active_auth_task.take() {
                        prev.abort();
                    }
                    if let Some(prev) = active_lyrics_task.take() {
                        prev.abort();
                    }
                    auth_service.spotify_logout().await;
                    playback.on_logout().await;
                    let _ = db.delete_user_profile();
                    spotify_client.clear_http_cache().await;
                    search_svc.clear_cache().await;
                    notify_ui(AppEvent::AuthLoggedOut);
                }
                AppCommand::RefreshLibrary => {
                    let lib = library_svc.clone();
                    let pl = playlist_svc.clone();
                    let db_c = db.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        let (liked, recent, top, playlists, albums, artists) = tokio::join!(
                            lib.saved_tracks(50),
                            lib.recently_played(30),
                            lib.top_tracks(20),
                            pl.current_user_playlists(50),
                            lib.saved_albums(50),
                            lib.followed_artists(30),
                        );

                        let liked_tracks = liked.unwrap_or_default();
                        let recently_played = recent.unwrap_or_default();
                        let top_tracks = top.unwrap_or_default();
                        let playlists_val = playlists.unwrap_or_default();
                        let albums_val = albums.unwrap_or_default();
                        let artists_val = artists.unwrap_or_default();

                        let _ = db_c.upsert_tracks(&liked_tracks);
                        let _ = db_c.upsert_playlists(&playlists_val);
                        let _ = db_c.upsert_albums(&albums_val);

                        notify(AppEvent::LibraryLoaded {
                            liked_tracks,
                            recently_played,
                            top_tracks,
                            playlists: playlists_val,
                            albums: albums_val,
                            artists: artists_val,
                        });
                    });
                }
                AppCommand::SearchCatalog { query } => {
                    let svc = search_svc.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        match svc.search(&query, 10).await {
                            Ok(results) => notify(AppEvent::SearchCompleted(results)),
                            Err(err) => notify(AppEvent::SearchFailed {
                                query,
                                error: err.to_string(),
                            }),
                        }
                    });
                }
                AppCommand::LoadPlaylistTracks { playlist_id } => {
                    let svc = playlist_svc.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        match svc.playlist_tracks(&playlist_id).await {
                            Ok(tracks) => notify(AppEvent::PlaylistTracksLoaded {
                                playlist_id,
                                tracks,
                            }),
                            Err(err) => notify(AppEvent::PlaylistTracksFailed {
                                playlist_id,
                                error: err.to_string(),
                            }),
                        }
                    });
                }
                AppCommand::LoadAlbumTracks { album_id } => {
                    let svc = library_svc.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        match svc.album_tracks(&album_id).await {
                            Ok(tracks) => notify(AppEvent::AlbumTracksLoaded { album_id, tracks }),
                            Err(err) => notify(AppEvent::AlbumTracksFailed {
                                album_id,
                                error: err.to_string(),
                            }),
                        }
                    });
                }
                AppCommand::LoadArtistAlbums { artist_id } => {
                    let svc = library_svc.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        match svc.artist_albums(&artist_id).await {
                            Ok(albums) => {
                                notify(AppEvent::ArtistAlbumsLoaded { artist_id, albums })
                            }
                            Err(err) => notify(AppEvent::ArtistAlbumsFailed {
                                artist_id,
                                error: err.to_string(),
                            }),
                        }
                    });
                }
                AppCommand::ToggleLikeTrack { track } => {
                    let new_liked = !track.liked;
                    let lib = library_svc.clone();
                    let db_c = db.clone();
                    let notify = notify_ui.clone();
                    let is_auth = session.is_authenticated().await;
                    tokio::spawn(async move {
                        if is_auth
                            && track.uri.starts_with("spotify:")
                            && let Err(err) = lib.set_library_saved(&track.uri, new_liked).await
                        {
                            notify(AppEvent::StatusBanner {
                                message: format!("Failed updating Spotify library: {err}"),
                                is_error: true,
                            });
                            return;
                        }
                        let mut updated = track.clone();
                        updated.liked = new_liked;
                        let _ = db_c.upsert_tracks(std::slice::from_ref(&updated));
                        let _ = db_c.set_track_liked(&updated.id, new_liked);
                        notify(AppEvent::TrackLikeUpdated {
                            track: updated,
                            liked: new_liked,
                        });
                    });
                }
                AppCommand::RefreshDevices => {
                    let lib = library_svc.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        match lib.available_devices().await {
                            Ok(devices) => notify(AppEvent::DevicesLoaded(devices)),
                            Err(err) => notify(AppEvent::StatusBanner {
                                message: format!("Failed refreshing Spotify Connect devices: {err}"),
                                is_error: true,
                            }),
                        }
                    });
                }
                AppCommand::SyncConnectState => {
                    let pb = playback.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        if let Err(err) = pb.sync_connect_now().await
                            && !matches!(err, crate::audio::engine::AudioError::Unauthenticated)
                        {
                            notify(AppEvent::StatusBanner {
                                message: format!("Spotify Connect sync failed: {err}"),
                                is_error: true,
                            });
                        }
                    });
                }
                AppCommand::TransferDevice { device_id } => {
                    let pb = playback.clone();
                    let lib = library_svc.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        match pb.transfer_connect_device(&device_id).await {
                            Ok(()) => {
                                if let Ok(devices) = lib.available_devices().await {
                                    notify(AppEvent::DevicesLoaded(devices));
                                }
                            }
                            Err(err) => {
                                notify(AppEvent::StatusBanner {
                                    message: format!("Device transfer failed: {err}"),
                                    is_error: true,
                                });
                            }
                        }
                    });
                }
                AppCommand::PlayTrack {
                    track,
                    queue_context,
                } => {
                    let pb = playback.clone();
                    let wav_dir = cache_dir.clone();
                    tokio::spawn(async move {
                        // Ensure local Soundcheck WAV exists if clicked within milliseconds of launch
                        if track.uri.starts_with("spotirust:sample:")
                            && let Some(path) = &track.local_path
                            && !path.exists()
                        {
                            let _ = tokio::task::spawn_blocking(move || {
                                crate::audio::local_rodio::ensure_soundcheck_wavs(&wav_dir)
                            })
                            .await;
                        }
                        let _ = pb.play_track_in_context(track, queue_context).await;
                    });
                }
                AppCommand::TogglePlayPause => {
                    let pb = playback.clone();
                    tokio::spawn(async move {
                        let _ = pb.toggle_play_pause().await;
                    });
                }
                AppCommand::NextTrack => {
                    let pb = playback.clone();
                    tokio::spawn(async move {
                        let _ = pb.next().await;
                    });
                }
                AppCommand::PreviousTrack => {
                    let pb = playback.clone();
                    tokio::spawn(async move {
                        let _ = pb.previous().await;
                    });
                }
                AppCommand::SeekTo { position_ms } => {
                    let pb = playback.clone();
                    tokio::spawn(async move {
                        let _ = pb.seek(position_ms).await;
                    });
                }
                AppCommand::SetVolume { volume } => {
                    let pb = playback.clone();
                    tokio::spawn(async move {
                        let _ = pb.set_volume(volume).await;
                    });
                }
                AppCommand::ToggleShuffle => {
                    playback.toggle_shuffle().await;
                }
                AppCommand::CycleRepeat => {
                    playback.cycle_repeat().await;
                }
                AppCommand::AddToQueue { track } => {
                    let pb = playback.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        match pb.add_to_queue(track).await {
                            Ok(()) => notify(AppEvent::StatusBanner {
                                message: "Added track to queue.".to_string(),
                                is_error: false,
                            }),
                            Err(err) => notify(AppEvent::StatusBanner {
                                message: format!("Could not add track to queue: {err}"),
                                is_error: true,
                            }),
                        }
                    });
                }
                AppCommand::PlayQueueIndex { index } => {
                    let pb = playback.clone();
                    tokio::spawn(async move {
                        let _ = pb.play_queue_index(index).await;
                    });
                }
                AppCommand::RemoveQueueIndex { index } => {
                    playback.remove_queue_index(index).await;
                }
                AppCommand::ClearQueue => {
                    playback.clear_queue().await;
                }
                AppCommand::SwitchAudioBackend { backend } => {
                    let pb = playback.clone();
                    let lib = library_svc.clone();
                    let sol = soloist.clone();
                    let notify = notify_ui.clone();
                    let key_opt = soloist_api_key.clone();
                    let dev_name = soloist_device_name.clone();
                    let custom_bin = soloist_custom_binary.clone();
                    let is_auth = session.is_authenticated().await;
                    tokio::spawn(async move {
                        pb.set_preferred_backend(backend).await;
                        if backend == AudioBackendKind::SpotifyConnect && is_auth {
                            if let Ok(devices) = lib.available_devices().await {
                                notify(AppEvent::DevicesLoaded(devices));
                            }
                        } else if backend == AudioBackendKind::SpotifySoloist {
                            let snap = sol.runtime_snapshot().await;
                            if !snap.status.is_running()
                                && let Some(key) = key_opt
                                && let Some(bin) = crate::audio::soloist::find_soloist_binary(
                                    custom_bin.as_deref(),
                                    sol.bin_dir(),
                                )
                                && let Err(err) = sol.start_managed(bin, dev_name, key).await
                            {
                                notify(AppEvent::StatusBanner {
                                    message: format!("Could not start Spotify Soloist: {err}"),
                                    is_error: true,
                                });
                            }
                        }
                    });
                }
                AppCommand::PickLocalMusicFolder => {
                    let db_c = db.clone();
                    let art_dir = artwork_cache_dir.clone();
                    let notify = notify_ui.clone();
                    let mut dirs_snapshot = music_dirs.clone();
                    tokio::spawn(async move {
                        let picked = rfd::AsyncFileDialog::new()
                            .set_title("Select Local Music Folder")
                            .pick_folder()
                            .await;
                        let Some(handle) = picked else {
                            return;
                        };
                        let folder = handle.path().to_path_buf();
                        if !dirs_snapshot.contains(&folder) {
                            dirs_snapshot.push(folder.clone());
                        }
                        notify(AppEvent::LocalScanStarted);
                        let report = tokio::task::spawn_blocking(move || {
                            scan_local_library_into_db(&db_c, &dirs_snapshot, &art_dir, false)
                        })
                        .await
                        .unwrap_or_default();
                        let summary = format_scan_summary(&report);
                        notify(AppEvent::LocalTracksScanned {
                            tracks: report.tracks,
                            added_folder: Some(folder),
                        });
                        notify(AppEvent::StatusBanner {
                            message: summary,
                            is_error: false,
                        });
                    });
                }
                AppCommand::PickLocalMusicFiles => {
                    let db_c = db.clone();
                    let art_dir = artwork_cache_dir.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        let picked = rfd::AsyncFileDialog::new()
                            .set_title("Add Local Audio Files")
                            .add_filter("Audio Files", &["mp3", "flac", "ogg", "wav"])
                            .pick_files()
                            .await;
                        let Some(handles) = picked else {
                            return;
                        };
                        let paths: Vec<PathBuf> = handles
                            .into_iter()
                            .map(|h| h.path().to_path_buf())
                            .collect();
                        if paths.is_empty() {
                            return;
                        }
                        notify(AppEvent::LocalScanStarted);
                        let report = tokio::task::spawn_blocking(move || {
                            import_local_files_into_db(&db_c, &paths, &art_dir)
                        })
                        .await
                        .unwrap_or_default();
                        let summary = format_scan_summary(&report);
                        notify(AppEvent::LocalTracksScanned {
                            tracks: report.tracks,
                            added_folder: None,
                        });
                        notify(AppEvent::StatusBanner {
                            message: summary,
                            is_error: false,
                        });
                    });
                }
                AppCommand::ScanLocalMusicDir { path } => {
                    if !music_dirs.contains(&path) {
                        music_dirs.push(path.clone());
                    }
                    let db_c = db.clone();
                    let art_dir = artwork_cache_dir.clone();
                    let dirs_snapshot = music_dirs.clone();
                    let notify = notify_ui.clone();
                    notify(AppEvent::LocalScanStarted);
                    tokio::spawn(async move {
                        let report = tokio::task::spawn_blocking(move || {
                            scan_local_library_into_db(&db_c, &dirs_snapshot, &art_dir, false)
                        })
                        .await
                        .unwrap_or_default();
                        let summary = format_scan_summary(&report);
                        notify(AppEvent::LocalTracksScanned {
                            tracks: report.tracks,
                            added_folder: Some(path),
                        });
                        notify(AppEvent::StatusBanner {
                            message: summary,
                            is_error: false,
                        });
                    });
                }
                AppCommand::RescanLocalLibrary => {
                    let db_c = db.clone();
                    let art_dir = artwork_cache_dir.clone();
                    let dirs_snapshot = music_dirs.clone();
                    let notify = notify_ui.clone();
                    notify(AppEvent::LocalScanStarted);
                    tokio::spawn(async move {
                        let report = tokio::task::spawn_blocking(move || {
                            scan_local_library_into_db(&db_c, &dirs_snapshot, &art_dir, true)
                        })
                        .await
                        .unwrap_or_default();
                        let summary = format_scan_summary(&report);
                        notify(AppEvent::LocalTracksScanned {
                            tracks: report.tracks,
                            added_folder: None,
                        });
                        notify(AppEvent::StatusBanner {
                            message: summary,
                            is_error: false,
                        });
                    });
                }
                AppCommand::RemoveLocalMusicDir { path } => {
                    music_dirs.retain(|d| d != &path);
                    let db_c = db.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        let (removed, remaining) = tokio::task::spawn_blocking(move || {
                            let removed = db_c.remove_local_tracks_in_dir(&path).unwrap_or(0);
                            let tracks = db_c.load_local_tracks().unwrap_or_default();
                            (removed, tracks)
                        })
                        .await
                        .unwrap_or((0, Vec::new()));
                        notify(AppEvent::LocalTracksScanned {
                            tracks: remaining,
                            added_folder: None,
                        });
                        notify(AppEvent::StatusBanner {
                            message: format!(
                                "Removed folder ({removed} indexed track(s) removed)."
                            ),
                            is_error: false,
                        });
                    });
                }
                AppCommand::ShufflePlayTracks { tracks } => {
                    let pb = playback.clone();
                    tokio::spawn(async move {
                        let _ = pb.shuffle_play_tracks(tracks).await;
                    });
                }
                AppCommand::FetchLyrics {
                    track,
                    force_refresh,
                } => {
                    // Cancel any in-flight lyrics request from a previous track immediately
                    if let Some(prev) = active_lyrics_task.take() {
                        prev.abort();
                    }
                    let svc = lyrics_service.clone();
                    let notify = notify_ui.clone();
                    let pref = lyrics_source;
                    let any_online = chain_config.enable_binilyrics
                        || chain_config.enable_unison
                        || chain_config.enable_amll_db
                        || chain_config.enable_musixmatch
                        || chain_config.enable_lrclib;
                    notify(AppEvent::LyricsLoading {
                        track_id: track.id.clone(),
                    });
                    active_lyrics_task = Some(tokio::spawn(async move {
                        use crate::lyrics::provider::LyricsResolution;
                        match svc
                            .resolve_with_preference(&track, pref, any_online, force_refresh)
                            .await
                        {
                            LyricsResolution::Found(document) => notify(AppEvent::LyricsLoaded {
                                track_id: track.id,
                                document,
                            }),
                            LyricsResolution::Ambiguous {
                                candidates,
                                provider_name,
                            } => notify(AppEvent::LyricsAmbiguous {
                                track_id: track.id,
                                provider_name,
                                candidates,
                            }),
                            LyricsResolution::Unavailable(reason) => {
                                notify(AppEvent::LyricsUnavailable {
                                    track_id: track.id,
                                    reason,
                                })
                            }
                        }
                    }));
                }
                AppCommand::PickImportLyricsFile { track } => {
                    let svc = lyrics_service.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        let picked = rfd::AsyncFileDialog::new()
                            .set_title("Import Local Lyrics (.ttml / .lyricsfile / .lrc)")
                            .add_filter(
                                "Lyrics Files",
                                &["ttml", "lrc", "lyricsfile", "yaml", "yml", "xml"],
                            )
                            .pick_file()
                            .await;
                        let Some(handle) = picked else {
                            return;
                        };
                        let path = handle.path().to_path_buf();
                        let track_clone = track.clone();
                        let res = tokio::task::spawn_blocking(move || {
                            svc.import_for_track(&track_clone, &path)
                        })
                        .await;
                        match res {
                            Ok(Ok(document)) => {
                                notify(AppEvent::LyricsLoaded {
                                    track_id: track.id,
                                    document,
                                });
                                notify(AppEvent::StatusBanner {
                                    message: "Imported local lyrics file and cached in SQLite."
                                        .to_string(),
                                    is_error: false,
                                });
                            }
                            Ok(Err(err)) => notify(AppEvent::StatusBanner {
                                message: format!("Failed to parse lyrics file: {err}"),
                                is_error: true,
                            }),
                            Err(err) => notify(AppEvent::StatusBanner {
                                message: format!("Failed reading lyrics file: {err}"),
                                is_error: true,
                            }),
                        }
                    });
                }
                AppCommand::ImportLyricsFile { track, path } => {
                    if let Some(prev) = active_lyrics_task.take() {
                        prev.abort();
                    }
                    match lyrics_service.import_for_track(&track, &path) {
                        Ok(document) => {
                            notify_ui(AppEvent::LyricsLoaded {
                                track_id: track.id,
                                document,
                            });
                            notify_ui(AppEvent::StatusBanner {
                                message: "Imported local lyrics file and cached in SQLite."
                                    .to_string(),
                                is_error: false,
                            });
                        }
                        Err(err) => notify_ui(AppEvent::StatusBanner {
                            message: format!("Failed to parse lyrics file: {err}"),
                            is_error: true,
                        }),
                    }
                }
                AppCommand::SelectLyricsCandidate { track, candidate } => {
                    if let Some(prev) = active_lyrics_task.take() {
                        prev.abort();
                    }
                    let svc = lyrics_service.clone();
                    let notify = notify_ui.clone();
                    notify(AppEvent::LyricsLoading {
                        track_id: track.id.clone(),
                    });
                    active_lyrics_task = Some(tokio::spawn(async move {
                        match svc.resolve_candidate(&track, candidate.as_ref()).await {
                            Ok(document) => notify(AppEvent::LyricsLoaded {
                                track_id: track.id,
                                document,
                            }),
                            Err(reason) => notify(AppEvent::LyricsUnavailable {
                                track_id: track.id,
                                reason,
                            }),
                        }
                    }));
                }
                AppCommand::SetLyricsOffset { offset_ms } => {
                    if let Some(track) = playback.snapshot().await.current_track {
                        lyrics_service.save_offset(&track, offset_ms);
                    }
                }
                AppCommand::ClearLyricsCache => {
                    let removed = lyrics_service.clear_cache();
                    notify_ui(AppEvent::LyricsCacheCleared {
                        freed_entries: removed,
                    });
                }
                AppCommand::RequestArtwork { url } => {
                    let svc = artwork_service.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        match svc.fetch_and_decode(&url).await {
                            Ok(decoded) => notify(AppEvent::ArtworkLoaded {
                                url: decoded.url,
                                width: decoded.width,
                                height: decoded.height,
                                rgba: decoded.rgba,
                                dominant_rgb: decoded.dominant_rgb,
                            }),
                            Err(_) => notify(AppEvent::ArtworkFailed { url }),
                        }
                    });
                }
                AppCommand::SaveSettings(settings) => {
                    lyrics_source = settings.lyrics_source;
                    chain_config = settings.lyrics_chain_config();
                    music_dirs = settings.local_music_dirs.clone();
                    soloist_device_name = if settings.soloist_device_name.trim().is_empty() {
                        "Spotirust".to_string()
                    } else {
                        settings.soloist_device_name.trim().to_string()
                    };
                    soloist_custom_binary = settings.soloist_custom_binary_path.clone();
                    // Preserve default Spotirust lyrics directory alongside user-configured dirs
                    let mut dirs = vec![default_lyrics_dir.clone()];
                    for dir in &settings.local_lyrics_dirs {
                        if !dirs.contains(dir) {
                            dirs.push(dir.clone());
                        }
                    }
                    lyrics_service.update_local_dirs(dirs);
                    lyrics_service.update_chain_config(chain_config.clone());
                    let _ = db.save_settings(&settings);
                    let sol = soloist.clone();
                    let dev = soloist_device_name.clone();
                    let bin = soloist_custom_binary.clone();
                    let has_key = soloist_api_key.is_some();
                    tokio::spawn(async move {
                        let _ = sol
                            .refresh_installation_status(bin.as_deref(), &dev, has_key)
                            .await;
                    });
                }
                AppCommand::SaveMusixmatchApiKey { api_key } => {
                    match api_key {
                        Some(key) if !key.trim().is_empty() => {
                            let vault_res =
                                crate::storage::credentials::save_musixmatch_api_key(&key);
                            lyrics_service.set_musixmatch_api_key(Some(key));
                            notify_ui(AppEvent::MusixmatchKeyUpdated {
                                configured: lyrics_service.has_musixmatch_api_key(),
                            });
                            let msg = match vault_res {
                                Ok(()) => "Musixmatch official API key saved to OS credential vault."
                                    .to_string(),
                                Err(err) => format!(
                                    "Musixmatch API key active for this session (OS vault unavailable: {err})."
                                ),
                            };
                            notify_ui(AppEvent::StatusBanner {
                                message: msg,
                                is_error: false,
                            });
                        }
                        _ => {
                            let _ = crate::storage::credentials::delete_musixmatch_api_key();
                            let fallback_env =
                                crate::lyrics::musixmatch::resolve_env_musixmatch_key();
                            lyrics_service.set_musixmatch_api_key(fallback_env);
                            notify_ui(AppEvent::MusixmatchKeyUpdated {
                                configured: lyrics_service.has_musixmatch_api_key(),
                            });
                            notify_ui(AppEvent::StatusBanner {
                                message: "Musixmatch API key removed from OS credential vault."
                                    .to_string(),
                                is_error: false,
                            });
                        }
                    }
                }
                AppCommand::SaveSoloistApiKey { api_key } => {
                    match api_key {
                        Some(key) if !key.trim().is_empty() => {
                            let trimmed = key.trim().to_string();
                            let vault_res =
                                crate::storage::credentials::save_soloist_api_key(&trimmed);
                            soloist_api_key = Some(trimmed.clone());
                            let msg = match vault_res {
                                Ok(()) => {
                                    "Spotify Soloist API key saved to OS credential vault."
                                        .to_string()
                                }
                                Err(err) => format!(
                                    "Spotify Soloist API key active for this session (OS vault unavailable: {err})."
                                ),
                            };
                            notify_ui(AppEvent::StatusBanner {
                                message: msg,
                                is_error: false,
                            });

                            let sol = soloist.clone();
                            let dev = soloist_device_name.clone();
                            let custom_bin = soloist_custom_binary.clone();
                            let notify = notify_ui.clone();
                            tokio::spawn(async move {
                                let existing_bin = sol.runtime_snapshot().await.binary_path;
                                let snap = sol
                                    .refresh_installation_status(
                                        custom_bin.as_deref().or(existing_bin.as_deref()),
                                        &dev,
                                        true,
                                    )
                                    .await;
                                if let Some(bin) = snap.binary_path
                                    && !snap.status.is_running()
                                    && let Err(err) = sol.start_managed(bin, dev, trimmed).await
                                {
                                    notify(AppEvent::StatusBanner {
                                        message: format!("Could not start Soloist: {err}"),
                                        is_error: true,
                                    });
                                }
                            });
                        }
                        _ => {
                            let _ = crate::storage::credentials::delete_soloist_api_key();
                            soloist_api_key = crate::audio::soloist::resolve_env_soloist_key();
                            soloist.stop().await;
                            let _ = soloist
                                .refresh_installation_status(
                                    soloist_custom_binary.as_deref(),
                                    &soloist_device_name,
                                    soloist_api_key.is_some(),
                                )
                                .await;
                            notify_ui(AppEvent::StatusBanner {
                                message: "Spotify Soloist API key removed from OS credential vault."
                                    .to_string(),
                                is_error: false,
                            });
                        }
                    }
                }
                AppCommand::DownloadSoloist => {
                    let sol = soloist.clone();
                    let http = shared_http.clone();
                    let notify = notify_ui.clone();
                    let dev = soloist_device_name.clone();
                    let key_opt = soloist_api_key.clone();
                    let pb = playback.clone();
                    let db_c = db.clone();
                    // Clear custom override when user explicitly installs the managed official build
                    soloist_custom_binary = None;
                    tokio::spawn(async move {
                        sol.stop().await;
                        sol.set_downloading(true).await;
                        match crate::audio::soloist::download_and_install_soloist(
                            &http,
                            sol.bin_dir(),
                        )
                        .await
                        {
                            Ok(info) => {
                                if let Ok(mut settings) = db_c.load_settings() {
                                    settings.soloist_custom_binary_path = None;
                                    let _ = db_c.save_settings(&settings);
                                }
                                notify(AppEvent::SoloistCustomBinaryUpdated(None));
                                sol.set_downloading(false).await;
                                let _ = sol
                                    .refresh_installation_status(None, &dev, key_opt.is_some())
                                    .await;
                                let ver_text = info
                                    .version_output
                                    .as_deref()
                                    .unwrap_or("installed");
                                notify(AppEvent::StatusBanner {
                                    message: format!("Spotify Soloist installed ({ver_text})."),
                                    is_error: false,
                                });
                                if let Some(key) = key_opt {
                                    pb.set_preferred_backend(AudioBackendKind::SpotifySoloist)
                                        .await;
                                    if let Err(err) =
                                        sol.start_managed(info.path, dev, key).await
                                    {
                                        sol.set_error_status(err.to_string()).await;
                                        notify(AppEvent::StatusBanner {
                                            message: format!("Soloist failed to start: {err}"),
                                            is_error: true,
                                        });
                                    }
                                }
                            }
                            Err(err) => {
                                sol.set_error_status(err.clone()).await;
                                notify(AppEvent::StatusBanner {
                                    message: format!("Spotify Soloist download failed: {err}"),
                                    is_error: true,
                                });
                            }
                        }
                    });
                }
                AppCommand::PickSoloistBinary => {
                    let sol = soloist.clone();
                    let db_c = db.clone();
                    let notify = notify_ui.clone();
                    let dev = soloist_device_name.clone();
                    let has_key = soloist_api_key.is_some();
                    tokio::spawn(async move {
                        let picked = rfd::AsyncFileDialog::new()
                            .set_title("Select Spotify Soloist Executable (soloist / soloist.exe)")
                            .pick_file()
                            .await;
                        let Some(handle) = picked else {
                            return;
                        };
                        let path = handle.path().to_path_buf();
                        match crate::audio::soloist::probe_soloist_version(&path).await {
                            Ok(ver) => {
                                if let Ok(mut settings) = db_c.load_settings() {
                                    settings.soloist_custom_binary_path = Some(path.clone());
                                    let _ = db_c.save_settings(&settings);
                                }
                                notify(AppEvent::SoloistCustomBinaryUpdated(Some(path.clone())));
                                let _ = sol
                                    .refresh_installation_status(Some(&path), &dev, has_key)
                                    .await;
                                notify(AppEvent::StatusBanner {
                                    message: format!("Selected Spotify Soloist binary ({ver})."),
                                    is_error: false,
                                });
                            }
                            Err(err) => {
                                notify(AppEvent::StatusBanner {
                                    message: format!("Selected file is not a valid Soloist binary: {err}"),
                                    is_error: true,
                                });
                            }
                        }
                    });
                }
                AppCommand::StartSoloist | AppCommand::RestartSoloist => {
                    let sol = soloist.clone();
                    let pb = playback.clone();
                    let notify = notify_ui.clone();
                    let key_opt = soloist_api_key.clone();
                    let dev = soloist_device_name.clone();
                    let custom_bin = soloist_custom_binary.clone();
                    tokio::spawn(async move {
                        let Some(key) = key_opt else {
                            notify(AppEvent::StatusBanner {
                                message: "Enter & save your Spotify Soloist API key first (Settings → Spotify Soloist)."
                                    .to_string(),
                                is_error: true,
                            });
                            return;
                        };
                        let snap_bin = sol.runtime_snapshot().await.binary_path;
                        let Some(bin) = snap_bin.or_else(|| {
                            crate::audio::soloist::find_soloist_binary(
                                custom_bin.as_deref(),
                                sol.bin_dir(),
                            )
                        }) else {
                            notify(AppEvent::StatusBanner {
                                message: "Spotify Soloist binary is not installed yet. Click 'Download / Update Soloist' first."
                                    .to_string(),
                                is_error: true,
                            });
                            return;
                        };
                        pb.set_preferred_backend(AudioBackendKind::SpotifySoloist)
                            .await;
                        if let Err(err) = sol.start_managed(bin, dev, key).await {
                            sol.set_error_status(err.to_string()).await;
                            notify(AppEvent::StatusBanner {
                                message: format!("Failed starting Spotify Soloist: {err}"),
                                is_error: true,
                            });
                        }
                    });
                }
                AppCommand::StopSoloist => {
                    let sol = soloist.clone();
                    let notify = notify_ui.clone();
                    tokio::spawn(async move {
                        sol.stop().await;
                        notify(AppEvent::StatusBanner {
                            message: "Spotify Soloist daemon stopped.".to_string(),
                            is_error: false,
                        });
                    });
                }
                AppCommand::ResetSoloistPairing => {
                    let sol = soloist.clone();
                    let notify = notify_ui.clone();
                    let key_opt = soloist_api_key.clone();
                    let dev = soloist_device_name.clone();
                    let custom_bin = soloist_custom_binary.clone();
                    tokio::spawn(async move {
                        let snap_bin = sol.runtime_snapshot().await.binary_path;
                        match sol.reset_pairing_session().await {
                            Ok(()) => {
                                let bin_opt = snap_bin.or_else(|| {
                                    crate::audio::soloist::find_soloist_binary(
                                        custom_bin.as_deref(),
                                        sol.bin_dir(),
                                    )
                                });
                                if let (Some(key), Some(bin)) = (key_opt, bin_opt) {
                                    let _ = sol.start_managed(bin, dev.clone(), key).await;
                                    notify(AppEvent::StatusBanner {
                                        message: format!(
                                            "Soloist pairing cleared. Select '{dev}' in the Spotify app to pair again."
                                        ),
                                        is_error: false,
                                    });
                                } else {
                                    notify(AppEvent::StatusBanner {
                                        message: "Soloist stored pairing session cleared."
                                            .to_string(),
                                        is_error: false,
                                    });
                                }
                            }
                            Err(err) => notify(AppEvent::StatusBanner {
                                message: format!("Failed clearing Soloist pairing: {err}"),
                                is_error: true,
                            }),
                        }
                    });
                }
                AppCommand::ClearCache => {
                    let db_removed = db.clear_cache().unwrap_or(0);
                    let disk_removed = artwork_service.clear_remote_disk_cache().unwrap_or(0);
                    spotify_client.clear_http_cache().await;
                    search_svc.clear_cache().await;
                    notify_ui(AppEvent::CacheCleared {
                        freed_entries: db_removed + disk_removed,
                    });
                }
                AppCommand::ResetApplication => {
                    if let Some(prev) = active_auth_task.take() {
                        prev.abort();
                    }
                    if let Some(prev) = active_lyrics_task.take() {
                        prev.abort();
                    }
                    music_dirs.clear();
                    let _ = crate::storage::credentials::delete_musixmatch_api_key();
                    let _ = crate::storage::credentials::delete_soloist_api_key();
                    soloist_api_key = crate::audio::soloist::resolve_env_soloist_key();
                    soloist_device_name = "Spotirust".to_string();
                    soloist_custom_binary = None;
                    let _ = soloist.reset_pairing_session().await;
                    let _ = soloist
                        .refresh_installation_status(
                            None,
                            &soloist_device_name,
                            soloist_api_key.is_some(),
                        )
                        .await;
                    lyrics_service
                        .set_musixmatch_api_key(crate::lyrics::musixmatch::resolve_env_musixmatch_key());
                    chain_config = Default::default();
                    lyrics_service.update_chain_config(chain_config.clone());
                    auth_service.spotify_logout().await;
                    playback.on_logout().await;
                    let _ = db.reset_application();
                    let _ = artwork_service.clear_disk_cache();
                    spotify_client.clear_http_cache().await;
                    search_svc.clear_cache().await;
                    playback.clear_queue().await;
                    notify_ui(AppEvent::ApplicationReset);
                }
                AppCommand::OpenPendingLoginUrl | AppCommand::OpenExternalUrl { .. } => {}
            }
        }
    });
}

fn format_scan_summary(report: &LocalScanReport) -> String {
    let mut parts = vec![format!(
        "Local library ready: {} track(s) ({} parsed, {} cached)",
        report.tracks.len(),
        report.parsed_count,
        report.reused_count,
    )];
    if report.skipped_corrupt_count > 0 {
        parts.push(format!(
            "{} unreadable/corrupt skipped",
            report.skipped_corrupt_count
        ));
    }
    if report.pruned_count > 0 {
        parts.push(format!("{} missing removed", report.pruned_count));
    }
    parts.join(" · ")
}
