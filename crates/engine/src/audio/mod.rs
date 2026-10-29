//! Audio source: system sound capture and bass detection.
//!
//! macOS 13+: ScreenCaptureKit (no driver, uses the screen recording
//! permission); macOS 12: the microphone. Windows: WASAPI loopback. Linux:
//! the PulseAudio / PipeWire output monitor.

pub mod bass;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod cpal_capture;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod unsupported;

pub use bass::BassProcessor;

/// Platform capture of the sound to react to: what the speakers play
/// (ScreenCaptureKit on macOS 13+, WASAPI loopback on Windows, the output
/// monitor through PulseAudio on Linux), or the microphone on macOS 12.
#[cfg(target_os = "windows")]
pub use cpal_capture::CpalCapture as PlatformAudioCapture;

#[cfg(target_os = "linux")]
pub use linux::LinuxAudioCapture as PlatformAudioCapture;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub use unsupported::UnsupportedAudioCapture as PlatformAudioCapture;

#[cfg(target_os = "macos")]
pub enum PlatformAudioCapture {
    System(macos::MacAudioCapture),
    Microphone(cpal_capture::CpalCapture),
}

#[cfg(target_os = "macos")]
impl PlatformAudioCapture {
    pub fn new() -> Result<Self, AudioError> {
        Ok(if !macos::system_audio_available() {
            Self::Microphone(cpal_capture::CpalCapture::new()?)
        } else {
            Self::System(macos::MacAudioCapture::new()?)
        })
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        match self {
            Self::System(capture) => capture.start(),
            Self::Microphone(capture) => capture.start(),
        }
    }

    pub fn stop(&mut self) {
        match self {
            Self::System(capture) => capture.stop(),
            Self::Microphone(capture) => capture.stop(),
        }
    }

    pub fn read_block_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Option<Vec<f32>>, AudioError> {
        match self {
            Self::System(capture) => capture.read_block_timeout(timeout),
            Self::Microphone(capture) => capture.read_block_timeout(timeout),
        }
    }
}

/// Music mode can run on this system.
pub fn is_supported() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux"
    ))
}

/// Music mode listens to the microphone instead of the computer's sound
/// (macOS 12 without a virtual loopback driver such as BlackHole).
pub fn uses_microphone() -> bool {
    #[cfg(target_os = "macos")]
    return !macos::system_audio_available() && !cpal_capture::has_loopback_device();
    #[cfg(not(target_os = "macos"))]
    false
}

/// Sample rate of the captured sound.
pub const SAMPLE_RATE: f32 = 48000.0;

/// Samples per analysis block (~20 ms at 48 kHz).
pub const BLOCK_SIZE: usize = 960;

/// Turns any stream of interleaved frames into mono blocks of `BLOCK_SIZE`
/// samples at `SAMPLE_RATE`, so every platform feeds the bass detector the
/// same way whatever its native rate, channel count or buffer size.
///
/// Resampling picks the nearest sample: crude, but plenty for bass detection.
pub(crate) struct BlockBuilder {
    channels: usize,
    /// Source frames per output sample.
    step: f64,
    /// Position of the next output sample, relative to the current frame.
    next: f64,
    block: Vec<f32>,
    sender: std::sync::mpsc::Sender<Vec<f32>>,
}

impl BlockBuilder {
    pub(crate) fn new(
        channels: usize,
        sample_rate: u32,
        sender: std::sync::mpsc::Sender<Vec<f32>>,
    ) -> Self {
        Self {
            channels: channels.max(1),
            step: f64::from(sample_rate) / f64::from(SAMPLE_RATE),
            next: 0.0,
            block: Vec::with_capacity(BLOCK_SIZE),
            sender,
        }
    }

    /// Feeds interleaved samples; sends every completed block.
    pub(crate) fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.channels) {
            let mono = frame.iter().sum::<f32>() / self.channels as f32;
            while self.next < 1.0 {
                self.block.push(mono);
                if self.block.len() == BLOCK_SIZE {
                    let block = std::mem::replace(&mut self.block, Vec::with_capacity(BLOCK_SIZE));
                    let _ = self.sender.send(block);
                }
                self.next += self.step;
            }
            self.next -= 1.0;
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no screen found for audio capture")]
    NoDisplay,
    #[error("audio capture: {0}")]
    Capture(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks_for(sample_rate: u32, channels: usize, seconds: usize) -> Vec<Vec<f32>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut builder = BlockBuilder::new(channels, sample_rate, sender);
        // Uneven buffer sizes, like real audio callbacks.
        let samples = vec![0.5_f32; sample_rate as usize * channels * seconds];
        for chunk in samples.chunks(441 * channels) {
            builder.push(chunk);
        }
        drop(builder);
        receiver.iter().collect()
    }

    #[test]
    fn native_rate_gives_50_blocks_per_second() {
        let blocks = blocks_for(48_000, 2, 2);
        assert_eq!(blocks.len(), 100);
        assert!(blocks.iter().all(|block| block.len() == BLOCK_SIZE));
    }

    #[test]
    fn other_rates_are_resampled_to_48_khz() {
        for rate in [44_100, 96_000] {
            let blocks = blocks_for(rate, 2, 2).len();
            assert!(
                (99..=100).contains(&blocks),
                "{rate} Hz gave {blocks} blocks"
            );
        }
    }

    #[test]
    fn channels_are_mixed_to_mono() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut builder = BlockBuilder::new(2, 48_000, sender);
        let stereo: Vec<f32> = (0..BLOCK_SIZE).flat_map(|_| [1.0, 0.0]).collect();
        builder.push(&stereo);
        let block = receiver.try_recv().expect("one block");
        assert!(block.iter().all(|&sample| (sample - 0.5).abs() < 1e-6));
    }
}
