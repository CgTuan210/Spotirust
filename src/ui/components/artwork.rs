//! Bounded LRU GPU texture cache, rounded artwork painter, procedural placeholder covers,
//! and optimized opaque album-colored ambient background gradient renderer.

use crate::core::events::AppCommand;
use egui::{
    Color32, ColorImage, CornerRadius, Pos2, Rect, Shape, Stroke, StrokeKind, TextureHandle,
    TextureOptions, Ui, Vec2,
};
use std::collections::{HashMap, HashSet, VecDeque};

/// Maximum decoded GPU textures held in RAM at once (~96 × 512×512×4B max, typically much smaller).
pub const MAX_GPU_TEXTURES: usize = 96;
const MAX_FAILED_URLS: usize = 256;

/// UI-thread bounded LRU GPU texture cache for album artwork.
pub struct ArtworkTextureCache {
    textures: HashMap<String, (TextureHandle, [u8; 3])>,
    order: VecDeque<String>,
    in_flight: HashSet<String>,
    failed: HashSet<String>,
    failed_order: VecDeque<String>,
    max_textures: usize,
    evicted_count: usize,
}

impl Default for ArtworkTextureCache {
    fn default() -> Self {
        Self::with_capacity(MAX_GPU_TEXTURES)
    }
}

impl ArtworkTextureCache {
    pub fn with_capacity(max_textures: usize) -> Self {
        Self {
            textures: HashMap::new(),
            order: VecDeque::new(),
            in_flight: HashSet::new(),
            failed: HashSet::new(),
            failed_order: VecDeque::new(),
            max_textures: max_textures.max(1),
            evicted_count: 0,
        }
    }

    pub fn insert(
        &mut self,
        ctx: &egui::Context,
        url: String,
        width: usize,
        height: usize,
        rgba: Vec<u8>,
        dominant_rgb: [u8; 3],
    ) {
        self.in_flight.remove(&url);
        if self.failed.remove(&url) {
            self.failed_order.retain(|u| u != &url);
        }

        let image = ColorImage::from_rgba_unmultiplied([width, height], &rgba);
        let handle = ctx.load_texture(url.clone(), image, TextureOptions::LINEAR);

        // Remove any prior occurrence in LRU order so keys remain unique
        if self.textures.contains_key(&url) {
            self.order.retain(|u| u != &url);
        }

        // Evict least-recently-used textures until strictly below capacity
        while self.textures.len() >= self.max_textures {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if self.textures.remove(&oldest).is_some() {
                self.evicted_count += 1;
            }
        }

        self.order.push_back(url.clone());
        self.textures.insert(url, (handle, dominant_rgb));
    }

    /// Clear `in_flight` on download/decode failure and remember the URL in a bounded ring
    /// to avoid retry loops without clearing all failed entries at once.
    pub fn mark_failed(&mut self, url: &str) {
        self.in_flight.remove(url);
        if self.failed.contains(url) {
            return;
        }
        while self.failed.len() >= MAX_FAILED_URLS {
            if let Some(oldest) = self.failed_order.pop_front() {
                self.failed.remove(&oldest);
            } else {
                break;
            }
        }
        let owned = url.to_string();
        self.failed_order.push_back(owned.clone());
        self.failed.insert(owned);
    }

    pub fn clear(&mut self) {
        self.textures.clear();
        self.order.clear();
        self.in_flight.clear();
        self.failed.clear();
        self.failed_order.clear();
    }

    /// True LRU lookup: promotes `url` to most-recently-used on hit.
    pub fn get(&mut self, url: &str) -> Option<&TextureHandle> {
        if self.textures.contains_key(url) {
            self.touch(url);
            self.textures.get(url).map(|(h, _)| h)
        } else {
            None
        }
    }

    /// Read-only dominant RGB lookup (does not disturb LRU order).
    pub fn dominant_color(&self, url: &str) -> Option<[u8; 3]> {
        self.textures.get(url).map(|(_, rgb)| *rgb)
    }

    pub fn len(&self) -> usize {
        self.textures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.max_textures
    }

    pub fn evicted_count(&self) -> usize {
        self.evicted_count
    }

    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    pub fn failed_len(&self) -> usize {
        self.failed.len()
    }

    pub fn contains(&self, url: &str) -> bool {
        self.textures.contains_key(url)
    }

    fn touch(&mut self, url: &str) {
        if self.order.back().map(String::as_str) == Some(url) {
            return;
        }
        if let Some(pos) = self.order.iter().position(|u| u == url)
            && let Some(key) = self.order.remove(pos)
        {
            self.order.push_back(key);
        }
    }

    /// Ensure an async download/decode command is queued once if the artwork isn't loaded yet.
    pub fn request_if_needed(&mut self, url: &str, commands: &mut Vec<AppCommand>) {
        if self.textures.contains_key(url)
            || self.in_flight.contains(url)
            || self.failed.contains(url)
        {
            return;
        }
        self.in_flight.insert(url.to_string());
        commands.push(AppCommand::RequestArtwork {
            url: url.to_string(),
        });
    }
}

/// Paint album artwork inside `rect` with smooth rounded corners, falling back to an
/// original procedural gradient cover when offline or loading.
#[allow(clippy::too_many_arguments)]
pub fn paint_artwork(
    ui: &mut Ui,
    rect: Rect,
    rounding: u8,
    url: Option<&str>,
    fallback_seed: &str,
    accent_rgb: Option<[u8; 3]>,
    cache: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) {
    if let Some(u) = url {
        if let Some(handle) = cache.get(u) {
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            ui.painter().image(handle.id(), rect, uv, Color32::WHITE);
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(rounding),
                Stroke::new(1.0, Color32::from_white_alpha(18)),
                StrokeKind::Inside,
            );
            return;
        }
        cache.request_if_needed(u, commands);
    }

    // Original procedural cover artwork (zero copyrighted assets)
    let rgb = accent_rgb.unwrap_or_else(|| deterministic_rgb(fallback_seed));
    let base = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    let dark = Color32::from_rgb(
        (f32::from(rgb[0]) * 0.28) as u8,
        (f32::from(rgb[1]) * 0.28) as u8,
        (f32::from(rgb[2]) * 0.34) as u8,
    );

    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(rounding), dark);
    let glow_center = Pos2::new(
        rect.left() + rect.width() * 0.32,
        rect.top() + rect.height() * 0.30,
    );
    painter.circle_filled(
        glow_center,
        rect.width().min(rect.height()) * 0.44,
        base.linear_multiply(0.45),
    );
    painter.circle_filled(
        rect.center(),
        rect.width().min(rect.height()) * 0.22,
        Color32::from_black_alpha(65),
    );
    painter.circle_stroke(
        rect.center(),
        rect.width().min(rect.height()) * 0.22,
        Stroke::new(1.5, Color32::from_white_alpha(130)),
    );
    painter.circle_filled(
        rect.center(),
        rect.width().min(rect.height()) * 0.055,
        Color32::from_white_alpha(200),
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(rounding),
        Stroke::new(1.0, Color32::from_white_alpha(24)),
        StrokeKind::Inside,
    );
}

#[inline]
fn blend_opaque(base: Color32, tint: [u8; 3], alpha: f32) -> Color32 {
    let a = alpha.clamp(0.0, 1.0);
    let inv = 1.0 - a;
    Color32::from_rgb(
        (f32::from(base.r()) * inv + f32::from(tint[0]) * a).round() as u8,
        (f32::from(base.g()) * inv + f32::from(tint[1]) * a).round() as u8,
        (f32::from(base.b()) * inv + f32::from(tint[2]) * a).round() as u8,
    )
}

/// Paint a rich dynamic ambient gradient backdrop tinted by the album's dominant color.
///
/// Pre-blends gradient vertices directly against `base_bg` into a single **opaque** mesh so
/// both hardware GPUs and software rasterizers avoid redundant full-window alpha overdraw.
pub fn paint_ambient_gradient(
    ui: &Ui,
    rect: Rect,
    dominant_rgb: [u8; 3],
    base_bg: Color32,
    is_playing: bool,
    time_secs: f64,
    reduce_motion: bool,
) {
    let painter = ui.painter();

    let pulse = if is_playing && !reduce_motion {
        0.5 + 0.5 * (time_secs * 1.15).sin() as f32
    } else {
        0.25
    };

    let top_left = blend_opaque(base_bg, dominant_rgb, 0.34 + pulse * 0.09);
    let top_right = blend_opaque(base_bg, dominant_rgb, 0.22 + pulse * 0.06);
    let mid_tint = [
        (f32::from(dominant_rgb[0]) * 0.6) as u8,
        (f32::from(dominant_rgb[1]) * 0.6) as u8,
        (f32::from(dominant_rgb[2]) * 0.7) as u8,
    ];
    let mid_left = blend_opaque(base_bg, mid_tint, 0.18 + pulse * 0.05);
    let mid_right = blend_opaque(base_bg, mid_tint, 0.11 + pulse * 0.03);

    let mut mesh = egui::Mesh::default();
    let tl = rect.left_top();
    let tr = rect.right_top();
    let ml = Pos2::new(rect.left(), rect.top() + rect.height() * 0.52);
    let mr = Pos2::new(rect.right(), rect.top() + rect.height() * 0.52);
    let bl = rect.left_bottom();
    let br = rect.right_bottom();

    let v = |pos: Pos2, color: Color32| egui::epaint::Vertex {
        pos,
        uv: Pos2::ZERO,
        color,
    };

    mesh.vertices = vec![
        v(tl, top_left),
        v(tr, top_right),
        v(ml, mid_left),
        v(mr, mid_right),
        v(bl, base_bg),
        v(br, base_bg),
    ];
    mesh.indices = vec![0, 1, 2, 1, 3, 2, 2, 3, 4, 3, 5, 4];
    painter.add(Shape::mesh(mesh));
}

/// Allocate and paint a square artwork widget of `size` logical pixels.
#[allow(clippy::too_many_arguments)]
pub fn artwork_box(
    ui: &mut Ui,
    size: f32,
    rounding: u8,
    url: Option<&str>,
    seed: &str,
    accent_rgb: Option<[u8; 3]>,
    cache: &mut ArtworkTextureCache,
    commands: &mut Vec<AppCommand>,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::click());
    paint_artwork(ui, rect, rounding, url, seed, accent_rgb, cache, commands);
    response
}

pub fn deterministic_rgb(seed: &str) -> [u8; 3] {
    let mut hash: u32 = 2_166_136_261;
    for b in seed.as_bytes() {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(16_777_619);
    }
    let palette: [[u8; 3]; 8] = [
        [30, 215, 96],
        [56, 189, 248],
        [168, 85, 247],
        [244, 114, 182],
        [251, 146, 60],
        [52, 211, 153],
        [99, 102, 241],
        [234, 179, 8],
    ];
    palette[(hash as usize) % palette.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_texture_cache_enforces_true_lru_eviction_and_deduplicates_requests() {
        let ctx = egui::Context::default();
        let mut cache = ArtworkTextureCache::with_capacity(3);
        let rgba = vec![255u8; 2 * 2 * 4];

        let mut cmds = Vec::new();
        cache.request_if_needed("https://cdn.example/a.jpg", &mut cmds);
        cache.request_if_needed("https://cdn.example/a.jpg", &mut cmds);
        assert_eq!(
            cmds.len(),
            1,
            "duplicate in-flight requests must be coalesced"
        );

        cache.insert(&ctx, "a".to_string(), 2, 2, rgba.clone(), [10, 20, 30]);
        cache.insert(&ctx, "b".to_string(), 2, 2, rgba.clone(), [40, 50, 60]);
        cache.insert(&ctx, "c".to_string(), 2, 2, rgba.clone(), [70, 80, 90]);
        assert_eq!(cache.len(), 3);

        // Touch 'a' so 'b' becomes the least-recently-used entry
        assert!(cache.get("a").is_some());

        cache.insert(&ctx, "d".to_string(), 2, 2, rgba.clone(), [1, 2, 3]);
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.evicted_count(), 1);
        assert!(cache.contains("a"), "'a' was touched and must remain");
        assert!(!cache.contains("b"), "'b' was LRU and must be evicted");
        assert!(cache.contains("c"));
        assert!(cache.contains("d"));

        // Failed URL tracking suppresses infinite retry loops
        cache.mark_failed("https://cdn.example/broken.jpg");
        cmds.clear();
        cache.request_if_needed("https://cdn.example/broken.jpg", &mut cmds);
        assert!(cmds.is_empty());
    }
}
