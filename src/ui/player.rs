//! Persistent bottom Player Bar and collapsible Queue drawer.

use crate::core::events::AppCommand;
use crate::core::models::{RepeatMode, Route, format_duration_ms};
use crate::core::state::{AppState, SideDrawer};
use crate::ui::components::artwork::{ArtworkTextureCache, artwork_box};
use crate::ui::theme::ThemePalette;
use egui::{Color32, CornerRadius, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, Ui, Vec2};

/// Render the fixed bottom Spotify-inspired player bar.
pub fn render_player_bar(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    let total_w = ui.available_width();
    let left_w = (total_w * 0.29).clamp(180.0, 320.0);
    let right_w = (total_w * 0.27).clamp(190.0, 310.0);
    let center_w = (total_w - left_w - right_w - 24.0).max(220.0);

    ui.horizontal_centered(|ui| {
        // 1. LEFT: Album Artwork + Track Title + Artist + Like Button
        ui.allocate_ui_with_layout(
            Vec2::new(left_w, 68.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                if let Some(track) = state.playback.current_track.clone() {
                    let art_resp = artwork_box(
                        ui,
                        54.0,
                        8,
                        track.artwork_url.as_deref(),
                        &track.title,
                        track.accent_rgb,
                        artwork,
                        commands,
                    );
                    if art_resp.on_hover_text("Open Now Playing").clicked() {
                        state.navigate(Route::NowPlaying);
                    }

                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.set_max_width((left_w - 106.0).max(80.0));
                        let title_resp = ui.add(
                            egui::Label::new(
                                RichText::new(&track.title)
                                    .strong()
                                    .size(13.5)
                                    .color(theme.text_primary),
                            )
                            .truncate()
                            .sense(Sense::click()),
                        );
                        if title_resp.clicked() {
                            state.navigate(Route::NowPlaying);
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(track.artist_display())
                                    .size(11.5)
                                    .color(theme.text_secondary),
                            )
                            .truncate(),
                        );
                        let (status_line, status_col) =
                            playback_target_badge(&state.playback, theme);
                        ui.add(
                            egui::Label::new(
                                RichText::new(status_line).size(10.0).color(status_col),
                            )
                            .truncate(),
                        );
                    });

                    ui.add_space(4.0);
                    let heart = if track.liked { "♥" } else { "♡" };
                    let heart_col = if track.liked {
                        theme.accent
                    } else {
                        theme.text_secondary
                    };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(heart).size(16.0).color(heart_col))
                                .frame(false),
                        )
                        .on_hover_text("Like / Save to Library")
                        .clicked()
                    {
                        commands.push(AppCommand::ToggleLikeTrack { track });
                    }
                } else {
                    artwork_box(ui, 54.0, 8, None, "spotirust", None, artwork, commands);
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Nothing playing")
                                .strong()
                                .size(13.0)
                                .color(theme.text_secondary),
                        );
                        let (status_line, status_col) =
                            playback_target_badge(&state.playback, theme);
                        ui.label(RichText::new(status_line).size(10.5).color(status_col));
                    });
                }
            },
        );

        ui.add_space(6.0);

        // 2. CENTER: Transport controls + custom interactive seek bar
        ui.allocate_ui_with_layout(
            Vec2::new(center_w, 68.0),
            egui::Layout::top_down(egui::Align::Center),
            |ui| {
                // Transport button row
                ui.horizontal(|ui| {
                    let approx_controls_w = 212.0;
                    let pad = ((center_w - approx_controls_w) * 0.5).max(0.0);
                    ui.add_space(pad);

                    // Shuffle
                    let shuffle_col = if state.shuffle {
                        theme.accent
                    } else {
                        theme.text_secondary
                    };
                    if ui
                        .add(
                            egui::Button::new(RichText::new("🔀").size(14.0).color(shuffle_col))
                                .frame(false),
                        )
                        .on_hover_text("Toggle Shuffle")
                        .clicked()
                    {
                        commands.push(AppCommand::ToggleShuffle);
                    }

                    // Previous
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("⏮").size(16.0).color(theme.text_primary),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Previous Track")
                        .clicked()
                    {
                        commands.push(AppCommand::PreviousTrack);
                    }

                    // Circular Play / Pause / Syncing button
                    let is_playing = state.playback.status.is_playing();
                    let is_loading = matches!(
                        state.playback.status,
                        crate::audio::engine::PlaybackStatus::Loading
                    );
                    let (btn_rect, play_resp) =
                        ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
                    let bg = if is_loading {
                        theme.warning
                    } else if play_resp.hovered() {
                        theme.accent_hover
                    } else {
                        theme.accent
                    };
                    ui.painter().circle_filled(btn_rect.center(), 17.0, bg);
                    let c = btn_rect.center();
                    if is_loading {
                        // Three sync dots while waiting for Connect confirmation / file open
                        for dx in [-6.0, 0.0, 6.0] {
                            ui.painter().circle_filled(
                                Pos2::new(c.x + dx, c.y),
                                2.2,
                                Color32::BLACK,
                            );
                        }
                    } else if is_playing {
                        // Pause bars
                        let bar_w = 3.2;
                        let bar_h = 12.0;
                        ui.painter().rect_filled(
                            Rect::from_center_size(
                                Pos2::new(c.x - 4.0, c.y),
                                Vec2::new(bar_w, bar_h),
                            ),
                            CornerRadius::same(1),
                            Color32::BLACK,
                        );
                        ui.painter().rect_filled(
                            Rect::from_center_size(
                                Pos2::new(c.x + 4.0, c.y),
                                Vec2::new(bar_w, bar_h),
                            ),
                            CornerRadius::same(1),
                            Color32::BLACK,
                        );
                    } else {
                        // Crisp Play triangle
                        let tri = vec![
                            Pos2::new(c.x - 4.5, c.y - 7.0),
                            Pos2::new(c.x - 4.5, c.y + 7.0),
                            Pos2::new(c.x + 7.5, c.y),
                        ];
                        ui.painter().add(egui::Shape::convex_polygon(
                            tri,
                            Color32::BLACK,
                            Stroke::NONE,
                        ));
                    }
                    if play_resp.on_hover_text("Play / Pause (Space)").clicked() && !is_loading {
                        commands.push(AppCommand::TogglePlayPause);
                    }

                    // Next
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("⏭").size(16.0).color(theme.text_primary),
                            )
                            .frame(false),
                        )
                        .on_hover_text("Next Track")
                        .clicked()
                    {
                        commands.push(AppCommand::NextTrack);
                    }

                    // Repeat
                    let (rep_icon, rep_col) = match state.repeat {
                        RepeatMode::Off => ("🔁", theme.text_secondary),
                        RepeatMode::Context => ("🔁", theme.accent),
                        RepeatMode::Track => ("🔂", theme.accent),
                    };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(rep_icon).size(14.0).color(rep_col))
                                .frame(false),
                        )
                        .on_hover_text(format!("Repeat: {:?}", state.repeat))
                        .clicked()
                    {
                        commands.push(AppCommand::CycleRepeat);
                    }
                });

                ui.add_space(2.0);

                // Progress / Seek bar (only interactive when real duration & active track exist)
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

                ui.horizontal(|ui| {
                    ui.add_sized(
                        [38.0, 16.0],
                        egui::Label::new(
                            RichText::new(format_duration_ms(display_ms))
                                .size(11.0)
                                .color(theme.text_muted),
                        ),
                    );

                    let bar_w = (center_w - 92.0).max(90.0);
                    let ratio = if duration_ms > 0 {
                        (display_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    if let Some((new_ratio, committed)) =
                        interactive_bar(ui, bar_w, 16.0, ratio, theme, seek_enabled)
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

                    ui.add_sized(
                        [38.0, 16.0],
                        egui::Label::new(
                            RichText::new(format_duration_ms(duration_ms))
                                .size(11.0)
                                .color(theme.text_muted),
                        ),
                    );
                });
            },
        );

        ui.add_space(6.0);

        // 3. RIGHT: Lyrics drawer, Queue drawer, Mute + Volume bar
        ui.allocate_ui_with_layout(
            Vec2::new(right_w, 68.0),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                // Volume bar (throttled in Connect mode to prevent HTTP rate-limit spam)
                let vol_w = 88.0;
                if let Some((new_vol, committed)) =
                    interactive_bar(ui, vol_w, 18.0, state.playback.volume, theme, true)
                {
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

                // Mute button
                let vol_icon = if state.playback.volume <= 0.01 {
                    "🔇"
                } else if state.playback.volume < 0.5 {
                    "🔉"
                } else {
                    "🔊"
                };
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(vol_icon)
                                .size(14.0)
                                .color(theme.text_secondary),
                        )
                        .frame(false),
                    )
                    .on_hover_text("Mute / Unmute")
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
                if ui
                    .add(
                        egui::Button::new(RichText::new("≡").size(16.0).color(if queue_active {
                            theme.accent
                        } else {
                            theme.text_secondary
                        }))
                        .frame(false),
                    )
                    .on_hover_text("Playback Queue")
                    .clicked()
                {
                    state.toggle_drawer(SideDrawer::Queue);
                }

                // Compact Lyrics drawer toggle
                let lyrics_active = state.side_drawer == SideDrawer::Lyrics
                    || state.current_route == Route::NowPlaying;
                if ui
                    .add(
                        egui::Button::new(RichText::new("🎤").size(14.0).color(if lyrics_active {
                            theme.accent
                        } else {
                            theme.text_secondary
                        }))
                        .frame(false),
                    )
                    .on_hover_text("Lyrics (Karaoke)")
                    .clicked()
                {
                    state.toggle_drawer(SideDrawer::Lyrics);
                }
            },
        );
    });
}

/// Custom sleek Spotify-style horizontal progress/volume scrubber.
/// Returns `Some((new_ratio, committed))` when dragged or clicked.
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

    let track_h = if hovered { 5.0 } else { 4.0 };
    let track_rect = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), track_h));

    let painter = ui.painter();
    painter.rect_filled(track_rect, CornerRadius::same(3), theme.bg_hover);

    let fill_w = (track_rect.width() * ratio.clamp(0.0, 1.0)).max(0.0);
    let fill_rect = Rect::from_min_size(track_rect.min, Vec2::new(fill_w, track_h));
    let fill_color = if hovered {
        theme.accent
    } else {
        theme.text_primary
    };
    painter.rect_filled(fill_rect, CornerRadius::same(3), fill_color);

    if hovered {
        let knob_x = track_rect.left() + fill_w;
        painter.circle_filled(
            Pos2::new(knob_x, track_rect.center().y),
            6.0,
            Color32::WHITE,
        );
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
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("✕").clicked() {
                state.side_drawer = SideDrawer::None;
            }
            if !state.queue_items.is_empty() && ui.small_button("Clear").clicked() {
                commands.push(AppCommand::ClearQueue);
            }
        });
    });
    ui.separator();
    ui.add_space(4.0);

    if state.queue_items.is_empty() {
        ui.label(
            RichText::new("Queue is empty. Play a song or click +Q on any track.")
                .size(12.5)
                .color(theme.text_muted),
        );
        return;
    }

    let items = state.queue_items.clone();
    let current_idx = state.queue_index;

    ScrollArea::vertical()
        .id_salt("queue_drawer_scroll")
        .show(ui, |ui| {
            for (idx, track) in items.iter().enumerate() {
                let is_cur = current_idx == Some(idx);
                ui.horizontal(|ui| {
                    let label_col = if is_cur {
                        theme.accent
                    } else {
                        theme.text_primary
                    };
                    let btn = egui::Button::new(
                        RichText::new(format!(
                            "{}  {}",
                            if is_cur { "▶" } else { "•" },
                            track.title
                        ))
                        .size(12.5)
                        .color(label_col),
                    )
                    .frame(false);
                    if ui.add(btn).clicked() {
                        commands.push(AppCommand::PlayQueueIndex { index: idx });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").clicked() {
                            commands.push(AppCommand::RemoveQueueIndex { index: idx });
                        }
                    });
                });
                ui.label(
                    RichText::new(format!("    {}", track.artist_display()))
                        .size(11.0)
                        .color(theme.text_muted),
                );
                ui.add_space(4.0);
            }
        });
}

fn playback_target_badge(
    playback: &crate::audio::engine::PlaybackSnapshot,
    theme: &ThemePalette,
) -> (String, Color32) {
    use crate::audio::engine::PlaybackStatus;
    use crate::core::models::AudioBackendKind;

    match &playback.status {
        PlaybackStatus::Error(msg) => {
            let short: String = msg.chars().take(42).collect();
            (format!("⚠ {short}"), theme.danger)
        }
        PlaybackStatus::Loading => match playback.backend {
            AudioBackendKind::SpotifySoloist => {
                ("Buffering on Soloist…".to_string(), theme.warning)
            }
            AudioBackendKind::SpotifyConnect => {
                ("Syncing with Connect…".to_string(), theme.warning)
            }
            AudioBackendKind::LocalRodio => ("Opening local audio…".to_string(), theme.warning),
            AudioBackendKind::Librespot => ("Connecting Librespot…".to_string(), theme.warning),
        },
        PlaybackStatus::Reconnecting => ("Reconnecting…".to_string(), theme.warning),
        PlaybackStatus::Playing => match playback.backend {
            AudioBackendKind::SpotifySoloist => {
                let dev = playback.active_device_name.as_deref().unwrap_or("Soloist");
                (format!("● Playing on Soloist ({dev})"), theme.accent)
            }
            AudioBackendKind::SpotifyConnect => {
                let dev = playback
                    .active_device_name
                    .as_deref()
                    .unwrap_or("Spotify Connect");
                (format!("● Playing on {dev}"), theme.accent)
            }
            AudioBackendKind::LocalRodio => ("● Local Audio (Rodio)".to_string(), theme.accent),
            AudioBackendKind::Librespot => ("● Librespot".to_string(), theme.accent),
        },
        PlaybackStatus::Paused => match playback.backend {
            AudioBackendKind::SpotifySoloist => {
                let dev = playback.active_device_name.as_deref().unwrap_or("Soloist");
                (format!("Paused · Soloist ({dev})"), theme.text_muted)
            }
            AudioBackendKind::SpotifyConnect => {
                let dev = playback.active_device_name.as_deref().unwrap_or("Connect");
                (format!("Paused on {dev}"), theme.text_muted)
            }
            AudioBackendKind::LocalRodio => ("Paused · Local Player".to_string(), theme.text_muted),
            AudioBackendKind::Librespot => ("Paused · Librespot".to_string(), theme.text_muted),
        },
        PlaybackStatus::Stopped => match playback.backend {
            AudioBackendKind::SpotifySoloist => match playback.active_device_name.as_deref() {
                Some(dev) => (format!("Soloist ready · {dev}"), theme.text_muted),
                None => (
                    "Soloist mode · Configure in Settings".to_string(),
                    theme.text_muted,
                ),
            },
            AudioBackendKind::SpotifyConnect => match playback.active_device_name.as_deref() {
                Some(dev) => (format!("Connect ready · {dev}"), theme.text_muted),
                None => (
                    "Connect mode · No active device".to_string(),
                    theme.text_muted,
                ),
            },
            AudioBackendKind::LocalRodio => (
                "Local Player (Rodio) · Stopped".to_string(),
                theme.text_muted,
            ),
            AudioBackendKind::Librespot => {
                ("Librespot · Blocked/Idle".to_string(), theme.text_muted)
            }
        },
    }
}
