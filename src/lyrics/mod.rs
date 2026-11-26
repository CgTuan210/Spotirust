//! Decoupled Lyrics Engine supporting real compliant lyrics providers, word-by-word timing
//! priority, strict anti-remix/live/remastered track matching, and SQLite caching.
//!
//! Supported real providers & formats:
//! - `UnisonProvider`: open-source MIT-permitted word-level TTML / LRC / plain API (`unison.betterlyrics.org`)
//! - `AmllTtmlDbProvider`: CC0-1.0 community hand-timed word-level TTML DB (`amll-dev/amll-ttml-db`)
//! - `MusixmatchProvider`: official Musixmatch Developer API (`RichSync` word-level, Subtitle LRC, Plain)
//! - `LrclibProvider`: public LRCLIB REST API with CC0 `Lyricsfile` v1.0 word-sync + LRC + plain
//! - `LocalFileLyricsProvider`: user-supplied `.ttml`, `.lrc`, and `.lyricsfile` files
//! - `AppleMusicLyricsProvider`: honest upstream-blocked status (zero unauthorized endpoints)

pub mod amll_db;
pub mod apple_music;
pub mod cache;
pub mod lrc;
pub mod lrclib;
pub mod lyricsfile;
pub mod matcher;
pub mod model;
pub mod musixmatch;
pub mod provider;
pub mod ttml;
pub mod unison;
