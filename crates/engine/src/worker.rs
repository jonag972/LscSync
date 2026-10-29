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

use lscsync_core::color::{dominant_color_with, to_lamp};
use lscsync_core::{
    AudioHue, ColorCommand, DarkScenes, DeviceColors, Hsv, Mode, Product, Rgb, Settings, Snapshot,
    Zone,
};
use thiserror::Error;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::capture::{CaptureError, ScreenCapture};
use crate::tuya::{Lsc, LscConfig, LscError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

// A color is only sent again when it changed at least this much.
const HUE_THRESHOLD: f32 = 0.02;
const SATURATION_THRESHOLD: f32 = 0.1;
const BRIGHTNESS_THRESHOLD: f32 = 0.04;

/// How often the sync writes how it is doing to the logs.
const STATS_PERIOD: Duration = Duration::from_secs(60);
/// A frame slower than this is logged: the capture or the sound stalled.
const SLOW_FRAME: Duration = Duration::from_millis(500);

/// Pause after a device stopped answering, before trying again.
const DEVICE_BACKOFF: Duration = Duration::from_secs(3);
/// Below this share of the brightness setting, a zone counts as dark.
const DARK: f32 = 0.06;
/// Each frame, a fading color keeps this much of its brightness (Dim).
const DIM_FACTOR: f32 = 0.96;

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
    /// How a single color is sent to a light.
    pub color: ColorCommand,
}

impl Target {
    /// `top_height` places the default zones (see [`lscsync_core::Device::layout`]).
    pub fn new(device: &lscsync_core::Device, top_height: f32) -> Self {
        Self {
            config: LscConfig::from(device),
            name: device.name.clone(),
            product: Product::of(device),
            zones: device.layout(top_height),
            color: device.color_command(),
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
}

/// A connected device: its task, the channel feeding it, its zones.
struct Link {
    id: String,
    product: Product,
    zones: Vec<Zone>,
    /// Smoothed zone colors, before conversion for the lamp.
    smoothed: Vec<Rgb>,
    /// Last colors shown before the picture went dark (Hold and Dim).
    lit: Vec<Hsv>,
    frames: watch::Sender<Option<Frame>>,
    task: JoinHandle<Lsc>,
}

pub struct Engine {
    runtime: tokio::runtime::Runtime,
    settings: Settings,
    /// Screen modes only.
    capture: Option<ScreenCapture>,
    links: Vec<Link>,
    highlight: Option<(String, usize)>,
    smoothed_top: Option<Rgb>,
    lit_top: Vec<Hsv>,
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
        tracing::info!(
            "sync starting: {:?} mode, {} device(s): {}",
            settings.mode,
            targets.len(),
            targets
                .iter()
                .map(|t| format!(
                    "{} ({:?}, {} zone(s), {:?})",
                    t.name,
                    t.product,
                    t.zones.len(),
                    t.color
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );

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
                Ok(lsc) => {
                    tracing::info!("{}: connected", target.name);
                    links.push(Self::link(&runtime, target, lsc, events.clone()))
                }
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

        // macOS: the capture APIs fail silently without the TCC grant, and the
        // consent dialog only appears when the app asks for it, so ask now.
        let needs_screen_capture = match settings.mode {
            Mode::ScreenZones | Mode::Screen => true,
            // System audio (ScreenCaptureKit) needs the same grant; the
            // microphone fallback asks for the Microphone one instead.
            Mode::Audio => !crate::audio::uses_microphone(),
            Mode::Fixed => false,
        };
        if needs_screen_capture && !crate::permission::ensure_screen_capture_access() {
            return Err(EngineError::Capture(CaptureError::PermissionDenied));
        }

        let capture = match settings.mode {
            Mode::ScreenZones | Mode::Screen => Some(ScreenCapture::new()?),
            Mode::Audio | Mode::Fixed => None,
        };
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
            lit_top: Vec::new(),
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
        // Logs the connection going down and coming back.
        let mut connection = lsc.watch_connection();
        let name = target.name.clone();
        runtime.spawn(async move {
            while connection.changed().await.is_ok() {
                let state = *connection.borrow_and_update();
                tracing::info!("{name}: connection {state:?}");
            }
        });
        let task = runtime.spawn(drive(
            lsc,
            target.product,
            target.color,
            target.name.clone(),
            receiver,
            events,
        ));
        Link {
            id: target.config.device_id,
            product: target.product,
            smoothed: Vec::new(),
            lit: Vec::new(),
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
        let mut stats = Stats::new();
        while !stop.load(Ordering::Relaxed) {
            let frame_start = Instant::now();
            for update in updates.try_iter() {
                self.apply(update);
            }
            let snapshot = self.tick().inspect_err(|error| {
                tracing::error!("sync stopped: {error}");
            })?;
            let _ = events.send(EngineEvent::Snapshot(snapshot));
            stats.frame(frame_start.elapsed());

            // Screen modes: fixed period. Music mode: reading the audio block
            // already paces the loop (~50 Hz), no extra sleep.
            if self.settings.mode != Mode::Audio
                && let Some(rest) = self.frame_period().checked_sub(frame_start.elapsed())
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
            if let Some(rest) = self.frame_period().checked_sub(frame_start.elapsed()) {
                std::thread::sleep(rest);
            }
        }
        Ok(RunStats {
            frames: self.frames,
            elapsed: start.elapsed(),
        })
    }

    fn frame_period(&self) -> Duration {
        Duration::from_secs_f32(1.0 / self.settings.frame_rate.clamp(5, 60) as f32)
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
            Mode::Fixed => Ok(self.tick_fixed()),
        }
    }

    fn capture(&self) -> Result<lscsync_core::Image, EngineError> {
        let capture = self.capture.as_ref().ok_or(CaptureError::NoMonitor)?;
        Ok(capture.capture()?)
    }

    /// The color chosen by hand, on every device.
    fn tick_fixed(&mut self) -> Snapshot {
        let color = Hsv::new(
            self.settings.fixed_hue as f32 / 360.0,
            self.settings.fixed_saturation,
            self.settings.lum_max as f32 / 100.0,
        );
        let mut devices = Vec::new();
        for link in &self.links {
            link.frames.send_replace(Some(Frame::Color(color)));
            devices.push(DeviceColors {
                id: link.id.clone(),
                colors: vec![color; link.zones.len().max(1)],
            });
        }
        Snapshot { devices }
    }

    fn tick_zones(&mut self) -> Result<Snapshot, EngineError> {
        let image = self.capture()?;
        let alpha = 1.0 - self.settings.smoothing;
        let mut devices = Vec::new();
        for link in &mut self.links {
            let colors: Vec<Rgb> = link
                .zones
                .iter()
                .map(|zone| dominant_color_with(&image.region(*zone), self.settings.vivid_priority))
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
            shape(&mut lamp, &mut link.lit, &self.settings);
            if let Some((device, zone)) = &self.highlight
                && *device == link.id
                && let Some(color) = lamp.get_mut(*zone)
            {
                *color = Hsv::new(0.0, 0.0, 1.0);
            }
            devices.push(DeviceColors {
                id: link.id.clone(),
                colors: lamp.clone(),
            });
            let frame = match link.product {
                Product::Panels => Frame::Zones(lamp),
                Product::Light => Frame::Color(lamp.first().copied().unwrap_or_default()),
            };
            link.frames.send_replace(Some(frame));
        }
        Ok(Snapshot { devices })
    }

    fn tick_color(&mut self) -> Result<Snapshot, EngineError> {
        let image = self.capture()?;
        let color = dominant_color_with(
            &image.top_band(self.settings.capture_height).pixels,
            self.settings.vivid_priority,
        );
        let alpha = 1.0 - self.settings.smoothing;
        let smoothed = self
            .smoothed_top
            .map_or(color, |previous| blend(previous, color, alpha));
        self.smoothed_top = Some(smoothed);
        let mut lamp = [to_lamp(
            smoothed,
            self.settings.boost_sat,
            self.settings.lum_max as f32,
        )];
        shape(&mut lamp, &mut self.lit_top, &self.settings);
        let [lamp] = lamp;
        let mut devices = Vec::new();
        for link in &self.links {
            link.frames.send_replace(Some(Frame::Color(lamp)));
            devices.push(DeviceColors {
                id: link.id.clone(),
                colors: vec![lamp; link.zones.len().max(1)],
            });
        }
        Ok(Snapshot { devices })
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
        let mut devices = Vec::new();
        for link in &self.links {
            // The 3 music zones spread over the device's zones, left to right.
            let count = link.zones.len().max(1);
            let colors: Vec<Hsv> = (0..count)
                .map(|zone| zones[zone * zones.len() / count])
                .collect();
            let frame = match link.product {
                // Through the instant DP 51 scene: what the bass needs.
                Product::Panels => Frame::Zones(colors.clone()),
                Product::Light => Frame::Color(zones[0]),
            };
            link.frames.send_replace(Some(frame));
            devices.push(DeviceColors {
                id: link.id.clone(),
                colors,
            });
        }
        Ok(Snapshot { devices })
    }

    /// Stops the device tasks, sets the devices back to white if asked, and
    /// closes the connections.
    pub fn close(self) {
        tracing::info!("sync stopping");
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

/// What the loop logs: frames per minute and the slowest one, and every
/// frame slow enough to be seen as a cut.
struct Stats {
    since: Instant,
    frames: u32,
    slowest: Duration,
}

impl Stats {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            frames: 0,
            slowest: Duration::ZERO,
        }
    }

    fn frame(&mut self, took: Duration) {
        if took > SLOW_FRAME {
            tracing::warn!(
                "a frame took {} ms: capture or sound stalled",
                took.as_millis()
            );
        }
        self.frames += 1;
        self.slowest = self.slowest.max(took);
        let elapsed = self.since.elapsed();
        if elapsed >= STATS_PERIOD {
            tracing::info!(
                "{} frames in {:.0} s ({:.1}/s), slowest {} ms",
                self.frames,
                elapsed.as_secs_f32(),
                self.frames as f32 / elapsed.as_secs_f32(),
                self.slowest.as_millis()
            );
            *self = Self::new();
        }
    }
}

/// Contrast, dark scenes and the brightness floor of the screen modes,
/// applied to one device's zones. `lit` remembers the last bright colors.
fn shape(colors: &mut [Hsv], lit: &mut Vec<Hsv>, settings: &Settings) {
    if lit.len() != colors.len() {
        *lit = colors.to_vec();
    }
    let max_v = colors.iter().map(|color| color.v).fold(0.0_f32, f32::max);
    if settings.contrast > 0.0 && max_v > 0.0 {
        let exponent = 1.0 + 2.0 * settings.contrast;
        for color in colors.iter_mut() {
            color.v = max_v * (color.v / max_v).powf(exponent);
        }
    }
    let scale = settings.lum_max as f32 / 100.0;
    for (color, lit) in colors.iter_mut().zip(lit.iter_mut()) {
        if color.v >= DARK * scale {
            *lit = *color;
            continue;
        }
        match settings.dark_scenes {
            DarkScenes::Follow => *lit = *color,
            DarkScenes::Hold => *color = *lit,
            DarkScenes::Dim => {
                lit.v *= DIM_FACTOR;
                *color = *lit;
            }
        }
    }
    let floor = settings.screen_min_light * scale;
    for color in colors {
        color.v = color.v.max(floor);
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
    command: ColorCommand,
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
            _ => {
                let color = match &frame {
                    Frame::Color(color) => *color,
                    Frame::Zones(colors) => colors.first().copied().unwrap_or_default(),
                };
                last_zones = None;
                if last_single.is_some_and(|last| !differs(last, color)) {
                    Ok(())
                } else {
                    last_single = Some(color);
                    match (product, command) {
                        (Product::Panels, _) | (Product::Light, ColorCommand::Direct) => {
                            lsc.send_direct(color).await
                        }
                        (Product::Light, ColorCommand::Fade) => {
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
                tracing::info!("{name}: answering again");
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

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Hsv = Hsv::new(0.0, 1.0, 0.8);
    const DARK_GRAY: Hsv = Hsv::new(0.5, 0.1, 0.01);

    fn settings(dark_scenes: DarkScenes) -> Settings {
        Settings {
            dark_scenes,
            ..Settings::default()
        }
    }

    #[test]
    fn dark_scenes_follow_hold_or_fade() {
        let mut lit = Vec::new();
        let mut colors = [RED];
        shape(&mut colors, &mut lit, &settings(DarkScenes::Hold));
        let mut dark = [DARK_GRAY];
        shape(&mut dark, &mut lit, &settings(DarkScenes::Follow));
        assert_eq!(dark, [DARK_GRAY]);

        let mut lit = vec![RED];
        let mut dark = [DARK_GRAY];
        shape(&mut dark, &mut lit, &settings(DarkScenes::Hold));
        assert_eq!(dark, [RED]);

        let mut dark = [DARK_GRAY];
        shape(&mut dark, &mut lit, &settings(DarkScenes::Dim));
        assert_eq!(dark[0].h, RED.h);
        assert!(dark[0].v < RED.v);
    }

    #[test]
    fn brightness_floor_and_contrast() {
        let mut colors = [DARK_GRAY, RED];
        let floor = Settings {
            screen_min_light: 0.2,
            ..Settings::default()
        };
        shape(&mut colors, &mut Vec::new(), &floor);
        assert!((colors[0].v - 0.2).abs() < 1e-6);

        let mut colors = [Hsv::new(0.0, 1.0, 0.4), RED];
        let contrast = Settings {
            contrast: 1.0,
            ..Settings::default()
        };
        shape(&mut colors, &mut Vec::new(), &contrast);
        assert!(colors[0].v < 0.4);
        assert_eq!(colors[1].v, RED.v);
    }
}
