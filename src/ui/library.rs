//! Library views: Your Library, Liked Songs, Recently Played, Playlists, Albums, Artists,
//! Local Files, and Playlist / Album / Artist detail views.

use crate::core::events::AppCommand;
use crate::core::models::Route;
use crate::core::state::AppState;
use crate::ui::components::artwork::{ArtworkTextureCache, artwork_box};
use crate::ui::components::cards::{media_card, track_row};
use crate::ui::home::{responsive_card_width, section_header};
use crate::ui::theme::ThemePalette;
use egui::{Color32, CornerRadius, Margin, RichText, ScrollArea, Ui};
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
            section_header(
                ui,
                theme,
                "Your Library",
                Some("Saved playlists, albums, artists, and local audio files"),
            );

            let card_w = responsive_card_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
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
                    &format!("{} local audio tracks", state.local_tracks.len()),
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
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                section_header(ui, theme, title, Some(subtitle));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(first) = tracks.first() {
                    let play_btn = egui::Button::new(
                        RichText::new("▶  Play All").strong().color(Color32::BLACK),
                    )
                    .fill(theme.accent)
                    .corner_radius(CornerRadius::same(18));
                    if ui.add(play_btn).clicked() {
                        commands.push(AppCommand::PlayTrack {
                            track: first.clone(),
                            queue_context: tracks.to_vec(),
                        });
                    }
                }
            });
        });

        ui.add_space(6.0);
        if tracks.is_empty() {
            ui.label(
                RichText::new("No tracks here yet.")
                    .size(13.0)
                    .color(theme.text_muted),
            );
            return;
        }

        let active_id = state.playback.current_track.as_ref().map(|t| t.id.as_str());
        let playing = state.playback.status.is_playing();
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
            section_header(
                ui,
                theme,
                "Playlists",
                Some("Your owned and followed Spotify playlists"),
            );
            if state.playlists.is_empty() {
                ui.label(
                    RichText::new("No playlists loaded. Sign in with Spotify and click Sync.")
                        .color(theme.text_muted),
                );
                return;
            }
            let playlists = state.playlists.clone();
            let card_w = responsive_card_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
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
            section_header(
                ui,
                theme,
                "Albums",
                Some("Saved albums in your Spotify library"),
            );
            if state.albums.is_empty() {
                ui.label(RichText::new("No saved albums loaded yet.").color(theme.text_muted));
                return;
            }
            let albums = state.albums.clone();
            let card_w = responsive_card_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
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
            section_header(ui, theme, "Artists", Some("Artists you follow on Spotify"));
            if state.artists.is_empty() {
                ui.label(RichText::new("No followed artists loaded yet.").color(theme.text_muted));
                return;
            }
            let artists = state.artists.clone();
            let card_w = responsive_card_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
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
                        if state.should_load_artist(&id) {
                            commands.push(AppCommand::LoadArtistAlbums { artist_id: id });
                        }
                    }
                }
            });
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
        section_header(
            ui,
            theme,
            "Local Music Player (Rodio Native)",
            Some("Native MP3, FLAC, OGG Vorbis, and WAV playback with lofty metadata, embedded artwork, SQLite persistence, and sidecar .ttml/.lrc lyrics"),
        );

        // Primary native file/folder picker toolbar + library controls
        ui.horizontal_wrapped(|ui| {
            let folder_btn = egui::Button::new(
                RichText::new("📁 Select Folder")
                    .strong()
                    .color(Color32::BLACK),
            )
            .fill(theme.accent)
            .corner_radius(CornerRadius::same(16));
            if ui
                .add_enabled(!state.local_scan_in_progress, folder_btn)
                .on_hover_text("Open native OS folder picker and scan music recursively")
                .clicked()
            {
                commands.push(AppCommand::PickLocalMusicFolder);
            }

            let files_btn = egui::Button::new(
                RichText::new("🎵 Add Files").color(theme.text_primary),
            )
            .fill(theme.bg_elevated)
            .corner_radius(CornerRadius::same(16));
            if ui
                .add_enabled(!state.local_scan_in_progress, files_btn)
                .on_hover_text("Select one or more local MP3/FLAC/OGG/WAV files")
                .clicked()
            {
                commands.push(AppCommand::PickLocalMusicFiles);
            }

            let rescan_btn = egui::Button::new(
                RichText::new("↻ Rescan").color(theme.text_primary),
            )
            .fill(theme.bg_elevated)
            .corner_radius(CornerRadius::same(16));
            if ui
                .add_enabled(
                    !state.local_scan_in_progress && !state.settings.local_music_dirs.is_empty(),
                    rescan_btn,
                )
                .on_hover_text("Re-read metadata & prune deleted files across configured folders")
                .clicked()
            {
                commands.push(AppCommand::RescanLocalLibrary);
            }

            if !state.local_tracks.is_empty() {
                if ui.button("▶ Play All").clicked()
                    && let Some(first) = state.local_tracks.first()
                {
                    commands.push(AppCommand::PlayTrack {
                        track: first.clone(),
                        queue_context: state.local_tracks.clone(),
                    });
                }
                if ui.button("🔀 Shuffle Play").clicked() {
                    commands.push(AppCommand::ShufflePlayTracks {
                        tracks: state.local_tracks.clone(),
                    });
                }
            }

            if state.local_scan_in_progress {
                ui.spinner();
                ui.label(
                    RichText::new("Scanning local audio files in background…")
                        .size(12.0)
                        .color(theme.accent),
                );
            }
        });

        ui.add_space(6.0);

        // Optional direct path input fallback (useful on minimal WMs without xdg-desktop-portal)
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut state.new_local_music_dir)
                    .hint_text("Or paste folder path directly: /path/to/your/Music")
                    .desired_width(340.0)
                    .margin(Margin::symmetric(10, 6)),
            );
            if ui
                .add_enabled(!state.local_scan_in_progress, egui::Button::new("Add Path"))
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
            ui.add_space(6.0);
            let mut dir_to_remove: Option<PathBuf> = None;
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Indexed folders:")
                        .size(11.5)
                        .color(theme.text_muted),
                );
                for dir in &state.settings.local_music_dirs {
                    egui::Frame::new()
                        .fill(theme.bg_elevated)
                        .corner_radius(CornerRadius::same(12))
                        .inner_margin(Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(dir.to_string_lossy().as_ref())
                                        .size(11.0)
                                        .color(theme.text_secondary),
                                );
                                if ui.small_button("✕").on_hover_text("Remove folder").clicked() {
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

        ui.add_space(12.0);
        let tracks = state.local_tracks.clone();
        let active_id = state.playback.current_track.as_ref().map(|t| t.id.as_str());
        let playing = state.playback.status.is_playing();

        if tracks.is_empty() {
            ui.label(
                RichText::new("No local tracks indexed yet. Click Select Folder or Add Files above — your library is saved in SQLite for instant startup.")
                    .size(13.0)
                    .color(theme.text_muted),
            );
        } else {
            section_header(
                ui,
                theme,
                &format!("Local Library ({} tracks)", tracks.len()),
                Some("Loaded directly from SQLite cache (use Rescan after adding or editing files)"),
            );
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
        }

        ui.add_space(16.0);
        section_header(
            ui,
            theme,
            "Built-in Soundcheck (Real Local WAV · Rodio)",
            Some("Royalty-free synthesized WAV audio files stored in Spotirust's local cache with bundled TTML/LRC karaoke timing"),
        );
        let showcase = state.showcase_tracks.clone();
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

    ScrollArea::vertical().id_salt(("pl_detail", playlist_id)).show(ui, |ui| {
        ui.horizontal(|ui| {
            let art_url = pl.and_then(|p| p.artwork_url.as_deref());
            let title = pl.map(|p| p.name.as_str()).unwrap_or("Playlist");
            artwork_box(ui, 136.0, 10, art_url, title, None, artwork, commands);
            ui.add_space(14.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("PLAYLIST").size(11.0).strong().color(theme.accent));
                ui.label(RichText::new(title).size(26.0).strong().color(theme.text_primary));
                if let Some(p) = pl {
                    if !p.description.is_empty() {
                        ui.label(RichText::new(&p.description).size(12.5).color(theme.text_secondary));
                    }
                    ui.label(
                        RichText::new(format!("By {} · {} tracks", p.owner_name, p.total_tracks))
                            .size(12.0)
                            .color(theme.text_muted),
                    );
                }
                ui.add_space(8.0);
                if let Some(tracks) = loaded_tracks
                    && let Some(first) = tracks.first()
                {
                    let btn = egui::Button::new(
                        RichText::new("▶  Play on Connect").strong().color(Color32::BLACK),
                    )
                    .fill(theme.accent)
                    .corner_radius(CornerRadius::same(18));
                    if ui.add(btn).clicked() {
                        commands.push(AppCommand::PlayTrack {
                            track: first.clone(),
                            queue_context: tracks.clone(),
                        });
                    }
                }
            });
        });

        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);

        match load_state {
            None | Some(LoadState::Loading) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Loading playlist items…")
                            .size(13.0)
                            .color(theme.text_secondary),
                    );
                });
            }
            Some(LoadState::Failed(err)) => {
                ui.label(
                    RichText::new(format!("Could not load playlist tracks: {err}"))
                        .size(13.0)
                        .color(theme.danger),
                );
                ui.add_space(6.0);
                if ui.button("Retry").clicked() {
                    commands.push(AppCommand::LoadPlaylistTracks {
                        playlist_id: playlist_id.to_string(),
                    });
                }
            }
            Some(LoadState::Loaded(tracks)) if tracks.is_empty() => {
                ui.label(
                    RichText::new(
                        "This playlist returned 0 accessible tracks. Under Spotify's Feb 2026 Development Mode rules, \
                         only playlists owned or collaborated on by the signed-in user expose full item lists.",
                    )
                    .size(12.5)
                    .color(theme.text_muted),
                );
                ui.add_space(6.0);
                if ui.button("↻ Refresh").clicked() {
                    commands.push(AppCommand::LoadPlaylistTracks {
                        playlist_id: playlist_id.to_string(),
                    });
                }
            }
            Some(LoadState::Loaded(tracks)) => {
                let active_id = state.playback.current_track.as_ref().map(|t| t.id.as_str());
                let playing = state.playback.status.is_playing();
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

    ScrollArea::vertical()
        .id_salt(("alb_detail", album_id))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let title = album.map(|a| a.name.as_str()).unwrap_or("Album");
                let art_url = album.and_then(|a| a.artwork_url.as_deref());
                artwork_box(ui, 136.0, 10, art_url, title, None, artwork, commands);
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("ALBUM")
                            .size(11.0)
                            .strong()
                            .color(theme.accent),
                    );
                    ui.label(
                        RichText::new(title)
                            .size(26.0)
                            .strong()
                            .color(theme.text_primary),
                    );
                    if let Some(a) = album {
                        ui.label(
                            RichText::new(format!(
                                "{} · {}",
                                a.artist_display(),
                                a.release_date.as_deref().unwrap_or("")
                            ))
                            .size(13.0)
                            .color(theme.text_secondary),
                        );
                    }
                    ui.add_space(8.0);
                    if let Some(tracks) = loaded_tracks
                        && let Some(first) = tracks.first()
                    {
                        let btn = egui::Button::new(
                            RichText::new("▶  Play Album")
                                .strong()
                                .color(Color32::BLACK),
                        )
                        .fill(theme.accent)
                        .corner_radius(CornerRadius::same(18));
                        if ui.add(btn).clicked() {
                            commands.push(AppCommand::PlayTrack {
                                track: first.clone(),
                                queue_context: tracks.clone(),
                            });
                        }
                    }
                });
            });

            ui.add_space(14.0);
            ui.separator();
            ui.add_space(8.0);

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
                    ui.label(
                        RichText::new(format!("Could not load album tracks: {err}"))
                            .size(13.0)
                            .color(theme.danger),
                    );
                    ui.add_space(6.0);
                    if ui.button("Retry").clicked() {
                        commands.push(AppCommand::LoadAlbumTracks {
                            album_id: album_id.to_string(),
                        });
                    }
                }
                Some(LoadState::Loaded(tracks)) if tracks.is_empty() => {
                    ui.label(
                        RichText::new("No tracks available for this album.")
                            .size(13.0)
                            .color(theme.text_muted),
                    );
                }
                Some(LoadState::Loaded(tracks)) => {
                    let active_id = state.playback.current_track.as_ref().map(|t| t.id.as_str());
                    let playing = state.playback.status.is_playing();
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

    ScrollArea::vertical()
        .id_salt(("art_detail", artist_id))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let name = artist.as_ref().map(|a| a.name.as_str()).unwrap_or("Artist");
                let img = artist.as_ref().and_then(|a| a.image_url.as_deref());
                artwork_box(ui, 128.0, 64, img, name, None, artwork, commands);
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("ARTIST")
                            .size(11.0)
                            .strong()
                            .color(theme.accent),
                    );
                    ui.label(
                        RichText::new(name)
                            .size(28.0)
                            .strong()
                            .color(theme.text_primary),
                    );
                    if let Some(a) = &artist
                        && !a.genres.is_empty()
                    {
                        ui.label(
                            RichText::new(a.genres.join(" · "))
                                .size(12.5)
                                .color(theme.text_secondary),
                        );
                    }
                });
            });

            ui.add_space(16.0);
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
                    ui.label(
                        RichText::new(format!("Could not load artist discography: {err}"))
                            .size(13.0)
                            .color(theme.danger),
                    );
                    ui.add_space(6.0);
                    if ui.button("Retry").clicked() {
                        commands.push(AppCommand::LoadArtistAlbums {
                            artist_id: artist_id.to_string(),
                        });
                    }
                }
                Some(LoadState::Loaded(albums)) if albums.is_empty() => {
                    ui.label(
                        RichText::new("No albums returned for this artist.")
                            .size(13.0)
                            .color(theme.text_muted),
                    );
                }
                Some(LoadState::Loaded(albums)) => {
                    let card_w = responsive_card_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
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
        });
}
