//! Pure geometric vector icon system and refined interactive icon primitives.
//!
//! Renders crisp, DPI-independent, theme-colorable vector icons via `egui::Painter`
//! on a normalized 24×24 viewBox — eliminating cross-platform OS emoji mismatches,
//! missing-glyph tofu boxes, and baseline drift.

use crate::ui::theme::ThemePalette;
use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Painter, Pos2, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2,
};
use std::f32::consts::TAU;

/// Semantic vector icons used across Spotirust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Home,
    Search,
    Library,
    Heart,
    HeartFilled,
    Clock,
    MusicNote,
    Disc,
    Artist,
    Folder,
    Settings,
    ChevronLeft,
    ChevronRight,
    Shuffle,
    SkipBack,
    SkipForward,
    Play,
    Pause,
    Repeat,
    RepeatOne,
    Lyrics,
    Queue,
    VolumeMute,
    VolumeLow,
    VolumeHigh,
    Speaker,
    Connect,
    LocalDrive,
    Expand,
    Close,
    Plus,
    ListPlus,
    Refresh,
    LogOut,
    Check,
    Alert,
    ArrowUp,
    ArrowDown,
}

/// Paint a crisp geometric vector icon centered within `rect` on a 24×24 coordinate grid.
pub fn paint_icon(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let size = rect.width().min(rect.height());
    let origin = Pos2::new(rect.center().x - size * 0.5, rect.center().y - size * 0.5);
    let s = size / 24.0;
    let sw = (s * 1.85).clamp(1.25, 2.2);
    let stroke = Stroke::new(sw, color);
    let thin_stroke = Stroke::new((sw * 0.85).max(1.1), color);

    let p = |x: f32, y: f32| Pos2::new(origin.x + x * s, origin.y + y * s);

    match icon {
        Icon::Home => {
            // Roof
            painter.add(Shape::line(
                vec![p(3.5, 11.0), p(12.0, 3.8), p(20.5, 11.0)],
                stroke,
            ));
            // House walls + floor
            painter.add(Shape::line(
                vec![p(5.8, 9.6), p(5.8, 19.6), p(18.2, 19.6), p(18.2, 9.6)],
                stroke,
            ));
            // Door
            painter.add(Shape::line(
                vec![p(9.8, 19.6), p(9.8, 14.2), p(14.2, 14.2), p(14.2, 19.6)],
                thin_stroke,
            ));
        }

        Icon::Search => {
            painter.circle_stroke(p(10.5, 10.5), 6.2 * s, stroke);
            painter.line_segment([p(15.2, 15.2), p(20.2, 20.2)], stroke);
        }

        Icon::Library => {
            // Three books/spines, rightmost tilted slightly
            painter.line_segment([p(4.5, 4.5), p(4.5, 19.5)], stroke);
            painter.line_segment([p(9.2, 4.5), p(9.2, 19.5)], stroke);
            painter.line_segment([p(13.8, 5.2), p(19.5, 19.2)], stroke);
            painter.line_segment([p(13.8, 5.2), p(13.8, 19.5)], thin_stroke);
        }

        Icon::Heart | Icon::HeartFilled => {
            let pts = heart_polygon(&p);
            if matches!(icon, Icon::HeartFilled) {
                // Two convex lobes + lower triangle for smooth concave fill
                painter.circle_filled(p(8.2, 8.8), 4.15 * s, color);
                painter.circle_filled(p(15.8, 8.8), 4.15 * s, color);
                painter.add(Shape::convex_polygon(
                    vec![p(4.3, 10.2), p(12.0, 9.4), p(19.7, 10.2), p(12.0, 19.6)],
                    color,
                    Stroke::NONE,
                ));
            } else {
                painter.add(Shape::closed_line(pts, stroke));
            }
        }

        Icon::Clock => {
            painter.circle_stroke(p(12.0, 12.0), 8.2 * s, stroke);
            painter.add(Shape::line(
                vec![p(12.0, 7.2), p(12.0, 12.2), p(15.6, 14.2)],
                stroke,
            ));
        }

        Icon::MusicNote => {
            // Beamed eighth notes
            painter.circle_filled(p(7.5, 17.6), 2.5 * s, color);
            painter.circle_filled(p(17.0, 15.8), 2.5 * s, color);
            painter.line_segment([p(9.7, 17.2), p(9.7, 6.2)], stroke);
            painter.line_segment([p(19.2, 15.4), p(19.2, 4.6)], stroke);
            painter.line_segment([p(9.7, 6.2), p(19.2, 4.6)], Stroke::new(sw * 1.25, color));
        }

        Icon::Disc => {
            painter.circle_stroke(p(12.0, 12.0), 8.4 * s, stroke);
            painter.circle_stroke(p(12.0, 12.0), 2.7 * s, thin_stroke);
            painter.circle_filled(p(12.0, 12.0), 0.9 * s, color);
        }

        Icon::Artist => {
            // User silhouette (clean head + shoulders arc)
            painter.circle_stroke(p(12.0, 8.2), 3.7 * s, stroke);
            let arc = arc_points(
                p(12.0, 21.5),
                7.4 * s,
                std::f32::consts::PI * 1.15,
                std::f32::consts::PI * 1.85,
                14,
            );
            painter.add(Shape::line(arc, stroke));
        }

        Icon::Folder => {
            painter.add(Shape::closed_line(
                vec![
                    p(3.5, 6.5),
                    p(9.5, 6.5),
                    p(11.6, 9.0),
                    p(20.5, 9.0),
                    p(20.5, 18.8),
                    p(3.5, 18.8),
                ],
                stroke,
            ));
        }

        Icon::Settings => {
            painter.circle_stroke(p(12.0, 12.0), 3.3 * s, stroke);
            painter.circle_stroke(p(12.0, 12.0), 6.6 * s, thin_stroke);
            // 8 clean radial gear teeth
            for i in 0..8 {
                let a = (i as f32 / 8.0) * TAU;
                let inner = Pos2::new(
                    p(12.0, 12.0).x + a.cos() * 6.8 * s,
                    p(12.0, 12.0).y + a.sin() * 6.8 * s,
                );
                let outer = Pos2::new(
                    p(12.0, 12.0).x + a.cos() * 9.3 * s,
                    p(12.0, 12.0).y + a.sin() * 9.3 * s,
                );
                painter.line_segment([inner, outer], Stroke::new(sw * 1.15, color));
            }
        }

        Icon::ChevronLeft => {
            painter.add(Shape::line(
                vec![p(14.8, 6.0), p(8.6, 12.0), p(14.8, 18.0)],
                stroke,
            ));
        }

        Icon::ChevronRight => {
            painter.add(Shape::line(
                vec![p(9.2, 6.0), p(15.4, 12.0), p(9.2, 18.0)],
                stroke,
            ));
        }

        Icon::Shuffle => {
            // Crossing paths with crisp arrowheads
            painter.add(Shape::line(
                vec![p(3.5, 17.0), p(8.2, 17.0), p(15.2, 7.0), p(20.0, 7.0)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![p(3.5, 7.0), p(8.2, 7.0), p(10.6, 10.3)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![p(13.4, 13.7), p(15.2, 17.0), p(20.0, 17.0)],
                stroke,
            ));
            // Arrowheads
            painter.add(Shape::line(
                vec![p(17.4, 4.6), p(20.2, 7.0), p(17.4, 9.4)],
                thin_stroke,
            ));
            painter.add(Shape::line(
                vec![p(17.4, 14.6), p(20.2, 17.0), p(17.4, 19.4)],
                thin_stroke,
            ));
        }

        Icon::SkipBack => {
            // Vertical bar + filled left-pointing triangle
            let bar = Rect::from_min_max(p(5.2, 5.8), p(7.4, 18.2));
            painter.rect_filled(bar, CornerRadius::same(1), color);
            painter.add(Shape::convex_polygon(
                vec![p(18.6, 5.8), p(18.6, 18.2), p(8.4, 12.0)],
                color,
                Stroke::NONE,
            ));
        }

        Icon::SkipForward => {
            // Filled right-pointing triangle + vertical bar
            painter.add(Shape::convex_polygon(
                vec![p(5.4, 5.8), p(5.4, 18.2), p(15.6, 12.0)],
                color,
                Stroke::NONE,
            ));
            let bar = Rect::from_min_max(p(16.6, 5.8), p(18.8, 18.2));
            painter.rect_filled(bar, CornerRadius::same(1), color);
        }

        Icon::Play => {
            // Optically shifted slightly right (+1.0 on 24 grid) for visual center balance
            painter.add(Shape::convex_polygon(
                vec![p(7.8, 5.2), p(7.8, 18.8), p(19.2, 12.0)],
                color,
                Stroke::NONE,
            ));
        }

        Icon::Pause => {
            let left = Rect::from_min_max(p(6.4, 5.5), p(10.3, 18.5));
            let right = Rect::from_min_max(p(13.7, 5.5), p(17.6, 18.5));
            let r = (s * 1.4).round().clamp(1.0, 3.0) as u8;
            painter.rect_filled(left, CornerRadius::same(r), color);
            painter.rect_filled(right, CornerRadius::same(r), color);
        }

        Icon::Repeat | Icon::RepeatOne => {
            // Top path -> right
            painter.add(Shape::line(
                vec![p(4.2, 11.2), p(4.2, 8.4), p(6.2, 6.6), p(19.5, 6.6)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![p(16.9, 4.2), p(19.7, 6.6), p(16.9, 9.0)],
                thin_stroke,
            ));
            // Bottom path -> left
            painter.add(Shape::line(
                vec![p(19.8, 12.8), p(19.8, 15.6), p(17.8, 17.4), p(4.5, 17.4)],
                stroke,
            ));
            painter.add(Shape::line(
                vec![p(7.1, 15.0), p(4.3, 17.4), p(7.1, 19.8)],
                thin_stroke,
            ));
            if matches!(icon, Icon::RepeatOne) {
                painter.text(
                    p(12.0, 12.0),
                    egui::Align2::CENTER_CENTER,
                    "1",
                    FontId::proportional((size * 0.42).max(8.0)),
                    color,
                );
            }
        }

        Icon::Lyrics => {
            // Speech/lyrics bubble with aligned lyric lines inside
            let bubble = Rect::from_min_max(p(3.8, 4.2), p(20.2, 16.8));
            painter.rect_stroke(bubble, CornerRadius::same(4), stroke, StrokeKind::Middle);
            painter.line_segment([p(7.4, 8.6), p(16.6, 8.6)], thin_stroke);
            painter.line_segment([p(7.4, 12.4), p(13.8, 12.4)], thin_stroke);
            painter.add(Shape::line(
                vec![p(8.2, 16.8), p(6.2, 20.4), p(11.6, 16.8)],
                thin_stroke,
            ));
        }

        Icon::Queue => {
            // Stacked track lines + small play chevron on top-left
            painter.add(Shape::convex_polygon(
                vec![p(4.2, 5.2), p(4.2, 10.6), p(8.4, 7.9)],
                color,
                Stroke::NONE,
            ));
            painter.line_segment([p(11.0, 7.9), p(20.0, 7.9)], stroke);
            painter.line_segment([p(4.2, 13.2), p(20.0, 13.2)], stroke);
            painter.line_segment([p(4.2, 18.2), p(16.2, 18.2)], stroke);
        }

        Icon::VolumeMute | Icon::VolumeLow | Icon::VolumeHigh => {
            // Speaker cone
            painter.add(Shape::closed_line(
                vec![
                    p(3.6, 9.6),
                    p(7.5, 9.6),
                    p(12.2, 5.6),
                    p(12.2, 18.4),
                    p(7.5, 14.4),
                    p(3.6, 14.4),
                ],
                stroke,
            ));
            match icon {
                Icon::VolumeMute => {
                    painter.line_segment([p(15.6, 9.4), p(20.6, 14.6)], stroke);
                    painter.line_segment([p(20.6, 9.4), p(15.6, 14.6)], stroke);
                }
                Icon::VolumeLow => {
                    let wave1 = arc_points(p(13.0, 12.0), 3.6 * s, -0.78, 0.78, 10);
                    painter.add(Shape::line(wave1, stroke));
                }
                Icon::VolumeHigh => {
                    let wave1 = arc_points(p(13.0, 12.0), 3.6 * s, -0.78, 0.78, 10);
                    let wave2 = arc_points(p(13.0, 12.0), 6.8 * s, -0.82, 0.82, 12);
                    painter.add(Shape::line(wave1, stroke));
                    painter.add(Shape::line(wave2, stroke));
                }
                _ => {}
            }
        }

        Icon::Speaker => {
            // Connect device / hi-fi speaker enclosure
            let body = Rect::from_min_max(p(5.8, 3.2), p(18.2, 20.8));
            painter.rect_stroke(body, CornerRadius::same(3), stroke, StrokeKind::Middle);
            painter.circle_filled(p(12.0, 7.6), 1.35 * s, color);
            painter.circle_stroke(p(12.0, 14.6), 3.2 * s, thin_stroke);
            painter.circle_filled(p(12.0, 14.6), 0.9 * s, color);
        }

        Icon::Connect => {
            // Broadcast / Spotify Connect waves
            painter.circle_filled(p(12.0, 14.2), 1.8 * s, color);
            let inner_l = arc_points(
                p(12.0, 14.2),
                4.6 * s,
                std::f32::consts::PI * 0.75,
                std::f32::consts::PI * 1.25,
                8,
            );
            let inner_r = arc_points(
                p(12.0, 14.2),
                4.6 * s,
                -std::f32::consts::PI * 0.25,
                std::f32::consts::PI * 0.25,
                8,
            );
            let outer_l = arc_points(
                p(12.0, 14.2),
                8.2 * s,
                std::f32::consts::PI * 0.72,
                std::f32::consts::PI * 1.28,
                10,
            );
            let outer_r = arc_points(
                p(12.0, 14.2),
                8.2 * s,
                -std::f32::consts::PI * 0.28,
                std::f32::consts::PI * 0.28,
                10,
            );
            painter.add(Shape::line(inner_l, thin_stroke));
            painter.add(Shape::line(inner_r, thin_stroke));
            painter.add(Shape::line(outer_l, thin_stroke));
            painter.add(Shape::line(outer_r, thin_stroke));
            painter.line_segment([p(12.0, 16.0), p(12.0, 20.2)], thin_stroke);
            painter.line_segment([p(9.0, 20.2), p(15.0, 20.2)], thin_stroke);
        }

        Icon::LocalDrive => {
            // Local hard drive / device player
            let drive = Rect::from_min_max(p(3.8, 13.0), p(20.2, 19.6));
            painter.rect_stroke(drive, CornerRadius::same(3), stroke, StrokeKind::Middle);
            painter.add(Shape::line(
                vec![p(4.4, 13.2), p(6.6, 5.4), p(17.4, 5.4), p(19.6, 13.2)],
                stroke,
            ));
            painter.circle_filled(p(16.6, 16.3), 1.15 * s, color);
        }

        Icon::Expand => {
            // Diagonal expand corners (Now Playing fullscreen)
            painter.add(Shape::line(
                vec![p(14.5, 4.5), p(19.5, 4.5), p(19.5, 9.5)],
                stroke,
            ));
            painter.line_segment([p(13.6, 10.4), p(19.4, 4.6)], stroke);
            painter.add(Shape::line(
                vec![p(9.5, 19.5), p(4.5, 19.5), p(4.5, 14.5)],
                stroke,
            ));
            painter.line_segment([p(10.4, 13.6), p(4.6, 19.4)], stroke);
        }

        Icon::Close => {
            painter.line_segment([p(6.2, 6.2), p(17.8, 17.8)], stroke);
            painter.line_segment([p(17.8, 6.2), p(6.2, 17.8)], stroke);
        }

        Icon::Plus => {
            painter.line_segment([p(12.0, 5.2), p(12.0, 18.8)], stroke);
            painter.line_segment([p(5.2, 12.0), p(18.8, 12.0)], stroke);
        }

        Icon::ListPlus => {
            painter.line_segment([p(3.8, 6.5), p(15.2, 6.5)], stroke);
            painter.line_segment([p(3.8, 11.8), p(15.2, 11.8)], stroke);
            painter.line_segment([p(3.8, 17.1), p(11.4, 17.1)], stroke);
            painter.line_segment([p(17.5, 13.5), p(17.5, 20.5)], stroke);
            painter.line_segment([p(14.0, 17.0), p(21.0, 17.0)], stroke);
        }

        Icon::Refresh => {
            let arc = arc_points(p(12.0, 12.0), 7.4 * s, -0.55, TAU - 1.15, 22);
            painter.add(Shape::line(arc, stroke));
            painter.add(Shape::line(
                vec![p(15.4, 4.4), p(18.8, 6.6), p(16.6, 10.0)],
                thin_stroke,
            ));
        }

        Icon::LogOut => {
            painter.add(Shape::line(
                vec![p(10.5, 4.5), p(5.0, 4.5), p(5.0, 19.5), p(10.5, 19.5)],
                stroke,
            ));
            painter.line_segment([p(10.2, 12.0), p(19.6, 12.0)], stroke);
            painter.add(Shape::line(
                vec![p(16.0, 8.2), p(19.8, 12.0), p(16.0, 15.8)],
                stroke,
            ));
        }

        Icon::Check => {
            painter.add(Shape::line(
                vec![p(5.2, 12.4), p(9.8, 17.2), p(19.0, 7.2)],
                Stroke::new(sw * 1.15, color),
            ));
        }

        Icon::Alert => {
            painter.circle_stroke(p(12.0, 12.0), 8.2 * s, stroke);
            painter.line_segment([p(12.0, 7.6), p(12.0, 13.0)], stroke);
            painter.circle_filled(p(12.0, 16.4), 1.15 * s, color);
        }

        Icon::ArrowUp => {
            painter.line_segment([p(12.0, 18.6), p(12.0, 5.8)], stroke);
            painter.add(Shape::line(
                vec![p(7.2, 10.4), p(12.0, 5.6), p(16.8, 10.4)],
                stroke,
            ));
        }

        Icon::ArrowDown => {
            painter.line_segment([p(12.0, 5.4), p(12.0, 18.2)], stroke);
            painter.add(Shape::line(
                vec![p(7.2, 13.6), p(12.0, 18.4), p(16.8, 13.6)],
                stroke,
            ));
        }
    }
}

fn heart_polygon(p: &impl Fn(f32, f32) -> Pos2) -> Vec<Pos2> {
    vec![
        p(12.0, 19.4),
        p(5.4, 13.2),
        p(4.1, 10.4),
        p(4.4, 7.4),
        p(6.4, 5.6),
        p(9.1, 5.6),
        p(11.1, 7.2),
        p(12.0, 8.5),
        p(12.9, 7.2),
        p(14.9, 5.6),
        p(17.6, 5.6),
        p(19.6, 7.4),
        p(19.9, 10.4),
        p(18.6, 13.2),
    ]
}

fn arc_points(center: Pos2, radius: f32, start_rad: f32, end_rad: f32, steps: usize) -> Vec<Pos2> {
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            let a = start_rad + (end_rad - start_rad) * t;
            Pos2::new(center.x + a.cos() * radius, center.y + a.sin() * radius)
        })
        .collect()
}

/// Paint three crisp equalizer bars representing active audio playback.
pub fn paint_equalizer_bars(
    painter: &Painter,
    rect: Rect,
    color: Color32,
    time_secs: f64,
    animate: bool,
) {
    let bar_w = (rect.width() * 0.21).clamp(2.0, 3.2);
    let gap = bar_w * 0.75;
    let total_w = bar_w * 3.0 + gap * 2.0;
    let start_x = rect.center().x - total_w * 0.5;
    let max_h = rect.height() * 0.72;
    let bottom_y = rect.center().y + max_h * 0.5;

    let heights = if animate {
        [
            0.35 + 0.65 * (0.5 + 0.5 * (time_secs * 6.2).sin() as f32),
            0.40 + 0.60 * (0.5 + 0.5 * (time_secs * 7.8 + 1.4).sin() as f32),
            0.30 + 0.70 * (0.5 + 0.5 * (time_secs * 5.4 + 2.8).sin() as f32),
        ]
    } else {
        [0.55, 0.92, 0.42]
    };

    for (i, h_ratio) in heights.iter().enumerate() {
        let h = (max_h * *h_ratio).max(3.0);
        let x = start_x + i as f32 * (bar_w + gap);
        let bar = Rect::from_min_size(Pos2::new(x, bottom_y - h), Vec2::new(bar_w, h));
        painter.rect_filled(bar, CornerRadius::same(1), color);
    }
}

/// Paint the original Spotirust acoustic-wave brand badge.
pub fn paint_brand_mark(painter: &Painter, rect: Rect, accent: Color32, fg: Color32) {
    let r = rect.width().min(rect.height()) * 0.5;
    let c = rect.center();
    painter.circle_filled(c, r, accent);
    let bar_w = (r * 0.21).clamp(2.0, 3.0);
    for (dx, h_scale) in [(-0.38, 0.46), (0.0, 0.74), (0.38, 0.38)] {
        let h = r * h_scale * 2.0;
        let x = c.x + dx * r;
        let bar = Rect::from_center_size(Pos2::new(x, c.y), Vec2::new(bar_w, h));
        painter.rect_filled(bar, CornerRadius::same(2), fg);
    }
}

/// Borderless circular/rounded icon button with smooth hover surface, pointer cursor,
/// and optional active indicator dot underneath (Apple / Spotify polish).
#[allow(clippy::too_many_arguments)]
pub fn icon_button(
    ui: &mut Ui,
    theme: &ThemePalette,
    icon: Icon,
    icon_size: f32,
    hit_size: f32,
    active: bool,
    enabled: bool,
    tooltip: &str,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(hit_size), sense);
    let hovered = enabled && response.hovered();
    let pressed = enabled && response.is_pointer_button_down_on();

    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let bg_alpha = ui.ctx().animate_bool_responsive(response.id, hovered);
    if bg_alpha > 0.01 {
        let hover_col = theme.hover_overlay();
        let fill = Color32::from_rgba_unmultiplied(
            hover_col.r(),
            hover_col.g(),
            hover_col.b(),
            (f32::from(hover_col.a()) * bg_alpha * if pressed { 1.4 } else { 1.0 }).min(255.0)
                as u8,
        );
        ui.painter()
            .circle_filled(rect.center(), hit_size * 0.48, fill);
    }

    let glyph_color = if !enabled {
        theme.text_muted.linear_multiply(0.45)
    } else if active {
        theme.accent
    } else if hovered {
        theme.text_primary
    } else {
        theme.text_secondary
    };

    let shrink = if pressed { 0.92 } else { 1.0 };
    let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(icon_size * shrink));
    paint_icon(ui.painter(), icon_rect, icon, glyph_color);

    if active && enabled && !matches!(icon, Icon::HeartFilled) {
        ui.painter().circle_filled(
            Pos2::new(rect.center().x, rect.bottom() - 2.5),
            1.8,
            theme.accent,
        );
    }

    if response.has_focus() {
        ui.painter().circle_stroke(
            rect.center(),
            hit_size * 0.48,
            Stroke::new(2.0, theme.accent),
        );
    }

    if !tooltip.is_empty() {
        response.widget_info(|| {
            if active {
                egui::WidgetInfo::selected(egui::Role::Button, enabled, true, tooltip)
            } else {
                egui::WidgetInfo::labeled(egui::Role::Button, enabled, tooltip)
            }
        });
    }

    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// Crisp pill-shaped button with a leading vector icon and label.
/// `primary = true` renders the single accented CTA treatment; `false` renders a quiet secondary pill.
pub fn pill_button(
    ui: &mut Ui,
    theme: &ThemePalette,
    icon: Option<Icon>,
    label: &str,
    primary: bool,
    enabled: bool,
) -> Response {
    let font_id = FontId::proportional(12.5);
    let text_galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, Color32::PLACEHOLDER);
    let icon_w = if icon.is_some() { 15.0 } else { 0.0 };
    let icon_gap = if icon.is_some() && !label.is_empty() {
        7.0
    } else {
        0.0
    };
    let h_pad = 14.0;
    let total_w = h_pad * 2.0 + icon_w + icon_gap + text_galley.size().x;
    let height = 32.0;

    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(total_w, height), sense);
    let hovered = enabled && response.hovered();
    let pressed = enabled && response.is_pointer_button_down_on();

    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let (fill, fg, stroke) = if !enabled {
        (
            theme.bg_elevated.linear_multiply(0.7),
            theme.text_muted,
            Stroke::new(1.0, theme.hairline()),
        )
    } else if primary {
        let bg = if pressed {
            theme.accent.linear_multiply(0.92)
        } else if hovered {
            theme.accent_hover
        } else {
            theme.accent
        };
        (bg, theme.on_accent(), Stroke::NONE)
    } else {
        let bg = if pressed || hovered {
            theme.bg_hover
        } else {
            theme.bg_elevated
        };
        (bg, theme.text_primary, Stroke::new(1.0, theme.hairline()))
    };

    let draw_rect = if pressed { rect.shrink(0.5) } else { rect };
    let r_px = (height * 0.5) as u8;
    let radius = CornerRadius::same(r_px);
    if primary && enabled {
        ui.painter()
            .add(theme.shadow_soft().as_shape(draw_rect, radius));
    }
    ui.painter().rect_filled(draw_rect, radius, fill);
    if enabled {
        let inset = f32::from(r_px).min(draw_rect.width() * 0.35);
        if draw_rect.width() > inset * 2.0 + 4.0 {
            ui.painter().line_segment(
                [
                    Pos2::new(draw_rect.left() + inset, draw_rect.top() + 0.9),
                    Pos2::new(draw_rect.right() - inset, draw_rect.top() + 0.9),
                ],
                Stroke::new(
                    1.0,
                    Color32::from_white_alpha(if primary {
                        85
                    } else if theme.dark {
                        48
                    } else {
                        220
                    }),
                ),
            );
        }
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            draw_rect,
            radius,
            Stroke::new(2.0, theme.accent),
            StrokeKind::Outside,
        );
    } else if stroke != Stroke::NONE {
        ui.painter()
            .rect_stroke(draw_rect, radius, stroke, StrokeKind::Inside);
    }

    let mut cursor_x = draw_rect.left() + h_pad;
    if let Some(ic) = icon {
        let ir = Rect::from_center_size(
            Pos2::new(cursor_x + icon_w * 0.5, draw_rect.center().y),
            Vec2::splat(icon_w),
        );
        paint_icon(ui.painter(), ir, ic, fg);
        cursor_x += icon_w + icon_gap;
    }

    if !label.is_empty() {
        let text_pos = Pos2::new(cursor_x, draw_rect.center().y - text_galley.size().y * 0.5);
        ui.painter().galley(text_pos, text_galley, fg);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    }

    response
}

/// Floating Liquid Glass pill button with leading vector icon + label (toolbar/drawer actions).
pub fn ghost_pill_button(
    ui: &mut Ui,
    theme: &ThemePalette,
    icon: Icon,
    label: &str,
    active: bool,
) -> Response {
    let font_id = FontId::proportional(12.0);
    let text_galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, Color32::PLACEHOLDER);
    let icon_w = 14.0;
    let icon_gap = if label.is_empty() { 0.0 } else { 6.0 };
    let h_pad = 12.0;
    let total_w = h_pad * 2.0 + icon_w + icon_gap + text_galley.size().x;
    let height = 32.0;

    let (rect, response) = ui.allocate_exact_size(Vec2::new(total_w, height), Sense::click());
    let hovered = response.hovered();
    let pressed = response.is_pointer_button_down_on();
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let (fill, fg, stroke) = if active {
        (
            theme.accent_soft(),
            theme.accent,
            Stroke::new(1.0, theme.accent.linear_multiply(0.55)),
        )
    } else if hovered {
        (
            theme.bg_hover,
            theme.text_primary,
            Stroke::new(1.0, theme.glass_border()),
        )
    } else {
        (
            theme.glass_pill_fill(),
            theme.text_secondary,
            Stroke::new(1.0, theme.glass_border()),
        )
    };

    let draw_rect = if pressed { rect.shrink(0.5) } else { rect };
    let r_px = (height * 0.5) as u8;
    let radius = CornerRadius::same(r_px);
    ui.painter()
        .add(theme.shadow_soft().as_shape(draw_rect, radius));
    ui.painter().rect_filled(draw_rect, radius, fill);
    if response.has_focus() {
        ui.painter().rect_stroke(
            draw_rect,
            radius,
            Stroke::new(2.0, theme.accent),
            StrokeKind::Outside,
        );
    } else {
        ui.painter()
            .rect_stroke(draw_rect, radius, stroke, StrokeKind::Inside);
    }
    let inset = f32::from(r_px).min(draw_rect.width() * 0.35);
    if draw_rect.width() > inset * 2.0 + 4.0 {
        ui.painter().line_segment(
            [
                Pos2::new(draw_rect.left() + inset, draw_rect.top() + 0.9),
                Pos2::new(draw_rect.right() - inset, draw_rect.top() + 0.9),
            ],
            Stroke::new(
                1.0,
                if active {
                    Color32::from_rgba_unmultiplied(
                        theme.accent.r(),
                        theme.accent.g(),
                        theme.accent.b(),
                        120,
                    )
                } else {
                    theme.glass_specular_rim()
                },
            ),
        );
    }

    let ir = Rect::from_center_size(
        Pos2::new(
            draw_rect.left() + h_pad + icon_w * 0.5,
            draw_rect.center().y,
        ),
        Vec2::splat(icon_w),
    );
    paint_icon(ui.painter(), ir, icon, fg);

    if !label.is_empty() {
        let text_pos = Pos2::new(
            draw_rect.left() + h_pad + icon_w + icon_gap,
            draw_rect.center().y - text_galley.size().y * 0.5,
        );
        ui.painter().galley(text_pos, text_galley, fg);
        response
            .widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, active, label));
    }

    response
}
