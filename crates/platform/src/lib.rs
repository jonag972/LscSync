//! Platform integration: the app's data folder.
//!
//! Paths follow each OS's conventions:
//! - macOS: `~/Library/Application Support/LscSync`
//! - Windows: `%APPDATA%\LscSync`
//! - Linux: `~/.local/share/lscsync`

use std::path::PathBuf;

use directories::ProjectDirs;

pub fn data_dir() -> PathBuf {
    ProjectDirs::from("", "", "LscSync")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Profile: devices and settings.
pub fn profile_path() -> PathBuf {
    data_dir().join("profile.json")
}

/// Preferences of this computer: language, update checks.
pub fn preferences_path() -> PathBuf {
    data_dir().join("preferences.json")
}

/// Folder of the app when it was called Lumière Sync.
fn legacy_data_dir() -> Option<PathBuf> {
    ProjectDirs::from("", "", "LumiereSync").map(|dirs| dirs.data_dir().to_path_buf())
}

/// Settings file of the original app, before profiles; migrated once.
pub fn legacy_settings_path() -> PathBuf {
    legacy_data_dir()
        .unwrap_or_else(data_dir)
        .join("settings.json")
}

/// On first run after the rename, copies the profile of the former Lumière
/// Sync app so nothing is lost. The old folder is left untouched.
pub fn migrate_legacy_data() -> std::io::Result<()> {
    let profile = profile_path();
    if profile.exists() {
        return Ok(());
    }
    let Some(old_profile) = legacy_data_dir().map(|dir| dir.join("profile.json")) else {
        return Ok(());
    };
    if old_profile.exists() {
        std::fs::create_dir_all(data_dir())?;
        std::fs::copy(old_profile, profile)?;
    }
    Ok(())
}
