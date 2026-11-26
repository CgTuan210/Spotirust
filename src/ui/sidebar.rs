//! Left navigation sidebar with primary routes, library shortcuts, pinned playlists,
//! and Spotify session badge.

use crate::core::events::AppCommand;
use crate::core::models::Route;
use crate::core::state::{AppState, AuthUiState};
use crate::ui::theme::ThemePalette;
use egui::{Color32, CornerRadius, Frame, Margin, RichText, ScrollArea, Ui, Vec2};

pub fn render_sidebar(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    commands: &mut Vec<AppCommand>,
) {
    ui.add_space(6.0);

    // Original Spotirust branding mark (zero copyrighted assets)
    ui.horizontal(|ui| {
        let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(28.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(icon_rect.center(), 13.0, theme.accent);
        // Three crisp acoustic bars inside the circle
        let c = icon_rect.center();
        for (dx, h) in [(-5.0, 5.0), (0.0, 8.0), (5.0, 4.0)] {
            ui.painter().line_segment(
                [
                    egui::pos2(c.x + dx, c.y - h * 0.5),
                    egui::pos2(c.x + dx, c.y + h * 0.5),
                ],
                egui::Stroke::new(2.2, Color32::BLACK),
            );
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(
                RichText::new("Spotirust")
                    .strong()
                    .size(16.0)
                    .color(theme.text_primary),
            );
            ui.label(
                RichText::new(if state.portable_mode {
                    "Native · Portable"
                } else {
                    "Native Rust Audio"
                })
                .size(10.0)
                .color(theme.text_muted),
            );
        });
    });

    ui.add_space(12.0);

    // Primary navigation
    nav_button(ui, state, theme, "⌂  Home", Route::Home);
    nav_button(ui, state, theme, "⌕  Search", Route::Search);
    nav_button(ui, state, theme, "▤  Your Library", Route::Library);

    ui.add_space(10.0);
    ui.label(
        RichText::new("COLLECTION")
            .size(10.5)
            .strong()
            .color(theme.text_muted),
    );
    ui.add_space(2.0);

    nav_button(ui, state, theme, "♥  Liked Songs", Route::LikedSongs);
    nav_button(
        ui,
        state,
        theme,
        "◷  Recently Played",
        Route::RecentlyPlayed,
    );
    nav_button(ui, state, theme, "♫  Playlists", Route::Playlists);
    nav_button(ui, state, theme, "◎  Albums", Route::Albums);
    nav_button(ui, state, theme, "★  Artists", Route::Artists);
    nav_button(ui, state, theme, "⛁  Local Files", Route::LocalFiles);

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(6.0);

    // Pinned playlists scrollable list
    ui.label(
        RichText::new("PLAYLISTS")
            .size(10.5)
            .strong()
            .color(theme.text_muted),
    );
    ui.add_space(2.0);

    let bottom_reserved = 122.0;
    let list_height = (ui.available_height() - bottom_reserved).max(40.0);
    ScrollArea::vertical()
        .id_salt("sidebar_playlists")
        .max_height(list_height)
        .show(ui, |ui| {
            if state.playlists.is_empty() {
                ui.label(
                    RichText::new("Sign in to sync your Spotify playlists.")
                        .size(11.5)
                        .color(theme.text_muted),
                );
            } else {
                let playlists = state.playlists.clone();
                for pl in playlists {
                    let route = Route::PlaylistDetail(pl.id.clone());
                    let active = state.current_route == route;
                    let text_color = if active {
                        theme.accent
                    } else {
                        theme.text_secondary
                    };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(&pl.name).size(12.5).color(text_color))
                                .frame(active)
                                .fill(if active {
                                    theme.bg_elevated
                                } else {
                                    Color32::TRANSPARENT
                                })
                                .min_size(Vec2::new(ui.available_width(), 26.0)),
                        )
                        .clicked()
                    {
                        state.navigate(route);
                        if state.should_load_playlist(&pl.id) {
                            commands.push(AppCommand::LoadPlaylistTracks { playlist_id: pl.id });
                        }
                    }
                }
            }
        });

    // Bottom Settings + Session card
    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        ui.add_space(6.0);
        render_auth_card(ui, state, theme, commands);
        ui.add_space(4.0);
        nav_button(ui, state, theme, "⚙  Settings", Route::Settings);
    });
}

fn nav_button(ui: &mut Ui, state: &mut AppState, theme: &ThemePalette, label: &str, target: Route) {
    let active = state.current_route == target;
    let fill = if active {
        theme.bg_elevated
    } else {
        Color32::TRANSPARENT
    };
    let text_color = if active {
        theme.accent
    } else {
        theme.text_secondary
    };

    let btn = egui::Button::new(RichText::new(label).size(13.5).strong().color(text_color))
        .fill(fill)
        .corner_radius(CornerRadius::same(8))
        .stroke(if active {
            egui::Stroke::new(1.0, theme.border)
        } else {
            egui::Stroke::NONE
        })
        .min_size(Vec2::new(ui.available_width(), 32.0));

    if ui.add(btn).clicked() {
        state.navigate(target);
    }
}

fn render_auth_card(
    ui: &mut Ui,
    state: &AppState,
    theme: &ThemePalette,
    commands: &mut Vec<AppCommand>,
) {
    Frame::new()
        .fill(theme.bg_elevated)
        .corner_radius(CornerRadius::same(10))
        .stroke(egui::Stroke::new(1.0, theme.border))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            match &state.auth {
                AuthUiState::Authenticated(profile) => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("●").color(theme.accent).size(11.0));
                        ui.add(
                            egui::Label::new(
                                RichText::new(&profile.display_name)
                                    .strong()
                                    .size(12.5)
                                    .color(theme.text_primary),
                            )
                            .truncate(),
                        );
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.small_button("Sync").clicked() {
                            commands.push(AppCommand::RefreshLibrary);
                        }
                        if ui.small_button("Logout").clicked() {
                            commands.push(AppCommand::Logout);
                        }
                    });
                }
                AuthUiState::OpeningBrowser { .. } => {
                    ui.label(
                        RichText::new("Opening browser…")
                            .size(11.5)
                            .color(theme.warning),
                    );
                    if ui.small_button("Open Spotify Login").clicked() {
                        commands.push(AppCommand::OpenPendingLoginUrl);
                    }
                }
                AuthUiState::WaitingForAuthorization { .. } => {
                    ui.label(
                        RichText::new("Waiting for authorization…")
                            .size(11.5)
                            .color(theme.warning),
                    );
                    if ui.small_button("Open Spotify Login").clicked() {
                        commands.push(AppCommand::OpenPendingLoginUrl);
                    }
                }
                AuthUiState::ExchangingToken => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Exchanging token…")
                                .size(11.5)
                                .color(theme.accent),
                        );
                    });
                }
                AuthUiState::Refreshing => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Refreshing session…")
                                .size(11.5)
                                .color(theme.accent),
                        );
                    });
                }
                AuthUiState::SessionExpired { .. } => {
                    ui.label(
                        RichText::new("Session expired")
                            .size(11.5)
                            .strong()
                            .color(theme.warning),
                    );
                    ui.add_space(4.0);
                    let btn = egui::Button::new(
                        RichText::new("Login with Spotify")
                            .strong()
                            .size(12.0)
                            .color(Color32::BLACK),
                    )
                    .fill(theme.accent)
                    .corner_radius(CornerRadius::same(16))
                    .min_size(Vec2::new(ui.available_width(), 28.0));
                    if ui.add(btn).clicked() {
                        commands.push(AppCommand::StartSpotifyLogin);
                    }
                }
                AuthUiState::Failed { authorize_url, .. } => {
                    ui.label(
                        RichText::new("Login failed")
                            .size(11.5)
                            .strong()
                            .color(theme.danger),
                    );
                    ui.horizontal(|ui| {
                        if ui.small_button("Retry Login").clicked() {
                            commands.push(AppCommand::StartSpotifyLogin);
                        }
                        if authorize_url.is_some() && ui.small_button("Open Login").clicked() {
                            commands.push(AppCommand::OpenPendingLoginUrl);
                        }
                    });
                }
                AuthUiState::SignedOut => {
                    let btn = egui::Button::new(
                        RichText::new("Login with Spotify")
                            .strong()
                            .size(12.5)
                            .color(Color32::BLACK),
                    )
                    .fill(theme.accent)
                    .corner_radius(CornerRadius::same(18))
                    .min_size(Vec2::new(ui.available_width(), 32.0));
                    if ui.add(btn).clicked() {
                        commands.push(AppCommand::StartSpotifyLogin);
                    }
                }
            }
        });
}
