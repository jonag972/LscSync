//! Colors: RGB/HSV conversions, dominant color of a zone, downscaled image.

/// Weight of a gray or black pixel relative to a saturated one.
pub const GRAY_WEIGHT: f32 = 0.30;

/// Gain applied to brightness before the global dimmer.
pub const LIGHTNESS_GAIN: f32 = 1.35;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Rgb {
    pub const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
    };

    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }

    /// RGB -> HSV conversion, components between 0 and 1.
    pub fn to_hsv(self) -> Hsv {
        let max = self.r.max(self.g).max(self.b);
        let min = self.r.min(self.g).min(self.b);
        let delta = max - min;

        let h = if delta == 0.0 {
            0.0
        } else if max == self.r {
            ((self.g - self.b) / delta).rem_euclid(6.0)
        } else if max == self.g {
            (self.b - self.r) / delta + 2.0
        } else {
            (self.r - self.g) / delta + 4.0
        };

        Hsv {
            h: h / 6.0,
            s: if max == 0.0 { 0.0 } else { delta / max },
            v: max,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Hsv {
    pub h: f32,
    pub s: f32,
    pub v: f32,
}

impl Hsv {
    pub const fn new(h: f32, s: f32, v: f32) -> Self {
        Self { h, s, v }
    }

    /// HSV -> RGB conversion, components between 0 and 1.
    pub fn to_rgb(self) -> Rgb {
        let h = self.h.rem_euclid(1.0) * 6.0;
        let i = h.floor() as i32;
        let f = h - i as f32;
        let p = self.v * (1.0 - self.s);
        let q = self.v * (1.0 - self.s * f);
        let t = self.v * (1.0 - self.s * (1.0 - f));

        let (r, g, b) = match i.rem_euclid(6) {
            0 => (self.v, t, p),
            1 => (q, self.v, p),
            2 => (p, self.v, t),
            3 => (p, q, self.v),
            4 => (t, p, self.v),
            _ => (self.v, p, q),
        };

        Rgb::new(r, g, b)
    }
}

/// Downscaled RGB image, stored row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<Rgb>,
}

impl Image {
    pub fn new(width: usize, height: usize, pixels: Vec<Rgb>) -> Self {
        assert_eq!(
            pixels.len(),
            width * height,
            "buffer size does not match {width}x{height}"
        );
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Top band: the panels sit above the screen.
    pub fn top_band(&self, fraction: f32) -> Self {
        if self.height == 0 {
            return self.clone();
        }
        let rows = ((self.height as f32 * fraction).round() as usize).clamp(1, self.height);
        Self {
            width: self.width,
            height: rows,
            pixels: self.pixels[..rows * self.width].to_vec(),
        }
    }

    /// Pixels inside a zone (at least one).
    pub fn region(&self, zone: crate::layout::Zone) -> Vec<Rgb> {
        if self.width == 0 || self.height == 0 {
            return Vec::new();
        }
        let span = |start: f32, size: f32, total: usize| {
            let first = ((start * total as f32).floor() as usize).min(total - 1);
            let end = (((start + size) * total as f32).ceil() as usize).clamp(first + 1, total);
            first..end
        };
        let columns = span(zone.x, zone.w, self.width);
        span(zone.y, zone.h, self.height)
            .flat_map(|y| {
                let row = y * self.width;
                self.pixels[row + columns.start..row + columns.end]
                    .iter()
                    .copied()
            })
            .collect()
    }

    /// Vertical bands from left to right, like `numpy.array_split`.
    pub fn split_zones(&self, n: usize) -> Vec<Self> {
        assert!(n > 0, "at least one zone");
        let base = self.width / n;
        let remainder = self.width % n;
        let mut zones = Vec::with_capacity(n);
        let mut start = 0;

        for i in 0..n {
            let width = base + usize::from(i < remainder);
            let mut pixels = Vec::with_capacity(width * self.height);
            for y in 0..self.height {
                let row = y * self.width;
                pixels.extend_from_slice(&self.pixels[row + start..row + start + width]);
            }
            zones.push(Self {
                width,
                height: self.height,
                pixels,
            });
            start += width;
        }

        zones
    }
}

/// Dominant color of a zone, with the default vividness priority.
pub fn dominant_color(pixels: &[Rgb]) -> Rgb {
    dominant_color_with(pixels, (1.0 + GRAY_WEIGHT) / GRAY_WEIGHT)
}

/// Dominant color of a zone.
///
/// Average weighted by vividness (saturation × brightness): black bars, text
/// and gray backgrounds no longer wash out the hue. A fully vivid pixel
/// weighs `vivid_priority` times a gray one (1: a plain average). Brightness
/// itself stays that of the whole zone.
pub fn dominant_color_with(pixels: &[Rgb], vivid_priority: f32) -> Rgb {
    let gray_weight = 1.0 / (vivid_priority - 1.0).max(0.001);
    if pixels.is_empty() {
        return Rgb::BLACK;
    }

    let mut weight_sum = 0.0_f32;
    let mut weighted = [0.0_f32; 3];
    let mut plain = [0.0_f32; 3];

    for pixel in pixels {
        let max = pixel.r.max(pixel.g).max(pixel.b);
        let min = pixel.r.min(pixel.g).min(pixel.b);
        let weight = (max - min) + gray_weight;

        weight_sum += weight;
        weighted[0] += pixel.r * weight;
        weighted[1] += pixel.g * weight;
        weighted[2] += pixel.b * weight;
        plain[0] += pixel.r;
        plain[1] += pixel.g;
        plain[2] += pixel.b;
    }

    let count = pixels.len() as f32;
    let mut color = [
        weighted[0] / weight_sum,
        weighted[1] / weight_sum,
        weighted[2] / weight_sum,
    ];
    let lightness = (plain[0] / count)
        .max(plain[1] / count)
        .max(plain[2] / count);
    let max_color = color[0].max(color[1]).max(color[2]);

    if max_color > 0.0 {
        let scale = lightness / max_color;
        color[0] *= scale;
        color[1] *= scale;
        color[2] *= scale;
    }

    Rgb::new(color[0], color[1], color[2])
}

/// RGB -> HSV ready for the lamp: boosted saturation, brightness gain, then
/// the global dimmer (`lum_max` as a percentage).
pub fn to_lamp(rgb: Rgb, boost_sat: f32, lum_max: f32) -> Hsv {
    let hsv = rgb.to_hsv();
    Hsv {
        h: hsv.h,
        s: (hsv.s * boost_sat + 0.05).min(1.0),
        v: (hsv.v * LIGHTNESS_GAIN).min(1.0) * (lum_max / 100.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: usize, height: usize, rgb: Rgb) -> Image {
        Image::new(width, height, vec![rgb; width * height])
    }

    fn hue(rgb: Rgb) -> f32 {
        rgb.to_hsv().h
    }

    #[test]
    fn solid_color() {
        let red = image(4, 4, Rgb::new(1.0, 0.0, 0.0));
        let dominant = dominant_color(&red.pixels);
        assert!((dominant.r - 1.0).abs() < 1e-6);
        assert!(dominant.g.abs() < 1e-6);
        assert!(dominant.b.abs() < 1e-6);
    }

    #[test]
    fn black_does_not_wash_out_the_hue() {
        let mut img = image(10, 10, Rgb::BLACK);
        for x in 0..10 {
            img.pixels[x] = Rgb::new(0.0, 0.0, 1.0);
        }
        let dominant = dominant_color(&img.pixels);
        assert!(dominant.b > 0.0);
        assert!(dominant.r.abs() < 1e-6);
        assert!(dominant.g.abs() < 1e-6);
    }

    #[test]
    fn brightness_follows_the_whole_zone() {
        let mut img = image(10, 10, Rgb::BLACK);
        for x in 0..10 {
            img.pixels[x] = Rgb::new(0.0, 0.0, 1.0);
        }
        let dominant = dominant_color(&img.pixels);
        assert!((dominant.b.max(dominant.r).max(dominant.g) - 0.1).abs() < 1e-5);
    }

    #[test]
    fn zones_left_to_right() {
        let mut img = image(9, 2, Rgb::BLACK);
        for y in 0..2 {
            for x in 0..9 {
                img.pixels[y * 9 + x] = match x / 3 {
                    0 => Rgb::new(1.0, 0.0, 0.0),
                    1 => Rgb::new(0.0, 1.0, 0.0),
                    _ => Rgb::new(0.0, 0.0, 1.0),
                };
            }
        }
        let hues: Vec<i32> = img
            .split_zones(3)
            .iter()
            .map(|zone| (hue(dominant_color(&zone.pixels)) * 360.0).round() as i32)
            .collect();
        assert_eq!(hues, vec![0, 120, 240]);
    }

    #[test]
    fn top_band_ignores_the_bottom_of_the_screen() {
        let mut img = image(4, 10, Rgb::new(0.0, 0.0, 1.0));
        for y in 0..3 {
            for x in 0..4 {
                img.pixels[y * 4 + x] = Rgb::new(1.0, 0.0, 0.0);
            }
        }
        let band = img.top_band(0.3);
        assert_eq!(band.height, 3);
        let dominant = dominant_color(&band.pixels);
        assert!((dominant.r - 1.0).abs() < 1e-6);
        assert!(dominant.b.abs() < 1e-6);
    }

    #[test]
    fn global_dimmer_scales_brightness() {
        let full = to_lamp(Rgb::new(0.5, 0.25, 0.25), 2.0, 100.0);
        let reduced = to_lamp(Rgb::new(0.5, 0.25, 0.25), 2.0, 40.0);
        assert!((reduced.v - full.v * 0.4).abs() < 1e-6);
        assert!((reduced.s - full.s).abs() < 1e-6);
        assert!((full.s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn dimmer_works_on_dark_images_too() {
        let dark = Rgb::new(0.1, 0.05, 0.05);
        assert!(to_lamp(dark, 1.9, 50.0).v < to_lamp(dark, 1.9, 100.0).v);
    }
}
