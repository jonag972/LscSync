//! Sync loop: capture -> colors -> LSC panels.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use lscsync_core::color::{dominant_color, to_lamp};
use lscsync_core::{AudioHue, Hsv, Mode, Rgb, Settings, Snapshot, lsc_colors};
use thiserror::Error;

use crate::capture::{CaptureError, ScreenCapture};
use crate::tuya::{Lsc, LscConfig, LscError};

const FRAME_PERIOD: Duration = Duration::from_millis(40); // 25 Hz

// A frame is only sent when the color changed at least this much.
const HUE_THRESHOLD: f32 = 0.02;
const SATURATION_THRESHOLD: f32 = 0.1;
const BRIGHTNESS_THRESHOLD: f32 = 0.04;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("screen capture: {0}")]
    Capture(#[from] CaptureError),
    #[error("audio: {0}")]
    Audio(String),
    #[error("Tuya: {0}")]
    Lsc(#[from] LscError),
    #[error("tokio runtime: {0}")]
    Runtime(#[from] std::io::Error),
    #[error("no device configured")]
    NotConfigured,
}

/// What went wrong, so the UI can explain it without parsing messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    NotConfigured,
    /// The device did not answer in time.
    Unreachable,
    ScreenCapture,
    Audio,
    /// The device answered but refused (wrong key, protocol error).
    Device,
    Other,
}

impl EngineError {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::NotConfigured => ErrorKind::NotConfigured,
            Self::Lsc(LscError::Timeout) => ErrorKind::Unreachable,
            Self::Lsc(_) => ErrorKind::Device,
            Self::Capture(_) => ErrorKind::ScreenCapture,
            Self::Audio(_) => ErrorKind::Audio,
            Self::Runtime(_) => ErrorKind::Other,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RunStats {
    pub frames: u64,
    pub sent: u64,
    pub elapsed: Duration,
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Snapshot(Snapshot),
    Error { kind: ErrorKind, message: String },
    Stopped,
}

/// Engine running on its own thread, driven by the UI.
pub struct EngineHandle {
    stop: Arc<AtomicBool>,
    updates: Sender<Settings>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl EngineHandle {
    pub fn start(device: LscConfig, settings: Settings) -> (Self, Receiver<EngineEvent>) {
        Self::start_after(None, device, settings)
    }

    /// Starts an engine once `previous` has finished, without blocking the
    /// caller: the controller accepts only one local connection at a time.
    pub fn start_after(
        previous: Option<EngineHandle>,
        device: LscConfig,
        settings: Settings,
    ) -> (Self, Receiver<EngineEvent>) {
        let (sender, receiver) = mpsc::channel();
        let (updates, updates_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);

        let join = std::thread::spawn(move || {
            drop(previous);
            if stop_thread.load(Ordering::Relaxed) {
                let _ = sender.send(EngineEvent::Stopped);
                return;
            }
            let result = run_engine(device, settings, stop_thread, sender.clone(), updates_rx);
            if let Err(error) = result {
                let _ = sender.send(EngineEvent::Error {
                    kind: error.kind(),
                    message: error.to_string(),
                });
            }
            let _ = sender.send(EngineEvent::Stopped);
        });

        (
            Self {
                stop,
                updates,
                join: Some(join),
            },
            receiver,
        )
    }

    /// Applies new settings live (changing the mode needs a restart).
    pub fn update(&self, settings: Settings) {
        let _ = self.updates.send(settings);
    }

    /// Asks the engine to stop without waiting for its thread.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_engine(
    device: LscConfig,
    settings: Settings,
    stop: Arc<AtomicBool>,
    events: Sender<EngineEvent>,
    updates: Receiver<Settings>,
) -> Result<(), EngineError> {
    let mut engine = Engine::new(&device, settings)?;
    engine.run_until(&stop, &events, &updates)
}

pub struct Engine {
    runtime: tokio::runtime::Runtime,
    settings: Settings,
    capture: ScreenCapture,
    lsc: Lsc,
    smoothed: Option<[Rgb; 4]>,
    last_sent: Option<Vec<Hsv>>,
    audio: Option<crate::audio::PlatformAudioCapture>,
    bass: crate::audio::BassProcessor,
    frames: u64,
    sent: u64,
}

impl Engine {
    pub fn new(device: &LscConfig, settings: Settings) -> Result<Self, EngineError> {
        if device.device_id.is_empty() || device.local_key.is_empty() {
            return Err(EngineError::NotConfigured);
        }
        // Multi-threaded: the rustuya actor must keep writing frames while the
        // capture loop sleeps between two images.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let lsc = runtime.block_on(Lsc::connect(device, Duration::from_secs(10)))?;
        let capture = ScreenCapture::new()?;

        // In Music mode, start the platform audio capture.
        let audio = if settings.mode == Mode::Audio {
            let mut capture = crate::audio::PlatformAudioCapture::new()
                .map_err(|e| EngineError::Audio(e.to_string()))?;
            capture
                .start()
                .map_err(|e| EngineError::Audio(e.to_string()))?;
            Some(capture)
        } else {
            None
        };

        Ok(Self {
            runtime,
            settings,
            capture,
            lsc,
            smoothed: None,
            last_sent: None,
            audio,
            bass: crate::audio::BassProcessor::new(),
            frames: 0,
            sent: 0,
        })
    }

    /// Long-running loop, driven by the UI through `stop`.
    pub fn run_until(
        &mut self,
        stop: &AtomicBool,
        events: &Sender<EngineEvent>,
        updates: &Receiver<Settings>,
    ) -> Result<(), EngineError> {
        while !stop.load(Ordering::Relaxed) {
            let frame_start = Instant::now();
            if let Some(settings) = updates.try_iter().last() {
                self.apply_settings(settings);
            }
            let snapshot = self.compute_snapshot()?;
            let _ = events.send(EngineEvent::Snapshot(snapshot));

            let colors = lsc_colors(&snapshot, &self.settings);
            if self.should_send(&colors) {
                self.runtime
                    .block_on(self.lsc.send(&colors, self.settings.invert_zones))?;
                self.last_sent = Some(colors);
                self.sent += 1;
            }

            // Screen modes: fixed period. Music mode: reading the audio block
            // already paces the loop (~50 Hz), no extra sleep.
            if self.settings.mode != Mode::Audio {
                let elapsed = frame_start.elapsed();
                if elapsed < FRAME_PERIOD {
                    std::thread::sleep(FRAME_PERIOD - elapsed);
                }
            }
        }

        if self.settings.restore_white {
            self.runtime.block_on(self.lsc.restore_white())?;
        }

        Ok(())
    }

    /// Settings received from the UI. The mode stays the one it started with.
    fn apply_settings(&mut self, settings: Settings) {
        let mode = self.settings.mode;
        self.settings = settings;
        self.settings.mode = mode;
        // Force a send: the user must see the effect right away.
        self.last_sent = None;
    }

    pub fn run(&mut self, duration: Duration) -> Result<RunStats, EngineError> {
        let start = Instant::now();

        while start.elapsed() < duration {
            let frame_start = Instant::now();
            self.tick()?;

            let elapsed = frame_start.elapsed();
            if elapsed < FRAME_PERIOD {
                std::thread::sleep(FRAME_PERIOD - elapsed);
            }
        }

        if self.settings.restore_white {
            self.runtime.block_on(self.lsc.restore_white())?;
        }

        Ok(RunStats {
            frames: self.frames,
            sent: self.sent,
            elapsed: start.elapsed(),
        })
    }

    fn tick(&mut self) -> Result<(), EngineError> {
        let snapshot = self.compute_snapshot()?;
        let colors = lsc_colors(&snapshot, &self.settings);

        if self.should_send(&colors) {
            self.runtime
                .block_on(self.lsc.send(&colors, self.settings.invert_zones))?;
            self.last_sent = Some(colors);
            self.sent += 1;
        }

        Ok(())
    }

    /// Computes a snapshot without sending it (diagnostics).
    pub fn preview(&mut self) -> Result<Snapshot, EngineError> {
        self.compute_snapshot()
    }

    /// Sends fixed colors through the same path as the sync loop.
    pub fn send_test(&mut self, colors: &[Hsv], duration: Duration) -> Result<(), EngineError> {
        self.runtime
            .block_on(self.lsc.send(colors, self.settings.invert_zones))?;
        std::thread::sleep(duration);
        if self.settings.restore_white {
            self.runtime.block_on(self.lsc.restore_white())?;
        }
        Ok(())
    }

    fn compute_snapshot(&mut self) -> Result<Snapshot, EngineError> {
        if self.settings.mode == Mode::Audio {
            self.compute_audio_snapshot()
        } else {
            self.compute_screen_snapshot()
        }
    }

    /// Snapshot from the sound: reading the block paces the loop.
    fn compute_audio_snapshot(&mut self) -> Result<Snapshot, EngineError> {
        // Update the bass processor settings.
        self.bass.reactivity = self.settings.reactivity;
        self.bass.min_brightness = self.settings.audio_min_light;
        self.bass.max_brightness = self.settings.lum_max as f32;
        self.bass.hue_speed = self.settings.hue_speed as f32;

        // Wait for the next block of samples (100 ms at most, to stay
        // responsive to the stop signal).
        let audio = self
            .audio
            .as_ref()
            .ok_or_else(|| EngineError::Audio("audio capture not started".to_owned()))?;
        // No sound: feed a silent block so the glow falls back to its floor
        // instead of stopping the engine.
        let samples = audio
            .read_block_timeout(Duration::from_millis(100))
            .map_err(|e| EngineError::Audio(e.to_string()))?
            .unwrap_or_else(|| vec![0.0; crate::audio::BLOCK_SIZE]);

        self.frames += 1;

        // Hue: cycling or fixed.
        let hue = match self.settings.audio_hue {
            AudioHue::Cycle => self.bass.cycling_hue(),
            AudioHue::Fixed => self.settings.audio_fixed_hue as f32 / 360.0,
        };

        let zones = self.bass.process(&samples, hue, self.settings.audio_zones);
        let base = zones[0];

        Ok(Snapshot { zones, top: base })
    }

    fn compute_screen_snapshot(&mut self) -> Result<Snapshot, EngineError> {
        let image = self.capture.capture()?;
        self.frames += 1;
        let top = image.top_band(self.settings.capture_height);
        let zones = top.split_zones(3);

        let colors = [
            dominant_color(&zones[0].pixels),
            dominant_color(&zones[1].pixels),
            dominant_color(&zones[2].pixels),
            dominant_color(&top.pixels),
        ];

        let alpha = 1.0 - self.settings.smoothing;
        let smoothed = match self.smoothed {
            None => colors,
            Some(previous) => std::array::from_fn(|i| {
                Rgb::new(
                    previous[i].r * (1.0 - alpha) + colors[i].r * alpha,
                    previous[i].g * (1.0 - alpha) + colors[i].g * alpha,
                    previous[i].b * (1.0 - alpha) + colors[i].b * alpha,
                )
            }),
        };
        self.smoothed = Some(smoothed);

        let hsv: Vec<Hsv> = smoothed
            .iter()
            .map(|color| {
                to_lamp(
                    *color,
                    self.settings.boost_sat,
                    self.settings.lum_max as f32,
                )
            })
            .collect();

        Ok(Snapshot {
            zones: [hsv[0], hsv[1], hsv[2]],
            top: hsv[3],
        })
    }

    fn should_send(&self, colors: &[Hsv]) -> bool {
        let Some(previous) = &self.last_sent else {
            return true;
        };
        if previous.len() != colors.len() {
            return true;
        }

        !previous.iter().zip(colors).all(|(a, b)| {
            let dh = (a.h - b.h).abs() % 1.0;
            let dh = dh.min(1.0 - dh);
            dh < HUE_THRESHOLD
                && (a.s - b.s).abs() < SATURATION_THRESHOLD
                && (a.v - b.v).abs() < BRIGHTNESS_THRESHOLD
        })
    }

    pub fn close(self) {
        let Self {
            runtime,
            lsc,
            mut audio,
            ..
        } = self;
        if let Some(audio) = audio.as_mut() {
            audio.stop();
        }
        runtime.block_on(lsc.close());
    }
}
