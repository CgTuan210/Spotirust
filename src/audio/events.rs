//! Broadcast event bus notifying UI subscribers and plugins whenever playback state changes.

use crate::audio::engine::{PlaybackSnapshot, PlaybackStatus};
use crate::core::models::{RepeatMode, Track};
use tokio::sync::broadcast;

/// Domain events emitted by the playback engine.
#[derive(Debug, Clone)]
pub enum PlaybackEvent {
    Snapshot(PlaybackSnapshot),
    TrackChanged(Option<Track>),
    StatusChanged(PlaybackStatus),
    Seeked {
        position_ms: u64,
    },
    VolumeChanged {
        volume: f32,
    },
    QueueChanged {
        items: Vec<Track>,
        current_index: Option<usize>,
        shuffle: bool,
        repeat: RepeatMode,
    },
    Reconnecting {
        attempt: u32,
        reason: String,
    },
    Error(String),
}

/// Multi-subscriber broadcast bus for playback events.
#[derive(Clone)]
pub struct PlaybackEventBus {
    sender: broadcast::Sender<PlaybackEvent>,
}

impl PlaybackEventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(32));
        Self { sender }
    }

    pub fn publish(&self, event: PlaybackEvent) {
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PlaybackEvent> {
        self.sender.subscribe()
    }
}

impl Default for PlaybackEventBus {
    fn default() -> Self {
        Self::new(128)
    }
}
