//! Extensible plugin and theme hook architecture for Spotirust.
//!
//! Allows external or built-in plugins to register metadata hooks, custom themes,
//! or custom lyrics providers without modifying core application code.

use crate::core::models::Track;
use crate::ui::theme::ThemePalette;
use std::sync::Arc;

/// Metadata describing a registered Spotirust plugin.
#[derive(Debug, Clone)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
}

/// Lifecycle and UI/playback extension hook implemented by Spotirust plugins.
pub trait SpotirustPlugin: Send + Sync {
    fn manifest(&self) -> PluginManifest;

    /// Optional custom themes contributed by this plugin.
    fn contributed_themes(&self) -> Vec<ThemePalette> {
        Vec::new()
    }

    /// Invoked whenever the active playing track changes (e.g., scrobbling, Discord RPC, logging).
    fn on_track_changed(&self, _track: &Track) {}

    /// Invoked when playback pauses or resumes.
    fn on_playback_state_changed(&self, _is_playing: bool) {}
}

/// Registry holding active plugins and dispatching non-blocking lifecycle notifications.
#[derive(Default)]
pub struct PluginRegistry {
    plugins: Vec<Arc<dyn SpotirustPlugin>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, plugin: Arc<dyn SpotirustPlugin>) {
        let manifest = plugin.manifest();
        tracing::info!(
            plugin_id = %manifest.id,
            version = %manifest.version,
            "Registered Spotirust plugin"
        );
        self.plugins.push(plugin);
    }

    pub fn manifests(&self) -> Vec<PluginManifest> {
        self.plugins.iter().map(|p| p.manifest()).collect()
    }

    pub fn all_contributed_themes(&self) -> Vec<ThemePalette> {
        self.plugins
            .iter()
            .flat_map(|p| p.contributed_themes())
            .collect()
    }

    pub fn notify_track_changed(&self, track: &Track) {
        for plugin in &self.plugins {
            plugin.on_track_changed(track);
        }
    }

    pub fn notify_playback_state(&self, is_playing: bool) {
        for plugin in &self.plugins {
            plugin.on_playback_state_changed(is_playing);
        }
    }

    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

/// Built-in lightweight logging/telemetry hook plugin demonstrating the extension API.
pub struct BuiltinSessionMetricsPlugin;

impl SpotirustPlugin for BuiltinSessionMetricsPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: "spotirust.builtin.metrics".to_string(),
            name: "Session Playback Observer".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            author: "Spotirust".to_string(),
            description: "Built-in plugin hook observing track transitions and playback state."
                .to_string(),
        }
    }

    fn on_track_changed(&self, track: &Track) {
        tracing::debug!(track_id = %track.id, title = %track.title, "Plugin observed track change");
    }
}
