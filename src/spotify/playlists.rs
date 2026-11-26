//! Playlist listing and item retrieval compatible with Spotify's Feb 2026 `/playlists/{id}/items`
//! migration as well as legacy `/playlists/{id}/tracks` Extended Quota responses.

use crate::core::models::{Playlist, Track};
use crate::spotify::client::{ApiPaging, ApiPlaylist, ApiTrack, SpotifyApiError, SpotifyClient};
use serde::Deserialize;

#[derive(Deserialize)]
struct ApiPlaylistItemEntry {
    /// Feb 2026 uses `item`, legacy Extended Quota uses `track`.
    #[serde(default, alias = "track")]
    item: Option<ApiTrack>,
}

/// Playlist operations on Spotify Web API.
#[derive(Clone)]
pub struct PlaylistService {
    client: SpotifyClient,
}

impl PlaylistService {
    pub fn new(client: SpotifyClient) -> Self {
        Self { client }
    }

    /// Fetch the current user's owned and followed playlists (`GET /me/playlists`).
    pub async fn current_user_playlists(
        &self,
        limit: u8,
    ) -> Result<Vec<Playlist>, SpotifyApiError> {
        let query = vec![("limit", limit.clamp(1, 50).to_string())];
        let page: ApiPaging<ApiPlaylist> = self.client.get_json("/me/playlists", &query).await?;
        Ok(page.items.into_iter().map(Playlist::from).collect())
    }

    /// Fetch items for a playlist using Feb 2026 `GET /playlists/{id}/items`.
    /// Note: Under Spotify's Feb 2026 Dev Mode rules, only playlists owned or collaborated on
    /// by the user expose their full item list.
    pub async fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<Track>, SpotifyApiError> {
        let query = vec![("limit", "50".to_string())];
        let primary_path = format!("/playlists/{playlist_id}/items");
        let page: ApiPaging<ApiPlaylistItemEntry> =
            match self.client.get_json(&primary_path, &query).await {
                Ok(p) => p,
                Err(SpotifyApiError::Api { status: 404, .. }) => {
                    // Graceful fallback for Extended Quota apps still served at `/tracks`
                    let legacy_path = format!("/playlists/{playlist_id}/tracks");
                    self.client.get_json(&legacy_path, &query).await?
                }
                Err(err) => return Err(err),
            };

        Ok(page
            .items
            .into_iter()
            .filter_map(|entry| entry.item)
            .map(|t| t.into_track(None, false))
            .collect())
    }
}
