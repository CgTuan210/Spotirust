//! OAuth 2.0 Authorization Code Flow with PKCE (RFC 7636) for Spotify Web API.
//!
//! Strictly follows Spotify's 2025–2026 security requirements:
//! - Authorization Code Flow with `S256` PKCE only (no implicit grant, no Client Secret).
//! - Explicit loopback IPv4 literal redirect URI (`http://127.0.0.1:<port>/callback`).
//! - Cryptographically random `state` token checked in constant time against CSRF.
//! - Zero logging of access tokens, refresh tokens, or PKCE verifiers.

use crate::storage::credentials::StoredTokenBundle;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fmt;
use thiserror::Error;
use url::Url;

pub const SPOTIFY_AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";
pub const SPOTIFY_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

/// Scopes requested during Spotify OAuth sign-in.
/// Strictly limited to the permissions required by Spotirust's existing modules:
/// - `user-read-private`: user profile (`GET /me`)
/// - `user-library-read` / `user-library-modify`: liked tracks, saved albums, `/me/library`
/// - `user-follow-read`: followed artists (`GET /me/following?type=artist`)
/// - `playlist-read-private` / `playlist-read-collaborative`: user playlists & items
/// - `user-read-recently-played` / `user-top-read`: home recently played & top tracks
/// - `user-read-playback-state` / `user-modify-playback-state` / `user-read-currently-playing`:
///   Spotify Connect devices, playback state, queue, and transport controls.
pub const DEFAULT_SCOPES: &[&str] = &[
    "user-read-private",
    "user-library-read",
    "user-library-modify",
    "user-follow-read",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-read-recently-played",
    "user-top-read",
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
];

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error(
        "Spotify OAuth Client ID is not configured in this build. Set SPOTIFY_CLIENT_ID \
         (or SPOTIRUST_CLIENT_ID) at build time or in the development environment. \
         See docs/ARCHITECTURE.md for Spotify Developer Dashboard configuration."
    )]
    ClientIdNotConfigured,
    #[error("invalid redirect URI: {0}")]
    InvalidRedirectUri(String),
    #[error("OAuth state validation failed (possible CSRF mismatch)")]
    StateMismatch,
    #[error(
        "{}",
        if error == "access_denied" {
            "Spotify sign-in was cancelled because permissions were not granted.".to_string()
        } else {
            format!(
                "Spotify authorization denied: {error}{}",
                description.as_ref().map(|d| format!(" ({d})")).unwrap_or_default()
            )
        }
    )]
    AccessDenied {
        error: String,
        description: Option<String>,
    },
    #[error("missing authorization code in OAuth callback")]
    MissingAuthorizationCode,
    #[error("missing state parameter in OAuth callback")]
    MissingState,
    #[error("callback listener error: {0}")]
    CallbackServer(String),
    #[error("authentication timed out waiting for browser callback")]
    TimedOut,
    #[error("HTTP token exchange error: {0}")]
    Http(String),
    #[error("token endpoint rejected request ({status}): {message}")]
    TokenRejected { status: u16, message: String },
    #[error("no refresh token available to renew expired session")]
    NoRefreshToken,
}

/// Cryptographic PKCE verifier/challenge pair and CSRF state for a single login attempt.
#[derive(Clone)]
pub struct PkceAuthRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub state: String,
    pub code_verifier: String,
    pub code_challenge: String,
    pub scopes: Vec<String>,
    pub authorize_url: String,
}

impl fmt::Debug for PkceAuthRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PkceAuthRequest")
            .field("client_id", &self.client_id)
            .field("redirect_uri", &self.redirect_uri)
            .field("state", &"[REDACTED]")
            .field("code_verifier", &"[REDACTED]")
            .field("code_challenge", &self.code_challenge)
            .field("scopes", &self.scopes)
            .finish()
    }
}

impl PkceAuthRequest {
    /// Generate a fresh PKCE `S256` request with cryptographic randomness.
    pub fn generate(
        client_id: &str,
        redirect_uri: &str,
        scopes: &[&str],
    ) -> Result<Self, OAuthError> {
        let client_id = client_id.trim();
        if client_id.is_empty() {
            return Err(OAuthError::ClientIdNotConfigured);
        }
        validate_loopback_redirect_uri(redirect_uri)?;

        let mut verifier_bytes = [0u8; 64];
        let mut state_bytes = [0u8; 32];
        let mut rng = rand::rng();
        rng.fill_bytes(&mut verifier_bytes);
        rng.fill_bytes(&mut state_bytes);

        let code_verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
        let code_challenge = compute_s256_challenge(&code_verifier);
        let state = URL_SAFE_NO_PAD.encode(state_bytes);

        let scope_vec: Vec<String> = scopes.iter().map(|s| (*s).to_string()).collect();
        let scope_joined = scope_vec.join(" ");

        let mut url = Url::parse(SPOTIFY_AUTHORIZE_URL)
            .map_err(|e| OAuthError::InvalidRedirectUri(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("scope", &scope_joined)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", &state)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &code_challenge);

        Ok(Self {
            client_id: client_id.to_string(),
            redirect_uri: redirect_uri.to_string(),
            state,
            code_verifier,
            code_challenge,
            scopes: scope_vec,
            authorize_url: url.into(),
        })
    }

    /// Validate an incoming callback query string against this request's CSRF `state`.
    pub fn validate_callback(&self, query: &str) -> Result<String, OAuthError> {
        validate_callback_params(query, &self.state)
    }
}

/// Compute RFC 7636 `S256` PKCE code challenge: `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`.
pub fn compute_s256_challenge(code_verifier: &str) -> String {
    let digest = Sha256::digest(code_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Validates that `redirect_uri` complies with Spotify's 2025+ loopback redirect URI rules:
/// - `localhost` is explicitly forbidden.
/// - HTTP is permitted ONLY for explicit loopback IP literals (`127.0.0.1` or `[::1]`).
/// - HTTPS is allowed for registered non-loopback domains.
pub fn validate_loopback_redirect_uri(redirect_uri: &str) -> Result<(), OAuthError> {
    let parsed = Url::parse(redirect_uri)
        .map_err(|e| OAuthError::InvalidRedirectUri(format!("{redirect_uri}: {e}")))?;

    let host = parsed
        .host_str()
        .ok_or_else(|| OAuthError::InvalidRedirectUri("missing host".to_string()))?;

    if host.eq_ignore_ascii_case("localhost") {
        return Err(OAuthError::InvalidRedirectUri(
            "Spotify forbids 'localhost' redirect URIs since Nov 2025; use explicit 127.0.0.1 or [::1]"
                .to_string(),
        ));
    }

    match parsed.scheme() {
        "http" => {
            if host != "127.0.0.1" && host != "[::1]" {
                return Err(OAuthError::InvalidRedirectUri(
                    "HTTP redirect URIs are only permitted for loopback IP literals (127.0.0.1 or [::1])"
                        .to_string(),
                ));
            }
            Ok(())
        }
        "https" => Ok(()),
        other => Err(OAuthError::InvalidRedirectUri(format!(
            "unsupported scheme '{other}'"
        ))),
    }
}

/// Parse and validate a desktop `SPOTIFY_REDIRECT_URI` for the local IPv4 (`127.0.0.1`) callback
/// listener, returning `(port, path)` where `port == 0` requests an OS-assigned dynamic port.
pub fn parse_loopback_redirect_uri_config(redirect_uri: &str) -> Result<(u16, String), OAuthError> {
    let trimmed = redirect_uri.trim();
    validate_loopback_redirect_uri(trimmed)?;
    let parsed = Url::parse(trimmed)
        .map_err(|e| OAuthError::InvalidRedirectUri(format!("{trimmed}: {e}")))?;

    if parsed.scheme() != "http" || parsed.host_str() != Some("127.0.0.1") {
        return Err(OAuthError::InvalidRedirectUri(
            "Desktop callback server requires an explicit http://127.0.0.1[:port]/callback redirect URI"
                .to_string(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(OAuthError::InvalidRedirectUri(
            "Redirect URI must not include query parameters or fragments".to_string(),
        ));
    }

    let port = parsed.port().unwrap_or(0);
    let path = match parsed.path() {
        "" | "/" => "/callback".to_string(),
        other => other.to_string(),
    };
    Ok((port, path))
}

/// Constant-time string comparison to protect CSRF `state` validation against timing side-channels.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.len() != b_bytes.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a_bytes.iter().zip(b_bytes.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Parse and validate query parameters returned to the loopback callback endpoint.
pub fn validate_callback_params(query: &str, expected_state: &str) -> Result<String, OAuthError> {
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    let mut error: Option<String> = None;
    let mut error_description: Option<String> = None;

    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "error_description" => error_description = Some(v.into_owned()),
            _ => {}
        }
    }

    let returned_state = state.ok_or(OAuthError::MissingState)?;
    if !constant_time_eq(&returned_state, expected_state) {
        return Err(OAuthError::StateMismatch);
    }

    if let Some(err) = error {
        return Err(OAuthError::AccessDenied {
            error: err,
            description: error_description,
        });
    }

    let auth_code = code
        .filter(|c| !c.trim().is_empty())
        .ok_or(OAuthError::MissingAuthorizationCode)?;
    Ok(auth_code)
}

#[derive(Deserialize)]
struct SpotifyTokenResponse {
    access_token: String,
    token_type: String,
    #[serde(default)]
    scope: String,
    expires_in: i64,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct SpotifyTokenErrorBody {
    error: Option<String>,
    error_description: Option<String>,
}

/// OAuth token HTTP client exchanging PKCE authorization codes and refreshing tokens.
#[derive(Clone)]
pub struct SpotifyOAuthClient {
    http: reqwest::Client,
    token_url: String,
}

impl SpotifyOAuthClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "Spotirust/{} (https://github.com/CgTuan210/Spotirust)",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self::with_http_client(http)
    }

    /// Construct an OAuth client reusing an existing pooled `reqwest::Client`.
    pub fn with_http_client(http: reqwest::Client) -> Self {
        Self {
            http,
            token_url: SPOTIFY_TOKEN_URL.to_string(),
        }
    }

    /// Return a cheap `Arc`-backed clone of the underlying pooled HTTP client.
    pub fn http_client(&self) -> reqwest::Client {
        self.http.clone()
    }

    /// Override the token endpoint URL (useful for local loopback integration tests).
    pub fn with_token_url(mut self, token_url: impl Into<String>) -> Self {
        self.token_url = token_url.into();
        self
    }

    /// Exchange an authorization code + PKCE `code_verifier` for an access/refresh token bundle.
    pub async fn exchange_code(
        &self,
        request: &PkceAuthRequest,
        authorization_code: &str,
    ) -> Result<StoredTokenBundle, OAuthError> {
        let params = [
            ("grant_type", "authorization_code"),
            ("code", authorization_code),
            ("redirect_uri", request.redirect_uri.as_str()),
            ("client_id", request.client_id.as_str()),
            ("code_verifier", request.code_verifier.as_str()),
        ];

        let response = self
            .http
            .post(&self.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?;

        if !status.is_success() {
            let message = parse_oauth_error_message(&body);
            return Err(OAuthError::TokenRejected {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: SpotifyTokenResponse = serde_json::from_str(&body)
            .map_err(|e| OAuthError::Http(format!("malformed token response JSON: {e}")))?;

        let now = Utc::now().timestamp();
        Ok(StoredTokenBundle {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token,
            token_type: parsed.token_type,
            scope: parsed.scope,
            expires_at_unix: now + parsed.expires_in.max(60),
            client_id: request.client_id.clone(),
        })
    }

    /// Refresh an access token using a PKCE `refresh_token`, preserving or rotating the refresh token
    /// per Spotify's June 2026 refresh-token rotation behavior.
    pub async fn refresh_token(
        &self,
        existing: &StoredTokenBundle,
    ) -> Result<StoredTokenBundle, OAuthError> {
        let refresh = existing
            .refresh_token
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(OAuthError::NoRefreshToken)?;

        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh),
            ("client_id", existing.client_id.as_str()),
        ];

        let response = self
            .http
            .post(&self.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| OAuthError::Http(e.to_string()))?;

        if !status.is_success() {
            let message = parse_oauth_error_message(&body);
            return Err(OAuthError::TokenRejected {
                status: status.as_u16(),
                message,
            });
        }

        let parsed: SpotifyTokenResponse = serde_json::from_str(&body)
            .map_err(|e| OAuthError::Http(format!("malformed refresh response JSON: {e}")))?;

        let now = Utc::now().timestamp();
        Ok(StoredTokenBundle {
            access_token: parsed.access_token,
            refresh_token: parsed
                .refresh_token
                .or_else(|| existing.refresh_token.clone()),
            token_type: parsed.token_type,
            scope: if parsed.scope.is_empty() {
                existing.scope.clone()
            } else {
                parsed.scope
            },
            expires_at_unix: now + parsed.expires_in.max(60),
            client_id: existing.client_id.clone(),
        })
    }
}

impl Default for SpotifyOAuthClient {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_oauth_error_message(body: &str) -> String {
    if let Ok(err) = serde_json::from_str::<SpotifyTokenErrorBody>(body) {
        match (err.error, err.error_description) {
            (Some(e), Some(d)) => format!("{e}: {d}"),
            (Some(e), None) => e,
            (None, Some(d)) => d,
            (None, None) => "unknown OAuth error".to_string(),
        }
    } else {
        "OAuth token endpoint returned an error".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_test_vector() {
        // RFC 7636 Appendix B official test vector
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(compute_s256_challenge(verifier), expected);
    }

    #[test]
    fn redirect_uri_validation_enforces_2025_spotify_rules() {
        assert!(validate_loopback_redirect_uri("http://127.0.0.1:43821/callback").is_ok());
        assert!(validate_loopback_redirect_uri("http://[::1]:8888/callback").is_ok());
        assert!(validate_loopback_redirect_uri("https://app.example.com/callback").is_ok());

        // Forbidden by Spotify since Nov 27, 2025
        assert!(validate_loopback_redirect_uri("http://localhost:8888/callback").is_err());
        assert!(validate_loopback_redirect_uri("http://example.com/callback").is_err());
    }

    #[test]
    fn oauth_state_validation_rejects_csrf_mismatch() {
        let req = PkceAuthRequest::generate(
            "test-client-id",
            "http://127.0.0.1:43821/callback",
            DEFAULT_SCOPES,
        )
        .expect("valid request");

        let valid_query = format!("code=abc123&state={}", req.state);
        assert_eq!(req.validate_callback(&valid_query).unwrap(), "abc123");

        let forged_query = "code=abc123&state=attacker-forged-state";
        assert!(matches!(
            req.validate_callback(forged_query),
            Err(OAuthError::StateMismatch)
        ));

        let denied_query = format!(
            "error=access_denied&error_description=User+cancelled&state={}",
            req.state
        );
        assert!(matches!(
            req.validate_callback(&denied_query),
            Err(OAuthError::AccessDenied { .. })
        ));
    }

    #[test]
    fn pkce_debug_redacts_secrets() {
        let req = PkceAuthRequest::generate(
            "public-client-id",
            "http://127.0.0.1:43821/callback",
            DEFAULT_SCOPES,
        )
        .unwrap();
        let dbg = format!("{req:?}");
        assert!(!dbg.contains(&req.code_verifier));
        assert!(!dbg.contains(&req.state));
        assert!(dbg.contains("[REDACTED]"));
    }
}
