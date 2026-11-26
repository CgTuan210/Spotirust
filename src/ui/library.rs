//! Library views: Your Library, Liked Songs, Recently Played, Playlists, Albums, Artists,
//! Local Files, and Playlist / Album / Artist detail views with atmospheric hero headers.

use crate::core::events::AppCommand;
use crate::core::models::Route;
use crate::core::state::AppState;
use crate::ui::components::artwork::{
    ArtworkTextureCache, artwork_box_shadowed, deterministic_rgb, paint_header_glow,
};
use crate::ui::components::cards::{
    artist_card, empty_state, media_card, track_row, track_table_header,
};
use crate::ui::components::icons::{Icon, icon_button, pill_button};
use crate::ui::home::{responsive_card_width, section_header};
use crate::ui::theme::ThemePalette;
use egui::{
    Align, CornerRadius, Frame, Layout, Margin, Rect, RichText, ScrollArea, Stroke, Ui, Vec2,
};
use std::path::PathBuf;

pub fn render_library_overview(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    ScrollArea::vertical()
        .id_salt("lib_overview")
        .show(ui, |ui| {
            ui.add_space(4.0);
            section_header(
                ui,
                theme,
                "Your Library",
                Some("Saved playlists, albums, artists, and local audio files"),
            );

            let card_w = responsive_card_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(14.0);
                if media_card(
                    ui,
                    theme,
                    "Liked Songs",
                    &format!("{} saved tracks", state.liked_tracks.len()),
                    Some("Collection"),
                    None,
                    Some([99, 102, 241]),
                    card_w,
                    artwork,
                    commands,
                ) {
                    state.navigate(Route::LikedSongs);
                }
                if media_card(
                    ui,
                    theme,
                    "Local Files",
                    &format!("{} local tracks", state.local_tracks.len()),
                    Some("Rodio Native"),
                    None,
                    Some([16, 185, 129]),
                    card_w,
                    artwork,
                    commands,
                ) {
                    state.navigate(Route::LocalFiles);
                }
                let playlists = state.playlists.clone();
                for pl in &playlists {
                    if media_card(
                        ui,
                        theme,
                        &pl.name,
                        &format!("Playlist · {}", pl.owner_name),
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
            ui.add_space(24.0);
        });
}

#[allow(clippy::too_many_arguments)]
pub fn render_track_list_page(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    tracks: &[crate::core::models::Track],
    state: &AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    ScrollArea::vertical().id_salt(title).show(ui, |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                section_header(ui, theme, title, Some(subtitle));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(first) = tracks.first() {
                    if pill_button(ui, theme, Some(Icon::Play), "Play All", true, true).clicked() {
                        commands.push(AppCommand::PlayTrack {
                            track: first.clone(),
                            queue_context: tracks.to_vec(),
                        });
                    }
                    if pill_button(ui, theme, Some(Icon::Shuffle), "Shuffle", false, true).clicked()
                    {
                        commands.push(AppCommand::ShufflePlayTracks {
                            tracks: tracks.to_vec(),
                        });
                    }
                }
            });
        });

        ui.add_space(4.0);
        if tracks.is_empty() {
            empty_state(
                ui,
                theme,
                Icon::MusicNote,
                "No tracks here yet",
                "Songs you save or play will show up here automatically.",
            );
            return;
        }

        track_table_header(ui, theme);
        let active_id = state.playback.current_track.as_ref().map(|t| t.id.as_str());
        let playing = state.playback.status.is_playing();
        ui.spacing_mut().item_spacing.y = 2.0;
        for (idx, track) in tracks.iter().enumerate() {
            track_row(
                ui,
                theme,
                idx,
                track,
                active_id == Some(track.id.as_str()),
                playing,
                tracks,
                artwork,
                commands,
            );
        }
        ui.add_space(24.0);
    });
}

pub fn render_playlists_page(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    ScrollArea::vertical()
        .id_salt("playlists_grid")
        .show(ui, |ui| {
            ui.add_space(4.0);
            section_header(
                ui,
                theme,
                "Playlists",
                Some("Your owned and followed Spotify playlists"),
            );
            if state.playlists.is_empty() {
                empty_state(
                    ui,
                    theme,
                    Icon::MusicNote,
                    "No playlists loaded",
                    "Sign in with Spotify and click the sync icon in the sidebar to load your playlists.",
                );
                return;
            }
            let playlists = state.playlists.clone();
            let card_w = responsive_card_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(14.0);
                for pl in &playlists {
                    if media_card(
                        ui,
                        theme,
                        &pl.name,
                        &format!("By {} · {} tracks", pl.owner_name, pl.total_tracks),
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
            ui.add_space(24.0);
        });
}

pub fn render_albums_page(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    ScrollArea::vertical()
        .id_salt("albums_grid")
        .show(ui, |ui| {
            ui.add_space(4.0);
            section_header(
                ui,
                theme,
                "Albums",
                Some("Saved albums in your Spotify library"),
            );
            if state.albums.is_empty() {
                empty_state(
                    ui,
                    theme,
                    Icon::Disc,
                    "No saved albums yet",
                    "Albums you save on Spotify will appear here once synced.",
                );
                return;
            }
            let albums = state.albums.clone();
            let card_w = responsive_card_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(14.0);
                for alb in &albums {
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
            ui.add_space(24.0);
        });
}

pub fn render_artists_page(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    ScrollArea::vertical()
        .id_salt("artists_grid")
        .show(ui, |ui| {
            ui.add_space(4.0);
            section_header(ui, theme, "Artists", Some("Artists you follow on Spotify"));
            if state.artists.is_empty() {
                empty_state(
                    ui,
                    theme,
                    Icon::Artist,
                    "No followed artists yet",
                    "Follow artists on Spotify to browse their discographies here.",
                );
                return;
            }
            let artists = state.artists.clone();
            let card_w = responsive_card_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::splat(14.0);
                for artist in &artists {
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
                        if state.should_load_artist(&id) {
                            commands.push(AppCommand::LoadArtistAlbums { artist_id: id });
                        }
                    }
                }
            });
            ui.add_space(24.0);
        });
}

pub fn render_local_files_page(
    ui: &mut Ui,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    ScrollArea::vertical().id_salt("local_files_page").show(ui, |ui| {
        ui.add_space(4.0);
        section_header(
            ui,
            theme,
            "Local Music Player",
            Some("Native Rodio MP3, FLAC, OGG Vorbis, and WAV playback with embedded artwork & sidecar karaoke lyrics"),
        );

        // Primary native file/folder picker toolbar + library controls
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(8.0);
            if pill_button(
                ui,
                theme,
                Some(Icon::Folder),
                "Select Folder",
                true,
                !state.local_scan_in_progress,
            )
            .on_hover_text("Open native OS folder picker and scan music recursively")
            .clicked()
            {
                commands.push(AppCommand::PickLocalMusicFolder);
            }

            if pill_button(
                ui,
                theme,
                Some(Icon::Plus),
                "Add Files",
                false,
                !state.local_scan_in_progress,
            )
            .on_hover_text("Select one or more local MP3/FLAC/OGG/WAV files")
            .clicked()
            {
                commands.push(AppCommand::PickLocalMusicFiles);
            }

            if pill_button(
                ui,
                theme,
                Some(Icon::Refresh),
                "Rescan",
                false,
                !state.local_scan_in_progress && !state.settings.local_music_dirs.is_empty(),
            )
            .on_hover_text("Re-read metadata & prune deleted files across configured folders")
            .clicked()
            {
                commands.push(AppCommand::RescanLocalLibrary);
            }

            if !state.local_tracks.is_empty() {
                if pill_button(ui, theme, Some(Icon::Play), "Play All", false, true).clicked()
                    && let Some(first) = state.local_tracks.first()
                {
                    commands.push(AppCommand::PlayTrack {
                        track: first.clone(),
                        queue_context: state.local_tracks.clone(),
                    });
                }
                if pill_button(ui, theme, Some(Icon::Shuffle), "Shuffle", false, true).clicked() {
                    commands.push(AppCommand::ShufflePlayTracks {
                        tracks: state.local_tracks.clone(),
                    });
                }
            }

            if state.local_scan_in_progress {
                ui.spinner();
                ui.label(
                    RichText::new("Scanning audio files…")
                        .size(12.0)
                        .color(theme.accent),
                );
            }
        });

        ui.add_space(10.0);

        // Direct path input fallback
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.new_local_music_dir)
                    .hint_text("Or paste folder path directly: /path/to/your/Music")
                    .desired_width(340.0)
                    .margin(Margin::symmetric(12, 7)),
            );
            if pill_button(
                ui,
                theme,
                None,
                "Add Path",
                false,
                !state.local_scan_in_progress,
            )
            .clicked()
            {
                let p = PathBuf::from(state.new_local_music_dir.trim());
                if !p.as_os_str().is_empty() {
                    if !state.settings.local_music_dirs.contains(&p) {
                        state.settings.local_music_dirs.push(p.clone());
                        commands.push(AppCommand::SaveSettings(state.settings.clone()));
                    }
                    state.new_local_music_dir.clear();
                    commands.push(AppCommand::ScanLocalMusicDir { path: p });
                }
            }
        });

        // Configured folders chips with Remove action
        if !state.settings.local_music_dirs.is_empty() {
            ui.add_space(8.0);
            let mut dir_to_remove: Option<PathBuf> = None;
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Indexed folders:")
                        .size(11.5)
                        .color(theme.text_muted),
                );
                for dir in &state.settings.local_music_dirs {
                    Frame::new()
                        .fill(theme.bg_elevated)
                        .corner_radius(CornerRadius::same(14))
                        .stroke(Stroke::new(1.0, theme.hairline()))
                        .inner_margin(Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 6.0;
                                ui.label(
                                    RichText::new(dir.to_string_lossy().as_ref())
                                        .size(11.5)
                                        .color(theme.text_secondary),
                                );
                                if icon_button(
                                    ui,
                                    theme,
                                    Icon::Close,
                                    11.0,
                                    18.0,
                                    false,
                                    true,
                                    "Remove folder",
                                )
                                .clicked()
                                {
                                    dir_to_remove = Some(dir.clone());
                                }
                            });
                        });
                }
            });
            if let Some(dir) = dir_to_remove {
                state.settings.local_music_dirs.retain(|d| d != &dir);
                commands.push(AppCommand::SaveSettings(state.settings.clone()));
                commands.push(AppCommand::RemoveLocalMusicDir { path: dir });
            }
        }

        ui.add_space(18.0);
        let tracks = state.local_tracks.clone();
        let active_id = state.playback.current_track.as_ref().map(|t| t.id.as_str());
        let playing = state.playback.status.is_playing();

        if tracks.is_empty() {
            empty_state(
                ui,
                theme,
                Icon::Folder,
                "No local tracks indexed yet",
                "Click Select Folder or Add Files above — your library persists in SQLite for instant startup.",
            );
        } else {
            section_header(
                ui,
                theme,
                &format!("Local Library ({} tracks)", tracks.len()),
                Some("Loaded directly from SQLite cache (use Rescan after editing files)"),
            );
            track_table_header(ui, theme);
            ui.spacing_mut().item_spacing.y = 2.0;
            for (idx, track) in tracks.iter().enumerate() {
                track_row(
                    ui,
                    theme,
                    idx,
                    track,
                    active_id == Some(track.id.as_str()),
                    playing,
                    &tracks,
                    artwork,
                    commands,
                );
            }
            ui.spacing_mut().item_spacing.y = 8.0;
        }

        ui.add_space(24.0);
        section_header(
            ui,
            theme,
            "Built-in Soundcheck",
            Some("Royalty-free synthesized WAV audio played natively via Rodio with bundled TTML/LRC karaoke timing"),
        );
        track_table_header(ui, theme);
        let showcase = state.showcase_tracks.clone();
        ui.spacing_mut().item_spacing.y = 2.0;
        for (idx, track) in showcase.iter().enumerate() {
            track_row(
                ui,
                theme,
                idx,
                track,
                active_id == Some(track.id.as_str()),
                playing,
                &showcase,
                artwork,
                commands,
            );
        }
        ui.add_space(24.0);
    });
}

pub fn render_playlist_detail(
    ui: &mut Ui,
    playlist_id: &str,
    state: &AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    use crate::core::state::LoadState;

    let pl = state.playlists.iter().find(|p| p.id == playlist_id);
    let load_state = state.playlist_tracks.get(playlist_id);
    let loaded_tracks: Option<&Vec<crate::core::models::Track>> = match load_state {
        Some(LoadState::Loaded(t)) => Some(t),
        _ => None,
    };

    let art_url = pl.and_then(|p| p.artwork_url.as_deref());
    let title = pl.map(|p| p.name.as_str()).unwrap_or("Playlist");
    let glow_rgb = art_url
        .and_then(|u| artwork.dominant_color(u))
        .unwrap_or_else(|| deterministic_rgb(title));

    let panel_rect = ui.available_rect_before_wrap();
    paint_header_glow(
        ui,
        Rect::from_min_size(
            panel_rect.min - Vec2::new(28.0, 12.0),
            Vec2::new(panel_rect.width() + 56.0, 230.0),
        ),
        glow_rgb,
        theme.bg_base,
        theme.dark,
    );

    ScrollArea::vertical().id_salt(("pl_detail", playlist_id)).show(ui, |ui| {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            artwork_box_shadowed(
                ui,
                148.0,
                12,
                theme.shadow_floating(),
                art_url,
                title,
                None,
                artwork,
                commands,
            );
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(RichText::new("PLAYLIST").size(10.5).strong().color(theme.accent));
                ui.label(RichText::new(title).size(28.0).strong().color(theme.text_primary));
                if let Some(p) = pl {
                    if !p.description.is_empty() {
                        ui.label(RichText::new(&p.description).size(12.5).color(theme.text_secondary));
                    }
                    ui.label(
                        RichText::new(format!("By {} · {} tracks", p.owner_name, p.total_tracks))
                            .size(12.5)
                            .color(theme.text_muted),
                    );
                }
                ui.add_space(8.0);
                if let Some(tracks) = loaded_tracks
                    && let Some(first) = tracks.first()
                {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if pill_button(ui, theme, Some(Icon::Play), "Play", true, true).clicked() {
                            commands.push(AppCommand::PlayTrack {
                                track: first.clone(),
                                queue_context: tracks.clone(),
                            });
                        }
                        if pill_button(ui, theme, Some(Icon::Shuffle), "Shuffle", false, true)
                            .clicked()
                        {
                            commands.push(AppCommand::ShufflePlayTracks {
                                tracks: tracks.clone(),
                            });
                        }
                    });
                }
            });
        });

        ui.add_space(18.0);

        match load_state {
            None | Some(LoadState::Loading) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Loading playlist tracks…")
                            .size(13.0)
                            .color(theme.text_secondary),
                    );
                });
            }
            Some(LoadState::Failed(err)) => {
                empty_state(
                    ui,
                    theme,
                    Icon::Alert,
                    "Could not load playlist tracks",
                    err,
                );
                ui.add_space(8.0);
                if pill_button(ui, theme, Some(Icon::Refresh), "Retry", false, true).clicked() {
                    commands.push(AppCommand::LoadPlaylistTracks {
                        playlist_id: playlist_id.to_string(),
                    });
                }
            }
            Some(LoadState::Loaded(tracks)) if tracks.is_empty() => {
                empty_state(
                    ui,
                    theme,
                    Icon::MusicNote,
                    "No accessible tracks",
                    "Under Spotify's Feb 2026 Development Mode rules, only playlists owned or collaborated on by the signed-in user expose full item lists.",
                );
                ui.add_space(8.0);
                if pill_button(ui, theme, Some(Icon::Refresh), "Refresh", false, true).clicked() {
                    commands.push(AppCommand::LoadPlaylistTracks {
                        playlist_id: playlist_id.to_string(),
                    });
                }
            }
            Some(LoadState::Loaded(tracks)) => {
                track_table_header(ui, theme);
                let active_id = state.playback.current_track.as_ref().map(|t| t.id.as_str());
                let playing = state.playback.status.is_playing();
                ui.spacing_mut().item_spacing.y = 2.0;
                for (idx, track) in tracks.iter().enumerate() {
                    track_row(
                        ui,
                        theme,
                        idx,
                        track,
                        active_id == Some(track.id.as_str()),
                        playing,
                        tracks,
                        artwork,
                        commands,
                    );
                }
            }
        }
        ui.add_space(24.0);
    });
}

pub fn render_album_detail(
    ui: &mut Ui,
    album_id: &str,
    state: &AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    use crate::core::state::LoadState;

    let album = state.albums.iter().find(|a| a.id == album_id);
    let load_state = state.album_tracks.get(album_id);
    let loaded_tracks: Option<&Vec<crate::core::models::Track>> = match load_state {
        Some(LoadState::Loaded(t)) => Some(t),
        _ => None,
    };

    let title = album.map(|a| a.name.as_str()).unwrap_or("Album");
    let art_url = album.and_then(|a| a.artwork_url.as_deref());
    let glow_rgb = art_url
        .and_then(|u| artwork.dominant_color(u))
        .or_else(|| album.and_then(|a| a.accent_rgb))
        .unwrap_or_else(|| deterministic_rgb(title));

    let panel_rect = ui.available_rect_before_wrap();
    paint_header_glow(
        ui,
        Rect::from_min_size(
            panel_rect.min - Vec2::new(28.0, 12.0),
            Vec2::new(panel_rect.width() + 56.0, 230.0),
        ),
        glow_rgb,
        theme.bg_base,
        theme.dark,
    );

    ScrollArea::vertical()
        .id_salt(("alb_detail", album_id))
        .show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                artwork_box_shadowed(
                    ui,
                    148.0,
                    12,
                    theme.shadow_floating(),
                    art_url,
                    title,
                    album.and_then(|a| a.accent_rgb),
                    artwork,
                    commands,
                );
                ui.add_space(18.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(
                        RichText::new("ALBUM")
                            .size(10.5)
                            .strong()
                            .color(theme.accent),
                    );
                    ui.label(
                        RichText::new(title)
                            .size(28.0)
                            .strong()
                            .color(theme.text_primary),
                    );
                    if let Some(a) = album {
                        let meta = match a.release_date.as_deref() {
                            Some(date) if !date.is_empty() => {
                                format!("{} · {date}", a.artist_display())
                            }
                            _ => a.artist_display(),
                        };
                        ui.label(RichText::new(meta).size(13.0).color(theme.text_secondary));
                    }
                    ui.add_space(8.0);
                    if let Some(tracks) = loaded_tracks
                        && let Some(first) = tracks.first()
                    {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            if pill_button(ui, theme, Some(Icon::Play), "Play Album", true, true)
                                .clicked()
                            {
                                commands.push(AppCommand::PlayTrack {
                                    track: first.clone(),
                                    queue_context: tracks.clone(),
                                });
                            }
                            if pill_button(ui, theme, Some(Icon::Shuffle), "Shuffle", false, true)
                                .clicked()
                            {
                                commands.push(AppCommand::ShufflePlayTracks {
                                    tracks: tracks.clone(),
                                });
                            }
                        });
                    }
                });
            });

            ui.add_space(18.0);

            match load_state {
                None | Some(LoadState::Loading) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Loading album tracks…")
                                .size(13.0)
                                .color(theme.text_secondary),
                        );
                    });
                }
                Some(LoadState::Failed(err)) => {
                    empty_state(ui, theme, Icon::Alert, "Could not load album tracks", err);
                    ui.add_space(8.0);
                    if pill_button(ui, theme, Some(Icon::Refresh), "Retry", false, true).clicked() {
                        commands.push(AppCommand::LoadAlbumTracks {
                            album_id: album_id.to_string(),
                        });
                    }
                }
                Some(LoadState::Loaded(tracks)) if tracks.is_empty() => {
                    empty_state(
                        ui,
                        theme,
                        Icon::Disc,
                        "No tracks available",
                        "This album did not return any playable tracks.",
                    );
                }
                Some(LoadState::Loaded(tracks)) => {
                    track_table_header(ui, theme);
                    let active_id = state.playback.current_track.as_ref().map(|t| t.id.as_str());
                    let playing = state.playback.status.is_playing();
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (idx, track) in tracks.iter().enumerate() {
                        track_row(
                            ui,
                            theme,
                            idx,
                            track,
                            active_id == Some(track.id.as_str()),
                            playing,
                            tracks,
                            artwork,
                            commands,
                        );
                    }
                }
            }
            ui.add_space(24.0);
        });
}

pub fn render_artist_detail(
    ui: &mut Ui,
    artist_id: &str,
    state: &mut AppState,
    theme: &ThemePalette,
    artwork: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    use crate::core::state::LoadState;

    let artist = state.artists.iter().find(|a| a.id == artist_id).cloned();
    let load_state = state.artist_albums.get(artist_id).cloned();
    let name = artist.as_ref().map(|a| a.name.as_str()).unwrap_or("Artist");
    let img = artist.as_ref().and_then(|a| a.image_url.as_deref());
    let glow_rgb = img
        .and_then(|u| artwork.dominant_color(u))
        .unwrap_or_else(|| deterministic_rgb(name));

    let panel_rect = ui.available_rect_before_wrap();
    paint_header_glow(
        ui,
        Rect::from_min_size(
            panel_rect.min - Vec2::new(28.0, 12.0),
            Vec2::new(panel_rect.width() + 56.0, 220.0),
        ),
        glow_rgb,
        theme.bg_base,
        theme.dark,
    );

    ScrollArea::vertical()
        .id_salt(("art_detail", artist_id))
        .show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                // True GPU-clipped circular portrait with soft elevation shadow
                artwork_box_shadowed(
                    ui,
                    136.0,
                    68,
                    theme.shadow_floating(),
                    img,
                    name,
                    None,
                    artwork,
                    commands,
                );
                ui.add_space(18.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(
                        RichText::new("ARTIST")
                            .size(10.5)
                            .strong()
                            .color(theme.accent),
                    );
                    ui.label(
                        RichText::new(name)
                            .size(30.0)
                            .strong()
                            .color(theme.text_primary),
                    );
                    if let Some(a) = &artist
                        && !a.genres.is_empty()
                    {
                        ui.label(
                            RichText::new(a.genres.join(" · "))
                                .size(13.0)
                                .color(theme.text_secondary),
                        );
                    }
                });
            });

            ui.add_space(22.0);
            section_header(ui, theme, "Discography", None);

            match load_state {
                None | Some(LoadState::Loading) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Loading artist albums…")
                                .size(13.0)
                                .color(theme.text_secondary),
                        );
                    });
                }
                Some(LoadState::Failed(err)) => {
                    empty_state(
                        ui,
                        theme,
                        Icon::Alert,
                        "Could not load artist discography",
                        &err,
                    );
                    ui.add_space(8.0);
                    if pill_button(ui, theme, Some(Icon::Refresh), "Retry", false, true).clicked() {
                        commands.push(AppCommand::LoadArtistAlbums {
                            artist_id: artist_id.to_string(),
                        });
                    }
                }
                Some(LoadState::Loaded(albums)) if albums.is_empty() => {
                    empty_state(
                        ui,
                        theme,
                        Icon::Disc,
                        "No albums returned",
                        "No discography releases were returned for this artist.",
                    );
                }
                Some(LoadState::Loaded(albums)) => {
                    let card_w = responsive_card_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::splat(14.0);
                        for alb in &albums {
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
                }
            }
            ui.add_space(24.0);
        });
}
