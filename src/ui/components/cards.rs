//! Reusable Spotify-inspired media cards, track table rows, and shimmer loading skeletons.

use crate::core::events::AppCommand;
use crate::core::models::{Track, format_duration_ms};
use crate::ui::components::artwork::{ArtworkTextureCache, paint_artwork};
use crate::ui::theme::ThemePalette;
use egui::{Color32, CornerRadius, Frame, Margin, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

/// Renders a polished square media card (Playlist, Album, Artist, or Showcase Track).
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
    let card_height = width + 72.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, card_height), Sense::click());
    let hovered = response.hovered();

    let bg = if hovered {
        theme.bg_hover
    } else {
        theme.bg_elevated
    };
    ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(
            1.0,
            if hovered {
                theme.accent.linear_multiply(0.55)
            } else {
                theme.border.linear_multiply(0.6)
            },
        ),
        StrokeKind::Inside,
    );

    let pad = 10.0;
    let art_size = width - pad * 2.0;
    let art_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + pad, rect.top() + pad),
        Vec2::splat(art_size),
    );
    paint_artwork(
        ui,
        art_rect,
        8,
        artwork_url,
        title,
        accent_rgb,
        cache,
        commands,
    );

    // Floating play pill on hover
    if hovered {
        let play_center = egui::pos2(art_rect.right() - 22.0, art_rect.bottom() - 22.0);
        ui.painter().circle_filled(play_center, 18.0, theme.accent);
        let tri = vec![
            egui::pos2(play_center.x - 5.0, play_center.y - 7.0),
            egui::pos2(play_center.x - 5.0, play_center.y + 7.0),
            egui::pos2(play_center.x + 8.0, play_center.y),
        ];
        ui.painter().add(egui::Shape::convex_polygon(
            tri,
            Color32::BLACK,
            Stroke::NONE,
        ));
    }

    // Text block below artwork
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + pad, art_rect.bottom() + 8.0),
        egui::pos2(rect.right() - pad, rect.bottom() - 8.0),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(text_rect));
    child.vertical(|ui| {
        ui.add(
            egui::Label::new(
                RichText::new(title)
                    .strong()
                    .size(13.5)
                    .color(theme.text_primary),
            )
            .truncate(),
        );
        ui.add(
            egui::Label::new(
                RichText::new(subtitle)
                    .size(11.5)
                    .color(theme.text_secondary),
            )
            .truncate(),
        );
        if let Some(tag) = badge {
            ui.label(RichText::new(tag).size(10.0).color(theme.accent));
        }
    });

    response.clicked()
}

/// Renders a single track row with index/playing indicator, artwork, title/artist, album,
/// like button, queue button, and duration.
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
    let row_bg = if is_active {
        theme.accent.linear_multiply(0.14)
    } else {
        Color32::TRANSPARENT
    };

    let frame = Frame::new()
        .fill(row_bg)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 6));

    let resp = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            // Track number or active indicator
            let idx_label = if is_active && is_playing {
                "▶".to_string()
            } else {
                format!("{:>2}", index + 1)
            };
            ui.add_sized(
                [24.0, 36.0],
                egui::Label::new(RichText::new(idx_label).size(12.5).color(if is_active {
                    theme.accent
                } else {
                    theme.text_muted
                })),
            );

            // Cover thumbnail
            let (art_rect, art_resp) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::click());
            paint_artwork(
                ui,
                art_rect,
                5,
                track.artwork_url.as_deref(),
                &track.title,
                track.accent_rgb,
                cache,
                commands,
            );
            if art_resp.clicked() {
                commands.push(AppCommand::PlayTrack {
                    track: track.clone(),
                    queue_context: context.to_vec(),
                });
            }

            ui.add_space(6.0);

            // Title & artist
            let title_width = (ui.available_width() - 250.0).max(120.0);
            ui.allocate_ui_with_layout(
                Vec2::new(title_width, 38.0),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    let title_resp = ui.add(
                        egui::Label::new(RichText::new(&track.title).size(13.5).strong().color(
                            if is_active {
                                theme.accent
                            } else {
                                theme.text_primary
                            },
                        ))
                        .truncate()
                        .sense(Sense::click()),
                    );
                    if title_resp.clicked() {
                        commands.push(AppCommand::PlayTrack {
                            track: track.clone(),
                            queue_context: context.to_vec(),
                        });
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(track.artist_display())
                                .size(11.5)
                                .color(theme.text_secondary),
                        )
                        .truncate(),
                    );
                },
            );

            // Album column (when wide enough)
            if ui.available_width() > 210.0 {
                ui.allocate_ui_with_layout(
                    Vec2::new(110.0, 36.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&track.album_name)
                                    .size(11.5)
                                    .color(theme.text_muted),
                            )
                            .truncate(),
                        );
                    },
                );
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Duration
                ui.add_sized(
                    [44.0, 24.0],
                    egui::Label::new(
                        RichText::new(format_duration_ms(track.duration_ms))
                            .size(12.0)
                            .color(theme.text_muted),
                    ),
                );

                // Add to queue
                if ui
                    .small_button("+Q")
                    .on_hover_text("Add to queue")
                    .clicked()
                {
                    commands.push(AppCommand::AddToQueue {
                        track: track.clone(),
                    });
                }

                // Like button
                let heart = if track.liked { "♥" } else { "♡" };
                let heart_color = if track.liked {
                    theme.accent
                } else {
                    theme.text_secondary
                };
                if ui
                    .add(
                        egui::Button::new(RichText::new(heart).color(heart_color).size(14.0))
                            .frame(false),
                    )
                    .on_hover_text("Toggle Liked Song")
                    .clicked()
                {
                    commands.push(AppCommand::ToggleLikeTrack {
                        track: track.clone(),
                    });
                }
            });
        });
    });

    if resp.response.double_clicked() {
        commands.push(AppCommand::PlayTrack {
            track: track.clone(),
            queue_context: context.to_vec(),
        });
    }
}

/// Animated shimmer skeleton row shown while Search or Library requests are in flight.
pub fn skeleton_track_row(ui: &mut Ui, theme: &ThemePalette, time: f64) {
    let shimmer = 0.5 + 0.5 * (time * 3.0).sin() as f32;
    let fill = Color32::from_rgba_unmultiplied(
        theme.bg_hover.r(),
        theme.bg_hover.g(),
        theme.bg_hover.b(),
        (110.0 + shimmer * 90.0) as u8,
    );

    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 48.0), Sense::hover());
    let painter = ui.painter();
    let art = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 34.0, rect.top() + 5.0),
        Vec2::splat(38.0),
    );
    painter.rect_filled(art, CornerRadius::same(6), fill);

    let bar1 = egui::Rect::from_min_size(
        egui::pos2(art.right() + 12.0, rect.top() + 9.0),
        Vec2::new((rect.width() * 0.34).min(240.0), 12.0),
    );
    painter.rect_filled(bar1, CornerRadius::same(4), fill);

    let bar2 = egui::Rect::from_min_size(
        egui::pos2(art.right() + 12.0, rect.top() + 27.0),
        Vec2::new((rect.width() * 0.22).min(150.0), 10.0),
    );
    painter.rect_filled(bar2, CornerRadius::same(4), fill.linear_multiply(0.75));
}

/// Animated shimmer skeleton media card for search/grid loading states.
pub fn skeleton_card(ui: &mut Ui, theme: &ThemePalette, width: f32, time: f64) {
    let shimmer = 0.5 + 0.5 * (time * 3.0).sin() as f32;
    let fill = Color32::from_rgba_unmultiplied(
        theme.bg_hover.r(),
        theme.bg_hover.g(),
        theme.bg_hover.b(),
        (100.0 + shimmer * 95.0) as u8,
    );
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, width + 66.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(10), theme.bg_elevated);
    let art = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 10.0, rect.top() + 10.0),
        Vec2::splat(width - 20.0),
    );
    painter.rect_filled(art, CornerRadius::same(8), fill);
    let title = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 10.0, art.bottom() + 10.0),
        Vec2::new(width * 0.65, 12.0),
    );
    painter.rect_filled(title, CornerRadius::same(4), fill);
    let sub = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 10.0, title.bottom() + 8.0),
        Vec2::new(width * 0.45, 10.0),
    );
    painter.rect_filled(sub, CornerRadius::same(4), fill.linear_multiply(0.7));
}
