//! Deterministic playback queue supporting shuffle (preserving the active track),
//! repeat modes (`Off`, `Context`, `Track`), history navigation, and persistence.

use crate::core::models::{RepeatMode, Track};
use rand::RngExt;

/// Ordered track queue with optional Fisher-Yates shuffle overlay and repeat handling.
#[derive(Debug, Clone, Default)]
pub struct PlaybackQueue {
    /// Original unshuffled track list.
    original: Vec<Track>,
    /// Playback order as indices into `original`.
    order: Vec<usize>,
    /// Current cursor inside `order`.
    cursor: Option<usize>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
}

impl PlaybackQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the queue with `tracks` and start playing at the track matching `selected_id` (or index 0).
    pub fn set_context(&mut self, tracks: Vec<Track>, selected_id: Option<&str>) -> Option<&Track> {
        self.original = tracks;
        self.order = (0..self.original.len()).collect();
        if self.original.is_empty() {
            self.cursor = None;
            return None;
        }

        let target_orig_idx = selected_id
            .and_then(|id| self.original.iter().position(|t| t.id == id || t.uri == id))
            .unwrap_or(0);

        if self.shuffle {
            self.rebuild_shuffled_order(Some(target_orig_idx));
            self.cursor = Some(0);
        } else {
            self.cursor = Some(target_orig_idx);
        }

        self.current()
    }

    /// Append a track to the end of the queue without interrupting the currently playing track.
    pub fn enqueue(&mut self, track: Track) {
        let idx = self.original.len();
        self.original.push(track);
        self.order.push(idx);
        if self.cursor.is_none() {
            self.cursor = Some(0);
        }
    }

    /// Remove the item at display queue index `display_index`.
    pub fn remove_at(&mut self, display_index: usize) -> bool {
        if display_index >= self.order.len() {
            return false;
        }
        let orig_idx = self.order.remove(display_index);
        self.original.remove(orig_idx);
        // Shift remaining original indices above `orig_idx`
        for idx in &mut self.order {
            if *idx > orig_idx {
                *idx -= 1;
            }
        }
        if self.order.is_empty() {
            self.cursor = None;
        } else if let Some(cur) = self.cursor {
            if display_index < cur {
                self.cursor = Some(cur - 1);
            } else if cur >= self.order.len() {
                self.cursor = Some(self.order.len() - 1);
            }
        }
        true
    }

    pub fn clear(&mut self) {
        self.original.clear();
        self.order.clear();
        self.cursor = None;
    }

    pub fn current(&self) -> Option<&Track> {
        let cur = self.cursor?;
        let orig_idx = *self.order.get(cur)?;
        self.original.get(orig_idx)
    }

    pub fn current_index(&self) -> Option<usize> {
        self.cursor
    }

    /// Ordered tracks as currently scheduled for playback.
    pub fn items_in_playback_order(&self) -> Vec<Track> {
        self.order
            .iter()
            .filter_map(|&idx| self.original.get(idx).cloned())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Jump directly to a track at playback-order index `index`.
    pub fn jump_to(&mut self, index: usize) -> Option<&Track> {
        if index < self.order.len() {
            self.cursor = Some(index);
            self.current()
        } else {
            None
        }
    }

    /// Advance explicitly triggered by the user clicking Next.
    pub fn next_track(&mut self) -> Option<&Track> {
        if self.order.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => Some(0),
            Some(cur) if cur + 1 < self.order.len() => Some(cur + 1),
            Some(_) => match self.repeat {
                RepeatMode::Context | RepeatMode::Track => Some(0),
                RepeatMode::Off => None,
            },
        };
        if let Some(idx) = next {
            self.cursor = Some(idx);
            self.current()
        } else {
            None
        }
    }

    /// Advance automatically when a track naturally finishes playing.
    pub fn advance_on_end(&mut self) -> Option<&Track> {
        if self.order.is_empty() {
            return None;
        }
        if self.repeat == RepeatMode::Track {
            return self.current();
        }
        self.next_track()
    }

    /// Step backward to the previous track.
    pub fn previous_track(&mut self) -> Option<&Track> {
        if self.order.is_empty() {
            return None;
        }
        let prev = match self.cursor {
            None => Some(0),
            Some(0) => match self.repeat {
                RepeatMode::Context => Some(self.order.len() - 1),
                _ => Some(0),
            },
            Some(cur) => Some(cur - 1),
        };
        self.cursor = prev;
        self.current()
    }

    /// Toggle shuffle while preserving the currently playing track seamlessly.
    pub fn set_shuffle(&mut self, enabled: bool) {
        if self.shuffle == enabled {
            return;
        }
        let current_orig = self.cursor.and_then(|c| self.order.get(c).copied());
        self.shuffle = enabled;

        if self.original.is_empty() {
            return;
        }

        if enabled {
            self.rebuild_shuffled_order(current_orig);
            self.cursor = Some(0);
        } else {
            self.order = (0..self.original.len()).collect();
            self.cursor = current_orig;
        }
    }

    pub fn set_repeat(&mut self, repeat: RepeatMode) {
        self.repeat = repeat;
    }

    fn rebuild_shuffled_order(&mut self, anchor_orig: Option<usize>) {
        let mut indices: Vec<usize> = (0..self.original.len())
            .filter(|&i| Some(i) != anchor_orig)
            .collect();

        let mut rng = rand::rng();
        for i in (1..indices.len()).rev() {
            let j = rng.random_range(0..=i);
            indices.swap(i, j);
        }

        self.order.clear();
        if let Some(anchor) = anchor_orig {
            self.order.push(anchor);
        }
        self.order.extend(indices);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tracks(count: usize) -> Vec<Track> {
        (0..count)
            .map(|i| Track {
                id: format!("t{i}"),
                uri: format!("spotify:track:t{i}"),
                title: format!("Track {i}"),
                artists: vec!["Artist".to_string()],
                album_id: None,
                album_name: "Album".to_string(),
                duration_ms: 180_000,
                isrc: None,
                artwork_url: None,
                explicit: false,
                liked: false,
                local_path: None,
                accent_rgb: None,
            })
            .collect()
    }

    #[test]
    fn queue_next_previous_and_repeat_modes() {
        let mut q = PlaybackQueue::new();
        q.set_context(sample_tracks(3), Some("t0"));
        assert_eq!(q.current().unwrap().id, "t0");

        assert_eq!(q.next_track().unwrap().id, "t1");
        assert_eq!(q.next_track().unwrap().id, "t2");
        assert!(q.next_track().is_none());

        // Context repeat wraps cleanly
        q.set_repeat(RepeatMode::Context);
        assert_eq!(q.next_track().unwrap().id, "t0");
        assert_eq!(q.previous_track().unwrap().id, "t2");

        // Track repeat holds the active song on natural end
        q.set_repeat(RepeatMode::Track);
        assert_eq!(q.advance_on_end().unwrap().id, "t2");
    }

    #[test]
    fn shuffle_preserves_current_track_and_restores_order() {
        let mut q = PlaybackQueue::new();
        q.set_context(sample_tracks(12), Some("t5"));
        assert_eq!(q.current().unwrap().id, "t5");

        q.set_shuffle(true);
        assert_eq!(q.current().unwrap().id, "t5");
        assert_eq!(q.current_index(), Some(0));
        assert_eq!(q.len(), 12);

        q.set_shuffle(false);
        assert_eq!(q.current().unwrap().id, "t5");
        assert_eq!(q.current_index(), Some(5));
    }
}
