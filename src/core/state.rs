//! Centralized client UI state with back/forward navigation history, search debounce,
//! playback snapshot, lyrics state, and notification banners.

use crate::audio::engine::PlaybackSnapshot;
use crate::core::config::UserSettings;
use crate::core::models::{
    Album, Artist, ConnectDevice, Playlist, RepeatMode, Route, SearchResults, Track, UserProfile,
    bundled_showcase_tracks,
};
use crate::lyrics::model::LyricsDocument;
use crate::lyrics::provider::LyricsCandidate;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Authentication state shown in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AuthUiState {
    #[default]
    SignedOut,
    /// Opening the user's default system browser with the Spotify OAuth PKCE URL.
    OpeningBrowser { authorize_url: String },
    /// Waiting for the user to approve permissions in the browser and redirect to `127.0.0.1`.
    WaitingForAuthorization { authorize_url: String },
    /// Authorization code received; exchanging PKCE verifier for access/refresh tokens.
    ExchangingToken,
    /// Restoring or refreshing an existing Spotify OAuth session via refresh token.
    Refreshing,
    /// Successfully authenticated with Spotify.
    Authenticated(UserProfile),
    /// Authentication attempt failed or was cancelled.
    Failed {
        message: String,
        authorize_url: Option<String>,
    },
    /// Stored or active Spotify session expired or was revoked; user must log in again.
    SessionExpired { message: String },
}

impl AuthUiState {
    /// Map fine-grained UI state to canonical frontend [`AuthStatus`](crate::auth::service::AuthStatus).
    pub fn auth_status(&self) -> crate::auth::service::AuthStatus {
        use crate::auth::service::AuthStatus;
        match self {
            Self::SignedOut => AuthStatus::NotLoggedIn,
            Self::OpeningBrowser { .. }
            | Self::WaitingForAuthorization { .. }
            | Self::ExchangingToken => AuthStatus::Authenticating,
            Self::Refreshing => AuthStatus::Refreshing,
            Self::Authenticated(_) => AuthStatus::Authenticated,
            Self::Failed { .. } | Self::SessionExpired { .. } => AuthStatus::AuthenticationFailed,
        }
    }
}

/// Current lyrics state for the active track.
#[derive(Debug, Clone, Default)]
pub enum LyricsUiState {
    #[default]
    Idle,
    Loading {
        track_id: String,
    },
    Loaded {
        track_id: String,
        document: std::sync::Arc<LyricsDocument>,
        /// Monotonic revision incremented when a new document is loaded or `offset_ms` changes,
        /// allowing `KaraokeLayoutCache` to invalidate in O(1) without deep comparisons.
        revision: u64,
    },
    Ambiguous {
        track_id: String,
        provider_name: String,
        candidates: Vec<LyricsCandidate>,
    },
    Unavailable {
        track_id: String,
        reason: String,
    },
}

/// Right-hand utility drawer mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SideDrawer {
    #[default]
    None,
    Lyrics,
    Queue,
}

/// Active search category filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchTab {
    #[default]
    All,
    Tracks,
    Artists,
    Albums,
    Playlists,
}

/// Explicit async load state preventing permanent "Loading..." UI traps.
#[derive(Debug, Clone)]
pub enum LoadState<T> {
    Loading,
    Loaded(T),
    Failed(String),
}

/// Active toast/banner message.
#[derive(Debug, Clone)]
pub struct StatusNotice {
    pub message: String,
    pub is_error: bool,
    pub created_at: Instant,
}

/// Complete client-side state owned by the GUI thread.
pub struct AppState {
    // Navigation & smooth transition timer
    pub current_route: Route,
    pub back_stack: Vec<Route>,
    pub forward_stack: Vec<Route>,
    pub route_changed_at: Instant,

    // Auth & settings
    pub auth: AuthUiState,
    pub client_id_configured: bool,
    pub portable_mode: bool,
    pub settings: UserSettings,
    pub side_drawer: SideDrawer,

    // Media library & catalog caches
    pub liked_tracks: Vec<Track>,
    pub recently_played: Vec<Track>,
    pub top_tracks: Vec<Track>,
    pub playlists: Vec<Playlist>,
    pub albums: Vec<Album>,
    pub artists: Vec<Artist>,
    pub local_tracks: Vec<Track>,
    pub showcase_tracks: Vec<Track>,
    pub playlist_tracks: HashMap<String, LoadState<Vec<Track>>>,
    pub album_tracks: HashMap<String, LoadState<Vec<Track>>>,
    pub artist_albums: HashMap<String, LoadState<Vec<Album>>>,
    pub devices: Vec<ConnectDevice>,

    // Search state with debounce
    pub search_input: String,
    pub last_dispatched_query: String,
    pub search_dirty_since: Option<Instant>,
    pub search_loading: bool,
    pub search_error: Option<String>,
    pub search_tab: SearchTab,
    pub search_results: Option<SearchResults>,
    pub search_cache: HashMap<String, SearchResults>,

    // Playback & queue snapshot
    pub playback: PlaybackSnapshot,
    pub queue_items: Vec<Track>,
    pub queue_index: Option<usize>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub seek_drag_ms: Option<u64>,
    pub muted_previous_volume: Option<f32>,
    pub last_volume_dispatch: Option<Instant>,

    // Lyrics UI state
    pub lyrics: LyricsUiState,
    pub lyrics_revision_counter: u64,
    pub lyrics_auto_scroll: bool,
    pub last_scrolled_line_idx: Option<usize>,
    pub last_lyrics_track_id: Option<String>,
    pub show_lyrics_import: bool,

    // Local library & OS media integration status
    pub local_scan_in_progress: bool,
    pub system_media_status: String,

    // Settings inputs
    pub new_local_music_dir: String,
    pub new_local_lyrics_dir: String,
    pub import_lyrics_path: String,
    pub musixmatch_key_configured: bool,
    pub musixmatch_key_input: String,
    pub soloist: crate::audio::soloist::SoloistRuntimeSnapshot,
    pub soloist_key_input: String,

    // Notifications
    pub notice: Option<StatusNotice>,
}

impl AppState {
    pub fn new(settings: UserSettings, client_id_configured: bool, portable_mode: bool) -> Self {
        let showcase_tracks = bundled_showcase_tracks();
        let mut soloist = crate::audio::soloist::SoloistRuntimeSnapshot::default();
        if !settings.soloist_device_name.trim().is_empty() {
            soloist.device_name = settings.soloist_device_name.clone();
        }
        Self {
            current_route: Route::Home,
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            route_changed_at: Instant::now(),
            auth: AuthUiState::SignedOut,
            client_id_configured,
            portable_mode,
            settings,
            side_drawer: SideDrawer::None,
            liked_tracks: Vec::new(),
            recently_played: Vec::new(),
            top_tracks: Vec::new(),
            playlists: Vec::new(),
            albums: Vec::new(),
            artists: Vec::new(),
            local_tracks: Vec::new(),
            showcase_tracks,
            playlist_tracks: HashMap::new(),
            album_tracks: HashMap::new(),
            artist_albums: HashMap::new(),
            devices: Vec::new(),
            search_input: String::new(),
            last_dispatched_query: String::new(),
            search_dirty_since: None,
            search_loading: false,
            search_error: None,
            search_tab: SearchTab::All,
            search_results: None,
            search_cache: HashMap::new(),
            playback: PlaybackSnapshot::default(),
            queue_items: Vec::new(),
            queue_index: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            seek_drag_ms: None,
            muted_previous_volume: None,
            last_volume_dispatch: None,
            lyrics: LyricsUiState::Idle,
            lyrics_revision_counter: 0,
            lyrics_auto_scroll: true,
            last_scrolled_line_idx: None,
            last_lyrics_track_id: None,
            show_lyrics_import: false,
            local_scan_in_progress: false,
            system_media_status: "Initializing…".to_string(),
            new_local_music_dir: String::new(),
            new_local_lyrics_dir: String::new(),
            import_lyrics_path: String::new(),
            musixmatch_key_configured: false,
            musixmatch_key_input: String::new(),
            soloist,
            soloist_key_input: String::new(),
            notice: None,
        }
    }

    pub fn next_lyrics_revision(&mut self) -> u64 {
        self.lyrics_revision_counter = self.lyrics_revision_counter.wrapping_add(1);
        self.lyrics_revision_counter
    }

    /// Navigate to a new route, pushing the previous route onto the back stack.
    pub fn navigate(&mut self, next: Route) {
        if self.current_route == next {
            return;
        }
        let prev = std::mem::replace(&mut self.current_route, next);
        self.back_stack.push(prev);
        if self.back_stack.len() > 64 {
            self.back_stack.remove(0);
        }
        self.forward_stack.clear();
        self.route_changed_at = Instant::now();
    }

    pub fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
    }

    pub fn go_back(&mut self) {
        if let Some(prev) = self.back_stack.pop() {
            let current = std::mem::replace(&mut self.current_route, prev);
            self.forward_stack.push(current);
            self.route_changed_at = Instant::now();
        }
    }

    pub fn go_forward(&mut self) {
        if let Some(next) = self.forward_stack.pop() {
            let current = std::mem::replace(&mut self.current_route, next);
            self.back_stack.push(current);
            self.route_changed_at = Instant::now();
        }
    }

    /// Smooth `0.0..=1.0` page transition progress (instant when `reduce_motion` is enabled).
    pub fn transition_progress(&self) -> f32 {
        if self.settings.reduce_motion {
            return 1.0;
        }
        let elapsed = self.route_changed_at.elapsed().as_secs_f32();
        (elapsed / 0.16).clamp(0.0, 1.0)
    }

    pub fn toggle_drawer(&mut self, drawer: SideDrawer) {
        if self.side_drawer == drawer {
            self.side_drawer = SideDrawer::None;
        } else {
            self.side_drawer = drawer;
        }
    }

    /// Returns a debounced search query when the user has paused typing for 320ms.
    pub fn poll_debounced_search(&mut self) -> Option<String> {
        let dirty_since = self.search_dirty_since?;
        if dirty_since.elapsed() < Duration::from_millis(320) {
            return None;
        }
        self.search_dirty_since = None;
        let query = self.search_input.trim().to_string();
        if query.is_empty() {
            self.search_results = None;
            self.search_loading = false;
            self.search_error = None;
            self.last_dispatched_query.clear();
            return None;
        }
        if query == self.last_dispatched_query {
            return None;
        }
        self.last_dispatched_query = query.clone();
        if let Some(cached) = self.search_cache.get(&query.to_lowercase()).cloned() {
            self.search_results = Some(cached);
            self.search_loading = false;
            self.search_error = None;
            return None;
        }
        self.search_loading = true;
        self.search_error = None;
        Some(query)
    }

    pub fn set_notice(&mut self, message: impl Into<String>, is_error: bool) {
        self.notice = Some(StatusNotice {
            message: message.into(),
            is_error,
            created_at: Instant::now(),
        });
    }

    pub fn should_load_playlist(&self, id: &str) -> bool {
        !matches!(
            self.playlist_tracks.get(id),
            Some(LoadState::Loaded(_) | LoadState::Loading)
        )
    }

    pub fn should_load_album(&self, id: &str) -> bool {
        !matches!(
            self.album_tracks.get(id),
            Some(LoadState::Loaded(_) | LoadState::Loading)
        )
    }

    pub fn should_load_artist(&self, id: &str) -> bool {
        !matches!(
            self.artist_albums.get(id),
            Some(LoadState::Loaded(_) | LoadState::Loading)
        )
    }

    pub fn expire_old_notice(&mut self) {
        if let Some(notice) = &self.notice {
            let ttl = if notice.is_error { 9 } else { 5 };
            if notice.created_at.elapsed() > Duration::from_secs(ttl) {
                self.notice = None;
            }
        }
    }
}
