//! Audio playback engine abstraction, queue manager, event bus, and concrete backends
//! (Official Spotify Connect Web API, Local Rodio audio, optional Librespot 0.8, and Preview Clock).

pub mod clock;
pub mod connect;
pub mod engine;
pub mod events;
#[cfg(feature = "librespot-backend")]
pub mod librespot_engine;
pub mod local_rodio;
pub mod playback;
pub mod queue;
