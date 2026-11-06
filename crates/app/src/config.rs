//! Local configuration (`~/.config/bang/config.toml`).

use serde::{Deserialize, Serialize};

/// Top-level configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// OAuth2 client ids per provider.
    #[serde(default)]
    pub oauth: OAuthConfig,
    /// Theme mode: `"light"`, `"dark"`, or `"system"`.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// UI language: system, fr or en.
    #[serde(default = "default_theme")]
    pub language: String,
    /// Fetch sender photos and logos online.
    #[serde(default = "default_online_avatars")]
    pub online_avatars: bool,
    /// Detect verification codes in newly received messages.
    #[serde(default)]
    pub otp_enabled: bool,
    /// First-launch setup: absent for legacy installs, false while pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub welcome_completed: Option<bool>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            oauth: OAuthConfig::default(),
            theme: default_theme(),
            language: default_theme(),
            online_avatars: true,
            otp_enabled: false,
            welcome_completed: None,
        }
    }
}

fn default_online_avatars() -> bool {
    true
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
    /// Development client ID override (empty = bundled public registration).
    #[serde(default)]
    pub client_id: String,
    /// Optional desktop secret issued with a Google OAuth client.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub client_secret: String,
}

impl OAuthConfig {
    /// Resolve a complete development override or the bundled public registration.
    pub fn registration(
        &self,
        provider: bang_core::OAuthProvider,
    ) -> Option<bang_core::OAuthRegistration> {
        let settings = match provider {
            bang_core::OAuthProvider::Google => &self.google,
            bang_core::OAuthProvider::Microsoft => &self.microsoft,
        };
        if !settings.client_id.trim().is_empty() {
            Some(bang_core::OAuthRegistration {
                client_id: settings.client_id.trim().into(),
                desktop_parameter: (provider == bang_core::OAuthProvider::Google
                    && !settings.client_secret.is_empty())
                .then(|| settings.client_secret.clone()),
            })
        } else {
            bang_core::providers::registration(provider)
        }
    }
}

impl Config {
    /// Load the config file, if present. Missing file or bad TOML yields defaults.
    pub fn load() -> Self {
        let Some(dirs) = directories::ProjectDirs::from("", "", "bang") else {
            return Self::default();
        };
        Self::load_from(&dirs.config_dir().join("config.toml"))
    }

    pub(crate) fn load_from(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save preferences and create their directory on first use.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(dirs) = directories::ProjectDirs::from("", "", "bang") else {
            return Err(std::io::Error::other("no configuration directory"));
        };
        self.save_to(&dirs.config_dir().join("config.toml"))
    }

    pub(crate) fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let s = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        // Write a temp file and rename it into place so a crash mid-write
        // cannot truncate the existing configuration.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, s)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_registration_is_not_serialized_and_overrides_are_complete() {
        let mut config = super::Config::default();
        let bundled = config.oauth.registration(bang_core::OAuthProvider::Google);
        assert_eq!(
            bundled.is_some(),
            bang_core::providers::registration(bang_core::OAuthProvider::Google).is_some()
        );
        let serialized = toml::to_string(&config).unwrap();
        assert!(!serialized.contains(bang_core::providers::GOOGLE_CLIENT_ID));
        if let Some(parameter) = bundled.and_then(|r| r.desktop_parameter) {
            assert!(!serialized.contains(&parameter));
        }
        config.oauth.google.client_id = "development-id".into();
        let custom = config
            .oauth
            .registration(bang_core::OAuthProvider::Google)
            .unwrap();
        assert_eq!(custom.client_id, "development-id");
        assert!(custom.desktop_parameter.is_none());
        config.oauth.microsoft.client_id = "public-microsoft".into();
        config.oauth.microsoft.client_secret = "do-not-use".into();
        assert!(config
            .oauth
            .registration(bang_core::OAuthProvider::Microsoft)
            .unwrap()
            .desktop_parameter
            .is_none());
    }
    #[test]
    fn existing_config_remains_compatible() {
        let cfg: super::Config = toml::from_str("theme = 'dark'").unwrap();
        assert_eq!(cfg.language, "system");
        assert_eq!(cfg.theme, "dark");
        assert!(cfg.online_avatars);
        let disabled: super::Config = toml::from_str("online_avatars = false").unwrap();
        assert!(!disabled.online_avatars);
        let default = super::Config::default();
        assert_eq!(default.theme, "system");
        assert_eq!(default.language, "system");
    }

    #[test]
    fn language_preferences_persist_without_losing_oauth() {
        let dir = std::env::temp_dir().join(format!(
            "bang-preferences-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("nested/config.toml");
        let mut config = super::Config::default();
        config.oauth.google.client_id = "local-test-client".into();
        config.online_avatars = false;
        for language in ["system", "fr", "en"] {
            config.language = language.into();
            config.save_to(&path).unwrap();
            let restored: super::Config =
                toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(restored.language, language);
            assert!(!restored.online_avatars);
            assert_eq!(restored.oauth.google.client_id, "local-test-client");
        }
        // The atomic save must not leave a temp file behind.
        assert!(!path.with_extension("toml.tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn welcome_marker_round_trips_without_changing_legacy_config() {
        let mut config: super::Config = toml::from_str("theme = 'dark'\nlanguage = 'fr'").unwrap();
        assert_eq!(config.welcome_completed, None);
        assert!(!toml::to_string(&config)
            .unwrap()
            .contains("welcome_completed"));
        config.oauth.google.client_id = "preserved-client".into();
        for completed in [false, true] {
            config.welcome_completed = Some(completed);
            let restored: super::Config =
                toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
            assert_eq!(restored.welcome_completed, Some(completed));
            assert_eq!(restored.theme, "dark");
            assert_eq!(restored.language, "fr");
            assert_eq!(restored.oauth.google.client_id, "preserved-client");
        }
    }
}
