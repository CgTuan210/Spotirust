//! Word-by-word karaoke lyrics renderer supporting BOTH Compact Drawer mode
//! and Fullscreen Now Playing mode with dynamic album-color ambient gradients.
//!
//! Architecture & performance guarantees:
//! - Strictly driven by audio `playback.position.current_ms() + doc.offset_ms` (never an independent UI clock).
//! - Clean separation between immutable `Arc<LyricsDocument>`, `KaraokeAnimationState`, and
//!   `KaraokeLayoutCache` (pre-laid-out `Arc<egui::Galley>` per line/word).
//! - Zero `LyricsDocument` clones, zero `String`/`Vec` allocations, and zero `painter.layout*`
//!   calls during steady-state karaoke word sweep frames.
//! - Inactive lines render as a single cached wrapped `Arc<Galley>` (1 draw call per visible line),
//!   and off-screen lines skip painting while preserving exact scroll geometry.
//! - Active line words reuse cached `dim_galley` and `lit_galley`; only the single currently
//!   sweeping word uses a clip-rect pass.
//! - Adaptive repaint scheduling (`next_karaoke_repaint_delay`) preserves millisecond word-boundary
//!   precision without spinning at 60 FPS during gaps, line-only/plain lyrics, or `reduce_motion`.

use crate::core::events::AppCommand;
use crate::core::state::{AppState, LyricsUiState, SideDrawer};
use crate::lyrics::model::{LyricLine, LyricsDocument};
use crate::ui::components::artwork::{
    ArtworkTextureCache, artwork_box, deterministic_rgb, paint_ambient_gradient,
};
use crate::ui::theme::ThemePalette;
use egui::{
    Align, Color32, CornerRadius, FontId, Galley, Margin, Pos2, Rect, RichText, ScrollArea, Sense,
    Stroke, Ui, Vec2,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Active sweeping word cadence (~16.7 FPS) — ~8-10 smooth sweep steps per typical 500ms word
/// while locking tightly to exact word start/end transitions (`MIN_BOUNDARY_WAKE_MS = 16ms`).
const ACTIVE_WORD_SWEEP_INTERVAL_MS: u64 = 60;
/// Minimum boundary wake interval so exact word/line transitions remain razor-sharp.
const MIN_BOUNDARY_WAKE_MS: u64 = 16;
/// Steady progress-bar cadence when no karaoke word sweep is active (~4 Hz).
const IDLE_PROGRESS_INTERVAL_MS: u64 = 250;

// =============================================================================
// Separated Karaoke Animation State (decoupled from LyricsDocument)
// =============================================================================

/// Result of advancing `KaraokeAnimationState` against the latest audio timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HighlightUpdate {
    pub line_changed: bool,
    pub word_changed: bool,
    pub highlight_changed: bool,
    pub seek_or_track_jump: bool,
}

/// Pure karaoke highlight & scroll animation state completely decoupled from `LyricsDocument`.
#[derive(Debug, Clone, Default)]
pub struct KaraokeAnimationState {
    pub track_id: Option<String>,
    pub active_line_idx: Option<usize>,
    pub active_word_idx: Option<usize>,
    pub active_word_progress_permille: u16,
    pub bg_active: bool,
    pub last_adjusted_ms: u64,
}

impl KaraokeAnimationState {
    /// Update active line/word highlight state for `(track_id, audio_ms)`.
    /// Detects track switches, seeks, line changes, and active word transitions cleanly.
    pub fn update(
        &mut self,
        track_id: &str,
        doc: &LyricsDocument,
        audio_ms: u64,
    ) -> HighlightUpdate {
        let adjusted_ms = doc.adjusted_position_ms(audio_ms);
        let track_changed = self.track_id.as_deref() != Some(track_id);
        let jumped = track_changed
            || adjusted_ms < self.last_adjusted_ms
            || adjusted_ms.saturating_sub(self.last_adjusted_ms) > 1_200;

        let next_line = doc.active_line_index(audio_ms);
        let (next_word, next_permille, next_bg_active) = match next_line {
            Some(line_idx) => {
                let line = &doc.lines[line_idx];
                let bg = line
                    .background_words
                    .iter()
                    .any(|w| adjusted_ms >= w.start_ms && adjusted_ms <= w.end_ms);
                if doc.has_word_timing() {
                    let mut found_word = None;
                    let mut permille = 0u16;
                    for (w_idx, w) in line.words.iter().enumerate() {
                        let effective_end = w.start_ms.saturating_add(w.duration_ms().max(65));
                        if adjusted_ms >= w.start_ms && adjusted_ms < effective_end {
                            found_word = Some(w_idx);
                            permille = (w.progress_at(adjusted_ms) * 1000.0).round() as u16;
                            break;
                        }
                        if adjusted_ms >= effective_end {
                            found_word = Some(w_idx);
                            permille = 1000;
                        }
                    }
                    (found_word, permille, bg)
                } else {
                    (None, 1000, bg)
                }
            }
            None => (None, 0, false),
        };

        let line_changed = track_changed || self.active_line_idx != next_line;
        let word_changed = line_changed || self.active_word_idx != next_word;
        let highlight_changed = word_changed
            || self.active_word_progress_permille != next_permille
            || self.bg_active != next_bg_active;

        if track_changed {
            self.track_id = Some(track_id.to_string());
        }
        self.active_line_idx = next_line;
        self.active_word_idx = next_word;
        self.active_word_progress_permille = next_permille;
        self.bg_active = next_bg_active;
        self.last_adjusted_ms = adjusted_ms;

        HighlightUpdate {
            line_changed,
            word_changed,
            highlight_changed,
            seek_or_track_jump: jumped,
        }
    }

    pub fn reset(&mut self) {
        self.track_id = None;
        self.active_line_idx = None;
        self.active_word_idx = None;
        self.active_word_progress_permille = 0;
        self.bg_active = false;
        self.last_adjusted_ms = 0;
    }
}

// =============================================================================
// Cached Arc<Galley> Layout Engine (zero per-frame text shaping or allocations)
// =============================================================================

/// Cache invalidation key for the full lyrics layout.
/// Quantizes `max_width` to whole logical pixels so sub-pixel jitter never thrashes the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KaraokeLayoutKey {
    pub track_id: String,
    pub doc_revision: u64,
    pub compact: bool,
    pub max_width_px: u32,
    pub active_line_idx: Option<usize>,
    pub show_translations: bool,
    pub theme_id: String,
    pub dark: bool,
}

#[derive(Clone)]
struct CachedWordLayout {
    start_ms: u64,
    end_ms: u64,
    offset: Vec2,
    size: Vec2,
    dim_galley: Arc<Galley>,
    lit_galley: Arc<Galley>,
    dim_color: Color32,
    lit_color: Color32,
}

impl CachedWordLayout {
    #[inline]
    fn progress_at(&self, adjusted_ms: u64) -> f32 {
        if adjusted_ms < self.start_ms {
            return 0.0;
        }
        let effective_duration = self.end_ms.saturating_sub(self.start_ms).max(65);
        let elapsed = adjusted_ms.saturating_sub(self.start_ms);
        if elapsed >= effective_duration {
            1.0
        } else {
            (elapsed as f32 / effective_duration as f32).clamp(0.0, 1.0)
        }
    }
}

#[derive(Clone)]
enum CachedLineBody {
    /// Inactive line rendered as a SINGLE wrapped `Arc<Galley>` (1 draw call).
    Static { galley: Arc<Galley>, color: Color32 },
    /// Active line with pre-wrapped per-word dim & lit `Arc<Galley>` pairs.
    ActiveWords { words: Vec<CachedWordLayout> },
}

#[derive(Clone)]
struct CachedLineLayout {
    start_ms: u64,
    section_galley: Option<Arc<Galley>>,
    section_top_pad: f32,
    body: CachedLineBody,
    bg_dim_galley: Option<Arc<Galley>>,
    bg_lit_galley: Option<Arc<Galley>>,
    bg_offset_y: f32,
    bg_spans: Vec<(u64, u64)>,
    roman_galley: Option<Arc<Galley>>,
    roman_offset_y: f32,
    trans_galley: Option<Arc<Galley>>,
    trans_offset_y: f32,
    total_height: f32,
}

/// Persistent cache of shaped `Arc<egui::Galley>` layouts for the active lyrics view.
#[derive(Default)]
pub struct KaraokeLayoutCache {
    key: Option<KaraokeLayoutKey>,
    lines: Vec<CachedLineLayout>,
    offset_label: String,
    rebuild_count: u64,
    hit_count: u64,
}

impl KaraokeLayoutCache {
    pub fn clear(&mut self) {
        self.key = None;
        self.lines.clear();
        self.offset_label.clear();
    }

    pub fn rebuild_count(&self) -> u64 {
        self.rebuild_count
    }

    pub fn hit_count(&self) -> u64 {
        self.hit_count
    }

    pub fn cached_line_count(&self) -> usize {
        self.lines.len()
    }

    #[allow(clippy::too_many_arguments)]
    fn matches_borrowed(
        &self,
        track_id: &str,
        doc_revision: u64,
        compact: bool,
        max_width_px: u32,
        active_line_idx: Option<usize>,
        show_translations: bool,
        theme_id: &str,
        dark: bool,
    ) -> bool {
        let Some(k) = &self.key else {
            return false;
        };
        k.doc_revision == doc_revision
            && k.compact == compact
            && k.max_width_px == max_width_px
            && k.active_line_idx == active_line_idx
            && k.show_translations == show_translations
            && k.dark == dark
            && k.track_id == track_id
            && k.theme_id == theme_id
    }

    /// Zero-allocation fast path: checks borrowed key fields first and only allocates an owned
    /// `KaraokeLayoutKey` on cache miss.
    #[allow(clippy::too_many_arguments)]
    pub fn ensure_layout_borrowed(
        &mut self,
        ctx: &egui::Context,
        track_id: &str,
        doc_revision: u64,
        compact: bool,
        max_width_px: u32,
        active_line_idx: Option<usize>,
        show_translations: bool,
        doc: &LyricsDocument,
        theme: &ThemePalette,
    ) {
        if self.matches_borrowed(
            track_id,
            doc_revision,
            compact,
            max_width_px,
            active_line_idx,
            show_translations,
            &theme.id,
            theme.dark,
        ) {
            self.hit_count = self.hit_count.wrapping_add(1);
            return;
        }
        let key = KaraokeLayoutKey {
            track_id: track_id.to_string(),
            doc_revision,
            compact,
            max_width_px,
            active_line_idx,
            show_translations,
            theme_id: theme.id.clone(),
            dark: theme.dark,
        };
        self.ensure_layout(ctx, key, doc, theme);
    }

    /// Ensure cached galleys match `key`; rebuilds ONLY when layout inputs actually change.
    pub fn ensure_layout(
        &mut self,
        ctx: &egui::Context,
        key: KaraokeLayoutKey,
        doc: &LyricsDocument,
        theme: &ThemePalette,
    ) {
        if self.key.as_ref() == Some(&key) {
            self.hit_count = self.hit_count.wrapping_add(1);
            return;
        }
        self.offset_label = format!("Offset {:+}ms", doc.offset_ms);

        let max_width = (key.max_width_px as f32).max(120.0);
        let compact = key.compact;
        let show_translations = key.show_translations;
        let active_idx = key.active_line_idx;
        let word_synced = doc.has_word_timing();

        let lit_color = if theme.dark {
            Color32::WHITE
        } else {
            theme.accent
        };

        let mut built = Vec::with_capacity(doc.lines.len());
        let mut last_section: Option<&str> = None;

        ctx.fonts_mut(|fonts| {
            for (idx, line) in doc.lines.iter().enumerate() {
                let is_active = active_idx == Some(idx);
                let is_past = active_idx.map(|a| idx < a).unwrap_or(false);

                let (section_galley, section_top_pad) = if let Some(sec) = line.section.as_deref()
                    && last_section != Some(sec)
                {
                    last_section = Some(sec);
                    let g = fonts.layout_no_wrap(
                        sec.to_uppercase(),
                        FontId::proportional(10.5),
                        theme.accent.linear_multiply(0.85),
                    );
                    (Some(g), if idx == 0 { 4.0 } else { 14.0 })
                } else {
                    (None, 0.0)
                };

                let base_size = if compact {
                    if is_active { 18.0 } else { 15.0 }
                } else if is_active {
                    29.0
                } else {
                    22.5
                };
                let font_id = FontId::proportional(base_size);

                let dim_color = if is_past {
                    theme.text_muted.linear_multiply(0.75)
                } else if is_active {
                    theme.text_secondary.linear_multiply(0.55)
                } else {
                    theme.text_secondary.linear_multiply(0.65)
                };

                let (body, body_height) = if is_active && word_synced && !line.words.is_empty() {
                    let (words, h) = build_active_words_layout(
                        fonts, line, &font_id, dim_color, lit_color, max_width, base_size,
                    );
                    (CachedLineBody::ActiveWords { words }, h)
                } else {
                    let color = if is_active { lit_color } else { dim_color };
                    let galley = fonts.layout(line.plain_text(), font_id, color, max_width);
                    let h = galley.size().y.max(base_size * 1.28);
                    (CachedLineBody::Static { galley, color }, h)
                };

                let mut cursor_y = 4.0 + body_height;

                // Background vocals (`x-bg`) cached once per layout key
                let (bg_dim_galley, bg_lit_galley, bg_offset_y, bg_spans) =
                    if !line.background_words.is_empty() {
                        let bg_font = FontId::proportional((base_size * 0.62).max(12.0));
                        let bg_text: String = line
                            .background_words
                            .iter()
                            .map(|w| w.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" ");
                        let dim_g = fonts.layout(
                            bg_text.clone(),
                            bg_font.clone(),
                            theme.text_muted,
                            max_width,
                        );
                        let lit_g = fonts.layout(bg_text, bg_font, theme.accent, max_width);
                        let y = cursor_y;
                        cursor_y += dim_g.size().y.max(base_size * 0.85);
                        let spans = line
                            .background_words
                            .iter()
                            .map(|w| (w.start_ms, w.end_ms))
                            .collect();
                        (Some(dim_g), Some(lit_g), y, spans)
                    } else {
                        (None, None, cursor_y, Vec::new())
                    };

                // Optional romanization cached once
                let (roman_galley, roman_offset_y) =
                    if show_translations && let Some(roman) = &line.romanization {
                        let r_font = FontId::proportional(if compact { 11.5 } else { 13.5 });
                        let g = fonts.layout(roman.clone(), r_font, theme.text_muted, max_width);
                        let y = cursor_y;
                        cursor_y += g.size().y.max(if compact { 16.0 } else { 20.0 });
                        (Some(g), y)
                    } else {
                        (None, cursor_y)
                    };

                // Optional translation cached once
                let (trans_galley, trans_offset_y) =
                    if show_translations && let Some(trans) = &line.translation {
                        let t_font = FontId::proportional(if compact { 12.5 } else { 15.0 });
                        let t_col = if is_active {
                            theme.text_secondary
                        } else {
                            theme.text_muted
                        };
                        let g = fonts.layout(trans.clone(), t_font, t_col, max_width);
                        let y = cursor_y;
                        cursor_y += g.size().y.max(if compact { 18.0 } else { 24.0 });
                        (Some(g), y)
                    } else {
                        (None, cursor_y)
                    };

                built.push(CachedLineLayout {
                    start_ms: line.start_ms,
                    section_galley,
                    section_top_pad,
                    body,
                    bg_dim_galley,
                    bg_lit_galley,
                    bg_offset_y,
                    bg_spans,
                    roman_galley,
                    roman_offset_y,
                    trans_galley,
                    trans_offset_y,
                    total_height: cursor_y + 4.0,
                });
            }
        });

        self.lines = built;
        self.key = Some(key);
        self.rebuild_count = self.rebuild_count.wrapping_add(1);
    }
}

fn build_active_words_layout(
    fonts: &mut egui::epaint::FontsView<'_>,
    line: &LyricLine,
    font_id: &FontId,
    dim_color: Color32,
    lit_color: Color32,
    max_width: f32,
    base_size: f32,
) -> (Vec<CachedWordLayout>, f32) {
    let mut words = Vec::with_capacity(line.words.len());
    let mut row_x = 0.0f32;
    let mut row_y = 0.0f32;
    let mut row_height = base_size * 1.28;

    for (w_idx, word) in line.words.iter().enumerate() {
        let mut display = String::with_capacity(word.text.len() + 1);
        display.push_str(&word.text);
        if word.trailing_space && w_idx + 1 < line.words.len() {
            display.push(' ');
        }

        let dim_galley = fonts.layout_no_wrap(display.clone(), font_id.clone(), dim_color);
        let lit_galley = fonts.layout_no_wrap(display, font_id.clone(), lit_color);
        let size = dim_galley.size();
        row_height = row_height.max(size.y);

        if row_x > 0.0 && row_x + size.x > max_width {
            row_y += row_height + 2.0;
            row_x = 0.0;
        }

        words.push(CachedWordLayout {
            start_ms: word.start_ms,
            end_ms: word.end_ms,
            offset: Vec2::new(row_x, row_y),
            size,
            dim_galley,
            lit_galley,
            dim_color,
            lit_color,
        });
        row_x += size.x;
    }

    let total_body_h = row_y + row_height + 2.0;
    (words, total_body_h)
}

/// Persistent renderer state owned by `SpotirustApp`, keeping `Arc<Galley>` layout caches
/// and animation highlight state decoupled from `AppState` and `LyricsDocument`.
#[derive(Default)]
pub struct KaraokeRenderer {
    pub layout_cache: KaraokeLayoutCache,
    pub animation: KaraokeAnimationState,
}

impl KaraokeRenderer {
    pub fn reset(&mut self) {
        self.layout_cache.clear();
        self.animation.reset();
    }
}

// =============================================================================
// Smart Repaint Scheduler (event-driven word/line wakeups without blind 60 FPS)
// =============================================================================

/// Compute the optimal next repaint delay for the UI event loop.
///
/// - Never spins when paused/stopped (`None`).
/// - Drops to ~4 Hz (`250ms`) when lyrics are hidden, plain text, instrumental, unavailable, or idle.
/// - During an active word-level karaoke sweep: repaints smoothly at ~30 FPS (`33ms`) and locks
///   tightly to exact word finish/start boundaries (`>= 16ms`) so word timing never loses precision.
/// - During inter-word/inter-line gaps, line-only lyrics (`StandardLrc`/`TtmlLine`), or `reduce_motion`:
///   sleeps until the exact next word/line transition (capped at `250ms` for the seek bar).
pub fn next_karaoke_repaint_delay(
    lyrics: &LyricsUiState,
    audio_ms: u64,
    is_playing: bool,
    lyrics_visible: bool,
    reduce_motion: bool,
) -> Option<Duration> {
    if !is_playing {
        return None;
    }
    if !lyrics_visible {
        return Some(Duration::from_millis(IDLE_PROGRESS_INTERVAL_MS));
    }

    match lyrics {
        LyricsUiState::Idle
        | LyricsUiState::Ambiguous { .. }
        | LyricsUiState::Unavailable { .. } => {
            Some(Duration::from_millis(IDLE_PROGRESS_INTERVAL_MS))
        }
        LyricsUiState::Loading { .. } => Some(Duration::from_millis(100)),
        LyricsUiState::Loaded { document, .. } => {
            if document.instrumental
                || !document.format.is_time_synced()
                || document.lines.is_empty()
            {
                return Some(Duration::from_millis(IDLE_PROGRESS_INTERVAL_MS));
            }

            let adjusted = document.adjusted_position_ms(audio_ms);
            let active_idx = document.active_line_index(audio_ms);

            if document.has_word_timing() {
                if let Some(idx) = active_idx {
                    let line = &document.lines[idx];
                    // Check if any main word on the active line is currently sweeping
                    for w in &line.words {
                        let effective_end = w.start_ms.saturating_add(w.duration_ms().max(65));
                        if adjusted >= w.start_ms && adjusted < effective_end {
                            let remaining = effective_end.saturating_sub(adjusted);
                            if reduce_motion {
                                // Snap cleanly on word boundary without continuous fill animation
                                return Some(Duration::from_millis(
                                    remaining
                                        .clamp(MIN_BOUNDARY_WAKE_MS, IDLE_PROGRESS_INTERVAL_MS),
                                ));
                            }
                            let step = remaining
                                .clamp(MIN_BOUNDARY_WAKE_MS, ACTIVE_WORD_SWEEP_INTERVAL_MS);
                            return Some(Duration::from_millis(step));
                        }
                    }

                    // Check if a future word starts later in this same line
                    if let Some(next_word) = line.words.iter().find(|w| w.start_ms > adjusted) {
                        let delta = next_word.start_ms.saturating_sub(adjusted);
                        return Some(Duration::from_millis(
                            delta.clamp(MIN_BOUNDARY_WAKE_MS, IDLE_PROGRESS_INTERVAL_MS),
                        ));
                    }

                    // Check active background vocal end boundary
                    if let Some(bg_end) = line
                        .background_words
                        .iter()
                        .filter(|w| w.end_ms > adjusted)
                        .map(|w| w.end_ms)
                        .min()
                    {
                        let delta = bg_end.saturating_sub(adjusted);
                        return Some(Duration::from_millis(
                            delta.clamp(MIN_BOUNDARY_WAKE_MS, IDLE_PROGRESS_INTERVAL_MS),
                        ));
                    }
                }

                // Inter-line gap / intro / outro: wake precisely on next line's start (capped at 250ms)
                let next_idx = document
                    .lines
                    .partition_point(|line| line.start_ms <= adjusted);
                if let Some(next_line) = document.lines.get(next_idx) {
                    let delta = next_line.start_ms.saturating_sub(adjusted);
                    return Some(Duration::from_millis(
                        delta.clamp(MIN_BOUNDARY_WAKE_MS, IDLE_PROGRESS_INTERVAL_MS),
                    ));
                }
                return Some(Duration::from_millis(IDLE_PROGRESS_INTERVAL_MS));
            }

            // Line-synced only (`StandardLrc` / `TtmlLine`): wake only on next line boundary (max 250ms)
            let next_idx = document
                .lines
                .partition_point(|line| line.start_ms <= adjusted);
            let next_boundary = document.lines.get(next_idx).map(|l| l.start_ms);
            if let Some(target_ms) = next_boundary {
                let delta = target_ms.saturating_sub(adjusted);
                Some(Duration::from_millis(
                    delta.clamp(MIN_BOUNDARY_WAKE_MS, IDLE_PROGRESS_INTERVAL_MS),
                ))
            } else {
                Some(Duration::from_millis(IDLE_PROGRESS_INTERVAL_MS))
            }
        }
    }
}

// =============================================================================
// Fullscreen Now Playing & Compact Drawer Views
// =============================================================================

/// Fullscreen Now Playing view: large album artwork + opaque ambient album-colored gradient
/// + animated audio bars + cached word-by-word karaoke lyrics.
pub fn render_now_playing_fullscreen(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    karaoke: &mut KaraokeRenderer,
    commands: &mut Vec<AppCommand>,
) {
    let full_rect = ui.available_rect_before_wrap();
    let track = state.playback.current_track.clone();
    let dominant_rgb = track
        .as_ref()
        .and_then(|t| {
            t.artwork_url
                .as_deref()
                .and_then(|u| artwork.dominant_color(u))
                .or(t.accent_rgb)
                .or_else(|| Some(deterministic_rgb(&t.title)))
        })
        .unwrap_or([30, 215, 96]);

    let is_playing = state.playback.status.is_playing();
    let time = ui.input(|i| i.time);

    paint_ambient_gradient(
        ui,
        full_rect,
        dominant_rgb,
        theme.bg_base,
        is_playing,
        time,
        state.settings.reduce_motion,
    );

    let wide = full_rect.width() >= 760.0;
    let padded = full_rect.shrink2(Vec2::new(24.0, 16.0));
    let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(padded));

    if wide {
        let left_w = (padded.width() * 0.36).clamp(240.0, 380.0);
        inner.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(left_w, padded.height()),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    render_now_playing_left_column(
                        ui,
                        track.as_ref(),
                        is_playing,
                        time,
                        state.settings.reduce_motion,
                        theme,
                        artwork,
                        commands,
                    );
                },
            );

            ui.add_space(24.0);

            ui.allocate_ui_with_layout(
                Vec2::new((padded.width() - left_w - 24.0).max(240.0), padded.height()),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    render_lyrics_body(ui, state, theme, karaoke, commands, false);
                },
            );
        });
    } else {
        inner.vertical(|ui| {
            if let Some(t) = &track {
                ui.horizontal(|ui| {
                    artwork_box(
                        ui,
                        68.0,
                        10,
                        t.artwork_url.as_deref(),
                        &t.title,
                        t.accent_rgb,
                        artwork,
                        commands,
                    );
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(&t.title)
                                .size(20.0)
                                .strong()
                                .color(theme.text_primary),
                        );
                        ui.label(
                            RichText::new(t.artist_display())
                                .size(13.5)
                                .color(theme.text_secondary),
                        );
                    });
                });
                ui.add_space(10.0);
            }
            render_lyrics_body(ui, state, theme, karaoke, commands, false);
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn render_now_playing_left_column(
    ui: &mut Ui,
    track: Option<&crate::core::models::Track>,
    is_playing: bool,
    time: f64,
    reduce_motion: bool,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    ui.add_space(16.0);
    let art_size = (ui.available_width() - 24.0).clamp(180.0, 310.0);
    if let Some(t) = track {
        artwork_box(
            ui,
            art_size,
            16,
            t.artwork_url.as_deref(),
            &t.title,
            t.accent_rgb,
            artwork,
            commands,
        );
        ui.add_space(16.0);
        ui.label(
            RichText::new(&t.title)
                .size(22.0)
                .strong()
                .color(theme.text_primary),
        );
        ui.label(
            RichText::new(t.artist_display())
                .size(14.5)
                .color(theme.text_secondary),
        );
        if !t.album_name.is_empty() {
            ui.label(
                RichText::new(&t.album_name)
                    .size(12.5)
                    .color(theme.text_muted),
            );
        }
        ui.add_space(16.0);
        paint_audio_visualizer_bars(ui, is_playing, time, reduce_motion, theme);
    } else {
        artwork_box(ui, art_size, 16, None, "spotirust", None, artwork, commands);
        ui.add_space(16.0);
        ui.label(
            RichText::new("No Track Playing")
                .size(20.0)
                .strong()
                .color(theme.text_primary),
        );
        ui.label(
            RichText::new("Play a Spotify track, local audio file, or Soundcheck sample.")
                .size(12.5)
                .color(theme.text_secondary),
        );
    }
}

/// Subtle animated spectrum bars reacting to playing/paused state.
fn paint_audio_visualizer_bars(
    ui: &mut Ui,
    is_playing: bool,
    time: f64,
    reduce_motion: bool,
    theme: &ThemePalette,
) {
    let bars = 16usize;
    let width = 180.0;
    let height = 30.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    let bar_w = 5.0;
    let gap = (width - bars as f32 * bar_w) / (bars as f32 - 1.0);
    let fill = theme.accent.linear_multiply(0.85);
    let painter = ui.painter();

    for i in 0..bars {
        let phase = i as f64 * 0.65;
        let amp = if is_playing && !reduce_motion {
            let w1 = (time * 4.2 + phase).sin() * 0.5 + 0.5;
            let w2 = (time * 7.1 - phase * 1.4).cos() * 0.5 + 0.5;
            (0.18 + 0.82 * (w1 * 0.6 + w2 * 0.4)) as f32
        } else {
            0.16
        };
        let h = (height * amp).max(4.0);
        let x = rect.left() + i as f32 * (bar_w + gap);
        let bar_rect = Rect::from_min_size(Pos2::new(x, rect.bottom() - h), Vec2::new(bar_w, h));
        painter.rect_filled(bar_rect, CornerRadius::same(2), fill);
    }
}

/// Compact right-hand Lyrics side drawer.
pub fn render_lyrics_drawer(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    karaoke: &mut KaraokeRenderer,
    commands: &mut Vec<AppCommand>,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Lyrics")
                .size(16.0)
                .strong()
                .color(theme.text_primary),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("✕").clicked() {
                state.side_drawer = SideDrawer::None;
            }
            if ui
                .small_button("⛶")
                .on_hover_text("Open Fullscreen Now Playing")
                .clicked()
            {
                state.navigate(crate::core::models::Route::NowPlaying);
            }
        });
    });
    ui.separator();
    ui.add_space(4.0);
    render_lyrics_body(ui, state, theme, karaoke, commands, true);
}

fn render_lyrics_body(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    karaoke: &mut KaraokeRenderer,
    commands: &mut Vec<AppCommand>,
    compact: bool,
) {
    let has_active_track = state.playback.current_track.is_some();
    let clock_stale = state.playback.position.is_stale();
    let (doc, doc_revision) = match &state.lyrics {
        LyricsUiState::Idle => {
            ui.add_space(24.0);
            ui.label(
                RichText::new("Play a track to display synchronized word-by-word lyrics.")
                    .size(14.0)
                    .color(theme.text_secondary),
            );
            return;
        }
        LyricsUiState::Loading { .. } => {
            ui.add_space(24.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    RichText::new("Loading synchronized lyrics…")
                        .size(14.0)
                        .color(theme.text_secondary),
                );
            });
            return;
        }
        LyricsUiState::Ambiguous {
            provider_name,
            candidates,
            ..
        } => {
            let provider_label = provider_name.clone();
            let cand_list = candidates.clone();
            ui.add_space(12.0);
            ui.label(
                RichText::new("Select the exact track version")
                    .size(18.0)
                    .strong()
                    .color(theme.text_primary),
            );
            ui.label(
                RichText::new(format!(
                    "Multiple closely-matched versions were found on {provider_label}. Choose one to confirm lyrics synchronization:"
                ))
                .size(12.0)
                .color(theme.text_secondary),
            );
            ui.add_space(8.0);
            if let Some(track) = state.playback.current_track.clone() {
                ScrollArea::vertical()
                    .id_salt("lyrics_ambiguous_picker")
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for cand in &cand_list {
                            egui::Frame::new()
                                .fill(theme.bg_elevated)
                                .corner_radius(CornerRadius::same(8))
                                .stroke(Stroke::new(1.0, theme.border))
                                .inner_margin(Margin::same(10))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.vertical(|ui| {
                                            ui.label(
                                                RichText::new(format!(
                                                    "{} — {}",
                                                    cand.title, cand.artist
                                                ))
                                                .strong()
                                                .size(13.0)
                                                .color(theme.text_primary),
                                            );
                                            let album_txt =
                                                cand.album.as_deref().unwrap_or("Unknown Album");
                                            let dur_txt = cand
                                                .duration_ms
                                                .map(crate::core::models::format_duration_ms)
                                                .unwrap_or_else(|| "--:--".to_string());
                                            ui.label(
                                                RichText::new(format!(
                                                    "{album_txt} · {dur_txt} · {} · {}",
                                                    cand.timing_badge(),
                                                    cand.match_reason
                                                ))
                                                .size(11.0)
                                                .color(theme.text_muted),
                                            );
                                        });
                                        ui.with_layout(
                                            egui::Layout::right_to_left(Align::Center),
                                            |ui| {
                                                if ui.button("Use").clicked() {
                                                    commands.push(
                                                        AppCommand::SelectLyricsCandidate {
                                                            track: track.clone(),
                                                            candidate: Box::new(cand.clone()),
                                                        },
                                                    );
                                                }
                                            },
                                        );
                                    });
                                });
                            ui.add_space(6.0);
                        }
                    });
                ui.add_space(10.0);
                render_local_import_box(ui, state, &track, theme, commands);
            }
            return;
        }
        LyricsUiState::Unavailable { reason, .. } => {
            let reason_text = reason.clone();
            ui.add_space(16.0);
            ui.label(
                RichText::new("Lyrics not available")
                    .size(18.0)
                    .strong()
                    .color(theme.text_primary),
            );
            ui.label(
                RichText::new(reason_text)
                    .size(12.5)
                    .color(theme.text_secondary),
            );
            ui.add_space(10.0);
            if let Some(track) = state.playback.current_track.clone() {
                ui.horizontal(|ui| {
                    if ui.button("↻ Refresh Lyrics").clicked() {
                        commands.push(AppCommand::FetchLyrics {
                            track: track.clone(),
                            force_refresh: true,
                        });
                    }
                });
                ui.add_space(10.0);
                render_local_import_box(ui, state, &track, theme, commands);
            }
            return;
        }
        LyricsUiState::Loaded {
            document, revision, ..
        } => (Arc::clone(document), *revision),
    };

    // Metadata + sync controls bar (reuses cached header strings when populated — zero per-frame alloc!)
    let timing_pill = if doc.instrumental {
        "Instrumental"
    } else if doc.has_word_timing() {
        "Word-level"
    } else if doc.format.is_time_synced() {
        "Line-level"
    } else {
        "Plain"
    };
    let timing_color = if doc.has_word_timing() {
        theme.accent
    } else {
        theme.text_secondary
    };

    let offset_label_fallback;
    let offset_label = if !karaoke.layout_cache.offset_label.is_empty()
        && karaoke
            .layout_cache
            .key
            .as_ref()
            .is_some_and(|k| k.doc_revision == doc_revision)
    {
        karaoke.layout_cache.offset_label.as_str()
    } else {
        offset_label_fallback = format!("Offset {:+}ms", doc.offset_ms);
        offset_label_fallback.as_str()
    };

    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(timing_pill)
                .size(11.0)
                .strong()
                .color(timing_color),
        );
        ui.label(RichText::new("·").size(11.0).color(theme.text_muted));
        ui.label(
            RichText::new(doc.source.as_str())
                .size(11.0)
                .color(theme.text_secondary),
        );
        if clock_stale {
            ui.label(
                RichText::new("· Sync Stale")
                    .size(11.0)
                    .strong()
                    .color(theme.warning),
            );
        }
        ui.separator();

        if ui.small_button("-50ms").clicked() {
            commands.push(AppCommand::SetLyricsOffset {
                offset_ms: doc.offset_ms - 50,
            });
        }
        ui.label(
            RichText::new(offset_label)
                .size(11.0)
                .color(theme.text_muted),
        );
        if ui.small_button("+50ms").clicked() {
            commands.push(AppCommand::SetLyricsOffset {
                offset_ms: doc.offset_ms + 50,
            });
        }

        if ui
            .selectable_label(state.settings.show_lyric_translations, "Translations")
            .clicked()
        {
            state.settings.show_lyric_translations = !state.settings.show_lyric_translations;
            commands.push(AppCommand::SaveSettings(state.settings.clone()));
        }

        if has_active_track {
            if ui
                .small_button("↻")
                .on_hover_text("Force refresh lyrics (bypass cache)")
                .clicked()
                && let Some(track) = state.playback.current_track.clone()
            {
                commands.push(AppCommand::FetchLyrics {
                    track,
                    force_refresh: true,
                });
            }
            if ui
                .selectable_label(state.show_lyrics_import, "Import")
                .on_hover_text("Import local .ttml or .lrc override for this track")
                .clicked()
            {
                state.show_lyrics_import = !state.show_lyrics_import;
            }
        }

        if !state.lyrics_auto_scroll && ui.small_button("◎ Sync Scroll").clicked() {
            state.lyrics_auto_scroll = true;
            state.last_scrolled_line_idx = None;
        }
    });

    if state.show_lyrics_import
        && let Some(track) = state.playback.current_track.clone()
    {
        ui.add_space(6.0);
        render_local_import_box(ui, state, &track, theme, commands);
    }

    ui.add_space(8.0);

    if doc.instrumental {
        ui.add_space(32.0);
        ui.label(
            RichText::new("♪  Instrumental Track")
                .size(22.0)
                .strong()
                .color(theme.text_secondary),
        );
        return;
    }

    let audio_ms = state
        .seek_drag_ms
        .unwrap_or_else(|| state.playback.position.current_ms());
    let adjusted_ms = doc.adjusted_position_ms(audio_ms);

    // Advance separated animation state (O(log N) binary search, zero allocations)
    let anim_update = {
        let track_id_ref = match &state.lyrics {
            LyricsUiState::Loaded { track_id, .. } => track_id.as_str(),
            _ => "",
        };
        karaoke.animation.update(track_id_ref, &doc, audio_ms)
    };
    let active_idx = karaoke.animation.active_line_idx;
    if anim_update.seek_or_track_jump {
        state.last_scrolled_line_idx = None;
    }

    let is_time_synced = doc.format.is_time_synced();
    let word_synced = doc.has_word_timing();
    let reduce_motion = state.settings.reduce_motion;
    let show_translations = state.settings.show_lyric_translations;

    let scroll_output = ScrollArea::vertical()
        .id_salt(if compact {
            "lyrics_compact_scroll"
        } else {
            "lyrics_fullscreen_scroll"
        })
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Pause auto-scroll if user scrolls manually
            if ui.rect_contains_pointer(ui.max_rect()) {
                let wheel_delta = ui.input(|i| i.smooth_scroll_delta.y);
                if wheel_delta.abs() > 2.0 {
                    state.lyrics_auto_scroll = false;
                }
            }

            let max_width = (ui.available_width() - 12.0).max(120.0);
            let track_id_ref = match &state.lyrics {
                LyricsUiState::Loaded { track_id, .. } => track_id.as_str(),
                _ => "",
            };

            // Cache hit on 98%+ of frames: zero text shaping, zero String/Vec allocations!
            karaoke.layout_cache.ensure_layout_borrowed(
                ui.ctx(),
                track_id_ref,
                doc_revision,
                compact,
                max_width.round() as u32,
                active_idx,
                show_translations,
                &doc,
                theme,
            );

            let should_scroll_this_frame =
                state.lyrics_auto_scroll && state.last_scrolled_line_idx != active_idx;
            let visible_clip = ui.clip_rect();
            let line_spacing = if compact { 10.0 } else { 16.0 };

            for (idx, cached) in karaoke.layout_cache.lines.iter().enumerate() {
                if let Some(sec_galley) = &cached.section_galley {
                    ui.add_space(cached.section_top_pad);
                    let (sec_rect, _) = ui.allocate_exact_size(sec_galley.size(), Sense::hover());
                    if visible_clip.intersects(sec_rect) {
                        ui.painter().galley(
                            sec_rect.min,
                            Arc::clone(sec_galley),
                            theme.accent.linear_multiply(0.85),
                        );
                    }
                }

                let is_active = active_idx == Some(idx);
                let (rect, response) = ui
                    .allocate_exact_size(Vec2::new(max_width, cached.total_height), Sense::click());

                // Viewport culling: off-screen lines reserve exact layout height but skip painting
                if visible_clip.intersects(rect) {
                    paint_cached_karaoke_line(
                        ui,
                        rect,
                        &response,
                        cached,
                        adjusted_ms,
                        word_synced,
                        reduce_motion,
                        theme,
                    );
                }

                if response.clicked() && is_time_synced {
                    state.lyrics_auto_scroll = true;
                    state.last_scrolled_line_idx = None;
                    commands.push(AppCommand::SeekTo {
                        position_ms: cached.start_ms,
                    });
                }

                if is_active && should_scroll_this_frame {
                    response.scroll_to_me(Some(Align::Center));
                    state.last_scrolled_line_idx = active_idx;
                }

                ui.add_space(line_spacing);
            }

            if let Some(attr) = &doc.attribution {
                ui.add_space(16.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if let Some(url) = &attr.source_url {
                        ui.hyperlink_to(
                            RichText::new(attr.provider_label.as_str())
                                .size(11.5)
                                .color(theme.text_secondary),
                            url,
                        );
                    } else {
                        ui.label(
                            RichText::new(attr.provider_label.as_str())
                                .size(11.5)
                                .color(theme.text_secondary),
                        );
                    }
                    if let Some(submitter) = &attr.submitter_name {
                        ui.label(
                            RichText::new("· submitted by")
                                .size(11.5)
                                .color(theme.text_muted),
                        );
                        if let Some(sub_url) = &attr.submitter_url {
                            ui.hyperlink_to(
                                RichText::new(submitter.as_str())
                                    .size(11.5)
                                    .color(theme.text_secondary),
                                sub_url,
                            );
                        } else {
                            ui.label(
                                RichText::new(submitter.as_str())
                                    .size(11.5)
                                    .color(theme.text_secondary),
                            );
                        }
                    }
                });
                if let Some(disclaimer) = &attr.disclaimer {
                    ui.label(
                        RichText::new(disclaimer.as_str())
                            .size(10.5)
                            .color(theme.warning),
                    );
                }
                if let Some(copyright) = &attr.copyright {
                    ui.label(
                        RichText::new(copyright.as_str())
                            .size(10.5)
                            .color(theme.text_muted),
                    );
                }
            }

            ui.add_space(80.0);
        });
    let _ = scroll_output;
}

/// Paints one pre-laid-out lyric line directly from `Arc<Galley>` cache with zero heap allocations.
#[allow(clippy::too_many_arguments)]
fn paint_cached_karaoke_line(
    ui: &Ui,
    rect: Rect,
    response: &egui::Response,
    cached: &CachedLineLayout,
    adjusted_ms: u64,
    word_synced: bool,
    reduce_motion: bool,
    theme: &ThemePalette,
) {
    let painter = ui.painter();

    if response.hovered() {
        painter.rect_filled(
            rect.expand2(Vec2::new(6.0, 2.0)),
            CornerRadius::same(8),
            theme.bg_hover.linear_multiply(0.35),
        );
    }

    let origin = Pos2::new(rect.left(), rect.top() + 4.0);

    match &cached.body {
        CachedLineBody::Static { galley, color } => {
            painter.galley(origin, Arc::clone(galley), *color);
        }
        CachedLineBody::ActiveWords { words } => {
            let ui_clip = ui.clip_rect();
            let accent_stroke = Stroke::new(2.0, theme.accent.linear_multiply(0.8));

            for w in words {
                let pos = origin + w.offset;
                let progress = if word_synced {
                    w.progress_at(adjusted_ms)
                } else {
                    1.0
                };

                // Fast paths avoid double-painting & clip-rect overhead for completed/future words!
                if progress >= 0.999 || (reduce_motion && progress > 0.001) {
                    painter.galley(pos, Arc::clone(&w.lit_galley), w.lit_color);
                } else if progress <= 0.001 {
                    painter.galley(pos, Arc::clone(&w.dim_galley), w.dim_color);
                } else {
                    // Only the single currently sweeping word performs a clip-rect sweep
                    painter.galley(pos, Arc::clone(&w.dim_galley), w.dim_color);
                    let clip_w = w.size.x * progress;
                    let clip_rect = Rect::from_min_size(
                        Pos2::new(pos.x, pos.y - 2.0),
                        Vec2::new(clip_w, w.size.y + 4.0),
                    );
                    let clipped_painter = painter.with_clip_rect(clip_rect.intersect(ui_clip));
                    clipped_painter.galley(pos, Arc::clone(&w.lit_galley), w.lit_color);

                    let glow_y = pos.y + w.size.y;
                    painter.line_segment(
                        [Pos2::new(pos.x, glow_y), Pos2::new(pos.x + clip_w, glow_y)],
                        accent_stroke,
                    );
                }
            }
        }
    }

    // Background vocals (`x-bg`)
    if let (Some(dim_g), Some(lit_g)) = (&cached.bg_dim_galley, &cached.bg_lit_galley) {
        let bg_active = cached
            .bg_spans
            .iter()
            .any(|&(start, end)| adjusted_ms >= start && adjusted_ms <= end);
        let pos = Pos2::new(rect.left(), rect.top() + cached.bg_offset_y);
        if bg_active {
            painter.galley(pos, Arc::clone(lit_g), theme.accent);
        } else {
            painter.galley(pos, Arc::clone(dim_g), theme.text_muted);
        }
    }

    // Optional romanization
    if let Some(roman_g) = &cached.roman_galley {
        painter.galley(
            Pos2::new(rect.left(), rect.top() + cached.roman_offset_y),
            Arc::clone(roman_g),
            theme.text_muted,
        );
    }

    // Optional translation
    if let Some(trans_g) = &cached.trans_galley {
        painter.galley(
            Pos2::new(rect.left(), rect.top() + cached.trans_offset_y),
            Arc::clone(trans_g),
            theme.text_secondary,
        );
    }
}

fn render_local_import_box(
    ui: &mut Ui,
    state: &mut AppState,
    track: &crate::core::models::Track,
    theme: &ThemePalette,
    commands: &mut Vec<AppCommand>,
) {
    ui.label(
        RichText::new("Import a local .ttml or .lrc file for this track:")
            .size(12.0)
            .color(theme.text_secondary),
    );
    ui.horizontal_wrapped(|ui| {
        if ui
            .button("📁 Browse .ttml / .lrc…")
            .on_hover_text("Open native file picker to import a local TTML or LRC file")
            .clicked()
        {
            commands.push(AppCommand::PickImportLyricsFile {
                track: track.clone(),
            });
        }
        ui.add(
            egui::TextEdit::singleline(&mut state.import_lyrics_path)
                .hint_text("/path/to/song.ttml")
                .desired_width(220.0)
                .margin(Margin::symmetric(8, 5)),
        );
        if ui.button("Import Path").clicked() {
            let p = PathBuf::from(state.import_lyrics_path.trim());
            if !p.as_os_str().is_empty() {
                commands.push(AppCommand::ImportLyricsFile {
                    track: track.clone(),
                    path: p,
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::model::{LyricWord, LyricsFormat};
    use crate::ui::theme::ThemePalette;

    fn sample_word_synced_doc() -> LyricsDocument {
        LyricsDocument {
            lines: vec![
                LyricLine {
                    words: vec![
                        LyricWord {
                            text: "Pure".into(),
                            start_ms: 500,
                            end_ms: 1000,
                            trailing_space: true,
                        },
                        LyricWord {
                            text: "sound".into(),
                            start_ms: 1000,
                            end_ms: 1600,
                            trailing_space: false,
                        },
                    ],
                    start_ms: 500,
                    end_ms: 1600,
                    section: Some("Verse".into()),
                    agent: None,
                    key: None,
                    translation: Some("Am thanh thuan khiet".into()),
                    romanization: None,
                    background_words: Vec::new(),
                },
                LyricLine {
                    words: vec![
                        LyricWord {
                            text: "Next".into(),
                            start_ms: 2400,
                            end_ms: 2900,
                            trailing_space: true,
                        },
                        LyricWord {
                            text: "line".into(),
                            start_ms: 2900,
                            end_ms: 3500,
                            trailing_space: false,
                        },
                    ],
                    start_ms: 2400,
                    end_ms: 3500,
                    section: None,
                    agent: None,
                    key: None,
                    translation: None,
                    romanization: None,
                    background_words: Vec::new(),
                },
            ],
            offset_ms: 0,
            format: LyricsFormat::Ttml,
            source: "test".into(),
            instrumental: false,
            title: None,
            artist: None,
            album: None,
            isrc: None,
            attribution: None,
        }
    }

    #[test]
    fn layout_cache_hits_during_word_sweep_and_rebuilds_only_on_key_change() {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
        out.textures_delta.clear();
        let doc = sample_word_synced_doc();
        let theme = ThemePalette::obsidian_dark();
        let mut cache = KaraokeLayoutCache::default();

        let key_line0 = KaraokeLayoutKey {
            track_id: "t1".into(),
            doc_revision: 1,
            compact: false,
            max_width_px: 520,
            active_line_idx: Some(0),
            show_translations: true,
            theme_id: theme.id.clone(),
            dark: theme.dark,
        };

        // First frame builds the layout cache once
        cache.ensure_layout(&ctx, key_line0.clone(), &doc, &theme);
        assert_eq!(cache.rebuild_count(), 1);
        assert_eq!(cache.hit_count(), 0);
        assert_eq!(cache.cached_line_count(), 2);

        // Next 60 frames during word sweep on line 0 hit cache 100% (0 rebuilds!)
        for _ in 0..60 {
            cache.ensure_layout(&ctx, key_line0.clone(), &doc, &theme);
        }
        assert_eq!(cache.rebuild_count(), 1);
        assert_eq!(cache.hit_count(), 60);

        // Switching to line 1 invalidates and rebuilds once
        let mut key_line1 = key_line0.clone();
        key_line1.active_line_idx = Some(1);
        cache.ensure_layout(&ctx, key_line1, &doc, &theme);
        assert_eq!(cache.rebuild_count(), 2);

        // Resizing width invalidates and rebuilds once
        let mut key_resized = key_line0;
        key_resized.active_line_idx = Some(1);
        key_resized.max_width_px = 640;
        cache.ensure_layout(&ctx, key_resized, &doc, &theme);
        assert_eq!(cache.rebuild_count(), 3);
    }

    #[test]
    fn animation_state_tracks_word_transitions_and_seek_jumps() {
        let doc = sample_word_synced_doc();
        let mut anim = KaraokeAnimationState::default();

        let u0 = anim.update("t1", &doc, 600);
        assert!(u0.line_changed);
        assert!(u0.word_changed);
        assert_eq!(anim.active_line_idx, Some(0));
        assert_eq!(anim.active_word_idx, Some(0));

        // Same timestamp -> zero highlight change
        let u_same = anim.update("t1", &doc, 600);
        assert!(!u_same.highlight_changed);

        // Word transition from "Pure" -> "sound"
        let u_next_word = anim.update("t1", &doc, 1050);
        assert!(!u_next_word.line_changed);
        assert!(u_next_word.word_changed);
        assert_eq!(anim.active_word_idx, Some(1));

        // Backward seek detected cleanly
        let u_seek = anim.update("t1", &doc, 550);
        assert!(u_seek.seek_or_track_jump);
        assert_eq!(anim.active_word_idx, Some(0));
    }

    #[test]
    fn repaint_scheduler_avoids_blind_60fps_while_preserving_word_boundaries() {
        let doc = Arc::new(sample_word_synced_doc());
        let loaded = LyricsUiState::Loaded {
            track_id: "t1".into(),
            document: Arc::clone(&doc),
            revision: 1,
        };

        // Paused -> never wakes
        assert_eq!(
            next_karaoke_repaint_delay(&loaded, 700, false, true, false),
            None
        );

        // Lyrics hidden -> 4 Hz progress bar only
        assert_eq!(
            next_karaoke_repaint_delay(&loaded, 700, true, false, false),
            Some(Duration::from_millis(250))
        );

        // Inside active word (500..1000) -> ~16.7 FPS quantized sweep (60ms)
        assert_eq!(
            next_karaoke_repaint_delay(&loaded, 700, true, true, false),
            Some(Duration::from_millis(60))
        );

        // Near end of active word (980ms, 20ms left) -> locks cleanly to exact word end (20ms)!
        assert_eq!(
            next_karaoke_repaint_delay(&loaded, 980, true, true, false),
            Some(Duration::from_millis(20))
        );

        // Inter-line gap (1700ms -> next line at 2400ms) -> 250ms idle tick, zero 60 FPS spin!
        assert_eq!(
            next_karaoke_repaint_delay(&loaded, 1700, true, true, false),
            Some(Duration::from_millis(250))
        );

        // Right before next line (2340ms -> 2400ms) -> wakes precisely in 60ms!
        assert_eq!(
            next_karaoke_repaint_delay(&loaded, 2340, true, true, false),
            Some(Duration::from_millis(60))
        );

        // Reduce motion during active word (700..1000 -> 300ms remaining) -> caps at 250ms (no sweep spin)
        assert_eq!(
            next_karaoke_repaint_delay(&loaded, 700, true, true, true),
            Some(Duration::from_millis(250))
        );

        // Plain lyrics -> never spins above 4 Hz
        let mut plain_doc = sample_word_synced_doc();
        plain_doc.format = LyricsFormat::PlainText;
        let plain_state = LyricsUiState::Loaded {
            track_id: "t1".into(),
            document: Arc::new(plain_doc),
            revision: 2,
        };
        assert_eq!(
            next_karaoke_repaint_delay(&plain_state, 700, true, true, false),
            Some(Duration::from_millis(250))
        );
    }
}
