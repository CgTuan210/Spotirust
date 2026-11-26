//! Comprehensive integration test suite for Spotirust.
//!
//! Covers:
//! - OAuth 2.0 PKCE `S256` challenge/verifier, CSRF state validation, loopback `127.0.0.1` callback
//!   server, and local token-exchange/refresh protocol (note: tests local HTTP protocol correctness;
//!   live Spotify server sign-in requires a real Spotify account & Client ID).
//! - OS credential store abstraction & strict token redaction in `Debug` output.
//! - SQLite migrations, schema integrity (verifying zero token columns), caching, and reset.
//! - TTML (W3C + Apple/AMLL), Enhanced LRC, and Standard LRC parsers + seek-accurate active-line lookup.
//! - Anti-remix / anti-live / anti-acoustic / duration-sensitive lyrics track matcher.
//! - Local `.ttml` / `.lrc` file provider + SQLite-cached `LyricsService` resolution priority.
//! - `AudioEngine` / `PlaybackController` play/pause/resume/seek/volume/next/prev/shuffle/repeat/events.
//! - Theme JSON loading, `ThemeManager`, `PluginRegistry`, and local audio folder scanning.

use spotirust::audio::engine::{AudioEngine, PlaybackStatus};
use spotirust::audio::events::{PlaybackEvent, PlaybackEventBus};
use spotirust::audio::local_rodio::scan_local_audio_dir;
use spotirust::audio::playback::PlaybackController;
use spotirust::auth::callback::LoopbackCallbackServer;
use spotirust::auth::oauth::{
    DEFAULT_SCOPES, OAuthError, PkceAuthRequest, SpotifyOAuthClient, compute_s256_challenge,
    validate_loopback_redirect_uri,
};
use spotirust::auth::session::SessionManager;
use spotirust::core::config::UserSettings;
use spotirust::core::models::{
    Album, AudioBackendKind, Playlist, RepeatMode, Track, UserProfile, bundled_showcase_tracks,
};
use spotirust::core::plugins::{BuiltinSessionMetricsPlugin, PluginRegistry};
use spotirust::lyrics::cache::LyricsCache;
use spotirust::lyrics::lrc::{parse_lrc, parse_plain_lyrics};
use spotirust::lyrics::matcher::{
    LyricCandidate, TrackMatchQuery, score_candidate, select_best_candidate,
};
use spotirust::lyrics::model::{LyricWord, LyricsFormat};
use spotirust::lyrics::provider::LyricsService;
use spotirust::lyrics::ttml::{parse_ttml, parse_ttml_time};
use spotirust::storage::credentials::{CredentialStore, MemoryCredentialStore, StoredTokenBundle};
use spotirust::storage::database::{Database, PlaybackPreferences};
use spotirust::ui::theme::{ThemeManager, ThemePalette};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn make_track(id: &str, title: &str, artist: &str, duration_ms: u64) -> Track {
    Track {
        id: id.to_string(),
        uri: format!("spotirust:sample:{id}"),
        title: title.to_string(),
        artists: vec![artist.to_string()],
        album_id: Some("alb1".to_string()),
        album_name: "Studio Album".to_string(),
        duration_ms,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    }
}

// =============================================================================
// 1. OAuth PKCE, State Validation, Loopback Callback & Token Redaction Tests
// =============================================================================

#[test]
fn oauth_pkce_state_and_2025_redirect_rules() {
    // RFC 7636 Appendix B test vector
    assert_eq!(
        compute_s256_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );

    // Spotify Nov 2025 redirect URI rules
    assert!(validate_loopback_redirect_uri("http://127.0.0.1:43821/callback").is_ok());
    assert!(validate_loopback_redirect_uri("http://[::1]:43821/callback").is_ok());
    assert!(validate_loopback_redirect_uri("http://localhost:8888/callback").is_err());
    assert!(validate_loopback_redirect_uri("http://insecure.example.com/callback").is_err());

    let req = PkceAuthRequest::generate(
        "demo-public-client-id",
        "http://127.0.0.1:43821/callback",
        DEFAULT_SCOPES,
    )
    .expect("PKCE request should generate cleanly");

    // Verifier length complies with RFC 7636 (43..=128)
    assert!((43..=128).contains(&req.code_verifier.len()));
    assert!(req.authorize_url.contains("code_challenge_method=S256"));
    assert!(req.authorize_url.contains("response_type=code"));

    // Reject CSRF mismatch & missing state
    assert!(matches!(
        req.validate_callback("code=ok&state=wrong"),
        Err(OAuthError::StateMismatch)
    ));
    assert!(matches!(
        req.validate_callback("code=ok"),
        Err(OAuthError::MissingState)
    ));

    // Accept matching state
    let ok_query = format!("code=auth_code_99&state={}", req.state);
    assert_eq!(req.validate_callback(&ok_query).unwrap(), "auth_code_99");
}

#[test]
fn credential_store_redacts_tokens_and_roundtrips() {
    let store = MemoryCredentialStore::new();
    let bundle = StoredTokenBundle {
        access_token: "super-secret-access-token".to_string(),
        refresh_token: Some("super-secret-refresh-token".to_string()),
        token_type: "Bearer".to_string(),
        scope: "user-read-private".to_string(),
        expires_at_unix: 9_999_999_999,
        client_id: "public-client-id".to_string(),
    };

    // Debug output MUST redact tokens
    let dbg = format!("{bundle:?}");
    assert!(!dbg.contains("super-secret-access-token"));
    assert!(!dbg.contains("super-secret-refresh-token"));
    assert!(dbg.contains("[REDACTED]"));

    store.save_tokens(&bundle).unwrap();
    assert_eq!(store.load_tokens().unwrap(), Some(bundle));

    store.delete_tokens().unwrap();
    assert_eq!(store.load_tokens().unwrap(), None);
}

#[tokio::test]
async fn loopback_callback_and_pkce_token_exchange_and_rotation_protocol() {
    // Spawn a local loopback token endpoint to test our HTTP exchange + refresh-token rotation
    // without touching or mocking live Spotify auth claims.
    let token_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_addr = token_listener.local_addr().unwrap();

    let token_server = tokio::spawn(async move {
        for call in 0..2 {
            let (mut sock, _) = token_listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = sock.read(&mut buf).await.unwrap();
            let req_text = String::from_utf8_lossy(&buf[..n]);

            let body = if call == 0 {
                assert!(req_text.contains("grant_type=authorization_code"));
                assert!(req_text.contains("code_verifier="));
                r#"{"access_token":"tok_initial","token_type":"Bearer","scope":"user-read-private","expires_in":3600,"refresh_token":"ref_v1"}"#
            } else {
                assert!(req_text.contains("grant_type=refresh_token"));
                assert!(req_text.contains("refresh_token=ref_v1"));
                r#"{"access_token":"tok_refreshed","token_type":"Bearer","scope":"user-read-private","expires_in":3600,"refresh_token":"ref_v2"}"#
            };

            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        }
    });

    // Bind real loopback callback server
    let cb_server = LoopbackCallbackServer::bind(0, "/callback").await.unwrap();
    let redirect_uri = cb_server.redirect_uri().to_string();
    let pkce_req =
        PkceAuthRequest::generate("local-test-client", &redirect_uri, DEFAULT_SCOPES).unwrap();
    let expected_state = pkce_req.state.clone();

    let cb_handle = tokio::spawn(async move {
        cb_server
            .wait_for_code(&expected_state, Duration::from_secs(5))
            .await
    });

    let http = reqwest::Client::new();
    let resp = http
        .get(format!(
            "{redirect_uri}?code=loopback_code_42&state={}",
            pkce_req.state
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let code = cb_handle.await.unwrap().unwrap();
    assert_eq!(code, "loopback_code_42");

    let oauth_client =
        SpotifyOAuthClient::new().with_token_url(format!("http://{token_addr}/api/token"));
    let store = Arc::new(MemoryCredentialStore::new());
    let session = SessionManager::new(
        store.clone(),
        Some("local-test-client".to_string()),
        0,
        "/callback".to_string(),
    )
    .with_oauth_client(oauth_client.clone());

    let initial = oauth_client.exchange_code(&pkce_req, &code).await.unwrap();
    assert_eq!(initial.access_token, "tok_initial");
    assert_eq!(initial.refresh_token.as_deref(), Some("ref_v1"));

    session.set_tokens(initial).await;
    let rotated_access = session.force_refresh_access_token().await.unwrap();
    assert_eq!(rotated_access, "tok_refreshed");

    // Verify rotated refresh token was persisted to credential store
    let persisted = store.load_tokens().unwrap().unwrap();
    assert_eq!(persisted.refresh_token.as_deref(), Some("ref_v2"));

    session.logout().await;
    assert!(!session.is_authenticated().await);
    assert!(store.load_tokens().unwrap().is_none());

    token_server.await.unwrap();
}

// =============================================================================
// 2. SQLite Migrations, Repositories, Cache & Reset Tests
// =============================================================================

#[test]
fn sqlite_migrations_persistence_and_reset() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("spotirust_test.db");

    let db = Database::open(&db_path).expect("migrations 0001..0003 should apply cleanly");
    assert_eq!(db.schema_version().unwrap(), 3);

    // Re-opening must be idempotent
    let db = Database::open(&db_path).expect("re-opening migrated db should succeed");
    assert_eq!(db.schema_version().unwrap(), 3);

    // Save & load profile (metadata only, zero tokens)
    let profile = UserProfile {
        id: "user_1".to_string(),
        account_id: Some("acct_immutable_99".to_string()),
        display_name: "Rust Listener".to_string(),
        avatar_url: None,
        uri: Some("spotify:user:user_1".to_string()),
    };
    db.save_user_profile(&profile).unwrap();
    assert_eq!(db.load_user_profile().unwrap(), Some(profile));

    // Tracks, albums, playlists, recently played
    let mut track = make_track("t1", "Midnight Compiler", "Spotirust Soundcheck", 22_000);
    track.liked = true;
    db.upsert_tracks(&[track.clone()]).unwrap();
    assert_eq!(db.load_liked_tracks(10).unwrap().len(), 1);

    db.record_recently_played(&track, None).unwrap();
    assert_eq!(db.load_recently_played(10).unwrap().len(), 1);

    let album = Album {
        id: "alb1".to_string(),
        uri: "spotify:album:alb1".to_string(),
        name: "Native Audio".to_string(),
        artists: vec!["Spotirust".to_string()],
        release_date: Some("2026-11-01".to_string()),
        total_tracks: 8,
        artwork_url: None,
        accent_rgb: None,
    };
    db.upsert_albums(&[album]).unwrap();
    assert_eq!(db.load_cached_albums(10).unwrap().len(), 1);

    let playlist = Playlist {
        id: "pl1".to_string(),
        uri: "spotify:playlist:pl1".to_string(),
        name: "Coding Focus".to_string(),
        description: "Deep work".to_string(),
        owner_name: "Rust Listener".to_string(),
        owner_id: "user_1".to_string(),
        snapshot_id: "snap1".to_string(),
        total_tracks: 12,
        artwork_url: None,
        collaborative: false,
        public: Some(false),
    };
    db.upsert_playlists(&[playlist]).unwrap();
    assert_eq!(db.load_cached_playlists(10).unwrap().len(), 1);

    // Settings & playback preferences
    let settings = UserSettings {
        theme_id: "midnight_cobalt".to_string(),
        lyrics_offset_ms: -100,
        ..Default::default()
    };
    db.save_settings(&settings).unwrap();
    let loaded_settings = db.load_settings().unwrap();
    assert_eq!(loaded_settings.theme_id, "midnight_cobalt");
    assert_eq!(loaded_settings.lyrics_offset_ms, -100);

    let prefs = PlaybackPreferences {
        volume: 0.65,
        shuffle: true,
        repeat_mode: RepeatMode::Context,
        backend_kind: AudioBackendKind::LocalRodio,
        last_track: Some(track),
        last_position_ms: 8_400,
        last_queue: bundled_showcase_tracks(),
    };
    db.save_playback_preferences(&prefs).unwrap();
    let loaded_prefs = db.load_playback_preferences().unwrap();
    assert!((loaded_prefs.volume - 0.65).abs() < 0.01);
    assert!(loaded_prefs.shuffle);
    assert_eq!(loaded_prefs.repeat_mode, RepeatMode::Context);
    assert_eq!(loaded_prefs.last_position_ms, 8_400);
    assert_eq!(loaded_prefs.last_queue.len(), 2);

    // Clear cache preserves settings & profile
    let removed = db.clear_cache().unwrap();
    assert!(removed >= 3);
    assert!(db.load_user_profile().unwrap().is_some());
    assert_eq!(db.load_settings().unwrap().theme_id, "midnight_cobalt");

    // Reset application wipes everything cleanly
    db.reset_application().unwrap();
    assert!(db.load_user_profile().unwrap().is_none());
    assert_eq!(db.load_settings().unwrap().theme_id, "obsidian_dark");
    assert_eq!(db.cache_stats().unwrap().cached_tracks, 0);
}

// =============================================================================
// 3. TTML, Enhanced LRC, Standard LRC, Seek Accuracy & Anti-Remix Matcher Tests
// =============================================================================

#[test]
fn ttml_and_lrc_parsers_handle_word_timing_short_words_and_seeking() {
    assert_eq!(parse_ttml_time("01:02.345"), Some(62_345));
    assert_eq!(parse_ttml_time("2.5s"), Some(2_500));
    assert_eq!(parse_ttml_time("450ms"), Some(450));

    let ttml = include_str!("../assets/sample_lyrics/spotirust_demo.ttml");
    let doc = parse_ttml(ttml, "integration").unwrap();
    assert_eq!(doc.format, LyricsFormat::Ttml);
    assert!(doc.has_word_timing());
    assert_eq!(doc.lines.len(), 4);

    // Seek forward to Chorus, then backward to Verse — must never desync
    assert_eq!(doc.active_line_index(10_500), Some(2));
    assert_eq!(doc.active_line_index(15_200), Some(3));
    assert_eq!(doc.active_line_index(1_200), Some(0));
    assert_eq!(doc.active_line_index(100), None);

    // Very short word (< 65ms) must still ramp smoothly over minimum visual window
    let short_word = LyricWord {
        text: "go".to_string(),
        start_ms: 1_000,
        end_ms: 1_015,
        trailing_space: true,
    };
    assert_eq!(short_word.progress_at(999), 0.0);
    let mid = short_word.progress_at(1_032);
    assert!(mid > 0.3 && mid < 0.7, "expected smooth ramp, got {mid}");
    assert_eq!(short_word.progress_at(1_070), 1.0);

    // Enhanced LRC
    let lrc = include_str!("../assets/sample_lyrics/spotirust_demo.lrc");
    let lrc_doc = parse_lrc(lrc, "integration").unwrap();
    assert_eq!(lrc_doc.format, LyricsFormat::EnhancedLrc);
    assert!(lrc_doc.has_word_timing());
    assert_eq!(lrc_doc.active_line_index(5_000), Some(1));

    // Standard LRC + plain fallback
    let std_lrc = parse_lrc("[00:05.00]Line one\n[00:10.00]Line two\n", "std").unwrap();
    assert_eq!(std_lrc.format, LyricsFormat::StandardLrc);
    assert!(!std_lrc.has_word_timing());
    assert_eq!(std_lrc.active_line_index(7_000), Some(0));
    assert_eq!(std_lrc.active_line_index(11_000), Some(1));

    let plain = parse_plain_lyrics("First line\nSecond line", "plain");
    assert_eq!(plain.format, LyricsFormat::PlainText);
    assert_eq!(plain.lines.len(), 2);
}

#[test]
fn track_matcher_prevents_live_and_remix_false_positives() {
    let query = TrackMatchQuery {
        title: "Blinding Lights".to_string(),
        artist: "The Weeknd".to_string(),
        album: "After Hours".to_string(),
        duration_ms: 200_000,
        isrc: None,
    };

    let candidates = vec![
        LyricCandidate {
            title: "Blinding Lights (Live at SoFi Stadium)".to_string(),
            artist: "The Weeknd".to_string(),
            album: Some("Live at SoFi".to_string()),
            duration_ms: Some(200_000),
            isrc: None,
            has_word_sync: true,
            has_line_sync: true,
        },
        LyricCandidate {
            title: "Blinding Lights - Chromatics Remix".to_string(),
            artist: "The Weeknd".to_string(),
            album: Some("After Hours Remixes".to_string()),
            duration_ms: Some(200_500),
            isrc: None,
            has_word_sync: true,
            has_line_sync: true,
        },
        LyricCandidate {
            title: "Blinding Lights (Sped Up)".to_string(),
            artist: "The Weeknd".to_string(),
            album: Some("After Hours".to_string()),
            duration_ms: Some(199_000),
            isrc: None,
            has_word_sync: true,
            has_line_sync: true,
        },
        LyricCandidate {
            title: "Blinding Lights".to_string(),
            artist: "The Weeknd".to_string(),
            album: Some("After Hours".to_string()),
            duration_ms: Some(200_400),
            isrc: None,
            has_word_sync: false,
            has_line_sync: true,
        },
    ];

    assert!(score_candidate(&query, &candidates[0]).is_none());
    assert!(score_candidate(&query, &candidates[1]).is_none());
    assert!(score_candidate(&query, &candidates[2]).is_none());
    assert_eq!(
        select_best_candidate(&query, &candidates).map(|(i, _)| i),
        Some(3)
    );
}

#[tokio::test]
async fn lyrics_service_prefers_ttml_over_lrc_and_caches_in_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let lyrics_dir = dir.path().join("lyrics");
    std::fs::create_dir_all(&lyrics_dir).unwrap();

    // Place BOTH .lrc and .ttml for the same track — TTML must win!
    std::fs::write(
        lyrics_dir.join("Aurora - Horizon.lrc"),
        "[00:01.00]Fallback LRC line\n",
    )
    .unwrap();
    std::fs::write(
        lyrics_dir.join("Aurora - Horizon.ttml"),
        r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" itunes:timing="Word">
            <body><div><p begin="00:01.000" end="00:03.000">
                <span begin="00:01.000" end="00:02.000">Word</span>
                <span begin="00:02.000" end="00:03.000">TTML</span>
            </p></div></body>
        </tt>"#,
    )
    .unwrap();

    let db = Database::open_in_memory().unwrap();
    let service = LyricsService::new(LyricsCache::new(db.clone()), vec![lyrics_dir.clone()]);
    let track = make_track("aurora_1", "Horizon", "Aurora", 180_000);

    let resolved = service.resolve(&track, false, false).await.unwrap();
    assert_eq!(resolved.format, LyricsFormat::Ttml);
    assert_eq!(resolved.lines[0].plain_text(), "Word TTML");

    // Delete files on disk — SQLite cache must still serve the cached TTML!
    std::fs::remove_dir_all(&lyrics_dir).unwrap();
    let cached = service.resolve(&track, false, false).await.unwrap();
    assert_eq!(cached.format, LyricsFormat::Ttml);
    assert_eq!(db.cache_stats().unwrap().cached_lyrics, 1);
}

#[tokio::test]
async fn lyrics_edge_cases_seek_pause_resume_malformed_and_override() {
    use spotirust::audio::engine::PlaybackPosition;
    use spotirust::lyrics::ttml::TtmlParseError;

    // 1. Word spans without `end`, duplicate timestamps, and ultra-short word (10ms)
    let edge_ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml">
        <body><div>
            <p begin="00:01.000" end="00:04.000">
                <span begin="00:01.000">Go</span>
                <span begin="00:01.000">now</span>
                <span begin="00:01.800" end="00:01.810">!</span>
                <span begin="00:02.500">finish</span>
            </p>
            <p begin="00:05.000">
                <span begin="00:05.000">Second</span>
                <span begin="00:06.000">line</span>
            </p>
        </div></body>
    </tt>"#;

    let doc = parse_ttml(edge_ttml, "edge").unwrap();
    assert_eq!(doc.format, LyricsFormat::Ttml);
    let l0 = &doc.lines[0];
    assert_eq!((l0.words[0].start_ms, l0.words[0].end_ms), (1_000, 1_800));
    assert_eq!((l0.words[1].start_ms, l0.words[1].end_ms), (1_000, 1_800));
    // Ultra-short 10ms word clamps smoothly to 65ms visual window
    assert_eq!(l0.words[2].duration_ms(), 10);
    assert!(l0.words[2].progress_at(1_832) > 0.3 && l0.words[2].progress_at(1_832) < 0.7);
    assert_eq!(l0.words[2].progress_at(1_870), 1.0);
    assert_eq!((l0.words[3].start_ms, l0.words[3].end_ms), (2_500, 4_000));

    // 2. Repeated forward/backward seeking + pause/resume clock stability (zero drift!)
    let mut clock = PlaybackPosition::new(1_200, 10_000, true);
    assert_eq!(doc.active_line_index(clock.current_ms()), Some(0));
    // Seek forward to line 1
    clock = PlaybackPosition::new(5_500, 10_000, true);
    assert_eq!(doc.active_line_index(clock.current_ms()), Some(1));
    // Pause and verify position stays frozen across sleep
    let paused = PlaybackPosition::new(5_500, 10_000, false);
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(paused.current_ms(), 5_500);
    assert_eq!(doc.active_line_index(paused.current_ms()), Some(1));
    // Seek backward repeatedly
    for target in [1_400u64, 5_900, 200, 2_600, 5_100, 1_050] {
        let expected = match target {
            200 => None,
            1_050 | 1_400 | 2_600 => Some(0),
            _ => Some(1),
        };
        assert_eq!(doc.active_line_index(target), expected);
    }

    // 3. Malformed / truncated TTML must fail cleanly (never panic or silently corrupt)
    assert!(matches!(
        parse_ttml("<tt><body><p begin=\"00:01\">cut off", "bad"),
        Err(TtmlParseError::Xml(_))
    ));

    // 4. Manual local `.ttml` override replaces an already-cached Standard LRC entry
    let dir = tempfile::tempdir().unwrap();
    let lrc_path = dir.path().join("track.lrc");
    let ttml_path = dir.path().join("track.ttml");
    std::fs::write(&lrc_path, "[00:01.00]Plain line LRC\n").unwrap();
    std::fs::write(&ttml_path, edge_ttml).unwrap();

    let db = Database::open_in_memory().unwrap();
    let svc = LyricsService::new(LyricsCache::new(db), vec![]);
    let track = make_track("override_1", "Override Song", "Artist", 10_000);

    let first = svc.import_for_track(&track, &lrc_path).unwrap();
    assert_eq!(first.format, LyricsFormat::StandardLrc);
    let upgraded = svc.import_for_track(&track, &ttml_path).unwrap();
    assert_eq!(upgraded.format, LyricsFormat::Ttml);
    let from_cache = svc.resolve(&track, false, false).await.unwrap();
    assert_eq!(from_cache.format, LyricsFormat::Ttml);
}

// =============================================================================
// 4. Playback Engine, Queue State, Seek, Shuffle, Repeat & Event Bus Tests
// =============================================================================

#[tokio::test]
async fn oauth_single_flight_refresh_revocation_and_access_denied() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // 1. Verify single-flight refresh lock: 5 concurrent callers with the same expired/401 token
    //    MUST issue only ONE HTTP `grant_type=refresh_token` request!
    let refresh_hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = Arc::clone(&refresh_hits);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        // Expect 1 refresh call + 1 invalid_grant call
        for call in 0..2 {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await.unwrap();
            if call == 0 {
                hits_clone.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(60)).await;
                let body = r#"{"access_token":"tok_new","token_type":"Bearer","scope":"user-read-private","expires_in":3600,"refresh_token":"ref_rotated"}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            } else {
                let body =
                    r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#;
                let resp = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        }
    });

    let oauth = SpotifyOAuthClient::new().with_token_url(format!("http://{addr}/api/token"));
    let store = Arc::new(MemoryCredentialStore::new());
    let session = SessionManager::new(
        store.clone(),
        Some("test-client".to_string()),
        0,
        "/callback".to_string(),
    )
    .with_oauth_client(oauth.clone());

    session
        .set_tokens(StoredTokenBundle {
            access_token: "tok_old".to_string(),
            refresh_token: Some("ref_old".to_string()),
            token_type: "Bearer".to_string(),
            scope: "user-read-private".to_string(),
            expires_at_unix: 1, // expired
            client_id: "test-client".to_string(),
        })
        .await;

    let mut handles = Vec::new();
    for _ in 0..5 {
        let s = session.clone();
        handles.push(tokio::spawn(async move {
            s.force_refresh_if_stale(Some("tok_old")).await
        }));
    }
    for h in handles {
        assert_eq!(h.await.unwrap().unwrap(), "tok_new");
    }
    assert_eq!(
        refresh_hits.load(Ordering::SeqCst),
        1,
        "concurrent refreshes must coalesce into a single HTTP token refresh"
    );

    // 2. Verify permanent 400 invalid_grant clears revoked credentials from OS vault during restore
    store
        .save_tokens(&StoredTokenBundle {
            access_token: "tok_expired".to_string(),
            refresh_token: Some("ref_revoked".to_string()),
            token_type: "Bearer".to_string(),
            scope: "user-read-private".to_string(),
            expires_at_unix: 1,
            client_id: "test-client".to_string(),
        })
        .unwrap();
    let restore_res = session.restore_session().await;
    assert!(matches!(
        restore_res,
        Err(OAuthError::TokenRejected { status: 400, .. })
    ));
    assert!(
        store.load_tokens().unwrap().is_none(),
        "permanently revoked tokens must be deleted from credential store"
    );

    // 3. Verify transient network failure preserves stored tokens for offline recovery
    let offline_oauth = SpotifyOAuthClient::new().with_token_url("http://127.0.0.1:1/api/token");
    let offline_store = Arc::new(MemoryCredentialStore::new());
    offline_store
        .save_tokens(&StoredTokenBundle {
            access_token: "tok_exp".to_string(),
            refresh_token: Some("ref_still_valid".to_string()),
            token_type: "Bearer".to_string(),
            scope: "user-read-private".to_string(),
            expires_at_unix: 1,
            client_id: "test-client".to_string(),
        })
        .unwrap();
    let offline_session = SessionManager::new(
        offline_store.clone(),
        Some("test-client".to_string()),
        0,
        "/callback".to_string(),
    )
    .with_oauth_client(offline_oauth);
    assert!(matches!(
        offline_session.restore_session().await,
        Err(OAuthError::Http(_))
    ));
    assert!(
        offline_store.load_tokens().unwrap().is_some(),
        "transient network error must preserve stored refresh token"
    );

    // 4. Verify loopback callback handles `error=access_denied` cleanly & timeout returns TimedOut
    let cb_denied = LoopbackCallbackServer::bind(0, "/callback").await.unwrap();
    let denied_uri = cb_denied.redirect_uri().to_string();
    let denied_task = tokio::spawn(async move {
        cb_denied
            .wait_for_code("state_deny", Duration::from_secs(5))
            .await
    });
    let resp = reqwest::Client::new()
        .get(format!("{denied_uri}?error=access_denied&state=state_deny"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let err = denied_task.await.unwrap().unwrap_err();
    assert!(matches!(err, OAuthError::AccessDenied { .. }));
    assert!(err.to_string().contains("cancelled"));

    let cb_timeout = LoopbackCallbackServer::bind(0, "/callback").await.unwrap();
    assert!(matches!(
        cb_timeout
            .wait_for_code("state_timeout", Duration::from_millis(80))
            .await,
        Err(OAuthError::TimedOut)
    ));

    server.await.unwrap();
}

// =============================================================================
// 4. Playback Mode Separation, Connect Confirmation & Local Rodio Tests
// =============================================================================

#[tokio::test]
async fn playback_never_fakes_playing_when_unauthenticated_or_unconfirmed() {
    use spotirust::audio::engine::AudioError;
    use spotirust::audio::local_rodio::{ensure_soundcheck_wavs, probe_audio_duration_ms};
    use spotirust::core::models::bundled_showcase_tracks_with_paths;
    use spotirust::spotify::client::SpotifyClient;

    // A. Real WAV generation & duration probing works cleanly
    let dir = tempfile::tempdir().unwrap();
    let (midnight_wav, velocity_wav) = ensure_soundcheck_wavs(dir.path()).unwrap();
    let midnight_dur = probe_audio_duration_ms(&midnight_wav);
    let velocity_dur = probe_audio_duration_ms(&velocity_wav);
    assert!(
        (21_800..=22_200).contains(&midnight_dur),
        "expected ~22s WAV, got {midnight_dur}ms"
    );
    assert!(
        (18_800..=19_200).contains(&velocity_dur),
        "expected ~19s WAV, got {velocity_dur}ms"
    );

    // B. Signed-out Spotify track playback MUST fail with Unauthenticated (zero fake clock!)
    let offline_controller = PlaybackController::for_testing();
    let spotify_track = Track {
        id: "sp_1".to_string(),
        uri: "spotify:track:sp_1".to_string(),
        title: "Remote Track".to_string(),
        artists: vec!["Artist".to_string()],
        album_id: None,
        album_name: "Album".to_string(),
        duration_ms: 180_000,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    let err = offline_controller
        .play_track_in_context(spotify_track.clone(), vec![spotify_track.clone()])
        .await
        .unwrap_err();
    assert!(matches!(err, AudioError::Unauthenticated));
    let snap = offline_controller.snapshot().await;
    assert!(!snap.status.is_playing());
    assert!(!snap.position.is_playing);

    // C. Local WAV playback: either opens a real OS audio sink OR honestly reports BackendUnavailable
    //    (NEVER fakes Playing without a real audio device!)
    let local_tracks = bundled_showcase_tracks_with_paths(Some(midnight_wav), Some(velocity_wav));
    match offline_controller
        .play_track_in_context(local_tracks[0].clone(), local_tracks.clone())
        .await
    {
        Ok(()) => {
            assert_eq!(
                offline_controller.snapshot().await.status,
                PlaybackStatus::Playing
            );
            offline_controller.pause().await.unwrap();
            assert_eq!(
                offline_controller.snapshot().await.status,
                PlaybackStatus::Paused
            );
        }
        Err(AudioError::BackendUnavailable(_)) => {
            let snap = offline_controller.snapshot().await;
            assert!(matches!(snap.status, PlaybackStatus::Error(_)));
            assert!(!snap.position.is_playing);
        }
        Err(other) => panic!("unexpected local playback error: {other}"),
    }

    // D. Spotify Connect confirmation test via local loopback HTTP server:
    //    Verifies Play/Pause/Seek/Next/Volume/Queue routing AND that unconfirmed 204 refuses Playing!
    let api_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_addr = api_listener.local_addr().unwrap();

    let api_server = tokio::spawn(async move {
        let mut is_playing = false;
        let mut progress_ms: u64 = 0;
        let mut current_track_id = "sp_1".to_string();
        let mut volume_pct: u8 = 80;
        let mut simulate_no_device = false;

        loop {
            let Ok((mut sock, _)) = api_listener.accept().await else {
                break;
            };
            let mut buf = vec![0u8; 8192];
            let Ok(n) = sock.read(&mut buf).await else {
                break;
            };
            if n == 0 {
                continue;
            }
            let req = String::from_utf8_lossy(&buf[..n]);
            let first_line = req.lines().next().unwrap_or("");
            let mut parts = first_line.split_whitespace();
            let method = parts.next().unwrap_or("");
            let target = parts.next().unwrap_or("");

            let (status, body) = match (method, target) {
                ("PUT", t) if t.starts_with("/me/player/play") => {
                    if !simulate_no_device {
                        is_playing = true;
                        progress_ms = 250;
                    }
                    ("204 No Content", String::new())
                }
                ("PUT", t) if t.starts_with("/me/player/pause") => {
                    is_playing = false;
                    ("204 No Content", String::new())
                }
                ("PUT", t) if t.starts_with("/me/player/seek") => {
                    if let Some(pos) = t
                        .split("position_ms=")
                        .nth(1)
                        .and_then(|s| s.split('&').next())
                        .and_then(|s| s.parse::<u64>().ok())
                    {
                        progress_ms = pos;
                    }
                    ("204 No Content", String::new())
                }
                ("PUT", t) if t.starts_with("/me/player/volume") => {
                    if let Some(v) = t
                        .split("volume_percent=")
                        .nth(1)
                        .and_then(|s| s.split('&').next())
                        .and_then(|s| s.parse::<u8>().ok())
                    {
                        volume_pct = v;
                    }
                    ("204 No Content", String::new())
                }
                ("POST", t) if t.starts_with("/me/player/next") => {
                    current_track_id = "sp_2".to_string();
                    progress_ms = 0;
                    is_playing = true;
                    ("204 No Content", String::new())
                }
                ("POST", t) if t.starts_with("/me/player/queue") => {
                    // Toggle no-device mode on special test track URI to verify anti-fake guard
                    if t.contains("sp_no_device") {
                        simulate_no_device = true;
                    }
                    ("204 No Content", String::new())
                }
                ("GET", t) if t.starts_with("/me/player/queue") => (
                    "200 OK",
                    format!(
                        r#"{{"currently_playing":{{"id":"{current_track_id}","uri":"spotify:track:{current_track_id}","name":"Track {current_track_id}","artists":[{{"name":"Artist"}}],"duration_ms":180000}},"queue":[]}}"#
                    ),
                ),
                ("GET", "/me/player") => {
                    if simulate_no_device {
                        ("204 No Content", String::new())
                    } else {
                        (
                            "200 OK",
                            format!(
                                r#"{{"is_playing":{is_playing},"progress_ms":{progress_ms},"shuffle_state":false,"repeat_state":"off","device":{{"id":"dev_1","name":"Studio MacBook","type":"Computer","is_active":true,"is_restricted":false,"volume_percent":{volume_pct}}},"item":{{"id":"{current_track_id}","uri":"spotify:track:{current_track_id}","name":"Track {current_track_id}","artists":[{{"name":"Artist"}}],"duration_ms":180000}}}}"#
                            ),
                        )
                    }
                }
                _ => ("204 No Content", String::new()),
            };

            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        }
    });

    let store = Arc::new(MemoryCredentialStore::new());
    let session = SessionManager::new(store, Some("cid".to_string()), 0, "/callback".to_string());
    session
        .set_tokens(StoredTokenBundle {
            access_token: "valid_tok".to_string(),
            refresh_token: Some("ref".to_string()),
            token_type: "Bearer".to_string(),
            scope: "user-modify-playback-state user-read-playback-state".to_string(),
            expires_at_unix: 9_999_999_999,
            client_id: "cid".to_string(),
        })
        .await;

    let client = SpotifyClient::new(session).with_base_url(format!("http://{api_addr}"));
    let db = Database::open_in_memory().unwrap();
    let bus = PlaybackEventBus::new(64);
    let mut rx = bus.subscribe();
    let connect_controller = PlaybackController::new(Some(client), Some(db.clone()), bus);

    // 1. Confirmed Connect play sets Playing + device name
    connect_controller
        .play_track_in_context(spotify_track.clone(), vec![spotify_track.clone()])
        .await
        .unwrap();
    let snap = connect_controller.snapshot().await;
    assert_eq!(snap.status, PlaybackStatus::Playing);
    assert_eq!(snap.backend, AudioBackendKind::SpotifyConnect);
    assert_eq!(snap.active_device_name.as_deref(), Some("Studio MacBook"));

    // 2. Seek confirmed by remote device
    connect_controller.seek(45_000).await.unwrap();
    assert!(connect_controller.get_position().await.current_ms() >= 45_000);

    // 3. Pause confirmed by remote device
    connect_controller.pause().await.unwrap();
    assert_eq!(
        connect_controller.snapshot().await.status,
        PlaybackStatus::Paused
    );

    // 4. Volume & Next route directly to Spotify Connect
    connect_controller.set_volume(0.55).await.unwrap();
    assert!((connect_controller.snapshot().await.volume - 0.55).abs() < 0.02);
    connect_controller.next().await.unwrap();
    assert_eq!(
        connect_controller
            .snapshot()
            .await
            .current_track
            .as_ref()
            .unwrap()
            .id,
        "sp_2"
    );

    // 5. Verify Seeked event arrived on bus
    let mut saw_seek = false;
    while let Ok(evt) = rx.try_recv() {
        if matches!(
            evt,
            PlaybackEvent::Seeked {
                position_ms: 45_000
            }
        ) {
            saw_seek = true;
        }
    }
    assert!(saw_seek);

    // 6. Trigger 204 No Content on GET /me/player -> Play MUST fail with NoActiveDevice and NEVER show Playing!
    let mut trigger_track = spotify_track.clone();
    trigger_track.uri = "spotify:track:sp_no_device".to_string();
    connect_controller
        .add_to_queue(trigger_track)
        .await
        .unwrap();
    let unconfirmed_err = connect_controller
        .play_track_in_context(spotify_track.clone(), vec![spotify_track])
        .await
        .unwrap_err();
    assert!(matches!(unconfirmed_err, AudioError::NoActiveDevice));
    assert!(!connect_controller.snapshot().await.status.is_playing());

    api_server.abort();
}

// =============================================================================
// 5. Themes, Plugins & Local Audio Scanner Tests
// =============================================================================

#[test]
fn themes_plugins_and_local_audio_scanner() {
    let dir = tempfile::tempdir().unwrap();

    // Custom JSON theme discovery
    let custom_json = include_str!("../assets/themes/nordic_slate.json");
    let parsed = ThemePalette::from_json_str(custom_json).unwrap();
    assert_eq!(parsed.id, "nordic_slate");

    let mut mgr = ThemeManager::new(dir.path(), Vec::new());
    assert!(mgr.list().len() >= 5);
    assert!(mgr.set_active("nordic_slate"));
    assert_eq!(mgr.active().name, "Nordic Slate");

    // Plugin registry
    let mut plugins = PluginRegistry::new();
    plugins.register(Arc::new(BuiltinSessionMetricsPlugin));
    assert_eq!(plugins.len(), 1);
    plugins.notify_track_changed(&bundled_showcase_tracks()[0]);

    // Local audio scanner filters extensions, validates decodability, parses "Artist - Title",
    // and safely skips corrupt/fake files
    let music_dir = dir.path().join("music");
    std::fs::create_dir_all(&music_dir).unwrap();
    spotirust::audio::local_rodio::write_synth_wav(
        &music_dir.join("Daft Punk - Voyager.wav"),
        0.5,
        &[[220.0, 261.63, 329.63]],
        1.0,
    )
    .unwrap();
    std::fs::write(music_dir.join("corrupt_fake.flac"), b"fake").unwrap();
    std::fs::write(music_dir.join("notes.txt"), b"ignore").unwrap();

    let scanned = scan_local_audio_dir(&music_dir);
    assert_eq!(scanned.len(), 1);
    assert_eq!(scanned[0].primary_artist(), "Daft Punk");
    assert_eq!(scanned[0].title, "Voyager");
}

// =============================================================================
// 6. Spotify Connect HTTP 429 Cooldown & Mid-Playback Device Disappearance Tests
// =============================================================================

#[tokio::test]
async fn spotify_connect_rate_limit_429_and_device_disappearance() {
    use spotirust::audio::engine::AudioError;
    use spotirust::spotify::client::SpotifyClient;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let hit_count = Arc::new(AtomicUsize::new(0));
    let mode = Arc::new(AtomicUsize::new(0)); // 0=playing, 1=204 disappeared, 2=429 rate-limited

    let hits_srv = Arc::clone(&hit_count);
    let mode_srv = Arc::clone(&mode);
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let mut buf = vec![0u8; 4096];
            let Ok(n) = sock.read(&mut buf).await else {
                break;
            };
            if n == 0 {
                continue;
            }
            hits_srv.fetch_add(1, Ordering::SeqCst);
            let req = String::from_utf8_lossy(&buf[..n]);
            let first = req.lines().next().unwrap_or("");
            let m = mode_srv.load(Ordering::SeqCst);

            let resp = if m == 2 {
                let body = r#"{"error":{"status":429,"message":"Too Many Requests","reason":"QUOTA_EXCEEDED"}}"#;
                format!(
                    "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 3\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else if first.starts_with("GET /me/player/queue") {
                let body = r#"{"currently_playing":null,"queue":[]}"#;
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else if first.starts_with("GET /me/player") {
                if m == 1 {
                    "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                } else {
                    let body = r#"{"is_playing":true,"progress_ms":5000,"shuffle_state":false,"repeat_state":"off","device":{"id":"dev_99","name":"Living Room Speaker","type":"Speaker","is_active":true,"is_restricted":false,"volume_percent":65},"item":{"id":"sp_live","uri":"spotify:track:sp_live","name":"Connect Song","artists":[{"name":"Live Artist"}],"duration_ms":200000}}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
            } else {
                "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string()
            };
            let _ = sock.write_all(resp.as_bytes()).await;
        }
    });

    let store = Arc::new(MemoryCredentialStore::new());
    let session = SessionManager::new(store, Some("cid".into()), 0, "/callback".into());
    session
        .set_tokens(StoredTokenBundle {
            access_token: "tok_live".into(),
            refresh_token: Some("ref".into()),
            token_type: "Bearer".into(),
            scope: "user-read-playback-state user-modify-playback-state".into(),
            expires_at_unix: 9_999_999_999,
            client_id: "cid".into(),
        })
        .await;

    let client = SpotifyClient::new(session).with_base_url(format!("http://{addr}"));
    let bus = PlaybackEventBus::new(64);
    let mut rx = bus.subscribe();
    let controller = PlaybackController::new(Some(client.clone()), None, bus);

    // 1. Sync active Connect device -> Confirmed Playing
    controller.sync_connect_now().await.unwrap();
    let snap = controller.snapshot().await;
    assert_eq!(snap.status, PlaybackStatus::Playing);
    assert_eq!(
        snap.active_device_name.as_deref(),
        Some("Living Room Speaker")
    );

    // 2. Simulate device disappearing mid-playback (GET /me/player returns 204)
    mode.store(1, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(2_550)).await;
    controller.tick_background().await;
    let snap_after_drop = controller.snapshot().await;
    assert_eq!(snap_after_drop.status, PlaybackStatus::Stopped);
    assert!(!snap_after_drop.position.is_playing);
    assert!(snap_after_drop.active_device_name.is_none());

    let mut saw_disconnect_banner = false;
    while let Ok(evt) = rx.try_recv() {
        if let PlaybackEvent::Error(msg) = evt
            && msg.contains("Living Room Speaker")
        {
            saw_disconnect_banner = true;
        }
    }
    assert!(
        saw_disconnect_banner,
        "device disconnect mid-playback must emit clear status error"
    );

    // 3. Simulate HTTP 429 Too Many Requests with Retry-After: 3s
    mode.store(2, Ordering::SeqCst);
    let err = controller.sync_connect_now().await.unwrap_err();
    assert!(matches!(
        err,
        AudioError::RateLimited {
            retry_after_secs: 3
        }
    ));
    assert!(client.rate_limit_remaining().await.is_some());

    // Subsequent immediate requests MUST NOT hit the HTTP server while cooldown is active!
    let hits_before_spam = hit_count.load(Ordering::SeqCst);
    for _ in 0..5 {
        let blocked = controller.sync_connect_now().await.unwrap_err();
        assert!(matches!(blocked, AudioError::RateLimited { .. }));
        controller.tick_background().await;
    }
    assert_eq!(
        hit_count.load(Ordering::SeqCst),
        hits_before_spam,
        "client must suppress all outgoing HTTP calls during HTTP 429 Retry-After cooldown"
    );

    server.abort();
}

// =============================================================================
// 7. Artwork Disk & RAM Cache Eviction Stability Tests
// =============================================================================

#[tokio::test]
async fn artwork_disk_and_ram_cache_eviction_stability() {
    use spotirust::storage::artwork::{ArtworkService, decode_and_sample, store_embedded_artwork};
    use spotirust::ui::components::artwork::ArtworkTextureCache;

    let dir = tempfile::tempdir().unwrap();
    let cache_dir = dir.path().join("art_cache");
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Generate 12 valid PNG files on disk exceeding max_files=4 and max_bytes=10_000
    for i in 0..12u8 {
        let mut img = image::RgbaImage::new(32, 32);
        for p in img.pixels_mut() {
            *p = image::Rgba([i.saturating_mul(20), 120, 200, 255]);
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        img.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let raw = bytes.into_inner();
        let file = cache_dir.join(format!("remote_{i:02}.img"));
        std::fs::write(&file, &raw).unwrap();
        std::thread::sleep(Duration::from_millis(6));
    }

    let svc = ArtworkService::with_limits(cache_dir.clone(), 10_000, 4);
    let evicted_disk = svc.enforce_disk_limits().unwrap();
    assert!(evicted_disk >= 8);
    let stats = svc.disk_stats();
    assert!(stats.file_count <= 4);
    assert!(stats.total_bytes <= 10_000);

    // Embedded localart:// storage & async decode
    let mut cover = image::RgbaImage::new(16, 16);
    for p in cover.pixels_mut() {
        *p = image::Rgba([30, 215, 96, 255]);
    }
    let mut png_buf = std::io::Cursor::new(Vec::new());
    cover
        .write_to(&mut png_buf, image::ImageFormat::Png)
        .unwrap();
    let png_bytes = png_buf.into_inner();
    let local_uri = store_embedded_artwork(&cache_dir, &png_bytes).unwrap();
    let decoded = svc.fetch_and_decode(&local_uri).await.unwrap();
    assert_eq!(decoded.width, 16);
    assert_eq!(decoded.height, 16);

    // Simulate browsing 160 distinct album covers through RAM GPU texture cache (cap = 32)
    let ctx = egui::Context::default();
    let mut gpu_cache = ArtworkTextureCache::with_capacity(32);
    let sample = decode_and_sample("sample", &png_bytes).unwrap();
    for i in 0..160usize {
        gpu_cache.insert(
            &ctx,
            format!("https://i.scdn.co/image/cover_{i}"),
            sample.width,
            sample.height,
            sample.rgba.clone(),
            sample.dominant_rgb,
        );
        assert!(
            gpu_cache.len() <= 32,
            "RAM GPU texture cache must never exceed its configured bound"
        );
    }
    assert_eq!(gpu_cache.len(), 32);
    assert_eq!(gpu_cache.evicted_count(), 128);
}

// =============================================================================
// 8. Local Library SQLite Persistence, Queue Persistence & Restart Restore Tests
// =============================================================================

#[tokio::test]
async fn local_library_sqlite_persistence_and_queue_restore_across_restarts() {
    use spotirust::audio::local_rodio::{
        import_local_files_into_db, scan_local_library_into_db, write_synth_wav,
    };

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("spotirust.db");
    let music_dir = dir.path().join("music");
    let extra_dir = dir.path().join("singles");
    let art_dir = dir.path().join("art");
    std::fs::create_dir_all(&music_dir).unwrap();
    std::fs::create_dir_all(&extra_dir).unwrap();

    let song1 = music_dir.join("Band A - Alpha.wav");
    let song2 = music_dir.join("Band B - Beta.wav");
    let single = extra_dir.join("Solo C - Gamma.wav");
    write_synth_wav(&song1, 0.7, &[[220.0, 277.18, 329.63]], 1.0).unwrap();
    write_synth_wav(&song2, 0.9, &[[246.94, 311.13, 369.99]], 1.0).unwrap();
    write_synth_wav(&single, 0.6, &[[261.63, 329.63, 392.0]], 1.0).unwrap();

    // Session 1: scan folder, import an extra single, persist queue + playback position
    {
        let db = Database::open(&db_path).unwrap();
        let scan =
            scan_local_library_into_db(&db, std::slice::from_ref(&music_dir), &art_dir, false);
        assert_eq!(scan.parsed_count, 2);
        assert_eq!(scan.tracks.len(), 2);

        let import = import_local_files_into_db(&db, std::slice::from_ref(&single), &art_dir);
        assert_eq!(import.parsed_count, 1);
        assert_eq!(import.tracks.len(), 3);

        let prefs = PlaybackPreferences {
            volume: 0.65,
            shuffle: true,
            repeat_mode: RepeatMode::Context,
            backend_kind: AudioBackendKind::LocalRodio,
            last_track: Some(import.tracks[1].clone()),
            last_position_ms: 420,
            last_queue: import.tracks.clone(),
        };
        db.save_playback_preferences(&prefs).unwrap();
    }

    // Session 2 (simulated restart): instant SQLite load WITHOUT rescanning directories!
    {
        let db = Database::open(&db_path).unwrap();
        let loaded = db.load_local_tracks().unwrap();
        assert_eq!(loaded.len(), 3);

        let controller =
            PlaybackController::new(None, Some(db.clone()), PlaybackEventBus::default());
        let snap = controller.snapshot().await;
        assert_eq!(snap.status, PlaybackStatus::Paused);
        assert_eq!(snap.position.position_ms, 420);
        assert!((snap.volume - 0.65).abs() < 0.01);
        assert!(snap.shuffle);
        assert_eq!(snap.repeat, RepeatMode::Context);
        assert_eq!(
            snap.current_track.as_ref().map(|t| t.title.as_str()),
            Some("Beta")
        );

        // If a saved local file is deleted on disk, restart cleanly filters it out
        std::fs::remove_file(&song2).unwrap();
        let controller_after_delete =
            PlaybackController::new(None, Some(db), PlaybackEventBus::default());
        let snap2 = controller_after_delete.snapshot().await;
        assert_ne!(
            snap2.current_track.as_ref().map(|t| t.title.as_str()),
            Some("Beta")
        );
    }
}

// =============================================================================
// 9. Local Audio End-of-Track Auto-Advance, Resume & Seek Tests
// =============================================================================

#[tokio::test]
async fn local_audio_end_of_track_auto_advance_resume_and_seek() {
    use spotirust::audio::engine::AudioError;
    use spotirust::audio::local_rodio::{inspect_local_audio_file, write_synth_wav};

    let dir = tempfile::tempdir().unwrap();
    let wav1 = dir.path().join("Artist - Track One.wav");
    let wav2 = dir.path().join("Artist - Track Two.wav");
    write_synth_wav(&wav1, 0.45, &[[220.0, 261.63, 329.63]], 1.0).unwrap();
    write_synth_wav(&wav2, 1.20, &[[261.63, 329.63, 392.0]], 1.0).unwrap();

    let t1 = inspect_local_audio_file(&wav1, None).unwrap().track;
    let t2 = inspect_local_audio_file(&wav2, None).unwrap().track;

    let controller = PlaybackController::for_testing();
    match controller
        .play_track_in_context(t1.clone(), vec![t1.clone(), t2.clone()])
        .await
    {
        Ok(()) => {
            // Pause & resume on live audio sink
            controller.pause().await.unwrap();
            assert_eq!(controller.snapshot().await.status, PlaybackStatus::Paused);
            controller.resume().await.unwrap();
            assert_eq!(controller.snapshot().await.status, PlaybackStatus::Playing);

            // Wait for 450ms track 1 to finish naturally & auto-advance to track 2 via tick_background
            let mut advanced = false;
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(120)).await;
                controller.tick_background().await;
                if controller
                    .snapshot()
                    .await
                    .current_track
                    .as_ref()
                    .map(|t| t.id.as_str())
                    == Some(t2.id.as_str())
                {
                    advanced = true;
                    break;
                }
            }
            assert!(advanced, "Track 1 should naturally auto-advance to Track 2");

            // Seek inside Track 2
            controller.seek(400).await.unwrap();
            let pos = controller.get_position().await.current_ms();
            assert!(
                pos >= 350,
                "seek should advance playback clock, got {pos}ms"
            );
            controller.stop_playback().await;
        }
        Err(AudioError::BackendUnavailable(_)) => {
            // In headless environments without an audio daemon, verify honest refusal (zero fake Playing)
            let snap = controller.snapshot().await;
            assert!(!snap.status.is_playing());
            assert!(!snap.position.is_playing);
        }
        Err(other) => panic!("unexpected local audio error: {other}"),
    }
}

// =============================================================================
// 10. Karaoke Layout Cache & Adaptive Repaint Scheduler Integration Tests
// =============================================================================

#[test]
fn karaoke_renderer_cache_and_smart_repaint_scheduler_integration() {
    use spotirust::core::state::LyricsUiState;
    use spotirust::ui::lyrics::{KaraokeLayoutKey, KaraokeRenderer, next_karaoke_repaint_delay};

    let ttml = include_str!("../assets/sample_lyrics/spotirust_demo.ttml");
    let doc = Arc::new(parse_ttml(ttml, "Bundled Sample TTML").unwrap());
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
    out.textures_delta.clear();
    let theme = ThemePalette::obsidian_dark();
    let mut renderer = KaraokeRenderer::default();

    // Simulate 120 frames during Line 1 ("Pure sound waves...") at constant width
    for step in 0..120u64 {
        let ms = 600 + step * 25; // 600ms..3575ms (all within Line 1: 500..4200ms)
        renderer
            .animation
            .update("local:midnight_compiler", &doc, ms);
        let key = KaraokeLayoutKey {
            track_id: "local:midnight_compiler".into(),
            doc_revision: 1,
            compact: false,
            max_width_px: 620,
            active_line_idx: renderer.animation.active_line_idx,
            show_translations: true,
            theme_id: theme.id.clone(),
            dark: theme.dark,
        };
        renderer.layout_cache.ensure_layout(&ctx, key, &doc, &theme);
    }
    assert_eq!(
        renderer.layout_cache.rebuild_count(),
        1,
        "120 frames on the same active line must rebuild layout exactly ONCE"
    );
    assert_eq!(renderer.layout_cache.hit_count(), 119);

    // Repaint scheduler: active word vs inter-line gap (4200ms..4600ms)
    let state = LyricsUiState::Loaded {
        track_id: "local:midnight_compiler".into(),
        document: Arc::clone(&doc),
        revision: 1,
    };
    let active_word_delay = next_karaoke_repaint_delay(&state, 700, true, true, false).unwrap();
    assert!(active_word_delay <= Duration::from_millis(60));

    let inter_line_gap_delay =
        next_karaoke_repaint_delay(&state, 4_250, true, true, false).unwrap();
    assert_eq!(
        inter_line_gap_delay,
        Duration::from_millis(250),
        "inter-line gap must not spin at 30/60 FPS"
    );
}

// =============================================================================
// 11. Linux MPRIS2 D-Bus Media Controls Integration Test
// =============================================================================

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_mpris_dbus_live_roundtrip() {
    use spotirust::audio::engine::{PlaybackPosition, PlaybackSnapshot};
    use spotirust::system::media_controls::{
        SystemMediaBridge, SystemMediaCommand, SystemMediaStatus,
    };
    use std::collections::HashMap;
    use tokio::sync::mpsc;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};

    let dir = tempfile::tempdir().unwrap();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let bus_name = format!(
        "org.mpris.MediaPlayer2.spotirust.test_{}",
        std::process::id()
    );

    let bridge = SystemMediaBridge::start(cmd_tx, dir.path().to_path_buf(), Some(bus_name.clone()));

    // Wait briefly for D-Bus registration or graceful Unavailable detection
    for _ in 0..25 {
        if !matches!(
            bridge.status(),
            SystemMediaStatus::Unavailable { ref reason } if reason.contains("initializing")
        ) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        // Outside a desktop D-Bus session (e.g. bare CI container), verify honest non-panicking fallback
        assert!(matches!(
            bridge.status(),
            SystemMediaStatus::Unavailable { .. } | SystemMediaStatus::Active { .. }
        ));
        return;
    }

    // Live D-Bus session is present (e.g. `dbus-run-session`): verify full MPRIS2 round-trip!
    match bridge.status() {
        SystemMediaStatus::Active {
            bus_name: Some(ref acquired),
            ..
        } => {
            assert_eq!(acquired, &bus_name);
        }
        other => panic!("expected Active Linux MPRIS2 status under D-Bus session, got {other:?}"),
    }

    // Push a Playing snapshot with metadata
    let track = make_track(
        "mpris_1",
        "Midnight Compiler",
        "Spotirust Soundcheck",
        22_000,
    );
    let snap = PlaybackSnapshot {
        current_track: Some(track),
        status: PlaybackStatus::Playing,
        position: PlaybackPosition::new(5_000, 22_000, true),
        volume: 0.75,
        shuffle: true,
        repeat: RepeatMode::Context,
        backend: AudioBackendKind::LocalRodio,
        preferred_backend: AudioBackendKind::LocalRodio,
        active_device_name: None,
        active_device_type: None,
    };
    bridge.update_snapshot(&snap);
    tokio::time::sleep(Duration::from_millis(60)).await;

    // Connect an independent D-Bus client (just like GNOME Shell / playerctl)
    let client = zbus::Connection::session().await.unwrap();
    let root_proxy = zbus::Proxy::new(
        &client,
        bus_name.as_str(),
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2",
    )
    .await
    .unwrap();
    let identity: String = root_proxy.get_property("Identity").await.unwrap();
    assert_eq!(identity, "Spotirust");

    let player_proxy = zbus::Proxy::new(
        &client,
        bus_name.as_str(),
        "/org/mpris/MediaPlayer2",
        "org.mpris.MediaPlayer2.Player",
    )
    .await
    .unwrap();

    let status: String = player_proxy.get_property("PlaybackStatus").await.unwrap();
    assert_eq!(status, "Playing");
    let loop_st: String = player_proxy.get_property("LoopStatus").await.unwrap();
    assert_eq!(loop_st, "Playlist");
    let shuffle: bool = player_proxy.get_property("Shuffle").await.unwrap();
    assert!(shuffle);
    let vol: f64 = player_proxy.get_property("Volume").await.unwrap();
    assert!((vol - 0.75).abs() < 0.01);

    let meta: HashMap<String, OwnedValue> = player_proxy.get_property("Metadata").await.unwrap();
    let title: String = meta
        .get("xesam:title")
        .unwrap()
        .try_clone()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(title, "Midnight Compiler");
    let artists: Vec<String> = meta
        .get("xesam:artist")
        .unwrap()
        .try_clone()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(artists, vec!["Spotirust Soundcheck".to_string()]);
    let length_us: i64 = meta
        .get("mpris:length")
        .unwrap()
        .try_clone()
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(length_us, 22_000_000);
    let track_id_path: OwnedObjectPath = meta
        .get("mpris:trackid")
        .unwrap()
        .try_clone()
        .unwrap()
        .try_into()
        .unwrap();
    assert!(track_id_path.as_str().starts_with("/org/spotirust/track/"));

    // Invoke remote D-Bus transport methods and assert SystemMediaCommand arrives
    player_proxy.call_method("PlayPause", &()).await.unwrap();
    assert_eq!(cmd_rx.recv().await, Some(SystemMediaCommand::PlayPause));

    player_proxy.call_method("Next", &()).await.unwrap();
    assert_eq!(cmd_rx.recv().await, Some(SystemMediaCommand::Next));

    player_proxy.call_method("Previous", &()).await.unwrap();
    assert_eq!(cmd_rx.recv().await, Some(SystemMediaCommand::Previous));

    player_proxy
        .call_method("Seek", &(5_000_000i64,))
        .await
        .unwrap();
    assert_eq!(
        cmd_rx.recv().await,
        Some(SystemMediaCommand::SeekOffsetUs(5_000_000))
    );

    player_proxy.set_property("Volume", 0.42f64).await.unwrap();
    assert_eq!(
        cmd_rx.recv().await,
        Some(SystemMediaCommand::SetVolume(0.42))
    );
}

// =============================================================================
// 12. Spotify Connect Mock + Word-Level TTML + PlaybackClock + Drift + Cancellation E2E Test
// =============================================================================

#[tokio::test]
async fn spotify_connect_mock_ttml_karaoke_sync_end_to_end() {
    use spotirust::lyrics::provider::{
        CandidatePayload, FetchedLyrics, LyricsCandidate, LyricsError, LyricsProvider,
        LyricsSourceKind, TrackMetadata,
    };
    use spotirust::spotify::client::SpotifyClient;
    use spotirust::ui::lyrics::KaraokeAnimationState;
    use std::sync::Mutex;

    #[derive(Clone)]
    struct MockConnectState {
        connected: bool,
        force_error_status: Option<u16>,
        response_delay_ms: u64,
        track_id: String,
        track_title: String,
        duration_ms: u64,
        progress_ms: u64,
        is_playing: bool,
    }

    let mock_state = Arc::new(Mutex::new(MockConnectState {
        connected: true,
        force_error_status: None,
        response_delay_ms: 0,
        track_id: "sp_track_a".to_string(),
        track_title: "Midnight Compiler".to_string(),
        duration_ms: 22_000,
        progress_ms: 520,
        is_playing: true,
    }));

    let server_state = Arc::clone(&mock_state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_task = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let st_ref = Arc::clone(&server_state);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                let mut parts = first_line.split_whitespace();
                let method = parts.next().unwrap_or("GET");
                let target = parts.next().unwrap_or("/");

                let delay_ms = st_ref.lock().unwrap().response_delay_ms;
                if delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }

                let response = if method == "PUT" && target.starts_with("/me/player/pause") {
                    st_ref.lock().unwrap().is_playing = false;
                    "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                } else if method == "PUT" && target.starts_with("/me/player/play") {
                    st_ref.lock().unwrap().is_playing = true;
                    "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                } else if method == "PUT" && target.starts_with("/me/player/seek") {
                    if let Some(pos_str) = target
                        .split("position_ms=")
                        .nth(1)
                        .and_then(|s| s.split('&').next())
                        && let Ok(pos) = pos_str.parse::<u64>()
                    {
                        st_ref.lock().unwrap().progress_ms = pos;
                    }
                    "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                } else if method == "GET" && target.starts_with("/me/player/queue") {
                    let body = r#"{"currently_playing":null,"queue":[]}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if method == "GET" && target.starts_with("/me/player") {
                    let snap = st_ref.lock().unwrap().clone();
                    if let Some(code) = snap.force_error_status {
                        let body = r#"{"error":{"status":503,"message":"Service Unavailable"}}"#;
                        format!(
                            "HTTP/1.1 {code} Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    } else if !snap.connected {
                        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    } else {
                        let body = format!(
                            r#"{{
                                "is_playing": {},
                                "progress_ms": {},
                                "shuffle_state": false,
                                "repeat_state": "off",
                                "device": {{"id": "dev_1", "name": "Studio Speaker", "type": "Speaker", "is_active": true, "is_restricted": false, "volume_percent": 75}},
                                "item": {{
                                    "id": "{}",
                                    "uri": "spotify:track:{}",
                                    "name": "{}",
                                    "duration_ms": {},
                                    "explicit": false,
                                    "artists": [{{"name": "Spotirust Soundcheck"}}],
                                    "album": {{"id": "alb1", "uri": "spotify:album:alb1", "name": "Native Audio", "images": []}},
                                    "external_ids": {{"isrc": "USRC99990001"}}
                                }}
                            }}"#,
                            snap.is_playing,
                            snap.progress_ms,
                            snap.track_id,
                            snap.track_id,
                            snap.track_title,
                            snap.duration_ms
                        );
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    }
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };

                let _ = sock.write_all(response.as_bytes()).await;
            });
        }
    });

    // Wire real SessionManager + SpotifyClient -> PlaybackController against the local Connect mock
    let store = Arc::new(MemoryCredentialStore::new());
    let session = SessionManager::new(
        store,
        Some("test-client".to_string()),
        0,
        "/callback".to_string(),
    );
    session
        .set_tokens(StoredTokenBundle {
            access_token: "mock-access-token".to_string(),
            refresh_token: None,
            token_type: "Bearer".to_string(),
            scope: "user-read-playback-state user-modify-playback-state".to_string(),
            expires_at_unix: 9_999_999_999,
            client_id: "test-client".to_string(),
        })
        .await;

    let spotify = SpotifyClient::new(session).with_base_url(format!("http://{addr}"));
    let controller = PlaybackController::new(Some(spotify), None, PlaybackEventBus::default());

    // Word-level TTML fixture (Track A) with exact word boundaries from `spotirust_demo.ttml`:
    // Line 0: [500..4200] "Pure" (500..1050), "sound" (1050..1550), "waves" (1550..2100), "gliding" (2100..2650)
    // Line 2 (Chorus): [9000..13400] "No" (9000..9500), "heavy" (9500..10100), "browser" (10100..10800)
    let ttml_a = include_str!("../assets/sample_lyrics/spotirust_demo.ttml");
    let doc_a = parse_ttml(ttml_a, "Local File (spotirust_demo.ttml)").unwrap();
    assert_eq!(doc_a.format, LyricsFormat::Ttml);
    assert!(doc_a.has_word_timing());

    // 1. Initial Connect sync -> Playing Track A at 520ms -> Word 0 ("Pure") active
    controller.sync_connect_now().await.unwrap();
    let snap = controller.snapshot().await;
    assert_eq!(snap.status, PlaybackStatus::Playing);
    assert!(!snap.position.is_stale());
    let mut anim = KaraokeAnimationState::default();
    let anchor = snap.position.synced_at;
    anim.update("sp_track_a", &doc_a, snap.position.current_ms_at(anchor));
    assert_eq!(anim.active_line_idx, Some(0));
    assert_eq!(anim.active_word_idx, Some(0));
    assert_eq!(doc_a.lines[0].words[0].text, "Pure");

    // 2. Clock advances monotonically through word timestamps -> highlights "sound" then "waves"
    let t_word1 = snap
        .position
        .current_ms_at(anchor + Duration::from_millis(600)); // ~1120ms -> "sound" [1050..1550]
    anim.update("sp_track_a", &doc_a, t_word1);
    assert_eq!(anim.active_line_idx, Some(0));
    assert_eq!(anim.active_word_idx, Some(1));
    assert_eq!(doc_a.lines[0].words[1].text, "sound");

    let t_word2 = snap
        .position
        .current_ms_at(anchor + Duration::from_millis(1_100)); // ~1620ms -> "waves" [1550..2100]
    anim.update("sp_track_a", &doc_a, t_word2);
    assert_eq!(anim.active_word_idx, Some(2));
    assert_eq!(doc_a.lines[0].words[2].text, "waves");

    // 3. Pause -> position AND active word freeze completely
    mock_state.lock().unwrap().progress_ms = 1_650;
    controller.pause().await.unwrap();
    let paused_snap = controller.snapshot().await;
    assert_eq!(paused_snap.status, PlaybackStatus::Paused);
    let frozen_now = paused_snap.position.current_ms();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(paused_snap.position.current_ms(), frozen_now);
    let u_frozen = anim.update("sp_track_a", &doc_a, paused_snap.position.current_ms());
    assert!(!u_frozen.word_changed);
    assert_eq!(anim.active_word_idx, Some(2));

    // 4. Resume -> clock and highlight continue accurately from confirmed anchor
    mock_state.lock().unwrap().progress_ms = 1_650;
    controller.resume().await.unwrap();
    let resumed_snap = controller.snapshot().await;
    assert_eq!(resumed_snap.status, PlaybackStatus::Playing);
    let resumed_anchor = resumed_snap.position.synced_at;
    let after_resume_ms = resumed_snap
        .position
        .current_ms_at(resumed_anchor + Duration::from_millis(550)); // ~2200ms -> "gliding" [2100..2650]
    anim.update("sp_track_a", &doc_a, after_resume_ms);
    assert_eq!(anim.active_word_idx, Some(3));
    assert_eq!(doc_a.lines[0].words[3].text, "gliding");

    // 5. Seek forward to Chorus (10_200ms) -> immediate active line & word jump (zero smoothing)
    controller.seek(10_200).await.unwrap();
    let seek_snap = controller.snapshot().await;
    let seek_ms = seek_snap.position.current_ms();
    assert!(
        (10_200..=10_400).contains(&seek_ms),
        "expected seek near 10200ms, got {seek_ms}"
    );
    let u_seek = anim.update("sp_track_a", &doc_a, seek_ms);
    assert!(u_seek.seek_or_track_jump);
    assert_eq!(anim.active_line_idx, Some(2)); // Chorus
    assert_eq!(anim.active_word_idx, Some(2)); // "browser" [10100..10800]
    assert_eq!(doc_a.lines[2].words[2].text, "browser");

    // 6. Delayed polling response (120ms RTT) + moderate drift (+180ms) -> soft drift correction active!
    {
        let mut st = mock_state.lock().unwrap();
        st.response_delay_ms = 120;
        st.progress_ms = seek_snap.position.current_ms() + 180;
    }
    controller.sync_connect_now().await.unwrap();
    let drift_snap = controller.snapshot().await;
    assert!(!drift_snap.position.is_stale());
    assert!(
        drift_snap.position.drift_blend_start.is_some(),
        "moderate drift (+180ms) with 120ms RTT should trigger soft drift blending"
    );
    mock_state.lock().unwrap().response_delay_ms = 0;

    // 7. Track change -> resets PlaybackClock AND cancels slow in-flight lyrics task for Track A
    struct SlowTrackProvider {
        delay_track_a_ms: u64,
    }
    #[async_trait::async_trait]
    impl LyricsProvider for SlowTrackProvider {
        fn name(&self) -> &'static str {
            "SlowTestProvider"
        }
        fn source_kind(&self) -> LyricsSourceKind {
            LyricsSourceKind::Custom
        }
        async fn search(&self, track: &TrackMetadata) -> Result<Vec<LyricsCandidate>, LyricsError> {
            if track.track_id == "sp_track_a" {
                tokio::time::sleep(Duration::from_millis(self.delay_track_a_ms)).await;
            }
            let xml = format!(
                r#"<tt xmlns="http://www.w3.org/ns/ttml" itunes:timing="Word" xmlns:itunes="http://music.apple.com/lyric-ttml-internal"><body><div><p begin="00:00.200" end="00:04.000"><span begin="00:00.200" end="00:01.000">{}</span> <span begin="00:01.000" end="00:02.000">lyrics</span></p></div></body></tt>"#,
                track.title
            );
            Ok(vec![LyricsCandidate {
                id: format!("slow:{}", track.track_id),
                provider_name: self.name().to_string(),
                source_kind: LyricsSourceKind::Custom,
                title: track.title.clone(),
                artist: track.artist.clone(),
                album: Some(track.album.clone()),
                duration_ms: Some(track.duration_ms),
                isrc: track.isrc.clone(),
                has_word_sync: true,
                has_line_sync: true,
                instrumental: false,
                format_hint: LyricsFormat::Ttml,
                match_score: 0.0,
                match_reason: String::new(),
                payload: CandidatePayload::InlineRaw {
                    format: LyricsFormat::Ttml,
                    raw_content: xml,
                    source_label: "Test TTML".to_string(),
                    attribution: None,
                },
            }])
        }
        async fn fetch(&self, cand: &LyricsCandidate) -> Result<FetchedLyrics, LyricsError> {
            if let CandidatePayload::InlineRaw {
                raw_content,
                source_label,
                ..
            } = &cand.payload
            {
                let doc = parse_ttml(raw_content, source_label)
                    .map_err(|e| LyricsError::Parse(e.to_string()))?;
                return Ok(FetchedLyrics {
                    document: doc,
                    raw_content: Some(raw_content.clone()),
                });
            }
            Err(LyricsError::NotFound)
        }
    }

    let db = Database::open_in_memory().unwrap();
    let mut lyrics_svc = LyricsService::new(LyricsCache::new(db), vec![]);
    lyrics_svc.add_provider(Arc::new(SlowTrackProvider {
        delay_track_a_ms: 400,
    }));

    let track_a = controller.snapshot().await.current_track.unwrap();
    let (res_tx, mut res_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();

    // Start slow lyrics fetch for Track A
    let svc_a = lyrics_svc.clone();
    let tx_a = res_tx.clone();
    let slow_task = tokio::spawn(async move {
        use spotirust::core::config::LyricsSourcePreference;
        use spotirust::lyrics::provider::LyricsResolution;
        if let LyricsResolution::Found(doc) = svc_a
            .resolve_with_preference(&track_a, LyricsSourcePreference::Automatic, false, true)
            .await
        {
            let _ = tx_a.send((track_a.id, doc.lines[0].plain_text()));
        }
    });

    // Switch Connect to Track B immediately and abort Track A's slow task (exact coordinator pattern!)
    {
        let mut st = mock_state.lock().unwrap();
        st.track_id = "sp_track_b".to_string();
        st.track_title = "SecondSong".to_string();
        st.duration_ms = 18_000;
        st.progress_ms = 300;
        st.is_playing = true;
    }
    controller.sync_connect_now().await.unwrap();
    slow_task.abort();

    let track_b_snap = controller.snapshot().await;
    let track_b = track_b_snap.current_track.clone().unwrap();
    assert_eq!(track_b.id, "sp_track_b");
    assert!(
        track_b_snap.position.current_ms() < 1_000,
        "track switch must reset PlaybackClock cleanly to Track B's anchor"
    );

    let doc_b = match lyrics_svc
        .resolve_with_preference(
            &track_b,
            spotirust::core::config::LyricsSourcePreference::Automatic,
            false,
            true,
        )
        .await
    {
        spotirust::lyrics::provider::LyricsResolution::Found(doc) => doc,
        other => panic!("expected Track B TTML lyrics, got {other:?}"),
    };
    let _ = res_tx.send((track_b.id.clone(), doc_b.lines[0].plain_text()));

    // Wait past Track A's 400ms delay — verify ONLY Track B's lyrics were ever emitted!
    tokio::time::sleep(Duration::from_millis(450)).await;
    let mut delivered = Vec::new();
    while let Ok(item) = res_rx.try_recv() {
        delivered.push(item);
    }
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].0, "sp_track_b");
    assert_eq!(delivered[0].1, "SecondSong lyrics");

    // 8. Repeated poll failures / timeouts -> clock freezes & marks stale (never claims fresh data!)
    mock_state.lock().unwrap().force_error_status = Some(503);
    let _ = controller.sync_connect_now().await; // failure 1
    let _ = controller.sync_connect_now().await; // failure 2 -> stale!
    let stale_snap = controller.snapshot().await;
    assert!(stale_snap.position.is_stale());
    assert!(!stale_snap.position.is_playing);
    assert_eq!(stale_snap.status, PlaybackStatus::Reconnecting);
    let frozen_stale_ms = stale_snap.position.current_ms();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(stale_snap.position.current_ms(), frozen_stale_ms);

    // 9. Spotify Connect disconnect (`204 No Content`) -> Stopped + stale + zero movement
    {
        let mut st = mock_state.lock().unwrap();
        st.force_error_status = None;
        st.connected = false;
    }
    controller.sync_connect_now().await.unwrap();
    let disc_snap = controller.snapshot().await;
    assert_eq!(disc_snap.status, PlaybackStatus::Stopped);
    assert!(disc_snap.position.is_stale());
    assert!(!disc_snap.position.is_playing);

    server_task.abort();
}

// =============================================================================
// 13. Lyrics Source Preference, Honest Apple Music Blocking, Ambiguity & Cache Clear
// =============================================================================

#[tokio::test]
async fn lyrics_source_preference_apple_music_honesty_ambiguity_and_independent_cache_clear() {
    use spotirust::core::config::LyricsSourcePreference;
    use spotirust::lyrics::provider::{LrclibProvider, LyricsResolution};

    // Mock LRCLIB server returning:
    // - `/api/get` -> 404
    // - `/api/search` -> a remastered version (must be REJECTED!) + two distinct studio editions (AMBIGUOUS!)
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                let target = first_line.split_whitespace().nth(1).unwrap_or("/");

                let resp = if target.starts_with("/api/get") {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                } else if target.starts_with("/api/search") {
                    let body = r#"[
                        {
                            "id": 100,
                            "trackName": "Midnight Sun (2024 Remastered)",
                            "artistName": "Solstice",
                            "albumName": "First Light (Remastered)",
                            "duration": 200.0,
                            "instrumental": false,
                            "syncedLyrics": "[00:01.00]Remastered wrong timing"
                        },
                        {
                            "id": 101,
                            "trackName": "Midnight Sun",
                            "artistName": "Solstice",
                            "albumName": "First Light",
                            "duration": 200.4,
                            "instrumental": false,
                            "syncedLyrics": "[00:01.00]First Light studio line"
                        },
                        {
                            "id": 102,
                            "trackName": "Midnight Sun",
                            "artistName": "Solstice",
                            "albumName": "Second Dawn",
                            "duration": 199.5,
                            "instrumental": false,
                            "syncedLyrics": "[00:01.00]Second Dawn studio line"
                        }
                    ]"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    let db = Database::open_in_memory().unwrap();
    // Seed a cached Spotify track so we can verify independent lyrics cache clearing preserves it
    let track = Track {
        id: "sp_ambig_1".to_string(),
        uri: "spotify:track:sp_ambig_1".to_string(),
        title: "Midnight Sun".to_string(),
        artists: vec!["Solstice".to_string()],
        album_id: Some("alb_comp".to_string()),
        album_name: "Greatest Hits".to_string(),
        duration_ms: 200_000,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: true,
        local_path: None,
        accent_rgb: None,
    };
    db.upsert_tracks(std::slice::from_ref(&track)).unwrap();

    let lrclib = LrclibProvider::new().with_base_url(format!("http://{addr}"));
    let lrclib_only_chain = spotirust::core::config::LyricsProviderChainConfig {
        enable_unison: false,
        enable_amll_db: false,
        enable_musixmatch: false,
        enable_lrclib: true,
        ..Default::default()
    };
    let svc = LyricsService::new(LyricsCache::new(db.clone()), vec![])
        .with_lrclib(lrclib)
        .with_chain_config(lrclib_only_chain);

    // 1. AppleMusic preference MUST honestly report upstream block — NEVER return LRCLIB!
    match svc
        .resolve_with_preference(&track, LyricsSourcePreference::AppleMusic, true, true)
        .await
    {
        LyricsResolution::Unavailable(msg) => {
            assert!(
                msg.contains("FB25664234") && msg.contains("no official public lyrics"),
                "expected honest Apple Music blocked message, got: {msg}"
            );
        }
        other => panic!("AppleMusic mode must not fake lyrics; got {other:?}"),
    }

    // 2. Automatic / LRCLIB mode MUST reject the 2024 Remastered candidate and flag the 2 studio editions as Ambiguous!
    let chosen = match svc
        .resolve_with_preference(&track, LyricsSourcePreference::Automatic, true, true)
        .await
    {
        LyricsResolution::Ambiguous {
            candidates,
            provider_name,
        } => {
            assert_eq!(provider_name, "LRCLIB Public API");
            assert_eq!(candidates.len(), 2);
            assert!(
                candidates
                    .iter()
                    .all(|c| !c.title.to_ascii_lowercase().contains("remastered")),
                "remastered variant must be rejected before ambiguity selection"
            );
            candidates[1].clone() // User chooses "Second Dawn"
        }
        other => {
            panic!("expected Ambiguous resolution for close distinct studio albums, got {other:?}")
        }
    };

    // 3. User selects candidate -> cached in SQLite with honest "LRCLIB" source label
    let resolved = svc.resolve_candidate(&track, &chosen).await.unwrap();
    assert_eq!(resolved.source, "LRCLIB");
    assert_eq!(resolved.lines[0].plain_text(), "Second Dawn studio line");
    assert_eq!(db.cache_stats().unwrap().cached_lyrics, 1);

    // 4. Even after caching LRCLIB, AppleMusic mode STILL refuses to serve LRCLIB under an Apple Music selection!
    assert!(matches!(
        svc.resolve_with_preference(&track, LyricsSourcePreference::AppleMusic, true, false)
            .await,
        LyricsResolution::Unavailable(_)
    ));

    // 5. Independent Clear Lyrics Cache removes lyrics_cache ONLY while preserving cached_tracks!
    let removed = svc.clear_cache();
    assert_eq!(removed, 1);
    let stats = db.cache_stats().unwrap();
    assert_eq!(stats.cached_lyrics, 0);
    assert_eq!(
        stats.cached_tracks, 1,
        "clearing lyrics cache must NOT wipe cached Spotify tracks"
    );

    server.abort();
}

// =============================================================================
// 14. Unison Provider (ISRC + Search/ID + Syllable TTML + ETag + Attribution + 429/503)
// =============================================================================

#[tokio::test]
async fn unison_provider_word_ttml_etag_attribution_search_and_error_handling() {
    use spotirust::core::config::{LyricsProviderChainConfig, LyricsSourcePreference};
    use spotirust::lyrics::provider::{
        LyricsError, LyricsProvider, LyricsResolution, TrackMetadata,
    };
    use spotirust::lyrics::unison::UnisonProvider;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let hits = Arc::new(AtomicUsize::new(0));
    let hits_srv = Arc::clone(&hits);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let hits_c = Arc::clone(&hits_srv);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                let target = first_line.split_whitespace().nth(1).unwrap_or("/");
                hits_c.fetch_add(1, Ordering::SeqCst);

                let has_matching_etag = req.lines().any(|l| {
                    l.to_ascii_lowercase().starts_with("if-none-match:")
                        && l.contains("W/\"u-17538\"")
                });

                let resp = if target.contains("isrc=USRATE429000") {
                    let body =
                        r#"{"success":false,"error":"Too many requests","code":"RATE_LIMITED"}"#;
                    format!(
                        "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 45\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if target.contains("isrc=GBUNI1234567") {
                    if has_matching_etag {
                        "HTTP/1.1 304 Not Modified\r\nETag: W/\"u-17538\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    } else {
                        let body = r#"{
                            "success": true,
                            "data": {
                                "id": 17538,
                                "videoId": "pog50K4_Z64",
                                "song": "Never Gonna Stop",
                                "artist": "Rick Astley",
                                "album": "Are We There Yet?",
                                "isrc": "GBUNI1234567",
                                "duration": 198,
                                "format": "ttml",
                                "syncType": "richsync",
                                "confidence": "high",
                                "matchedBy": "isrc",
                                "submitter": {
                                    "keyId": "curator_key_abc123",
                                    "displayName": "SigmaDrumFlash"
                                },
                                "lyrics": "<tt xmlns=\"http://www.w3.org/ns/ttml\" xmlns:ttm=\"http://www.w3.org/ns/ttml#metadata\" xmlns:itunes=\"http://music.apple.com/lyric-ttml-internal\" itunes:timing=\"Word\"><body><div><p begin=\"0:01.841\" end=\"0:04.933\" itunes:key=\"L1\" ttm:agent=\"v1\"><span begin=\"0:01.841\" end=\"0:02.025\">Can</span> <span begin=\"0:02.025\" end=\"0:02.409\">any</span><span begin=\"0:02.409\" end=\"0:02.741\">body</span> <span begin=\"0:02.741\" end=\"0:03.068\">stop</span> <span begin=\"0:03.068\" end=\"0:03.485\">this</span> <span begin=\"0:03.485\" end=\"0:03.995\">fee</span><span begin=\"0:03.995\" end=\"0:04.933\">ling?</span></p></div></body></tt>"
                            }
                        }"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nETag: W/\"u-17538\"\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    }
                } else if target.contains("spotify=22charSpotifyIdPending") {
                    let body =
                        r#"{"success":false,"code":"LOOKUP_PENDING","error":"Lookup in progress"}"#;
                    format!(
                        "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 5\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if target.starts_with("/lyrics/search?") {
                    let body = r#"{
                        "success": true,
                        "data": [
                            {
                                "id": 9001,
                                "videoId": "vid_live",
                                "song": "Solar Flare (Live at Wembley)",
                                "artist": "Aurora",
                                "album": "Tour Live",
                                "duration": 210,
                                "format": "ttml",
                                "syncType": "richsync"
                            },
                            {
                                "id": 9002,
                                "videoId": "vid_studio",
                                "song": "Solar Flare",
                                "artist": "Aurora",
                                "album": "Helios",
                                "duration": 210,
                                "format": "ttml",
                                "syncType": "richsync"
                            }
                        ]
                    }"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if target == "/lyrics/9002" {
                    let body = r#"{
                        "success": true,
                        "data": {
                            "id": 9002,
                            "videoId": "vid_studio",
                            "song": "Solar Flare",
                            "artist": "Aurora",
                            "album": "Helios",
                            "duration": 210,
                            "format": "ttml",
                            "syncType": "richsync",
                            "confidence": "medium",
                            "matchedBy": "linked_video",
                            "submitter": {
                                "keyId": "key_9002",
                                "displayName": "HeliosCurator"
                            },
                            "lyrics": "<tt xmlns=\"http://www.w3.org/ns/ttml\" xmlns:itunes=\"http://music.apple.com/lyric-ttml-internal\" itunes:timing=\"Word\"><body><div><p begin=\"00:01.000\" end=\"00:03.000\"><span begin=\"00:01.000\" end=\"00:01.800\">Solar</span> <span begin=\"00:01.800\" end=\"00:03.000\">flare</span></p></div></body></tt>"
                        }
                    }"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    let body = r#"{"success":false,"error":"Lyrics not found","code":"NOT_FOUND"}"#;
                    format!(
                        "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                };

                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    let unison = UnisonProvider::new().with_base_url(format!("http://{addr}"));
    let db = Database::open_in_memory().unwrap();
    let svc = LyricsService::new(LyricsCache::new(db.clone()), vec![])
        .with_unison(unison.clone())
        .with_chain_config(LyricsProviderChainConfig {
            enable_unison: true,
            enable_amll_db: false,
            enable_musixmatch: false,
            enable_lrclib: false,
            ..Default::default()
        });

    // 1. Direct ISRC hit -> preserves syllable joining ("anybody", "feeling?"), word timing, & mandatory attribution
    let track_isrc = Track {
        id: "sp_unison_1".into(),
        uri: "spotify:track:4cOdK2wGLETKBW3PvgPWqT".into(),
        title: "Never Gonna Stop".into(),
        artists: vec!["Rick Astley".into()],
        album_id: None,
        album_name: "Are We There Yet?".into(),
        duration_ms: 198_000,
        isrc: Some("GBUNI1234567".into()),
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };

    let doc = match svc
        .resolve_with_preference(&track_isrc, LyricsSourcePreference::Unison, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected Unison word TTML hit, got {other:?}"),
    };
    assert_eq!(doc.format, LyricsFormat::Ttml);
    assert!(doc.has_word_timing());
    assert_eq!(doc.lines[0].words.len(), 7);
    assert_eq!(doc.lines[0].plain_text(), "Can anybody stop this feeling?");
    let attr = doc
        .attribution
        .as_ref()
        .expect("Unison attribution required");
    assert_eq!(attr.provider_label, "Lyrics from Unison");
    assert_eq!(
        attr.source_url.as_deref(),
        Some("https://unison.betterlyrics.org/song/pog50K4_Z64")
    );
    assert_eq!(attr.submitter_name.as_deref(), Some("SigmaDrumFlash"));
    assert_eq!(
        attr.submitter_url.as_deref(),
        Some("https://unison.betterlyrics.org/curator/curator_key_abc123")
    );

    // 2. Second lookup with cache enabled -> 0 HTTP requests AND preserves attribution from SQLite!
    let hits_before_cache = hits.load(Ordering::SeqCst);
    let cached_doc = match svc
        .resolve_with_preference(&track_isrc, LyricsSourcePreference::Unison, true, false)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected SQLite cache hit, got {other:?}"),
    };
    assert_eq!(hits.load(Ordering::SeqCst), hits_before_cache);
    assert!(cached_doc.has_word_timing());
    assert_eq!(
        cached_doc
            .attribution
            .as_ref()
            .and_then(|a| a.submitter_name.as_deref()),
        Some("SigmaDrumFlash")
    );

    // 3. Force refresh hits Unison with `If-None-Match` -> 304 Not Modified reuses cached ETag payload!
    let refreshed = match svc
        .resolve_with_preference(&track_isrc, LyricsSourcePreference::Unison, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected ETag 304 resolution, got {other:?}"),
    };
    assert_eq!(
        refreshed.lines[0].plain_text(),
        "Can anybody stop this feeling?"
    );

    // 4. 503 LOOKUP_PENDING on Spotify ID falls through cleanly to /lyrics/search -> rejects Live -> fetches /lyrics/9002
    let track_search = Track {
        id: "22charSpotifyIdPending".into(),
        uri: "spotify:track:22charSpotifyIdPending".into(),
        title: "Solar Flare".into(),
        artists: vec!["Aurora".into()],
        album_id: None,
        album_name: "Helios".into(),
        duration_ms: 210_000,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    let doc2 = match svc
        .resolve_with_preference(&track_search, LyricsSourcePreference::Unison, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected Unison /lyrics/search -> /lyrics/9002 hit, got {other:?}"),
    };
    assert!(doc2.has_word_timing());
    assert_eq!(doc2.lines[0].plain_text(), "Solar flare");
    assert!(
        doc2.attribution
            .as_ref()
            .and_then(|a| a.disclaimer.as_deref())
            .is_some_and(|d| d.contains("linked YouTube Music"))
    );

    // 5. 429 RATE_LIMITED maps to LyricsError::RateLimited with Retry-After
    let rate_meta = TrackMetadata {
        track_id: "sp_rate".into(),
        uri: "spotify:track:sp_rate".into(),
        title: "Rate Test".into(),
        artist: "Tester".into(),
        artists: vec!["Tester".into()],
        album: String::new(),
        duration_ms: 180_000,
        isrc: Some("USRATE429000".into()),
        explicit: false,
        local_path: None,
    };
    assert!(matches!(
        unison.search(&rate_meta).await,
        Err(LyricsError::RateLimited {
            retry_after_secs: 45
        })
    ));

    server.abort();
}

// =============================================================================
// 15. AMLL TTML DB Provider (CC0-1.0 Direct Spotify ID Lookup & Variant Safety)
// =============================================================================

#[tokio::test]
async fn amll_ttml_db_provider_spotify_id_lookup_attribution_and_variant_guard() {
    use spotirust::core::config::LyricsSourcePreference;
    use spotirust::lyrics::amll_db::AmllTtmlDbProvider;
    use spotirust::lyrics::provider::LyricsResolution;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let req_count = Arc::new(AtomicUsize::new(0));
    let req_srv = Arc::clone(&req_count);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let rc = Arc::clone(&req_srv);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                rc.fetch_add(1, Ordering::SeqCst);

                let resp = if target == "/spotify-lyrics/7ajpbW6tBpqUI9foCtwlLw.ttml" {
                    let body = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" xmlns:amll="http://www.example.com/ns/amll"><head><metadata><amll:meta key="musicName" value="Shukufuku"/><amll:meta key="artists" value="YOASOBI"/><amll:meta key="spotifyId" value="7ajpbW6tBpqUI9foCtwlLw"/><amll:meta key="ttmlAuthorGithubLogin" value="amll_curator"/></metadata></head><body><div><p begin="00:00.917" end="00:03.669" ttm:agent="v1" itunes:key="L1"><span begin="00:00.917" end="00:01.169">遥</span><span begin="00:01.169" end="00:01.385">か</span><span begin="00:01.385" end="00:01.743">遠</span><span begin="00:01.743" end="00:01.908">く</span><span ttm:role="x-translation" xml:lang="en">Far away</span></p></div></body></tt>"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if target == "/spotify-lyrics/1111111111111111111111.ttml" {
                    // Mismatched Live recording stored under studio ID -> MUST be rejected by strict matcher!
                    let body = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:amll="http://www.example.com/ns/amll"><head><metadata><amll:meta key="musicName" value="Starlight (Live at Dome)"/><amll:meta key="artists" value="Aurora"/></metadata></head><body><div><p begin="00:01.000" end="00:03.000"><span begin="00:01.000" end="00:02.000">Wrong</span> <span begin="00:02.000" end="00:03.000">version</span></p></div></body></tt>"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    let amll = AmllTtmlDbProvider::new().with_base_url(format!("http://{addr}"));
    let db = Database::open_in_memory().unwrap();
    let svc = LyricsService::new(LyricsCache::new(db), vec![]).with_amll_db(amll);

    // 1. Local non-Spotify track skips AMLL with ZERO HTTP requests!
    let local_track = make_track("local_file_1", "Local Song", "Artist", 180_000);
    assert!(matches!(
        svc.resolve_with_preference(&local_track, LyricsSourcePreference::AmllTtmlDb, true, true)
            .await,
        LyricsResolution::Unavailable(_)
    ));
    assert_eq!(req_count.load(Ordering::SeqCst), 0);

    // 2. Valid Spotify ID fetches CC0-1.0 word-level TTML + translation + @amll_curator credit!
    let sp_track = Track {
        id: "7ajpbW6tBpqUI9foCtwlLw".into(),
        uri: "spotify:track:7ajpbW6tBpqUI9foCtwlLw".into(),
        title: "Shukufuku".into(),
        artists: vec!["YOASOBI".into()],
        album_id: None,
        album_name: "THE BOOK 3".into(),
        duration_ms: 196_000,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    let doc = match svc
        .resolve_with_preference(&sp_track, LyricsSourcePreference::AmllTtmlDb, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected AMLL TTML hit, got {other:?}"),
    };
    assert_eq!(doc.format, LyricsFormat::Ttml);
    assert!(doc.has_word_timing());
    assert_eq!(doc.lines[0].words.len(), 4);
    assert_eq!(doc.lines[0].plain_text(), "遥か遠く");
    assert_eq!(doc.lines[0].translation.as_deref(), Some("Far away"));
    let attr = doc.attribution.as_ref().expect("AMLL attribution expected");
    assert!(attr.provider_label.contains("AMLL TTML DB"));
    assert_eq!(attr.submitter_name.as_deref(), Some("@amll_curator"));

    // 3. Mismatched Live variant under a studio track query is rejected!
    let studio_track = Track {
        id: "1111111111111111111111".into(),
        uri: "spotify:track:1111111111111111111111".into(),
        title: "Starlight".into(),
        artists: vec!["Aurora".into()],
        album_id: None,
        album_name: "Nightfall".into(),
        duration_ms: 210_000,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    assert!(matches!(
        svc.resolve_with_preference(
            &studio_track,
            LyricsSourcePreference::AmllTtmlDb,
            true,
            true
        )
        .await,
        LyricsResolution::Unavailable(_)
    ));

    server.abort();
}

// =============================================================================
// 16. Official Musixmatch Provider (RichSync Word -> Subtitle Line -> Plain 30% + Key Guard)
// =============================================================================

#[tokio::test]
async fn musixmatch_provider_richsync_word_subtitle_fallback_free_preview_and_key_guard() {
    use spotirust::core::config::LyricsSourcePreference;
    use spotirust::lyrics::musixmatch::MusixmatchProvider;
    use spotirust::lyrics::provider::{
        LyricsError, LyricsProvider, LyricsResolution, TrackMetadata,
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");

                let body = if target.contains("apikey=rate_limit_key") {
                    r#"{"message":{"header":{"status_code":402,"hint":"usage limit reached"},"body":[]}}"#
                        .to_string()
                } else if target.starts_with("/matcher.track.get") {
                    if target.contains("q_track=RichTrack") {
                        r#"{"message":{"header":{"status_code":200},"body":{"track":{"commontrack_id":5001,"track_name":"RichTrack","artist_name":"Nova","album_name":"Pulse","track_length":200,"track_isrc":"USMXM0005001","has_lyrics":1,"has_subtitles":1,"has_richsync":1,"instrumental":0}}}}"#
                            .to_string()
                    } else if target.contains("q_track=SubTrack") {
                        r#"{"message":{"header":{"status_code":200},"body":{"track":{"commontrack_id":5002,"track_name":"SubTrack","artist_name":"Nova","album_name":"Pulse","track_length":200,"has_lyrics":1,"has_subtitles":1,"has_richsync":1,"instrumental":0}}}}"#
                            .to_string()
                    } else {
                        r#"{"message":{"header":{"status_code":200},"body":{"track":{"commontrack_id":5003,"track_name":"FreeTrack","artist_name":"Nova","album_name":"Pulse","track_length":200,"has_lyrics":1,"has_subtitles":1,"has_richsync":1,"instrumental":0}}}}"#
                            .to_string()
                    }
                } else if target.starts_with("/track.richsync.get") {
                    if target.contains("commontrack_id=5001") {
                        r#"{"message":{"header":{"status_code":200},"body":{"richsync":{"richsync_body":"[{\"ts\":2.0,\"te\":4.5,\"x\":\"Hello world\",\"l\":[{\"c\":\"Hello\",\"o\":0.0},{\"c\":\" \",\"o\":0.9},{\"c\":\"world\",\"o\":1.0}]}]","lyrics_copyright":"Lyrics powered by www.musixmatch.com"}}}}"#
                            .to_string()
                    } else {
                        // Plan doesn't include RichSync -> 403 triggers clean downgrade!
                        r#"{"message":{"header":{"status_code":403},"body":[]}}"#.to_string()
                    }
                } else if target.starts_with("/track.subtitle.get") {
                    if target.contains("commontrack_id=5002") {
                        r#"{"message":{"header":{"status_code":200},"body":{"subtitle":{"subtitle_body":"[00:05.00]Line synced from Musixmatch\n[00:09.00]Second line","lyrics_copyright":"Lyrics powered by www.musixmatch.com"}}}}"#
                            .to_string()
                    } else {
                        r#"{"message":{"header":{"status_code":403},"body":[]}}"#.to_string()
                    }
                } else if target.starts_with("/track.lyrics.get") {
                    r#"{"message":{"header":{"status_code":200},"body":{"lyrics":{"lyrics_body":"First plain line\nSecond plain line\n...\n\n******* This Lyrics is NOT for Commercial use *******\n(1409623)","lyrics_copyright":"Lyrics powered by www.musixmatch.com","instrumental":0}}}}"#
                        .to_string()
                } else {
                    r#"{"message":{"header":{"status_code":404},"body":[]}}"#.to_string()
                };

                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    // 1. Without API key: instantly unavailable, zero network calls
    let no_key = MusixmatchProvider::new(None).with_base_url(format!("http://{addr}"));
    assert!(!no_key.is_available());
    let dummy_meta = TrackMetadata {
        track_id: "t0".into(),
        uri: "spotify:track:t0".into(),
        title: "RichTrack".into(),
        artist: "Nova".into(),
        artists: vec!["Nova".into()],
        album: "Pulse".into(),
        duration_ms: 200_000,
        isrc: None,
        explicit: false,
        local_path: None,
    };
    assert!(matches!(
        no_key.search(&dummy_meta).await,
        Err(LyricsError::AuthRequired(_))
    ));

    // 2. With official API key: RichSync word-level JSON parses & round-trips through SQLite cache!
    let mxm = MusixmatchProvider::new(Some("valid_official_key".into()))
        .with_base_url(format!("http://{addr}"));
    let db = Database::open_in_memory().unwrap();
    let svc = LyricsService::new(LyricsCache::new(db), vec![]).with_musixmatch(mxm);

    let rich_track = Track {
        id: "sp_mxm_1".into(),
        uri: "spotify:track:sp_mxm_1".into(),
        title: "RichTrack".into(),
        artists: vec!["Nova".into()],
        album_id: None,
        album_name: "Pulse".into(),
        duration_ms: 200_000,
        isrc: Some("USMXM0005001".into()),
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    let doc_word = match svc
        .resolve_with_preference(&rich_track, LyricsSourcePreference::Musixmatch, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected Musixmatch RichSync word hit, got {other:?}"),
    };
    assert_eq!(doc_word.format, LyricsFormat::MusixmatchRichSync);
    assert!(doc_word.has_word_timing());
    assert_eq!(doc_word.lines[0].words.len(), 2);
    assert_eq!(
        (
            doc_word.lines[0].words[0].start_ms,
            doc_word.lines[0].words[0].end_ms
        ),
        (2_000, 3_000)
    );
    assert_eq!(doc_word.lines[0].plain_text(), "Hello world");
    assert!(
        doc_word
            .attribution
            .as_ref()
            .and_then(|a| a.copyright.as_deref())
            .is_some_and(|c| c.contains("musixmatch.com"))
    );

    // SQLite cache round-trip MUST preserve MusixmatchRichSync word timing losslessly!
    let cached_word = match svc
        .resolve_with_preference(&rich_track, LyricsSourcePreference::Musixmatch, true, false)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected cached Musixmatch RichSync, got {other:?}"),
    };
    assert_eq!(cached_word.format, LyricsFormat::MusixmatchRichSync);
    assert!(cached_word.has_word_timing());
    assert_eq!(cached_word.lines[0].words[1].start_ms, 3_000);

    // 3. 403 on RichSync gracefully downgrades to Subtitle LRC (Line-level)
    let sub_track = Track {
        id: "sp_mxm_2".into(),
        uri: "spotify:track:sp_mxm_2".into(),
        title: "SubTrack".into(),
        artists: vec!["Nova".into()],
        album_id: None,
        album_name: "Pulse".into(),
        duration_ms: 200_000,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    let doc_line = match svc
        .resolve_with_preference(&sub_track, LyricsSourcePreference::Musixmatch, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected Musixmatch Subtitle fallback, got {other:?}"),
    };
    assert_eq!(doc_line.format, LyricsFormat::StandardLrc);
    assert!(!doc_line.has_word_timing());
    assert!(doc_line.format.is_time_synced());

    // 4. 403 on RichSync & Subtitle gracefully downgrades to Plain 30% preview (NO fake timestamps!)
    let free_track = Track {
        id: "sp_mxm_3".into(),
        uri: "spotify:track:sp_mxm_3".into(),
        title: "FreeTrack".into(),
        artists: vec!["Nova".into()],
        album_id: None,
        album_name: "Pulse".into(),
        duration_ms: 200_000,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    let doc_plain = match svc
        .resolve_with_preference(&free_track, LyricsSourcePreference::Musixmatch, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected Musixmatch plain 30% preview, got {other:?}"),
    };
    assert_eq!(doc_plain.format, LyricsFormat::PlainText);
    assert!(!doc_plain.format.is_time_synced());
    assert!(doc_plain.source.contains("30% Preview"));
    assert!(
        doc_plain
            .lines
            .iter()
            .all(|l| !l.plain_text().contains("NOT for Commercial use")),
        "free-tier disclaimer banner must be stripped from lyric lines and shown in attribution"
    );
    assert!(
        doc_plain
            .attribution
            .as_ref()
            .and_then(|a| a.disclaimer.as_deref())
            .is_some_and(|d| d.contains("30%"))
    );

    // 5. Status 402 maps to RateLimited
    let limited = MusixmatchProvider::new(Some("rate_limit_key".into()))
        .with_base_url(format!("http://{addr}"));
    assert!(matches!(
        limited.search(&dummy_meta).await,
        Err(LyricsError::RateLimited { .. })
    ));

    server.abort();
}

// =============================================================================
// 17. LRCLIB v2 Search + Lyricsfile v1.0 Word-Sync + 503 Retry-After + Cache
// =============================================================================

#[tokio::test]
async fn lrclib_provider_v2_search_lyricsfile_word_sync_cache_and_503_retry_after() {
    use spotirust::core::config::LyricsSourcePreference;
    use spotirust::lyrics::lrclib::LrclibProvider;
    use spotirust::lyrics::provider::{
        LyricsError, LyricsProvider, LyricsResolution, TrackMetadata,
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");

                let resp = if target.contains("OverloadedTrack") {
                    "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 12\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                } else if target.starts_with("/api/get?") {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                } else if target.starts_with("/api/v2/search?") {
                    // Returns a remix (must be rejected!), a line-only studio copy, and a word-synced studio copy!
                    let body = r#"[
                        {"id":700,"trackName":"Dawn Chorus (Club Remix)","artistName":"Vesper","albumName":"Remixes","duration":205.0,"instrumental":false,"hasWordSync":true,"hasSyncedLyrics":true,"hasPlainLyrics":true,"hasLyricsfile":true},
                        {"id":701,"trackName":"Dawn Chorus","artistName":"Vesper","albumName":"Daybreak","duration":205.0,"instrumental":false,"hasWordSync":false,"hasSyncedLyrics":true,"hasPlainLyrics":true,"hasLyricsfile":false},
                        {"id":702,"trackName":"Dawn Chorus","artistName":"Vesper","albumName":"Daybreak","duration":205.0,"instrumental":false,"hasWordSync":true,"hasSyncedLyrics":true,"hasPlainLyrics":true,"hasLyricsfile":true}
                    ]"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if target == "/api/get/702" {
                    let body = r#"{
                        "id": 702,
                        "trackName": "Dawn Chorus",
                        "artistName": "Vesper",
                        "albumName": "Daybreak",
                        "duration": 205.0,
                        "instrumental": false,
                        "hasWordSync": true,
                        "syncedLyrics": "[00:04.20]Stay until morning",
                        "lyricsfile": "version: \"1.0\"\nmetadata:\n  title: Dawn Chorus\n  artist: Vesper\nlines:\n- text: \"Stay until morning\"\n  start_ms: 4200\n  end_ms: 6800\n  words:\n  - text: \"Stay \"\n    start_ms: 4200\n    end_ms: 4900\n  - text: \"until \"\n    start_ms: 4900\n    end_ms: 5600\n  - text: \"morning\"\n    start_ms: 5600\n    end_ms: 6800\n"
                    }"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    let lrclib = LrclibProvider::new().with_base_url(format!("http://{addr}"));
    let db = Database::open_in_memory().unwrap();
    let svc = LyricsService::new(LyricsCache::new(db), vec![]).with_lrclib(lrclib.clone());

    let track = Track {
        id: "sp_lrc_v2".into(),
        uri: "spotify:track:sp_lrc_v2".into(),
        title: "Dawn Chorus".into(),
        artists: vec!["Vesper".into()],
        album_id: None,
        album_name: "Daybreak".into(),
        duration_ms: 205_000,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };

    let doc = match svc
        .resolve_with_preference(&track, LyricsSourcePreference::Lrclib, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected LRCLIB v2 -> /api/get/702 Lyricsfile word hit, got {other:?}"),
    };
    assert_eq!(doc.format, LyricsFormat::LyricsfileWord);
    assert!(doc.has_word_timing());
    assert_eq!(doc.lines[0].words.len(), 3);
    assert_eq!(doc.lines[0].plain_text(), "Stay until morning");
    assert_eq!(
        (doc.lines[0].words[0].start_ms, doc.lines[0].words[0].end_ms),
        (4_200, 4_900)
    );

    // SQLite cache round-trip MUST preserve LyricsfileWord word-level timings!
    let cached = match svc
        .resolve_with_preference(&track, LyricsSourcePreference::Lrclib, true, false)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected cached LyricsfileWord, got {other:?}"),
    };
    assert_eq!(cached.format, LyricsFormat::LyricsfileWord);
    assert!(cached.has_word_timing());
    assert_eq!(cached.lines[0].words[2].start_ms, 5_600);

    // 503 Service Unavailable respects Retry-After
    let overloaded_meta = TrackMetadata {
        track_id: "ov".into(),
        uri: "spotify:track:ov".into(),
        title: "OverloadedTrack".into(),
        artist: "Vesper".into(),
        artists: vec!["Vesper".into()],
        album: String::new(),
        duration_ms: 200_000,
        isrc: None,
        explicit: false,
        local_path: None,
    };
    assert!(matches!(
        lrclib.search(&overloaded_meta).await,
        Err(LyricsError::RateLimited {
            retry_after_secs: 12
        })
    ));

    server.abort();
}

// =============================================================================
// 18. Ordered Multi-Provider Fallback Chain: Timeout/404/429/503, Word Priority & Anti-Spam
// =============================================================================

#[tokio::test]
async fn lyrics_service_ordered_fallback_timeout_404_429_503_word_priority_and_zero_spam() {
    use spotirust::core::config::{
        LyricsProviderChainConfig, LyricsSourcePreference, OnlineLyricsProviderId,
    };
    use spotirust::lyrics::amll_db::AmllTtmlDbProvider;
    use spotirust::lyrics::lrclib::LrclibProvider;
    use spotirust::lyrics::musixmatch::MusixmatchProvider;
    use spotirust::lyrics::provider::LyricsResolution;
    use spotirust::lyrics::unison::UnisonProvider;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Helper to spawn a simple programmable HTTP server with hit counter
    async fn spawn_mock(
        handler: impl Fn(&str) -> (u64, String) + Send + Sync + 'static,
    ) -> (
        std::net::SocketAddr,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_srv = Arc::clone(&hits);
        let handler = Arc::new(handler);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let hc = Arc::clone(&hits_srv);
                let h = Arc::clone(&handler);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let Ok(n) = sock.read(&mut buf).await else {
                        return;
                    };
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let target = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("/");
                    hc.fetch_add(1, Ordering::SeqCst);
                    let (delay_ms, resp) = h(target);
                    if delay_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        (addr, hits, handle)
    }

    // --- Scenario A: BiniLyrics returns 404 -> Unison returns Word TTML -> ZERO calls to AMLL, Musixmatch, LRCLIB ---
    let (b_addr, b_hits, b_srv) = spawn_mock(|_| {
        (
            0,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
        )
    })
    .await;
    let (u_addr, u_hits, u_srv) = spawn_mock(|_| {
        let body = r#"{"success":true,"data":{"id":1,"videoId":"v1","song":"FastHit","artist":"Echo","duration":180,"format":"ttml","syncType":"richsync","lyrics":"<tt xmlns=\"http://www.w3.org/ns/ttml\" itunes:timing=\"Word\" xmlns:itunes=\"http://music.apple.com/lyric-ttml-internal\"><body><div><p begin=\"00:01.000\" end=\"00:03.000\"><span begin=\"00:01.000\" end=\"00:02.000\">Word</span> <span begin=\"00:02.000\" end=\"00:03.000\">hit</span></p></div></body></tt>"}}"#;
        (
            0,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        )
    })
    .await;
    let (a_addr, a_hits, a_srv) = spawn_mock(|_| {
        (
            0,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
        )
    })
    .await;
    let (m_addr, m_hits, m_srv) = spawn_mock(|_| {
        (
            0,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 43\r\nConnection: close\r\n\r\n{\"message\":{\"header\":{\"status_code\":404}}}"
                .to_string(),
        )
    })
    .await;
    let (l_addr, l_hits, l_srv) = spawn_mock(|_| {
        (
            0,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
        )
    })
    .await;

    let db = Database::open_in_memory().unwrap();
    let svc_fast = LyricsService::new(LyricsCache::new(db), vec![])
        .with_binilyrics(
            spotirust::lyrics::binilyrics::BiniLyricsProvider::new()
                .with_base_url(format!("http://{b_addr}")),
        )
        .with_unison(UnisonProvider::new().with_base_url(format!("http://{u_addr}")))
        .with_amll_db(AmllTtmlDbProvider::new().with_base_url(format!("http://{a_addr}")))
        .with_musixmatch(
            MusixmatchProvider::new(Some("key".into())).with_base_url(format!("http://{m_addr}")),
        )
        .with_lrclib(LrclibProvider::new().with_base_url(format!("http://{l_addr}")));

    let track_a = Track {
        id: "7ajpbW6tBpqUI9foCtwlLw".into(),
        uri: "spotify:track:7ajpbW6tBpqUI9foCtwlLw".into(),
        title: "FastHit".into(),
        artists: vec!["Echo".into()],
        album_id: None,
        album_name: String::new(),
        duration_ms: 180_000,
        isrc: Some("USFAST000001".into()),
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    let doc_a = match svc_fast
        .resolve_with_preference(&track_a, LyricsSourcePreference::Automatic, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected fast Unison hit, got {other:?}"),
    };
    assert!(doc_a.has_word_timing());
    assert!(b_hits.load(Ordering::SeqCst) >= 1);
    assert_eq!(u_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        a_hits.load(Ordering::SeqCst),
        0,
        "AMLL must NOT be called after word hit"
    );
    assert_eq!(
        m_hits.load(Ordering::SeqCst),
        0,
        "Musixmatch must NOT be called after word hit"
    );
    assert_eq!(
        l_hits.load(Ordering::SeqCst),
        0,
        "LRCLIB must NOT be called after word hit"
    );

    // Second play hits SQLite cache -> ZERO network requests to ANY provider!
    let _ = svc_fast
        .resolve_with_preference(&track_a, LyricsSourcePreference::Automatic, true, false)
        .await;
    assert_eq!(u_hits.load(Ordering::SeqCst), 1);

    b_srv.abort();
    u_srv.abort();
    a_srv.abort();
    m_srv.abort();
    l_srv.abort();

    // --- Scenario B: BiniLyrics 404 -> Unison Timeout (500ms vs 150ms) -> AMLL 404 -> Musixmatch 402 -> LRCLIB Word Lyricsfile ---
    let (b2_addr, _, b2_srv) = spawn_mock(|_| {
        (
            0,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
        )
    })
    .await;
    let (u2_addr, _, u2_srv) = spawn_mock(|_| {
        (
            500,
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_string(),
        )
    })
    .await;
    let (a2_addr, _, a2_srv) = spawn_mock(|_| {
        (
            0,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
        )
    })
    .await;
    let (m2_addr, _, m2_srv) = spawn_mock(|_| {
        let body = r#"{"message":{"header":{"status_code":402},"body":[]}}"#;
        (
            0,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        )
    })
    .await;
    let (l2_addr, _, l2_srv) = spawn_mock(|target| {
        if target.starts_with("/api/get?") {
            let body = r#"{"id":88,"trackName":"Resilient","artistName":"Echo","duration":180.0,"instrumental":false,"hasWordSync":true,"lyricsfile":"version: \"1.0\"\nlines:\n- text: \"Fallback works\"\n  start_ms: 1000\n  end_ms: 3000\n  words:\n  - text: \"Fallback \"\n    start_ms: 1000\n    end_ms: 2000\n  - text: \"works\"\n    start_ms: 2000\n    end_ms: 3000\n"}"#;
            (
                0,
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                ),
            )
        } else {
            (
                0,
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string(),
            )
        }
    })
    .await;

    let db2 = Database::open_in_memory().unwrap();
    let svc_fallback = LyricsService::new(LyricsCache::new(db2), vec![])
        .with_binilyrics(
            spotirust::lyrics::binilyrics::BiniLyricsProvider::new()
                .with_base_url(format!("http://{b2_addr}")),
        )
        .with_unison(
            UnisonProvider::with_timeout(Duration::from_millis(150))
                .with_base_url(format!("http://{u2_addr}")),
        )
        .with_amll_db(AmllTtmlDbProvider::new().with_base_url(format!("http://{a2_addr}")))
        .with_musixmatch(
            MusixmatchProvider::new(Some("key".into())).with_base_url(format!("http://{m2_addr}")),
        )
        .with_lrclib(LrclibProvider::new().with_base_url(format!("http://{l2_addr}")));

    let track_b = Track {
        id: "2222222222222222222222".into(),
        uri: "spotify:track:2222222222222222222222".into(),
        title: "Resilient".into(),
        artists: vec!["Echo".into()],
        album_id: None,
        album_name: String::new(),
        duration_ms: 180_000,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    let doc_b = match svc_fallback
        .resolve_with_preference(&track_b, LyricsSourcePreference::Automatic, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected fallback to LRCLIB after timeout+404+402, got {other:?}"),
    };
    assert_eq!(doc_b.format, LyricsFormat::LyricsfileWord);
    assert_eq!(doc_b.lines[0].plain_text(), "Fallback works");

    b2_srv.abort();
    u2_srv.abort();
    a2_srv.abort();
    m2_srv.abort();
    l2_srv.abort();

    // --- Scenario C: Word > Line > Plain quality priority ---
    // Unison returns Plain text -> LRCLIB returns Word Lyricsfile -> Word MUST win over Plain!
    let (u3_addr, _, u3_srv) = spawn_mock(|_| {
        let body = r#"{"success":true,"data":{"id":3,"videoId":"v3","song":"UpgradeMe","artist":"Echo","duration":180,"format":"plain","syncType":"plain","lyrics":"Plain fallback line"}}"#;
        (
            0,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            ),
        )
    })
    .await;
    let (l3_addr, _, l3_srv) = spawn_mock(|target| {
        if target.starts_with("/api/get?") {
            let body = r#"{"id":99,"trackName":"UpgradeMe","artistName":"Echo","duration":180.0,"instrumental":false,"hasWordSync":true,"lyricsfile":"version: \"1.0\"\nlines:\n- text: \"Word upgraded\"\n  start_ms: 500\n  end_ms: 2500\n  words:\n  - text: \"Word \"\n    start_ms: 500\n    end_ms: 1500\n  - text: \"upgraded\"\n    start_ms: 1500\n    end_ms: 2500\n"}"#;
            (
                0,
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                ),
            )
        } else {
            (
                0,
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_string(),
            )
        }
    })
    .await;

    let db3 = Database::open_in_memory().unwrap();
    let svc_upgrade = LyricsService::new(LyricsCache::new(db3), vec![])
        .with_unison(UnisonProvider::new().with_base_url(format!("http://{u3_addr}")))
        .with_lrclib(LrclibProvider::new().with_base_url(format!("http://{l3_addr}")))
        .with_chain_config(LyricsProviderChainConfig {
            order: vec![
                OnlineLyricsProviderId::Unison,
                OnlineLyricsProviderId::Lrclib,
            ],
            enable_binilyrics: false,
            enable_unison: true,
            enable_amll_db: false,
            enable_musixmatch: false,
            enable_lrclib: true,
        });

    let track_c = Track {
        id: "sp_upgrade".into(),
        uri: "spotify:track:sp_upgrade".into(),
        title: "UpgradeMe".into(),
        artists: vec!["Echo".into()],
        album_id: None,
        album_name: String::new(),
        duration_ms: 180_000,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    let doc_c = match svc_upgrade
        .resolve_with_preference(&track_c, LyricsSourcePreference::Automatic, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected word-synced LRCLIB to beat plain Unison, got {other:?}"),
    };
    assert_eq!(doc_c.format, LyricsFormat::LyricsfileWord);
    assert!(doc_c.has_word_timing());
    assert_eq!(doc_c.lines[0].plain_text(), "Word upgraded");

    u3_srv.abort();
    l3_srv.abort();
}

#[tokio::test]
async fn binilyrics_provider_isrc_search_ttml_word_sync_ssrf_guard_and_rate_limit() {
    use spotirust::core::config::LyricsSourcePreference;
    use spotirust::lyrics::binilyrics::BiniLyricsProvider;
    use spotirust::lyrics::cache::LyricsCache;
    use spotirust::lyrics::model::LyricsFormat;
    use spotirust::lyrics::provider::{
        LyricsError, LyricsProvider, LyricsResolution, LyricsService, TrackMetadata,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let base_srv = base.clone();
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_srv = Arc::clone(&hits);

    let srv = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let base_inner = base_srv.clone();
            let hc = Arc::clone(&hits_srv);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                hc.fetch_add(1, Ordering::SeqCst);

                let (status, content_type, body) = if target.contains("isrc=GBAAW9400140") {
                    let json = format!(
                        r#"{{"results":[{{"id":"GBAAW9400140","track_name":"Live Forever","artist_name":"Oasis","album_name":"Definitely Maybe","duration":276,"isrc":"GBAAW9400140","lyricsUrl":"{base_inner}/s/GBAAW9400140.ttml","timing_type":"word"}},{{"id":"EVIL","track_name":"Live Forever","artist_name":"Oasis","album_name":"Definitely Maybe","duration":276,"isrc":"GBAAW9400140","lyricsUrl":"https://evil.example/ssrf.ttml","timing_type":"word"}}],"source":"HIT-LRC-RED"}}"#
                    );
                    ("200 OK", "application/json", json)
                } else if target == "/s/GBAAW9400140.ttml" {
                    let ttml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:lrc="http://lrc.red/lyric-ttml-internal" lrc:timing="Word" xml:lang="en"><body><div><p begin="00:01.000" end="00:03.500"><span begin="00:01.000" end="00:01.700">May</span><span begin="00:01.700" end="00:02.200">be</span> <span begin="00:02.200" end="00:02.700">I</span> <span begin="00:02.700" end="00:03.500">fly</span></p></div></body></tt>"#;
                    ("200 OK", "application/ttml+xml", ttml.to_string())
                } else if target.contains("track=RateLimited") {
                    (
                        "429 Too Many Requests\r\nRetry-After: 42",
                        "application/json",
                        r#"{"error":"rate limited"}"#.to_string(),
                    )
                } else {
                    ("404 Not Found", "application/json", String::new())
                };

                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    let provider = BiniLyricsProvider::new().with_base_url(&base);

    // 1. Search filters out SSRF evil.example URL and returns only the allowlisted TTML candidate
    let meta = TrackMetadata {
        track_id: "bini_track_1".into(),
        uri: "spotify:track:bini_track_1".into(),
        title: "Live Forever".into(),
        artist: "Oasis".into(),
        artists: vec!["Oasis".into()],
        album: "Definitely Maybe".into(),
        duration_ms: 276_000,
        isrc: Some("GBAAW9400140".into()),
        explicit: false,
        local_path: None,
    };
    let candidates = provider.search(&meta).await.expect("search should succeed");
    assert_eq!(
        candidates.len(),
        1,
        "evil.example SSRF candidate must be rejected"
    );
    assert!(candidates[0].has_word_sync);

    // 2. Full LyricsService resolution via LyricsSourcePreference::BiniLyrics parses word TTML & caches
    let db = Database::open_in_memory().unwrap();
    let svc = LyricsService::new(LyricsCache::new(db), vec![]).with_binilyrics(provider.clone());
    let track = Track {
        id: "bini_track_1".into(),
        uri: "spotify:track:bini_track_1".into(),
        title: "Live Forever".into(),
        artists: vec!["Oasis".into()],
        album_id: None,
        album_name: "Definitely Maybe".into(),
        duration_ms: 276_000,
        isrc: Some("GBAAW9400140".into()),
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    let doc = match svc
        .resolve_with_preference(&track, LyricsSourcePreference::BiniLyrics, true, true)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected BiniLyrics word TTML hit, got {other:?}"),
    };
    assert_eq!(doc.format, LyricsFormat::Ttml);
    assert!(doc.has_word_timing());
    assert_eq!(doc.source, "BiniLyrics");
    assert_eq!(doc.lines[0].words.len(), 4);
    assert_eq!(doc.lines[0].plain_text(), "Maybe I fly");

    let hits_after_first = hits.load(Ordering::SeqCst);
    let cached_doc = match svc
        .resolve_with_preference(&track, LyricsSourcePreference::BiniLyrics, true, false)
        .await
    {
        LyricsResolution::Found(d) => d,
        other => panic!("expected cached BiniLyrics doc, got {other:?}"),
    };
    assert_eq!(cached_doc.lines[0].plain_text(), "Maybe I fly");
    assert_eq!(
        hits.load(Ordering::SeqCst),
        hits_after_first,
        "cached BiniLyrics lookup must issue zero network requests"
    );

    // 3. 429 Too Many Requests surfaces LyricsError::RateLimited with Retry-After
    let rl_meta = TrackMetadata {
        track_id: "rl".into(),
        uri: "spotify:track:rl".into(),
        title: "RateLimited".into(),
        artist: "Oasis".into(),
        artists: vec!["Oasis".into()],
        album: String::new(),
        duration_ms: 180_000,
        isrc: None,
        explicit: false,
        local_path: None,
    };
    match provider.search(&rl_meta).await {
        Err(LyricsError::RateLimited { retry_after_secs }) => assert_eq!(retry_after_secs, 42),
        other => panic!("expected RateLimited(42), got {other:?}"),
    }

    srv.abort();
}

// =============================================================================
// 19. Live End-to-End Real Lyrics Providers Smoke Test (`--ignored` for offline CI safety)
// =============================================================================

#[tokio::test]
#[ignore = "hits live public Unison, AMLL TTML DB, and LRCLIB APIs over the network"]
async fn live_real_lyrics_providers_smoke_test() {
    use spotirust::core::config::LyricsSourcePreference;
    use spotirust::lyrics::provider::LyricsResolution;

    let db = Database::open_in_memory().unwrap();
    let svc = LyricsService::new(LyricsCache::new(db), vec![]);

    // 1. Live Unison word-level TTML (Rick Astley - Never Gonna Stop)
    let unison_track = Track {
        id: "live_unison_1".into(),
        uri: "spotify:track:live_unison_1".into(),
        title: "Never Gonna Stop".into(),
        artists: vec!["Rick Astley".into()],
        album_id: None,
        album_name: "Are We There Yet?".into(),
        duration_ms: 198_000,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    match svc
        .resolve_with_preference(&unison_track, LyricsSourcePreference::Unison, true, true)
        .await
    {
        LyricsResolution::Found(doc) => {
            println!(
                "[LIVE UNISON] source={} format={:?} word_synced={} lines={} first_line={:?} attr={:?}",
                doc.source,
                doc.format,
                doc.has_word_timing(),
                doc.lines.len(),
                doc.lines.first().map(|l| l.plain_text()),
                doc.attribution
            );
            assert!(doc.has_word_timing());
            assert!(doc.attribution.is_some());
        }
        other => panic!("LIVE Unison expected Found, got {other:?}"),
    }

    // 2. Live AMLL TTML DB (CC0-1.0) via direct Spotify ID (`7ajpbW6tBpqUI9foCtwlLw` - YOASOBI)
    let amll_track = Track {
        id: "7ajpbW6tBpqUI9foCtwlLw".into(),
        uri: "spotify:track:7ajpbW6tBpqUI9foCtwlLw".into(),
        title: "祝福".into(),
        artists: vec!["YOASOBI".into()],
        album_id: None,
        album_name: "THE BOOK 3".into(),
        duration_ms: 196_000,
        isrc: Some("JPP302200613".into()),
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    match svc
        .resolve_with_preference(&amll_track, LyricsSourcePreference::AmllTtmlDb, true, true)
        .await
    {
        LyricsResolution::Found(doc) => {
            println!(
                "[LIVE AMLL] source={} format={:?} word_synced={} lines={} first_line={:?} attr={:?}",
                doc.source,
                doc.format,
                doc.has_word_timing(),
                doc.lines.len(),
                doc.lines.first().map(|l| l.plain_text()),
                doc.attribution
            );
            assert!(doc.has_word_timing());
            assert_eq!(doc.format, LyricsFormat::Ttml);
        }
        other => panic!("LIVE AMLL TTML DB expected Found, got {other:?}"),
    }

    // 3. Live LRCLIB Word-Synced Lyricsfile v1.0 (Rick Astley - Never Gonna Give You Up)
    let lrclib_track = Track {
        id: "4cOdK2wGLETKBW3PvgPWqT".into(),
        uri: "spotify:track:4cOdK2wGLETKBW3PvgPWqT".into(),
        title: "Never Gonna Give You Up".into(),
        artists: vec!["Rick Astley".into()],
        album_id: None,
        album_name: "Whenever You Need Somebody".into(),
        duration_ms: 213_560,
        isrc: None,
        artwork_url: None,
        explicit: false,
        liked: false,
        local_path: None,
        accent_rgb: None,
    };
    match svc
        .resolve_with_preference(&lrclib_track, LyricsSourcePreference::Lrclib, true, true)
        .await
    {
        LyricsResolution::Found(doc) => {
            println!(
                "[LIVE LRCLIB] source={} format={:?} word_synced={} lines={} first_line={:?} words_in_line0={}",
                doc.source,
                doc.format,
                doc.has_word_timing(),
                doc.lines.len(),
                doc.lines.first().map(|l| l.plain_text()),
                doc.lines.first().map(|l| l.words.len()).unwrap_or(0)
            );
            assert!(doc.format.is_time_synced());
            assert!(doc.has_word_timing());
            assert_eq!(doc.format, LyricsFormat::LyricsfileWord);
        }
        other => panic!("LIVE LRCLIB expected Found, got {other:?}"),
    }
}

// =============================================================================
// 20. Auth Progress Stages, Runtime Session Expiration, Shared HTTP & API Errors
// =============================================================================

#[tokio::test]
async fn auth_progress_stages_runtime_session_expired_and_centralized_api_errors() {
    use spotirust::auth::session::AuthProgressStage;
    use spotirust::spotify::client::{SpotifyApiError, SpotifyClient};
    use spotirust::spotify::library::LibraryService;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    // Mock OAuth token server: first exchanges authorization_code, later rejects refresh with 400 invalid_grant
    let revoke_refresh = Arc::new(AtomicBool::new(false));
    let revoke_srv = Arc::clone(&revoke_refresh);
    let token_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_addr = token_listener.local_addr().unwrap();

    let token_server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = token_listener.accept().await else {
                break;
            };
            let rev = Arc::clone(&revoke_srv);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let resp = if req.contains("grant_type=authorization_code") {
                    assert!(req.contains("code_verifier="));
                    assert!(req.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A"));
                    let body = r#"{"access_token":"tok_live_1","token_type":"Bearer","scope":"user-read-private","expires_in":3600,"refresh_token":"ref_live_1"}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if rev.load(Ordering::SeqCst) {
                    let body =
                        r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#;
                    format!(
                        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    let body = r#"{"access_token":"tok_live_2","token_type":"Bearer","scope":"user-read-private","expires_in":3600,"refresh_token":"ref_live_2"}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    // Mock Spotify Web API server testing /me/player/currently-playing, 403 Forbidden, 501 Unsupported, and 401 -> session expiration
    let api_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_addr = api_listener.local_addr().unwrap();
    let force_401 = Arc::new(AtomicBool::new(false));
    let force_401_srv = Arc::clone(&force_401);

    let api_server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = api_listener.accept().await else {
                break;
            };
            let f401 = Arc::clone(&force_401_srv);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");

                let resp = if f401.load(Ordering::SeqCst) {
                    let body = r#"{"error":{"status":401,"message":"The access token expired"}}"#;
                    format!(
                        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if target.starts_with("/me/player/currently-playing") {
                    let body = r#"{"is_playing":true,"item":{"id":"tr_cp_1","uri":"spotify:track:tr_cp_1","name":"Signal Drift","duration_ms":201000,"artists":[{"name":"Neon Rust"}],"album":{"id":"alb_cp","uri":"spotify:album:alb_cp","name":"Loopback","images":[]}}}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if target.starts_with("/forbidden") {
                    let body = r#"{"error":{"status":403,"message":"Insufficient client scope"}}"#;
                    format!(
                        "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if target.starts_with("/unsupported") {
                    let body = r#"{"error":{"status":501,"message":"Not implemented"}}"#;
                    format!(
                        "HTTP/1.1 501 Not Implemented\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    let store = Arc::new(MemoryCredentialStore::new());
    let oauth_client =
        SpotifyOAuthClient::new().with_token_url(format!("http://{token_addr}/api/token"));
    let session = SessionManager::new(
        store.clone(),
        Some("public-pkce-client".to_string()),
        0,
        "/callback".to_string(),
    )
    .with_oauth_client(oauth_client);

    let mut expired_rx = session.subscribe_session_expired();

    // 1. Run full authenticate_via_browser_with_progress (without launching a real browser)
    //    and assert exact ordered stages: OpeningBrowser -> WaitingForAuthorization -> ExchangingToken!
    let stages = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let stages_cb = Arc::clone(&stages);
    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<String>();
    let url_tx_opt = Arc::new(Mutex::new(Some(url_tx)));

    let sess_auth = session.clone();
    let auth_handle = tokio::spawn(async move {
        sess_auth
            .authenticate_via_browser_with_progress(false, Duration::from_secs(5), move |stage| {
                match stage {
                    AuthProgressStage::OpeningBrowser { .. } => {
                        stages_cb.lock().unwrap().push("opening_browser");
                    }
                    AuthProgressStage::WaitingForAuthorization { authorize_url } => {
                        stages_cb.lock().unwrap().push("waiting_for_authorization");
                        if let Some(tx) = url_tx_opt.lock().unwrap().take() {
                            let _ = tx.send(authorize_url);
                        }
                    }
                    AuthProgressStage::ExchangingToken => {
                        stages_cb.lock().unwrap().push("exchanging_token");
                    }
                }
            })
            .await
    });

    // Extract redirect_uri and state from the authorize URL and simulate Spotify redirecting the browser
    let auth_url = url_rx.await.unwrap();
    let parsed = url::Url::parse(&auth_url).unwrap();
    let mut redirect_uri = String::new();
    let mut state = String::new();
    for (k, v) in parsed.query_pairs() {
        if k == "redirect_uri" {
            redirect_uri = v.into_owned();
        } else if k == "state" {
            state = v.into_owned();
        }
    }
    assert!(redirect_uri.starts_with("http://127.0.0.1:"));
    let cb_resp = reqwest::Client::new()
        .get(format!("{redirect_uri}?code=pkce_code_77&state={state}"))
        .send()
        .await
        .unwrap();
    assert_eq!(cb_resp.status(), 200);

    let bundle = auth_handle.await.unwrap().unwrap();
    assert_eq!(bundle.access_token, "tok_live_1");
    assert_eq!(
        *stages.lock().unwrap(),
        vec![
            "opening_browser",
            "waiting_for_authorization",
            "exchanging_token"
        ]
    );
    assert!(store.load_tokens().unwrap().is_some());

    // 2. Verify SpotifyClient shares session's HTTP client & handles currently_playing + 403 + 501
    let client = SpotifyClient::new(session.clone()).with_base_url(format!("http://{api_addr}"));
    let library = LibraryService::new(client.clone());

    let playing = library.currently_playing_track().await.unwrap().unwrap();
    assert_eq!(playing.id, "tr_cp_1");
    assert_eq!(playing.title, "Signal Drift");

    let forbidden: Result<serde_json::Value, _> = client.get_json("/forbidden", &[]).await;
    assert!(matches!(forbidden, Err(SpotifyApiError::Forbidden(_))));

    let unsupported: Result<serde_json::Value, _> = client.get_json("/unsupported", &[]).await;
    assert!(matches!(unsupported, Err(SpotifyApiError::Unsupported(_))));

    // 3. Simulate runtime 401 + revoked refresh token -> clears OS store AND emits SessionExpired broadcast!
    force_401.store(true, Ordering::SeqCst);
    revoke_refresh.store(true, Ordering::SeqCst);
    let expired_call = library.currently_playing_track().await;
    assert!(matches!(
        expired_call,
        Err(SpotifyApiError::Unauthenticated)
    ));
    assert!(!session.is_authenticated().await);
    assert!(store.load_tokens().unwrap().is_none());

    let expired_msg = tokio::time::timeout(Duration::from_secs(2), expired_rx.recv())
        .await
        .expect("SessionExpired broadcast should fire immediately")
        .unwrap();
    assert!(expired_msg.contains("expired") || expired_msg.contains("revoked"));

    token_server.abort();
    api_server.abort();
}

// =============================================================================
// 21. Full SpotifyAuthService + TokenManager Lifecycle: Login, Deny, Bad State,
//     Timeout, Restart Auto-Restore, Refresh, Logout & Re-Login, Revoked Refresh
// =============================================================================

#[tokio::test]
async fn spotify_auth_service_full_lifecycle_restart_logout_relogin_and_error_paths() {
    use spotirust::auth::oauth::{DEFAULT_SCOPES, parse_loopback_redirect_uri_config};
    use spotirust::auth::service::{AuthServiceError, AuthStatus, SpotifyAuthService};
    use spotirust::auth::session::AuthProgressStage;
    use spotirust::core::state::AuthUiState;
    use spotirust::spotify::client::SpotifyClient;
    use spotirust::spotify::library::LibraryService;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // Verify minimal required scopes (no unused write scopes or unnecessary PII scopes)
    assert!(!DEFAULT_SCOPES.contains(&"user-follow-modify"));
    assert!(!DEFAULT_SCOPES.contains(&"playlist-modify-public"));
    assert!(!DEFAULT_SCOPES.contains(&"playlist-modify-private"));
    assert!(!DEFAULT_SCOPES.contains(&"user-read-email"));
    assert!(DEFAULT_SCOPES.contains(&"user-read-private"));
    assert!(DEFAULT_SCOPES.contains(&"user-read-playback-state"));
    assert!(DEFAULT_SCOPES.contains(&"user-modify-playback-state"));
    assert!(DEFAULT_SCOPES.contains(&"user-read-currently-playing"));
    assert!(DEFAULT_SCOPES.contains(&"playlist-read-private"));
    assert!(DEFAULT_SCOPES.contains(&"user-library-read"));
    assert!(DEFAULT_SCOPES.contains(&"user-library-modify"));
    assert!(DEFAULT_SCOPES.contains(&"user-follow-read"));

    // Verify SPOTIFY_REDIRECT_URI validation rejects localhost and accepts 127.0.0.1
    assert!(parse_loopback_redirect_uri_config("http://localhost:8888/callback").is_err());
    assert_eq!(
        parse_loopback_redirect_uri_config("http://127.0.0.1/callback").unwrap(),
        (0, "/callback".to_string())
    );
    assert_eq!(
        parse_loopback_redirect_uri_config("http://127.0.0.1:43821/callback").unwrap(),
        (43821, "/callback".to_string())
    );

    // Mock OAuth token server
    let refresh_count = Arc::new(AtomicUsize::new(0));
    let revoke_refresh = Arc::new(AtomicBool::new(false));
    let refresh_count_srv = Arc::clone(&refresh_count);
    let revoke_srv = Arc::clone(&revoke_refresh);
    let token_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_addr = token_listener.local_addr().unwrap();

    let token_server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = token_listener.accept().await else {
                break;
            };
            let rc = Arc::clone(&refresh_count_srv);
            let rev = Arc::clone(&revoke_srv);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let resp = if req.contains("grant_type=authorization_code") {
                    assert!(req.contains("code_verifier="));
                    assert!(!req.contains("client_secret="));
                    let body = r#"{"access_token":"tok_login_1","token_type":"Bearer","scope":"user-read-private","expires_in":3600,"refresh_token":"ref_login_1"}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if rev.load(Ordering::SeqCst) {
                    let body =
                        r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#;
                    format!(
                        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    let n_ref = rc.fetch_add(1, Ordering::SeqCst) + 1;
                    let body = format!(
                        r#"{{"access_token":"tok_ref_{n_ref}","token_type":"Bearer","scope":"user-read-private","expires_in":3600,"refresh_token":"ref_rot_{n_ref}"}}"#
                    );
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    // Mock Spotify Web API `/me` server
    let api_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_addr = api_listener.local_addr().unwrap();
    let api_server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = api_listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let target = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let resp = if target.starts_with("/me") {
                    let body = r#"{"id":"user_99","display_name":"Rust Listener","images":[],"uri":"spotify:user:user_99"}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });

    let build_service = |store: Arc<MemoryCredentialStore>, client_id: Option<String>| {
        let oauth =
            SpotifyOAuthClient::new().with_token_url(format!("http://{token_addr}/api/token"));
        let session = SessionManager::new(store, client_id, 0, "/callback".to_string())
            .with_oauth_client(oauth);
        let client =
            SpotifyClient::new(session.clone()).with_base_url(format!("http://{api_addr}"));
        let library = LibraryService::new(client);
        SpotifyAuthService::new(session, library)
    };

    let store = Arc::new(MemoryCredentialStore::new());

    // 1. Missing Client ID fails cleanly without panicking
    let unconfigured = build_service(store.clone(), None);
    assert_eq!(
        unconfigured.spotify_get_auth_status().await,
        AuthStatus::NotLoggedIn
    );
    let missing_err = unconfigured
        .spotify_login(false, Duration::from_secs(2), |_| {})
        .await;
    assert!(matches!(
        missing_err,
        Err(AuthServiceError::OAuth(OAuthError::ClientIdNotConfigured))
    ));
    assert_eq!(
        unconfigured.spotify_get_auth_status().await,
        AuthStatus::AuthenticationFailed
    );

    let auth = build_service(store.clone(), Some("public-pkce-client".to_string()));
    assert_eq!(
        auth.spotify_get_auth_status().await,
        AuthStatus::NotLoggedIn
    );
    assert_eq!(
        AuthUiState::SignedOut.auth_status(),
        AuthStatus::NotLoggedIn
    );
    assert_eq!(
        AuthUiState::Refreshing.auth_status(),
        AuthStatus::Refreshing
    );

    // Helper to run spotify_login and return `(authorize_url, JoinHandle)`
    let spawn_login = |svc: SpotifyAuthService, timeout: Duration| async move {
        let (url_tx, url_rx) = tokio::sync::oneshot::channel::<String>();
        let tx_cell = Arc::new(Mutex::new(Some(url_tx)));
        let handle = tokio::spawn(async move {
            svc.spotify_login(false, timeout, move |stage| {
                if let AuthProgressStage::WaitingForAuthorization { authorize_url } = stage
                    && let Some(tx) = tx_cell.lock().unwrap().take()
                {
                    let _ = tx.send(authorize_url);
                }
            })
            .await
        });
        let auth_url = url_rx.await.unwrap();
        let parsed = url::Url::parse(&auth_url).unwrap();
        let mut redirect_uri = String::new();
        let mut state = String::new();
        for (k, v) in parsed.query_pairs() {
            if k == "redirect_uri" {
                redirect_uri = v.into_owned();
            } else if k == "state" {
                state = v.into_owned();
            }
        }
        (redirect_uri, state, handle)
    };

    let http = reqwest::Client::new();

    // 2. User denies consent (access_denied)
    let (redir_deny, state_deny, h_deny) = spawn_login(auth.clone(), Duration::from_secs(5)).await;
    let r = http
        .get(format!(
            "{redir_deny}?error=access_denied&state={state_deny}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert!(matches!(
        h_deny.await.unwrap(),
        Err(AuthServiceError::OAuth(OAuthError::AccessDenied { .. }))
    ));
    assert_eq!(
        auth.spotify_get_auth_status().await,
        AuthStatus::AuthenticationFailed
    );

    // 3. Forged CSRF state rejected
    let (redir_csrf, _state_csrf, h_csrf) = spawn_login(auth.clone(), Duration::from_secs(5)).await;
    let r = http
        .get(format!("{redir_csrf}?code=abc&state=forged_attacker_state"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert!(matches!(
        h_csrf.await.unwrap(),
        Err(AuthServiceError::OAuth(OAuthError::StateMismatch))
    ));
    assert_eq!(
        auth.spotify_get_auth_status().await,
        AuthStatus::AuthenticationFailed
    );

    // 4. Callback timeout shuts down listener cleanly
    let (_redir_to, _state_to, h_to) = spawn_login(auth.clone(), Duration::from_millis(120)).await;
    assert!(matches!(
        h_to.await.unwrap(),
        Err(AuthServiceError::OAuth(OAuthError::TimedOut))
    ));
    assert_eq!(
        auth.spotify_get_auth_status().await,
        AuthStatus::AuthenticationFailed
    );

    // 5. Successful login via PKCE S256 loopback
    let (redir_ok, state_ok, h_ok) = spawn_login(auth.clone(), Duration::from_secs(5)).await;
    let r = http
        .get(format!("{redir_ok}?code=valid_code_1&state={state_ok}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let profile = h_ok.await.unwrap().unwrap();
    assert_eq!(profile.id, "user_99");
    assert_eq!(profile.display_name, "Rust Listener");
    assert_eq!(
        auth.spotify_get_auth_status().await,
        AuthStatus::Authenticated
    );
    assert_eq!(
        auth.spotify_get_user_profile().await.unwrap().unwrap().id,
        "user_99"
    );

    // 6. Simulate app close & reopen with expired access token in OS store -> auto-refresh on startup!
    let mut expired_bundle = store.load_tokens().unwrap().unwrap();
    expired_bundle.expires_at_unix = 100; // far in the past
    store.save_tokens(&expired_bundle).unwrap();

    let restarted_auth = build_service(store.clone(), Some("public-pkce-client".to_string()));
    assert_eq!(
        restarted_auth.spotify_get_auth_status().await,
        AuthStatus::NotLoggedIn
    );
    let restored_profile = restarted_auth
        .restore_on_startup(None)
        .await
        .unwrap()
        .expect("should auto-restore & refresh persisted session on launch");
    assert_eq!(restored_profile.id, "user_99");
    assert_eq!(
        restarted_auth.spotify_get_auth_status().await,
        AuthStatus::Authenticated
    );
    assert_eq!(refresh_count.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.load_tokens().unwrap().unwrap().access_token,
        "tok_ref_1"
    );

    // 7. Explicit spotify_refresh_session rotates token and remains Authenticated
    let refreshed_profile = restarted_auth
        .spotify_refresh_session()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed_profile.id, "user_99");
    assert_eq!(refresh_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        store
            .load_tokens()
            .unwrap()
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("ref_rot_2")
    );

    // 8. Logout removes credentials & resets status to NotLoggedIn, then Re-Login succeeds
    restarted_auth.spotify_logout().await;
    assert_eq!(
        restarted_auth.spotify_get_auth_status().await,
        AuthStatus::NotLoggedIn
    );
    assert!(
        restarted_auth
            .spotify_get_user_profile()
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.load_tokens().unwrap().is_none());

    let (redir_re, state_re, h_re) =
        spawn_login(restarted_auth.clone(), Duration::from_secs(5)).await;
    let r = http
        .get(format!("{redir_re}?code=relogin_code_2&state={state_re}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(h_re.await.unwrap().unwrap().id, "user_99");
    assert_eq!(
        restarted_auth.spotify_get_auth_status().await,
        AuthStatus::Authenticated
    );

    // 9. Invalid/revoked refresh token clears OS store & marks session expired
    revoke_refresh.store(true, Ordering::SeqCst);
    let rev_res = restarted_auth.spotify_refresh_session().await;
    let rev_err = rev_res.expect_err("revoked refresh token must fail");
    assert!(rev_err.is_session_expired());
    assert_eq!(
        restarted_auth.spotify_get_auth_status().await,
        AuthStatus::AuthenticationFailed
    );
    assert!(store.load_tokens().unwrap().is_none());

    token_server.abort();
    api_server.abort();
}

// =============================================================================
// 11. Spotify Soloist Local WebSocket Protocol, Daemon Supervision & Connect Fallback
// =============================================================================

#[tokio::test]
async fn soloist_ws_protocol_playback_clock_and_commands_end_to_end() {
    use futures_util::{SinkExt, StreamExt};
    use spotirust::audio::soloist::SoloistStatus;
    use tokio_tungstenite::tungstenite::Message;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let (srv_push_tx, mut srv_push_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (client_cmd_tx, mut client_cmd_rx) =
        tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

    let server_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let (mut writer, mut reader) = ws.split();

        loop {
            tokio::select! {
                Some(outgoing) = srv_push_rx.recv() => {
                    if writer.send(Message::Text(outgoing.into())).await.is_err() {
                        break;
                    }
                }
                incoming = reader.next() => {
                    match incoming {
                        Some(Ok(Message::Text(txt))) => {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(txt.as_str()) {
                                let _ = client_cmd_tx.send(val);
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        _ => {}
                    }
                }
            }
        }
    });

    let tmp = tempfile::tempdir().unwrap();
    let controller = PlaybackController::for_testing_with_soloist(tmp.path().to_path_buf());
    let listener_handle = controller.spawn_soloist_event_listener();
    let soloist = controller.soloist().clone();

    soloist
        .connect_external_ws(format!("ws://{addr}"), "Spotirust Desk".to_string())
        .await
        .unwrap();

    // 1. Server sends auth_state with logged_in: false -> WaitingForPairing
    srv_push_tx
        .send(
            r#"{"type":"auth_state","logged_in":false,"is_active":false,"device_name":"Spotirust Desk"}"#
                .to_string(),
        )
        .unwrap();

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let st = soloist.runtime_snapshot().await;
            if matches!(st.status, SoloistStatus::WaitingForPairing { .. }) && !st.logged_in {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("should report WaitingForPairing before one-time Connect handshake");

    // 2. Server completes pairing -> auth_state logged_in: true -> client requests get_state + get_queue
    srv_push_tx
        .send(
            r#"{"type":"auth_state","logged_in":true,"is_active":false,"device_name":"Spotirust Desk"}"#
                .to_string(),
        )
        .unwrap();

    let cmd1 = tokio::time::timeout(Duration::from_secs(3), client_cmd_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let cmd2 = tokio::time::timeout(Duration::from_secs(3), client_cmd_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cmd1["type"], "command");
    assert_eq!(cmd1["command"], "get_state");
    assert_eq!(cmd2["type"], "command");
    assert_eq!(cmd2["command"], "get_queue");
    assert_eq!(cmd2["limit"], 50);

    let snap_rt = soloist.runtime_snapshot().await;
    assert!(snap_rt.logged_in);
    assert!(matches!(
        snap_rt.status,
        SoloistStatus::Connected {
            is_active: false,
            ..
        }
    ));

    // 3. Switch controller to SpotifySoloist (triggers immediate get_state + get_queue sync)
    controller
        .set_preferred_backend(AudioBackendKind::SpotifySoloist)
        .await;
    let sw1 = tokio::time::timeout(Duration::from_secs(3), client_cmd_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let sw2 = tokio::time::timeout(Duration::from_secs(3), client_cmd_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sw1["command"], "get_state");
    assert_eq!(sw2["command"], "get_queue");
    let now_unix_ms = chrono::Utc::now().timestamp_millis();
    srv_push_tx
        .send(format!(
            r#"{{
                "type": "playback_state",
                "status": "playing",
                "item": {{
                    "uri": "spotify:track:sol_track_1",
                    "entity_type": "track",
                    "decorations": {{
                        "identity": {{ "name": "Headless Groove" }},
                        "visual_identity": {{
                            "cover": [{{ "url": "https://i.scdn.co/large_art", "size": "large" }}]
                        }},
                        "parent": {{
                            "entity": {{
                                "uri": "spotify:album:sol_alb_1",
                                "entity_type": "album",
                                "decorations": {{ "identity": {{ "name": "Soloist Sessions" }} }}
                            }}
                        }},
                        "creators": [{{
                            "entity": {{
                                "uri": "spotify:artist:sol_art_1",
                                "entity_type": "artist",
                                "decorations": {{ "identity": {{ "name": "Rust Ensemble" }} }}
                            }}
                        }}],
                        "playback": {{ "duration_ms": 240000, "content_ratings": ["explicit"] }}
                    }}
                }},
                "position": {{ "position_ms": 15000, "timestamp_ms": {now_unix_ms}, "speed": 1.0 }},
                "volume": 72,
                "is_active": false,
                "options": {{ "shuffle": true, "repeat": "context", "playback_speed": 1.0 }}
            }}"#
        ))
        .unwrap();

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let s = controller.snapshot().await;
            if s.backend == AudioBackendKind::SpotifySoloist
                && s.status == PlaybackStatus::Playing
                && s.current_track.as_ref().map(|t| t.id.as_str()) == Some("sol_track_1")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("PlaybackController should synchronize with Soloist playback_state");

    let snap = controller.snapshot().await;
    let track = snap.current_track.as_ref().unwrap();
    assert_eq!(track.title, "Headless Groove");
    assert_eq!(track.artists, vec!["Rust Ensemble"]);
    assert_eq!(track.album_name, "Soloist Sessions");
    assert_eq!(
        track.artwork_url.as_deref(),
        Some("https://i.scdn.co/large_art")
    );
    assert!(track.explicit);
    assert!((snap.volume - 0.72).abs() < 0.01);
    assert!(snap.shuffle);
    assert_eq!(snap.repeat, RepeatMode::Context);
    assert!(snap.position.current_ms() >= 14_800 && snap.position.current_ms() < 18_000);

    // 4. Granular position_sync + volume_changed + queue_changed
    let sync_ts = chrono::Utc::now().timestamp_millis();
    srv_push_tx
        .send(format!(
            r#"{{"type":"position_sync","position":{{"position_ms":91000,"timestamp_ms":{sync_ts},"speed":1.0}}}}"#
        ))
        .unwrap();
    srv_push_tx
        .send(r#"{"type":"volume_changed","volume":45}"#.to_string())
        .unwrap();
    srv_push_tx
        .send(
            r#"{
                "type":"queue_changed",
                "previous":[],
                "upcoming":[{
                    "uid":"q1",
                    "source":"user",
                    "item":{
                        "uri":"spotify:track:sol_track_2",
                        "entity_type":"track",
                        "decorations":{
                            "identity":{"name":"Next Track"},
                            "playback":{"duration_ms":180000,"content_ratings":[]}
                        }
                    }
                }]
            }"#
            .to_string(),
        )
        .unwrap();

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let s = controller.snapshot().await;
            if (s.volume - 0.45).abs() < 0.01 && s.position.current_ms() >= 90_000 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("PlaybackController should absorb position_sync and volume_changed");

    // 5. Outbound transport & option commands verify exact official Soloist JSON schema
    // Play a Spotify track while is_active == false -> sends activate, then play(uri)
    let mut sp_track = make_track("sol_track_3", "Play Me", "Artist", 200_000);
    sp_track.uri = "spotify:track:sol_track_3".to_string();
    controller
        .play_track_in_context(sp_track.clone(), vec![sp_track.clone()])
        .await
        .unwrap();

    let mut received = Vec::new();
    for _ in 0..2 {
        received.push(
            tokio::time::timeout(Duration::from_secs(3), client_cmd_rx.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert_eq!(received[0]["command"], "activate");
    assert_eq!(received[1]["command"], "play");
    assert_eq!(received[1]["uri"], "spotify:track:sol_track_3");

    // Mark device active so resume does not re-send activate
    srv_push_tx
        .send(
            r#"{"type":"device_changed","is_active":true,"device_name":"Spotirust Desk"}"#
                .to_string(),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !soloist.is_active_device().await {
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .unwrap();

    controller.pause().await.unwrap();
    controller.resume().await.unwrap();
    controller.seek(42_500).await.unwrap();
    controller.set_volume(0.83).await.unwrap();
    controller.toggle_shuffle().await;
    // Currently RepeatMode::Context -> cycle_repeat transitions to RepeatMode::Track (2 ordered frames!)
    controller.cycle_repeat().await;
    controller.add_to_queue(sp_track).await.unwrap();
    controller.next().await.unwrap();
    // At 42.5s (> 3s), previous() restarts the track via seek(0); at 0ms, previous() sends skip_prev!
    controller.previous().await.unwrap();
    controller.previous().await.unwrap();

    let mut cmds = Vec::new();
    for _ in 0..12 {
        cmds.push(
            tokio::time::timeout(Duration::from_secs(3), client_cmd_rx.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert_eq!(cmds[0]["command"], "pause");
    assert_eq!(cmds[1]["command"], "play");
    assert!(cmds[1].get("uri").is_none());
    assert_eq!(cmds[2]["command"], "seek");
    assert_eq!(cmds[2]["position_ms"], 42_500);
    assert_eq!(cmds[3]["command"], "set_volume");
    assert_eq!(cmds[3]["volume"], 83);
    assert_eq!(cmds[4]["command"], "set_shuffle");
    assert_eq!(cmds[4]["enabled"], false);
    assert_eq!(cmds[5]["command"], "set_repeat_context");
    assert_eq!(cmds[5]["enabled"], false);
    assert_eq!(cmds[6]["command"], "set_repeat_track");
    assert_eq!(cmds[6]["enabled"], true);
    assert_eq!(cmds[7]["command"], "add_to_queue");
    assert_eq!(cmds[7]["uri"], "spotify:track:sol_track_3");
    assert_eq!(cmds[8]["command"], "get_queue");
    assert_eq!(cmds[9]["command"], "skip_next");
    assert_eq!(cmds[10]["command"], "seek");
    assert_eq!(cmds[10]["position_ms"], 0);
    assert_eq!(cmds[11]["command"], "skip_prev");

    soloist.stop().await;
    assert_eq!(
        soloist.runtime_snapshot().await.status,
        SoloistStatus::Stopped
    );
    listener_handle.abort();
    server_task.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn soloist_managed_daemon_expiry_exit_code_10_redaction_and_ws_discovery() {
    use futures_util::SinkExt;
    use spotirust::audio::soloist::{SoloistEngine, SoloistStatus};
    use std::os::unix::fs::PermissionsExt;
    use tokio_tungstenite::tungstenite::Message;

    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    let data_dir = tmp.path().join("data");
    let cache_dir = tmp.path().join("cache");
    std::fs::create_dir_all(&bin_dir).unwrap();

    let write_script = |name: &str, body: &str| {
        let p = bin_dir.join(name);
        std::fs::write(&p, body).unwrap();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&p, perms).unwrap();
        p
    };

    let engine = SoloistEngine::new(bin_dir.clone(), data_dir.clone(), cache_dir);

    // Case A: Exit code 10 -> official 90-day build expiry detection
    let expired_script = write_script("soloist_expired", "#!/bin/sh\nexit 10\n");
    engine
        .start_managed(
            expired_script,
            "Spotirust".to_string(),
            "test-api-key-secret".to_string(),
        )
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if matches!(
                engine.runtime_snapshot().await.status,
                SoloistStatus::Expired { .. }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("exit code 10 must transition SoloistStatus to Expired");

    // Case B: Non-zero crash printing API key to stderr -> secret MUST be redacted
    let leak_script = write_script(
        "soloist_crash",
        "#!/bin/sh\necho 'auth rejected for key super-secret-soloist-key-xyz' >&2\nexit 2\n",
    );
    engine
        .start_managed(
            leak_script,
            "Spotirust".to_string(),
            "super-secret-soloist-key-xyz".to_string(),
        )
        .await
        .unwrap();

    let err_msg = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if let SoloistStatus::Error { message } = engine.runtime_snapshot().await.status {
                break message;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("crash exit must transition SoloistStatus to Error");
    assert!(
        !err_msg.contains("super-secret-soloist-key-xyz"),
        "API key must NEVER leak in Soloist error diagnostics: {err_msg}"
    );
    assert!(err_msg.contains("[REDACTED]"));

    // Case C: Real ws.addr / ws.port discovery + WebSocket handshake + clean stop()
    let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    let ws_server = tokio::spawn(async move {
        let (stream, _) = ws_listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let _ = ws
            .send(Message::Text(
                r#"{"type":"auth_state","logged_in":true,"is_active":true,"device_name":"Spotirust Managed"}"#
                    .into(),
            ))
            .await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let ws_script = write_script(
        "soloist_ok",
        &format!(
            "#!/bin/sh\n\
             DATA_DIR=''\n\
             while [ $# -gt 0 ]; do\n\
               if [ \"$1\" = '--data-dir' ]; then DATA_DIR=\"$2\"; shift 2; else shift; fi\n\
             done\n\
             printf '127.0.0.1' > \"$DATA_DIR/ws.addr\"\n\
             printf '{ws_port}' > \"$DATA_DIR/ws.port\"\n\
             sleep 30\n"
        ),
    );

    engine
        .start_managed(
            ws_script,
            "Spotirust Managed".to_string(),
            "key-ok".to_string(),
        )
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                engine.runtime_snapshot().await.status,
                SoloistStatus::Connected {
                    is_active: true,
                    ws_port: p,
                    ..
                } if p == ws_port
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("managed Soloist should discover ws.addr/ws.port and connect over WebSocket");

    engine.stop().await;
    assert_eq!(
        engine.runtime_snapshot().await.status,
        SoloistStatus::Stopped
    );
    ws_server.abort();
}

#[tokio::test]
async fn spotify_connect_auto_resolves_device_from_devices_endpoint() {
    use spotirust::spotify::client::SpotifyClient;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    let playing = Arc::new(AtomicBool::new(false));
    let played_device = Arc::new(Mutex::new(String::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let playing_srv = Arc::clone(&playing);
    let played_dev_srv = Arc::clone(&played_device);
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let playing_c = Arc::clone(&playing_srv);
            let played_dev_c = Arc::clone(&played_dev_srv);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                let mut parts = first_line.split_whitespace();
                let method = parts.next().unwrap_or("");
                let target = parts.next().unwrap_or("");

                let response = if method == "GET" && target.starts_with("/me/player/devices") {
                    let body = r#"{"devices":[
                        {"id":"dev_restricted","is_active":false,"is_restricted":true,"name":"Locked Speaker","type":"Speaker","volume_percent":50},
                        {"id":"dev_soloist_1","is_active":false,"is_restricted":false,"name":"Spotirust Soloist","type":"Computer","volume_percent":80}
                    ]}"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else if method == "PUT" && target.starts_with("/me/player/play") {
                    if let Some(q) = target.split_once('?').map(|(_, q)| q) {
                        *played_dev_c.lock().unwrap() = q.to_string();
                    }
                    playing_c.store(true, Ordering::SeqCst);
                    "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                } else if method == "GET" && target == "/me/player" {
                    if !playing_c.load(Ordering::SeqCst) {
                        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    } else {
                        let body = r#"{
                            "is_playing": true,
                            "progress_ms": 1200,
                            "shuffle_state": false,
                            "repeat_state": "off",
                            "device": {"id":"dev_soloist_1","is_active":true,"is_restricted":false,"name":"Spotirust Soloist","type":"Computer","volume_percent":80},
                            "item": {
                                "id": "sp_fallback_1",
                                "uri": "spotify:track:sp_fallback_1",
                                "name": "Connect Fallback Song",
                                "duration_ms": 195000,
                                "explicit": false,
                                "artists": [{"id":"a1","uri":"spotify:artist:a1","name":"Fallback Artist"}],
                                "album": {"id":"alb1","uri":"spotify:album:alb1","name":"Fallback Album","images":[]}
                            }
                        }"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    }
                } else {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_string()
                };

                let _ = sock.write_all(response.as_bytes()).await;
            });
        }
    });

    let store = Arc::new(MemoryCredentialStore::new());
    let session = SessionManager::new(
        store,
        Some("test-client".to_string()),
        0,
        "/callback".to_string(),
    );
    session
        .set_tokens(StoredTokenBundle {
            access_token: "mock-access-token".to_string(),
            refresh_token: None,
            token_type: "Bearer".to_string(),
            scope: "user-read-playback-state user-modify-playback-state".to_string(),
            expires_at_unix: 9_999_999_999,
            client_id: "test-client".to_string(),
        })
        .await;

    let spotify = SpotifyClient::new(session).with_base_url(format!("http://{addr}"));
    let controller = PlaybackController::new(Some(spotify), None, PlaybackEventBus::default());
    controller
        .set_preferred_backend(AudioBackendKind::SpotifyConnect)
        .await;

    // Even before any manual device refresh, playing a Spotify track must query /me/player/devices,
    // skip the restricted speaker, target dev_soloist_1, and confirm Playing!
    let mut sp_track = make_track(
        "sp_fallback_1",
        "Connect Fallback Song",
        "Fallback Artist",
        195_000,
    );
    sp_track.uri = "spotify:track:sp_fallback_1".to_string();
    controller
        .play_track_in_context(sp_track.clone(), vec![sp_track])
        .await
        .expect(
            "Connect play_track_in_context should auto-resolve available non-restricted device",
        );

    assert_eq!(
        played_device.lock().unwrap().as_str(),
        "device_id=dev_soloist_1"
    );
    let snap = controller.snapshot().await;
    assert_eq!(snap.status, PlaybackStatus::Playing);
    assert_eq!(snap.backend, AudioBackendKind::SpotifyConnect);
    assert_eq!(
        snap.active_device_name.as_deref(),
        Some("Spotirust Soloist")
    );

    server.abort();
}
