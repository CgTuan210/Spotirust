//! High-precision monotonic `PlaybackClock` with drift correction, RTT latency compensation,
//! immediate seek/track-change resets, and stale/disconnected safety guards.
//!
//! Designed primarily for synchronizing word-level karaoke lyrics with remote **Spotify Connect**
//! polling (`GET /me/player`), while remaining reusable for any confirmed playback source.
//!
//! ## Drift Correction Policy (Explicit & Testable)
//! - **Deadband (`|delta| <= 35ms`):** Ignored as normal network/polling quantization jitter so the
//!   word sweep never micro-stutters.
//! - **Soft Correction (`35ms < |delta| <= 450ms`):** Re-anchors to Spotify's confirmed position
//!   while linearly blending out the residual visual delta over `400ms` so words smoothly catch up
//!   or settle without jumping backward.
//! - **Hard Snap (`|delta| > 450ms` or explicit seek):** Immediately snaps to the confirmed anchor
//!   with zero smoothing from the old position.
//! - **Pause / Disconnect / Stale Poll Timeout:** Freezes the clock immediately and marks `stale`
//!   when remote confirmation fails or times out — never extrapolates indefinitely without fresh
//!   confirmed Spotify state.

use crate::audio::engine::PlaybackPosition;
use std::time::{Duration, Instant};

/// Drift below or equal to this threshold (ms) is treated as harmless polling jitter.
pub const DRIFT_DEADBAND_MS: i64 = 35;

/// Maximum drift magnitude (ms) corrected smoothly via soft linear blending.
/// Deltas exceeding this threshold trigger an immediate hard snap (e.g. external seek).
pub const SOFT_DRIFT_MAX_MS: i64 = 450;

/// Duration (ms) over which moderate drift is linearly blended away.
pub const DRIFT_BLEND_DURATION_MS: u64 = 400;

/// Maximum one-way HTTP latency compensation (half-RTT, ms) applied when a track is playing.
pub const MAX_RTT_COMPENSATION_MS: u64 = 350;

/// Maximum age (ms) of the last confirmed remote poll before an advancing remote clock is marked stale.
pub const POLL_STALE_TIMEOUT_MS: u64 = 10_000;

/// Consecutive polling failures/timeouts before freezing and marking the clock stale.
pub const MAX_CONSECUTIVE_POLL_FAILURES: u32 = 2;

/// Outcome returned when reconciling a confirmed remote state against `PlaybackClock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockSyncOutcome {
    /// Track switched — clock was completely reset to the new track's anchor.
    TrackReset,
    /// Transitioned from playing to paused — clock frozen at confirmed position.
    Paused,
    /// Transitioned from paused/stopped/stale to playing — clock anchored cleanly.
    Resumed,
    /// Still paused/stopped — anchored to confirmed paused position without advancing.
    Frozen,
    /// Playing drift stayed inside `±DRIFT_DEADBAND_MS` — existing smooth anchor kept.
    WithinDeadband { delta_ms: i64 },
    /// Playing drift fell in `(DRIFT_DEADBAND_MS, SOFT_DRIFT_MAX_MS]` — smoothed over `DRIFT_BLEND_DURATION_MS`.
    SoftCorrected { delta_ms: i64 },
    /// Playing drift exceeded `SOFT_DRIFT_MAX_MS` — snapped immediately without smoothing.
    HardSnapped { delta_ms: i64 },
}

/// Monotonic playback clock decoupled from wall-clock time (`SystemTime`) to prevent
/// NTP jumps or OS clock adjustments from corrupting karaoke synchronization.
#[derive(Debug, Clone)]
pub struct PlaybackClock {
    track_id: Option<String>,
    confirmed_position_ms: u64,
    confirmed_at: Instant,
    is_playing: bool,
    duration_ms: u64,
    playback_speed: f64,
    /// Initial visual offset `(prev_estimated - new_anchor)` that decays linearly to `0.0`.
    drift_offset_ms: f64,
    drift_blend_start: Option<Instant>,
    drift_blend_ms: u64,
    /// Last monotonic instant a remote/backend confirmation succeeded.
    last_confirmed_at: Option<Instant>,
    /// Last measured drift delta (`compensated_remote - estimated_before_sync`) in ms.
    last_drift_ms: i64,
    /// Number of consecutive failed/timed-out remote polls.
    consecutive_failures: u32,
    /// True when remote polling timed out or Spotify Connect disconnected.
    stale: bool,
}

impl Default for PlaybackClock {
    fn default() -> Self {
        Self::new_at(Instant::now())
    }
}

impl PlaybackClock {
    pub fn new() -> Self {
        Self::new_at(Instant::now())
    }

    pub fn new_at(now: Instant) -> Self {
        Self {
            track_id: None,
            confirmed_position_ms: 0,
            confirmed_at: now,
            is_playing: false,
            duration_ms: 0,
            playback_speed: 1.0,
            drift_offset_ms: 0.0,
            drift_blend_start: None,
            drift_blend_ms: DRIFT_BLEND_DURATION_MS,
            last_confirmed_at: None,
            last_drift_ms: 0,
            consecutive_failures: 0,
            stale: false,
        }
    }

    pub fn track_id(&self) -> Option<&str> {
        self.track_id.as_deref()
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing && !self.stale
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    pub fn playback_speed(&self) -> f64 {
        self.playback_speed
    }

    pub fn last_drift_ms(&self) -> i64 {
        self.last_drift_ms
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    pub fn set_playback_speed_at(&mut self, speed: f64, now: Instant) {
        let clamped_speed = speed.clamp(0.25, 4.0);
        // Re-anchor current estimated position before changing speed so there is no discontinuity.
        let current = self.position_ms_at(now);
        self.confirmed_position_ms = current;
        self.confirmed_at = now;
        self.clear_drift();
        self.playback_speed = clamped_speed;
    }

    /// Computes the current estimated playback timestamp in milliseconds at `now` with zero allocations.
    #[inline]
    pub fn position_ms_at(&self, now: Instant) -> u64 {
        if !self.is_playing || self.stale {
            return self.clamp_to_duration(self.confirmed_position_ms);
        }

        let elapsed_ms = now
            .saturating_duration_since(self.confirmed_at)
            .as_secs_f64()
            * 1000.0
            * self.playback_speed;

        let remaining_drift = match self.drift_blend_start {
            Some(start) if self.drift_blend_ms > 0 && self.drift_offset_ms.abs() > 0.001 => {
                let blend_elapsed = now.saturating_duration_since(start).as_secs_f64() * 1000.0;
                let progress = (blend_elapsed / self.drift_blend_ms as f64).clamp(0.0, 1.0);
                self.drift_offset_ms * (1.0 - progress)
            }
            _ => 0.0,
        };

        let raw = (self.confirmed_position_ms as f64 + elapsed_ms + remaining_drift)
            .round()
            .max(0.0) as u64;
        self.clamp_to_duration(raw)
    }

    /// Convenience wrapper using `Instant::now()`.
    #[inline]
    pub fn position_ms(&self) -> u64 {
        self.position_ms_at(Instant::now())
    }

    /// Export a zero-allocation `PlaybackPosition` snapshot carrying any active soft-drift blend
    /// so the UI thread interpolates smoothly between backend updates.
    pub fn to_playback_position(&self) -> PlaybackPosition {
        PlaybackPosition::from_clock_parts(
            self.confirmed_position_ms,
            self.duration_ms,
            self.is_playing && !self.stale,
            self.confirmed_at,
            self.playback_speed,
            self.drift_offset_ms,
            self.drift_blend_start,
            self.drift_blend_ms,
            self.stale,
        )
    }

    /// Reset the clock completely for a new track (e.g. local start, Connect track switch, or stop).
    pub fn on_track_change_at(
        &mut self,
        track_id: Option<String>,
        duration_ms: u64,
        position_ms: u64,
        is_playing: bool,
        now: Instant,
    ) {
        self.track_id = track_id;
        self.duration_ms = duration_ms;
        self.confirmed_position_ms = self.clamp_to_duration(position_ms);
        self.confirmed_at = now;
        self.is_playing = is_playing;
        self.clear_drift();
        self.last_confirmed_at = Some(now);
        self.last_drift_ms = 0;
        self.consecutive_failures = 0;
        self.stale = false;
    }

    pub fn on_track_change(
        &mut self,
        track_id: Option<String>,
        duration_ms: u64,
        position_ms: u64,
        is_playing: bool,
    ) {
        self.on_track_change_at(
            track_id,
            duration_ms,
            position_ms,
            is_playing,
            Instant::now(),
        );
    }

    /// Apply an explicit seek immediately, resetting the anchor with zero smoothing from the old position.
    pub fn on_seek_at(&mut self, target_ms: u64, now: Instant) {
        self.confirmed_position_ms = self.clamp_to_duration(target_ms);
        self.confirmed_at = now;
        self.clear_drift();
        self.last_drift_ms = 0;
    }

    pub fn on_seek(&mut self, target_ms: u64) {
        self.on_seek_at(target_ms, Instant::now());
    }

    /// Freeze playback immediately at `confirmed_ms` (or the current interpolated timestamp if `None`).
    pub fn on_pause_at(&mut self, confirmed_ms: Option<u64>, now: Instant) {
        let frozen = confirmed_ms.unwrap_or_else(|| self.position_ms_at(now));
        self.confirmed_position_ms = self.clamp_to_duration(frozen);
        self.confirmed_at = now;
        self.is_playing = false;
        self.clear_drift();
    }

    pub fn on_pause(&mut self, confirmed_ms: Option<u64>) {
        self.on_pause_at(confirmed_ms, Instant::now());
    }

    /// Reconcile a confirmed remote state (e.g. Spotify Connect `GET /me/player`) with RTT compensation
    /// and tiered drift correction (deadband / soft blend / hard snap).
    pub fn reconcile_remote_at(
        &mut self,
        track_id: Option<&str>,
        remote_progress_ms: u64,
        duration_ms: u64,
        remote_is_playing: bool,
        rtt: Duration,
        now: Instant,
    ) -> ClockSyncOutcome {
        let was_stale = self.stale;
        let was_playing = self.is_playing && !was_stale;
        let prev_estimated = self.position_ms_at(now);

        self.consecutive_failures = 0;
        self.stale = false;
        self.last_confirmed_at = Some(now);
        if duration_ms > 0 {
            self.duration_ms = duration_ms;
        }

        // Half-RTT latency compensation applies only when the remote player is actively advancing.
        let rtt_comp_ms = if remote_is_playing {
            ((rtt.as_millis() as u64) / 2).min(MAX_RTT_COMPENSATION_MS)
        } else {
            0
        };
        let target_ms = self.clamp_to_duration(remote_progress_ms.saturating_add(rtt_comp_ms));

        // 1. Track changed -> complete anchor reset (never blend across tracks!)
        let track_changed = match (self.track_id.as_deref(), track_id) {
            (Some(old), Some(new)) => old != new,
            (None, Some(_)) => true,
            (Some(_), None) => true,
            (None, None) => false,
        };
        if track_changed {
            self.track_id = track_id.map(str::to_string);
            self.duration_ms = duration_ms;
            self.confirmed_position_ms = self.clamp_to_duration(target_ms);
            self.confirmed_at = now;
            self.is_playing = remote_is_playing;
            self.clear_drift();
            self.last_drift_ms = 0;
            return ClockSyncOutcome::TrackReset;
        }

        // 2. Playing -> Paused transition -> freeze cleanly at confirmed remote position
        if !remote_is_playing {
            self.confirmed_position_ms = self.clamp_to_duration(remote_progress_ms);
            self.confirmed_at = now;
            self.is_playing = false;
            self.clear_drift();
            self.last_drift_ms = 0;
            return if was_playing {
                ClockSyncOutcome::Paused
            } else {
                ClockSyncOutcome::Frozen
            };
        }

        // 3. Paused / Stopped / Stale -> Playing transition -> clean anchor without blending old paused state
        if !was_playing {
            self.confirmed_position_ms = target_ms;
            self.confirmed_at = now;
            self.is_playing = true;
            self.clear_drift();
            self.last_drift_ms = 0;
            return ClockSyncOutcome::Resumed;
        }

        // 4. Both previously playing and still playing on the same track -> evaluate drift
        let delta_ms = target_ms as i64 - prev_estimated as i64;
        self.last_drift_ms = delta_ms;
        let abs_delta = delta_ms.abs();

        if abs_delta <= DRIFT_DEADBAND_MS {
            // Keep smooth monotonic trajectory untouched; just refresh confirmation freshness.
            return ClockSyncOutcome::WithinDeadband { delta_ms };
        }

        if abs_delta <= SOFT_DRIFT_MAX_MS {
            // Re-anchor to Spotify's confirmed time while linearly blending out the residual offset
            // so display position is continuous at `now` and converges completely over `DRIFT_BLEND_DURATION_MS`.
            self.confirmed_position_ms = target_ms;
            self.confirmed_at = now;
            self.drift_offset_ms = prev_estimated as f64 - target_ms as f64;
            self.drift_blend_start = Some(now);
            self.drift_blend_ms = DRIFT_BLEND_DURATION_MS;
            return ClockSyncOutcome::SoftCorrected { delta_ms };
        }

        // Large drift (external seek, buffering stall, device transfer) -> hard snap immediately.
        self.confirmed_position_ms = target_ms;
        self.confirmed_at = now;
        self.clear_drift();
        ClockSyncOutcome::HardSnapped { delta_ms }
    }

    pub fn reconcile_remote(
        &mut self,
        track_id: Option<&str>,
        remote_progress_ms: u64,
        duration_ms: u64,
        remote_is_playing: bool,
        rtt: Duration,
    ) -> ClockSyncOutcome {
        self.reconcile_remote_at(
            track_id,
            remote_progress_ms,
            duration_ms,
            remote_is_playing,
            rtt,
            Instant::now(),
        )
    }

    /// Record a failed/timed-out remote poll. Once failures reach `MAX_CONSECUTIVE_POLL_FAILURES`
    /// or the last confirmed update exceeds `POLL_STALE_TIMEOUT_MS`, freezes the clock and marks it stale
    /// so Spotirust never pretends to have fresh confirmed playback.
    ///
    /// Returns `true` if the clock transitioned into the stale/frozen state on this call.
    pub fn on_poll_failure_at(&mut self, now: Instant) -> bool {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let timed_out = self.last_confirmed_at.is_some_and(|t| {
            now.saturating_duration_since(t) >= Duration::from_millis(POLL_STALE_TIMEOUT_MS)
        });

        if !self.stale && (self.consecutive_failures >= MAX_CONSECUTIVE_POLL_FAILURES || timed_out)
        {
            self.freeze_stale_at(now);
            return true;
        }
        false
    }

    pub fn on_poll_failure(&mut self) -> bool {
        self.on_poll_failure_at(Instant::now())
    }

    /// Check whether an active remote clock has exceeded `POLL_STALE_TIMEOUT_MS` without confirmation.
    /// Freezes and marks stale if expired.
    pub fn check_stale_at(&mut self, now: Instant) -> bool {
        if self.is_playing
            && !self.stale
            && let Some(last) = self.last_confirmed_at
            && now.saturating_duration_since(last) >= Duration::from_millis(POLL_STALE_TIMEOUT_MS)
        {
            self.freeze_stale_at(now);
            return true;
        }
        self.stale
    }

    /// Immediately freeze and mark stale when Spotify Connect reports no active device (`204 No Content`)
    /// or disconnects mid-track.
    pub fn on_disconnected_at(&mut self, now: Instant) {
        self.freeze_stale_at(now);
    }

    pub fn on_disconnected(&mut self) {
        self.on_disconnected_at(Instant::now());
    }

    /// Reset to idle/stopped zero state (e.g. logout or clear queue).
    pub fn reset_stopped_at(&mut self, now: Instant) {
        self.track_id = None;
        self.confirmed_position_ms = 0;
        self.confirmed_at = now;
        self.is_playing = false;
        self.duration_ms = 0;
        self.clear_drift();
        self.last_confirmed_at = None;
        self.last_drift_ms = 0;
        self.consecutive_failures = 0;
        self.stale = false;
    }

    pub fn reset_stopped(&mut self) {
        self.reset_stopped_at(Instant::now());
    }

    fn freeze_stale_at(&mut self, now: Instant) {
        let frozen = self.position_ms_at(now);
        self.confirmed_position_ms = frozen;
        self.confirmed_at = now;
        self.is_playing = false;
        self.stale = true;
        self.clear_drift();
    }

    fn clear_drift(&mut self) {
        self.drift_offset_ms = 0.0;
        self.drift_blend_start = None;
    }

    #[inline]
    fn clamp_to_duration(&self, ms: u64) -> u64 {
        if self.duration_ms > 0 {
            ms.min(self.duration_ms)
        } else {
            ms
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_play_advances_monotonically_from_anchor() {
        let t0 = Instant::now();
        let mut clock = PlaybackClock::new_at(t0);
        let outcome = clock.reconcile_remote_at(
            Some("sp_track_1"),
            10_000,
            180_000,
            true,
            Duration::ZERO,
            t0,
        );
        assert_eq!(outcome, ClockSyncOutcome::TrackReset);
        assert!(clock.is_playing());
        assert_eq!(clock.position_ms_at(t0), 10_000);
        assert_eq!(
            clock.position_ms_at(t0 + Duration::from_millis(250)),
            10_250
        );
        assert_eq!(
            clock.position_ms_at(t0 + Duration::from_millis(1_500)),
            11_500
        );
    }

    #[test]
    fn clock_pause_freezes_completely() {
        let t0 = Instant::now();
        let mut clock = PlaybackClock::new_at(t0);
        clock.reconcile_remote_at(Some("t1"), 5_000, 120_000, true, Duration::ZERO, t0);

        let t1 = t0 + Duration::from_millis(1_200);
        assert_eq!(clock.position_ms_at(t1), 6_200);

        let outcome =
            clock.reconcile_remote_at(Some("t1"), 6_200, 120_000, false, Duration::ZERO, t1);
        assert_eq!(outcome, ClockSyncOutcome::Paused);
        assert!(!clock.is_playing());

        // Position must remain frozen regardless of how much monotonic time elapses
        assert_eq!(clock.position_ms_at(t1 + Duration::from_secs(30)), 6_200);
        let snap = clock.to_playback_position();
        assert!(!snap.is_playing);
        assert_eq!(snap.current_ms_at(t1 + Duration::from_secs(30)), 6_200);
    }

    #[test]
    fn clock_resume_continues_only_from_confirmed_anchor() {
        let t0 = Instant::now();
        let mut clock = PlaybackClock::new_at(t0);
        clock.reconcile_remote_at(Some("t1"), 15_000, 180_000, false, Duration::ZERO, t0);
        assert_eq!(clock.position_ms_at(t0 + Duration::from_secs(5)), 15_000);

        // Resume confirmed at t1 with remote progress 15_050ms and 40ms RTT -> half-RTT = 20ms => 15_070ms
        let t1 = t0 + Duration::from_secs(5);
        let outcome = clock.reconcile_remote_at(
            Some("t1"),
            15_050,
            180_000,
            true,
            Duration::from_millis(40),
            t1,
        );
        assert_eq!(outcome, ClockSyncOutcome::Resumed);
        assert_eq!(clock.position_ms_at(t1), 15_070);
        assert_eq!(
            clock.position_ms_at(t1 + Duration::from_millis(500)),
            15_570
        );
    }

    #[test]
    fn clock_seek_forward_resets_immediately_without_smoothing() {
        let t0 = Instant::now();
        let mut clock = PlaybackClock::new_at(t0);
        clock.reconcile_remote_at(Some("t1"), 2_000, 200_000, true, Duration::ZERO, t0);

        let t1 = t0 + Duration::from_millis(500);
        clock.on_seek_at(45_000, t1);
        assert_eq!(clock.position_ms_at(t1), 45_000);
        assert_eq!(
            clock.position_ms_at(t1 + Duration::from_millis(100)),
            45_100
        );
    }

    #[test]
    fn clock_seek_backward_resets_immediately_without_smoothing() {
        let t0 = Instant::now();
        let mut clock = PlaybackClock::new_at(t0);
        clock.reconcile_remote_at(Some("t1"), 80_000, 200_000, true, Duration::ZERO, t0);

        let t1 = t0 + Duration::from_millis(300);
        clock.on_seek_at(12_000, t1);
        assert_eq!(clock.position_ms_at(t1), 12_000);
        assert_eq!(
            clock.position_ms_at(t1 + Duration::from_millis(250)),
            12_250
        );
    }

    #[test]
    fn clock_track_changed_resets_completely_and_clears_drift() {
        let t0 = Instant::now();
        let mut clock = PlaybackClock::new_at(t0);
        clock.reconcile_remote_at(Some("track_a"), 50_000, 200_000, true, Duration::ZERO, t0);

        // Induce a soft drift on track_a
        let t1 = t0 + Duration::from_secs(2);
        let soft =
            clock.reconcile_remote_at(Some("track_a"), 52_200, 200_000, true, Duration::ZERO, t1);
        assert_eq!(soft, ClockSyncOutcome::SoftCorrected { delta_ms: 200 });

        // Track switches to track_b -> must reset immediately with zero residual drift from track_a!
        let t2 = t1 + Duration::from_millis(50);
        let reset =
            clock.reconcile_remote_at(Some("track_b"), 120, 150_000, true, Duration::ZERO, t2);
        assert_eq!(reset, ClockSyncOutcome::TrackReset);
        assert_eq!(clock.track_id(), Some("track_b"));
        assert_eq!(clock.duration_ms(), 150_000);
        assert_eq!(clock.position_ms_at(t2), 120);
        assert_eq!(clock.position_ms_at(t2 + Duration::from_millis(100)), 220);
    }

    #[test]
    fn clock_polling_delay_compensates_half_rtt_and_clamps_extreme_rtt() {
        let t0 = Instant::now();
        let mut clock = PlaybackClock::new_at(t0);

        // 160ms RTT -> +80ms latency compensation while playing
        clock.reconcile_remote_at(
            Some("t1"),
            10_000,
            180_000,
            true,
            Duration::from_millis(160),
            t0,
        );
        assert_eq!(clock.position_ms_at(t0), 10_080);

        // Extreme 2000ms RTT -> compensation clamped to MAX_RTT_COMPENSATION_MS (350ms)
        let t1 = t0 + Duration::from_secs(10);
        let outcome = clock.reconcile_remote_at(
            Some("t2"),
            1_000,
            180_000,
            true,
            Duration::from_millis(2_000),
            t1,
        );
        assert_eq!(outcome, ClockSyncOutcome::TrackReset);
        assert_eq!(clock.position_ms_at(t1), 1_000 + MAX_RTT_COMPENSATION_MS);
    }

    #[test]
    fn clock_polling_timeout_and_disconnect_mark_stale_and_freeze() {
        let t0 = Instant::now();
        let mut clock = PlaybackClock::new_at(t0);
        clock.reconcile_remote_at(Some("t1"), 10_000, 180_000, true, Duration::ZERO, t0);

        // First transient failure within timeout window keeps clock running
        let t1 = t0 + Duration::from_secs(2);
        assert!(!clock.on_poll_failure_at(t1));
        assert!(!clock.is_stale());
        assert!(clock.is_playing());

        // Second consecutive failure freezes and marks stale so Spotirust doesn't claim fresh state
        let t2 = t0 + Duration::from_secs(5);
        assert!(clock.on_poll_failure_at(t2));
        assert!(clock.is_stale());
        assert!(!clock.is_playing());
        let frozen_pos = clock.position_ms_at(t2);
        assert_eq!(frozen_pos, 15_000);
        assert_eq!(
            clock.position_ms_at(t2 + Duration::from_secs(20)),
            frozen_pos
        );

        // Fresh confirmed response recovers cleanly from stale state
        let t3 = t2 + Duration::from_secs(3);
        let recovered =
            clock.reconcile_remote_at(Some("t1"), 18_000, 180_000, true, Duration::ZERO, t3);
        assert_eq!(recovered, ClockSyncOutcome::Resumed);
        assert!(!clock.is_stale());
        assert!(clock.is_playing());
        assert_eq!(clock.position_ms_at(t3), 18_000);

        // Stale timeout check after POLL_STALE_TIMEOUT_MS without polls also freezes automatically
        let t4 = t3 + Duration::from_millis(POLL_STALE_TIMEOUT_MS + 500);
        assert!(clock.check_stale_at(t4));
        assert!(clock.is_stale());
        assert!(!clock.is_playing());
    }

    #[test]
    fn clock_drift_correction_deadband_soft_blend_and_hard_snap() {
        let t0 = Instant::now();
        let mut clock = PlaybackClock::new_at(t0);
        clock.reconcile_remote_at(Some("t1"), 10_000, 200_000, true, Duration::ZERO, t0);

        // 1. Small jitter (+20ms <= 35ms deadband) -> ignored, smooth trajectory preserved
        let t1 = t0 + Duration::from_secs(2);
        let deadband =
            clock.reconcile_remote_at(Some("t1"), 12_020, 200_000, true, Duration::ZERO, t1);
        assert_eq!(deadband, ClockSyncOutcome::WithinDeadband { delta_ms: 20 });
        assert_eq!(clock.position_ms_at(t1), 12_000);

        // 2. Moderate drift (+200ms in (35, 450]) -> soft linear blend over 400ms
        let t2 = t1 + Duration::from_secs(2);
        // Expected before sync: 14_000ms; remote reports 14_200ms (+200ms ahead)
        let soft = clock.reconcile_remote_at(Some("t1"), 14_200, 200_000, true, Duration::ZERO, t2);
        assert_eq!(soft, ClockSyncOutcome::SoftCorrected { delta_ms: 200 });
        // At t2 (0% blended): continuous at 14_000ms (zero sudden jump!)
        assert_eq!(clock.position_ms_at(t2), 14_000);
        // At t2 + 200ms (50% blended): 14_200 + 200 - 100 = 14_300ms
        assert_eq!(
            clock.position_ms_at(t2 + Duration::from_millis(200)),
            14_300
        );
        // At t2 + 400ms (100% blended): 14_200 + 400 = 14_600ms (fully converged!)
        assert_eq!(
            clock.position_ms_at(t2 + Duration::from_millis(400)),
            14_600
        );

        // Also check exported PlaybackPosition matches exact blend math on UI thread!
        let pos_snap = clock.to_playback_position();
        assert_eq!(
            pos_snap.current_ms_at(t2 + Duration::from_millis(200)),
            14_300
        );
        assert_eq!(
            pos_snap.current_ms_at(t2 + Duration::from_millis(400)),
            14_600
        );

        // 3. Large drift (+3_000ms > 450ms, e.g. external seek on phone) -> immediate hard snap
        let t3 = t2 + Duration::from_secs(2);
        // Before sync at t3: 14_200 + 2_000 = 16_200ms; remote jumped to 19_200ms (+3_000ms)
        let hard = clock.reconcile_remote_at(Some("t1"), 19_200, 200_000, true, Duration::ZERO, t3);
        assert_eq!(hard, ClockSyncOutcome::HardSnapped { delta_ms: 3_000 });
        assert_eq!(clock.position_ms_at(t3), 19_200);
    }

    #[test]
    fn clock_clamps_cleanly_near_track_end() {
        let t0 = Instant::now();
        let mut clock = PlaybackClock::new_at(t0);
        clock.reconcile_remote_at(Some("t1"), 179_500, 180_000, true, Duration::ZERO, t0);

        assert_eq!(
            clock.position_ms_at(t0 + Duration::from_millis(300)),
            179_800
        );
        assert_eq!(
            clock.position_ms_at(t0 + Duration::from_millis(500)),
            180_000
        );
        // Must never overshoot track duration
        assert_eq!(clock.position_ms_at(t0 + Duration::from_secs(5)), 180_000);

        // Seeking beyond duration also clamps cleanly
        clock.on_seek_at(250_000, t0 + Duration::from_secs(6));
        assert_eq!(clock.position_ms_at(t0 + Duration::from_secs(6)), 180_000);
    }
}
