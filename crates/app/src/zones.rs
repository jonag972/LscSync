//! Where each device looks on the screen: the zone editor of a device's
//! page, and the overview of every synced device on the devices page.

use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lscsync_core::{Product, Zone};
use lscsync_engine::capture::ScreenCapture;
use rust_i18n::t;

use crate::theme::*;
use crate::{Page, Root};

/// Width of the screen preview, the window's content width.
pub const PREVIEW_WIDTH: f32 = WINDOW_WIDTH - 44.0;
/// Height / width of the preview until the screen is captured.
const DEFAULT_RATIO: f32 = 10.0 / 16.0;
/// One color per zone number, so zones are easy to tell apart.
const ZONE_COLORS: [u32; 6] = [0xD9662B, 0x7A4FD0, 0x1FA38A, 0xE8A838, 0x378ADD, 0xD4537E];

/// Small capture of the screen, behind the zones.
pub struct ScreenPreview {
    image: Arc<Image>,
    /// Height / width.
    ratio: f32,
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
    /// Captures the screen in the background for the previews.
    pub(crate) fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let captured = cx
                .background_executor()
                .spawn(async {
                    ScreenCapture::new()
                        .and_then(|capture| capture.preview_png(PREVIEW_WIDTH as u32 * 2))
                })
                .await;
            let _ = this.update(cx, |root, cx| match captured {
                Ok((png, ratio)) => {
                    root.preview = Some(ScreenPreview {
                        image: Arc::new(Image::from_bytes(ImageFormat::Png, png)),
                        ratio,
                    });
                    cx.notify();
                }
                Err(error) => tracing::warn!("screen preview: {error}"),
            });
        })
        .detach();
    }

    pub(crate) fn open_device(&mut self, id: String, cx: &mut Context<Self>) {
        self.page = Page::Device(id);
        self.selected_zone = None;
        self.refresh_preview(cx);
        cx.notify();
    }

    /// Back to the devices page: the highlighted zone goes back to normal.
    fn close_device(&mut self, cx: &mut Context<Self>) {
        self.select_zone(None);
        self.page = Page::Devices;
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

    /// Follows the mouse while a zone is dragged (bound to the whole page,
    /// so the drag goes on when the mouse leaves the preview).
    pub(crate) fn on_drag_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        let height = PREVIEW_WIDTH * self.preview_ratio();
        let dx = f32::from(event.position.x - drag.start.x) / PREVIEW_WIDTH;
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

    /// The screen preview, `PREVIEW_WIDTH` wide, with `zones` drawn on it.
    fn screen(&self, zones: Vec<Stateful<Div>>) -> Div {
        let height = PREVIEW_WIDTH * self.preview_ratio();
        div()
            .relative()
            .w(px(PREVIEW_WIDTH))
            .h(px(height))
            .rounded(px(8.0))
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(LINE_STRONG))
            .when_some(self.preview.as_ref(), |screen, preview| {
                screen.child(
                    img(preview.image.clone())
                        .absolute()
                        .inset_0()
                        .size_full()
                        .rounded(px(8.0))
                        .opacity(0.55),
                )
            })
            .children(zones)
    }

    /// A zone on the preview, numbered from 1.
    fn zone_box(
        &self,
        id: impl Into<ElementId>,
        zone: Zone,
        index: usize,
        label: String,
    ) -> Stateful<Div> {
        let height = PREVIEW_WIDTH * self.preview_ratio();
        let color = ZONE_COLORS[index % ZONE_COLORS.len()];
        div()
            .id(id)
            .absolute()
            .left(px(zone.x * PREVIEW_WIDTH))
            .top(px(zone.y * height))
            .w(px(zone.w * PREVIEW_WIDTH))
            .h(px(zone.h * height))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .border_2()
            .border_color(rgb(color))
            .bg(rgba((color << 8) | 0x59))
            .text_size(px(11.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(0xFFFFFF))
            .child(label)
    }

    /// A device's page: its zones on the screen, and whether it is synced.
    pub(crate) fn render_device(&self, id: &str, cx: &mut Context<Self>) -> Div {
        let Some(device) = self.profile.devices.iter().find(|d| d.id == id) else {
            return div();
        };
        let zones = self.zones_of(id);
        let count = zones.len();
        let boxes = zones
            .into_iter()
            .enumerate()
            .map(|(index, zone)| {
                let selected = self.selected_zone == Some(index);
                let (move_id, resize_id) = (id.to_owned(), id.to_owned());
                self.zone_box(("zone", index), zone, index, (index + 1).to_string())
                    .cursor_move()
                    .when(selected, |zone| zone.border_color(rgb(0xFFFFFF)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.start_drag(&move_id, index, false, event.position);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
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
                                    this.start_drag(&resize_id, index, true, event.position);
                                    cx.stop_propagation();
                                    cx.notify();
                                }),
                            ),
                    )
            })
            .collect();
        let synced = device.synced;
        let counts = Product::of(device).zone_counts();
        let (toggle_id, reset_id) = (id.to_owned(), id.to_owned());
        let hint = if self.engine.is_some() {
            t!("zones.hint_running")
        } else {
            t!("zones.hint_stopped")
        };

        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .flex_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(title(device.name.clone()))
                    .child(
                        link("back", t!("action.back").to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.close_device(cx))),
                    ),
            )
            .child(section_label(
                t!("zones.heading", count = count).to_string(),
            ))
            .child(self.screen(boxes))
            .child(body(hint.to_string()))
            .child(
                card()
                    .when(counts.start() != counts.end(), |card| {
                        card.child(
                            row(t!("zones.panels").to_string())
                                .child(self.render_count(id, count, counts, cx)),
                        )
                    })
                    .child(row(t!("zones.in_sync").to_string()).child(
                        toggle("in-sync", synced).on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_synced(&toggle_id, cx);
                        })),
                    )),
            )
            .child(div().flex_1())
            .child(div().flex().justify_center().child(
                link("reset-zones", t!("zones.reset").to_string()).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.set_zones(&reset_id, Vec::new());
                        let zones = this.zones_of(&reset_id);
                        this.set_zones(&reset_id, zones);
                        this.save();
                        cx.notify();
                    },
                )),
            ))
    }

    /// − n + to change how many panels the kit has.
    fn render_count(
        &self,
        id: &str,
        count: usize,
        counts: std::ops::RangeInclusive<usize>,
        cx: &mut Context<Self>,
    ) -> Div {
        let step = |label: &'static str, target: usize, cx: &mut Context<Self>| {
            let id = id.to_owned();
            let enabled = counts.contains(&target);
            div()
                .id(label)
                .w(px(26.0))
                .h(px(24.0))
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_zone_count(&id, target);
                            cx.notify();
                        }))
                })
                .child(label)
        };
        div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(step("−", count.saturating_sub(1), cx))
            .child(
                div()
                    .w(px(18.0))
                    .flex()
                    .justify_center()
                    .font_family(FONT_MONO)
                    .child(count.to_string()),
            )
            .child(step("+", count + 1, cx))
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
        self.selected_zone = None;
        self.set_zones(id, zones);
        self.save();
    }

    /// Every synced device's zones on the screen, numbered per device.
    pub(crate) fn render_overview(&self) -> Div {
        let mut boxes = Vec::new();
        for (number, device) in self.profile.sync_targets().enumerate() {
            for (index, zone) in self.zones_of(&device.id).into_iter().enumerate() {
                let label = if device.layout(0.0).len() > 1 {
                    format!("{}.{}", number + 1, index + 1)
                } else {
                    (number + 1).to_string()
                };
                boxes.push(self.zone_box(
                    SharedString::from(format!("overview-{}-{index}", device.id)),
                    zone,
                    number,
                    label,
                ));
            }
        }
        self.screen(boxes)
    }
}
