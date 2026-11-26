# Spotirust Installation, Platform & Troubleshooting Guide

This guide covers non-Rust user installation, runtime dependencies, data/cache locations, OAuth setup, audio playback (Local Player, Spotify Connect & Spotify Soloist), lyrics, Linux MPRIS2 integration, and clean uninstall / full data reset.

---

## 1. Installing & Running Without Rust (Portable Archive)

End users do **not** need Rust or Cargo installed when using a pre-built portable archive.

### Linux (`x86_64` — **VERIFIED** locally)
```bash
tar -xzf spotirust-linux-x86_64-portable.tar.gz
cd spotirust
./spotirust
```
Because the archive includes an empty `portable.flag` file beside the `spotirust` binary, Spotirust starts in **Portable Mode** automatically and keeps all SQLite data and caches inside `./spotirust-data/`.

To run in **standard system mode** instead (storing data in your home XDG directories), delete `portable.flag` or copy the binary to `~/.local/bin/spotirust`.

### Windows & macOS (**UNVERIFIED** on native hosts)
Portable packaging scripts (`scripts/package_portable.ps1` for Windows ZIP, `scripts/package_portable.sh` for macOS `.tar.gz`) and CI workflows exist, but have **not** yet been tested on physical Windows or macOS machines.

---

## 2. Runtime Dependencies

### Linux
- **Graphics:** OpenGL 3.3+ (`libGL` / Mesa drivers) and X11 or Wayland compositor (`libxkbcommon`, `libwayland-client`).
- **Audio (Local Player):** PulseAudio, PipeWire (`pipewire-pulse` / `pipewire-alsa`), or ALSA (`libasound2`).
- **Native File/Folder Pickers (`rfd`):** `xdg-desktop-portal` plus your desktop backend (`xdg-desktop-portal-gtk`, `-gnome`, or `-kde`). On minimal window managers without a portal daemon, Spotirust still provides a direct folder-path input box on the **Local Files** page.
- **Session Persistence (`keyring 4`):** D-Bus Secret Service (`gnome-keyring-daemon`, KWallet, or KeePassXC). Without it, OAuth tokens stay strictly in ephemeral RAM for the current session and are never written in plaintext to SQLite.
- **OS Media Controls (`MPRIS2`):** D-Bus session bus (`org.mpris.MediaPlayer2.spotirust`). Automatically disabled without error if no session bus is running.

### Building From Source (Ubuntu / Debian)
```bash
sudo apt-get update
sudo apt-get install -y libasound2-dev pkg-config libxkbcommon-dev libwayland-dev libgl1-mesa-dev
```

---

## 3. Exact Data, Database & Cache Locations

| Mode / Platform | SQLite DB (`spotirust.db`), Themes & Local Lyrics | Artwork & Soundcheck Cache | OAuth Tokens |
|---|---|---|---|
| **Portable Mode** (`--portable`, `SPOTIRUST_PORTABLE=1`, or `portable.flag`) | `<exe_dir>/spotirust-data/data/` | `<exe_dir>/spotirust-data/cache/` | OS Keychain / Credential Vault (never in SQLite) |
| **Linux (Standard)** | `~/.local/share/spotirust/` (`$XDG_DATA_HOME/spotirust`) | `~/.cache/spotirust/` (`$XDG_CACHE_HOME/spotirust`) | Linux DBus Secret Service (`service="spotirust"`) |
| **macOS (Standard)** | `~/Library/Application Support/spotirust/` | `~/Library/Caches/spotirust/` | macOS Keychain (`service="spotirust"`) |
| **Windows (Standard)** | `%APPDATA%\spotirust\` | `%LOCALAPPDATA%\spotirust\` | Windows Credential Manager (`spotirust`) |

Inside the data/cache directories:
- `spotirust.db` — SQLite database (WAL mode, schema v2: cached catalog metadata, local library index, lyrics cache, settings, playback preferences).
- `themes/` — Optional drop-in custom `*.json` theme palettes.
- `lyrics/` — Optional user-supplied `.ttml` and `.lrc` lyrics files.
- `artwork/` — Bounded LRU disk cache (capped at 64 MiB / 512 files) + `localart_*.img` embedded covers extracted via `lofty`.
- `soundcheck/` — Locally synthesized royalty-free WAV Soundcheck audio files.

---

## 4. Spotify Sign-In (`Continue with Spotify`)

### "Spotify OAuth Client ID is not configured in this build"
Spotify requires a registered OAuth Client ID even for Authorization Code Flow with PKCE. Per Spotirust's security rules, the application never embeds a Client Secret and never shows a manual Client ID box to end users.

**For developers / self-hosted builds:**
1. Create an app in the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard) (requires an active Spotify Premium account under Spotify's February 2026 Development Mode rules).
2. Add the loopback redirect URI:
   - Recommended (dynamic port): `http://127.0.0.1/callback`
   - Or fixed port: `http://127.0.0.1:43821/callback` (and set `SPOTIRUST_REDIRECT_PORT=43821`).
   - **Important:** Do **not** use `http://localhost:...` — Spotify permanently banned `localhost` redirect URIs on November 27, 2025.
3. Build or run with your Client ID:
   ```bash
   SPOTIRUST_CLIENT_ID="your_spotify_client_id" cargo build --release
   ```

### Browser Does Not Open Automatically
Click **Open Spotify Login** inside Spotirust's Home banner or Sidebar card. Spotirust binds a local loopback listener on `127.0.0.1` and completes authentication automatically once Spotify redirects back.

### Development Mode Limits (Spotify 2026 Policy)
- **5-user allowlist:** Development Mode apps only authorize accounts explicitly added to the app's User Management allowlist (up to 5 users).
- **Spotify Premium:** App owners must keep an active Premium subscription for Development Mode API calls to succeed.
- **Third-party playlists:** In Development Mode, `GET /playlists/{id}/items` only returns tracks for playlists you own or collaborate on.
- **Search limit:** Search returns up to 10 items per category in Development Mode (`GET /search` cap introduced February 2026).

---

## 5. Audio Playback, Spotify Connect & Spotify Soloist

### "Spotify Premium is required for this playback action"
Spotify restricts `/me/player/*` remote playback control to **Spotify Premium** accounts. Free accounts can still use **Local Music Player (`rodio`)** for local audio files and offline karaoke lyrics.

### "No active Spotify Connect playback device found"
The official Spotify Web API Connect backend controls an active Connect receiver rather than streaming audio bytes directly:
1. Open the official Spotify desktop/mobile app, a Connect speaker, or **Spotify Soloist** (Spotify's official headless Connect receiver).
2. In Spotirust's top bar, open **Connect Devices → Sync Active Playback & Devices** and select an available (non-restricted) device.
3. Devices marked `⊘ Restricted` by Spotify cannot be controlled via the Web API.

### About Spotify Soloist
Spotify Soloist (`spotify-soloist`) is Spotify's official standalone Connect receiver (introduced 2026) that requires each user to download Spotify's proprietary binary and supply their own non-redistributable Soloist key (90-day expiry).
- **Recommended usage:** Run Spotify Soloist normally so it appears in your Spotify Connect device list; Spotirust controls it cleanly through standard Spotify Connect (`/me/player/*`).
- **Why direct Soloist WebSocket (`--ws`) is NOT built in:** Soloist's optional local WebSocket interface (`127.0.0.1`) is disabled by default, requires opt-in flags, and exposes an unauthenticated localhost control channel. Using standard Spotify Connect keeps authentication and security unified without distributing or depending on proprietary binaries.

### Community `librespot` Backend (`--features librespot-backend`)
- Since August 10, 2026 (`librespot-org/librespot#1737`), Spotify blocks `login5` token exchange for third-party developer Client IDs (`FaultyRequest(INVALID_CREDENTIALS)`).
- Spotirust refuses to embed unauthorized first-party credentials. Use **Spotify Connect** for Spotify tracks or **Local Player (`rodio`)** for local audio files.

### Local Audio Device / ALSA Errors on Linux
- Verify PipeWire / PulseAudio / ALSA is running (`pactl info` or `aplay -l`).
- If a file in your local library is corrupt or truncated, Spotirust skips it safely during scanning and logs a warning rather than crashing or faking playback.

---

## 6. Word-by-Word Lyrics & Timing

- **Timing offset:** Use **`-50ms`** / **`+50ms`** in the Lyrics drawer or Fullscreen Now Playing view; per-track offsets persist in SQLite.
- **Local `.ttml` / `.lrc` files:** Place sidecar files beside local audio files (`Song.flac` + `Song.ttml`) or inside `<data_dir>/lyrics/`, or click **Import** in the Lyrics panel.
- **Anti-remix matching:** `src/lyrics/matcher.rs` rejects mismatched versions (`live`, `remix`, `acoustic`, `sped up`, `slowed`, `karaoke`, `instrumental`) and duration mismatches (> ±6 s).

---

## 7. Uninstall & Complete Data Reset

### In-App Reset (Recommended First Step)
1. Open **Settings → Security, Storage & Session**.
2. Click **Clear Cache** to purge cached Spotify metadata, lyrics cache, and remote disk artwork (keeps your login and local music library index).
3. Or click **Reset Application** to sign out, remove OAuth tokens from your OS credential vault, wipe all SQLite tables (including local library index and settings), and delete all cached artwork.

### Complete Uninstall

**Portable install:**
1. Run **Reset Application** in Settings (clears OS keychain entry).
2. Delete the extracted `spotirust/` directory (which contains `spotirust`, `portable.flag`, and `spotirust-data/`).

**Standard Linux install:**
```bash
# 1. Remove binary (wherever installed, e.g. ~/.local/bin/spotirust)
rm -f ~/.local/bin/spotirust
# 2. Remove SQLite database, themes, local lyrics, and caches
rm -rf ~/.local/share/spotirust ~/.cache/spotirust
# 3. Optional: remove OS Secret Service token entry if not already logged out in-app
secret-tool clear service spotirust 2>/dev/null || true
```

**Standard macOS install:**
```bash
rm -rf ~/Library/Application\ Support/spotirust ~/Library/Caches/spotirust
# Remove "spotirust" entry in Keychain Access if still present
```

**Standard Windows install (PowerShell):**
```powershell
Remove-Item -Recurse -Force "$env:APPDATA\spotirust", "$env:LOCALAPPDATA\spotirust" -ErrorAction SilentlyContinue
# Remove "spotirust" entry in Windows Credential Manager if still present
```
