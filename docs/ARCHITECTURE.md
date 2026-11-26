# Spotirust — System Architecture, Capability Matrix & License Audit

> **Status:** Living Architecture Specification (Updated November 2026)  
> **Project:** Spotirust — Native Cross-Platform Spotify Desktop Client in Rust  
> **License:** MIT

---

## 1. Design Principles & Non-Negotiable Rules

Spotirust is an independent, native desktop music application written in Rust (`egui`/`eframe` + `tokio`), inspired by the desktop layout and ergonomics of Spotify Desktop, while maintaining its own branding, clean modular architecture, and strict compliance with platform security and licensing boundaries.

1. **Pure Native Rust, Zero Electron/WebView:** No Chromium, no embedded browser shell for the app UI.
2. **Official Browser OAuth 2.0 + PKCE Only:**
   - Users authenticate exclusively on Spotify's official web login page via the OS default browser.
   - No password prompt, no browser cookie scraping, no manual access token paste, no Client Secret embedded in the binary.
   - Spotify requires a registered `client_id` even for PKCE. Spotirust embeds an optional maintainer-configured `SPOTIRUST_CLIENT_ID` at compile time (`option_env!`) with runtime env-var fallback for developer/self-hosted builds, never exposing a Client ID input field to end users.
3. **Strict Honesty & Verifiability:**
   - Every feature is categorized transparently in the Capability Matrix below.
   - Live Spotify authentication and streaming require a real Spotify account (and Spotify Premium for playback APIs / `librespot` / Development Mode app ownership) and are clearly marked as unverified against live Spotify servers in CI/offline environments.
4. **Zero Plaintext Secrets in SQLite:** Tokens and session secrets are stored in the OS credential store (Windows Credential Manager, macOS Keychain, Linux Secret Service via `keyring`), never in SQLite or logs.

---

## 2. Spotify Platform Reality Check (2025–2026)

Before writing code, all current Spotify Developer documentation and `librespot` upstream behavior were audited:

### 2.1 OAuth 2.0 & Redirect URI Rules (Enforced Nov 27, 2025)
- **Implicit Grant Flow:** Permanently removed. Only **Authorization Code Flow with PKCE** is valid for native desktop apps.
- **Redirect URIs:** `localhost` aliases and non-loopback `http://` URIs are rejected. Explicit loopback IP literals over HTTP (`http://127.0.0.1:<PORT>/callback` and `http://[::1]:<PORT>/callback`) remain officially supported, including dynamic port assignment when registered without a port (`http://127.0.0.1/callback`).
- **Refresh Tokens (June 2026):** Refresh tokens may rotate or expire; Spotirust persists any rotated `refresh_token` returned on refresh and handles `invalid_grant` by cleanly transitioning to re-authentication.

### 2.2 Spotify Web API Quota & Endpoint Changes (Nov 2024 – Jul 2026)
- **Development Mode Restrictions (Feb 2026, updated Jul 2026):**
  - App owner must maintain an active **Spotify Premium** subscription.
  - Up to **5 authorized allowlist users** per Client ID (up to 25 Client IDs per developer as of Jul 2026, sharing per-account rate quota).
  - **Extended Quota Mode** (for general public distribution) requires a legally registered business entity and ≥ 250,000 MAU.
  - *Implication for open-source distribution:* A maintainer cannot legally share a single hobbyist Development Mode Client ID with unlimited public users. Valid paths are: (a) an organization applying for Extended Quota Mode, or (b) self-hosted / personal builds compiling with `SPOTIRUST_CLIENT_ID=<your-dev-app-id>`.
- **Removed Endpoints (Nov 2024 & Feb 2026 Dev Mode):**
  - Removed: `/recommendations`, `/audio-features`, `/audio-analysis`, `/browse/featured-playlists`, `/browse/new-releases`, `/browse/categories`, `/artists/{id}/related-artists`, `/artists/{id}/top-tracks`, batch endpoints (`/tracks`, `/albums`, `/artists`), `/users/{id}`, `/users/{id}/playlists`.
  - Removed `User` fields: `product`, `email`, `country`, `followers`. Added `account_id` (May 2026).
  - **Reverted (March 2026):** `external_ids` (including **ISRC**) remains available on Track and Album objects — crucial for exact lyrics matching.
- **Modernized Endpoints (Feb 2026):**
  - Library mutations unified under `PUT /me/library`, `DELETE /me/library`, and `GET /me/library/contains` using Spotify URIs (`spotify:track:...`).
  - Playlist items migrated from `/playlists/{id}/tracks` to `/playlists/{id}/items` (note: in Dev Mode, only playlists owned or collaborated on by the user expose items).
  - `GET /search` maximum `limit` is `10` (default `5`) in Dev Mode. Spotirust respects `limit <= 10` by default to remain compatible with both Dev Mode and Extended Quota Mode.

### 2.3 Audio Playback Reality (`librespot` vs Connect vs Soloist vs Local)
- **Spotify Web API Player Control (`/me/player/*`):** Official, requires Spotify Premium, controls any active Spotify Connect device (official app, hardware speaker, or Spotify Soloist). Does not deliver raw audio streams directly to third-party apps.
- **Spotify Soloist (Launched Aug 2026; macOS/Windows Oct 2026):** Official headless Spotify Connect receiver with a local WebSocket control/queue API and lossless support. Requires each user to generate a personal Soloist API key and download Spotify's official binary (cannot be redistributed, expires after 90 days).
- **`librespot` 0.8.0 (Community, MIT):** Unofficial open-source Rust Spotify playback & Connect engine. Requires **Spotify Premium**.
  - **August 2026 Limitation (`librespot-org/librespot#1737`):** Spotify closed `login5` playback token issuance for third-party developer Client IDs (`FaultyRequest(INVALID_CREDENTIALS)`). Passing a regular third-party Web API OAuth token to `librespot` cannot initialize playback without Spotify's private/official client flow or ZeroConf Connect credentials. Spotirust **refuses** to embed someone else's official Client ID; therefore `librespot` is integrated cleanly behind the `librespot-backend` feature flag using valid session/stored credentials, with its upstream limitation documented clearly.
- **Local Audio (`rodio`):** Optional native audio playback for local `.mp3`/`.flac`/`.ogg`/`.wav` files and bundled demo playback, giving immediate offline verification of real audio decoding, seeking, volume, and word-by-word lyrics sync.

---

## 3. Capability Matrix

| Feature Area | Implementation in Spotirust | Category | Spotify Premium Required? | Status / Constraints |
|---|---|---|---|---|
| **Browser OAuth 2.0 + PKCE (`127.0.0.1`)** | `src/auth/oauth.rs` + `callback.rs` | Official | Owner: Yes (Dev Mode) | Requires build/env `SPOTIRUST_CLIENT_ID`; live login requires real Spotify account |
| **OS Keychain Session Storage** | `src/storage/credentials.rs` (`keyring`) | Official / OS | No | Windows Credential Manager, macOS Keychain, Linux Secret Service |
| **User Profile (`GET /me`)** | `src/spotify/client.rs` | Official | Dev Mode owner | Uses `account_id` + `id` fallback per May 2026 API |
| **Search (Tracks/Artists/Albums/Playlists)** | `src/spotify/search.rs` | Official | Dev Mode owner | Debounced, cached, `limit <= 10` for Feb 2026 Dev Mode compatibility |
| **Library (Saved Tracks, Albums, Followed Artists, Playlists)** | `src/spotify/library.rs`, `playlists.rs` | Official | Dev Mode owner | Supports both Feb 2026 `/me/library` + `/playlists/{id}/items` and legacy fallbacks |
| **Home Feed (Recently Played, Top Items, Saved)** | `src/spotify/library.rs` | Official | Dev Mode owner | Built from surviving endpoints (`recently-played`, `me/top`, `me/playlists`, `me/albums`) since `/recommendations` & `/browse/*` were removed |
| **Playback Control via Spotify Connect API** | `src/audio/connect.rs` (`/me/player/*`) | Official | **Yes** | Controls any active Connect target (Desktop app, speaker, Spotify Soloist) with automatic `/me/player/devices` fallback |
| **Built-in Spotify Soloist Headless Engine (`--ws 127.0.0.1:0`)** | `src/audio/soloist.rs` + `src/audio/playback.rs` | Official (User-Fetched Binary) | **Yes** | Downloads official CDN binary on user action (never redistributed), stores user's personal API key strictly in OS keyring, supervises local loopback WebSocket for zero-polling real-time playback & karaoke sync even after closing the official Spotify app |
| **Direct Streaming via `librespot` 0.8** | `src/audio/librespot_engine.rs` (`--features librespot-backend`) | Community / Unofficial | **Yes** | Compiled & integrated; blocked on third-party OAuth tokens since Aug 2026 (`librespot#1737`) without ZeroConf/stored session blob |
| **Local Audio Playback (`rodio`)** | `src/audio/local_rodio.rs` | Native / Offline | No | Plays real local MP3/FLAC/OGG/WAV files (plus locally synthesized Soundcheck WAVs) directly via OS audio sink with duration probing & `OutputStream` release on stop |
| **TTML Word-by-Word Parser (W3C + local AMLL-schema)** | `src/lyrics/ttml.rs` | Open Standard | No | Full word/syllable timing, `TtmlLine` fallback, `x-bg`, `x-translation`, `x-roman`, sidecar translations, missing-end & duplicate-timestamp inference, strict XML validation. **No Apple Music API integration.** |
| **Enhanced LRC & Standard LRC Parser** | `src/lyrics/lrc.rs` | Open Standard | No | `<mm:ss.xx>` word tags, `[mm:ss.xx]` line tags, shifted multi-timestamp expansion, `[offset:]` |
| **LRCLIB Public Lyrics Provider** | `src/lyrics/provider.rs` | Community / Open API | No | Free public API (`lrclib.net`), proper `User-Agent`, respects `Retry-After` |
| **Local `.ttml` / `.lrc` File Provider** | `src/lyrics/provider.rs` | Native / User-supplied | No | Loads user-provided TTML/LRC files from data dir or manual override import |
| **Anti-Remix/Live Track Matcher** | `src/lyrics/matcher.rs` | Native | No | Scores title/artist/album/duration/ISRC, rejects remix/live/acoustic mismatches |
| **Apple Music Private Lyrics Endpoint** | **Not Implemented** | Disallowed | N/A | Excluded by design; violates platform authorization rules |
| **Spotify Free Direct Audio / Ad Bypass / DRM Bypass** | **Not Implemented** | Infeasible / Disallowed | N/A | Strictly prohibited; violates DRM and Terms of Service |
| **SQLite Metadata & Lyrics Cache** | `src/storage/database.rs` + `migrations/` | Native | No | Versioned SQL migrations, WAL mode, zero plaintext tokens |
| **Theme & Extensibility System** | `src/ui/theme.rs` + `src/core/plugins.rs` | Native | No | Built-in presets + JSON custom themes + trait-based plugin registry |

---

## 4. High-Level Architecture

```text
+-------------------------------------------------------------------------+
|                           GUI Thread (eframe/egui)                      |
|  Sidebar | Top Nav (Soloist/Local/Connect) | Home | Search | Library    |
|  Player Bar (Device Status) | Now Playing / Karaoke | Settings & Themes |
+--------------------+----------------------------------+-----------------+
                     | AppCommand (mpsc)                ^ AppEvent (mpsc / broadcast)
                     v                                  |
+-------------------------------------------------------------------------+
|                     Async Runtime (Tokio Multi-Thread)                  |
|                                                                         |
|  +------------------+   +------------------+   +---------------------+  |
|  | Auth Manager     |   | Spotify Web API  |   | Lyrics Pipeline     |  |
|  | - PKCE (S256)    |   | - Rate Limit/429 |   | - SQLite Cache      |  |
|  | - 127.0.0.1 CB   |   | - ETag Cache     |   | - Local TTML/LRC    |  |
|  | - Single-Flight  |   | - Dev Mode 2026  |   | - LRCLIB Provider   |  |
|  | - OS Keyring     |   | - Debounce/Cache |   | - Strict Matcher    |  |
|  +------------------+   +------------------+   +---------------------+  |
|                                                                         |
|  +-------------------------------------------------------------------+  |
|  | AudioEngine Abstraction + Queue Manager + Event Bus               |  |
|  |  - SoloistEngine (Official Headless Daemon + ws://127.0.0.1:0)    |  |
|  |  - SpotifyConnectEngine (Official /me/player/* + Confirmation)    |  |
|  |  - LocalRodioPlayer (Real MP3/FLAC/OGG/WAV via rodio 0.21)        |  |
|  |  - LibrespotEngine (Feature: librespot-backend — Blocked Aug 2026)|  |
|  +-------------------------------------------------------------------+  |
|                                                                         |
|  +-------------------------------------------------------------------+  |
|  | Storage Layer                                                     |  |
|  |  - SQLite (rusqlite bundled, WAL, versioned migrations)           |  |
|  |  - OS Secure Credentials (keyring 4.2: Keychain/CredMan/SecretSvc)|  |
|  |  - Artwork Async Loader + Bounded Memory/Disk Cache               |  |
|  +-------------------------------------------------------------------+  |
+-------------------------------------------------------------------------+
```

### Threading & Responsiveness Invariants
- **Never block the UI thread:** All HTTP, SQLite, OS keyring, OAuth callback server, native file dialogs (`rfd::AsyncFileDialog`), `lofty` tag scanning, audio decoding, and image decoding run on the Tokio runtime or dedicated worker threads.
- **Event-driven wakeups (`ctx.request_repaint` + `next_karaoke_repaint_delay`):** When idle/paused, UI repaints only on user interaction or state events (`0%` CPU). During active playback, the progress bar updates at ~4 Hz (`250ms`), line-only/plain/gap karaoke sleeps until exact next boundaries, and active word-synced karaoke sweeps smoothly at ~30 FPS (`33ms`) locked to exact millisecond word boundaries with cached `Arc<egui::Galley>` layouts.
- **Zero-allocation position reads:** `PlaybackPosition` uses monotonic `Instant` interpolation between backend sync points so reading playback position every frame performs zero heap allocations and zero locks on network I/O.

---

## 5. Module Layout

```text
Spotirust/
├── Cargo.toml
├── docs/
│   ├── ARCHITECTURE.md         # This document
│   └── TROUBLESHOOTING.md      # Install/uninstall, runtime deps, OAuth, audio & lyrics guide
├── migrations/
│   ├── 0001_initial.sql        # SQLite schema v1 (profile, recent, cache, lyrics, settings)
│   └── 0002_local_library.sql  # SQLite schema v2 (persisted local_tracks with mtime/size cache)
├── scripts/
│   ├── benchmark_gui.py        # Reproducible Xvfb + PulseAudio benchmark & thread-CPU profiler
│   ├── package_portable.sh     # Linux/macOS portable tar.gz packager
│   └── package_portable.ps1    # Windows portable ZIP packager
├── assets/
│   ├── themes/                 # Built-in JSON theme examples
│   └── sample_lyrics/          # Royalty-free sample TTML & Enhanced LRC for local testing
├── src/
│   ├── main.rs                 # Entry point, tracing init, portable mode, eframe bootstrap
│   ├── lib.rs                  # Public library exports for integration tests & plugins
│   ├── app.rs                  # Main eframe::App orchestrator & command/event bridge
│   ├── core/
│   │   ├── mod.rs
│   │   ├── config.rs           # AppConfig, portable paths, build-time SPOTIRUST_CLIENT_ID
│   │   ├── events.rs           # AppCommand / AppEvent message bus
│   │   ├── models.rs           # Domain types (Track, Album, Artist, Playlist, UserProfile)
│   │   ├── plugins.rs          # Plugin & LyricsProvider/Theme extension registry
│   │   └── state.rs            # Centralized UI & session state with nav history
│   ├── auth/
│   │   ├── mod.rs
│   │   ├── oauth.rs            # PKCE S256 verifier/challenge, state validation, token exchange/refresh
│   │   ├── callback.rs         # Loopback 127.0.0.1 ephemeral-port HTTP callback listener
│   │   └── session.rs          # Session lifecycle, auto-restore, token expiry & rotation
│   ├── spotify/
│   │   ├── mod.rs
│   │   ├── client.rs           # reqwest client, bearer injection, strict 429 cooldown, ETag cache
│   │   ├── search.rs           # Debounced & cached catalog search (tracks/artists/albums/playlists)
│   │   ├── library.rs          # Profile, saved tracks/albums, followed artists, /me/library, top/recent
│   │   └── playlists.rs        # User playlists & /playlists/{id}/items management
│   ├── audio/
│   │   ├── mod.rs
│   │   ├── engine.rs           # async trait AudioEngine, PlaybackPosition, RepeatMode
│   │   ├── events.rs           # PlaybackEvent broadcast channel
│   │   ├── queue.rs            # Queue, history, Fisher-Yates shuffle, repeat modes
│   │   ├── playback.rs         # Unified PlaybackController (Soloist + Connect Remote + Local Rodio + Librespot)
│   │   ├── soloist.rs          # Official Spotify Soloist daemon supervisor, CDN downloader & local WS client
│   │   ├── connect.rs          # Official Spotify Connect Web API backend (/me/player/*)
│   │   ├── local_rodio.rs      # Native rodio player + lofty metadata/artwork + SQLite incremental scan
│   │   └── librespot_engine.rs # Optional librespot 0.8 backend (--features librespot-backend)
│   ├── lyrics/
│   │   ├── mod.rs
│   │   ├── model.rs            # LyricWord, LyricLine, LyricsDocument, TimingGranularity
│   │   ├── ttml.rs             # W3C + Apple/AMLL TTML parser (quick-xml)
│   │   ├── lrc.rs              # Enhanced LRC & Standard LRC parser
│   │   ├── matcher.rs          # Multi-factor track matcher rejecting remix/live mismatches
│   │   ├── provider.rs         # LyricsProvider trait, LocalFileProvider, LrclibProvider, ChainedProvider
│   │   └── cache.rs            # SQLite-backed lyrics cache with TTL & format preservation
│   ├── storage/
│   │   ├── mod.rs
│   │   ├── database.rs         # SQLite manager, versioned migrations (v1-v2), CRUD repositories
│   │   ├── credentials.rs      # OS Keychain / Credential Manager / Secret Service wrapper
│   │   └── artwork.rs          # Async image fetcher, bounded LRU disk cache, localart:// & dominant RGB
│   ├── system/
│   │   ├── mod.rs
│   │   └── media_controls.rs   # Decoupled SystemMediaBridge + Linux MPRIS2 D-Bus server (zbus)
│   └── ui/
│       ├── mod.rs
│       ├── theme.rs            # Theme tokens, presets (Obsidian, Midnight, Emerald, Light), JSON loader
│       ├── sidebar.rs          # Left navigation rail + playlists + auth badge
│       ├── topbar.rs           # Back/forward nav, search, Local/Connect switcher, device status menu
│       ├── home.rs             # Time-based greeting, recently played, top/recommended, albums
│       ├── search.rs           # Live debounced search, loading skeletons, tabbed results
│       ├── library.rs          # Liked songs, playlists, albums, artists, local files (rfd), detail views
│       ├── player.rs           # Persistent bottom player bar (transport, seek, volume, lyrics/queue)
│       ├── lyrics.rs           # Cached Arc<Galley> karaoke renderer + adaptive repaint scheduler
│       ├── settings.rs         # Theme picker, audio mode, lyrics dirs, diagnostics, cache/reset/logout
│       └── components/
│           ├── mod.rs
│           ├── cards.rs        # Media cards, track rows, skeleton placeholders
│           └── artwork.rs      # Bounded LRU GPU texture cache + opaque ambient gradient painter
└── tests/
    └── integration_tests.rs    # End-to-end tests across auth, SQLite, lyrics, audio, Connect, artwork & MPRIS2
```

---

## 6. Dependency & License Audit

All direct dependencies were audited via `cargo info` against crates.io (November 2026). Every direct dependency uses a permissive OSI-approved license compatible with Spotirust's **MIT License** (zero GPL/AGPL/MPL direct dependencies):

| Crate | Version | Purpose | License |
|---|---|---|---|
| `eframe` | `0.37.0` | Native cross-platform app shell (`glow` / optional `wgpu`, Wayland/X11/macOS/Win) | MIT OR Apache-2.0 |
| `egui` | `0.37.0` | Immediate-mode native Rust GUI | MIT OR Apache-2.0 |
| `tokio` | `1.54` | Async runtime (`rt-multi-thread`, `net`, `sync`, `time`, `fs`, `process`) | MIT |
| `tokio-tungstenite` | `0.28` | Local loopback WebSocket client for Spotify Soloist (`ws://127.0.0.1:0`) | MIT |
| `futures-util` | `0.3` | Async stream/sink combinators for Soloist WebSocket transport | MIT OR Apache-2.0 |
| `flate2` | `1.1` | Pure-Rust gzip/deflate decompression for official Spotify Soloist archives | MIT OR Apache-2.0 |
| `reqwest` | `0.13` | Async HTTP/2 client with `rustls` TLS | MIT OR Apache-2.0 |
| `serde` / `serde_json` | `1.0` | Serialization / JSON API parsing | MIT OR Apache-2.0 |
| `rusqlite` | `0.43` (`bundled`) | Embedded SQLite database with bundled C library | MIT |
| `quick-xml` | `0.42` | Zero-copy streaming XML parser for TTML lyrics | MIT |
| `keyring` | `4.2` | OS Keychain / Credential Manager / Secret Service | MIT OR Apache-2.0 |
| `rodio` | `0.21` | Local audio playback (compiled `symphonia` FLAC, MP3, Vorbis, WAV decoders) | MIT OR Apache-2.0 |
| `lofty` | `0.25` | Pure-Rust audio metadata & embedded album artwork extractor (ID3/FLAC/Vorbis/WAV) | MIT OR Apache-2.0 |
| `rfd` | `0.18` | Native OS file/folder pickers (`xdg-portal` on Linux, Win32, AppKit) | MIT |
| `zbus` *(Linux only)* | `5.19` | Native Linux MPRIS2 D-Bus media integration (already in Linux dependency graph) | MIT |
| `tracing` / `tracing-subscriber` | `0.1` / `0.3` | Structured logging (with token redaction discipline) | MIT |
| `open` | `5.4` | Launch OS default browser for Spotify OAuth | MIT |
| `url` | `2.5` | WHATWG URL construction & query parsing | MIT OR Apache-2.0 |
| `base64` | `0.23` | RFC 4648 base64url encoding for PKCE | MIT OR Apache-2.0 |
| `sha2` | `0.11` | SHA-256 PKCE `S256` challenge & artwork content hashing | MIT OR Apache-2.0 |
| `rand` | `0.10` | Cryptographically secure PKCE verifier & OAuth `state` | MIT OR Apache-2.0 |
| `thiserror` / `anyhow` | `2.0` / `1.0` | Typed domain errors & ergonomic propagation | MIT OR Apache-2.0 |
| `image` | `0.25` | Async album artwork decoding & dominant color sampling | MIT OR Apache-2.0 |
| `dirs` | `7.0` | XDG / macOS / Windows standard data & cache directories | MIT OR Apache-2.0 |
| `chrono` | `0.4` | Local time-of-day greetings & RFC 3339 timestamps | MIT OR Apache-2.0 |
| `async-trait` | `0.1` | Object-safe async traits (`AudioEngine`, `LyricsProvider`) | MIT OR Apache-2.0 |
| `librespot-core` / `librespot-playback` *(optional)* | `0.8.0` | Feature-gated Spotify audio streaming backend (blocked upstream Aug 2026) | MIT |

---

## 7. Security & Privacy Controls

- **CSRF Protection:** 256-bit cryptographically random `state` token validated with constant-time comparison before exchanging any authorization code.
- **PKCE (`S256`):** 64-byte random `code_verifier` (base64url-encoded, 86 chars, conforming to RFC 7636 length 43–128) hashed with SHA-256 into `code_challenge`.
- **Loopback Binding:** OAuth callback HTTP server binds exclusively to `127.0.0.1` (never `0.0.0.0`), accepts one valid callback, sends a clean confirmation HTML page, and shuts down immediately (with a strict timeout).
- **Redacted Logging:** Token structs implement custom `Debug` redacting `access_token` and `refresh_token` (`[REDACTED]`).
- **Storage Separation:** SQLite stores only non-sensitive metadata, caches, and UI settings. Tokens live exclusively in OS secure storage (`keyring`).
