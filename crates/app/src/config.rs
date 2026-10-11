//! Local configuration (`~/.config/bang/config.toml`).

use serde::{Deserialize, Serialize};

/// Top-level configuration.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    /// OAuth2 client ids per provider.
    #[serde(default)]
    pub oauth: OAuthConfig,
    /// Theme mode: `"light"`, `"dark"`, or `"system"`.
    #[serde(default = "default_theme")]
    pub theme: String,
}

fn default_theme() -> String {
    "system".into()
}

/// OAuth2 section.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OAuthConfig {
    /// Google (Gmail) client id.
    #[serde(default)]
    pub google: ProviderConfig,
    /// Microsoft (Outlook) client id.
    #[serde(default)]
    pub microsoft: ProviderConfig,
}

/// One provider's OAuth2 settings.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProviderConfig {
    /// OAuth2 client id (empty = not configured).
    #[serde(default)]
    pub client_id: String,
}

impl Config {
    /// Load the config file, if present. Missing file or bad TOML yields defaults.
    pub fn load() -> Self {
        let Some(dirs) = directories::ProjectDirs::from("", "", "bang") else {
            return Self::default();
        };
        match std::fs::read_to_string(dirs.config_dir().join("config.toml")) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save the config file (best effort; ignored on failure).
    pub fn save(&self) {
        let Some(dirs) = directories::ProjectDirs::from("", "", "bang") else {
            return;
        };
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(dirs.config_dir().join("config.toml"), s);
        }
    }
}
