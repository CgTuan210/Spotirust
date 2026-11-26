# Spotirust

**Spotirust** is a native, cross-platform desktop music application written in pure Rust (`egui`/`eframe` + `tokio`), inspired by the layout and ergonomics of Spotify Desktop — with **zero Electron / Chromium / WebView** overhead, a complete **Local Music Player**, **Spotify Connect Remote Control**, **Linux MPRIS2 integration**, and an optimized **word-by-word karaoke lyrics engine (TTML & Enhanced LRC)**.

> **Honesty & Platform Scope Notice:** Spotirust is an independent open-source project. It does not bundle copyrighted Spotify logos or assets, never bypasses DRM, never scrapes browser tokens, and never simulates fake playback. Read [`PRODUCTION_READINESS.md`](PRODUCTION_READINESS.md) and [`PERFORMANCE_REPORT.md`](PERFORMANCE_REPORT.md) for the complete verification matrix and reproducible benchmarks.

---

## Three Honest Operating Modes

Spotirust strictly separates standalone headless Spotify playback, remote Spotify control, and native local playback — and **never** advances a fake progress bar or reports `Playing` without real audio or backend confirmation:

1. **Spotify Soloist Built-in Headless Engine (`⚡ Soloist` — Recommended for Standalone Spotify):**
   - Supervises Spotify's official headless **Spotify Soloist** engine locally (`soloist --ws 127.0.0.1:0`) with one-click official CDN download/update (never redistributing Spotify's proprietary binary), OS-keyring personal API key storage, and zero-polling real-time WebSocket playback & karaoke clock sync.
   - Pair **once** from the official Spotify app on the same network, then **close the official Spotify app permanently** — Spotirust keeps playing full Spotify tracks natively on your computer.
   - **Requires Spotify Premium** and your own personal Soloist API key (`https://developer.spotify.com/dashboard/soloist`). Automatically detects 90-day build expiry (exit code `10`).
2. **Spotify Connect Remote Controller (`📡 Connect`):**
   - Controls an active external Spotify Connect device (official Spotify Desktop/Mobile app, hardware speaker, or external Soloist instance) via official Spotify Web API `/me/player/*` endpoints.
   - Automatically resolves an available non-restricted device from `/me/player/devices` if no active device is cached, distinguishes **Active**, **Available**, and **Restricted** devices, confirms state via `GET /me/player`, detects device disconnection mid-playback, and strictly enforces HTTP `429 Retry-After` cooldowns.
   - **Requires Spotify Premium** and an active external Connect target.
3. **Local Music Player (`🎵 Local` — `rodio 0.21` + `lofty 0.25` + `rfd 0.18`):**
   - Native OS **Select Folder** and **Add Files** pickers (`rfd`) plus manual path input fallback, non-blocking background recursive scanning, and explicit **Rescan**.
   - Real metadata & embedded cover artwork extraction via `lofty 0.25` (title, artist, album, duration, embedded cover art persisted as `localart://` thumbnails).
   - Persisted in SQLite (`schema v2`) with file size/mtime fingerprinting so startup loads instantly without rescanning folders every launch. Safely skips corrupt/truncated files.
   - Compiled `rodio`/`symphonia` decoders for **WAV** *(runtime-verified)*, **MP3**, **FLAC**, and **OGG Vorbis** *(decoders compiled; see [`PRODUCTION_READINESS.md`](PRODUCTION_READINESS.md))*, with Play All, Shuffle Play, Repeat, Queue persistence, and restart position restore.
   - Includes two locally synthesized royalty-free `.wav` Soundcheck tracks paired with bundled `.ttml` and Enhanced `.lrc` files for offline testing.
4. **Optional Community `librespot 0.8` (`--features librespot-backend` — Currently Blocked):**
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

Download or extract the portable archive (`spotirust-linux-x86_64-portable.tar.gz`, `spotirust-macos-arm64-portable.tar.gz`, or `spotirust-windows-x86_64-portable.zip`):

```bash
tar -xzf spotirust-linux-x86_64-portable.tar.gz
cd spotirust
./spotirust
```

Portable mode keeps all SQLite data, local library index, artwork cache, themes, lyrics, and managed Soloist data inside `./spotirust-data/`. See [`docs/TROUBLESHOOTING.md`](docs/TROUBLESHOOTING.md) for Linux runtime dependencies, standard XDG/macOS/Windows paths, and full uninstall/reset steps. *(Windows and macOS installers/archives are built in CI and not yet verified on physical Windows/macOS machines.)*

---

## User Setup Guide (Playing Spotify & Self-Hosting)

### What Do You Need?

| Mode | Spotify Premium? | Spotify OAuth Client ID? | Personal Soloist API Key? | Keeps Official Spotify App Open? |
|---|:---:|:---:|:---:|:---:|
| **`🎵 Local`** *(MP3/FLAC/OGG/WAV + Karaoke)* | **No** | **No** | **No** | **No** |
| **`⚡ Soloist`** *(Recommended for Spotify)* | **Yes** | **Yes** *(for browsing/search/library)* | **Yes** *(each user generates their own)* | **No** *(pair once, then close it permanently)* |
| **`📡 Connect`** *(Remote Control)* | **Yes** | **Yes** | **No** | **Yes** *(or a Connect speaker)* |

### Step 1 — Sign In to Spotify (For New Users / Self-Hosting)

Under Spotify's 2026 Developer Policy, a Development Mode `client_id` is capped at **5 allowlisted users** (public unlimited apps require Extended Quota Mode: registered business + ≥ 250,000 MAU).

- **If you are on the maintainer's allowlist** (or using an Extended Quota build): simply launch Spotirust and click **Login with Spotify**.
- **If you are setting up Spotirust for yourself (takes ~2 minutes, no Rust required for prebuilt binaries):**
  1. Open the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard) *(requires an active Spotify Premium account)* and click **Create app**.
  2. Add Redirect URI: `http://127.0.0.1/callback` *(do **not** use `localhost` — Spotify permanently banned `localhost` redirect URIs on Nov 27, 2025)*.
     - *Optional fixed port:* use `http://127.0.0.1:43821/callback` and set `SPOTIRUST_REDIRECT_PORT=43821`.
  3. Copy your app's **Client ID** *(Spotirust uses PKCE and never needs or stores your Client Secret)*.
  4. Launch Spotirust with your Client ID via environment variable (overrides any baked-in default):
     - **Linux / macOS:**
       ```bash
       SPOTIFY_CLIENT_ID="your_spotify_client_id" ./spotirust
       ```
     - **Windows (PowerShell):**
       ```powershell
       $env:SPOTIFY_CLIENT_ID = "your_spotify_client_id"
       .\spotirust.exe
       ```
     - **Or bake it once when building from source:**
       ```bash
       SPOTIFY_CLIENT_ID="your_spotify_client_id" cargo build --release
       ```
  5. Click **Login with Spotify** inside Spotirust — your browser opens Spotify's official consent page and returns automatically to `127.0.0.1`. Your OAuth session is stored strictly in your OS credential vault (never in SQLite).

### Step 2 — Set Up `⚡ Soloist` (Play Spotify Without Keeping the Official App Open)

Because Spotify prohibits redistributing its proprietary Soloist binary or sharing Soloist API keys across users, each user completes this quick one-time setup inside Spotirust:

1. Open **Settings → Spotify Soloist — Built-in Headless Engine**.
2. **Personal Soloist API Key:** Click **Get Key on Spotify Dashboard ↗** ([`developer.spotify.com/dashboard/soloist`](https://developer.spotify.com/dashboard/soloist), requires **Spotify Premium**), generate/copy your personal key, paste it into Spotirust, and click **Save Key**.
   - Stored strictly in your OS Keyring (`com.spotirust.desktop` / `spotify_soloist_api_key`), never written to SQLite or logs. *(You may also pass it via `SOLOIST_API_KEY` env var.)*
3. **Install Official Soloist Binary:** Click **Download & Install Soloist** — Spotirust downloads the official build directly from Spotify's CDN (`soloist-builds.spotifycdn.com`), verifies its executable header, and installs it locally (or click **Browse Existing Binary…** if you downloaded it manually).
   - *Official Soloist platforms:* Linux (`x86_64`, `aarch64`, `armv7`), macOS Apple Silicon (`arm64`), and Windows (`x86_64`). *(Intel macOS users can use `📡 Connect` or `🎵 Local`.)*
4. **One-Time Pairing:** Click **Start Soloist**. Open the official Spotify app (phone or PC on the same Wi-Fi) **once**, open **Connect to a device**, and select **`Spotirust`**.
5. Once Spotirust shows **`● Connected & Paired`**, you can **close the official Spotify app permanently**. On future launches, Spotirust auto-starts Soloist in the background (`--ws 127.0.0.1:0`) and plays full Spotify tracks directly with zero-polling karaoke sync.
   - **90-Day Build Updates:** Official Soloist builds expire after 90 days (`exit code 10`). Spotirust detects this automatically and shows a one-click **Update Soloist** button without losing your paired session.

### Alternative — Using `📡 Connect` or `🎵 Local` Only

- **`📡 Connect`:** Keep the official Spotify app or a Connect speaker open, switch the top-bar pill to **`📡 Connect`**, and play any track. Spotirust automatically discovers an available non-restricted device via `/me/player/devices` (or pick a device from the **🔊 Connect Devices** menu).
- **`🎵 Local`:** Switch to **`🎵 Local`**, open **Local Files**, add a music folder (`MP3`, `FLAC`, `OGG`, `WAV`), and play offline with word-by-word karaoke lyrics — no Spotify account required.

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
# Default build (Native GUI + Spotify Web API + Soloist + Local Rodio/Lofty/RFD + Lyrics + Linux MPRIS2)
cargo build --release

# Optional: include feature-gated librespot 0.8 module (see blocked status in PRODUCTION_READINESS.md)
cargo build --release --features librespot-backend

# Launch desktop GUI (or add `-- --portable`)
cargo run --release

# Run headless parser, SQLite v3 & KaraokeLayoutCache micro-benchmarks
cargo run --release -- --diagnostics

# Build Linux portable archive (target/portable/spotirust-linux-x86_64-portable.tar.gz)
./scripts/package_portable.sh
```

---

## Maintainer Notes: Spotify 2026 Development Mode Limits

- Spotirust intentionally does **not** display a Client ID input box in the end-user GUI and **never** embeds a Client Secret.
- Maintainers can bake a default public Client ID at compile time with `SPOTIRUST_CLIENT_ID="..." cargo build --release` (or leave it unset in public CI artifacts so each self-hosted user passes their own `SPOTIFY_CLIENT_ID` at launch).
- In Development Mode, Spotify limits each Client ID to **5 allowlisted accounts** under **User Management**, caps `/search` at `limit <= 10`, and restricts `/playlists/{id}/items` to owned/collaborated playlists.

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
