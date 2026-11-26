# Spotirust — Production Readiness & Release Candidate Audit

> **Audit Date:** November 26, 2026  
> **Repository:** `/home/antigravity/CgTuan210/Spotirust`  
> **Version:** `0.1.0` (Release Candidate — Rust `1.100.0`, Edition 2024)  
> **Companion Report:** [`PERFORMANCE_REPORT.md`](PERFORMANCE_REPORT.md)  
> **Honesty Standard:** Automated unit/loopback tests are classified as `TESTED`, never `VERIFIED` end-to-end against external platforms unless executed against real live services or hardware.

---

## 1. Module-by-Module Verification Matrix

Classification definitions:
- **`VERIFIED`**: Executed end-to-end in a real runtime environment (GUI window / real audio pipeline / live D-Bus session / real SQLite / real portable archive) with observed evidence.
- **`TESTED`**: Verified via automated unit and/or local loopback integration tests (`cargo test`), not yet verified end-to-end against live external servers or physical desktop OS hardware.
- **`PARTIAL`**: Core implementation works, with known environmental or cross-platform gaps.
- **`BLOCKED`**: Restricted by upstream API policy, DRM, or upstream library incompatibility.
- **`UNVERIFIED`**: Compiled/written but no live runtime evidence on the target platform/service.

| Subsystem / Module | Source Files | Classification | Evidence & Concrete Limitations |
|---|---|---|---|
| **Native Desktop GUI (`eframe`/`egui`)** | `src/main.rs`, `src/app.rs`, `src/ui/*.rs` | **VERIFIED** *(Linux X11/Xvfb)* / **UNVERIFIED** *(Windows, macOS, Wayland, hardware GPU)* | Release binary (`28.14 MB`) launched under `Xvfb` (1240×800, Mesa LLVMpipe swGL). Cold UI init avg `232.0 ms`, X11 mapped `153.6 ms`, post-stop idle `0.00%` CPU, 30s RAM delta `+0.22 MB` (flat `111.54 MB`). Hardware GPU & Windows/macOS rendering not tested. |
| **Karaoke Renderer & Repaint Scheduler** | `src/ui/lyrics.rs`, `src/ui/components/artwork.rs` | **VERIFIED** *(Linux X11 GUI + unit/integration tests)* | Cached `Arc<egui::Galley>` (`KaraokeLayoutCache`: `18.5 µs` uncached → `16.3 ns` hit = `1,135×` faster), separated `KaraokeAnimationState`, opaque gradient mesh, and adaptive `next_karaoke_repaint_delay` cut Fullscreen Karaoke CPU from **`179.10%` → `86.78%` (-51.5%)** (`RustUI 7.50% → 5.33%`, `Mesa swGL 170.27% → 79.95%`) with zero loss of word-level timing accuracy. |
| **Local Music Player (`rodio 0.21`, `lofty 0.25`, `rfd 0.18`)** | `src/audio/local_rodio.rs`, `src/storage/database.rs`, `migrations/0002_local_library.sql`, `src/ui/library.rs` | **VERIFIED** *(WAV playback, `lofty` metadata/embedded PNG artwork, SQLite v2 incremental scan, corrupt-file skipping, PulseAudio auto-advance/resume/seek)* / **PARTIAL** *(compiled MP3/FLAC/OGG decoders & native `rfd` portal UI unverified with desktop portal/binary fixtures)* | `rodio 0.21.1` compiles real `symphonia` decoders for `flac`, `mp3`, `ogg`/`vorbis`, and `wav`/`pcm`. Real synthesized WAV files verified end-to-end through PulseAudio `module-null-sink` (pause, resume, natural end-of-track auto-advance, seek, clean `OutputStream` release). `lofty` tag + embedded cover art extraction, corrupt/0-byte file rejection, SQLite v2 instant startup (no rescan on boot), explicit Rescan + pruning, and restart position restore (`on_exit`) all tested & verified. Native `rfd` folder/file picker dialog requires an active OS desktop/portal (`xdg-desktop-portal` on Linux); fallback manual path input is provided. |
| **Artwork Disk & RAM Cache** | `src/storage/artwork.rs`, `src/ui/components/artwork.rs` | **TESTED** *(bounded disk & LRU RAM eviction, `localart://` decode, deduplication, failed-URL ring)* / **UNVERIFIED** *(live Spotify CDN HTTPS)* | Bounded disk cache (`64 MiB` / `512` files max with oldest-first eviction + atomic temp-rename writes) and true LRU GPU texture cache (`MAX_GPU_TEXTURES = 96`, touch-on-`get`, bounded failed ring) tested across 160 simulated covers. Live remote Spotify CDN fetches not tested in restricted sandbox. |
| **Spotify Connect Remote Control (`/me/player/*`)** | `src/audio/connect.rs`, `src/audio/playback.rs`, `src/spotify/client.rs`, `src/ui/topbar.rs` | **TESTED** *(loopback HTTP)* / **UNVERIFIED** *(live Spotify Premium device)* | Loopback HTTP tests verify Play + `GET /me/player` confirmation, Pause, Seek, Next, Volume, Queue, strict anti-fake guard (refuses `Playing` on HTTP `204`), **HTTP `429` + `Retry-After` shared cooldown** (0 extra requests sent during cooldown), and **mid-playback device disappearance** (stops clock & surfaces disconnect banner). Device menu clearly separates Active, Available, and Restricted (disabled) devices. Not tested against live `api.spotify.com` with a real Spotify Premium account. |
| **Spotify Soloist Direct WebSocket (`--ws`)** | *Intentionally Not Implemented* | **VERIFIED** *(via standard Spotify Connect compatibility)* | Researched official Nov 2026 Soloist docs: `--ws 127.0.0.1:PORT` is opt-in, unauthenticated localhost socket, and Soloist requires each user's own proprietary binary & non-redistributable 90-day key. Soloist already works as a standard Spotify Connect target without custom WebSocket coupling. |
| **Direct In-Window Spotify Streaming (`librespot 0.8`)** | `src/audio/librespot_engine.rs` | **BLOCKED** | Compiles cleanly with `--features librespot-backend` (0 clippy warnings, all 47 tests pass). Blocked upstream since **August 10, 2026** (`librespot-org/librespot#1737`): Spotify rejects third-party OAuth tokens on `login5` (`INVALID_CREDENTIALS`). Spotirust refuses to embed unauthorized first-party credentials or bypass DRM. |
| **OS Media Controls (`SystemMediaBridge`)** | `src/system/media_controls.rs`, `src/app.rs` | **VERIFIED** *(Linux MPRIS2 over live `dbus-run-session`)* / **UNVERIFIED** *(Windows SMTC & macOS Now Playing — Not yet implemented)* | Completely decoupled from `AudioEngine`. On Linux, native MPRIS2 D-Bus server (`org.mpris.MediaPlayer2.spotirust` via `zbus 5.19`) verified live under `dbus-run-session` (`Identity`, `PlaybackStatus`, `LoopStatus`, `Shuffle`, `Volume`, `Metadata`, `PlayPause`, `Next`, `Previous`, `Seek`, `SetVolume`). Degrades cleanly when D-Bus is absent. On Windows/macOS, honestly reports `UnsupportedPlatform` (zero fake stubs). Keyboard media shortcuts active cross-platform. |
| **Spotify OAuth 2.0 PKCE & Loopback Callback** | `src/auth/oauth.rs`, `src/auth/callback.rs`, `src/auth/session.rs` | **TESTED** *(protocol & loopback)* / **UNVERIFIED** *(live `accounts.spotify.com`)* | RFC 7636 `S256`, constant-time CSRF `state`, `127.0.0.1` loopback listener, single-flight concurrent refresh mutex, token rotation, and `400 invalid_grant` credential cleanup all tested over real loopback TCP. Live Spotify login requires real account & `SPOTIRUST_CLIENT_ID`. |
| **Spotify Web API (Profile, Library, Playlists, Search)** | `src/spotify/{client,library,playlists,search}.rs` | **TESTED** *(DTOs & client logic)* / **UNVERIFIED** *(live `api.spotify.com`)* | Compatible with Feb–Jul 2026 API rules (`account_id`, `/me/library`, `/playlists/{id}/items`, `GET /search` `limit <= 10`, ETag `304`, strict `429 Retry-After` cooldown, single-flight `401` retry). Not yet verified against live Spotify servers. |
| **OS Secure Credential Storage (`keyring 4.2`)** | `src/storage/credentials.rs` | **PARTIAL** *(fallback verified; OS vaults unverified)* | Token `Debug` redaction and `MemoryCredentialStore` tested. Without DBus Secret Service on headless Linux, cleanly falls back to session-only RAM (never writes tokens to SQLite). Desktop GNOME Keyring / KWallet, Windows Credential Manager, and macOS Keychain are **UNVERIFIED**. |
| **Lyrics Providers & Parsers (`Unison`, `AMLL TTML DB`, `LRCLIB`, `Musixmatch Official`, Local Files)** | `src/lyrics/{model,ttml,lrc,lyricsfile,matcher,cache,provider,unison,amll_db,lrclib,musixmatch,apple_music}.rs` | **VERIFIED** *(live Unison word TTML, live AMLL CC0 Spotify-ID word TTML, live LRCLIB `Lyricsfile v1.0` word YAML, offline TTML/LRC/Lyricsfile & SQLite v3 cache)* / **TESTED** *(Musixmatch Official RichSync/Subtitle/Plain mock tests, ordered fallback, timeout/404/429/503, zero-spam)* / **BLOCKED** *(Apple Music API & BiniLyrics proxy)* | See [`docs/LYRICS_PROVIDERS.md`](docs/LYRICS_PROVIDERS.md) for the full 7-column licensing & verification table. Ordered fallback chain (`Local -> Unison -> AMLL -> Musixmatch [if key] -> LRCLIB`) prioritizes Word > Line > Plain with mandatory attribution rendered in UI and persisted in SQLite v3. |
| **Playback Clock & Drift Correction (`PlaybackClock`)** | `src/audio/clock.rs`, `src/audio/engine.rs`, `src/audio/connect.rs`, `src/audio/playback.rs` | **TESTED** *(10 standalone clock unit tests + Spotify Connect loopback E2E test)* / **UNVERIFIED** *(live Spotify Connect device over real WAN)* | Monotonic interpolation, half-RTT latency compensation (clamped `350ms`), explicit deadband (`<= 35ms`), soft blend (`35..=450ms` over `400ms`), hard snap (`> 450ms` & seeks), track-change reset + async lyrics cancellation, and stale/disconnected freeze guards. |
| **SQLite Persistence & Migrations** | `src/storage/database.rs`, `migrations/0001_initial.sql`, `migrations/0002_local_library.sql`, `migrations/0003_lyrics_attribution.sql` | **VERIFIED** | Versioned idempotent migrations **v1–v3**, WAL mode, instant `local_tracks` loading & incremental fingerprinting, `lyrics_cache.attribution_json`, queue/playback restore, independent `clear_lyrics_cache()`, full `clear_cache()`, and full app reset verified. Zero OAuth token or API key columns in SQLite. |
| **Packaging & Portable Mode** | `scripts/package_portable.sh`, `scripts/package_portable.ps1`, `.github/workflows/ci.yml` | **VERIFIED** *(Linux `.tar.gz` built, extracted & executed in portable mode)* / **UNVERIFIED** *(Windows `.zip`, macOS `.tar.gz`/`.dmg`)* | `./scripts/package_portable.sh` produces a self-contained portable archive with `portable.flag` and docs; verified running extracted binary without Rust/Cargo. Windows & macOS installers/packages have **not** been tested on native Windows/macOS machines. |

---

## 1B. Word-Level Lyrics & Spotify Connect Synchronization Matrix

> Companion research document: [`docs/APPLE_MUSIC_LYRICS_RESEARCH.md`](docs/APPLE_MUSIC_LYRICS_RESEARCH.md).  
> Statuses below are reported strictly per component — never collapsed into a blanket "Lyrics supported" claim.

| Component | Status | Evidence & Honest Scope |
|---|---|---|
| **Apple Music provider** | **BLOCKED** | Official Apple Music API (`api.music.apple.com/v1`) and MusicKit expose only `Songs.Attributes.hasLyrics: boolean` — no official public lyrics/TTML endpoint exists (confirmed Nov 21, 2026, Apple Feedback `FB25664234`). Spotirust refuses to call private `amp-api.music.apple.com/.../syllable-lyrics` or scrape `media-user-token` cookies. `AppleMusicLyricsProvider` honestly reports `UpstreamUnsupported` and never mislabels LRCLIB/local files as Apple Music. |
| **TTML parser** | **VERIFIED** | Parses W3C + Apple/AMLL word-level TTML (`itunes:timing="Word"`, `x-bg`, `itunes:song-part`, `ttm:agent`, inline & sidecar translations/romanizations), line-only `TtmlLine`, missing/duplicate end timestamps, XML entities, CJK spacing, and rejects malformed/truncated XML. Verified via unit tests, integration tests, `--diagnostics` micro-benchmark (`79.4 µs/doc`), and live GUI karaoke rendering. |
| **Word-level timing** | **VERIFIED** *(Local TTML & Enhanced LRC)* / **TESTED** *(mock Spotify Connect E2E)* | `LyricWord::progress_at`, `KaraokeAnimationState`, `KaraokeLayoutCache`, and `next_karaoke_repaint_delay` verified with real local WAV + bundled TTML in GUI (`83.97%` total / `5.00%` Rust UI on Xvfb+Mesa swGL) and tested step-by-step against mock Spotify Connect state. |
| **Spotify playback clock** | **TESTED** *(unit + loopback E2E)* / **UNVERIFIED** *(live Spotify Premium device)* | `PlaybackClock` (`src/audio/clock.rs`) tested deterministically across 10 standalone unit tests: Play, Pause freeze, Resume anchor, Seek forward/backward immediate reset, Track change full reset, RTT polling delay compensation, Polling timeout/stale freeze, Drift correction, and Track-end clamping. |
| **Drift correction** | **TESTED** *(unit + loopback E2E)* / **UNVERIFIED** *(live Spotify Connect WAN jitter)* | Explicit testable thresholds verified: deadband (`<= 35ms`), linear soft blend (`35 < |delta| <= 450ms` over `400ms`), and immediate hard snap (`> 450ms` / external seek). |
| **Spotify Connect synchronization** | **TESTED** *(loopback HTTP mock + TTML + cancellation E2E)* / **UNVERIFIED** *(live `api.spotify.com` Premium device)* | End-to-end loopback test verifies Connect Play/Pause/Resume/Seek/Track Switch/In-Flight Lyrics Cancellation/Delayed Poll/503 Timeout Stale Freeze/`204` Disconnect synchronized with word-level TTML. Requires a real Spotify Premium account & external device for live verification. |
| **Local TTML import** | **VERIFIED** *(sidecar, folder lookup, path import & SQLite cache)* / **PARTIAL** *(native `rfd` dialog requires live OS desktop portal)* | `LocalFileLyricsProvider` and `LyricsService::import_for_track` verified offline + cached in SQLite (overrides LRC cleanly). UI offers both native `Browse .ttml / .lrc…` (`rfd`) picker and direct path import fallback. |

---

## 2. Priority #1 — Definitive Technical Answers on Spotify Playback

1. **Can users play Spotify music directly inside the Spotirust window?**
   **No.** Pure native Rust desktop apps cannot officially stream encrypted Spotify audio tracks directly in-window without Chromium/EME Widevine DRM (Web Playback SDK) or unauthorized credentials.
2. **Or does Spotirust only control Spotify running on another device?**
   **For Spotify catalog tracks, yes.** Spotirust operates in **Spotify Connect Remote Controller Mode** (`📡 Connect`), controlling an active external Spotify Connect target (Spotify Desktop/Mobile, hardware speaker, or **Spotify Soloist**) via `/me/player/*`. For local audio files (`MP3`, `FLAC`, `OGG`, `WAV`), Spotirust operates independently in **Local Music Player Mode** (`🎵 Local`, powered by `rodio`).
3. **Which `librespot` authentication flows still work and are permitted?**
   None that comply with official third-party OAuth rules. Since **August 10, 2026** (`librespot-org/librespot#1737`), Spotify's `login5` service rejects third-party developer Client ID OAuth tokens (`INVALID_CREDENTIALS`). Spotirust refuses to embed unauthorized first-party credentials.
4. **Does the Spotify Web API permit playback the way Spotirust implements it?**
   **Yes, for Spotify Connect Remote Mode.** All endpoints used are official `/me/player/*` endpoints, enforcing Spotify Premium (`403 PREMIUM_REQUIRED`), active device verification (`404 NO_ACTIVE_DEVICE`), and strict HTTP `429 Retry-After` cooldowns.
5. **Which device do Play, Pause, Seek, Next, Previous, Volume, and Queue act on?**
   - **`📡 Connect` Mode:** Every transport/volume/queue command acts on the displayed external Spotify Connect device, and never shows `Playing` before `GET /me/player` confirms `is_playing == true`. If the device disappears mid-playback (`204 No Content`) or polling repeatedly fails, `PlaybackClock` freezes and marks itself `stale` immediately.
   - **`🎵 Local` Mode:** Every action targets the local `rodio` audio sink and local `PlaybackQueue`, persisting state on exit and resuming cleanly on restart.

---

## 3. Release Candidate Regression & Test Summary

All mandatory build, lint, test, live integration, and packaging checks pass cleanly:
- `cargo fmt --all -- --check` — **PASS**
- `cargo check --all-targets` — **PASS**
- `cargo clippy --all-targets -- -D warnings` — **PASS** (0 warnings)
- `cargo clippy --all-targets --features librespot-backend -- -D warnings` — **PASS** (0 warnings)
- `cargo test --all-targets` — **PASS** (`48` unit + `24` offline integration = **72 passed, 0 failed**, `1` live smoke test ignored by default)
- `cargo test --all-targets --features librespot-backend` — **PASS** (**72 passed, 0 failed**)
- `cargo test --test integration_tests live_real_lyrics_providers_smoke_test -- --ignored --nocapture` — **PASS** (live **Unison** word TTML, live **AMLL TTML DB** CC0 Spotify-ID word TTML, and live **LRCLIB** `Lyricsfile v1.0` word YAML verified in `0.95s`)
- `dbus-run-session -- cargo test --test integration_tests linux_mpris_dbus_live_roundtrip` — **PASS** (live D-Bus MPRIS2 verified)
- Live PulseAudio virtual sink `local_audio_end_of_track_auto_advance_resume_and_seek` — **PASS**
- `cargo build --release` — **PASS** (`29M` stripped release binary)
- `python3 scripts/benchmark_gui.py` — **PASS** (`83.97%` total / `5.00%` Rust UI / `0.00%` post-stop idle — zero regression)
- `./scripts/package_portable.sh` — **PASS** (portable Linux `.tar.gz` built)

---

## 4. Remaining External Verification Before General Public Release

1. **Live Spotify Premium Account Verification (External Dependency):** Execute full OAuth PKCE browser consent, token refresh/rotation, `/me/*` library/search, and Spotify Connect playback transfer/control against live `api.spotify.com` with a real Spotify Premium account and physical Connect target.
2. **Physical Desktop Hardware Testing (Linux GPU, Windows 11, macOS):** Measure hardware-GPU karaoke CPU/RAM, verify native OS keychains (`Credential Manager`, `macOS Keychain`, `GNOME Keyring`), verify native `rfd` file dialogs across desktop portals/Win32/AppKit, and test real `.mp3`/`.flac`/`.ogg` binary playback on physical speakers (`PipeWire`, `WASAPI`, `CoreAudio`).
3. **Windows SMTC & macOS Now Playing Backends:** Implement native `MediaControls` backends for Windows (`SystemMediaTransportControls`) and macOS (`MPNowPlayingInfoCenter`) on top of `SystemMediaBridge` once physical Windows/macOS test hosts are available.
4. **Live LRCLIB Verification:** Test online lyrics lookup against live `https://lrclib.net` on an unrestricted network host.
5. **Spotify Extended Quota Mode (Policy Gate):** Public distribution to >5 arbitrary Spotify users requires an organization approved by Spotify for Extended Quota Mode (≥ 250k MAU). Self-hosted/personal builds work with a maintainer's own `SPOTIRUST_CLIENT_ID`.
