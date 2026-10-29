//! Main window: the panels' light is the only color in the app.

use std::borrow::Cow;
use std::sync::LazyLock;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_tray::{Icon, Tray};
use lscsync_core::{Hsv, Mode, Profile, Snapshot};
use lscsync_engine::tuya::LscConfig;
use lscsync_engine::worker::{EngineEvent, EngineHandle, ErrorKind};
use lscsync_platform::{legacy_settings_path, profile_path};

mod devices;
mod setup;
mod theme;

use theme::*;

actions!(
    lscsync,
    [
        Quit,
        ShowWindow,
        ShowDevices,
        CloseWindow,
        ToggleSync,
        ModeAmbilight,
        ModeColor,
        ModeMusic,
        Noop
    ]
);

/// Music mode can run on this system.
static AUDIO_SUPPORTED: LazyLock<bool> = LazyLock::new(lscsync_engine::audio::is_supported);
/// Music mode hears the room through the microphone (macOS 12 without a
/// loopback driver). Checked once: it lists the audio devices.
static MUSIC_VIA_MICROPHONE: LazyLock<bool> = LazyLock::new(lscsync_engine::audio::uses_microphone);

struct AppState {
    tray: Option<Tray>,
    window: Option<AnyWindowHandle>,
    /// The view outlives the window: closing the window does not stop the sync.
    root: Entity<Root>,
}

impl Global for AppState {}

/// What the menu bar / notification area menu shows.
struct TrayModel {
    phase: Phase,
    mode: Mode,
    status: String,
}

impl Global for TrayModel {}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Connecting,
    Running,
    Failed,
}

/// What the window shows when no setup is in progress.
#[derive(Clone, Copy, PartialEq)]
enum Page {
    Sync,
    Devices,
}

struct Root {
    profile: Profile,
    page: Page,
    /// First-run or reconnection flow, shown instead of the page.
    setup: Option<Entity<setup::SetupFlow>>,
    /// Feedback on the devices page (scan result, export, import).
    notice: Option<String>,
    scanning: bool,
    /// The profile changed under the sliders (import): resync on next render.
    sliders_stale: bool,
    /// Whether the window is currently sized for the setup flow.
    sized_for_setup: Option<bool>,
    engine: Option<EngineHandle>,
    /// Stopped engine whose thread finishes in the background (back to white).
    retiring: Option<EngineHandle>,
    events: Option<Receiver<EngineEvent>>,
    snapshot: Option<Snapshot>,
    /// Last engine error, kept until the next start.
    error: Option<(ErrorKind, String)>,
    // Sliders
    lum_max: Entity<SliderState>,
    boost_sat: Entity<SliderState>,
    smoothing: Entity<SliderState>,
    capture_height: Entity<SliderState>,
    reactivity: Entity<SliderState>,
    audio_min_light: Entity<SliderState>,
    /// Last state shown in the menu bar, to refresh it only on change.
    tray_shown: Option<(Phase, Mode)>,
    _subscriptions: Vec<Subscription>,
}

impl Root {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut profile = load_profile();
        if !*AUDIO_SUPPORTED && profile.settings.mode == Mode::Audio {
            profile.settings.mode = Mode::ScreenZones;
        }
        let settings = &profile.settings;

        let slider = |cx: &mut Context<Self>, min: f32, max: f32, step: f32, value: f32| {
            cx.new(|_| {
                SliderState::new()
                    .min(min)
                    .max(max)
                    .step(step)
                    .default_value(value)
            })
        };
        let lum_max = slider(cx, 10.0, 100.0, 5.0, settings.lum_max as f32);
        let boost_sat = slider(cx, 10.0, 25.0, 1.0, settings.boost_sat * 10.0);
        let smoothing = slider(cx, 0.0, 90.0, 5.0, settings.smoothing * 100.0);
        let capture_height = slider(cx, 10.0, 100.0, 5.0, settings.capture_height * 100.0);
        let reactivity = slider(cx, 10.0, 90.0, 5.0, settings.reactivity * 100.0);
        let audio_min_light = slider(cx, 0.0, 20.0, 1.0, settings.audio_min_light * 100.0);

        let mut subscriptions: Vec<Subscription> = [
            &lum_max,
            &boost_sat,
            &smoothing,
            &capture_height,
            &reactivity,
            &audio_min_light,
        ]
        .into_iter()
        .map(|state| {
            cx.subscribe(state, |this: &mut Self, _, event: &SliderEvent, cx| {
                this.on_slider(event, cx)
            })
        })
        .collect();
        subscriptions.push(cx.on_app_quit(|this, _| {
            this.shutdown();
            async {}
        }));

        Self::spawn_poll(cx);

        let needs_setup = profile.sync_target().is_none();
        let mut root = Self {
            profile,
            page: Page::Sync,
            setup: None,
            notice: None,
            scanning: false,
            sliders_stale: false,
            sized_for_setup: None,
            engine: None,
            retiring: None,
            events: None,
            snapshot: None,
            error: None,
            lum_max,
            boost_sat,
            smoothing,
            capture_height,
            reactivity,
            audio_min_light,
            tray_shown: None,
            _subscriptions: subscriptions,
        };
        if needs_setup {
            root.open_setup(false, None, cx);
        }
        root
    }

    /// Moves the sliders to the profile's settings (after an import).
    fn sync_sliders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = &self.profile.settings;
        let values = [
            (&self.lum_max, settings.lum_max as f32),
            (&self.boost_sat, settings.boost_sat * 10.0),
            (&self.smoothing, settings.smoothing * 100.0),
            (&self.capture_height, settings.capture_height * 100.0),
            (&self.reactivity, settings.reactivity * 100.0),
            (&self.audio_min_light, settings.audio_min_light * 100.0),
        ];
        for (slider, value) in values {
            slider.update(cx, |slider, cx| slider.set_value(value, window, cx));
        }
        self.sliders_stale = false;
    }

    /// The profile was replaced: refresh the sliders on the next render.
    fn load_sliders(&mut self, cx: &mut Context<Self>) {
        self.sliders_stale = true;
        cx.notify();
    }

    fn spawn_poll(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
                if this
                    .update(cx, |root, cx| {
                        if root.drain_events() {
                            cx.notify();
                        }
                        root.sync_tray(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn drain_events(&mut self) -> bool {
        let Some(receiver) = &self.events else {
            return false;
        };
        let mut changed = false;
        while let Ok(event) = receiver.try_recv() {
            changed = true;
            match event {
                EngineEvent::Snapshot(snapshot) => self.snapshot = Some(snapshot),
                EngineEvent::Error { kind, message } => {
                    tracing::error!("engine stopped: {message}");
                    self.error = Some((kind, message));
                }
                EngineEvent::Stopped => {
                    // The engine thread is done: release the handle, otherwise
                    // `start()` thinks an engine is still running and does nothing.
                    self.engine = None;
                    self.events = None;
                    self.snapshot = None;
                    break;
                }
            }
        }
        changed
    }

    /// Updates the menu bar icon and menu when the state changes.
    fn sync_tray(&mut self, cx: &mut Context<Self>) {
        let shown = (self.phase(), self.profile.settings.mode);
        if self.tray_shown == Some(shown) {
            return;
        }
        let Some(tray) = cx.try_global::<AppState>().and_then(|s| s.tray.clone()) else {
            return;
        };
        self.tray_shown = Some(shown);
        let (phase, mode) = shown;
        let status = match phase {
            Phase::Idle => "En pause".to_owned(),
            Phase::Connecting => "Connexion aux panneaux…".to_owned(),
            Phase::Running => format!("Synchronisé · {}", mode_label(mode)),
            Phase::Failed => self.describe_error().0,
        };
        let _ = tray.set_tooltip(Some(format!("LscSync — {status}")), cx);
        cx.set_global(TrayModel {
            phase,
            mode,
            status,
        });
        if let Err(error) = tray_icon(phase).and_then(|icon| tray.set_icon(icon, cx)) {
            tracing::warn!("menu bar icon: {error}");
        }
        if let Err(error) = tray.refresh_menu(cx) {
            tracing::warn!("menu bar menu: {error}");
        }
    }

    fn describe_error(&self) -> (String, String) {
        let (kind, message) = self
            .error
            .clone()
            .unwrap_or((ErrorKind::Other, String::new()));
        describe_error(kind, &message, &self.device_ip())
    }

    /// Last known LAN address of the synced device.
    fn device_ip(&self) -> String {
        self.profile
            .sync_target()
            .map(|device| device.ip.clone())
            .unwrap_or_default()
    }

    fn phase(&self) -> Phase {
        match (&self.engine, &self.snapshot, &self.error) {
            (Some(_), Some(_), _) => Phase::Running,
            (Some(_), None, _) => Phase::Connecting,
            (None, _, Some(_)) => Phase::Failed,
            (None, _, None) => Phase::Idle,
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.engine.is_some() {
            return;
        }
        self.read_sliders(cx);
        self.save();
        self.error = None;
        let Some(device) = self.profile.sync_target().map(LscConfig::from) else {
            self.error = Some((ErrorKind::NotConfigured, "no device configured".to_owned()));
            cx.notify();
            return;
        };
        let (handle, receiver) =
            EngineHandle::start_after(self.retiring.take(), device, self.profile.settings.clone());
        self.engine = Some(handle);
        self.events = Some(receiver);
        cx.notify();
    }

    /// Stops without waiting for the thread: the UI never freezes, even while connecting.
    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.engine.take() {
            handle.request_stop();
            self.retiring = Some(handle);
        }
        self.events = None;
        self.snapshot = None;
        self.error = None;
        cx.notify();
    }

    fn primary_action(&mut self, cx: &mut Context<Self>) {
        match self.phase() {
            Phase::Idle | Phase::Failed => self.start(cx),
            Phase::Connecting | Phase::Running => self.stop(cx),
        }
    }

    /// On quit: give the engine time to set the panels back to white.
    fn shutdown(&mut self) {
        let handles: Vec<EngineHandle> = self
            .engine
            .take()
            .into_iter()
            .chain(self.retiring.take())
            .collect();
        if handles.is_empty() {
            return;
        }
        for handle in &handles {
            handle.request_stop();
        }
        let (done, wait) = mpsc::channel();
        std::thread::spawn(move || {
            drop(handles);
            let _ = done.send(());
        });
        let _ = wait.recv_timeout(Duration::from_millis(1500));
    }

    fn on_slider(&mut self, event: &SliderEvent, cx: &mut Context<Self>) {
        self.read_sliders(cx);
        if let Some(engine) = &self.engine {
            engine.update(self.profile.settings.clone());
        }
        if matches!(event, SliderEvent::Release(_)) {
            self.save();
        }
        cx.notify();
    }

    /// Reads the sliders into the settings.
    fn read_sliders(&mut self, cx: &App) {
        self.profile.settings.lum_max = slider_value(&self.lum_max, cx) as u8;
        self.profile.settings.boost_sat = slider_value(&self.boost_sat, cx) / 10.0;
        self.profile.settings.smoothing = slider_value(&self.smoothing, cx) / 100.0;
        self.profile.settings.capture_height = slider_value(&self.capture_height, cx) / 100.0;
        self.profile.settings.reactivity = slider_value(&self.reactivity, cx) / 100.0;
        self.profile.settings.audio_min_light = slider_value(&self.audio_min_light, cx) / 100.0;
    }

    fn save(&self) {
        if let Err(error) = self.profile.save(&profile_path()) {
            tracing::warn!("profile not saved: {error}");
        }
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.profile.settings.mode == mode {
            return;
        }
        self.profile.settings.mode = mode;
        self.save();
        // The mode changes the source (screen or sound): restart the engine.
        if self.engine.is_some() {
            self.stop(cx);
            self.start(cx);
        } else {
            cx.notify();
        }
    }

    fn render_header(&self, phase: Phase) -> Div {
        let (dot, label) = match phase {
            Phase::Idle => (TEXT_FAINT, "En pause"),
            Phase::Connecting => (ACCENT, "Connexion…"),
            Phase::Running => (SUCCESS, "Synchronisé"),
            Phase::Failed => (DANGER_TEXT, "Erreur"),
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
                    .child(label),
            )
    }

    fn render_modes(&self, cx: &mut Context<Self>) -> Div {
        let current = self.profile.settings.mode;
        div()
            .flex()
            .gap(px(2.0))
            .p(px(3.0))
            .rounded(px(11.0))
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(LINE))
            .child(mode_pill("Ambilight", Mode::ScreenZones, current, true, cx))
            .child(mode_pill("Couleur", Mode::Screen, current, true, cx))
            .child(mode_pill(
                "Musique",
                Mode::Audio,
                current,
                *AUDIO_SUPPORTED,
                cx,
            ))
    }

    fn render_hero(&self, phase: Phase) -> Div {
        let hero = div()
            .relative()
            .h(px(160.0))
            .rounded(px(16.0))
            .overflow_hidden();

        match (phase, self.snapshot) {
            (Phase::Running, Some(snapshot)) => {
                let colors: Vec<Hsv> = match self.profile.settings.mode {
                    Mode::Screen => vec![snapshot.top],
                    Mode::ScreenZones | Mode::Audio => snapshot.zones.to_vec(),
                };
                let tag = match self.profile.settings.mode {
                    Mode::ScreenZones => "3 zones · écran",
                    Mode::Screen => "1 couleur · écran",
                    Mode::Audio if *MUSIC_VIA_MICROPHONE => "Basses · micro",
                    Mode::Audio => "Basses · 3 zones",
                };
                hero.child(
                    color_bands(colors.into_iter().map(display_color), px(16.0))
                        .absolute()
                        .inset_0(),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(12.0))
                        .bottom(px(12.0))
                        .px(px(9.0))
                        .py(px(4.0))
                        .rounded_full()
                        .bg(rgba(0x00000066))
                        .text_size(px(11.0))
                        .text_color(rgb(0xFFFFFF))
                        .child(tag),
                )
            }
            _ => {
                let (bg, line, title_color, title, body) = match phase {
                    Phase::Failed => {
                        let (title, body) = self.describe_error();
                        (DANGER_BG, DANGER_LINE, DANGER_TEXT, title, body)
                    }
                    Phase::Connecting => (
                        SURFACE,
                        LINE,
                        TEXT,
                        "Connexion aux panneaux…".to_owned(),
                        self.device_ip(),
                    ),
                    _ => (
                        SURFACE,
                        LINE,
                        TEXT,
                        "Panneaux en pause".to_owned(),
                        "Lance la synchro pour les piloter.".to_owned(),
                    ),
                };
                hero.flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(4.0))
                    .px(px(24.0))
                    .bg(rgb(bg))
                    .border_1()
                    .border_color(rgb(line))
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(title_color))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_center()
                            .text_color(rgb(TEXT_MUTED))
                            .child(body),
                    )
            }
        }
    }

    fn render_primary(&self, phase: Phase, cx: &mut Context<Self>) -> Stateful<Div> {
        let (label, filled) = match phase {
            Phase::Idle => ("Démarrer la synchro", true),
            Phase::Failed => ("Réessayer", true),
            Phase::Connecting => ("Annuler", false),
            Phase::Running => ("Arrêter", false),
        };
        div()
            .id("primary")
            .h(px(44.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.0))
            .rounded(px(12.0))
            .cursor_pointer()
            .text_size(px(13.5))
            .on_click(cx.listener(|this, _, _, cx| this.primary_action(cx)))
            .map(|button| {
                if filled {
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
            .when(phase == Phase::Running, |button| {
                button.child(div().size(px(8.0)).rounded(px(2.0)).bg(rgb(TEXT)))
            })
            .child(label)
    }

    fn render_settings(&self, cx: &Context<Self>) -> Div {
        let percent = |v: f32| format!("{}%", v.round() as u32);
        let rows = div().flex().flex_col();
        let rows = rows.child(setting("Luminosité", &self.lum_max, cx, percent));
        let rows = if self.profile.settings.mode == Mode::Audio {
            rows.child(setting("Réactivité", &self.reactivity, cx, percent))
                .child(setting("Lueur min.", &self.audio_min_light, cx, percent))
                .when(*MUSIC_VIA_MICROPHONE, |rows| {
                    // macOS 12 cannot capture the computer's sound without a
                    // driver: say so, and how to get the best of the microphone.
                    rows.child(
                        div()
                            .mt(px(6.0))
                            .text_size(px(11.5))
                            .line_height(px(17.0))
                            .text_color(rgb(TEXT_MUTED))
                            .child(
                                "Sur macOS 12, le mode Musique écoute avec le micro du Mac : \
                                 le son de la pièce compte aussi. Monte le volume des enceintes \
                                 pour de meilleures basses (macOS 13 capte le son directement).",
                            ),
                    )
                })
        } else {
            rows.child(setting("Saturation", &self.boost_sat, cx, |v| {
                format!("×{:.1}", v / 10.0)
            }))
            .child(setting("Fluidité", &self.smoothing, cx, percent))
            .child(setting("Hauteur", &self.capture_height, cx, percent))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(TEXT_FAINT))
                    .child("RÉGLAGES"),
            )
            .child(rows)
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .pt(px(12.0))
            .border_t_1()
            .border_color(rgb(0x1C1C1F))
            .text_size(px(11.5))
            .text_color(rgb(TEXT_FAINT))
            .child(
                link(
                    "devices",
                    match self.profile.sync_target() {
                        Some(device) => format!("Appareils · {}", device.name),
                        None => "Appareils".to_owned(),
                    },
                )
                .text_size(px(11.5))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.page = Page::Devices;
                    cx.notify();
                })),
            )
            .child(
                div()
                    .id("quit")
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgb(TEXT_MUTED)))
                    .on_click(|_, _, cx| cx.quit())
                    .child("Quitter"),
            )
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.sliders_stale {
            self.sync_sliders(window, cx);
        }
        // The setup flow needs less room: the window shrinks while it shows.
        let setup = self.setup.is_some();
        if self.sized_for_setup != Some(setup) {
            if self.sized_for_setup.is_some() {
                window.resize(window_size(setup));
            }
            self.sized_for_setup = Some(setup);
        }
        let shell = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .font_family(FONT)
            .text_color(rgb(TEXT))
            .pt(px(TITLEBAR_INSET + 18.0))
            .px(px(22.0))
            .pb(px(16.0));
        if let Some(setup) = &self.setup {
            return shell.child(setup.clone());
        }
        if self.page == Page::Devices {
            return shell.child(self.render_devices(cx));
        }
        let phase = self.phase();
        shell
            .child(self.render_header(phase))
            .child(div().h(px(18.0)))
            .child(self.render_modes(cx))
            .child(div().h(px(14.0)))
            .child(self.render_hero(phase))
            .child(div().h(px(12.0)))
            .child(self.render_primary(phase, cx))
            .child(div().h(px(22.0)))
            .child(self.render_settings(cx))
            .child(div().flex_1())
            .child(self.render_footer(cx))
    }
}

/// Loads the profile, migrating the old `settings.json` on first run.
fn load_profile() -> Profile {
    if let Err(error) = lscsync_platform::migrate_legacy_data() {
        tracing::warn!("former Lumière Sync profile not copied: {error}");
    }
    let profile = match Profile::load_or_migrate(&profile_path(), &legacy_settings_path()) {
        Ok(profile) => profile,
        Err(error) => {
            tracing::error!("profile not loaded: {error}");
            return Profile::default();
        }
    };
    if !profile_path().exists()
        && !profile.devices.is_empty()
        && let Err(error) = profile.save(&profile_path())
    {
        tracing::warn!("migrated profile not saved: {error}");
    }
    profile
}

fn mode_pill(
    label: &'static str,
    target: Mode,
    current: Mode,
    available: bool,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    let pill = div()
        .id(label)
        .flex_1()
        .flex()
        .justify_center()
        .py(px(7.0))
        .rounded(px(8.0))
        .text_size(px(12.5))
        .child(label);

    if !available {
        return pill.text_color(rgb(TEXT_FAINT));
    }
    if target == current {
        return pill
            .bg(rgb(SURFACE_RAISED))
            .border_1()
            .border_color(rgb(LINE_STRONG))
            .text_color(rgb(TEXT))
            .font_weight(FontWeight::MEDIUM);
    }
    pill.cursor_pointer()
        .text_color(rgb(TEXT_MUTED))
        .hover(|style| style.text_color(rgb(TEXT)).bg(rgb(SURFACE_HOVER)))
        .on_click(cx.listener(move |this, _, _, cx| this.set_mode(target, cx)))
}

fn setting(
    label: &str,
    state: &Entity<SliderState>,
    cx: &Context<Root>,
    format: impl Fn(f32) -> String,
) -> Div {
    let value = slider_value(state, cx);
    div()
        .flex()
        .items_center()
        .gap(px(12.0))
        .h(px(38.0))
        .child(
            div()
                .w(px(80.0))
                .text_size(px(13.0))
                .text_color(rgb(TEXT_MUTED))
                .child(label.to_owned()),
        )
        .child(
            Slider::new(state)
                .horizontal()
                .flex_1()
                .bg(rgb(ACCENT))
                .text_color(rgb(TEXT)),
        )
        .child(
            div()
                .w(px(40.0))
                .flex()
                .justify_end()
                .font_family(FONT_MONO)
                .text_size(px(12.0))
                .text_color(rgb(TEXT_SOFT))
                .child(format(value)),
        )
}

fn slider_value(state: &Entity<SliderState>, cx: &App) -> f32 {
    match state.read(cx).value() {
        SliderValue::Single(v) => v,
        _ => 0.0,
    }
}

/// Preview color: same hue as the panels, lightened so it stays readable on
/// screen even when the brightness sent is low.
fn display_color(color: Hsv) -> Rgba {
    let c = Hsv::new(color.h, color.s, 0.3 + 0.7 * color.v.clamp(0.0, 1.0)).to_rgb();
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    rgb((channel(c.r) << 16) | (channel(c.g) << 8) | channel(c.b))
}

/// Turns an engine error into a title and an actionable explanation.
fn describe_error(kind: ErrorKind, message: &str, ip: &str) -> (String, String) {
    const PERMISSION: &str = "Autorise LscSync dans Réglages Système › Confidentialité et sécurité › Enregistrement de l'écran et de l'audio système.";
    let (title, body) = match kind {
        ErrorKind::NotConfigured => (
            "Aucun appareil configuré",
            "Connecte tes appareils LSC pour démarrer.".to_owned(),
        ),
        ErrorKind::Unreachable => (
            "Panneaux injoignables",
            format!(
                "Pas de réponse de {ip} en 10 s. Vérifie qu'ils sont allumés et sur le même réseau."
            ),
        ),
        ErrorKind::ScreenCapture => (
            "Capture d'écran impossible",
            if cfg!(target_os = "macos") {
                PERMISSION.to_owned()
            } else {
                message.to_owned()
            },
        ),
        ErrorKind::Audio if !*AUDIO_SUPPORTED => (
            "Son indisponible",
            "Le mode Musique n'est pas encore disponible sur ce système.".to_owned(),
        ),
        ErrorKind::Audio if *MUSIC_VIA_MICROPHONE => (
            "Micro inaccessible",
            "Autorise LscSync dans Réglages Système › Confidentialité et sécurité › Micro."
                .to_owned(),
        ),
        ErrorKind::Audio => ("Son indisponible", PERMISSION.to_owned()),
        ErrorKind::Device => (
            "Connexion refusée par les panneaux",
            "Leur clé a peut-être changé : reconnecte tes appareils.".to_owned(),
        ),
        ErrorKind::Other => ("Une erreur est survenue", message.to_owned()),
    };
    (title.to_owned(), body)
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::ScreenZones => "Ambilight",
        Mode::Screen => "Couleur",
        Mode::Audio => "Musique",
    }
}

/// No title next to the icon, to keep the macOS menu bar clean. On Windows,
/// left click opens the window and right click the menu.
fn build_tray(cx: &mut App) -> gpui_tray::Result<Tray> {
    Tray::builder()
        .icon(tray_icon(Phase::Idle)?)
        .tooltip("LscSync — En pause")
        .on_activate(ShowWindow)
        .menu(tray_menu)
        .build(cx)
}

fn tray_menu(cx: &mut App) -> Vec<MenuItem> {
    let (phase, mode, status) = match cx.try_global::<TrayModel>() {
        Some(model) => (model.phase, model.mode, model.status.clone()),
        None => (Phase::Idle, Mode::ScreenZones, "En pause".to_owned()),
    };
    let toggle = match phase {
        Phase::Idle => "Démarrer la synchro",
        Phase::Failed => "Réessayer",
        Phase::Connecting => "Annuler la connexion",
        Phase::Running => "Arrêter la synchro",
    };
    vec![
        MenuItem::action(status, Noop).disabled(true),
        MenuItem::separator(),
        MenuItem::action(toggle, ToggleSync),
        MenuItem::separator(),
        MenuItem::action("Ambilight", ModeAmbilight).checked(mode == Mode::ScreenZones),
        MenuItem::action("Couleur", ModeColor).checked(mode == Mode::Screen),
        MenuItem::action(
            if *AUDIO_SUPPORTED {
                "Musique"
            } else {
                "Musique (indisponible sur ce système)"
            },
            ModeMusic,
        )
        .checked(mode == Mode::Audio)
        .disabled(!*AUDIO_SUPPORTED),
        MenuItem::separator(),
        MenuItem::action("Ouvrir LscSync", ShowWindow),
        MenuItem::action("Appareils…", ShowDevices),
        MenuItem::separator(),
        MenuItem::action("Quitter LscSync", Quit),
    ]
}

/// Hand-drawn glyph: a screen with three rays of light above it (the
/// panels). The rays fade while the sync is paused.
///
/// macOS: monochrome template image, tinted by the system to match the menu
/// bar (light, dark, selected). Windows: amber glyph, readable on both light
/// and dark taskbars.
fn tray_icon(phase: Phase) -> gpui_tray::Result<Icon> {
    // 18 pt logical size, rendered @2x on macOS.
    let size: u32 = if cfg!(target_os = "macos") { 36 } else { 32 };
    let k = size as f32 / 18.0;
    let rays_alpha = if phase == Phase::Running { 1.0 } else { 0.4 };
    let [r, g, b] = if cfg!(target_os = "macos") {
        [0, 0, 0]
    } else {
        [0xE8, 0xA8, 0x38]
    };

    let mut rgba = vec![0_u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            // Pixel center, in points.
            let (px, py) = ((x as f32 + 0.5) / k, (y as f32 + 0.5) / k);
            let screen = (rounded_box(px - 9.0, py - 10.6, 7.0, 4.1, 1.5).abs() - 0.7) * k;
            let stand = segment(px, py, [6.6, 16.9, 11.4, 16.9], 0.75) * k;
            // Vertical center ray, outer rays fanned out.
            let rays = [
                [9.0, 1.3, 9.0, 3.9],
                [3.3, 2.3, 4.6, 4.5],
                [14.7, 2.3, 13.4, 4.5],
            ]
            .into_iter()
            .map(|ray| segment(px, py, ray, 0.8) * k)
            .fold(f32::MAX, f32::min);
            let coverage = |d: f32| (0.5 - d).clamp(0.0, 1.0);
            let alpha = coverage(screen)
                .max(coverage(stand))
                .max(coverage(rays) * rays_alpha);
            let i = ((y * size + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[r, g, b, (alpha * 255.0).round() as u8]);
        }
    }

    let icon = Icon::from_rgba(rgba, size, size);
    #[cfg(target_os = "macos")]
    let icon = icon.scale_factor(2.0).template(true);
    icon.build()
}

/// Signed distance to a centered rounded rectangle (half sizes `hw`, `hh`).
fn rounded_box(x: f32, y: f32, hw: f32, hh: f32, radius: f32) -> f32 {
    let qx = x.abs() - (hw - radius);
    let qy = y.abs() - (hh - radius);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

/// Signed distance to a rounded stroke from (ax, ay) to (bx, by).
fn segment(x: f32, y: f32, [ax, ay, bx, by]: [f32; 4], radius: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (x - ax - t * dx).hypot(y - ay - t * dy) - radius
}

fn with_root(cx: &mut App, update: impl FnOnce(&mut Root, &mut Context<Root>)) {
    let root = cx.global::<AppState>().root.clone();
    root.update(cx, update);
}

fn window_size(setup: bool) -> Size<Pixels> {
    let height = if setup { SETUP_HEIGHT } else { MAIN_HEIGHT };
    size(px(WINDOW_WIDTH), px(height + TITLEBAR_INSET))
}

fn window_options(setup: bool) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(260.0), px(120.0)),
            window_size(setup),
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("LscSync".into()),
            appears_transparent: cfg!(target_os = "macos"),
            traffic_light_position: Some(point(px(14.0), px(14.0))),
        }),
        window_background: WindowBackgroundAppearance::Opaque,
        is_resizable: false,
        ..Default::default()
    }
}

/// Brings the window to the front, or recreates it if it was closed. On macOS
/// the Dock icon only shows while the window is open.
fn show_window(cx: &mut App) {
    cx.set_activation_policy(ActivationPolicy::Regular);
    set_dock_icon();
    cx.activate(true);
    if let Some(window) = cx.global::<AppState>().window
        && window
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let root = cx.global::<AppState>().root.clone();
    let setup = root.read(cx).setup.is_some();
    // A reopened window is sized for what it shows from the start.
    root.update(cx, |root, _| root.sized_for_setup = None);
    match gpui_kit::open_window(window_options(setup), cx, |_, _| root) {
        Ok((window, _)) => cx.global_mut::<AppState>().window = Some(window),
        Err(error) => tracing::error!("opening the window: {error}"),
    }
}

/// Window closed: the app keeps running in the menu bar, without a Dock icon.
fn on_window_closed(cx: &mut App) {
    cx.global_mut::<AppState>().window = None;
    cx.set_activation_policy(ActivationPolicy::Accessory);
}

/// Dock icon, set at runtime so it also shows in development, outside the
/// `.app` bundle.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::{AnyThread as _, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(include_bytes!("../../../assets/icon/lscsync-1024.png"));
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }
}

#[cfg(not(target_os = "macos"))]
fn set_dock_icon() {}

fn app_menus() -> Vec<Menu> {
    vec![Menu::new("LscSync").items([
        MenuItem::action("Fermer la fenêtre", CloseWindow),
        MenuItem::separator(),
        MenuItem::action("Quitter LscSync", Quit),
    ])]
}

/// Geist is embedded in the binary: identical rendering on macOS and Windows.
fn load_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/Geist-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/Geist-Medium.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/Geist-SemiBold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/GeistMono-Medium.ttf")),
    ];
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        tracing::warn!("Geist fonts not loaded: {error}");
    }
}

fn apply_theme(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        theme.font_family = FONT.into();
        theme.mono_font_family = FONT_MONO.into();
        theme.colors.background = rgb(BG).into();
        theme.colors.foreground = rgb(TEXT).into();
        theme.colors.slider_bar = rgb(ACCENT).into();
        theme.colors.slider_thumb = rgb(TEXT).into();
        theme.colors.ring = rgb(ACCENT).into();
    });
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "lscsync_app=info".into()),
        )
        .init();

    gpui_kit::application()
        .with_quit_mode(QuitMode::Explicit)
        .run(|cx| {
            gpui_kit::init(cx);
            load_fonts(cx);
            apply_theme(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_action(|_: &ShowWindow, cx| show_window(cx));
            cx.on_action(|_: &ShowDevices, cx| {
                with_root(cx, |root, cx| {
                    if root.setup.is_none() {
                        root.page = Page::Devices;
                        cx.notify();
                    }
                });
                show_window(cx);
            });
            cx.on_action(|_: &CloseWindow, cx| {
                if let Some(window) = cx.global::<AppState>().window {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                }
            });
            cx.on_window_closed(|cx, _| on_window_closed(cx)).detach();
            let (quit, close) = if cfg!(target_os = "macos") {
                ("cmd-q", "cmd-w")
            } else {
                ("ctrl-q", "ctrl-w")
            };
            cx.bind_keys([
                KeyBinding::new(quit, Quit, None),
                KeyBinding::new(close, CloseWindow, None),
            ]);
            cx.set_menus(app_menus());
            cx.on_action(|_: &Noop, _| {});
            cx.on_action(|_: &ToggleSync, cx| with_root(cx, |root, cx| root.primary_action(cx)));
            cx.on_action(|_: &ModeAmbilight, cx| {
                with_root(cx, |root, cx| root.set_mode(Mode::ScreenZones, cx))
            });
            cx.on_action(|_: &ModeColor, cx| {
                with_root(cx, |root, cx| root.set_mode(Mode::Screen, cx))
            });
            cx.on_action(|_: &ModeMusic, cx| {
                if *AUDIO_SUPPORTED {
                    with_root(cx, |root, cx| root.set_mode(Mode::Audio, cx));
                }
            });

            let root = cx.new(Root::new);
            cx.set_global(AppState {
                tray: None,
                window: None,
                root,
            });
            show_window(cx);
            match build_tray(cx) {
                Ok(tray) => cx.global_mut::<AppState>().tray = Some(tray),
                Err(error) => tracing::warn!("menu bar unavailable: {error}"),
            }
        });
}
