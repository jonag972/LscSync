//! Devices page: pick the synced device, reconnect the account, find devices
//! on the network, export and import the profile.

use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lscsync_core::Profile;
use lscsync_engine::tuya::scan_local_network_blocking;
use lscsync_platform::profile_path;

use crate::setup::{SetupEvent, SetupFlow, device_row};
use crate::theme::*;
use crate::{Page, Root};

const NETWORK_SCAN: Duration = Duration::from_secs(4);
const EXPORT_NAME: &str = "lscsync.lscsync";

impl Root {
    /// Shows the setup flow; `reconnect` jumps straight to the user code step.
    pub(crate) fn open_setup(
        &mut self,
        reconnect: bool,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let cancellable = !self.profile.devices.is_empty();
        let flow = cx.new(|_| SetupFlow::new(cancellable));
        if reconnect && let Some(window) = window {
            flow.update(cx, |flow, cx| flow.go_to_code(window, cx));
        }
        self._subscriptions
            .push(cx.subscribe(&flow, |this: &mut Self, _, event, cx| {
                this.on_setup_event(event, cx)
            }));
        self.setup = Some(flow);
        cx.notify();
    }

    fn on_setup_event(&mut self, event: &SetupEvent, cx: &mut Context<Self>) {
        match event {
            SetupEvent::Done {
                devices,
                sync_device,
            } => {
                self.profile.merge_devices(devices.clone());
                self.set_sync_device(sync_device.clone(), cx);
                self.setup = None;
                self.page = Page::Sync;
            }
            SetupEvent::ImportRequested => self.import(cx),
            SetupEvent::Cancelled => self.setup = None,
        }
        cx.notify();
    }

    /// Changes the device that follows the screen, restarting a running sync.
    pub(crate) fn set_sync_device(&mut self, id: String, cx: &mut Context<Self>) {
        if self.profile.sync_device.as_deref() == Some(id.as_str()) {
            self.save();
            return;
        }
        self.profile.sync_device = Some(id);
        self.save();
        if self.engine.is_some() {
            self.stop(cx);
            self.start(cx);
        }
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { scan_local_network_blocking(NETWORK_SCAN) })
                .await;
            let _ = this.update(cx, |root, cx| {
                root.scanning = false;
                match found {
                    Ok(found) => {
                        let known = |id: &str| root.profile.devices.iter().any(|d| d.id == id);
                        let matches = found.iter().filter(|(id, _)| known(id)).count();
                        for device in &mut root.profile.devices {
                            device.ip = found
                                .iter()
                                .find(|(id, _)| *id == device.id)
                                .map(|(_, ip)| ip.clone())
                                .unwrap_or_default();
                        }
                        root.save();
                        root.notice = Some(match matches {
                            0 => "Aucun de tes appareils n'a répondu sur ce réseau.".to_owned(),
                            1 => "1 appareil trouvé sur ce réseau.".to_owned(),
                            n => format!("{n} appareils trouvés sur ce réseau."),
                        });
                    }
                    Err(error) => {
                        tracing::warn!("network scan: {error}");
                        root.notice = Some("La recherche sur le réseau a échoué.".to_owned());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        let content = self.profile.to_export();
        let directory = dirs_home();
        let path = cx.prompt_for_new_path(&directory, Some(EXPORT_NAME));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            let written = std::fs::write(&path, content);
            let _ = this.update(cx, |root, cx| {
                root.notice = Some(match written {
                    Ok(()) => format!("Profil exporté : {}", path.display()),
                    Err(error) => format!("Export impossible : {error}"),
                });
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn import(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Importer".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let imported = std::fs::read_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|text| Profile::from_import(&text).map_err(|error| error.to_string()));
            let _ = this.update(cx, |root, cx| {
                match imported {
                    Ok(profile) => root.apply_imported(profile, cx),
                    Err(error) => {
                        tracing::warn!("import {}: {error}", path.display());
                        root.notice =
                            Some("Ce fichier n'est pas un profil LscSync valide.".to_owned());
                        root.page = Page::Devices;
                        root.setup = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_imported(&mut self, profile: Profile, cx: &mut Context<Self>) {
        if self.engine.is_some() {
            self.stop(cx);
        }
        let count = profile.devices.len();
        self.profile = profile;
        self.load_sliders(cx);
        self.save();
        self.setup = None;
        self.page = Page::Devices;
        self.notice = Some(match count {
            1 => "Profil importé : 1 appareil.".to_owned(),
            n => format!("Profil importé : {n} appareils."),
        });
    }

    pub(crate) fn render_devices(&self, cx: &mut Context<Self>) -> Div {
        let sync_id = self.profile.sync_device.clone();
        let mut list = card().gap(px(4.0)).p(px(6.0));
        for (index, device) in self.profile.devices.iter().enumerate() {
            let id = device.id.clone();
            list = list.child(device_row(
                ("device", index),
                device,
                sync_id.as_deref() == Some(id.as_str()),
                cx.listener(move |this, _, _, cx| {
                    this.set_sync_device(id.clone(), cx);
                    cx.notify();
                }),
            ));
        }

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
                    .child(title("Appareils"))
                    .child(
                        link("back", "Retour").on_click(cx.listener(|this, _, _, cx| {
                            this.page = Page::Sync;
                            this.notice = None;
                            cx.notify();
                        })),
                    ),
            )
            .child(list)
            .child(body(
                "Touche un appareil pour choisir celui qui suit ton écran.",
            ))
            .when_some(self.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .text_size(px(12.0))
                        .text_color(rgb(TEXT_SOFT))
                        .child(notice),
                )
            })
            .child(div().flex_1())
            .child(
                button(
                    "rescan",
                    if self.scanning {
                        "Recherche en cours…"
                    } else {
                        "Rechercher sur le réseau"
                    },
                    false,
                )
                .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
            )
            .child(
                button("reconnect", "Reconnecter mon compte LSC", false).on_click(
                    cx.listener(|this, _, window, cx| this.open_setup(true, Some(window), cx)),
                ),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .child(
                        button("export", "Exporter le profil", false)
                            .flex_1()
                            .on_click(cx.listener(|this, _, _, cx| this.export(cx))),
                    )
                    .child(
                        button("import", "Importer", false)
                            .flex_1()
                            .on_click(cx.listener(|this, _, _, cx| this.import(cx))),
                    ),
            )
            .child(body(
                "Le profil exporté contient les clés de tes appareils : garde-le pour toi.",
            ))
    }
}

/// Default folder for the export dialog.
fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(Into::into)
        .unwrap_or_else(|| profile_path().parent().map(Into::into).unwrap_or_default())
}
