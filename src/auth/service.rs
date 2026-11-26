//! High-level frontend-facing Spotify authentication API façade.
//!
//! Exposes the 5 standard UI-facing operations without ever leaking raw access or refresh tokens:
//! - [`SpotifyAuthService::spotify_login`]
//! - [`SpotifyAuthService::spotify_logout`]
//! - [`SpotifyAuthService::spotify_get_auth_status`]
//! - [`SpotifyAuthService::spotify_get_user_profile`]
//! - [`SpotifyAuthService::spotify_refresh_session`]
//!
//! And maintains explicit frontend authentication states:
//! [`AuthStatus::NotLoggedIn`], [`AuthStatus::Authenticating`], [`AuthStatus::Authenticated`],
//! [`AuthStatus::Refreshing`], and [`AuthStatus::AuthenticationFailed`].

use crate::auth::oauth::OAuthError;
use crate::auth::session::{AuthProgressStage, SessionManager, is_permanent_token_rejection};
use crate::core::models::UserProfile;
use crate::spotify::client::SpotifyApiError;
use crate::spotify::library::LibraryService;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::RwLock;

/// Canonical frontend authentication state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AuthStatus {
    #[default]
    NotLoggedIn,
    Authenticating,
    Authenticated,
    Refreshing,
    AuthenticationFailed,
}

/// Errors surfaced by [`SpotifyAuthService`] to the desktop UI layer (never contains raw tokens).
#[derive(Debug, Error)]
pub enum AuthServiceError {
    #[error(transparent)]
    OAuth(#[from] OAuthError),
    #[error("authenticated with Spotify, but failed loading profile: {0}")]
    ProfileFetch(#[from] SpotifyApiError),
}

impl AuthServiceError {
    /// Returns `true` when the failure means the stored/active Spotify session was permanently
    /// expired or revoked and the user must sign in again.
    pub fn is_session_expired(&self) -> bool {
        match self {
            Self::OAuth(err) => is_permanent_token_rejection(err),
            Self::ProfileFetch(SpotifyApiError::Unauthenticated) => true,
            Self::ProfileFetch(_) => false,
        }
    }
}

/// Thread-safe authentication façade used by the desktop coordinator and automated tests.
///
/// Reuses the existing [`SessionManager`] (backed by `TokenManager`) and [`LibraryService`]
/// (backed by `SpotifyClient`) without exposing bearer tokens to UI state.
#[derive(Clone)]
pub struct SpotifyAuthService {
    session: SessionManager,
    library: LibraryService,
    status: Arc<RwLock<AuthStatus>>,
    profile: Arc<RwLock<Option<UserProfile>>>,
    last_error: Arc<RwLock<Option<String>>>,
}

impl SpotifyAuthService {
    pub fn new(session: SessionManager, library: LibraryService) -> Self {
        Self {
            session,
            library,
            status: Arc::new(RwLock::new(AuthStatus::NotLoggedIn)),
            profile: Arc::new(RwLock::new(None)),
            last_error: Arc::new(RwLock::new(None)),
        }
    }

    pub fn session(&self) -> &SessionManager {
        &self.session
    }

    /// Return the current frontend authentication status (`NotLoggedIn`, `Authenticating`,
    /// `Authenticated`, `Refreshing`, or `AuthenticationFailed`).
    pub async fn spotify_get_auth_status(&self) -> AuthStatus {
        *self.status.read().await
    }

    /// Return the last human-readable authentication error message, if any.
    pub async fn last_error(&self) -> Option<String> {
        self.last_error.read().await.clone()
    }

    /// Return the currently authenticated Spotify user's profile without exposing raw tokens.
    /// If authenticated in memory without a cached profile yet, fetches `GET /me`.
    pub async fn spotify_get_user_profile(&self) -> Result<Option<UserProfile>, AuthServiceError> {
        if let Some(cached) = self.profile.read().await.clone() {
            return Ok(Some(cached));
        }
        if !self.session.is_authenticated().await {
            return Ok(None);
        }
        let fetched = self.library.current_user_profile().await?;
        *self.profile.write().await = Some(fetched.clone());
        *self.status.write().await = AuthStatus::Authenticated;
        Ok(Some(fetched))
    }

    /// Run browser-based OAuth 2.0 + PKCE login, persist tokens securely via `TokenManager`,
    /// load the user's Spotify profile, and update status immediately.
    pub async fn spotify_login<F>(
        &self,
        open_browser: bool,
        timeout: Duration,
        on_stage: F,
    ) -> Result<UserProfile, AuthServiceError>
    where
        F: FnMut(AuthProgressStage) + Send,
    {
        *self.status.write().await = AuthStatus::Authenticating;
        *self.last_error.write().await = None;

        if let Err(err) = self
            .session
            .authenticate_via_browser_with_progress(open_browser, timeout, on_stage)
            .await
        {
            let msg = err.to_string();
            *self.status.write().await = AuthStatus::AuthenticationFailed;
            *self.last_error.write().await = Some(msg);
            return Err(AuthServiceError::OAuth(err));
        }

        match self.library.current_user_profile().await {
            Ok(profile) => {
                *self.profile.write().await = Some(profile.clone());
                *self.status.write().await = AuthStatus::Authenticated;
                *self.last_error.write().await = None;
                Ok(profile)
            }
            Err(err) => {
                let msg = format!("Authenticated, but profile fetch failed: {err}");
                *self.status.write().await = AuthStatus::AuthenticationFailed;
                *self.last_error.write().await = Some(msg);
                Err(AuthServiceError::ProfileFetch(err))
            }
        }
    }

    /// Refresh an active session or restore & refresh a persisted session from the OS keyring.
    ///
    /// Transitions through [`AuthStatus::Refreshing`] while renewing tokens and resolves to:
    /// - `Ok(Some(profile))` + [`AuthStatus::Authenticated`] when valid/refreshed
    /// - `Ok(None)` + [`AuthStatus::NotLoggedIn`] when no saved session exists
    /// - `Err(_)` + [`AuthStatus::AuthenticationFailed`] when refresh fails or token is revoked
    pub async fn spotify_refresh_session(&self) -> Result<Option<UserProfile>, AuthServiceError> {
        if !self.session.is_authenticated().await && !self.session.has_persisted_session() {
            *self.profile.write().await = None;
            *self.status.write().await = AuthStatus::NotLoggedIn;
            return Ok(None);
        }

        *self.status.write().await = AuthStatus::Refreshing;
        *self.last_error.write().await = None;

        let refresh_res: Result<bool, OAuthError> = if self.session.is_authenticated().await {
            self.session
                .force_refresh_access_token()
                .await
                .map(|_| true)
        } else {
            self.session
                .restore_session()
                .await
                .map(|opt| opt.is_some())
        };

        match refresh_res {
            Ok(true) => match self.library.current_user_profile().await {
                Ok(profile) => {
                    *self.profile.write().await = Some(profile.clone());
                    *self.status.write().await = AuthStatus::Authenticated;
                    Ok(Some(profile))
                }
                Err(err) => {
                    if let Some(cached) = self.profile.read().await.clone() {
                        *self.status.write().await = AuthStatus::Authenticated;
                        return Ok(Some(cached));
                    }
                    *self.status.write().await = AuthStatus::AuthenticationFailed;
                    *self.last_error.write().await = Some(err.to_string());
                    Err(AuthServiceError::ProfileFetch(err))
                }
            },
            Ok(false) => {
                *self.profile.write().await = None;
                *self.status.write().await = AuthStatus::NotLoggedIn;
                Ok(None)
            }
            Err(err) => {
                if is_permanent_token_rejection(&err) {
                    *self.profile.write().await = None;
                }
                *self.status.write().await = AuthStatus::AuthenticationFailed;
                *self.last_error.write().await = Some(err.to_string());
                Err(AuthServiceError::OAuth(err))
            }
        }
    }

    /// Restore a previously persisted OS-keyring session on application launch.
    /// Only forces a network refresh when the stored access token is expired or near expiry.
    pub async fn restore_on_startup(
        &self,
        cached_profile_fallback: Option<UserProfile>,
    ) -> Result<Option<UserProfile>, AuthServiceError> {
        if !self.session.has_persisted_session() {
            *self.status.write().await = AuthStatus::NotLoggedIn;
            return Ok(None);
        }

        *self.status.write().await = AuthStatus::Refreshing;
        *self.last_error.write().await = None;

        match self.session.restore_session().await {
            Ok(Some(_bundle)) => match self.library.current_user_profile().await {
                Ok(profile) => {
                    *self.profile.write().await = Some(profile.clone());
                    *self.status.write().await = AuthStatus::Authenticated;
                    Ok(Some(profile))
                }
                Err(err) => {
                    if let Some(fallback) = cached_profile_fallback {
                        *self.profile.write().await = Some(fallback.clone());
                        *self.status.write().await = AuthStatus::Authenticated;
                        Ok(Some(fallback))
                    } else {
                        *self.status.write().await = AuthStatus::AuthenticationFailed;
                        *self.last_error.write().await = Some(err.to_string());
                        Err(AuthServiceError::ProfileFetch(err))
                    }
                }
            },
            Ok(None) => {
                *self.profile.write().await = None;
                *self.status.write().await = AuthStatus::NotLoggedIn;
                Ok(None)
            }
            Err(err) => {
                *self.profile.write().await = None;
                *self.status.write().await = AuthStatus::AuthenticationFailed;
                *self.last_error.write().await = Some(err.to_string());
                Err(AuthServiceError::OAuth(err))
            }
        }
    }

    /// Clear in-memory session, remove credentials from OS keyring, and reset status to `NotLoggedIn`.
    pub async fn spotify_logout(&self) {
        self.session.logout().await;
        *self.profile.write().await = None;
        *self.last_error.write().await = None;
        *self.status.write().await = AuthStatus::NotLoggedIn;
    }

    /// Update status immediately when runtime `TokenManager` broadcasts permanent session expiration.
    pub async fn mark_session_expired(&self, message: String) {
        *self.profile.write().await = None;
        *self.last_error.write().await = Some(message);
        *self.status.write().await = AuthStatus::AuthenticationFailed;
    }
}
