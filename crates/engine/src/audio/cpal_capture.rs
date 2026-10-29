//! Sound capture through `cpal`.
//!
//! - Windows: what the speakers play (WASAPI loopback: an input stream on the
//!   default output device).
//! - macOS 12: a virtual loopback device if the user installed one (BlackHole,
//!   Soundflower, Loopback), else the microphone. System audio capture needs
//!   macOS 13 or such a driver; the microphone hears the room's music instead.
//!
//! Same API as `MacAudioCapture`.

use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

use super::{AudioError, BlockBuilder};

pub struct CpalCapture {
    stream: Option<cpal::Stream>,
    receiver: Option<Receiver<Vec<f32>>>,
}

impl CpalCapture {
    pub fn new() -> Result<Self, AudioError> {
        Ok(Self {
            stream: None,
            receiver: None,
        })
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        if self.stream.is_some() {
            return Ok(());
        }
        let host = cpal::default_host();
        let (device, config) = if cfg!(target_os = "windows") {
            let device = host
                .default_output_device()
                .ok_or_else(|| AudioError::Capture("no output device".to_owned()))?;
            let config = device
                .default_output_config()
                .map_err(|e| capture_error("output format", e))?;
            (device, config)
        } else {
            let device = loopback_device(&host)
                .or_else(|| host.default_input_device())
                .ok_or_else(|| AudioError::Capture("no microphone".to_owned()))?;
            let config = device
                .default_input_config()
                .map_err(|e| capture_error("microphone format", e))?;
            (device, config)
        };

        let (sender, receiver) = mpsc::channel();
        let builder =
            BlockBuilder::new(usize::from(config.channels()), config.sample_rate(), sender);
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => input_stream::<f32>(&device, config.into(), builder),
            cpal::SampleFormat::I16 => input_stream::<i16>(&device, config.into(), builder),
            cpal::SampleFormat::I32 => input_stream::<i32>(&device, config.into(), builder),
            cpal::SampleFormat::U16 => input_stream::<u16>(&device, config.into(), builder),
            other => Err(format!("unsupported sample format {other:?}")),
        }
        .map_err(|e| capture_error("input stream", e))?;
        stream.play().map_err(|e| capture_error("start", e))?;

        self.stream = Some(stream);
        self.receiver = Some(receiver);
        Ok(())
    }

    pub fn stop(&mut self) {
        self.stream = None;
        self.receiver = None;
    }

    /// `Ok(None)`: no block within `timeout` (Windows sends nothing while the
    /// output is silent). Only a closed channel is an error.
    pub fn read_block_timeout(&self, timeout: Duration) -> Result<Option<Vec<f32>>, AudioError> {
        use std::sync::mpsc::RecvTimeoutError;
        let receiver = self
            .receiver
            .as_ref()
            .ok_or_else(|| AudioError::Capture("audio capture not started".to_owned()))?;
        match receiver.recv_timeout(timeout) {
            Ok(block) => Ok(Some(block)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err(AudioError::Capture("audio stream interrupted".to_owned()))
            }
        }
    }
}

impl Drop for CpalCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Names of virtual devices that carry the computer's own sound.
const LOOPBACK_DEVICES: [&str; 3] = ["BlackHole", "Soundflower", "Loopback Audio"];

/// A virtual loopback device is installed (macOS 12 then hears the computer's
/// sound rather than the room).
#[cfg(target_os = "macos")]
pub fn has_loopback_device() -> bool {
    loopback_device(&cpal::default_host()).is_some()
}

/// An installed virtual loopback device, if any: better than the microphone.
fn loopback_device(host: &cpal::Host) -> Option<cpal::Device> {
    host.input_devices().ok()?.find(|device| {
        device.description().is_ok_and(|description| {
            LOOPBACK_DEVICES
                .iter()
                .any(|known| description.name().contains(known))
        })
    })
}

fn capture_error(what: &str, error: impl std::fmt::Display) -> AudioError {
    AudioError::Capture(format!("{what}: {error}"))
}

fn input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut builder: BlockBuilder,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut converted = Vec::new();
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                converted.clear();
                converted.extend(data.iter().map(|sample| sample.to_sample::<f32>()));
                builder.push(&converted);
            },
            |error| eprintln!("audio stream error: {error}"),
            None,
        )
        .map_err(|e| e.to_string())
}
