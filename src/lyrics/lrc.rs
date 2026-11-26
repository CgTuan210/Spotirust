//! Enhanced LRC (word-by-word `<mm:ss.xx>`) and Standard LRC (line-by-line `[mm:ss.xx]`) parser.
//!
//! Handles:
//! - Metadata headers (`[ti:]`, `[ar:]`, `[al:]`, `[offset:±ms]`)
//! - Multi-timestamp lines (`[00:12.00][01:05.00]Chorus line`)
//! - Centisecond (`[01:23.45]`) and millisecond (`[01:23.456]`) timestamps
//! - Enhanced LRC inline word timing (`<00:12.00>Hello <00:12.40>world`)
//! - Automatic end-time inference from adjacent word/line timestamps

use crate::lyrics::model::{LyricLine, LyricWord, LyricsDocument, LyricsFormat};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LrcParseError {
    #[error("LRC document contains no timed lyric lines")]
    EmptyDocument,
}

/// Parse either Enhanced LRC or Standard LRC into a normalized `LyricsDocument`.
pub fn parse_lrc(input: &str, source: impl Into<String>) -> Result<LyricsDocument, LrcParseError> {
    let mut doc = LyricsDocument {
        source: source.into(),
        ..Default::default()
    };

    // `(effective_line_start_ms, first_prefix_anchor_ms, content)`
    let mut raw_entries: Vec<(u64, u64, String)> = Vec::new();
    let mut any_word_tags = false;

    for raw_line in input.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        // Collect all leading `[...]` tags
        let mut rest = line;
        let mut line_timestamps: Vec<u64> = Vec::new();

        while rest.starts_with('[') {
            let Some(close_idx) = rest.find(']') else {
                break;
            };
            let tag = &rest[1..close_idx];
            if let Some(ms) = parse_lrc_timestamp(tag) {
                line_timestamps.push(ms);
                rest = &rest[close_idx + 1..];
            } else {
                parse_metadata_tag(tag, &mut doc);
                rest = &rest[close_idx + 1..];
            }
        }

        if line_timestamps.is_empty() {
            continue;
        }

        let content = rest.trim().to_string();
        let anchor_ts = line_timestamps[0];
        for ts in line_timestamps {
            raw_entries.push((ts, anchor_ts, content.clone()));
        }
    }

    if raw_entries.is_empty() {
        return Err(LrcParseError::EmptyDocument);
    }

    raw_entries.sort_by_key(|(ts, _, _)| *ts);

    for i in 0..raw_entries.len() {
        let (start_ms, anchor_ts, content) = &raw_entries[i];
        let next_line_start = raw_entries[i + 1..]
            .iter()
            .find(|(ts, _, _)| *ts > *start_ms)
            .map(|(ts, _, _)| *ts);

        // Skip empty instrumental break markers after they served as end-time anchors
        if content.is_empty() {
            continue;
        }

        let default_end = next_line_start.unwrap_or_else(|| start_ms.saturating_add(4_000));
        let shift_ms = *start_ms as i64 - *anchor_ts as i64;

        let words = if let Some(enhanced) =
            parse_enhanced_words(content, *start_ms, default_end, shift_ms)
        {
            any_word_tags = true;
            enhanced
        } else {
            vec![LyricWord {
                text: content.clone(),
                start_ms: *start_ms,
                end_ms: default_end,
                trailing_space: false,
            }]
        };

        let line_end = words
            .iter()
            .map(|w| w.end_ms)
            .max()
            .unwrap_or(default_end)
            .max(default_end);

        doc.lines.push(LyricLine {
            words,
            start_ms: *start_ms,
            end_ms: line_end,
            section: None,
            agent: None,
            key: None,
            translation: None,
            romanization: None,
            background_words: Vec::new(),
        });
    }

    if doc.lines.is_empty() {
        return Err(LrcParseError::EmptyDocument);
    }

    doc.format = if any_word_tags {
        LyricsFormat::EnhancedLrc
    } else {
        LyricsFormat::StandardLrc
    };

    Ok(doc)
}

/// Parse plain unsynced lyrics into a `LyricsFormat::PlainText` document for graceful display.
pub fn parse_plain_lyrics(input: &str, source: impl Into<String>) -> LyricsDocument {
    let lines = input
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|text| LyricLine {
            words: vec![LyricWord {
                text: text.to_string(),
                start_ms: 0,
                end_ms: 0,
                trailing_space: false,
            }],
            start_ms: 0,
            end_ms: 0,
            section: None,
            agent: None,
            key: None,
            translation: None,
            romanization: None,
            background_words: Vec::new(),
        })
        .collect();

    LyricsDocument {
        lines,
        offset_ms: 0,
        format: LyricsFormat::PlainText,
        source: source.into(),
        ..Default::default()
    }
}

fn parse_metadata_tag(tag: &str, doc: &mut LyricsDocument) {
    let Some((key, val)) = tag.split_once(':') else {
        return;
    };
    let val = val.trim();
    if val.is_empty() {
        return;
    }
    match key.trim().to_ascii_lowercase().as_str() {
        "ti" => doc.title = Some(val.to_string()),
        "ar" => doc.artist = Some(val.to_string()),
        "al" => doc.album = Some(val.to_string()),
        "offset" => {
            if let Ok(offset) = val.parse::<i64>() {
                doc.offset_ms = offset;
            }
        }
        _ => {}
    }
}

/// Parse Enhanced LRC inline `<mm:ss.xx>word` spans. Returns `None` if no valid `<...>` tags exist.
/// `shift_ms` shifts inline timestamps cleanly when a multi-timestamp chorus line is expanded.
fn parse_enhanced_words(
    content: &str,
    line_start_ms: u64,
    line_end_ms: u64,
    shift_ms: i64,
) -> Option<Vec<LyricWord>> {
    if !content.contains('<') {
        return None;
    }

    let mut tokens: Vec<(u64, String, bool)> = Vec::new();
    let bytes = content.as_bytes();
    let mut i = 0usize;
    let mut current_ts: Option<u64> = None;
    let mut text_start = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'<'
            && let Some(rel_close) = content[i + 1..].find('>')
        {
            let tag_inner = &content[i + 1..i + 1 + rel_close];
            if let Some(raw_ts) = parse_lrc_timestamp(tag_inner) {
                let shifted_ts = (raw_ts as i64 + shift_ms).max(0) as u64;
                if text_start < i {
                    let segment = &content[text_start..i];
                    let ends_ws = segment.ends_with(char::is_whitespace);
                    let cleaned = segment.trim();
                    if !cleaned.is_empty() {
                        tokens.push((
                            current_ts.unwrap_or(line_start_ms),
                            cleaned.to_string(),
                            ends_ws,
                        ));
                    }
                }
                current_ts = Some(shifted_ts);
                i = i + 1 + rel_close + 1;
                text_start = i;
                continue;
            }
        }
        i += 1;
    }

    if text_start < content.len() {
        let segment = &content[text_start..];
        let cleaned = segment.trim();
        if !cleaned.is_empty() {
            tokens.push((
                current_ts.unwrap_or(line_start_ms),
                cleaned.to_string(),
                false,
            ));
        }
    }

    if current_ts.is_none() || tokens.is_empty() {
        return None;
    }

    let mut words = Vec::with_capacity(tokens.len());
    for idx in 0..tokens.len() {
        let (start_ms, ref text, trailing_space) = tokens[idx];
        // Scan ahead to first strictly greater word timestamp so duplicate-timestamp words
        // share the proper segment boundary instead of stretching to `line_end_ms`.
        let next_strict_start = tokens[idx + 1..]
            .iter()
            .find(|(ts, _, _)| *ts > start_ms)
            .map(|(ts, _, _)| *ts);
        let end_ms = next_strict_start.unwrap_or_else(|| line_end_ms.max(start_ms + 350));
        words.push(LyricWord {
            text: text.clone(),
            start_ms,
            end_ms,
            trailing_space: trailing_space && idx + 1 < tokens.len(),
        });
    }

    Some(words)
}

/// Parse LRC timestamp `mm:ss.xx`, `mm:ss.xxx`, or `mm:ss`.
pub fn parse_lrc_timestamp(raw: &str) -> Option<u64> {
    let s = raw.trim();
    let (min_str, sec_part) = s.split_once(':')?;
    let mins: u64 = min_str.parse().ok()?;

    let (secs, millis) = match sec_part
        .split_once('.')
        .or_else(|| sec_part.split_once(':'))
    {
        Some((sec_s, frac_s)) => {
            let secs: u64 = sec_s.parse().ok()?;
            let frac_digits: String = frac_s.chars().take_while(|c| c.is_ascii_digit()).collect();
            let ms = match frac_digits.len() {
                0 => 0,
                1 => frac_digits.parse::<u64>().ok()? * 100,
                2 => frac_digits.parse::<u64>().ok()? * 10,
                _ => frac_digits[..3].parse::<u64>().ok()?,
            };
            (secs, ms)
        }
        None => (sec_part.parse::<u64>().ok()?, 0),
    };

    Some(mins * 60_000 + secs * 1_000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_enhanced_lrc_sample() {
        let raw = include_str!("../../assets/sample_lyrics/spotirust_demo.lrc");
        let doc = parse_lrc(raw, "sample").expect("enhanced LRC should parse");
        assert_eq!(doc.format, LyricsFormat::EnhancedLrc);
        assert_eq!(doc.title.as_deref(), Some("Velocity Pulse"));
        assert_eq!(doc.lines.len(), 4);
        assert_eq!(doc.lines[0].words.len(), 6);
        assert_eq!(doc.lines[0].words[0].text, "Neon");
        assert_eq!(doc.lines[0].words[0].start_ms, 500);
        assert_eq!(doc.lines[0].words[0].end_ms, 1_100);
        assert_eq!(
            doc.lines[0].plain_text(),
            "Neon signals racing across the glass"
        );
    }

    #[test]
    fn parses_standard_lrc_with_multi_timestamp_and_offset() {
        let raw = "\
[ti:Test Song]
[ar:Test Artist]
[offset:-250]
[00:10.00][00:40.00]Repeated chorus line
[00:20.500]Second verse line
";
        let doc = parse_lrc(raw, "std").unwrap();
        assert_eq!(doc.format, LyricsFormat::StandardLrc);
        assert_eq!(doc.offset_ms, -250);
        assert_eq!(doc.lines.len(), 3);
        assert_eq!(doc.lines[0].start_ms, 10_000);
        assert_eq!(doc.lines[1].start_ms, 20_500);
        assert_eq!(doc.lines[2].start_ms, 40_000);
    }

    #[test]
    fn handles_duplicate_word_timestamps_and_multi_timestamp_enhanced_lrc() {
        let raw = "\
[00:10.00][00:40.00]<00:10.00>We <00:10.00>rise <00:11.00>up
[00:15.00]<00:15.00>Next
";
        let doc = parse_lrc(raw, "edge-lrc").unwrap();
        assert_eq!(doc.format, LyricsFormat::EnhancedLrc);
        assert_eq!(doc.lines.len(), 3);

        // First occurrence at 10s: "We" and "rise" share 10_000 and must both end at 11_000
        let first = &doc.lines[0].words;
        assert_eq!((first[0].start_ms, first[0].end_ms), (10_000, 11_000));
        assert_eq!((first[1].start_ms, first[1].end_ms), (10_000, 11_000));
        assert_eq!((first[2].start_ms, first[2].end_ms), (11_000, 15_000));

        // Repeated occurrence at 40s: inline timestamps shifted by +30_000ms!
        let repeated = &doc.lines[2].words;
        assert_eq!((repeated[0].start_ms, repeated[0].end_ms), (40_000, 41_000));
        assert_eq!((repeated[1].start_ms, repeated[1].end_ms), (40_000, 41_000));
        assert_eq!((repeated[2].start_ms, repeated[2].end_ms), (41_000, 44_000));
    }
}
