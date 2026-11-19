//! 설정 > 업데이트: 앱 버전·재시작과 릴리스별 변경 사항.

use std::collections::BTreeSet;
use std::time::Duration;

use gpui::{Context, Div, FontWeight, Task, div, prelude::*};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _, button::ButtonVariants as _};

use super::{
    Settings, SettingsView, edit, layout, restart_staged_update, state_dir, text,
    trigger_manual_update_check,
};
use crate::ui::Palette;
use crate::updater::release_notes::{self, RELEASES_PAGE_URL, ReleaseNote};
use crate::updater::{CheckStatus, UpdateState};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// 확인 중 문구를 띄우기 전에 기다리는 시간(STYLEGUIDE §7.3).
const CHECK_NOTICE_DELAY: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct Panel {
    notes: Vec<ReleaseNote>,
    loading: bool,
    failed: bool,
    /// 펼친 지난 릴리스 버전.
    open: BTreeSet<String>,
    /// 확인을 시작하기 전까지 보이던 상태. 확인이 빨리 끝나면 문구가 깜박이지 않게 그대로 둔다.
    settled: Option<(String, Tone)>,
    check_is_slow: bool,
    check_timer: Option<Task<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Muted,
    Success,
    Preserve,
    Warning,
    Destructive,
}

impl Tone {
    fn color(self, p: &Palette) -> gpui::Hsla {
        match self {
            Self::Muted => p.muted_foreground,
            Self::Success => p.success,
            Self::Preserve => p.preserve,
            Self::Warning => p.warning,
            Self::Destructive => p.destructive,
        }
    }
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

fn status_line(state: &UpdateState) -> (String, Tone) {
    if state.restart.is_starting() {
        return (
            crate::i18n::catalog::updater_relaunch::CARD_RESTARTING_BTN
                .get()
                .to_owned(),
            Tone::Preserve,
        );
    }
    if let Some(error) = state.restart.error() {
        return (error.to_owned(), Tone::Destructive);
    }
    let ready = |version: &str| {
        (
            text::UPDATES_READY.format(&[("version", version)]),
            Tone::Preserve,
        )
    };
    match &state.status {
        CheckStatus::Idle => state.staged.as_ref().map_or_else(
            || (text::UPDATES_DESC.get().to_owned(), Tone::Muted),
            |staged| ready(&staged.to_version),
        ),
        CheckStatus::Checking => (text::UPDATES_CHECKING.get().to_owned(), Tone::Muted),
        CheckStatus::UpToDate { .. } => (text::UPDATES_UP_TO_DATE.get().to_owned(), Tone::Success),
        CheckStatus::ChannelMismatch {
            configured,
            offered,
        } => (
            text::UPDATES_CHANNEL_MISMATCH.format(&[
                ("configured", configured.as_str()),
                ("offered", offered.as_str()),
            ]),
            Tone::Warning,
        ),
        CheckStatus::Ready(staged) => ready(&staged.to_version),
        CheckStatus::Error(err) => (err.clone(), Tone::Destructive),
    }
}

/// 릴리스 글을 재시작하면 바뀌는 것, 새 릴리스, 현재 버전, 지난 릴리스로 나눈다. 받은 순서를 지킨다.
#[derive(Debug, Default)]
struct NoteGroups<'a> {
    on_restart: Vec<&'a ReleaseNote>,
    newer: Vec<&'a ReleaseNote>,
    current: Vec<&'a ReleaseNote>,
    past: Vec<&'a ReleaseNote>,
}

fn group_notes<'a>(
    notes: &'a [ReleaseNote],
    staged: Option<&str>,
    version: &str,
) -> NoteGroups<'a> {
    let newer_than = |a: &str, b: &str| krill_proto::control::is_newer(a, b) == Some(true);
    let mut groups = NoteGroups::default();
    for note in notes {
        let bucket = if note.version == version {
            &mut groups.current
        } else if !newer_than(&note.version, version) {
            &mut groups.past
        } else if staged.is_some_and(|staged| !newer_than(&note.version, staged)) {
            &mut groups.on_restart
        } else {
            &mut groups.newer
        };
        bucket.push(note);
    }
    groups
}

/// 묶음 제목. 구분선 행과 같은 선을 쓴다.
fn group_title(p: &Palette, title: &'static str, extra: Option<String>) -> Div {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_baseline()
        .gap_1p5()
        .pt_3p5()
        .pb_1()
        .border_b_1()
        .border_color(p.border)
        .text_size(crate::ui::TEXT_SM)
        .text_color(p.muted_foreground)
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
        .children(extra.map(|extra| div().child(extra)))
}

fn note_body(p: &Palette, note: &ReleaseNote) -> Div {
    let items = note.items_in(crate::i18n::language());
    if items.is_empty() {
        return div()
            .text_size(crate::ui::TEXT_SM)
            .text_color(p.muted_foreground)
            .child(text::UPDATES_NOTES_EMPTY.get());
    }
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
}

fn render_note(p: &Palette, note: &ReleaseNote) -> Div {
    let version_label = format!("v{}", note.version);
    let selector = format!("release-note-{}", note.version);
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
                        .child(version_label),
                )
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(note.date.clone()),
                ),
        )
        .child(note_body(p, note))
}

impl SettingsView {
    /// 확인이 끝난 상태를 기억하고, 확인이 1초를 넘길 때만 상태 줄을 바꾸도록 타이머를 건다(§7.3).
    pub(super) fn track_update_check(&mut self, cx: &mut Context<Self>) {
        let Some(state) = cx.try_global::<UpdateState>() else {
            return;
        };
        let panel = &mut self.release_notes;
        if !matches!(state.status, CheckStatus::Checking) {
            panel.settled = Some(status_line(state));
            panel.check_is_slow = false;
            panel.check_timer = None;
            return;
        }
        if panel.check_timer.is_some() || panel.check_is_slow {
            return;
        }
        panel.check_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CHECK_NOTICE_DELAY).await;
            this.update(cx, |this, cx| {
                this.release_notes.check_is_slow = true;
                this.release_notes.check_timer = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// 지금 보여 줄 상태. 재시작 상태와 오류는 바로 보이고, 확인 중 문구만 1초 늦게 나온다.
    fn shown_status(&self, state: &UpdateState) -> (String, Tone) {
        let hold = matches!(state.status, CheckStatus::Checking)
            && !self.release_notes.check_is_slow
            && !state.restart.is_starting()
            && state.restart.error().is_none();
        if hold {
            let idle = UpdateState {
                status: CheckStatus::Idle,
                ..state.clone()
            };
            return self
                .release_notes
                .settled
                .clone()
                .unwrap_or_else(|| status_line(&idle));
        }
        status_line(state)
    }

    /// 지난 릴리스 한 줄. 펼친 버전만 본문을 만든다.
    fn render_past_note(&self, note: &ReleaseNote, cx: &mut Context<Self>) -> Div {
        let p = crate::ui::palette(cx);
        let open = self.release_notes.open.contains(&note.version);
        let selector = format!("release-note-{}", note.version);
        let head_id = format!("release-note-head-{}", note.version);
        let head_selector = head_id.clone();
        let version = note.version.clone();
        let version_label = format!("v{}", note.version);
        let keyboard_version = version.clone();
        let count = note.items_in(crate::i18n::language()).len();
        let summary = if count == 0 {
            text::UPDATES_NOTE_GITHUB.get().to_owned()
        } else {
            text::UPDATES_NOTE_COUNT.format(&[("count", &count.to_string())])
        };
        div()
            .debug_selector(move || selector)
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .id(gpui::SharedString::from(head_id))
                    .debug_selector(move || head_selector)
                    .role(gpui::Role::Button)
                    .aria_label(version_label.clone())
                    .aria_expanded(open)
                    .tab_index(0)
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .py_1p5()
                    .px_0p5()
                    .cursor_pointer()
                    .hover(|d| d.bg(p.accent))
                    .on_key_down(cx.listener(move |this, e: &gpui::KeyDownEvent, _, cx| {
                        if matches!(e.keystroke.key.as_str(), "enter" | "space") {
                            this.toggle_past_note(&keyboard_version, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_past_note(&version, cx);
                    }))
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall()
                        .text_color(p.muted_foreground),
                    )
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_SM)
                            .font_weight(FontWeight::MEDIUM)
                            .child(version_label),
                    )
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(note.date.clone()),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(summary),
                    ),
            )
            .when(open, |d| {
                d.child(div().pl_5().pr_0p5().pb_2p5().child(note_body(p, note)))
            })
    }

    fn toggle_past_note(&mut self, version: &str, cx: &mut Context<Self>) {
        let open = &mut self.release_notes.open;
        if !open.remove(version) {
            open.insert(version.to_owned());
        }
        cx.notify();
    }

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
            page.child(self.render_app_row(&state, cx)).child(
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
        let staged = state
            .staged
            .as_ref()
            .map(|s| s.to_version.clone())
            .or_else(|| match &state.status {
                CheckStatus::Ready(staged) => Some(staged.to_version.clone()),
                _ => None,
            });
        let groups = group_notes(&self.release_notes.notes, staged.as_deref(), VERSION);
        let page = page.children(self.release_notes.failed.then(|| {
            div()
                .debug_selector(|| "release-notes-failed".into())
                .py_3()
                .text_size(crate::ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(text::UPDATES_NOTES_FAILED.get())
        }));
        let open_group = |title, extra, notes: &[&ReleaseNote]| {
            (!notes.is_empty()).then(|| {
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(group_title(p, title, extra))
                    .children(notes.iter().map(|note| render_note(p, note)))
            })
        };
        let new_hint =
            cfg!(target_os = "macos").then(|| text::UPDATES_GROUP_NEW_HINT.get().to_owned());
        let past = (!groups.past.is_empty()).then(|| {
            let count =
                text::UPDATES_PAST_COUNT.format(&[("count", &groups.past.len().to_string())]);
            div()
                .w_full()
                .flex()
                .flex_col()
                .child(group_title(p, text::UPDATES_GROUP_PAST.get(), Some(count)))
                .children(
                    groups
                        .past
                        .iter()
                        .map(|note| self.render_past_note(note, cx)),
                )
        });
        page.child(
            div()
                .id("updates-section-notes")
                .debug_selector(|| "updates-section-notes".into())
                .w_full()
                .flex()
                .flex_col()
                .children(open_group(
                    text::UPDATES_GROUP_RESTART.get(),
                    None,
                    &groups.on_restart,
                ))
                .children(open_group(
                    text::UPDATES_GROUP_NEW.get(),
                    new_hint,
                    &groups.newer,
                ))
                .children(open_group(
                    text::UPDATES_NOTES_CURRENT.get(),
                    None,
                    &groups.current,
                ))
                .children(past),
        )
        .child(
            div().pt_2().child(
                layout::ghost("release-notes-all", text::UPDATES_NOTES_ALL.get())
                    .debug_selector(|| "release-notes-all".into())
                    .on_click(|_, _, cx| cx.open_url(RELEASES_PAGE_URL)),
            ),
        )
    }

    fn render_app_row(&self, state: &UpdateState, cx: &mut Context<Self>) -> Div {
        let p = crate::ui::palette(cx);
        let (status, tone) = self.shown_status(state);
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
                layout::label(p, format!("Krill v{VERSION}"), None::<&'static str>)
                    .debug_selector(|| "settings-update-status".into())
                    .child(
                        div()
                            .min_w_0()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(tone.color(p))
                            .child(status),
                    ),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn notes(versions: &[&str]) -> Vec<ReleaseNote> {
        versions
            .iter()
            .map(|version| ReleaseNote {
                version: (*version).to_owned(),
                date: "2026-11-06".to_owned(),
                items: Vec::new(),
                items_ko: Vec::new(),
            })
            .collect()
    }

    fn versions<'a>(list: &[&'a ReleaseNote]) -> Vec<&'a str> {
        list.iter().map(|note| note.version.as_str()).collect()
    }

    #[test]
    fn notes_split_by_staged_and_current_version() {
        let all = notes(&["0.4.0", "0.3.2", "0.3.1", "0.3.0", "0.2.0"]);
        let groups = group_notes(&all, Some("0.3.2"), "0.3.0");
        assert_eq!(versions(&groups.newer), ["0.4.0"]);
        assert_eq!(versions(&groups.on_restart), ["0.3.2", "0.3.1"]);
        assert_eq!(versions(&groups.current), ["0.3.0"]);
        assert_eq!(versions(&groups.past), ["0.2.0"]);

        let groups = group_notes(&all, None, "0.3.0");
        assert_eq!(versions(&groups.newer), ["0.4.0", "0.3.2", "0.3.1"]);
        assert!(groups.on_restart.is_empty());
    }

    #[test]
    fn status_tone_marks_failure_and_ready() {
        let error = UpdateState {
            status: CheckStatus::Error("boom".into()),
            ..UpdateState::default()
        };
        assert_eq!(status_line(&error), ("boom".to_owned(), Tone::Destructive));
        let idle = UpdateState::default();
        assert_eq!(status_line(&idle).1, Tone::Muted);
    }
}
