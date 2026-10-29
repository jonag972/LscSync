//! Main page: on the left, the screen with what each light reflects and the
//! modes; on the right, the main button, the lights and the adjustments.
//! While the sync runs, the background takes a faint tint of the lights.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lscsync_core::{Mode, Rgb};
use rust_i18n::t;

use crate::theme::*;
use crate::{AUDIO_SUPPORTED, Page, Phase, Root, display_color, mode_label};

/// Strength of the lights' tint over the background.
const TINT_STRENGTH: f32 = 0.14;
/// Share of the way to the lights' color the tint covers every 40 ms:
/// about a second to settle.
const TINT_EASING: f32 = 0.05;

impl Root {
    pub(crate) fn render_home(&self, cx: &mut Context<Self>) -> Div {
        let phase = self.phase();
        let left = div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .p(px(PAGE_PADDING))
            .pt(px(TITLEBAR_INSET + PAGE_PADDING))
            .child(self.render_status(phase))
            .child(self.render_screen(cx))
            .child(self.render_modes(cx))
            .child(match self.selected_light.clone() {
                Some(id) => self
                    .render_light_bar(&id, cx)
                    .unwrap_or_else(|| self.render_mode_hint()),
                None => self.render_mode_hint(),
            });

        let right = div()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .p(px(PAGE_PADDING))
            .pt(px(TITLEBAR_INSET + PAGE_PADDING))
            .border_l_1()
            .border_color(rgba(0xFFFFFF0F))
            .child(self.render_primary(phase, cx))
            .children(self.render_update_notice(cx))
            .children(self.render_error(phase))
            .child(
                // Scrolls when the Advanced adjustments do not fit.
                div()
                    .id("sidebar")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(self.render_lights(cx))
                    .child(self.render_settings(cx)),
            )
            .child(self.render_footer(cx));

        div()
            .size_full()
            .flex()
            .on_mouse_move(cx.listener(|this, event, _, cx| this.on_drag_move(event, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_drag(cx)),
            )
            .child(left)
            .child(right)
    }

    fn render_status(&self, phase: Phase) -> Div {
        let (dot, label) = match phase {
            Phase::Idle => (TEXT_FAINT, t!("status.paused")),
            Phase::Connecting => (ACCENT, t!("status.connecting")),
            Phase::Running => (SUCCESS, t!("status.live")),
            Phase::Failed => (DANGER_TEXT, t!("status.error")),
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(px(15.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("LscSync"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.5))
                    .text_color(rgb(TEXT_MUTED))
                    .child(div().size(px(6.0)).rounded_full().bg(rgb(dot)))
                    .child(label.to_string()),
            )
    }

    fn render_modes(&self, cx: &mut Context<Self>) -> Div {
        let current = self.profile.settings.mode;
        div()
            .flex()
            .gap(px(6.0))
            .children(Mode::ALL.into_iter().map(|mode| {
                let available = mode != Mode::Audio || *AUDIO_SUPPORTED;
                let chosen = mode == current;
                div()
                    .id(match mode {
                        Mode::ScreenZones => "mode-screen",
                        Mode::Screen => "mode-ambience",
                        Mode::Audio => "mode-music",
                        Mode::Fixed => "mode-color",
                    })
                    .flex_1()
                    .flex()
                    .justify_center()
                    .py(px(8.0))
                    .rounded(px(10.0))
                    .border_1()
                    .text_size(px(12.5))
                    .map(|pill| {
                        if chosen {
                            pill.bg(rgba(0x1A1712E6))
                                .border_color(rgb(ACCENT))
                                .text_color(rgb(TEXT))
                                .font_weight(FontWeight::MEDIUM)
                        } else if available {
                            pill.bg(rgba(0x121215BF))
                                .border_color(rgba(0xFFFFFF12))
                                .text_color(rgb(TEXT_MUTED))
                                .cursor_pointer()
                                .hover(|style| style.text_color(rgb(TEXT)))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)),
                                )
                        } else {
                            pill.border_color(rgba(0xFFFFFF0A))
                                .text_color(rgb(TEXT_FAINT))
                        }
                    })
                    .child(mode_label(mode))
            }))
    }

    /// One sentence on what the current mode does.
    fn render_mode_hint(&self) -> Div {
        let text = match self.profile.settings.mode {
            Mode::ScreenZones => t!("home.screen"),
            Mode::Screen => t!("home.ambience"),
            Mode::Audio => t!("home.music"),
            Mode::Fixed => t!("home.color"),
        };
        div()
            .text_size(px(12.0))
            .text_color(rgb(TEXT_MUTED))
            .child(text.to_string())
    }

    fn render_primary(&self, phase: Phase, cx: &mut Context<Self>) -> Stateful<Div> {
        let (label, filled) = match phase {
            Phase::Idle => (t!("action.start"), true),
            Phase::Failed => (t!("action.retry"), true),
            Phase::Connecting => (t!("action.cancel"), false),
            Phase::Running => (t!("action.stop"), false),
        };
        button("primary", label.to_string(), filled)
            .h(px(44.0))
            .on_click(cx.listener(|this, _, _, cx| this.primary_action(cx)))
    }

    /// "My lights": each light's color, name, state, and whether it is used
    /// in the current mode. A click selects it.
    fn render_lights(&self, cx: &mut Context<Self>) -> Div {
        let mode = self.profile.settings.mode;
        let running = self.engine.is_some();
        let mut list = div().flex().flex_col().gap(px(6.0));
        for (index, device) in self.profile.devices.iter().enumerate() {
            let used = device.modes.contains(&mode);
            let unreachable = running && self.unreachable.contains(&device.name);
            let selected = self.selected_light.as_deref() == Some(device.id.as_str());
            let state = if unreachable {
                Some(t!("lights.unreachable"))
            } else if !used {
                Some(t!("lights.unused"))
            } else {
                None
            };
            let (select_id, toggle_id) = (device.id.clone(), device.id.clone());
            list = list.child(
                div()
                    .id(SharedString::from(format!("light-{}", device.id)))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .rounded(px(12.0))
                    .bg(rgba(if selected { 0x1F1F23E6 } else { 0x151517CC }))
                    .border_1()
                    .border_color(if selected {
                        rgb(LINE_STRONG)
                    } else {
                        rgba(0xFFFFFF12)
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = select_id.clone();
                        let already = this.selected_light.as_deref() == Some(id.as_str());
                        this.select_light((!already).then_some(id), cx);
                    }))
                    .child(
                        div()
                            .size(px(11.0))
                            .flex_none()
                            .rounded_full()
                            .bg(rgb(if used {
                                identity_color(index)
                            } else {
                                TEXT_FAINT
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(rgb(if used { TEXT } else { TEXT_MUTED }))
                                    .truncate()
                                    .child(device.name.clone()),
                            )
                            .when_some(state, |this, state| {
                                this.child(
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(rgb(if unreachable {
                                            DANGER_TEXT
                                        } else {
                                            TEXT_FAINT
                                        }))
                                        .child(state.to_string()),
                                )
                            }),
                    )
                    .child(
                        toggle(SharedString::from(format!("use-{}", device.id)), used).on_click(
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_synced(&toggle_id, cx);
                            }),
                        ),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(section_label(t!("lights.heading").to_string()).mt(px(0.0)))
            .child(list)
    }

    fn render_error(&self, phase: Phase) -> Option<Div> {
        if phase != Phase::Failed {
            return None;
        }
        let (title, body) = self.describe_error();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .p(px(12.0))
                .rounded(px(12.0))
                .bg(rgb(DANGER_BG))
                .border_1()
                .border_color(rgb(DANGER_LINE))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(DANGER_TEXT))
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .line_height(px(16.0))
                        .text_color(rgb(TEXT_MUTED))
                        .child(body),
                ),
        )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .pt(px(10.0))
            .border_t_1()
            .border_color(rgba(0xFFFFFF0F))
            .text_size(px(11.5))
            .child(
                link("manage", t!("footer.manage").to_string())
                    .text_size(px(11.5))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.page = Page::Devices;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .gap(px(14.0))
                    .child(
                        link("settings", t!("footer.settings").to_string())
                            .text_size(px(11.5))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.page = Page::Settings;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("quit")
                            .cursor_pointer()
                            .text_color(rgb(TEXT_FAINT))
                            .hover(|style| style.text_color(rgb(TEXT_MUTED)))
                            .on_click(|_, _, cx| cx.quit())
                            .child(t!("footer.quit").to_string()),
                    ),
            )
    }

    /// Moves the background tint toward the lights' average color while the
    /// sync runs, back to plain when it stops. True when it changed.
    pub(crate) fn update_tint(&mut self) -> bool {
        let target = self
            .snapshot
            .as_ref()
            .filter(|_| self.preferences.ambient_background)
            .and_then(|snapshot| {
                let colors: Vec<Rgba> = snapshot.all().map(|c| display_color(*c)).collect();
                (!colors.is_empty()).then(|| {
                    let n = colors.len() as f32;
                    Rgb::new(
                        colors.iter().map(|c| c.r).sum::<f32>() / n,
                        colors.iter().map(|c| c.g).sum::<f32>() / n,
                        colors.iter().map(|c| c.b).sum::<f32>() / n,
                    )
                })
            });
        let level_target = if target.is_some() { 1.0 } else { 0.0 };
        if (self.tint_level - level_target).abs() < 0.005 && target.is_none() {
            return false;
        }
        self.tint_level += (level_target - self.tint_level) * TINT_EASING;
        if let Some(target) = target {
            let current = self.tint.unwrap_or(target);
            self.tint = Some(Rgb::new(
                current.r + (target.r - current.r) * TINT_EASING,
                current.g + (target.g - current.g) * TINT_EASING,
                current.b + (target.b - current.b) * TINT_EASING,
            ));
        }
        true
    }

    /// The window background: plain, or tinted by the lights.
    pub(crate) fn background(&self) -> Rgba {
        let base = rgb(BG);
        let Some(tint) = self.tint else {
            return base;
        };
        let amount = TINT_STRENGTH * self.tint_level;
        Rgba {
            r: base.r + (tint.r - base.r) * amount,
            g: base.g + (tint.g - base.g) * amount,
            b: base.b + (tint.b - base.b) * amount,
            a: 1.0,
        }
    }
}
