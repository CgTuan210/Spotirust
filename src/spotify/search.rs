//! Spotify catalog search respecting Feb 2026 Development Mode limits (`limit <= 10`)
//! with bounded in-memory TTL caching.

use crate::core::models::{Album, Artist, Playlist, SearchResults, Track};
use crate::spotify::client::{
    ApiAlbum, ApiArtistFull, ApiPaging, ApiPlaylist, ApiTrack, SpotifyApiError, SpotifyClient,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Maximum per-type result count supported under Spotify's Feb 2026 Development Mode rules.
pub const DEV_MODE_MAX_SEARCH_LIMIT: u8 = 10;
const SEARCH_CACHE_TTL: Duration = Duration::from_secs(120);
const SEARCH_CACHE_CAPACITY: usize = 64;

#[derive(Deserialize)]
struct ApiSearchResponse {
    #[serde(default)]
    tracks: Option<ApiPaging<ApiTrack>>,
    #[serde(default)]
    artists: Option<ApiPaging<ApiArtistFull>>,
    #[serde(default)]
    albums: Option<ApiPaging<ApiAlbum>>,
    #[serde(default)]
    playlists: Option<ApiPaging<Option<ApiPlaylist>>>,
}

#[derive(Clone)]
struct CachedSearchEntry {
    results: SearchResults,
    inserted_at: Instant,
}

/// Cached Spotify catalog search service.
#[derive(Clone)]
pub struct SearchService {
    client: SpotifyClient,
    cache: Arc<RwLock<HashMap<String, CachedSearchEntry>>>,
}

impl SearchService {
    pub fn new(client: SpotifyClient) -> Self {
        Self {
            client,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Search tracks, artists, albums, and playlists via `GET /search`.
    /// Clamps `limit` to `1..=10` so requests work in both Dev Mode and Extended Quota Mode.
    pub async fn search(
        &self,
        raw_query: &str,
        limit: u8,
    ) -> Result<SearchResults, SpotifyApiError> {
        let query = raw_query.trim();
        if query.is_empty() {
            return Ok(SearchResults::default());
        }

        let effective_limit = limit.clamp(1, DEV_MODE_MAX_SEARCH_LIMIT);
        let cache_key = format!("{}::{effective_limit}", query.to_lowercase());

        if let Some(hit) = self.cache.read().await.get(&cache_key)
            && hit.inserted_at.elapsed() < SEARCH_CACHE_TTL
        {
            return Ok(hit.results.clone());
        }

        let params = vec![
            ("q", query.to_string()),
            ("type", "track,artist,album,playlist".to_string()),
            ("limit", effective_limit.to_string()),
        ];

        let response: ApiSearchResponse = self.client.get_json("/search", &params).await?;

        let tracks: Vec<Track> = response
            .tracks
            .map(|p| {
                p.items
                    .into_iter()
                    .map(|t| t.into_track(None, false))
                    .collect()
            })
            .unwrap_or_default();

        let artists: Vec<Artist> = response
            .artists
            .map(|p| p.items.into_iter().map(Artist::from).collect())
            .unwrap_or_default();

        let albums: Vec<Album> = response
            .albums
            .map(|p| p.items.into_iter().map(Album::from).collect())
            .unwrap_or_default();

        let playlists: Vec<Playlist> = response
            .playlists
            .map(|p| p.items.into_iter().flatten().map(Playlist::from).collect())
            .unwrap_or_default();

        let results = SearchResults {
            query: query.to_string(),
            tracks,
            artists,
            albums,
            playlists,
        };

        let mut cache = self.cache.write().await;
        if cache.len() >= SEARCH_CACHE_CAPACITY
            && let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.inserted_at)
                .map(|(k, _)| k.clone())
        {
            cache.remove(&oldest);
        }
        cache.insert(
            cache_key,
            CachedSearchEntry {
                results: results.clone(),
                inserted_at: Instant::now(),
            },
        );

        Ok(results)
    }

    pub async fn clear_cache(&self) {
        self.cache.write().await.clear();
    }
}
