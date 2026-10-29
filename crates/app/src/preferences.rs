//! Settings page (language, update checks, about) and the update notice.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use lscsync_core::Language;
use lscsync_platform::preferences_path;
use rust_i18n::t;

use crate::theme::*;
use crate::updates::{self, CHECK_INTERVAL, FIRST_CHECK_DELAY, REPOSITORY, UpdateState};
use crate::{Page, Root, app_menus, apply_language};

impl Root {
    /// Checks for updates shortly after launch, then once a day, while the
    /// user allows it.
    pub(crate) fn spawn_update_checks(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_CHECK_DELAY).await;
            loop {
                let checked = this.update(cx, |root, cx| {
                    if root.preferences.check_updates {
                        root.check_updates(cx);
                    }
                });
                if checked.is_err() {
                    break;
                }
                cx.background_executor().timer(CHECK_INTERVAL).await;
            }
        })
        .detach();
    }

    pub(crate) fn check_updates(&mut self, cx: &mut Context<Self>) {
        if matches!(self.update, UpdateState::Checking) {
            return;
        }
        self.update = UpdateState::Checking;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { updates::check() })
                .await;
            let _ = this.update(cx, |root, cx| {
                root.update = result.unwrap_or_else(|error| {
                    tracing::warn!("update check: {error}");
                    UpdateState::Failed
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn save_preferences(&self) {
        if let Err(error) = self.preferences.save(&preferences_path()) {
            tracing::warn!("preferences not saved: {error}");
        }
    }

    fn set_language(&mut self, language: Language, cx: &mut Context<Self>) {
        self.preferences.language = language;
        self.save_preferences();
        apply_language(&self.preferences);
        // The menu bar menu follows on its next refresh (its key holds the
        // locale); the app menu is rebuilt here.
        cx.set_menus(app_menus());
        cx.notify();
    }

    fn toggle_update_checks(&mut self, cx: &mut Context<Self>) {
        self.preferences.check_updates = !self.preferences.check_updates;
        self.save_preferences();
        if self.preferences.check_updates && matches!(self.update, UpdateState::NotChecked) {
            self.check_updates(cx);
        }
        cx.notify();
    }

    /// "Version x is available" on the main page, until the user closes it
    /// for that version.
    pub(crate) fn render_update_notice(&self, cx: &mut Context<Self>) -> Option<Div> {
        let release = self.update.available()?;
        if self.preferences.dismissed_update.as_deref() == Some(release.version.as_str()) {
            return None;
        }
        let (url, version) = (release.url.clone(), release.version.clone());
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(9.0))
                .rounded(px(10.0))
                .bg(rgb(SURFACE))
                .border_1()
                .border_color(rgb(LINE_STRONG))
                .text_size(px(12.5))
                .child(div().size(px(6.0)).rounded_full().bg(rgb(ACCENT)))
                .child(
                    div()
                        .flex_1()
                        .child(t!("update.available", version = release.version).to_string()),
                )
                .child(
                    div()
                        .id("update-download")
                        .cursor_pointer()
                        .text_color(rgb(ACCENT))
                        .hover(|style| style.text_color(rgb(ACCENT_HOVER)))
                        .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url)))
                        .child(t!("update.download").to_string()),
                )
                .child(
                    div()
                        .id("update-dismiss")
                        .cursor_pointer()
                        .px(px(2.0))
                        .text_color(rgb(TEXT_FAINT))
                        .hover(|style| style.text_color(rgb(TEXT)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.preferences.dismissed_update = Some(version.clone());
                            this.save_preferences();
                            cx.notify();
                        }))
                        .child("✕"),
                ),
        )
    }

    pub(crate) fn render_preferences(&self, cx: &mut Context<Self>) -> Div {
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
                    .child(title(t!("settings.title").to_string()))
                    .child(
                        link("back", t!("action.back").to_string()).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.page = Page::Sync;
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(section_label(t!("settings.language_heading").to_string()))
            .child(card().child(
                row(t!("settings.language").to_string()).child(self.render_language_choice(cx)),
            ))
            .child(section_label(t!("settings.updates_heading").to_string()))
            .child(self.render_updates(cx))
            .when(
                cfg!(target_os = "macos") && self.update.available().is_some(),
                |this| this.child(body(t!("settings.macos_permission").to_string())),
            )
            .child(section_label(t!("settings.about_heading").to_string()))
            .child(
                card()
                    .child(row(t!("settings.source").to_string()).child(
                        link("github", "GitHub").on_click(|_, _, cx| cx.open_url(REPOSITORY)),
                    ))
                    .child(
                        row(t!("settings.license").to_string()).child(
                            div()
                                .text_size(px(12.0))
                                .text_color(rgb(TEXT_MUTED))
                                .child("Jonathan Gromat"),
                        ),
                    ),
            )
    }

    fn render_language_choice(&self, cx: &mut Context<Self>) -> Div {
        let current = self.preferences.language;
        let option = |language: Language, label: String, cx: &mut Context<Self>| {
            let chosen = language == current;
            div()
                .id(match language {
                    Language::Auto => "language-auto",
                    Language::English => "language-en",
                    Language::French => "language-fr",
                })
                .px(px(9.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .text_size(px(12.0))
                .map(|option| {
                    if chosen {
                        option.bg(rgb(SURFACE_RAISED)).text_color(rgb(TEXT))
                    } else {
                        option
                            .cursor_pointer()
                            .text_color(rgb(TEXT_MUTED))
                            .hover(|style| style.text_color(rgb(TEXT)))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_language(language, cx)),
                            )
                    }
                })
                .child(label)
        };
        div()
            .flex()
            .gap(px(2.0))
            .p(px(2.0))
            .rounded(px(8.0))
            .bg(rgb(BG))
            .border_1()
            .border_color(rgb(LINE))
            .child(option(
                Language::Auto,
                t!("settings.language_auto").to_string(),
                cx,
            ))
            // Each language is named in its own language.
            .child(option(Language::English, "English".to_owned(), cx))
            .child(option(Language::French, "Français".to_owned(), cx))
    }

    fn render_updates(&self, cx: &mut Context<Self>) -> Div {
        let current = updates::CURRENT_VERSION;
        let status = match &self.update {
            UpdateState::NotChecked => t!("settings.version", version = current),
            UpdateState::Checking => t!("settings.checking"),
            UpdateState::UpToDate => t!("settings.up_to_date", version = current),
            UpdateState::Available(release) => {
                t!("update.available", version = release.version)
            }
            UpdateState::Failed => t!("settings.check_failed"),
        };
        let action = match self.update.available() {
            Some(release) => {
                let url = release.url.clone();
                div()
                    .id("update-action")
                    .cursor_pointer()
                    .text_size(px(12.0))
                    .text_color(rgb(ACCENT))
                    .hover(|style| style.text_color(rgb(ACCENT_HOVER)))
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .child(t!("update.download").to_string())
            }
            None => link("update-action", t!("settings.check_now").to_string())
                .on_click(cx.listener(|this, _, _, cx| this.check_updates(cx))),
        };
        card()
            .child(
                row(t!("settings.check_automatically").to_string()).child(
                    toggle("check-updates", self.preferences.check_updates)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_update_checks(cx))),
                ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(TEXT_MUTED))
                            .child(status.to_string()),
                    )
                    .child(action),
            )
    }
}
