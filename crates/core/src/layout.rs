//! Where each zone of a device looks on the screen, and what each product can
//! show.
//!
//! A zone is a rectangle in screen proportions (0 to 1 from the top left
//! corner): it stays right when the resolution changes.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::profile::Device;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Zone {
    /// Smallest side, so a zone can always be grabbed again.
    pub const MIN_SIZE: f32 = 0.04;
    pub const WHOLE_SCREEN: Self = Self {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    /// Kept inside the screen, never smaller than [`Self::MIN_SIZE`].
    pub fn clamped(self) -> Self {
        let w = self.w.clamp(Self::MIN_SIZE, 1.0);
        let h = self.h.clamp(Self::MIN_SIZE, 1.0);
        Self {
            x: self.x.clamp(0.0, 1.0 - w),
            y: self.y.clamp(0.0, 1.0 - h),
            w,
            h,
        }
    }

    pub fn moved(self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            ..self
        }
        .clamped()
    }

    /// Resized from its bottom right corner.
    pub fn resized(self, dw: f32, dh: f32) -> Self {
        Self {
            w: (self.w + dw).min(1.0 - self.x),
            h: (self.h + dh).min(1.0 - self.y),
            ..self
        }
        .clamped()
    }
}

/// `count` zones side by side along the top of the screen.
pub fn top_row(count: usize, height: f32) -> Vec<Zone> {
    let width = 1.0 / count.max(1) as f32;
    (0..count)
        .map(|index| Zone {
            x: index as f32 * width,
            y: 0.0,
            w: width,
            h: height,
        })
        .collect()
}

/// How a device is driven, from what is known about its product.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Product {
    /// LSC light panel kit: one color per panel, applied instantly (a
    /// 12-segment DP 51 scene, so up to 12 panels; 6 in the kit), and an
    /// instant single color (DP 27).
    Panels,
    /// Any other light: one color (DP 24).
    Light,
}

impl Product {
    /// Recognized by the product name until the profile stores Tuya's
    /// product key.
    pub fn of(device: &Device) -> Self {
        if device
            .product
            .trim()
            .eq_ignore_ascii_case("Smart panel lights")
        {
            Self::Panels
        } else {
            Self::Light
        }
    }

    /// How many zones the device can have: one per panel for the panels.
    pub fn zone_counts(self) -> RangeInclusive<usize> {
        match self {
            Self::Panels => 1..=crate::protocol::SCENE_SEGMENTS,
            Self::Light => 1..=1,
        }
    }

    pub fn default_zone_count(self) -> usize {
        match self {
            Self::Panels => 6,
            Self::Light => 1,
        }
    }

    /// Starting layout: the panels along the top of the screen, a light on
    /// the whole screen (its dominant color).
    pub fn default_zones(self, top_height: f32) -> Vec<Zone> {
        match self {
            Self::Panels => top_row(self.default_zone_count(), top_height),
            Self::Light => vec![Zone::WHOLE_SCREEN],
        }
    }
}

impl Device {
    /// The device's zones on the screen: the saved ones, or the product's
    /// default layout.
    pub fn layout(&self, top_height: f32) -> Vec<Zone> {
        let product = Product::of(self);
        if product.zone_counts().contains(&self.zones.len()) {
            self.zones.clone()
        } else {
            product.default_zones(top_height)
        }
    }

    /// The layout with `count` zones: the first ones keep their place, new
    /// ones take their slot along the top of the screen.
    pub fn layout_with(&self, count: usize, top_height: f32) -> Vec<Zone> {
        let mut zones = self.layout(top_height);
        zones.truncate(count);
        let row = top_row(count, top_height);
        zones.extend_from_slice(&row[zones.len()..]);
        zones
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_stay_on_screen() {
        let zone = Zone {
            x: 0.9,
            y: 0.0,
            w: 0.2,
            h: 0.3,
        };
        assert_eq!(zone.moved(0.5, -1.0).x, 0.8);
        assert_eq!(zone.moved(0.5, -1.0).y, 0.0);
        let small = zone.resized(-1.0, -1.0);
        assert_eq!((small.w, small.h), (Zone::MIN_SIZE, Zone::MIN_SIZE));
        let wide = Zone { x: 0.5, ..zone }.resized(1.0, 0.0);
        assert_eq!(wide.x + wide.w, 1.0);
    }

    #[test]
    fn changing_the_panel_count_keeps_placed_zones() {
        let mut device: Device = serde_json::from_str(
            r#"{"id": "a", "name": "Panels", "local_key": "k", "product": "Smart panel lights"}"#,
        )
        .unwrap();
        assert_eq!(device.layout(0.3).len(), 6);
        let placed = Zone {
            x: 0.0,
            y: 0.5,
            w: 0.1,
            h: 0.5,
        };
        device.zones = device.layout(0.3);
        device.zones[0] = placed;
        let more = device.layout_with(8, 0.3);
        assert_eq!(more.len(), 8);
        assert_eq!(more[0], placed);
        assert_eq!(device.layout_with(2, 0.3), vec![placed, device.zones[1]]);
    }

    #[test]
    fn top_row_splits_the_width() {
        let row = top_row(6, 0.3);
        assert_eq!(row.len(), 6);
        assert!((row[5].x + row[5].w - 1.0).abs() < 1e-6);
        assert!(row.iter().all(|zone| zone.y == 0.0 && zone.h == 0.3));
    }
}
