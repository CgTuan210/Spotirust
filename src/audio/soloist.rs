//! Official **Spotify Soloist** managed headless playback engine & local WebSocket API client.
//!
//! Spotify Soloist is Spotify's official standalone headless Connect client (launched Aug 2026;
//! Linux, Apple Silicon macOS, and x86_64 Windows). Spotirust integrates Soloist cleanly and
//! compliantly:
//! - Never bundles or redistributes Spotify's proprietary binary; downloads directly from Spotify's
//!   official CDN (`soloist-builds.spotifycdn.com`) on explicit user action, or uses a local binary.
//! - Each user supplies their own personal Soloist API key from
//!   `https://developer.spotify.com/dashboard/soloist` (requires Spotify Premium), stored strictly
//!   in the OS credential vault (`keyring`) and never written to SQLite or logs.
//! - Spawns `soloist --ws 127.0.0.1:0` bound strictly to IPv4 loopback, discovers the ephemeral port
//!   via `ws.addr` / `ws.port`, and speaks the official JSON WebSocket protocol for zero-polling
//!   real-time playback state, queue, and millisecond `position_sync` karaoke updates.
//! - Detects 90-day build expiry (exit code `10`) cleanly and surfaces a one-click Update action.

use crate::audio::engine::{AudioEngine, AudioError, PlaybackPosition};
use crate::core::models::{RepeatMode, Track};
use async_trait::async_trait;
use chrono::Utc;
use flate2::read::{DeflateDecoder, GzDecoder};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

/// Official Spotify Developer Dashboard page where each user generates their personal Soloist API key.
pub const SOLOIST_DASHBOARD_URL: &str = "https://developer.spotify.com/dashboard/soloist";

/// Official Spotify Soloist Downloads & Updates documentation page.
pub const SOLOIST_DOWNLOADS_PAGE_URL: &str =
    "https://developer.spotify.com/documentation/soloist/reference/downloads-and-updates";

/// Official Spotify Soloist CDN base URL documented in Spotify's Getting Started guide.
pub const SOLOIST_CDN_BASE: &str = "https://soloist-builds.spotifycdn.com";

/// Official exit code returned by `soloist` when its 90-day build lifetime has expired.
pub const SOLOIST_EXPIRED_EXIT_CODE: i32 = 10;

/// Maximum decompressed executable size (256 MiB) accepted from archives as an anti-zip-bomb guard.
const MAX_SOLOIST_BINARY_BYTES: usize = 256 * 1024 * 1024;

/// Maximum stderr diagnostic lines retained in memory (secret-redacted).
const MAX_STDERR_LINES: usize = 24;

/// Resolve an optional Spotify Soloist API key from the `SOLOIST_API_KEY` environment variable.
pub fn resolve_env_soloist_key() -> Option<String> {
    std::env::var("SOLOIST_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Supported archive container format for official Soloist builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoloistArchiveKind {
    TarGz,
    Zip,
}

/// Platform-specific official Spotify Soloist archive metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoloistArchiveSpec {
    pub url: String,
    pub archive_kind: SoloistArchiveKind,
    pub executable_name: &'static str,
    pub platform_label: &'static str,
}

/// Resolve the official Spotify Soloist archive specification for an explicit `(os, arch)` pair.
pub fn archive_spec_for(os: &str, arch: &str) -> Result<SoloistArchiveSpec, String> {
    match (os, arch) {
        ("linux", "x86_64") => Ok(SoloistArchiveSpec {
            url: format!("{SOLOIST_CDN_BASE}/soloist_release_x86_64.tar.gz"),
            archive_kind: SoloistArchiveKind::TarGz,
            executable_name: "soloist",
            platform_label: "Linux x86_64",
        }),
        ("linux", "aarch64") => Ok(SoloistArchiveSpec {
            url: format!("{SOLOIST_CDN_BASE}/soloist_release_arm64.tar.gz"),
            archive_kind: SoloistArchiveKind::TarGz,
            executable_name: "soloist",
            platform_label: "Linux ARM64 (AArch64)",
        }),
        ("linux", "arm") => Ok(SoloistArchiveSpec {
            url: format!("{SOLOIST_CDN_BASE}/soloist_release_arm32.tar.gz"),
            archive_kind: SoloistArchiveKind::TarGz,
            executable_name: "soloist",
            platform_label: "Linux ARMv7 (ARM32)",
        }),
        ("macos", "aarch64") => Ok(SoloistArchiveSpec {
            url: format!("{SOLOIST_CDN_BASE}/soloist_release_osx_arm64.tar.gz"),
            archive_kind: SoloistArchiveKind::TarGz,
            executable_name: "soloist",
            platform_label: "macOS Apple Silicon (arm64)",
        }),
        ("windows", "x86_64") => Ok(SoloistArchiveSpec {
            url: format!("{SOLOIST_CDN_BASE}/soloist_release_windows_x86_64.zip"),
            archive_kind: SoloistArchiveKind::Zip,
            executable_name: "soloist.exe",
            platform_label: "Windows x86_64",
        }),
        _ => Err(format!(
            "Spotify Soloist does not publish an official prebuilt binary for {os}/{arch}. \
             Supported platforms: Linux (x86_64, aarch64, armv7), macOS Apple Silicon (arm64), and Windows (x86_64)."
        )),
    }
}

/// Resolve the official Spotify Soloist archive specification for the host platform.
pub fn current_platform_archive_spec() -> Result<SoloistArchiveSpec, String> {
    archive_spec_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Default executable filename for the current OS (`soloist` or `soloist.exe`).
pub fn soloist_executable_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "soloist.exe"
    } else {
        "soloist"
    }
}

/// Locate an installed Spotify Soloist binary in priority order:
/// 1. User-specified custom binary path
/// 2. Spotirust-managed binary inside `bin_dir`
/// 3. System `PATH`
pub fn find_soloist_binary(custom_path: Option<&Path>, bin_dir: &Path) -> Option<PathBuf> {
    if let Some(p) = custom_path
        && p.is_file()
    {
        return Some(p.to_path_buf());
    }

    let managed = bin_dir.join(soloist_executable_name());
    if managed.is_file() {
        return Some(managed);
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(soloist_executable_name());
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Metadata reported after installing or probing a Soloist executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoloistBinaryInfo {
    pub path: PathBuf,
    pub version_output: Option<String>,
}

/// Probe `soloist --version` safely with a short timeout.
pub async fn probe_soloist_version(binary: &Path) -> Result<String, String> {
    let mut cmd = Command::new(binary);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    configure_hidden_process(&mut cmd);

    let output = tokio::time::timeout(Duration::from_secs(5), cmd.output())
        .await
        .map_err(|_| "Timed out running soloist --version".to_string())?
        .map_err(|e| format!("Failed executing {}: {e}", binary.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.code() == Some(SOLOIST_EXPIRED_EXIT_CODE) {
        return Err(
            "Spotify Soloist build has expired (90-day limit, exit code 10). Download a newer build."
                .to_string(),
        );
    }

    if !output.status.success() {
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(format!("soloist --version failed: {detail}"));
    }

    let first_line = stdout
        .lines()
        .find(|l| !l.trim().is_empty())
        .or_else(|| stderr.lines().find(|l| !l.trim().is_empty()))
        .unwrap_or("Spotify Soloist")
        .trim()
        .to_string();
    Ok(first_line)
}

/// Download the latest official Spotify Soloist build directly from Spotify's CDN, safely
/// extract only the `soloist` executable, verify its executable header, and install atomically.
pub async fn download_and_install_soloist(
    http: &reqwest::Client,
    bin_dir: &Path,
) -> Result<SoloistBinaryInfo, String> {
    let spec = current_platform_archive_spec()?;
    tracing::info!(
        url = %spec.url,
        platform = spec.platform_label,
        "Downloading official Spotify Soloist build"
    );

    let response = http
        .get(&spec.url)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| format!("Failed downloading Spotify Soloist from Spotify CDN: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Spotify CDN returned HTTP {} downloading {}",
            response.status().as_u16(),
            spec.url
        ));
    }

    let archive_bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed reading Spotify Soloist archive stream: {e}"))?;

    let expected_name = spec.executable_name;
    let kind = spec.archive_kind;
    let target_os = std::env::consts::OS;
    let bin_dir_owned = bin_dir.to_path_buf();

    let installed_path = tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
        let exe_bytes = match kind {
            SoloistArchiveKind::TarGz => extract_binary_from_targz(&archive_bytes, expected_name)?,
            SoloistArchiveKind::Zip => extract_binary_from_zip(&archive_bytes, expected_name)?,
        };
        validate_executable_magic(&exe_bytes, target_os)?;

        std::fs::create_dir_all(&bin_dir_owned)
            .map_err(|e| format!("Failed creating Soloist bin directory: {e}"))?;

        let final_path = bin_dir_owned.join(expected_name);
        let tmp_path = bin_dir_owned.join(format!(".{expected_name}.download.tmp"));
        std::fs::write(&tmp_path, &exe_bytes)
            .map_err(|e| format!("Failed writing Soloist executable: {e}"))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&tmp_path)
                .map_err(|e| format!("Failed reading permissions: {e}"))?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&tmp_path, perms)
                .map_err(|e| format!("Failed setting executable permission: {e}"))?;
        }

        // On Windows, rename over an existing file fails if the destination exists.
        #[cfg(windows)]
        if final_path.exists() {
            let _ = std::fs::remove_file(&final_path);
        }

        std::fs::rename(&tmp_path, &final_path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            format!(
                "Failed installing Soloist executable to {}: {e}",
                final_path.display()
            )
        })?;

        Ok(final_path)
    })
    .await
    .map_err(|e| format!("Soloist extraction task failed: {e}"))??;

    let version_output = probe_soloist_version(&installed_path).await.ok();
    Ok(SoloistBinaryInfo {
        path: installed_path,
        version_output,
    })
}

/// Validate that extracted executable bytes match the host OS binary header (ELF, Mach-O, or PE).
pub fn validate_executable_magic(bytes: &[u8], os: &str) -> Result<(), String> {
    if bytes.len() < 4 {
        return Err("Extracted Soloist binary is unexpectedly empty or truncated".to_string());
    }
    let ok = match os {
        "linux" => bytes.starts_with(b"\x7fELF"),
        "macos" => {
            let magic = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            matches!(
                magic,
                0xFEED_FACE | 0xCEFA_EDFE | 0xFEED_FACF | 0xCFFA_EDFE | 0xCAFE_BABE | 0xBEBA_FECA
            )
        }
        "windows" => bytes.starts_with(b"MZ"),
        _ => true,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "Downloaded Soloist file does not have a valid {os} executable header"
        ))
    }
}

/// Safely extract `expected_filename` from a `.tar.gz` archive (ignoring README/CHANGELOG/LICENSE
/// files and rejecting path traversal).
pub fn extract_binary_from_targz(
    archive_bytes: &[u8],
    expected_filename: &str,
) -> Result<Vec<u8>, String> {
    let mut decoder = GzDecoder::new(archive_bytes);
    let mut header = [0u8; 512];
    let mut pending_long_name: Option<String> = None;

    loop {
        if !read_exact_or_eof(&mut decoder, &mut header)? {
            break;
        }

        // Standard tar end-of-archive marker is two consecutive 512-byte zero blocks.
        if header.iter().all(|&b| b == 0) {
            break;
        }

        let size = parse_tar_octal(&header[124..136])
            .ok_or_else(|| "Malformed tar entry size in Soloist archive".to_string())?;
        let typeflag = header[156];

        let raw_name = if let Some(long) = pending_long_name.take() {
            long
        } else {
            let name_part = parse_cstr(&header[0..100]);
            let prefix_part = parse_cstr(&header[345..500]);
            if prefix_part.is_empty() {
                name_part
            } else {
                format!("{prefix_part}/{name_part}")
            }
        };

        let padded_len = (size + 511) & !511;

        // GNU long filename ('L') or PAX extended header ('x')
        if typeflag == b'L' {
            if size > 64 * 1024 {
                return Err("Tar GNU long name exceeds safety limit".to_string());
            }
            let mut buf = vec![0u8; size as usize];
            decoder
                .read_exact(&mut buf)
                .map_err(|e| format!("Failed reading tar long name: {e}"))?;
            skip_bytes(&mut decoder, padded_len - size)?;
            pending_long_name = Some(parse_cstr(&buf));
            continue;
        }
        if typeflag == b'x' {
            if size > 64 * 1024 {
                return Err("Tar PAX header exceeds safety limit".to_string());
            }
            let mut buf = vec![0u8; size as usize];
            decoder
                .read_exact(&mut buf)
                .map_err(|e| format!("Failed reading tar PAX header: {e}"))?;
            skip_bytes(&mut decoder, padded_len - size)?;
            if let Some(pax_path) = parse_pax_path(&buf) {
                pending_long_name = Some(pax_path);
            }
            continue;
        }

        let is_regular_file = typeflag == 0 || typeflag == b'0';
        if is_regular_file && safe_basename_matches(&raw_name, expected_filename) {
            if size as usize > MAX_SOLOIST_BINARY_BYTES {
                return Err("Soloist binary in archive exceeds maximum allowed size".to_string());
            }
            let mut data = vec![0u8; size as usize];
            decoder
                .read_exact(&mut data)
                .map_err(|e| format!("Failed extracting '{expected_filename}' from tar.gz: {e}"))?;
            return Ok(data);
        }

        skip_bytes(&mut decoder, padded_len)?;
    }

    Err(format!(
        "Could not locate '{expected_filename}' executable inside Spotify Soloist tar.gz archive"
    ))
}

/// Safely extract `expected_filename` from a `.zip` archive (supporting Stored & Deflate entries,
/// ignoring README/CHANGELOG/LICENSE files, and rejecting path traversal).
pub fn extract_binary_from_zip(
    archive_bytes: &[u8],
    expected_filename: &str,
) -> Result<Vec<u8>, String> {
    if archive_bytes.len() < 22 {
        return Err("Spotify Soloist ZIP archive is truncated".to_string());
    }

    // Locate End of Central Directory record (PK\x05\x06) scanning backwards from EOF.
    let min_eocd_pos = archive_bytes.len().saturating_sub(22 + 65_535);
    let mut eocd_pos = None;
    let mut i = archive_bytes.len() - 22;
    loop {
        if &archive_bytes[i..i + 4] == b"PK\x05\x06" {
            let comment_len = read_u16_le(archive_bytes, i + 20) as usize;
            if i + 22 + comment_len == archive_bytes.len() {
                eocd_pos = Some(i);
                break;
            }
        }
        if i == min_eocd_pos {
            break;
        }
        i -= 1;
    }
    let eocd = eocd_pos.ok_or_else(|| "Invalid ZIP archive (missing EOCD record)".to_string())?;

    let entry_count = read_u16_le(archive_bytes, eocd + 10) as usize;
    let cd_size = read_u32_le(archive_bytes, eocd + 12) as usize;
    let cd_offset = read_u32_le(archive_bytes, eocd + 16) as usize;

    if cd_offset
        .checked_add(cd_size)
        .is_none_or(|end| end > archive_bytes.len())
    {
        return Err("ZIP central directory out of bounds".to_string());
    }

    let mut pos = cd_offset;
    for _ in 0..entry_count {
        if pos + 46 > archive_bytes.len() || &archive_bytes[pos..pos + 4] != b"PK\x01\x02" {
            return Err("Malformed ZIP central directory entry".to_string());
        }
        let method = read_u16_le(archive_bytes, pos + 10);
        let compressed_size = read_u32_le(archive_bytes, pos + 20) as usize;
        let uncompressed_size = read_u32_le(archive_bytes, pos + 24) as usize;
        let name_len = read_u16_le(archive_bytes, pos + 28) as usize;
        let extra_len = read_u16_le(archive_bytes, pos + 30) as usize;
        let comment_len = read_u16_le(archive_bytes, pos + 32) as usize;
        let local_header_offset = read_u32_le(archive_bytes, pos + 42) as usize;

        let name_start = pos + 46;
        let name_end = name_start + name_len;
        if name_end > archive_bytes.len() {
            return Err("ZIP entry filename exceeds archive bounds".to_string());
        }
        let name = String::from_utf8_lossy(&archive_bytes[name_start..name_end]).to_string();

        pos = name_end + extra_len + comment_len;

        if name.ends_with('/') || !safe_basename_matches(&name, expected_filename) {
            continue;
        }

        if uncompressed_size > MAX_SOLOIST_BINARY_BYTES {
            return Err("Soloist binary in ZIP exceeds maximum allowed size".to_string());
        }

        if local_header_offset + 30 > archive_bytes.len()
            || &archive_bytes[local_header_offset..local_header_offset + 4] != b"PK\x03\x04"
        {
            return Err("Malformed ZIP local file header".to_string());
        }
        let lh_name_len = read_u16_le(archive_bytes, local_header_offset + 26) as usize;
        let lh_extra_len = read_u16_le(archive_bytes, local_header_offset + 28) as usize;
        let data_start = local_header_offset + 30 + lh_name_len + lh_extra_len;
        let data_end = data_start
            .checked_add(compressed_size)
            .ok_or_else(|| "ZIP entry size overflow".to_string())?;
        if data_end > archive_bytes.len() {
            return Err("ZIP entry payload out of bounds".to_string());
        }
        let payload = &archive_bytes[data_start..data_end];

        let extracted = match method {
            0 => payload.to_vec(),
            8 => {
                let decoder = DeflateDecoder::new(payload);
                let mut out = Vec::with_capacity(uncompressed_size);
                decoder
                    .take((MAX_SOLOIST_BINARY_BYTES + 1) as u64)
                    .read_to_end(&mut out)
                    .map_err(|e| format!("Failed inflating '{expected_filename}' from ZIP: {e}"))?;
                out
            }
            other => {
                return Err(format!(
                    "Unsupported ZIP compression method {other} for '{expected_filename}'"
                ));
            }
        };

        if extracted.len() != uncompressed_size {
            return Err(format!(
                "Extracted size mismatch for '{expected_filename}' (expected {uncompressed_size}, got {})",
                extracted.len()
            ));
        }

        return Ok(extracted);
    }

    Err(format!(
        "Could not locate '{expected_filename}' executable inside Spotify Soloist ZIP archive"
    ))
}

fn safe_basename_matches(entry_path: &str, expected_filename: &str) -> bool {
    let normalized = entry_path.replace('\\', "/");
    if normalized.starts_with('/') || normalized.split('/').any(|seg| seg == "..") {
        return false;
    }
    let basename = normalized.rsplit('/').next().unwrap_or("");
    basename.eq_ignore_ascii_case(expected_filename)
}

fn parse_cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

fn parse_tar_octal(field: &[u8]) -> Option<u64> {
    // Support GNU base-256 binary size encoding (leading 0x80) as well as standard octal ASCII.
    if field.first() == Some(&0x80) {
        let mut val = 0u64;
        for &b in &field[1..] {
            val = (val << 8) | u64::from(b);
        }
        return Some(val);
    }
    let s = parse_cstr(field);
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(trimmed, 8).ok()
}

fn parse_pax_path(pax_bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(pax_bytes);
    for line in text.lines() {
        if let Some((_, kv)) = line.split_once(' ')
            && let Some(val) = kv.strip_prefix("path=")
        {
            return Some(val.to_string());
        }
    }
    None
}

fn read_exact_or_eof(reader: &mut impl Read, buf: &mut [u8]) -> Result<bool, String> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(false);
                }
                return Err("Unexpected EOF inside tar.gz archive".to_string());
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("Failed reading tar.gz stream: {e}")),
        }
    }
    Ok(true)
}

fn skip_bytes(reader: &mut impl Read, mut remaining: u64) -> Result<(), String> {
    let mut sink = [0u8; 4096];
    while remaining > 0 {
        let to_read = (remaining as usize).min(sink.len());
        let n = reader
            .read(&mut sink[..to_read])
            .map_err(|e| format!("Failed skipping tar entry payload: {e}"))?;
        if n == 0 {
            return Err("Unexpected EOF while skipping tar entry".to_string());
        }
        remaining -= n as u64;
    }
    Ok(())
}

fn read_u16_le(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

// =============================================================================
// Official Spotify Soloist Local WebSocket Protocol DTOs
// =============================================================================

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SoloistIdentity {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SoloistCoverImage {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub size: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SoloistVisualIdentity {
    #[serde(default)]
    pub cover: Vec<SoloistCoverImage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SoloistEntityRef {
    pub entity: Option<Box<SoloistEntity>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SoloistPlaybackDecoration {
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub content_ratings: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SoloistDecorations {
    #[serde(default)]
    pub identity: Option<SoloistIdentity>,
    #[serde(default)]
    pub visual_identity: Option<SoloistVisualIdentity>,
    #[serde(default)]
    pub parent: Option<SoloistEntityRef>,
    #[serde(default)]
    pub creators: Vec<SoloistEntityRef>,
    #[serde(default)]
    pub playback: Option<SoloistPlaybackDecoration>,
}

/// Canonical entity envelope returned by Soloist WebSocket events (`item`, `context`, queue entries).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SoloistEntity {
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub entity_type: String,
    #[serde(default)]
    pub decorations: SoloistDecorations,
}

impl SoloistEntity {
    /// Convert a Soloist entity into Spotirust's canonical `Track` model when playable metadata exists.
    pub fn into_track(&self) -> Option<Track> {
        let uri = self.uri.trim();
        let title = self
            .decorations
            .identity
            .as_ref()
            .map(|id| id.name.trim().to_string())
            .filter(|s| !s.is_empty());

        if uri.is_empty() && title.is_none() {
            return None;
        }
        if !self.entity_type.is_empty()
            && self.entity_type != "track"
            && self.entity_type != "episode"
            && !uri.starts_with("spotify:track:")
            && !uri.starts_with("spotify:episode:")
        {
            return None;
        }

        let id = uri
            .strip_prefix("spotify:track:")
            .or_else(|| uri.strip_prefix("spotify:episode:"))
            .unwrap_or(uri)
            .to_string();

        let artists: Vec<String> = self
            .decorations
            .creators
            .iter()
            .filter_map(|c| c.entity.as_deref())
            .filter_map(|e| e.decorations.identity.as_ref())
            .map(|i| i.name.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let (album_id, album_name) = self
            .decorations
            .parent
            .as_ref()
            .and_then(|p| p.entity.as_deref())
            .map(|parent| {
                let alb_id = parent
                    .uri
                    .strip_prefix("spotify:album:")
                    .or_else(|| parent.uri.strip_prefix("spotify:show:"))
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string);
                let alb_name = parent
                    .decorations
                    .identity
                    .as_ref()
                    .map(|i| i.name.trim().to_string())
                    .unwrap_or_default();
                (alb_id, alb_name)
            })
            .unwrap_or((None, String::new()));

        let artwork_url = self
            .decorations
            .visual_identity
            .as_ref()
            .and_then(|vis| pick_best_cover(&vis.cover));

        let (duration_ms, explicit) = self
            .decorations
            .playback
            .as_ref()
            .map(|pb| {
                let exp = pb
                    .content_ratings
                    .iter()
                    .any(|r| r.eq_ignore_ascii_case("explicit"));
                (pb.duration_ms, exp)
            })
            .unwrap_or((0, false));

        Some(Track {
            id: if id.is_empty() {
                title.clone().unwrap_or_default()
            } else {
                id
            },
            uri: uri.to_string(),
            title: title.unwrap_or_else(|| uri.to_string()),
            artists,
            album_id,
            album_name,
            duration_ms,
            isrc: None,
            artwork_url,
            explicit,
            liked: false,
            local_path: None,
            accent_rgb: None,
        })
    }
}

fn pick_best_cover(covers: &[SoloistCoverImage]) -> Option<String> {
    for preferred in ["large", "xlarge", "default", "small"] {
        if let Some(img) = covers
            .iter()
            .find(|c| c.size.eq_ignore_ascii_case(preferred) && !c.url.trim().is_empty())
        {
            return Some(img.url.clone());
        }
    }
    covers
        .iter()
        .find(|c| !c.url.trim().is_empty())
        .map(|c| c.url.clone())
}

/// Position anchor transmitted in `playback_state` and `position_sync`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SoloistPosition {
    #[serde(default)]
    pub position_ms: u64,
    #[serde(default)]
    pub timestamp_ms: i64,
    #[serde(default)]
    pub speed: f64,
}

impl SoloistPosition {
    /// Compute the current playback position in milliseconds at `now_unix_ms`,
    /// rejecting implausible wall-clock skew (`> 2000 ms`) so karaoke sync stays rock-solid.
    pub fn estimated_ms_at(&self, now_unix_ms: i64) -> u64 {
        if self.speed <= 0.0 || self.timestamp_ms <= 0 {
            return self.position_ms;
        }
        let elapsed = now_unix_ms.saturating_sub(self.timestamp_ms);
        if elapsed.abs() > 2_000 {
            return self.position_ms;
        }
        let delta = (elapsed as f64 * self.speed).round();
        (self.position_ms as f64 + delta).max(0.0).round() as u64
    }

    pub fn estimated_ms_now(&self) -> u64 {
        self.estimated_ms_at(Utc::now().timestamp_millis())
    }
}

/// Playback options transmitted in `playback_state` and `options_changed`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SoloistOptions {
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default)]
    pub repeat: String,
    #[serde(default = "default_speed_one")]
    pub playback_speed: f64,
}

fn default_speed_one() -> f64 {
    1.0
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SoloistQueueEntry {
    #[serde(default)]
    pub uid: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub item: SoloistEntity,
}

/// Full playback snapshot delivered in Soloist's `playback_state` WebSocket event.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SoloistPlaybackStatePayload {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub item: Option<SoloistEntity>,
    #[serde(default)]
    pub context: Option<SoloistEntity>,
    #[serde(default)]
    pub position: SoloistPosition,
    #[serde(default)]
    pub volume: u8,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub options: SoloistOptions,
}

/// Server-to-client WebSocket JSON event frames emitted by Spotify Soloist.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SoloistWsEvent {
    AuthState {
        #[serde(default)]
        logged_in: bool,
        #[serde(default)]
        is_active: bool,
        #[serde(default)]
        device_name: String,
    },
    PlaybackState(Box<SoloistPlaybackStatePayload>),
    TrackChanged {
        #[serde(default)]
        item: Option<SoloistEntity>,
    },
    PlaybackChanged {
        #[serde(default)]
        status: String,
    },
    VolumeChanged {
        #[serde(default)]
        volume: u8,
    },
    DeviceChanged {
        #[serde(default)]
        is_active: bool,
        #[serde(default)]
        device_name: String,
    },
    ContextChanged {
        #[serde(default)]
        context: Option<SoloistEntity>,
    },
    OptionsChanged {
        #[serde(default)]
        options: SoloistOptions,
    },
    PositionSync {
        #[serde(default)]
        position: SoloistPosition,
    },
    QueueChanged {
        #[serde(default)]
        previous: Vec<SoloistQueueEntry>,
        #[serde(default)]
        upcoming: Vec<SoloistQueueEntry>,
    },
    CommandResult {
        #[serde(default)]
        command: String,
    },
    Error {
        #[serde(default)]
        message: String,
    },
}

/// Client-to-server JSON command payload matching Spotify Soloist's WebSocket reference.
#[derive(Debug, Clone, Serialize)]
struct SoloistWsCommandFrame {
    #[serde(rename = "type")]
    msg_type: &'static str,
    command: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    volume: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
}

impl SoloistWsCommandFrame {
    fn simple(command: &'static str) -> Self {
        Self {
            msg_type: "command",
            command,
            uri: None,
            position_ms: None,
            volume: None,
            enabled: None,
            limit: None,
        }
    }

    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

// =============================================================================
// Runtime Status, Events & Managed Soloist Engine
// =============================================================================

/// Honest lifecycle state of the managed Spotify Soloist daemon & local WebSocket connection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SoloistStatus {
    #[default]
    Stopped,
    Downloading,
    Starting,
    /// Daemon is running and advertising on LAN; user needs to select it ONCE in the Spotify app.
    WaitingForPairing {
        device_name: String,
        ws_port: u16,
    },
    /// Paired and authenticated — works even after closing the official Spotify app.
    Connected {
        device_name: String,
        is_active: bool,
        ws_port: u16,
    },
    /// Official 90-day build expired (`soloist` exited with code 10).
    Expired {
        message: String,
    },
    /// Startup, binary, or connection error with actionable diagnostics.
    Error {
        message: String,
    },
}

impl SoloistStatus {
    pub fn short_label(&self) -> String {
        match self {
            Self::Stopped => "Stopped".to_string(),
            Self::Downloading => "Downloading…".to_string(),
            Self::Starting => "Starting…".to_string(),
            Self::WaitingForPairing { device_name, .. } => {
                format!("Waiting to pair '{device_name}' in Spotify app")
            }
            Self::Connected {
                device_name,
                is_active,
                ..
            } => {
                if *is_active {
                    format!("Active · {device_name}")
                } else {
                    format!("Ready · {device_name}")
                }
            }
            Self::Expired { .. } => "Build expired (update required)".to_string(),
            Self::Error { .. } => "Error".to_string(),
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::WaitingForPairing { .. } | Self::Connected { .. }
        )
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

/// UI-facing snapshot of Spotify Soloist installation, key status, and live daemon state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoloistRuntimeSnapshot {
    pub status: SoloistStatus,
    pub binary_path: Option<PathBuf>,
    pub binary_version: Option<String>,
    pub api_key_configured: bool,
    pub device_name: String,
    pub logged_in: bool,
    pub is_active: bool,
    pub platform_supported: bool,
    pub platform_note: Option<String>,
}

impl Default for SoloistRuntimeSnapshot {
    fn default() -> Self {
        let (platform_supported, platform_note) = match current_platform_archive_spec() {
            Ok(spec) => (true, Some(spec.platform_label.to_string())),
            Err(err) => (false, Some(err)),
        };
        Self {
            status: SoloistStatus::Stopped,
            binary_path: None,
            binary_version: None,
            api_key_configured: false,
            device_name: "Spotirust".to_string(),
            logged_in: false,
            is_active: false,
            platform_supported,
            platform_note,
        }
    }
}

/// Events emitted by [`SoloistEngine`] to `PlaybackController` and the desktop UI coordinator.
#[derive(Debug, Clone)]
pub enum SoloistEvent {
    RuntimeChanged(SoloistRuntimeSnapshot),
    PlaybackState(SoloistPlaybackStatePayload),
    TrackChanged(Option<Track>),
    PlaybackStatusChanged(String),
    PositionSync(SoloistPosition),
    VolumeChanged(u8),
    OptionsChanged(SoloistOptions),
    QueueChanged {
        upcoming: Vec<Track>,
    },
    DeviceChanged {
        is_active: bool,
        device_name: String,
    },
    ApiError(String),
}

struct SoloistInner {
    snapshot: SoloistRuntimeSnapshot,
    ws_tx: Option<mpsc::UnboundedSender<String>>,
    last_position: PlaybackPosition,
    task_handle: Option<tokio::task::JoinHandle<()>>,
    child: Arc<Mutex<Option<Child>>>,
}

/// Managed Spotify Soloist daemon supervisor and official local WebSocket API engine.
#[derive(Clone)]
pub struct SoloistEngine {
    inner: Arc<RwLock<SoloistInner>>,
    events_tx: broadcast::Sender<SoloistEvent>,
    generation: Arc<AtomicU64>,
    bin_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

impl SoloistEngine {
    pub fn new(bin_dir: PathBuf, data_dir: PathBuf, cache_dir: PathBuf) -> Self {
        let (events_tx, _) = broadcast::channel(128);
        Self {
            inner: Arc::new(RwLock::new(SoloistInner {
                snapshot: SoloistRuntimeSnapshot::default(),
                ws_tx: None,
                last_position: PlaybackPosition::default(),
                task_handle: None,
                child: Arc::new(Mutex::new(None)),
            })),
            events_tx,
            generation: Arc::new(AtomicU64::new(0)),
            bin_dir,
            data_dir,
            cache_dir,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SoloistEvent> {
        self.events_tx.subscribe()
    }

    pub fn bin_dir(&self) -> &Path {
        &self.bin_dir
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub async fn runtime_snapshot(&self) -> SoloistRuntimeSnapshot {
        self.inner.read().await.snapshot.clone()
    }

    /// Returns `true` when Soloist is running, WebSocket-connected, and paired (`logged_in == true`).
    pub async fn is_ready(&self) -> bool {
        let guard = self.inner.read().await;
        guard.ws_tx.is_some() && guard.snapshot.logged_in
    }

    pub async fn is_active_device(&self) -> bool {
        self.inner.read().await.snapshot.is_active
    }

    pub async fn device_name(&self) -> String {
        self.inner.read().await.snapshot.device_name.clone()
    }

    /// Refresh binary/key/device metadata in the snapshot without starting the daemon.
    pub async fn refresh_installation_status(
        &self,
        custom_binary: Option<&Path>,
        device_name: &str,
        api_key_configured: bool,
    ) -> SoloistRuntimeSnapshot {
        let found = find_soloist_binary(custom_binary, &self.bin_dir);
        let version = match &found {
            Some(path) => probe_soloist_version(path).await.ok(),
            None => None,
        };
        let snap = {
            let mut guard = self.inner.write().await;
            guard.snapshot.binary_path = found;
            guard.snapshot.binary_version = version;
            guard.snapshot.api_key_configured = api_key_configured;
            if !device_name.trim().is_empty() {
                guard.snapshot.device_name = device_name.trim().to_string();
            }
            guard.snapshot.clone()
        };
        let _ = self
            .events_tx
            .send(SoloistEvent::RuntimeChanged(snap.clone()));
        snap
    }

    pub async fn set_downloading(&self, downloading: bool) {
        let snap = {
            let mut guard = self.inner.write().await;
            if downloading {
                guard.snapshot.status = SoloistStatus::Downloading;
            } else if matches!(guard.snapshot.status, SoloistStatus::Downloading) {
                guard.snapshot.status = SoloistStatus::Stopped;
            }
            guard.snapshot.clone()
        };
        let _ = self.events_tx.send(SoloistEvent::RuntimeChanged(snap));
    }

    pub async fn set_error_status(&self, message: String) {
        let snap = {
            let mut guard = self.inner.write().await;
            guard.snapshot.status = SoloistStatus::Error { message };
            guard.snapshot.logged_in = false;
            guard.snapshot.is_active = false;
            guard.snapshot.clone()
        };
        let _ = self.events_tx.send(SoloistEvent::RuntimeChanged(snap));
    }

    /// Spawn and supervise the official `soloist` binary bound strictly to `127.0.0.1:0`,
    /// discover its ephemeral port via `ws.addr`/`ws.port`, and connect over local WebSocket.
    pub async fn start_managed(
        &self,
        binary_path: PathBuf,
        device_name: String,
        api_key: String,
    ) -> Result<(), AudioError> {
        let api_key = api_key.trim().to_string();
        if api_key.is_empty() {
            return Err(AudioError::BackendUnavailable(
                "Spotify Soloist API key is not configured. Generate your personal key in Settings."
                    .to_string(),
            ));
        }
        if !binary_path.is_file() {
            return Err(AudioError::BackendUnavailable(format!(
                "Spotify Soloist executable not found at {}. Click 'Download Soloist' in Settings.",
                binary_path.display()
            )));
        }

        let clean_name = if device_name.trim().is_empty() {
            "Spotirust".to_string()
        } else {
            device_name.trim().to_string()
        };

        self.stop().await;

        std::fs::create_dir_all(&self.data_dir)
            .map_err(|e| AudioError::BackendUnavailable(format!("create soloist data dir: {e}")))?;
        std::fs::create_dir_all(&self.cache_dir).map_err(|e| {
            AudioError::BackendUnavailable(format!("create soloist cache dir: {e}"))
        })?;

        // Remove stale runtime discovery files from any prior ungraceful exit.
        let _ = std::fs::remove_file(self.data_dir.join("ws.addr"));
        let _ = std::fs::remove_file(self.data_dir.join("ws.port"));

        let session_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut guard = self.inner.write().await;
            guard.snapshot.binary_path = Some(binary_path.clone());
            guard.snapshot.device_name = clean_name.clone();
            guard.snapshot.api_key_configured = true;
            guard.snapshot.logged_in = false;
            guard.snapshot.is_active = false;
            guard.snapshot.status = SoloistStatus::Starting;
            let _ = self
                .events_tx
                .send(SoloistEvent::RuntimeChanged(guard.snapshot.clone()));
        }

        let mut cmd = Command::new(&binary_path);
        cmd.arg("--device-name")
            .arg(&clean_name)
            .arg("--api-key")
            .arg(&api_key)
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--cache-dir")
            .arg(&self.cache_dir)
            .arg("--ws")
            .arg("127.0.0.1:0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        configure_hidden_process(&mut cmd);

        tracing::info!(
            device_name = %clean_name,
            binary = %binary_path.display(),
            data_dir = %self.data_dir.display(),
            "Spawning managed Spotify Soloist daemon (--ws 127.0.0.1:0)"
        );

        let mut child = cmd.spawn().map_err(|e| {
            let msg = format!("Failed launching Spotify Soloist binary: {e}");
            AudioError::BackendUnavailable(msg)
        })?;

        let stderr_lines = Arc::new(Mutex::new(VecDeque::<String>::with_capacity(
            MAX_STDERR_LINES,
        )));
        if let Some(stderr) = child.stderr.take() {
            let buf_ref = Arc::clone(&stderr_lines);
            let redact_key = api_key.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let safe_line = redact_secret(&line, &redact_key);
                    tracing::debug!(soloist_stderr = %safe_line);
                    let mut q = buf_ref.lock().await;
                    if q.len() >= MAX_STDERR_LINES {
                        q.pop_front();
                    }
                    q.push_back(safe_line);
                }
            });
        }

        let child_slot = {
            let guard = self.inner.read().await;
            Arc::clone(&guard.child)
        };
        *child_slot.lock().await = Some(child);

        let engine = self.clone();
        let data_dir = self.data_dir.clone();
        let handle = tokio::spawn(async move {
            engine
                .supervise_managed_daemon(
                    session_gen,
                    clean_name,
                    data_dir,
                    child_slot,
                    stderr_lines,
                )
                .await;
        });

        self.inner.write().await.task_handle = Some(handle);
        Ok(())
    }

    /// Connect directly to an existing local Soloist WebSocket endpoint (`ws://127.0.0.1:<port>`).
    /// Also used by automated end-to-end loopback WebSocket integration tests.
    pub async fn connect_external_ws(
        &self,
        ws_url: String,
        device_name: String,
    ) -> Result<(), AudioError> {
        self.stop().await;
        let session_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let port = parse_ws_port(&ws_url).unwrap_or(0);

        {
            let mut guard = self.inner.write().await;
            guard.snapshot.device_name = device_name.clone();
            guard.snapshot.logged_in = false;
            guard.snapshot.is_active = false;
            guard.snapshot.status = SoloistStatus::Starting;
            let _ = self
                .events_tx
                .send(SoloistEvent::RuntimeChanged(guard.snapshot.clone()));
        }

        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|e| {
                AudioError::BackendUnavailable(format!("Soloist WebSocket connect failed: {e}"))
            })?;

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<String>();
        {
            let mut guard = self.inner.write().await;
            guard.ws_tx = Some(cmd_tx);
            guard.snapshot.status = SoloistStatus::WaitingForPairing {
                device_name: device_name.clone(),
                ws_port: port,
            };
            let _ = self
                .events_tx
                .send(SoloistEvent::RuntimeChanged(guard.snapshot.clone()));
        }

        let engine = self.clone();
        let handle = tokio::spawn(async move {
            engine
                .run_ws_session(session_gen, ws_stream, cmd_rx, port)
                .await;
            if engine.generation.load(Ordering::SeqCst) == session_gen {
                let mut guard = engine.inner.write().await;
                guard.ws_tx = None;
                guard.snapshot.logged_in = false;
                guard.snapshot.is_active = false;
                if !matches!(guard.snapshot.status, SoloistStatus::Stopped) {
                    guard.snapshot.status = SoloistStatus::Stopped;
                }
                let _ = engine
                    .events_tx
                    .send(SoloistEvent::RuntimeChanged(guard.snapshot.clone()));
            }
        });

        self.inner.write().await.task_handle = Some(handle);
        Ok(())
    }

    /// Gracefully stop any managed Soloist child process and close the local WebSocket link.
    pub async fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let (handle, child_slot) = {
            let mut guard = self.inner.write().await;
            guard.ws_tx = None;
            guard.last_position.is_playing = false;
            guard.snapshot.logged_in = false;
            guard.snapshot.is_active = false;
            guard.snapshot.status = SoloistStatus::Stopped;
            let _ = self
                .events_tx
                .send(SoloistEvent::RuntimeChanged(guard.snapshot.clone()));
            (guard.task_handle.take(), Arc::clone(&guard.child))
        };

        if let Some(h) = handle {
            h.abort();
        }

        if let Some(mut child) = child_slot.lock().await.take() {
            let _ = child.kill().await;
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        }
    }

    /// Synchronous best-effort shutdown hook called from `SpotirustApp::on_exit`.
    pub fn stop_blocking(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = self.inner.try_write() {
            guard.ws_tx = None;
            if let Some(h) = guard.task_handle.take() {
                h.abort();
            }
            if let Ok(mut child_guard) = guard.child.try_lock()
                && let Some(mut child) = child_guard.take()
            {
                let _ = child.start_kill();
            }
        }
    }

    /// Stop Soloist and wipe its stored Connect session directory so a different account can pair.
    pub async fn reset_pairing_session(&self) -> Result<(), String> {
        self.stop().await;
        if self.data_dir.exists() {
            std::fs::remove_dir_all(&self.data_dir)
                .map_err(|e| format!("Failed clearing Soloist session data: {e}"))?;
        }
        std::fs::create_dir_all(&self.data_dir)
            .map_err(|e| format!("Failed recreating Soloist data dir: {e}"))?;
        Ok(())
    }

    async fn supervise_managed_daemon(
        &self,
        session_gen: u64,
        device_name: String,
        data_dir: PathBuf,
        child_slot: Arc<Mutex<Option<Child>>>,
        stderr_lines: Arc<Mutex<VecDeque<String>>>,
    ) {
        // Phase 1: Wait for Soloist to write ws.addr + ws.port (or exit early on error/expiry).
        let endpoint = match self
            .wait_for_ws_discovery(session_gen, &data_dir, &child_slot, &stderr_lines)
            .await
        {
            Ok(ep) => ep,
            Err(status) => {
                if self.generation.load(Ordering::SeqCst) == session_gen {
                    let mut guard = self.inner.write().await;
                    guard.ws_tx = None;
                    guard.snapshot.logged_in = false;
                    guard.snapshot.is_active = false;
                    guard.snapshot.status = status;
                    let _ = self
                        .events_tx
                        .send(SoloistEvent::RuntimeChanged(guard.snapshot.clone()));
                }
                return;
            }
        };

        let (ws_url, ws_port) = endpoint;
        tracing::info!(ws_url = %ws_url, "Discovered local Spotify Soloist WebSocket endpoint");

        // Phase 2: Keep WebSocket connected while the managed child process stays alive.
        let mut backoff_ms = 250u64;
        loop {
            if self.generation.load(Ordering::SeqCst) != session_gen {
                return;
            }

            if let Some(exit_status) = check_child_exited(&child_slot, &stderr_lines).await {
                if self.generation.load(Ordering::SeqCst) == session_gen {
                    let mut guard = self.inner.write().await;
                    guard.ws_tx = None;
                    guard.snapshot.logged_in = false;
                    guard.snapshot.is_active = false;
                    guard.snapshot.status = exit_status;
                    let _ = self
                        .events_tx
                        .send(SoloistEvent::RuntimeChanged(guard.snapshot.clone()));
                }
                return;
            }

            match tokio_tungstenite::connect_async(&ws_url).await {
                Ok((ws_stream, _)) => {
                    backoff_ms = 250;
                    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<String>();
                    {
                        let mut guard = self.inner.write().await;
                        guard.ws_tx = Some(cmd_tx);
                        if matches!(guard.snapshot.status, SoloistStatus::Starting) {
                            guard.snapshot.status = SoloistStatus::WaitingForPairing {
                                device_name: device_name.clone(),
                                ws_port,
                            };
                            let _ = self
                                .events_tx
                                .send(SoloistEvent::RuntimeChanged(guard.snapshot.clone()));
                        }
                    }

                    self.run_ws_session(session_gen, ws_stream, cmd_rx, ws_port)
                        .await;

                    if self.generation.load(Ordering::SeqCst) != session_gen {
                        return;
                    }
                    self.inner.write().await.ws_tx = None;
                }
                Err(err) => {
                    tracing::debug!(error = %err, "Retrying local Soloist WebSocket connection");
                }
            }

            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(2_000);
        }
    }

    async fn wait_for_ws_discovery(
        &self,
        session_gen: u64,
        data_dir: &Path,
        child_slot: &Arc<Mutex<Option<Child>>>,
        stderr_lines: &Arc<Mutex<VecDeque<String>>>,
    ) -> Result<(String, u16), SoloistStatus> {
        let addr_path = data_dir.join("ws.addr");
        let port_path = data_dir.join("ws.port");
        let deadline = Instant::now() + Duration::from_secs(12);

        while Instant::now() < deadline {
            if self.generation.load(Ordering::SeqCst) != session_gen {
                return Err(SoloistStatus::Stopped);
            }

            if let Some(exit_status) = check_child_exited(child_slot, stderr_lines).await {
                return Err(exit_status);
            }

            if let (Ok(addr_raw), Ok(port_raw)) = (
                std::fs::read_to_string(&addr_path),
                std::fs::read_to_string(&port_path),
            ) {
                let addr = addr_raw.trim();
                if let Ok(port) = port_raw.trim().parse::<u16>()
                    && !addr.is_empty()
                    && port > 0
                {
                    let host = if addr.contains(':') && !addr.starts_with('[') {
                        format!("[{addr}]")
                    } else {
                        addr.to_string()
                    };
                    return Ok((format!("ws://{host}:{port}"), port));
                }
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let tail = format_stderr_tail(stderr_lines).await;
        let detail = if tail.is_empty() {
            "Timed out waiting for Spotify Soloist local WebSocket (ws.addr / ws.port).".to_string()
        } else {
            format!("Timed out waiting for Spotify Soloist WebSocket. Last log: {tail}")
        };
        Err(SoloistStatus::Error { message: detail })
    }

    async fn run_ws_session(
        &self,
        session_gen: u64,
        ws_stream: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        mut cmd_rx: mpsc::UnboundedReceiver<String>,
        ws_port: u16,
    ) {
        let (mut writer, mut reader) = ws_stream.split();

        loop {
            if self.generation.load(Ordering::SeqCst) != session_gen {
                let _ = writer.close().await;
                return;
            }

            tokio::select! {
                outgoing = cmd_rx.recv() => {
                    match outgoing {
                        Some(json) => {
                            if writer.send(Message::Text(json.into())).await.is_err() {
                                return;
                            }
                        }
                        None => {
                            let _ = writer.close().await;
                            return;
                        }
                    }
                }
                incoming = reader.next() => {
                    match incoming {
                        Some(Ok(Message::Text(text))) => {
                            self.handle_ws_text(session_gen, text.as_str(), ws_port).await;
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            if writer.send(Message::Pong(payload)).await.is_err() {
                                return;
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => return,
                        Some(Err(err)) => {
                            tracing::debug!(error = %err, "Soloist WebSocket stream ended");
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    async fn handle_ws_text(&self, session_gen: u64, raw: &str, ws_port: u16) {
        if self.generation.load(Ordering::SeqCst) != session_gen {
            return;
        }
        let evt: SoloistWsEvent = match serde_json::from_str(raw) {
            Ok(e) => e,
            Err(err) => {
                tracing::debug!(error = %err, "Ignoring unrecognized Soloist WebSocket frame");
                return;
            }
        };

        match evt {
            SoloistWsEvent::AuthState {
                logged_in,
                is_active,
                device_name,
            } => {
                let snap = {
                    let mut guard = self.inner.write().await;
                    if !device_name.trim().is_empty() {
                        guard.snapshot.device_name = device_name.clone();
                    }
                    let name = guard.snapshot.device_name.clone();
                    guard.snapshot.logged_in = logged_in;
                    guard.snapshot.is_active = is_active;
                    guard.snapshot.status = if logged_in {
                        SoloistStatus::Connected {
                            device_name: name,
                            is_active,
                            ws_port,
                        }
                    } else {
                        SoloistStatus::WaitingForPairing {
                            device_name: name,
                            ws_port,
                        }
                    };
                    guard.snapshot.clone()
                };
                let _ = self.events_tx.send(SoloistEvent::RuntimeChanged(snap));
                if logged_in {
                    let _ = self.request_state().await;
                    let _ = self.request_queue(50).await;
                }
            }
            SoloistWsEvent::PlaybackState(state) => {
                let is_playing = state.status.eq_ignore_ascii_case("playing");
                let dur = state
                    .item
                    .as_ref()
                    .and_then(|i| i.decorations.playback.as_ref())
                    .map(|p| p.duration_ms)
                    .unwrap_or(0);
                let pos_ms = state.position.estimated_ms_now();
                let snap_changed = {
                    let mut guard = self.inner.write().await;
                    guard.last_position = PlaybackPosition::new(pos_ms, dur, is_playing);
                    let prev_active = guard.snapshot.is_active;
                    guard.snapshot.logged_in = true;
                    guard.snapshot.is_active = state.is_active;
                    let name = guard.snapshot.device_name.clone();
                    guard.snapshot.status = SoloistStatus::Connected {
                        device_name: name,
                        is_active: state.is_active,
                        ws_port,
                    };
                    if prev_active != state.is_active {
                        Some(guard.snapshot.clone())
                    } else {
                        None
                    }
                };
                if let Some(snap) = snap_changed {
                    let _ = self.events_tx.send(SoloistEvent::RuntimeChanged(snap));
                }
                let _ = self.events_tx.send(SoloistEvent::PlaybackState(*state));
            }
            SoloistWsEvent::TrackChanged { item } => {
                let track = item.as_ref().and_then(SoloistEntity::into_track);
                if let Some(ref t) = track {
                    let mut guard = self.inner.write().await;
                    guard.last_position.duration_ms = t.duration_ms;
                }
                let _ = self.events_tx.send(SoloistEvent::TrackChanged(track));
                let _ = self.request_queue(50).await;
            }
            SoloistWsEvent::PlaybackChanged { status } => {
                {
                    let mut guard = self.inner.write().await;
                    if status.eq_ignore_ascii_case("playing") {
                        guard.last_position.is_playing = true;
                    } else if status.eq_ignore_ascii_case("paused")
                        || status.eq_ignore_ascii_case("idle")
                    {
                        guard.last_position.position_ms = guard.last_position.current_ms();
                        guard.last_position.is_playing = false;
                        guard.last_position.synced_at = Instant::now();
                    }
                }
                let _ = self
                    .events_tx
                    .send(SoloistEvent::PlaybackStatusChanged(status));
            }
            SoloistWsEvent::PositionSync { position } => {
                let est = position.estimated_ms_now();
                {
                    let mut guard = self.inner.write().await;
                    guard.last_position.position_ms = est;
                    guard.last_position.is_playing = position.speed > 0.0;
                    guard.last_position.playback_speed = if position.speed > 0.0 {
                        position.speed
                    } else {
                        1.0
                    };
                    guard.last_position.synced_at = Instant::now();
                }
                let _ = self.events_tx.send(SoloistEvent::PositionSync(position));
            }
            SoloistWsEvent::VolumeChanged { volume } => {
                let _ = self.events_tx.send(SoloistEvent::VolumeChanged(volume));
            }
            SoloistWsEvent::DeviceChanged {
                is_active,
                device_name,
            } => {
                let snap = {
                    let mut guard = self.inner.write().await;
                    if !device_name.trim().is_empty() {
                        guard.snapshot.device_name = device_name.clone();
                    }
                    guard.snapshot.is_active = is_active;
                    if guard.snapshot.logged_in {
                        let name = guard.snapshot.device_name.clone();
                        guard.snapshot.status = SoloistStatus::Connected {
                            device_name: name,
                            is_active,
                            ws_port,
                        };
                    }
                    guard.snapshot.clone()
                };
                let _ = self.events_tx.send(SoloistEvent::RuntimeChanged(snap));
                let _ = self.events_tx.send(SoloistEvent::DeviceChanged {
                    is_active,
                    device_name,
                });
            }
            SoloistWsEvent::OptionsChanged { options } => {
                let _ = self.events_tx.send(SoloistEvent::OptionsChanged(options));
            }
            SoloistWsEvent::QueueChanged { upcoming, .. } => {
                let tracks: Vec<Track> = upcoming
                    .iter()
                    .filter_map(|e| e.item.into_track())
                    .collect();
                let _ = self
                    .events_tx
                    .send(SoloistEvent::QueueChanged { upcoming: tracks });
            }
            SoloistWsEvent::ContextChanged { .. } | SoloistWsEvent::CommandResult { .. } => {}
            SoloistWsEvent::Error { message } => {
                tracing::warn!(soloist_error = %message, "Spotify Soloist returned WebSocket error");
                let _ = self.events_tx.send(SoloistEvent::ApiError(message));
            }
        }
    }

    async fn send_command_frame(
        &self,
        frame: SoloistWsCommandFrame,
        require_auth: bool,
    ) -> Result<(), AudioError> {
        let tx = {
            let guard = self.inner.read().await;
            if require_auth && !guard.snapshot.logged_in {
                let name = &guard.snapshot.device_name;
                return Err(match &guard.snapshot.status {
                    SoloistStatus::WaitingForPairing { .. } => AudioError::CommandFailed(format!(
                        "Spotify Soloist is waiting to pair: select '{name}' ONCE in the Spotify app \
                         (phone or desktop on the same Wi-Fi), then you can close the official Spotify app."
                    )),
                    SoloistStatus::Expired { message } => AudioError::BackendUnavailable(message.clone()),
                    SoloistStatus::Error { message } => AudioError::BackendUnavailable(message.clone()),
                    _ => AudioError::BackendUnavailable(
                        "Spotify Soloist is not running. Configure your Soloist API key and start Soloist in Settings."
                            .to_string(),
                    ),
                });
            }
            guard.ws_tx.clone().ok_or_else(|| {
                AudioError::BackendUnavailable(
                    "Spotify Soloist WebSocket is not connected. Start Soloist in Settings."
                        .to_string(),
                )
            })?
        };

        tx.send(frame.to_json()).map_err(|_| {
            AudioError::BackendUnavailable("Spotify Soloist WebSocket channel closed".to_string())
        })
    }

    pub async fn request_auth_state(&self) -> Result<(), AudioError> {
        self.send_command_frame(SoloistWsCommandFrame::simple("get_auth_state"), false)
            .await
    }

    pub async fn request_state(&self) -> Result<(), AudioError> {
        self.send_command_frame(SoloistWsCommandFrame::simple("get_state"), true)
            .await
    }

    pub async fn request_queue(&self, limit: u32) -> Result<(), AudioError> {
        let mut frame = SoloistWsCommandFrame::simple("get_queue");
        frame.limit = Some(limit);
        self.send_command_frame(frame, true).await
    }

    pub async fn activate(&self) -> Result<(), AudioError> {
        self.send_command_frame(SoloistWsCommandFrame::simple("activate"), true)
            .await
    }

    pub async fn set_shuffle(&self, enabled: bool) -> Result<(), AudioError> {
        let mut frame = SoloistWsCommandFrame::simple("set_shuffle");
        frame.enabled = Some(enabled);
        self.send_command_frame(frame, true).await
    }

    /// Coordinate Soloist's two repeat commands exactly as documented in the official WebSocket spec.
    pub async fn set_repeat(&self, repeat: RepeatMode) -> Result<(), AudioError> {
        let (first_cmd, first_val, second_cmd, second_val) = match repeat {
            RepeatMode::Off => ("set_repeat_track", false, "set_repeat_context", false),
            RepeatMode::Context => ("set_repeat_track", false, "set_repeat_context", true),
            RepeatMode::Track => ("set_repeat_context", false, "set_repeat_track", true),
        };
        let mut f1 = SoloistWsCommandFrame::simple(first_cmd);
        f1.enabled = Some(first_val);
        self.send_command_frame(f1, true).await?;

        let mut f2 = SoloistWsCommandFrame::simple(second_cmd);
        f2.enabled = Some(second_val);
        self.send_command_frame(f2, true).await
    }

    pub async fn add_to_queue(&self, track_uri: &str) -> Result<(), AudioError> {
        let uri = normalize_spotify_track_uri(track_uri);
        let mut frame = SoloistWsCommandFrame::simple("add_to_queue");
        frame.uri = Some(uri);
        self.send_command_frame(frame, true).await
    }
}

#[async_trait]
impl AudioEngine for SoloistEngine {
    async fn play(&self, track_id: &str) -> Result<(), AudioError> {
        let uri = normalize_spotify_track_uri(track_id);
        if !self.is_active_device().await {
            let _ = self.activate().await;
        }
        let mut frame = SoloistWsCommandFrame::simple("play");
        frame.uri = Some(uri);
        self.send_command_frame(frame, true).await
    }

    async fn pause(&self) -> Result<(), AudioError> {
        self.send_command_frame(SoloistWsCommandFrame::simple("pause"), true)
            .await
    }

    async fn resume(&self) -> Result<(), AudioError> {
        if !self.is_active_device().await {
            let _ = self.activate().await;
        }
        self.send_command_frame(SoloistWsCommandFrame::simple("play"), true)
            .await
    }

    async fn seek(&self, position_ms: u64) -> Result<(), AudioError> {
        let mut frame = SoloistWsCommandFrame::simple("seek");
        frame.position_ms = Some(position_ms);
        self.send_command_frame(frame, true).await
    }

    async fn set_volume(&self, volume: f32) -> Result<(), AudioError> {
        let pct = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
        let mut frame = SoloistWsCommandFrame::simple("set_volume");
        frame.volume = Some(pct);
        self.send_command_frame(frame, true).await
    }

    async fn next(&self) -> Result<(), AudioError> {
        self.send_command_frame(SoloistWsCommandFrame::simple("skip_next"), true)
            .await
    }

    async fn previous(&self) -> Result<(), AudioError> {
        self.send_command_frame(SoloistWsCommandFrame::simple("skip_prev"), true)
            .await
    }

    async fn get_position(&self) -> PlaybackPosition {
        self.inner.read().await.last_position.clone()
    }
}

fn normalize_spotify_track_uri(track_id: &str) -> String {
    if track_id.starts_with("spotify:") {
        track_id.to_string()
    } else {
        format!("spotify:track:{track_id}")
    }
}

fn parse_ws_port(ws_url: &str) -> Option<u16> {
    url::Url::parse(ws_url).ok()?.port()
}

fn redact_secret(input: &str, secret: &str) -> String {
    if secret.is_empty() {
        input.to_string()
    } else {
        input.replace(secret, "[REDACTED]")
    }
}

async fn check_child_exited(
    child_slot: &Arc<Mutex<Option<Child>>>,
    stderr_lines: &Arc<Mutex<VecDeque<String>>>,
) -> Option<SoloistStatus> {
    let exit_res = {
        let mut guard = child_slot.lock().await;
        let child = guard.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => {
                guard.take();
                Some(status)
            }
            Ok(None) => None,
            Err(_) => {
                guard.take();
                None
            }
        }
    };

    let status = exit_res?;
    let code = status.code();
    let tail = format_stderr_tail(stderr_lines).await;

    if code == Some(SOLOIST_EXPIRED_EXIT_CODE) {
        return Some(SoloistStatus::Expired {
            message: "Spotify Soloist build expired (90-day build lifetime, exit code 10). \
                      Click 'Download / Update Soloist' in Settings to install the latest official build."
                .to_string(),
        });
    }

    let detail = if tail.is_empty() {
        format!(
            "Spotify Soloist daemon exited unexpectedly (code {:?}).",
            code
        )
    } else {
        format!("Spotify Soloist daemon exited (code {:?}): {tail}", code)
    };
    Some(SoloistStatus::Error { message: detail })
}

async fn format_stderr_tail(stderr_lines: &Arc<Mutex<VecDeque<String>>>) -> String {
    let q = stderr_lines.lock().await;
    q.iter()
        .rev()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(windows)]
fn configure_hidden_process(cmd: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_hidden_process(_cmd: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder};
    use std::io::Write;

    fn build_test_targz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        for (name, data) in entries {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..107].copy_from_slice(b"0000755");
            let size_oct = format!("{:011o}", data.len());
            header[124..135].copy_from_slice(size_oct.as_bytes());
            header[156] = b'0';
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");
            // Checksum field filled with spaces before summing
            header[148..156].fill(b' ');
            let cksum: u32 = header.iter().map(|&b| u32::from(b)).sum();
            let cksum_str = format!("{cksum:06o}\0 ");
            header[148..156].copy_from_slice(cksum_str.as_bytes());

            tar_bytes.extend_from_slice(&header);
            tar_bytes.extend_from_slice(data);
            let pad = (512 - (data.len() % 512)) % 512;
            tar_bytes.extend(std::iter::repeat_n(0u8, pad));
        }
        // End-of-archive zero blocks
        tar_bytes.extend(std::iter::repeat_n(0u8, 1024));

        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&tar_bytes).unwrap();
        enc.finish().unwrap()
    }

    fn build_test_zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();

        for (name, data, deflate) in entries {
            let local_offset = out.len() as u32;
            let (method, compressed) = if *deflate {
                let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
                enc.write_all(data).unwrap();
                (8u16, enc.finish().unwrap())
            } else {
                (0u16, data.to_vec())
            };

            // Local file header
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes()); // crc unused by extractor
            out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&compressed);

            // Central directory header
            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&local_offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }

        let cd_offset = out.len() as u32;
        let cd_size = central.len() as u32;
        out.extend_from_slice(&central);

        // End of central directory
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[test]
    fn platform_archive_specs_match_official_spotify_cdn() {
        let lin = archive_spec_for("linux", "x86_64").unwrap();
        assert_eq!(
            lin.url,
            "https://soloist-builds.spotifycdn.com/soloist_release_x86_64.tar.gz"
        );
        assert_eq!(lin.executable_name, "soloist");

        let mac = archive_spec_for("macos", "aarch64").unwrap();
        assert_eq!(
            mac.url,
            "https://soloist-builds.spotifycdn.com/soloist_release_osx_arm64.tar.gz"
        );

        let win = archive_spec_for("windows", "x86_64").unwrap();
        assert_eq!(
            win.url,
            "https://soloist-builds.spotifycdn.com/soloist_release_windows_x86_64.zip"
        );
        assert_eq!(win.executable_name, "soloist.exe");

        assert!(archive_spec_for("macos", "x86_64").is_err());
    }

    #[test]
    fn extracts_soloist_from_targz_and_zip_ignoring_docs_and_rejecting_traversal() {
        let fake_elf = b"\x7fELFsoloist-linux-payload-12345";
        let tgz = build_test_targz(&[
            ("CHANGELOG.md", b"# Changelog\n"),
            ("LICENSE", b"Proprietary"),
            ("../soloist", b"evil-traversal"),
            ("release/soloist", fake_elf),
        ]);
        let extracted = extract_binary_from_targz(&tgz, "soloist").unwrap();
        assert_eq!(extracted, fake_elf);
        assert!(validate_executable_magic(&extracted, "linux").is_ok());

        let fake_pe = b"MZ\x90\x00soloist-windows-payload-99999";
        let zip = build_test_zip(&[
            ("CHANGELOG.md", b"# Changelog", true),
            ("THIRD_PARTY_LICENSES.txt", b"MIT", false),
            ("soloist/soloist.exe", fake_pe, true),
        ]);
        let extracted_win = extract_binary_from_zip(&zip, "soloist.exe").unwrap();
        assert_eq!(extracted_win, fake_pe);
        assert!(validate_executable_magic(&extracted_win, "windows").is_ok());
    }

    #[test]
    fn parses_official_soloist_ws_events_and_rejects_clock_skew() {
        let raw_state = r#"{
            "type": "playback_state",
            "status": "playing",
            "item": {
                "uri": "spotify:track:2JRo0gjbX4GrCqBYdRohoo",
                "entity_type": "track",
                "decorations": {
                    "identity": { "name": "Midnight Signal" },
                    "visual_identity": {
                        "cover": [
                            { "url": "https://i.scdn.co/small", "size": "small" },
                            { "url": "https://i.scdn.co/large", "size": "large" }
                        ]
                    },
                    "parent": {
                        "entity": {
                            "uri": "spotify:album:alb99",
                            "entity_type": "album",
                            "decorations": { "identity": { "name": "Night Drive" } }
                        }
                    },
                    "creators": [
                        {
                            "entity": {
                                "uri": "spotify:artist:art1",
                                "entity_type": "artist",
                                "decorations": { "identity": { "name": "Rust Wave" } }
                            }
                        }
                    ],
                    "playback": { "duration_ms": 210000, "content_ratings": ["explicit"] }
                }
            },
            "position": { "position_ms": 45000, "timestamp_ms": 1747654321000, "speed": 1.0 },
            "volume": 68,
            "is_active": true,
            "options": { "shuffle": true, "repeat": "context", "playback_speed": 1.0 }
        }"#;

        let evt: SoloistWsEvent = serde_json::from_str(raw_state).unwrap();
        match evt {
            SoloistWsEvent::PlaybackState(st) => {
                assert_eq!(st.status, "playing");
                assert_eq!(st.volume, 68);
                assert!(st.is_active);
                assert!(st.options.shuffle);
                assert_eq!(st.options.repeat, "context");
                let track = st.item.unwrap().into_track().unwrap();
                assert_eq!(track.id, "2JRo0gjbX4GrCqBYdRohoo");
                assert_eq!(track.title, "Midnight Signal");
                assert_eq!(track.artists, vec!["Rust Wave"]);
                assert_eq!(track.album_name, "Night Drive");
                assert_eq!(
                    track.artwork_url.as_deref(),
                    Some("https://i.scdn.co/large")
                );
                assert_eq!(track.duration_ms, 210_000);
                assert!(track.explicit);

                // Within 2s -> interpolates cleanly
                assert_eq!(st.position.estimated_ms_at(1_747_654_321_500), 45_500);
                // Implausible >2s clock skew -> clamps safely to position_ms
                assert_eq!(st.position.estimated_ms_at(1_747_654_335_000), 45_000);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
