//! OWE-946 / MK-243: 데스크톱 `[Chat | Term]` 하이브리드 뷰 확장.
//!
//! 활성 세션의 PTY 연결(`Live`)을 끊지 않은 채 `Request::AgentHistory(Action::ChatPoll)`로
//! 정규화된 `ChatItem` 카드 스트림·인터랙티브 질문 카드·권한 승인 카드·하단 Live Activity Strip을
//! 렌더링하고 1-클릭으로 `[Chat | Term]` 양방향 전환을 제공한다.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, div,
};
use gpui_component::ActiveTheme as _;
use gpui_component::Disableable as _;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use krill_app::i18n::catalog::shell as text;
use krill_app::ui;
use krill_proto::SessionId;
use krill_proto::agent_history::{Action, ChatItem, ChatItemKind, ChatPollSnapshot, Response};
use krill_proto::control::{Reply, Request};
use krill_term::ime::Output;

use super::Shell;

#[derive(Default)]
pub(crate) struct SessionChatLog {
    pub agent: String,
    pub offset: u64,
    pub bound: bool,
    pub items: Vec<ChatItem>,
    pub answered: BTreeSet<String>,
    pub multi_selected: BTreeMap<String, BTreeSet<usize>>,
}

#[derive(Default)]
pub(crate) struct ChatState {
    pub active_sessions: BTreeSet<SessionId>,
    pub logs: BTreeMap<SessionId, SessionChatLog>,
    pub last_poll: Option<std::time::Instant>,
    pub task: Option<Task<()>>,
    pub keystroke_task: Option<Task<()>>,
}

impl Shell {
    /// 현재 포커스된 칸의 세션이 Chat 뷰 모드인지 반환한다.
    pub(super) fn is_active_chat_view(&self) -> bool {
        self.active_session_id()
            .is_some_and(|id| self.chat.active_sessions.contains(&id))
    }

    pub(super) fn active_session_id(&self) -> Option<SessionId> {
        let pane = self.layout.focused()?;
        if self.layout.file(pane).is_some() {
            return None;
        }
        self.layout.session(pane)
    }

    /// `[Chat | Term]` 모드를 토글한다(OWE-946 / MK-243).
    pub(super) fn toggle_chat_view(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.active_session_id() else {
            return;
        };
        if !self.chat.active_sessions.remove(&session) {
            self.chat.active_sessions.insert(session);
            self.chat.last_poll = None;
            self.poll_active_chat(cx);
        }
        cx.notify();
    }

    pub(super) fn maybe_poll_active_chat(&mut self, cx: &mut Context<Self>) {
        if !self.is_active_chat_view() {
            return;
        }
        let now = std::time::Instant::now();
        if self
            .chat
            .last_poll
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(800))
        {
            return;
        }
        self.poll_active_chat(cx);
    }

    /// 데몬에 `Action::ChatPoll`을 요청해 활성 세션의 증분 트랜스크립트를 가져온다.
    pub(super) fn poll_active_chat(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.active_session_id() else {
            return;
        };
        if !self.chat.active_sessions.contains(&session) {
            return;
        }
        self.chat.last_poll = Some(std::time::Instant::now());
        let offset = self.chat.logs.get(&session).map_or(0, |l| l.offset);
        let pane = self.layout.focused();
        let host = pane.map_or(&krill_app::hosts::HostId::Local, |p| self.layout.host(p));
        let target = self.target_for_host(host);
        let call = krill_app::agent_history::daemon_call_for(target);
        self.chat.task = Some(cx.spawn(async move |this, cx| {
            let reply =
                cx.background_executor()
                    .spawn(async move {
                        call(&Request::AgentHistory(Action::ChatPoll { session, offset }))
                    })
                    .await;
            this.update(cx, |this, cx| {
                if let Ok(Reply::AgentHistory(Response::ChatPolled(snap))) = reply {
                    this.apply_chat_poll(session, snap);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    pub(super) fn apply_chat_poll(&mut self, session: SessionId, snap: ChatPollSnapshot) {
        let log = self.chat.logs.entry(session).or_default();
        if !snap.agent.is_empty() {
            log.agent = snap.agent;
        }
        log.bound = snap.bound;
        if snap.reset {
            log.items.clear();
        }
        if !snap.items.is_empty() {
            log.items.extend(snap.items);
            if log.items.len() > 250 {
                let excess = log.items.len() - 250;
                log.items.drain(0..excess);
            }
        }
        log.offset = snap.next_offset;
    }

    fn send_to_session(
        &mut self,
        session: SessionId,
        bytes: Vec<u8>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.active_session_id() == Some(session) {
            return self.send(
                &Output {
                    pty: bytes,
                    discard_ime: false,
                },
                None,
                cx,
            );
        }
        let Some(pane) =
            self.layout.visible().into_iter().find(|&p| {
                self.layout.file(p).is_none() && self.layout.session(p) == Some(session)
            })
        else {
            return false;
        };
        let Some(l) = self.live.get_mut(&pane) else {
            return false;
        };
        l.clear_selection();
        l.echo.key_sent(std::time::Instant::now());
        let n = l.pane.input(&bytes, super::Now::get());
        self.show(n.as_ref(), cx);
        cx.notify();
        n.is_none()
    }

    pub(super) fn toggle_chat_multi_option(
        &mut self,
        session: SessionId,
        answer_key: String,
        opt_idx: usize,
        cx: &mut Context<Self>,
    ) {
        let log = self.chat.logs.entry(session).or_default();
        let set = log.multi_selected.entry(answer_key).or_default();
        if !set.remove(&opt_idx) {
            set.insert(opt_idx);
        }
        cx.notify();
    }

    /// 인터랙티브 질문 카드 선택지를 60ms 간격 키스트로크 시퀀스로 해당 세션 PTY에 주입한다(OWE-946).
    pub(super) fn answer_chat_question(
        &mut self,
        session: SessionId,
        answer_key: String,
        selected: Vec<usize>,
        multi_select: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(log) = self.chat.logs.get_mut(&session) {
            log.answered.insert(answer_key);
        }
        let mut steps: Vec<Vec<u8>> = Vec::new();
        if multi_select {
            let mut sorted: Vec<usize> = selected.into_iter().filter(|&idx| idx < 16).collect();
            sorted.sort_unstable();
            sorted.dedup();
            let mut cursor = 0usize;
            for target in sorted {
                while cursor < target {
                    steps.push(b"\x1b[B".to_vec());
                    cursor += 1;
                }
                steps.push(b" ".to_vec());
            }
            steps.push(b"\r".to_vec());
        } else if let Some(&choice_idx) = selected.first() {
            for _ in 0..choice_idx.min(16) {
                steps.push(b"\x1b[B".to_vec());
            }
            steps.push(b"\r".to_vec());
        } else {
            return;
        }
        self.chat.keystroke_task = Some(cx.spawn(async move |this, cx| {
            for (idx, bytes) in steps.into_iter().enumerate() {
                if idx > 0 {
                    cx.background_executor()
                        .timer(Duration::from_millis(60))
                        .await;
                }
                let ok = this
                    .update(cx, |this, cx| this.send_to_session(session, bytes, cx))
                    .unwrap_or(false);
                if !ok {
                    break;
                }
            }
        }));
        cx.notify();
    }

    pub(super) fn approve_chat_permission(
        &mut self,
        session: SessionId,
        yes: bool,
        cx: &mut Context<Self>,
    ) {
        let bytes = if yes {
            b"y\r".to_vec()
        } else {
            b"n\r".to_vec()
        };
        self.send_to_session(session, bytes, cx);
        cx.notify();
    }

    fn active_live_status_line(&self) -> Option<String> {
        let pane = self.layout.focused()?;
        let live = self.live.get(&pane)?;
        let screen = live.pane.screen();
        for row in screen.rows.iter().rev() {
            let line: String = row.iter().map(|c| c.ch).collect();
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.chars().take(120).collect());
            }
        }
        None
    }

    pub(super) fn render_chat_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.active_session_id()?;
        if !self.chat.active_sessions.contains(&session) {
            return None;
        }
        let theme = cx.theme();
        let bg = theme.background;
        let panel_bg = theme.muted;
        let border = theme.border;
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let warning = theme.warning;

        let log = self.chat.logs.get(&session);
        let agent_label = log
            .and_then(|l| (!l.agent.is_empty()).then_some(l.agent.as_str()))
            .unwrap_or("agent");
        let bound = log.is_some_and(|l| l.bound);
        let items = log.map_or(&[][..], |l| l.items.as_slice());

        let mut feed = div()
            .id("desktop-chat-feed")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_2();

        if items.is_empty() {
            let empty_msg = if bound {
                text::CHAT_EMPTY.get()
            } else {
                text::CHAT_UNBOUND.get()
            };
            feed = feed.child(
                div()
                    .p_4()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(border)
                    .bg(panel_bg)
                    .text_size(ui::TEXT_SM)
                    .text_color(muted_fg)
                    .child(empty_msg),
            );
        } else {
            for (idx, item) in items.iter().enumerate() {
                let role_label = match item.kind {
                    ChatItemKind::User => "YOU",
                    ChatItemKind::Assistant => "AGENT",
                    ChatItemKind::Reasoning => "REASONING",
                    ChatItemKind::ToolCall => "TOOL CALL",
                    ChatItemKind::ToolResult => "TOOL RESULT",
                    ChatItemKind::Question => "QUESTION",
                    ChatItemKind::Permission => "PERMISSION",
                };
                let card_border =
                    if matches!(item.kind, ChatItemKind::Question | ChatItemKind::Permission) {
                        warning
                    } else {
                        border
                    };
                let mut card = div()
                    .id(SharedString::from(format!("chat-item-{idx}")))
                    .p_3()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(card_border)
                    .bg(panel_bg)
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_size(ui::TEXT_XS).text_color(muted_fg).child(
                                if item.title.is_empty() {
                                    SharedString::from(role_label)
                                } else {
                                    SharedString::from(format!("{role_label} • {}", item.title))
                                },
                            ))
                            .children((!item.file_path.is_empty()).then(|| {
                                let path = std::path::PathBuf::from(&item.file_path);
                                Button::new(SharedString::from(format!("chat-open-file-{idx}")))
                                    .label(item.file_path.clone())
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_file_at(path.clone(), 1, 1, cx);
                                    }))
                            })),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(fg)
                            .child(item.text.clone()),
                    );

                if item.kind == ChatItemKind::Question && !item.options.is_empty() {
                    let answer_key = format!("{}:{idx}", item.tool_id);
                    let answered = log.is_some_and(|l| l.answered.contains(&answer_key));
                    let multi_select = item.multi_select;
                    let selected_set = log
                        .and_then(|l| l.multi_selected.get(&answer_key))
                        .cloned()
                        .unwrap_or_default();
                    let mut opts_row = div().flex().flex_wrap().gap_1p5();
                    for (opt_idx, opt) in item.options.iter().enumerate() {
                        let key_clone = answer_key.clone();
                        let is_checked = selected_set.contains(&opt_idx);
                        let prefix = if multi_select {
                            if is_checked { "[✓] " } else { "[ ] " }
                        } else {
                            ""
                        };
                        let label = if opt.description.is_empty() {
                            format!("{prefix}{}", opt.label)
                        } else {
                            format!("{prefix}{} — {}", opt.label, opt.description)
                        };
                        opts_row = opts_row.child(
                            Button::new(SharedString::from(format!("chat-q-{idx}-{opt_idx}")))
                                .label(label)
                                .small()
                                .ghost()
                                .disabled(answered)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if multi_select {
                                        this.toggle_chat_multi_option(
                                            session,
                                            key_clone.clone(),
                                            opt_idx,
                                            cx,
                                        );
                                    } else {
                                        this.answer_chat_question(
                                            session,
                                            key_clone.clone(),
                                            vec![opt_idx],
                                            false,
                                            cx,
                                        );
                                    }
                                })),
                        );
                    }
                    if multi_select {
                        let key_submit = answer_key.clone();
                        let picks: Vec<usize> = selected_set.into_iter().collect();
                        opts_row = opts_row.child(
                            Button::new(SharedString::from(format!("chat-q-submit-{idx}")))
                                .label(text::CHAT_SUBMIT.format(&[("n", &picks.len().to_string())]))
                                .small()
                                .primary()
                                .disabled(answered || picks.is_empty())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.answer_chat_question(
                                        session,
                                        key_submit.clone(),
                                        picks.clone(),
                                        true,
                                        cx,
                                    );
                                })),
                        );
                    }
                    card = card.child(opts_row);
                } else if item.kind == ChatItemKind::Permission {
                    card = card.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new(SharedString::from(format!("chat-perm-yes-{idx}")))
                                    .label(text::CHAT_APPROVE.get())
                                    .small()
                                    .primary()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.approve_chat_permission(session, true, cx);
                                    })),
                            )
                            .child(
                                Button::new(SharedString::from(format!("chat-perm-no-{idx}")))
                                    .label(text::CHAT_DENY.get())
                                    .small()
                                    .ghost()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.approve_chat_permission(session, false, cx);
                                    })),
                            ),
                    );
                }

                feed = feed.child(card);
            }
        }

        let live_line = self.active_live_status_line();
        let strip_label = match live_line {
            Some(line) => format!("Hybrid Chat • {agent_label} (#{}) — {line}", session.0),
            None => format!("Hybrid Chat • {agent_label} (Session #{})", session.0),
        };

        let strip = div()
            .px_3()
            .py_1p5()
            .border_t_1()
            .border_color(border)
            .bg(panel_bg)
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(muted_fg)
                    .overflow_hidden()
                    .child(strip_label),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        Button::new("desktop-chat-refresh")
                            .label(text::CHAT_REFRESH.get())
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.poll_active_chat(cx);
                            })),
                    )
                    .child(
                        Button::new("desktop-chat-switch-term")
                            .label(text::CHAT_TERM.get())
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_chat_view(cx);
                            })),
                    ),
            );

        Some(
            div()
                .id("desktop-chat-overlay")
                .debug_selector(|| "desktop-chat-overlay".into())
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .occlude()
                .flex()
                .flex_col()
                .bg(bg)
                .child(feed)
                .child(strip)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use krill_proto::agent_history::ChatQuestionOption;

    #[gpui::test]
    fn desktop_hybrid_chat_toggle_and_incremental_poll_render(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(None, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-chat-overlay").is_none());

        shell.update(cx, |this, cx| {
            let pane = this.layout.focused().unwrap();
            this.layout.set_session(pane, SessionId(9));
            this.toggle_chat_view(cx);
            this.apply_chat_poll(
                SessionId(9),
                ChatPollSnapshot {
                    agent: "claude".into(),
                    session_id: "sid-9".into(),
                    transcript_path: "/tmp/sid-9.jsonl".into(),
                    next_offset: 512,
                    reset: false,
                    bound: true,
                    items: vec![
                        ChatItem {
                            kind: ChatItemKind::User,
                            text: "안녕".into(),
                            ..ChatItem::default()
                        },
                        ChatItem {
                            kind: ChatItemKind::Question,
                            title: "Mode".into(),
                            text: "Choose one".into(),
                            options: vec![ChatQuestionOption {
                                label: "Fast".into(),
                                description: String::new(),
                            }],
                            ..ChatItem::default()
                        },
                    ],
                },
            );
            cx.notify();
        });
        cx.run_until_parked();
        let bounds = cx
            .debug_bounds("desktop-chat-overlay")
            .expect("overlay should be rendered");
        assert!(bounds.size.width > gpui::px(200.0));
        assert!(bounds.size.height > gpui::px(100.0));

        shell.update(cx, |this, cx| {
            assert!(this.is_active_chat_view());
            assert_eq!(this.chat.logs[&SessionId(9)].items.len(), 2);
            this.toggle_chat_view(cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-chat-overlay").is_none());
    }
}
