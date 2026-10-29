//! Color snapshot and the colors sent to the panels.

use crate::color::Hsv;
use crate::settings::{Mode, Settings};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snapshot {
    /// 3 colors, left -> right (top band of the screen).
    pub zones: [Hsv; 3],
    /// Color of the whole top band.
    pub top: Hsv,
}

/// 1 color (DP 24) or 3 zones (DP 51) depending on the mode.
///
/// Music mode always goes through DP 51 (3 zones, identical unless offset):
/// the controller applies it instantly, while DP 24 fades.
pub fn lsc_colors(snapshot: &Snapshot, settings: &Settings) -> Vec<Hsv> {
    match settings.mode {
        Mode::ScreenZones | Mode::Audio => snapshot.zones.to_vec(),
        Mode::Screen => vec![snapshot.top],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Hsv = Hsv::new(0.0, 1.0, 1.0);
    const MIDDLE: Hsv = Hsv::new(0.3, 1.0, 1.0);
    const RIGHT: Hsv = Hsv::new(0.6, 1.0, 1.0);
    const TOP: Hsv = Hsv::new(0.1, 1.0, 1.0);

    fn snapshot() -> Snapshot {
        Snapshot {
            zones: [LEFT, MIDDLE, RIGHT],
            top: TOP,
        }
    }

    #[test]
    fn ambilight_sends_the_3_zones() {
        let settings = Settings::default();
        assert_eq!(
            lsc_colors(&snapshot(), &settings),
            vec![LEFT, MIDDLE, RIGHT]
        );
    }

    #[test]
    fn color_mode_sends_the_top_band() {
        let settings = Settings {
            mode: Mode::Screen,
            ..Settings::default()
        };
        assert_eq!(lsc_colors(&snapshot(), &settings), vec![TOP]);
    }

    #[test]
    fn music_always_uses_dp51_zones() {
        let settings = Settings {
            mode: Mode::Audio,
            ..Settings::default()
        };
        assert_eq!(
            lsc_colors(&snapshot(), &settings),
            vec![LEFT, MIDDLE, RIGHT]
        );

        let settings = Settings {
            mode: Mode::Audio,
            audio_zones: true,
            ..Settings::default()
        };
        assert_eq!(
            lsc_colors(&snapshot(), &settings),
            vec![LEFT, MIDDLE, RIGHT]
        );
    }
}
