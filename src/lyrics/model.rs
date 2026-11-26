//! Canonical normalized lyrics data model (`LyricWord`, `LyricLine`, `LyricsDocument`)
//! shared by all parsers (TTML, Enhanced LRC, Standard LRC), caches, and the karaoke renderer.

use serde::{Deserialize, Serialize};

/// Format of the parsed lyrics source, ranked by timing fidelity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsFormat {
    #[default]
    PlainText,
    StandardLrc,
    LyricsfileLine,
    TtmlLine,
    EnhancedLrc,
    LyricsfileWord,
    MusixmatchRichSync,
    Ttml,
}

/// Coarse timing precision tier used for provider quality selection & UI badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TimingTier {
    None = 0,
    Plain = 1,
    Line = 2,
    Word = 3,
}

impl LyricsFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ttml => "ttml",
            Self::TtmlLine => "ttml_line",
            Self::MusixmatchRichSync => "musixmatch_richsync",
            Self::LyricsfileWord => "lyricsfile_word",
            Self::LyricsfileLine => "lyricsfile_line",
            Self::EnhancedLrc => "enhanced_lrc",
            Self::StandardLrc => "standard_lrc",
            Self::PlainText => "plain_text",
        }
    }

    pub fn from_db_str(s: &str) -> Self {
        match s {
            "ttml" => Self::Ttml,
            "ttml_line" => Self::TtmlLine,
            "musixmatch_richsync" => Self::MusixmatchRichSync,
            "lyricsfile_word" => Self::LyricsfileWord,
            "lyricsfile_line" => Self::LyricsfileLine,
            "enhanced_lrc" => Self::EnhancedLrc,
            "standard_lrc" => Self::StandardLrc,
            _ => Self::PlainText,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ttml => "TTML · Word-Synced",
            Self::TtmlLine => "TTML · Line-Synced",
            Self::MusixmatchRichSync => "RichSync · Word-Synced",
            Self::LyricsfileWord => "Lyricsfile · Word-Synced",
            Self::LyricsfileLine => "Lyricsfile · Line-Synced",
            Self::EnhancedLrc => "Enhanced LRC · Word-Synced",
            Self::StandardLrc => "LRC · Line-Synced",
            Self::PlainText => "Plain Lyrics",
        }
    }

    pub fn timing_tier(self) -> TimingTier {
        match self {
            Self::Ttml | Self::MusixmatchRichSync | Self::LyricsfileWord | Self::EnhancedLrc => {
                TimingTier::Word
            }
            Self::TtmlLine | Self::LyricsfileLine | Self::StandardLrc => TimingTier::Line,
            Self::PlainText => TimingTier::Plain,
        }
    }

    pub fn is_word_synced(self) -> bool {
        self.timing_tier() == TimingTier::Word
    }

    pub fn is_time_synced(self) -> bool {
        !matches!(self, Self::PlainText)
    }
}

/// Single word or syllable unit with millisecond-accurate timing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricWord {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Whether a space separates this word from the next word when rendering inline.
    #[serde(default)]
    pub trailing_space: bool,
}

impl LyricWord {
    /// Duration of this word in milliseconds.
    #[inline]
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }

    /// Computes normalized `0.0..=1.0` karaoke sweep progress at `position_ms`.
    /// Clamps very short words (`< 65ms`) to a minimum visual duration of `65ms`
    /// so fast syllables remain visibly legible without flickering past in one frame.
    #[inline]
    pub fn progress_at(&self, position_ms: u64) -> f32 {
        if position_ms < self.start_ms {
            return 0.0;
        }
        let effective_duration = self.duration_ms().max(65);
        let elapsed = position_ms.saturating_sub(self.start_ms);
        if elapsed >= effective_duration {
            1.0
        } else {
            (elapsed as f32 / effective_duration as f32).clamp(0.0, 1.0)
        }
    }
}

/// A single lyric line containing ordered timed words plus optional translation, romanization,
/// background vocals, singer agent ID, and song section label (Verse, Chorus, Bridge...).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    pub words: Vec<LyricWord>,
    pub start_ms: u64,
    pub end_ms: u64,
    #[serde(default)]
    pub section: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub translation: Option<String>,
    #[serde(default)]
    pub romanization: Option<String>,
    #[serde(default)]
    pub background_words: Vec<LyricWord>,
}

impl LyricLine {
    /// Concatenated full line text preserving natural spacing.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for (idx, word) in self.words.iter().enumerate() {
            out.push_str(&word.text);
            if word.trailing_space && idx + 1 < self.words.len() {
                out.push(' ');
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.background_words.is_empty()
    }
}

/// Source attribution & legal/copyright metadata required by compliant lyrics providers
/// (e.g. Unison mandatory song + curator attribution links, AMLL TTML DB credit,
/// Musixmatch copyright notice and 30% free-tier truncation notice).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LyricsAttribution {
    pub provider_label: String,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub submitter_name: Option<String>,
    #[serde(default)]
    pub submitter_url: Option<String>,
    #[serde(default)]
    pub copyright: Option<String>,
    #[serde(default)]
    pub disclaimer: Option<String>,
}

/// Complete normalized lyrics document independent of the originating format or provider.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LyricsDocument {
    pub lines: Vec<LyricLine>,
    /// User- or file-defined global timing offset in milliseconds (positive shifts lyrics earlier).
    pub offset_ms: i64,
    pub format: LyricsFormat,
    pub source: String,
    pub instrumental: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub isrc: Option<String>,
    #[serde(default)]
    pub attribution: Option<Box<LyricsAttribution>>,
}

impl LyricsDocument {
    /// Apply global offset cleanly without underflowing below `0`.
    #[inline]
    pub fn adjusted_position_ms(&self, audio_position_ms: u64) -> u64 {
        let signed = audio_position_ms as i64 + self.offset_ms;
        signed.max(0) as u64
    }

    /// Returns the index of the currently active lyric line using binary search (`O(log N)`),
    /// performing zero allocations and remaining 100% accurate immediately after seeking.
    pub fn active_line_index(&self, audio_position_ms: u64) -> Option<usize> {
        if self.lines.is_empty() || !self.format.is_time_synced() {
            return None;
        }
        let pos = self.adjusted_position_ms(audio_position_ms);

        // Find first line with `start_ms > pos`
        let upper = self.lines.partition_point(|line| line.start_ms <= pos);
        if upper == 0 {
            return None;
        }
        let candidate = upper - 1;
        let line = &self.lines[candidate];

        // Active if within `[start_ms, end_ms]` or during a brief natural inter-line hold
        if pos <= line.end_ms || candidate + 1 < self.lines.len() {
            // Avoid keeping an old line lit across long instrumental breaks (> 3.5s past end_ms)
            if line.end_ms > line.start_ms && pos > line.end_ms.saturating_add(3_500) {
                return None;
            }
            Some(candidate)
        } else if pos <= line.end_ms.saturating_add(2_000) {
            Some(candidate)
        } else {
            None
        }
    }

    /// Returns `true` if any line in the document carries multiple timed word spans.
    pub fn has_word_timing(&self) -> bool {
        self.format.is_word_synced() && self.lines.iter().any(|l| l.words.len() > 1)
    }

    /// Effective timing precision tier of this parsed document.
    pub fn timing_tier(&self) -> TimingTier {
        if self.instrumental {
            TimingTier::None
        } else if self.has_word_timing() {
            TimingTier::Word
        } else if self.format.is_time_synced() {
            TimingTier::Line
        } else {
            TimingTier::Plain
        }
    }
}
