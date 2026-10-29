//! The adjustments in the main window's sidebar: a few sliders per mode,
//! described in words, and the rest under "More settings", with exact values.

use gpui_kit::component::slider::{Slider, SliderState, SliderValue};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lscsync_core::{AudioHue, DarkScenes, Hsv, Mode, Settings};
use rust_i18n::t;

use crate::theme::*;
use crate::{MUSIC_VIA_MICROPHONE, Root, display_color};

/// The delay Music mode applies by itself: for a Bluetooth speaker or
/// headphones, the time their sound takes to come out. Checked once.
static AUTOMATIC_MUSIC_DELAY: std::sync::LazyLock<u16> = std::sync::LazyLock::new(|| {
    lscsync_engine::audio::output::automatic_delay().as_millis() as u16
});

/// Hues offered as swatches, in degrees.
const SWATCH_HUES: [u16; 9] = [0, 25, 45, 60, 120, 175, 215, 265, 310];

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
    HueSpeed,
    /// Hue of the Color mode.
    Hue,
    /// Music mode: how long the lights wait for the sound (Bluetooth).
    MusicDelay,
}

impl Knob {
    pub const ALL: [Self; 13] = [
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
        Self::HueSpeed,
        Self::Hue,
        Self::MusicDelay,
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
            Self::HueSpeed => (5.0, 120.0, 5.0),
            Self::Hue => (0.0, 359.0, 1.0),
            Self::MusicDelay => (0.0, 500.0, 10.0),
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
            Self::HueSpeed => settings.hue_speed as f32,
            Self::Hue => settings.fixed_hue as f32,
            Self::MusicDelay => settings.music_delay.unwrap_or(*AUTOMATIC_MUSIC_DELAY) as f32,
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
            Self::HueSpeed => settings.hue_speed = value.round() as u16,
            Self::Hue => settings.fixed_hue = value.round() as u16,
            // Every slider is read on each move: the automatic delay only
            // becomes a chosen one when this slider moved.
            Self::MusicDelay => {
                if (value - self.get(settings)).abs() >= 1.0 {
                    settings.music_delay = Some(value.round() as u16);
                }
            }
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
            Self::HueSpeed => t!("tuning.hue_speed"),
            Self::Hue => t!("tuning.hue"),
            Self::MusicDelay => t!("tuning.music_delay"),
        }
        .to_string()
    }

    /// The words at both ends of a main slider.
    fn ends(self) -> Option<(String, String)> {
        let (low, high) = match self {
            Self::Brightness => (t!("tuning.brightness_low"), t!("tuning.brightness_high")),
            Self::Saturation => (t!("tuning.saturation_low"), t!("tuning.saturation_high")),
            Self::Smoothing => (t!("tuning.smoothing_low"), t!("tuning.smoothing_high")),
            Self::Reactivity => (t!("tuning.reactivity_low"), t!("tuning.reactivity_high")),
            _ => return None,
        };
        Some((low.to_string(), high.to_string()))
    }

    fn format(self, value: f32) -> String {
        match self {
            Self::Saturation | Self::Vivid => format!("×{:.1}", value / 10.0),
            Self::FrameRate => t!("tuning.fps", count = value.round() as u32).to_string(),
            Self::Hue => format!("{}°", value.round() as u32),
            Self::HueSpeed => format!("{} s", value.round() as u32),
            Self::MusicDelay => format!("{} ms", value.round() as u32),
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
    /// Moves the sliders to the profile's settings (after an import, or a
    /// swatch that changed a hue).
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

    /// A setting that is not a slider changed: applied live and saved. The
    /// sliders follow, in case it is one of their values.
    fn change_settings(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        change(&mut self.profile.settings);
        if let Some(engine) = &self.engine {
            engine.update(self.profile.settings.clone());
        }
        self.save();
        self.sliders_stale = true;
        cx.notify();
    }

    /// A slider with its label; words at both ends for the main ones, the
    /// exact value for the others.
    fn knob(&self, knob: Knob, cx: &Context<Self>) -> Option<Div> {
        let (_, state) = self.sliders.iter().find(|(k, _)| *k == knob)?;
        let ends = knob.ends();
        let automatic = knob == Knob::MusicDelay && self.profile.settings.music_delay.is_none();
        let value = (ends.is_none()).then(|| {
            let value = knob.format(slider_value(state, cx));
            if automatic {
                t!("tuning.automatic", value = value).to_string()
            } else {
                value
            }
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap(px(6.0))
                .py(px(4.0))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .text_size(px(12.5))
                        .text_color(rgb(TEXT_SOFT))
                        .child(knob.label())
                        .when_some(value, |row, value| {
                            row.child(
                                div()
                                    .font_family(FONT_MONO)
                                    .text_size(px(11.5))
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(value),
                            )
                        }),
                )
                .child(
                    Slider::new(state)
                        .horizontal()
                        .bg(rgb(ACCENT))
                        .text_color(rgb(TEXT)),
                )
                .when_some(ends, |this, (low, high)| {
                    this.child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(10.5))
                            .text_color(rgb(TEXT_FAINT))
                            .child(low)
                            .child(high),
                    )
                }),
        )
    }

    pub(crate) fn render_settings(&self, cx: &mut Context<Self>) -> Div {
        let settings = &self.profile.settings;
        let mode = settings.mode;
        let main: &[Knob] = match mode {
            Mode::ScreenZones | Mode::Screen => {
                &[Knob::Brightness, Knob::Saturation, Knob::Smoothing]
            }
            Mode::Audio => &[Knob::Brightness, Knob::Reactivity],
            Mode::Fixed => &[Knob::Hue, Knob::Brightness],
        };
        let mut rows = div().flex().flex_col().gap(px(4.0));
        match mode {
            Mode::Audio => rows = rows.child(self.render_music_swatches(cx)),
            Mode::Fixed => rows = rows.child(self.render_color_swatches(cx)),
            _ => {}
        }
        for knob in main {
            rows = rows.children(self.knob(*knob, cx));
        }
        if mode == Mode::Audio && *MUSIC_VIA_MICROPHONE {
            // macOS 12 cannot capture the computer's sound without a driver:
            // say so, and how to get the best of the microphone.
            rows = rows.child(
                div()
                    .text_size(px(11.0))
                    .line_height(px(16.0))
                    .text_color(rgb(TEXT_MUTED))
                    .child(t!("tuning.microphone_note").to_string()),
            );
        }

        let open = self.advanced_open;
        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
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
                    .border_color(rgba(0xFFFFFF0F))
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(rgb(TEXT_MUTED))
                    .hover(|style| style.text_color(rgb(TEXT)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.advanced_open = !this.advanced_open;
                        cx.notify();
                    }))
                    .child(t!("tuning.advanced").to_string())
                    .child(if open { "▾" } else { "›" }),
            )
            .when(open, |this| this.child(self.render_advanced(cx)))
    }

    fn render_advanced(&self, cx: &mut Context<Self>) -> Div {
        let settings = &self.profile.settings;
        let mode = settings.mode;
        let knobs: &[Knob] = match mode {
            Mode::ScreenZones => &[
                Knob::ScreenGlow,
                Knob::Vivid,
                Knob::Contrast,
                Knob::FrameRate,
            ],
            Mode::Screen => &[Knob::ScreenGlow, Knob::Vivid, Knob::Height, Knob::FrameRate],
            Mode::Audio if settings.audio_hue == AudioHue::Cycle => {
                &[Knob::MusicDelay, Knob::MusicGlow, Knob::HueSpeed]
            }
            Mode::Audio => &[Knob::MusicDelay, Knob::MusicGlow],
            Mode::Fixed => &[],
        };
        let mut rows = div().flex().flex_col().gap(px(4.0));
        if matches!(mode, Mode::ScreenZones | Mode::Screen) {
            let dark = settings.dark_scenes;
            let option =
                |id: &'static str, key: &str, value: DarkScenes, cx: &mut Context<Self>| {
                    (
                        id,
                        t!(key).to_string(),
                        dark == value,
                        Box::new(cx.listener(move |this: &mut Self, _: &ClickEvent, _, cx| {
                            this.change_settings(|s| s.dark_scenes = value, cx)
                        })) as ClickHandler,
                    )
                };
            rows = rows.child(choice(
                t!("tuning.dark").to_string(),
                vec![
                    option("dark-follow", "tuning.dark_follow", DarkScenes::Follow, cx),
                    option("dark-hold", "tuning.dark_hold", DarkScenes::Hold, cx),
                    option("dark-dim", "tuning.dark_dim", DarkScenes::Dim, cx),
                ],
            ));
        }
        for knob in knobs {
            rows = rows.children(self.knob(*knob, cx));
        }
        if mode == Mode::Audio && settings.music_delay.is_some() {
            rows = rows.child(
                div().flex().justify_end().child(
                    link(
                        "music-delay-auto",
                        t!("tuning.music_delay_reset").to_string(),
                    )
                    .text_size(px(11.5))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.change_settings(|s| s.music_delay = None, cx)
                    })),
                ),
            );
        }
        if mode == Mode::Audio {
            rows = rows.child(switch_row(
                "wave",
                t!("tuning.wave").to_string(),
                settings.audio_zones,
                cx.listener(|this, _, _, cx| {
                    this.change_settings(|s| s.audio_zones = !s.audio_zones, cx)
                }),
            ));
        }
        rows.child(switch_row(
            "restore-white",
            t!("tuning.restore_white").to_string(),
            settings.restore_white,
            cx.listener(|this, _, _, cx| {
                this.change_settings(|s| s.restore_white = !s.restore_white, cx)
            }),
        ))
    }

    /// Music colors: the rainbow, or one hue.
    fn render_music_swatches(&self, cx: &mut Context<Self>) -> Div {
        let settings = &self.profile.settings;
        let rainbow = settings.audio_hue == AudioHue::Cycle;
        let mut swatches =
            vec![
                swatch("music-rainbow", None, rainbow).on_click(cx.listener(|this, _, _, cx| {
                    this.change_settings(|s| s.audio_hue = AudioHue::Cycle, cx)
                })),
            ];
        for hue in SWATCH_HUES {
            let chosen = !rainbow && settings.audio_fixed_hue == hue;
            swatches.push(
                swatch(
                    SharedString::from(format!("music-{hue}")),
                    Some(Hsv::new(hue as f32 / 360.0, 1.0, 1.0)),
                    chosen,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.change_settings(
                        |s| {
                            s.audio_hue = AudioHue::Fixed;
                            s.audio_fixed_hue = hue;
                        },
                        cx,
                    )
                })),
            );
        }
        swatch_row(t!("tuning.music_color").to_string(), swatches)
    }

    /// Color mode: white, or one of the hues (the Hue slider refines it).
    fn render_color_swatches(&self, cx: &mut Context<Self>) -> Div {
        let settings = &self.profile.settings;
        let white = settings.fixed_saturation < 0.5;
        let mut swatches =
            vec![
                swatch("color-white", Some(Hsv::new(0.0, 0.0, 1.0)), white).on_click(cx.listener(
                    |this, _, _, cx| this.change_settings(|s| s.fixed_saturation = 0.0, cx),
                )),
            ];
        for hue in SWATCH_HUES {
            let chosen = !white && settings.fixed_hue == hue;
            swatches.push(
                swatch(
                    SharedString::from(format!("color-{hue}")),
                    Some(Hsv::new(hue as f32 / 360.0, 1.0, 1.0)),
                    chosen,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.change_settings(
                        |s| {
                            s.fixed_hue = hue;
                            s.fixed_saturation = 1.0;
                        },
                        cx,
                    )
                })),
            );
        }
        swatch_row(t!("tuning.color").to_string(), swatches)
    }
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A round color to pick; `None` draws the rainbow.
fn swatch(id: impl Into<ElementId>, color: Option<Hsv>, chosen: bool) -> Stateful<Div> {
    let dot = match color {
        Some(color) => div().size(px(16.0)).rounded_full().bg(display_color(color)),
        // Three hues side by side, for "all the colors".
        None => div()
            .size(px(16.0))
            .rounded_full()
            .overflow_hidden()
            .flex()
            .children(
                [0xE0533A, 0x34D399, 0x5A7BE8].map(|hue| div().flex_1().h_full().bg(rgb(hue))),
            ),
    };
    div()
        .id(id)
        .size(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(if chosen { rgb(TEXT) } else { rgba(0x00000000) })
        .cursor_pointer()
        .child(dot)
}

fn swatch_row(label: String, swatches: Vec<Stateful<Div>>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .py(px(4.0))
        .child(
            div()
                .text_size(px(12.5))
                .text_color(rgb(TEXT_SOFT))
                .child(label),
        )
        .child(div().flex().flex_wrap().gap(px(2.0)).children(swatches))
}

/// A label and a few mutually exclusive options under it.
fn choice(label: String, options: Vec<(&'static str, String, bool, ClickHandler)>) -> Div {
    let choices = div()
        .flex()
        .gap(px(2.0))
        .p(px(2.0))
        .rounded(px(8.0))
        .bg(rgba(0x151517CC))
        .border_1()
        .border_color(rgba(0xFFFFFF12))
        .children(options.into_iter().map(|(id, text, chosen, on_click)| {
            div()
                .id(id)
                .flex_1()
                .flex()
                .justify_center()
                .py(px(4.0))
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
        .flex_col()
        .gap(px(6.0))
        .py(px(4.0))
        .child(
            div()
                .text_size(px(12.5))
                .text_color(rgb(TEXT_SOFT))
                .child(label),
        )
        .child(choices)
}

fn switch_row(
    id: &'static str,
    label: String,
    on: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(10.0))
        .py(px(6.0))
        .text_size(px(12.5))
        .text_color(rgb(TEXT_SOFT))
        .child(div().flex_1().child(label))
        .child(toggle(id, on).on_click(on_click))
}

pub fn slider_value(state: &Entity<SliderState>, cx: &App) -> f32 {
    match state.read(cx).value() {
        SliderValue::Single(v) => v,
        _ => 0.0,
    }
}
