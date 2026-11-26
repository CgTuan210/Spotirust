//! Spotify OAuth 2.0 Authorization Code Flow with PKCE, loopback callback listener,
//! and secure OS-backed session persistence.

pub mod callback;
pub mod oauth;
pub mod service;
pub mod session;
pub mod token_manager;
