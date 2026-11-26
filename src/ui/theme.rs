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
    /// Default Spotify-inspired dark palette using original Spotirust tones.
    pub fn obsidian_dark() -> Self {
        Self {
            id: "obsidian_dark".to_string(),
            name: "Obsidian Dark".to_string(),
            dark: true,
            bg_base: Color32::from_rgb(10, 12, 14),
            bg_surface: Color32::from_rgb(18, 20, 24),
            bg_elevated: Color32::from_rgb(26, 30, 36),
            bg_hover: Color32::from_rgb(38, 43, 52),
            accent: Color32::from_rgb(30, 215, 96),
            accent_hover: Color32::from_rgb(60, 232, 122),
            text_primary: Color32::from_rgb(245, 247, 250),
            text_secondary: Color32::from_rgb(174, 182, 194),
            text_muted: Color32::from_rgb(112, 122, 136),
            border: Color32::from_rgb(42, 47, 56),
            danger: Color32::from_rgb(239, 83, 80),
            warning: Color32::from_rgb(245, 176, 65),
        }
    }

    pub fn midnight_cobalt() -> Self {
        Self {
            id: "midnight_cobalt".to_string(),
            name: "Midnight Cobalt".to_string(),
            dark: true,
            bg_base: Color32::from_rgb(10, 15, 26),
            bg_surface: Color32::from_rgb(17, 24, 39),
            bg_elevated: Color32::from_rgb(26, 36, 56),
            bg_hover: Color32::from_rgb(37, 50, 77),
            accent: Color32::from_rgb(56, 189, 248),
            accent_hover: Color32::from_rgb(125, 211, 252),
            text_primary: Color32::from_rgb(241, 245, 249),
            text_secondary: Color32::from_rgb(168, 181, 201),
            text_muted: Color32::from_rgb(100, 116, 139),
            border: Color32::from_rgb(44, 58, 86),
            danger: Color32::from_rgb(248, 113, 113),
            warning: Color32::from_rgb(251, 191, 36),
        }
    }

    pub fn emerald_studio() -> Self {
        Self {
            id: "emerald_studio".to_string(),
            name: "Emerald Studio".to_string(),
            dark: true,
            bg_base: Color32::from_rgb(11, 17, 15),
            bg_surface: Color32::from_rgb(17, 26, 23),
            bg_elevated: Color32::from_rgb(25, 38, 33),
            bg_hover: Color32::from_rgb(35, 52, 46),
            accent: Color32::from_rgb(16, 185, 129),
            accent_hover: Color32::from_rgb(52, 211, 153),
            text_primary: Color32::from_rgb(236, 253, 245),
            text_secondary: Color32::from_rgb(167, 198, 186),
            text_muted: Color32::from_rgb(107, 138, 127),
            border: Color32::from_rgb(39, 59, 51),
            danger: Color32::from_rgb(248, 113, 113),
            warning: Color32::from_rgb(251, 191, 36),
        }
    }

    pub fn daylight_paper() -> Self {
        Self {
            id: "daylight_paper".to_string(),
            name: "Daylight Paper".to_string(),
            dark: false,
            bg_base: Color32::from_rgb(244, 246, 249),
            bg_surface: Color32::from_rgb(255, 255, 255),
            bg_elevated: Color32::from_rgb(236, 240, 245),
            bg_hover: Color32::from_rgb(223, 229, 237),
            accent: Color32::from_rgb(16, 152, 74),
            accent_hover: Color32::from_rgb(22, 178, 88),
            text_primary: Color32::from_rgb(18, 22, 28),
            text_secondary: Color32::from_rgb(76, 86, 102),
            text_muted: Color32::from_rgb(122, 134, 150),
            border: Color32::from_rgb(211, 218, 228),
            danger: Color32::from_rgb(217, 45, 32),
            warning: Color32::from_rgb(217, 119, 6),
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

        visuals.panel_fill = self.bg_base;
        visuals.window_fill = self.bg_surface;
        visuals.extreme_bg_color = self.bg_base;
        visuals.faint_bg_color = self.bg_elevated;
        visuals.code_bg_color = self.bg_elevated;
        visuals.override_text_color = Some(self.text_primary);
        visuals.selection.bg_fill = self.accent.linear_multiply(0.32);
        visuals.selection.stroke = Stroke::new(1.0, self.accent);
        visuals.hyperlink_color = self.accent;
        visuals.warn_fg_color = self.warning;
        visuals.error_fg_color = self.danger;

        visuals.widgets.noninteractive.bg_fill = self.bg_surface;
        visuals.widgets.noninteractive.weak_bg_fill = self.bg_surface;
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.text_secondary);
        visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);

        visuals.widgets.inactive.bg_fill = self.bg_elevated;
        visuals.widgets.inactive.weak_bg_fill = self.bg_elevated;
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, self.border);
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, self.text_primary);
        visuals.widgets.inactive.corner_radius = CornerRadius::same(8);

        visuals.widgets.hovered.bg_fill = self.bg_hover;
        visuals.widgets.hovered.weak_bg_fill = self.bg_hover;
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, self.accent);
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.2, self.text_primary);
        visuals.widgets.hovered.corner_radius = CornerRadius::same(8);

        visuals.widgets.active.bg_fill = self.accent;
        visuals.widgets.active.weak_bg_fill = self.accent;
        visuals.widgets.active.bg_stroke = Stroke::new(1.0, self.accent_hover);
        visuals.widgets.active.fg_stroke = Stroke::new(1.5, Color32::BLACK);
        visuals.widgets.active.corner_radius = CornerRadius::same(8);

        visuals.window_corner_radius = CornerRadius::same(12);
        visuals.window_stroke = Stroke::new(1.0, self.border);

        let mut style = Style {
            visuals,
            ..Default::default()
        };
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.scroll.bar_width = 6.0;
        style.spacing.scroll.floating = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_sample_json_theme() {
        let raw = include_str!("../../assets/themes/nordic_slate.json");
        let palette = ThemePalette::from_json_str(raw).expect("valid JSON theme");
        assert_eq!(palette.id, "nordic_slate");
        assert_eq!(palette.accent, Color32::from_rgb(0x38, 0xBD, 0xF8));
    }
}
