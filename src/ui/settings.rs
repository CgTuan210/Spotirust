//! Settings, Theme Swatches, Audio Backend configuration, Lyrics Engine priority,
//! Security/Storage Diagnostics, and Plugin Registry.

use crate::core::events::AppCommand;
use crate::core::models::AudioBackendKind;
use crate::core::plugins::PluginManifest;
use crate::core::state::AppState;
use crate::storage::database::CacheStats;
use crate::ui::components::icons::{Icon, icon_button, paint_icon, pill_button};
use crate::ui::theme::{ThemeManager, ThemePalette};
use egui::{
    Align, Color32, CornerRadius, CursorIcon, Frame, Layout, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2,
};
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
            // Readable column width constraint (prevents stretched prose on ultrawide monitors)
            let max_col_w = ui.available_width().min(820.0);
            ui.set_max_width(max_col_w);

            ui.add_space(4.0);
            ui.label(
                RichText::new("Settings")
                    .size(26.0)
                    .strong()
                    .color(theme.text_primary),
            );
            ui.label(
                RichText::new("Appearance, audio output mode, karaoke lyrics providers, and diagnostics")
                    .size(13.0)
                    .color(theme.text_secondary),
            );
            ui.add_space(18.0);

            // 1. Appearance & Themes
            settings_card(ui, &theme, "APPEARANCE", "Themes & Motion", |ui| {
                ui.label(
                    RichText::new(
                        "Switch presets live or drop custom *.json theme manifests into your Spotirust themes folder.",
                    )
                    .size(12.5)
                    .color(theme.text_secondary),
                );
                ui.add_space(10.0);

                let themes: Vec<ThemePalette> = theme_mgr.list().to_vec();
                let mut picked_id: Option<String> = None;

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(10.0);
                    for preset in &themes {
                        let is_active = preset.id == state.settings.theme_id;
                        if theme_swatch_card(ui, preset, is_active) {
                            picked_id = Some(preset.id.clone());
                        }
                    }
                });

                if let Some(id) = picked_id
                    && theme_mgr.set_active(&id)
                {
                    state.settings.theme_id = id;
                    theme_mgr.active().apply_to_egui(ui.ctx());
                    settings_changed = true;
                }

                ui.add_space(12.0);
                if ui
                    .checkbox(
                        &mut state.settings.reduce_motion,
                        "Reduce UI & ambient animations",
                    )
                    .changed()
                {
                    settings_changed = true;
                }
            });

            ui.add_space(14.0);

            // 2. Audio Playback Mode
            settings_card(ui, &theme, "AUDIO", "Playback Operating Mode", |ui| {
                ui.label(
                    RichText::new(
                        "Spotirust strictly separates honest operating modes and never simulates fake playback:\n\
                         • Spotify Soloist (Recommended for Spotify): manages Spotify's official headless playback engine locally via loopback WebSocket — works even after closing the official Spotify app (requires Spotify Premium & personal Soloist key).\n\
                         • Spotify Connect (Remote): controls an active external Spotify app or speaker via Web API (requires Spotify Premium).\n\
                         • Local Player (Rodio): plays real local MP3/FLAC/OGG/WAV files natively on your sound card.",
                    )
                    .size(12.5)
                    .color(theme.text_secondary),
                );
                ui.add_space(10.0);

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

                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "Platform note: Direct third-party OAuth streaming via librespot 0.8 is blocked since Aug 10, 2026 \
                         (librespot#1737). Use Spotify Soloist to play Spotify tracks without keeping the official Spotify app open.",
                    )
                    .size(11.5)
                    .color(theme.text_muted),
                );
            });

            ui.add_space(14.0);

            // 2B. Spotify Soloist Setup & Daemon Management
            settings_card(
                ui,
                &theme,
                "BUILT-IN ENGINE",
                "Spotify Soloist",
                |ui| {
                    use crate::audio::soloist::{
                        SOLOIST_DASHBOARD_URL, SOLOIST_DOWNLOADS_PAGE_URL, SoloistStatus,
                    };

                    let sol = &state.soloist;

                    let (status_text, status_color) = match &sol.status {
                        SoloistStatus::Connected {
                            device_name,
                            is_active,
                            ws_port,
                        } => (
                            format!(
                                "Connected & Paired ('{device_name}' · {} · ws://127.0.0.1:{ws_port}) — You can close the official Spotify app.",
                                if *is_active { "Active device" } else { "Standby / Ready" }
                            ),
                            theme.accent,
                        ),
                        SoloistStatus::WaitingForPairing {
                            device_name,
                            ws_port,
                        } => (
                            format!(
                                "Waiting for one-time pairing (ws://127.0.0.1:{ws_port}): Open the Spotify app on your phone or PC on the same network → Connect to a device → select '{device_name}' ONCE. After pairing completes, you can close the official Spotify app."
                            ),
                            theme.warning,
                        ),
                        SoloistStatus::Downloading => (
                            "Downloading official Spotify Soloist binary from Spotify CDN…".to_string(),
                            theme.accent,
                        ),
                        SoloistStatus::Starting => (
                            "Starting managed Spotify Soloist daemon…".to_string(),
                            theme.warning,
                        ),
                        SoloistStatus::Expired { message } => (message.clone(), theme.danger),
                        SoloistStatus::Error { message } => (message.clone(), theme.danger),
                        SoloistStatus::Stopped => (
                            "Stopped — Complete Steps 1 & 2 below, then click Start Soloist.".to_string(),
                            theme.text_muted,
                        ),
                    };

                    Frame::new()
                        .fill(Color32::from_rgba_unmultiplied(
                            status_color.r(),
                            status_color.g(),
                            status_color.b(),
                            if theme.dark { 28 } else { 22 },
                        ))
                        .corner_radius(CornerRadius::same(12))
                        .stroke(Stroke::new(1.0, status_color.linear_multiply(0.55)))
                        .inner_margin(Margin::symmetric(12, 9))
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
                                } else {
                                    let (dot_r, _) =
                                        ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                                    ui.painter().circle_filled(dot_r.center(), 3.5, status_color);
                                }
                                ui.label(
                                    RichText::new(status_text)
                                        .size(12.0)
                                        .color(theme.text_primary),
                                );
                            });
                        });

                    ui.add_space(12.0);

                    // Step 1: Personal Soloist API Key (OS Keyring only)
                    ui.label(
                        RichText::new("Step 1 · Personal Spotify Soloist API Key (OS Keyring only)")
                            .size(12.5)
                            .strong()
                            .color(theme.text_primary),
                    );
                    ui.label(
                        RichText::new(
                            "Requires Spotify Premium. Each user must generate their own personal key on Spotify for Developers.",
                        )
                        .size(11.5)
                        .color(theme.text_muted),
                    );
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut state.soloist_key_input)
                                .password(true)
                                .hint_text(if sol.api_key_configured {
                                    "Soloist API key stored in OS vault (paste new key to replace)"
                                } else {
                                    "Paste your personal Spotify Soloist API key…"
                                })
                                .desired_width(300.0)
                                .margin(Margin::symmetric(10, 6)),
                        );
                        if pill_button(ui, &theme, None, "Save Key", false, true).clicked() {
                            let key = state.soloist_key_input.trim().to_string();
                            if !key.is_empty() {
                                commands.push(AppCommand::SaveSoloistApiKey { api_key: Some(key) });
                                state.soloist_key_input.clear();
                            }
                        }
                        if sol.api_key_configured
                            && pill_button(ui, &theme, None, "Clear Key", false, true).clicked()
                        {
                            commands.push(AppCommand::SaveSoloistApiKey { api_key: None });
                        }
                        if pill_button(
                            ui,
                            &theme,
                            Some(Icon::Expand),
                            "Spotify Dashboard",
                            false,
                            true,
                        )
                        .clicked()
                        {
                            commands.push(AppCommand::OpenExternalUrl {
                                url: SOLOIST_DASHBOARD_URL.to_string(),
                            });
                        }
                    });

                    ui.add_space(12.0);

                    // Step 2: Official Spotify Soloist Executable
                    ui.label(
                        RichText::new(
                            "Step 2 · Official Spotify Soloist Executable (90-day build lifetime)",
                        )
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
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        let is_downloading = matches!(sol.status, SoloistStatus::Downloading);
                        let dl_label = if sol.binary_path.is_some() {
                            "Update Soloist from Spotify CDN"
                        } else {
                            "Download & Install Soloist"
                        };
                        if pill_button(
                            ui,
                            &theme,
                            Some(Icon::ArrowDown),
                            dl_label,
                            false,
                            sol.platform_supported && !is_downloading,
                        )
                        .clicked()
                        {
                            commands.push(AppCommand::DownloadSoloist);
                        }
                        if pill_button(
                            ui,
                            &theme,
                            Some(Icon::Folder),
                            "Browse Existing Binary…",
                            false,
                            true,
                        )
                        .clicked()
                        {
                            commands.push(AppCommand::PickSoloistBinary);
                        }
                        if pill_button(
                            ui,
                            &theme,
                            Some(Icon::Expand),
                            "Official Downloads Page",
                            false,
                            true,
                        )
                        .clicked()
                        {
                            commands.push(AppCommand::OpenExternalUrl {
                                url: SOLOIST_DOWNLOADS_PAGE_URL.to_string(),
                            });
                        }
                    });

                    ui.add_space(12.0);

                    // Step 3: Device Identity & Daemon Controls
                    ui.label(
                        RichText::new("Step 3 · Device Identity & Background Engine Controls")
                            .size(12.5)
                            .strong()
                            .color(theme.text_primary),
                    );
                    ui.add_space(6.0);
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
                                    .desired_width(180.0)
                                    .margin(Margin::symmetric(10, 6)),
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

                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        let can_start = sol.api_key_configured
                            && sol.binary_path.is_some()
                            && !sol.status.is_running();
                        if pill_button(ui, &theme, Some(Icon::Play), "Start Soloist", true, can_start)
                            .clicked()
                        {
                            state.settings.audio_backend = AudioBackendKind::SpotifySoloist;
                            commands.push(AppCommand::SaveSettings(state.settings.clone()));
                            commands.push(AppCommand::StartSoloist);
                        }

                        if sol.status.is_running() {
                            if pill_button(
                                ui,
                                &theme,
                                Some(Icon::Refresh),
                                "Restart Soloist",
                                false,
                                true,
                            )
                            .clicked()
                            {
                                commands.push(AppCommand::SaveSettings(state.settings.clone()));
                                commands.push(AppCommand::RestartSoloist);
                            }
                            if pill_button(ui, &theme, Some(Icon::Close), "Stop Soloist", false, true)
                                .clicked()
                            {
                                commands.push(AppCommand::StopSoloist);
                            }
                        }

                        if (sol.logged_in || sol.status.is_running())
                            && pill_button(
                                ui,
                                &theme,
                                Some(Icon::Refresh),
                                "Re-pair Account",
                                false,
                                true,
                            )
                            .on_hover_text(
                                "Clears Soloist's stored Spotify Connect session so you can pair another account",
                            )
                            .clicked()
                        {
                            commands.push(AppCommand::ResetSoloistPairing);
                        }
                    });
                },
            );

            ui.add_space(14.0);

            // 3. Lyrics Engine Preferences
            settings_card(ui, &theme, "KARAOKE", "Lyrics Engine & Providers", |ui| {
                use crate::core::config::{LyricsSourcePreference, OnlineLyricsProviderId};
                use crate::lyrics::apple_music::AppleMusicProviderStatus;

                ui.label(
                    RichText::new("Lyrics Source Strategy")
                        .size(13.0)
                        .strong()
                        .color(theme.text_primary),
                );
                ui.add_space(4.0);

                let prev_source = state.settings.lyrics_source;
                for pref in [
                    LyricsSourcePreference::Automatic,
                    LyricsSourcePreference::BiniLyrics,
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

                ui.add_space(12.0);
                ui.label(
                    RichText::new("Automatic Provider Priority")
                        .size(13.0)
                        .strong()
                        .color(theme.text_primary),
                );
                ui.label(
                    RichText::new(
                        "Local .ttml/.lyricsfile/.lrc files are checked first. Online providers run in order below and stop immediately once Word-level synced lyrics are found.",
                    )
                    .size(11.5)
                    .color(theme.text_muted),
                );
                ui.add_space(6.0);

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
                            OnlineLyricsProviderId::BiniLyrics => {
                                &mut state.settings.enable_binilyrics_online
                            }
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
                        if idx > 0
                            && icon_button(
                                ui,
                                &theme,
                                Icon::ArrowUp,
                                13.0,
                                24.0,
                                false,
                                true,
                                "Move up",
                            )
                            .clicked()
                        {
                            swap_action = Some((idx, idx - 1));
                        }
                        if idx + 1 < count
                            && icon_button(
                                ui,
                                &theme,
                                Icon::ArrowDown,
                                13.0,
                                24.0,
                                false,
                                true,
                                "Move down",
                            )
                            .clicked()
                        {
                            swap_action = Some((idx, idx + 1));
                        }
                        if pid == OnlineLyricsProviderId::Musixmatch
                            && !state.musixmatch_key_configured
                        {
                            ui.label(
                                RichText::new("(No API key — skipped)")
                                    .size(11.0)
                                    .color(theme.text_muted),
                            );
                        }
                    });
                }
                if let Some((a, b)) = swap_action {
                    state.settings.lyrics_provider_order.swap(a, b);
                    settings_changed = true;
                }

                ui.add_space(12.0);
                ui.label(
                    RichText::new("Optional Musixmatch Developer API Key (stored in OS Keyring only)")
                        .size(12.5)
                        .color(theme.text_secondary),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.musixmatch_key_input)
                            .password(true)
                            .hint_text(if state.musixmatch_key_configured {
                                "API key configured in OS vault (enter new key to replace)"
                            } else {
                                "Optional developer.musixmatch.com API key"
                            })
                            .desired_width(300.0)
                            .margin(Margin::symmetric(10, 6)),
                    );
                    if pill_button(ui, &theme, None, "Save Key", false, true).clicked() {
                        let key = state.musixmatch_key_input.trim().to_string();
                        if !key.is_empty() {
                            commands.push(AppCommand::SaveMusixmatchApiKey { api_key: Some(key) });
                            state.musixmatch_key_input.clear();
                        }
                    }
                    if state.musixmatch_key_configured
                        && pill_button(ui, &theme, None, "Clear Key", false, true).clicked()
                    {
                        commands.push(AppCommand::SaveMusixmatchApiKey { api_key: None });
                    }
                });

                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "Unavailable upstream source (not faked):\n• Apple Music Official API: {}",
                        AppleMusicProviderStatus::BlockedNoPublicLyricsApi.summary()
                    ))
                    .size(11.5)
                    .color(theme.text_muted),
                );

                ui.add_space(10.0);
                if ui
                    .checkbox(
                        &mut state.settings.show_lyric_translations,
                        "Show TTML translations & romanization when available",
                    )
                    .changed()
                {
                    settings_changed = true;
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if pill_button(ui, &theme, None, "Clear Lyrics Cache", false, true).clicked() {
                        commands.push(AppCommand::ClearLyricsCache);
                    }
                    ui.label(
                        RichText::new(format!(
                            "{} cached entries (keeps catalog & artwork)",
                            cache_stats.cached_lyrics
                        ))
                        .size(11.5)
                        .color(theme.text_muted),
                    );
                });

                ui.add_space(12.0);
                ui.label(
                    RichText::new("Additional Local Lyrics Folders (.ttml / .lyricsfile / .lrc)")
                        .size(12.5)
                        .color(theme.text_secondary),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.new_local_lyrics_dir)
                            .hint_text("/path/to/lyrics")
                            .desired_width(300.0)
                            .margin(Margin::symmetric(10, 6)),
                    );
                    if pill_button(ui, &theme, Some(Icon::Plus), "Add Folder", false, true)
                        .clicked()
                    {
                        let p = PathBuf::from(state.new_local_lyrics_dir.trim());
                        if !p.as_os_str().is_empty()
                            && !state.settings.local_lyrics_dirs.contains(&p)
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
                                .size(12.0)
                                .color(theme.text_secondary),
                        );
                        if icon_button(
                            ui,
                            &theme,
                            Icon::Close,
                            12.0,
                            22.0,
                            false,
                            true,
                            "Remove folder",
                        )
                        .clicked()
                        {
                            remove_idx = Some(i);
                        }
                    });
                }
                if let Some(idx) = remove_idx {
                    state.settings.local_lyrics_dirs.remove(idx);
                    settings_changed = true;
                }
            });

            ui.add_space(14.0);

            // 4. Security, Storage & Session
            settings_card(ui, &theme, "STORAGE & PRIVACY", "Security, Cache & Session", |ui| {
                ui.spacing_mut().item_spacing.y = 5.0;
                diagnostic_row(ui, &theme, "Credential Vault", credential_backend, theme.text_primary);
                diagnostic_row(
                    ui,
                    &theme,
                    "OS Media Controls",
                    &state.system_media_status,
                    theme.text_secondary,
                );
                let db_summary = format!(
                    "v{} · {} local · {} tracks · {} albums · {} playlists · {} lyrics",
                    cache_stats.schema_version,
                    cache_stats.local_tracks,
                    cache_stats.cached_tracks,
                    cache_stats.cached_albums,
                    cache_stats.cached_playlists,
                    cache_stats.cached_lyrics,
                );
                diagnostic_row(ui, &theme, "SQLite Cache", &db_summary, theme.text_secondary);
                diagnostic_row(
                    ui,
                    &theme,
                    "Spotify OAuth Client ID",
                    if state.client_id_configured {
                        "Configured (PKCE S256)"
                    } else {
                        "Not configured (set SPOTIFY_CLIENT_ID)"
                    },
                    if state.client_id_configured {
                        theme.accent
                    } else {
                        theme.warning
                    },
                );

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if pill_button(ui, &theme, None, "Clear Cache", false, true).clicked() {
                        commands.push(AppCommand::ClearCache);
                    }
                    if pill_button(ui, &theme, Some(Icon::LogOut), "Sign Out", false, true)
                        .clicked()
                    {
                        commands.push(AppCommand::Logout);
                    }
                    if destructive_pill_button(ui, &theme, "Reset Application") {
                        commands.push(AppCommand::ResetApplication);
                    }
                });
            });

            ui.add_space(14.0);

            // 5. Plugins
            settings_card(ui, &theme, "EXTENSIONS", "Loaded Plugins", |ui| {
                if plugins.is_empty() {
                    ui.label(
                        RichText::new("No plugins registered.")
                            .size(12.5)
                            .color(theme.text_muted),
                    );
                } else {
                    for p in plugins {
                        ui.label(
                            RichText::new(format!("{} v{} · {}", p.name, p.version, p.author))
                                .strong()
                                .size(13.0)
                                .color(theme.text_primary),
                        );
                        ui.label(
                            RichText::new(&p.description)
                                .size(12.0)
                                .color(theme.text_secondary),
                        );
                        ui.add_space(6.0);
                    }
                }
            });

            ui.add_space(28.0);
        });

    if settings_changed {
        commands.push(AppCommand::SaveSettings(state.settings.clone()));
    }
}

fn theme_swatch_card(ui: &mut Ui, preset: &ThemePalette, is_active: bool) -> bool {
    let size = Vec2::new(148.0, 84.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let radius = CornerRadius::same(16);
    let painter = ui.painter();
    painter.rect_filled(rect, radius, preset.bg_base);
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(
            if is_active { 2.0 } else { 1.0 },
            if is_active {
                preset.accent
            } else if hovered {
                preset.text_muted
            } else {
                preset.glass_border()
            },
        ),
        StrokeKind::Inside,
    );

    // Mini UI preview strip at top of swatch card
    let preview = Rect::from_min_max(
        Pos2::new(rect.left() + 10.0, rect.top() + 10.0),
        Pos2::new(rect.right() - 10.0, rect.top() + 38.0),
    );
    painter.rect_filled(preview, CornerRadius::same(9), preset.bg_surface);
    // Mini sidebar slice
    let mini_side = Rect::from_min_max(
        preview.left_top(),
        Pos2::new(preview.left() + 28.0, preview.bottom()),
    );
    painter.rect_filled(mini_side, CornerRadius::same(8), preset.bg_elevated);
    // Accent pill inside preview
    let accent_bar = Rect::from_min_size(
        Pos2::new(preview.left() + 36.0, preview.top() + 8.0),
        Vec2::new(34.0, 5.0),
    );
    painter.rect_filled(accent_bar, CornerRadius::same(3), preset.accent);
    let muted_bar = Rect::from_min_size(
        Pos2::new(preview.left() + 36.0, preview.top() + 17.0),
        Vec2::new(56.0, 4.0),
    );
    painter.rect_filled(muted_bar, CornerRadius::same(2), preset.text_muted);

    // Active checkmark badge
    if is_active {
        let badge_c = Pos2::new(rect.right() - 16.0, rect.bottom() - 18.0);
        painter.circle_filled(badge_c, 9.0, preset.accent);
        paint_icon(
            painter,
            Rect::from_center_size(badge_c, Vec2::splat(11.0)),
            Icon::Check,
            preset.on_accent(),
        );
    }

    // Preset name & Dark/Light label
    let text_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 12.0, rect.top() + 45.0),
        Pos2::new(rect.right() - 28.0, rect.bottom() - 8.0),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::top_down(Align::LEFT)),
    );
    child.spacing_mut().item_spacing.y = 1.0;
    child.add(
        egui::Label::new(
            RichText::new(&preset.name)
                .strong()
                .size(12.0)
                .color(preset.text_primary),
        )
        .truncate()
        .selectable(false),
    );
    child.label(
        RichText::new(if preset.dark { "Dark" } else { "Light" })
            .size(10.5)
            .color(preset.text_muted),
    );

    resp.clicked()
}

fn diagnostic_row(
    ui: &mut Ui,
    theme: &ThemePalette,
    label: &str,
    value: &str,
    value_color: Color32,
) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [168.0, 18.0],
            egui::Label::new(RichText::new(label).size(12.5).color(theme.text_muted)),
        );
        ui.label(RichText::new(value).size(12.5).color(value_color));
    });
}

fn destructive_pill_button(ui: &mut Ui, theme: &ThemePalette, label: &str) -> bool {
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(12.5),
        Color32::PLACEHOLDER,
    );
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(galley.size().x + 28.0, 32.0), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if hovered {
        theme.danger
    } else {
        Color32::from_rgba_unmultiplied(theme.danger.r(), theme.danger.g(), theme.danger.b(), 34)
    };
    let fg = if hovered {
        Color32::WHITE
    } else {
        theme.danger
    };
    let r = CornerRadius::same(16);
    ui.painter().rect_filled(rect, r, fill);
    ui.painter().rect_stroke(
        rect,
        r,
        Stroke::new(1.0, theme.danger.linear_multiply(0.65)),
        StrokeKind::Inside,
    );
    ui.painter().galley(
        Pos2::new(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        fg,
    );
    resp.clicked()
}

fn settings_card(
    ui: &mut Ui,
    theme: &ThemePalette,
    eyebrow: &str,
    title: &str,
    add_contents: impl FnOnce(&mut Ui),
) {
    Frame::new()
        .fill(theme.bg_elevated)
        .corner_radius(CornerRadius::same(20))
        .stroke(Stroke::new(1.0, theme.glass_border()))
        .inner_margin(Margin::symmetric(22, 20))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(eyebrow)
                    .size(10.0)
                    .strong()
                    .color(theme.text_muted),
            );
            ui.add_space(2.0);
            ui.label(
                RichText::new(title)
                    .size(16.5)
                    .strong()
                    .color(theme.text_primary),
            );
            ui.add_space(10.0);
            add_contents(ui);
        });
}
