//! Encoding of the LSC controller's Tuya commands, without network access.
//!
//! DP 51 ("dreamlight" scene, base64), format found by trial and error:
//!
//! ```text
//! 01 | scene id | mode | speed | speed | options | 00 00 | brightness (0-100)
//!    | N × [hue (2 bytes, 0-360), saturation (0-100)]
//! ```
//!
//! With mode 00, the split option and 6 colors, the 6-panel Z-shaped kit
//! shows 3 zones of 2 panels: color 1 = right zone, color 2 = middle,
//! color 3 = left. Colors 4 to 6 are ignored but must be present.
//! See `docs/lsc-protocol.md`.

use base64::{Engine, engine::general_purpose::STANDARD};

use crate::color::Hsv;

pub const DP_POWER: &str = "20";
pub const DP_MODE: &str = "21";
pub const DP_COLOR: &str = "24";
pub const DP_SCENE: &str = "51";

pub const MODE_WHITE: &str = "white";
pub const MODE_COLOR: &str = "colour";
pub const MODE_SCENE: &str = "scene";

pub const NB_ZONES: usize = 3;

const SCENE_VERSION: u8 = 0x01;
const SCENE_ID: u8 = 0x63;
const SCENE_STATIC: u8 = 0x00;
const SCENE_SPEED: u8 = 0x32;
const SCENE_SPLIT: u8 = 0x80;
const COLORS_PER_SCENE: usize = 6;

/// DP 24 value: HHHHSSSSVVVV in hexadecimal (0-360, 0-1000, 0-1000).
pub fn dp_color(color: Hsv) -> String {
    let h = ((color.h as f64 * 360.0).round() as i64).rem_euclid(360);
    let s = ((color.s as f64 * 1000.0).round() as i64).clamp(0, 1000);
    let v = ((color.v as f64 * 1000.0).round() as i64).clamp(0, 1000);
    format!("{h:04x}{s:04x}{v:04x}")
}

/// DP 51 value showing one color per zone (3 zones).
pub fn dp_scene_zones(zones: &[Hsv; NB_ZONES], invert: bool) -> String {
    encode_scene(zones, invert)
}

fn encode_scene(zones: &[Hsv], invert: bool) -> String {
    // The controller fills zones from right to left. Without inversion we
    // send the zones reversed; with inversion, in their original order.
    let mut scene: Vec<Hsv> = if invert {
        zones.to_vec()
    } else {
        zones.iter().rev().copied().collect()
    };
    let repeat = scene.clone();
    scene.extend_from_slice(&repeat[..NB_ZONES]);

    // A single brightness for the whole scene: that of the brightest zone.
    let max_v = zones.iter().map(|c| c.v).fold(0.0_f32, f32::max);
    let luminosity = ((max_v as f64 * 100.0).round() as i64).clamp(1, 100) as u8;

    let mut bytes = Vec::with_capacity(9 + COLORS_PER_SCENE * 3);
    bytes.extend_from_slice(&[
        SCENE_VERSION,
        SCENE_ID,
        SCENE_STATIC,
        SCENE_SPEED,
        SCENE_SPEED,
        SCENE_SPLIT,
        0x00,
        0x00,
        luminosity,
    ]);

    for color in &scene {
        let hue = ((color.h as f64 * 360.0).round() as i64).rem_euclid(360) as u16;
        let saturation = ((color.s as f64 * 100.0).round() as i64).clamp(0, 100) as u8;
        bytes.extend_from_slice(&hue.to_be_bytes());
        bytes.push(saturation);
    }

    STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Hsv = Hsv::new(0.0, 1.0, 1.0);
    const GREEN: Hsv = Hsv::new(1.0 / 3.0, 1.0, 1.0);
    const BLUE: Hsv = Hsv::new(2.0 / 3.0, 1.0, 1.0);

    fn hues(value: &str) -> Vec<u16> {
        let bytes = STANDARD.decode(value).expect("valid base64");
        bytes[9..]
            .chunks(3)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect()
    }

    #[test]
    fn dp_color_hex_format() {
        assert_eq!(dp_color(RED), "000003e803e8");
        assert_eq!(dp_color(Hsv::new(2.0 / 3.0, 0.5, 0.25)), "00f001f400fa");
    }

    #[test]
    fn dp_color_hue_360_wraps_to_0() {
        assert!(dp_color(Hsv::new(1.0, 1.0, 1.0)).starts_with("0000"));
    }

    #[test]
    fn scene_validated_on_the_lamp() {
        // Frame verified on the hardware: left blue, middle green, right red.
        let expected_header = [0x01, 0x63, 0x00, 0x32, 0x32, 0x80, 0x00, 0x00, 0x64];
        let value = dp_scene_zones(&[BLUE, GREEN, RED], false);
        let bytes = STANDARD.decode(&value).expect("valid base64");
        assert_eq!(&bytes[..9], &expected_header);
        assert_eq!(hues(&value), vec![0, 120, 240, 0, 120, 240]);
    }

    #[test]
    fn invert_swaps_left_and_right() {
        assert_eq!(
            hues(&dp_scene_zones(&[BLUE, GREEN, RED], true))[..3],
            [240, 120, 0]
        );
    }

    #[test]
    fn brightness_of_the_brightest_zone() {
        let value = dp_scene_zones(
            &[
                Hsv::new(0.0, 1.0, 0.2),
                Hsv::new(0.0, 1.0, 0.7),
                Hsv::new(0.0, 1.0, 0.0),
            ],
            false,
        );
        let bytes = STANDARD.decode(value).expect("valid base64");
        assert_eq!(bytes[8], 70);
    }

    #[test]
    fn brightness_never_zero() {
        let value = dp_scene_zones(&[Hsv::new(0.0, 1.0, 0.0); 3], false);
        let bytes = STANDARD.decode(value).expect("valid base64");
        assert_eq!(bytes[8], 1);
    }
}
