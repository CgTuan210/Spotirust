# Apple Music Word-Level Lyrics — Technical, API & Policy Research

> **Research Date:** November 25, 2026  
> **Scope:** Evaluating whether **Spotirust** (an independent, cross-platform native Rust desktop client using Spotify Connect for playback) can legally and reliably obtain **word-level synchronized lyrics (TTML)** directly from Apple Music.  
> **Verdict:** **BLOCKED FOR LIVE AUTOMATIC FETCHING.** Apple exposes **no official public Apple Music API or MusicKit endpoint** for lyric text or word-level TTML timestamps. Spotirust **will not** call private/undocumented `amp-api.music.apple.com` endpoints, scrape `media-user-token` cookies, spoof browser/device headers, or bundle third-party scraping relays.

---

## 1. Executive Summary

| Category | Mechanism | Word-Level Timestamps? | Official / Supported? | Usable in Spotirust Release? |
| :--- | :--- | :--- | :--- | :--- |
| **1. Official Public Apple Music API** | `GET https://api.music.apple.com/v1/catalog/{storefront}/songs` (`filter[isrc]`, search, song attributes) | **No** — exposes only `hasLyrics: boolean` | Yes (documented REST API) | Metadata/ISRC lookup only (requires paid Apple Developer Program JWT; **zero lyric text**) |
| **2. Entitled / Subscriber MusicKit API** | MusicKit for Swift (iOS/macOS), MusicKit JS (Web), MusicKit for Android (`Music-User-Token`) | **No** — `Song.hasLyrics` is boolean only; no lyric text API exists even for paid subscribers | Yes (requires entitlements + subscriber auth) | **No** — still lacks any lyric text endpoint, and has no native Linux/Windows Rust SDK |
| **3. Private / Internal Apple Endpoints** | `https://amp-api.music.apple.com/v1/catalog/{storefront}/songs/{id}/syllable-lyrics` & `/lyrics` | Yes (returns Apple-namespaced TTML with `itunes:timing="Word"`) | **No** — undocumented first-party internal endpoint | **PROHIBITED** — requires Apple's first-party web JWT + scraped `media-user-token` cookie; violates Apple ToS |
| **4. Unofficial Community Solutions** | `media-user-token` extractors (`apple-music-downloader`, `MusanovaKit`), third-party scraper relays, community TTML mirrors (`amll-ttml-db`) | Yes (TTML with syllable timing) | **No** — unsupported & reverse-engineered | **DO NOT INTEGRATE** into Spotirust releases (ToS, account ban, & lyric publisher copyright risk) |
| **5. User-Supplied Local TTML / LRC Files** | Local `.ttml` (W3C + Apple/AMLL TTML schema) and `.lrc` / Enhanced LRC files imported by the user | **Yes** — full word/syllable timing, `x-bg`, translations, romanization | **Yes** — user-owned local files | **VERIFIED & SUPPORTED** via `LocalFileLyricsProvider` + `parse_ttml` |
| **6. Compliant Open Lyrics Fallback** | **LRCLIB** Public API (`https://lrclib.net/docs`) | Mostly line-level LRC (word-level only when Enhanced LRC `<mm:ss.xx>` is supplied) | Yes — open public API | **SUPPORTED** as honest fallback (clearly labeled as `LRCLIB`, never mislabeled as Apple Music) |

---

## 2. Detailed Investigation by Access Method

### 2.1 Official Apple Music API (`api.music.apple.com/v1`)

**Primary Documentation Verified (November 2026):**
- Apple Music API — `Songs.Attributes`: https://developer.apple.com/documentation/applemusicapi/songs/attributes-data.dictionary
- Apple Music API — Get Multiple Catalog Songs by ISRC: https://developer.apple.com/documentation/applemusicapi/get-multiple-catalog-songs-by-isrc
- Apple Music API — Get Multiple Library Songs: https://developer.apple.com/documentation/applemusicapi/get-multiple-library-songs
- Generating Developer Tokens (ES256 JWT): https://developer.apple.com/documentation/applemusicapi/generating-developer-tokens
- User Authentication for MusicKit (`Music-User-Token`): https://developer.apple.com/documentation/applemusicapi/user-authentication-for-musickit

**What IS publicly documented:**
- Catalog track matching by ISRC (`GET /v1/catalog/{storefront}/songs?filter[isrc]=...`) or search (`GET /v1/catalog/{storefront}/search`).
- Song metadata attributes: `name`, `artistName`, `albumName`, `durationInMillis`, `isrc`, `contentRating` (explicit/clean), `genreNames`, `releaseDate`, and **`hasLyrics: boolean`**.

**What is NOT available:**
- There is **no** documented `/lyrics` or `/syllable-lyrics` resource or relationship under `Songs` or `LibrarySongs` in the Apple Music API.
- `hasLyrics` strictly tells an app whether Apple's first-party Music app has lyrics for that catalog item — it does not return text, timestamps, or a URL to retrieve lyrics.

---

### 2.2 MusicKit Frameworks (Swift / Web JS / Android)

**Primary Documentation & Developer Forum Verification (November 2026):**
- MusicKit `Song` struct: https://developer.apple.com/documentation/musickit/song
- MusicKit `Song.hasLyrics`: https://developer.apple.com/documentation/musickit/song/haslyrics
- Apple Developer Forums (Nov 21, 2026 — Enhancement Request **`FB25664234`**): https://developer.apple.com/forums/thread/859013

**Findings:**
- Even for first-class native Apple apps built with MusicKit for Swift and signed in as active Apple Music subscribers, Apple provides **zero API access to lyric text, line-synced lyrics, or word-by-word lyrics**.
- Confirmed explicitly as of **November 21, 2026** (`FB25664234`, Apple Developer Forums thread `859013`):
  > *"Lyrics, including time-synced lyrics: FB25664234 — MusicKit gives no access to lyric text, so subscribers lose lyrics, including time-synced and word-by-word lyrics, whenever they play through a MusicKit app."*
- Furthermore, MusicKit's automatic `Music-User-Token` management is available only on Apple platforms (Swift), Web browsers (MusicKit JS), and Android — there is **no official native MusicKit SDK for Linux or Windows desktop apps in Rust**.

---

### 2.3 Private / Internal Apple Endpoints (`amp-api.music.apple.com`) — **Research Only, Not Integrated**

**Observed Technical Structure (from public reverse-engineering notes & developer discussions):**
- Apple's first-party web player (`music.apple.com`) and native Music apps fetch timed lyrics from undocumented internal endpoints on `amp-api.music.apple.com`, such as:
  - `GET https://amp-api.music.apple.com/v1/catalog/{storefront}/songs/{id}/syllable-lyrics` (word/syllable-level TTML)
  - `GET https://amp-api.music.apple.com/v1/catalog/{storefront}/songs/{id}/lyrics` (line-level TTML)
  - Optional query extensions like `extend=ttmlLocalizations` for translations/pronunciation.
- These endpoints reject normal third-party MusicKit developer tokens or lack subscriber context unless the caller supplies:
  1. An Apple first-party web bearer JWT extracted from `music.apple.com` (or a private MusicKit token scoped to Apple's AMP host), **and**
  2. A user's `media-user-token` session cookie scraped from an authenticated `music.apple.com` browser session, often accompanied by spoofed `Origin: https://music.apple.com` headers.

**Why Spotirust Strictly Refuses to Call These Endpoints:**
1. **Explicitly undocumented & private:** Apple can change, rate-limit, block, or enforce attestation on `amp-api.music.apple.com` at any moment with zero deprecation notice.
2. **Credential & cookie scraping prohibited:** Extracting `media-user-token` cookies from a user's browser or instructing users to paste raw Apple session cookies violates both Apple's terms and Spotirust's security model.
3. **Device/origin spoofing prohibited:** Impersonating `music.apple.com` or Apple's first-party clients breaches API access controls.

---

### 2.4 Unofficial Community Tools & Third-Party Relays — **Research Only, Not Integrated**

**Surveyed Ecosystem:**
- **Cookie-based scripts & Swift private-API kits** (e.g., `MusanovaKit`, `apple-music-downloader`): Require users to open browser DevTools (`F12`), copy `media-user-token` from `music.apple.com`, and hit `/syllable-lyrics` directly.
- **Third-party community TTML repositories / scraping proxies** (e.g., `amll-ttml-db`, undocumented relay servers): Either crowd-source user-authored TTML files or proxy scraped Apple Music/Musixmatch payloads.

**Risks:**
- **Copyright & Licensing:** Song lyrics are copyrighted works licensed by music publishers (e.g., Musixmatch, LyricFind, Sony Music Publishing, UMG) specifically to Apple Music for playback inside Apple's authorized surfaces. Redistributing or scraping Apple's licensed TTML catalog to power a third-party Spotify client creates direct copyright and licensing liability.
- **Stability:** Community proxies regularly go offline, hit IP bans, or break when Apple rotates web tokens or tightens `media-user-token` binding.
- **Policy:** Spotirust will not embed, proxy through, or automatically query unauthorized community mirrors of Apple Music's lyric catalog.

---

### 2.5 Apple Music TTML Schema vs. User-Supplied Local `.ttml` Files

**Reference Specifications:**
- W3C Timed Text Markup Language (TTML): https://www.w3.org/TR/ttml2/
- Community TTML Specification (describing W3C + `itunes:` extensions used by Apple Music & AMLL): https://amll.dev/en/guides/lyric/ttml.html

Apple Music's internal lyric payload is standard **W3C TTML XML** augmented with the `xmlns:itunes="http://music.apple.com/lyric-ttml-internal"` namespace:
- Root `<tt itunes:timing="Word">` (word/syllable timing) or `<tt itunes:timing="Line">` (line timing).
- Structural song parts: `<div itunes:song-part="Verse|Chorus|Bridge|Outro">`.
- Timed lines: `<p begin="00:10.000" end="00:14.200" itunes:key="L1" ttm:agent="v1">`.
- Timed words/syllables: `<span begin="00:10.000" end="00:10.450">word</span>` (whitespace-sensitive XML distinguishing word boundaries from multi-syllable joins).
- Background vocals: `<span ttm:role="x-bg" begin="..." end="...">...</span>`.
- Translations & transliterations/romanizations: either inline (`<span ttm:role="x-translation">` / `<span ttm:role="x-roman">`) or sidecar `<iTunesMetadata><translations><translation><text for="L1">...` / `<transliterations>`.

**Crucial Distinction:**
- **Parsing TTML** is completely legal, open, offline, and **VERIFIED** in Spotirust (`src/lyrics/ttml.rs`).
- **Fetching Apple Music's copyrighted TTML from Apple's private servers** is **BLOCKED**.
- Users who legitimately author, purchase, or possess local `.ttml` files (or `.lrc` / Enhanced LRC files) can place them beside local audio tracks, inside configured local lyrics folders, or import them per track directly in Spotirust's Lyrics UI — even while playing the matching song via **Spotify Connect**!

---

## 3. Authentication, Licensing & Legal Constraints (Official Apple Music API)

Even if Apple adds an official lyrics endpoint in the future, an independent native Rust desktop client faces four structural requirements verified from official documentation:

1. **Paid Apple Developer Program Membership ($99/year):** Required to create a MusicKit identifier and ES256 `.p8` private key (`developer.apple.com/documentation/applemusicapi/generating-developer-tokens`).
2. **Developer Token Security & Expiry:** Developer JWTs must be signed with ES256 and expire after at most **6 months (15,777,000 seconds)**. Open-source desktop binaries cannot embed the `.p8` private key; a backend signing service or maintainer-rotated token would be required.
3. **Apple Developer Program License Agreement (Oct 13, 2026) — Section 3.3.6(D) (MusicKit) & App Review Guideline 4.5.2:**
   - Source: https://developer.apple.com/support/terms/apple-developer-program-license-agreement/
   - MusicKit content may only be used as documented by Apple, cannot be permanently cached/archived to build an independent catalog, and **cannot be synchronized with non-Apple content or competing playback services unless expressly permitted in Apple's documentation**.
   - Using Apple Music content to enhance **Spotify Connect** playback directly conflicts with cross-service synchronization restrictions in Section 3.3.6(D) unless Apple explicitly licenses such usage.
4. **No Native Linux/Windows MusicKit User Auth SDK:** Obtaining a `Music-User-Token` officially requires MusicKit for Swift, MusicKit JS in a browser, or MusicKit for Android.

---

## 4. Long-Term Stability & Architectural Decision for Spotirust

| Approach | Long-Term Stability | Legal / Policy Compliance | Decision in Spotirust |
| :--- | :--- | :--- | :--- |
| Official Apple Music API (`api.music.apple.com`) for lyrics | N/A — **does not exist** (`FB25664234`) | Compliant for catalog metadata only | Implement `AppleMusicLyricsProvider` interface honestly returning `LyricsError::UpstreamUnsupported` (blocked until Apple publishes an official lyrics API). Never fake success. |
| Private `amp-api.music.apple.com/syllable-lyrics` + `media-user-token` | **Low** — breaks without notice; risks account/IP blocks | **Non-compliant** (ToS + publisher copyright) | **Rejected.** Strictly excluded from codebase and releases. |
| User-supplied Local `.ttml` / `.lrc` files (W3C + Apple/AMLL schema) | **High** — fully offline, deterministic | **100% compliant** | **Implemented & First-Class:** sidecar lookup, lyrics folders, and native one-click **Import `.ttml`/`.lrc`** file picker in the Lyrics view (works seamlessly with Spotify Connect tracks!). |
| Public LRCLIB API (`https://lrclib.net/docs`) | **High** — public open API, respectful User-Agent & `Retry-After` | **Compliant** (open community API) | **Implemented as Automatic/Selectable Fallback** with strict anti-live/remix/remastered/duration matching & ambiguous-candidate picker. Always labeled honestly as `LRCLIB` — never labeled "Apple Music". |
| Future Licensed Commercial Lyrics Provider (e.g., Musixmatch / LyricFind commercial SDK) | **High** (with commercial license) | **Compliant** (requires paid licensing contract) | Supported architecturally via the decoupled `LyricsProvider` trait (`search` + `fetch`) without changing `PlaybackClock`, `matcher`, or the karaoke renderer. |

---

## 5. Conclusion

1. **Can Spotirust legally integrate live Apple Music word-level lyrics right now?**  
   **No.** As of November 2026, Apple provides **no official public Apple Music API or MusicKit endpoint** to fetch lyric text or word-level TTML timestamps (`Songs.Attributes.hasLyrics` is boolean-only; confirmed open enhancement request `FB25664234`). Furthermore, Apple's Developer Program License Agreement (§3.3.6(D)) restricts syncing MusicKit content with competing streaming services like Spotify.
2. **What Spotirust builds instead (100% honest, compliant, and production-ready):**
   - A clean, decoupled **`LyricsProvider`** abstraction (`search` + `fetch`) containing an honest **`AppleMusicLyricsProvider`** that reports its upstream API block clearly instead of scraping private endpoints or impersonating other sources.
   - A full **W3C + Apple/AMLL TTML word-level parser** (`src/lyrics/ttml.rs`) for user-supplied local `.ttml` files (supporting word/syllable timing, `x-bg`, song sections, translations, and romanizations).
   - A strict **Spotify ↔ Lyrics Track Matcher** (`src/lyrics/matcher.rs`) prioritizing ISRC, rejecting live/remix/acoustic/sped-up/remastered mismatches, and surfacing close-scoring ambiguous candidates to the user rather than guessing wrong.
   - A high-precision, monotonic **`PlaybackClock` with drift correction** (`src/audio/clock.rs`) driving the karaoke renderer from confirmed Spotify Connect playback positions — so word-level TTML (local or future licensed provider) and line-level LRCLIB lyrics stay tightly synchronized through play, pause, resume, seek, track changes, network jitter, and disconnects.
