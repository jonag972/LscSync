//! Which DP 61 command to send next to a light with several zones.
//!
//! A light takes a limited number of commands per second, and each command
//! paints one color on a list of panels (or turns them off). [`Painter`]
//! sends first the panel that looks the most different from what it should
//! show, together with every panel that should show about the same color.

use crate::color::Hsv;

/// Below this brightness a zone is turned off (the lights' minimum is 1 %).
pub const OFF: f32 = 0.04;
/// How different a panel must look before it is sent again.
const CHANGED: f32 = 0.06;
/// How close two zones must be to share a command.
const SAME: f32 = 0.03;

/// A command: a color (`None` to turn off) on some zones (0-based).
#[derive(Clone, Debug, PartialEq)]
pub struct Paint {
    pub color: Option<Hsv>,
    pub zones: Vec<usize>,
}

/// What each zone shows and should show.
#[derive(Debug, Default)]
pub struct Painter {
    /// `None` while unknown; `Some(None)` when off.
    shown: Vec<Option<Option<Hsv>>>,
    wanted: Vec<Option<Hsv>>,
}

impl Painter {
    /// The colors the zones should show now.
    pub fn want(&mut self, colors: &[Hsv]) {
        self.wanted = colors
            .iter()
            .map(|color| (color.v >= OFF).then_some(*color))
            .collect();
        if self.shown.len() != self.wanted.len() {
            self.shown = vec![None; self.wanted.len()];
        }
    }

    /// The light's state is unknown (an error, another mode): every zone is
    /// sent again.
    pub fn forget(&mut self) {
        self.shown.iter_mut().for_each(|zone| *zone = None);
    }

    /// The next command to send, or `None` when every zone looks right.
    pub fn next(&self) -> Option<Paint> {
        let worst = (0..self.wanted.len())
            .filter(|&zone| self.gap(zone) > CHANGED)
            .max_by(|&a, &b| self.gap(a).total_cmp(&self.gap(b)))?;
        let color = self.wanted[worst];
        let zones = (0..self.wanted.len())
            .filter(|&zone| match (color, self.wanted[zone]) {
                (None, None) => true,
                (Some(a), Some(b)) => distance(a, b) < SAME,
                _ => false,
            })
            .collect();
        Some(Paint { color, zones })
    }

    /// The light took `paint`.
    pub fn sent(&mut self, paint: &Paint) {
        for &zone in &paint.zones {
            if let Some(shown) = self.shown.get_mut(zone) {
                *shown = Some(paint.color);
            }
        }
    }

    fn gap(&self, zone: usize) -> f32 {
        match (self.shown[zone], self.wanted[zone]) {
            (None, _) => f32::INFINITY,
            (Some(None), None) => 0.0,
            (Some(Some(a)), Some(b)) => distance(a, b),
            _ => 1.0,
        }
    }
}

/// How different two colors look, 0 to 1: the hue counts less when the
/// colors are pale or dark.
fn distance(a: Hsv, b: Hsv) -> f32 {
    let hue = (a.h - b.h).rem_euclid(1.0);
    let hue = hue.min(1.0 - hue) * 2.0 * a.s.min(b.s) * a.v.max(b.v);
    hue.max((a.s - b.s).abs() * a.v.max(b.v))
        .max((a.v - b.v).abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Hsv = Hsv::new(0.0, 1.0, 1.0);
    const BLUE: Hsv = Hsv::new(2.0 / 3.0, 1.0, 1.0);
    const BLACK: Hsv = Hsv::new(0.0, 0.0, 0.0);

    fn send_all(painter: &mut Painter) -> Vec<Paint> {
        let mut sent = Vec::new();
        while let Some(paint) = painter.next() {
            painter.sent(&paint);
            sent.push(paint);
        }
        sent
    }

    #[test]
    fn zones_with_the_same_color_share_a_command() {
        let mut painter = Painter::default();
        painter.want(&[RED, BLUE, RED, BLACK, BLACK, RED]);
        let sent = send_all(&mut painter);
        assert_eq!(sent.len(), 3);
        assert!(sent.contains(&Paint {
            color: Some(RED),
            zones: vec![0, 2, 5]
        }));
        assert!(sent.contains(&Paint {
            color: None,
            zones: vec![3, 4]
        }));
    }

    #[test]
    fn only_what_changed_is_sent_again() {
        let mut painter = Painter::default();
        painter.want(&[RED, BLUE, RED]);
        send_all(&mut painter);
        assert_eq!(painter.next(), None);
        painter.want(&[RED, RED, Hsv::new(0.0, 1.0, 0.98)]);
        assert_eq!(
            send_all(&mut painter),
            vec![Paint {
                color: Some(RED),
                zones: vec![0, 1, 2]
            }]
        );
    }

    #[test]
    fn the_most_different_zone_goes_first() {
        let mut painter = Painter::default();
        painter.want(&[RED, RED]);
        send_all(&mut painter);
        painter.want(&[Hsv::new(0.0, 1.0, 0.8), BLUE]);
        assert_eq!(painter.next().map(|paint| paint.zones), Some(vec![1]));
    }

    #[test]
    fn forgetting_sends_everything_again() {
        let mut painter = Painter::default();
        painter.want(&[RED, BLUE]);
        send_all(&mut painter);
        painter.forget();
        assert_eq!(send_all(&mut painter).len(), 2);
    }
}
