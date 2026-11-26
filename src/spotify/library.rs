//! User profile, library (saved tracks/albums, followed artists), recently played, top items,
//! and Feb 2026 unified `/me/library` mutations.

use crate::core::models::{Album, Artist, ConnectDevice, Track, UserProfile};
use crate::spotify::client::{
    ApiAlbum, ApiArtistFull, ApiDevice, ApiPaging, ApiTrack, ApiUserProfile, SpotifyApiError,
    SpotifyClient,
};
use reqwest::Method;
use serde::Deserialize;

#[derive(Deserialize)]
struct ApiSavedTrackItem {
    track: ApiTrack,
}

#[derive(Deserialize)]
struct ApiSavedAlbumItem {
    album: ApiAlbum,
}

#[derive(Deserialize)]
struct ApiFollowedArtistsEnvelope {
    artists: ApiPaging<ApiArtistFull>,
}

#[derive(Deserialize)]
struct ApiRecentlyPlayedItem {
    track: ApiTrack,
}

#[derive(Deserialize)]
struct ApiRecentlyPlayedResponse {
    #[serde(default)]
    items: Vec<ApiRecentlyPlayedItem>,
}

#[derive(Deserialize)]
struct ApiDevicesResponse {
    #[serde(default)]
    devices: Vec<ApiDevice>,
}

/// Spotify user library and catalog metadata operations.
#[derive(Clone)]
pub struct LibraryService {
    client: SpotifyClient,
}

impl LibraryService {
    pub fn new(client: SpotifyClient) -> Self {
        Self { client }
    }

    /// Fetch the authenticated user's profile (`GET /me`).
    pub async fn current_user_profile(&self) -> Result<UserProfile, SpotifyApiError> {
        let dto: ApiUserProfile = self.client.get_json("/me", &[]).await?;
        Ok(UserProfile::from(dto))
    }

    /// Fetch saved ("Liked") tracks (`GET /me/tracks`).
    pub async fn saved_tracks(&self, limit: u8) -> Result<Vec<Track>, SpotifyApiError> {
        let query = vec![("limit", limit.clamp(1, 50).to_string())];
        let page: ApiPaging<ApiSavedTrackItem> = self.client.get_json("/me/tracks", &query).await?;
        Ok(page
            .items
            .into_iter()
            .map(|item| item.track.into_track(None, true))
            .collect())
    }

    /// Fetch saved albums (`GET /me/albums`).
    pub async fn saved_albums(&self, limit: u8) -> Result<Vec<Album>, SpotifyApiError> {
        let query = vec![("limit", limit.clamp(1, 50).to_string())];
        let page: ApiPaging<ApiSavedAlbumItem> = self.client.get_json("/me/albums", &query).await?;
        Ok(page
            .items
            .into_iter()
            .map(|item| Album::from(item.album))
            .collect())
    }

    /// Fetch followed artists (`GET /me/following?type=artist`).
    pub async fn followed_artists(&self, limit: u8) -> Result<Vec<Artist>, SpotifyApiError> {
        let query = vec![
            ("type", "artist".to_string()),
            ("limit", limit.clamp(1, 50).to_string()),
        ];
        let env: ApiFollowedArtistsEnvelope = self.client.get_json("/me/following", &query).await?;
        Ok(env.artists.items.into_iter().map(Artist::from).collect())
    }

    /// Fetch recently played tracks (`GET /me/player/recently-played`).
    pub async fn recently_played(&self, limit: u8) -> Result<Vec<Track>, SpotifyApiError> {
        let query = vec![("limit", limit.clamp(1, 50).to_string())];
        let resp: ApiRecentlyPlayedResponse = self
            .client
            .get_json("/me/player/recently-played", &query)
            .await?;
        let mut seen = std::collections::HashSet::new();
        let mut tracks = Vec::new();
        for item in resp.items {
            let track = item.track.into_track(None, false);
            if seen.insert(track.id.clone()) {
                tracks.push(track);
            }
        }
        Ok(tracks)
    }

    /// Fetch user's top tracks (`GET /me/top/tracks`).
    pub async fn top_tracks(&self, limit: u8) -> Result<Vec<Track>, SpotifyApiError> {
        let query = vec![
            ("limit", limit.clamp(1, 50).to_string()),
            ("time_range", "short_term".to_string()),
        ];
        let page: ApiPaging<ApiTrack> = self.client.get_json("/me/top/tracks", &query).await?;
        Ok(page
            .items
            .into_iter()
            .map(|t| t.into_track(None, false))
            .collect())
    }

    /// Fetch tracks for a single album (`GET /albums/{id}` + `GET /albums/{id}/tracks`).
    pub async fn album_tracks(&self, album_id: &str) -> Result<Vec<Track>, SpotifyApiError> {
        let album_dto: ApiAlbum = self
            .client
            .get_json(&format!("/albums/{album_id}"), &[])
            .await?;
        let album = Album::from(album_dto);
        let query = vec![("limit", "50".to_string())];
        let page: ApiPaging<ApiTrack> = self
            .client
            .get_json(&format!("/albums/{album_id}/tracks"), &query)
            .await?;
        Ok(page
            .items
            .into_iter()
            .map(|t| t.into_track(Some(&album), false))
            .collect())
    }

    /// Fetch albums by an artist (`GET /artists/{id}/albums`).
    pub async fn artist_albums(&self, artist_id: &str) -> Result<Vec<Album>, SpotifyApiError> {
        let query = vec![("limit", "20".to_string())];
        let page: ApiPaging<ApiAlbum> = self
            .client
            .get_json(&format!("/artists/{artist_id}/albums"), &query)
            .await?;
        Ok(page.items.into_iter().map(Album::from).collect())
    }

    /// Save or remove a Spotify URI (`spotify:track:...`) in the user's library using
    /// Spotify's Feb 2026 unified `/me/library` endpoint.
    pub async fn set_library_saved(&self, uri: &str, saved: bool) -> Result<(), SpotifyApiError> {
        let method = if saved { Method::PUT } else { Method::DELETE };
        let query = vec![("uris", uri.to_string())];
        self.client
            .send_empty(method, "/me/library", &query, None)
            .await
    }

    /// Check whether Spotify URIs are saved in the user's library (`GET /me/library/contains`).
    pub async fn check_library_contains(
        &self,
        uris: &[String],
    ) -> Result<Vec<bool>, SpotifyApiError> {
        if uris.is_empty() {
            return Ok(Vec::new());
        }
        let query = vec![("uris", uris.join(","))];
        self.client.get_json("/me/library/contains", &query).await
    }

    /// List available Spotify Connect devices (`GET /me/player/devices`).
    pub async fn available_devices(&self) -> Result<Vec<ConnectDevice>, SpotifyApiError> {
        let resp: ApiDevicesResponse = self.client.get_json("/me/player/devices", &[]).await?;
        Ok(resp.devices.into_iter().map(ConnectDevice::from).collect())
    }

    /// Fetch the user's currently playing track (`GET /me/player/currently-playing`).
    /// Returns `Ok(None)` when Spotify responds with `204 No Content` or no active item.
    pub async fn currently_playing_track(&self) -> Result<Option<Track>, SpotifyApiError> {
        #[derive(Deserialize)]
        struct ApiCurrentlyPlaying {
            #[serde(default)]
            item: Option<ApiTrack>,
        }

        let resp: Option<ApiCurrentlyPlaying> = self
            .client
            .get_optional_json("/me/player/currently-playing", &[])
            .await?;
        Ok(resp.and_then(|r| r.item).map(|t| t.into_track(None, false)))
    }
}
