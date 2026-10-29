//! Main window: the panels' light is the only color in the app.

// Windows: a GUI app, without a console window. Debug builds keep the console
// for the logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::borrow::Cow;
use std::sync::LazyLock;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use gpui_tray::{Icon, Tray};
use lscsync_core::{Hsv, Mode, Preferences, Profile, Snapshot};
use lscsync_engine::worker::{EngineEvent, EngineHandle, ErrorKind, Target};
use lscsync_platform::{legacy_settings_path, preferences_path, profile_path};
use rust_i18n::t;

mod devices;
mod preferences;
mod setup;
mod theme;
mod tuning;
mod updates;
mod zones;

use theme::*;
use updates::UpdateState;

// Interface texts, from crates/app/locales (compiled in).
rust_i18n::i18n!("locales", fallback = "en");

actions!(
    lscsync,
    [
        Quit,
        ShowWindow,
        ShowDevices,
        ShowSettings,
        OpenUpdate,
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
    /// Version of an available update.
    update: Option<String>,
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
#[derive(Clone, PartialEq)]
enum Page {
    Sync,
    Devices,
    /// A device's page, by id: its zones on the screen.
    Device(String),
    Settings,
}

/// What the menu bar shows; it is rebuilt when this changes.
#[derive(Clone, PartialEq)]
struct TrayKey {
    phase: Phase,
    mode: Mode,
    update: Option<String>,
    locale: String,
}

struct Root {
    profile: Profile,
    /// This computer's language and update preferences (not exported).
    preferences: Preferences,
    update: UpdateState,
    page: Page,
    /// First-run or reconnection flow, shown instead of the page.
    setup: Option<Entity<setup::SetupFlow>>,
    /// Feedback on the devices page (scan result, export, import).
    notice: Option<String>,
    scanning: bool,
    /// The profile changed under the sliders (import): resync on next render.
    sliders_stale: bool,
    /// Content height the window is currently sized for.
    sized_for: Option<f32>,
    /// Screen capture behind the zones, on the device pages.
    preview: Option<zones::ScreenPreview>,
    /// Zone being moved or resized in the editor.
    drag: Option<zones::Drag>,
    /// Zone chosen in the editor, lit in white while the sync runs.
    selected_zone: Option<usize>,
    /// Synced devices that do not answer.
    unreachable: Vec<String>,
    /// Calibration in progress on a device's page.
    calibration: Option<zones::Calibration>,
    engine: Option<EngineHandle>,
    /// Stopped engine whose thread finishes in the background (back to white).
    retiring: Option<EngineHandle>,
    events: Option<Receiver<EngineEvent>>,
    snapshot: Option<Snapshot>,
    /// Last engine error, kept until the next start.
    error: Option<(ErrorKind, String)>,
    /// One slider per adjustment, shown or not depending on the mode.
    sliders: Vec<(tuning::Knob, Entity<SliderState>)>,
    /// The Advanced adjustments are unfolded.
    advanced_open: bool,
    /// Last state shown in the menu bar, to refresh it only on change.
    tray_shown: Option<TrayKey>,
    _subscriptions: Vec<Subscription>,
}

impl Root {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut profile = load_profile();
        if !*AUDIO_SUPPORTED && profile.settings.mode == Mode::Audio {
            profile.settings.mode = Mode::ScreenZones;
        }
        let settings = &profile.settings;

        let sliders = tuning::sliders(settings, cx);
        let mut subscriptions: Vec<Subscription> = sliders
            .iter()
            .map(|(_, state)| {
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
        Self::spawn_update_checks(cx);

        let preferences = Preferences::load(&preferences_path());
        apply_language(&preferences);
        let needs_setup = profile.sync_target().is_none();
        let mut root = Self {
            profile,
            preferences,
            update: UpdateState::NotChecked,
            page: Page::Sync,
            setup: None,
            notice: None,
            scanning: false,
            sliders_stale: false,
            sized_for: None,
            preview: None,
            drag: None,
            selected_zone: None,
            unreachable: Vec::new(),
            calibration: None,
            engine: None,
            retiring: None,
            events: None,
            snapshot: None,
            error: None,
            sliders,
            advanced_open: false,
            tray_shown: None,
            _subscriptions: subscriptions,
        };
        if needs_setup {
            root.open_setup(false, None, cx);
        }
        root
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
                EngineEvent::DeviceUnreachable(name) => {
                    if !self.unreachable.contains(&name) {
                        self.unreachable.push(name);
                    }
                }
                EngineEvent::DeviceBack(name) => self.unreachable.retain(|n| *n != name),
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
        let shown = TrayKey {
            phase: self.phase(),
            mode: self.profile.settings.mode,
            update: self
                .update
                .available()
                .map(|release| release.version.clone()),
            locale: rust_i18n::locale().to_string(),
        };
        if self.tray_shown.as_ref() == Some(&shown) {
            return;
        }
        let Some(tray) = cx.try_global::<AppState>().and_then(|s| s.tray.clone()) else {
            return;
        };
        self.tray_shown = Some(shown.clone());
        let TrayKey {
            phase,
            mode,
            update,
            ..
        } = shown;
        let status = match phase {
            Phase::Idle => t!("status.paused").to_string(),
            Phase::Connecting => t!("status.connecting_panels").to_string(),
            Phase::Running => t!("status.synced_mode", mode = mode_label(mode)).to_string(),
            Phase::Failed => self.describe_error().0,
        };
        let _ = tray.set_tooltip(Some(t!("status.tooltip", status = status).to_string()), cx);
        cx.set_global(TrayModel {
            phase,
            mode,
            status,
            update,
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

    /// Content height for what the window shows.
    fn wanted_height(&self) -> f32 {
        match (&self.setup, &self.page) {
            (Some(_), _) => SETUP_HEIGHT,
            (None, Page::Devices | Page::Device(_)) => DEVICES_HEIGHT,
            (None, _) => MAIN_HEIGHT,
        }
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
        let height = self.profile.settings.capture_height;
        let targets: Vec<Target> = self
            .profile
            .sync_targets(self.profile.settings.mode)
            .map(|device| Target::new(device, height))
            .collect();
        if targets.is_empty() {
            self.error = Some((ErrorKind::NotConfigured, String::new()));
            cx.notify();
            return;
        }
        self.unreachable.clear();
        let (handle, receiver) =
            EngineHandle::start_after(self.retiring.take(), targets, self.profile.settings.clone());
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
            Phase::Idle => (TEXT_FAINT, t!("status.paused")),
            Phase::Connecting => (ACCENT, t!("status.connecting")),
            Phase::Running => (SUCCESS, t!("status.synced")),
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
            .gap(px(2.0))
            .p(px(3.0))
            .rounded(px(11.0))
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(LINE))
            .child(mode_pill(Mode::ScreenZones, current, true, cx))
            .child(mode_pill(Mode::Screen, current, true, cx))
            .child(mode_pill(Mode::Audio, current, *AUDIO_SUPPORTED, cx))
    }

    fn render_hero(&self, phase: Phase) -> Div {
        let hero = div()
            .relative()
            .h(px(160.0))
            .rounded(px(16.0))
            .overflow_hidden();

        match (phase, &self.snapshot) {
            (Phase::Running, Some(snapshot)) if snapshot.all().next().is_some() => {
                let colors: Vec<Hsv> = snapshot.all().copied().collect();
                let tag = match self.profile.settings.mode {
                    Mode::ScreenZones => t!("hero.zones_screen"),
                    Mode::Screen => t!("hero.color_screen"),
                    Mode::Audio if *MUSIC_VIA_MICROPHONE => t!("hero.bass_microphone"),
                    Mode::Audio => t!("hero.bass_zones"),
                    Mode::Fixed => t!("mode.fixed"),
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
                        .child(tag.to_string()),
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
                        t!("status.connecting_panels").to_string(),
                        self.device_ip(),
                    ),
                    _ => (
                        SURFACE,
                        LINE,
                        TEXT,
                        t!("hero.paused_title").to_string(),
                        t!("hero.paused_body").to_string(),
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
            Phase::Idle => (t!("action.start"), true),
            Phase::Failed => (t!("action.retry"), true),
            Phase::Connecting => (t!("action.cancel"), false),
            Phase::Running => (t!("action.stop"), false),
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
            .child(label.to_string())
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
                        Some(device) => t!("footer.devices_named", name = device.name).to_string(),
                        None => t!("footer.devices").to_string(),
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
                    .flex()
                    .gap(px(16.0))
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
                            .hover(|style| style.text_color(rgb(TEXT_MUTED)))
                            .on_click(|_, _, cx| cx.quit())
                            .child(t!("footer.quit").to_string()),
                    ),
            )
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.sliders_stale {
            self.sync_sliders(window, cx);
        }
        // The setup flow needs less room, the device pages more.
        let height = self.wanted_height();
        if self.sized_for != Some(height) {
            if self.sized_for.is_some() {
                window.resize(window_size(height));
            }
            self.sized_for = Some(height);
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
        match &self.page {
            Page::Devices => return shell.child(self.render_devices(cx)),
            Page::Device(id) => {
                let page = self.render_device(&id.clone(), cx);
                return shell
                    .on_mouse_move(cx.listener(|this, event, _, cx| this.on_drag_move(event, cx)))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.end_drag(cx)),
                    )
                    .child(page);
            }
            Page::Settings => return shell.child(self.render_preferences(cx)),
            Page::Sync => {}
        }
        let phase = self.phase();
        let notice = self.render_update_notice(cx);
        // The notice takes some of the header's breathing room.
        let below_header = if notice.is_some() { 12.0 } else { 18.0 };
        shell
            .child(self.render_header(phase))
            .when_some(notice, |shell, notice| {
                shell.child(div().h(px(14.0))).child(notice)
            })
            .child(div().h(px(below_header)))
            .child(self.render_modes(cx))
            .child(div().h(px(14.0)))
            .child(self.render_hero(phase))
            .child(div().h(px(12.0)))
            .child(self.render_primary(phase, cx))
            .when(
                phase == Phase::Running && !self.unreachable.is_empty(),
                |shell| {
                    shell.child(
                        div()
                            .pt(px(8.0))
                            .text_size(px(11.5))
                            .text_color(rgb(TEXT_MUTED))
                            .child(
                                t!("sync.unreachable", names = self.unreachable.join(", "))
                                    .to_string(),
                            ),
                    )
                },
            )
            .child(div().h(px(22.0)))
            .child(
                // Scrolls when the Advanced adjustments do not fit.
                div()
                    .id("adjustments")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .child(self.render_settings(cx)),
            )
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
    target: Mode,
    current: Mode,
    available: bool,
    cx: &mut Context<Root>,
) -> Stateful<Div> {
    let id = match target {
        Mode::ScreenZones => "ambilight",
        Mode::Screen => "color",
        Mode::Audio => "music",
        Mode::Fixed => "fixed",
    };
    let pill = div()
        .id(id)
        .flex_1()
        .flex()
        .justify_center()
        .py(px(7.0))
        .rounded(px(8.0))
        .text_size(px(12.5))
        .child(mode_label(target));

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

/// Preview color: same hue as the panels, lightened so it stays readable on
/// screen even when the brightness sent is low.
fn display_color(color: Hsv) -> Rgba {
    let c = Hsv::new(color.h, color.s, 0.3 + 0.7 * color.v.clamp(0.0, 1.0)).to_rgb();
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    rgb((channel(c.r) << 16) | (channel(c.g) << 8) | channel(c.b))
}

/// Turns an engine error into a title and an actionable explanation. The
/// engine's own messages stay in English: they are only shown when nothing
/// more helpful can be said.
fn describe_error(kind: ErrorKind, message: &str, ip: &str) -> (String, String) {
    let (title, body) = match kind {
        ErrorKind::NotConfigured => (
            t!("error.not_configured_title"),
            t!("error.not_configured_body"),
        ),
        ErrorKind::Unreachable => (
            t!("error.unreachable_title"),
            t!("error.unreachable_body", ip = ip),
        ),
        ErrorKind::ScreenCapture => (
            t!("error.screen_title"),
            if cfg!(target_os = "macos") {
                t!("error.screen_permission")
            } else {
                message.to_owned().into()
            },
        ),
        ErrorKind::Audio if !*AUDIO_SUPPORTED => {
            (t!("error.sound_title"), t!("error.sound_unsupported"))
        }
        ErrorKind::Audio if *MUSIC_VIA_MICROPHONE => (
            t!("error.microphone_title"),
            t!("error.microphone_permission"),
        ),
        ErrorKind::Audio => (t!("error.sound_title"), t!("error.screen_permission")),
        ErrorKind::Device => (t!("error.device_title"), t!("error.device_body")),
        ErrorKind::Other => (t!("error.other_title"), message.to_owned().into()),
    };
    (title.to_string(), body.to_string())
}

fn mode_label(mode: Mode) -> String {
    match mode {
        Mode::ScreenZones => t!("mode.ambilight"),
        Mode::Screen => t!("mode.color"),
        Mode::Audio => t!("mode.music"),
        Mode::Fixed => t!("mode.fixed"),
    }
    .to_string()
}

/// Shows the interface in the preferred language, or the system's.
fn apply_language(preferences: &Preferences) {
    let system = sys_locale::get_locale();
    rust_i18n::set_locale(preferences.language.locale(system.as_deref()));
}

/// No title next to the icon, to keep the macOS menu bar clean. On Windows,
/// left click opens the window and right click the menu.
fn build_tray(cx: &mut App) -> gpui_tray::Result<Tray> {
    Tray::builder()
        .icon(tray_icon(Phase::Idle)?)
        .tooltip(t!("status.tooltip", status = t!("status.paused")).to_string())
        .on_activate(ShowWindow)
        .menu(tray_menu)
        .build(cx)
}

fn tray_menu(cx: &mut App) -> Vec<MenuItem> {
    let (phase, mode, status, update) = match cx.try_global::<TrayModel>() {
        Some(model) => (
            model.phase,
            model.mode,
            model.status.clone(),
            model.update.clone(),
        ),
        None => (
            Phase::Idle,
            Mode::ScreenZones,
            t!("status.paused").to_string(),
            None,
        ),
    };
    let toggle = match phase {
        Phase::Idle => t!("menu.start"),
        Phase::Failed => t!("menu.retry"),
        Phase::Connecting => t!("menu.cancel"),
        Phase::Running => t!("menu.stop"),
    };
    let mut items = vec![
        MenuItem::action(status, Noop).disabled(true),
        MenuItem::separator(),
        MenuItem::action(toggle.to_string(), ToggleSync),
        MenuItem::separator(),
        MenuItem::action(mode_label(Mode::ScreenZones), ModeAmbilight)
            .checked(mode == Mode::ScreenZones),
        MenuItem::action(mode_label(Mode::Screen), ModeColor).checked(mode == Mode::Screen),
        MenuItem::action(
            if *AUDIO_SUPPORTED {
                mode_label(Mode::Audio)
            } else {
                t!("menu.music_unavailable").to_string()
            },
            ModeMusic,
        )
        .checked(mode == Mode::Audio)
        .disabled(!*AUDIO_SUPPORTED),
        MenuItem::separator(),
        MenuItem::action(t!("menu.open").to_string(), ShowWindow),
        MenuItem::action(t!("menu.devices").to_string(), ShowDevices),
        MenuItem::action(t!("menu.settings").to_string(), ShowSettings),
    ];
    if let Some(version) = update {
        items.push(MenuItem::action(
            t!("menu.update", version = version).to_string(),
            OpenUpdate,
        ));
    }
    items.extend([
        MenuItem::separator(),
        MenuItem::action(t!("menu.quit").to_string(), Quit),
    ]);
    items
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

/// Opens the window on a page (from the menu bar), unless the setup flow is
/// in progress.
fn show_page(page: Page, cx: &mut App) {
    with_root(cx, |root, cx| {
        if root.setup.is_none() {
            root.page = page;
            cx.notify();
        }
    });
    show_window(cx);
}

fn with_root(cx: &mut App, update: impl FnOnce(&mut Root, &mut Context<Root>)) {
    let root = cx.global::<AppState>().root.clone();
    root.update(cx, update);
}

fn window_size(height: f32) -> Size<Pixels> {
    size(px(WINDOW_WIDTH), px(height + TITLEBAR_INSET))
}

fn window_options(height: f32) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(260.0), px(120.0)),
            window_size(height),
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
    let height = root.read(cx).wanted_height();
    // A reopened window is sized for what it shows from the start.
    root.update(cx, |root, _| root.sized_for = None);
    match gpui_kit::open_window(window_options(height), cx, |_, _| root) {
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
        MenuItem::action(t!("menu.close_window").to_string(), CloseWindow),
        MenuItem::separator(),
        MenuItem::action(t!("menu.quit").to_string(), Quit),
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
                .unwrap_or_else(|_| "lscsync_app=info,lscsync_engine=info".into()),
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
            cx.on_action(|_: &ShowDevices, cx| show_page(Page::Devices, cx));
            cx.on_action(|_: &ShowSettings, cx| show_page(Page::Settings, cx));
            cx.on_action(|_: &OpenUpdate, cx| {
                let url = cx
                    .global::<AppState>()
                    .root
                    .read(cx)
                    .update
                    .available()
                    .map(|r| r.url.clone());
                if let Some(url) = url {
                    cx.open_url(&url);
                }
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

            // Root::new picks the language: the menus come after it.
            let root = cx.new(Root::new);
            cx.set_menus(app_menus());
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
