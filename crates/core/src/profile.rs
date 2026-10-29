//! Profile: the user's devices and settings, in a versioned format. It is
//! also the file exchanged by export / import.
//!
//! The profile holds the devices' local keys: it is a secret.

use std::io::ErrorKind;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::layout::{Capabilities, ColorCommand, Zone};
use crate::settings::{Mode, Settings};

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
    /// Modes in which the device follows the sync.
    #[serde(default)]
    pub modes: Vec<Mode>,
    /// Before modes were chosen per device: followed the sync in every mode.
    #[serde(default, skip_serializing)]
    pub synced: bool,
    /// Where each zone looks on the screen; empty for the product's default
    /// layout (see [`Device::layout`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zones: Vec<Zone>,
    /// What Tuya says it can do (fetched at login).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
    /// How its color is sent, once calibrated by the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_command: Option<ColorCommand>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub version: u32,
    #[serde(default)]
    pub devices: Vec<Device>,
    /// First device of the sync. Each device now says whether it is synced
    /// ([`Device::synced`]); kept for profiles written before that.
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
                modes: Mode::ALL.to_vec(),
                synced: false,
                zones: Vec::new(),
                capabilities: None,
                color_command: None,
            });
        }
        Ok(profile)
    }

    fn parse(text: &str) -> Result<Self, ProfileError> {
        let mut profile: Self = serde_json::from_str(text)?;
        if profile.version > PROFILE_VERSION {
            return Err(ProfileError::TooNew);
        }
        // Before multi-device sync, a single device followed the screen.
        if !profile
            .devices
            .iter()
            .any(|device| device.synced || !device.modes.is_empty())
            && let Some(id) = profile.sync_device.clone()
            && let Some(device) = profile.devices.iter_mut().find(|d| d.id == id)
        {
            device.synced = true;
        }
        // Before modes were chosen per device, a synced device followed all.
        for device in &mut profile.devices {
            if device.synced && device.modes.is_empty() {
                device.modes = Mode::ALL.to_vec();
            }
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
        profile.ensure_one_synced();
        Ok(profile)
    }

    /// First device used in some mode, if any: named in errors.
    pub fn sync_target(&self) -> Option<&Device> {
        self.devices.iter().find(|device| !device.modes.is_empty())
    }

    /// Devices that follow the sync in `mode`.
    pub fn sync_targets(&self, mode: Mode) -> impl Iterator<Item = &Device> {
        self.devices
            .iter()
            .filter(move |device| device.modes.contains(&mode))
    }

    /// Adds a device to the sync in `mode`, or removes it.
    pub fn set_synced(&mut self, id: &str, mode: Mode, synced: bool) {
        if let Some(device) = self.devices.iter_mut().find(|d| d.id == id) {
            device.modes.retain(|m| *m != mode);
            if synced {
                device.modes.push(mode);
            }
        }
        self.sync_device = self.sync_target().map(|device| device.id.clone());
    }

    /// With devices but none used anywhere, the first one follows every mode.
    fn ensure_one_synced(&mut self) {
        if self.sync_target().is_none()
            && let Some(first) = self.devices.first_mut()
        {
            first.modes = Mode::ALL.to_vec();
            self.sync_device = Some(first.id.clone());
        }
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
                    device.modes = std::mem::take(&mut known.modes);
                    device.zones = std::mem::take(&mut known.zones);
                    device.color_command = known.color_command;
                    if device.capabilities.is_none() {
                        device.capabilities = known.capabilities.take();
                    }
                    *known = device;
                }
                None => self.devices.push(device),
            }
        }
        self.ensure_one_synced();
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
            modes: Vec::new(),
            synced: false,
            zones: Vec::new(),
            capabilities: None,
            color_command: None,
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
        profile.devices[0].modes.clear();
        profile.set_synced("b", Mode::Audio, true);
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

    #[test]
    fn devices_are_chosen_per_mode() {
        let mut profile = Profile::default();
        profile.merge_devices(vec![device("a", ""), device("b", "")]);
        assert_eq!(profile.devices[0].modes, Mode::ALL);
        profile.set_synced("b", Mode::Audio, true);
        profile.set_synced("a", Mode::Audio, false);
        let music: Vec<&str> = profile
            .sync_targets(Mode::Audio)
            .map(|d| d.id.as_str())
            .collect();
        let screen: Vec<&str> = profile
            .sync_targets(Mode::ScreenZones)
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!((music, screen), (vec!["b"], vec!["a"]));
    }

    #[test]
    fn single_device_profiles_become_synced_devices() {
        let profile = Profile {
            devices: vec![device("a", ""), device("b", "")],
            sync_device: Some("b".to_owned()),
            ..Profile::default()
        };
        let loaded = Profile::parse(&profile.to_export()).unwrap();
        let synced: Vec<&str> = loaded
            .sync_targets(Mode::Screen)
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(synced, ["b"]);
        assert_eq!(loaded.devices[1].modes, Mode::ALL);
    }
}
