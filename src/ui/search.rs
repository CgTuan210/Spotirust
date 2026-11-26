//! Search screen with integrated pill input, category filter tabs,
//! zero-CLS shimmer skeletons, circular artist cards, aligned song table,
//! and warm empty states.

use crate::core::events::AppCommand;
use crate::core::models::Route;
use crate::core::state::{AppState, AuthUiState, SearchTab};
use crate::ui::components::artwork::ArtworkTextureCache;
use crate::ui::components::cards::{
    artist_card, empty_state, media_card, skeleton_card, skeleton_track_row, track_row,
    track_table_header,
};
use crate::ui::components::icons::{Icon, icon_button, paint_icon};
use crate::ui::home::{responsive_card_width, section_header};
use crate::ui::theme::ThemePalette;
use egui::{
    Color32, CornerRadius, CursorIcon, FontId, Frame, Margin, Pos2, RichText, ScrollArea, Sense,
    Stroke, StrokeKind, Ui, Vec2,
};
use std::time::Instant;

pub fn render_search(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    ui.add_space(4.0);
    ui.label(
        RichText::new("Search")
            .size(26.0)
            .strong()
            .color(theme.text_primary),
    );
    ui.add_space(10.0);

    // Spacious pill search input
    let bar_w = ui.available_width().min(540.0);
    Frame::new()
        .fill(theme.bg_elevated)
        .corner_radius(CornerRadius::same(20))
        .stroke(Stroke::new(1.0, theme.card_stroke()))
        .inner_margin(Margin::symmetric(14, 8))
        .show(ui, |ui| {
            ui.set_width(bar_w);
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (ic_rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                paint_icon(ui.painter(), ic_rect, Icon::Search, theme.text_muted);

                let clear_w = if state.search_input.is_empty() {
                    0.0
                } else {
                    24.0
                };
                let edit = egui::TextEdit::singleline(&mut state.search_input)
                    .frame(Frame::NONE)
                    .hint_text(
                        RichText::new("What do you want to listen to?")
                            .size(13.5)
                            .color(theme.text_muted),
                    )
                    .desired_width((ui.available_width() - clear_w).max(120.0));
                if ui.add(edit).changed() {
                    state.search_dirty_since = Some(Instant::now());
                }
                if !state.search_input.is_empty()
                    && icon_button(
                        ui,
                        theme,
                        Icon::Close,
                        13.0,
                        22.0,
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
            });
        });

    ui.add_space(12.0);

    // Filter pills
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (tab, label) in [
            (SearchTab::All, "All"),
            (SearchTab::Tracks, "Songs"),
            (SearchTab::Artists, "Artists"),
            (SearchTab::Albums, "Albums"),
            (SearchTab::Playlists, "Playlists"),
        ] {
            if filter_chip(ui, theme, label, state.search_tab == tab) {
                state.search_tab = tab;
            }
        }
    });

    ui.add_space(16.0);

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
                ui.add_space(20.0);
                let card_w = responsive_card_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(14.0);
                    for _ in 0..4 {
                        skeleton_card(ui, theme, card_w, time);
                    }
                });
                return;
            }

            if let Some(err) = &state.search_error {
                Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(
                        theme.danger.r(),
                        theme.danger.g(),
                        theme.danger.b(),
                        26,
                    ))
                    .corner_radius(CornerRadius::same(10))
                    .stroke(Stroke::new(1.0, theme.danger.linear_multiply(0.55)))
                    .inner_margin(Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("Search error: {err}"))
                                .size(12.5)
                                .color(theme.text_primary),
                        );
                    });
                ui.add_space(12.0);
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
                let mut showed_any = false;

                if matches!(state.search_tab, SearchTab::All | SearchTab::Tracks)
                    && !results.tracks.is_empty()
                {
                    showed_any = true;
                    section_header(ui, theme, "Songs", None);
                    track_table_header(ui, theme);
                    ui.spacing_mut().item_spacing.y = 2.0;
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
                    ui.spacing_mut().item_spacing.y = 8.0;
                    ui.add_space(22.0);
                }

                if matches!(state.search_tab, SearchTab::All | SearchTab::Artists)
                    && !results.artists.is_empty()
                {
                    showed_any = true;
                    section_header(ui, theme, "Artists", None);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::splat(14.0);
                        for artist in &results.artists {
                            let sub = if artist.genres.is_empty() {
                                "Artist".to_string()
                            } else {
                                artist
                                    .genres
                                    .iter()
                                    .take(2)
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(" · ")
                            };
                            if artist_card(
                                ui,
                                theme,
                                &artist.name,
                                &sub,
                                artist.image_url.as_deref(),
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
                    ui.add_space(22.0);
                }

                if matches!(state.search_tab, SearchTab::All | SearchTab::Albums)
                    && !results.albums.is_empty()
                {
                    showed_any = true;
                    section_header(ui, theme, "Albums", None);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::splat(14.0);
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
                    ui.add_space(22.0);
                }

                if matches!(state.search_tab, SearchTab::All | SearchTab::Playlists)
                    && !results.playlists.is_empty()
                {
                    showed_any = true;
                    section_header(ui, theme, "Playlists", None);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::splat(14.0);
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

                if !showed_any {
                    empty_state(
                        ui,
                        theme,
                        Icon::Search,
                        "No matches in this category",
                        "Try switching to All or refining your search query.",
                    );
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
                        "Try a different song title, artist name, or album."
                    } else {
                        "Sign in with Spotify to search the full catalog, or index local folders in Local Files."
                    };
                    empty_state(ui, theme, Icon::Search, "No matching tracks found", hint);
                } else {
                    section_header(ui, theme, "Matching Local & Showcase Tracks", None);
                    track_table_header(ui, theme);
                    let active_id = state
                        .playback
                        .current_track
                        .as_ref()
                        .map(|t| t.id.as_str());
                    let playing = state.playback.status.is_playing();
                    ui.spacing_mut().item_spacing.y = 2.0;
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
                empty_state(
                    ui,
                    theme,
                    Icon::Search,
                    "Search your music",
                    "Find songs, artists, albums, and playlists. Queries are debounced (320 ms) and cached locally.",
                );
            }
        });
}

fn filter_chip(ui: &mut Ui, theme: &ThemePalette, label: &str, active: bool) -> bool {
    let font_id = FontId::proportional(12.5);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, Color32::PLACEHOLDER);
    let w = galley.size().x + 26.0;
    let h = 30.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    let hovered = resp.hovered();
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let radius = CornerRadius::same(15);
    let (bg, fg, stroke) = if active {
        (theme.play_circle_bg(), theme.play_circle_fg(), Stroke::NONE)
    } else if hovered {
        (
            theme.bg_hover,
            theme.text_primary,
            Stroke::new(1.0, theme.card_stroke()),
        )
    } else {
        (
            theme.bg_elevated,
            theme.text_secondary,
            Stroke::new(1.0, theme.hairline()),
        )
    };

    ui.painter().rect_filled(rect, radius, bg);
    if stroke != Stroke::NONE {
        ui.painter()
            .rect_stroke(rect, radius, stroke, StrokeKind::Inside);
    }
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
