//! OWE-723 프로젝트 추가 대화상자. 사이드바 폴더 아이콘·빈 상태 '폴더 열기'로 연다.
//! 모양은 Orca `AddRepoDialog` 시작 단계(주 동작 카드 + '다른 추가 방법' 목록)를 따르되 Krill에 있는 것만 둔다.
//! Orca처럼 위에서 실행 위치(내 Mac + 연결된 원격 호스트)를 고른다. 원격 호스트를 고르면
//! 폴더 찾아보기·복제 위치·새 프로젝트 경로를 호스트에 직접 입력받는다. 실제 동작은 셸이 [`Event`]를 받아 한다.

use std::path::PathBuf;

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, KeyDownEvent, SharedString, Subscription, Window,
    div, prelude::*,
};
use gpui_component::Icon;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_kit_assets::IconName;

use crate::hosts::HostId;
use crate::i18n::catalog::add_project as text;
use crate::ui;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 로컬 폴더를 고른다(셸이 OS 폴더 선택 패널을 띄운다).
    Browse(HostId),
    /// 호스트의 폴더를 경로로 직접 연다(원격 찾아보기·입력).
    OpenPath(HostId, PathBuf),
    /// 로컬에 복제한다(위치는 셸이 묻는다).
    Clone {
        host: HostId,
        url: String,
    },
    /// 호스트의 `parent`에 복제한다(원격, 경로 입력済み).
    CloneAt {
        host: HostId,
        url: String,
        parent: PathBuf,
    },
    /// 빈 폴더를 만들고 `git init` 한다(이름·위치는 셸이 저장 패널로 묻는다, 로컬).
    Create(HostId),
    /// 호스트의 `path`를 만들고 `git init` 한다(원격, 경로 입력済み).
    CreateAt(HostId, PathBuf),
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Start,
    Clone,
    RemotePath(RemoteMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteMode {
    Browse,
    Create,
}

pub struct Dialog {
    step: Step,
    hosts: Vec<(HostId, String)>,
    host: HostId,
    url: Entity<InputState>,
    dest: Entity<InputState>,
    remote_path: Entity<InputState>,
    remote_mode: RemoteMode,
    error: Option<&'static str>,
    focus: FocusHandle,
    _enter_url: Subscription,
    _enter_dest: Subscription,
    _enter_path: Subscription,
    /// OWE-852: 시작 단계 3개 카드의 키보드 선택 위치(0=찾아보기, 1=복제, 2=만들기).
    selected: usize,
}

impl EventEmitter<Event> for Dialog {}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dialog")
            .field("step", &self.step)
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// `git clone`이 만들 폴더 이름(git의 기본 규칙: 마지막 경로 조각에서 `.git`·`/` 제거).
pub fn clone_dir(url: &str) -> Option<String> {
    let tail = url.trim().trim_end_matches('/').rsplit(['/', ':']).next()?;
    let name = tail.strip_suffix(".git").unwrap_or(tail);
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_owned())
}

/// 호스트의 입력 경로(앞뒤 공백 제거, 절대 경로만 받는다).
fn host_path(raw: &str) -> Option<PathBuf> {
    let path = PathBuf::from(raw.trim());
    (path.is_absolute()
        && path
            .components()
            .any(|c| matches!(c, std::path::Component::Normal(_))))
    .then_some(path)
}

impl Dialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_hosts(window, cx, Vec::new(), HostId::Local)
    }

    pub fn new_with_hosts(
        window: &mut Window,
        cx: &mut Context<Self>,
        mut hosts: Vec<(HostId, String)>,
        initial: HostId,
    ) -> Self {
        if hosts.is_empty() {
            hosts.push((
                HostId::Local,
                crate::i18n::catalog::shell::HOST_LOCAL
                    .mac_or(crate::i18n::catalog::other_os::HOST_LOCAL)
                    .get()
                    .to_owned(),
            ));
        }
        let host = if hosts.iter().any(|(id, _)| id == &initial) {
            initial
        } else {
            HostId::Local
        };
        let url = cx.new(|cx| InputState::new(window, cx).placeholder(text::URL_PLACEHOLDER.get()));
        let dest = cx
            .new(|cx| InputState::new(window, cx).placeholder(text::REMOTE_PATH_PLACEHOLDER.get()));
        let remote_path = cx
            .new(|cx| InputState::new(window, cx).placeholder(text::REMOTE_PATH_PLACEHOLDER.get()));
        let enter_url = cx.subscribe(&url, |d, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                d.submit_clone(cx);
            }
        });
        let enter_dest = cx.subscribe(&dest, |d, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                d.submit_clone(cx);
            }
        });
        let enter_path = cx.subscribe(&remote_path, |d, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                d.submit_remote_path(cx);
            }
        });
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            step: Step::Start,
            hosts,
            host,
            url,
            dest,
            remote_path,
            remote_mode: RemoteMode::Browse,
            error: None,
            focus,
            _enter_url: enter_url,
            _enter_dest: enter_dest,
            _enter_path: enter_path,
            selected: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn host(&self) -> &HostId {
        &self.host
    }

    #[cfg(test)]
    pub(crate) fn hosts_len(&self) -> usize {
        self.hosts.len()
    }

    fn set_host(&mut self, host: HostId, window: &mut Window, cx: &mut Context<Self>) {
        self.host = host;
        self.error = None;
        if self.step == Step::Clone && self.host.is_local() {
            // 로컬 복제는 목적지 입력이 필요 없다.
        } else if self.step == Step::Clone {
            self.dest.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    fn browse_activated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.host.is_local() {
            cx.emit(Event::Browse(self.host.clone()));
        } else {
            self.remote_mode = RemoteMode::Browse;
            self.step = Step::RemotePath(RemoteMode::Browse);
            self.error = None;
            self.remote_path
                .update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
        }
    }

    fn create_activated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.host.is_local() {
            cx.emit(Event::Create(self.host.clone()));
        } else {
            self.remote_mode = RemoteMode::Create;
            self.step = Step::RemotePath(RemoteMode::Create);
            self.error = None;
            self.remote_path
                .update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
        }
    }

    fn show_clone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step = Step::Clone;
        self.error = None;
        self.url.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn show_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step = Step::Start;
        self.error = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    fn activate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected {
            0 => self.browse_activated(window, cx),
            1 => self.show_clone(window, cx),
            _ => self.create_activated(window, cx),
        }
    }

    fn submit_clone(&mut self, cx: &mut Context<Self>) {
        let url = self.url.read(cx).value().trim().to_owned();
        if clone_dir(&url).is_none() {
            self.error = Some(text::BAD_URL.get());
            cx.notify();
            return;
        }
        if self.host.is_local() {
            cx.emit(Event::Clone {
                host: self.host.clone(),
                url,
            });
            return;
        }
        let raw = self.dest.read(cx).value().trim().to_owned();
        let Some(parent) = host_path(&raw) else {
            self.error = Some(text::REMOTE_PATH_BAD.get());
            cx.notify();
            return;
        };
        cx.emit(Event::CloneAt {
            host: self.host.clone(),
            url,
            parent,
        });
    }

    fn submit_remote_path(&mut self, cx: &mut Context<Self>) {
        let raw = self.remote_path.read(cx).value().trim().to_owned();
        let Some(path) = host_path(&raw) else {
            self.error = Some(text::REMOTE_PATH_BAD.get());
            cx.notify();
            return;
        };
        match self.remote_mode {
            RemoteMode::Browse => cx.emit(Event::OpenPath(self.host.clone(), path)),
            RemoteMode::Create => cx.emit(Event::CreateAt(self.host.clone(), path)),
        }
    }

    fn on_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match e.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                if self.step == Step::Start {
                    cx.emit(Event::Close);
                } else {
                    // OWE-852: 하위 단계 Esc는 모달을 닫지 않고 시작 단계로 복귀한다.
                    self.show_start(window, cx);
                }
            }
            "up" if self.step == Step::Start && self.focus.is_focused(window) => {
                cx.stop_propagation();
                self.selected = match self.selected {
                    0 => 2,
                    n => n - 1,
                };
                cx.notify();
            }
            "down" if self.step == Step::Start && self.focus.is_focused(window) => {
                cx.stop_propagation();
                self.selected = match self.selected {
                    0 | 1 => self.selected + 1,
                    _ => 0,
                };
                cx.notify();
            }
            "enter" if self.step == Step::Start && self.focus.is_focused(window) => {
                cx.stop_propagation();
                self.activate_selected(window, cx);
            }
            _ => {}
        }
    }

    fn host_selector(
        &self,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if self.hosts.len() <= 1 {
            return None;
        }
        let current = self.host.clone();
        let chips = self
            .hosts
            .iter()
            .map(|(id, label)| {
                let selected = id == &current;
                let next = id.clone();
                let key = id.as_key().to_owned();
                let debug_key = key.clone();
                let chip_label = label.clone();
                div()
                    .id(SharedString::from(format!("add-project-host-{key}")))
                    .debug_selector(move || format!("add-project-host-{debug_key}"))
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .focus_visible(|d| d.border_1().border_color(p.ring))
                    .aria_label(chip_label.clone())
                    .px_3()
                    .py_1p5()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .cursor_pointer()
                    .text_size(ui::TEXT_SM)
                    .when(selected, |d| {
                        d.border_color(p.ring).bg(p.accent).text_color(p.foreground)
                    })
                    .when(!selected, |d| {
                        d.border_color(p.input)
                            .bg(p.background)
                            .text_color(p.muted_foreground)
                    })
                    .child(label.clone())
                    .on_click(cx.listener(move |d, _, window, cx| {
                        d.set_host(next.clone(), window, cx);
                    }))
            })
            .collect::<Vec<_>>();
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(ui::field_label(text::LOCATION.get(), None, p))
                .child(div().flex().flex_wrap().gap_2().children(chips)),
        )
    }
}

/// 아이콘 + 제목 + 설명 한 줄(Orca `AddRepoPrimaryStartAction`/`AddRepoSecondaryStartAction`).
/// OWE-852: 스크린 리더가 버튼으로 읽도록 `Role::Button`을 단다.
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
        .role(gpui::Role::Button)
        .tab_index(0)
        .focus_visible(|d| d.border_1().border_color(p.ring))
        .aria_label(title)
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

/// OWE-852: 키보드 선택 위치를 카드 테두리·배경으로 드러낸다(선택 카드만 링+강조).
fn selected_card(
    base: gpui::Stateful<gpui::Div>,
    p: &ui::Palette,
    is_selected: bool,
) -> gpui::Stateful<gpui::Div> {
    if is_selected {
        base.rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(p.ring)
            .bg(p.accent)
    } else {
        base.rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(p.input)
            .bg(p.background)
    }
}

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let title = match self.step {
            Step::Start => text::TITLE.get(),
            Step::Clone => text::CLONE.get(),
            Step::RemotePath(RemoteMode::Browse) => text::BROWSE.get(),
            Step::RemotePath(RemoteMode::Create) => text::CREATE.get(),
        };
        let host_selector = self.host_selector(p, cx);
        let body = div()
            .id("add-project-body")
            .debug_selector(|| "add-project-body".into())
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3();
        let body = match self.step {
            Step::Start => {
                let selected = self.selected;
                body.children(host_selector)
                    .child(selected_card(
                        action(
                            p,
                            "add-project-browse",
                            IconName::FolderOpen,
                            text::BROWSE.get(),
                            text::BROWSE_DESC.get(),
                        )
                        .child(ui::keycap(p, "↵"))
                        .on_click(cx.listener(|d, _, window, cx| d.browse_activated(window, cx))),
                        p,
                        selected == 0,
                    ))
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(p.muted_foreground)
                            .child(text::OTHER.get()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(selected_card(
                                action(
                                    p,
                                    "add-project-clone",
                                    IconName::Globe,
                                    text::CLONE.get(),
                                    text::CLONE_DESC.get(),
                                )
                                .on_click(cx.listener(|d, _, window, cx| d.show_clone(window, cx))),
                                p,
                                selected == 1,
                            ))
                            .child(selected_card(
                                action(
                                    p,
                                    "add-project-create",
                                    IconName::Plus,
                                    text::CREATE.get(),
                                    text::CREATE_DESC.get(),
                                )
                                .on_click(cx.listener(
                                    |d, _, window, cx| {
                                        d.create_activated(window, cx);
                                    },
                                )),
                                p,
                                selected == 2,
                            )),
                    )
            }
            Step::Clone => {
                let remote = self.host.is_remote();
                body.children(host_selector)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::URL.get(), None, p))
                            .child(Input::new(&self.url)),
                    )
                    .when(remote, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::REMOTE_DEST.get(), None, p))
                                .child(Input::new(&self.dest)),
                        )
                    })
                    .children(self.error.map(|e| ui::error_banner(p, e)))
            }
            Step::RemotePath(_) => body
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(ui::field_label(text::REMOTE_PATH.get(), None, p))
                        .child(Input::new(&self.remote_path)),
                )
                .children(self.error.map(|e| ui::error_banner(p, e))),
        };
        let back = || {
            Button::new("add-project-back")
                .ghost()
                .label(text::BACK.get())
                .on_click(cx.listener(|d, _, window, cx| d.show_start(window, cx)))
        };
        let footer = || {
            div()
                .id("add-project-actions")
                .debug_selector(|| "add-project-actions".into())
                .flex_none()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
        };
        let actions = match self.step {
            Step::Start => None,
            Step::Clone => Some(
                footer().child(back()).child(
                    Button::new("add-project-clone-submit")
                        .primary()
                        .label(if self.host.is_remote() {
                            text::CLONE_SUBMIT_REMOTE.get()
                        } else {
                            text::CLONE_SUBMIT.get()
                        })
                        .debug_selector(|| "add-project-clone-submit".into())
                        .on_click(cx.listener(|d, _, _, cx| d.submit_clone(cx))),
                ),
            ),
            Step::RemotePath(mode) => Some(
                footer().child(back()).child(
                    Button::new("add-project-remote-submit")
                        .primary()
                        .label(match mode {
                            RemoteMode::Browse => text::OPEN_SUBMIT.get(),
                            RemoteMode::Create => text::CREATE_SUBMIT.get(),
                        })
                        .on_click(cx.listener(|d, _, _, cx| d.submit_remote_path(cx))),
                ),
            ),
        };
        let card = div()
            .id("add-project-dialog")
            .debug_selector(|| "add-project-dialog".into())
            .track_focus(&self.focus)
            .occlude()
            .w(ui::DIALOG_WIDTH)
            .max_w((window.viewport_size().width - ui::SPACE_4 * 2.).max(gpui::px(0.)))
            .max_h((window.viewport_size().height - ui::SPACE_4 * 2.).max(gpui::px(0.)))
            .role(gpui::Role::Dialog)
            .aria_label(title)
            .p_6()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_MD)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .capture_key_down(cx.listener(|d, e: &KeyDownEvent, window, cx| {
                d.on_key(e, window, cx);
            }))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Event::Close)))
            .child(
                div()
                    .flex_none()
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
                            .small()
                            .icon(gpui_component::IconName::Close)
                            .accessibility_label(text::CLOSE.get())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                    ),
            )
            .child(body)
            .children(actions);
        ui::modal_backdrop(
            window.viewport_size(),
            ui::modal_focus_scope(card, &self.focus),
        )
    }
}

#[cfg(test)]
#[path = "add_project/tests.rs"]
mod tests;
