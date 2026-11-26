//! W3C TTML word-by-word lyrics parser powered by `quick-xml 0.42`.
//!
//! Parses standard W3C TTML and user-supplied local `.ttml` files using AMLL / `itunes:` metadata
//! conventions. Spotirust does NOT integrate with any unauthorized Apple Music lyrics API.
//!
//! Supported features:
//! - Word-level inline `<span>` timing (`begin`, `end`, `dur`) with whitespace preservation
//! - Automatic end-time inference when word spans omit `end`/`dur` or share identical timestamps
//! - Line-level `<p>` timing fallback (`LyricsFormat::TtmlLine`) when spans omit word timestamps
//! - Song sections / paragraphs (`<div itunes:song-part="...">` / `itunes:songPart`)
//! - Singer / duet agents (`ttm:agent="v1"`)
//! - Background vocals (`<span ttm:role="x-bg">`)
//! - Inline translations (`ttm:role="x-translation"`) and romanizations (`ttm:role="x-roman"`)
//! - Sidecar `<translations>` and `<transliterations>` linked via `for="L1"` -> `itunes:key="L1"`
//! - Metadata extraction (`<ttm:title>`, `<amll:meta key="..." value="..." />`)
//! - Strict malformed/truncated XML rejection via tag-balance and `<tt>` root validation

use crate::lyrics::model::{LyricLine, LyricWord, LyricsDocument, LyricsFormat};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TtmlParseError {
    #[error("XML parse error: {0}")]
    Xml(String),
    #[error("TTML document contains no lyric lines")]
    EmptyDocument,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextTarget {
    None,
    Title,
    HeadTranslation,
    HeadTransliteration,
    ParagraphPlain,
    WordSpan,
    BgOuter,
    BgWordSpan,
    InlineTranslation,
    InlineRomanization,
}

#[derive(Default)]
struct PendingSpan {
    text: String,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    dur_ms: Option<u64>,
}

/// Parse a W3C / local AMLL-schema TTML XML string into a normalized `LyricsDocument`.
pub fn parse_ttml(xml: &str, source: impl Into<String>) -> Result<LyricsDocument, TtmlParseError> {
    let mut reader = Reader::from_str(xml);
    // TTML is whitespace-sensitive XML; preserve spaces between inline word spans.
    reader.config_mut().trim_text(false);

    let mut doc = LyricsDocument {
        format: LyricsFormat::Ttml,
        source: source.into(),
        ..Default::default()
    };

    let mut open_tags: Vec<String> = Vec::new();
    let mut saw_tt_root = false;

    let mut sidecar_translations: HashMap<String, String> = HashMap::new();
    let mut sidecar_transliterations: HashMap<String, String> = HashMap::new();
    let mut in_translations = false;
    let mut in_transliterations = false;
    let mut current_sidecar_for: Option<String> = None;
    let mut current_sidecar_buf = String::new();

    let mut title_buf = String::new();
    let mut current_section: Option<String> = None;

    let mut in_p = false;
    let mut p_start_ms: Option<u64> = None;
    let mut p_end_ms: Option<u64> = None;
    let mut p_dur_ms: Option<u64> = None;
    let mut p_key: Option<String> = None;
    let mut p_agent: Option<String> = None;
    let mut p_words: Vec<LyricWord> = Vec::new();
    let mut p_bg_words: Vec<LyricWord> = Vec::new();
    let mut p_plain_buf = String::new();
    let mut p_translation: Option<String> = None;
    let mut p_romanization: Option<String> = None;
    let mut had_space_since_last_word = false;
    let mut saw_any_word_span = false;
    let mut saw_any_line_time = false;
    let mut declared_timing_none = false;

    let mut span_stack: Vec<(TextTarget, PendingSpan)> = Vec::new();
    let mut target = TextTarget::None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let local = local_name_of(e.name().as_ref());
                if local == "tt" {
                    saw_tt_root = true;
                    if let Some(timing) = attr_value(&e, &["timing"])
                        && timing.eq_ignore_ascii_case("none")
                    {
                        declared_timing_none = true;
                    }
                }
                open_tags.push(local.clone());
                match local.as_str() {
                    "title" => {
                        title_buf.clear();
                        target = TextTarget::Title;
                    }
                    "meta" => apply_amll_meta(&e, &mut doc),
                    "translations" => in_translations = true,
                    "transliterations" => in_transliterations = true,
                    "text" if in_translations || in_transliterations => {
                        current_sidecar_for = attr_value(&e, &["for"]);
                        current_sidecar_buf.clear();
                        target = if in_translations {
                            TextTarget::HeadTranslation
                        } else {
                            TextTarget::HeadTransliteration
                        };
                    }
                    "div" => {
                        if let Some(part) = attr_value(&e, &["song-part", "songPart"]) {
                            current_section = Some(part);
                        }
                    }
                    "p" => {
                        in_p = true;
                        p_start_ms = attr_value(&e, &["begin"]).and_then(|s| parse_ttml_time(&s));
                        p_end_ms = attr_value(&e, &["end"]).and_then(|s| parse_ttml_time(&s));
                        p_dur_ms = attr_value(&e, &["dur"]).and_then(|s| parse_ttml_time(&s));
                        if p_start_ms.is_some() || p_end_ms.is_some() {
                            saw_any_line_time = true;
                        }
                        p_key = attr_value(&e, &["key"]);
                        p_agent = attr_value(&e, &["agent"]);
                        p_words.clear();
                        p_bg_words.clear();
                        p_plain_buf.clear();
                        p_translation = None;
                        p_romanization = None;
                        had_space_since_last_word = false;
                        target = TextTarget::ParagraphPlain;
                    }
                    "span" if in_p => {
                        let role = attr_value(&e, &["role"]);
                        let begin = attr_value(&e, &["begin"]).and_then(|s| parse_ttml_time(&s));
                        let end = attr_value(&e, &["end"]).and_then(|s| parse_ttml_time(&s));
                        let dur = attr_value(&e, &["dur"]).and_then(|s| parse_ttml_time(&s));
                        let in_bg = span_stack.iter().any(|(t, _)| *t == TextTarget::BgOuter);

                        let span_kind = match role.as_deref() {
                            Some("x-bg") => TextTarget::BgOuter,
                            Some("x-translation") => TextTarget::InlineTranslation,
                            Some("x-roman") => TextTarget::InlineRomanization,
                            _ if in_bg => TextTarget::BgWordSpan,
                            _ => TextTarget::WordSpan,
                        };

                        if span_kind == TextTarget::WordSpan
                            && had_space_since_last_word
                            && let Some(prev) = p_words.last_mut()
                        {
                            prev.trailing_space = true;
                        }
                        had_space_since_last_word = false;

                        span_stack.push((
                            span_kind,
                            PendingSpan {
                                text: String::new(),
                                start_ms: begin,
                                end_ms: end,
                                dur_ms: dur,
                            },
                        ));
                        target = span_kind;
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let local = local_name_of(e.name().as_ref());
                if local == "tt" {
                    saw_tt_root = true;
                }
                if local == "meta" {
                    apply_amll_meta(&e, &mut doc);
                }
            }
            Ok(Event::Text(e)) => {
                let text = e.xml10_content().into_owned();
                append_text(
                    &text,
                    target,
                    in_p,
                    &mut title_buf,
                    &mut current_sidecar_buf,
                    &mut p_plain_buf,
                    &mut span_stack,
                    &mut had_space_since_last_word,
                    &p_words,
                );
            }
            Ok(Event::GeneralRef(e)) => {
                if let Some(resolved) = resolve_xml_entity(e.as_ref()) {
                    append_text(
                        &resolved,
                        target,
                        in_p,
                        &mut title_buf,
                        &mut current_sidecar_buf,
                        &mut p_plain_buf,
                        &mut span_stack,
                        &mut had_space_since_last_word,
                        &p_words,
                    );
                }
            }
            Ok(Event::End(e)) => {
                let local = local_name_of(e.name().as_ref());
                match open_tags.pop() {
                    Some(expected) if expected == local => {}
                    Some(expected) => {
                        return Err(TtmlParseError::Xml(format!(
                            "mismatched closing tag </{local}> (expected </{expected}>)"
                        )));
                    }
                    None => {
                        return Err(TtmlParseError::Xml(format!(
                            "unexpected closing tag </{local}>"
                        )));
                    }
                }
                match local.as_str() {
                    "title" => {
                        let cleaned = title_buf.trim().to_string();
                        if !cleaned.is_empty() && doc.title.is_none() {
                            doc.title = Some(cleaned);
                        }
                        target = TextTarget::None;
                    }
                    "translations" => in_translations = false,
                    "transliterations" => in_transliterations = false,
                    "text" if in_translations || in_transliterations => {
                        if let Some(key) = current_sidecar_for.take() {
                            let val = current_sidecar_buf.trim().to_string();
                            if !val.is_empty() {
                                if in_translations {
                                    sidecar_translations.entry(key).or_insert(val);
                                } else {
                                    sidecar_transliterations.entry(key).or_insert(val);
                                }
                            }
                        }
                        target = TextTarget::None;
                    }
                    "div" => {
                        current_section = None;
                    }
                    "span" if in_p => {
                        if let Some((kind, pending)) = span_stack.pop() {
                            match kind {
                                TextTarget::WordSpan => {
                                    let raw = pending.text;
                                    let starts_ws = raw.starts_with(char::is_whitespace);
                                    let ends_ws = raw.ends_with(char::is_whitespace);
                                    let cleaned = raw.trim().to_string();
                                    if !cleaned.is_empty() {
                                        if let Some(start) = pending.start_ms.or(p_start_ms) {
                                            if starts_ws && let Some(prev) = p_words.last_mut() {
                                                prev.trailing_space = true;
                                            }
                                            // Do NOT eagerly assign `p_end_ms` when span omits `end`/`dur`:
                                            // leaving `end == start` allows post-processing to infer
                                            // end time from the next strictly greater word timestamp.
                                            let end = resolve_end_ms(
                                                start,
                                                pending.end_ms,
                                                pending.dur_ms,
                                                None,
                                            );
                                            if pending.start_ms.is_some() {
                                                saw_any_word_span = true;
                                            }
                                            p_words.push(LyricWord {
                                                text: cleaned,
                                                start_ms: start,
                                                end_ms: end,
                                                trailing_space: ends_ws,
                                            });
                                        } else {
                                            if !p_plain_buf.is_empty() {
                                                p_plain_buf.push(' ');
                                            }
                                            p_plain_buf.push_str(&cleaned);
                                        }
                                    }
                                }
                                TextTarget::BgWordSpan | TextTarget::BgOuter => {
                                    let cleaned = pending.text.trim().to_string();
                                    if !cleaned.is_empty() {
                                        let start = pending.start_ms.or(p_start_ms).unwrap_or(0);
                                        let end = resolve_end_ms(
                                            start,
                                            pending.end_ms,
                                            pending.dur_ms,
                                            None,
                                        );
                                        p_bg_words.push(LyricWord {
                                            text: cleaned,
                                            start_ms: start,
                                            end_ms: end,
                                            trailing_space: true,
                                        });
                                    }
                                }
                                TextTarget::InlineTranslation => {
                                    let cleaned = pending.text.trim().to_string();
                                    if !cleaned.is_empty() && p_translation.is_none() {
                                        p_translation = Some(cleaned);
                                    }
                                }
                                TextTarget::InlineRomanization => {
                                    let cleaned = pending.text.trim().to_string();
                                    if !cleaned.is_empty() && p_romanization.is_none() {
                                        p_romanization = Some(cleaned);
                                    }
                                }
                                _ => {}
                            }
                        }
                        target = span_stack
                            .last()
                            .map(|(k, _)| *k)
                            .unwrap_or(TextTarget::ParagraphPlain);
                    }
                    "p" if in_p => {
                        in_p = false;
                        span_stack.clear();
                        target = TextTarget::None;

                        if p_words.is_empty() {
                            let line_text = collapse_whitespace(&p_plain_buf);
                            if !line_text.is_empty() {
                                let start = p_start_ms.unwrap_or(0);
                                let end = resolve_end_ms(start, p_end_ms, p_dur_ms, None);
                                p_words.push(LyricWord {
                                    text: line_text,
                                    start_ms: start,
                                    end_ms: end,
                                    trailing_space: false,
                                });
                            }
                        } else {
                            // Preserve explicit syllable boundaries (e.g. Unison/AMLL `<span..>any</span><span..>body</span>`)
                            // whenever the TTML paragraph already specifies spaces between words; only synthesize
                            // inter-word spaces for completely whitespace-stripped Latin XML.
                            if !p_words.iter().any(|w| w.trailing_space) {
                                normalize_word_spacing(&mut p_words);
                            } else if let Some(last) = p_words.last_mut() {
                                last.trailing_space = false;
                            }
                        }

                        if !p_words.is_empty() || !p_bg_words.is_empty() {
                            let line_start = p_start_ms
                                .or_else(|| p_words.first().map(|w| w.start_ms))
                                .or_else(|| p_bg_words.first().map(|w| w.start_ms))
                                .unwrap_or(0);
                            let inferred_max_end = p_words
                                .iter()
                                .chain(p_bg_words.iter())
                                .filter(|w| w.end_ms > w.start_ms)
                                .map(|w| w.end_ms)
                                .max();
                            let line_end =
                                resolve_end_ms(line_start, p_end_ms, p_dur_ms, inferred_max_end);

                            let key_ref = p_key.as_deref();
                            let translation = p_translation.take().or_else(|| {
                                key_ref.and_then(|k| sidecar_translations.get(k).cloned())
                            });
                            let romanization = p_romanization.take().or_else(|| {
                                key_ref.and_then(|k| sidecar_transliterations.get(k).cloned())
                            });

                            doc.lines.push(LyricLine {
                                words: std::mem::take(&mut p_words),
                                start_ms: line_start,
                                end_ms: line_end,
                                section: current_section.clone(),
                                agent: p_agent.take(),
                                key: p_key.take(),
                                translation,
                                romanization,
                                background_words: std::mem::take(&mut p_bg_words),
                            });
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(err) => return Err(TtmlParseError::Xml(err.to_string())),
            _ => {}
        }
    }

    if !open_tags.is_empty() {
        return Err(TtmlParseError::Xml(format!(
            "unexpected EOF: unclosed XML element(s): {}",
            open_tags.join(", ")
        )));
    }
    if !saw_tt_root {
        return Err(TtmlParseError::Xml(
            "document is missing required <tt> root element".to_string(),
        ));
    }

    if doc.lines.is_empty() {
        return Err(TtmlParseError::EmptyDocument);
    }

    let is_untimed = declared_timing_none || (!saw_any_word_span && !saw_any_line_time);

    if is_untimed {
        for line in &mut doc.lines {
            line.start_ms = 0;
            line.end_ms = 0;
            for w in &mut line.words {
                w.start_ms = 0;
                w.end_ms = 0;
            }
        }
        doc.format = LyricsFormat::PlainText;
        return Ok(doc);
    }

    // Ensure lines are ordered by start_ms and backfill missing end timestamps cleanly
    doc.lines.sort_by_key(|l| l.start_ms);
    for i in 0..doc.lines.len() {
        let next_start = doc.lines[i + 1..]
            .iter()
            .find(|l| l.start_ms > doc.lines[i].start_ms)
            .map(|l| l.start_ms);
        let line = &mut doc.lines[i];
        if line.end_ms <= line.start_ms {
            line.end_ms = next_start.unwrap_or_else(|| line.start_ms + 4_000);
        }
        let line_end = line.end_ms;
        backfill_word_end_times(&mut line.words, line_end);
        backfill_word_end_times(&mut line.background_words, line_end);
    }

    doc.format = if saw_any_word_span {
        LyricsFormat::Ttml
    } else {
        LyricsFormat::TtmlLine
    };

    Ok(doc)
}

/// Infer missing `end_ms` values by scanning ahead to the first subsequent word with a
/// strictly greater `start_ms` (properly handling duplicate-timestamp word groups),
/// falling back to `line_end` only for the final word(s) in the line.
fn backfill_word_end_times(words: &mut [LyricWord], line_end: u64) {
    let count = words.len();
    for w_idx in 0..count {
        if words[w_idx].end_ms <= words[w_idx].start_ms {
            let cur_start = words[w_idx].start_ms;
            let next_strict = words[w_idx + 1..]
                .iter()
                .find(|nw| nw.start_ms > cur_start)
                .map(|nw| nw.start_ms);
            words[w_idx].end_ms = next_strict.unwrap_or_else(|| line_end.max(cur_start + 250));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn append_text(
    text: &str,
    target: TextTarget,
    in_p: bool,
    title_buf: &mut String,
    sidecar_buf: &mut String,
    p_plain_buf: &mut String,
    span_stack: &mut [(TextTarget, PendingSpan)],
    had_space_since_last_word: &mut bool,
    p_words: &[LyricWord],
) {
    match target {
        TextTarget::Title => title_buf.push_str(text),
        TextTarget::HeadTranslation | TextTarget::HeadTransliteration => sidecar_buf.push_str(text),
        TextTarget::WordSpan
        | TextTarget::BgOuter
        | TextTarget::BgWordSpan
        | TextTarget::InlineTranslation
        | TextTarget::InlineRomanization => {
            if let Some((_, pending)) = span_stack.last_mut() {
                pending.text.push_str(text);
            }
        }
        TextTarget::ParagraphPlain if in_p => {
            if !p_words.is_empty() {
                if text.chars().any(char::is_whitespace) {
                    *had_space_since_last_word = true;
                }
            } else {
                p_plain_buf.push_str(text);
            }
        }
        _ => {}
    }
}

fn apply_amll_meta(e: &BytesStart<'_>, doc: &mut LyricsDocument) {
    let Some(key) = attr_value(e, &["key"]) else {
        return;
    };
    let Some(val) = attr_value(e, &["value"]) else {
        return;
    };
    let trimmed = val.trim();
    if trimmed.is_empty() {
        return;
    }
    match key.as_str() {
        "musicName" if doc.title.is_none() => doc.title = Some(trimmed.to_string()),
        "artists" if doc.artist.is_none() => doc.artist = Some(trimmed.to_string()),
        "album" if doc.album.is_none() => doc.album = Some(trimmed.to_string()),
        "isrc" if doc.isrc.is_none() => doc.isrc = Some(trimmed.to_string()),
        "ttmlAuthorGithubLogin" => {
            let attr = doc.attribution.get_or_insert_with(|| {
                Box::new(crate::lyrics::model::LyricsAttribution {
                    provider_label: doc.source.clone(),
                    ..Default::default()
                })
            });
            if attr.submitter_name.is_none() {
                attr.submitter_name = Some(format!("@{trimmed}"));
                attr.submitter_url = Some(format!("https://github.com/{trimmed}"));
            }
        }
        _ => {}
    }
}

fn local_name_of(raw: &str) -> String {
    match raw.rsplit_once(':') {
        Some((_, local)) => local.to_string(),
        None => raw.to_string(),
    }
}

fn attr_value(e: &BytesStart<'_>, local_names: &[&str]) -> Option<String> {
    for attr in e.attributes().flatten() {
        let name = local_name_of(attr.key.as_ref());
        if local_names.iter().any(|&cand| cand == name)
            && let Ok(val) = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
        {
            return Some(val.into_owned());
        }
    }
    None
}

fn resolve_xml_entity(name: &str) -> Option<String> {
    match name {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        s if s.starts_with("#x") || s.starts_with("#X") => u32::from_str_radix(&s[2..], 16)
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string()),
        s if s.starts_with('#') => s[1..]
            .parse::<u32>()
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string()),
        _ => None,
    }
}

fn resolve_end_ms(
    start_ms: u64,
    end_ms: Option<u64>,
    dur_ms: Option<u64>,
    fallback: Option<u64>,
) -> u64 {
    if let Some(end) = end_ms {
        return end.max(start_ms);
    }
    if let Some(dur) = dur_ms {
        return start_ms.saturating_add(dur);
    }
    fallback.unwrap_or(start_ms)
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_word_spacing(words: &mut [LyricWord]) {
    let len = words.len();
    for i in 0..len.saturating_sub(1) {
        if words[i].trailing_space {
            continue;
        }
        let cur_last = words[i].text.chars().last();
        let next_first = words[i + 1].text.chars().next();
        if let (Some(a), Some(b)) = (cur_last, next_first)
            && !is_cjk(a)
            && !is_cjk(b)
            && !matches!(b, ',' | '.' | '!' | '?' | ';' | ':' | ')' | ']' | '\'')
        {
            words[i].trailing_space = true;
        }
    }
    if let Some(last) = words.last_mut() {
        last.trailing_space = false;
    }
}

fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xFF00..=0xFFEF
    )
}

/// Parse TTML timestamps in clock or offset-time formats:
/// - `HH:MM:SS.fff`
/// - `MM:SS.fff`
/// - `SS.fff`
/// - `12.34s`, `1230ms`, `1.5m`, `0.5h`
pub fn parse_ttml_time(raw: &str) -> Option<u64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }

    if let Some(val) = s.strip_suffix("ms") {
        return val
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| v.max(0.0).round() as u64);
    }
    if let Some(val) = s.strip_suffix('s') {
        return val
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| (v.max(0.0) * 1000.0).round() as u64);
    }
    if let Some(val) = s.strip_suffix('m') {
        return val
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| (v.max(0.0) * 60_000.0).round() as u64);
    }
    if let Some(val) = s.strip_suffix('h') {
        return val
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| (v.max(0.0) * 3_600_000.0).round() as u64);
    }

    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        1 => parts[0]
            .parse::<f64>()
            .ok()
            .map(|sec| (sec.max(0.0) * 1000.0).round() as u64),
        2 => {
            let mins = parts[0].parse::<u64>().ok()?;
            let secs = parts[1].parse::<f64>().ok()?;
            Some(mins * 60_000 + (secs.max(0.0) * 1000.0).round() as u64)
        }
        3 => {
            let hours = parts[0].parse::<u64>().ok()?;
            let mins = parts[1].parse::<u64>().ok()?;
            let secs = parts[2].parse::<f64>().ok()?;
            Some(hours * 3_600_000 + mins * 60_000 + (secs.max(0.0) * 1000.0).round() as u64)
        }
        4 => {
            // HH:MM:SS:FF (assume 30fps if frame-based clock appears)
            let hours = parts[0].parse::<u64>().ok()?;
            let mins = parts[1].parse::<u64>().ok()?;
            let secs = parts[2].parse::<u64>().ok()?;
            let frames = parts[3].parse::<u64>().ok()?;
            Some(hours * 3_600_000 + mins * 60_000 + secs * 1000 + (frames * 1000) / 30)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ttml_time_formats() {
        assert_eq!(parse_ttml_time("00:01.500"), Some(1_500));
        assert_eq!(parse_ttml_time("01:02:03.250"), Some(3_723_250));
        assert_eq!(parse_ttml_time("12.34s"), Some(12_340));
        assert_eq!(parse_ttml_time("850ms"), Some(850));
        assert_eq!(parse_ttml_time("10.000"), Some(10_000));
    }

    #[test]
    fn parses_word_synced_ttml_with_sections_bg_and_sidecar_translation() {
        let xml = include_str!("../../assets/sample_lyrics/spotirust_demo.ttml");
        let doc = parse_ttml(xml, "unit-test").expect("sample TTML should parse cleanly");

        assert_eq!(doc.format, LyricsFormat::Ttml);
        assert_eq!(doc.title.as_deref(), Some("Midnight Compiler"));
        assert_eq!(doc.artist.as_deref(), Some("Spotirust Soundcheck"));
        assert_eq!(doc.lines.len(), 4);

        let first = &doc.lines[0];
        assert_eq!(first.section.as_deref(), Some("Verse"));
        assert_eq!(first.start_ms, 500);
        assert_eq!(first.end_ms, 4_200);
        assert_eq!(first.words.len(), 7);
        assert_eq!(first.words[0].text, "Pure");
        assert_eq!(first.words[0].start_ms, 500);
        assert_eq!(first.words[0].end_ms, 1_050);
        assert_eq!(
            first.translation.as_deref(),
            Some("Sóng âm thuần khiết lướt qua đêm tĩnh lặng")
        );

        let second = &doc.lines[1];
        assert_eq!(second.background_words.len(), 1);
        assert_eq!(second.background_words[0].text, "(right on time)");

        assert_eq!(doc.lines[2].section.as_deref(), Some("Chorus"));
        assert_eq!(doc.active_line_index(10_000), Some(2));
        assert_eq!(doc.active_line_index(1_000), Some(0));
    }

    #[test]
    fn handles_xml_entities_and_cjk_without_extra_spaces() {
        let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" itunes:timing="Word">
            <body><div>
                <p begin="00:01.000" end="00:03.000">
                    <span begin="00:01.000" end="00:01.500">Don&apos;t</span>
                    <span begin="00:01.500" end="00:02.000">stop</span>
                    <span begin="00:02.000" end="00:03.000">&amp; go</span>
                </p>
                <p begin="00:04.000" end="00:06.000">
                    <span begin="00:04.000" end="00:05.000">こ</span><span begin="00:05.000" end="00:06.000">れ</span>
                </p>
            </div></body>
        </tt>"#;

        let doc = parse_ttml(xml, "entity-test").unwrap();
        assert_eq!(doc.lines[0].plain_text(), "Don't stop & go");
        assert_eq!(doc.lines[1].plain_text(), "これ");
    }

    #[test]
    fn infers_missing_word_end_and_handles_duplicate_timestamps() {
        let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml">
            <body><div>
                <p begin="00:01.000" end="00:05.000">
                    <span begin="00:01.000">Hold</span>
                    <span begin="00:01.000">fast</span>
                    <span begin="00:02.000">now</span>
                    <span begin="00:03.500">end</span>
                </p>
            </div></body>
        </tt>"#;

        let doc = parse_ttml(xml, "edge-test").unwrap();
        assert_eq!(doc.format, LyricsFormat::Ttml);
        let words = &doc.lines[0].words;
        assert_eq!(words.len(), 4);
        // Words 0 & 1 share start_ms=1000 and must BOTH end at next strictly greater start (2000),
        // NOT stretch all the way to line end (5000)!
        assert_eq!((words[0].start_ms, words[0].end_ms), (1_000, 2_000));
        assert_eq!((words[1].start_ms, words[1].end_ms), (1_000, 2_000));
        assert_eq!((words[2].start_ms, words[2].end_ms), (2_000, 3_500));
        assert_eq!((words[3].start_ms, words[3].end_ms), (3_500, 5_000));
    }

    #[test]
    fn line_only_ttml_reports_ttml_line_and_rejects_malformed_xml() {
        let line_xml = r#"<tt xmlns="http://www.w3.org/ns/ttml">
            <body><div>
                <p begin="00:01.000">First line without end</p>
                <p begin="00:04.000">Second line</p>
            </div></body>
        </tt>"#;
        let doc = parse_ttml(line_xml, "line-only").unwrap();
        assert_eq!(doc.format, LyricsFormat::TtmlLine);
        assert!(!doc.has_word_timing());
        assert!(doc.format.is_time_synced());
        assert_eq!(doc.lines[0].end_ms, 4_000);
        assert_eq!(doc.lines[1].end_ms, 8_000);

        // Truncated XML with unclosed tags MUST return Xml error
        let truncated = r#"<tt><body><p begin="00:01.000">Unclosed"#;
        assert!(matches!(
            parse_ttml(truncated, "bad"),
            Err(TtmlParseError::Xml(_))
        ));

        // Mismatched tags MUST return Xml error
        let mismatched = r#"<tt><body><p begin="00:01.000">Bad</span></body></tt>"#;
        assert!(matches!(
            parse_ttml(mismatched, "bad"),
            Err(TtmlParseError::Xml(_))
        ));

        // Non-TTML root MUST return Xml error
        let not_ttml = r#"<html><body><p begin="00:01.000">Nope</p></body></html>"#;
        assert!(matches!(
            parse_ttml(not_ttml, "bad"),
            Err(TtmlParseError::Xml(_))
        ));
    }

    #[test]
    fn preserves_syllable_joining_in_unison_and_amll_ttml() {
        let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" xmlns:amll="http://www.example.com/ns/amll" itunes:timing="Word">
            <head><metadata>
                <amll:meta key="musicName" value="Never Gonna Stop"/>
                <amll:meta key="artists" value="Rick Astley"/>
                <amll:meta key="ttmlAuthorGithubLogin" value="demo_timer"/>
            </metadata></head>
            <body><div>
                <p begin="0:01.841" end="0:04.933" itunes:key="L1" ttm:agent="v1"><span begin="0:01.841" end="0:02.025">Can</span> <span begin="0:02.025" end="0:02.409">any</span><span begin="0:02.409" end="0:02.741">body</span> <span begin="0:02.741" end="0:03.068">stop</span> <span begin="0:03.068" end="0:03.485">this</span> <span begin="0:03.485" end="0:03.995">fee</span><span begin="0:03.995" end="0:04.933">ling?</span></p>
            </div></body>
        </tt>"#;

        let doc = parse_ttml(xml, "Unison").expect("syllable TTML must parse");
        assert_eq!(doc.format, LyricsFormat::Ttml);
        assert!(doc.has_word_timing());
        assert_eq!(doc.lines[0].words.len(), 7);
        assert_eq!(doc.lines[0].plain_text(), "Can anybody stop this feeling?");
        assert_eq!(
            doc.attribution
                .as_ref()
                .and_then(|a| a.submitter_name.as_deref()),
            Some("@demo_timer")
        );
    }

    #[test]
    fn classifies_untimed_lrc_red_ttml_as_plain_and_preserves_leading_span_spaces() {
        let untimed_xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:lrc="http://lrc.red/lyric-ttml-internal" lrc:timing="None" xml:lang="en">
            <body><div>
                <p>Maybe I don't really wanna know</p>
                <p>How your garden grows</p>
            </div></body>
        </tt>"#;
        let plain = parse_ttml(untimed_xml, "BiniLyrics").expect("untimed TTML must parse");
        assert_eq!(plain.format, LyricsFormat::PlainText);
        assert!(!plain.format.is_time_synced());
        assert_eq!(plain.lines.len(), 2);
        assert_eq!(
            plain.lines[0].plain_text(),
            "Maybe I don't really wanna know"
        );

        let leading_space_xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:lrc="http://lrc.red/lyric-ttml-internal" lrc:timing="Word">
            <body><div>
                <p begin="00:01.000" end="00:03.000"><span begin="00:01.000" end="00:01.800">Hello</span><span begin="00:01.800" end="00:03.000"> world</span></p>
            </div></body>
        </tt>"#;
        let timed = parse_ttml(leading_space_xml, "BiniLyrics").expect("word TTML must parse");
        assert_eq!(timed.format, LyricsFormat::Ttml);
        assert!(timed.has_word_timing());
        assert_eq!(timed.lines[0].plain_text(), "Hello world");
    }
}
