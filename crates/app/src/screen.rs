//! The screen on the main page: a live capture in a monitor frame, with the
//! part of the screen each light reflects drawn on it. A selected light's
//! zones can be moved and resized right there, and a light can be calibrated.

use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lscsync_core::{ColorCommand, Hsv, Mode, Product, Zone};
use lscsync_engine::capture::ScreenCapture;
use lscsync_engine::tuya::{LscConfig, flip_colors_blocking};
use rust_i18n::t;

use crate::theme::*;
use crate::{Root, display_color};

/// Width of the monitor drawing, frame included.
pub const MONITOR_WIDTH: f32 = WINDOW_WIDTH - SIDEBAR_WIDTH - 2.0 * PAGE_PADDING;
/// Thickness of the monitor's frame around the picture.
const BEZEL: f32 = 7.0;
/// Width of the picture inside the frame.
const SCREEN_WIDTH: f32 = MONITOR_WIDTH - 2.0 * BEZEL;
/// Height / width of the picture until the screen is captured.
const DEFAULT_RATIO: f32 = 9.0 / 16.0;
/// How long the light flips between two colors before the question.
const CALIBRATION_FLIPS: std::time::Duration = std::time::Duration::from_secs(4);

/// Small capture of the screen, behind the zones.
pub struct ScreenPreview {
    image: Arc<Image>,
    /// Height / width.
    ratio: f32,
}

/// Finding how a light changes color: each candidate command flips the
/// light between red and blue, then the user says whether it was sharp.
pub struct Calibration {
    device: String,
    /// Commands left to try, the one being tried first.
    candidates: Vec<ColorCommand>,
    step: CalibrationStep,
}

enum CalibrationStep {
    Flipping,
    Asking,
    Done(String),
}

/// A zone being moved, or resized from its corner.
pub struct Drag {
    device: String,
    zone: usize,
    resize: bool,
    start: Point<Pixels>,
    origin: Zone,
}

impl Root {
    /// Captures the screen in the background for the preview.
    pub(crate) fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        if self.capturing {
            return;
        }
        self.capturing = true;
        cx.spawn(async move |this, cx| {
            let captured = cx
                .background_executor()
                .spawn(async {
                    ScreenCapture::new()
                        .and_then(|capture| capture.preview_png(SCREEN_WIDTH as u32 * 2))
                })
                .await;
            let _ = this.update(cx, |root, cx| {
                root.capturing = false;
                match captured {
                    Ok((png, ratio)) => {
                        root.preview = Some(ScreenPreview {
                            image: Arc::new(Image::from_bytes(ImageFormat::Png, png)),
                            ratio,
                        });
                        cx.notify();
                    }
                    Err(error) => tracing::warn!("screen preview: {error}"),
                }
            });
        })
        .detach();
    }

    /// Selects a light to edit its zones and see its options, or nothing.
    pub(crate) fn select_light(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.select_zone(None);
        if self
            .calibration
            .as_ref()
            .is_some_and(|c| matches!(c.step, CalibrationStep::Done(_)))
        {
            self.calibration = None;
        }
        self.selected_light = id;
        cx.notify();
    }

    fn select_zone(&mut self, zone: Option<(String, usize)>) {
        self.selected_zone = zone.as_ref().map(|(_, index)| *index);
        if let Some(engine) = &self.engine {
            engine.highlight(zone);
        }
    }

    fn zones_of(&self, id: &str) -> Vec<Zone> {
        let height = self.profile.settings.capture_height;
        self.profile
            .devices
            .iter()
            .find(|device| device.id == id)
            .map(|device| device.layout(height))
            .unwrap_or_default()
    }

    /// Saves a device's zones in the profile and hands them to the engine.
    fn set_zones(&mut self, id: &str, zones: Vec<Zone>) {
        if let Some(device) = self.profile.devices.iter_mut().find(|d| d.id == id) {
            device.zones = zones.clone();
        }
        if let Some(engine) = &self.engine {
            engine.set_zones(id.to_owned(), zones);
        }
    }

    fn start_drag(&mut self, device: &str, zone: usize, resize: bool, start: Point<Pixels>) {
        let Some(origin) = self.zones_of(device).get(zone).copied() else {
            return;
        };
        self.select_zone(Some((device.to_owned(), zone)));
        self.drag = Some(Drag {
            device: device.to_owned(),
            zone,
            resize,
            start,
            origin,
        });
    }

    /// Follows the mouse while a zone is dragged (bound to the whole window,
    /// so the drag goes on when the mouse leaves the preview).
    pub(crate) fn on_drag_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        let height = SCREEN_WIDTH * self.preview_ratio();
        let dx = f32::from(event.position.x - drag.start.x) / SCREEN_WIDTH;
        let dy = f32::from(event.position.y - drag.start.y) / height;
        let zone = if drag.resize {
            drag.origin.resized(dx, dy)
        } else {
            drag.origin.moved(dx, dy)
        };
        let (device, index) = (drag.device.clone(), drag.zone);
        let mut zones = self.zones_of(&device);
        if let Some(slot) = zones.get_mut(index) {
            *slot = zone;
            self.set_zones(&device, zones);
            cx.notify();
        }
    }

    pub(crate) fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            self.save();
            cx.notify();
        }
    }

    fn preview_ratio(&self) -> f32 {
        self.preview.as_ref().map_or(DEFAULT_RATIO, |p| p.ratio)
    }

    /// The monitor: the capture in a frame on a small stand, with each
    /// light's zones on it.
    pub(crate) fn render_screen(&self, cx: &mut Context<Self>) -> Div {
        let height = SCREEN_WIDTH * self.preview_ratio();
        let mode = self.profile.settings.mode;
        // The picture only matters when the lights follow it.
        let picture = match mode {
            Mode::ScreenZones | Mode::Screen => 0.8,
            Mode::Audio | Mode::Fixed => 0.25,
        };
        let screen = div()
            .relative()
            .w(px(SCREEN_WIDTH))
            .h(px(height))
            .rounded(px(5.0))
            .overflow_hidden()
            .bg(rgb(0x14161C))
            .when_some(self.preview.as_ref(), |screen, preview| {
                screen.child(
                    img(preview.image.clone())
                        .absolute()
                        .inset_0()
                        .size_full()
                        .opacity(picture),
                )
            })
            .children(self.zone_boxes(height, cx))
            // A click on the picture, outside any zone, closes the selection.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.select_light(None, cx)),
            );
        div()
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .p(px(BEZEL))
                    .rounded(px(11.0))
                    .bg(rgb(MONITOR))
                    .child(screen),
            )
            .child(
                div()
                    .w(px(MONITOR_WIDTH * 0.08))
                    .h(px(12.0))
                    .bg(rgb(MONITOR)),
            )
            .child(
                div()
                    .w(px(MONITOR_WIDTH * 0.24))
                    .h(px(7.0))
                    .rounded(px(4.0))
                    .bg(rgb(MONITOR)),
            )
    }

    /// The zones of every light used in the current mode. In Ambience mode,
    /// the band all the lights take their color from.
    fn zone_boxes(&self, height: f32, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let mode = self.profile.settings.mode;
        let running = self.engine.is_some();
        if mode == Mode::Screen {
            let band = Zone {
                h: self.profile.settings.capture_height,
                ..Zone::WHOLE_SCREEN
            };
            let color = self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.all().next().copied());
            return vec![zone_box("ambience-band", band, height, TEXT_SOFT, color)];
        }
        let editing = mode == Mode::ScreenZones;
        let mut boxes = Vec::new();
        for (index, device) in self.profile.devices.iter().enumerate() {
            if !device.modes.contains(&mode) {
                continue;
            }
            let selected = self.selected_light.as_deref() == Some(device.id.as_str());
            let dimmed = self.selected_light.is_some() && !selected;
            let live = self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.colors(&device.id));
            for (zone_index, zone) in self.zones_of(&device.id).into_iter().enumerate() {
                let color = live.and_then(|colors| colors.get(zone_index).copied());
                let id = SharedString::from(format!("zone-{}-{zone_index}", device.id));
                let mut zone_box = zone_box(id, zone, height, identity_color(index), color)
                    .when(dimmed, |zone| zone.opacity(0.35));
                if editing {
                    let device_id = device.id.clone();
                    zone_box = zone_box.cursor_move().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.selected_light = Some(device_id.clone());
                            this.start_drag(&device_id, zone_index, false, event.position);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    );
                    if selected {
                        let device_id = device.id.clone();
                        zone_box = zone_box
                            .when(running && self.selected_zone == Some(zone_index), |zone| {
                                zone.border_color(rgb(0xFFFFFF))
                            })
                            .child(
                                div()
                                    .absolute()
                                    .right(px(-5.0))
                                    .bottom(px(-5.0))
                                    .size(px(10.0))
                                    .rounded(px(3.0))
                                    .bg(rgb(0xFFFFFF))
                                    .cursor_nwse_resize()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                            this.start_drag(
                                                &device_id,
                                                zone_index,
                                                true,
                                                event.position,
                                            );
                                            cx.stop_propagation();
                                            cx.notify();
                                        }),
                                    ),
                            );
                    }
                }
                boxes.push(zone_box);
            }
        }
        boxes
    }

    /// Under the screen when a light is selected: its name, how many panels
    /// it has, calibration, back to its default zones.
    pub(crate) fn render_light_bar(&self, id: &str, cx: &mut Context<Self>) -> Option<Div> {
        let (index, device) = self
            .profile
            .devices
            .iter()
            .enumerate()
            .find(|(_, device)| device.id == id)?;
        let product = Product::of(device);
        let counts = product.zone_counts();
        let count = self.zones_of(id).len();
        let editing = self.profile.settings.mode == Mode::ScreenZones;
        let (count_id, reset_id) = (id.to_owned(), id.to_owned());

        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(14.0))
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(10.0))
            .bg(rgba(0x15151799))
            .border_1()
            .border_color(rgb(LINE))
            .text_size(px(12.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .flex_1()
                    .child(
                        div()
                            .size(px(10.0))
                            .rounded_full()
                            .bg(rgb(identity_color(index))),
                    )
                    .child(device.name.clone()),
            );
        if counts.start() != counts.end() {
            bar = bar.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .text_color(rgb(TEXT_MUTED))
                    .child(t!("zones.panels").to_string())
                    .child(stepper(count, counts, cx, move |this, count, cx| {
                        this.set_zone_count(&count_id, count);
                        cx.notify();
                    })),
            );
        }
        if product == Product::Light {
            let calibrate_id = id.to_owned();
            bar = bar.child(
                link("calibrate", t!("calibrate.start").to_string()).on_click(
                    cx.listener(move |this, _, _, cx| this.start_calibration(&calibrate_id, cx)),
                ),
            );
        }
        if editing {
            bar = bar.child(link("reset-zones", t!("zones.reset").to_string()).on_click(
                cx.listener(move |this, _, _, cx| {
                    let height = this.profile.settings.capture_height;
                    let zones = this
                        .profile
                        .devices
                        .iter()
                        .find(|d| d.id == reset_id)
                        .map(|d| Product::of(d).default_zones(height))
                        .unwrap_or_default();
                    this.set_zones(&reset_id, zones);
                    this.save();
                    cx.notify();
                }),
            ));
        }
        bar = bar.child(
            link("close-light", "✕")
                .on_click(cx.listener(|this, _, _, cx| this.select_light(None, cx))),
        );

        let hint = if editing {
            Some(if self.engine.is_some() {
                t!("zones.hint_running")
            } else {
                t!("zones.hint_stopped")
            })
        } else {
            None
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(bar)
                .children(self.render_calibration(id, cx))
                .when_some(hint, |this, hint| {
                    this.child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgb(TEXT_MUTED))
                            .child(hint.to_string()),
                    )
                }),
        )
    }

    fn set_zone_count(&mut self, id: &str, count: usize) {
        let height = self.profile.settings.capture_height;
        let Some(zones) = self
            .profile
            .devices
            .iter()
            .find(|device| device.id == id)
            .map(|device| device.layout_with(count, height))
        else {
            return;
        };
        self.select_zone(None);
        self.set_zones(id, zones);
        self.save();
    }

    fn start_calibration(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(device) = self.profile.devices.iter().find(|d| d.id == id) else {
            return;
        };
        let candidates = device.color_commands();
        let config = LscConfig::from(device);
        // The light accepts one connection at a time: the sync lets go first.
        if self.engine.is_some() {
            self.stop(cx);
        }
        self.calibration = Some(Calibration {
            device: id.to_owned(),
            candidates,
            step: CalibrationStep::Flipping,
        });
        self.flip(config, cx);
    }

    /// Flips the light with the first candidate, then asks.
    fn flip(&mut self, config: LscConfig, cx: &mut Context<Self>) {
        let Some(command) = self
            .calibration
            .as_ref()
            .and_then(|calibration| calibration.candidates.first().copied())
        else {
            return;
        };
        let previous = self.retiring.take();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let flipped = cx
                .background_executor()
                .spawn(async move {
                    drop(previous);
                    flip_colors_blocking(&config, command, CALIBRATION_FLIPS)
                })
                .await;
            let _ = this.update(cx, |root, cx| {
                if let Some(calibration) = &mut root.calibration {
                    calibration.step = match flipped {
                        Ok(()) => CalibrationStep::Asking,
                        Err(error) => {
                            tracing::warn!("calibration: {error}");
                            CalibrationStep::Done(t!("calibrate.failed").to_string())
                        }
                    };
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The user's answer: sharp keeps the command, otherwise the next one.
    fn answer_calibration(&mut self, sharp: bool, cx: &mut Context<Self>) {
        let Some(calibration) = &mut self.calibration else {
            return;
        };
        let id = calibration.device.clone();
        let tried = calibration.candidates.remove(0);
        let chosen = if sharp {
            Some((tried, t!("calibrate.sharp").to_string()))
        } else if calibration.candidates.is_empty() {
            Some((tried, t!("calibrate.fades").to_string()))
        } else {
            None
        };
        match chosen {
            Some((command, message)) => {
                calibration.step = CalibrationStep::Done(message);
                if let Some(device) = self.profile.devices.iter_mut().find(|d| d.id == id) {
                    device.color_command = Some(command);
                }
                self.save();
                cx.notify();
            }
            None => {
                calibration.step = CalibrationStep::Flipping;
                let config = self
                    .profile
                    .devices
                    .iter()
                    .find(|d| d.id == id)
                    .map(LscConfig::from);
                if let Some(config) = config {
                    self.flip(config, cx);
                }
            }
        }
    }

    /// The calibration in progress for a light: watch, then answer.
    fn render_calibration(&self, id: &str, cx: &mut Context<Self>) -> Option<Div> {
        let calibration = self.calibration.as_ref().filter(|c| c.device == id)?;
        let text = |text: String| {
            div()
                .text_size(px(12.0))
                .text_color(rgb(TEXT_SOFT))
                .child(text)
        };
        Some(match &calibration.step {
            CalibrationStep::Flipping => div().child(text(t!("calibrate.watch").to_string())),
            CalibrationStep::Done(message) => div().child(text(message.clone())),
            CalibrationStep::Asking => div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(text(t!("calibrate.question").to_string()).flex_1())
                .child(
                    button("calibrate-yes", t!("calibrate.yes").to_string(), true)
                        .h(px(32.0))
                        .px(px(14.0))
                        .on_click(cx.listener(|this, _, _, cx| this.answer_calibration(true, cx))),
                )
                .child(
                    button("calibrate-no", t!("calibrate.no").to_string(), false)
                        .h(px(32.0))
                        .px(px(14.0))
                        .on_click(cx.listener(|this, _, _, cx| this.answer_calibration(false, cx))),
                ),
        })
    }
}

/// A zone on the picture: outlined in its light's color, filled with the
/// color the light shows while syncing.
fn zone_box(
    id: impl Into<ElementId>,
    zone: Zone,
    height: f32,
    outline: u32,
    live: Option<Hsv>,
) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .left(px(zone.x * SCREEN_WIDTH))
        .top(px(zone.y * height))
        .w(px(zone.w * SCREEN_WIDTH))
        .h(px(zone.h * height))
        .rounded(px(4.0))
        .border_2()
        .border_color(rgb(outline))
        .map(|zone| match live {
            Some(color) => {
                let mut fill = display_color(color);
                fill.a = 0.6;
                zone.bg(fill)
            }
            None => zone,
        })
}

/// − n + within `counts`.
fn stepper(
    count: usize,
    counts: std::ops::RangeInclusive<usize>,
    cx: &mut Context<Root>,
    on_change: impl Fn(&mut Root, usize, &mut Context<Root>) + Clone + 'static,
) -> Div {
    let step = |label: &'static str, target: usize, cx: &mut Context<Root>| {
        let enabled = counts.contains(&target);
        let on_change = on_change.clone();
        div()
            .id(label)
            .w(px(24.0))
            .h(px(22.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(LINE))
            .text_color(rgb(if enabled { TEXT } else { TEXT_FAINT }))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(SURFACE_HOVER)))
                    .on_click(cx.listener(move |this, _, _, cx| on_change(this, target, cx)))
            })
            .child(label)
    };
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(step("−", count.saturating_sub(1), cx))
        .child(
            div()
                .w(px(16.0))
                .flex()
                .justify_center()
                .font_family(FONT_MONO)
                .text_color(rgb(TEXT))
                .child(count.to_string()),
        )
        .child(step("+", count + 1, cx))
}
