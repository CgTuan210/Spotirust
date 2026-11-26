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
                        "Spotirust strictly separates two honest modes and never simulates fake playback:\n\
                         • Spotify Connect (Remote): controls an active external Spotify app, speaker, or Spotify Soloist (requires Spotify Premium).\n\
                         • Local Player (Rodio): plays real local MP3/FLAC/OGG/WAV files natively on your sound card.",
                    )
                    .size(12.0)
                    .color(theme.text_secondary),
                );
                ui.add_space(8.0);

                for kind in [
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
                        "Platform note: Direct in-window Spotify streaming via librespot 0.8 is currently blocked \
                         for third-party OAuth tokens since Aug 10, 2026 (librespot#1737). Use Spotify Connect \
                         for Spotify tracks or Local Player for local audio files.",
                    )
                    .size(11.0)
                    .color(theme.text_muted),
                );
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
