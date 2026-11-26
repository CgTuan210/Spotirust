//! Spotirust desktop entry point supporting portable mode (`--portable`)
//! and headless benchmark/diagnostics (`--diagnostics`).

use spotirust::app::SpotirustApp;
use spotirust::core::config::AppConfig;
use std::time::Instant;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    let startup_clock = Instant::now();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("spotirust=info,warn")),
        )
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let force_portable = args.iter().any(|a| a == "--portable");
    let run_diagnostics = args.iter().any(|a| a == "--diagnostics");

    if run_diagnostics {
        return run_headless_diagnostics(startup_clock, force_portable);
    }

    let config = AppConfig::load(force_portable)?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        portable = config.portable_mode,
        client_id_configured = config.spotify_client_id.is_some(),
        "Launching Spotirust"
    );

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Spotirust")
            .with_inner_size([1240.0, 800.0])
            .with_min_inner_size([840.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Spotirust",
        native_options,
        Box::new(move |cc| {
            let app = SpotirustApp::bootstrap(cc, config)
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
            tracing::info!(
                startup_ms = startup_clock.elapsed().as_millis(),
                "Spotirust UI initialized"
            );
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

/// Runs real, reproducible startup, memory, SQLite migration, and lyrics parser/lookup
/// measurements without requiring a graphical display server (useful in CI and profiling).
fn run_headless_diagnostics(startup_clock: Instant, force_portable: bool) -> anyhow::Result<()> {
    use spotirust::lyrics::lrc::parse_lrc;
    use spotirust::lyrics::ttml::parse_ttml;
    use spotirust::storage::credentials::OsKeyringStore;
    use spotirust::storage::database::Database;

    let config = AppConfig::load(force_portable)?;
    let db = Database::open_in_memory()?;
    let schema_ver = db.schema_version()?;

    let ttml_raw = include_str!("../assets/sample_lyrics/spotirust_demo.ttml");
    let lrc_raw = include_str!("../assets/sample_lyrics/spotirust_demo.lrc");

    // Benchmark 1,000 TTML parses
    let t0 = Instant::now();
    let mut doc = None;
    for _ in 0..1_000 {
        doc = Some(parse_ttml(ttml_raw, "bench")?);
    }
    let ttml_us = t0.elapsed().as_micros() as f64 / 1_000.0;

    // Benchmark 1,000 Enhanced LRC parses
    let t1 = Instant::now();
    for _ in 0..1_000 {
        let _ = parse_lrc(lrc_raw, "bench")?;
    }
    let lrc_us = t1.elapsed().as_micros() as f64 / 1_000.0;

    // Benchmark 100,000 karaoke active-line + word-progress evaluations
    let doc = doc.unwrap();
    let t2 = Instant::now();
    let mut acc = 0.0f32;
    for i in 0..100_000u64 {
        let pos = (i * 17) % 20_000;
        if let Some(idx) = doc.active_line_index(pos) {
            for w in &doc.lines[idx].words {
                acc += w.progress_at(pos);
            }
        }
    }
    let karaoke_ns = t2.elapsed().as_nanos() as f64 / 100_000.0;

    // Benchmark KaraokeLayoutCache: uncached full-doc Arc<Galley> shaping vs steady-state cache hit
    let egui_ctx = egui::Context::default();
    let mut init_out = egui_ctx.run_ui(egui::RawInput::default(), |_| {});
    init_out.textures_delta.clear();
    let theme = spotirust::ui::theme::ThemePalette::obsidian_dark();
    let mut layout_cache = spotirust::ui::lyrics::KaraokeLayoutCache::default();

    let t3 = Instant::now();
    for _ in 0..500u64 {
        layout_cache.clear();
        layout_cache.ensure_layout_borrowed(
            &egui_ctx,
            "local:midnight_compiler",
            1,
            false,
            620,
            Some(1),
            true,
            &doc,
            &theme,
        );
    }
    let layout_rebuild_us = t3.elapsed().as_micros() as f64 / 500.0;

    let t4 = Instant::now();
    for _ in 0..10_000u64 {
        layout_cache.ensure_layout_borrowed(
            &egui_ctx,
            "local:midnight_compiler",
            1,
            false,
            620,
            Some(1),
            true,
            &doc,
            &theme,
        );
    }
    let layout_hit_ns = t4.elapsed().as_nanos() as f64 / 10_000.0;

    let rss_kb = read_linux_rss_kb();
    let elapsed_ms = startup_clock.elapsed().as_secs_f64() * 1000.0;

    println!("Spotirust Diagnostics v{}", env!("CARGO_PKG_VERSION"));
    println!("  Portable mode:             {}", config.portable_mode);
    println!(
        "  Spotify Client ID set:     {}",
        config.spotify_client_id.is_some()
    );
    println!(
        "  OS Keyring available:      {}",
        OsKeyringStore::is_available()
    );
    println!("  SQLite schema version:     v{schema_ver}");
    println!("  Headless init time:        {elapsed_ms:.2} ms");
    if let Some(kb) = rss_kb {
        println!(
            "  Process RSS (headless):    {:.2} MB ({kb} KB)",
            kb as f64 / 1024.0
        );
    }
    println!("  TTML parse avg (1k runs):  {ttml_us:.2} µs/doc");
    println!("  Enhanced LRC avg (1k):     {lrc_us:.2} µs/doc");
    println!("  Karaoke lookup avg (100k): {karaoke_ns:.1} ns/frame (checksum={acc:.1})");
    println!(
        "  Karaoke layout uncached:   {layout_rebuild_us:.1} µs/frame (cached hit: {layout_hit_ns:.1} ns/frame)"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_linux_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.split_whitespace().next()?.parse::<u64>().ok();
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn read_linux_rss_kb() -> Option<u64> {
    None
}
