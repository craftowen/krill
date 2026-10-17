//! OWE-723 프로젝트 추가 대화상자. 사이드바 폴더 아이콘·빈 상태 '폴더 열기'로 연다.
//! 모양은 Orca `AddRepoDialog` 시작 단계(주 동작 카드 + '다른 추가 방법' 목록)를 따르되 Krill에 있는 것만 둔다:
//! 원격 호스트 선택은 없다. 폴더 선택·복제·새 프로젝트의 실제 동작은 셸이 [`Event`]를 받아 한다.

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, KeyDownEvent, SharedString, Subscription, Window,
    div, prelude::*,
};
use gpui_component::Icon;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_kit_assets::IconName;

use crate::i18n::catalog::add_project as text;
use crate::ui;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 기존 폴더를 고른다.
    Browse,
    /// 이 URL을 복제한다(위치는 셸이 묻는다).
    Clone(String),
    /// 빈 폴더를 만들고 `git init` 한다(이름·위치는 셸이 저장 패널로 묻는다).
    Create,
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Start,
    Clone,
}

pub struct Dialog {
    step: Step,
    url: Entity<InputState>,
    error: Option<&'static str>,
    focus: FocusHandle,
    _enter: Subscription,
}

impl EventEmitter<Event> for Dialog {}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dialog")
            .field("step", &self.step)
            .finish_non_exhaustive()
    }
}

/// `git clone`이 만들 폴더 이름(git의 기본 규칙: 마지막 경로 조각에서 `.git`·`/` 제거).
pub fn clone_dir(url: &str) -> Option<String> {
    let tail = url.trim().trim_end_matches('/').rsplit(['/', ':']).next()?;
    let name = tail.strip_suffix(".git").unwrap_or(tail);
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_owned())
}

impl Dialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder(text::URL_PLACEHOLDER.get()));
        let enter = cx.subscribe(&url, |d, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                d.submit_clone(cx);
            }
        });
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            step: Step::Start,
            url,
            error: None,
            focus,
            _enter: enter,
        }
    }

    fn show_clone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step = Step::Clone;
        self.url.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn submit_clone(&mut self, cx: &mut Context<Self>) {
        let url = self.url.read(cx).value().trim().to_owned();
        if clone_dir(&url).is_none() {
            self.error = Some(text::BAD_URL.get());
            cx.notify();
            return;
        }
        cx.emit(Event::Clone(url));
    }

    fn on_key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        match e.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                cx.emit(Event::Close);
            }
            "enter" if self.step == Step::Start => {
                cx.stop_propagation();
                cx.emit(Event::Browse);
            }
            _ => {}
        }
    }
}

/// 아이콘 + 제목 + 설명 한 줄(Orca `AddRepoPrimaryStartAction`/`AddRepoSecondaryStartAction`).
fn action(
    p: &ui::Palette,
    id: &'static str,
    icon: IconName,
    title: &'static str,
    desc: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2p5()
        .cursor_pointer()
        .hover(|d| d.bg(p.accent))
        .child(
            div()
                .flex_none()
                .size_7()
                .flex()
                .items_center()
                .justify_center()
                .rounded(ui::RADIUS_SM)
                .child(Icon::new(icon)),
        )
        .child(ui::label(p, title, Some(desc)))
}

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let title = match self.step {
            Step::Start => text::TITLE.get(),
            Step::Clone => text::CLONE.get(),
        };
        let body = div().flex().flex_col().gap_3();
        let body = match self.step {
            Step::Start => body
                .child(
                    // Orca: 주 동작은 선택된 카드처럼 보이고 Enter 칩을 단다.
                    action(
                        p,
                        "add-project-browse",
                        IconName::FolderOpen,
                        text::BROWSE.get(),
                        text::BROWSE_DESC.get(),
                    )
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.ring)
                    .bg(p.accent)
                    .child(ui::keycap(p, "↵"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Browse))),
                )
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(p.muted_foreground)
                        .child(text::OTHER.get()),
                )
                .child(
                    div()
                        .rounded(ui::RADIUS_MD)
                        .border_1()
                        .border_color(p.input)
                        .bg(p.background)
                        .overflow_hidden()
                        .child(
                            action(
                                p,
                                "add-project-clone",
                                IconName::Globe,
                                text::CLONE.get(),
                                text::CLONE_DESC.get(),
                            )
                            .on_click(cx.listener(|d, _, window, cx| d.show_clone(window, cx))),
                        )
                        .child(
                            action(
                                p,
                                "add-project-create",
                                IconName::Plus,
                                text::CREATE.get(),
                                text::CREATE_DESC.get(),
                            )
                            .border_t_1()
                            .border_color(p.border)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Create))),
                        ),
                ),
            Step::Clone => body
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(ui::field_label(text::URL.get(), None, p))
                        .child(Input::new(&self.url)),
                )
                .children(self.error.map(|e| ui::error_banner(p, e)))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("add-project-back")
                                .ghost()
                                .label(text::BACK.get())
                                .on_click(cx.listener(|d, _, window, cx| {
                                    d.step = Step::Start;
                                    d.error = None;
                                    d.focus.focus(window, cx);
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("add-project-clone-submit")
                                .primary()
                                .label(text::CLONE_SUBMIT.get())
                                .on_click(cx.listener(|d, _, _, cx| d.submit_clone(cx))),
                        ),
                ),
        };
        let card = div()
            .id("add-project-dialog")
            .track_focus(&self.focus)
            .occlude()
            .w(ui::DIALOG_WIDTH)
            .max_w(window.viewport_size().width - ui::SPACE_4 * 2.)
            .p_6()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .capture_key_down(cx.listener(|d, e: &KeyDownEvent, _, cx| d.on_key(e, cx)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Event::Close)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(SharedString::from(title)),
                    )
                    .child(
                        Button::new("add-project-close")
                            .ghost()
                            .icon(gpui_component::IconName::Close)
                            .accessibility_label(text::CLOSE.get())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                    ),
            )
            .child(body);
        ui::modal_backdrop(window.viewport_size(), card)
    }
}

#[cfg(test)]
#[path = "add_project/tests.rs"]
mod tests;
