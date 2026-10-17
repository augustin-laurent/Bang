//! Shared English/French catalogue for Slint, Rust and the isolated composer.
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
};
static FRENCH: AtomicBool = AtomicBool::new(false);
/// Resolve the system locale, with English as the supported fallback.
pub fn resolve(mode: &str) -> &'static str {
    resolve_with(
        mode,
        &std::env::var("LANGUAGE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::var("LC_ALL").ok().filter(|s| !s.is_empty()))
            .or_else(|| std::env::var("LC_MESSAGES").ok().filter(|s| !s.is_empty()))
            .or_else(|| std::env::var("LANG").ok())
            .unwrap_or_default(),
    )
}
fn resolve_with(mode: &str, locale: &str) -> &'static str {
    let value = if mode == "system" || mode.is_empty() {
        locale
    } else {
        mode
    };
    if value.to_ascii_lowercase().starts_with("fr") {
        "fr"
    } else {
        "en"
    }
}
/// Activate a bundled translation after constructing the Slint window.
pub fn select(mode: &str) {
    let lang = resolve(mode);
    FRENCH.store(lang == "fr", Ordering::Relaxed);
    if let Err(e) = slint::select_bundled_translation(lang) {
        tracing::warn!("Translation: {e}");
    }
}
/// Whether the active language is French.
pub fn french() -> bool {
    FRENCH.load(Ordering::Relaxed)
}
/// The shared French source catalogue.
pub fn catalogue() -> &'static BTreeMap<String, String> {
    static CATALOGUE: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        serde_json::from_str(include_str!("../i18n/fr.json"))
            .expect("validated translation catalogue")
    })
}
/// Translate an application-owned string. Mail content is never translated.
pub fn tr(message: &str) -> String {
    if french() {
        catalogue()
            .get(message)
            .cloned()
            .unwrap_or_else(|| message.into())
    } else {
        message.into()
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn locale_resolution() {
        for (mode, locale, expected) in [
            ("system", "fr_FR.UTF-8", "fr"),
            ("system", "de_DE", "en"),
            ("en", "fr", "en"),
            ("fr", "en_US", "fr"),
        ] {
            assert_eq!(super::resolve_with(mode, locale), expected);
        }
    }
    #[test]
    fn catalogue_is_complete_and_nonempty() {
        assert!(super::catalogue().len() > 90);
        assert!(super::catalogue().values().all(|v| !v.is_empty()));
    }
}
