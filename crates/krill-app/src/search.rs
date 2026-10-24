//! MK-059 터미널 검색 바(⌘F): 검색어 입력, 이전(↑·⇧Enter)·다음(Enter·↓) 순회, 일치 수 표시, Esc 닫기.

use std::time::Duration;

use gpui::{
    Context, Entity, EventEmitter, Focusable as _, SharedString, Subscription, Task, Window,
    deferred, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Enter, Escape, Input, InputState, MoveDown, MoveUp};
use gpui_component::{IconName, Selectable as _, Sizable as _};
use krill_proto::search::{Direction, SearchResult};

const DEBOUNCE: Duration = Duration::from_millis(150);

/// 검색 바 키 문맥(OWE-868). ⇧⌘G가 바 안에서만 이전 찾기로 동작하고, 바깥에서는 우측 패널 전환을 유지한다.
pub const CONTEXT: &str = "TerminalSearch";

use crate::i18n::catalog::search as text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Query(String),
    Step(Direction),
    Dismiss,
}

/// 검색 결과 요약 문구. 일치 항목이 있으면 `1/N`, 없으면 `"0건"`, 아직 응답이 없으면 빈 문자열.
/// 이력이 바뀌어 처음부터 시작하면 `" 오래됨"`, 끝에서 순환하면 `" 순환"`을 붙인다.
/// AS-15: 데몬이 `u64::MAX`를 보내도 panic 없이 포화한다.
pub fn status_text(result: Option<&SearchResult>) -> String {
    let Some(r) = result else {
        return String::new();
    };
    let base = match &r.hit {
        Some(hit) => format!("{}/{}", hit.index.saturating_add(1), r.total),
        None => text::EMPTY.get().to_owned(),
    };
    if r.stale {
        format!("{} {}", base, text::STALE.get())
    } else if r.wrapped {
        format!("{} {}", base, text::WRAPPED.get())
    } else {
        base
    }
}

pub struct Bar {
    language: crate::i18n::Language,
    query: String,
    options: krill_proto::search::Options,
    options_supported: bool,
    status: SharedString,
    result: Option<SearchResult>,
    input: Entity<InputState>,
    debounce: Option<Task<()>>,
    _observe: Subscription,
}

impl EventEmitter<Event> for Bar {}

impl std::fmt::Debug for Bar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bar")
            .field("query", &self.query)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl Bar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(text::PLACEHOLDER.get()));
        let observe = cx.observe(&input, |bar, input, cx| {
            let value = input.read(cx).value();
            if *value != bar.query {
                bar.query = value.to_string();
                bar.debounce = None;
                if bar.query.is_empty() {
                    bar.status = SharedString::default();
                    bar.result = None;
                    cx.emit(Event::Query(String::new()));
                } else {
                    let timer = cx.background_executor().timer(DEBOUNCE);
                    bar.debounce = Some(cx.spawn(async move |this, cx| {
                        timer.await;
                        this.update(cx, |bar, cx| cx.emit(Event::Query(bar.query.clone())))
                            .ok();
                    }));
                }
                cx.notify();
            }
        });
        Self {
            language: crate::i18n::language(),
            query: String::new(),
            options: krill_proto::search::Options::default(),
            options_supported: false,
            status: SharedString::default(),
            result: None,
            input,
            debounce: None,
            _observe: observe,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    pub fn is_focused(&self, window: &Window, cx: &gpui::App) -> bool {
        self.input.read(cx).focus_handle(cx).is_focused(window)
    }

    pub fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value(query, window, cx));
    }

    pub fn options(&self) -> krill_proto::search::Options {
        self.options
    }

    pub fn set_options_supported(&mut self, supported: bool) {
        self.options_supported = supported;
    }

    fn toggle_option(&mut self, regex: bool, cx: &mut Context<Self>) {
        if regex {
            self.options.regex = !self.options.regex;
        } else {
            self.options.case_sensitive = !self.options.case_sensitive;
        }
        self.result = None;
        self.status = SharedString::default();
        if self.options.regex && self.options.compile(&self.query).is_err() {
            self.status = text::INVALID_REGEX.get().into();
        }
        cx.emit(Event::Query(self.query.clone()));
        cx.notify();
    }

    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn set_result(&mut self, result: Option<&SearchResult>, cx: &mut Context<Self>) {
        self.result = result.cloned();
        self.status = if self.options.regex && self.options.compile(&self.query).is_err() {
            text::INVALID_REGEX.get().into()
        } else {
            SharedString::from(status_text(result))
        };
        cx.notify();
    }

    fn step(&mut self, direction: Direction, cx: &mut Context<Self>) {
        self.debounce = None;
        cx.emit(Event::Step(direction));
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.debounce = None;
        cx.emit(Event::Dismiss);
    }
}

impl Render for Bar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.language != crate::i18n::language() {
            self.language = crate::i18n::language();
            self.input.update(cx, |input, cx| {
                input.set_placeholder(text::PLACEHOLDER.get(), window, cx);
            });
            self.status = status_text(self.result.as_ref()).into();
        }
        let pal = crate::ui::palette(cx);
        let (bg, fg, border, muted) = (
            pal.popover,
            pal.popover_foreground,
            pal.border,
            pal.muted_foreground,
        );
        let bar = div()
            .id("search-bar")
            .key_context(CONTEXT)
            .debug_selector(|| "search-bar".into())
            .w(px(360.))
            .p_1()
            .flex()
            .items_center()
            .gap_1()
            .bg(bg)
            .font_family(crate::ui::FONT_UI)
            .text_size(crate::ui::TEXT_SM)
            .text_color(fg)
            .border_1()
            .border_color(border)
            .rounded(crate::ui::RADIUS_MD)
            .shadow_lg()
            .capture_action(cx.listener(|bar, _: &MoveUp, _, cx| {
                cx.stop_propagation();
                bar.step(Direction::Previous, cx);
            }))
            .capture_action(cx.listener(|bar, _: &MoveDown, _, cx| {
                cx.stop_propagation();
                bar.step(Direction::Next, cx);
            }))
            .capture_action(cx.listener(|bar, _: &Enter, window, cx| {
                cx.stop_propagation();
                // OWE-868: ⇧Enter는 이전 찾기(표준 단축키). Enter·↓는 다음, ↑는 이전이다.
                if window.modifiers().shift {
                    bar.step(Direction::Previous, cx);
                } else {
                    bar.step(Direction::Next, cx);
                }
            }))
            .capture_action(cx.listener(|bar, _: &Escape, _, cx| {
                cx.stop_propagation();
                bar.dismiss(cx);
            }))
            .child(div().flex_1().min_w_0().child(Input::new(&self.input)))
            .when(self.options_supported, |bar| {
                bar.child(
                    Button::new("search-case-sensitive")
                        .ghost()
                        .xsmall()
                        .label(text::CASE_LABEL.get())
                        .selected(self.options.case_sensitive)
                        .accessibility_label(text::CASE_SENSITIVE.get())
                        .on_click(cx.listener(|bar, _, _, cx| bar.toggle_option(false, cx))),
                )
                .child(
                    Button::new("search-regex")
                        .ghost()
                        .xsmall()
                        .label(text::REGEX_LABEL.get())
                        .selected(self.options.regex)
                        .accessibility_label(text::REGEX.get())
                        .on_click(cx.listener(|bar, _, _, cx| bar.toggle_option(true, cx))),
                )
            })
            .when(!self.status.is_empty(), |d| {
                d.child(
                    div()
                        .debug_selector(|| "search-status".into())
                        .px_1()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(muted)
                        .child(self.status.clone()),
                )
            })
            .child(
                crate::ui::button_with_tooltip(
                    Button::new("search-prev")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ChevronUp),
                    "search-prev",
                    text::PREV.get(),
                )
                .accessibility_label(text::PREV.get())
                .on_click(cx.listener(|bar, _, _, cx| {
                    bar.step(Direction::Previous, cx);
                })),
            )
            .child(
                crate::ui::button_with_tooltip(
                    Button::new("search-next")
                        .ghost()
                        .xsmall()
                        .icon(IconName::ChevronDown),
                    "search-next",
                    text::NEXT.get(),
                )
                .accessibility_label(text::NEXT.get())
                .on_click(cx.listener(|bar, _, _, cx| {
                    bar.step(Direction::Next, cx);
                })),
            )
            .child(
                crate::ui::button_with_tooltip(
                    Button::new("search-close")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close),
                    "search-close",
                    text::CLOSE.get(),
                )
                .accessibility_label(text::CLOSE.get())
                .on_click(cx.listener(|bar, _, _, cx| {
                    bar.dismiss(cx);
                })),
            );
        deferred(
            div()
                .absolute()
                .top(crate::ui::SPACE_2)
                .right(crate::ui::SPACE_4)
                .child(bar),
        )
    }
}

#[cfg(test)]
#[path = "search/tests.rs"]
mod tests;
