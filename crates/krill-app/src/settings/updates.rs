//! 설정 > 업데이트: 앱 버전·재시작과 릴리스별 변경 사항.

use gpui::{Context, Div, FontWeight, div, prelude::*};
use gpui_component::{Disableable as _, Sizable as _, button::ButtonVariants as _};

use super::{
    Settings, SettingsView, edit, layout, restart_staged_update, state_dir, text,
    trigger_manual_update_check,
};
use crate::ui::Palette;
use crate::ui::primitives::{BadgeTone, badge};
use crate::updater::release_notes::{self, RELEASES_PAGE_URL, ReleaseNote};
use crate::updater::{CheckStatus, UpdateState};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Default)]
pub(super) struct Panel {
    notes: Vec<ReleaseNote>,
    loading: bool,
    failed: bool,
}

/// 칸막이 없이 구분선만 있는 행. 이 페이지는 카드 대신 이 행으로 가볍게 그린다.
fn flat_row(p: &Palette) -> Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_3()
        .py_3()
        .border_b_1()
        .border_color(p.border)
}

fn status_text(state: &UpdateState) -> String {
    if state.restart.is_starting() {
        return crate::i18n::catalog::updater_relaunch::CARD_RESTARTING_BTN
            .get()
            .to_owned();
    }
    if let Some(error) = state.restart.error() {
        return error.to_owned();
    }
    match &state.status {
        CheckStatus::Idle => state.staged.as_ref().map_or_else(
            || text::UPDATES_DESC.get().to_owned(),
            |staged| text::UPDATES_READY.format(&[("version", &staged.to_version)]),
        ),
        CheckStatus::Checking => text::UPDATES_CHECKING.get().to_owned(),
        CheckStatus::UpToDate { .. } => text::UPDATES_UP_TO_DATE.get().to_owned(),
        CheckStatus::ChannelMismatch {
            configured,
            offered,
        } => text::UPDATES_CHANNEL_MISMATCH.format(&[
            ("configured", configured.as_str()),
            ("offered", offered.as_str()),
        ]),
        CheckStatus::Ready(staged) => {
            text::UPDATES_READY.format(&[("version", &staged.to_version)])
        }
        CheckStatus::Error(err) => err.clone(),
    }
}

fn render_note(p: &Palette, note: &ReleaseNote) -> Div {
    let tag = if note.version == VERSION {
        Some(badge(
            p,
            BadgeTone::Muted,
            text::UPDATES_NOTES_CURRENT.get(),
        ))
    } else if krill_proto::control::is_newer(&note.version, VERSION) == Some(true) {
        Some(badge(p, BadgeTone::Accent, text::UPDATES_NOTES_NEW.get()))
    } else {
        None
    };
    let selector = format!("release-note-{}", note.version);
    let version = format!("v{}", note.version);
    let items = note.items_in(crate::i18n::language());
    let body = if items.is_empty() {
        div()
            .text_size(crate::ui::TEXT_SM)
            .text_color(p.muted_foreground)
            .child(text::UPDATES_NOTES_EMPTY.get())
    } else {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(items.iter().map(|item| {
                div()
                    .flex()
                    .gap_1p5()
                    .text_size(crate::ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(div().flex_none().child("•"))
                    .child(div().flex_1().min_w_0().child(item.clone()))
            }))
    };
    div()
        .debug_selector(move || selector)
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1p5()
        .py_3()
        .border_b_1()
        .border_color(p.border)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(crate::ui::TEXT_SM)
                        .font_weight(FontWeight::MEDIUM)
                        .child(version),
                )
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(note.date.clone()),
                )
                .children(tag),
        )
        .child(body)
}

impl SettingsView {
    /// 캐시를 먼저 보여 주고 필요할 때만 `GitHub`에서 받는다. 받는 동안 문구를 띄우지 않는다(STYLEGUIDE §7.0).
    pub(super) fn refresh_release_notes(&mut self, cx: &mut Context<Self>) {
        if self.release_notes.loading {
            return;
        }
        let Some(dir) = state_dir(cx) else {
            return;
        };
        let waiting = cx
            .try_global::<UpdateState>()
            .and_then(|u| u.staged.as_ref().map(|s| s.to_version.clone()));
        self.release_notes.loading = true;
        cx.spawn(async move |this, cx| {
            let cache_dir = dir.clone();
            let (cached, fresh) = cx
                .background_spawn(async move {
                    (
                        release_notes::read_cache(&cache_dir),
                        release_notes::cache_is_fresh(&cache_dir),
                    )
                })
                .await;
            // 준비된 새 버전의 글은 캐시가 새것이어도 있어야 한다.
            let complete = waiting
                .is_none_or(|version| cached.iter().flatten().any(|note| note.version == version));
            let done = (fresh && complete) || cfg!(test);
            this.update(cx, |this, cx| {
                if let Some(notes) = cached {
                    this.release_notes.notes = notes;
                }
                this.release_notes.loading = !done;
                cx.notify();
            })
            .ok();
            if done {
                return;
            }
            let fetched = cx
                .background_spawn(async move { release_notes::fetch(&dir) })
                .await;
            this.update(cx, |this, cx| {
                let panel = &mut this.release_notes;
                panel.loading = false;
                panel.failed = fetched.is_err() && panel.notes.is_empty();
                if let Ok(notes) = fetched {
                    panel.notes = notes;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn render_updates_tab(&self, s: &Settings, cx: &mut Context<Self>) -> Div {
        let p = crate::ui::palette(cx);
        let state = cx.try_global::<UpdateState>().cloned().unwrap_or_default();
        let page = div().flex().flex_col();
        // 자체 업데이터는 macOS 번들 전용이라 다른 OS에서는 버전과 변경 사항만 보인다(규칙 10).
        let page = if cfg!(target_os = "macos") {
            page.child(Self::render_app_row(&state, cx)).child(
                flat_row(p)
                    .child(layout::label(
                        p,
                        text::UPDATES_AUTO_RESTART.get(),
                        Some(text::UPDATES_AUTO_RESTART_DESC.get()),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "update-auto-restart-toggle",
                                s.auto_restart_update,
                                false,
                                |next, _, cx| edit(cx, move |s| s.auto_restart_update = next),
                            )
                            .aria_label(text::UPDATES_AUTO_RESTART.get()),
                        ),
                    ),
            )
        } else {
            page.child(flat_row(p).child(layout::label(
                p,
                format!("Krill v{VERSION}"),
                None::<&'static str>,
            )))
        };
        page.child(div().h(crate::ui::SPACE_4))
            .child(layout::section_title(
                p,
                "updates-section-notes",
                text::UPDATES_NOTES_TITLE.get(),
            ))
            .children(self.release_notes.failed.then(|| {
                div()
                    .debug_selector(|| "release-notes-failed".into())
                    .py_3()
                    .text_size(crate::ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::UPDATES_NOTES_FAILED.get())
            }))
            .children(
                self.release_notes
                    .notes
                    .iter()
                    .map(|note| render_note(p, note)),
            )
            .child(
                div().pt_2().child(
                    layout::ghost("release-notes-all", text::UPDATES_NOTES_ALL.get())
                        .debug_selector(|| "release-notes-all".into())
                        .on_click(|_, _, cx| cx.open_url(RELEASES_PAGE_URL)),
                ),
            )
    }

    fn render_app_row(state: &UpdateState, cx: &mut Context<Self>) -> Div {
        let p = crate::ui::palette(cx);
        let starting = state.restart.is_starting();
        let checking = matches!(state.status, CheckStatus::Checking);
        let staged = state.staged.clone().or_else(|| match &state.status {
            CheckStatus::Ready(staged) => Some(staged.clone()),
            _ => None,
        });
        let mut controls = layout::controls().flex_wrap().flex_shrink_1();
        if let Some(staged) = staged {
            let label = if state.restart.error().is_some() {
                crate::i18n::catalog::updater_relaunch::CARD_RETRY_BTN
                    .get()
                    .to_owned()
            } else {
                text::UPDATES_RESTART.format(&[("version", &staged.to_version)])
            };
            controls = controls.child(
                crate::ui::button_with_reserved_label(
                    "settings-update-restart",
                    label,
                    crate::i18n::catalog::updater_relaunch::CARD_RESTARTING_BTN.get(),
                    starting,
                )
                .small()
                .primary()
                .disabled(starting || checking)
                .debug_selector(|| "settings-update-restart".into())
                .on_click(move |_, _, cx| restart_staged_update(staged.clone(), cx)),
            );
        }
        flat_row(p)
            .child(
                layout::label(p, format!("Krill v{VERSION}"), status_text(state))
                    .debug_selector(|| "settings-update-status".into()),
            )
            .child(
                controls.child(
                    crate::ui::button_with_reserved_label(
                        "settings-update-check",
                        text::UPDATES_CHECK.get(),
                        text::UPDATES_CHECKING.get(),
                        checking,
                    )
                    .small()
                    .outline()
                    .disabled(starting || checking)
                    .debug_selector(|| "settings-update-check".into())
                    .on_click(|_, _, cx| trigger_manual_update_check(cx)),
                ),
            )
    }
}
