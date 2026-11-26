//! Real Official Musixmatch Developer API Provider (`https://api.musixmatch.com/ws/1.1/`).
//!
//! ## License, Authentication & Compliance Verification (Nov 2026)
//! - Official API Documentation: `https://developer.musixmatch.com/`
//! - Requires an official Musixmatch Developer API key (`apikey` query parameter).
//!   Spotirust NEVER embeds a private/leaked key and NEVER calls undocumented desktop/mobile
//!   `usertoken` endpoints.
//! - API key resolution:
//!   1. Maintainer build-time (`SPOTIRUST_MUSIXMATCH_API_KEY`) or runtime env var
//!   2. Optional user-provided key stored strictly in the OS credential vault (never SQLite)
//! - When no key is configured, `is_available()` returns `false` and `search()` returns
//!   `LyricsError::AuthRequired` immediately with zero network latency so the fallback chain moves on.
//! - Strict Fetch Priority (Word -> Line -> Plain):
//!   1. `track.richsync.get` -> Word/syllable-level JSON (`LyricsFormat::MusixmatchRichSync`, Pro/Commercial plan)
//!   2. `track.subtitle.get?subtitle_format=lrc` -> Line-level LRC (Plus/Commercial plan)
//!   3. `track.lyrics.get` -> Plain text (Free tier returns a 30% non-commercial preview, explicitly
//!      detected and labeled honestly — NEVER faked with synthetic timestamps!)
//! - Preserves required `lyrics_copyright` attribution and never logs `apikey`.

use crate::lyrics::lrc::{parse_lrc, parse_plain_lyrics};
use crate::lyrics::model::{LyricLine, LyricWord, LyricsAttribution, LyricsDocument, LyricsFormat};
use crate::lyrics::provider::{
    CandidatePayload, FetchedLyrics, LyricsCandidate, LyricsError, LyricsProvider,
    LyricsSourceKind, TrackMetadata,
};
use crate::lyrics::unison::extract_spotify_track_id;
use async_trait::async_trait;
use serde::Deserialize;
use std::fmt;
use std::time::Duration;
use thiserror::Error;

const MUSIXMATCH_DEFAULT_BASE_URL: &str = "https://api.musixmatch.com/ws/1.1";
const MUSIXMATCH_DEFAULT_TIMEOUT: Duration = Duration::from_secs(6);
const FREE_TIER_TRUNCATION_MARKER: &str = "This Lyrics is NOT for Commercial use";

#[derive(Debug, Error)]
pub enum RichSyncParseError {
    #[error("invalid Musixmatch RichSync JSON: {0}")]
    Json(String),
    #[error("Musixmatch RichSync payload contains no lyric lines")]
    Empty,
}

#[derive(Debug, Deserialize)]
struct RichSyncLine {
    ts: f64,
    te: f64,
    #[serde(default)]
    x: String,
    #[serde(default)]
    l: Vec<RichSyncToken>,
}

#[derive(Debug, Deserialize)]
struct RichSyncToken {
    c: String,
    #[serde(default)]
    o: f64,
}

/// Parse an official Musixmatch `track.richsync.get` `richsync_body` JSON string into a
/// word-level `LyricsDocument` (`LyricsFormat::MusixmatchRichSync`).
///
/// Format per `https://developer.musixmatch.com/documentation/api-reference/track-richsync-get`:
/// `[{"ts": 10.67, "te": 15.61, "l": [{"c": "Hello", "o": 0}, {"c": " ", "o": 0.4}, {"c": "world", "o": 0.5}], "x": "Hello world"}]`
pub fn parse_musixmatch_richsync(
    raw_json: &str,
    source: impl Into<String>,
) -> Result<LyricsDocument, RichSyncParseError> {
    let entries: Vec<RichSyncLine> = serde_json::from_str(raw_json.trim())
        .map_err(|e| RichSyncParseError::Json(e.to_string()))?;

    let mut lines: Vec<LyricLine> = Vec::new();
    let mut saw_word_timing = false;

    for entry in entries {
        let line_start_ms = (entry.ts.max(0.0) * 1000.0).round() as u64;
        let line_end_ms = (entry.te.max(entry.ts) * 1000.0).round() as u64;

        let mut words: Vec<LyricWord> = Vec::new();
        for tok in &entry.l {
            let tok_start_ms = ((entry.ts + tok.o).max(0.0) * 1000.0).round() as u64;
            if tok.c.trim().is_empty() {
                if let Some(prev) = words.last_mut() {
                    prev.trailing_space = true;
                }
                continue;
            }
            let starts_ws = tok.c.starts_with(char::is_whitespace);
            let ends_ws = tok.c.ends_with(char::is_whitespace);
            if starts_ws && let Some(prev) = words.last_mut() {
                prev.trailing_space = true;
            }
            let cleaned = tok.c.trim();
            if cleaned.is_empty() {
                continue;
            }
            words.push(LyricWord {
                text: cleaned.to_string(),
                start_ms: tok_start_ms,
                end_ms: tok_start_ms,
                trailing_space: ends_ws,
            });
        }

        if let Some(last) = words.last_mut() {
            last.trailing_space = false;
        }

        if words.is_empty() {
            let fallback_text = entry.x.trim();
            if fallback_text.is_empty() {
                continue;
            }
            words.push(LyricWord {
                text: fallback_text.to_string(),
                start_ms: line_start_ms,
                end_ms: line_end_ms.max(line_start_ms + 250),
                trailing_space: false,
            });
        } else {
            if words.len() > 1 {
                saw_word_timing = true;
            }
            let count = words.len();
            for i in 0..count {
                let cur = words[i].start_ms;
                let next_start = words[i + 1..]
                    .iter()
                    .find(|w| w.start_ms > cur)
                    .map(|w| w.start_ms);
                words[i].end_ms = next_start.unwrap_or_else(|| line_end_ms.max(cur + 200));
            }
        }

        lines.push(LyricLine {
            words,
            start_ms: line_start_ms,
            end_ms: line_end_ms.max(line_start_ms + 250),
            section: None,
            agent: None,
            key: None,
            translation: None,
            romanization: None,
            background_words: Vec::new(),
        });
    }

    if lines.is_empty() {
        return Err(RichSyncParseError::Empty);
    }

    lines.sort_by_key(|l| l.start_ms);

    Ok(LyricsDocument {
        lines,
        offset_ms: 0,
        format: if saw_word_timing {
            LyricsFormat::MusixmatchRichSync
        } else {
            LyricsFormat::StandardLrc
        },
        source: source.into(),
        instrumental: false,
        title: None,
        artist: None,
        album: None,
        isrc: None,
        attribution: None,
    })
}

#[derive(Debug, Deserialize)]
struct MxmEnvelope {
    message: MxmMessage,
}

#[derive(Debug, Deserialize)]
struct MxmMessage {
    header: MxmHeader,
    #[serde(default)]
    body: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct MxmHeader {
    status_code: u16,
    #[serde(default)]
    hint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct MxmTrack {
    #[serde(default)]
    commontrack_id: u64,
    #[serde(default)]
    track_id: u64,
    #[serde(default)]
    track_name: String,
    #[serde(default)]
    artist_name: String,
    #[serde(default)]
    album_name: Option<String>,
    #[serde(default)]
    track_length: Option<u64>,
    #[serde(default)]
    track_isrc: Option<String>,
    #[serde(default)]
    has_lyrics: u8,
    #[serde(default)]
    has_subtitles: u8,
    #[serde(default)]
    has_richsync: u8,
    #[serde(default)]
    instrumental: u8,
    #[serde(default)]
    track_share_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MxmTrackWrapper {
    track: MxmTrack,
}

#[derive(Debug, Deserialize)]
struct MxmTrackListBody {
    #[serde(default)]
    track_list: Vec<MxmTrackWrapper>,
}

#[derive(Debug, Deserialize)]
struct MxmRichSyncBody {
    richsync: MxmRichSyncData,
}

#[derive(Debug, Deserialize)]
struct MxmRichSyncData {
    #[serde(default)]
    richsync_body: String,
    #[serde(default)]
    lyrics_copyright: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MxmSubtitleBody {
    subtitle: MxmSubtitleData,
}

#[derive(Debug, Deserialize)]
struct MxmSubtitleData {
    #[serde(default)]
    subtitle_body: String,
    #[serde(default)]
    lyrics_copyright: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MxmLyricsBody {
    lyrics: MxmLyricsData,
}

#[derive(Debug, Deserialize)]
struct MxmLyricsData {
    #[serde(default)]
    lyrics_body: String,
    #[serde(default)]
    lyrics_copyright: Option<String>,
    #[serde(default)]
    instrumental: u8,
}

/// Official Musixmatch Developer API client (redacts `apikey` in all `Debug` output).
#[derive(Clone)]
pub struct MusixmatchProvider {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

impl fmt::Debug for MusixmatchProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MusixmatchProvider")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl Default for MusixmatchProvider {
    fn default() -> Self {
        Self::new(resolve_env_musixmatch_key())
    }
}

impl MusixmatchProvider {
    pub fn new(api_key: Option<String>) -> Self {
        Self::with_timeout(api_key, MUSIXMATCH_DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(api_key: Option<String>, timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "Spotirust/{} (https://github.com/CgTuan210/Spotirust)",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: MUSIXMATCH_DEFAULT_BASE_URL.to_string(),
            api_key: sanitize_api_key(api_key),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn set_api_key(&mut self, api_key: Option<String>) {
        self.api_key = sanitize_api_key(api_key);
    }

    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    fn require_api_key(&self) -> Result<&str, LyricsError> {
        self.api_key.as_deref().ok_or_else(|| {
            LyricsError::AuthRequired(
                "Musixmatch official API requires a Developer API key (SPOTIRUST_MUSIXMATCH_API_KEY or Settings)."
                    .to_string(),
            )
        })
    }

    async fn call_api(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<(u16, serde_json::Value, Option<String>), LyricsError> {
        let api_key = self.require_api_key()?;
        let mut query: Vec<(&str, String)> = Vec::with_capacity(params.len() + 1);
        query.push(("apikey", api_key.to_string()));
        query.extend_from_slice(params);

        let url = format!("{}/{method}", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&query)
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    LyricsError::Timeout
                } else {
                    LyricsError::Network(err.to_string())
                }
            })?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(LyricsError::RateLimited {
                retry_after_secs: 60,
            });
        }
        if !resp.status().is_success() {
            return Err(LyricsError::Network(format!(
                "Musixmatch HTTP {}",
                resp.status()
            )));
        }

        let env: MxmEnvelope = resp
            .json()
            .await
            .map_err(|e| LyricsError::Parse(e.to_string()))?;

        match env.message.header.status_code {
            200 => Ok((200, env.message.body, env.message.header.hint)),
            401 => Err(LyricsError::AuthRequired(
                "Musixmatch API rejected the configured API key (status 401).".to_string(),
            )),
            402 => Err(LyricsError::RateLimited {
                retry_after_secs: 3600,
            }),
            403 => Ok((403, env.message.body, env.message.header.hint)),
            404 => Ok((404, env.message.body, env.message.header.hint)),
            code => Err(LyricsError::Network(format!(
                "Musixmatch API status {code}"
            ))),
        }
    }

    fn track_to_candidate(t: MxmTrack) -> Option<LyricsCandidate> {
        if t.commontrack_id == 0 && t.track_id == 0 {
            return None;
        }
        let has_any =
            t.has_richsync > 0 || t.has_subtitles > 0 || t.has_lyrics > 0 || t.instrumental > 0;
        if !has_any {
            return None;
        }

        let ext_id = if t.commontrack_id > 0 {
            format!("commontrack:{}", t.commontrack_id)
        } else {
            format!("track:{}", t.track_id)
        };
        let dur_ms = t.track_length.filter(|d| *d > 0).map(|s| s * 1000);
        let fmt_hint = if t.has_richsync > 0 {
            LyricsFormat::MusixmatchRichSync
        } else if t.has_subtitles > 0 {
            LyricsFormat::StandardLrc
        } else {
            LyricsFormat::PlainText
        };

        let _ = &t.track_share_url;
        Some(LyricsCandidate {
            id: format!("musixmatch:{ext_id}"),
            provider_name: "Musixmatch (Official API)".to_string(),
            source_kind: LyricsSourceKind::Musixmatch,
            title: t.track_name,
            artist: t.artist_name,
            album: t.album_name.filter(|s| !s.trim().is_empty()),
            duration_ms: dur_ms,
            isrc: t.track_isrc.filter(|s| !s.trim().is_empty()),
            has_word_sync: t.has_richsync > 0,
            has_line_sync: t.has_richsync > 0 || t.has_subtitles > 0,
            instrumental: t.instrumental > 0,
            format_hint: fmt_hint,
            match_score: 0.0,
            match_reason: String::new(),
            payload: if t.instrumental > 0 {
                CandidatePayload::Instrumental {
                    source_label: "Musixmatch (Instrumental)".to_string(),
                }
            } else {
                CandidatePayload::ExternalId(ext_id)
            },
        })
    }

    fn id_param(ext_id: &str) -> (&'static str, String) {
        if let Some(id) = ext_id.strip_prefix("commontrack:") {
            ("commontrack_id", id.to_string())
        } else if let Some(id) = ext_id.strip_prefix("track:") {
            ("track_id", id.to_string())
        } else {
            ("commontrack_id", ext_id.to_string())
        }
    }

    fn make_attribution(
        copyright: Option<String>,
        disclaimer: Option<String>,
    ) -> Box<LyricsAttribution> {
        Box::new(LyricsAttribution {
            provider_label: "Lyrics powered by Musixmatch".to_string(),
            source_url: Some("https://www.musixmatch.com".to_string()),
            submitter_name: None,
            submitter_url: None,
            copyright: copyright.filter(|s| !s.trim().is_empty()),
            disclaimer,
        })
    }
}

#[async_trait]
impl LyricsProvider for MusixmatchProvider {
    fn name(&self) -> &'static str {
        "Musixmatch (Official API)"
    }

    fn source_kind(&self) -> LyricsSourceKind {
        LyricsSourceKind::Musixmatch
    }

    fn is_available(&self) -> bool {
        self.api_key.is_some()
    }

    fn unavailable_reason(&self) -> Option<&'static str> {
        if self.api_key.is_some() {
            None
        } else {
            Some(
                "Requires an official Musixmatch Developer API key (SPOTIRUST_MUSIXMATCH_API_KEY or Settings).",
            )
        }
    }

    fn supports_word_sync(&self) -> bool {
        true
    }

    async fn search(&self, track: &TrackMetadata) -> Result<Vec<LyricsCandidate>, LyricsError> {
        let _ = self.require_api_key()?;
        if track.title.trim().is_empty() && track.isrc.is_none() {
            return Ok(Vec::new());
        }

        // 1. Official matcher endpoint (`matcher.track.get`) supporting ISRC, Spotify ID, and metadata
        let mut match_params: Vec<(&str, String)> = Vec::new();
        if let Some(isrc) = track
            .isrc
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match_params.push(("track_isrc", isrc.to_string()));
        }
        if let Some(sp_id) = extract_spotify_track_id(track) {
            match_params.push(("track_spotify_id", sp_id.to_string()));
        }
        if !track.title.trim().is_empty() {
            match_params.push(("q_track", track.title.clone()));
        }
        if !track.artist.trim().is_empty() {
            match_params.push(("q_artist", track.artist.clone()));
        }
        if !track.album.trim().is_empty() {
            match_params.push(("q_album", track.album.clone()));
        }
        match_params.push(("f_has_lyrics", "1".to_string()));

        let (code, body, _) = self.call_api("matcher.track.get", &match_params).await?;
        if code == 200
            && let Ok(wrapper) = serde_json::from_value::<MxmTrackWrapper>(body)
            && let Some(cand) = Self::track_to_candidate(wrapper.track)
        {
            return Ok(vec![cand]);
        }

        // 2. Structured catalog search (`track.search`) ranked strictly by Spotirust's matcher.rs
        if track.title.trim().is_empty() {
            return Ok(Vec::new());
        }
        let mut search_params = vec![
            ("q_track", track.title.clone()),
            ("f_has_lyrics", "1".to_string()),
            ("page_size", "10".to_string()),
            ("s_track_rating", "desc".to_string()),
        ];
        if !track.artist.trim().is_empty() {
            search_params.push(("q_artist", track.artist.clone()));
        }

        let (code, body, _) = self.call_api("track.search", &search_params).await?;
        if code != 200 {
            return Ok(Vec::new());
        }

        let list: MxmTrackListBody =
            serde_json::from_value(body).map_err(|e| LyricsError::Parse(e.to_string()))?;
        let candidates = list
            .track_list
            .into_iter()
            .filter_map(|w| Self::track_to_candidate(w.track))
            .collect();
        Ok(candidates)
    }

    async fn fetch(&self, candidate: &LyricsCandidate) -> Result<FetchedLyrics, LyricsError> {
        let _ = self.require_api_key()?;
        match &candidate.payload {
            CandidatePayload::Instrumental { source_label } => Ok(FetchedLyrics {
                document: LyricsDocument {
                    lines: Vec::new(),
                    offset_ms: 0,
                    format: LyricsFormat::PlainText,
                    source: source_label.clone(),
                    instrumental: true,
                    title: Some(candidate.title.clone()),
                    artist: Some(candidate.artist.clone()),
                    album: candidate.album.clone(),
                    isrc: candidate.isrc.clone(),
                    attribution: Some(Self::make_attribution(None, None)),
                },
                raw_content: None,
            }),
            CandidatePayload::ExternalId(ext_id) => {
                let id_param = Self::id_param(ext_id);

                // Tier 1: Word-level RichSync (`track.richsync.get`)
                if candidate.has_word_sync {
                    let (code, body, _) = self
                        .call_api("track.richsync.get", std::slice::from_ref(&id_param))
                        .await?;
                    if code == 200
                        && let Ok(rs) = serde_json::from_value::<MxmRichSyncBody>(body)
                        && !rs.richsync.richsync_body.trim().is_empty()
                        && let Ok(mut doc) = parse_musixmatch_richsync(
                            &rs.richsync.richsync_body,
                            "Musixmatch (RichSync)",
                        )
                    {
                        doc.title = Some(candidate.title.clone());
                        doc.artist = Some(candidate.artist.clone());
                        doc.album = candidate.album.clone();
                        doc.isrc = candidate.isrc.clone();
                        doc.attribution =
                            Some(Self::make_attribution(rs.richsync.lyrics_copyright, None));
                        return Ok(FetchedLyrics {
                            document: doc,
                            raw_content: Some(rs.richsync.richsync_body),
                        });
                    }
                    // 403 (plan lacks RichSync) or 404 -> gracefully fall through to Subtitle
                }

                // Tier 2: Line-level Subtitle LRC (`track.subtitle.get?subtitle_format=lrc`)
                if candidate.has_line_sync {
                    let sub_params = [id_param.clone(), ("subtitle_format", "lrc".to_string())];
                    let (code, body, _) = self.call_api("track.subtitle.get", &sub_params).await?;
                    if code == 200
                        && let Ok(sub) = serde_json::from_value::<MxmSubtitleBody>(body)
                        && !sub.subtitle.subtitle_body.trim().is_empty()
                        && let Ok(mut doc) =
                            parse_lrc(&sub.subtitle.subtitle_body, "Musixmatch (Subtitle)")
                    {
                        doc.title = Some(candidate.title.clone());
                        doc.artist = Some(candidate.artist.clone());
                        doc.album = candidate.album.clone();
                        doc.isrc = candidate.isrc.clone();
                        doc.attribution =
                            Some(Self::make_attribution(sub.subtitle.lyrics_copyright, None));
                        return Ok(FetchedLyrics {
                            document: doc,
                            raw_content: Some(sub.subtitle.subtitle_body),
                        });
                    }
                    // 403 (plan lacks Subtitles) or 404 -> gracefully fall through to Plain lyrics
                }

                // Tier 3: Plain lyrics (`track.lyrics.get`) — honestly detects 30% free-tier preview!
                let (code, body, _) = self
                    .call_api("track.lyrics.get", std::slice::from_ref(&id_param))
                    .await?;
                if code == 403 {
                    return Err(LyricsError::UpstreamUnsupported(
                        "Musixmatch API plan does not allow lyrics access for this track (403)."
                            .to_string(),
                    ));
                }
                if code != 200 {
                    return Err(LyricsError::NotFound);
                }

                let lyr: MxmLyricsBody =
                    serde_json::from_value(body).map_err(|e| LyricsError::Parse(e.to_string()))?;
                if lyr.lyrics.instrumental > 0 {
                    return Ok(FetchedLyrics {
                        document: LyricsDocument {
                            lines: Vec::new(),
                            offset_ms: 0,
                            format: LyricsFormat::PlainText,
                            source: "Musixmatch (Instrumental)".to_string(),
                            instrumental: true,
                            title: Some(candidate.title.clone()),
                            artist: Some(candidate.artist.clone()),
                            album: candidate.album.clone(),
                            isrc: candidate.isrc.clone(),
                            attribution: Some(Self::make_attribution(
                                lyr.lyrics.lyrics_copyright,
                                None,
                            )),
                        },
                        raw_content: None,
                    });
                }

                let raw_plain = lyr.lyrics.lyrics_body;
                let is_free_preview = raw_plain.contains(FREE_TIER_TRUNCATION_MARKER);
                let cleaned_plain = strip_musixmatch_free_banner(&raw_plain);
                if cleaned_plain.trim().is_empty() {
                    return Err(LyricsError::NotFound);
                }

                let (source_label, disclaimer) = if is_free_preview {
                    (
                        "Musixmatch (Plain · 30% Preview)",
                        Some(
                            "Musixmatch Free Developer API returns a 30% non-commercial lyrics preview without timestamps."
                                .to_string(),
                        ),
                    )
                } else {
                    ("Musixmatch (Plain)", None)
                };

                let mut doc = parse_plain_lyrics(&cleaned_plain, source_label);
                doc.title = Some(candidate.title.clone());
                doc.artist = Some(candidate.artist.clone());
                doc.album = candidate.album.clone();
                doc.isrc = candidate.isrc.clone();
                doc.attribution = Some(Self::make_attribution(
                    lyr.lyrics.lyrics_copyright,
                    disclaimer,
                ));

                Ok(FetchedLyrics {
                    document: doc,
                    raw_content: Some(cleaned_plain),
                })
            }
            _ => Err(LyricsError::NotFound),
        }
    }
}

fn strip_musixmatch_free_banner(raw: &str) -> String {
    raw.lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.contains(FREE_TIER_TRUNCATION_MARKER)
                && !trimmed.starts_with("*******")
                && !(trimmed.starts_with('(')
                    && trimmed.ends_with(')')
                    && trimmed[1..trimmed.len() - 1]
                        .chars()
                        .all(|c| c.is_ascii_digit()))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

pub fn resolve_env_musixmatch_key() -> Option<String> {
    if let Some(compiled) = option_env!("SPOTIRUST_MUSIXMATCH_API_KEY") {
        let trimmed = compiled.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    sanitize_api_key(std::env::var("SPOTIRUST_MUSIXMATCH_API_KEY").ok())
}

fn sanitize_api_key(key: Option<String>) -> Option<String> {
    key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_official_musixmatch_richsync_word_timestamps() {
        let raw = r#"[
            {
                "ts": 10.50,
                "te": 13.80,
                "x": "Pure sound waves",
                "l": [
                    {"c": "Pure", "o": 0.0},
                    {"c": " ", "o": 0.55},
                    {"c": "sound", "o": 0.60},
                    {"c": " ", "o": 1.15},
                    {"c": "waves", "o": 1.25}
                ]
            },
            {
                "ts": 14.00,
                "te": 16.50,
                "x": "Gliding through",
                "l": [
                    {"c": "Glid", "o": 0.0},
                    {"c": "ing", "o": 0.40},
                    {"c": " ", "o": 0.90},
                    {"c": "through", "o": 1.00}
                ]
            }
        ]"#;

        let doc = parse_musixmatch_richsync(raw, "Musixmatch (RichSync)").unwrap();
        assert_eq!(doc.format, LyricsFormat::MusixmatchRichSync);
        assert!(doc.has_word_timing());
        assert_eq!(doc.lines.len(), 2);

        let l0 = &doc.lines[0];
        assert_eq!(l0.plain_text(), "Pure sound waves");
        assert_eq!(l0.words.len(), 3);
        assert_eq!((l0.words[0].start_ms, l0.words[0].end_ms), (10_500, 11_100));
        assert_eq!((l0.words[1].start_ms, l0.words[1].end_ms), (11_100, 11_750));
        assert_eq!((l0.words[2].start_ms, l0.words[2].end_ms), (11_750, 13_800));

        // Syllable joining (`Glid` + `ing` without space, followed by space + `through`)
        let l1 = &doc.lines[1];
        assert_eq!(l1.plain_text(), "Gliding through");
        assert!(!l1.words[0].trailing_space);
        assert!(l1.words[1].trailing_space);
    }

    #[test]
    fn redacts_api_key_in_debug_and_skips_when_missing() {
        let provider = MusixmatchProvider::new(Some("super-secret-mxm-key".to_string()));
        let dbg = format!("{provider:?}");
        assert!(!dbg.contains("super-secret-mxm-key"));
        assert!(dbg.contains("[REDACTED]"));

        let unconfigured = MusixmatchProvider::new(None);
        assert!(!unconfigured.is_available());
    }
}
