//! Native local audio playback engine powered by `rodio 0.21`
//! (compiled symphonia decoders: MP3, FLAC, OGG Vorbis, WAV),
//! `lofty 0.25` metadata & embedded artwork extraction, SQLite-backed incremental library
//! indexing, and pure-Rust royalty-free WAV synthesis for offline Soundcheck tracks.

use crate::audio::engine::{AudioError, PlaybackPosition};
use crate::core::models::Track;
use crate::storage::artwork::{local_artwork_exists, store_embedded_artwork};
use crate::storage::database::{Database, PersistedLocalTrack};
use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "wav"];
const MAX_SCAN_DEPTH: usize = 5;

/// Summary report returned after scanning local audio folders or importing files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalScanReport {
    pub tracks: Vec<Track>,
    /// Files newly parsed via `lofty` / `rodio`.
    pub parsed_count: usize,
    /// Unchanged files served directly from SQLite cache without re-parsing tags.
    pub reused_count: usize,
    /// Corrupt, truncated, unreadable, or unsupported files safely skipped.
    pub skipped_corrupt_count: usize,
    /// Missing/deleted entries pruned from SQLite during a full folder rescan.
    pub pruned_count: usize,
}

/// Scan a single local directory (recursively up to depth 5) for valid supported audio files,
/// extracting metadata via `lofty` without requiring a database handle.
pub fn scan_local_audio_dir(root: &Path) -> Vec<Track> {
    let mut paths = Vec::new();
    collect_audio_files_recursive(root, 0, &mut paths);
    let mut out = Vec::new();
    for path in paths {
        if let Ok(entry) = inspect_local_audio_file(&path, None) {
            out.push(entry.track);
        }
    }
    out.sort_by_key(|a| a.title.to_lowercase());
    out
}

/// Incrementally scan `dirs` into SQLite, reusing cached fingerprints for unchanged files
/// unless `force_rescan` is `true`, extracting embedded artwork into `artwork_cache_dir`,
/// skipping corrupt/unreadable files cleanly, and pruning deleted tracks on full rescans.
pub fn scan_local_library_into_db(
    db: &Database,
    dirs: &[PathBuf],
    artwork_cache_dir: &Path,
    force_rescan: bool,
) -> LocalScanReport {
    let cached = db.load_local_track_fingerprints().unwrap_or_default();
    let mut discovered_paths = Vec::new();
    let mut seen = HashSet::new();

    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let mut dir_files = Vec::new();
        collect_audio_files_recursive(dir, 0, &mut dir_files);
        for p in dir_files {
            let key = p.to_string_lossy().to_string();
            if seen.insert(key) {
                discovered_paths.push(p);
            }
        }
    }

    let mut report = LocalScanReport::default();
    let mut to_upsert = Vec::new();
    let mut keep_paths = HashSet::new();

    for path in discovered_paths {
        match process_candidate_file(&path, &cached, artwork_cache_dir, force_rescan, &mut report) {
            Some(entry) => {
                keep_paths.insert(entry.path.clone());
                if entry.was_parsed {
                    to_upsert.push(entry.persisted);
                }
            }
            None => {
                report.skipped_corrupt_count += 1;
            }
        }
    }

    if !to_upsert.is_empty()
        && let Err(err) = db.upsert_local_tracks(&to_upsert)
    {
        tracing::warn!(error = %err, "Failed persisting scanned local tracks to SQLite");
    }

    // Preserve any individually imported files outside scanned dirs that still exist on disk
    for existing_path in cached.keys() {
        let p = Path::new(existing_path);
        let inside_scanned_dir = dirs.iter().any(|d| p.starts_with(d));
        if !inside_scanned_dir && p.is_file() {
            keep_paths.insert(existing_path.clone());
        }
    }

    report.pruned_count = db.prune_local_tracks_outside(&keep_paths).unwrap_or(0);
    report.tracks = db.load_local_tracks().unwrap_or_default();
    report
}

/// Import one or more user-selected audio files into the persisted SQLite local library
/// without pruning existing scanned folders.
pub fn import_local_files_into_db(
    db: &Database,
    files: &[PathBuf],
    artwork_cache_dir: &Path,
) -> LocalScanReport {
    let cached = db.load_local_track_fingerprints().unwrap_or_default();
    let mut report = LocalScanReport::default();
    let mut to_upsert = Vec::new();

    for path in files {
        if !path.is_file() || !is_supported_audio(path) {
            report.skipped_corrupt_count += 1;
            continue;
        }
        match process_candidate_file(path, &cached, artwork_cache_dir, true, &mut report) {
            Some(entry) => {
                if entry.was_parsed {
                    to_upsert.push(entry.persisted);
                }
            }
            None => {
                report.skipped_corrupt_count += 1;
            }
        }
    }

    if !to_upsert.is_empty()
        && let Err(err) = db.upsert_local_tracks(&to_upsert)
    {
        tracing::warn!(error = %err, "Failed persisting imported local files to SQLite");
    }
    report.tracks = db.load_local_tracks().unwrap_or_default();
    report
}

struct ProcessedCandidate {
    path: String,
    persisted: PersistedLocalTrack,
    was_parsed: bool,
}

fn process_candidate_file(
    path: &Path,
    cached: &HashMap<String, PersistedLocalTrack>,
    artwork_cache_dir: &Path,
    force_rescan: bool,
    report: &mut LocalScanReport,
) -> Option<ProcessedCandidate> {
    let key = path.to_string_lossy().to_string();
    let (file_size, mtime_secs) = file_fingerprint(path)?;
    if file_size == 0 {
        tracing::warn!(path = %key, "Skipping empty 0-byte audio file");
        return None;
    }

    if !force_rescan
        && let Some(existing) = cached.get(&key)
        && existing.file_size == file_size
        && existing.mtime_secs == mtime_secs
    {
        let art_ok = existing
            .track
            .artwork_url
            .as_deref()
            .map(|u| local_artwork_exists(artwork_cache_dir, u))
            .unwrap_or(true);
        if art_ok {
            report.reused_count += 1;
            return Some(ProcessedCandidate {
                path: key,
                persisted: existing.clone(),
                was_parsed: false,
            });
        }
    }

    match inspect_local_audio_file(path, Some(artwork_cache_dir)) {
        Ok(persisted) => {
            report.parsed_count += 1;
            Some(ProcessedCandidate {
                path: key,
                persisted,
                was_parsed: true,
            })
        }
        Err(err) => {
            tracing::warn!(path = %key, error = %err, "Skipping unreadable/corrupt local audio file");
            None
        }
    }
}

fn collect_audio_files_recursive(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_SCAN_DEPTH || !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_audio_files_recursive(&path, depth + 1, out);
        } else if path.is_file() && is_supported_audio(&path) {
            out.push(path);
        }
    }
}

pub fn is_supported_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| AUDIO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn file_fingerprint(path: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((meta.len(), mtime_secs))
}

/// Probe actual audio duration in milliseconds via `rodio::Decoder` when `local-audio` is enabled.
/// Returns `None` if the file cannot be decoded, or `Some(ms)` (`0` if header omits length).
pub fn probe_rodio_decodable_duration_ms(path: &Path) -> Option<u64> {
    #[cfg(feature = "local-audio")]
    {
        use rodio::{Decoder, Source};
        use std::fs::File;
        use std::io::BufReader;

        let file = File::open(path).ok()?;
        let decoder = Decoder::new(BufReader::new(file)).ok()?;
        Some(
            decoder
                .total_duration()
                .map(|dur| dur.as_millis() as u64)
                .unwrap_or(0),
        )
    }
    #[cfg(not(feature = "local-audio"))]
    {
        let _ = path;
        Some(0)
    }
}

/// Probe actual audio duration in milliseconds via `rodio::Decoder` (returns `0` on failure).
pub fn probe_audio_duration_ms(path: &Path) -> u64 {
    probe_rodio_decodable_duration_ms(path).unwrap_or(0)
}

/// Inspect a local audio file using `lofty 0.25` + `rodio` decoder validation.
/// Extracts `title`, `artist`, `album`, `duration_ms`, and embedded cover artwork (`localart://`).
/// Returns `Err` for empty, corrupt, or undecodable audio files so they never break the library.
pub fn inspect_local_audio_file(
    path: &Path,
    artwork_cache_dir: Option<&Path>,
) -> anyhow::Result<PersistedLocalTrack> {
    let (file_size, mtime_secs) =
        file_fingerprint(path).ok_or_else(|| anyhow::anyhow!("file not readable"))?;
    if file_size == 0 {
        anyhow::bail!("empty audio file");
    }

    let tagged_res = lofty::read_from_path(path);
    let rodio_dur = probe_rodio_decodable_duration_ms(path);

    // Reject corrupt files that fail rodio decoding (when local-audio is enabled)
    #[cfg(feature = "local-audio")]
    let rodio_dur_ms = rodio_dur.ok_or_else(|| {
        anyhow::anyhow!("audio decoder rejected file (corrupt or unsupported codec)")
    })?;
    #[cfg(not(feature = "local-audio"))]
    let rodio_dur_ms = rodio_dur.unwrap_or(0);

    let (fallback_artist, fallback_title) = fallback_artist_title_from_filename(path);
    let fallback_album = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("Local Files")
        .to_string();

    let mut title = fallback_title;
    let mut artist = fallback_artist;
    let mut album_name = fallback_album;
    let mut duration_ms = rodio_dur_ms;
    let mut artwork_url: Option<String> = None;

    if let Ok(tagged) = tagged_res {
        let prop_dur = tagged.properties().duration().as_millis() as u64;
        if prop_dur > 0 {
            duration_ms = prop_dur;
        }
        if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
            if let Some(t) = tag
                .title()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            {
                title = t;
            }
            if let Some(a) = tag
                .artist()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            {
                artist = a;
            }
            if let Some(alb) = tag
                .album()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
            {
                album_name = alb;
            }
            if let Some(cache_dir) = artwork_cache_dir
                && let Some(pic) = tag.pictures().first()
            {
                artwork_url = store_embedded_artwork(cache_dir, pic.data());
            }
        }
    }

    let canonical = path.to_string_lossy().to_string();
    let track = Track {
        id: format!("localfile:{canonical}"),
        uri: format!("file://{canonical}"),
        title,
        artists: vec![artist],
        album_id: None,
        album_name,
        duration_ms,
        isrc: None,
        artwork_url,
        explicit: false,
        liked: false,
        local_path: Some(path.to_path_buf()),
        accent_rgb: None,
    };

    Ok(PersistedLocalTrack {
        path: canonical,
        file_size,
        mtime_secs,
        track,
    })
}

fn fallback_artist_title_from_filename(path: &Path) -> (String, String) {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled Track");

    match stem.split_once(" - ") {
        Some((a, t)) if !a.trim().is_empty() && !t.trim().is_empty() => {
            (a.trim().to_string(), t.trim().to_string())
        }
        _ => ("Local Library".to_string(), stem.to_string()),
    }
}

/// Ensure the two royalty-free local WAV Soundcheck files exist inside `cache_dir/soundcheck`.
/// These are real 16-bit PCM WAV files decoded and played natively by `rodio` — never a fake clock.
pub fn ensure_soundcheck_wavs(cache_dir: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    let dir = cache_dir.join("soundcheck");
    std::fs::create_dir_all(&dir)?;

    let midnight_path = dir.join("midnight_compiler.wav");
    if !is_valid_wav_file(&midnight_path) {
        // 22.0s warm ambient chord progression matching `spotirust_demo.ttml`
        write_synth_wav(
            &midnight_path,
            22.0,
            &[
                [220.0, 261.63, 329.63], // Am
                [174.61, 220.0, 261.63], // F
                [261.63, 329.63, 392.0], // C
                [196.0, 246.94, 293.66], // G
            ],
            1.0,
        )?;
    }

    let velocity_path = dir.join("velocity_pulse.wav");
    if !is_valid_wav_file(&velocity_path) {
        // 19.0s upbeat harmonic pulse matching `spotirust_demo.lrc`
        write_synth_wav(
            &velocity_path,
            19.0,
            &[
                [246.94, 293.66, 369.99], // Bm
                [196.0, 246.94, 293.66],  // G
                [293.66, 369.99, 440.0],  // D
                [220.0, 277.18, 329.63],  // A
            ],
            2.0,
        )?;
    }

    Ok((midnight_path, velocity_path))
}

fn is_valid_wav_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.len() > 1024)
        .unwrap_or(false)
}

/// Synthesize a clean, royalty-free stereo 16-bit PCM WAV file at 22,050 Hz.
pub fn write_synth_wav(
    path: &Path,
    duration_secs: f32,
    chords: &[[f32; 3]],
    pulse_hz: f32,
) -> std::io::Result<()> {
    const SAMPLE_RATE: u32 = 22_050;
    const CHANNELS: u16 = 2;
    const BITS_PER_SAMPLE: u16 = 16;

    let total_frames = ((duration_secs.max(0.1)) * SAMPLE_RATE as f32).round() as u32;
    let byte_rate = SAMPLE_RATE * u32::from(CHANNELS) * u32::from(BITS_PER_SAMPLE / 8);
    let block_align = CHANNELS * (BITS_PER_SAMPLE / 8);
    let data_bytes = total_frames * u32::from(block_align);
    let riff_size = 36 + data_bytes;

    let file = std::fs::File::create(path)?;
    let mut w = std::io::BufWriter::with_capacity(64 * 1024, file);

    // Standard 44-byte RIFF/WAVE PCM header
    w.write_all(b"RIFF")?;
    w.write_all(&riff_size.to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // PCM fmt chunk size
    w.write_all(&1u16.to_le_bytes())?; // PCM format tag
    w.write_all(&CHANNELS.to_le_bytes())?;
    w.write_all(&SAMPLE_RATE.to_le_bytes())?;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&BITS_PER_SAMPLE.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_bytes.to_le_bytes())?;

    let chord_len = (duration_secs / chords.len().max(1) as f32).max(0.5);
    let tau = std::f32::consts::TAU;

    for frame in 0..total_frames {
        let t = frame as f32 / SAMPLE_RATE as f32;
        let chord_idx = ((t / chord_len) as usize) % chords.len().max(1);
        let chord = chords[chord_idx];

        // Smooth fade-in and fade-out envelope so audio never clicks
        let fade_in = (t / 0.35).min(1.0);
        let fade_out = ((duration_secs - t) / 0.5).clamp(0.0, 1.0);
        let pulse = 0.72 + 0.28 * (tau * pulse_hz * t).sin();
        let env = 0.16 * fade_in * fade_out * pulse;

        let mut left = 0.0f32;
        let mut right = 0.0f32;
        for (i, &freq) in chord.iter().enumerate() {
            let detune = 1.0 + (i as f32 - 1.0) * 0.0025;
            left += (tau * freq * t).sin() * 0.65 + (tau * freq * 2.0 * t).sin() * 0.15;
            right += (tau * freq * detune * t).sin() * 0.65
                + (tau * freq * 2.0 * detune * t).sin() * 0.15;
        }

        let s_l = ((left * env).clamp(-0.95, 0.95) * f32::from(i16::MAX)) as i16;
        let s_r = ((right * env).clamp(-0.95, 0.95) * f32::from(i16::MAX)) as i16;
        w.write_all(&s_l.to_le_bytes())?;
        w.write_all(&s_r.to_le_bytes())?;
    }

    w.flush()
}

#[cfg(feature = "local-audio")]
mod rodio_impl {
    use super::*;
    use rodio::{Decoder, OutputStreamBuilder, Sink, Source};
    use std::fs::File;
    use std::io::BufReader;
    use std::sync::mpsc::{self, Sender};

    enum RodioCmd {
        PlayFile {
            path: PathBuf,
            volume: f32,
            reply: Sender<Result<u64, AudioError>>,
        },
        Pause,
        Resume(Sender<Result<(), AudioError>>),
        Seek(u64, Sender<Result<(), AudioError>>),
        SetVolume(f32),
        Stop,
    }

    #[derive(Clone)]
    pub struct LocalRodioPlayer {
        tx: Sender<RodioCmd>,
        position: Arc<Mutex<PlaybackPosition>>,
        has_sink: Arc<AtomicBool>,
        finished: Arc<AtomicBool>,
    }

    impl LocalRodioPlayer {
        pub fn spawn() -> Self {
            let (tx, rx) = mpsc::channel::<RodioCmd>();
            let position = Arc::new(Mutex::new(PlaybackPosition::default()));
            let has_sink = Arc::new(AtomicBool::new(false));
            let finished = Arc::new(AtomicBool::new(false));

            let pos_thread = Arc::clone(&position);
            let has_sink_thread = Arc::clone(&has_sink);
            let finished_thread = Arc::clone(&finished);

            std::thread::Builder::new()
                .name("spotirust-rodio".to_string())
                .spawn(move || {
                    // Open audio device lazily on first local file playback to keep startup instant
                    // and avoid unnecessary audio daemon connections when using Spotify Connect.
                    let mut stream_opt: Option<rodio::OutputStream> = None;
                    let mut sink_opt: Option<Sink> = None;

                    loop {
                        match rx.recv_timeout(Duration::from_millis(100)) {
                            Ok(cmd) => match cmd {
                                RodioCmd::PlayFile {
                                    path,
                                    volume,
                                    reply,
                                } => {
                                    // Validate and open audio file before probing output device
                                    let decoded = File::open(&path)
                                        .map_err(|e| AudioError::LocalIo(e.to_string()))
                                        .and_then(|file| {
                                            Decoder::new(BufReader::new(file))
                                                .map_err(|e| AudioError::LocalIo(e.to_string()))
                                        });

                                    let source = match decoded {
                                        Ok(s) => s,
                                        Err(err) => {
                                            let _ = reply.send(Err(err));
                                            continue;
                                        }
                                    };

                                    if stream_opt.is_none() {
                                        stream_opt = OutputStreamBuilder::open_default_stream()
                                            .ok()
                                            .map(|mut s| {
                                                s.log_on_drop(false);
                                                s
                                            });
                                    }
                                    let Some(stream) = stream_opt.as_ref() else {
                                        has_sink_thread.store(false, Ordering::SeqCst);
                                        finished_thread.store(false, Ordering::SeqCst);
                                        if let Ok(mut guard) = pos_thread.lock() {
                                            guard.is_playing = false;
                                        }
                                        let _ = reply.send(Err(AudioError::BackendUnavailable(
                                            "No system audio output device available (ALSA/PulseAudio/PipeWire/WASAPI/CoreAudio)"
                                                .to_string(),
                                        )));
                                        continue;
                                    };

                                    let duration_ms = source
                                        .total_duration()
                                        .map(|d| d.as_millis() as u64)
                                        .unwrap_or(0);
                                    if let Some(old) = sink_opt.take() {
                                        old.stop();
                                    }
                                    let sink = Sink::connect_new(stream.mixer());
                                    sink.set_volume(volume.clamp(0.0, 1.0));
                                    sink.append(source);
                                    sink.play();
                                    let confirmed_active = !sink.is_paused();
                                    sink_opt = Some(sink);
                                    has_sink_thread.store(confirmed_active, Ordering::SeqCst);
                                    finished_thread.store(false, Ordering::SeqCst);
                                    if let Ok(mut guard) = pos_thread.lock() {
                                        *guard = PlaybackPosition::new(
                                            0,
                                            duration_ms,
                                            confirmed_active,
                                        );
                                    }
                                    let _ = reply.send(Ok(duration_ms));
                                }
                                RodioCmd::Pause => {
                                    if let Some(sink) = sink_opt.as_ref() {
                                        sink.pause();
                                        if let Ok(mut guard) = pos_thread.lock() {
                                            guard.position_ms = sink.get_pos().as_millis() as u64;
                                            guard.is_playing = false;
                                            guard.synced_at = Instant::now();
                                        }
                                    }
                                }
                                RodioCmd::Resume(reply) => {
                                    if let Some(sink) = sink_opt.as_ref() {
                                        if sink.empty() {
                                            has_sink_thread.store(false, Ordering::SeqCst);
                                            let _ = reply.send(Err(AudioError::BackendUnavailable(
                                                "Local audio stream reached end; reload required"
                                                    .to_string(),
                                            )));
                                        } else {
                                            sink.play();
                                            finished_thread.store(false, Ordering::SeqCst);
                                            if let Ok(mut guard) = pos_thread.lock() {
                                                guard.position_ms =
                                                    sink.get_pos().as_millis() as u64;
                                                guard.is_playing = true;
                                                guard.synced_at = Instant::now();
                                            }
                                            let _ = reply.send(Ok(()));
                                        }
                                    } else {
                                        let _ = reply.send(Err(AudioError::BackendUnavailable(
                                            "No loaded local audio sink to resume".to_string(),
                                        )));
                                    }
                                }
                                RodioCmd::Seek(ms, reply) => {
                                    if let Some(sink) = sink_opt.as_ref() {
                                        let r = sink
                                            .try_seek(Duration::from_millis(ms))
                                            .map_err(|e| AudioError::CommandFailed(e.to_string()));
                                        if r.is_ok() {
                                            finished_thread.store(false, Ordering::SeqCst);
                                            if let Ok(mut guard) = pos_thread.lock() {
                                                guard.position_ms = ms;
                                                guard.synced_at = Instant::now();
                                            }
                                        }
                                        let _ = reply.send(r);
                                    } else {
                                        let _ = reply.send(Err(AudioError::BackendUnavailable(
                                            "No loaded local audio sink to seek".to_string(),
                                        )));
                                    }
                                }
                                RodioCmd::SetVolume(vol) => {
                                    if let Some(sink) = sink_opt.as_ref() {
                                        sink.set_volume(vol.clamp(0.0, 1.0));
                                    }
                                }
                                RodioCmd::Stop => {
                                    if let Some(sink) = sink_opt.take() {
                                        sink.stop();
                                    }
                                    // Release OS audio device handle when stopped so audio hardware
                                    // and cpal output threads sleep cleanly at 0% CPU.
                                    stream_opt = None;
                                    has_sink_thread.store(false, Ordering::SeqCst);
                                    finished_thread.store(false, Ordering::SeqCst);
                                    if let Ok(mut guard) = pos_thread.lock() {
                                        guard.is_playing = false;
                                        guard.position_ms = 0;
                                        guard.synced_at = Instant::now();
                                    }
                                }
                            },
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                if let Some(sink) = sink_opt.as_ref() {
                                    let is_empty = sink.empty();
                                    let is_paused = sink.is_paused();
                                    if !is_paused && is_empty {
                                        finished_thread.store(true, Ordering::SeqCst);
                                    }
                                    if let Ok(mut guard) = pos_thread.lock() {
                                        guard.position_ms = sink.get_pos().as_millis() as u64;
                                        guard.is_playing = !is_paused && !is_empty;
                                        guard.synced_at = Instant::now();
                                    }
                                }
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .expect("failed to spawn rodio audio worker thread");

            Self {
                tx,
                position,
                has_sink,
                finished,
            }
        }

        pub fn play_file(&self, path: PathBuf, volume: f32) -> Result<u64, AudioError> {
            let (reply_tx, reply_rx) = mpsc::channel();
            self.tx
                .send(RodioCmd::PlayFile {
                    path,
                    volume,
                    reply: reply_tx,
                })
                .map_err(|_| AudioError::BackendUnavailable("audio thread exited".to_string()))?;
            reply_rx.recv().map_err(|_| {
                AudioError::BackendUnavailable("audio thread dropped reply".to_string())
            })?
        }

        pub fn pause(&self) {
            let _ = self.tx.send(RodioCmd::Pause);
        }

        pub fn resume(&self) -> Result<(), AudioError> {
            let (reply_tx, reply_rx) = mpsc::channel();
            self.tx
                .send(RodioCmd::Resume(reply_tx))
                .map_err(|_| AudioError::BackendUnavailable("audio thread exited".to_string()))?;
            reply_rx.recv().map_err(|_| {
                AudioError::BackendUnavailable("audio thread dropped resume reply".to_string())
            })?
        }

        pub fn seek(&self, position_ms: u64) -> Result<(), AudioError> {
            let (reply_tx, reply_rx) = mpsc::channel();
            self.tx
                .send(RodioCmd::Seek(position_ms, reply_tx))
                .map_err(|_| AudioError::BackendUnavailable("audio thread exited".to_string()))?;
            reply_rx.recv().map_err(|_| {
                AudioError::BackendUnavailable("audio thread dropped seek".to_string())
            })?
        }

        pub fn set_volume(&self, volume: f32) {
            let _ = self.tx.send(RodioCmd::SetVolume(volume));
        }

        pub fn stop(&self) {
            let _ = self.tx.send(RodioCmd::Stop);
        }

        pub fn has_active_sink(&self) -> bool {
            self.has_sink.load(Ordering::SeqCst)
        }

        pub fn is_finished(&self) -> bool {
            self.finished.load(Ordering::SeqCst)
        }

        pub fn position(&self) -> PlaybackPosition {
            self.position.lock().map(|g| g.clone()).unwrap_or_default()
        }
    }
}

#[cfg(not(feature = "local-audio"))]
mod rodio_impl {
    use super::*;

    #[derive(Clone)]
    pub struct LocalRodioPlayer;

    impl LocalRodioPlayer {
        pub fn spawn() -> Self {
            Self
        }

        pub fn play_file(&self, _path: PathBuf, _volume: f32) -> Result<u64, AudioError> {
            Err(AudioError::BackendUnavailable(
                "Spotirust was built without the 'local-audio' feature".to_string(),
            ))
        }

        pub fn pause(&self) {}
        pub fn resume(&self) -> Result<(), AudioError> {
            Err(AudioError::BackendUnavailable(
                "Spotirust was built without the 'local-audio' feature".to_string(),
            ))
        }
        pub fn seek(&self, _position_ms: u64) -> Result<(), AudioError> {
            Err(AudioError::BackendUnavailable(
                "Spotirust was built without the 'local-audio' feature".to_string(),
            ))
        }
        pub fn set_volume(&self, _volume: f32) {}
        pub fn stop(&self) {}
        pub fn has_active_sink(&self) -> bool {
            false
        }
        pub fn is_finished(&self) -> bool {
            false
        }
        pub fn position(&self) -> PlaybackPosition {
            PlaybackPosition::default()
        }
    }
}

pub use rodio_impl::LocalRodioPlayer;

#[cfg(test)]
mod tests {
    use super::*;
    use lofty::config::WriteOptions;
    use lofty::picture::{MimeType, Picture, PictureType};
    use lofty::prelude::TaggedFileExt;
    use lofty::tag::{Tag, TagType};

    fn make_test_png() -> Vec<u8> {
        let mut img = image::RgbaImage::new(8, 8);
        for p in img.pixels_mut() {
            *p = image::Rgba([255, 80, 120, 255]);
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn generates_and_probes_real_wav_file() {
        let dir = tempfile::tempdir().unwrap();
        let wav = dir.path().join("tone.wav");
        write_synth_wav(&wav, 1.25, &[[220.0, 261.63, 329.63]], 1.0).unwrap();
        let dur = probe_audio_duration_ms(&wav);
        assert!(
            (1_200..=1_300).contains(&dur),
            "expected ~1250ms WAV duration, got {dur}ms"
        );
    }

    #[test]
    fn extracts_lofty_metadata_and_embedded_artwork_from_tagged_wav() {
        let dir = tempfile::tempdir().unwrap();
        let art_dir = dir.path().join("art_cache");
        let wav = dir.path().join("raw_filename.wav");
        write_synth_wav(&wav, 1.5, &[[220.0, 261.63, 329.63]], 1.0).unwrap();

        // Embed ID3v2 metadata + PNG cover art into the real WAV file using lofty
        let mut tagged = lofty::read_from_path(&wav).unwrap();
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("Tagged Midnight".to_string());
        tag.set_artist("Rust Audio Lab".to_string());
        tag.set_album("Native Acoustics".to_string());
        tag.push_picture(
            Picture::unchecked(make_test_png())
                .pic_type(PictureType::CoverFront)
                .mime_type(MimeType::Png)
                .build(),
        );
        tagged.insert_tag(tag);
        tagged.save_to_path(&wav, WriteOptions::default()).unwrap();

        let inspected = inspect_local_audio_file(&wav, Some(&art_dir)).unwrap();
        assert_eq!(inspected.track.title, "Tagged Midnight");
        assert_eq!(inspected.track.artists, vec!["Rust Audio Lab".to_string()]);
        assert_eq!(inspected.track.album_name, "Native Acoustics");
        assert!((1_450..=1_550).contains(&inspected.track.duration_ms));
        let art_url = inspected
            .track
            .artwork_url
            .expect("embedded artwork must be extracted");
        assert!(art_url.starts_with("localart://"));
        assert!(local_artwork_exists(&art_dir, &art_url));
    }

    #[test]
    fn rejects_corrupt_files_and_persists_incremental_sqlite_scans() {
        let dir = tempfile::tempdir().unwrap();
        let music_dir = dir.path().join("music");
        let art_dir = dir.path().join("art");
        std::fs::create_dir_all(&music_dir).unwrap();

        let valid1 = music_dir.join("Artist One - First Song.wav");
        let valid2 = music_dir.join("Artist Two - Second Song.wav");
        let corrupt = music_dir.join("broken.wav");
        let empty = music_dir.join("empty.mp3");
        write_synth_wav(&valid1, 0.8, &[[220.0, 330.0, 440.0]], 1.0).unwrap();
        write_synth_wav(&valid2, 1.0, &[[261.63, 329.63, 392.0]], 1.0).unwrap();
        std::fs::write(&corrupt, b"NOT_A_VALID_WAV_HEADER_1234567890").unwrap();
        std::fs::write(&empty, b"").unwrap();

        let db = Database::open_in_memory().unwrap();

        // First scan parses 2 valid files and safely skips 2 corrupt/empty files
        let first =
            scan_local_library_into_db(&db, std::slice::from_ref(&music_dir), &art_dir, false);
        assert_eq!(first.parsed_count, 2);
        assert_eq!(first.reused_count, 0);
        assert_eq!(first.skipped_corrupt_count, 2);
        assert_eq!(first.tracks.len(), 2);

        // Second incremental scan reuses SQLite fingerprints (0 re-parses!)
        let second =
            scan_local_library_into_db(&db, std::slice::from_ref(&music_dir), &art_dir, false);
        assert_eq!(second.parsed_count, 0);
        assert_eq!(second.reused_count, 2);
        assert_eq!(second.tracks.len(), 2);

        // Deleting a file + explicit rescan prunes the missing track from SQLite
        std::fs::remove_file(&valid2).unwrap();
        let third =
            scan_local_library_into_db(&db, std::slice::from_ref(&music_dir), &art_dir, true);
        assert_eq!(third.parsed_count, 1);
        assert_eq!(third.pruned_count, 1);
        assert_eq!(third.tracks.len(), 1);
        assert_eq!(third.tracks[0].title, "First Song");
    }
}
