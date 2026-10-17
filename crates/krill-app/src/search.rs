//! MK-059 터미널 검색 바(⌘F): 검색어 입력, 이전(↑)·다음(Enter·↓) 순회, 일치 수 표시, Esc 닫기.

use std::time::Duration;

use gpui::{
    Context, Entity, EventEmitter, SharedString, Subscription, Task, Window, deferred, div,
    prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Enter, Escape, Input, InputState, MoveDown, MoveUp};
use krill_proto::search::{Direction, SearchResult};

const DEBOUNCE: Duration = Duration::from_millis(150);

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
            status: SharedString::default(),
            result: None,
            input,
            debounce: None,
            _observe: observe,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
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
        self.status = SharedString::from(status_text(result));
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
            .w(px(360.))
            .p_1()
            .flex()
            .items_center()
            .gap_1()
            .bg(bg)
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
            .capture_action(cx.listener(|bar, _: &Enter, _, cx| {
                cx.stop_propagation();
                bar.step(Direction::Next, cx);
            }))
            .capture_action(cx.listener(|bar, _: &Escape, _, cx| {
                cx.stop_propagation();
                bar.dismiss(cx);
            }))
            .child(div().flex_1().min_w_0().child(Input::new(&self.input)))
            .when(!self.status.is_empty(), |d| {
                d.child(
                    div()
                        .debug_selector(|| "search-status".into())
                        .px_1()
                        .font_family(crate::ui::FONT_UI)
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(muted)
                        .child(self.status.clone()),
                )
            })
            .child(
                Button::new("search-prev")
                    .label("↑")
                    .ghost()
                    .accessibility_label(text::PREV.get())
                    .tooltip(text::PREV.get())
                    .on_click(cx.listener(|bar, _, _, cx| {
                        bar.step(Direction::Previous, cx);
                    })),
            )
            .child(
                Button::new("search-next")
                    .label("↓")
                    .ghost()
                    .accessibility_label(text::NEXT.get())
                    .tooltip(text::NEXT.get())
                    .on_click(cx.listener(|bar, _, _, cx| {
                        bar.step(Direction::Next, cx);
                    })),
            )
            .child(
                Button::new("search-close")
                    .label("×")
                    .ghost()
                    .accessibility_label(text::CLOSE.get())
                    .tooltip(text::CLOSE.get())
                    .on_click(cx.listener(|bar, _, _, cx| {
                        bar.dismiss(cx);
                    })),
            );
        deferred(
            div()
                .absolute()
                .top(crate::ui::TITLEBAR_HEIGHT + crate::ui::TAB_BAR_HEIGHT + crate::ui::SPACE_2)
                .right(px(crate::ui::PANEL_DEFAULT_WIDTH) + crate::ui::SPACE_4)
                .child(bar),
        )
    }
}

#[cfg(test)]
#[path = "search/tests.rs"]
mod tests;
