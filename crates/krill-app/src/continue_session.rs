//! OWE-736 (`[MK-182]`): 새 세션에 요약 이어하기(`Continue in New Session`) 모달 대화상자.
//!
//! 터미널/탭 우클릭 메뉴에서 현재 세션의 최근 터미널 맥락을 대상 에이전트(동일 에이전트 또는
//! Claude↔Codex 등 교차 핸드오프)와 추가 지시사항(프롬프트)과 함께 새 에이전트 탭으로 전달한다.

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, KeyDownEvent, SharedString, Window, div, prelude::*,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use krill_proto::SessionId;

use crate::i18n::catalog::continue_session as text;
use crate::ui;

/// 추가 지시사항을 비웠을 때 쓰는 기본 핸드오프 지시문(`krill handoff`와 동일).
pub const DEFAULT_PROMPT: &str = "Continue work from the previous agent session.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Submit {
        source: SessionId,
        agent: String,
        prompt: String,
    },
    Close,
}

pub struct Dialog {
    source: SessionId,
    agents: Vec<(String, String)>,
    selected_agent: String,
    prompt: Entity<InputState>,
    focus: FocusHandle,
}

impl EventEmitter<Event> for Dialog {}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dialog")
            .field("source", &self.source)
            .field("selected_agent", &self.selected_agent)
            .finish_non_exhaustive()
    }
}

/// `Continue in New Session` 핸드오프 브리프 문자열을 조립한다.
#[must_use]
pub fn format_handoff_brief(
    source: SessionId,
    target_agent: &str,
    prompt: &str,
    context: &str,
) -> String {
    let trimmed = prompt.trim();
    let instruction = if trimmed.is_empty() {
        DEFAULT_PROMPT
    } else {
        trimmed
    };
    let body = if context.trim().is_empty() {
        "(empty)"
    } else {
        context.trim_end()
    };
    format!(
        "[MK SESSION HANDOFF]\nSource session: s{}\nTarget agent: {target_agent}\nInstructions: {instruction}\n--- Recent terminal context ---\n{body}\n--- End context ---\n",
        source.0
    )
}

fn default_agents() -> Vec<(String, String)> {
    vec![
        ("claude".to_owned(), "Claude Code".to_owned()),
        ("codex".to_owned(), "Codex".to_owned()),
    ]
}

impl Dialog {
    pub fn new(
        source: SessionId,
        agents: Vec<(String, String)>,
        default_agent: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let agents = if agents.is_empty() {
            default_agents()
        } else {
            agents
        };
        let selected_agent = default_agent
            .and_then(|want| {
                agents
                    .iter()
                    .find(|(id, _)| id == want)
                    .map(|(id, _)| id.clone())
            })
            .or_else(|| agents.first().map(|(id, _)| id.clone()))
            .unwrap_or_else(|| "claude".to_owned());
        let prompt =
            cx.new(|cx| InputState::new(window, cx).placeholder(text::PROMPT_PLACEHOLDER.get()));
        let focus = cx.focus_handle();
        prompt.update(cx, |input, cx| input.focus(window, cx));
        Self {
            source,
            agents,
            selected_agent,
            prompt,
            focus,
        }
    }

    #[must_use]
    pub fn selected_agent(&self) -> &str {
        &self.selected_agent
    }

    pub fn select_agent(&mut self, agent: impl Into<String>, cx: &mut Context<Self>) {
        let agent = agent.into();
        if self.agents.iter().any(|(id, _)| id == &agent) {
            self.selected_agent = agent;
            cx.notify();
        }
    }

    pub fn set_prompt(
        &mut self,
        value: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value: SharedString = value.into();
        self.prompt.update(cx, |input, cx| {
            input.set_value(value, window, cx);
        });
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        let prompt = self.prompt.read(cx).value().trim().to_owned();
        cx.emit(Event::Submit {
            source: self.source,
            agent: self.selected_agent.clone(),
            prompt,
        });
    }

    fn on_key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        match e.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                cx.emit(Event::Close);
            }
            "enter" => {
                cx.stop_propagation();
                self.submit(cx);
            }
            _ => {}
        }
    }
}

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let subtitle = text::SUBTITLE.format(&[("id", &self.source.0.to_string())]);
        let selected = self.selected_agent.clone();
        let agent_choices = self
            .agents
            .iter()
            .enumerate()
            .map(|(idx, (id, name))| {
                let is_selected = *id == selected;
                let agent_id = id.clone();
                let selector = format!("continue-agent-{agent_id}");
                let label = SharedString::from(name.clone());
                div()
                    .id(("continue-agent", idx))
                    .debug_selector(move || selector.clone())
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(if is_selected { p.ring } else { p.border })
                    .bg(if is_selected { p.accent } else { p.card })
                    .cursor_pointer()
                    .hover(|d| d.bg(p.accent))
                    .child(ui::icons::agent_brand_icon(id, ui::ICON_SM, p.foreground))
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .font_weight(if is_selected {
                                gpui::FontWeight::SEMIBOLD
                            } else {
                                gpui::FontWeight::MEDIUM
                            })
                            .text_color(p.foreground)
                            .child(label),
                    )
                    .on_click(cx.listener(move |d, _, _, cx| {
                        d.select_agent(agent_id.clone(), cx);
                    }))
            })
            .collect::<Vec<_>>();

        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(SharedString::from(subtitle)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(ui::field_label(text::TARGET_AGENT.get(), None, p))
                    .child(div().flex().flex_wrap().gap_2().children(agent_choices)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(ui::field_label(
                        text::PROMPT_LABEL.get(),
                        Some(text::OPTIONAL.get()),
                        p,
                    ))
                    .child(Input::new(&self.prompt)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("continue-session-cancel")
                            .ghost()
                            .label(text::CANCEL.get())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                    )
                    .child(
                        Button::new("continue-session-submit")
                            .primary()
                            .label(text::SUBMIT.get())
                            .debug_selector(|| "continue-session-submit".into())
                            .on_click(cx.listener(|d, _, _, cx| d.submit(cx))),
                    ),
            );

        let card = div()
            .id("continue-session-dialog")
            .debug_selector(|| "continue-session-dialog".into())
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
                            .child(text::TITLE.get()),
                    )
                    .child(
                        Button::new("continue-session-close")
                            .ghost()
                            .small()
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
#[path = "continue_session/tests.rs"]
mod tests;
