//! Pure-Rust parser for the CC0 1.0 LRCLIB `Lyricsfile` v1.0 YAML specification
//! (`https://lrclib.net/lyricsfile`).
//!
//! Supports:
//! - Word-level synchronized lyrics (`lines[].words[]` with `text`, `start_ms`, optional `end_ms`)
//!   -> `LyricsFormat::LyricsfileWord`
//! - Line-level synchronized lyrics (`lines[]` with `text`, `start_ms`, optional `end_ms`)
//!   -> `LyricsFormat::LyricsfileLine`
//! - Plain text fallback (`plain:` block scalar) -> `LyricsFormat::PlainText`
//! - Instrumental flag (`metadata.instrumental: true`)
//! - Trailing/leading whitespace preservation in quoted word strings (so `"We're "` + `"no "`
//!   reconstructs natural spacing and syllable spans like `"any"` + `"body "` stay joined!)
//! - Overlapping vocal lines (sorted by `start_ms`) and automatic end-time inference when `end_ms`
//!   is omitted.
//!
//! Never synthesizes fake word timestamps when `words[]` is absent.

use crate::lyrics::lrc::parse_plain_lyrics;
use crate::lyrics::model::{LyricLine, LyricWord, LyricsDocument, LyricsFormat};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LyricsfileError {
    #[error("empty lyricsfile payload")]
    Empty,
    #[error("invalid lyricsfile structure: {0}")]
    InvalidFormat(String),
}

#[derive(Default)]
struct RawWord {
    text: Option<String>,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
}

#[derive(Default)]
struct RawLine {
    text: Option<String>,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    words: Vec<RawWord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Root,
    Metadata,
    Lines,
    PlainBlock {
        base_indent: usize,
        chomp_strip: bool,
    },
}

/// Parse an LRCLIB `Lyricsfile` v1.0 YAML string into a normalized `LyricsDocument`.
pub fn parse_lyricsfile(
    input: &str,
    source: impl Into<String>,
) -> Result<LyricsDocument, LyricsfileError> {
    let source = source.into();
    let trimmed_all = input.trim();
    if trimmed_all.is_empty() {
        return Err(LyricsfileError::Empty);
    }

    let mut doc = LyricsDocument {
        source: source.clone(),
        ..Default::default()
    };

    let mut saw_known_key = false;
    let mut section = Section::Root;
    let mut lines: Vec<RawLine> = Vec::new();
    let mut in_words_list = false;
    let mut words_indent: usize = 0;
    let mut plain_lines: Vec<String> = Vec::new();
    let mut inline_plain: Option<String> = None;

    for raw_line in input.lines() {
        let line_no_cr = raw_line.trim_end_matches('\r');

        // Handle multiline `plain: |` block scalar first
        if let Section::PlainBlock {
            base_indent,
            chomp_strip,
        } = section
        {
            if line_no_cr.trim().is_empty() {
                plain_lines.push(String::new());
                continue;
            }
            let indent = leading_spaces(line_no_cr);
            if indent > base_indent {
                let strip = base_indent.min(indent);
                plain_lines.push(line_no_cr[strip..].to_string());
                continue;
            } else {
                // Trim trailing blank lines for `|-` chomping
                if chomp_strip {
                    while plain_lines.last().is_some_and(|l| l.is_empty()) {
                        plain_lines.pop();
                    }
                }
                section = Section::Root;
                // Fall through to parse this top-level line
            }
        }

        let trimmed = line_no_cr.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" || trimmed == "..." {
            continue;
        }

        let indent = leading_spaces(line_no_cr);

        // Top-level keys (indent == 0), except sequence items `- ...` directly under `lines:`
        if indent == 0 && !(section == Section::Lines && strip_list_dash(trimmed).is_some()) {
            in_words_list = false;
            if let Some((key, val_raw)) = split_kv(trimmed) {
                match key {
                    "version" => {
                        saw_known_key = true;
                        section = Section::Root;
                    }
                    "metadata" => {
                        saw_known_key = true;
                        section = Section::Metadata;
                    }
                    "lines" => {
                        saw_known_key = true;
                        section = Section::Lines;
                    }
                    "plain" => {
                        saw_known_key = true;
                        let v = val_raw.trim();
                        if v.starts_with('|') || v.starts_with('>') {
                            let chomp_strip = v.contains('-');
                            section = Section::PlainBlock {
                                base_indent: 0,
                                chomp_strip,
                            };
                        } else if !v.is_empty() {
                            inline_plain = Some(parse_yaml_scalar(v));
                            section = Section::Root;
                        } else {
                            section = Section::Root;
                        }
                    }
                    _ => {
                        section = Section::Root;
                    }
                }
            }
            continue;
        }

        match section {
            Section::Metadata => {
                if let Some((key, val_raw)) = split_kv(trimmed) {
                    let val = parse_yaml_scalar(val_raw);
                    match key {
                        "title" if !val.is_empty() => doc.title = Some(val),
                        "artist" if !val.is_empty() => doc.artist = Some(val),
                        "album" if !val.is_empty() => doc.album = Some(val),
                        "instrumental" => {
                            doc.instrumental = val.eq_ignore_ascii_case("true");
                        }
                        _ => {}
                    }
                }
            }
            Section::Lines => {
                if let Some(after_dash) = strip_list_dash(trimmed) {
                    // Distinguish between a new line entry and a nested word entry.
                    // In LRCLIB live YAML, `lines:` items can start at indent 0 (`- text:`)
                    // OR indent 2 (`  - text:`), and `words:` items are indented strictly deeper
                    // than the `words:` key (or at `words_indent` when `words:` used `- ` at same level).
                    if in_words_list && indent >= words_indent {
                        let mut word = RawWord::default();
                        if let Some((k, v)) = split_kv(after_dash) {
                            apply_word_kv(&mut word, k, v);
                        }
                        if let Some(cur_line) = lines.last_mut() {
                            cur_line.words.push(word);
                        }
                    } else {
                        in_words_list = false;
                        let mut raw_entry = RawLine::default();
                        if let Some((k, v)) = split_kv(after_dash) {
                            if k == "words" {
                                in_words_list = true;
                                words_indent = indent;
                            } else {
                                apply_line_kv(&mut raw_entry, k, v);
                            }
                        }
                        lines.push(raw_entry);
                    }
                } else if let Some((key, val_raw)) = split_kv(trimmed) {
                    if key == "words" {
                        in_words_list = true;
                        // In YAML, `- text:` under `words:` may sit at the exact same indent as
                        // `words:` (sequence-in-mapping compact style, as LRCLIB emits!) OR deeper.
                        words_indent = indent;
                    } else if in_words_list && indent > words_indent {
                        if let Some(word) = lines.last_mut().and_then(|l| l.words.last_mut()) {
                            apply_word_kv(word, key, val_raw);
                        }
                    } else {
                        in_words_list = false;
                        if let Some(cur_line) = lines.last_mut() {
                            apply_line_kv(cur_line, key, val_raw);
                        }
                    }
                }
            }
            Section::Root | Section::PlainBlock { .. } => {}
        }
    }

    if !saw_known_key {
        return Err(LyricsfileError::InvalidFormat(
            "missing required lyricsfile fields (version/metadata/lines/plain)".to_string(),
        ));
    }

    let mut saw_any_word_timing = false;
    let mut parsed_lines: Vec<LyricLine> = Vec::new();

    for raw in lines {
        let line_start = raw
            .start_ms
            .or_else(|| raw.words.first().and_then(|w| w.start_ms));
        let Some(start_ms) = line_start else {
            continue;
        };

        let mut words: Vec<LyricWord> = Vec::new();
        if !raw.words.is_empty() {
            for rw in &raw.words {
                let Some(raw_text) = rw.text.as_deref() else {
                    continue;
                };
                let Some(w_start) = rw.start_ms else {
                    continue;
                };
                let ends_ws = raw_text.ends_with(char::is_whitespace);
                let starts_ws = raw_text.starts_with(char::is_whitespace);
                let cleaned = raw_text.trim();
                if starts_ws && let Some(prev) = words.last_mut() {
                    prev.trailing_space = true;
                }
                if cleaned.is_empty() {
                    if let Some(prev) = words.last_mut() {
                        prev.trailing_space = true;
                    }
                    continue;
                }
                let w_end = rw.end_ms.unwrap_or(w_start).max(w_start);
                words.push(LyricWord {
                    text: cleaned.to_string(),
                    start_ms: w_start,
                    end_ms: w_end,
                    trailing_space: ends_ws,
                });
            }
            if let Some(last) = words.last_mut() {
                last.trailing_space = false;
            }
            if words.len() > 1 {
                saw_any_word_timing = true;
            } else if words.len() == 1 && raw.words.len() == 1 {
                // Single-word line inside a word-synced document counts as word-timed if it came from `words:`
                saw_any_word_timing = true;
            }
        }

        if words.is_empty() {
            let text = raw.text.as_deref().unwrap_or("").trim();
            if text.is_empty() {
                // Skip empty instrumental gap markers (`text: ""`) while keeping timeline intact
                continue;
            }
            let end_ms = raw.end_ms.unwrap_or(start_ms).max(start_ms);
            words.push(LyricWord {
                text: text.to_string(),
                start_ms,
                end_ms,
                trailing_space: false,
            });
        }

        let max_word_end = words
            .iter()
            .filter(|w| w.end_ms > w.start_ms)
            .map(|w| w.end_ms)
            .max();
        let line_end = raw
            .end_ms
            .or(max_word_end)
            .unwrap_or(start_ms)
            .max(start_ms);

        parsed_lines.push(LyricLine {
            words,
            start_ms,
            end_ms: line_end,
            section: None,
            agent: None,
            key: None,
            translation: None,
            romanization: None,
            background_words: Vec::new(),
        });
    }

    if !parsed_lines.is_empty() {
        parsed_lines.sort_by_key(|l| l.start_ms);
        for i in 0..parsed_lines.len() {
            let next_start = parsed_lines[i + 1..]
                .iter()
                .find(|l| l.start_ms > parsed_lines[i].start_ms)
                .map(|l| l.start_ms);
            let line = &mut parsed_lines[i];
            if line.end_ms <= line.start_ms {
                line.end_ms = next_start.unwrap_or_else(|| line.start_ms + 4_000);
            }
            let line_end = line.end_ms;
            let count = line.words.len();
            for w_idx in 0..count {
                if line.words[w_idx].end_ms <= line.words[w_idx].start_ms {
                    let cur = line.words[w_idx].start_ms;
                    let next_w = line.words[w_idx + 1..]
                        .iter()
                        .find(|nw| nw.start_ms > cur)
                        .map(|nw| nw.start_ms);
                    line.words[w_idx].end_ms = next_w.unwrap_or_else(|| line_end.max(cur + 250));
                }
            }
        }

        doc.lines = parsed_lines;
        doc.format = if saw_any_word_timing {
            LyricsFormat::LyricsfileWord
        } else {
            LyricsFormat::LyricsfileLine
        };
        return Ok(doc);
    }

    if doc.instrumental {
        doc.format = LyricsFormat::PlainText;
        return Ok(doc);
    }

    let plain_text = inline_plain.unwrap_or_else(|| plain_lines.join("\n"));
    if !plain_text.trim().is_empty() {
        let mut plain_doc = parse_plain_lyrics(&plain_text, source);
        plain_doc.title = doc.title;
        plain_doc.artist = doc.artist;
        plain_doc.album = doc.album;
        return Ok(plain_doc);
    }

    Err(LyricsfileError::Empty)
}

fn leading_spaces(s: &str) -> usize {
    s.bytes().take_while(|&b| b == b' ').count()
}

fn strip_list_dash(trimmed: &str) -> Option<&str> {
    if let Some(rest) = trimmed.strip_prefix("- ") {
        Some(rest.trim_start())
    } else if trimmed == "-" {
        Some("")
    } else {
        None
    }
}

fn split_kv(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    for (idx, &b) in bytes.iter().enumerate() {
        if b == b':' {
            let next = bytes.get(idx + 1).copied();
            if next.is_none() || next == Some(b' ') || next == Some(b'\t') {
                let key = s[..idx].trim();
                let val = s[idx + 1..].trim();
                if !key.is_empty() && !key.starts_with('-') {
                    return Some((key, val));
                }
                return None;
            }
        }
    }
    None
}

fn apply_line_kv(line: &mut RawLine, key: &str, val_raw: &str) {
    match key {
        "text" => line.text = Some(parse_yaml_scalar(val_raw)),
        "start_ms" => line.start_ms = parse_u64_scalar(val_raw),
        "end_ms" => line.end_ms = parse_u64_scalar(val_raw),
        _ => {}
    }
}

fn apply_word_kv(word: &mut RawWord, key: &str, val_raw: &str) {
    match key {
        "text" => word.text = Some(parse_yaml_scalar(val_raw)),
        "start_ms" => word.start_ms = parse_u64_scalar(val_raw),
        "end_ms" => word.end_ms = parse_u64_scalar(val_raw),
        _ => {}
    }
}

fn parse_u64_scalar(raw: &str) -> Option<u64> {
    let cleaned = strip_inline_comment(raw).trim();
    cleaned.parse::<u64>().ok().or_else(|| {
        cleaned
            .parse::<f64>()
            .ok()
            .map(|v| v.max(0.0).round() as u64)
    })
}

fn strip_inline_comment(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.starts_with('"') || trimmed.starts_with('\'') {
        return trimmed;
    }
    if let Some(pos) = trimmed.find(" #") {
        &trimmed[..pos]
    } else {
        trimmed
    }
}

/// Parse a YAML double-quoted, single-quoted, or unquoted scalar string,
/// preserving intentional trailing/leading whitespace inside quotes!
fn parse_yaml_scalar(raw: &str) -> String {
    let s = raw.trim();
    if s.starts_with('"') && s.len() >= 2 {
        // Parse until matching unescaped closing `"`
        let mut out = String::new();
        let mut chars = s[1..].chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('0') => out.push('\0'),
                    Some('u') => {
                        let hex: String = chars.by_ref().take(4).collect();
                        if let Ok(code) = u32::from_str_radix(&hex, 16)
                            && let Some(ch) = char::from_u32(code)
                        {
                            out.push(ch);
                        }
                    }
                    Some(other) => out.push(other),
                    None => break,
                },
                other => out.push(other),
            }
        }
        return out;
    }

    if s.starts_with('\'') && s.len() >= 2 {
        let mut out = String::new();
        let mut chars = s[1..].chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    out.push('\'');
                } else {
                    break;
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }

    strip_inline_comment(s).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_live_lrclib_word_synced_lyricsfile_with_compact_indent() {
        // Exact structure observed live from https://lrclib.net/api/get (Nov 2026)
        let yaml = r#"version: "1.0"
metadata:
  title: Never Gonna Give You Up
  artist: Rick Astley
  album: Whenever You Need Somebody
  duration_ms: 213560
lines:
- text: "We're no strangers to love "
  start_ms: 18834
  end_ms: 22088
  words:
  - text: "We're "
    start_ms: 18834
    end_ms: 19086
  - text: "no "
    start_ms: 19086
    end_ms: 19336
  - text: "strangers "
    start_ms: 19336
    end_ms: 19870
  - text: "to "
    start_ms: 19870
    end_ms: 20120
  - text: "love "
    start_ms: 20120
    end_ms: 22088
- text: ''
  start_ms: 22088
  end_ms: 24000
- text: "You know the rules"
  start_ms: 24000
  end_ms: 26500
  words:
  - text: "You "
    start_ms: 24000
  - text: "know "
    start_ms: 24500
  - text: "the "
    start_ms: 25100
  - text: "rules"
    start_ms: 25600
"#;
        let doc = parse_lyricsfile(yaml, "LRCLIB (Lyricsfile)").expect("should parse");
        assert_eq!(doc.format, LyricsFormat::LyricsfileWord);
        assert!(doc.has_word_timing());
        assert_eq!(doc.title.as_deref(), Some("Never Gonna Give You Up"));
        assert_eq!(doc.artist.as_deref(), Some("Rick Astley"));
        assert_eq!(doc.album.as_deref(), Some("Whenever You Need Somebody"));
        assert_eq!(doc.lines.len(), 2);

        let l0 = &doc.lines[0];
        assert_eq!(l0.plain_text(), "We're no strangers to love");
        assert_eq!(l0.words.len(), 5);
        assert_eq!(l0.words[0].text, "We're");
        assert_eq!((l0.words[0].start_ms, l0.words[0].end_ms), (18834, 19086));
        assert!(l0.words[0].trailing_space);

        // Second line omits word end_ms -> inferred cleanly from next word & line end
        let l1 = &doc.lines[1];
        assert_eq!(l1.plain_text(), "You know the rules");
        assert_eq!((l1.words[0].start_ms, l1.words[0].end_ms), (24000, 24500));
        assert_eq!((l1.words[3].start_ms, l1.words[3].end_ms), (25600, 26500));
    }

    #[test]
    fn parses_indented_yaml_word_sync_and_syllable_joining() {
        let yaml = r#"version: "1.0"
metadata:
  title: Small Hours
  artist: The Static Pines
lines:
  - text: "Anybody feeling it?"
    start_ms: 4200
    end_ms: 6800
    words:
      - text: "Any"
        start_ms: 4200
        end_ms: 4500
      - text: "body "
        start_ms: 4500
        end_ms: 5100
      - text: "feeling "
        start_ms: 5100
        end_ms: 6000
      - text: "it?"
        start_ms: 6000
        end_ms: 6800
"#;
        let doc = parse_lyricsfile(yaml, "LRCLIB").unwrap();
        assert_eq!(doc.format, LyricsFormat::LyricsfileWord);
        assert_eq!(doc.lines.len(), 1);
        assert_eq!(doc.lines[0].words.len(), 4);
        assert!(!doc.lines[0].words[0].trailing_space);
        assert!(doc.lines[0].words[1].trailing_space);
        assert_eq!(doc.lines[0].plain_text(), "Anybody feeling it?");
    }

    #[test]
    fn parses_line_only_plain_and_instrumental_lyricsfiles_honestly() {
        let line_yaml = r#"version: '1.0'
metadata:
  title: Line Track
  artist: Artist
  instrumental: false
lines:
- text: 'First ''quoted'' line'
  start_ms: 1000
  end_ms: 3000
- text: Second line
  start_ms: 3500
"#;
        let doc = parse_lyricsfile(line_yaml, "LRCLIB").unwrap();
        assert_eq!(doc.format, LyricsFormat::LyricsfileLine);
        assert!(!doc.has_word_timing());
        assert!(doc.format.is_time_synced());
        assert_eq!(doc.lines[0].plain_text(), "First 'quoted' line");
        assert_eq!(doc.lines[1].end_ms, 7500);

        let plain_yaml = r#"version: "1.0"
metadata:
  title: Plain Song
plain: |
  Line one
  Line two
"#;
        let plain_doc = parse_lyricsfile(plain_yaml, "LRCLIB").unwrap();
        assert_eq!(plain_doc.format, LyricsFormat::PlainText);
        assert!(!plain_doc.format.is_time_synced());
        assert_eq!(plain_doc.lines.len(), 2);

        let inst_yaml = r#"version: "1.0"
metadata:
  title: Orion
  instrumental: true
"#;
        let inst_doc = parse_lyricsfile(inst_yaml, "LRCLIB").unwrap();
        assert!(inst_doc.instrumental);
        assert!(inst_doc.lines.is_empty());
    }
}
