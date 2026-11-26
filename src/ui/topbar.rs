//! Unified top navigation toolbar: circular vector Back/Forward chevrons,
//! pill search field with inline magnifying glass & clear action, iOS-style
//! segmented Local/Connect switcher, Connect Devices menu, Now Playing toggle,
//! and refined status toast banner.

use crate::core::events::AppCommand;
use crate::core::models::{AudioBackendKind, Route};
use crate::core::state::{AppState, AuthUiState};
use crate::ui::components::icons::{Icon, ghost_pill_button, icon_button, paint_icon};
use crate::ui::theme::{ThemePalette, paint_liquid_glass};
use egui::{
    Align, Color32, CornerRadius, CursorIcon, FontId, Frame, Layout, Margin, Pos2, Rect, RichText,
    Sense, Stroke, StrokeKind, Ui, Vec2,
};
use std::time::Instant;

pub fn render_topbar(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    commands: &mut Vec<AppCommand>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;

        // 1. Unified iOS 26 Liquid Glass Back / Forward navigation capsule
        {
            let nav_size = Vec2::new(66.0, 32.0);
            let (nav_rect, _) = ui.allocate_exact_size(nav_size, Sense::hover());
            paint_liquid_glass(
                ui.painter(),
                nav_rect,
                16,
                theme.glass_pill_fill(),
                None,
                theme,
                false,
            );
            // Subtle center vertical hairline divider
            ui.painter().line_segment(
                [
                    Pos2::new(nav_rect.center().x, nav_rect.top() + 8.0),
                    Pos2::new(nav_rect.center().x, nav_rect.bottom() - 8.0),
                ],
                Stroke::new(1.0, theme.hairline()),
            );

            let back_rect = Rect::from_center_size(
                Pos2::new(nav_rect.left() + 17.0, nav_rect.center().y),
                Vec2::splat(28.0),
            );
            let mut back_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(back_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            if icon_button(
                &mut back_ui,
                theme,
                Icon::ChevronLeft,
                15.0,
                28.0,
                false,
                state.can_go_back(),
                "Back",
            )
            .clicked()
            {
                state.go_back();
            }

            let fwd_rect = Rect::from_center_size(
                Pos2::new(nav_rect.right() - 17.0, nav_rect.center().y),
                Vec2::splat(28.0),
            );
            let mut fwd_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(fwd_rect)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            if icon_button(
                &mut fwd_ui,
                theme,
                Icon::ChevronRight,
                15.0,
                28.0,
                false,
                state.can_go_forward(),
                "Forward",
            )
            .clicked()
            {
                state.go_forward();
            }
        }

        // 2. Floating Liquid Glass Pill Search Field
        let avail_after_nav = ui.available_width();
        let compact_toolbar = avail_after_nav < 720.0;
        let is_authed = matches!(state.auth, AuthUiState::Authenticated(_));
        let device_max_chars = if compact_toolbar { 10 } else { 18 };
        let device_reserve = if is_authed {
            let char_len = state
                .playback
                .active_device_name
                .as_deref()
                .map(|s| s.chars().count().min(device_max_chars))
                .unwrap_or(7);
            54.0 + char_len as f32 * 6.8 + 8.0
        } else {
            0.0
        };
        let np_reserve = if compact_toolbar { 36.0 } else { 122.0 };
        let segment_reserve = 256.0;
        let right_reserve = np_reserve + device_reserve + segment_reserve + 24.0;
        let search_width = (avail_after_nav - right_reserve - 24.0).clamp(100.0, 340.0);

        let search_frame_id = ui.scope_id().with("topbar_search_focus");
        let was_focused = ui.data(|d| d.get_temp::<bool>(search_frame_id).unwrap_or(false));

        let (search_rect, _) =
            ui.allocate_exact_size(Vec2::new(search_width + 20.0, 32.0), Sense::hover());
        paint_liquid_glass(
            ui.painter(),
            search_rect,
            16,
            theme.glass_pill_fill(),
            None,
            theme,
            false,
        );
        if was_focused {
            ui.painter().rect_stroke(
                search_rect,
                CornerRadius::same(16),
                Stroke::new(1.3, theme.accent.linear_multiply(0.8)),
                StrokeKind::Inside,
            );
        }

        let inner_search = search_rect.shrink2(Vec2::new(11.0, 2.0));
        let mut search_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner_search)
                .layout(Layout::left_to_right(Align::Center)),
        );
        search_ui.spacing_mut().item_spacing.x = 6.0;
        let (ic_rect, _) = search_ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
        paint_icon(
            search_ui.painter(),
            ic_rect,
            Icon::Search,
            if was_focused {
                theme.text_primary
            } else {
                theme.text_muted
            },
        );

        let clear_w = if state.search_input.is_empty() {
            0.0
        } else {
            22.0
        };
        let edit_w = (search_ui.available_width() - clear_w).max(60.0);
        let search_edit = egui::TextEdit::singleline(&mut state.search_input)
            .frame(Frame::NONE)
            .align(egui::Align2::LEFT_CENTER)
            .hint_text(
                RichText::new(if compact_toolbar {
                    "Search…"
                } else {
                    "Search songs, artists, albums…"
                })
                .size(12.5)
                .color(theme.text_muted),
            )
            .desired_width(edit_w);
        let resp = search_ui.add(search_edit);
        let is_focused = resp.has_focus();
        search_ui.data_mut(|d| d.insert_temp(search_frame_id, is_focused));

        if resp.gained_focus() && state.current_route != Route::Search {
            state.navigate(Route::Search);
        }
        if resp.changed() {
            state.search_dirty_since = Some(Instant::now());
            if state.current_route != Route::Search {
                state.navigate(Route::Search);
            }
        }

        if !state.search_input.is_empty()
            && icon_button(
                &mut search_ui,
                theme,
                Icon::Close,
                12.0,
                20.0,
                false,
                true,
                "Clear search",
            )
            .clicked()
        {
            state.search_input.clear();
            state.last_dispatched_query.clear();
            state.search_results = None;
            state.search_loading = false;
            state.search_error = None;
        }

        // 3. Right control cluster (Now Playing · Connect Devices · Local/Connect Segmented Control)
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;

            // Now Playing fullscreen toggle
            let in_now_playing = state.current_route == Route::NowPlaying;
            let np_icon = if in_now_playing {
                Icon::Close
            } else {
                Icon::Expand
            };
            let np_tip = if in_now_playing {
                "Exit fullscreen Now Playing"
            } else {
                "Open fullscreen Now Playing & Lyrics"
            };
            let np_clicked = if compact_toolbar {
                icon_button(ui, theme, np_icon, 15.0, 30.0, in_now_playing, true, np_tip).clicked()
            } else {
                ghost_pill_button(
                    ui,
                    theme,
                    np_icon,
                    if in_now_playing {
                        "Exit"
                    } else {
                        "Now Playing"
                    },
                    in_now_playing,
                )
                .on_hover_text(np_tip)
                .clicked()
            };
            if np_clicked {
                if in_now_playing {
                    state.go_back();
                } else {
                    state.navigate(Route::NowPlaying);
                }
            }

            // Connect devices menu when authenticated
            if is_authed {
                render_devices_menu(ui, state, theme, device_max_chars, commands);
            }

            // Segmented Control: Local | Soloist | Connect
            render_mode_segmented_control(ui, state, theme, commands);
        });
    });

    // Optional dismissible toast/status banner
    if let Some(notice) = state.notice.clone() {
        ui.add_space(8.0);
        let accent_col = if notice.is_error {
            theme.danger
        } else {
            theme.accent
        };
        let fill = Color32::from_rgba_unmultiplied(
            accent_col.r(),
            accent_col.g(),
            accent_col.b(),
            if theme.dark { 28 } else { 22 },
        );
        Frame::new()
            .fill(fill)
            .corner_radius(CornerRadius::same(12))
            .stroke(Stroke::new(1.0, accent_col.linear_multiply(0.55)))
            .inner_margin(Margin::symmetric(12, 7))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (ic_rect, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
                    paint_icon(
                        ui.painter(),
                        ic_rect,
                        if notice.is_error {
                            Icon::Alert
                        } else {
                            Icon::Check
                        },
                        accent_col,
                    );
                    ui.label(
                        RichText::new(&notice.message)
                            .size(12.5)
                            .color(theme.text_primary),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, theme, Icon::Close, 12.0, 22.0, false, true, "Dismiss")
                            .clicked()
                        {
                            state.notice = None;
                        }
                    });
                });
            });
    }
}

fn render_mode_segmented_control(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    commands: &mut Vec<AppCommand>,
) {
    let active_mode = state.playback.backend;
    let is_local = active_mode == AudioBackendKind::LocalRodio;
    let is_soloist = active_mode == AudioBackendKind::SpotifySoloist;
    let is_connect = active_mode == AudioBackendKind::SpotifyConnect;

    Frame::new()
        .fill(theme.glass_pill_fill())
        .corner_radius(CornerRadius::same(16))
        .stroke(Stroke::new(1.0, theme.glass_border()))
        .shadow(theme.shadow_soft())
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;

                if segment_pill(ui, theme, Icon::Connect, "Connect", is_connect, false)
                    .on_hover_text(
                        "Spotify Connect Remote Controller (controls external Spotify device; requires Premium)",
                    )
                    .clicked()
                    && !is_connect
                {
                    state.settings.audio_backend = AudioBackendKind::SpotifyConnect;
                    commands.push(AppCommand::SwitchAudioBackend {
                        backend: AudioBackendKind::SpotifyConnect,
                    });
                    commands.push(AppCommand::SaveSettings(state.settings.clone()));
                }

                if segment_pill(
                    ui,
                    theme,
                    Icon::Disc,
                    "Soloist",
                    is_soloist,
                    state.soloist.logged_in,
                )
                .on_hover_text(
                    "Spotify Soloist built-in headless engine — works even after closing the official Spotify app (requires Premium + personal Soloist key)",
                )
                .clicked()
                {
                    if !is_soloist {
                        state.settings.audio_backend = AudioBackendKind::SpotifySoloist;
                        commands.push(AppCommand::SwitchAudioBackend {
                            backend: AudioBackendKind::SpotifySoloist,
                        });
                        commands.push(AppCommand::SaveSettings(state.settings.clone()));
                    }
                    if !state.soloist.api_key_configured || state.soloist.binary_path.is_none() {
                        state.navigate(Route::Settings);
                    }
                }

                if segment_pill(ui, theme, Icon::LocalDrive, "Local", is_local, false)
                    .on_hover_text("Local Music Player (plays local MP3/FLAC/OGG/WAV via rodio)")
                    .clicked()
                    && !is_local
                {
                    state.settings.audio_backend = AudioBackendKind::LocalRodio;
                    commands.push(AppCommand::SwitchAudioBackend {
                        backend: AudioBackendKind::LocalRodio,
                    });
                    commands.push(AppCommand::SaveSettings(state.settings.clone()));
                }
            });
        });
}

fn segment_pill(
    ui: &mut Ui,
    theme: &ThemePalette,
    icon: Icon,
    label: &str,
    selected: bool,
    show_live_dot: bool,
) -> egui::Response {
    let font_id = FontId::proportional(11.5);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, Color32::PLACEHOLDER);
    let icon_w = 13.0;
    let gap = 5.0;
    let pad_x = 10.0;
    let dot_extra = if show_live_dot { 10.0 } else { 0.0 };
    let w = pad_x * 2.0 + icon_w + gap + galley.size().x + dot_extra;
    let h = 26.0;

    let (rect, response) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let radius = CornerRadius::same(13);
    if selected {
        let thumb_bg = if theme.dark {
            Color32::from_white_alpha(28)
        } else {
            Color32::WHITE
        };
        ui.painter().add(theme.shadow_soft().as_shape(rect, radius));
        ui.painter().rect_filled(rect, radius, thumb_bg);
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, theme.glass_border()),
            StrokeKind::Inside,
        );
        ui.painter().line_segment(
            [
                Pos2::new(rect.left() + 12.0, rect.top() + 0.9),
                Pos2::new(rect.right() - 12.0, rect.top() + 0.9),
            ],
            Stroke::new(1.0, theme.glass_specular_rim()),
        );
    } else if hovered {
        ui.painter()
            .rect_filled(rect, radius, theme.hover_overlay());
    }

    let icon_col = if selected || show_live_dot {
        theme.accent
    } else if hovered {
        theme.text_primary
    } else {
        theme.text_muted
    };
    let text_col = if selected || hovered {
        theme.text_primary
    } else if show_live_dot {
        theme.accent
    } else {
        theme.text_secondary
    };

    let ic_rect = Rect::from_center_size(
        Pos2::new(rect.left() + pad_x + icon_w * 0.5, rect.center().y),
        Vec2::splat(icon_w),
    );
    paint_icon(ui.painter(), ic_rect, icon, icon_col);

    let text_x = rect.left() + pad_x + icon_w + gap;
    let text_w = galley.size().x;
    ui.painter().galley(
        Pos2::new(text_x, rect.center().y - galley.size().y * 0.5),
        galley,
        text_col,
    );

    if show_live_dot {
        ui.painter().circle_filled(
            Pos2::new(text_x + text_w + 6.0, rect.center().y),
            3.0,
            theme.accent,
        );
    }

    response
}

fn render_devices_menu(
    ui: &mut Ui,
    state: &AppState,
    theme: &ThemePalette,
    max_chars: usize,
    commands: &mut Vec<AppCommand>,
) {
    let has_active = state.playback.active_device_name.is_some();
    let label = match state.playback.active_device_name.as_deref() {
        Some(name) => {
            let short: String = name.chars().take(max_chars).collect();
            if name.chars().count() > max_chars {
                format!("{short}…")
            } else {
                short
            }
        }
        None => "Devices".to_string(),
    };

    let btn_resp = ghost_pill_button(ui, theme, Icon::Speaker, &label, has_active)
        .on_hover_text("Spotify Connect Devices");

    egui::Popup::menu(&btn_resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(250.0);
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("CONNECT DEVICES")
                        .size(10.5)
                        .strong()
                        .color(theme.text_muted),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(
                        ui,
                        theme,
                        Icon::Refresh,
                        13.0,
                        24.0,
                        false,
                        true,
                        "Sync active playback & devices",
                    )
                    .clicked()
                    {
                        commands.push(AppCommand::RefreshDevices);
                        commands.push(AppCommand::SyncConnectState);
                    }
                });
            });
            ui.separator();

            if state.devices.is_empty() {
                ui.label(
                    RichText::new(
                        "No active Spotify Connect devices found.\nOpen Spotify Desktop or Mobile, then click Sync.",
                    )
                    .size(12.0)
                    .color(theme.text_muted),
                );
            } else {
                for dev in &state.devices {
                    let vol_suffix = dev
                        .volume_percent
                        .map(|v| format!(" · {v}%"))
                        .unwrap_or_default();
                    let subtitle = format!("{}{vol_suffix}", dev.device_type);

                    if dev.is_restricted || dev.id.is_none() {
                        ui.add_enabled_ui(false, |ui| {
                            ui.label(
                                RichText::new(format!("{} ({}) — Restricted", dev.name, subtitle))
                                    .size(12.0)
                                    .color(theme.text_muted),
                            );
                        });
                    } else if let Some(id) = &dev.id {
                        let w = ui.available_width();
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(w, 36.0), Sense::click());
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            ui.painter().rect_filled(
                                rect,
                                CornerRadius::same(6),
                                theme.hover_overlay(),
                            );
                        }
                        let ic_rect = Rect::from_center_size(
                            Pos2::new(rect.left() + 14.0, rect.center().y),
                            Vec2::splat(15.0),
                        );
                        paint_icon(
                            ui.painter(),
                            ic_rect,
                            Icon::Speaker,
                            if dev.is_active {
                                theme.accent
                            } else {
                                theme.text_secondary
                            },
                        );
                        let text_rect = Rect::from_min_max(
                            Pos2::new(rect.left() + 30.0, rect.top() + 3.0),
                            Pos2::new(rect.right() - 8.0, rect.bottom() - 3.0),
                        );
                        let mut child = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(text_rect)
                                .layout(Layout::top_down(Align::LEFT)),
                        );
                        child.spacing_mut().item_spacing.y = 0.0;
                        child.label(
                            RichText::new(&dev.name)
                                .size(12.5)
                                .strong()
                                .color(if dev.is_active {
                                    theme.accent
                                } else {
                                    theme.text_primary
                                }),
                        );
                        child.label(
                            RichText::new(if dev.is_active {
                                format!("Active · {subtitle}")
                            } else {
                                subtitle
                            })
                            .size(10.5)
                            .color(theme.text_muted),
                        );

                        if resp.clicked() {
                            commands.push(AppCommand::TransferDevice {
                                device_id: id.clone(),
                            });
                            ui.close();
                        }
                    }
                }
            }
        });
}
