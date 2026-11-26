//! Shared domain models for tracks, albums, artists, playlists, user profiles, and playback.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Playback repeat mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    Context,
    Track,
}

impl RepeatMode {
    /// Cycle through `Off -> Context -> Track -> Off`.
    pub fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Context,
            Self::Context => Self::Track,
            Self::Track => Self::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Context => "context",
            Self::Track => "track",
        }
    }

    pub fn from_db_str(value: &str) -> Self {
        match value {
            "context" => Self::Context,
            "track" => Self::Track,
            _ => Self::Off,
        }
    }
}

/// Explicit, honest audio operating mode.
///
/// Spotirust separates remote Spotify Connect control from native local audio playback
/// and NEVER simulates fake playback or fake progress without a real audio sink or confirmed Connect device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioBackendKind {
    /// Official Spotify Soloist headless playback engine managed locally via `--ws 127.0.0.1:0`
    /// (requires personal Soloist API key & Spotify Premium; works after closing the official Spotify app).
    SpotifySoloist,
    /// Official Spotify Web API Connect remote controller (`/me/player/*`, requires Premium & active device).
    SpotifyConnect,
    /// Native local audio file playback via `rodio` (MP3, FLAC, OGG, WAV).
    #[default]
    #[serde(alias = "auto", alias = "preview_clock")]
    LocalRodio,
    /// Optional community `librespot` 0.8 backend (blocked for third-party OAuth tokens since Aug 2026; see docs).
    Librespot,
}

impl AudioBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SpotifySoloist => "spotify_soloist",
            Self::SpotifyConnect => "spotify_connect",
            Self::LocalRodio => "local_rodio",
            Self::Librespot => "librespot",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SpotifySoloist => "Spotify Soloist (Built-in Headless Engine)",
            Self::SpotifyConnect => "Spotify Connect (Remote)",
            Self::LocalRodio => "Local Player (Rodio)",
            Self::Librespot => "Librespot 0.8 (Blocked / Experimental)",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::SpotifySoloist => "Soloist",
            Self::SpotifyConnect => "Spotify Connect",
            Self::LocalRodio => "Local Player",
            Self::Librespot => "Librespot (Blocked)",
        }
    }

    pub fn from_db_str(value: &str) -> Self {
        match value {
            "spotify_soloist" => Self::SpotifySoloist,
            "spotify_connect" => Self::SpotifyConnect,
            "librespot" => Self::Librespot,
            _ => Self::LocalRodio,
        }
    }
}

/// Authenticated Spotify user profile metadata (never contains tokens or secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: String,
    /// Immutable pseudoanonymous identifier added in Spotify Web API May 2026.
    pub account_id: Option<String>,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub uri: Option<String>,
}

/// Canonical music track representation shared across Spotify catalog, local audio, and lyrics matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub uri: String,
    pub title: String,
    pub artists: Vec<String>,
    pub album_id: Option<String>,
    pub album_name: String,
    pub duration_ms: u64,
    pub isrc: Option<String>,
    pub artwork_url: Option<String>,
    pub explicit: bool,
    pub liked: bool,
    /// Populated when this track points to a local audio file on disk.
    pub local_path: Option<PathBuf>,
    /// Optional accent color for local/demo tracks without remote artwork.
    #[serde(default)]
    pub accent_rgb: Option<[u8; 3]>,
}

impl Track {
    pub fn artist_display(&self) -> String {
        if self.artists.is_empty() {
            "Unknown Artist".to_string()
        } else {
            self.artists.join(", ")
        }
    }

    pub fn primary_artist(&self) -> &str {
        self.artists.first().map(String::as_str).unwrap_or("")
    }
}

/// Album metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Album {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artists: Vec<String>,
    pub release_date: Option<String>,
    pub total_tracks: u32,
    pub artwork_url: Option<String>,
    #[serde(default)]
    pub accent_rgb: Option<[u8; 3]>,
}

impl Album {
    pub fn artist_display(&self) -> String {
        if self.artists.is_empty() {
            "Unknown Artist".to_string()
        } else {
            self.artists.join(", ")
        }
    }
}

/// Artist metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub genres: Vec<String>,
    pub image_url: Option<String>,
}

/// Playlist metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playlist {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub description: String,
    pub owner_name: String,
    pub owner_id: String,
    pub snapshot_id: String,
    pub total_tracks: u32,
    pub artwork_url: Option<String>,
    pub collaborative: bool,
    pub public: Option<bool>,
}

/// Spotify Connect playback device metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectDevice {
    pub id: Option<String>,
    pub name: String,
    pub device_type: String,
    pub is_active: bool,
    pub is_restricted: bool,
    pub volume_percent: Option<u8>,
}

/// Aggregated catalog search results.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResults {
    pub query: String,
    pub tracks: Vec<Track>,
    pub artists: Vec<Artist>,
    pub albums: Vec<Album>,
    pub playlists: Vec<Playlist>,
}

/// Active navigation route in the desktop UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    Home,
    Search,
    Library,
    LikedSongs,
    RecentlyPlayed,
    Playlists,
    Albums,
    Artists,
    LocalFiles,
    PlaylistDetail(String),
    AlbumDetail(String),
    ArtistDetail(String),
    NowPlaying,
    Settings,
}

impl Route {
    pub fn title(&self) -> &str {
        match self {
            Self::Home => "Home",
            Self::Search => "Search",
            Self::Library => "Your Library",
            Self::LikedSongs => "Liked Songs",
            Self::RecentlyPlayed => "Recently Played",
            Self::Playlists => "Playlists",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::LocalFiles => "Local Files",
            Self::PlaylistDetail(_) => "Playlist",
            Self::AlbumDetail(_) => "Album",
            Self::ArtistDetail(_) => "Artist",
            Self::NowPlaying => "Now Playing",
            Self::Settings => "Settings",
        }
    }
}

/// Format milliseconds into `m:ss`.
pub fn format_duration_ms(ms: u64) -> String {
    let total_secs = ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{mins}:{secs:02}")
}

/// Built-in royalty-free Soundcheck tracks backed by real locally generated WAV audio files.
/// Played strictly through the native `rodio` Local Player — never via a fake silent clock.
pub fn bundled_showcase_tracks_with_paths(
    midnight_wav: Option<PathBuf>,
    velocity_wav: Option<PathBuf>,
) -> Vec<Track> {
    vec![
        Track {
            id: "local:midnight_compiler".to_string(),
            uri: "spotirust:sample:midnight_compiler".to_string(),
            title: "Midnight Compiler".to_string(),
            artists: vec!["Spotirust Soundcheck".to_string()],
            album_id: Some("local:native_audio_showcase".to_string()),
            album_name: "Native Audio Showcase (Local WAV)".to_string(),
            duration_ms: 22_000,
            isrc: None,
            artwork_url: None,
            explicit: false,
            liked: true,
            local_path: midnight_wav,
            accent_rgb: Some([29, 185, 138]),
        },
        Track {
            id: "local:velocity_pulse".to_string(),
            uri: "spotirust:sample:velocity_pulse".to_string(),
            title: "Velocity Pulse".to_string(),
            artists: vec!["Spotirust Soundcheck".to_string()],
            album_id: Some("local:native_audio_showcase".to_string()),
            album_name: "Native Audio Showcase (Local WAV)".to_string(),
            duration_ms: 19_000,
            isrc: None,
            artwork_url: None,
            explicit: false,
            liked: false,
            local_path: velocity_wav,
            accent_rgb: Some([76, 139, 245]),
        },
    ]
}

/// Metadata-only variant of Soundcheck tracks (before local WAV cache paths are attached).
pub fn bundled_showcase_tracks() -> Vec<Track> {
    bundled_showcase_tracks_with_paths(None, None)
}
