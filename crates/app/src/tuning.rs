//! The adjustments under the main window's sync button: a few sliders per
//! mode, and the rest in an Advanced section, folded by default.

use gpui_kit::component::slider::{Slider, SliderState, SliderValue};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lscsync_core::{AudioHue, DarkScenes, Mode, Settings};
use rust_i18n::t;

use crate::theme::*;
use crate::{MUSIC_VIA_MICROPHONE, Root};

/// A slider, in its own units (percent, tenths, frames per second…).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Knob {
    Brightness,
    Saturation,
    Smoothing,
    Height,
    Reactivity,
    MusicGlow,
    ScreenGlow,
    Vivid,
    Contrast,
    FrameRate,
    FixedHue,
    HueSpeed,
}

impl Knob {
    pub const ALL: [Self; 12] = [
        Self::Brightness,
        Self::Saturation,
        Self::Smoothing,
        Self::Height,
        Self::Reactivity,
        Self::MusicGlow,
        Self::ScreenGlow,
        Self::Vivid,
        Self::Contrast,
        Self::FrameRate,
        Self::FixedHue,
        Self::HueSpeed,
    ];

    /// Minimum, maximum and step, in slider units.
    fn range(self) -> (f32, f32, f32) {
        match self {
            Self::Brightness => (1.0, 100.0, 1.0),
            Self::Saturation => (5.0, 30.0, 1.0),
            Self::Smoothing => (0.0, 95.0, 5.0),
            Self::Height => (5.0, 100.0, 5.0),
            Self::Reactivity => (5.0, 100.0, 5.0),
            Self::MusicGlow | Self::ScreenGlow => (0.0, 50.0, 1.0),
            Self::Vivid => (10.0, 100.0, 5.0),
            Self::Contrast => (0.0, 100.0, 5.0),
            Self::FrameRate => (10.0, 50.0, 5.0),
            Self::FixedHue => (0.0, 359.0, 1.0),
            Self::HueSpeed => (5.0, 120.0, 5.0),
        }
    }

    fn get(self, settings: &Settings) -> f32 {
        match self {
            Self::Brightness => settings.lum_max as f32,
            Self::Saturation => settings.boost_sat * 10.0,
            Self::Smoothing => settings.smoothing * 100.0,
            Self::Height => settings.capture_height * 100.0,
            Self::Reactivity => settings.reactivity * 100.0,
            Self::MusicGlow => settings.audio_min_light * 100.0,
            Self::ScreenGlow => settings.screen_min_light * 100.0,
            Self::Vivid => settings.vivid_priority * 10.0,
            Self::Contrast => settings.contrast * 100.0,
            Self::FrameRate => settings.frame_rate as f32,
            Self::FixedHue => settings.audio_fixed_hue as f32,
            Self::HueSpeed => settings.hue_speed as f32,
        }
    }

    fn set(self, settings: &mut Settings, value: f32) {
        match self {
            Self::Brightness => settings.lum_max = value.round() as u8,
            Self::Saturation => settings.boost_sat = value / 10.0,
            Self::Smoothing => settings.smoothing = value / 100.0,
            Self::Height => settings.capture_height = value / 100.0,
            Self::Reactivity => settings.reactivity = value / 100.0,
            Self::MusicGlow => settings.audio_min_light = value / 100.0,
            Self::ScreenGlow => settings.screen_min_light = value / 100.0,
            Self::Vivid => settings.vivid_priority = value / 10.0,
            Self::Contrast => settings.contrast = value / 100.0,
            Self::FrameRate => settings.frame_rate = value.round() as u8,
            Self::FixedHue => settings.audio_fixed_hue = value.round() as u16,
            Self::HueSpeed => settings.hue_speed = value.round() as u16,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Brightness => t!("tuning.brightness"),
            Self::Saturation => t!("tuning.saturation"),
            Self::Smoothing => t!("tuning.smoothing"),
            Self::Height => t!("tuning.height"),
            Self::Reactivity => t!("tuning.reactivity"),
            Self::MusicGlow | Self::ScreenGlow => t!("tuning.min_glow"),
            Self::Vivid => t!("tuning.vivid"),
            Self::Contrast => t!("tuning.contrast"),
            Self::FrameRate => t!("tuning.frame_rate"),
            Self::FixedHue => t!("tuning.hue"),
            Self::HueSpeed => t!("tuning.hue_speed"),
        }
        .to_string()
    }

    fn format(self, value: f32) -> String {
        match self {
            Self::Saturation | Self::Vivid => format!("×{:.1}", value / 10.0),
            Self::FrameRate => t!("tuning.fps", count = value.round() as u32).to_string(),
            Self::FixedHue => format!("{}°", value.round() as u32),
            Self::HueSpeed => format!("{} s", value.round() as u32),
            _ => format!("{}%", value.round() as u32),
        }
    }
}

/// One slider per knob.
pub fn sliders(settings: &Settings, cx: &mut Context<Root>) -> Vec<(Knob, Entity<SliderState>)> {
    Knob::ALL
        .into_iter()
        .map(|knob| {
            let (min, max, step) = knob.range();
            let value = knob.get(settings).clamp(min, max);
            let state = cx.new(|_| {
                SliderState::new()
                    .min(min)
                    .max(max)
                    .step(step)
                    .default_value(value)
            });
            (knob, state)
        })
        .collect()
}

impl Root {
    /// Moves the sliders to the profile's settings (after an import).
    pub(crate) fn sync_sliders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (knob, slider) in &self.sliders {
            let (min, max, _) = knob.range();
            let value = knob.get(&self.profile.settings).clamp(min, max);
            slider.update(cx, |slider, cx| slider.set_value(value, window, cx));
        }
        self.sliders_stale = false;
    }

    /// Reads the sliders into the settings.
    pub(crate) fn read_sliders(&mut self, cx: &App) {
        for (knob, slider) in &self.sliders {
            knob.set(&mut self.profile.settings, slider_value(slider, cx));
        }
    }

    /// A setting that is not a slider changed: applied live and saved.
    fn change_settings(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        change(&mut self.profile.settings);
        if let Some(engine) = &self.engine {
            engine.update(self.profile.settings.clone());
        }
        self.save();
        cx.notify();
    }

    fn knob_row(&self, knob: Knob, cx: &Context<Self>) -> Option<Div> {
        let (_, state) = self.sliders.iter().find(|(k, _)| *k == knob)?;
        let value = slider_value(state, cx);
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(12.0))
                .h(px(38.0))
                .child(label_cell(knob.label()))
                .child(
                    Slider::new(state)
                        .horizontal()
                        .flex_1()
                        .bg(rgb(ACCENT))
                        .text_color(rgb(TEXT)),
                )
                .child(
                    div()
                        .w(px(46.0))
                        .flex()
                        .justify_end()
                        .font_family(FONT_MONO)
                        .text_size(px(12.0))
                        .text_color(rgb(TEXT_SOFT))
                        .child(knob.format(value)),
                ),
        )
    }

    pub(crate) fn render_settings(&self, cx: &mut Context<Self>) -> Div {
        let settings = &self.profile.settings;
        let mode = settings.mode;
        let (main, advanced): (&[Knob], &[Knob]) = match mode {
            Mode::ScreenZones => (
                &[Knob::Brightness, Knob::Saturation, Knob::Smoothing],
                &[
                    Knob::ScreenGlow,
                    Knob::Vivid,
                    Knob::Contrast,
                    Knob::FrameRate,
                ],
            ),
            Mode::Screen => (
                &[
                    Knob::Brightness,
                    Knob::Saturation,
                    Knob::Smoothing,
                    Knob::Height,
                ],
                &[Knob::ScreenGlow, Knob::Vivid, Knob::FrameRate],
            ),
            Mode::Audio => (
                &[Knob::Brightness, Knob::Reactivity, Knob::MusicGlow],
                if settings.audio_hue == AudioHue::Fixed {
                    &[Knob::FixedHue]
                } else {
                    &[Knob::HueSpeed]
                },
            ),
        };
        let open = self.advanced_open;

        let mut rows = div().flex().flex_col();
        for knob in main {
            rows = rows.children(self.knob_row(*knob, cx));
        }
        if mode == Mode::Audio && *MUSIC_VIA_MICROPHONE {
            // macOS 12 cannot capture the computer's sound without a driver:
            // say so, and how to get the best of the microphone.
            rows = rows.child(
                div()
                    .mt(px(6.0))
                    .text_size(px(11.5))
                    .line_height(px(17.0))
                    .text_color(rgb(TEXT_MUTED))
                    .child(t!("tuning.microphone_note").to_string()),
            );
        }

        let mut extra = div().flex().flex_col();
        if open {
            if mode == Mode::Audio {
                extra = extra.child(choice_row(
                    t!("tuning.music_color").to_string(),
                    vec![
                        (
                            "hue-cycle",
                            t!("tuning.rainbow").to_string(),
                            settings.audio_hue == AudioHue::Cycle,
                            Box::new(cx.listener(|this, _, _, cx| {
                                this.change_settings(|s| s.audio_hue = AudioHue::Cycle, cx)
                            })) as ClickHandler,
                        ),
                        (
                            "hue-fixed",
                            t!("tuning.fixed").to_string(),
                            settings.audio_hue == AudioHue::Fixed,
                            Box::new(cx.listener(|this, _, _, cx| {
                                this.change_settings(|s| s.audio_hue = AudioHue::Fixed, cx)
                            })) as ClickHandler,
                        ),
                    ],
                ));
            } else {
                let dark = settings.dark_scenes;
                let option =
                    |id: &'static str, key: &str, value: DarkScenes, cx: &mut Context<Self>| {
                        (
                            id,
                            t!(key).to_string(),
                            dark == value,
                            Box::new(cx.listener(
                                move |this: &mut Self, _: &ClickEvent, _: &mut Window, cx| {
                                    this.change_settings(|s| s.dark_scenes = value, cx)
                                },
                            )) as ClickHandler,
                        )
                    };
                extra = extra.child(choice_row(
                    t!("tuning.dark").to_string(),
                    vec![
                        option("dark-follow", "tuning.dark_follow", DarkScenes::Follow, cx),
                        option("dark-hold", "tuning.dark_hold", DarkScenes::Hold, cx),
                        option("dark-dim", "tuning.dark_dim", DarkScenes::Dim, cx),
                    ],
                ));
            }
            for knob in advanced {
                extra = extra.children(self.knob_row(*knob, cx));
            }
            if mode == Mode::Audio {
                extra = extra.child(
                    div().h(px(38.0)).flex().items_center().child(
                        row(t!("tuning.wave").to_string())
                            .flex_1()
                            .text_color(rgb(TEXT_MUTED))
                            .child(toggle("wave", settings.audio_zones).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.change_settings(|s| s.audio_zones = !s.audio_zones, cx)
                                },
                            ))),
                    ),
                );
            }
            extra = extra.child(
                div().h(px(38.0)).flex().items_center().child(
                    row(t!("tuning.restore_white").to_string())
                        .flex_1()
                        .text_color(rgb(TEXT_MUTED))
                        .child(toggle("restore-white", settings.restore_white).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.change_settings(|s| s.restore_white = !s.restore_white, cx)
                            }),
                        )),
                ),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(section_label(t!("tuning.heading").to_string()).mt(px(0.0)))
            .child(rows)
            .child(
                div()
                    .id("advanced")
                    .flex()
                    .items_center()
                    .justify_between()
                    .mt(px(4.0))
                    .py(px(8.0))
                    .border_t_1()
                    .border_color(rgb(0x1C1C1F))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(rgb(TEXT_SOFT))
                    .hover(|style| style.text_color(rgb(TEXT)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.advanced_open = !this.advanced_open;
                        cx.notify();
                    }))
                    .child(t!("tuning.advanced").to_string())
                    .child(div().text_color(rgb(TEXT_FAINT)).child(if open {
                        "▾"
                    } else {
                        "▸"
                    })),
            )
            .child(extra)
    }
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Slider label, the same width on every row so the sliders line up.
fn label_cell(label: String) -> Div {
    div()
        .w(px(96.0))
        .flex_none()
        .text_size(px(13.0))
        .text_color(rgb(TEXT_MUTED))
        .child(label)
}

/// A label and a few mutually exclusive options.
fn choice_row(label: String, options: Vec<(&'static str, String, bool, ClickHandler)>) -> Div {
    let choices = div()
        .flex()
        .gap(px(2.0))
        .p(px(2.0))
        .rounded(px(8.0))
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(LINE))
        .children(options.into_iter().map(|(id, text, chosen, on_click)| {
            div()
                .id(id)
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(6.0))
                .text_size(px(11.5))
                .map(|option| {
                    if chosen {
                        option.bg(rgb(SURFACE_RAISED)).text_color(rgb(TEXT))
                    } else {
                        option
                            .cursor_pointer()
                            .text_color(rgb(TEXT_MUTED))
                            .hover(|style| style.text_color(rgb(TEXT)))
                            .on_click(on_click)
                    }
                })
                .child(text)
        }));
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .h(px(38.0))
        .child(label_cell(label))
        .child(div().flex_1())
        .child(choices)
}

pub fn slider_value(state: &Entity<SliderState>, cx: &App) -> f32 {
    match state.read(cx).value() {
        SliderValue::Single(v) => v,
        _ => 0.0,
    }
}
