//! Persistent docked bottom Player Bar and collapsible Queue drawer.
//!
//! Features crisp geometric vector transport icons, optically centered high-contrast
//! Play/Pause control, tabular monospaced timestamps, variable-wave volume icon,
//! and responsive tactile scrubber bars.

use crate::core::events::AppCommand;
use crate::core::models::{RepeatMode, Route, format_duration_ms};
use crate::core::state::{AppState, SideDrawer};
use crate::ui::components::artwork::{ArtworkTextureCache, deterministic_rgb, paint_artwork};
use crate::ui::components::cards::empty_state;
use crate::ui::components::icons::{Icon, icon_button, paint_equalizer_bars, paint_icon};
use crate::ui::theme::{ThemePalette, paint_liquid_glass};
use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Layout, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, Ui, Vec2,
};

/// Render the floating iOS 26 / macOS Tahoe Liquid Glass player dock with deterministic zones.
pub fn render_player_bar(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    let dock_rect = ui.available_rect_before_wrap();
    ui.allocate_rect(dock_rect, Sense::hover());

    // Subtle album-color refraction wash blended softly into the floating Liquid Glass capsule
    let album_tint = state.playback.current_track.as_ref().map(|track| {
        let [r, g, b] = track
            .accent_rgb
            .or_else(|| {
                track
                    .artwork_url
                    .as_deref()
                    .and_then(|u| artwork.dominant_color(u))
            })
            .unwrap_or_else(|| deterministic_rgb(&track.title));
        Color32::from_rgba_unmultiplied(r, g, b, if theme.dark { 22 } else { 14 })
    });

    paint_liquid_glass(
        ui.painter(),
        dock_rect,
        26,
        theme.glass_fill(),
        album_tint,
        theme,
        true,
    );

    let bar_rect = dock_rect.shrink2(Vec2::new(16.0, 6.0));
    let total_w = bar_rect.width();
    let bar_h = bar_rect.height();
    let cy = bar_rect.center().y;
    let gap = 14.0;
    let left_w = (total_w * 0.29).clamp(200.0, 330.0);
    let right_w = (total_w * 0.27).clamp(206.0, 320.0);
    let center_w = (total_w - left_w - right_w - gap * 2.0).max(240.0);

    let left_rect = Rect::from_min_max(
        Pos2::new(bar_rect.left(), bar_rect.top()),
        Pos2::new(bar_rect.left() + left_w, bar_rect.bottom()),
    );
    let center_rect = Rect::from_min_max(
        Pos2::new(left_rect.right() + gap, bar_rect.top()),
        Pos2::new(left_rect.right() + gap + center_w, bar_rect.bottom()),
    );
    let right_rect = Rect::from_min_max(
        Pos2::new(center_rect.right() + gap, bar_rect.top()),
        Pos2::new(bar_rect.right(), bar_rect.bottom()),
    );

    // 1. LEFT: Concentric Rounded Artwork (13px inside 26px dock) + Title / Artist / Status + Heart
    {
        let art_size = 54.0_f32.min(bar_h);
        let art_rect = Rect::from_center_size(
            Pos2::new(left_rect.left() + art_size * 0.5, cy),
            Vec2::splat(art_size),
        );

        if let Some(track) = state.playback.current_track.clone() {
            let art_resp = ui.interact(art_rect, ui.scope_id().with("player_art"), Sense::click());
            ui.painter().add(
                theme
                    .shadow_soft()
                    .as_shape(art_rect, CornerRadius::same(13)),
            );
            paint_artwork(
                ui,
                art_rect,
                13,
                track.artwork_url.as_deref(),
                &track.title,
                track.accent_rgb,
                artwork,
                commands,
            );
            if art_resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                ui.painter().rect_filled(
                    art_rect,
                    CornerRadius::same(13),
                    Color32::from_black_alpha(115),
                );
                paint_icon(
                    ui.painter(),
                    Rect::from_center_size(art_rect.center(), Vec2::splat(18.0)),
                    Icon::Expand,
                    Color32::WHITE,
                );
            }
            if art_resp.on_hover_text("Open Now Playing").clicked() {
                state.navigate(Route::NowPlaying);
            }

            let text_left = art_rect.right() + 12.0;
            let text_right = (left_rect.right() - 36.0).max(text_left + 56.0);
            let text_h = 54.0_f32.min(bar_h);
            let text_rect = Rect::from_min_max(
                Pos2::new(text_left, cy - text_h * 0.5),
                Pos2::new(text_right, cy + text_h * 0.5),
            );
            let mut text_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(text_rect)
                    .layout(Layout::top_down(Align::LEFT)),
            );
            text_ui.set_clip_rect(text_rect.intersect(ui.clip_rect()));
            text_ui.spacing_mut().item_spacing.y = 1.5;

            let title_resp = text_ui.add(
                egui::Label::new(
                    RichText::new(&track.title)
                        .strong()
                        .size(13.5)
                        .color(theme.text_primary),
                )
                .truncate()
                .sense(Sense::click()),
            );
            if title_resp.hovered() {
                text_ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            if title_resp.clicked() {
                state.navigate(Route::NowPlaying);
            }

            text_ui.add(
                egui::Label::new(
                    RichText::new(track.artist_display())
                        .size(11.5)
                        .color(theme.text_secondary),
                )
                .truncate()
                .selectable(false),
            );

            let (status_line, dot_col, text_col) = playback_target_badge(&state.playback, theme);
            text_ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter().circle_filled(dot_rect.center(), 3.0, dot_col);
                ui.add(
                    egui::Label::new(RichText::new(status_line).size(10.5).color(text_col))
                        .truncate()
                        .selectable(false),
                );
            });

            let heart_rect =
                Rect::from_center_size(Pos2::new(left_rect.right() - 15.0, cy), Vec2::splat(30.0));
            let mut heart_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(heart_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            let heart_icon = if track.liked {
                Icon::HeartFilled
            } else {
                Icon::Heart
            };
            if icon_button(
                &mut heart_ui,
                theme,
                heart_icon,
                16.0,
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
                commands.push(AppCommand::ToggleLikeTrack { track });
            }
        } else {
            paint_artwork(ui, art_rect, 13, None, "spotirust", None, artwork, commands);
            let text_left = art_rect.right() + 12.0;
            let text_h = 38.0_f32.min(bar_h);
            let text_rect = Rect::from_min_max(
                Pos2::new(text_left, cy - text_h * 0.5),
                Pos2::new(left_rect.right(), cy + text_h * 0.5),
            );
            let mut text_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(text_rect)
                    .layout(Layout::top_down(Align::LEFT)),
            );
            text_ui.spacing_mut().item_spacing.y = 2.0;
            text_ui.label(
                RichText::new("Nothing playing")
                    .strong()
                    .size(13.0)
                    .color(theme.text_secondary),
            );
            let (status_line, dot_col, text_col) = playback_target_badge(&state.playback, theme);
            text_ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                let (dot_rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter().circle_filled(dot_rect.center(), 3.0, dot_col);
                ui.label(RichText::new(status_line).size(11.0).color(text_col));
            });
        }
    }

    // 2. CENTER: Optically balanced vector Transport Controls (top) + Seek Bar (bottom)
    {
        let stack_h = 38.0 + 4.0 + 18.0; // 60px cleanly centered inside 72px bar
        let stack_top = (cy - stack_h * 0.5).max(bar_rect.top());

        let controls_w = 30.0 + 32.0 + 38.0 + 32.0 + 30.0 + 4.0 * 10.0;
        let transport_rect = Rect::from_min_size(
            Pos2::new(center_rect.center().x - controls_w * 0.5, stack_top),
            Vec2::new(controls_w, 38.0),
        );
        let mut transport_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(transport_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        transport_ui.spacing_mut().item_spacing.x = 10.0;

        // Shuffle
        if icon_button(
            &mut transport_ui,
            theme,
            Icon::Shuffle,
            15.0,
            30.0,
            state.shuffle,
            true,
            if state.shuffle {
                "Shuffle: On"
            } else {
                "Shuffle: Off"
            },
        )
        .clicked()
        {
            commands.push(AppCommand::ToggleShuffle);
        }

        // Previous
        if icon_button(
            &mut transport_ui,
            theme,
            Icon::SkipBack,
            16.0,
            32.0,
            false,
            true,
            "Previous Track (Ctrl+Left)",
        )
        .clicked()
        {
            commands.push(AppCommand::PreviousTrack);
        }

        // Primary circular Play / Pause button (high-contrast neutral circle with tactile hover/press)
        let is_playing = state.playback.status.is_playing();
        let is_loading = matches!(
            state.playback.status,
            crate::audio::engine::PlaybackStatus::Loading
        );
        let (btn_rect, play_resp) =
            transport_ui.allocate_exact_size(Vec2::splat(38.0), Sense::click());
        let hovered = play_resp.hovered();
        let pressed = play_resp.is_pointer_button_down_on();
        if hovered && !is_loading {
            transport_ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        let radius = if pressed {
            16.8
        } else if hovered {
            18.4
        } else {
            17.6
        };
        let bg = if is_loading {
            theme.warning
        } else if hovered && theme.dark {
            Color32::WHITE
        } else {
            theme.play_circle_bg()
        };
        let fg = if is_loading {
            Color32::from_rgb(12, 14, 18)
        } else {
            theme.play_circle_fg()
        };

        transport_ui.painter().add(
            theme
                .shadow_soft()
                .as_shape(btn_rect, CornerRadius::same(19)),
        );
        transport_ui
            .painter()
            .circle_filled(btn_rect.center(), radius, bg);

        if is_loading {
            let c = btn_rect.center();
            for dx in [-6.0, 0.0, 6.0] {
                transport_ui
                    .painter()
                    .circle_filled(Pos2::new(c.x + dx, c.y), 2.1, fg);
            }
        } else {
            let icon = if is_playing { Icon::Pause } else { Icon::Play };
            paint_icon(
                transport_ui.painter(),
                Rect::from_center_size(btn_rect.center(), Vec2::splat(17.0)),
                icon,
                fg,
            );
        }
        play_resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Button,
                !is_loading,
                if is_playing { "Pause" } else { "Play" },
            )
        });
        if play_resp.on_hover_text("Play / Pause (Space)").clicked() && !is_loading {
            commands.push(AppCommand::TogglePlayPause);
        }

        // Next
        if icon_button(
            &mut transport_ui,
            theme,
            Icon::SkipForward,
            16.0,
            32.0,
            false,
            true,
            "Next Track (Ctrl+Right)",
        )
        .clicked()
        {
            commands.push(AppCommand::NextTrack);
        }

        // Repeat
        let (rep_icon, rep_active, rep_tip) = match state.repeat {
            RepeatMode::Off => (Icon::Repeat, false, "Repeat: Off"),
            RepeatMode::Context => (Icon::Repeat, true, "Repeat: Playlist / Context"),
            RepeatMode::Track => (Icon::RepeatOne, true, "Repeat: Current Track"),
        };
        if icon_button(
            &mut transport_ui,
            theme,
            rep_icon,
            15.0,
            30.0,
            rep_active,
            true,
            rep_tip,
        )
        .clicked()
        {
            commands.push(AppCommand::CycleRepeat);
        }

        // Seek bar with tabular monospaced timestamps
        let duration_ms = state.playback.position.duration_ms;
        let live_ms = state.playback.position.current_ms();
        let display_ms = state.seek_drag_ms.unwrap_or(live_ms);
        let seek_enabled = duration_ms > 0
            && state.playback.current_track.is_some()
            && matches!(
                state.playback.status,
                crate::audio::engine::PlaybackStatus::Playing
                    | crate::audio::engine::PlaybackStatus::Paused
            );

        let seek_rect = Rect::from_min_size(
            Pos2::new(center_rect.left(), stack_top + 42.0),
            Vec2::new(center_rect.width(), 18.0),
        );
        let mut seek_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(seek_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        seek_ui.spacing_mut().item_spacing.x = 8.0;

        let (left_ts, _) = seek_ui.allocate_exact_size(Vec2::new(38.0, 18.0), Sense::hover());
        seek_ui.painter().text(
            Pos2::new(left_ts.right(), left_ts.center().y),
            Align2::RIGHT_CENTER,
            format_duration_ms(display_ms),
            FontId::monospace(11.0),
            theme.text_muted,
        );

        let bar_w = (seek_rect.width() - 92.0).max(80.0);
        let ratio = if duration_ms > 0 {
            (display_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        if let Some((new_ratio, committed)) =
            interactive_bar(&mut seek_ui, bar_w, 18.0, ratio, theme, seek_enabled)
        {
            let target_ms = (new_ratio as f64 * duration_ms as f64).round() as u64;
            if committed {
                state.seek_drag_ms = None;
                state.last_scrolled_line_idx = None;
                commands.push(AppCommand::SeekTo {
                    position_ms: target_ms,
                });
            } else {
                state.seek_drag_ms = Some(target_ms);
            }
        }

        let (right_ts, _) = seek_ui.allocate_exact_size(Vec2::new(38.0, 18.0), Sense::hover());
        seek_ui.painter().text(
            Pos2::new(right_ts.left(), right_ts.center().y),
            Align2::LEFT_CENTER,
            format_duration_ms(duration_ms),
            FontId::monospace(11.0),
            theme.text_muted,
        );
    }

    // 3. RIGHT: Lyrics toggle, Queue toggle, Variable Volume icon + Scrubber
    {
        let mut right_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(right_rect)
                .layout(Layout::right_to_left(Align::Center)),
        );
        right_ui.spacing_mut().item_spacing.x = 6.0;

        // Volume scrubber
        let vol_w = 92.0;
        if let Some((new_vol, committed)) = interactive_bar(
            &mut right_ui,
            vol_w,
            18.0,
            state.playback.volume,
            theme,
            true,
        ) {
            state.playback.volume = new_vol;
            state.muted_previous_volume = None;
            let throttle = if state.playback.backend
                == crate::core::models::AudioBackendKind::SpotifyConnect
            {
                std::time::Duration::from_millis(200)
            } else {
                std::time::Duration::from_millis(40)
            };
            let due = committed
                || state
                    .last_volume_dispatch
                    .map(|t| t.elapsed() >= throttle)
                    .unwrap_or(true);
            if due {
                state.last_volume_dispatch = Some(std::time::Instant::now());
                commands.push(AppCommand::SetVolume { volume: new_vol });
            }
        }

        // Dynamic variable-wave volume icon
        let vol_icon = if state.playback.volume <= 0.01 {
            Icon::VolumeMute
        } else if state.playback.volume < 0.5 {
            Icon::VolumeLow
        } else {
            Icon::VolumeHigh
        };
        if icon_button(
            &mut right_ui,
            theme,
            vol_icon,
            16.0,
            30.0,
            false,
            true,
            "Mute / Unmute",
        )
        .clicked()
        {
            if state.playback.volume > 0.01 {
                state.muted_previous_volume = Some(state.playback.volume);
                state.playback.volume = 0.0;
                commands.push(AppCommand::SetVolume { volume: 0.0 });
            } else {
                let restored = state.muted_previous_volume.take().unwrap_or(0.8);
                state.playback.volume = restored;
                commands.push(AppCommand::SetVolume { volume: restored });
            }
        }

        // Queue drawer toggle
        let queue_active = state.side_drawer == SideDrawer::Queue;
        if icon_button(
            &mut right_ui,
            theme,
            Icon::Queue,
            16.0,
            30.0,
            queue_active,
            true,
            "Playback Queue",
        )
        .clicked()
        {
            state.toggle_drawer(SideDrawer::Queue);
        }

        // Lyrics drawer toggle
        let lyrics_active =
            state.side_drawer == SideDrawer::Lyrics || state.current_route == Route::NowPlaying;
        if icon_button(
            &mut right_ui,
            theme,
            Icon::Lyrics,
            16.0,
            30.0,
            lyrics_active,
            true,
            "Karaoke Lyrics",
        )
        .clicked()
        {
            state.toggle_drawer(SideDrawer::Lyrics);
        }
    }
}

/// Tactile horizontal scrubber bar with clearly visible rail, hover-tinted fill,
/// and smooth thumb reveal.
fn interactive_bar(
    ui: &mut Ui,
    width: f32,
    height: f32,
    ratio: f32,
    theme: &ThemePalette,
    enabled: bool,
) -> Option<(f32, bool)> {
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), sense);
    let hovered = enabled && (response.hovered() || response.dragged());

    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let hover_t = ui.ctx().animate_bool_responsive(response.id, hovered);
    let track_h = 4.0 + 1.5 * hover_t;
    let track_rect = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), track_h));
    let r = CornerRadius::same(3);

    let painter = ui.painter();
    painter.rect_filled(track_rect, r, theme.track_rail());

    let fill_w = (track_rect.width() * ratio.clamp(0.0, 1.0)).max(0.0);
    if fill_w > 0.5 {
        let fill_rect = Rect::from_min_size(track_rect.min, Vec2::new(fill_w, track_h));
        let fill_color = if hover_t > 0.1 {
            theme.accent
        } else {
            theme.text_primary
        };
        painter.rect_filled(fill_rect, r, fill_color);
    }

    if hover_t > 0.02 {
        let knob_x = track_rect.left() + fill_w;
        let knob_center = Pos2::new(knob_x, track_rect.center().y);
        let knob_r = 5.8 * hover_t;
        painter.circle_filled(knob_center, knob_r + 1.0, Color32::from_black_alpha(70));
        painter.circle_filled(knob_center, knob_r, Color32::WHITE);
    }

    if !enabled {
        return None;
    }

    if response.dragged() || response.clicked() || response.drag_stopped() {
        let pointer_x = response
            .interact_pointer_pos()
            .or_else(|| ui.ctx().pointer_latest_pos())
            .map(|p| p.x)
            .unwrap_or(track_rect.left());
        let new_ratio = ((pointer_x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0);
        let committed = response.clicked() || response.drag_stopped();
        return Some((new_ratio, committed));
    }

    None
}

/// Render the collapsible right-hand Queue drawer.
pub fn render_queue_drawer(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    commands: &mut Vec<AppCommand>,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("Queue")
                .size(16.0)
                .strong()
                .color(theme.text_primary),
        );
        if !state.queue_items.is_empty() {
            ui.label(
                RichText::new(format!("{} tracks", state.queue_items.len()))
                    .size(11.5)
                    .color(theme.text_muted),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if icon_button(
                ui,
                theme,
                Icon::Close,
                14.0,
                26.0,
                false,
                true,
                "Close Queue",
            )
            .clicked()
            {
                state.side_drawer = SideDrawer::None;
            }
            if !state.queue_items.is_empty() {
                let clear_galley = ui.painter().layout_no_wrap(
                    "Clear".to_string(),
                    FontId::proportional(11.5),
                    Color32::PLACEHOLDER,
                );
                let (c_rect, c_resp) = ui.allocate_exact_size(
                    Vec2::new(clear_galley.size().x + 20.0, 24.0),
                    Sense::click(),
                );
                if c_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let c_fill = if c_resp.hovered() {
                    theme.hover_overlay()
                } else {
                    theme.glass_pill_fill()
                };
                let c_r = CornerRadius::same(12);
                ui.painter().rect_filled(c_rect, c_r, c_fill);
                ui.painter().rect_stroke(
                    c_rect,
                    c_r,
                    Stroke::new(1.0, theme.glass_border()),
                    egui::StrokeKind::Inside,
                );
                ui.painter().galley(
                    Pos2::new(
                        c_rect.center().x - clear_galley.size().x * 0.5,
                        c_rect.center().y - clear_galley.size().y * 0.5,
                    ),
                    clear_galley,
                    if c_resp.hovered() {
                        theme.text_primary
                    } else {
                        theme.text_secondary
                    },
                );
                if c_resp.clicked() {
                    commands.push(AppCommand::ClearQueue);
                }
            }
        });
    });
    ui.add_space(4.0);
    let (sep_rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter().line_segment(
        [sep_rect.left_center(), sep_rect.right_center()],
        Stroke::new(1.0, theme.hairline()),
    );
    ui.add_space(8.0);

    if state.queue_items.is_empty() {
        empty_state(
            ui,
            theme,
            Icon::Queue,
            "Queue is empty",
            "Play any track or click the add-to-queue icon on a song to line it up.",
        );
        return;
    }

    let items = state.queue_items.clone();
    let current_idx = state.queue_index;
    let is_playing = state.playback.status.is_playing();
    let time = ui.input(|i| i.time);

    ScrollArea::vertical()
        .id_salt("queue_drawer_scroll")
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            for (idx, track) in items.iter().enumerate() {
                let is_cur = current_idx == Some(idx);
                let w = ui.available_width();
                let (rect, row_resp) = ui.allocate_exact_size(Vec2::new(w, 46.0), Sense::hover());
                let hovered = ui.rect_contains_pointer(rect);

                if is_cur {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(12), theme.accent_soft());
                } else if hovered {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(12), theme.hover_overlay());
                }

                let play_rect = Rect::from_min_max(
                    rect.left_top(),
                    Pos2::new(rect.right() - 32.0, rect.bottom()),
                );
                let play_resp = ui.interact(play_rect, row_resp.id.with("play"), Sense::click());
                if play_resp.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }

                // Status / index indicator
                let ind_rect = Rect::from_center_size(
                    Pos2::new(rect.left() + 18.0, rect.center().y),
                    Vec2::splat(14.0),
                );
                if is_cur && is_playing {
                    paint_equalizer_bars(ui.painter(), ind_rect, theme.accent, time, true);
                } else if hovered {
                    paint_icon(
                        ui.painter(),
                        ind_rect,
                        Icon::Play,
                        if is_cur {
                            theme.accent
                        } else {
                            theme.text_primary
                        },
                    );
                } else {
                    ui.painter().text(
                        ind_rect.center(),
                        Align2::CENTER_CENTER,
                        format!("{}", idx + 1),
                        FontId::monospace(11.0),
                        if is_cur {
                            theme.accent
                        } else {
                            theme.text_muted
                        },
                    );
                }

                // Title & artist
                let text_rect = Rect::from_min_max(
                    Pos2::new(rect.left() + 34.0, rect.top() + 5.0),
                    Pos2::new(rect.right() - 34.0, rect.bottom() - 5.0),
                );
                let mut text_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(text_rect)
                        .layout(Layout::top_down(Align::LEFT)),
                );
                text_ui.spacing_mut().item_spacing.y = 1.0;
                text_ui.add(
                    egui::Label::new(RichText::new(&track.title).size(12.5).strong().color(
                        if is_cur {
                            theme.accent
                        } else {
                            theme.text_primary
                        },
                    ))
                    .truncate()
                    .selectable(false),
                );
                text_ui.add(
                    egui::Label::new(
                        RichText::new(track.artist_display())
                            .size(11.0)
                            .color(theme.text_muted),
                    )
                    .truncate()
                    .selectable(false),
                );

                // Remove button
                let rm_rect = Rect::from_min_max(
                    Pos2::new(rect.right() - 30.0, rect.top()),
                    rect.right_bottom(),
                );
                let mut rm_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(rm_rect)
                        .layout(Layout::right_to_left(Align::Center)),
                );
                if icon_button(
                    &mut rm_ui,
                    theme,
                    Icon::Close,
                    12.0,
                    24.0,
                    false,
                    true,
                    "Remove from Queue",
                )
                .clicked()
                {
                    commands.push(AppCommand::RemoveQueueIndex { index: idx });
                }

                if play_resp.clicked() {
                    commands.push(AppCommand::PlayQueueIndex { index: idx });
                }
            }
        });
}

fn playback_target_badge(
    playback: &crate::audio::engine::PlaybackSnapshot,
    theme: &ThemePalette,
) -> (String, Color32, Color32) {
    use crate::audio::engine::PlaybackStatus;
    use crate::core::models::AudioBackendKind;

    match &playback.status {
        PlaybackStatus::Error(msg) => {
            let short: String = msg.chars().take(38).collect();
            (short, theme.danger, theme.danger)
        }
        PlaybackStatus::Loading => {
            let msg = match playback.backend {
                AudioBackendKind::SpotifySoloist => "Buffering Soloist…",
                AudioBackendKind::SpotifyConnect => "Syncing Connect…",
                AudioBackendKind::LocalRodio => "Opening audio…",
                AudioBackendKind::Librespot => "Connecting…",
            };
            (msg.to_string(), theme.warning, theme.warning)
        }
        PlaybackStatus::Reconnecting => ("Reconnecting…".to_string(), theme.warning, theme.warning),
        PlaybackStatus::Playing => {
            let msg = match playback.backend {
                AudioBackendKind::SpotifySoloist => {
                    let dev = playback.active_device_name.as_deref().unwrap_or("Soloist");
                    format!("Playing · Soloist ({dev})")
                }
                AudioBackendKind::SpotifyConnect => {
                    let dev = playback
                        .active_device_name
                        .as_deref()
                        .unwrap_or("Spotify Connect");
                    format!("Playing on {dev}")
                }
                AudioBackendKind::LocalRodio => "Local Audio · Rodio".to_string(),
                AudioBackendKind::Librespot => "Librespot".to_string(),
            };
            (msg, theme.accent, theme.text_secondary)
        }
        PlaybackStatus::Paused => {
            let msg = match playback.backend {
                AudioBackendKind::SpotifySoloist => {
                    let dev = playback.active_device_name.as_deref().unwrap_or("Soloist");
                    format!("Paused · Soloist ({dev})")
                }
                AudioBackendKind::SpotifyConnect => {
                    let dev = playback.active_device_name.as_deref().unwrap_or("Connect");
                    format!("Paused · {dev}")
                }
                AudioBackendKind::LocalRodio => "Paused · Local".to_string(),
                AudioBackendKind::Librespot => "Paused · Librespot".to_string(),
            };
            (msg, theme.text_muted, theme.text_muted)
        }
        PlaybackStatus::Stopped => {
            let msg = match playback.backend {
                AudioBackendKind::SpotifySoloist => match playback.active_device_name.as_deref() {
                    Some(dev) => format!("Soloist ready · {dev}"),
                    None => "Soloist · Configure in Settings".to_string(),
                },
                AudioBackendKind::SpotifyConnect => match playback.active_device_name.as_deref() {
                    Some(dev) => format!("Connect ready · {dev}"),
                    None => "Connect · No active device".to_string(),
                },
                AudioBackendKind::LocalRodio => "Local Player · Ready".to_string(),
                AudioBackendKind::Librespot => "Librespot · Idle".to_string(),
            };
            (msg, theme.text_muted, theme.text_muted)
        }
    }
}
