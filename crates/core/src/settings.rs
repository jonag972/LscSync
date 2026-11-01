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
    /// A color chosen by hand, nothing captured.
    #[serde(rename = "fixed")]
    Fixed,
}

impl Mode {
    pub const ALL: [Self; 4] = [Self::ScreenZones, Self::Screen, Self::Audio, Self::Fixed];
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioHue {
    #[default]
    #[serde(rename = "cycle", alias = "defile")]
    Cycle,
    #[serde(rename = "fixed", alias = "fixe")]
    Fixed,
}

/// What multi-zone lights show in music mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MusicStyle {
    /// Every zone pulses with the bass.
    #[default]
    Pulse,
    /// A meter: as many zones lit as the sound is loud.
    Level,
}

/// What the lights show once the sync stops.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhenStopped {
    /// Turned off.
    Off,
    /// Plain white.
    #[default]
    White,
    /// What they showed before the sync: off, or white at the same
    /// brightness. A color or a scene can't be read back: white instead.
    Previous,
}

/// What the screen modes do when the picture goes dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DarkScenes {
    /// The lights go dark with the picture.
    #[default]
    Follow,
    /// The lights keep the last color they had.
    Hold,
    /// The lights keep that color and fade out slowly.
    Dim,
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
    pub music_style: MusicStyle,
    /// Music mode, Pulse: the frequency range treated as the bass, in Hz.
    pub music_bass_min_hz: u16,
    pub music_bass_max_hz: u16,
    pub when_stopped: WhenStopped,
    /// Before `when_stopped`: white or not. Read once, then dropped (see
    /// [`Settings::upgrade`]).
    #[serde(default, skip_serializing, alias = "retour_blanc")]
    pub restore_white: Option<bool>,
    #[serde(alias = "inverser_zones")]
    pub invert_zones: bool,
    /// Screen modes: the lights never go below this brightness (0 to 1).
    #[serde(rename = "screen_min_brightness")]
    pub screen_min_light: f32,
    pub dark_scenes: DarkScenes,
    /// How much a vivid pixel outweighs a gray one in a zone's color.
    pub vivid_priority: f32,
    /// Widens the brightness gaps between zones (0 to 1).
    pub contrast: f32,
    /// Screen captures per second.
    pub frame_rate: u8,
    /// Music mode: how long the lights wait so they land with the sound, in
    /// milliseconds. None: automatic (a delay for Bluetooth speakers).
    #[serde(rename = "music_delay_ms")]
    pub music_delay: Option<u16>,
    /// Fixed color mode: hue (0 to 359) and saturation (0 for white).
    pub fixed_hue: u16,
    pub fixed_saturation: f32,
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
            music_style: MusicStyle::Pulse,
            music_bass_min_hz: 20,
            music_bass_max_hz: 250,
            when_stopped: WhenStopped::White,
            restore_white: None,
            invert_zones: false,
            screen_min_light: 0.0,
            dark_scenes: DarkScenes::Follow,
            vivid_priority: 4.3,
            contrast: 0.0,
            frame_rate: 25,
            music_delay: None,
            fixed_hue: 30,
            fixed_saturation: 1.0,
        }
    }
}

impl Settings {
    /// Brings settings from an older version up to date: "white when
    /// stopped" turned off left the lights alone, now "as before".
    pub fn upgrade(&mut self) {
        if self.restore_white.take() == Some(false) {
            self.when_stopped = WhenStopped::Previous;
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
        assert_eq!(json["music_style"], "pulse");
        assert_eq!(json["music_bass_min_hz"], 20);
        assert_eq!(json["music_bass_max_hz"], 250);
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
    fn old_profiles_get_the_music_defaults() {
        let settings: Settings =
            serde_json::from_str(r#"{"mode": "music", "music_offset_zones": true}"#).expect("load");
        assert_eq!(settings.music_style, MusicStyle::Pulse);
        assert_eq!(settings.music_bass_min_hz, 20);
        assert_eq!(settings.music_bass_max_hz, 250);
    }

    #[test]
    fn restore_white_becomes_when_stopped() {
        let read = |json: &str| {
            let mut settings: Settings = serde_json::from_str(json).expect("load");
            settings.upgrade();
            settings
        };
        assert_eq!(
            read(r#"{"restore_white": true}"#).when_stopped,
            WhenStopped::White
        );
        assert_eq!(
            read(r#"{"retour_blanc": false}"#).when_stopped,
            WhenStopped::Previous
        );
        assert_eq!(read("{}").when_stopped, WhenStopped::White);
        let json = serde_json::to_value(read(r#"{"restore_white": false}"#)).unwrap();
        assert_eq!(json["when_stopped"], "previous");
        assert!(json.get("restore_white").is_none());
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
