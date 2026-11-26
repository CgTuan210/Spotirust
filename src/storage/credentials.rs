//! OS-backed secure credential storage (Windows Credential Manager, macOS Keychain,
//! Linux Secret Service) with strict token redaction.
//!
//! Tokens and credentials are NEVER written to SQLite or plaintext files.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;
use thiserror::Error;

pub const KEYRING_SERVICE: &str = "com.spotirust.desktop";
pub const KEYRING_OAUTH_ACCOUNT: &str = "spotify_oauth_session";
pub const KEYRING_MUSIXMATCH_ACCOUNT: &str = "musixmatch_official_api_key";
pub const KEYRING_SOLOIST_ACCOUNT: &str = "spotify_soloist_api_key";

/// Load an optional user-supplied official Musixmatch Developer API key from the OS credential vault.
/// Falls back cleanly to `None` if no key is stored or the OS keyring is unavailable.
pub fn load_musixmatch_api_key() -> Option<String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_MUSIXMATCH_ACCOUNT).ok()?;
    entry
        .get_password()
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Save an optional official Musixmatch Developer API key strictly inside the OS credential vault.
pub fn save_musixmatch_api_key(key: &str) -> Result<(), CredentialError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return delete_musixmatch_api_key();
    }
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_MUSIXMATCH_ACCOUNT)
        .map_err(|e| CredentialError::Backend(e.to_string()))?;
    entry
        .set_password(trimmed)
        .map_err(|e| CredentialError::Backend(e.to_string()))
}

/// Remove any saved Musixmatch API key from the OS credential vault.
pub fn delete_musixmatch_api_key() -> Result<(), CredentialError> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_MUSIXMATCH_ACCOUNT)
        .map_err(|e| CredentialError::Backend(e.to_string()))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(keyring::Error::NoDefaultStore) => Ok(()),
        Err(e) => Err(CredentialError::Backend(e.to_string())),
    }
}

/// Load the user's personal Spotify Soloist API key from the OS credential vault.
/// Falls back cleanly to `None` if not stored or if the OS keyring is unavailable.
pub fn load_soloist_api_key() -> Option<String> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_SOLOIST_ACCOUNT).ok()?;
    entry
        .get_password()
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Save the user's personal Spotify Soloist API key strictly inside the OS credential vault
/// (never written to SQLite or plaintext config files).
pub fn save_soloist_api_key(key: &str) -> Result<(), CredentialError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return delete_soloist_api_key();
    }
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_SOLOIST_ACCOUNT)
        .map_err(|e| CredentialError::Backend(e.to_string()))?;
    entry
        .set_password(trimmed)
        .map_err(|e| CredentialError::Backend(e.to_string()))
}

/// Remove any saved Spotify Soloist API key from the OS credential vault.
pub fn delete_soloist_api_key() -> Result<(), CredentialError> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_SOLOIST_ACCOUNT)
        .map_err(|e| CredentialError::Backend(e.to_string()))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(keyring::Error::NoDefaultStore) => Ok(()),
        Err(e) => Err(CredentialError::Backend(e.to_string())),
    }
}

/// Errors raised by secure credential operations.
#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("OS credential store is unavailable: {0}")]
    StoreUnavailable(String),
    #[error("failed to access OS credential store: {0}")]
    Backend(String),
    #[error("failed to serialize or deserialize session bundle: {0}")]
    Serialization(String),
}

/// Persisted OAuth token bundle stored strictly inside the OS credential manager.
///
/// Implements a custom `Debug` that redacts sensitive tokens so they can never leak into logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTokenBundle {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub scope: String,
    /// Unix timestamp in seconds when `access_token` expires.
    pub expires_at_unix: i64,
    /// Client ID associated with this token grant (public identifier, never a secret).
    pub client_id: String,
}

impl StoredTokenBundle {
    /// Returns `true` if the access token is expired or within `skew_secs` of expiring.
    pub fn is_expired_with_skew(&self, now_unix: i64, skew_secs: i64) -> bool {
        now_unix + skew_secs >= self.expires_at_unix
    }
}

impl fmt::Debug for StoredTokenBundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredTokenBundle")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .field("expires_at_unix", &self.expires_at_unix)
            .field("client_id", &self.client_id)
            .finish()
    }
}

/// Abstract credential store allowing OS keyring in production and in-memory isolation in tests.
pub trait CredentialStore: Send + Sync {
    fn backend_name(&self) -> &'static str;
    fn save_tokens(&self, tokens: &StoredTokenBundle) -> Result<(), CredentialError>;
    fn load_tokens(&self) -> Result<Option<StoredTokenBundle>, CredentialError>;
    fn delete_tokens(&self) -> Result<(), CredentialError>;
}

/// Cross-platform OS secure credential store powered by `keyring 4`.
///
/// - Windows: Windows Credential Manager
/// - macOS: Apple Keychain Services
/// - Linux: DBus Secret Service (GNOME Keyring / KWallet)
#[derive(Debug, Clone)]
pub struct OsKeyringStore {
    service: String,
    account: String,
}

impl OsKeyringStore {
    pub fn new() -> Self {
        Self {
            service: KEYRING_SERVICE.to_string(),
            account: KEYRING_OAUTH_ACCOUNT.to_string(),
        }
    }

    pub fn with_names(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }

    /// Probes whether the platform keyring backend initialized successfully.
    pub fn is_available() -> bool {
        keyring::Entry::store_status().is_ok()
    }

    fn entry(&self) -> Result<keyring::Entry, CredentialError> {
        keyring::Entry::new(&self.service, &self.account).map_err(|err| match err {
            keyring::Error::NoDefaultStore => CredentialError::StoreUnavailable(err.to_string()),
            other => CredentialError::Backend(other.to_string()),
        })
    }
}

impl Default for OsKeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore for OsKeyringStore {
    fn backend_name(&self) -> &'static str {
        #[cfg(target_os = "windows")]
        {
            "Windows Credential Manager"
        }
        #[cfg(target_os = "macos")]
        {
            "macOS Keychain"
        }
        #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
        {
            "Linux Secret Service (DBus)"
        }
    }

    fn save_tokens(&self, tokens: &StoredTokenBundle) -> Result<(), CredentialError> {
        let json = serde_json::to_string(tokens)
            .map_err(|e| CredentialError::Serialization(e.to_string()))?;
        let entry = self.entry()?;
        entry
            .set_password(&json)
            .map_err(|e| CredentialError::Backend(e.to_string()))
    }

    fn load_tokens(&self) -> Result<Option<StoredTokenBundle>, CredentialError> {
        let entry = self.entry()?;
        match entry.get_password() {
            Ok(json) => {
                let bundle = serde_json::from_str(&json)
                    .map_err(|e| CredentialError::Serialization(e.to_string()))?;
                Ok(Some(bundle))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(keyring::Error::NoDefaultStore) => Err(CredentialError::StoreUnavailable(
                "No OS keyring store available".to_string(),
            )),
            Err(err) => Err(CredentialError::Backend(err.to_string())),
        }
    }

    fn delete_tokens(&self) -> Result<(), CredentialError> {
        let entry = self.entry()?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(CredentialError::Backend(err.to_string())),
        }
    }
}

/// Ephemeral in-memory credential store used in unit tests and as a non-persistent
/// safety fallback when running in headless Linux environments without a Secret Service daemon.
/// Never writes tokens to disk or SQLite.
#[derive(Default)]
pub struct MemoryCredentialStore {
    inner: Mutex<HashMap<String, String>>,
}

impl MemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn backend_name(&self) -> &'static str {
        "In-Memory Ephemeral Store (Session-Only)"
    }

    fn save_tokens(&self, tokens: &StoredTokenBundle) -> Result<(), CredentialError> {
        let json = serde_json::to_string(tokens)
            .map_err(|e| CredentialError::Serialization(e.to_string()))?;
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| CredentialError::Backend("mutex poisoned".to_string()))?;
        guard.insert(KEYRING_OAUTH_ACCOUNT.to_string(), json);
        Ok(())
    }

    fn load_tokens(&self) -> Result<Option<StoredTokenBundle>, CredentialError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| CredentialError::Backend("mutex poisoned".to_string()))?;
        match guard.get(KEYRING_OAUTH_ACCOUNT) {
            Some(json) => {
                let bundle = serde_json::from_str(json)
                    .map_err(|e| CredentialError::Serialization(e.to_string()))?;
                Ok(Some(bundle))
            }
            None => Ok(None),
        }
    }

    fn delete_tokens(&self) -> Result<(), CredentialError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| CredentialError::Backend("mutex poisoned".to_string()))?;
        guard.remove(KEYRING_OAUTH_ACCOUNT);
        Ok(())
    }
}

/// Resilient credential store that uses the OS Keychain when available and falls back
/// to ephemeral RAM-only storage (never plaintext disk!) if the OS daemon is unreachable.
pub struct ResilientCredentialStore {
    primary: OsKeyringStore,
    fallback: MemoryCredentialStore,
}

impl ResilientCredentialStore {
    pub fn new() -> Self {
        Self {
            primary: OsKeyringStore::new(),
            fallback: MemoryCredentialStore::new(),
        }
    }
}

impl Default for ResilientCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore for ResilientCredentialStore {
    fn backend_name(&self) -> &'static str {
        if OsKeyringStore::is_available() {
            self.primary.backend_name()
        } else {
            self.fallback.backend_name()
        }
    }

    fn save_tokens(&self, tokens: &StoredTokenBundle) -> Result<(), CredentialError> {
        match self.primary.save_tokens(tokens) {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "OS keyring unavailable; holding OAuth tokens in ephemeral memory only (not persisted to disk)"
                );
                self.fallback.save_tokens(tokens)
            }
        }
    }

    fn load_tokens(&self) -> Result<Option<StoredTokenBundle>, CredentialError> {
        match self.primary.load_tokens() {
            Ok(Some(tokens)) => Ok(Some(tokens)),
            Ok(None) => self.fallback.load_tokens(),
            Err(err) => {
                tracing::debug!(error = %err, "OS keyring unavailable during session restore");
                self.fallback.load_tokens()
            }
        }
    }

    fn delete_tokens(&self) -> Result<(), CredentialError> {
        let _ = self.fallback.delete_tokens();
        match self.primary.delete_tokens() {
            Ok(()) => Ok(()),
            Err(CredentialError::StoreUnavailable(_)) => Ok(()),
            Err(err) => {
                tracing::debug!(error = %err, "Ignoring keyring delete error on unavailable store");
                Ok(())
            }
        }
    }
}
