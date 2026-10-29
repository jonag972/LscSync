//! Bass detection by FFT: the peak of the 20-250 Hz band becomes the
//! brightness, while the hue cycles or stays fixed.
//!
//! Fast attack / 8× slower release, a reference peak that decays over 1-2 s,
//! and an adjustable brightness floor.

use std::f32::consts::PI;
use std::sync::Arc;
use std::time::Instant;

use lscsync_core::Hsv;
use lscsync_core::protocol::NB_ZONES;
use realfft::RealFftPlanner;
use realfft::num_complex::Complex32;

use super::{BLOCK_SIZE, SAMPLE_RATE};

/// Frequency range treated as "the bass".
const BASS_RANGE_HZ: (f32, f32) = (20.0, 250.0);

/// Decay of the reference peak (~1-2 s to fall back to silence).
const PEAK_DECAY: f32 = 0.99;

pub struct BassProcessor {
    fft: Arc<dyn realfft::RealToComplex<f32>>,
    /// Hann window applied before the FFT.
    window: Vec<f32>,
    /// Input buffer, pre-multiplied by the window.
    input: Vec<f32>,
    /// Output spectrum (complex).
    spectrum: Vec<Complex32>,
    /// Reference peak: the loudest level seen recently.
    peak: f32,
    /// Smoothed brightness (fast attack, slow release).
    smoothed: f32,
    /// For the cycling hue.
    started_at: Instant,
    // Settings refreshed by the worker on every frame.
    pub reactivity: f32,
    pub min_brightness: f32,
    pub max_brightness: f32,
    pub hue_speed: f32,
}

impl BassProcessor {
    pub fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(BLOCK_SIZE);
        let output_len = BLOCK_SIZE / 2 + 1;

        // Hann window: reduces spectral leakage.
        let window: Vec<f32> = (0..BLOCK_SIZE)
            .map(|i| {
                let x = i as f32 / BLOCK_SIZE as f32;
                0.5 - 0.5 * (2.0 * PI * x).cos()
            })
            .collect();

        Self {
            input: vec![0.0; BLOCK_SIZE],
            spectrum: vec![Complex32::new(0.0, 0.0); output_len],
            fft,
            window,
            peak: 1e-6,
            smoothed: 0.0,
            started_at: Instant::now(),
            reactivity: 0.4,
            min_brightness: 0.02,
            max_brightness: 100.0,
            hue_speed: 25.0,
        }
    }

    /// Analyzes a block of mono samples and returns the bass level (0.0 to
    /// 1.0, normalized by the reference peak).
    fn bass_level(&mut self, samples: &[f32]) -> f32 {
        if samples.len() != BLOCK_SIZE {
            return self.smoothed;
        }

        // Apply the Hann window.
        for (i, sample) in samples.iter().enumerate() {
            self.input[i] = sample * self.window[i];
        }

        // Real -> complex FFT.
        self.fft
            .process(&mut self.input, &mut self.spectrum)
            .expect("FFT");

        // Frequency of each spectrum bin.
        let bin_width = SAMPLE_RATE / BLOCK_SIZE as f32;
        let (bass_lo, bass_hi) = BASS_RANGE_HZ;
        let lo_bin = (bass_lo / bin_width).ceil() as usize;
        let hi_bin = ((bass_hi / bin_width).ceil() as usize).min(self.spectrum.len());

        // Maximum amplitude in the bass range (the peak, not the mean: kicks
        // stand out better).
        let mut bass = 0.0_f32;
        for bin in &self.spectrum[lo_bin..hi_bin] {
            let amplitude = bin.norm();
            if amplitude > bass {
                bass = amplitude;
            }
        }

        // Update the reference peak (it decays on its own).
        self.peak = (self.peak * PEAK_DECAY).max(bass).max(1e-9);
        (bass / self.peak).min(1.0)
    }

    /// Processes a block of samples and returns the colors for the panels.
    ///
    /// `hue`: the hue to apply (computed by the caller from the cycling or
    /// fixed setting).
    /// `offset_zones`: if true, shifts the hue by 1/3 on each zone.
    pub fn process(&mut self, samples: &[f32], hue: f32, offset_zones: bool) -> [Hsv; 3] {
        let level = self.bass_level(samples);

        // Target brightness: floor + (1 - floor) × level.
        let target = self.min_brightness + (1.0 - self.min_brightness) * level;

        // Fast attack, 8× slower release (fixed 1:8 ratio).
        let alpha_up = self.reactivity;
        let alpha_down = alpha_up / 8.0;
        let alpha = if target > self.smoothed {
            alpha_up
        } else {
            alpha_down
        };
        self.smoothed = self.smoothed * (1.0 - alpha) + target * alpha;

        // Global dimmer.
        let value = (self.smoothed * self.max_brightness / 100.0).min(1.0);

        // Hue: the same everywhere, or shifted by 1/3 per zone.
        if offset_zones {
            core::array::from_fn(|i| Hsv::new((hue + i as f32 / NB_ZONES as f32) % 1.0, 1.0, value))
        } else {
            [Hsv::new(hue, 1.0, value); NB_ZONES]
        }
    }

    /// Cycling hue: a full turn every `hue_speed` seconds.
    pub fn cycling_hue(&self) -> f32 {
        let elapsed = self.started_at.elapsed().as_secs_f32();
        (elapsed / self.hue_speed.max(5.0)) % 1.0
    }
}

impl Default for BassProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_gives_the_floor() {
        let mut processor = BassProcessor::new();
        processor.min_brightness = 0.02;
        processor.max_brightness = 100.0;

        let silence = vec![0.0; BLOCK_SIZE];
        for _ in 0..50 {
            // Run several frames so the smoothing converges.
            let _ = processor.process(&silence, 0.5, false);
        }
        let zones = processor.process(&silence, 0.5, false);
        // Once converged, brightness sits at the floor.
        assert!(zones[0].v < 0.1, "brightness = {}", zones[0].v);
    }

    #[test]
    fn loud_bass_gives_light() {
        let mut processor = BassProcessor::new();
        processor.min_brightness = 0.02;
        processor.max_brightness = 100.0;
        processor.reactivity = 0.9;

        // Loud 100 Hz sine wave (in the bass range).
        let bass_wave: Vec<f32> = (0..BLOCK_SIZE)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE;
                (2.0 * PI * 100.0 * t).sin() * 0.8
            })
            .collect();

        let zones = processor.process(&bass_wave, 0.5, false);
        assert!(zones[0].v > 0.5, "brightness = {}", zones[0].v);
        assert_eq!(zones[0].h, 0.5);
    }

    #[test]
    fn offset_zones() {
        let mut processor = BassProcessor::new();
        let silence = vec![0.0; BLOCK_SIZE];
        let zones = processor.process(&silence, 0.0, true);
        // Hues shifted by 1/3.
        assert!((zones[1].h - 1.0 / 3.0).abs() < 0.01);
        assert!((zones[2].h - 2.0 / 3.0).abs() < 0.01);
    }
}
