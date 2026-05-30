//! Persistent settings — the native equivalent of ebb's `localStorage`-backed
//! settings. Stored as JSON under the platform config dir, with an env-var
//! override matching ebb's `SETTING_*` scheme.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Gelbooru API credentials, pasted verbatim — e.g.
    /// `&api_key=...&user_id=...`.
    #[serde(default)]
    pub gelbooru_credentials: String,
}

impl Settings {
    /// Location of the settings file (`<config>/ribb/settings.json`).
    fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "ribb")
            .map(|dirs| dirs.config_dir().join("settings.json"))
    }

    /// Load settings from disk, then apply any env-var overrides.
    pub fn load() -> Self {
        let mut settings: Settings = Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();

        // Parity with ebb's `SETTING_GELBOORU_API_CREDENTIALS` override.
        if let Ok(creds) = std::env::var("SETTING_GELBOORU_API_CREDENTIALS") {
            settings.gelbooru_credentials = creds;
        }
        settings
    }

    /// Persist settings to disk, creating the config directory if needed.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::path() else {
            tracing::warn!("no config directory available; settings not saved");
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).unwrap_or_default();
        std::fs::write(path, json)
    }
}
