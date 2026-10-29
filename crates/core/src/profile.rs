//! Profile: the user's devices and settings, in a versioned format. It is
//! also the file exchanged by export / import.
//!
//! The profile holds the devices' local keys: it is a secret.

use std::io::ErrorKind;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::settings::Settings;

/// Format version written by this version of the app.
pub const PROFILE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub local_key: String,
    /// Last known LAN address; the engine rediscovers it if the device moves.
    #[serde(default)]
    pub ip: String,
    #[serde(default)]
    pub product: String,
    #[serde(default)]
    pub category: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub version: u32,
    #[serde(default)]
    pub devices: Vec<Device>,
    /// ID of the device driven by the sync.
    #[serde(default)]
    pub sync_device: Option<String>,
    #[serde(default)]
    pub settings: Settings,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            version: PROFILE_VERSION,
            devices: Vec::new(),
            sync_device: None,
            settings: Settings::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("cannot read or write: {0}")]
    Io(#[from] std::io::Error),
    #[error("unreadable file: {0}")]
    Json(#[from] serde_json::Error),
    #[error("this profile comes from a newer version of the app")]
    TooNew,
    #[error("this file contains no device")]
    Empty,
}

/// Single controller from the old `settings.json`, before profiles.
#[derive(Deserialize)]
struct LegacyDevice {
    #[serde(default)]
    ip: String,
    #[serde(default)]
    device_id: String,
    #[serde(default)]
    local_key: String,
}

impl Profile {
    /// Loads the profile, or else migrates the old `settings.json`.
    /// With neither: an empty profile (first run).
    pub fn load_or_migrate(path: &Path, legacy_settings: &Path) -> Result<Self, ProfileError> {
        match std::fs::read_to_string(path) {
            Ok(text) => return Self::parse(&text),
            Err(error) if error.kind() != ErrorKind::NotFound => return Err(error.into()),
            Err(_) => {}
        }
        match std::fs::read_to_string(legacy_settings) {
            Ok(text) => Self::from_legacy(&text),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    fn from_legacy(text: &str) -> Result<Self, ProfileError> {
        let settings: Settings = serde_json::from_str(text)?;
        let legacy: LegacyDevice = serde_json::from_str(text)?;
        let mut profile = Self {
            settings,
            ..Self::default()
        };
        if !legacy.device_id.is_empty() && !legacy.local_key.is_empty() {
            profile.sync_device = Some(legacy.device_id.clone());
            profile.devices.push(Device {
                id: legacy.device_id,
                name: "LSC panels".to_owned(),
                local_key: legacy.local_key,
                ip: legacy.ip,
                product: String::new(),
                category: String::new(),
            });
        }
        Ok(profile)
    }

    fn parse(text: &str) -> Result<Self, ProfileError> {
        let profile: Self = serde_json::from_str(text)?;
        if profile.version > PROFILE_VERSION {
            return Err(ProfileError::TooNew);
        }
        Ok(profile)
    }

    pub fn save(&self, path: &Path) -> Result<(), ProfileError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.to_export())?;
        Ok(())
    }

    /// Content of an export file (same as the saved profile).
    pub fn to_export(&self) -> String {
        serde_json::to_string_pretty(self).expect("a profile is always serializable")
    }

    /// Reads an export file. Rejects a file without any device.
    pub fn from_import(text: &str) -> Result<Self, ProfileError> {
        let mut profile = Self::parse(text)?;
        if profile.devices.is_empty() {
            return Err(ProfileError::Empty);
        }
        profile.version = PROFILE_VERSION;
        if profile.sync_target().is_none() {
            profile.sync_device = profile.devices.first().map(|d| d.id.clone());
        }
        Ok(profile)
    }

    /// Device driven by the sync, if configured.
    pub fn sync_target(&self) -> Option<&Device> {
        let id = self.sync_device.as_deref()?;
        self.devices.iter().find(|device| device.id == id)
    }

    /// Merges the devices returned by the account: keys and names updated,
    /// local address kept when the device was already known.
    pub fn merge_devices(&mut self, found: Vec<Device>) {
        for mut device in found {
            match self.devices.iter_mut().find(|known| known.id == device.id) {
                Some(known) => {
                    if device.ip.is_empty() {
                        device.ip = std::mem::take(&mut known.ip);
                    }
                    *known = device;
                }
                None => self.devices.push(device),
            }
        }
        if self.sync_target().is_none() {
            self.sync_device = self.devices.first().map(|d| d.id.clone());
        }
    }

    /// Updates the local address of a device found on the network.
    pub fn set_ip(&mut self, id: &str, ip: String) {
        if let Some(device) = self.devices.iter_mut().find(|d| d.id == id) {
            device.ip = ip;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Mode;

    fn device(id: &str, ip: &str) -> Device {
        Device {
            id: id.to_owned(),
            name: format!("device {id}"),
            local_key: "0123456789abcdef".to_owned(),
            ip: ip.to_owned(),
            product: String::new(),
            category: "dd".to_owned(),
        }
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lscsync-profile-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn first_run_gives_an_empty_profile() {
        let dir = temp_dir("empty");
        let profile =
            Profile::load_or_migrate(&dir.join("profile.json"), &dir.join("settings.json"))
                .unwrap();
        assert!(profile.devices.is_empty());
        assert!(profile.sync_target().is_none());
    }

    #[test]
    fn migrates_the_old_settings_json() {
        let dir = temp_dir("migration");
        std::fs::write(
            dir.join("settings.json"),
            r#"{"mode": "audio", "lum_max": 60, "ip": "10.0.0.2", "device_id": "abc",
                "local_key": "k", "tapo_actif": false}"#,
        )
        .unwrap();
        let profile =
            Profile::load_or_migrate(&dir.join("profile.json"), &dir.join("settings.json"))
                .unwrap();
        assert_eq!(profile.settings.mode, Mode::Audio);
        assert_eq!(profile.settings.lum_max, 60);
        let target = profile.sync_target().expect("migrated device");
        assert_eq!(
            (target.id.as_str(), target.ip.as_str()),
            ("abc", "10.0.0.2")
        );
    }

    #[test]
    fn profile_wins_over_the_old_file() {
        let dir = temp_dir("priority");
        std::fs::write(
            dir.join("settings.json"),
            r#"{"device_id": "old", "local_key": "k"}"#,
        )
        .unwrap();
        let mut profile = Profile::default();
        profile.merge_devices(vec![device("new", "")]);
        profile.save(&dir.join("profile.json")).unwrap();

        let loaded =
            Profile::load_or_migrate(&dir.join("profile.json"), &dir.join("settings.json"))
                .unwrap();
        assert_eq!(loaded.sync_target().unwrap().id, "new");
    }

    #[test]
    fn export_then_import() {
        let mut profile = Profile::default();
        profile.merge_devices(vec![device("a", "10.0.0.2"), device("b", "")]);
        profile.sync_device = Some("b".to_owned());
        profile.settings.mode = Mode::Screen;

        let imported = Profile::from_import(&profile.to_export()).unwrap();
        assert_eq!(imported.devices, profile.devices);
        assert_eq!(imported.sync_target().unwrap().id, "b");
        assert_eq!(imported.settings.mode, Mode::Screen);
    }

    #[test]
    fn import_rejects_a_file_without_devices() {
        let empty = Profile::default().to_export();
        assert!(matches!(
            Profile::from_import(&empty),
            Err(ProfileError::Empty)
        ));
        assert!(Profile::from_import(r#"{"other": 1}"#).is_err());
    }

    #[test]
    fn import_rejects_a_future_version() {
        let text = format!(r#"{{"version": {}, "devices": []}}"#, PROFILE_VERSION + 1);
        assert!(matches!(
            Profile::from_import(&text),
            Err(ProfileError::TooNew)
        ));
    }

    #[test]
    fn merge_keeps_the_known_local_address() {
        let mut profile = Profile::default();
        profile.merge_devices(vec![device("a", "10.0.0.2")]);
        let mut renamed = device("a", "");
        renamed.name = "Living room".to_owned();
        renamed.local_key = "new-key-00000000".to_owned();
        profile.merge_devices(vec![renamed, device("b", "")]);

        assert_eq!(profile.devices.len(), 2);
        let a = &profile.devices[0];
        assert_eq!(
            (a.name.as_str(), a.ip.as_str()),
            ("Living room", "10.0.0.2")
        );
        assert_eq!(a.local_key, "new-key-00000000");
        assert_eq!(profile.sync_target().unwrap().id, "a");
    }
}
