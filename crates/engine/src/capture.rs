//! Main screen capture, downscaled for color analysis.

use lscsync_core::{Image, Rgb};
use thiserror::Error;
use xcap::{Monitor, XCapError};

/// 1 pixel in 8 in each direction: plenty for a zone color, and far cheaper
/// than a full-resolution capture.
const STEP: usize = 8;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("xcap: {0}")]
    Xcap(#[from] XCapError),
    #[error("no screen found")]
    NoMonitor,
}

pub struct ScreenCapture {
    monitor: Monitor,
}

impl ScreenCapture {
    pub fn new() -> Result<Self, CaptureError> {
        let mut monitors = Monitor::all()?;
        if monitors.is_empty() {
            return Err(CaptureError::NoMonitor);
        }
        let index = monitors
            .iter()
            .position(|monitor| monitor.is_primary().unwrap_or(false))
            .unwrap_or(0);
        Ok(Self {
            monitor: monitors.swap_remove(index),
        })
    }

    /// Captures the whole screen and downscales it (RGBA -> RGB, 1 pixel in 8).
    pub fn capture(&self) -> Result<Image, CaptureError> {
        let frame = self.monitor.capture_image()?;
        let width = frame.width() as usize;
        let height = frame.height() as usize;
        let raw = frame.as_raw();

        let out_width = width.div_ceil(STEP);
        let out_height = height.div_ceil(STEP);
        let mut pixels = Vec::with_capacity(out_width * out_height);

        for y in (0..height).step_by(STEP) {
            for x in (0..width).step_by(STEP) {
                let i = (y * width + x) * 4;
                pixels.push(Rgb::new(
                    raw[i] as f32 / 255.0,
                    raw[i + 1] as f32 / 255.0,
                    raw[i + 2] as f32 / 255.0,
                ));
            }
        }

        Ok(Image::new(out_width, out_height, pixels))
    }
}
