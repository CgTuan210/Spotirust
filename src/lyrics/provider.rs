//! Decoupled `LyricsProvider` architecture (`search` -> `matcher` -> `fetch`) and compliant
//! multi-provider fallback chain prioritizing Word-level -> Line-level -> Plain timing.
//!
//! Implemented real providers:
//! 1. `BundledShowcaseLyricsProvider`: royalty-free offline Soundcheck TTML & Enhanced LRC
//! 2. `LocalFileLyricsProvider`: user-supplied `.ttml`, `.lyricsfile`, and `.lrc` files
//! 3. `BiniLyricsProvider`: word-level & line-synced TTML lookup (`lyrics-api.binimum.org` / `lrc.red`)
//! 4. `UnisonProvider`: open-source MIT-permitted word-level TTML API (`unison.betterlyrics.org`)
//! 5. `AmllTtmlDbProvider`: CC0-1.0 community hand-timed word-level TTML DB (`amll-dev/amll-ttml-db`)
//! 6. `MusixmatchProvider`: official Musixmatch Developer API (`RichSync` word, Subtitle LRC, Plain)
//! 7. `LrclibProvider`: public LRCLIB REST API with CC0 `Lyricsfile` v1.0 word-sync + LRC + Plain
//! 8. `AppleMusicLyricsProvider`: honest upstream-blocked status (zero unauthorized endpoints)
//!
//! Always labels every lyrics source honestly and NEVER fabricates fake timestamps for plain lyrics.

use crate::core::config::{
    LyricsProviderChainConfig, LyricsSourcePreference, OnlineLyricsProviderId,
};
use crate::core::models::Track;
use crate::lyrics::amll_db::AmllTtmlDbProvider;
use crate::lyrics::apple_music::AppleMusicLyricsProvider;
pub use crate::lyrics::binilyrics::BiniLyricsProvider;
use crate::lyrics::cache::LyricsCache;
use crate::lyrics::lrc::{parse_lrc, parse_plain_lyrics};
pub use crate::lyrics::lrclib::LrclibProvider;
use crate::lyrics::lyricsfile::parse_lyricsfile;
use crate::lyrics::matcher::{
    LyricCandidate, MatchResolution, TrackMatchQuery, normalize_text, resolve_candidates,
};
use crate::lyrics::model::{LyricsAttribution, LyricsDocument, LyricsFormat, TimingTier};
use crate::lyrics::musixmatch::MusixmatchProvider;
use crate::lyrics::ttml::parse_ttml;
use crate::lyrics::unison::UnisonProvider;
use async_trait::async_trait;
use reqwest::header::RETRY_AFTER;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

const BUNDLED_TTML: &str = include_str!("../../assets/sample_lyrics/spotirust_demo.ttml");
const BUNDLED_LRC: &str = include_str!("../../assets/sample_lyrics/spotirust_demo.lrc");

/// Parse an HTTP `Retry-After` integer header (falling back to `default_secs`).
pub(crate) fn parse_retry_after(resp: &reqwest::Response, default_secs: u64) -> u64 {
    resp.headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(default_secs)
}

/// Canonical source category for a lyrics provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsSourceKind {
    BundledSample,
    LocalFiles,
    BiniLyrics,
    Unison,
    AmllTtmlDb,
    Musixmatch,
    Lrclib,
    AppleMusic,
    Custom,
}

impl LyricsSourceKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::BundledSample => "Bundled Sample",
            Self::LocalFiles => "Local File",
            Self::BiniLyrics => "BiniLyrics",
            Self::Unison => "Unison",
            Self::AmllTtmlDb => "AMLL TTML DB",
            Self::Musixmatch => "Musixmatch",
            Self::Lrclib => "LRCLIB",
            Self::AppleMusic => "Apple Music",
            Self::Custom => "Plugin",
        }
    }
}

/// Decoupled track metadata passed to `LyricsProvider::search`.
#[derive(Debug, Clone)]
pub struct TrackMetadata {
    pub track_id: String,
    pub uri: String,
    pub title: String,
    pub artist: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_ms: u64,
    pub isrc: Option<String>,
    pub explicit: bool,
    pub local_path: Option<PathBuf>,
}

impl From<&Track> for TrackMetadata {
    fn from(t: &Track) -> Self {
        Self {
            track_id: t.id.clone(),
            uri: t.uri.clone(),
            title: t.title.clone(),
            artist: t.primary_artist().to_string(),
            artists: t.artists.clone(),
            album: t.album_name.clone(),
            duration_ms: t.duration_ms,
            isrc: t.isrc.clone(),
            explicit: t.explicit,
            local_path: t.local_path.clone(),
        }
    }
}

impl From<&TrackMetadata> for TrackMatchQuery {
    fn from(m: &TrackMetadata) -> Self {
        Self {
            title: m.title.clone(),
            artist: m.artist.clone(),
            album: m.album.clone(),
            duration_ms: m.duration_ms,
            isrc: m.isrc.clone(),
        }
    }
}

/// Internal payload reference carried by a `LyricsCandidate` so `fetch()` can materialize
/// a `LyricsDocument` cleanly (either from local disk, bundled sample, inline API payload,
/// or remote ID fetch).
#[derive(Debug, Clone)]
pub enum CandidatePayload {
    Bundled {
        raw: &'static str,
        is_ttml: bool,
        source_label: &'static str,
    },
    LocalFile(PathBuf),
    InlineRaw {
        format: LyricsFormat,
        raw_content: String,
        source_label: String,
        attribution: Option<LyricsAttribution>,
    },
    Instrumental {
        source_label: String,
    },
    ExternalId(String),
}

/// Candidate lyrics match returned by `LyricsProvider::search` and optionally displayed
/// in the UI when multiple distinct versions score closely.
#[derive(Debug, Clone)]
pub struct LyricsCandidate {
    pub id: String,
    pub provider_name: String,
    pub source_kind: LyricsSourceKind,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
    pub isrc: Option<String>,
    pub has_word_sync: bool,
    pub has_line_sync: bool,
    pub instrumental: bool,
    pub format_hint: LyricsFormat,
    pub match_score: f32,
    pub match_reason: String,
    pub payload: CandidatePayload,
}

impl LyricsCandidate {
    pub fn to_matcher_candidate(&self) -> LyricCandidate {
        LyricCandidate {
            title: self.title.clone(),
            artist: self.artist.clone(),
            album: self.album.clone(),
            duration_ms: self.duration_ms,
            isrc: self.isrc.clone(),
            has_word_sync: self.has_word_sync,
            has_line_sync: self.has_line_sync,
        }
    }

    pub fn timing_badge(&self) -> &'static str {
        if self.instrumental {
            "Instrumental"
        } else if self.has_word_sync {
            "Word-level"
        } else if self.has_line_sync {
            "Line-level"
        } else {
            "Plain"
        }
    }
}

/// Errors produced by `LyricsProvider` implementations.
#[derive(Debug, Error, Clone)]
pub enum LyricsError {
    #[error("lyrics request timed out")]
    Timeout,
    #[error("network error: {0}")]
    Network(String),
    #[error("lyrics provider rate-limited; retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("authentication required: {0}")]
    AuthRequired(String),
    #[error("{0}")]
    UpstreamUnsupported(String),
    #[error("no matching lyrics found")]
    NotFound,
    #[error("failed to parse lyrics payload: {0}")]
    Parse(String),
    #[error("local lyrics I/O error: {0}")]
    Io(String),
}

/// Raw + parsed output returned by `LyricsProvider::fetch`.
#[derive(Debug, Clone)]
pub struct FetchedLyrics {
    pub document: LyricsDocument,
    pub raw_content: Option<String>,
}

/// Rich resolution outcome returned by `LyricsService::resolve_with_preference`.
#[derive(Debug, Clone)]
pub enum LyricsResolution {
    /// Unambiguous lyrics document ready to render.
    Found(LyricsDocument),
    /// Multiple distinct versions matched with close scores; ask the user to confirm.
    Ambiguous {
        candidates: Vec<LyricsCandidate>,
        provider_name: String,
    },
    /// No lyrics available or selected provider is blocked upstream.
    Unavailable(String),
}

/// Pluggable asynchronous lyrics provider completely decoupled from Spotify playback & UI.
#[async_trait]
pub trait LyricsProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn source_kind(&self) -> LyricsSourceKind;
    fn is_available(&self) -> bool {
        true
    }
    fn unavailable_reason(&self) -> Option<&'static str> {
        None
    }
    /// Whether this provider can supply word-level synchronized lyrics.
    fn supports_word_sync(&self) -> bool {
        false
    }
    /// Whether this provider can supply at least line-level synchronized lyrics.
    fn supports_line_sync(&self) -> bool {
        true
    }
    async fn search(&self, track: &TrackMetadata) -> Result<Vec<LyricsCandidate>, LyricsError>;
    async fn fetch(&self, candidate: &LyricsCandidate) -> Result<FetchedLyrics, LyricsError>;
}

// =============================================================================
// 1. Bundled Showcase Provider (for instant offline word-by-word karaoke demo)
// =============================================================================

pub struct BundledShowcaseLyricsProvider;

#[async_trait]
impl LyricsProvider for BundledShowcaseLyricsProvider {
    fn name(&self) -> &'static str {
        "Spotirust Bundled Sample"
    }

    fn source_kind(&self) -> LyricsSourceKind {
        LyricsSourceKind::BundledSample
    }

    fn supports_word_sync(&self) -> bool {
        true
    }

    async fn search(&self, track: &TrackMetadata) -> Result<Vec<LyricsCandidate>, LyricsError> {
        match track.track_id.as_str() {
            "local:midnight_compiler" => Ok(vec![LyricsCandidate {
                id: "bundled:midnight_compiler".to_string(),
                provider_name: self.name().to_string(),
                source_kind: self.source_kind(),
                title: track.title.clone(),
                artist: track.artist.clone(),
                album: Some(track.album.clone()),
                duration_ms: Some(track.duration_ms),
                isrc: track.isrc.clone(),
                has_word_sync: true,
                has_line_sync: true,
                instrumental: false,
                format_hint: LyricsFormat::Ttml,
                match_score: 200.0,
                match_reason: "Bundled Soundcheck sample TTML".to_string(),
                payload: CandidatePayload::Bundled {
                    raw: BUNDLED_TTML,
                    is_ttml: true,
                    source_label: "Bundled Sample TTML",
                },
            }]),
            "local:velocity_pulse" => Ok(vec![LyricsCandidate {
                id: "bundled:velocity_pulse".to_string(),
                provider_name: self.name().to_string(),
                source_kind: self.source_kind(),
                title: track.title.clone(),
                artist: track.artist.clone(),
                album: Some(track.album.clone()),
                duration_ms: Some(track.duration_ms),
                isrc: track.isrc.clone(),
                has_word_sync: true,
                has_line_sync: true,
                instrumental: false,
                format_hint: LyricsFormat::EnhancedLrc,
                match_score: 200.0,
                match_reason: "Bundled Soundcheck sample Enhanced LRC".to_string(),
                payload: CandidatePayload::Bundled {
                    raw: BUNDLED_LRC,
                    is_ttml: false,
                    source_label: "Bundled Sample Enhanced LRC",
                },
            }]),
            _ => Ok(Vec::new()),
        }
    }

    async fn fetch(&self, candidate: &LyricsCandidate) -> Result<FetchedLyrics, LyricsError> {
        match &candidate.payload {
            CandidatePayload::Bundled {
                raw,
                is_ttml,
                source_label,
            } => {
                let doc = if *is_ttml {
                    parse_ttml(raw, *source_label).map_err(|e| LyricsError::Parse(e.to_string()))?
                } else {
                    parse_lrc(raw, *source_label).map_err(|e| LyricsError::Parse(e.to_string()))?
                };
                Ok(FetchedLyrics {
                    document: doc,
                    raw_content: Some((*raw).to_string()),
                })
            }
            _ => Err(LyricsError::NotFound),
        }
    }
}

// =============================================================================
// 2. Local File Provider (User-supplied `.ttml` > `.lyricsfile` > `.lrc`)
// =============================================================================

#[derive(Clone)]
pub struct LocalFileLyricsProvider {
    search_dirs: Vec<PathBuf>,
}

impl LocalFileLyricsProvider {
    pub fn new(search_dirs: Vec<PathBuf>) -> Self {
        Self { search_dirs }
    }

    /// Parse a user-selected `.ttml`, `.lyricsfile`/`.yaml`, or `.lrc` file directly from disk.
    pub fn load_file(path: &Path) -> anyhow::Result<FetchedLyrics> {
        let content = std::fs::read_to_string(path)?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let source = format!(
            "Local File ({})",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("lyrics")
        );

        let doc = if ext == "ttml" || ext == "xml" || content.trim_start().starts_with('<') {
            parse_ttml(&content, &source)?
        } else if ext == "lyricsfile" || ext == "yaml" || ext == "yml" {
            parse_lyricsfile(&content, &source)?
        } else {
            parse_lrc(&content, &source).unwrap_or_else(|_| parse_plain_lyrics(&content, &source))
        };

        Ok(FetchedLyrics {
            document: doc,
            raw_content: Some(content),
        })
    }

    fn candidate_stems(track: &TrackMetadata) -> Vec<String> {
        let mut stems = Vec::new();
        if let Some(isrc) = &track.isrc
            && !isrc.trim().is_empty()
        {
            stems.push(isrc.trim().to_string());
        }
        if !track.track_id.is_empty()
            && !track.track_id.contains('/')
            && !track.track_id.contains('\\')
        {
            stems.push(track.track_id.replace(':', "_"));
        }
        if !track.artist.is_empty() {
            stems.push(format!("{} - {}", track.artist, track.title));
        }
        stems.push(track.title.clone());
        stems
    }

    fn make_candidate(
        &self,
        track: &TrackMetadata,
        path: PathBuf,
        reason: &str,
    ) -> LyricsCandidate {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let is_ttml = ext == "ttml" || ext == "xml";
        let is_lf = ext == "lyricsfile" || ext == "yaml" || ext == "yml";
        LyricsCandidate {
            id: format!("local:{}", path.to_string_lossy()),
            provider_name: self.name().to_string(),
            source_kind: self.source_kind(),
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: if track.album.is_empty() {
                None
            } else {
                Some(track.album.clone())
            },
            duration_ms: Some(track.duration_ms),
            isrc: track.isrc.clone(),
            has_word_sync: is_ttml || is_lf,
            has_line_sync: true,
            instrumental: false,
            format_hint: if is_ttml {
                LyricsFormat::Ttml
            } else if is_lf {
                LyricsFormat::LyricsfileWord
            } else {
                LyricsFormat::StandardLrc
            },
            match_score: if is_ttml {
                195.0
            } else if is_lf {
                190.0
            } else {
                185.0
            },
            match_reason: reason.to_string(),
            payload: CandidatePayload::LocalFile(path),
        }
    }
}

#[async_trait]
impl LyricsProvider for LocalFileLyricsProvider {
    fn name(&self) -> &'static str {
        "Local TTML/LRC Files"
    }

    fn source_kind(&self) -> LyricsSourceKind {
        LyricsSourceKind::LocalFiles
    }

    fn supports_word_sync(&self) -> bool {
        true
    }

    async fn search(&self, track: &TrackMetadata) -> Result<Vec<LyricsCandidate>, LyricsError> {
        let mut found = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // 1. Check sidecar file beside a local audio track first (`song.mp3` -> `.ttml` / `.lyricsfile` / `.lrc`)
        if let Some(audio_path) = &track.local_path {
            for ext in ["ttml", "lyricsfile", "lrc"] {
                let sidecar = audio_path.with_extension(ext);
                if sidecar.is_file() && seen.insert(sidecar.clone()) {
                    found.push(self.make_candidate(
                        track,
                        sidecar,
                        "Sidecar file next to audio track",
                    ));
                    if ext == "ttml" || ext == "lyricsfile" {
                        return Ok(found);
                    }
                }
            }
        }

        let stems = Self::candidate_stems(track);
        let norm_title = normalize_text(&track.title);
        let norm_artist = normalize_text(&track.artist);

        // 2. Prefer `.ttml`, then `.lyricsfile`, then `.lrc` across all search directories
        for ext in ["ttml", "lyricsfile", "lrc"] {
            for dir in &self.search_dirs {
                if !dir.is_dir() {
                    continue;
                }
                for stem in &stems {
                    let direct = dir.join(format!("{stem}.{ext}"));
                    if direct.is_file() && seen.insert(direct.clone()) {
                        found.push(self.make_candidate(
                            track,
                            direct,
                            "Exact local filename / ISRC match",
                        ));
                        return Ok(found);
                    }
                }

                // Fuzzy filename match within directory
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if !path.is_file() {
                            continue;
                        }
                        let file_ext = path
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if file_ext != ext {
                            continue;
                        }
                        let file_stem = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .map(normalize_text)
                            .unwrap_or_default();
                        if !norm_title.is_empty()
                            && file_stem.contains(&norm_title)
                            && (norm_artist.is_empty() || file_stem.contains(&norm_artist))
                            && seen.insert(path.clone())
                        {
                            found.push(self.make_candidate(
                                track,
                                path,
                                "Local directory filename match",
                            ));
                            return Ok(found);
                        }
                    }
                }
            }
        }

        Ok(found)
    }

    async fn fetch(&self, candidate: &LyricsCandidate) -> Result<FetchedLyrics, LyricsError> {
        let CandidatePayload::LocalFile(path) = &candidate.payload else {
            return Err(LyricsError::NotFound);
        };
        let path_owned = path.clone();
        tokio::task::spawn_blocking(move || Self::load_file(&path_owned))
            .await
            .map_err(|e| LyricsError::Io(e.to_string()))?
            .map_err(|e| LyricsError::Parse(e.to_string()))
    }
}

// =============================================================================
// 3. Unified Ordered Multi-Provider Lyrics Service (Word > Line > Plain)
// =============================================================================

#[derive(Clone)]
pub struct LyricsService {
    cache: LyricsCache,
    local_dirs: Vec<PathBuf>,
    binilyrics: BiniLyricsProvider,
    unison: UnisonProvider,
    amll_db: AmllTtmlDbProvider,
    musixmatch: MusixmatchProvider,
    lrclib: LrclibProvider,
    apple_music: AppleMusicLyricsProvider,
    chain_config: LyricsProviderChainConfig,
    extra_providers: Vec<Arc<dyn LyricsProvider>>,
}

impl LyricsService {
    pub fn new(cache: LyricsCache, local_dirs: Vec<PathBuf>) -> Self {
        Self {
            cache,
            local_dirs,
            binilyrics: BiniLyricsProvider::new(),
            unison: UnisonProvider::new(),
            amll_db: AmllTtmlDbProvider::new(),
            musixmatch: MusixmatchProvider::default(),
            lrclib: LrclibProvider::new(),
            apple_music: AppleMusicLyricsProvider::default(),
            chain_config: LyricsProviderChainConfig::default(),
            extra_providers: Vec::new(),
        }
    }

    pub fn with_binilyrics(mut self, binilyrics: BiniLyricsProvider) -> Self {
        self.binilyrics = binilyrics;
        self
    }

    pub fn with_unison(mut self, unison: UnisonProvider) -> Self {
        self.unison = unison;
        self
    }

    pub fn with_amll_db(mut self, amll_db: AmllTtmlDbProvider) -> Self {
        self.amll_db = amll_db;
        self
    }

    pub fn with_musixmatch(mut self, musixmatch: MusixmatchProvider) -> Self {
        self.musixmatch = musixmatch;
        self
    }

    pub fn with_lrclib(mut self, lrclib: LrclibProvider) -> Self {
        self.lrclib = lrclib;
        self
    }

    pub fn with_chain_config(mut self, config: LyricsProviderChainConfig) -> Self {
        self.chain_config = config;
        self
    }

    pub fn add_provider(&mut self, provider: Arc<dyn LyricsProvider>) {
        self.extra_providers.push(provider);
    }

    pub fn update_local_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.local_dirs = dirs;
    }

    pub fn update_chain_config(&mut self, config: LyricsProviderChainConfig) {
        self.chain_config = config;
    }

    pub fn set_musixmatch_api_key(&mut self, api_key: Option<String>) {
        self.musixmatch.set_api_key(api_key);
    }

    pub fn has_musixmatch_api_key(&self) -> bool {
        self.musixmatch.has_api_key()
    }

    pub fn apple_music_provider(&self) -> &AppleMusicLyricsProvider {
        &self.apple_music
    }

    /// Clear ONLY cached lyrics entries in SQLite without disturbing catalog/artwork caches.
    pub fn clear_cache(&self) -> usize {
        self.cache.clear()
    }

    fn online_provider_for(&self, id: OnlineLyricsProviderId) -> Arc<dyn LyricsProvider> {
        match id {
            OnlineLyricsProviderId::BiniLyrics => Arc::new(self.binilyrics.clone()),
            OnlineLyricsProviderId::Unison => Arc::new(self.unison.clone()),
            OnlineLyricsProviderId::AmllTtmlDb => Arc::new(self.amll_db.clone()),
            OnlineLyricsProviderId::Musixmatch => Arc::new(self.musixmatch.clone()),
            OnlineLyricsProviderId::Lrclib => Arc::new(self.lrclib.clone()),
        }
    }

    /// Backwards-compatible wrapper using `Automatic` (when `enable_online == true`) or `LocalOnly`.
    pub async fn resolve(
        &self,
        track: &Track,
        enable_online: bool,
        force_refresh: bool,
    ) -> Result<LyricsDocument, String> {
        let pref = if enable_online {
            LyricsSourcePreference::Automatic
        } else {
            LyricsSourcePreference::LocalOnly
        };
        match self
            .resolve_with_preference(track, pref, enable_online, force_refresh)
            .await
        {
            LyricsResolution::Found(doc) => Ok(doc),
            LyricsResolution::Ambiguous { candidates, .. } => {
                if let Some(first) = candidates.first() {
                    self.resolve_candidate(track, first).await
                } else {
                    Err("No unambiguous lyrics match found.".to_string())
                }
            }
            LyricsResolution::Unavailable(reason) => Err(reason),
        }
    }

    /// Full ordered lyrics resolution respecting `LyricsSourcePreference`, SQLite cache,
    /// strict metadata/variant matching, **Word-level > Line-level > Plain** quality priority,
    /// per-provider error/timeout isolation, and honest source attribution.
    ///
    /// **Anti-spam & efficiency guarantee:**
    /// - Returns immediately from SQLite cache on hit (0 network calls).
    /// - Stops immediately once a Word-level synced match (or local exact `.ttml`) is obtained.
    /// - Skips unconfigured/unavailable providers in 0ms without issuing network requests.
    /// - Only continues after a Line-level or Plain hit if a subsequent available provider can
    ///   deliver a strictly higher timing tier.
    pub async fn resolve_with_preference(
        &self,
        track: &Track,
        preference: LyricsSourcePreference,
        enable_online_fallback: bool,
        force_refresh: bool,
    ) -> LyricsResolution {
        // 1. SQLite cache lookup (only if cached source is permitted by current preference!)
        if !force_refresh
            && preference != LyricsSourcePreference::AppleMusic
            && let Some(cached) = self.cache.get(track)
        {
            match cached {
                Ok(doc) if cached_source_matches_preference(&doc.source, preference) => {
                    return LyricsResolution::Found(doc);
                }
                Err(msg) if preference == LyricsSourcePreference::Automatic => {
                    return LyricsResolution::Unavailable(msg);
                }
                _ => {}
            }
        }

        let meta = TrackMetadata::from(track);
        let query = TrackMatchQuery::from(&meta);
        let is_bundled_sample = track.uri.starts_with("spotirust:sample:");

        // 2. Build ordered provider chain strictly honoring preference & user priority order
        let providers: Vec<Arc<dyn LyricsProvider>> = match preference {
            LyricsSourcePreference::AppleMusic => {
                vec![Arc::new(self.apple_music.clone())]
            }
            LyricsSourcePreference::LocalOnly => {
                vec![
                    Arc::new(BundledShowcaseLyricsProvider),
                    Arc::new(LocalFileLyricsProvider::new(self.local_dirs.clone())),
                ]
            }
            LyricsSourcePreference::BiniLyrics => {
                let mut list: Vec<Arc<dyn LyricsProvider>> =
                    vec![Arc::new(BundledShowcaseLyricsProvider)];
                if !is_bundled_sample {
                    list.push(Arc::new(self.binilyrics.clone()));
                }
                list
            }
            LyricsSourcePreference::Unison => {
                let mut list: Vec<Arc<dyn LyricsProvider>> =
                    vec![Arc::new(BundledShowcaseLyricsProvider)];
                if !is_bundled_sample {
                    list.push(Arc::new(self.unison.clone()));
                }
                list
            }
            LyricsSourcePreference::AmllTtmlDb => {
                let mut list: Vec<Arc<dyn LyricsProvider>> =
                    vec![Arc::new(BundledShowcaseLyricsProvider)];
                if !is_bundled_sample {
                    list.push(Arc::new(self.amll_db.clone()));
                }
                list
            }
            LyricsSourcePreference::Musixmatch => {
                let mut list: Vec<Arc<dyn LyricsProvider>> =
                    vec![Arc::new(BundledShowcaseLyricsProvider)];
                if !is_bundled_sample {
                    list.push(Arc::new(self.musixmatch.clone()));
                }
                list
            }
            LyricsSourcePreference::Lrclib => {
                let mut list: Vec<Arc<dyn LyricsProvider>> =
                    vec![Arc::new(BundledShowcaseLyricsProvider)];
                if !is_bundled_sample {
                    list.push(Arc::new(self.lrclib.clone()));
                }
                list
            }
            LyricsSourcePreference::Automatic => {
                let mut list: Vec<Arc<dyn LyricsProvider>> = vec![
                    Arc::new(BundledShowcaseLyricsProvider),
                    Arc::new(LocalFileLyricsProvider::new(self.local_dirs.clone())),
                ];
                list.extend(self.extra_providers.iter().cloned());
                if enable_online_fallback && !is_bundled_sample {
                    for provider_id in self.chain_config.normalized_order() {
                        if self.chain_config.is_enabled(provider_id) {
                            list.push(self.online_provider_for(provider_id));
                        }
                    }
                }
                list
            }
        };

        let mut best_fallback: Option<(FetchedLyrics, TimingTier)> = None;
        let mut first_ambiguous: Option<(Vec<LyricsCandidate>, String)> = None;
        let mut last_error: Option<String> = None;
        let mut saw_transient_error = false;

        for (idx, provider) in providers.iter().enumerate() {
            // Skip unavailable providers (e.g. unconfigured Musixmatch key) in 0ms;
            // still record reason when the user explicitly targeted that single provider!
            if !provider.is_available() {
                if let Some(reason) = provider.unavailable_reason()
                    && (preference != LyricsSourcePreference::Automatic || last_error.is_none())
                {
                    last_error = Some(reason.to_string());
                }
                continue;
            }

            // If we already have a fallback, skip any remaining provider that cannot beat its timing tier
            if let Some((_, best_tier)) = &best_fallback
                && !provider_can_beat(provider.as_ref(), *best_tier)
            {
                continue;
            }

            match provider.search(&meta).await {
                Ok(candidates) if candidates.is_empty() => continue,
                Ok(mut candidates) => {
                    // Local / Bundled exact hits are already verified by filename / sidecar / ID
                    if matches!(
                        provider.source_kind(),
                        LyricsSourceKind::BundledSample | LyricsSourceKind::LocalFiles
                    ) {
                        if let Some(cand) = candidates.first() {
                            match provider.fetch(cand).await {
                                Ok(fetched) => {
                                    let tier = fetched.document.timing_tier();
                                    // Local files always win immediately if Word-synced or if user chose LocalOnly
                                    if tier == TimingTier::Word
                                        || fetched.document.instrumental
                                        || preference == LyricsSourcePreference::LocalOnly
                                    {
                                        self.cache_fetched(track, &fetched);
                                        return LyricsResolution::Found(fetched.document);
                                    }
                                    update_best_fallback(&mut best_fallback, fetched, tier);
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        provider = provider.name(),
                                        error = %err,
                                        "Failed loading local/bundled lyrics candidate"
                                    );
                                }
                            }
                        }
                    } else {
                        // External providers pass through strict anti-remix/live/remastered matcher
                        let matcher_inputs: Vec<LyricCandidate> = candidates
                            .iter()
                            .map(LyricsCandidate::to_matcher_candidate)
                            .collect();
                        match resolve_candidates(&query, &matcher_inputs) {
                            MatchResolution::Selected(winner) => {
                                if let Some(cand) = candidates.get_mut(winner.index) {
                                    cand.match_score = winner.score;
                                    cand.match_reason = winner.reason;
                                    match provider.fetch(cand).await {
                                        Ok(fetched) => {
                                            let tier = fetched.document.timing_tier();
                                            // Word-level synced or confirmed Instrumental -> STOP IMMEDIATELY!
                                            if tier == TimingTier::Word
                                                || fetched.document.instrumental
                                            {
                                                self.cache_fetched(track, &fetched);
                                                return LyricsResolution::Found(fetched.document);
                                            }
                                            update_best_fallback(&mut best_fallback, fetched, tier);
                                        }
                                        Err(LyricsError::Timeout) => {
                                            saw_transient_error = true;
                                            tracing::warn!(
                                                provider = provider.name(),
                                                "Lyrics fetch timed out; falling back"
                                            );
                                            last_error = Some(format!(
                                                "{} timed out while fetching lyrics.",
                                                provider.name()
                                            ));
                                        }
                                        Err(LyricsError::RateLimited { retry_after_secs }) => {
                                            saw_transient_error = true;
                                            tracing::warn!(
                                                provider = provider.name(),
                                                retry_after_secs,
                                                "Lyrics fetch rate-limited; falling back"
                                            );
                                            last_error = Some(format!(
                                                "{} rate-limited; retry after {}s.",
                                                provider.name(),
                                                retry_after_secs
                                            ));
                                        }
                                        Err(err) => {
                                            tracing::warn!(
                                                provider = provider.name(),
                                                error = %err,
                                                "Lyrics fetch failed; falling back"
                                            );
                                            last_error = Some(err.to_string());
                                        }
                                    }
                                }
                            }
                            MatchResolution::Ambiguous(scored_list) => {
                                if first_ambiguous.is_none() {
                                    let mut choices = Vec::with_capacity(scored_list.len());
                                    for sm in scored_list {
                                        if let Some(mut c) = candidates.get(sm.index).cloned() {
                                            c.match_score = sm.score;
                                            c.match_reason = sm.reason;
                                            choices.push(c);
                                        }
                                    }
                                    if !choices.is_empty() {
                                        first_ambiguous =
                                            Some((choices, provider.name().to_string()));
                                    }
                                }
                            }
                            MatchResolution::NoMatch => continue,
                        }
                    }

                    // Early-exit check: if we already have a fallback AND no remaining available
                    // provider can deliver a higher timing tier, stop immediately!
                    if let Some((_, best_tier)) = &best_fallback {
                        let any_remaining_better = providers[idx + 1..]
                            .iter()
                            .any(|p| p.is_available() && provider_can_beat(p.as_ref(), *best_tier));
                        if !any_remaining_better {
                            let (best_fetched, _) = best_fallback.take().unwrap();
                            self.cache_fetched(track, &best_fetched);
                            return LyricsResolution::Found(best_fetched.document);
                        }
                    }
                }
                Err(LyricsError::UpstreamUnsupported(reason)) => {
                    last_error = Some(reason);
                }
                Err(LyricsError::AuthRequired(reason)) => {
                    tracing::info!(
                        provider = provider.name(),
                        "Lyrics provider requires API key"
                    );
                    last_error = Some(reason);
                }
                Err(LyricsError::RateLimited { retry_after_secs }) => {
                    saw_transient_error = true;
                    tracing::warn!(
                        provider = provider.name(),
                        retry_after_secs,
                        "Lyrics provider rate-limited; falling back"
                    );
                    last_error = Some(format!(
                        "{} rate-limited; retry after {}s.",
                        provider.name(),
                        retry_after_secs
                    ));
                }
                Err(LyricsError::Timeout) => {
                    saw_transient_error = true;
                    tracing::warn!(
                        provider = provider.name(),
                        "Lyrics provider timed out; falling back"
                    );
                    last_error = Some(format!(
                        "{} timed out while searching lyrics.",
                        provider.name()
                    ));
                }
                Err(LyricsError::NotFound) => continue,
                Err(err) => {
                    if matches!(err, LyricsError::Network(_)) {
                        saw_transient_error = true;
                    }
                    tracing::warn!(
                        provider = provider.name(),
                        error = %err,
                        "Lyrics provider failed; falling back"
                    );
                    last_error = Some(err.to_string());
                }
            }
        }

        // If we captured a valid Line-level or Plain fallback while searching for Word-level,
        // return it cleanly and cache it in SQLite!
        if let Some((best_fetched, _)) = best_fallback {
            self.cache_fetched(track, &best_fetched);
            return LyricsResolution::Found(best_fetched.document);
        }

        if let Some((candidates, provider_name)) = first_ambiguous {
            return LyricsResolution::Ambiguous {
                candidates,
                provider_name,
            };
        }

        // Negative-cache clean misses in Automatic mode ONLY when no transient network/rate-limit error occurred
        if preference == LyricsSourcePreference::Automatic && !saw_transient_error {
            self.cache
                .put_raw(track, LyricsFormat::PlainText, "none", None, 0, false, None);
        }

        let fallback_msg = last_error.unwrap_or_else(|| match preference {
            LyricsSourcePreference::LocalOnly => {
                "No local .ttml, .lyricsfile, or .lrc file found for this track. Click Import to attach one."
                    .to_string()
            }
            LyricsSourcePreference::BiniLyrics => {
                "No matching lyrics found on BiniLyrics (lyrics-api.binimum.org).".to_string()
            }
            LyricsSourcePreference::Unison => {
                "No matching lyrics found on Unison (unison.betterlyrics.org).".to_string()
            }
            LyricsSourcePreference::AmllTtmlDb => {
                "No matching Spotify ID entry found in AMLL TTML DB (CC0-1.0).".to_string()
            }
            LyricsSourcePreference::Musixmatch => {
                "No matching lyrics found on Musixmatch Official API.".to_string()
            }
            LyricsSourcePreference::Lrclib => {
                "No matching lyrics found on LRCLIB (lrclib.net).".to_string()
            }
            _ => {
                "No lyrics available from enabled providers yet. You can import a local .ttml or .lrc file."
                    .to_string()
            }
        });
        LyricsResolution::Unavailable(fallback_msg)
    }

    /// Materialize and cache a user-selected `LyricsCandidate` (from the Ambiguous match picker).
    pub async fn resolve_candidate(
        &self,
        track: &Track,
        candidate: &LyricsCandidate,
    ) -> Result<LyricsDocument, String> {
        let fetched = match candidate.source_kind {
            LyricsSourceKind::BundledSample => BundledShowcaseLyricsProvider
                .fetch(candidate)
                .await
                .map_err(|e| e.to_string())?,
            LyricsSourceKind::LocalFiles => LocalFileLyricsProvider::new(self.local_dirs.clone())
                .fetch(candidate)
                .await
                .map_err(|e| e.to_string())?,
            LyricsSourceKind::BiniLyrics => self
                .binilyrics
                .fetch(candidate)
                .await
                .map_err(|e| e.to_string())?,
            LyricsSourceKind::Unison => self
                .unison
                .fetch(candidate)
                .await
                .map_err(|e| e.to_string())?,
            LyricsSourceKind::AmllTtmlDb => self
                .amll_db
                .fetch(candidate)
                .await
                .map_err(|e| e.to_string())?,
            LyricsSourceKind::Musixmatch => self
                .musixmatch
                .fetch(candidate)
                .await
                .map_err(|e| e.to_string())?,
            LyricsSourceKind::Lrclib => self
                .lrclib
                .fetch(candidate)
                .await
                .map_err(|e| e.to_string())?,
            LyricsSourceKind::AppleMusic => self
                .apple_music
                .fetch(candidate)
                .await
                .map_err(|e| e.to_string())?,
            LyricsSourceKind::Custom => {
                let mut fetched_opt = None;
                for p in &self.extra_providers {
                    if p.name() == candidate.provider_name {
                        fetched_opt = Some(p.fetch(candidate).await.map_err(|e| e.to_string())?);
                        break;
                    }
                }
                fetched_opt
                    .ok_or_else(|| "Custom lyrics provider no longer registered".to_string())?
            }
        };

        self.cache_fetched(track, &fetched);
        Ok(fetched.document)
    }

    /// Import a user-supplied `.ttml`, `.lyricsfile`, or `.lrc` file for `track` and persist it in SQLite cache.
    pub fn import_for_track(&self, track: &Track, path: &Path) -> anyhow::Result<LyricsDocument> {
        let fetched = LocalFileLyricsProvider::load_file(path)?;
        self.cache_fetched(track, &fetched);
        Ok(fetched.document)
    }

    pub fn save_offset(&self, track: &Track, offset_ms: i64) {
        self.cache.update_offset(track, offset_ms);
    }

    fn cache_fetched(&self, track: &Track, fetched: &FetchedLyrics) {
        self.cache.put_raw(
            track,
            fetched.document.format,
            &fetched.document.source,
            fetched.raw_content.clone(),
            fetched.document.offset_ms,
            fetched.document.instrumental,
            fetched.document.attribution.as_deref(),
        );
    }
}

fn provider_can_beat(provider: &dyn LyricsProvider, current_best: TimingTier) -> bool {
    match current_best {
        TimingTier::Word => false,
        TimingTier::Line => provider.supports_word_sync(),
        TimingTier::Plain | TimingTier::None => {
            provider.supports_word_sync() || provider.supports_line_sync()
        }
    }
}

fn update_best_fallback(
    best: &mut Option<(FetchedLyrics, TimingTier)>,
    fetched: FetchedLyrics,
    tier: TimingTier,
) {
    let should_replace = match best {
        None => true,
        Some((_, cur_tier)) => tier > *cur_tier,
    };
    if should_replace {
        *best = Some((fetched, tier));
    }
}

fn cached_source_matches_preference(source: &str, pref: LyricsSourcePreference) -> bool {
    let lower = source.to_ascii_lowercase();
    match pref {
        LyricsSourcePreference::Automatic => true,
        LyricsSourcePreference::AppleMusic => false,
        LyricsSourcePreference::LocalOnly => {
            lower.starts_with("local file") || lower.starts_with("bundled sample")
        }
        LyricsSourcePreference::BiniLyrics => {
            lower.starts_with("binilyrics") || lower.starts_with("bundled sample")
        }
        LyricsSourcePreference::Unison => {
            lower.starts_with("unison") || lower.starts_with("bundled sample")
        }
        LyricsSourcePreference::AmllTtmlDb => {
            lower.starts_with("amll") || lower.starts_with("bundled sample")
        }
        LyricsSourcePreference::Musixmatch => {
            lower.starts_with("musixmatch") || lower.starts_with("bundled sample")
        }
        LyricsSourcePreference::Lrclib => {
            lower.starts_with("lrclib") || lower.starts_with("bundled sample")
        }
    }
}
