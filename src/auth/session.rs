//! Session manager orchestrating browser OAuth 2.0 + PKCE sign-in and delegating
//! all token storage, expiry tracking, single-flight refresh, and logout cleanup
//! to the dedicated [`TokenManager`](crate::auth::token_manager::TokenManager).

use crate::auth::callback::LoopbackCallbackServer;
use crate::auth::oauth::{DEFAULT_SCOPES, OAuthError, PkceAuthRequest, SpotifyOAuthClient};
use crate::auth::token_manager::TokenManager;
pub use crate::auth::token_manager::is_permanent_token_rejection;
use crate::storage::credentials::{CredentialStore, StoredTokenBundle};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// Fine-grained authentication lifecycle stage emitted during browser PKCE login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthProgressStage {
    OpeningBrowser { authorize_url: String },
    WaitingForAuthorization { authorize_url: String },
    ExchangingToken,
}

/// Coordinates browser PKCE sign-in and exposes authenticated tokens via [`TokenManager`].
#[derive(Clone)]
pub struct SessionManager {
    tokens: TokenManager,
    client_id: Option<String>,
    redirect_port: u16,
    redirect_path: String,
    redirect_uri_error: Option<String>,
}

impl SessionManager {
    pub fn new(
        credentials: Arc<dyn CredentialStore>,
        client_id: Option<String>,
        redirect_port: u16,
        redirect_path: String,
    ) -> Self {
        Self {
            tokens: TokenManager::new(credentials, SpotifyOAuthClient::new()),
            client_id,
            redirect_port,
            redirect_path,
            redirect_uri_error: None,
        }
    }

    /// Attach an optional redirect URI validation error detected at config load time so the
    /// application starts cleanly and surfaces an actionable error when login is attempted.
    pub fn with_redirect_uri_error(mut self, err: Option<String>) -> Self {
        self.redirect_uri_error = err;
        self
    }

    pub fn with_oauth_client(mut self, oauth: SpotifyOAuthClient) -> Self {
        self.tokens = self.tokens.with_oauth_client(oauth);
        self
    }

    /// Access the underlying dedicated [`TokenManager`].
    pub fn token_manager(&self) -> &TokenManager {
        &self.tokens
    }

    /// Return a cheap `Arc`-backed clone of the shared `reqwest::Client` so `SpotifyClient`
    /// and `SpotifyOAuthClient` reuse the same connection & TLS pool.
    pub fn http_client(&self) -> reqwest::Client {
        self.tokens.http_client()
    }

    /// Subscribe to runtime session-expiration events triggered when token refresh is
    /// permanently rejected (`400`/`401` `invalid_grant` or missing refresh token).
    pub fn subscribe_session_expired(&self) -> broadcast::Receiver<String> {
        self.tokens.subscribe_session_expired()
    }

    pub fn credential_backend_name(&self) -> &'static str {
        self.tokens.credential_backend_name()
    }

    pub fn has_client_id(&self) -> bool {
        self.client_id
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    }

    /// Check whether OS secure storage currently contains a persisted session bundle.
    pub fn has_persisted_session(&self) -> bool {
        self.tokens.has_persisted_tokens()
    }

    /// Attempt to restore a persisted session from the OS keyring on application startup.
    /// Automatically refreshes expired tokens when a refresh token is present.
    pub async fn restore_session(&self) -> Result<Option<StoredTokenBundle>, OAuthError> {
        self.tokens.restore_from_vault().await
    }

    /// Begin a browser OAuth 2.0 + PKCE login (legacy single-callback wrapper).
    pub async fn authenticate_via_browser<F>(
        &self,
        open_browser: bool,
        timeout: Duration,
        on_url_ready: F,
    ) -> Result<StoredTokenBundle, OAuthError>
    where
        F: FnOnce(&str) + Send,
    {
        let mut callback = Some(on_url_ready);
        self.authenticate_via_browser_with_progress(open_browser, timeout, move |stage| {
            if let AuthProgressStage::OpeningBrowser { authorize_url } = stage
                && let Some(cb) = callback.take()
            {
                cb(&authorize_url);
            }
        })
        .await
    }

    /// Begin a browser OAuth 2.0 + PKCE login reporting explicit stages:
    /// 1. Bind `127.0.0.1` loopback listener (ephemeral port when `redirect_port == 0`).
    /// 2. Generate PKCE verifier/challenge (`S256`) and CSRF `state`.
    /// 3. Emit `AuthProgressStage::OpeningBrowser` and launch default browser off-thread.
    /// 4. Emit `AuthProgressStage::WaitingForAuthorization` and await callback.
    /// 5. Emit `AuthProgressStage::ExchangingToken`, exchange code + verifier, and store via `TokenManager`.
    pub async fn authenticate_via_browser_with_progress<F>(
        &self,
        open_browser: bool,
        timeout: Duration,
        mut on_stage: F,
    ) -> Result<StoredTokenBundle, OAuthError>
    where
        F: FnMut(AuthProgressStage) + Send,
    {
        if let Some(err) = &self.redirect_uri_error {
            return Err(OAuthError::InvalidRedirectUri(err.clone()));
        }

        let client_id = self
            .client_id
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .ok_or(OAuthError::ClientIdNotConfigured)?;

        let server = LoopbackCallbackServer::bind(self.redirect_port, &self.redirect_path).await?;
        let request = PkceAuthRequest::generate(client_id, server.redirect_uri(), DEFAULT_SCOPES)?;

        on_stage(AuthProgressStage::OpeningBrowser {
            authorize_url: request.authorize_url.clone(),
        });

        if open_browser {
            let url_to_open = request.authorize_url.clone();
            let launch = tokio::task::spawn_blocking(move || open::that(url_to_open)).await;
            match launch {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::warn!(
                        error = %err,
                        "Could not auto-launch default browser; user can click 'Open Spotify Login'"
                    );
                }
                Err(err) => {
                    tracing::warn!(error = %err, "Browser launch task panicked");
                }
            }
        }

        on_stage(AuthProgressStage::WaitingForAuthorization {
            authorize_url: request.authorize_url.clone(),
        });

        // Consumes `server` so the loopback TCP listener closes immediately upon success, error, or timeout.
        let code = server.wait_for_code(&request.state, timeout).await?;

        on_stage(AuthProgressStage::ExchangingToken);
        let bundle = self
            .tokens
            .oauth_client()
            .exchange_code(&request, &code)
            .await?;

        self.tokens.store_tokens(bundle.clone()).await;
        Ok(bundle)
    }

    /// Return a fresh, non-expired bearer access token (auto-refreshing via `TokenManager`).
    pub async fn valid_access_token(&self) -> Result<String, OAuthError> {
        self.tokens.valid_access_token().await
    }

    /// Force an access token refresh unconditionally (serialized via `TokenManager`).
    pub async fn force_refresh_access_token(&self) -> Result<String, OAuthError> {
        self.tokens.force_refresh().await
    }

    /// Single-flight token refresh delegated to `TokenManager`.
    pub async fn force_refresh_if_stale(
        &self,
        failed_access_token: Option<&str>,
    ) -> Result<String, OAuthError> {
        self.tokens.refresh_if_stale(failed_access_token).await
    }

    /// Directly store a validated token bundle (used by tests and custom session flows).
    pub async fn set_tokens(&self, bundle: StoredTokenBundle) {
        self.tokens.store_tokens(bundle).await;
    }

    pub async fn is_authenticated(&self) -> bool {
        self.tokens.has_valid_session().await
    }

    /// Clear in-memory tokens and delete credentials from OS secure storage.
    pub async fn logout(&self) {
        self.tokens.clear_tokens().await;
    }
}
