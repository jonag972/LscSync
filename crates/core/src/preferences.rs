//! App preferences: interface language and update checks.
//!
//! Kept apart from the profile on purpose: they belong to this computer, and
//! an exported profile must not change the language of the one importing it.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// The system's language, English when it is not translated.
    #[default]
    Auto,
    English,
    French,
}

impl Language {
    /// Locale code of the translation to show; `system` is the system locale
    /// (such as "fr-FR"), used by `Auto`.
    pub fn locale(self, system: Option<&str>) -> &'static str {
        match self {
            Self::English => "en",
            Self::French => "fr",
            Self::Auto => match system {
                Some(locale) if locale.to_ascii_lowercase().starts_with("fr") => "fr",
                _ => "en",
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub language: Language,
    /// Look for a new version on GitHub at launch and once a day.
    pub check_updates: bool,
    /// Update notice closed by the user: not shown again for this version.
    pub dismissed_update: Option<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            language: Language::Auto,
            check_updates: true,
            dismissed_update: None,
        }
    }
}

impl Preferences {
    /// Missing or unreadable file: default preferences.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).expect("preferences are serializable");
        std::fs::write(path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follows_the_system_language() {
        assert_eq!(Language::Auto.locale(Some("fr-FR")), "fr");
        assert_eq!(Language::Auto.locale(Some("fr_CA")), "fr");
        assert_eq!(Language::Auto.locale(Some("de-DE")), "en");
        assert_eq!(Language::Auto.locale(None), "en");
        assert_eq!(Language::French.locale(Some("en-US")), "fr");
        assert_eq!(Language::English.locale(Some("fr-FR")), "en");
    }

    #[test]
    fn missing_keys_take_defaults() {
        let preferences: Preferences = serde_json::from_str(r#"{"language": "french"}"#).unwrap();
        assert_eq!(preferences.language, Language::French);
        assert!(preferences.check_updates);
        assert_eq!(preferences.dismissed_update, None);
    }
}
