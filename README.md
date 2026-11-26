# Spotirust

**Spotirust** is a native, cross-platform desktop music application written in pure Rust (`egui`/`eframe` + `tokio`), inspired by the layout and ergonomics of Spotify Desktop — with **zero Electron / Chromium / WebView** overhead, a complete **Local Music Player**, **Spotify Connect Remote Control**, **Linux MPRIS2 integration**, and an optimized **word-by-word karaoke lyrics engine (TTML & Enhanced LRC)**.

> **Honesty & Platform Scope Notice:** Spotirust is an independent open-source project. It does not bundle copyrighted Spotify logos or assets, never bypasses DRM, never scrapes browser tokens, and never simulates fake playback. Read [`PRODUCTION_READINESS.md`](PRODUCTION_READINESS.md) and [`PERFORMANCE_REPORT.md`](PERFORMANCE_REPORT.md) for the complete verification matrix and reproducible benchmarks.

---

## Two Honest Operating Modes

Spotirust strictly separates remote Spotify control from native local playback and **never** advances a fake progress bar or reports `Playing` without real audio or remote device confirmation:

1. **Spotify Connect Remote Controller (`📡 Connect`):**
   - Controls an active external Spotify Connect device (official Spotify Desktop/Mobile app, hardware speaker, or **Spotify Soloist**) via official Spotify Web API `/me/player/*` endpoints.
   - Distinguishes **Active**, **Available**, and **Restricted** devices; waits for `GET /me/player` confirmation before showing `Playing`; detects device disconnection mid-playback; and strictly enforces HTTP `429 Retry-After` cooldowns without API spam.
   - **Requires Spotify Premium** and an active external Connect target (Spotify Web API does not stream audio bytes directly to third-party apps).
2. **Local Music Player (`🎵 Local` — `rodio 0.21` + `lofty 0.25` + `rfd 0.18`):**
   - Native OS **Select Folder** and **Add Files** pickers (`rfd`) plus manual path input fallback, non-blocking background recursive scanning, and explicit **Rescan**.
   - Real metadata & embedded cover artwork extraction via `lofty 0.25` (title, artist, album, duration, embedded cover art persisted as `localart://` thumbnails).
   - Persisted in SQLite (`schema v2`) with file size/mtime fingerprinting so startup loads instantly without rescanning folders every launch. Safely skips corrupt/truncated files.
   - Compiled `rodio`/`symphonia` decoders for **WAV** *(runtime-verified)*, **MP3**, **FLAC**, and **OGG Vorbis** *(decoders compiled; see [`PRODUCTION_READINESS.md`](PRODUCTION_READINESS.md))*, with Play All, Shuffle Play, Repeat, Queue persistence, and restart position restore.
   - Includes two locally synthesized royalty-free `.wav` Soundcheck tracks paired with bundled `.ttml` and Enhanced `.lrc` files for offline testing.
3. **Optional Community `librespot 0.8` (`--features librespot-backend` — Currently Blocked):**
   - Compiles cleanly behind a feature flag (0 warnings), but starting **August 10, 2026** (`librespot-org/librespot#1737`), Spotify blocked third-party OAuth tokens on `login5`. Spotirust refuses to embed unauthorized first-party credentials.

---

## Highlights

- **Optimized Native Rust GUI (`eframe 0.37` / `egui 0.37`):**
  - Cached `Arc<egui::Galley>` karaoke renderer (`18.5 µs` uncached → `16.3 ns` cached hit = `1,135×` faster layout), separated `KaraokeAnimationState`, single-pass opaque ambient gradients, and adaptive repaint scheduling that cut Fullscreen Karaoke CPU by **51.5%** (`179.10%` → `86.78%` on Mesa software GL; `0.00%` CPU when stopped/idle) while keeping exact millisecond word-boundary timing.
  - Bounded artwork caches: `64 MiB` / `512`-file LRU disk cache + true LRU GPU texture cache (`MAX_GPU_TEXTURES = 96`) with in-flight deduplication and bounded failed-URL tracking.
- **OS Media Integration (`src/system/media_controls.rs`):**
  - **Linux:** Native **MPRIS2** D-Bus server (`org.mpris.MediaPlayer2.spotirust` via `zbus 5.19`) verified live under `dbus-run-session`, supporting Play/Pause/Stop/Next/Previous/Seek/Volume and live metadata (`xesam:title`, `xesam:artist`, `xesam:album`, `mpris:length`, `mpris:artUrl`).
  - **Cross-platform keyboard shortcuts:** `Space` (Play/Pause), `Ctrl+Right/Left` (Next/Previous), `Right/Left` (±5s seek), `Ctrl+Up/Down` (±5% volume).
  - *Windows SMTC and macOS Now Playing are not yet implemented and are reported honestly as unsupported on those platforms.*
- **Official Browser OAuth 2.0 + PKCE (`127.0.0.1` loopback):**
  - Constant-time CSRF `state`, single-flight token refresh lock (safe for rotated refresh tokens), automatic `invalid_grant` cleanup, OS credential vault (`keyring 4.2`), zero Client Secret in binary, zero tokens in SQLite/logs.
- **Real Multi-Provider Word-by-Word Karaoke Lyrics & High-Precision Playback Clock (`src/audio/clock.rs`, `src/lyrics/`):**
  - Monotonic `PlaybackClock` with half-RTT latency compensation, tiered drift correction (`<= 35ms` deadband, `35..=450ms` linear soft blend over `400ms`, `> 450ms` / explicit seek immediate hard snap), track-change reset + async lyrics cancellation, and stale/disconnected freeze guards.
  - Real, audited online & local providers prioritizing **Word-level → Line-level → Plain** without faking timestamps or spamming providers:
    - **Unison (`unison.betterlyrics.org`)** — Free MIT-permitted word/syllable-level TTML API (`?isrc`, `?spotify`, `?song`+`artist`, `/lyrics/search`, ETag `304`, mandatory song + curator attribution).
    - **AMLL TTML DB (`amll-dev/amll-ttml-db`)** — CC0-1.0 community hand-timed word-level TTML indexed directly by Spotify Track ID.
    - **LRCLIB (`lrclib.net`)** — Public API supporting CC0 `Lyricsfile v1.0` YAML word-level timing, `/api/v2/search` + `/api/get/{id}`, LRC, and plain lyrics.
    - **Musixmatch Official Developer API** — Optional official `api.musixmatch.com/ws/1.1/` client (`RichSync` word-level JSON → `Subtitle` LRC → `Plain`, API key stored strictly in OS keyring).
    - **Local Files** — Sidecar/folder/one-click import for `.ttml`, `.lyricsfile`, and `.lrc` files, cached in SQLite (`schema v3`) with attribution.
  - **Blocked / Excluded Sources (Honesty Guarantee):** Official Apple Music API (`FB25664234`) and BiniLyrics (unlicensed Apple Music scraping proxy) are documented honestly as `BLOCKED_BY_ACCESS` and never faked or mislabeled (see [`docs/LYRICS_PROVIDERS.md`](docs/LYRICS_PROVIDERS.md) & [`docs/APPLE_MUSIC_LYRICS_RESEARCH.md`](docs/APPLE_MUSIC_LYRICS_RESEARCH.md)).
- **Extensible Themes & Plugins + Portable Mode:**
  - 5 built-in themes + drop-in JSON themes (`assets/themes/`), trait-based `PluginRegistry`, and self-contained portable mode (`--portable`, `SPOTIRUST_PORTABLE=1`, or `portable.flag`).

---

## Documentation

- **[`docs/LYRICS_PROVIDERS.md`](docs/LYRICS_PROVIDERS.md)** — Complete lyrics provider audit, licensing/ToS table (`Unison`, `AMLL TTML DB`, `LRCLIB`, `Musixmatch Official`, `BiniLyrics`, `Apple Music`), fallback architecture, and live verification evidence.
- **[`docs/APPLE_MUSIC_LYRICS_RESEARCH.md`](docs/APPLE_MUSIC_LYRICS_RESEARCH.md)** — Official Apple Music API, MusicKit, TTML schema, licensing/ToS analysis, and legal/technical verdict on Apple Music word-level lyrics.
- **[`PRODUCTION_READINESS.md`](PRODUCTION_READINESS.md)** — Full Release Candidate audit matrix (`VERIFIED`/`TESTED`/`PARTIAL`/`BLOCKED`/`UNVERIFIED`), Spotify playback Q&A, and full regression test summary.
- **[`PERFORMANCE_REPORT.md`](PERFORMANCE_REPORT.md)** — Reproducible Before vs After benchmarks, per-thread CPU breakdown (`Rust UI`, `Audio`, `Tokio`, `Mesa swGL`), layout cache micro-benchmarks, and environment disclosures.
- **[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)** — System architecture, capability matrix, and dependency license audit (100% permissive direct dependencies).
- **[`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md)** — Non-Rust portable install guide, runtime dependencies, exact DB/cache/config paths, Spotify Soloist notes, and complete uninstall / data reset instructions.

---

## Quick Start (Without Installing Rust)

Download or extract the Linux portable archive (`spotirust-linux-x86_64-portable.tar.gz`):

```bash
tar -xzf spotirust-linux-x86_64-portable.tar.gz
cd spotirust
./spotirust
```

Portable mode keeps all SQLite data, local library index, artwork cache, themes, and lyrics inside `./spotirust-data/`. See [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md) for Linux runtime dependencies, standard XDG/macOS/Windows paths, and full uninstall/reset steps. *(Windows and macOS installers/archives are not yet verified on native Windows/macOS machines.)*

---

## Building & Running from Source

### Prerequisites

- **Rust** `1.95+` (Edition 2024)
- **Linux (Ubuntu / Debian):**
  ```bash
  sudo apt-get update
  sudo apt-get install -y libasound2-dev pkg-config libxkbcommon-dev libwayland-dev libgl1-mesa-dev
  ```
- **macOS:** Xcode Command Line Tools (`xcode-select --install`)
- **Windows:** Visual Studio Build Tools with C++ Desktop workload

### Build, Run & Package

```bash
# Default build (Native GUI + Spotify Web API + Local Rodio/Lofty/RFD + Lyrics + Linux MPRIS2)
cargo build --release

# Optional: include feature-gated librespot 0.8 module (see blocked status in PRODUCTION_READINESS.md)
cargo build --release --features librespot-backend

# Launch desktop GUI (or add `-- --portable`)
cargo run --release

# Run headless parser, SQLite v2 & KaraokeLayoutCache micro-benchmarks
cargo run --release -- --diagnostics

# Build Linux portable archive (target/portable/spotirust-linux-x86_64-portable.tar.gz)
./scripts/package_portable.sh
```

---

## Spotify OAuth Configuration & 2026 Development Mode Limits (Maintainers)

Spotify requires a registered OAuth `client_id` for PKCE. Spotirust intentionally does **not** display a Client ID input box to end users and **never** embeds a Client Secret.

Under Spotify's February–July 2026 Developer Policy, Development Mode requires an active **Spotify Premium** account for the app owner and caps each Client ID at **5 allowlisted test users** (`/search` `limit <= 10`, owned/collaborated playlist items only). Public unlimited distribution requires **Extended Quota Mode** (registered organization, ≥ 250,000 MAU). For personal/team builds:

1. Create an app in the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard).
2. Add Redirect URI `http://127.0.0.1/callback` (`localhost` is banned since Nov 2025), or fixed port `http://127.0.0.1:43821/callback` with `SPOTIRUST_REDIRECT_PORT=43821`.
3. Add up to 5 test accounts under **User Management**.
4. Bake the public Client ID once at build time:
   ```bash
   SPOTIRUST_CLIENT_ID="your_public_spotify_client_id" cargo build --release
   ```

---

## Measured Performance (Before vs After — Release Build, Linux Xvfb + Mesa swGL)

Measured via [`scripts/benchmark_gui.py`](scripts/benchmark_gui.py) on `Linux 6.18.57 x86_64` (2 vCPU Intel Xeon 2.30 GHz, 7.8 GiB RAM, `Xvfb` 1280×800, Mesa LLVMpipe software GL, timer-paced PulseAudio `module-null-sink` — **NOT physical GPU/desktop/speakers**). Full report in [`PERFORMANCE_REPORT.md`](PERFORMANCE_REPORT.md):

| Metric | BEFORE | AFTER (Post-Sync RC) | Notes |
|---|---:|---:|---|
| Release binary size (stripped, LTO) | `25.95 MB` | **`28.30 MB`** | Added `lofty`, `rfd`, `zbus`, `PlaybackClock`, lyrics providers |
| Cold GUI startup (UI init / X11 mapped, 3-run avg) | `258.7 ms` / `158.3 ms` | **`264.7 ms` / `146.0 ms`** | Real X11 window creation + SQLite v2 |
| Idle Home / Post-Stop Idle CPU | `0.17%` / `0.20%` | **`0.17%` / `0.00%`** | `0.00%` Rust UI, Audio, Tokio & Mesa swGL |
| Playing Local WAV (Bottom Bar, 4 Hz) | `18.33%` (`109.19 MB`) | **`19.33%` (`111.02 MB`)** | Rust UI `1.50%` · Audio `0.50%` · Mesa swGL `16.33%` |
| **Playing + Fullscreen Karaoke (Total CPU)** | **`179.10%`** | **`83.97%` (-53.1%)** | **Rust UI `7.50% → 5.00%` · Mesa swGL `170.27% → 77.30%`** |
| 30s Continuous Playback RAM Delta | `+0.05 MB` | **`+0.15 MB`** | Zero leak across track auto-advance |
| `KaraokeLayoutCache` (`--diagnostics`) | — | **`18.5 µs` → `16.3 ns`** | **1,135× faster** cached `Arc<Galley>` hit (0 allocs) |

*(Hardware GPU Linux, Windows 11, macOS, and live Spotify Connect GUI benchmarks are not yet verified.)*

---

## Testing & Verification

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features librespot-backend -- -D warnings
cargo test --all-targets
cargo test --all-targets --features librespot-backend
dbus-run-session -- cargo test --test integration_tests linux_mpris_dbus_live_roundtrip
./scripts/package_portable.sh
```

---

## License

Licensed under the [MIT License](LICENSE).
