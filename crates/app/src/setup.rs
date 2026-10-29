//! First-run setup: connect the LSC account with a QR code, find the devices
//! on the local network, pick the one that follows the screen.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lscsync_cloud::{CloudError, QrLogin};
use lscsync_core::Device;
use lscsync_engine::tuya::scan_local_network_blocking;

use crate::theme::*;

/// How long a QR code stays valid before we offer a new one.
const SCAN_TIMEOUT: Duration = Duration::from_secs(180);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How long we listen for devices announcing themselves on the network.
const NETWORK_SCAN: Duration = Duration::from_secs(4);
/// Size of one QR module, in pixels.
const QR_MODULE: f32 = 4.0;

pub enum SetupEvent {
    /// Devices of the account, with the one chosen for the sync.
    Done {
        devices: Vec<Device>,
        sync_device: String,
    },
    ImportRequested,
    Cancelled,
}

enum Step {
    Welcome,
    UserCode {
        busy: bool,
        error: Option<String>,
    },
    Scan {
        qr: Qr,
        status: ScanStatus,
    },
    Found {
        devices: Vec<Device>,
        selected: usize,
    },
}

enum ScanStatus {
    Waiting,
    Fetching,
    Failed(String),
}

/// Devices of the account, and the (id, ip) pairs seen on the local network.
type Fetched = (Vec<lscsync_cloud::CloudDevice>, Vec<(String, String)>);

/// QR code as a square grid of dark modules.
struct Qr {
    width: usize,
    dark: Vec<bool>,
}

impl Qr {
    fn new(content: &str) -> Option<Self> {
        let code = qrcode::QrCode::new(content).ok()?;
        let dark = code
            .to_colors()
            .into_iter()
            .map(|color| color == qrcode::Color::Dark)
            .collect();
        Some(Self {
            width: code.width(),
            dark,
        })
    }
}

pub struct SetupFlow {
    step: Step,
    user_code: Option<Entity<InputState>>,
    /// Bumped on every new attempt so stale background tasks give up.
    attempt: u64,
    /// The user already has devices: offer a way back.
    cancellable: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SetupEvent> for SetupFlow {}

impl SetupFlow {
    pub fn new(cancellable: bool) -> Self {
        Self {
            step: Step::Welcome,
            user_code: None,
            attempt: 0,
            cancellable,
            _subscriptions: Vec::new(),
        }
    }

    /// Goes straight to the user code step (reconnecting from the devices page).
    pub fn go_to_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = match &self.user_code {
            Some(input) => input.clone(),
            None => {
                let input =
                    cx.new(|cx| InputState::new(window, cx).placeholder("Code utilisateur"));
                self._subscriptions.push(cx.subscribe(
                    &input,
                    |this: &mut Self, _, event: &InputEvent, cx| {
                        if matches!(event, InputEvent::PressEnter { .. }) {
                            this.submit_code(cx);
                        }
                    },
                ));
                self.user_code = Some(input.clone());
                input
            }
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        self.attempt += 1;
        self.step = Step::UserCode {
            busy: false,
            error: None,
        };
        cx.notify();
    }

    fn submit_code(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.user_code else {
            return;
        };
        let code = input.read(cx).value().trim().to_owned();
        if code.is_empty() {
            self.step = Step::UserCode {
                busy: false,
                error: Some("Saisis ton code utilisateur.".to_owned()),
            };
            cx.notify();
            return;
        }
        self.attempt += 1;
        let attempt = self.attempt;
        self.step = Step::UserCode {
            busy: true,
            error: None,
        };
        cx.notify();

        cx.spawn(async move |this, cx| {
            let started = cx
                .background_executor()
                .spawn(async move { QrLogin::start(&code) })
                .await;
            let _ = this.update(cx, |flow, cx| flow.on_login_started(attempt, started, cx));
        })
        .detach();
    }

    fn on_login_started(
        &mut self,
        attempt: u64,
        started: Result<QrLogin, CloudError>,
        cx: &mut Context<Self>,
    ) {
        if attempt != self.attempt {
            return;
        }
        let login = match started {
            Ok(login) => login,
            Err(error) => {
                tracing::warn!("QR login: {error}");
                self.step = Step::UserCode {
                    busy: false,
                    error: Some(login_error(&error)),
                };
                cx.notify();
                return;
            }
        };
        let Some(qr) = Qr::new(&login.qr_content()) else {
            self.step = Step::UserCode {
                busy: false,
                error: Some("Impossible d'afficher le QR code.".to_owned()),
            };
            cx.notify();
            return;
        };
        self.step = Step::Scan {
            qr,
            status: ScanStatus::Waiting,
        };
        cx.notify();
        self.wait_for_scan(attempt, Arc::new(login), cx);
    }

    /// Polls Tuya until the user confirms in the LSC app, then fetches the
    /// devices and looks for them on the local network.
    fn wait_for_scan(&mut self, attempt: u64, login: Arc<QrLogin>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let deadline = Instant::now() + SCAN_TIMEOUT;
            let session = loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let current = this.read_with(cx, |flow, _| flow.attempt == attempt);
                if !matches!(current, Ok(true)) {
                    return;
                }
                let login = login.clone();
                let polled = cx
                    .background_executor()
                    .spawn(async move { login.poll() })
                    .await;
                match polled {
                    Ok(Some(session)) => break session,
                    Ok(None) if Instant::now() < deadline => continue,
                    Ok(None) => {
                        let _ = this.update(cx, |flow, cx| {
                            flow.scan_failed(attempt, "Le QR code a expiré.".to_owned(), cx)
                        });
                        return;
                    }
                    Err(error) => {
                        tracing::warn!("QR login poll: {error}");
                        let _ = this.update(cx, |flow, cx| {
                            flow.scan_failed(attempt, login_error(&error), cx)
                        });
                        return;
                    }
                }
            };

            let _ = this.update(cx, |flow, cx| {
                if let Step::Scan { status, .. } = &mut flow.step {
                    *status = ScanStatus::Fetching;
                    cx.notify();
                }
            });
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    let devices = session.devices()?;
                    // The LAN scan is a bonus: without it, the engine
                    // rediscovers the address when it connects.
                    let found = scan_local_network_blocking(NETWORK_SCAN).unwrap_or_default();
                    Ok::<_, CloudError>((devices, found))
                })
                .await;
            let _ = this.update(cx, |flow, cx| flow.on_devices(attempt, fetched, cx));
        })
        .detach();
    }

    fn scan_failed(&mut self, attempt: u64, message: String, cx: &mut Context<Self>) {
        if attempt != self.attempt {
            return;
        }
        if let Step::Scan { status, .. } = &mut self.step {
            *status = ScanStatus::Failed(message);
            cx.notify();
        }
    }

    fn on_devices(
        &mut self,
        attempt: u64,
        fetched: Result<Fetched, CloudError>,
        cx: &mut Context<Self>,
    ) {
        if attempt != self.attempt {
            return;
        }
        let (devices, found) = match fetched {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!("device list: {error}");
                self.scan_failed(attempt, login_error(&error), cx);
                return;
            }
        };
        let mut devices: Vec<Device> = devices.into_iter().map(Device::from).collect();
        for device in &mut devices {
            if let Some((_, ip)) = found.iter().find(|(id, _)| *id == device.id) {
                device.ip = ip.clone();
            }
        }
        // Devices reachable on this network first: they are the useful ones.
        devices.sort_by_key(|device| device.ip.is_empty());
        self.step = Step::Found {
            devices,
            selected: 0,
        };
        cx.notify();
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        if let Step::Found { devices, selected } = &self.step
            && let Some(sync_device) = devices.get(*selected).map(|d| d.id.clone())
        {
            cx.emit(SetupEvent::Done {
                devices: devices.clone(),
                sync_device,
            });
        }
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .flex_1()
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(10.0))
                    .child(app_logo(px(96.0)))
                    .child(title("Bienvenue dans LscSync"))
                    .child(
                        body("Connecte tes appareils LSC Smart Connect en une minute, avec l'app LSC de ton téléphone.")
                            .text_center()
                            .max_w(px(300.0)),
                    ),
            )
            .child(
                button("connect", "Connecter mes appareils", true).on_click(cx.listener(
                    |this, _, window, cx| this.go_to_code(window, cx),
                )),
            )
            .child(
                button("import", "Importer un profil", false)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SetupEvent::ImportRequested))),
            )
            .when(self.cancellable, |this| {
                this.child(
                    div().flex().justify_center().child(
                        link("cancel", "Annuler")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SetupEvent::Cancelled))),
                    ),
                )
            })
    }

    fn render_user_code(&self, busy: bool, error: Option<&str>, cx: &mut Context<Self>) -> Div {
        let mut content = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .flex_1()
            .child(step_label("ÉTAPE 1 SUR 2"))
            .child(title("Ton code utilisateur LSC"))
            .child(
                card().child(
                    div()
                        .text_size(px(12.5))
                        .line_height(px(19.0))
                        .text_color(rgb(TEXT_SOFT))
                        .child("Dans l'app LSC Smart Connect : Moi › Paramètres › Compte et sécurité › Code utilisateur."),
                ),
            );
        if let Some(input) = &self.user_code {
            content = content.child(div().font_family(FONT_MONO).child(Input::new(input)));
        }
        content
            .when_some(error, |this, error| {
                this.child(error_text(error.to_owned()))
            })
            .child(body(
                "Ce code n'est envoyé qu'à Tuya, le temps de la connexion.",
            ))
            .child(div().flex_1())
            .child(
                button(
                    "continue",
                    if busy { "Connexion…" } else { "Continuer" },
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !busy {
                        this.submit_code(cx);
                    }
                })),
            )
            .child(
                div()
                    .flex()
                    .justify_center()
                    .child(
                        link("back", "Retour").on_click(cx.listener(|this, _, _, cx| {
                            this.attempt += 1;
                            this.step = Step::Welcome;
                            cx.notify();
                        })),
                    ),
            )
    }

    fn render_scan(&self, qr: &Qr, status: &ScanStatus, cx: &mut Context<Self>) -> Div {
        let module = px(QR_MODULE);
        let grid = div()
            .p(px(QR_MODULE * 3.0))
            .rounded(px(12.0))
            .bg(rgb(0xFFFFFF))
            .flex()
            .flex_col()
            .children(qr.dark.chunks(qr.width).map(|row| {
                div().flex().children(
                    row.iter()
                        .map(|&dark| div().size(module).when(dark, |cell| cell.bg(rgb(0x000000)))),
                )
            }));

        let status_row = match status {
            ScanStatus::Waiting => status_line(ACCENT, "En attente du scan…"),
            ScanStatus::Fetching => status_line(SUCCESS, "Récupération de tes appareils…"),
            ScanStatus::Failed(message) => status_line(DANGER_TEXT, message),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .flex_1()
            .child(step_label("ÉTAPE 2 SUR 2"))
            .child(title("Scanne ce code avec l'app LSC"))
            .child(div().flex().justify_center().child(grid))
            .child(body(
                "App LSC › Moi › icône de scan en haut à droite, puis confirme la connexion.",
            ))
            .child(status_row)
            .child(div().flex_1())
            .when(matches!(status, ScanStatus::Failed(_)), |this| {
                this.child(
                    button("retry", "Nouveau QR code", true)
                        .on_click(cx.listener(|this, _, _, cx| this.submit_code(cx))),
                )
            })
            .child(
                div().flex().justify_center().child(
                    link("back", "Retour")
                        .on_click(cx.listener(|this, _, window, cx| this.go_to_code(window, cx))),
                ),
            )
    }

    fn render_found(&self, devices: &[Device], selected: usize, cx: &mut Context<Self>) -> Div {
        let count = devices.len();
        let mut list = card().gap(px(4.0)).p(px(6.0));
        for (index, device) in devices.iter().enumerate() {
            list = list.child(device_row(
                ("device", index),
                device,
                index == selected,
                cx.listener(move |this, _, _, cx| {
                    if let Step::Found { selected, .. } = &mut this.step {
                        *selected = index;
                        cx.notify();
                    }
                }),
            ));
        }

        let content = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .flex_1()
            .child(title(match count {
                0 => "Aucun appareil sur ce compte".to_owned(),
                1 => "1 appareil sur ton compte".to_owned(),
                n => format!("{n} appareils sur ton compte"),
            }));

        if count == 0 {
            return content
                .child(body(
                    "Ajoute d'abord tes appareils dans l'app LSC Smart Connect, puis réessaie.",
                ))
                .child(div().flex_1())
                .child(
                    button("retry", "Réessayer", true)
                        .on_click(cx.listener(|this, _, window, cx| this.go_to_code(window, cx))),
                );
        }

        content
            .child(list)
            .child(body(
                "Choisis l'appareil qui suit ton écran. Les clés restent sur cet ordinateur.",
            ))
            .child(div().flex_1())
            .child(
                button("done", "C'est parti", true)
                    .on_click(cx.listener(|this, _, _, cx| this.finish(cx))),
            )
    }
}

impl Render for SetupFlow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.step {
            Step::Welcome => self.render_welcome(cx),
            Step::UserCode { busy, error } => {
                let (busy, error) = (*busy, error.clone());
                self.render_user_code(busy, error.as_deref(), cx)
            }
            Step::Scan { qr, status } => self.render_scan(qr, status, cx),
            Step::Found { devices, selected } => self.render_found(devices, *selected, cx),
        };
        div().size_full().flex().flex_col().child(content)
    }
}

/// One device in a list: status dot, name, address, and a "Synchro" tag when
/// it is the device that follows the screen.
pub fn device_row(
    id: impl Into<ElementId>,
    device: &Device,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let online = !device.ip.is_empty();
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(10.0))
        .py(px(9.0))
        .rounded(px(9.0))
        .cursor_pointer()
        .text_size(px(12.5))
        .when(selected, |row| row.bg(rgb(SURFACE_RAISED)))
        .hover(|style| style.bg(rgb(SURFACE_HOVER)))
        .on_click(on_click)
        .child(div().size(px(8.0)).rounded_full().bg(rgb(if online {
            SUCCESS
        } else {
            TEXT_FAINT
        })))
        .child(
            div()
                .flex_1()
                .text_color(rgb(if online { TEXT } else { TEXT_MUTED }))
                .child(device.name.clone()),
        )
        .when(selected, |row| {
            row.child(
                div()
                    .px(px(7.0))
                    .py(px(2.0))
                    .rounded_full()
                    .bg(rgb(ACCENT))
                    .text_size(px(10.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(ON_ACCENT))
                    .child("Synchro"),
            )
        })
        .child(
            div()
                .font_family(FONT_MONO)
                .text_size(px(11.0))
                .text_color(rgb(TEXT_FAINT))
                .child(if online {
                    device.ip.clone()
                } else {
                    "hors ligne".to_owned()
                }),
        )
}

fn status_line(dot: u32, text: &str) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(8.0))
        .text_size(px(12.0))
        .text_color(rgb(TEXT_MUTED))
        .child(div().size(px(6.0)).rounded_full().bg(rgb(dot)))
        .child(text.to_owned())
}

/// User-facing explanation of a login failure.
fn login_error(error: &CloudError) -> String {
    match error {
        CloudError::Network(_) => {
            "Pas de connexion à Internet : vérifie ta connexion et réessaie.".to_owned()
        }
        CloudError::Refused { .. } => {
            "Code utilisateur refusé : vérifie-le dans l'app LSC Smart Connect.".to_owned()
        }
        CloudError::Response(_) => {
            "Réponse inattendue de Tuya, réessaie dans un instant.".to_owned()
        }
    }
}
