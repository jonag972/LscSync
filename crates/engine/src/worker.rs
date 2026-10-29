//! Sync loop: capture -> colors -> every synced device.
//!
//! The loop captures the screen (or the sound) and computes one color per
//! zone of each device. Each device has its own task, fed through a watch
//! channel that only keeps the latest frame: a slow device skips frames
//! instead of slowing the others down.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use lscsync_core::color::{dominant_color, to_lamp};
use lscsync_core::{AudioHue, Hsv, Mode, Product, Rgb, Settings, Snapshot, Zone};
use thiserror::Error;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::capture::{CaptureError, ScreenCapture};
use crate::tuya::{Lsc, LscConfig, LscError};

const FRAME_PERIOD: Duration = Duration::from_millis(40); // 25 Hz
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// A color is only sent again when it changed at least this much.
const HUE_THRESHOLD: f32 = 0.02;
const SATURATION_THRESHOLD: f32 = 0.1;
const BRIGHTNESS_THRESHOLD: f32 = 0.04;

/// Pause after a device stopped answering, before trying again.
const DEVICE_BACKOFF: Duration = Duration::from_secs(3);
/// Single-color updates per second for lights that fade (DP 24).
const LIGHT_UPDATES_PER_SECOND: f64 = 10.0;

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

/// A device to drive, with where its zones look on the screen.
#[derive(Debug, Clone)]
pub struct Target {
    pub config: LscConfig,
    pub name: String,
    pub product: Product,
    pub zones: Vec<Zone>,
}

impl Target {
    /// `top_height` places the default zones (see [`lscsync_core::Device::layout`]).
    pub fn new(device: &lscsync_core::Device, top_height: f32) -> Self {
        Self {
            config: LscConfig::from(device),
            name: device.name.clone(),
            product: Product::of(device),
            zones: device.layout(top_height),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RunStats {
    pub frames: u64,
    pub elapsed: Duration,
}

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Snapshot(Snapshot),
    /// A device stopped answering; the others keep going.
    DeviceUnreachable(String),
    /// A device answers again.
    DeviceBack(String),
    Error {
        kind: ErrorKind,
        message: String,
    },
    Stopped,
}

/// Changes applied to a running engine.
#[derive(Debug, Clone)]
pub enum Update {
    Settings(Settings),
    /// New zones for a device (the zone editor).
    Zones {
        device: String,
        zones: Vec<Zone>,
    },
    /// Lights one zone in white so the user can find it, or `None` to stop.
    Highlight(Option<(String, usize)>),
}

/// Engine running on its own thread, driven by the UI.
pub struct EngineHandle {
    stop: Arc<AtomicBool>,
    updates: Sender<Update>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl EngineHandle {
    pub fn start(targets: Vec<Target>, settings: Settings) -> (Self, Receiver<EngineEvent>) {
        Self::start_after(None, targets, settings)
    }

    /// Starts an engine once `previous` has finished, without blocking the
    /// caller: the controllers accept only one local connection at a time.
    pub fn start_after(
        previous: Option<EngineHandle>,
        targets: Vec<Target>,
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
            let result =
                Engine::new(targets, settings, Some(sender.clone())).and_then(|mut engine| {
                    let result = engine.run_until(&stop_thread, &sender, &updates_rx);
                    engine.close();
                    result
                });
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
        let _ = self.updates.send(Update::Settings(settings));
    }

    pub fn set_zones(&self, device: String, zones: Vec<Zone>) {
        let _ = self.updates.send(Update::Zones { device, zones });
    }

    pub fn highlight(&self, zone: Option<(String, usize)>) {
        let _ = self.updates.send(Update::Highlight(zone));
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

/// What a device task is asked to show.
#[derive(Debug, Clone)]
enum Frame {
    /// One color per zone (Ambilight).
    Zones(Vec<Hsv>),
    /// One color everywhere (Color mode).
    Color(Hsv),
    /// Left / middle / right, applied instantly (Music mode on the panels).
    Scene { zones: [Hsv; 3], invert: bool },
}

/// A connected device: its task, the channel feeding it, its zones.
struct Link {
    id: String,
    product: Product,
    zones: Vec<Zone>,
    /// Smoothed zone colors, before conversion for the lamp.
    smoothed: Vec<Rgb>,
    frames: watch::Sender<Option<Frame>>,
    task: JoinHandle<Lsc>,
}

pub struct Engine {
    runtime: tokio::runtime::Runtime,
    settings: Settings,
    capture: ScreenCapture,
    links: Vec<Link>,
    highlight: Option<(String, usize)>,
    smoothed_top: Option<Rgb>,
    audio: Option<crate::audio::PlatformAudioCapture>,
    bass: crate::audio::BassProcessor,
    frames: u64,
}

impl Engine {
    /// Connects to every target. Those that do not answer are reported and
    /// skipped; it fails only when none answers.
    pub fn new(
        targets: Vec<Target>,
        settings: Settings,
        events: Option<Sender<EngineEvent>>,
    ) -> Result<Self, EngineError> {
        let targets: Vec<Target> = targets
            .into_iter()
            .filter(|t| !t.config.device_id.is_empty() && !t.config.local_key.is_empty())
            .collect();
        if targets.is_empty() {
            return Err(EngineError::NotConfigured);
        }
        // Multi-threaded: the rustuya actors must keep writing frames while
        // the capture loop sleeps between two images.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;

        let connections: Vec<_> = targets
            .iter()
            .map(|target| {
                let config = target.config.clone();
                runtime.spawn(async move { Lsc::connect(&config, CONNECT_TIMEOUT).await })
            })
            .collect();
        let mut links = Vec::new();
        let mut first_error = None;
        for (target, connection) in targets.into_iter().zip(connections) {
            let connected = runtime
                .block_on(connection)
                .map_err(|error| LscError::Io(error.to_string()))
                .and_then(|result| result);
            match connected {
                Ok(lsc) => links.push(Self::link(&runtime, target, lsc, events.clone())),
                Err(error) => {
                    tracing::warn!("{}: {error}", target.name);
                    if let Some(events) = &events {
                        let _ = events.send(EngineEvent::DeviceUnreachable(target.name.clone()));
                    }
                    first_error.get_or_insert(error);
                }
            }
        }
        if links.is_empty() {
            return Err(first_error.map_or(EngineError::NotConfigured, EngineError::Lsc));
        }

        let capture = ScreenCapture::new()?;
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
            links,
            highlight: None,
            smoothed_top: None,
            audio,
            bass: crate::audio::BassProcessor::new(),
            frames: 0,
        })
    }

    fn link(
        runtime: &tokio::runtime::Runtime,
        target: Target,
        lsc: Lsc,
        events: Option<Sender<EngineEvent>>,
    ) -> Link {
        let (frames, receiver) = watch::channel(None);
        let task = runtime.spawn(drive(
            lsc,
            target.product,
            target.name.clone(),
            receiver,
            events,
        ));
        Link {
            id: target.config.device_id,
            product: target.product,
            smoothed: Vec::new(),
            zones: target.zones,
            frames,
            task,
        }
    }

    /// Long-running loop, driven by the UI through `stop`.
    pub fn run_until(
        &mut self,
        stop: &AtomicBool,
        events: &Sender<EngineEvent>,
        updates: &Receiver<Update>,
    ) -> Result<(), EngineError> {
        while !stop.load(Ordering::Relaxed) {
            let frame_start = Instant::now();
            for update in updates.try_iter() {
                self.apply(update);
            }
            let snapshot = self.tick()?;
            let _ = events.send(EngineEvent::Snapshot(snapshot));

            // Screen modes: fixed period. Music mode: reading the audio block
            // already paces the loop (~50 Hz), no extra sleep.
            if self.settings.mode != Mode::Audio
                && let Some(rest) = FRAME_PERIOD.checked_sub(frame_start.elapsed())
            {
                std::thread::sleep(rest);
            }
        }
        Ok(())
    }

    /// Runs for a fixed time (diagnostics).
    pub fn run(&mut self, duration: Duration) -> Result<RunStats, EngineError> {
        let start = Instant::now();
        while start.elapsed() < duration {
            let frame_start = Instant::now();
            self.tick()?;
            if let Some(rest) = FRAME_PERIOD.checked_sub(frame_start.elapsed()) {
                std::thread::sleep(rest);
            }
        }
        Ok(RunStats {
            frames: self.frames,
            elapsed: start.elapsed(),
        })
    }

    fn apply(&mut self, update: Update) {
        match update {
            Update::Settings(settings) => {
                // The mode stays the one the engine started with.
                let mode = self.settings.mode;
                self.settings = settings;
                self.settings.mode = mode;
            }
            Update::Zones { device, zones } => {
                if let Some(link) = self.links.iter_mut().find(|link| link.id == device) {
                    link.zones = zones;
                    link.smoothed.clear();
                }
            }
            Update::Highlight(zone) => self.highlight = zone,
        }
    }

    /// Computes the colors of every device, hands them to the device tasks,
    /// and returns them for the app.
    fn tick(&mut self) -> Result<Snapshot, EngineError> {
        self.frames += 1;
        match self.settings.mode {
            Mode::ScreenZones => self.tick_zones(),
            Mode::Screen => self.tick_color(),
            Mode::Audio => self.tick_audio(),
        }
    }

    fn tick_zones(&mut self) -> Result<Snapshot, EngineError> {
        let image = self.capture.capture()?;
        let alpha = 1.0 - self.settings.smoothing;
        let mut shown = Vec::new();
        for link in &mut self.links {
            let colors: Vec<Rgb> = link
                .zones
                .iter()
                .map(|zone| dominant_color(&image.region(*zone)))
                .collect();
            link.smoothed = if link.smoothed.len() == colors.len() {
                link.smoothed
                    .iter()
                    .zip(&colors)
                    .map(|(previous, color)| blend(*previous, *color, alpha))
                    .collect()
            } else {
                colors
            };
            let mut lamp: Vec<Hsv> = link
                .smoothed
                .iter()
                .map(|color| {
                    to_lamp(
                        *color,
                        self.settings.boost_sat,
                        self.settings.lum_max as f32,
                    )
                })
                .collect();
            if let Some((device, zone)) = &self.highlight
                && *device == link.id
                && let Some(color) = lamp.get_mut(*zone)
            {
                *color = Hsv::new(0.0, 0.0, 1.0);
            }
            shown.extend_from_slice(&lamp);
            let frame = match link.product {
                Product::Panels => Frame::Zones(lamp),
                Product::Light => Frame::Color(lamp.first().copied().unwrap_or_default()),
            };
            link.frames.send_replace(Some(frame));
        }
        Ok(Snapshot { zones: shown })
    }

    fn tick_color(&mut self) -> Result<Snapshot, EngineError> {
        let image = self.capture.capture()?;
        let color = dominant_color(&image.top_band(self.settings.capture_height).pixels);
        let alpha = 1.0 - self.settings.smoothing;
        let smoothed = self
            .smoothed_top
            .map_or(color, |previous| blend(previous, color, alpha));
        self.smoothed_top = Some(smoothed);
        let lamp = to_lamp(
            smoothed,
            self.settings.boost_sat,
            self.settings.lum_max as f32,
        );
        for link in &self.links {
            link.frames.send_replace(Some(Frame::Color(lamp)));
        }
        Ok(Snapshot { zones: vec![lamp] })
    }

    /// Colors from the sound: reading the block paces the loop.
    fn tick_audio(&mut self) -> Result<Snapshot, EngineError> {
        self.bass.reactivity = self.settings.reactivity;
        self.bass.min_brightness = self.settings.audio_min_light;
        self.bass.max_brightness = self.settings.lum_max as f32;
        self.bass.hue_speed = self.settings.hue_speed as f32;

        // Wait for the next block of samples (100 ms at most, to stay
        // responsive to the stop signal). No sound: a silent block, so the
        // glow falls back to its floor instead of stopping the engine.
        let audio = self
            .audio
            .as_ref()
            .ok_or_else(|| EngineError::Audio("audio capture not started".to_owned()))?;
        let samples = audio
            .read_block_timeout(Duration::from_millis(100))
            .map_err(|e| EngineError::Audio(e.to_string()))?
            .unwrap_or_else(|| vec![0.0; crate::audio::BLOCK_SIZE]);

        let hue = match self.settings.audio_hue {
            AudioHue::Cycle => self.bass.cycling_hue(),
            AudioHue::Fixed => self.settings.audio_fixed_hue as f32 / 360.0,
        };
        let zones = self.bass.process(&samples, hue, self.settings.audio_zones);
        for link in &self.links {
            let frame = match link.product {
                // DP 51 is applied instantly: what the bass needs.
                Product::Panels => Frame::Scene {
                    zones,
                    invert: self.settings.invert_zones,
                },
                Product::Light => Frame::Color(zones[0]),
            };
            link.frames.send_replace(Some(frame));
        }
        Ok(Snapshot {
            zones: zones.to_vec(),
        })
    }

    /// Stops the device tasks, sets the devices back to white if asked, and
    /// closes the connections.
    pub fn close(self) {
        let Self {
            runtime,
            settings,
            links,
            mut audio,
            ..
        } = self;
        if let Some(audio) = audio.as_mut() {
            audio.stop();
        }
        runtime.block_on(async move {
            for link in links {
                // Closing the channel ends the task, which hands the device back.
                drop(link.frames);
                let Ok(lsc) = link.task.await else {
                    continue;
                };
                if settings.restore_white
                    && let Err(error) = lsc.restore_white().await
                {
                    tracing::warn!("back to white: {error}");
                }
                lsc.close().await;
            }
        });
    }
}

/// Exponential smoothing: `alpha` of the new color.
fn blend(previous: Rgb, color: Rgb, alpha: f32) -> Rgb {
    Rgb::new(
        previous.r * (1.0 - alpha) + color.r * alpha,
        previous.g * (1.0 - alpha) + color.g * alpha,
        previous.b * (1.0 - alpha) + color.b * alpha,
    )
}

/// Whether two colors look different enough to send the new one.
fn differs(a: Hsv, b: Hsv) -> bool {
    let dh = (a.h - b.h).abs() % 1.0;
    let dh = dh.min(1.0 - dh);
    dh >= HUE_THRESHOLD
        || (a.s - b.s).abs() >= SATURATION_THRESHOLD
        || (a.v - b.v).abs() >= BRIGHTNESS_THRESHOLD
}

/// Evenly spaced commands: at most `per_second`.
struct Pace {
    interval: Duration,
    next: tokio::time::Instant,
}

impl Pace {
    fn new(per_second: f64) -> Self {
        Self {
            interval: Duration::from_secs_f64(1.0 / per_second),
            next: tokio::time::Instant::now(),
        }
    }

    async fn wait(&mut self) {
        tokio::time::sleep_until(self.next).await;
        self.next = self.next.max(tokio::time::Instant::now()) + self.interval;
    }
}

/// A device's task: shows the latest frame, then waits for the next one.
/// Returns the device when the engine closes its channel.
async fn drive(
    lsc: Lsc,
    product: Product,
    name: String,
    mut frames: watch::Receiver<Option<Frame>>,
    events: Option<Sender<EngineEvent>>,
) -> Lsc {
    let report = |event: EngineEvent| {
        if let Some(events) = &events {
            let _ = events.send(event);
        }
    };
    // What the device shows, as far as we know.
    let mut last_zones: Option<Vec<Hsv>> = None;
    let mut last_single: Option<Hsv> = None;
    let mut light_pace = Pace::new(LIGHT_UPDATES_PER_SECOND);
    let mut failing = false;

    while frames.changed().await.is_ok() {
        let Some(frame) = frames.borrow_and_update().clone() else {
            continue;
        };
        let result = match (&frame, product) {
            (Frame::Zones(colors), Product::Panels) => {
                last_single = None;
                let unchanged = last_zones.as_ref().is_some_and(|last: &Vec<Hsv>| {
                    last.len() == colors.len()
                        && last.iter().zip(colors).all(|(a, b)| !differs(*a, *b))
                });
                if unchanged {
                    Ok(())
                } else {
                    last_zones = Some(colors.clone());
                    lsc.send_scene_panels(colors).await
                }
            }
            (Frame::Scene { zones, invert }, Product::Panels) => {
                last_zones = None;
                last_single = None;
                lsc.send_zones(zones, *invert).await
            }
            _ => {
                let color = match &frame {
                    Frame::Color(color) => *color,
                    Frame::Zones(colors) => colors.first().copied().unwrap_or_default(),
                    Frame::Scene { zones, .. } => zones[0],
                };
                last_zones = None;
                if last_single.is_some_and(|last| !differs(last, color)) {
                    Ok(())
                } else {
                    last_single = Some(color);
                    match product {
                        Product::Panels => lsc.send_direct(color).await,
                        Product::Light => {
                            light_pace.wait().await;
                            lsc.send_color(color).await
                        }
                    }
                }
            }
        };
        match result {
            Ok(()) if failing => {
                failing = false;
                report(EngineEvent::DeviceBack(name.clone()));
            }
            Ok(()) => {}
            Err(error) => {
                tracing::warn!("{name}: {error}");
                if !failing {
                    failing = true;
                    report(EngineEvent::DeviceUnreachable(name.clone()));
                }
                // Unknown state: resend everything once it answers again.
                last_zones = None;
                last_single = None;
                tokio::time::sleep(DEVICE_BACKOFF).await;
            }
        }
    }
    lsc
}
