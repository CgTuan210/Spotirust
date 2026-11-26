//! Left navigation sidebar following macOS/Apple Music source-list conventions:
//! crisp geometric vector icons, borderless quiet rows with leading active pills,
//! clean section hierarchy, scrollable playlists, and an upright profile footer.

use crate::core::events::AppCommand;
use crate::core::models::Route;
use crate::core::state::{AppState, AuthUiState};
use crate::ui::components::icons::{Icon, icon_button, paint_brand_mark, paint_icon, pill_button};
use crate::ui::theme::ThemePalette;
use egui::{
    Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Layout, Margin, Pos2, Rect,
    RichText, ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2,
};

pub fn render_sidebar(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    commands: &mut Vec<AppCommand>,
) {
    ui.spacing_mut().item_spacing.y = 2.0;

    // 1. Brand header
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
        paint_brand_mark(ui.painter(), icon_rect, theme.accent, theme.on_accent());
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(
                RichText::new("Spotirust")
                    .strong()
                    .size(15.5)
                    .color(theme.text_primary),
            );
            ui.label(
                RichText::new(if state.portable_mode {
                    "Native · Portable"
                } else {
                    "Native Audio"
                })
                .size(10.5)
                .color(theme.text_muted),
            );
        });
    });

    ui.add_space(16.0);

    // 2. Primary navigation
    nav_item(ui, state, theme, Icon::Home, "Home", Route::Home);
    nav_item(ui, state, theme, Icon::Search, "Search", Route::Search);
    nav_item(
        ui,
        state,
        theme,
        Icon::Library,
        "Your Library",
        Route::Library,
    );

    // 3. Library Collection
    section_label(ui, theme, "COLLECTION");
    let liked_icon = if state.current_route == Route::LikedSongs {
        Icon::HeartFilled
    } else {
        Icon::Heart
    };
    nav_item(
        ui,
        state,
        theme,
        liked_icon,
        "Liked Songs",
        Route::LikedSongs,
    );
    nav_item(
        ui,
        state,
        theme,
        Icon::Clock,
        "Recently Played",
        Route::RecentlyPlayed,
    );
    nav_item(
        ui,
        state,
        theme,
        Icon::MusicNote,
        "Playlists",
        Route::Playlists,
    );
    nav_item(ui, state, theme, Icon::Disc, "Albums", Route::Albums);
    nav_item(ui, state, theme, Icon::Artist, "Artists", Route::Artists);
    nav_item(
        ui,
        state,
        theme,
        Icon::Folder,
        "Local Files",
        Route::LocalFiles,
    );

    // 4. Playlists header + scrollable list
    section_label(ui, theme, "PLAYLISTS");

    let footer_reserved = 10.0 + 34.0 + 8.0 + auth_card_estimated_height(&state.auth) + 4.0;
    let list_height = (ui.available_height() - footer_reserved).max(40.0);
    let list_rect = Rect::from_min_size(
        ui.cursor().min,
        Vec2::new(ui.available_width(), list_height),
    );
    ui.allocate_rect(list_rect, Sense::hover());
    let mut list_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(list_rect)
            .layout(Layout::top_down(Align::LEFT)),
    );
    list_ui.set_clip_rect(list_rect.intersect(ui.clip_rect()));
    ScrollArea::vertical()
        .id_salt("sidebar_playlists")
        .max_height(list_height)
        .auto_shrink([false, false])
        .show(&mut list_ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            if state.playlists.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Sign in to sync playlists")
                            .size(12.0)
                            .color(theme.text_muted),
                    );
                });
            } else {
                let playlists = state.playlists.clone();
                for pl in playlists {
                    let route = Route::PlaylistDetail(pl.id.clone());
                    let active = state.current_route == route;
                    if playlist_row(ui, theme, &pl.name, active) {
                        state.navigate(route);
                        if state.should_load_playlist(&pl.id) {
                            commands.push(AppCommand::LoadPlaylistTracks { playlist_id: pl.id });
                        }
                    }
                }
            }
        });

    // 5. Pinned Footer (Hairline -> Settings -> Compact Account Card) in natural top-down flow
    let (sep_rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 10.0), Sense::hover());
    ui.painter().line_segment(
        [
            Pos2::new(sep_rect.left() + 6.0, sep_rect.center().y),
            Pos2::new(sep_rect.right() - 6.0, sep_rect.center().y),
        ],
        Stroke::new(1.0, theme.hairline()),
    );
    nav_item(
        ui,
        state,
        theme,
        Icon::Settings,
        "Settings",
        Route::Settings,
    );
    ui.add_space(6.0);
    render_auth_card(ui, state, theme, commands);
}

fn auth_card_estimated_height(auth: &AuthUiState) -> f32 {
    match auth {
        AuthUiState::Authenticated(_) | AuthUiState::SignedOut => 52.0,
        AuthUiState::ExchangingToken | AuthUiState::Refreshing => 44.0,
        AuthUiState::OpeningBrowser { .. }
        | AuthUiState::WaitingForAuthorization { .. }
        | AuthUiState::SessionExpired { .. }
        | AuthUiState::Failed { .. } => 78.0,
    }
}

fn section_label(ui: &mut Ui, theme: &ThemePalette, text: &str) {
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(
            RichText::new(text)
                .size(10.5)
                .strong()
                .color(theme.text_muted),
        );
    });
    ui.add_space(4.0);
}

fn nav_item(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    icon: Icon,
    label: &str,
    target: Route,
) {
    let active = state.current_route == target;
    let width = ui.available_width();
    let height = 34.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let hovered = response.hovered();

    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let hover_t = ui.ctx().animate_bool_responsive(response.id, hovered);
    let painter = ui.painter();
    let radius = CornerRadius::same(12);

    if active {
        painter.rect_filled(rect, radius, theme.active_pill_fill());
        painter.rect_stroke(
            rect,
            radius,
            Stroke::new(1.0, theme.glass_border()),
            StrokeKind::Inside,
        );
        // Subtle leading vertical accent capsule
        let bar = Rect::from_center_size(
            Pos2::new(rect.left() + 4.0, rect.center().y),
            Vec2::new(3.0, 16.0),
        );
        painter.rect_filled(bar, CornerRadius::same(2), theme.accent);
    } else if hover_t > 0.01 {
        let h = theme.hover_overlay();
        let fill = Color32::from_rgba_unmultiplied(
            h.r(),
            h.g(),
            h.b(),
            (f32::from(h.a()) * hover_t).min(255.0) as u8,
        );
        painter.rect_filled(rect, radius, fill);
    }

    let icon_color = if active {
        theme.accent
    } else if hovered {
        theme.text_primary
    } else {
        theme.text_secondary
    };
    let text_color = if active || hovered {
        theme.text_primary
    } else {
        theme.text_secondary
    };

    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.left() + 20.0, rect.center().y),
        Vec2::splat(16.0),
    );
    paint_icon(painter, icon_rect, icon, icon_color);

    let text_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 38.0, rect.top()),
        Pos2::new(rect.right() - 8.0, rect.bottom()),
    );
    let mut text_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    let rt = RichText::new(label).size(13.0).color(text_color);
    text_ui.add(
        egui::Label::new(if active { rt.strong() } else { rt })
            .truncate()
            .selectable(false),
    );

    if response.clicked() {
        state.navigate(target);
    }
}

fn playlist_row(ui: &mut Ui, theme: &ThemePalette, name: &str, active: bool) -> bool {
    let width = ui.available_width();
    let height = 30.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let hovered = response.hovered();

    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }

    let hover_t = ui.ctx().animate_bool_responsive(response.id, hovered);
    let painter = ui.painter();
    let radius = CornerRadius::same(10);

    if active {
        painter.rect_filled(rect, radius, theme.active_pill_fill());
        let bar = Rect::from_center_size(
            Pos2::new(rect.left() + 4.0, rect.center().y),
            Vec2::new(2.5, 14.0),
        );
        painter.rect_filled(bar, CornerRadius::same(2), theme.accent);
    } else if hover_t > 0.01 {
        let h = theme.hover_overlay();
        let fill = Color32::from_rgba_unmultiplied(
            h.r(),
            h.g(),
            h.b(),
            (f32::from(h.a()) * hover_t).min(255.0) as u8,
        );
        painter.rect_filled(rect, radius, fill);
    }

    let dot_col = if active {
        theme.accent
    } else if hovered {
        theme.text_secondary
    } else {
        theme.text_muted
    };
    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.left() + 18.0, rect.center().y),
        Vec2::splat(13.0),
    );
    paint_icon(painter, icon_rect, Icon::MusicNote, dot_col);

    let text_col = if active {
        theme.accent
    } else if hovered {
        theme.text_primary
    } else {
        theme.text_secondary
    };
    let text_rect = Rect::from_min_max(
        Pos2::new(rect.left() + 34.0, rect.top()),
        Pos2::new(rect.right() - 8.0, rect.bottom()),
    );
    let mut text_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(text_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    text_ui.add(
        egui::Label::new(RichText::new(name).size(12.5).color(text_col))
            .truncate()
            .selectable(false),
    );

    response.clicked()
}

fn render_auth_card(
    ui: &mut Ui,
    state: &AppState,
    theme: &ThemePalette,
    commands: &mut Vec<AppCommand>,
) {
    Frame::new()
        .fill(theme.glass_pill_fill())
        .corner_radius(CornerRadius::same(14))
        .stroke(Stroke::new(1.0, theme.glass_border()))
        .inner_margin(Margin::symmetric(10, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            match &state.auth {
                AuthUiState::Authenticated(profile) => {
                    ui.horizontal(|ui| {
                        // User initial avatar with live online status dot
                        let (av_rect, _) =
                            ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                        ui.painter()
                            .circle_filled(av_rect.center(), 15.0, theme.accent_soft());
                        ui.painter().circle_stroke(
                            av_rect.center(),
                            15.0,
                            Stroke::new(1.0, theme.accent.linear_multiply(0.45)),
                        );
                        let initial = profile
                            .display_name
                            .chars()
                            .next()
                            .map(|c| c.to_uppercase().to_string())
                            .unwrap_or_else(|| "U".to_string());
                        ui.painter().text(
                            av_rect.center(),
                            Align2::CENTER_CENTER,
                            initial,
                            FontId::proportional(13.0),
                            theme.accent,
                        );
                        // Online dot
                        let dot_c = Pos2::new(av_rect.right() - 3.5, av_rect.bottom() - 3.5);
                        ui.painter().circle_filled(dot_c, 4.5, theme.bg_elevated);
                        ui.painter().circle_filled(dot_c, 3.0, theme.accent);

                        // Name + subtle status
                        let actions_reserve = 52.0;
                        let name_w = (ui.available_width() - actions_reserve).max(58.0);
                        ui.allocate_ui_with_layout(
                            Vec2::new(name_w, 30.0),
                            Layout::top_down(Align::LEFT),
                            |ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&profile.display_name)
                                            .strong()
                                            .size(12.5)
                                            .color(theme.text_primary),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                                ui.add(
                                    egui::Label::new(
                                        RichText::new("Connected")
                                            .size(10.5)
                                            .color(theme.text_muted),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                            },
                        );

                        // Compact vector Sync & Logout buttons
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if icon_button(
                                ui,
                                theme,
                                Icon::LogOut,
                                13.5,
                                24.0,
                                false,
                                true,
                                "Sign out",
                            )
                            .clicked()
                            {
                                commands.push(AppCommand::Logout);
                            }
                            if icon_button(
                                ui,
                                theme,
                                Icon::Refresh,
                                13.5,
                                24.0,
                                false,
                                true,
                                "Sync Spotify library",
                            )
                            .clicked()
                            {
                                commands.push(AppCommand::RefreshLibrary);
                            }
                        });
                    });
                }
                AuthUiState::OpeningBrowser { .. } => {
                    ui.label(
                        RichText::new("Opening browser…")
                            .size(11.5)
                            .color(theme.warning),
                    );
                    if pill_button(ui, theme, None, "Open Spotify Login", false, true).clicked() {
                        commands.push(AppCommand::OpenPendingLoginUrl);
                    }
                }
                AuthUiState::WaitingForAuthorization { .. } => {
                    ui.label(
                        RichText::new("Waiting for browser login…")
                            .size(11.5)
                            .color(theme.warning),
                    );
                    if pill_button(ui, theme, None, "Open Spotify Login", false, true).clicked() {
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
                    if full_width_accent_cta(ui, theme, "Login with Spotify") {
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
                        if pill_button(ui, theme, None, "Retry", true, true).clicked() {
                            commands.push(AppCommand::StartSpotifyLogin);
                        }
                        if authorize_url.is_some()
                            && pill_button(ui, theme, None, "Open", false, true).clicked()
                        {
                            commands.push(AppCommand::OpenPendingLoginUrl);
                        }
                    });
                }
                AuthUiState::SignedOut => {
                    if full_width_accent_cta(ui, theme, "Login with Spotify") {
                        commands.push(AppCommand::StartSpotifyLogin);
                    }
                }
            }
        });
}

fn full_width_accent_cta(ui: &mut Ui, theme: &ThemePalette, label: &str) -> bool {
    let w = ui.available_width();
    let h = 32.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(w, h), Sense::click());
    let hovered = response.hovered();
    let pressed = response.is_pointer_button_down_on();
    if hovered {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let fill = if pressed {
        theme.accent.linear_multiply(0.92)
    } else if hovered {
        theme.accent_hover
    } else {
        theme.accent
    };
    let draw_rect = if pressed { rect.shrink(0.5) } else { rect };
    let radius = CornerRadius::same(16);
    ui.painter().rect_filled(draw_rect, radius, fill);
    ui.painter().rect_stroke(
        draw_rect,
        radius,
        Stroke::new(1.0, Color32::from_white_alpha(20)),
        StrokeKind::Inside,
    );
    ui.painter().text(
        draw_rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(12.5),
        theme.on_accent(),
    );
    response.clicked()
}
