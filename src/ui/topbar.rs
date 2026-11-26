//! Top mini-navigation bar with Back/Forward history, quick search bar, audio backend badge,
//! Now Playing shortcut, and status notice banner.

use crate::core::events::AppCommand;
use crate::core::models::Route;
use crate::core::state::{AppState, AuthUiState};
use crate::ui::theme::ThemePalette;
use egui::{Color32, CornerRadius, Frame, Margin, RichText, Stroke, Ui, Vec2};
use std::time::Instant;

pub fn render_topbar(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    commands: &mut Vec<AppCommand>,
) {
    ui.horizontal(|ui| {
        // Back / Forward mini navigation
        let back_btn = egui::Button::new(
            RichText::new("◀").size(12.0).color(if state.can_go_back() {
                theme.text_primary
            } else {
                theme.text_muted
            }),
        )
        .corner_radius(CornerRadius::same(16))
        .min_size(Vec2::splat(30.0));
        if ui
            .add_enabled(state.can_go_back(), back_btn)
            .on_hover_text("Back")
            .clicked()
        {
            state.go_back();
        }

        let fwd_btn = egui::Button::new(
            RichText::new("▶").size(12.0).color(if state.can_go_forward() {
                theme.text_primary
            } else {
                theme.text_muted
            }),
        )
        .corner_radius(CornerRadius::same(16))
        .min_size(Vec2::splat(30.0));
        if ui
            .add_enabled(state.can_go_forward(), fwd_btn)
            .on_hover_text("Forward")
            .clicked()
        {
            state.go_forward();
        }

        ui.add_space(6.0);

        // Compact global search input
        let search_width = (ui.available_width() * 0.36).clamp(160.0, 360.0);
        let search_edit = egui::TextEdit::singleline(&mut state.search_input)
            .hint_text("Search songs, artists, albums, playlists…")
            .desired_width(search_width)
            .margin(Margin::symmetric(12, 6));
        let resp = ui.add(search_edit);
        if resp.gained_focus() && state.current_route != Route::Search {
            state.navigate(Route::Search);
        }
        if resp.changed() {
            state.search_dirty_since = Some(Instant::now());
            if state.current_route != Route::Search {
                state.navigate(Route::Search);
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Fullscreen Now Playing shortcut
            let in_now_playing = state.current_route == Route::NowPlaying;
            let np_btn = egui::Button::new(
                RichText::new(if in_now_playing {
                    "Exit Now Playing"
                } else {
                    "✦ Now Playing"
                })
                .size(12.0)
                .color(if in_now_playing {
                    Color32::BLACK
                } else {
                    theme.text_primary
                }),
            )
            .fill(if in_now_playing {
                theme.accent
            } else {
                theme.bg_elevated
            })
            .corner_radius(CornerRadius::same(16));
            if ui.add(np_btn).clicked() {
                if in_now_playing {
                    state.go_back();
                } else {
                    state.navigate(Route::NowPlaying);
                }
            }

            // Connect devices popup menu when authenticated
            if matches!(state.auth, AuthUiState::Authenticated(_)) {
                let device_label = match (
                    state.playback.active_device_name.as_deref(),
                    state.playback.active_device_type.as_deref(),
                ) {
                    (Some(name), Some(kind)) if !kind.is_empty() => format!("🔊 {name} ({kind})"),
                    (Some(name), _) => format!("🔊 {name}"),
                    (None, _) => "🔊 Connect Devices".to_string(),
                };
                ui.menu_button(RichText::new(device_label).size(12.0), |ui| {
                    if ui.button("↻ Sync Active Playback & Devices").clicked() {
                        commands.push(AppCommand::RefreshDevices);
                        commands.push(AppCommand::SyncConnectState);
                    }
                    ui.separator();
                    if state.devices.is_empty() {
                        ui.label(
                            RichText::new(
                                "No active Spotify Connect devices found.\nOpen Spotify Desktop/Mobile, a Connect speaker, or Spotify Soloist, then click Sync.",
                            )
                            .size(11.5)
                            .color(theme.text_muted),
                        );
                    } else {
                        let devices = state.devices.clone();
                        for dev in devices {
                            let vol_suffix = dev
                                .volume_percent
                                .map(|v| format!(" · {v}%"))
                                .unwrap_or_default();
                            if dev.is_restricted || dev.id.is_none() {
                                let label = format!(
                                    "⊘ {} ({}) — Restricted",
                                    dev.name, dev.device_type
                                );
                                ui.add_enabled(
                                    false,
                                    egui::Button::new(
                                        RichText::new(label).size(12.0).color(theme.text_muted),
                                    ),
                                )
                                .on_disabled_hover_text(
                                    "Spotify marks this device as restricted; remote Web API control is disabled.",
                                );
                            } else if let Some(id) = dev.id {
                                let (label, text_col) = if dev.is_active {
                                    (
                                        format!(
                                            "● {} ({}) — Active{vol_suffix}",
                                            dev.name, dev.device_type
                                        ),
                                        theme.accent,
                                    )
                                } else {
                                    (
                                        format!(
                                            "○ {} ({}) — Available{vol_suffix}",
                                            dev.name, dev.device_type
                                        ),
                                        theme.text_primary,
                                    )
                                };
                                if ui
                                    .button(RichText::new(label).size(12.0).color(text_col))
                                    .clicked()
                                {
                                    commands.push(AppCommand::TransferDevice { device_id: id });
                                    ui.close();
                                }
                            }
                        }
                    }
                });
            }

            // Explicit Mode Switcher: Local Music Player | Spotify Soloist | Spotify Connect Remote
            let active_mode = state.playback.backend;
            Frame::new()
                .fill(theme.bg_elevated)
                .corner_radius(CornerRadius::same(16))
                .stroke(Stroke::new(1.0, theme.border))
                .inner_margin(Margin::symmetric(4, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        let is_local = active_mode == crate::core::models::AudioBackendKind::LocalRodio;
                        let is_soloist =
                            active_mode == crate::core::models::AudioBackendKind::SpotifySoloist;
                        let is_connect =
                            active_mode == crate::core::models::AudioBackendKind::SpotifyConnect;

                        let local_btn = egui::Button::new(
                            RichText::new("🎵 Local")
                                .size(11.0)
                                .strong()
                                .color(if is_local {
                                    Color32::BLACK
                                } else {
                                    theme.text_secondary
                                }),
                        )
                        .fill(if is_local {
                            theme.accent
                        } else {
                            Color32::TRANSPARENT
                        })
                        .corner_radius(CornerRadius::same(12));
                        if ui
                            .add(local_btn)
                            .on_hover_text("Local Music Player (plays local MP3/FLAC/OGG/WAV via rodio)")
                            .clicked()
                            && !is_local
                        {
                            state.settings.audio_backend =
                                crate::core::models::AudioBackendKind::LocalRodio;
                            commands.push(AppCommand::SwitchAudioBackend {
                                backend: crate::core::models::AudioBackendKind::LocalRodio,
                            });
                            commands.push(AppCommand::SaveSettings(state.settings.clone()));
                        }

                        let soloist_label = if state.soloist.logged_in {
                            "⚡ Soloist ●"
                        } else {
                            "⚡ Soloist"
                        };
                        let soloist_btn = egui::Button::new(
                            RichText::new(soloist_label)
                                .size(11.0)
                                .strong()
                                .color(if is_soloist {
                                    Color32::BLACK
                                } else if state.soloist.logged_in {
                                    theme.accent
                                } else {
                                    theme.text_secondary
                                }),
                        )
                        .fill(if is_soloist {
                            theme.accent
                        } else {
                            Color32::TRANSPARENT
                        })
                        .corner_radius(CornerRadius::same(12));
                        if ui
                            .add(soloist_btn)
                            .on_hover_text(
                                "Spotify Soloist built-in headless engine — works even after closing the official Spotify app (requires Premium + personal Soloist key)",
                            )
                            .clicked()
                        {
                            if !is_soloist {
                                state.settings.audio_backend =
                                    crate::core::models::AudioBackendKind::SpotifySoloist;
                                commands.push(AppCommand::SwitchAudioBackend {
                                    backend: crate::core::models::AudioBackendKind::SpotifySoloist,
                                });
                                commands.push(AppCommand::SaveSettings(state.settings.clone()));
                            }
                            if !state.soloist.api_key_configured
                                || state.soloist.binary_path.is_none()
                            {
                                state.navigate(Route::Settings);
                            }
                        }

                        let connect_btn = egui::Button::new(
                            RichText::new("📡 Connect")
                                .size(11.0)
                                .strong()
                                .color(if is_connect {
                                    Color32::BLACK
                                } else {
                                    theme.text_secondary
                                }),
                        )
                        .fill(if is_connect {
                            theme.accent
                        } else {
                            Color32::TRANSPARENT
                        })
                        .corner_radius(CornerRadius::same(12));
                        if ui
                            .add(connect_btn)
                            .on_hover_text(
                                "Spotify Connect Remote Controller (controls external Spotify device; requires Premium)",
                            )
                            .clicked()
                            && !is_connect
                        {
                            state.settings.audio_backend =
                                crate::core::models::AudioBackendKind::SpotifyConnect;
                            commands.push(AppCommand::SwitchAudioBackend {
                                backend: crate::core::models::AudioBackendKind::SpotifyConnect,
                            });
                            commands.push(AppCommand::SaveSettings(state.settings.clone()));
                        }
                    });
                });
        });
    });

    // Optional dismissible toast/status banner
    if let Some(notice) = state.notice.clone() {
        ui.add_space(6.0);
        let fill = if notice.is_error {
            theme.danger.linear_multiply(0.18)
        } else {
            theme.accent.linear_multiply(0.16)
        };
        let stroke_col = if notice.is_error {
            theme.danger
        } else {
            theme.accent
        };
        Frame::new()
            .fill(fill)
            .corner_radius(CornerRadius::same(8))
            .stroke(Stroke::new(1.0, stroke_col))
            .inner_margin(Margin::symmetric(12, 7))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&notice.message)
                            .size(12.5)
                            .color(theme.text_primary),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").clicked() {
                            state.notice = None;
                        }
                    });
                });
            });
    }
}
