//! Audio capture not available on this platform.
//!
//! Windows: WASAPI loopback coming. Same API as `MacAudioCapture`, so the
//! worker has a single type.

use std::time::Duration;

use super::AudioError;

pub struct UnsupportedAudioCapture;

impl UnsupportedAudioCapture {
    pub fn new() -> Result<Self, AudioError> {
        Err(AudioError::Capture(
            "Music mode is not available on this platform yet".to_owned(),
        ))
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        Err(AudioError::Capture(
            "Music mode is not available on this platform yet".to_owned(),
        ))
    }

    pub fn stop(&mut self) {}

    pub fn read_block_timeout(&self, _: Duration) -> Result<Option<Vec<f32>>, AudioError> {
        Ok(None)
    }
}
