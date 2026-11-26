//! Home dashboard: time-of-day greeting, Spotify OAuth hero (with graceful error recovery),
//! Recently Played, Made For You / Top Tracks, Recommended Playlists, Saved Albums,
//! and Spotirust Soundcheck (offline word-by-word karaoke showcase).

use crate::audio::soloist::SoloistStatus;
use crate::core::events::AppCommand;
use crate::core::models::Route;
use crate::core::state::{AppState, AuthUiState};
use crate::ui::components::artwork::ArtworkTextureCache;
use crate::ui::components::cards::{media_card, track_row};
use crate::ui::theme::ThemePalette;
use chrono::Timelike;
use egui::{Color32, CornerRadius, Frame, Margin, RichText, ScrollArea, Stroke, Ui, Vec2};

pub fn render_home(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    ScrollArea::vertical()
        .id_salt("home_scroll")
        .show(ui, |ui| {
            // Time-based greeting
            let greeting = time_of_day_greeting();
            let sub = match &state.auth {
                AuthUiState::Authenticated(p) => format!("Welcome back, {}", p.display_name),
                _ => "Native Rust music experience with word-by-word karaoke lyrics".to_string(),
            };

            ui.label(
                RichText::new(greeting)
                    .size(28.0)
                    .strong()
                    .color(theme.text_primary),
            );
            ui.label(RichText::new(sub).size(13.5).color(theme.text_secondary));
            ui.add_space(14.0);

            // Authentication hero banner when signed out or recovering from login error
            if !matches!(state.auth, AuthUiState::Authenticated(_)) {
                render_auth_hero(ui, state, theme, commands);
                ui.add_space(18.0);
            } else if !state.soloist.logged_in {
                render_soloist_banner(ui, state, theme, commands);
                ui.add_space(14.0);
            }

            // Recently Played section
            if !state.recently_played.is_empty() {
                section_header(ui, theme, "Recently Played", Some("Your latest listening history"));
                let recent = state.recently_played.clone();
                render_track_cards_row(ui, theme, &recent, artwork, commands, false);
                ui.add_space(18.0);
            }

            // Made For You / Your Top Tracks
            if !state.top_tracks.is_empty() {
                section_header(
                    ui,
                    theme,
                    "Made For You · Heavy Rotation",
                    Some("Based on your Spotify Top Tracks"),
                );
                let active_id = state
                    .playback
                    .current_track
                    .as_ref()
                    .map(|t| t.id.as_str());
                let playing = state.playback.status.is_playing();
                let top = state.top_tracks.clone();
                for (idx, track) in top.iter().take(6).enumerate() {
                    track_row(
                        ui,
                        theme,
                        idx,
                        track,
                        active_id == Some(track.id.as_str()),
                        playing,
                        &top,
                        artwork,
                        commands,
                    );
                }
                ui.add_space(18.0);
            }

            // Playlists section
            if !state.playlists.is_empty() {
                section_header(ui, theme, "Recommended & Your Playlists", None);
                let playlists = state.playlists.clone();
                let card_w = responsive_card_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    for pl in playlists.iter().take(8) {
                        let sub = format!("By {} · {} items", pl.owner_name, pl.total_tracks);
                        if media_card(
                            ui,
                            theme,
                            &pl.name,
                            &sub,
                            None,
                            pl.artwork_url.as_deref(),
                            None,
                            card_w,
                            artwork,
                            commands,
                        ) {
                            let id = pl.id.clone();
                            state.navigate(Route::PlaylistDetail(id.clone()));
                            if state.should_load_playlist(&id) {
                                commands.push(AppCommand::LoadPlaylistTracks { playlist_id: id });
                            }
                        }
                    }
                });
                ui.add_space(18.0);
            }

            // Saved Albums section
            if !state.albums.is_empty() {
                section_header(ui, theme, "Popular & Saved Albums", None);
                let albums = state.albums.clone();
                let card_w = responsive_card_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    for alb in albums.iter().take(8) {
                        if media_card(
                            ui,
                            theme,
                            &alb.name,
                            &alb.artist_display(),
                            alb.release_date.as_deref(),
                            alb.artwork_url.as_deref(),
                            alb.accent_rgb,
                            card_w,
                            artwork,
                            commands,
                        ) {
                            let id = alb.id.clone();
                            state.navigate(Route::AlbumDetail(id.clone()));
                            if state.should_load_album(&id) {
                                commands.push(AppCommand::LoadAlbumTracks { album_id: id });
                            }
                        }
                    }
                });
                ui.add_space(18.0);
            }

            // Featured Local Soundcheck (real synthesized WAV files played natively via rodio)
            section_header(
                ui,
                theme,
                "Local Soundcheck · Native Rodio WAV + Karaoke",
                Some("Royalty-free local WAV audio files played through your real audio output device with TTML & Enhanced LRC word-by-word timing"),
            );
            let showcase = state.showcase_tracks.clone();
            render_track_cards_row(ui, theme, &showcase, artwork, commands, true);

            ui.add_space(24.0);
        });
}

fn render_auth_hero(
    ui: &mut Ui,
    state: &AppState,
    theme: &ThemePalette,
    commands: &mut Vec<AppCommand>,
) {
    Frame::new()
        .fill(theme.bg_elevated)
        .corner_radius(CornerRadius::same(14))
        .stroke(Stroke::new(1.0, theme.border))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("Connect your Spotify account")
                            .size(18.0)
                            .strong()
                            .color(theme.text_primary),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Sign in securely through your default browser using Spotify's official OAuth 2.0 + PKCE flow. \
                             Spotirust never asks for your password, Client Secret, or manual access tokens.",
                        )
                        .size(12.5)
                        .color(theme.text_secondary),
                    );
                    ui.add_space(10.0);

                    match &state.auth {
                        AuthUiState::SignedOut => {
                            let btn = egui::Button::new(
                                RichText::new("Login with Spotify")
                                    .strong()
                                    .size(14.0)
                                    .color(Color32::BLACK),
                            )
                            .fill(theme.accent)
                            .corner_radius(CornerRadius::same(20))
                            .min_size(Vec2::new(200.0, 38.0));
                            if ui.add(btn).clicked() {
                                commands.push(AppCommand::StartSpotifyLogin);
                            }
                            if !state.client_id_configured {
                                ui.add_space(6.0);
                                ui.label(
                                    RichText::new(
                                        "Note: This local build has no developer SPOTIFY_CLIENT_ID configured yet. \
                                         Set SPOTIFY_CLIENT_ID (or SPOTIRUST_CLIENT_ID) during build/launch (see README & docs/ARCHITECTURE.md).",
                                    )
                                    .size(11.5)
                                    .color(theme.warning),
                                );
                            }
                        }
                        AuthUiState::OpeningBrowser { .. } => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(
                                    RichText::new("Opening your default browser for Spotify login…")
                                        .size(12.5)
                                        .color(theme.accent),
                                );
                            });
                            ui.add_space(8.0);
                            if ui.button("Open Spotify Login").clicked() {
                                commands.push(AppCommand::OpenPendingLoginUrl);
                            }
                        }
                        AuthUiState::WaitingForAuthorization { .. } => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(
                                    RichText::new("Waiting for authorization in your browser… Spotirust will receive the loopback callback automatically.")
                                        .size(12.5)
                                        .color(theme.accent),
                                );
                            });
                            ui.add_space(8.0);
                            if ui.button("Open Spotify Login").clicked() {
                                commands.push(AppCommand::OpenPendingLoginUrl);
                            }
                        }
                        AuthUiState::ExchangingToken => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(
                                    RichText::new("Authorization received — exchanging PKCE code for secure tokens…")
                                        .size(12.5)
                                        .color(theme.accent),
                                );
                            });
                        }
                        AuthUiState::Refreshing => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(
                                    RichText::new("Restoring & refreshing your Spotify session…")
                                        .size(12.5)
                                        .color(theme.accent),
                                );
                            });
                        }
                        AuthUiState::SessionExpired { message } => {
                            Frame::new()
                                .fill(theme.warning.linear_multiply(0.14))
                                .corner_radius(CornerRadius::same(8))
                                .stroke(Stroke::new(1.0, theme.warning))
                                .inner_margin(Margin::same(10))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!("Session expired: {message}"))
                                            .size(12.0)
                                            .color(theme.text_primary),
                                    );
                                });
                            ui.add_space(8.0);
                            let btn = egui::Button::new(
                                RichText::new("Login with Spotify")
                                    .strong()
                                    .size(13.5)
                                    .color(Color32::BLACK),
                            )
                            .fill(theme.accent)
                            .corner_radius(CornerRadius::same(18))
                            .min_size(Vec2::new(190.0, 34.0));
                            if ui.add(btn).clicked() {
                                commands.push(AppCommand::StartSpotifyLogin);
                            }
                        }
                        AuthUiState::Failed {
                            message,
                            authorize_url,
                        } => {
                            Frame::new()
                                .fill(theme.danger.linear_multiply(0.14))
                                .corner_radius(CornerRadius::same(8))
                                .stroke(Stroke::new(1.0, theme.danger))
                                .inner_margin(Margin::same(10))
                                .show(ui, |ui| {
                                    ui.label(
                                        RichText::new(format!("Authentication error: {message}"))
                                            .size(12.0)
                                            .color(theme.text_primary),
                                    );
                                });
                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                let retry = egui::Button::new(
                                    RichText::new("Retry Login")
                                        .strong()
                                        .color(Color32::BLACK),
                                )
                                .fill(theme.accent)
                                .corner_radius(CornerRadius::same(16));
                                if ui.add(retry).clicked() {
                                    commands.push(AppCommand::StartSpotifyLogin);
                                }
                                if authorize_url.is_some()
                                    && ui.button("Open Spotify Login").clicked()
                                {
                                    commands.push(AppCommand::OpenPendingLoginUrl);
                                }
                            });
                        }
                        AuthUiState::Authenticated(_) => {}
                    }
                });
            });
        });
}

fn render_soloist_banner(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    commands: &mut Vec<AppCommand>,
) {
    let (accent_color, heading, detail) = match &state.soloist.status {
        SoloistStatus::WaitingForPairing { device_name, .. } => (
            theme.accent,
            format!("Pair Spotify Soloist once: select '{device_name}' in Spotify"),
            format!(
                "Open the official Spotify app (phone or desktop on the same Wi-Fi) and pick '{device_name}' \
                 in Connect devices. After that one-time handshake, you can close the official Spotify app permanently."
            ),
        ),
        SoloistStatus::Expired { message } => (
            theme.warning,
            "Spotify Soloist build expired (90-day limit)".to_string(),
            message.clone(),
        ),
        SoloistStatus::Downloading => (
            theme.accent,
            "Downloading official Spotify Soloist binary…".to_string(),
            "Fetching the official prebuilt archive directly from Spotify's CDN and verifying its executable header."
                .to_string(),
        ),
        SoloistStatus::Starting => (
            theme.accent,
            "Starting Spotify Soloist engine…".to_string(),
            "Launching local headless playback engine on 127.0.0.1 and waiting for its WebSocket handshake."
                .to_string(),
        ),
        SoloistStatus::Error { message } => (
            theme.danger,
            "Spotify Soloist needs attention".to_string(),
            message.clone(),
        ),
        _ => (
            theme.border,
            "Play Spotify directly without keeping the official Spotify app open".to_string(),
            "Enable Spotify Soloist in Settings (Spotify's official headless engine, requires Premium) \
             so Spotirust plays full Spotify tracks natively even after you close the official Spotify app."
                .to_string(),
        ),
    };

    Frame::new()
        .fill(theme.bg_elevated)
        .corner_radius(CornerRadius::same(12))
        .stroke(Stroke::new(1.0, accent_color))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                if matches!(
                    state.soloist.status,
                    SoloistStatus::Downloading | SoloistStatus::Starting
                ) {
                    ui.spinner();
                }
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(heading)
                            .size(14.5)
                            .strong()
                            .color(theme.text_primary),
                    );
                    ui.add_space(3.0);
                    ui.label(RichText::new(detail).size(12.0).color(theme.text_secondary));
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        let primary_label = match state.soloist.status {
                            SoloistStatus::WaitingForPairing { .. }
                            | SoloistStatus::Downloading
                            | SoloistStatus::Starting => "Soloist Settings",
                            SoloistStatus::Expired { .. } => "Update Soloist",
                            _ if state.soloist.api_key_configured
                                && state.soloist.binary_path.is_some() =>
                            {
                                "Start Soloist"
                            }
                            _ => "Set up Spotify Soloist",
                        };

                        let btn = egui::Button::new(
                            RichText::new(primary_label)
                                .strong()
                                .size(12.5)
                                .color(Color32::BLACK),
                        )
                        .fill(theme.accent)
                        .corner_radius(CornerRadius::same(16))
                        .min_size(Vec2::new(150.0, 30.0));

                        if ui.add(btn).clicked() {
                            match state.soloist.status {
                                SoloistStatus::Expired { .. } => {
                                    commands.push(AppCommand::DownloadSoloist);
                                }
                                SoloistStatus::Stopped | SoloistStatus::Error { .. }
                                    if state.soloist.api_key_configured
                                        && state.soloist.binary_path.is_some() =>
                                {
                                    commands.push(AppCommand::StartSoloist);
                                }
                                _ => {
                                    state.navigate(Route::Settings);
                                }
                            }
                        }

                        if primary_label != "Soloist Settings"
                            && ui.button("Open Settings").clicked()
                        {
                            state.navigate(Route::Settings);
                        }
                    });
                });
            });
        });
}

fn render_track_cards_row(
    ui: &mut Ui,
    theme: &ThemePalette,
    tracks: &[crate::core::models::Track],
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
    show_lyrics_badge: bool,
) {
    let card_w = responsive_card_width(ui.available_width());
    ui.horizontal_wrapped(|ui| {
        for track in tracks.iter().take(8) {
            let badge = if show_lyrics_badge {
                if track.id.contains("midnight") {
                    Some("TTML · Word-by-Word")
                } else {
                    Some("Enhanced LRC · Word-by-Word")
                }
            } else {
                None
            };
            if media_card(
                ui,
                theme,
                &track.title,
                &track.artist_display(),
                badge,
                track.artwork_url.as_deref(),
                track.accent_rgb,
                card_w,
                artwork,
                commands,
            ) {
                commands.push(AppCommand::PlayTrack {
                    track: track.clone(),
                    queue_context: tracks.to_vec(),
                });
            }
        }
    });
}

pub fn section_header(ui: &mut Ui, theme: &ThemePalette, title: &str, subtitle: Option<&str>) {
    ui.label(
        RichText::new(title)
            .size(19.0)
            .strong()
            .color(theme.text_primary),
    );
    if let Some(sub) = subtitle {
        ui.label(RichText::new(sub).size(12.0).color(theme.text_secondary));
    }
    ui.add_space(8.0);
}

pub fn responsive_card_width(avail_width: f32) -> f32 {
    let cols = ((avail_width / 178.0).floor() as usize).clamp(2, 6);
    ((avail_width - (cols as f32 - 1.0) * 10.0) / cols as f32).clamp(145.0, 200.0)
}

pub fn time_of_day_greeting() -> &'static str {
    let hour = chrono::Local::now().hour();
    match hour {
        5..=11 => "Good morning",
        12..=17 => "Good afternoon",
        18..=22 => "Good evening",
        _ => "Late night listening",
    }
}
