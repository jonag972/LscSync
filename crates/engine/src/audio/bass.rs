//! Bass detection by FFT: the peak of the bass band (20-250 Hz by default)
//! becomes the brightness, while the hue cycles or stays fixed. The overall
//! volume (RMS) is measured too, for the panel meter.
//!
//! Fast attack / 8× slower release, a reference peak that decays over 1-2 s,
//! and an adjustable brightness floor.

use std::f32::consts::PI;
use std::sync::Arc;
use std::time::Instant;

use lscsync_core::Hsv;
use realfft::RealFftPlanner;
use realfft::num_complex::Complex32;

use super::{BLOCK_SIZE, SAMPLE_RATE};

/// Lowest frequency the bass range can start at.
const BASS_MIN_HZ: f32 = 10.0;

/// RMS under which the sound counts as silence for the meter (about
/// -50 dBFS): background noise must not light a panel.
const VOLUME_FLOOR: f32 = 0.003;

/// Decay of the reference peak (~1-2 s to fall back to silence).
const PEAK_DECAY: f32 = 0.99;

/// Bands of the app's equalizer, log-spaced from `EQ_MIN_HZ` to Nyquist.
pub const EQ_BANDS: usize = 24;

/// Lowest frequency of the equalizer.
const EQ_MIN_HZ: f32 = 20.0;

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
    /// Reference peak of the volume (RMS), decaying like `peak`.
    volume_peak: f32,
    /// Smoothed volume, 0.0 to 1.0 (fast attack, slow release).
    volume: f32,
    /// Bin range (lo, hi) of each equalizer band.
    band_bins: Vec<(usize, usize)>,
    /// Latest level of each equalizer band (0.0 to 1.0).
    bands: Vec<f32>,
    /// Reference peak of the whole spectrum, for the bands (~1-2 s decay).
    bands_peak: f32,
    /// For the cycling hue.
    started_at: Instant,
    // Settings refreshed by the worker on every frame.
    pub reactivity: f32,
    pub min_brightness: f32,
    pub max_brightness: f32,
    pub hue_speed: f32,
    /// Frequency range treated as the bass, in Hz.
    pub bass_range: (f32, f32),
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

        // Log-spaced band edges from EQ_MIN_HZ to Nyquist, as bin ranges.
        let bin_width = SAMPLE_RATE / BLOCK_SIZE as f32;
        let nyquist = SAMPLE_RATE / 2.0;
        let ratio = (nyquist / EQ_MIN_HZ).powf(1.0 / EQ_BANDS as f32);
        let band_bins = (0..EQ_BANDS)
            .scan(EQ_MIN_HZ, |hz, _| {
                let lo_hz = *hz;
                *hz *= ratio;
                Some((lo_hz, *hz))
            })
            .map(|(lo_hz, hi_hz)| {
                let lo = ((lo_hz / bin_width).ceil() as usize).min(output_len - 1);
                let hi = (((hi_hz / bin_width).ceil() as usize).max(lo + 1)).min(output_len);
                (lo, hi)
            })
            .collect();

        Self {
            input: vec![0.0; BLOCK_SIZE],
            spectrum: vec![Complex32::new(0.0, 0.0); output_len],
            fft,
            window,
            peak: 1e-6,
            smoothed: 0.0,
            volume_peak: VOLUME_FLOOR,
            volume: 0.0,
            band_bins,
            bands: vec![0.0; EQ_BANDS],
            bands_peak: 1e-6,
            started_at: Instant::now(),
            reactivity: 0.4,
            min_brightness: 0.02,
            max_brightness: 100.0,
            hue_speed: 25.0,
            bass_range: (20.0, 250.0),
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

        // Frequency of each spectrum bin. The range is put back in order and
        // always keeps at least one bin, whatever the settings say.
        let bin_width = SAMPLE_RATE / BLOCK_SIZE as f32;
        let nyquist = SAMPLE_RATE / 2.0;
        let (low, high) = self.bass_range;
        let bass_lo = low.min(high).clamp(BASS_MIN_HZ, nyquist);
        let bass_hi = low.max(high).clamp(bass_lo, nyquist);
        let lo_bin = ((bass_lo / bin_width).ceil() as usize).min(self.spectrum.len() - 1);
        let hi_bin = ((bass_hi / bin_width).ceil() as usize)
            .max(lo_bin + 1)
            .min(self.spectrum.len());

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

        // Equalizer bands: the max amplitude of each band, normalized by a
        // decaying reference peak over the whole spectrum.
        let mut all = 0.0_f32;
        for bin in &self.spectrum {
            all = all.max(bin.norm());
        }
        self.bands_peak = (self.bands_peak * PEAK_DECAY).max(all).max(1e-9);
        for (band, &(lo, hi)) in self.bands.iter_mut().zip(&self.band_bins) {
            let mut level = 0.0_f32;
            for bin in &self.spectrum[lo..hi] {
                level = level.max(bin.norm());
            }
            *band = (level / self.bands_peak).min(1.0);
        }

        (bass / self.peak).min(1.0)
    }

    /// Levels of the equalizer bands, low to high frequency, 0.0 to 1.0.
    pub fn bands(&self) -> &[f32] {
        &self.bands
    }

    /// Overall volume of a block (RMS), normalized by a decaying reference
    /// peak: 0.0 to 1.0, 0.0 under the silence floor.
    fn volume_level(&mut self, samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        self.volume_peak = (self.volume_peak * PEAK_DECAY).max(rms).max(VOLUME_FLOOR);
        if rms < VOLUME_FLOOR {
            0.0
        } else {
            (rms / self.volume_peak).min(1.0)
        }
    }

    /// Fast attack, 8× slower release (fixed 1:8 ratio).
    fn follow(&self, current: f32, target: f32) -> f32 {
        let alpha = if target > current {
            self.reactivity
        } else {
            self.reactivity / 8.0
        };
        current * (1.0 - alpha) + target * alpha
    }

    /// Processes a block of samples and returns the color every panel
    /// pulses with. The volume for [`Self::volume`] is updated too.
    ///
    /// `hue`: the hue to apply (computed by the caller from the cycling or
    /// fixed setting).
    pub fn process(&mut self, samples: &[f32], hue: f32) -> Hsv {
        let level = self.bass_level(samples);
        let volume = self.volume_level(samples);
        self.volume = self.follow(self.volume, volume);

        // Target brightness: floor + (1 - floor) × level.
        let target = self.min_brightness + (1.0 - self.min_brightness) * level;
        self.smoothed = self.follow(self.smoothed, target);

        // Global dimmer.
        let value = (self.smoothed * self.max_brightness / 100.0).min(1.0);
        Hsv::new(hue, 1.0, value)
    }

    /// Smoothed overall volume, 0.0 to 1.0: how full the panel meter is.
    pub fn volume(&self) -> f32 {
        self.volume
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
            let _ = processor.process(&silence, 0.5);
        }
        let color = processor.process(&silence, 0.5);
        // Once converged, brightness sits at the floor.
        assert!(color.v < 0.1, "brightness = {}", color.v);
        assert_eq!(processor.volume(), 0.0);
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

        let color = processor.process(&bass_wave, 0.5);
        assert!(color.v > 0.5, "brightness = {}", color.v);
        assert_eq!(color.h, 0.5);
    }

    #[test]
    fn bands_cover_the_spectrum() {
        let mut processor = BassProcessor::new();
        assert_eq!(processor.bands().len(), EQ_BANDS);
        assert!(processor.bands().iter().all(|&b| (0.0..=1.0).contains(&b)));

        // A 100 Hz sine: its band reads high, the far bands read low.
        let wave: Vec<f32> = (0..BLOCK_SIZE)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE;
                (2.0 * PI * 100.0 * t).sin() * 0.8
            })
            .collect();
        for _ in 0..50 {
            let _ = processor.process(&wave, 0.5);
        }
        let bands = processor.bands();
        let loudest = bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        // 100 Hz falls in the first few (lowest) bands.
        assert!(loudest < EQ_BANDS / 3, "loudest band = {loudest}");
        assert!(bands[loudest] > 0.9, "level = {}", bands[loudest]);

        // Silence: every band falls back to zero.
        let silence = vec![0.0; BLOCK_SIZE];
        for _ in 0..500 {
            let _ = processor.process(&silence, 0.5);
        }
        assert!(processor.bands().iter().all(|&b| b < 0.01));
    }

    fn sine(hz: f32, amplitude: f32) -> Vec<f32> {
        (0..BLOCK_SIZE)
            .map(|i| (2.0 * PI * hz * i as f32 / SAMPLE_RATE).sin() * amplitude)
            .collect()
    }

    #[test]
    fn the_bass_range_is_adjustable() {
        let wave = sine(100.0, 0.8);
        let mut inside = BassProcessor::new();
        let mut outside = BassProcessor::new();
        outside.bass_range = (300.0, 500.0);
        // A 400 Hz tone sets the reference peak of the 300-500 Hz range.
        let _ = outside.bass_level(&sine(400.0, 0.8));
        assert!(inside.bass_level(&wave) > 0.9);
        let level = outside.bass_level(&wave);
        assert!(level < 0.1, "outside the range: {level}");
    }

    #[test]
    fn a_reversed_bass_range_is_put_back_in_order() {
        let mut processor = BassProcessor::new();
        processor.bass_range = (500.0, 300.0);
        let _ = processor.bass_level(&sine(400.0, 0.8));
        assert!(processor.bass_level(&sine(400.0, 0.8)) > 0.9);
    }

    #[test]
    fn volume_follows_the_sound_not_only_the_bass() {
        let mut processor = BassProcessor::new();
        processor.reactivity = 0.9;
        // A voice-like 1 kHz tone, far above the bass.
        for _ in 0..10 {
            let _ = processor.process(&sine(1000.0, 0.5), 0.5);
        }
        assert!(processor.volume() > 0.8, "loud: {}", processor.volume());
        // Quieter: the meter drops, without reaching zero.
        for _ in 0..30 {
            let _ = processor.process(&sine(1000.0, 0.1), 0.5);
        }
        let quiet = processor.volume();
        assert!(quiet > 0.0 && quiet < 0.8, "quiet: {quiet}");
    }
}
