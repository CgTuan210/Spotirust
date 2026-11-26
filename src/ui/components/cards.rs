//! Reusable media cards, mathematically aligned track table rows, warm empty states,
//! and zero-CLS shimmer loading skeletons.

use crate::core::events::AppCommand;
use crate::core::models::{Track, format_duration_ms};
use crate::ui::components::artwork::{ArtworkTextureCache, paint_artwork};
use crate::ui::components::icons::{Icon, icon_button, paint_equalizer_bars, paint_icon};
use crate::ui::theme::ThemePalette;
use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Layout, Pos2, Rect, RichText, Sense,
    Stroke, StrokeKind, Ui, Vec2,
};

const TRACK_ROW_HEIGHT: f32 = 54.0;
const TRACK_IDX_W: f32 = 34.0;
const TRACK_ART_SIZE: f32 = 40.0;
const TRACK_ACTIONS_W: f32 = 128.0;

#[inline]
fn compute_track_columns(rect: Rect) -> (f32, f32, bool, f32, f32, f32) {
    let inner_left = rect.left() + 12.0;
    let inner_right = rect.right() - 12.0;
    let total_w = (inner_right - inner_left).max(240.0);

    let show_album = total_w >= 600.0;
    let album_w = if show_album {
        (total_w * 0.27).clamp(130.0, 260.0)
    } else {
        0.0
    };

    let art_x = inner_left + TRACK_IDX_W + 6.0;
    let title_x = art_x + TRACK_ART_SIZE + 12.0;
    let actions_x = inner_right - TRACK_ACTIONS_W;
    let album_x = if show_album {
        actions_x - album_w - 14.0
    } else {
        actions_x
    };
    let title_w = (album_x - 16.0 - title_x).max(90.0);

    (art_x, title_x, show_album, album_x, album_w, title_w)
}

/// Renders a polished square media card (Playlist, Album, or Showcase Track).
/// Returns `true` if clicked.
#[allow(clippy::too_many_arguments)]
pub fn media_card(
    ui: &mut Ui,
    theme: &ThemePalette,
    title: &str,
    subtitle: &str,
    badge: Option<&str>,
    artwork_url: Option<&str>,
    accent_rgb: Option<[u8; 3]>,
    width: f32,
    cache: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) -> bool {
    render_card_inner(
        ui,
        theme,
        title,
        subtitle,
        badge,
        artwork_url,
        accent_rgb,
        width,
        false,
        cache,
        commands,
    )
}

/// Renders a circular-portrait artist card following Apple Music & Spotify desktop conventions.
/// Returns `true` if clicked.
#[allow(clippy::too_many_arguments)]
pub fn artist_card(
    ui: &mut Ui,
    theme: &ThemePalette,
    name: &str,
    subtitle: &str,
    image_url: Option<&str>,
    width: f32,
    cache: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) -> bool {
    render_card_inner(
        ui, theme, name, subtitle, None, image_url, None, width, true, cache, commands,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_card_inner(
    ui: &mut Ui,
    theme: &ThemePalette,
    title: &str,
    subtitle: &str,
    badge: Option<&str>,
    artwork_url: Option<&str>,
    accent_rgb: Option<[u8; 3]>,
    width: f32,
    circular_art: bool,
    cache: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) -> bool {
    let extra_h = if badge.is_some() { 82.0 } else { 66.0 };
    let card_height = width + extra_h;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, card_height), Sense::click());
    let hovered = response.hovered();
    let pressed = response.is_pointer_button_down_on();

    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let hover_t = ui.ctx().animate_bool_responsive(response.id, hovered);
    let bg = lerp_color(theme.bg_elevated, theme.bg_hover, hover_t);
    let card_radius = CornerRadius::same(18);

    let draw_rect = if pressed { rect.shrink(0.5) } else { rect };
    if !theme.dark || hover_t > 0.05 {
        ui.painter()
            .add(theme.shadow_soft().as_shape(draw_rect, card_radius));
    }
    ui.painter().rect_filled(draw_rect, card_radius, bg);

    let stroke_col = lerp_color(theme.hairline(), theme.glass_border(), hover_t);
    ui.painter().rect_stroke(
        draw_rect,
        card_radius,
        Stroke::new(1.0, stroke_col),
        StrokeKind::Inside,
    );
    // Subtle iOS glass catch-light along top edge of card
    if draw_rect.width() > 44.0 {
        let top_alpha = if theme.dark {
            (22.0 + 30.0 * hover_t) as u8
        } else {
            210
        };
        ui.painter().line_segment(
            [
                Pos2::new(draw_rect.left() + 18.0, draw_rect.top() + 0.9),
                Pos2::new(draw_rect.right() - 18.0, draw_rect.top() + 0.9),
            ],
            Stroke::new(1.0, Color32::from_white_alpha(top_alpha)),
        );
    }

    let pad = 12.0;
    let art_size = width - pad * 2.0;
    let art_rect = Rect::from_min_size(
        Pos2::new(draw_rect.left() + pad, draw_rect.top() + pad),
        Vec2::splat(art_size),
    );
    let art_rounding = if circular_art {
        ((art_size * 0.5).round() as u32).min(255) as u8
    } else {
        12
    };

    ui.painter().add(
        theme
            .shadow_soft()
            .as_shape(art_rect, CornerRadius::same(art_rounding)),
    );
    paint_artwork(
        ui,
        art_rect,
        art_rounding,
        artwork_url,
        title,
        accent_rgb,
        cache,
        commands,
    );

    // Smoothly rising floating Play CTA on hover
    if hover_t > 0.02 {
        let rise_y = (1.0 - hover_t) * 6.0;
        let play_r = 19.0;
        let play_center = Pos2::new(
            art_rect.right() - play_r - 8.0,
            art_rect.bottom() - play_r - 8.0 + rise_y,
        );
        let play_rect = Rect::from_center_size(play_center, Vec2::splat(play_r * 2.0));
        let shadow = egui::epaint::Shadow {
            offset: [0, 4],
            blur: 12,
            spread: 0,
            color: Color32::from_black_alpha((110.0 * hover_t) as u8),
        };
        ui.painter()
            .add(shadow.as_shape(play_rect, CornerRadius::same(play_r as u8)));

        let pill_bg = Color32::from_rgba_unmultiplied(
            theme.accent.r(),
            theme.accent.g(),
            theme.accent.b(),
            (255.0 * hover_t) as u8,
        );
        let pill_fg = Color32::from_rgba_unmultiplied(
            theme.on_accent().r(),
            theme.on_accent().g(),
            theme.on_accent().b(),
            (255.0 * hover_t) as u8,
        );
        ui.painter().circle_filled(play_center, play_r, pill_bg);
        paint_icon(
            ui.painter(),
            Rect::from_center_size(play_center, Vec2::splat(16.0)),
            Icon::Play,
            pill_fg,
        );
    }

    // Text block below artwork (asymmetric bottom breathing room)
    let text_rect = Rect::from_min_max(
        Pos2::new(draw_rect.left() + pad, art_rect.bottom() + 10.0),
        Pos2::new(draw_rect.right() - pad, draw_rect.bottom() - 10.0),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect));
    child.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.add(
            egui::Label::new(
                RichText::new(title)
                    .strong()
                    .size(13.5)
                    .color(theme.text_primary),
            )
            .truncate()
            .selectable(false),
        );
        ui.add(
            egui::Label::new(
                RichText::new(subtitle)
                    .size(12.0)
                    .color(theme.text_secondary),
            )
            .truncate()
            .selectable(false),
        );
        if let Some(tag) = badge {
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(RichText::new(tag).size(10.5).strong().color(theme.accent))
                    .truncate()
                    .selectable(false),
            );
        }
    });

    response.clicked()
}

/// Renders the aligned column header bar above a list of `track_row` items.
pub fn track_table_header(ui: &mut Ui, theme: &ThemePalette) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::hover());
    let (_, title_x, show_album, album_x, _, _) = compute_track_columns(rect);

    let painter = ui.painter();
    let label_font = FontId::proportional(10.5);
    let col = theme.text_muted;
    let cy = rect.center().y;

    painter.text(
        Pos2::new(rect.left() + 12.0 + TRACK_IDX_W * 0.5, cy),
        Align2::CENTER_CENTER,
        "#",
        label_font.clone(),
        col,
    );
    painter.text(
        Pos2::new(title_x, cy),
        Align2::LEFT_CENTER,
        "TITLE",
        label_font.clone(),
        col,
    );
    if show_album {
        painter.text(
            Pos2::new(album_x, cy),
            Align2::LEFT_CENTER,
            "ALBUM",
            label_font,
            col,
        );
    }

    // Crisp vector clock icon aligned above the duration column
    let clock_rect = Rect::from_center_size(Pos2::new(rect.right() - 28.0, cy), Vec2::splat(13.0));
    paint_icon(painter, clock_rect, Icon::Clock, col);

    // Subtle 1px structural hairline
    painter.line_segment(
        [
            Pos2::new(rect.left() + 4.0, rect.bottom() - 0.5),
            Pos2::new(rect.right() - 4.0, rect.bottom() - 0.5),
        ],
        Stroke::new(1.0, theme.hairline()),
    );
    ui.add_space(4.0);
}

/// Renders a single mathematically aligned track row with index/equalizer/play-on-hover,
/// GPU-rounded thumbnail, title/artist, strictly aligned album column, vector like & queue buttons,
/// and tabular monospaced duration.
#[allow(clippy::too_many_arguments)]
pub fn track_row(
    ui: &mut Ui,
    theme: &ThemePalette,
    index: usize,
    track: &Track,
    is_active: bool,
    is_playing: bool,
    context: &[Track],
    cache: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    let width = ui.available_width();
    let (rect, row_resp) =
        ui.allocate_exact_size(Vec2::new(width, TRACK_ROW_HEIGHT), Sense::hover());

    // Detect pointer hover anywhere within the full row rect (even over inner action buttons)
    let row_hovered = ui.rect_contains_pointer(rect);
    let hover_t = ui.ctx().animate_bool_responsive(row_resp.id, row_hovered);

    let painter = ui.painter();
    let row_radius = CornerRadius::same(12);

    if is_active {
        let base_fill = theme.accent_soft();
        let fill = if hover_t > 0.01 {
            Color32::from_rgba_unmultiplied(
                base_fill.r(),
                base_fill.g(),
                base_fill.b(),
                (f32::from(base_fill.a()) + 12.0 * hover_t).min(255.0) as u8,
            )
        } else {
            base_fill
        };
        painter.rect_filled(rect, row_radius, fill);
        painter.rect_stroke(
            rect,
            row_radius,
            Stroke::new(
                1.0,
                Color32::from_rgba_unmultiplied(
                    theme.accent.r(),
                    theme.accent.g(),
                    theme.accent.b(),
                    55,
                ),
            ),
            StrokeKind::Inside,
        );

        // Leading-edge vertical accent capsule
        let bar_rect = Rect::from_center_size(
            Pos2::new(rect.left() + 4.0, rect.center().y),
            Vec2::new(3.0, 24.0),
        );
        painter.rect_filled(bar_rect, CornerRadius::same(2), theme.accent);
    } else if hover_t > 0.01 {
        let h = theme.hover_overlay();
        let fill = Color32::from_rgba_unmultiplied(
            h.r(),
            h.g(),
            h.b(),
            (f32::from(h.a()) * hover_t).min(255.0) as u8,
        );
        painter.rect_filled(rect, row_radius, fill);
    }

    let (art_x, title_x, show_album, album_x, album_w, title_w) = compute_track_columns(rect);
    let cy = rect.center().y;

    // 1. Clickable main track area (from left edge through album column)
    let play_hit_rect = Rect::from_min_max(
        rect.left_top(),
        Pos2::new(rect.right() - 12.0 - TRACK_ACTIONS_W, rect.bottom()),
    );
    let play_resp = ui.interact(play_hit_rect, row_resp.id.with("play"), Sense::click());
    if play_resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    // Column 1: Index / Active Equalizer / Hover Play icon
    let idx_rect = Rect::from_center_size(
        Pos2::new(rect.left() + 12.0 + TRACK_IDX_W * 0.5, cy),
        Vec2::splat(20.0),
    );
    if row_hovered {
        let icon = if is_active && is_playing {
            Icon::Pause
        } else {
            Icon::Play
        };
        let icon_col = if is_active {
            theme.accent
        } else {
            theme.text_primary
        };
        paint_icon(
            ui.painter(),
            Rect::from_center_size(idx_rect.center(), Vec2::splat(14.0)),
            icon,
            icon_col,
        );
    } else if is_active && is_playing {
        let time = ui.input(|i| i.time);
        let animate = ui.style().animation_time > 0.0;
        paint_equalizer_bars(
            ui.painter(),
            Rect::from_center_size(idx_rect.center(), Vec2::splat(14.0)),
            theme.accent,
            time,
            animate,
        );
    } else {
        ui.painter().text(
            idx_rect.center(),
            Align2::CENTER_CENTER,
            format!("{}", index + 1),
            FontId::monospace(12.0),
            if is_active {
                theme.accent
            } else {
                theme.text_muted
            },
        );
    }

    // Column 2: Concentric GPU-rounded cover thumbnail
    let art_rect = Rect::from_center_size(
        Pos2::new(art_x + TRACK_ART_SIZE * 0.5, cy),
        Vec2::splat(TRACK_ART_SIZE),
    );
    paint_artwork(
        ui,
        art_rect,
        9,
        track.artwork_url.as_deref(),
        &track.title,
        track.accent_rgb,
        cache,
        commands,
    );

    // Column 3: Title & Artist (strictly clipped to `title_w`)
    let title_rect = Rect::from_min_size(
        Pos2::new(title_x, rect.top() + 8.0),
        Vec2::new(title_w, TRACK_ROW_HEIGHT - 16.0),
    );
    let mut title_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(title_rect)
            .layout(Layout::top_down(Align::LEFT)),
    );
    title_ui.spacing_mut().item_spacing.y = 2.0;
    title_ui.add(
        egui::Label::new(
            RichText::new(&track.title)
                .size(13.5)
                .strong()
                .color(if is_active {
                    theme.accent
                } else {
                    theme.text_primary
                }),
        )
        .truncate()
        .selectable(false),
    );
    title_ui.add(
        egui::Label::new(
            RichText::new(track.artist_display())
                .size(12.0)
                .color(if row_hovered {
                    theme.text_primary.linear_multiply(0.85)
                } else {
                    theme.text_secondary
                }),
        )
        .truncate()
        .selectable(false),
    );

    // Column 4: Strictly aligned Album column
    if show_album {
        let album_rect = Rect::from_min_size(
            Pos2::new(album_x, rect.top() + 14.0),
            Vec2::new(album_w, TRACK_ROW_HEIGHT - 28.0),
        );
        let mut album_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(album_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        album_ui.add(
            egui::Label::new(
                RichText::new(&track.album_name)
                    .size(12.0)
                    .color(if row_hovered {
                        theme.text_secondary
                    } else {
                        theme.text_muted
                    }),
            )
            .truncate()
            .selectable(false),
        );
    }

    // Column 5: Actions (Heart + Add to Queue) & Tabular Monospaced Duration
    let actions_rect = Rect::from_min_size(
        Pos2::new(rect.right() - 12.0 - TRACK_ACTIONS_W, rect.top()),
        Vec2::new(TRACK_ACTIONS_W, TRACK_ROW_HEIGHT),
    );
    let mut actions_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(actions_rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    actions_ui.spacing_mut().item_spacing.x = 4.0;

    // Right-aligned tabular duration
    let (dur_rect, _) = actions_ui.allocate_exact_size(Vec2::new(46.0, 24.0), Sense::hover());
    actions_ui.painter().text(
        Pos2::new(dur_rect.right() - 2.0, dur_rect.center().y),
        Align2::RIGHT_CENTER,
        format_duration_ms(track.duration_ms),
        FontId::monospace(12.0),
        theme.text_muted,
    );

    // Add to queue icon button (subtle at rest, crisp on hover)
    if icon_button(
        &mut actions_ui,
        theme,
        Icon::ListPlus,
        15.0,
        30.0,
        false,
        true,
        "Add to Queue",
    )
    .clicked()
    {
        commands.push(AppCommand::AddToQueue {
            track: track.clone(),
        });
    }

    // Like / Heart toggle button
    let heart_icon = if track.liked {
        Icon::HeartFilled
    } else {
        Icon::Heart
    };
    if icon_button(
        &mut actions_ui,
        theme,
        heart_icon,
        15.0,
        30.0,
        track.liked,
        true,
        if track.liked {
            "Remove from Liked Songs"
        } else {
            "Save to Liked Songs"
        },
    )
    .clicked()
    {
        commands.push(AppCommand::ToggleLikeTrack {
            track: track.clone(),
        });
    }

    // Trigger playback on click (or pause/resume if clicking the active track's icon)
    if play_resp.clicked() {
        if is_active
            && is_playing
            && play_resp
                .interact_pointer_pos()
                .is_some_and(|p| p.x <= art_x)
        {
            commands.push(AppCommand::TogglePlayPause);
        } else {
            commands.push(AppCommand::PlayTrack {
                track: track.clone(),
                queue_context: context.to_vec(),
            });
        }
    }
}

/// Warm, intentional empty state card with icon badge, heading, and guidance copy.
pub fn empty_state(ui: &mut Ui, theme: &ThemePalette, icon: Icon, title: &str, subtitle: &str) {
    egui::Frame::new()
        .fill(theme.bg_elevated)
        .corner_radius(CornerRadius::same(14))
        .stroke(Stroke::new(1.0, theme.hairline()))
        .inner_margin(egui::Margin::symmetric(24, 26))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (badge_rect, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
                ui.painter()
                    .circle_filled(badge_rect.center(), 22.0, theme.active_pill_fill());
                paint_icon(
                    ui.painter(),
                    Rect::from_center_size(badge_rect.center(), Vec2::splat(20.0)),
                    icon,
                    theme.text_secondary,
                );
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.label(
                        RichText::new(title)
                            .size(15.0)
                            .strong()
                            .color(theme.text_primary),
                    );
                    ui.label(
                        RichText::new(subtitle)
                            .size(12.5)
                            .color(theme.text_secondary),
                    );
                });
            });
        });
}

/// Animated shimmer skeleton row matching exact `track_row` geometry (zero CLS).
pub fn skeleton_track_row(ui: &mut Ui, theme: &ThemePalette, time: f64) {
    let shimmer = 0.5 + 0.5 * (time * 3.0).sin() as f32;
    let fill = Color32::from_rgba_unmultiplied(
        theme.bg_hover.r(),
        theme.bg_hover.g(),
        theme.bg_hover.b(),
        (95.0 + shimmer * 85.0) as u8,
    );

    let (rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), TRACK_ROW_HEIGHT),
        Sense::hover(),
    );
    let (art_x, title_x, show_album, album_x, album_w, title_w) = compute_track_columns(rect);
    let cy = rect.center().y;
    let painter = ui.painter();

    let art = Rect::from_center_size(
        Pos2::new(art_x + TRACK_ART_SIZE * 0.5, cy),
        Vec2::splat(TRACK_ART_SIZE),
    );
    painter.rect_filled(art, CornerRadius::same(6), fill);

    let bar1 = Rect::from_min_size(
        Pos2::new(title_x, rect.top() + 12.0),
        Vec2::new((title_w * 0.58).min(220.0), 12.0),
    );
    painter.rect_filled(bar1, CornerRadius::same(4), fill);

    let bar2 = Rect::from_min_size(
        Pos2::new(title_x, rect.top() + 30.0),
        Vec2::new((title_w * 0.36).min(140.0), 10.0),
    );
    painter.rect_filled(bar2, CornerRadius::same(4), fill.linear_multiply(0.75));

    if show_album {
        let album_bar = Rect::from_min_size(
            Pos2::new(album_x, cy - 5.0),
            Vec2::new((album_w * 0.62).min(140.0), 10.0),
        );
        painter.rect_filled(album_bar, CornerRadius::same(4), fill.linear_multiply(0.7));
    }

    let dur_bar = Rect::from_min_size(
        Pos2::new(rect.right() - 48.0, cy - 5.0),
        Vec2::new(32.0, 10.0),
    );
    painter.rect_filled(dur_bar, CornerRadius::same(4), fill.linear_multiply(0.7));
}

/// Animated shimmer skeleton media card matching exact `media_card` geometry (zero CLS).
pub fn skeleton_card(ui: &mut Ui, theme: &ThemePalette, width: f32, time: f64) {
    let shimmer = 0.5 + 0.5 * (time * 3.0).sin() as f32;
    let fill = Color32::from_rgba_unmultiplied(
        theme.bg_hover.r(),
        theme.bg_hover.g(),
        theme.bg_hover.b(),
        (95.0 + shimmer * 85.0) as u8,
    );
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, width + 66.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(12), theme.bg_elevated);
    let pad = 12.0;
    let art = Rect::from_min_size(
        Pos2::new(rect.left() + pad, rect.top() + pad),
        Vec2::splat(width - pad * 2.0),
    );
    painter.rect_filled(art, CornerRadius::same(8), fill);
    let title = Rect::from_min_size(
        Pos2::new(rect.left() + pad, art.bottom() + 12.0),
        Vec2::new((width - pad * 2.0) * 0.72, 12.0),
    );
    painter.rect_filled(title, CornerRadius::same(4), fill);
    let sub = Rect::from_min_size(
        Pos2::new(rect.left() + pad, title.bottom() + 8.0),
        Vec2::new((width - pad * 2.0) * 0.48, 10.0),
    );
    painter.rect_filled(sub, CornerRadius::same(4), fill.linear_multiply(0.72));
}

#[inline]
fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let inv = 1.0 - t;
    Color32::from_rgba_unmultiplied(
        (f32::from(a.r()) * inv + f32::from(b.r()) * t).round() as u8,
        (f32::from(a.g()) * inv + f32::from(b.g()) * t).round() as u8,
        (f32::from(a.b()) * inv + f32::from(b.b()) * t).round() as u8,
        (f32::from(a.a()) * inv + f32::from(b.a()) * t).round() as u8,
    )
}
