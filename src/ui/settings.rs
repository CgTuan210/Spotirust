//! Settings, Theme Customization, Audio Backend configuration, Lyrics Directories,
//! Cache/Session Management, and Plugin Diagnostics.

use crate::core::events::AppCommand;
use crate::core::models::AudioBackendKind;
use crate::core::plugins::PluginManifest;
use crate::core::state::AppState;
use crate::storage::database::CacheStats;
use crate::ui::theme::{ThemeManager, ThemePalette};
use egui::{Color32, CornerRadius, Frame, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};
use std::path::PathBuf;

#[allow(clippy::too_many_arguments)]
pub fn render_settings(
    ui: &mut Ui,
    state: &mut AppState,
    theme_mgr: &mut ThemeManager,
    credential_backend: &str,
    cache_stats: &CacheStats,
    plugins: &[PluginManifest],
    commands: &mut Vec<AppCommand>,
) {
    let theme = theme_mgr.active().clone();
    let mut settings_changed = false;

    ScrollArea::vertical()
        .id_salt("settings_scroll")
        .show(ui, |ui| {
            ui.label(
                RichText::new("Settings & Diagnostics")
                    .size(26.0)
                    .strong()
                    .color(theme.text_primary),
            );
            ui.add_space(12.0);

            // 1. Themes
            settings_card(ui, &theme, "Appearance & Themes", |ui| {
                ui.label(
                    RichText::new(
                        "Switch themes live or place custom *.json manifests in your Spotirust themes folder.",
                    )
                    .size(12.0)
                    .color(theme.text_secondary),
                );
                ui.add_space(8.0);
                let themes: Vec<ThemePalette> = theme_mgr.list().to_vec();
                let mut picked_id: Option<String> = None;

                ui.horizontal_wrapped(|ui| {
                    for preset in &themes {
                        let is_active = preset.id == state.settings.theme_id;
                        Frame::new()
                            .fill(preset.bg_surface)
                            .corner_radius(CornerRadius::same(10))
                            .stroke(Stroke::new(
                                if is_active { 2.0 } else { 1.0 },
                                if is_active {
                                    preset.accent
                                } else {
                                    preset.border
                                },
                            ))
                            .inner_margin(Margin::same(10))
                            .show(ui, |ui| {
                                ui.set_width(146.0);
                                ui.horizontal(|ui| {
                                    let (r, _) = ui
                                        .allocate_exact_size(Vec2::splat(16.0), egui::Sense::hover());
                                    ui.painter().circle_filled(r.center(), 7.0, preset.accent);
                                    ui.label(
                                        RichText::new(&preset.name)
                                            .strong()
                                            .size(12.5)
                                            .color(preset.text_primary),
                                    );
                                });
                                ui.add_space(4.0);
                                if ui
                                    .small_button(if is_active { "Active" } else { "Apply" })
                                    .clicked()
                                {
                                    picked_id = Some(preset.id.clone());
                                }
                            });
                    }
                });

                if let Some(id) = picked_id
                    && theme_mgr.set_active(&id)
                {
                    state.settings.theme_id = id;
                    theme_mgr.active().apply_to_egui(ui.ctx());
                    settings_changed = true;
                }

                ui.add_space(8.0);
                if ui
                    .checkbox(&mut state.settings.reduce_motion, "Reduce UI & ambient animations")
                    .changed()
                {
                    settings_changed = true;
                }
            });

            ui.add_space(12.0);

            // 2. Audio Playback Mode
            settings_card(ui, &theme, "Audio Playback Operating Mode", |ui| {
                ui.label(
                    RichText::new(
                        "Spotirust strictly separates honest operating modes and never simulates fake playback:\n\
                         • Spotify Soloist (Recommended for Spotify): manages Spotify's official headless playback engine locally via loopback WebSocket — works even after closing the official Spotify app (requires Spotify Premium & personal Soloist key).\n\
                         • Spotify Connect (Remote): controls an active external Spotify app or speaker via Web API (requires Spotify Premium).\n\
                         • Local Player (Rodio): plays real local MP3/FLAC/OGG/WAV files natively on your sound card.",
                    )
                    .size(12.0)
                    .color(theme.text_secondary),
                );
                ui.add_space(8.0);

                for kind in [
                    AudioBackendKind::SpotifySoloist,
                    AudioBackendKind::LocalRodio,
                    AudioBackendKind::SpotifyConnect,
                    AudioBackendKind::Librespot,
                ] {
                    if ui
                        .radio_value(&mut state.settings.audio_backend, kind, kind.label())
                        .changed()
                    {
                        commands.push(AppCommand::SwitchAudioBackend { backend: kind });
                        settings_changed = true;
                    }
                }

                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Platform note: Direct third-party OAuth streaming via librespot 0.8 is blocked since Aug 10, 2026 \
                         (librespot#1737). Use Spotify Soloist to play Spotify tracks without keeping the official Spotify app open.",
                    )
                    .size(11.0)
                    .color(theme.text_muted),
                );
            });

            ui.add_space(12.0);

            // 2B. Spotify Soloist Setup & Daemon Management
            settings_card(ui, &theme, "Spotify Soloist — Built-in Headless Engine", |ui| {
                use crate::audio::soloist::{
                    SOLOIST_DASHBOARD_URL, SOLOIST_DOWNLOADS_PAGE_URL, SoloistStatus,
                };

                let sol = &state.soloist;

                // Live status pill banner
                let (status_text, status_color) = match &sol.status {
                    SoloistStatus::Connected {
                        device_name,
                        is_active,
                        ws_port,
                    } => (
                        format!(
                            "● Connected & Paired ('{device_name}' · {} · ws://127.0.0.1:{ws_port}) — You can close the official Spotify app!",
                            if *is_active { "Active device" } else { "Standby / Ready" }
                        ),
                        theme.accent,
                    ),
                    SoloistStatus::WaitingForPairing {
                        device_name,
                        ws_port,
                    } => (
                        format!(
                            "⏳ Waiting for one-time pairing (ws://127.0.0.1:{ws_port}): Open the Spotify app on your phone or PC on the same network → Connect to a device → select '{device_name}' ONCE. After pairing completes, you can close the official Spotify app."
                        ),
                        theme.warning,
                    ),
                    SoloistStatus::Downloading => (
                        "⬇ Downloading official Spotify Soloist binary from Spotify CDN…".to_string(),
                        theme.accent,
                    ),
                    SoloistStatus::Starting => (
                        "Starting managed Spotify Soloist daemon…".to_string(),
                        theme.warning,
                    ),
                    SoloistStatus::Expired { message } => (format!("⚠ {message}"), theme.danger),
                    SoloistStatus::Error { message } => (format!("⚠ {message}"), theme.danger),
                    SoloistStatus::Stopped => (
                        "Stopped — Complete Steps 1 & 2 below, then click Start Soloist.".to_string(),
                        theme.text_muted,
                    ),
                };

                Frame::new()
                    .fill(status_color.linear_multiply(0.14))
                    .corner_radius(CornerRadius::same(8))
                    .stroke(Stroke::new(1.0, status_color))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            if matches!(
                                sol.status,
                                SoloistStatus::Downloading
                                    | SoloistStatus::Starting
                                    | SoloistStatus::WaitingForPairing { .. }
                            ) {
                                ui.spinner();
                            }
                            ui.label(
                                RichText::new(status_text)
                                    .size(12.0)
                                    .color(theme.text_primary),
                            );
                        });
                    });

                ui.add_space(10.0);

                // Step 1: Personal Soloist API Key (OS Keyring only)
                ui.label(
                    RichText::new("Step 1 · Personal Spotify Soloist API Key (OS Keyring only, never SQLite)")
                        .size(12.5)
                        .strong()
                        .color(theme.text_primary),
                );
                ui.label(
                    RichText::new(
                        "Requires Spotify Premium. Each user must generate their own personal key on Spotify for Developers.",
                    )
                    .size(11.0)
                    .color(theme.text_muted),
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.soloist_key_input)
                            .password(true)
                            .hint_text(if sol.api_key_configured {
                                "Soloist API key stored in OS vault (paste new key to replace)"
                            } else {
                                "Paste your personal Spotify Soloist API key…"
                            })
                            .desired_width(300.0),
                    );
                    if ui.button("Save Key").clicked() {
                        let key = state.soloist_key_input.trim().to_string();
                        if !key.is_empty() {
                            commands.push(AppCommand::SaveSoloistApiKey { api_key: Some(key) });
                            state.soloist_key_input.clear();
                        }
                    }
                    if sol.api_key_configured && ui.button("Clear Key").clicked() {
                        commands.push(AppCommand::SaveSoloistApiKey { api_key: None });
                    }
                    if ui.button("Get Key on Spotify Dashboard ↗").clicked() {
                        commands.push(AppCommand::OpenExternalUrl {
                            url: SOLOIST_DASHBOARD_URL.to_string(),
                        });
                    }
                });

                ui.add_space(10.0);

                // Step 2: Official Spotify Soloist Executable
                ui.label(
                    RichText::new("Step 2 · Official Spotify Soloist Executable (90-day build lifetime)")
                        .size(12.5)
                        .strong()
                        .color(theme.text_primary),
                );
                let bin_line = match (&sol.binary_path, &sol.binary_version) {
                    (Some(path), Some(ver)) => format!("Installed: {ver} ({})", path.display()),
                    (Some(path), None) => format!("Installed: {}", path.display()),
                    (None, _) => {
                        if let Some(note) = &sol.platform_note {
                            if sol.platform_supported {
                                format!("Not installed yet · Host platform: {note}")
                            } else {
                                note.clone()
                            }
                        } else {
                            "Not installed yet.".to_string()
                        }
                    }
                };
                ui.label(
                    RichText::new(bin_line)
                        .size(11.5)
                        .color(if sol.binary_path.is_some() {
                            theme.accent
                        } else {
                            theme.text_secondary
                        }),
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    let is_downloading = matches!(sol.status, SoloistStatus::Downloading);
                    let dl_label = if sol.binary_path.is_some() {
                        "Update Soloist from Spotify CDN"
                    } else {
                        "Download & Install Soloist"
                    };
                    if ui
                        .add_enabled(
                            sol.platform_supported && !is_downloading,
                            egui::Button::new(dl_label),
                        )
                        .clicked()
                    {
                        commands.push(AppCommand::DownloadSoloist);
                    }
                    if ui.button("Browse Existing Binary…").clicked() {
                        commands.push(AppCommand::PickSoloistBinary);
                    }
                    if ui.button("Official Downloads Page ↗").clicked() {
                        commands.push(AppCommand::OpenExternalUrl {
                            url: SOLOIST_DOWNLOADS_PAGE_URL.to_string(),
                        });
                    }
                });

                ui.add_space(10.0);

                // Step 3: Device Identity & Daemon Controls
                ui.label(
                    RichText::new("Step 3 · Device Identity & Background Engine Controls")
                        .size(12.5)
                        .strong()
                        .color(theme.text_primary),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Device name:")
                            .size(12.0)
                            .color(theme.text_secondary),
                    );
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut state.settings.soloist_device_name)
                                .hint_text("Spotirust")
                                .desired_width(180.0),
                        )
                        .changed()
                    {
                        settings_changed = true;
                    }
                    if ui
                        .checkbox(
                            &mut state.settings.soloist_auto_start,
                            "Auto-start Soloist when Spotirust opens",
                        )
                        .changed()
                    {
                        settings_changed = true;
                    }
                });

                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    let can_start =
                        sol.api_key_configured && sol.binary_path.is_some() && !sol.status.is_running();
                    let start_btn = egui::Button::new(
                        RichText::new("Start Soloist").strong().color(if can_start {
                            Color32::BLACK
                        } else {
                            theme.text_muted
                        }),
                    )
                    .fill(if can_start {
                        theme.accent
                    } else {
                        theme.bg_surface
                    });
                    if ui.add_enabled(can_start, start_btn).clicked() {
                        state.settings.audio_backend = AudioBackendKind::SpotifySoloist;
                        commands.push(AppCommand::SaveSettings(state.settings.clone()));
                        commands.push(AppCommand::StartSoloist);
                    }

                    if sol.status.is_running() {
                        if ui.button("Restart Soloist").clicked() {
                            commands.push(AppCommand::SaveSettings(state.settings.clone()));
                            commands.push(AppCommand::RestartSoloist);
                        }
                        if ui.button("Stop Soloist").clicked() {
                            commands.push(AppCommand::StopSoloist);
                        }
                    }

                    if (sol.logged_in || sol.status.is_running())
                        && ui
                            .button("Re-pair Account")
                            .on_hover_text(
                                "Clears Soloist's stored Spotify Connect session so you can pair another account",
                            )
                            .clicked()
                    {
                        commands.push(AppCommand::ResetSoloistPairing);
                    }
                });
            });

            ui.add_space(12.0);

            // 3. Lyrics Engine Preferences
            settings_card(ui, &theme, "Lyrics & Karaoke Engine", |ui| {
                use crate::core::config::{LyricsSourcePreference, OnlineLyricsProviderId};
                use crate::lyrics::apple_music::AppleMusicProviderStatus;

                ui.label(
                    RichText::new("Lyrics Source Strategy:")
                        .size(12.5)
                        .strong()
                        .color(theme.text_primary),
                );
                ui.add_space(4.0);

                let prev_source = state.settings.lyrics_source;
                for pref in [
                    LyricsSourcePreference::Automatic,
                    LyricsSourcePreference::Unison,
                    LyricsSourcePreference::AmllTtmlDb,
                    LyricsSourcePreference::Musixmatch,
                    LyricsSourcePreference::Lrclib,
                    LyricsSourcePreference::LocalOnly,
                    LyricsSourcePreference::AppleMusic,
                ] {
                    if ui
                        .radio_value(&mut state.settings.lyrics_source, pref, pref.label())
                        .changed()
                    {
                        settings_changed = true;
                    }
                }
                if state.settings.lyrics_source != prev_source
                    && let Some(track) = state.playback.current_track.clone()
                {
                    commands.push(AppCommand::FetchLyrics {
                        track,
                        force_refresh: true,
                    });
                }

                ui.add_space(8.0);
                ui.label(
                    RichText::new("Automatic Mode Provider Priority (Word-level -> Line-level -> Plain):")
                        .size(12.5)
                        .strong()
                        .color(theme.text_primary),
                );
                ui.label(
                    RichText::new(
                        "Local .ttml/.lyricsfile/.lrc files are checked first. Online providers run in order below and stop immediately once Word-level synced lyrics are found.",
                    )
                    .size(11.0)
                    .color(theme.text_muted),
                );
                ui.add_space(4.0);

                // Ensure order list always contains all 4 compliant online providers
                let normalized = state.settings.lyrics_chain_config().normalized_order();
                if state.settings.lyrics_provider_order != normalized {
                    state.settings.lyrics_provider_order = normalized;
                }

                let mut swap_action: Option<(usize, usize)> = None;
                let count = state.settings.lyrics_provider_order.len();
                for idx in 0..count {
                    let pid = state.settings.lyrics_provider_order[idx];
                    ui.horizontal(|ui| {
                        let enabled_flag = match pid {
                            OnlineLyricsProviderId::Unison => {
                                &mut state.settings.enable_unison_online
                            }
                            OnlineLyricsProviderId::AmllTtmlDb => {
                                &mut state.settings.enable_amll_db_online
                            }
                            OnlineLyricsProviderId::Musixmatch => {
                                &mut state.settings.enable_musixmatch_online
                            }
                            OnlineLyricsProviderId::Lrclib => {
                                &mut state.settings.enable_lrclib_online
                            }
                        };
                        if ui
                            .checkbox(enabled_flag, format!("{}. {}", idx + 1, pid.label()))
                            .changed()
                        {
                            settings_changed = true;
                        }
                        if idx > 0 && ui.small_button("↑").on_hover_text("Move up").clicked() {
                            swap_action = Some((idx, idx - 1));
                        }
                        if idx + 1 < count
                            && ui.small_button("↓").on_hover_text("Move down").clicked()
                        {
                            swap_action = Some((idx, idx + 1));
                        }
                        if pid == OnlineLyricsProviderId::Musixmatch
                            && !state.musixmatch_key_configured
                        {
                            ui.label(
                                RichText::new("(No API key — skipped)")
                                    .size(10.5)
                                    .color(theme.text_muted),
                            );
                        }
                    });
                }
                if let Some((a, b)) = swap_action {
                    state.settings.lyrics_provider_order.swap(a, b);
                    settings_changed = true;
                }

                ui.add_space(8.0);
                ui.label(
                    RichText::new("Optional Musixmatch Official Developer API Key (OS Keyring only, never SQLite):")
                        .size(12.0)
                        .color(theme.text_secondary),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.musixmatch_key_input)
                            .password(true)
                            .hint_text(if state.musixmatch_key_configured {
                                "API key configured in OS vault (enter new key to replace)"
                            } else {
                                "Optional developer.musixmatch.com API key"
                            })
                            .desired_width(280.0),
                    );
                    if ui.button("Save Key").clicked() {
                        let key = state.musixmatch_key_input.trim().to_string();
                        if !key.is_empty() {
                            commands.push(AppCommand::SaveMusixmatchApiKey { api_key: Some(key) });
                            state.musixmatch_key_input.clear();
                        }
                    }
                    if state.musixmatch_key_configured && ui.button("Clear Key").clicked() {
                        commands.push(AppCommand::SaveMusixmatchApiKey { api_key: None });
                    }
                });

                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "Blocked / excluded sources (not faked):\n\
                         • Apple Music: {}\n\
                         • BiniLyrics: Excluded (BLOCKED_BY_ACCESS — unofficial proxy redistributing scraped Apple Music TTML without publisher licensing).",
                        AppleMusicProviderStatus::BlockedNoPublicLyricsApi.summary()
                    ))
                    .size(11.0)
                    .color(theme.text_muted),
                );

                ui.add_space(8.0);
                if ui
                    .checkbox(
                        &mut state.settings.show_lyric_translations,
                        "Show TTML translations & romanization when available",
                    )
                    .changed()
                {
                    settings_changed = true;
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Clear Lyrics Cache").clicked() {
                        commands.push(AppCommand::ClearLyricsCache);
                    }
                    ui.label(
                        RichText::new(format!(
                            "({} cached lyrics entries — clears lyrics only, keeps catalog & artwork)",
                            cache_stats.cached_lyrics
                        ))
                        .size(11.5)
                        .color(theme.text_muted),
                    );
                });

                ui.add_space(8.0);
                ui.label(
                    RichText::new("Additional Local Lyrics Folders (.ttml / .lyricsfile / .lrc):")
                        .size(12.0)
                        .color(theme.text_secondary),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.new_local_lyrics_dir)
                            .hint_text("/path/to/lyrics")
                            .desired_width(280.0),
                    );
                    if ui.button("Add Folder").clicked() {
                        let p = PathBuf::from(state.new_local_lyrics_dir.trim());
                        if !p.as_os_str().is_empty() && !state.settings.local_lyrics_dirs.contains(&p)
                        {
                            state.settings.local_lyrics_dirs.push(p);
                            state.new_local_lyrics_dir.clear();
                            settings_changed = true;
                        }
                    }
                });
                let mut remove_idx = None;
                for (i, dir) in state.settings.local_lyrics_dirs.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(dir.to_string_lossy().as_ref())
                                .size(11.5)
                                .color(theme.text_muted),
                        );
                        if ui.small_button("Remove").clicked() {
                            remove_idx = Some(i);
                        }
                    });
                }
                if let Some(idx) = remove_idx {
                    state.settings.local_lyrics_dirs.remove(idx);
                    settings_changed = true;
                }
            });

            ui.add_space(12.0);

            // 4. Security, Storage & Session
            settings_card(ui, &theme, "Security, Storage & Session", |ui| {
                ui.label(
                    RichText::new(format!("Credential Vault: {credential_backend}"))
                        .size(12.5)
                        .color(theme.text_primary),
                );
                ui.label(
                    RichText::new(format!("OS Media Controls: {}", state.system_media_status))
                        .size(12.0)
                        .color(theme.text_secondary),
                );
                ui.label(
                    RichText::new(format!(
                        "SQLite Schema v{} · Local Tracks: {} · Cached Tracks: {} · Albums: {} · Playlists: {} · Lyrics: {} · Recent: {}",
                        cache_stats.schema_version,
                        cache_stats.local_tracks,
                        cache_stats.cached_tracks,
                        cache_stats.cached_albums,
                        cache_stats.cached_playlists,
                        cache_stats.cached_lyrics,
                        cache_stats.recently_played,
                    ))
                    .size(12.0)
                    .color(theme.text_secondary),
                );
                ui.label(
                    RichText::new(format!(
                        "Spotify OAuth Client ID: {}",
                        if state.client_id_configured {
                            "Configured at build/runtime (PKCE S256)"
                        } else {
                            "Not configured in this build (set SPOTIFY_CLIENT_ID or SPOTIRUST_CLIENT_ID when building)"
                        }
                    ))
                    .size(12.0)
                    .color(if state.client_id_configured {
                        theme.accent
                    } else {
                        theme.warning
                    }),
                );

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Clear Cache").clicked() {
                        commands.push(AppCommand::ClearCache);
                    }
                    if ui.button("Logout").clicked() {
                        commands.push(AppCommand::Logout);
                    }
                    let reset_btn = egui::Button::new(
                        RichText::new("Reset Application").color(Color32::WHITE),
                    )
                    .fill(theme.danger);
                    if ui.add(reset_btn).clicked() {
                        commands.push(AppCommand::ResetApplication);
                    }
                });
            });

            ui.add_space(12.0);

            // 5. Plugins
            settings_card(ui, &theme, "Loaded Plugins", |ui| {
                if plugins.is_empty() {
                    ui.label(
                        RichText::new("No plugins registered.")
                            .size(12.0)
                            .color(theme.text_muted),
                    );
                } else {
                    for p in plugins {
                        ui.label(
                            RichText::new(format!("{} v{} — {}", p.name, p.version, p.author))
                                .strong()
                                .size(12.5)
                                .color(theme.text_primary),
                        );
                        ui.label(
                            RichText::new(&p.description)
                                .size(11.5)
                                .color(theme.text_secondary),
                        );
                        ui.add_space(4.0);
                    }
                }
            });

            ui.add_space(24.0);
        });

    if settings_changed {
        commands.push(AppCommand::SaveSettings(state.settings.clone()));
    }
}

fn settings_card(
    ui: &mut Ui,
    theme: &ThemePalette,
    title: &str,
    add_contents: impl FnOnce(&mut Ui),
) {
    Frame::new()
        .fill(theme.bg_elevated)
        .corner_radius(CornerRadius::same(12))
        .stroke(Stroke::new(1.0, theme.border))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(title)
                    .size(16.0)
                    .strong()
                    .color(theme.text_primary),
            );
            ui.add_space(6.0);
            add_contents(ui);
        });
}
