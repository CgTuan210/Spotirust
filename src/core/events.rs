//! Bidirectional command & event protocol decoupling the egui UI thread from Tokio background workers.

use crate::audio::engine::PlaybackSnapshot;
use crate::core::models::{
    Album, Artist, AudioBackendKind, ConnectDevice, Playlist, RepeatMode, SearchResults, Track,
    UserProfile,
};
use crate::lyrics::model::LyricsDocument;
use crate::lyrics::provider::LyricsCandidate;
use std::path::PathBuf;

/// Commands issued from the UI thread to the asynchronous backend coordinator.
#[derive(Debug, Clone)]
pub enum AppCommand {
    // Authentication
    StartSpotifyLogin,
    OpenPendingLoginUrl,
    RefreshSpotifySession,
    Logout,

    // Spotify catalog & library
    RefreshLibrary,
    SearchCatalog {
        query: String,
    },
    LoadPlaylistTracks {
        playlist_id: String,
    },
    LoadAlbumTracks {
        album_id: String,
    },
    LoadArtistAlbums {
        artist_id: String,
    },
    ToggleLikeTrack {
        track: Track,
    },
    RefreshDevices,
    SyncConnectState,
    TransferDevice {
        device_id: String,
    },

    // Playback controls
    PlayTrack {
        track: Track,
        queue_context: Vec<Track>,
    },
    TogglePlayPause,
    NextTrack,
    PreviousTrack,
    SeekTo {
        position_ms: u64,
    },
    SetVolume {
        volume: f32,
    },
    ToggleShuffle,
    CycleRepeat,
    AddToQueue {
        track: Track,
    },
    PlayQueueIndex {
        index: usize,
    },
    RemoveQueueIndex {
        index: usize,
    },
    ClearQueue,
    SwitchAudioBackend {
        backend: AudioBackendKind,
    },

    // Local audio & lyrics
    PickLocalMusicFolder,
    PickLocalMusicFiles,
    ScanLocalMusicDir {
        path: PathBuf,
    },
    RescanLocalLibrary,
    RemoveLocalMusicDir {
        path: PathBuf,
    },
    ShufflePlayTracks {
        tracks: Vec<Track>,
    },
    FetchLyrics {
        track: Track,
        force_refresh: bool,
    },
    PickImportLyricsFile {
        track: Track,
    },
    ImportLyricsFile {
        track: Track,
        path: PathBuf,
    },
    SelectLyricsCandidate {
        track: Track,
        candidate: Box<LyricsCandidate>,
    },
    SetLyricsOffset {
        offset_ms: i64,
    },
    ClearLyricsCache,

    // Artwork
    RequestArtwork {
        url: String,
    },

    // Maintenance & settings
    SaveSettings(crate::core::config::UserSettings),
    SaveMusixmatchApiKey {
        api_key: Option<String>,
    },
    SaveSoloistApiKey {
        api_key: Option<String>,
    },
    DownloadSoloist,
    PickSoloistBinary,
    StartSoloist,
    StopSoloist,
    RestartSoloist,
    ResetSoloistPairing,
    OpenExternalUrl {
        url: String,
    },
    ClearCache,
    ResetApplication,
}

/// Events emitted by background tasks to update UI state without blocking the GUI thread.
#[derive(Debug, Clone)]
pub enum AppEvent {
    // Authentication lifecycle
    AuthOpeningBrowser {
        authorize_url: String,
    },
    AuthWaitingForCallback {
        authorize_url: String,
    },
    AuthExchangingToken,
    AuthRefreshing,
    AuthSucceeded {
        profile: UserProfile,
    },
    AuthFailed {
        message: String,
        authorize_url: Option<String>,
    },
    SessionExpired {
        message: String,
    },
    AuthLoggedOut,

    // Catalog & library updates
    LibraryLoaded {
        liked_tracks: Vec<Track>,
        recently_played: Vec<Track>,
        top_tracks: Vec<Track>,
        playlists: Vec<Playlist>,
        albums: Vec<Album>,
        artists: Vec<Artist>,
    },
    SearchCompleted(SearchResults),
    SearchFailed {
        query: String,
        error: String,
    },
    PlaylistTracksLoaded {
        playlist_id: String,
        tracks: Vec<Track>,
    },
    PlaylistTracksFailed {
        playlist_id: String,
        error: String,
    },
    AlbumTracksLoaded {
        album_id: String,
        tracks: Vec<Track>,
    },
    AlbumTracksFailed {
        album_id: String,
        error: String,
    },
    ArtistAlbumsLoaded {
        artist_id: String,
        albums: Vec<Album>,
    },
    ArtistAlbumsFailed {
        artist_id: String,
        error: String,
    },
    TrackLikeUpdated {
        track: Track,
        liked: bool,
    },
    DevicesLoaded(Vec<ConnectDevice>),

    // Playback updates
    PlaybackUpdated(PlaybackSnapshot),
    SoloistStatusUpdated(crate::audio::soloist::SoloistRuntimeSnapshot),
    SoloistCustomBinaryUpdated(Option<PathBuf>),
    QueueUpdated {
        items: Vec<Track>,
        current_index: Option<usize>,
        shuffle: bool,
        repeat: RepeatMode,
    },
    LocalScanStarted,
    LocalTracksScanned {
        tracks: Vec<Track>,
        added_folder: Option<PathBuf>,
    },
    SystemMediaStatusUpdated(String),

    // Lyrics updates
    LyricsLoading {
        track_id: String,
    },
    LyricsLoaded {
        track_id: String,
        document: LyricsDocument,
    },
    LyricsAmbiguous {
        track_id: String,
        provider_name: String,
        candidates: Vec<LyricsCandidate>,
    },
    LyricsUnavailable {
        track_id: String,
        reason: String,
    },
    LyricsCacheCleared {
        freed_entries: usize,
    },

    // Artwork decoded off-thread
    ArtworkLoaded {
        url: String,
        width: usize,
        height: usize,
        rgba: Vec<u8>,
        dominant_rgb: [u8; 3],
    },
    ArtworkFailed {
        url: String,
    },

    // Notifications & errors
    MusixmatchKeyUpdated {
        configured: bool,
    },
    StatusBanner {
        message: String,
        is_error: bool,
    },
    CacheCleared {
        freed_entries: usize,
    },
    ApplicationReset,
}
