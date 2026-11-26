# Spotirust — Real Lyrics Providers Audit & Implementation Report

*Last verified: 2026-11-25 against live public endpoints and primary-source documentation.*

This document records the primary-source research, legal/licensing audit, real implementation architecture, and automated + live verification results for every lyrics provider evaluated for **Spotirust** (MIT-licensed native Rust desktop Spotify client).

> **Design rules enforced strictly in code:**
> - **Zero fake providers:** Only providers with legitimate, feasible access are implemented.
> - **Zero unauthorized scraping proxies or credential theft:** No Apple Music `amp-api` cookie scraping, no BiniLyrics / LyricsPlus unlicensed Apple TTML relays, and no reverse-engineered Musixmatch anonymous desktop token spoofing.
> - **Word-level timing priority without faking:** `Word-level` (`TTML` / `Musixmatch RichSync` / `LRCLIB Lyricsfile` / `Enhanced LRC`) → `Line-level` (`TTML Line` / `Lyricsfile Line` / `Standard LRC`) → `Plain`. Spotirust NEVER synthesizes fake word/line timestamps for plain lyrics.
> - **Honest source & required attribution:** Every `LyricsDocument` carries its true provider source and structured `LyricsAttribution` (persisted in SQLite via migration `0003_lyrics_attribution.sql` and rendered in the lyrics footer).

---

## 1. Provider Status Matrix

Status classifications strictly follow:
- `IMPLEMENTED_AND_TESTED`: Real provider implemented, unit + mock-HTTP integration tests passing, **and** live end-to-end verification succeeded.
- `IMPLEMENTED_BUT_UNVERIFIED`: Real provider implemented and passing unit + mock-HTTP integration tests, but live end-to-end lyrics retrieval could not be verified without a user/maintainer paid API key.
- `BLOCKED_BY_ACCESS`: No legal/authorized public access mechanism for an open-source desktop app (copyrighted redistribution proxy or missing official public API). Not implemented as a fake stub.
- `NOT_IMPLEMENTED`: Evaluated and intentionally not implemented (ToS violation, redundant, or superseded).

| Provider | Search | Fetch | Word timing | Line timing | License / Terms | Trạng thái |
| --- | --- | --- | --- | --- | --- | --- |
| **Unison** (`unison.betterlyrics.org`) | Yes (`?isrc`, `?spotify`, `?song`+`artist`, `/lyrics/search`) | Yes (inline + `/lyrics/{id}`, ETag `304`) | **Yes** (TTML `richsync` / syllable-level) | Yes (LRC / TTML line) | Free read API for individuals, non-profits & **OSI open-source apps (MIT)** providing lyrics free of charge; commercial use banned; **mandatory song + curator attribution** by Feb 2027 | `IMPLEMENTED_AND_TESTED` |
| **AMLL TTML DB** (`amll-dev/amll-ttml-db`) | Yes (direct O(1) Spotify Track ID lookup + `<amll:meta>` verification) | Yes (`spotify-lyrics/{spotify_id}.ttml`) | **Yes** (hand-timed community TTML) | Yes (TTML line fallback) | **CC0-1.0** for community-authored TTML; explicitly prohibits uploading raw Apple Music dumps; repo/author attribution recommended | `IMPLEMENTED_AND_TESTED` |
| **LRCLIB** (`lrclib.net`) | Yes (`/api/get`, `/api/v2/search`, `/api/search` fallback) | Yes (inline + `/api/get/{id}`) | **Yes** (CC0 `Lyricsfile` v1.0 YAML `lines[].words[]` + Enhanced LRC) | Yes (`syncedLyrics` LRC + `Lyricsfile` line) | Free open community public API, no key required (optional `Lrclib-App-Id`); `Lyricsfile` spec is **CC0-1.0**; polite `User-Agent` & `Retry-After` on `429`/`503` | `IMPLEMENTED_AND_TESTED` |
| **Local Files** (`.ttml`, `.lyricsfile`, `.lrc`) | Yes (sidecar + ISRC / track ID / title-artist folders) | Yes (async `spawn_blocking` disk read) | **Yes** (TTML, `Lyricsfile`, Enhanced LRC) | Yes (Standard LRC, TTML line) | User-supplied local files (offline, zero network) | `IMPLEMENTED_AND_TESTED` |
| **Musixmatch (Official Developer API)** (`api.musixmatch.com/ws/1.1/`) | Yes (`matcher.track.get` via ISRC/Spotify ID/meta + `track.search`) | Yes (`track.richsync.get` → `track.subtitle.get` → `track.lyrics.get`) | **Yes** (`RichSync` JSON — Pro/Commercial plan) | Yes (`Subtitle` LRC — Plus/Commercial plan) | Official Musixmatch Developer ToS; requires official `apikey` (OS Keyring or `SPOTIRUST_MUSIXMATCH_API_KEY`); Free tier returns 30% non-commercial plain preview + mandatory `lyrics_copyright` | `IMPLEMENTED_BUT_UNVERIFIED` |
| **BiniLyrics** (`lyrics-api.binimum.org` / `lrc.red`) | Unofficial proxy (`?isrc`, `?title`, `?artist`) | Unofficial proxy redistributing Apple TTML | Unlicensed Apple TTML relay | Fallback relay | **Unauthorized redistribution proxy:** scrapes Apple Music TTML via operator-owned paid streaming accounts without publisher licensing or redistribution terms (confirmed in `music-assistant/discussions/5345`, Nov 19, 2026); `binimum/am-lyrics` (MPL-2.0) is only the UI component | `BLOCKED_BY_ACCESS` |
| **Apple Music** (`api.music.apple.com/v1`) | Catalog metadata only (`hasLyrics: bool`) | **No** official public lyrics endpoint (`FB25664234`) | Blocked upstream (private `amp-api` requires cookie/token scraping) | Blocked upstream | Official Apple Music API / MusicKit exposes `hasLyrics` boolean only; private `amp-api` requires scraping `media-user-token` cookies (prohibited) | `BLOCKED_BY_ACCESS` |
| **Musixmatch Unofficial** (`musixmatch-inofficial` / anonymous desktop `usertoken`) | Private reverse-engineered endpoint | Private endpoint | Reverse-engineered | Reverse-engineered | Violates Musixmatch ToS by spoofing desktop app credentials & secret signing; excluded deliberately | `NOT_IMPLEMENTED` |

---

## 2. Primary-Source Technical Findings

### 2.1 Unison (`src/lyrics/unison.rs`)
- **Upstream repository:** `https://github.com/better-lyrics/unison`
- **Access & licensing:** Open read API at `https://unison.betterlyrics.org` with no API key required for open-source OSI-licensed projects (`Spotirust` is MIT) as long as Unison lyrics remain free to all users.
- **Lookup chain implemented:**
  1. `GET /lyrics?isrc=<ISRC>` (highest precision)
  2. `GET /lyrics?spotify=<22-char-id>` (gracefully handles `503 LOOKUP_PENDING` without aborting subsequent fallbacks)
  3. `GET /lyrics?song=<title>&artist=<artist>[&album=<album>][&duration=<secs>]`
  4. `GET /lyrics/search?q=<title> <artist>` ranked through Spotirust's strict `matcher.rs` → `GET /lyrics/{id}`
- **Syllable-level TTML support:** Live Unison TTML frequently contains syllable spans without spaces between syllables (`<span>any</span><span>body</span>`) alongside explicit inter-word spaces (`</span> <span`). `src/lyrics/ttml.rs` preserves these exact syllable boundaries rather than inserting synthetic spaces.
- **ETag & rate-limit politeness:** Sends `If-None-Match` (`304 Not Modified` reuse), honors `429 RATE_LIMITED` `Retry-After`, enforces a 6s per-provider timeout.
- **Mandatory Attribution:** Stores `"Lyrics from Unison"` (`https://unison.betterlyrics.org/song/<videoId>`) and submitter credit (`https://unison.betterlyrics.org/curator/<keyId>`) in `LyricsDocument.attribution`, cached in SQLite and rendered as clickable links at the bottom of the lyrics view (`src/ui/lyrics.rs`).

### 2.2 AMLL TTML DB (`src/lyrics/amll_db.rs`)
- **Upstream repository:** `https://github.com/amll-dev/amll-ttml-db` (`CC0-1.0`)
- **Why it is compliant:** Community contributors hand-time word-level TTML lyrics and explicitly reject bulk-copied Apple Music TTML dumps.
- **Lookup design:** O(1) direct fetch via `spotify-lyrics/{spotify_id}.ttml` only for tracks with a valid 22-character Spotify Base62 track ID (zero requests for local files). Parses and validates the XML with `parse_ttml`, verifies `<amll:meta>` metadata through `matcher.rs` to guard against mismatched/live variants, and surfaces `<amll:meta key="ttmlAuthorGithubLogin">` contributor credit alongside the repository link.

### 2.3 LRCLIB (`src/lyrics/lrclib.rs` + `src/lyrics/lyricsfile.rs`)
- **Upstream documentation:** `https://lrclib.net/docs` and `https://lrclib.net/lyricsfile` (`CC0-1.0` format spec).
- **2026 Word-Level Upgrade (`Lyricsfile v1.0`):** LRCLIB now serves `hasWordSync: true` and structured YAML `lyricsfile` payloads containing `lines[].words[]` (`text`, `start_ms`, optional `end_ms`, preserving trailing spaces). Spotirust implements a zero-dependency pure-Rust parser (`src/lyrics/lyricsfile.rs`) producing `LyricsFormat::LyricsfileWord` (word-level) or `LyricsFormat::LyricsfileLine` (line-level) without synthesizing fake timestamps.
- **Lookup chain implemented:**
  1. `GET /api/get` (exact signature — prefers `lyricsfile` word-sync → `syncedLyrics` Enhanced/Standard LRC → `lyricsfile` line-sync → `plainLyrics`)
  2. `GET /api/v2/search` (lightweight metadata + `hasWordSync`/`hasSyncedLyrics`/`hasPlainLyrics` flags) ranked through `matcher.rs` → `GET /api/get/{id}`
  3. Automatic transparent fallback to `GET /api/search` (v1) if a mirror/server doesn't expose v2.
- **Resilience:** Honors `Retry-After` on both `429` and `503`, optional `SPOTIRUST_LRCLIB_APP_ID` (`Lrclib-App-Id` header), 8s per-provider timeout.

### 2.4 Musixmatch Official Developer API (`src/lyrics/musixmatch.rs`)
- **Upstream documentation:** `https://developer.musixmatch.com/`
- **Rust crate evaluation:** Existing crates (`musixmatch` v0.1 from Mar 2024 lacks `track.richsync.get`; `musixmatch-inofficial` scrapes private desktop tokens against ToS). Spotirust implements a clean native `reqwest` client strictly targeting official `https://api.musixmatch.com/ws/1.1/`.
- **Authentication & security:**
  - Activated only when an official Developer API key is provided via `SPOTIRUST_MUSIXMATCH_API_KEY` or stored by the user in the **OS credential vault** (`Settings -> Lyrics & Karaoke Engine`). Keys are **never** stored in SQLite and are strictly redacted (`[REDACTED]`) in `Debug` output and logs.
  - When no key is configured, `is_available()` is `false` and the fallback chain skips Musixmatch in `0ms`.
- **Word → Line → Plain downgrade chain:**
  1. `track.richsync.get` → `parse_musixmatch_richsync` (`LyricsFormat::MusixmatchRichSync`, exact millisecond word timing + syllable joining)
  2. `track.subtitle.get?subtitle_format=lrc` → `parse_lrc` (Line-level LRC when RichSync returns `403`/`404`)
  3. `track.lyrics.get` → `parse_plain_lyrics` (detects Musixmatch Free-tier 30% non-commercial truncation banner, strips banner from lyrics lines, labels source honestly as `"Musixmatch (Plain · 30% Preview)"`, attaches `lyrics_copyright` + disclaimer).

### 2.5 Why BiniLyrics Is `BLOCKED_BY_ACCESS` (Not Integrated)
- **Source audit:** `binimum/am-lyrics` (MPL-2.0) is an HTML Web Component frontend only; its backend API (`lyrics-api.binimum.org` / `lrc.red`) acts as an unauthorized proxy fetching Apple Music word-level TTML using operator-owned streaming accounts and redistributing copyrighted publisher lyrics without licensing.
- Independent open-source projects (e.g., Music Assistant maintainers in `music-assistant/discussions/5345`, Nov 19, 2026) reached the identical conclusion and declined integration on copyright/ToS grounds.
- Per Spotirust's legal & architectural standards, BiniLyrics is **not** integrated and **not** faked with a stub.

---

## 3. Ordered Fallback Chain & Anti-Spam Architecture

In `LyricsSourcePreference::Automatic` mode (`src/lyrics/provider.rs`), Spotirust resolves lyrics in strict order:

1. **SQLite Cache (`LyricsCache`)** — Instant hit (`0` network calls), losslessly re-parsing TTML, `Lyricsfile`, `RichSync`, LRC, or Plain plus persisted `LyricsAttribution` and user `offset_ms`.
2. **Bundled Sample / Local Files (`.ttml` → `.lyricsfile` → `.lrc`)** — Offline disk lookup first.
3. **Configurable Online Provider Order** (default: `Unison` → `AMLL TTML DB` → `Musixmatch [if key]` → `LRCLIB`; reorderable and individually toggleable in Settings):
   - **Immediate early exit on Word-level hit:** As soon as any provider returns genuine word-level synced lyrics (or confirmed instrumental), downstream providers are **not called**.
   - **Quality upgrade without spam:** If an earlier provider returns only `Plain` or `Line-level`, Spotirust keeps it as `best_fallback` and only queries remaining available providers capable of delivering a strictly higher timing tier (`supports_word_sync()`).
   - **Error & timeout isolation:** Each provider enforces its own timeout (`5s–8s`) and isolates `404`, `429`, `503`, `401`, `403`, and network errors so UI and playback never stall. Transient outages (`Timeout`, `RateLimited`, `Network`) are never negative-cached for 6 hours.
   - **Rapid track-switch cancellation:** `src/app.rs` aborts any in-flight lyrics task immediately on track change and checks `is_current_track_id(&track_id)` before applying async events.
   - **Independent clock sync:** Once fetched asynchronously, karaoke highlight computes position directly from `PlaybackClock::current_ms() + doc.offset_ms`, so lyrics immediately lock to the exact word even if the HTTP fetch finished mid-song.

---

## 4. Verification Results (2026-11-25)

### 4.1 Automated Offline Unit & Mock-Server Integration Tests
All tests run offline via deterministic local `127.0.0.1` mock HTTP servers (`cargo test --all-targets`):
- `src/lyrics/ttml.rs`: W3C + AMLL + Unison syllable-joining TTML parsing & malformed XML rejection.
- `src/lyrics/lyricsfile.rs`: Live LRCLIB `Lyricsfile v1.0` compact YAML word-sync, syllable joining, line-only, plain, and instrumental parsing.
- `src/lyrics/musixmatch.rs`: Official `RichSync` JSON millisecond word timing & syllable joining + API key `Debug` redaction.
- `tests/integration_tests.rs`:
  - `unison_provider_word_ttml_etag_attribution_search_and_error_handling` — ISRC lookup, syllable TTML, mandatory attribution + SQLite persistence, ETag `304`, `503 LOOKUP_PENDING` → `/lyrics/search` → `/lyrics/{id}`, `429` `Retry-After`.
  - `amll_ttml_db_provider_spotify_id_lookup_attribution_and_variant_guard` — Direct Spotify ID lookup, zero calls for local tracks, CC0 attribution + `@author`, live-variant rejection.
  - `musixmatch_provider_richsync_word_subtitle_fallback_free_preview_and_key_guard` — Missing-key 0ms skip, RichSync word timing + SQLite round-trip, `403` downgrade to Subtitle LRC, `403` downgrade to 30% Plain preview (no fake timestamps), `402` rate limit.
  - `lrclib_provider_v2_search_lyricsfile_word_sync_cache_and_503_retry_after` — `/api/v2/search` remix rejection + word-sync selection → `/api/get/{id}` `LyricsfileWord` + SQLite round-trip + `503` `Retry-After`.
  - `lyrics_service_ordered_fallback_timeout_404_429_503_word_priority_and_zero_spam` — Zero downstream calls after word-level hit, zero calls on cache hit, timeout + `404` + `402` fallback recovery, and Word > Plain quality upgrade.

### 4.2 Real Live HTTP End-to-End Verification (`--ignored` Smoke Test)
Executed live on 2026-11-25 via:
`cargo test --test integration_tests live_real_lyrics_providers_smoke_test -- --ignored --nocapture`

Observed real production results (completed in **1.02s**, all assertions passed):
1. **Unison (`unison.betterlyrics.org`)**: Returned **53 lines**, `format = Ttml`, `word_synced = true`, first line `"Can anybody stop this feeling?"` (syllables joined cleanly), full `LyricsAttribution` (`Lyrics from Unison` + song URL `pog50K4_Z64` + submitter `SigmaDrumFlash` curator URL).
2. **AMLL TTML DB (`raw.githubusercontent.com/amll-dev/amll-ttml-db`)**: Direct Spotify ID `7ajpbW6tBpqUI9foCtwlLw` returned **49 lines**, `format = Ttml`, `word_synced = true`, first line `"遥か遠くに浮かぶ星を"`, attribution `Lyrics from AMLL TTML DB (CC0-1.0)` + `@Steve-xmh`.
3. **LRCLIB (`lrclib.net`)**: Returned **60 lines**, `format = LyricsfileWord`, `word_synced = true`, first line `"We're no strangers to love"` with 5 millisecond-timed words (`18834ms..22088ms`).
4. **Musixmatch (`api.musixmatch.com/ws/1.1/`)**: Live HTTPS endpoint reachable (returned HTTP 200 with Musixmatch JSON `status_code: 401` without API key); full lyrics retrieval remains `IMPLEMENTED_BUT_UNVERIFIED` live until configured with an official Musixmatch Developer API key.
