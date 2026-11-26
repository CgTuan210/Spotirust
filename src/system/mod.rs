//! Operating-system desktop integrations (media controls, MPRIS2 on Linux).
//!
//! Strictly decoupled from `AudioEngine`: OS media controls emit high-level
//! `SystemMediaCommand` values and observe read-only `PlaybackSnapshot` updates.

pub mod media_controls;
