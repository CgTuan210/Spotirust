//! Dedicated Spotify OAuth Token Manager with OS keyring persistence, expiry tracking,
//! single-flight refresh-token rotation, revocation cleanup, and strict secret redaction.
//!
//! Tokens are NEVER logged and NEVER persisted in SQLite, plaintext JSON, or UI state.

use crate::auth::oauth::{OAuthError, SpotifyOAuthClient};
use crate::storage::credentials::{CredentialStore, StoredTokenBundle};
use chrono::Utc;
use std::fmt;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, broadcast};

/// Proactive refresh lead time during normal API usage (seconds).
const ACTIVE_REFRESH_SKEW_SECS: i64 = 60;
/// Proactive refresh lead time when restoring a persisted session on app launch (seconds).
const RESTORE_REFRESH_SKEW_SECS: i64 = 90;

/// Manages Spotify access & refresh tokens safely across concurrent async tasks.
#[derive(Clone)]
pub struct TokenManager {
    credentials: Arc<dyn CredentialStore>,
    oauth: SpotifyOAuthClient,
    active_tokens: Arc<RwLock<Option<StoredTokenBundle>>>,
    /// Single-flight lock ensuring concurrent 401 retries or expiry checks never race on a
    /// single-use rotated Spotify refresh token.
    refresh_lock: Arc<Mutex<()>>,
    /// Broadcast channel notifying UI/backend listeners when an active session permanently
    /// expires or is revoked by Spotify (`400`/`401` `invalid_grant` or missing refresh token).
    session_expired_tx: broadcast::Sender<String>,
}

impl fmt::Debug for TokenManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenManager")
            .field("credential_backend", &self.credentials.backend_name())
            .field("active_tokens", &"[REDACTED]")
            .finish()
    }
}

impl TokenManager {
    pub fn new(credentials: Arc<dyn CredentialStore>, oauth: SpotifyOAuthClient) -> Self {
        let (session_expired_tx, _) = broadcast::channel(8);
        Self {
            credentials,
            oauth,
            active_tokens: Arc::new(RwLock::new(None)),
            refresh_lock: Arc::new(Mutex::new(())),
            session_expired_tx,
        }
    }

    pub fn with_oauth_client(mut self, oauth: SpotifyOAuthClient) -> Self {
        self.oauth = oauth;
        self
    }

    /// Return a cheap `Arc`-backed clone of the shared pooled `reqwest::Client`.
    pub fn http_client(&self) -> reqwest::Client {
        self.oauth.http_client()
    }

    /// Borrow the underlying OAuth HTTP client (used by `SessionManager` for code exchange).
    pub fn oauth_client(&self) -> &SpotifyOAuthClient {
        &self.oauth
    }

    /// Human-readable name of the active OS secure credential backend.
    pub fn credential_backend_name(&self) -> &'static str {
        self.credentials.backend_name()
    }

    /// Subscribe to runtime session-expiration notifications triggered when token refresh is
    /// permanently rejected by Spotify.
    pub fn subscribe_session_expired(&self) -> broadcast::Receiver<String> {
        self.session_expired_tx.subscribe()
    }

    /// Check whether a saved token bundle currently exists in OS secure storage (without logging
    /// or exposing token values). Useful at startup to transition UI to `Refreshing`.
    pub fn has_persisted_tokens(&self) -> bool {
        matches!(self.credentials.load_tokens(), Ok(Some(_)))
    }

    /// Persist a freshly obtained token bundle into OS secure storage and memory.
    pub async fn store_tokens(&self, bundle: StoredTokenBundle) {
        if let Err(err) = self.credentials.save_tokens(&bundle) {
            tracing::warn!(error = %err, "Failed persisting token bundle to OS credential store");
        }
        *self.active_tokens.write().await = Some(bundle);
    }

    /// Restore a saved session from the OS keyring on application startup.
    ///
    /// - Automatically refreshes expired or near-expiry access tokens using the stored refresh token.
    /// - Deletes revoked/invalid credentials from the OS vault on permanent `400`/`401` rejections.
    /// - Preserves stored credentials across transient network outages so offline restarts don't
    ///   force unnecessary re-authentication.
    pub async fn restore_from_vault(&self) -> Result<Option<StoredTokenBundle>, OAuthError> {
        let _guard = self.refresh_lock.lock().await;

        let stored = match self.credentials.load_tokens() {
            Ok(Some(bundle)) => bundle,
            Ok(None) => return Ok(None),
            Err(err) => {
                tracing::warn!(error = %err, "Unable to read session from credential store");
                return Ok(None);
            }
        };

        let now = Utc::now().timestamp();
        let valid_bundle = if stored.is_expired_with_skew(now, RESTORE_REFRESH_SKEW_SECS) {
            tracing::info!("Stored Spotify access token expired; refreshing via OAuth");
            match self.oauth.refresh_token(&stored).await {
                Ok(refreshed) => {
                    if let Err(err) = self.credentials.save_tokens(&refreshed) {
                        tracing::warn!(error = %err, "Failed saving refreshed token to keyring");
                    }
                    refreshed
                }
                Err(err) => {
                    tracing::warn!(error = %err, "Token refresh failed during session restore");
                    if is_permanent_token_rejection(&err) {
                        tracing::info!(
                            "Clearing permanently rejected OAuth credentials from OS vault"
                        );
                        *self.active_tokens.write().await = None;
                        let _ = self.credentials.delete_tokens();
                    }
                    return Err(err);
                }
            }
        } else {
            stored
        };

        *self.active_tokens.write().await = Some(valid_bundle.clone());
        Ok(Some(valid_bundle))
    }

    /// Return a fresh, non-expired access token, automatically refreshing if expired or within
    /// 60 seconds of expiry.
    pub async fn valid_access_token(&self) -> Result<String, OAuthError> {
        let current = self.active_tokens.read().await.clone();
        let Some(bundle) = current else {
            return Err(OAuthError::NoRefreshToken);
        };

        let now = Utc::now().timestamp();
        if !bundle.is_expired_with_skew(now, ACTIVE_REFRESH_SKEW_SECS) {
            return Ok(bundle.access_token);
        }

        self.refresh_if_stale(Some(&bundle.access_token)).await
    }

    /// Unconditionally refresh the active access token (serialized via single-flight mutex).
    pub async fn force_refresh(&self) -> Result<String, OAuthError> {
        self.refresh_if_stale(None).await
    }

    /// Single-flight token refresh.
    ///
    /// When `failed_access_token` is provided (e.g. after a `401 Unauthorized` or expiry race)
    /// and another concurrent task has already rotated the token while we waited on `refresh_lock`,
    /// returns the freshly rotated access token immediately without consuming the refresh token twice.
    pub async fn refresh_if_stale(
        &self,
        failed_access_token: Option<&str>,
    ) -> Result<String, OAuthError> {
        let _guard = self.refresh_lock.lock().await;

        let existing = self
            .active_tokens
            .read()
            .await
            .clone()
            .ok_or(OAuthError::NoRefreshToken)?;

        let now = Utc::now().timestamp();
        if let Some(stale) = failed_access_token
            && existing.access_token != stale
            && !existing.is_expired_with_skew(now, ACTIVE_REFRESH_SKEW_SECS)
        {
            return Ok(existing.access_token);
        }

        match self.oauth.refresh_token(&existing).await {
            Ok(refreshed) => {
                if let Err(err) = self.credentials.save_tokens(&refreshed) {
                    tracing::warn!(error = %err, "Failed saving rotated refresh token to keyring");
                }
                let access_token = refreshed.access_token.clone();
                *self.active_tokens.write().await = Some(refreshed);
                Ok(access_token)
            }
            Err(err) => {
                if is_permanent_token_rejection(&err) {
                    tracing::warn!(
                        error = %err,
                        "Refresh token permanently rejected; clearing invalid session from OS vault"
                    );
                    *self.active_tokens.write().await = None;
                    let _ = self.credentials.delete_tokens();
                    let _ = self.session_expired_tx.send(
                        "Your Spotify session expired or was revoked. Please log in again."
                            .to_string(),
                    );
                }
                Err(err)
            }
        }
    }

    /// Returns `true` if an active token bundle is currently loaded in memory.
    pub async fn has_valid_session(&self) -> bool {
        self.active_tokens.read().await.is_some()
    }

    /// Returns the active access token expiration timestamp (Unix seconds) without exposing tokens.
    pub async fn expires_at_unix(&self) -> Option<i64> {
        self.active_tokens
            .read()
            .await
            .as_ref()
            .map(|b| b.expires_at_unix)
    }

    /// Clear in-memory tokens and delete saved credentials from OS secure storage (logout).
    pub async fn clear_tokens(&self) {
        *self.active_tokens.write().await = None;
        if let Err(err) = self.credentials.delete_tokens() {
            tracing::warn!(error = %err, "Error removing credentials from OS keyring during logout");
        }
    }
}

/// Returns `true` when an `OAuthError` indicates permanently expired/revoked credentials
/// rather than a transient network outage.
pub fn is_permanent_token_rejection(err: &OAuthError) -> bool {
    matches!(
        err,
        OAuthError::TokenRejected {
            status: 400 | 401,
            ..
        } | OAuthError::NoRefreshToken
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::credentials::MemoryCredentialStore;

    fn sample_bundle(expires_at_unix: i64) -> StoredTokenBundle {
        StoredTokenBundle {
            access_token: "secret-access-token-xyz".to_string(),
            refresh_token: Some("secret-refresh-token-abc".to_string()),
            token_type: "Bearer".to_string(),
            scope: "user-read-private".to_string(),
            expires_at_unix,
            client_id: "public-client-id".to_string(),
        }
    }

    #[tokio::test]
    async fn token_manager_stores_restores_tracks_expiry_and_clears_on_logout() {
        let store = Arc::new(MemoryCredentialStore::new());
        let tm1 = TokenManager::new(store.clone(), SpotifyOAuthClient::new());

        assert!(!tm1.has_persisted_tokens());
        assert!(!tm1.has_valid_session().await);

        let future_exp = Utc::now().timestamp() + 3600;
        tm1.store_tokens(sample_bundle(future_exp)).await;

        assert!(tm1.has_persisted_tokens());
        assert!(tm1.has_valid_session().await);
        assert_eq!(tm1.expires_at_unix().await, Some(future_exp));
        assert_eq!(
            tm1.valid_access_token().await.unwrap(),
            "secret-access-token-xyz"
        );

        // Debug representation must never leak secret token strings
        let dbg = format!("{tm1:?}");
        assert!(!dbg.contains("secret-access-token-xyz"));
        assert!(!dbg.contains("secret-refresh-token-abc"));
        assert!(dbg.contains("[REDACTED]"));

        // Simulate app restart with a brand-new TokenManager reading the same persistent store
        let tm2 = TokenManager::new(store.clone(), SpotifyOAuthClient::new());
        let restored = tm2.restore_from_vault().await.unwrap();
        assert!(restored.is_some());
        assert!(tm2.has_valid_session().await);
        assert_eq!(
            tm2.valid_access_token().await.unwrap(),
            "secret-access-token-xyz"
        );

        // Logout clears both memory and persistent store
        tm2.clear_tokens().await;
        assert!(!tm2.has_valid_session().await);
        assert!(!tm2.has_persisted_tokens());
        assert!(store.load_tokens().unwrap().is_none());
    }
}
