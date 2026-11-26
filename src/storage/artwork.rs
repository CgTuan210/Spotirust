//! Asynchronous album artwork downloader, bounded LRU disk cache, RGBA decoder,
//! embedded local artwork persistence (`localart://`), and dominant color extractor.
//!
//! Runs strictly on background Tokio / blocking worker threads so the egui UI thread never stalls.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Semaphore;

/// Default maximum total bytes stored in the on-disk artwork cache (64 MiB).
pub const DEFAULT_MAX_DISK_BYTES: u64 = 64 * 1024 * 1024;
/// Default maximum number of cached artwork files on disk.
pub const DEFAULT_MAX_DISK_FILES: usize = 512;

/// Decoded artwork image ready to upload as a GPU texture on the UI thread.
#[derive(Debug, Clone)]
pub struct DecodedArtwork {
    pub url: String,
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub dominant_rgb: [u8; 3],
}

/// Summary metrics for the on-disk artwork cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskCacheStats {
    pub file_count: usize,
    pub total_bytes: u64,
}

/// Bounded asynchronous artwork fetcher and on-disk LRU cache.
#[derive(Clone)]
pub struct ArtworkService {
    http: reqwest::Client,
    cache_dir: PathBuf,
    max_disk_bytes: u64,
    max_disk_files: usize,
    /// Limits concurrent downloads/decodes to protect memory & bandwidth.
    concurrency: Arc<Semaphore>,
}

impl ArtworkService {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self::with_limits(cache_dir, DEFAULT_MAX_DISK_BYTES, DEFAULT_MAX_DISK_FILES)
    }

    pub fn with_limits(cache_dir: PathBuf, max_disk_bytes: u64, max_disk_files: usize) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "Spotirust/{} (https://github.com/CgTuan210/Spotirust)",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            http,
            cache_dir,
            max_disk_bytes: max_disk_bytes.max(1024),
            max_disk_files: max_disk_files.max(1),
            concurrency: Arc::new(Semaphore::new(4)),
        }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Fetch artwork from local embedded cache (`localart://`), disk cache, or HTTPS,
    /// decode on a blocking worker, enforce bounded disk size, and extract dominant RGB.
    pub async fn fetch_and_decode(&self, url: &str) -> anyhow::Result<DecodedArtwork> {
        let _permit = self.concurrency.acquire().await?;

        if let Some(hex) = url.strip_prefix("localart://") {
            let local_file = self.local_art_path(hex);
            let bytes = tokio::fs::read(&local_file).await?;
            let url_owned = url.to_string();
            return tokio::task::spawn_blocking(move || decode_and_sample(&url_owned, &bytes))
                .await?;
        }

        let cache_file = self.cache_path_for(url);
        let mut from_cache = false;

        let bytes = if cache_file.exists() {
            from_cache = true;
            tokio::fs::read(&cache_file).await?
        } else {
            let response = self.http.get(url).send().await?.error_for_status()?;
            let data = response.bytes().await?.to_vec();
            self.write_cache_file(&cache_file, &data).await;
            data
        };

        let url_owned = url.to_string();
        let decoded =
            tokio::task::spawn_blocking(move || decode_and_sample(&url_owned, &bytes)).await?;

        match decoded {
            Ok(img) => {
                if !from_cache {
                    let dir = self.cache_dir.clone();
                    let max_bytes = self.max_disk_bytes;
                    let max_files = self.max_disk_files;
                    let protect = cache_file.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let _ = evict_disk_cache(&dir, max_bytes, max_files, Some(&protect));
                    })
                    .await;
                }
                Ok(img)
            }
            Err(err) => {
                // Purge corrupt cached file so a subsequent retry can heal cleanly.
                if from_cache {
                    let _ = tokio::fs::remove_file(&cache_file).await;
                }
                Err(err)
            }
        }
    }

    async fn write_cache_file(&self, cache_file: &Path, data: &[u8]) {
        if let Some(parent) = cache_file.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        // Atomic write via unique sibling temp file so concurrent readers never observe partial bytes.
        let tmp_name = format!(
            ".{}.tmp",
            cache_file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("art")
        );
        let tmp_path = cache_file.with_file_name(tmp_name);
        if tokio::fs::write(&tmp_path, data).await.is_ok() {
            let _ = tokio::fs::rename(&tmp_path, cache_file).await;
        }
    }

    /// Enforce disk cache byte & file-count limits immediately (oldest files evicted first).
    pub fn enforce_disk_limits(&self) -> anyhow::Result<usize> {
        evict_disk_cache(
            &self.cache_dir,
            self.max_disk_bytes,
            self.max_disk_files,
            None,
        )
    }

    pub fn disk_stats(&self) -> DiskCacheStats {
        disk_cache_stats(&self.cache_dir)
    }

    /// Remove cached remote artwork files while preserving local embedded `localart_*` thumbnails.
    pub fn clear_remote_disk_cache(&self) -> anyhow::Result<usize> {
        if !self.cache_dir.exists() {
            return Ok(0);
        }
        let mut removed = 0;
        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            let is_local = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("localart_"))
                .unwrap_or(false);
            if path.is_file() && !is_local && std::fs::remove_file(path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn clear_disk_cache(&self) -> anyhow::Result<usize> {
        if !self.cache_dir.exists() {
            return Ok(0);
        }
        let mut removed = 0;
        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            if entry.path().is_file() && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn cache_path_for(&self, url: &str) -> PathBuf {
        let hex = sha256_hex(url.as_bytes());
        self.cache_dir.join(format!("{hex}.img"))
    }

    fn local_art_path(&self, hex: &str) -> PathBuf {
        let clean: String = hex
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .take(64)
            .collect();
        self.cache_dir.join(format!("localart_{clean}.img"))
    }
}

/// Store embedded album artwork bytes extracted from a local audio tag (`lofty`) into
/// `cache_dir` using content-addressed SHA-256 deduplication and return a `localart://<hex>` URI.
/// Returns `None` if `raw_bytes` is empty or not a recognized image format.
pub fn store_embedded_artwork(cache_dir: &Path, raw_bytes: &[u8]) -> Option<String> {
    if raw_bytes.is_empty() || image::guess_format(raw_bytes).is_err() {
        return None;
    }
    let hex = sha256_hex(raw_bytes);
    let target = cache_dir.join(format!("localart_{hex}.img"));
    if !target.is_file() {
        if std::fs::create_dir_all(cache_dir).is_err() {
            return None;
        }
        let tmp = cache_dir.join(format!(".localart_{hex}.tmp"));
        if std::fs::write(&tmp, raw_bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &target);
        } else {
            return None;
        }
    }
    Some(format!("localart://{hex}"))
}

/// Check whether a `localart://<hex>` file still exists inside `cache_dir`.
pub fn local_artwork_exists(cache_dir: &Path, artwork_url: &str) -> bool {
    let Some(hex) = artwork_url.strip_prefix("localart://") else {
        return true;
    };
    let clean: String = hex
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(64)
        .collect();
    cache_dir.join(format!("localart_{clean}.img")).is_file()
}

/// Inspect file count and total byte usage in the artwork cache directory.
pub fn disk_cache_stats(cache_dir: &Path) -> DiskCacheStats {
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return DiskCacheStats::default();
    };
    let mut stats = DiskCacheStats::default();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path.extension().and_then(|e| e.to_str()) == Some("img")
            && let Ok(meta) = entry.metadata()
        {
            stats.file_count += 1;
            stats.total_bytes += meta.len();
        }
    }
    stats
}

/// Evict oldest cached `.img` files from `cache_dir` until both `max_bytes` and `max_files`
/// are satisfied. Returns the number of evicted files.
pub fn evict_disk_cache(
    cache_dir: &Path,
    max_bytes: u64,
    max_files: usize,
    protect: Option<&Path>,
) -> anyhow::Result<usize> {
    if !cache_dir.is_dir() {
        return Ok(0);
    }

    struct EntryInfo {
        path: PathBuf,
        size: u64,
        mtime: SystemTime,
        is_local_art: bool,
    }

    let mut files = Vec::new();
    let mut total_bytes = 0u64;

    for entry in std::fs::read_dir(cache_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("img") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let size = meta.len();
        total_bytes += size;
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let is_local_art = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("localart_"))
            .unwrap_or(false);
        files.push(EntryInfo {
            path,
            size,
            mtime,
            is_local_art,
        });
    }

    if total_bytes <= max_bytes && files.len() <= max_files {
        return Ok(0);
    }

    // Evict remote cache entries before local embedded artwork, oldest mtime first within each tier.
    files.sort_by(|a, b| {
        a.is_local_art
            .cmp(&b.is_local_art)
            .then(a.mtime.cmp(&b.mtime))
    });

    let mut remaining_files = files.len();
    let mut evicted = 0usize;

    for info in files {
        if total_bytes <= max_bytes && remaining_files <= max_files {
            break;
        }
        if protect.is_some_and(|p| p == info.path.as_path()) {
            continue;
        }
        if std::fs::remove_file(&info.path).is_ok() {
            total_bytes = total_bytes.saturating_sub(info.size);
            remaining_files = remaining_files.saturating_sub(1);
            evicted += 1;
        }
    }

    Ok(evicted)
}

fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.as_slice() {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// Decode raw image bytes into a bounded RGBA8 buffer (max 512px) and sample dominant color.
pub fn decode_and_sample(url: &str, bytes: &[u8]) -> anyhow::Result<DecodedArtwork> {
    let img = image::load_from_memory(bytes)?;
    let resized = if img.width() > 512 || img.height() > 512 {
        img.thumbnail(512, 512)
    } else {
        img
    };
    let rgba_img = resized.to_rgba8();
    let width = rgba_img.width() as usize;
    let height = rgba_img.height() as usize;
    let dominant_rgb = extract_dominant_rgb(rgba_img.as_raw());

    Ok(DecodedArtwork {
        url: url.to_string(),
        width,
        height,
        rgba: rgba_img.into_raw(),
        dominant_rgb,
    })
}

/// Extracts a pleasant, saturation-weighted dominant RGB color from RGBA8 pixels
/// for the Now Playing background gradient.
pub fn extract_dominant_rgb(rgba: &[u8]) -> [u8; 3] {
    let mut r_acc: f64 = 0.0;
    let mut g_acc: f64 = 0.0;
    let mut b_acc: f64 = 0.0;
    let mut weight_sum: f64 = 0.0;

    // Sample every 16th pixel for negligible CPU overhead
    for chunk in rgba.as_chunks::<4>().0.iter().step_by(16) {
        if chunk[3] < 128 {
            continue;
        }
        let r = f64::from(chunk[0]);
        let g = f64::from(chunk[1]);
        let b = f64::from(chunk[2]);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        // Favor rich mid-tone hues over pure black/white borders
        let luma_weight = if !(22.0..=235.0).contains(&luma) {
            0.15
        } else {
            1.0
        };
        let w = (0.25 + sat * 2.2) * luma_weight;
        r_acc += r * w;
        g_acc += g * w;
        b_acc += b * w;
        weight_sum += w;
    }

    if weight_sum <= 0.0 {
        return [29, 185, 84];
    }

    [
        (r_acc / weight_sum).clamp(0.0, 255.0) as u8,
        (g_acc / weight_sum).clamp(0.0, 255.0) as u8,
        (b_acc / weight_sum).clamp(0.0, 255.0) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tiny_png(rgb: [u8; 3]) -> Vec<u8> {
        let mut img = image::RgbaImage::new(4, 4);
        for pixel in img.pixels_mut() {
            *pixel = image::Rgba([rgb[0], rgb[1], rgb[2], 255]);
        }
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[tokio::test]
    async fn stores_and_decodes_local_embedded_artwork() {
        let dir = tempfile::tempdir().unwrap();
        let png = make_tiny_png([30, 215, 96]);
        let uri = store_embedded_artwork(dir.path(), &png).expect("valid png should store");
        assert!(uri.starts_with("localart://"));
        assert!(local_artwork_exists(dir.path(), &uri));

        let svc = ArtworkService::new(dir.path().to_path_buf());
        let decoded = svc.fetch_and_decode(&uri).await.unwrap();
        assert_eq!(decoded.width, 4);
        assert_eq!(decoded.height, 4);
        assert_eq!(decoded.dominant_rgb, [30, 215, 96]);
    }

    #[test]
    fn disk_cache_eviction_enforces_file_and_byte_caps() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..6 {
            let p = dir.path().join(format!("item_{i:02}.img"));
            std::fs::write(&p, vec![b'x'; 2048]).unwrap();
            // Ensure deterministic mtime progression on filesystems with coarse timestamps
            std::thread::sleep(std::time::Duration::from_millis(12));
        }

        let evicted = evict_disk_cache(dir.path(), 5000, 3, None).unwrap();
        assert_eq!(
            evicted, 4,
            "5000 bytes / 2048 per file allows at most 2 files"
        );
        let stats = disk_cache_stats(dir.path());
        assert_eq!(stats.file_count, 2);
        assert!(stats.total_bytes <= 5000);
    }
}
