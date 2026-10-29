//! Palette, fonts and small building blocks shared by every screen.

use std::sync::{Arc, LazyLock};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

pub const BG: u32 = 0x0B0B0D;
pub const SURFACE: u32 = 0x151517;
pub const SURFACE_RAISED: u32 = 0x1F1F23;
pub const SURFACE_HOVER: u32 = 0x1C1C1F;
pub const LINE: u32 = 0x232326;
pub const LINE_STRONG: u32 = 0x2C2C30;
pub const TEXT: u32 = 0xF2F1EF;
pub const TEXT_SOFT: u32 = 0xC9C8C4;
pub const TEXT_MUTED: u32 = 0x8E8D89;
pub const TEXT_FAINT: u32 = 0x61605D;
pub const ACCENT: u32 = 0xE8A838;
pub const ACCENT_HOVER: u32 = 0xF0B44A;
pub const ON_ACCENT: u32 = 0x1A1206;
pub const SUCCESS: u32 = 0x34D399;
pub const DANGER_BG: u32 = 0x1A1213;
pub const DANGER_LINE: u32 = 0x4A2629;
pub const DANGER_TEXT: u32 = 0xF2A3A3;

/// On macOS the title bar is transparent: leave room for the traffic lights.
pub const TITLEBAR_INSET: f32 = if cfg!(target_os = "macos") { 28.0 } else { 0.0 };

/// Window width, and its height for the main page, the setup flow and the
/// device pages (they show the screen preview).
pub const WINDOW_WIDTH: f32 = 400.0;
pub const MAIN_HEIGHT: f32 = 610.0;
pub const SETUP_HEIGHT: f32 = 450.0;
pub const DEVICES_HEIGHT: f32 = 690.0;

pub const FONT: &str = "Geist";
pub const FONT_MONO: &str = "Geist Mono";

/// The app icon, embedded in the binary.
static LOGO: LazyLock<Arc<Image>> = LazyLock::new(|| {
    Arc::new(Image::from_bytes(
        ImageFormat::Png,
        include_bytes!("../../../assets/icon/lscsync-256.png").to_vec(),
    ))
});

pub fn app_logo(size: Pixels) -> Img {
    img(LOGO.clone()).size(size)
}

/// Side-by-side color bands with rounded outer corners. GPUI clips children
/// to a rectangle, not to the parent's radius, so the end bands round
/// themselves.
pub fn color_bands(colors: impl IntoIterator<Item = Rgba>, radius: Pixels) -> Div {
    let colors: Vec<Rgba> = colors.into_iter().collect();
    let last = colors.len().saturating_sub(1);
    div()
        .flex()
        .gap(px(2.0))
        .children(colors.into_iter().enumerate().map(|(index, color)| {
            div()
                .flex_1()
                .h_full()
                .bg(color)
                .when(index == 0, |band| band.rounded_l(radius))
                .when(index == last, |band| band.rounded_r(radius))
        }))
}

/// Full-width button: amber when it is the screen's main action.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    primary: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(42.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(12.0))
        .cursor_pointer()
        .text_size(px(13.5))
        .child(label.into())
        .map(|button| {
            if primary {
                button
                    .bg(rgb(ACCENT))
                    .text_color(rgb(ON_ACCENT))
                    .font_weight(FontWeight::SEMIBOLD)
                    .hover(|style| style.bg(rgb(ACCENT_HOVER)))
            } else {
                button
                    .bg(rgb(SURFACE))
                    .border_1()
                    .border_color(rgb(LINE))
                    .text_color(rgb(TEXT))
                    .font_weight(FontWeight::MEDIUM)
                    .hover(|style| style.bg(rgb(SURFACE_HOVER)).border_color(rgb(LINE_STRONG)))
            }
        })
}

/// Discreet text link.
pub fn link(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .cursor_pointer()
        .text_size(px(12.0))
        .text_color(rgb(TEXT_MUTED))
        .hover(|style| style.text_color(rgb(TEXT)))
        .child(label.into())
}

pub fn step_label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(11.0))
        .text_color(rgb(TEXT_FAINT))
        .child(text.into())
}

pub fn title(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(16.0))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

pub fn body(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.5))
        .line_height(px(19.0))
        .text_color(rgb(TEXT_MUTED))
        .child(text.into())
}

/// Bordered surface, for instructions or lists.
pub fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .p(px(12.0))
        .rounded(px(12.0))
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(LINE))
}

pub fn error_text(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.0))
        .text_color(rgb(DANGER_TEXT))
        .child(text.into())
}

/// Small uppercase title above a group of settings.
pub fn section_label(text: String) -> Div {
    div()
        .mt(px(4.0))
        .text_size(px(11.0))
        .text_color(rgb(TEXT_FAINT))
        .child(text)
}

/// Label on the left, control on the right.
pub fn row(label: String) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .text_size(px(13.0))
        .child(label)
}

/// On/off switch, amber when on.
pub fn toggle(id: &'static str, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .w(px(30.0))
        .h(px(18.0))
        .p(px(2.0))
        .rounded_full()
        .cursor_pointer()
        .flex()
        .when(on, |toggle| toggle.justify_end())
        .bg(rgb(if on { ACCENT } else { LINE_STRONG }))
        .child(
            div()
                .size(px(14.0))
                .rounded_full()
                .bg(rgb(if on { ON_ACCENT } else { TEXT_MUTED })),
        )
}
