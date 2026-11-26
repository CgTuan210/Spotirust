//! Application configuration, portable mode resolution, and build-time Spotify Client ID handling.

use crate::core::models::AudioBackendKind;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Optional developer-configured Spotify Client ID injected at build/release time.
///
/// Per project security policy:
/// - Never embed a Client Secret in a desktop application.
/// - Never present a manual Client ID input field to end users.
/// - Maintainers set `SPOTIFY_CLIENT_ID` (or `SPOTIRUST_CLIENT_ID`) at build time or dev env.
pub const COMPILE_TIME_SPOTIFY_CLIENT_ID: Option<&str> = option_env!("SPOTIFY_CLIENT_ID");
pub const COMPILE_TIME_CLIENT_ID: Option<&str> = option_env!("SPOTIRUST_CLIENT_ID");

/// Optional developer-configured Spotify IPv4 loopback redirect URI (e.g.
/// `http://127.0.0.1/callback` for dynamic OS port or `http://127.0.0.1:43821/callback`
/// for a fixed registered port).
pub const COMPILE_TIME_SPOTIFY_REDIRECT_URI: Option<&str> = option_env!("SPOTIFY_REDIRECT_URI");

/// Default loopback callback port (`0` means bind an ephemeral port on `127.0.0.1`).
///
/// Maintainers who registered a fixed loopback redirect URI (e.g. `http://127.0.0.1:43821/callback`)
/// can bake `SPOTIFY_REDIRECT_URI` or `SPOTIRUST_REDIRECT_PORT=43821` at build time. Dynamic ports
/// are supported by Spotify when `http://127.0.0.1/callback` is registered (RFC 8252 / 2025+ rules).
pub const COMPILE_TIME_REDIRECT_PORT: Option<&str> = option_env!("SPOTIRUST_REDIRECT_PORT");

/// Default registered loopback redirect URI template.
pub const DEFAULT_SPOTIFY_REDIRECT_URI: &str = "http://127.0.0.1/callback";

/// Identifier for each compliant online lyrics provider that can be ordered & toggled in Automatic mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnlineLyricsProviderId {
    Unison,
    AmllTtmlDb,
    Musixmatch,
    Lrclib,
}

impl OnlineLyricsProviderId {
    pub fn default_order() -> Vec<Self> {
        vec![
            Self::Unison,
            Self::AmllTtmlDb,
            Self::Musixmatch,
            Self::Lrclib,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unison => "Unison (Word TTML · Open-Source MIT)",
            Self::AmllTtmlDb => "AMLL TTML DB (Word TTML · CC0-1.0 · Spotify ID)",
            Self::Musixmatch => "Musixmatch Official API (RichSync/Subtitle/Plain · API Key)",
            Self::Lrclib => "LRCLIB Public API (Lyricsfile Word-Sync + LRC + Plain)",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Unison => "Unison",
            Self::AmllTtmlDb => "AMLL TTML DB",
            Self::Musixmatch => "Musixmatch",
            Self::Lrclib => "LRCLIB",
        }
    }
}

/// Runtime provider chain configuration passed from Settings into `LyricsService`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsProviderChainConfig {
    pub order: Vec<OnlineLyricsProviderId>,
    pub enable_unison: bool,
    pub enable_amll_db: bool,
    pub enable_musixmatch: bool,
    pub enable_lrclib: bool,
}

impl Default for LyricsProviderChainConfig {
    fn default() -> Self {
        Self {
            order: OnlineLyricsProviderId::default_order(),
            enable_unison: true,
            enable_amll_db: true,
            enable_musixmatch: true,
            enable_lrclib: true,
        }
    }
}

impl LyricsProviderChainConfig {
    pub fn is_enabled(&self, id: OnlineLyricsProviderId) -> bool {
        match id {
            OnlineLyricsProviderId::Unison => self.enable_unison,
            OnlineLyricsProviderId::AmllTtmlDb => self.enable_amll_db,
            OnlineLyricsProviderId::Musixmatch => self.enable_musixmatch,
            OnlineLyricsProviderId::Lrclib => self.enable_lrclib,
        }
    }

    /// Return deduplicated configured provider order with any missing providers appended.
    pub fn normalized_order(&self) -> Vec<OnlineLyricsProviderId> {
        let mut out = Vec::with_capacity(4);
        for &id in &self.order {
            if !out.contains(&id) {
                out.push(id);
            }
        }
        for id in OnlineLyricsProviderId::default_order() {
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }
}

/// User-selected lyrics source strategy shown in Settings.
///
/// Spotirust NEVER mislabels LRCLIB, Unison, AMLL, or local files as "Apple Music" or "BiniLyrics".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsSourcePreference {
    /// Automatic compliant chain prioritizing Word-level -> Line-level -> Plain across enabled providers.
    #[default]
    Automatic,
    /// Unison open-source word-level TTML API only (`unison.betterlyrics.org`).
    Unison,
    /// AMLL TTML DB (CC0-1.0) hand-timed word-level TTML by Spotify ID only.
    AmllTtmlDb,
    /// Official Musixmatch Developer API only (requires official API key).
    Musixmatch,
    /// Public LRCLIB REST API only (`lrclib.net` — Lyricsfile word-sync + LRC + plain).
    Lrclib,
    /// Local user-supplied `.ttml` / `.lrc` / `.lyricsfile` files only (zero network requests).
    LocalOnly,
    /// Apple Music official API only (currently blocked upstream: no public lyrics endpoint;
    /// will NOT silently fall back to other sources while pretending to be Apple Music).
    AppleMusic,
}

impl LyricsSourcePreference {
    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic (Local -> Unison -> AMLL -> Musixmatch -> LRCLIB)",
            Self::Unison => "Unison Only (Word-Synced TTML · unison.betterlyrics.org)",
            Self::AmllTtmlDb => "AMLL TTML DB Only (Word-Synced TTML · CC0-1.0)",
            Self::Musixmatch => "Musixmatch Official API Only (Requires Developer API Key)",
            Self::Lrclib => "LRCLIB Public API Only (Lyricsfile Word-Sync + LRC)",
            Self::LocalOnly => "Local .ttml / .lrc / .lyricsfile Files Only (Offline)",
            Self::AppleMusic => "Apple Music (Blocked — No Official Public Lyrics API)",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::Unison => "Unison",
            Self::AmllTtmlDb => "AMLL DB",
            Self::Musixmatch => "Musixmatch",
            Self::Lrclib => "LRCLIB",
            Self::LocalOnly => "Local Only",
            Self::AppleMusic => "Apple Music (Blocked)",
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_provider_order() -> Vec<OnlineLyricsProviderId> {
    OnlineLyricsProviderId::default_order()
}

/// Persistent user-editable preferences stored in SQLite (`app_settings`).
/// Note: API keys and OAuth tokens are NEVER stored here; they live exclusively in the OS credential vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSettings {
    pub theme_id: String,
    pub audio_backend: AudioBackendKind,
    #[serde(default)]
    pub lyrics_source: LyricsSourcePreference,
    #[serde(default = "default_provider_order")]
    pub lyrics_provider_order: Vec<OnlineLyricsProviderId>,
    pub prefer_ttml_lyrics: bool,
    #[serde(default = "default_true")]
    pub enable_unison_online: bool,
    #[serde(default = "default_true")]
    pub enable_amll_db_online: bool,
    #[serde(default = "default_true")]
    pub enable_musixmatch_online: bool,
    pub enable_lrclib_online: bool,
    pub show_lyric_translations: bool,
    pub lyrics_offset_ms: i64,
    pub local_music_dirs: Vec<PathBuf>,
    pub local_lyrics_dirs: Vec<PathBuf>,
    pub compact_sidebar: bool,
    pub reduce_motion: bool,
}

impl UserSettings {
    pub fn lyrics_chain_config(&self) -> LyricsProviderChainConfig {
        LyricsProviderChainConfig {
            order: self.lyrics_provider_order.clone(),
            enable_unison: self.enable_unison_online,
            enable_amll_db: self.enable_amll_db_online,
            enable_musixmatch: self.enable_musixmatch_online,
            enable_lrclib: self.enable_lrclib_online,
        }
    }
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            theme_id: "obsidian_dark".to_string(),
            audio_backend: AudioBackendKind::LocalRodio,
            lyrics_source: LyricsSourcePreference::Automatic,
            lyrics_provider_order: OnlineLyricsProviderId::default_order(),
            prefer_ttml_lyrics: true,
            enable_unison_online: true,
            enable_amll_db_online: true,
            enable_musixmatch_online: true,
            enable_lrclib_online: true,
            show_lyric_translations: true,
            lyrics_offset_ms: 0,
            local_music_dirs: Vec::new(),
            local_lyrics_dirs: Vec::new(),
            compact_sidebar: false,
            reduce_motion: false,
        }
    }
}

/// Runtime filesystem paths and build configuration for Spotirust.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub portable_mode: bool,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub db_path: PathBuf,
    pub artwork_cache_dir: PathBuf,
    pub themes_dir: PathBuf,
    pub lyrics_dir: PathBuf,
    /// Maintainer-provided Spotify OAuth Client ID (`SPOTIFY_CLIENT_ID` or `SPOTIRUST_CLIENT_ID`).
    pub spotify_client_id: Option<String>,
    /// Configured loopback redirect URI (`SPOTIFY_REDIRECT_URI`, default `http://127.0.0.1/callback`).
    pub oauth_redirect_uri: String,
    /// Non-fatal redirect URI configuration error surfaced cleanly when attempting login.
    pub oauth_redirect_uri_error: Option<String>,
    /// Loopback callback port (`0` = dynamic ephemeral port on `127.0.0.1`).
    pub oauth_redirect_port: u16,
    /// Callback path component (default `/callback`).
    pub oauth_redirect_path: String,
}

impl AppConfig {
    /// Resolve runtime directories, portable mode, and maintainer OAuth settings.
    pub fn load(force_portable: bool) -> anyhow::Result<Self> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));

        let portable_flag_exists = exe_dir
            .as_ref()
            .map(|dir| dir.join("portable.flag").exists())
            .unwrap_or(false);

        let portable_env = std::env::var("SPOTIRUST_PORTABLE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let portable_mode = force_portable || portable_flag_exists || portable_env;

        let (data_dir, cache_dir) = if portable_mode {
            let base = exe_dir
                .unwrap_or_else(|| PathBuf::from("."))
                .join("spotirust-data");
            (base.join("data"), base.join("cache"))
        } else {
            let data = dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("spotirust");
            let cache = dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("spotirust");
            (data, cache)
        };

        let db_path = data_dir.join("spotirust.db");
        let artwork_cache_dir = cache_dir.join("artwork");
        let themes_dir = data_dir.join("themes");
        let lyrics_dir = data_dir.join("lyrics");

        let spotify_client_id = resolve_spotify_client_id();
        let resolved_redirect = resolve_redirect_config();

        Ok(Self {
            portable_mode,
            data_dir,
            cache_dir,
            db_path,
            artwork_cache_dir,
            themes_dir,
            lyrics_dir,
            spotify_client_id,
            oauth_redirect_uri: resolved_redirect.uri,
            oauth_redirect_uri_error: resolved_redirect.error,
            oauth_redirect_port: resolved_redirect.port,
            oauth_redirect_path: resolved_redirect.path,
        })
    }

    /// Create an isolated config rooted at `root` (ideal for tests and sandboxes).
    pub fn in_directory(root: &Path, client_id: Option<String>) -> Self {
        let data_dir = root.join("data");
        let cache_dir = root.join("cache");
        Self {
            portable_mode: true,
            db_path: data_dir.join("spotirust.db"),
            artwork_cache_dir: cache_dir.join("artwork"),
            themes_dir: data_dir.join("themes"),
            lyrics_dir: data_dir.join("lyrics"),
            data_dir,
            cache_dir,
            spotify_client_id: client_id,
            oauth_redirect_uri: DEFAULT_SPOTIFY_REDIRECT_URI.to_string(),
            oauth_redirect_uri_error: None,
            oauth_redirect_port: 0,
            oauth_redirect_path: "/callback".to_string(),
        }
    }

    /// Ensure all required data and cache directories exist with safe permissions.
    pub fn ensure_dirs(&self) -> anyhow::Result<()> {
        for dir in [
            &self.data_dir,
            &self.cache_dir,
            &self.artwork_cache_dir,
            &self.themes_dir,
            &self.lyrics_dir,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedRedirectConfig {
    uri: String,
    port: u16,
    path: String,
    error: Option<String>,
}

fn resolve_spotify_client_id() -> Option<String> {
    for id in [COMPILE_TIME_SPOTIFY_CLIENT_ID, COMPILE_TIME_CLIENT_ID]
        .into_iter()
        .flatten()
    {
        let trimmed = id.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for env_key in ["SPOTIFY_CLIENT_ID", "SPOTIRUST_CLIENT_ID"] {
        if let Ok(val) = std::env::var(env_key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn resolve_redirect_config() -> ResolvedRedirectConfig {
    let raw_uri = COMPILE_TIME_SPOTIFY_REDIRECT_URI
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            std::env::var("SPOTIFY_REDIRECT_URI")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });

    if let Some(uri) = raw_uri {
        return parse_redirect_uri_or_error(&uri);
    }

    let port = resolve_redirect_port();
    let uri = if port == 0 {
        DEFAULT_SPOTIFY_REDIRECT_URI.to_string()
    } else {
        format!("http://127.0.0.1:{port}/callback")
    };
    ResolvedRedirectConfig {
        uri,
        port,
        path: "/callback".to_string(),
        error: None,
    }
}

fn parse_redirect_uri_or_error(uri: &str) -> ResolvedRedirectConfig {
    match crate::auth::oauth::parse_loopback_redirect_uri_config(uri) {
        Ok((port, path)) => ResolvedRedirectConfig {
            uri: uri.to_string(),
            port,
            path,
            error: None,
        },
        Err(err) => {
            tracing::warn!(
                redirect_uri = %uri,
                error = %err,
                "Invalid SPOTIFY_REDIRECT_URI configured; startup preserved, login will report error"
            );
            ResolvedRedirectConfig {
                uri: uri.to_string(),
                port: 0,
                path: "/callback".to_string(),
                error: Some(err.to_string()),
            }
        }
    }
}

fn resolve_redirect_port() -> u16 {
    if let Some(raw) = COMPILE_TIME_REDIRECT_PORT
        && let Ok(port) = raw.trim().parse::<u16>()
    {
        return port;
    }
    std::env::var("SPOTIRUST_REDIRECT_PORT")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dynamic_and_fixed_127_0_0_1_redirect_uris_and_rejects_localhost_safely() {
        let dyn_cfg = parse_redirect_uri_or_error("http://127.0.0.1/callback");
        assert!(dyn_cfg.error.is_none());
        assert_eq!(dyn_cfg.port, 0);
        assert_eq!(dyn_cfg.path, "/callback");

        let fixed_cfg = parse_redirect_uri_or_error("http://127.0.0.1:43821/auth/cb");
        assert!(fixed_cfg.error.is_none());
        assert_eq!(fixed_cfg.port, 43821);
        assert_eq!(fixed_cfg.path, "/auth/cb");

        // Localhost is rejected by Spotify rules and captured safely without panicking
        let bad_cfg = parse_redirect_uri_or_error("http://localhost:8888/callback");
        assert!(bad_cfg.error.is_some());
    }
}
