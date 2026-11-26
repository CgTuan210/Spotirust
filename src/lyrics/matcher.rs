//! Strict multi-factor track matcher for Spotify <-> external lyrics sources.
//!
//! Priority hierarchy:
//! 1. **ISRC exact match** (highest confidence; unambiguous)
//! 2. **Artist match**
//! 3. **Normalized Title similarity**
//! 4. **Album & version qualifiers** (strictly rejects studio vs live, remix, acoustic,
//!    sped-up, slowed, remastered, radio edit, extended, clean/censored, demo, karaoke, instrumental)
//! 5. **Duration proximity** (hard reject beyond `6_000ms` delta)
//!
//! Includes **ambiguity detection**: when multiple genuinely distinct versions score closely
//! without an ISRC match, returns `MatchResolution::Ambiguous` so the UI can let the user pick
//! the exact recording instead of silently displaying mismatched lyrics.

use crate::core::models::Track;
use std::collections::HashSet;

/// Minimum composite confidence score required for a candidate without ISRC match.
pub const MIN_MATCH_CONFIDENCE: f32 = 65.0;

/// Maximum duration difference (ms) allowed when both query and candidate specify durations.
pub const MAX_DURATION_DIFF_MS: u64 = 6_000;

/// Score gap below which two distinct non-ISRC candidates are flagged as ambiguous.
pub const AMBIGUITY_SCORE_MARGIN: f32 = 6.0;

/// Variant qualifiers that fundamentally change lyrical timing, intro length, or vocal content.
/// A studio track MUST NOT match a candidate carrying any different marker set (and vice versa).
const VARIANT_MARKERS: &[&str] = &[
    "live",
    "remix",
    "remixes",
    "remixed",
    "remaster",
    "remastered",
    "acoustic",
    "karaoke",
    "instrumental",
    "sped up",
    "sped-up",
    "speed up",
    "slowed",
    "slowed down",
    "reverb",
    "nightcore",
    "demo",
    "unplugged",
    "concert",
    "mashup",
    "cover",
    "club mix",
    "extended",
    "extended mix",
    "radio edit",
    "clean",
    "censored",
    "dub mix",
    "vip mix",
    "vip",
    "a cappella",
    "acapella",
    "orchestral",
    "symphonic",
    "session",
];

/// Query metadata describing the target Spotify/local track needing lyrics.
#[derive(Debug, Clone)]
pub struct TrackMatchQuery {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    pub isrc: Option<String>,
}

impl From<&Track> for TrackMatchQuery {
    fn from(t: &Track) -> Self {
        Self {
            title: t.title.clone(),
            artist: t.primary_artist().to_string(),
            album: t.album_name.clone(),
            duration_ms: t.duration_ms,
            isrc: t.isrc.clone(),
        }
    }
}

/// Candidate lyrics metadata returned by a provider.
#[derive(Debug, Clone)]
pub struct LyricCandidate {
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
    pub isrc: Option<String>,
    pub has_word_sync: bool,
    pub has_line_sync: bool,
}

/// Detailed scoring result for an accepted candidate, including non-sensitive match reasoning.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredMatch {
    pub index: usize,
    pub score: f32,
    pub isrc_matched: bool,
    pub reason: String,
}

/// Outcome of matching a track against a list of provider candidates.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchResolution {
    /// High-confidence unambiguous winner.
    Selected(ScoredMatch),
    /// Multiple distinct versions scored closely without an ISRC match — ask the user to choose.
    Ambiguous(Vec<ScoredMatch>),
    /// Every candidate failed safety or confidence thresholds.
    NoMatch,
}

/// Evaluates whether `candidate` is a valid match for `query` and returns detailed score + reason.
/// Returns `None` if the candidate violates duration, similarity, or variant-safety rules.
pub fn evaluate_candidate(
    query: &TrackMatchQuery,
    candidate: &LyricCandidate,
    index: usize,
) -> Option<ScoredMatch> {
    // 1. Exact ISRC match has highest priority and resolves recording identity uniquely.
    if let (Some(q_isrc), Some(c_isrc)) = (&query.isrc, &candidate.isrc)
        && !q_isrc.trim().is_empty()
        && q_isrc.trim().eq_ignore_ascii_case(c_isrc.trim())
    {
        let score = 180.0 + sync_bonus(candidate);
        let sync_tag = sync_label(candidate);
        return Some(ScoredMatch {
            index,
            score,
            isrc_matched: true,
            reason: format!("ISRC exact match ({sync_tag})"),
        });
    }

    // 2. Strict variant guard: studio vs live/remix/acoustic/sped-up/remastered/clean MUST match.
    let q_variants = extract_variant_markers(&query.title, Some(&query.album));
    let c_variants = extract_variant_markers(&candidate.title, candidate.album.as_deref());
    if q_variants != c_variants {
        return None;
    }

    // 3. Duration proximity check (hard reject when both durations known and differ by > 6 seconds).
    let mut duration_score = 0.0f32;
    let mut dur_diff_opt: Option<u64> = None;
    if query.duration_ms > 0
        && let Some(cand_dur) = candidate.duration_ms
        && cand_dur > 0
    {
        let diff_ms = query.duration_ms.abs_diff(cand_dur);
        if diff_ms > MAX_DURATION_DIFF_MS {
            return None;
        }
        dur_diff_opt = Some(diff_ms);
        duration_score = if diff_ms <= 1_500 {
            30.0
        } else if diff_ms <= 3_000 {
            20.0
        } else {
            8.0
        };
    }

    // 4. Artist match (prioritized before album)
    let q_artist = normalize_text(&query.artist);
    let c_artist = normalize_text(&candidate.artist);
    let artist_sim = if q_artist.is_empty() || c_artist.is_empty() {
        0.5
    } else if c_artist.contains(&q_artist) || q_artist.contains(&c_artist) {
        1.0
    } else {
        token_similarity(&q_artist, &c_artist)
    };
    if artist_sim < 0.45 {
        return None;
    }

    // 5. Normalized Title similarity
    let q_title = normalize_core_title(&query.title);
    let c_title = normalize_core_title(&candidate.title);
    let title_sim = token_similarity(&q_title, &c_title);
    if title_sim < 0.55 {
        return None;
    }

    // 6. Album similarity (tie-breaker for distinguishing studio releases vs compilations)
    let album_sim = match &candidate.album {
        Some(c_alb) if !query.album.is_empty() && !c_alb.is_empty() => {
            token_similarity(&normalize_text(&query.album), &normalize_text(c_alb))
        }
        _ => 0.0,
    };
    let album_score = album_sim * 15.0;

    let total =
        title_sim * 55.0 + artist_sim * 35.0 + duration_score + album_score + sync_bonus(candidate);
    if total < MIN_MATCH_CONFIDENCE {
        return None;
    }

    let dur_text = match dur_diff_opt {
        Some(d) => format!("Δdur={}ms", d),
        None => "dur=n/a".to_string(),
    };
    let reason = format!(
        "title={title_sim:.2}, artist={artist_sim:.2}, album={album_sim:.2}, {dur_text}, {}",
        sync_label(candidate)
    );

    Some(ScoredMatch {
        index,
        score: total,
        isrc_matched: false,
        reason,
    })
}

/// Evaluates whether `candidate` is a valid match for `query` and returns a confidence score (`0..=200`).
/// Returns `None` if the candidate violates duration or variant-safety rules.
pub fn score_candidate(query: &TrackMatchQuery, candidate: &LyricCandidate) -> Option<f32> {
    evaluate_candidate(query, candidate, 0).map(|m| m.score)
}

/// Select the highest-scoring valid candidate index from a list.
pub fn select_best_candidate(
    query: &TrackMatchQuery,
    candidates: &[LyricCandidate],
) -> Option<(usize, f32)> {
    candidates
        .iter()
        .enumerate()
        .filter_map(|(idx, cand)| score_candidate(query, cand).map(|score| (idx, score)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// Resolve candidates with confidence scoring, ambiguity detection, and structured tracing logs
/// (never logs tokens or credentials).
pub fn resolve_candidates(
    query: &TrackMatchQuery,
    candidates: &[LyricCandidate],
) -> MatchResolution {
    let mut scored: Vec<ScoredMatch> = candidates
        .iter()
        .enumerate()
        .filter_map(|(idx, cand)| evaluate_candidate(query, cand, idx))
        .collect();

    if scored.is_empty() {
        tracing::info!(
            track = %query.title,
            artist = %query.artist,
            candidates = candidates.len(),
            "Lyrics matcher rejected all candidates"
        );
        return MatchResolution::NoMatch;
    }

    scored.sort_by(|a, b| b.score.total_cmp(&a.score));

    // Exact ISRC hit is unambiguous
    if scored[0].isrc_matched {
        let winner = scored.remove(0);
        tracing::info!(
            track = %query.title,
            artist = %query.artist,
            score = winner.score,
            reason = %winner.reason,
            "Lyrics matcher selected unambiguous ISRC match"
        );
        return MatchResolution::Selected(winner);
    }

    // Check whether top two candidates are distinct releases with closely tied confidence scores
    if scored.len() >= 2 && (scored[0].score - scored[1].score) < AMBIGUITY_SCORE_MARGIN {
        let first_cand = &candidates[scored[0].index];
        let second_cand = &candidates[scored[1].index];
        if are_distinct_versions(first_cand, second_cand) {
            let top_score = scored[0].score;
            let close: Vec<ScoredMatch> = scored
                .into_iter()
                .filter(|m| top_score - m.score < AMBIGUITY_SCORE_MARGIN)
                .take(5)
                .collect();
            tracing::info!(
                track = %query.title,
                artist = %query.artist,
                ambiguous_count = close.len(),
                top_score,
                "Lyrics matcher flagged ambiguous candidates for user confirmation"
            );
            return MatchResolution::Ambiguous(close);
        }
    }

    let winner = scored.remove(0);
    tracing::info!(
        track = %query.title,
        artist = %query.artist,
        score = winner.score,
        reason = %winner.reason,
        "Lyrics matcher selected best candidate"
    );
    MatchResolution::Selected(winner)
}

/// Determine whether two candidates represent genuinely different recordings/releases
/// (rather than duplicate uploads of the same track with identical metadata).
fn are_distinct_versions(a: &LyricCandidate, b: &LyricCandidate) -> bool {
    let dur_differs = match (a.duration_ms, b.duration_ms) {
        (Some(da), Some(db)) => da.abs_diff(db) > 1_500,
        _ => false,
    };
    let album_differs = match (&a.album, &b.album) {
        (Some(aa), Some(ab)) => {
            let na = normalize_text(aa);
            let nb = normalize_text(ab);
            !na.is_empty() && !nb.is_empty() && na != nb
        }
        _ => false,
    };
    let title_differs = normalize_text(&a.title) != normalize_text(&b.title);
    dur_differs || album_differs || title_differs
}

fn sync_bonus(cand: &LyricCandidate) -> f32 {
    if cand.has_word_sync {
        14.0
    } else if cand.has_line_sync {
        8.0
    } else {
        0.0
    }
}

fn sync_label(cand: &LyricCandidate) -> &'static str {
    if cand.has_word_sync {
        "word-synced"
    } else if cand.has_line_sync {
        "line-synced"
    } else {
        "plain"
    }
}

/// Extract canonical version-variant markers (e.g. `live`, `remix`, `acoustic`, `remastered`)
/// from title & album.
pub fn extract_variant_markers(title: &str, album: Option<&str>) -> HashSet<&'static str> {
    let mut found = HashSet::new();
    let combined = match album {
        Some(alb) => format!(
            "{} {}",
            title.to_ascii_lowercase(),
            alb.to_ascii_lowercase()
        ),
        None => title.to_ascii_lowercase(),
    };
    let padded = format!(
        " {} ",
        combined
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect::<String>()
    );

    for &marker in VARIANT_MARKERS {
        let needle = format!(" {marker} ");
        if padded.contains(&needle) {
            // Canonicalize equivalent variant words so "remaster" == "remastered", "sped-up" == "sped up"
            let canonical = match marker {
                "remaster" | "remastered" => "remastered",
                "remix" | "remixes" | "remixed" => "remix",
                "sped up" | "sped-up" | "speed up" => "sped_up",
                "slowed" | "slowed down" => "slowed",
                "a cappella" | "acapella" => "acapella",
                "extended" | "extended mix" => "extended",
                "vip" | "vip mix" => "vip",
                "clean" | "censored" => "clean",
                other => other,
            };
            found.insert(canonical);
        }
    }
    found
}

/// Normalize a track title by lowercasing and stripping featured-artist suffixes
/// (variant checks run separately so remastered/live/remix are never silently erased).
pub fn normalize_core_title(title: &str) -> String {
    let lower = title.to_ascii_lowercase();
    let stripped = lower
        .split(" - ")
        .next()
        .unwrap_or(&lower)
        .split("(feat.")
        .next()
        .unwrap_or(&lower)
        .split("(ft.")
        .next()
        .unwrap_or(&lower);
    normalize_text(stripped)
}

pub fn normalize_text(input: &str) -> String {
    input
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn token_similarity(a: &str, b: &str) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let set_a: HashSet<&str> = a.split_whitespace().collect();
    let set_b: HashSet<&str> = b.split_whitespace().collect();
    let intersection = set_a.intersection(&set_b).count() as f32;
    let union = set_a.union(&set_b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn studio_track_rejects_remix_live_acoustic_sped_up_and_remastered() {
        let studio = TrackMatchQuery {
            title: "Starlight".to_string(),
            artist: "Aurora".to_string(),
            album: "Nightfall".to_string(),
            duration_ms: 210_000,
            isrc: None,
        };

        let remix = LyricCandidate {
            title: "Starlight (Club Remix)".to_string(),
            artist: "Aurora".to_string(),
            album: Some("Nightfall Remixes".to_string()),
            duration_ms: Some(210_000),
            isrc: None,
            has_word_sync: true,
            has_line_sync: true,
        };

        let live = LyricCandidate {
            title: "Starlight - Live at Tokyo Dome".to_string(),
            artist: "Aurora".to_string(),
            album: Some("Live Tour".to_string()),
            duration_ms: Some(211_000),
            isrc: None,
            has_word_sync: true,
            has_line_sync: true,
        };

        let remastered = LyricCandidate {
            title: "Starlight (2024 Remastered)".to_string(),
            artist: "Aurora".to_string(),
            album: Some("Nightfall (Remaster)".to_string()),
            duration_ms: Some(210_200),
            isrc: None,
            has_word_sync: true,
            has_line_sync: true,
        };

        let acoustic = LyricCandidate {
            title: "Starlight (Acoustic)".to_string(),
            artist: "Aurora".to_string(),
            album: Some("Nightfall".to_string()),
            duration_ms: Some(210_000),
            isrc: None,
            has_word_sync: true,
            has_line_sync: true,
        };

        let exact = LyricCandidate {
            title: "Starlight".to_string(),
            artist: "Aurora".to_string(),
            album: Some("Nightfall".to_string()),
            duration_ms: Some(210_800),
            isrc: None,
            has_word_sync: false,
            has_line_sync: true,
        };

        assert!(score_candidate(&studio, &remix).is_none());
        assert!(score_candidate(&studio, &live).is_none());
        assert!(score_candidate(&studio, &remastered).is_none());
        assert!(score_candidate(&studio, &acoustic).is_none());
        assert!(score_candidate(&studio, &exact).is_some());

        let best = select_best_candidate(&studio, &[remix, live, remastered, acoustic, exact]);
        assert_eq!(best.map(|(i, _)| i), Some(4));
    }

    #[test]
    fn rejects_large_duration_mismatch_and_honors_isrc() {
        let query = TrackMatchQuery {
            title: "Echoes".to_string(),
            artist: "Horizon".to_string(),
            album: "Waves".to_string(),
            duration_ms: 200_000,
            isrc: Some("USRC12345678".to_string()),
        };

        let wrong_len = LyricCandidate {
            title: "Echoes".to_string(),
            artist: "Horizon".to_string(),
            album: Some("Waves".to_string()),
            duration_ms: Some(260_000),
            isrc: None,
            has_word_sync: false,
            has_line_sync: true,
        };
        assert!(score_candidate(&query, &wrong_len).is_none());

        let isrc_hit = LyricCandidate {
            title: "Echoes (2025 Master)".to_string(),
            artist: "Horizon".to_string(),
            album: None,
            duration_ms: Some(201_000),
            isrc: Some("USRC12345678".to_string()),
            has_word_sync: true,
            has_line_sync: true,
        };
        assert!(score_candidate(&query, &isrc_hit).unwrap() > 180.0);
        assert!(matches!(
            resolve_candidates(&query, &[wrong_len, isrc_hit]),
            MatchResolution::Selected(m) if m.index == 1 && m.isrc_matched
        ));
    }

    #[test]
    fn flags_ambiguous_close_scoring_distinct_editions_for_user_selection() {
        let query = TrackMatchQuery {
            title: "Midnight Sun".to_string(),
            artist: "Solstice".to_string(),
            album: "Greatest Hits".to_string(),
            duration_ms: 200_000,
            isrc: None,
        };

        let edition_a = LyricCandidate {
            title: "Midnight Sun".to_string(),
            artist: "Solstice".to_string(),
            album: Some("First Light".to_string()),
            duration_ms: Some(200_500),
            isrc: None,
            has_word_sync: false,
            has_line_sync: true,
        };

        let edition_b = LyricCandidate {
            title: "Midnight Sun".to_string(),
            artist: "Solstice".to_string(),
            album: Some("Second Dawn".to_string()),
            duration_ms: Some(199_400),
            isrc: None,
            has_word_sync: false,
            has_line_sync: true,
        };

        match resolve_candidates(&query, &[edition_a, edition_b]) {
            MatchResolution::Ambiguous(matches) => {
                assert_eq!(matches.len(), 2);
            }
            other => panic!("expected Ambiguous match resolution, got {other:?}"),
        }
    }
}
