//! Sync settings, stored in the profile.
//!
//! JSON keys are in English; the historical French names are still accepted
//! when reading (migration from the old `settings.json`).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    #[serde(rename = "ambilight", alias = "ecran_zones")]
    ScreenZones,
    #[serde(rename = "color", alias = "ecran")]
    Screen,
    #[serde(rename = "music", alias = "audio")]
    Audio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioHue {
    #[default]
    #[serde(rename = "cycle", alias = "defile")]
    Cycle,
    #[serde(rename = "fixed", alias = "fixe")]
    Fixed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: Mode,
    #[serde(rename = "max_brightness", alias = "lum_max")]
    pub lum_max: u8,
    #[serde(rename = "saturation_boost", alias = "boost_sat")]
    pub boost_sat: f32,
    #[serde(alias = "lissage")]
    pub smoothing: f32,
    #[serde(alias = "hauteur_captee")]
    pub capture_height: f32,
    #[serde(rename = "music_reactivity", alias = "reactivite")]
    pub reactivity: f32,
    #[serde(rename = "music_hue_cycle_seconds", alias = "vitesse_hue")]
    pub hue_speed: u16,
    #[serde(rename = "music_hue", alias = "audio_teinte")]
    pub audio_hue: AudioHue,
    #[serde(rename = "music_fixed_hue", alias = "audio_hue_fixe")]
    pub audio_fixed_hue: u16,
    #[serde(rename = "music_min_brightness", alias = "audio_lum_min")]
    pub audio_min_light: f32,
    #[serde(rename = "music_offset_zones", alias = "audio_zones")]
    pub audio_zones: bool,
    #[serde(alias = "retour_blanc")]
    pub restore_white: bool,
    #[serde(alias = "inverser_zones")]
    pub invert_zones: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::ScreenZones,
            lum_max: 100,
            boost_sat: 1.9,
            smoothing: 0.5,
            capture_height: 0.3,
            reactivity: 0.4,
            hue_speed: 25,
            audio_hue: AudioHue::Cycle,
            audio_fixed_hue: 200,
            audio_min_light: 0.02,
            audio_zones: false,
            restore_white: true,
            invert_zones: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_english() {
        let json = serde_json::to_value(Settings::default()).expect("serializable");
        assert_eq!(json["mode"], "ambilight");
        assert_eq!(json["max_brightness"], 100);
        assert_eq!(json["invert_zones"], false);
    }

    #[test]
    fn reads_the_old_french_names() {
        let settings: Settings = serde_json::from_str(
            r#"{"mode": "audio", "lum_max": 60, "lissage": 0.2, "audio_teinte": "fixe",
                "inverser_zones": true, "tapo_actif": true, "actif": true}"#,
        )
        .expect("load");
        assert_eq!(settings.mode, Mode::Audio);
        assert_eq!(settings.lum_max, 60);
        assert_eq!(settings.smoothing, 0.2);
        assert_eq!(settings.audio_hue, AudioHue::Fixed);
        assert!(settings.invert_zones);
    }

    #[test]
    fn json_round_trip() {
        let source = Settings {
            mode: Mode::Screen,
            lum_max: 42,
            ..Settings::default()
        };
        let text = serde_json::to_string(&source).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.mode, Mode::Screen);
        assert_eq!(back.lum_max, 42);
    }
}
