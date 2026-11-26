//! Extensible theme engine with built-in dark/light presets and JSON custom theme loading.

use egui::{Color32, CornerRadius, Stroke, Style, Visuals};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// JSON-serializable theme manifest loadable from `assets/themes/` or the user's `themes/` dir.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeManifest {
    pub id: String,
    pub name: String,
    pub dark: bool,
    pub bg_base: String,
    pub bg_surface: String,
    pub bg_elevated: String,
    pub bg_hover: String,
    pub accent: String,
    pub accent_hover: String,
    pub text_primary: String,
    pub text_secondary: String,
    pub text_muted: String,
    pub border: String,
    pub danger: String,
    pub warning: String,
}

/// Resolved RGBA color palette used throughout the Spotirust UI.
#[derive(Debug, Clone)]
pub struct ThemePalette {
    pub id: String,
    pub name: String,
    pub dark: bool,
    pub bg_base: Color32,
    pub bg_surface: Color32,
    pub bg_elevated: Color32,
    pub bg_hover: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,
    pub border: Color32,
    pub danger: Color32,
    pub warning: Color32,
}

impl ThemePalette {
    /// Default Spotify-inspired dark palette with OLED-friendly neutral layering
    /// and WCAG AA (>= 4.5:1) text contrast across all tiers.
    pub fn obsidian_dark() -> Self {
        Self {
            id: "obsidian_dark".to_string(),
            name: "Obsidian Dark".to_string(),
            dark: true,
            bg_base: Color32::from_rgb(11, 12, 15),
            bg_surface: Color32::from_rgb(16, 18, 22),
            bg_elevated: Color32::from_rgb(24, 27, 33),
            bg_hover: Color32::from_rgb(35, 39, 48),
            accent: Color32::from_rgb(29, 214, 96),
            accent_hover: Color32::from_rgb(56, 232, 120),
            text_primary: Color32::from_rgb(245, 246, 248),
            text_secondary: Color32::from_rgb(168, 175, 187),
            text_muted: Color32::from_rgb(122, 130, 144),
            border: Color32::from_rgb(38, 43, 52),
            danger: Color32::from_rgb(240, 86, 86),
            warning: Color32::from_rgb(245, 180, 68),
        }
    }

    pub fn midnight_cobalt() -> Self {
        Self {
            id: "midnight_cobalt".to_string(),
            name: "Midnight Cobalt".to_string(),
            dark: true,
            bg_base: Color32::from_rgb(10, 14, 24),
            bg_surface: Color32::from_rgb(15, 21, 34),
            bg_elevated: Color32::from_rgb(23, 32, 50),
            bg_hover: Color32::from_rgb(33, 45, 69),
            accent: Color32::from_rgb(56, 189, 248),
            accent_hover: Color32::from_rgb(125, 211, 252),
            text_primary: Color32::from_rgb(242, 246, 252),
            text_secondary: Color32::from_rgb(168, 182, 204),
            text_muted: Color32::from_rgb(122, 138, 162),
            border: Color32::from_rgb(38, 52, 78),
            danger: Color32::from_rgb(248, 113, 113),
            warning: Color32::from_rgb(251, 191, 36),
        }
    }

    pub fn emerald_studio() -> Self {
        Self {
            id: "emerald_studio".to_string(),
            name: "Emerald Studio".to_string(),
            dark: true,
            bg_base: Color32::from_rgb(10, 15, 13),
            bg_surface: Color32::from_rgb(15, 22, 19),
            bg_elevated: Color32::from_rgb(22, 33, 29),
            bg_hover: Color32::from_rgb(32, 47, 41),
            accent: Color32::from_rgb(16, 185, 129),
            accent_hover: Color32::from_rgb(52, 211, 153),
            text_primary: Color32::from_rgb(238, 252, 246),
            text_secondary: Color32::from_rgb(165, 196, 184),
            text_muted: Color32::from_rgb(118, 148, 137),
            border: Color32::from_rgb(34, 52, 45),
            danger: Color32::from_rgb(248, 113, 113),
            warning: Color32::from_rgb(251, 191, 36),
        }
    }

    pub fn daylight_paper() -> Self {
        Self {
            id: "daylight_paper".to_string(),
            name: "Daylight Paper".to_string(),
            dark: false,
            bg_base: Color32::from_rgb(245, 246, 248),
            bg_surface: Color32::from_rgb(255, 255, 255),
            bg_elevated: Color32::from_rgb(237, 240, 245),
            bg_hover: Color32::from_rgb(226, 231, 239),
            accent: Color32::from_rgb(15, 126, 62),
            accent_hover: Color32::from_rgb(18, 146, 72),
            text_primary: Color32::from_rgb(17, 20, 26),
            text_secondary: Color32::from_rgb(68, 77, 92),
            text_muted: Color32::from_rgb(95, 105, 120),
            border: Color32::from_rgb(218, 224, 233),
            danger: Color32::from_rgb(217, 45, 32),
            warning: Color32::from_rgb(194, 104, 6),
        }
    }

    /// Subtle translucent 1px structural hairline separator (never harsh).
    #[inline]
    pub fn hairline(&self) -> Color32 {
        if self.dark {
            Color32::from_white_alpha(18)
        } else {
            Color32::from_black_alpha(22)
        }
    }

    /// Subtle edge sheen for elevated cards and artwork frames.
    #[inline]
    pub fn card_stroke(&self) -> Color32 {
        if self.dark {
            Color32::from_white_alpha(15)
        } else {
            Color32::from_black_alpha(18)
        }
    }

    /// Soft translucent hover surface for borderless rows and ghost buttons.
    #[inline]
    pub fn hover_overlay(&self) -> Color32 {
        if self.dark {
            Color32::from_white_alpha(14)
        } else {
            Color32::from_black_alpha(11)
        }
    }

    /// Subtle pressed/active fill for selected navigation rows and pills.
    #[inline]
    pub fn active_pill_fill(&self) -> Color32 {
        if self.dark {
            Color32::from_white_alpha(20)
        } else {
            Color32::from_black_alpha(16)
        }
    }

    /// Soft translucent accent wash for active track rows and subtle badges.
    #[inline]
    pub fn accent_soft(&self) -> Color32 {
        let a = if self.dark { 28 } else { 24 };
        Color32::from_rgba_unmultiplied(self.accent.r(), self.accent.g(), self.accent.b(), a)
    }

    /// High-contrast foreground color (ink or white) chosen by WCAG relative luminance.
    #[inline]
    pub fn on_accent(&self) -> Color32 {
        let lin = |v: u8| {
            let s = f32::from(v) / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        let l = 0.2126 * lin(self.accent.r())
            + 0.7152 * lin(self.accent.g())
            + 0.0722 * lin(self.accent.b());
        let contrast_white = 1.05 / (l + 0.05);
        let contrast_ink = (l + 0.05) / 0.054;
        if contrast_ink > contrast_white {
            Color32::from_rgb(10, 12, 15)
        } else {
            Color32::WHITE
        }
    }

    /// Restrained high-contrast primary transport button background (white in dark mode, ink in light).
    #[inline]
    pub fn play_circle_bg(&self) -> Color32 {
        if self.dark {
            Color32::from_rgb(245, 246, 248)
        } else {
            Color32::from_rgb(20, 23, 29)
        }
    }

    /// Foreground glyph color inside `play_circle_bg()`.
    #[inline]
    pub fn play_circle_fg(&self) -> Color32 {
        if self.dark {
            Color32::from_rgb(11, 12, 15)
        } else {
            Color32::WHITE
        }
    }

    /// Clearly legible unfilled rail for seek and volume scrubbers.
    #[inline]
    pub fn track_rail(&self) -> Color32 {
        if self.dark {
            Color32::from_white_alpha(38)
        } else {
            Color32::from_black_alpha(32)
        }
    }

    /// Gentle elevation shadow for large artwork and floating controls.
    #[inline]
    pub fn shadow_floating(&self) -> egui::epaint::Shadow {
        egui::epaint::Shadow {
            offset: [0, 6],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(if self.dark { 95 } else { 38 }),
        }
    }

    /// Subtle soft shadow for media cards and compact pills.
    #[inline]
    pub fn shadow_soft(&self) -> egui::epaint::Shadow {
        egui::epaint::Shadow {
            offset: [0, 3],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(if self.dark { 65 } else { 20 }),
        }
    }

    /// Frosted Liquid Glass base fill for floating docks, sidebars, and utility sheets.
    #[inline]
    pub fn glass_fill(&self) -> Color32 {
        if self.dark {
            Color32::from_rgba_unmultiplied(
                self.bg_surface.r().saturating_add(9),
                self.bg_surface.g().saturating_add(11),
                self.bg_surface.b().saturating_add(15),
                238,
            )
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, 238)
        }
    }

    /// Translucent Liquid Glass fill for floating topbar capsules, search bar, and segmented controls.
    #[inline]
    pub fn glass_pill_fill(&self) -> Color32 {
        if self.dark {
            Color32::from_rgba_unmultiplied(
                self.bg_elevated.r().saturating_add(7),
                self.bg_elevated.g().saturating_add(9),
                self.bg_elevated.b().saturating_add(13),
                222,
            )
        } else {
            Color32::from_rgba_unmultiplied(255, 255, 255, 228)
        }
    }

    /// Refractive outer edge stroke for Liquid Glass surfaces.
    #[inline]
    pub fn glass_border(&self) -> Color32 {
        if self.dark {
            Color32::from_white_alpha(28)
        } else {
            Color32::from_black_alpha(22)
        }
    }

    /// Overhead specular catch-light rim along the top bevel of Liquid Glass surfaces.
    #[inline]
    pub fn glass_specular_rim(&self) -> Color32 {
        if self.dark {
            Color32::from_white_alpha(68)
        } else {
            Color32::from_white_alpha(240)
        }
    }

    /// Elevated ambient lift shadow for floating Liquid Glass docks and sheets.
    #[inline]
    pub fn shadow_glass(&self) -> egui::epaint::Shadow {
        egui::epaint::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(if self.dark { 110 } else { 28 }),
        }
    }

    pub fn from_manifest(m: ThemeManifest) -> Option<Self> {
        Some(Self {
            id: m.id,
            name: m.name,
            dark: m.dark,
            bg_base: parse_hex_color(&m.bg_base)?,
            bg_surface: parse_hex_color(&m.bg_surface)?,
            bg_elevated: parse_hex_color(&m.bg_elevated)?,
            bg_hover: parse_hex_color(&m.bg_hover)?,
            accent: parse_hex_color(&m.accent)?,
            accent_hover: parse_hex_color(&m.accent_hover)?,
            text_primary: parse_hex_color(&m.text_primary)?,
            text_secondary: parse_hex_color(&m.text_secondary)?,
            text_muted: parse_hex_color(&m.text_muted)?,
            border: parse_hex_color(&m.border)?,
            danger: parse_hex_color(&m.danger)?,
            warning: parse_hex_color(&m.warning)?,
        })
    }

    pub fn from_json_str(raw: &str) -> anyhow::Result<Self> {
        let manifest: ThemeManifest = serde_json::from_str(raw)?;
        Self::from_manifest(manifest)
            .ok_or_else(|| anyhow::anyhow!("invalid hex color in theme manifest"))
    }

    /// Apply this theme cleanly to an `egui::Context`.
    pub fn apply_to_egui(&self, ctx: &egui::Context) {
        let mut visuals = if self.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        };

        let hairline = self.hairline();
        let subtle_focus = if self.dark {
            Color32::from_white_alpha(48)
        } else {
            Color32::from_black_alpha(52)
        };

        visuals.panel_fill = self.bg_base;
        visuals.window_fill = self.bg_surface;
        visuals.extreme_bg_color = if self.dark {
            Color32::from_rgb(15, 17, 21)
        } else {
            self.bg_surface
        };
        visuals.faint_bg_color = self.bg_elevated;
        visuals.code_bg_color = self.bg_elevated;
        visuals.override_text_color = Some(self.text_primary);
        visuals.selection.bg_fill = self.accent_soft();
        visuals.selection.stroke = Stroke::new(1.0, self.accent);
        visuals.hyperlink_color = self.accent;
        visuals.warn_fg_color = self.warning;
        visuals.error_fg_color = self.danger;

        // Quiet, border-restrained widget chrome (no harsh boxes around every control)
        visuals.widgets.noninteractive.bg_fill = self.bg_surface;
        visuals.widgets.noninteractive.weak_bg_fill = Color32::TRANSPARENT;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, hairline);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.text_secondary);
        visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);

        visuals.widgets.inactive.bg_fill = self.bg_elevated;
        visuals.widgets.inactive.weak_bg_fill = self.bg_elevated;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, hairline);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, self.text_primary);
        visuals.widgets.inactive.corner_radius = CornerRadius::same(8);

        visuals.widgets.hovered.bg_fill = self.bg_hover;
        visuals.widgets.hovered.weak_bg_fill = self.bg_hover;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, subtle_focus);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.2, self.text_primary);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
        visuals.widgets.hovered.expansion = 0.0;

        visuals.widgets.active.bg_fill = self.bg_hover;
        visuals.widgets.active.weak_bg_fill = self.bg_hover;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, self.accent);
        visuals.widgets.active.fg_stroke = Stroke::new(1.4, self.text_primary);
        visuals.widgets.active.corner_radius = CornerRadius::same(8);
        visuals.widgets.active.expansion = 0.0;

        visuals.widgets.open.bg_fill = self.bg_hover;
        visuals.widgets.open.weak_bg_fill = self.bg_hover;
        visuals.widgets.open.bg_stroke = Stroke::new(1.0, subtle_focus);
        visuals.widgets.open.fg_stroke = Stroke::new(1.2, self.text_primary);
        visuals.widgets.open.corner_radius = CornerRadius::same(8);

        visuals.window_corner_radius = CornerRadius::same(12);
        visuals.window_stroke = Stroke::new(1.0, hairline);
        visuals.window_shadow = self.shadow_floating();
        visuals.popup_shadow = self.shadow_floating();

        let mut style = Style {
            visuals,
            animation_time: 0.14,
            ..Default::default()
        };
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size = egui::vec2(32.0, 28.0);
        style.spacing.scroll.bar_width = 5.0;
        style.spacing.scroll.floating = true;
        style.spacing.scroll.floating_width = 4.0;
        style.spacing.scroll.floating_allocated_width = 0.0;

        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(13.5));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(11.5));
        style
            .text_styles
            .insert(egui::TextStyle::Monospace, egui::FontId::monospace(12.0));

        let target_theme = if self.dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        };
        ctx.set_theme(target_theme);
        ctx.set_style_of(target_theme, style);
    }
}

/// Registry of built-in, JSON-loaded, and plugin-contributed themes.
pub struct ThemeManager {
    themes: Vec<ThemePalette>,
    active_id: String,
}

impl ThemeManager {
    pub fn new(themes_dir: &Path, extra_themes: Vec<ThemePalette>) -> Self {
        let mut themes = vec![
            ThemePalette::obsidian_dark(),
            ThemePalette::midnight_cobalt(),
            ThemePalette::emerald_studio(),
            ThemePalette::daylight_paper(),
        ];

        // Load bundled sample JSON theme (`assets/themes/nordic_slate.json`)
        if let Ok(nordic) =
            ThemePalette::from_json_str(include_str!("../../assets/themes/nordic_slate.json"))
        {
            themes.push(nordic);
        }

        // Load user-provided JSON themes from data_dir/themes/
        if let Ok(entries) = std::fs::read_dir(themes_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json")
                    && let Ok(raw) = std::fs::read_to_string(&path)
                    && let Ok(palette) = ThemePalette::from_json_str(&raw)
                    && !themes.iter().any(|t| t.id == palette.id)
                {
                    themes.push(palette);
                }
            }
        }

        for extra in extra_themes {
            if !themes.iter().any(|t| t.id == extra.id) {
                themes.push(extra);
            }
        }

        Self {
            themes,
            active_id: "obsidian_dark".to_string(),
        }
    }

    pub fn active(&self) -> &ThemePalette {
        self.themes
            .iter()
            .find(|t| t.id == self.active_id)
            .unwrap_or(&self.themes[0])
    }

    pub fn set_active(&mut self, id: &str) -> bool {
        if self.themes.iter().any(|t| t.id == id) {
            self.active_id = id.to_string();
            true
        } else {
            false
        }
    }

    pub fn list(&self) -> &[ThemePalette] {
        &self.themes
    }
}

pub fn parse_hex_color(hex: &str) -> Option<Color32> {
    let trimmed = hex.trim().trim_start_matches('#');
    if trimmed.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&trimmed[0..2], 16).ok()?;
    let g = u8::from_str_radix(&trimmed[2..4], 16).ok()?;
    let b = u8::from_str_radix(&trimmed[4..6], 16).ok()?;
    Some(Color32::from_rgb(r, g, b))
}

/// Paint an iOS 26 / macOS Tahoe Liquid Glass floating surface:
/// ambient lift shadow -> frosted translucent body -> optional subtle album/accent tint ->
/// concentric upper-bevel light wash -> refractive border -> overhead specular catch-light rim.
pub fn paint_liquid_glass(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: u8,
    fill: Color32,
    tint: Option<Color32>,
    theme: &ThemePalette,
    elevated_shadow: bool,
) {
    let r = CornerRadius::same(radius);
    let shadow = if elevated_shadow {
        theme.shadow_glass()
    } else {
        theme.shadow_soft()
    };
    painter.add(shadow.as_shape(rect, r));
    painter.rect_filled(rect, r, fill);

    if let Some(t) = tint {
        painter.rect_filled(rect, r, t);
    }

    // Soft inner bevel refraction ring (seamless, zero horizontal mid-surface seam)
    if rect.width() > 16.0 && rect.height() > 14.0 {
        let inner_r = CornerRadius::same(radius.saturating_sub(1));
        painter.rect_stroke(
            rect.shrink(1.0),
            inner_r,
            Stroke::new(
                1.0,
                Color32::from_white_alpha(if theme.dark { 10 } else { 150 }),
            ),
            egui::StrokeKind::Inside,
        );
    }

    painter.rect_stroke(
        rect,
        r,
        Stroke::new(1.0, theme.glass_border()),
        egui::StrokeKind::Inside,
    );

    // Overhead specular catch-light along the straight top rim + soft secondary feather
    let inset = f32::from(radius).min(rect.width() * 0.35);
    if rect.width() > inset * 2.0 + 4.0 {
        painter.line_segment(
            [
                egui::pos2(rect.left() + inset, rect.top() + 0.9),
                egui::pos2(rect.right() - inset, rect.top() + 0.9),
            ],
            Stroke::new(1.0, theme.glass_specular_rim()),
        );
        painter.line_segment(
            [
                egui::pos2(rect.left() + inset + 2.0, rect.top() + 2.0),
                egui::pos2(rect.right() - inset - 2.0, rect.top() + 2.0),
            ],
            Stroke::new(
                1.0,
                Color32::from_white_alpha(if theme.dark { 22 } else { 120 }),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative_luminance(c: Color32) -> f64 {
        let chan = |v: u8| {
            let s = f64::from(v) / 255.0;
            if s <= 0.03928 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * chan(c.r()) + 0.7152 * chan(c.g()) + 0.0722 * chan(c.b())
    }

    fn contrast_ratio(a: Color32, b: Color32) -> f64 {
        let l1 = relative_luminance(a);
        let l2 = relative_luminance(b);
        let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn loads_sample_json_theme() {
        let raw = include_str!("../../assets/themes/nordic_slate.json");
        let palette = ThemePalette::from_json_str(raw).expect("valid JSON theme");
        assert_eq!(palette.id, "nordic_slate");
        assert_eq!(palette.accent, Color32::from_rgb(0x38, 0xBD, 0xF8));
    }

    #[test]
    fn builtin_themes_meet_wcag_aa_contrast() {
        for theme in [
            ThemePalette::obsidian_dark(),
            ThemePalette::midnight_cobalt(),
            ThemePalette::emerald_studio(),
            ThemePalette::daylight_paper(),
        ] {
            for bg in [theme.bg_base, theme.bg_surface] {
                assert!(
                    contrast_ratio(theme.text_primary, bg) >= 7.0,
                    "{} primary text contrast too low",
                    theme.id
                );
                assert!(
                    contrast_ratio(theme.text_secondary, bg) >= 4.5,
                    "{} secondary text contrast too low",
                    theme.id
                );
                assert!(
                    contrast_ratio(theme.text_muted, bg) >= 4.5,
                    "{} muted text contrast too low: {:.2}",
                    theme.id,
                    contrast_ratio(theme.text_muted, bg)
                );
            }
            assert!(
                contrast_ratio(theme.on_accent(), theme.accent) >= 4.5,
                "{} on_accent contrast too low",
                theme.id
            );
        }
    }
}
