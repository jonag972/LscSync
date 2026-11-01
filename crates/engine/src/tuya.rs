//! Local connection to the LSC controller (Tuya v3.5) through `rustuya`.
//!
//! `rustuya` handles framing, encryption, keepalive and reconnection. This
//! module adds the device logic: power, mode, and restoring white.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use lscsync_core::protocol;
use lscsync_core::{ColorCommand, Hsv};
use rustuya::tokio::{ConnectionState, Device, Discovery, Listener, Version};
use thiserror::Error;

const MODE_UNKNOWN: u8 = 0;
const MODE_COLOR: u8 = 1;
const MODE_SCENE: u8 = 2;
const MODE_MUSIC: u8 = 3;

#[derive(Debug, Clone)]
pub struct LscConfig {
    pub device_id: String,
    pub local_key: String,
    pub ip: String,
    /// TCP port of the controller (6668 for real lights). Tests point it at a
    /// fake light on another port.
    pub port: u16,
}

impl From<&lscsync_core::Device> for LscConfig {
    fn from(device: &lscsync_core::Device) -> Self {
        Self {
            device_id: device.id.clone(),
            local_key: device.local_key.clone(),
            ip: device.ip.clone(),
            port: DEFAULT_PORT,
        }
    }
}

/// The Tuya local protocol's TCP port.
pub const DEFAULT_PORT: u16 = 6668;

#[derive(Debug, Error)]
pub enum LscError {
    #[error("Tuya: {0}")]
    Tuya(#[from] rustuya::tokio::TuyaError),
    #[error("connection timed out")]
    Timeout,
    #[error("I/O: {0}")]
    Io(String),
}

/// Tuya devices announcing themselves on the local network, as (id, ip).
pub async fn scan_local_network(window: Duration) -> Result<Vec<(String, String)>, LscError> {
    let discovery = Discovery::new()?;
    let found = discovery.scan(window).await;
    discovery.close().await;
    Ok(found
        .into_iter()
        .map(|device| (device.id, device.ip.to_string()))
        .collect())
}

/// Blocking version of [`scan_local_network`], for callers without a runtime.
pub fn scan_local_network_blocking(window: Duration) -> Result<Vec<(String, String)>, LscError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| LscError::Io(error.to_string()))?
        .block_on(scan_local_network(window))
}

/// Calibration: switches a light between red and blue for `duration` with
/// `command`, then back to white, so the user can tell whether it fades.
/// Blocking.
pub fn flip_colors_blocking(
    config: &LscConfig,
    command: ColorCommand,
    duration: Duration,
) -> Result<(), LscError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| LscError::Io(error.to_string()))?;
    runtime.block_on(async {
        let lsc = Lsc::connect(config, Duration::from_secs(10)).await?;
        let start = std::time::Instant::now();
        let mut red = true;
        while start.elapsed() < duration {
            let color = Hsv::new(if red { 0.0 } else { 2.0 / 3.0 }, 1.0, 1.0);
            match command {
                ColorCommand::Direct => lsc.send_direct(color).await?,
                ColorCommand::Fade => lsc.send_color(color).await?,
            }
            red = !red;
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        lsc.restore_white().await?;
        lsc.close().await;
        Ok(())
    })
}

pub struct Lsc {
    device: Device,
    discovery: Discovery,
    powered: AtomicBool,
    mode: AtomicU8,
}

impl Lsc {
    pub async fn connect(config: &LscConfig, timeout: Duration) -> Result<Self, LscError> {
        let discovery = Discovery::new()?;
        let mut builder = Device::builder(config.device_id.clone(), config.local_key.clone())
            .version(Version::V3_5)
            .port(config.port)
            .rediscover(&discovery);
        // Unknown address (fresh from the QR login): discovery finds it.
        if !config.ip.is_empty() {
            builder = builder.address(config.ip.clone());
        }
        let device = builder.connect()?;

        match device.wait_connected(timeout).await {
            Ok(()) => Ok(Self {
                device,
                discovery,
                powered: AtomicBool::new(false),
                mode: AtomicU8::new(MODE_UNKNOWN),
            }),
            Err(rustuya::tokio::TuyaError::Timeout) => Err(LscError::Timeout),
            Err(error) => Err(LscError::Tuya(error)),
        }
    }

    /// Connection state changes, for the logs: rustuya reconnects on its own.
    pub fn watch_connection(&self) -> tokio::sync::watch::Receiver<ConnectionState> {
        self.device.watch_connection_state()
    }

    pub fn listener(&self) -> Listener {
        self.device.listener()
    }

    pub async fn query(&self) -> Result<(), LscError> {
        self.device.query().await?;
        Ok(())
    }

    async fn ensure_power(&self) -> Result<(), LscError> {
        if !self.powered.load(Ordering::Relaxed) {
            self.device.set_value(protocol::DP_POWER, true).await?;
            self.powered.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn ensure_mode(&self, mode: u8, name: &str) -> Result<(), LscError> {
        if self.mode.load(Ordering::Relaxed) != mode {
            self.device.set_value(protocol::DP_MODE, name).await?;
            self.mode.store(mode, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Single color for the whole light (DP 24).
    pub async fn send_color(&self, color: Hsv) -> Result<(), LscError> {
        self.ensure_power().await?;
        self.ensure_mode(MODE_COLOR, protocol::MODE_COLOR).await?;
        self.device
            .set_value(protocol::DP_COLOR, protocol::dp_color(color))
            .await?;
        Ok(())
    }

    /// Single color, applied instantly (DP 27, music mode): DP 24 fades.
    pub async fn send_direct(&self, color: Hsv) -> Result<(), LscError> {
        self.ensure_power().await?;
        self.ensure_mode(MODE_MUSIC, protocol::MODE_MUSIC).await?;
        self.device
            .set_value(protocol::DP_MUSIC, protocol::dp_music(color))
            .await?;
        Ok(())
    }

    /// One color per zone, applied instantly (DP 51, 12-segment scene).
    pub async fn send_scene_segments(&self, colors: &[Hsv]) -> Result<(), LscError> {
        self.ensure_power().await?;
        self.ensure_mode(MODE_SCENE, protocol::MODE_SCENE).await?;
        self.device
            .set_value(protocol::DP_SCENE, protocol::dp_scene_segments(colors))
            .await?;
        Ok(())
    }

    /// A color on some zones, or turns them off, instantly (DP 61, colour
    /// mode): each zone has its own brightness.
    pub async fn send_paint(&self, paint: &lscsync_core::paint::Paint) -> Result<(), LscError> {
        self.ensure_power().await?;
        self.ensure_mode(MODE_COLOR, protocol::MODE_COLOR).await?;
        self.device
            .set_value(
                protocol::DP_PAINT,
                protocol::dp_paint(paint.color, &paint.zones),
            )
            .await?;
        Ok(())
    }

    /// 3 colors left / middle / right (DP 51).
    pub async fn send_zones(&self, zones: &[Hsv; 3], invert: bool) -> Result<(), LscError> {
        self.ensure_power().await?;
        self.ensure_mode(MODE_SCENE, protocol::MODE_SCENE).await?;
        self.device
            .set_value(protocol::DP_SCENE, protocol::dp_scene_zones(zones, invert))
            .await?;
        Ok(())
    }

    /// Whether the light has several zones, which Tuya doesn't say: in
    /// colour mode, given a DP 61 command painting every segment white, a
    /// light that has it reports the command back; others only report the
    /// power and the mode (panel kit and an unknown DP, 01/11/2026). `None`
    /// when the light reports nothing at all within `timeout`.
    pub async fn detect_zones(&self, timeout: Duration) -> Option<bool> {
        let mut events = self.listener();
        let white = lscsync_core::paint::Paint {
            color: Some(Hsv::new(0.0, 0.0, 1.0)),
            zones: (0..protocol::SCENE_SEGMENTS).collect(),
        };
        self.send_paint(&white).await.ok()?;
        let mut answered = false;
        let found = tokio::time::timeout(timeout, async {
            while let Some(event) = events.recv().await {
                if let rustuya::tokio::Event::Frame(frame) = event
                    && let Some(dps) = protocol::reported_dps(&frame.payload)
                {
                    if dps.contains_key(protocol::DP_PAINT) {
                        return true;
                    }
                    answered = true;
                }
            }
            false
        })
        .await
        .unwrap_or(false);
        (found || answered).then_some(found)
    }

    /// Asks the light what it shows. `None` without an answer in `timeout`.
    pub async fn read_state(&self, timeout: Duration) -> Option<protocol::LightState> {
        let mut events = self.listener();
        self.query().await.ok()?;
        tokio::time::timeout(timeout, async {
            while let Some(event) = events.recv().await {
                if let rustuya::tokio::Event::Frame(frame) = event
                    && let Some(state) = protocol::parse_status(&frame.payload)
                {
                    return Some(state);
                }
            }
            None
        })
        .await
        .ok()
        .flatten()
    }

    /// Puts the light back as it was: off, or white at its brightness. A
    /// color or a scene can't be read back: white instead.
    pub async fn restore(&self, state: Option<protocol::LightState>) -> Result<(), LscError> {
        match state {
            Some(protocol::LightState::Off) => self.power_off().await,
            Some(protocol::LightState::White(Some(brightness))) => {
                self.device
                    .set_value(protocol::DP_WHITE_BRIGHTNESS, brightness)
                    .await?;
                self.restore_white().await
            }
            _ => self.restore_white().await,
        }
    }

    pub async fn power_off(&self) -> Result<(), LscError> {
        self.device.set_value(protocol::DP_POWER, false).await?;
        // Fire-and-forget, like `restore_white`.
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.powered.store(false, Ordering::Relaxed);
        Ok(())
    }

    pub async fn restore_white(&self) -> Result<(), LscError> {
        self.device
            .set_value(protocol::DP_MODE, protocol::MODE_WHITE)
            .await?;
        // `set_value` is fire-and-forget: give the actor time to write the
        // frame before closing the connection.
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.mode.store(MODE_UNKNOWN, Ordering::Relaxed);
        Ok(())
    }

    pub async fn close(self) {
        self.device.close().await;
        self.discovery.close().await;
    }
}
