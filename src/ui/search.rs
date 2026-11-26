//! Search screen with debounced input, category tabs (All/Tracks/Artists/Albums/Playlists),
//! animated shimmer loading skeletons, result caching, and offline local filtering.

use crate::core::events::AppCommand;
use crate::core::models::Route;
use crate::core::state::{AppState, AuthUiState, SearchTab};
use crate::ui::components::artwork::ArtworkTextureCache;
use crate::ui::components::cards::{media_card, skeleton_card, skeleton_track_row, track_row};
use crate::ui::home::{responsive_card_width, section_header};
use crate::ui::theme::ThemePalette;
use egui::{Color32, CornerRadius, Margin, RichText, ScrollArea, Ui};
use std::time::Instant;

pub fn render_search(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    ui.label(
        RichText::new("Search")
            .size(26.0)
            .strong()
            .color(theme.text_primary),
    );
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        let edit = egui::TextEdit::singleline(&mut state.search_input)
            .hint_text("What do you want to listen to?")
            .desired_width((ui.available_width() - 90.0).max(200.0))
            .margin(Margin::symmetric(14, 9));
        if ui.add(edit).changed() {
            state.search_dirty_since = Some(Instant::now());
        }
        if !state.search_input.is_empty() && ui.button("Clear").clicked() {
            state.search_input.clear();
            state.last_dispatched_query.clear();
            state.search_results = None;
            state.search_loading = false;
            state.search_error = None;
        }
    });

    ui.add_space(10.0);

    // Filter pills
    ui.horizontal(|ui| {
        for (tab, label) in [
            (SearchTab::All, "All"),
            (SearchTab::Tracks, "Songs"),
            (SearchTab::Artists, "Artists"),
            (SearchTab::Albums, "Albums"),
            (SearchTab::Playlists, "Playlists"),
        ] {
            let active = state.search_tab == tab;
            let btn =
                egui::Button::new(RichText::new(label).size(12.5).strong().color(if active {
                    Color32::BLACK
                } else {
                    theme.text_primary
                }))
                .fill(if active {
                    theme.accent
                } else {
                    theme.bg_elevated
                })
                .corner_radius(CornerRadius::same(16));
            if ui.add(btn).clicked() {
                state.search_tab = tab;
            }
        }
    });

    ui.add_space(12.0);

    ScrollArea::vertical()
        .id_salt("search_results_scroll")
        .show(ui, |ui| {
            let time = ui.input(|i| i.time);

            if state.search_loading {
                ui.ctx().request_repaint();
                section_header(ui, theme, "Searching catalog…", None);
                for _ in 0..5 {
                    skeleton_track_row(ui, theme, time);
                }
                ui.add_space(14.0);
                let card_w = responsive_card_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    for _ in 0..4 {
                        skeleton_card(ui, theme, card_w, time);
                    }
                });
                return;
            }

            if let Some(err) = &state.search_error {
                ui.label(
                    RichText::new(format!("Search error: {err}"))
                        .size(13.0)
                        .color(theme.danger),
                );
                ui.add_space(8.0);
            }

            // Show Spotify catalog results if available
            if let Some(results) = state.search_results.clone() {
                let active_id = state
                    .playback
                    .current_track
                    .as_ref()
                    .map(|t| t.id.as_str());
                let playing = state.playback.status.is_playing();
                let card_w = responsive_card_width(ui.available_width());

                if matches!(state.search_tab, SearchTab::All | SearchTab::Tracks)
                    && !results.tracks.is_empty()
                {
                    section_header(ui, theme, "Songs", None);
                    for (idx, track) in results.tracks.iter().enumerate() {
                        track_row(
                            ui,
                            theme,
                            idx,
                            track,
                            active_id == Some(track.id.as_str()),
                            playing,
                            &results.tracks,
                            artwork,
                            commands,
                        );
                    }
                    ui.add_space(16.0);
                }

                if matches!(state.search_tab, SearchTab::All | SearchTab::Artists)
                    && !results.artists.is_empty()
                {
                    section_header(ui, theme, "Artists", None);
                    ui.horizontal_wrapped(|ui| {
                        for artist in &results.artists {
                            let sub = if artist.genres.is_empty() {
                                "Artist".to_string()
                            } else {
                                artist.genres.iter().take(2).cloned().collect::<Vec<_>>().join(" · ")
                            };
                            if media_card(
                                ui,
                                theme,
                                &artist.name,
                                &sub,
                                None,
                                artist.image_url.as_deref(),
                                None,
                                card_w,
                                artwork,
                                commands,
                            ) {
                                let id = artist.id.clone();
                                state.navigate(Route::ArtistDetail(id.clone()));
                                commands.push(AppCommand::LoadArtistAlbums { artist_id: id });
                            }
                        }
                    });
                    ui.add_space(16.0);
                }

                if matches!(state.search_tab, SearchTab::All | SearchTab::Albums)
                    && !results.albums.is_empty()
                {
                    section_header(ui, theme, "Albums", None);
                    ui.horizontal_wrapped(|ui| {
                        for alb in &results.albums {
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
                                commands.push(AppCommand::LoadAlbumTracks { album_id: id });
                            }
                        }
                    });
                    ui.add_space(16.0);
                }

                if matches!(state.search_tab, SearchTab::All | SearchTab::Playlists)
                    && !results.playlists.is_empty()
                {
                    section_header(ui, theme, "Playlists", None);
                    ui.horizontal_wrapped(|ui| {
                        for pl in &results.playlists {
                            if media_card(
                                ui,
                                theme,
                                &pl.name,
                                &format!("By {}", pl.owner_name),
                                None,
                                pl.artwork_url.as_deref(),
                                None,
                                card_w,
                                artwork,
                                commands,
                            ) {
                                let id = pl.id.clone();
                                state.navigate(Route::PlaylistDetail(id.clone()));
                                commands.push(AppCommand::LoadPlaylistTracks { playlist_id: id });
                            }
                        }
                    });
                }
                return;
            }

            // Local/offline filter fallback when signed out or query empty
            let q = state.search_input.trim().to_lowercase();
            let mut local_pool = state.local_tracks.clone();
            local_pool.extend(state.showcase_tracks.clone());

            if !q.is_empty() {
                let matches: Vec<_> = local_pool
                    .into_iter()
                    .filter(|t| {
                        t.title.to_lowercase().contains(&q)
                            || t.artist_display().to_lowercase().contains(&q)
                            || t.album_name.to_lowercase().contains(&q)
                    })
                    .collect();

                if matches.is_empty() {
                    let hint = if matches!(state.auth, AuthUiState::Authenticated(_)) {
                        "No matching tracks found."
                    } else {
                        "Sign in with Spotify to search the full Spotify catalog, or add local music folders in Local Files."
                    };
                    ui.label(RichText::new(hint).size(13.0).color(theme.text_secondary));
                } else {
                    section_header(ui, theme, "Matching Local & Showcase Tracks", None);
                    let active_id = state
                        .playback
                        .current_track
                        .as_ref()
                        .map(|t| t.id.as_str());
                    let playing = state.playback.status.is_playing();
                    for (idx, track) in matches.iter().enumerate() {
                        track_row(
                            ui,
                            theme,
                            idx,
                            track,
                            active_id == Some(track.id.as_str()),
                            playing,
                            &matches,
                            artwork,
                            commands,
                        );
                    }
                }
            } else {
                ui.label(
                    RichText::new(
                        "Type above to search songs, artists, albums, and playlists. Requests are debounced (320 ms) and cached locally.",
                    )
                    .size(13.0)
                    .color(theme.text_secondary),
                );
            }
        });
}
