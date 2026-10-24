//! OWE-946 / MK-243: 데스크톱 `[Chat | Term]` 하이브리드 뷰 확장.
//!
//! 활성 세션의 PTY 연결(`Live`)을 끊지 않은 채 `Request::AgentHistory(Action::ChatPoll)`로
//! 정규화된 `ChatItem` 카드 스트림·인터랙티브 질문 카드·권한 승인 카드·하단 Live Activity Strip을
//! 렌더링하고 1-클릭으로 `[Chat | Term]` 양방향 전환을 제공한다.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, Subscription, Task, Window, div,
};
use gpui_component::ActiveTheme as _;
use gpui_component::Disableable as _;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use krill_app::i18n::catalog::chat as ctext;
use krill_app::ui;
use krill_proto::SessionId;
use krill_proto::agent_history::{Action, ChatItem, ChatItemKind, ChatPollSnapshot, Response};
use krill_proto::control::{Reply, Request};
use krill_proto::screen::Cell;

use super::Shell;

mod actions;
use actions::{RequestIdentity, Submission};
#[cfg(test)]
mod action_tests;

#[derive(Default)]
pub(crate) struct SessionChatLog {
    pub agent: String,
    pub offset: u64,
    pub bound: bool,
    pub base_index: usize,
    pub items: Vec<Arc<ChatItem>>,
    pub revision: u64,
    pub answered: BTreeSet<String>,
    binding: (String, String),
    host: Option<krill_app::hosts::HostId>,
    epoch: u64,
    submissions: BTreeMap<String, Submission>,
    pub multi_selected: BTreeMap<String, BTreeSet<usize>>,
}

struct ChatList {
    base_index: usize,
    count: usize,
    revision: u64,
    state: gpui::ListState,
}

#[derive(Default)]
pub(crate) struct ChatState {
    pub active_sessions: BTreeSet<SessionId>,
    pub logs: BTreeMap<SessionId, SessionChatLog>,
    pub inputs: BTreeMap<SessionId, (gpui::Entity<InputState>, Subscription)>,
    pub last_poll: Option<std::time::Instant>,
    pub task: Option<Task<()>>,
    pub poll_generation: u64,
    #[cfg(test)]
    pub next_poll_io: Option<tokio::sync::oneshot::Receiver<()>>,
    #[cfg(test)]
    pub poll_call: Option<krill_app::agent_history::Call>,
    request_tasks: BTreeMap<SessionId, Task<()>>,
    prompt_failed: BTreeSet<SessionId>,
    #[cfg(test)]
    send_input: Option<Box<dyn FnMut(Vec<u8>) -> bool>>,
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct QuestionOptionRow {
    cursor: bool,
    checked: Option<bool>,
}

fn strip_option_number(text: &str) -> Option<&str> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let suffix = text.get(digits..)?;
    [". ", ") ", ": "]
        .iter()
        .find_map(|prefix| suffix.strip_prefix(prefix))
        .map(str::trim_start)
}

fn strip_option_checkbox(text: &str) -> Option<(&str, bool)> {
    [
        ("[✓]", true),
        ("[x]", true),
        ("☑", true),
        ("[ ]", false),
        ("☐", false),
    ]
    .into_iter()
    .find_map(|(prefix, checked)| {
        text.strip_prefix(prefix)
            .map(|rest| (rest.trim_start(), checked))
    })
}

fn option_label_matches(text: &str, label: &str) -> bool {
    text.strip_prefix(label).is_some_and(|suffix| {
        suffix.is_empty()
            || [" — ", " – ", " - ", ": "]
                .iter()
                .any(|sep| suffix.starts_with(sep))
            || (suffix.starts_with(" (") && suffix.ends_with(')'))
    })
}

/// Match whole labels after known TUI decorations, with optional delimited
/// descriptions. A row must identify one option, and an option at most one row.
/// Keep both numbered and literal interpretations so ambiguous labels fail shut.
fn question_option_rows(
    rows: &[Vec<Cell>],
    labels: &[String],
) -> Option<Vec<Option<QuestionOptionRow>>> {
    let labels: Vec<_> = labels.iter().map(|label| normalized(label)).collect();
    let unique: BTreeSet<_> = labels.iter().collect();
    if labels.is_empty() || labels.iter().any(String::is_empty) || unique.len() != labels.len() {
        return None;
    }
    let mut matched = vec![None; labels.len()];
    for row in rows {
        let line = normalized(&row.iter().map(|cell| cell.ch).collect::<String>());
        let body = line.strip_prefix(['❯', '>', '›', '●', '*', '→']);
        let cursor = body.is_some();
        let body = body.unwrap_or(&line).trim_start();
        let mut variants = vec![(body, None)];
        if let Some(numbered) = strip_option_number(body) {
            variants.push((numbered, None));
        }
        for index in 0..variants.len() {
            if let Some((text, checked)) = strip_option_checkbox(variants[index].0) {
                variants.push((text, Some(checked)));
                if let Some(numbered) = strip_option_number(text) {
                    variants.push((numbered, Some(checked)));
                }
            }
        }
        let mut candidate = None;
        for (index, label) in labels.iter().enumerate() {
            for &(text, checked) in &variants {
                if option_label_matches(text, label) {
                    let found = (index, QuestionOptionRow { cursor, checked });
                    if candidate.is_some_and(|previous| previous != found) {
                        return None;
                    }
                    candidate = Some(found);
                }
            }
        }
        if let Some((index, row)) = candidate {
            if matched[index].is_some() {
                return None;
            }
            matched[index] = Some(row);
        }
    }
    Some(matched)
}

/// Return the unique option with a visible TUI cursor; ambiguous prompts require Term.
pub(crate) fn detect_question_cursor_index(
    rows: &[Vec<Cell>],
    options: &[String],
) -> Option<usize> {
    let matched = question_option_rows(rows, options)?;
    let mut cursors = matched
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.is_some_and(|row| row.cursor).then_some(index));
    let cursor = cursors.next()?;
    cursors.next().is_none().then_some(cursor)
}

fn step_cursor(steps: &mut Vec<Vec<u8>>, cursor: &mut usize, target: usize) {
    while *cursor < target {
        steps.push(b"\x1b[B".to_vec());
        *cursor += 1;
    }
    while *cursor > target {
        steps.push(b"\x1b[A".to_vec());
        *cursor -= 1;
    }
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
        self.diff
            .update(cx, krill_app::diff::Panel::clear_selection);
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
        let Some(pane) = self.layout.focused() else {
            return;
        };
        let Some(session) = self.active_session_id() else {
            return;
        };
        self.poll_session_chat(pane, session, cx);
    }

    fn poll_session_chat(
        &mut self,
        pane: super::PaneId,
        session: SessionId,
        cx: &mut Context<Self>,
    ) {
        if self.chat.task.is_some() || !self.chat.active_sessions.contains(&session) {
            return;
        }
        let host = self.layout.host(pane).clone();
        let Some(target) = self.target_for_host(&host) else {
            return;
        };
        let call = krill_app::agent_history::daemon_call_for(Some(target));
        #[cfg(test)]
        let call = self.chat.poll_call.clone().unwrap_or(call);
        #[cfg(test)]
        let gate = self.chat.next_poll_io.take();
        self.chat.last_poll = Some(std::time::Instant::now());
        self.chat.poll_generation = self.chat.poll_generation.wrapping_add(1);
        let generation = self.chat.poll_generation;
        let offset = self.chat.logs.get(&session).map_or(0, |log| log.offset);
        self.chat.task = Some(cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    if let Some(gate) = gate {
                        gate.await.ok();
                    }
                    call(&Request::AgentHistory(Action::ChatPoll { session, offset }))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.chat.poll_generation != generation {
                    return;
                }
                this.chat.task = None;
                if this.layout.host(pane) == &host
                    && this.layout.session(pane) == Some(session)
                    && this.chat.active_sessions.contains(&session)
                    && this.chat.logs.get(&session).map_or(0, |log| log.offset) == offset
                    && let Ok(Reply::AgentHistory(Response::ChatPolled(snapshot))) = reply
                {
                    this.apply_chat_poll_for_host(session, host, snapshot);
                    cx.notify();
                }
                if this
                    .active_session_id()
                    .is_some_and(|active| active != session)
                {
                    this.poll_active_chat(cx);
                }
            })
            .ok();
        }));
    }

    #[cfg(test)]
    pub(super) fn apply_chat_poll(&mut self, session: SessionId, snap: ChatPollSnapshot) {
        let host = self
            .layout
            .visible()
            .into_iter()
            .find(|&pane| self.layout.session(pane) == Some(session))
            .map(|pane| self.layout.host(pane).clone());
        let Some(host) = host else {
            return;
        };
        self.apply_chat_poll_for_host(session, host, snap);
    }

    fn apply_chat_poll_for_host(
        &mut self,
        session: SessionId,
        host: krill_app::hosts::HostId,
        snap: ChatPollSnapshot,
    ) {
        let host = Some(host);
        let log = self.chat.logs.entry(session).or_default();
        let binding = (snap.session_id.clone(), snap.transcript_path.clone());
        let reset = snap.reset || log.binding != binding || log.host != host || !snap.bound;
        if !snap.agent.is_empty() {
            log.agent = snap.agent;
        }
        log.bound = snap.bound;
        if reset || !snap.items.is_empty() {
            log.revision = log.revision.wrapping_add(1);
        }
        if reset {
            log.epoch = log.epoch.wrapping_add(1);
            log.items.clear();
            log.base_index = 0;
            log.answered.clear();
            log.multi_selected.clear();
            log.submissions.clear();
            self.chat.request_tasks.remove(&session);
        }
        log.binding = binding;
        log.host = host;
        if !snap.items.is_empty() {
            log.items.extend(snap.items.into_iter().map(Arc::new));
            if log.items.len() > 250 {
                let excess = log.items.len() - 250;
                log.items.drain(0..excess);
                log.base_index = log.base_index.saturating_add(excess);
                let retained: BTreeSet<_> = log
                    .items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| format!("{}:{}", item.tool_id, log.base_index + index))
                    .collect();
                log.answered.retain(|key| retained.contains(key));
                log.multi_selected.retain(|key, _| retained.contains(key));
                log.submissions.retain(|key, _| retained.contains(key));
            }
        }
        log.offset = snap.next_offset;
        // A matching transcript result is evidence of a response; queue delivery is not.
        let resolved: Vec<_> = log
            .items
            .iter()
            .enumerate()
            .filter(|(index, item)| {
                matches!(item.kind, ChatItemKind::Question | ChatItemKind::Permission)
                    && !item.tool_id.is_empty()
                    && log.items.iter().skip(index + 1).any(|later| {
                        later.kind == ChatItemKind::ToolResult && later.tool_id == item.tool_id
                    })
            })
            .map(|(index, item)| format!("{}:{}", item.tool_id, log.base_index + index))
            .collect();
        for key in resolved {
            log.answered.insert(key.clone());
            log.submissions.insert(key, Submission::Confirmed);
        }
    }

    fn send_to_session(
        &mut self,
        session: SessionId,
        bytes: &[u8],
        cx: &mut Context<Self>,
    ) -> bool {
        let pane = self
            .layout
            .focused()
            .filter(|&pane| self.layout.session(pane) == Some(session))
            .or_else(|| {
                self.layout
                    .visible()
                    .into_iter()
                    .find(|&pane| self.layout.session(pane) == Some(session))
            });
        pane.is_some_and(|pane| self.send_to_chat_pane(pane, session, bytes, cx))
    }

    fn send_to_chat_pane(
        &mut self,
        pane: super::PaneId,
        session: SessionId,
        bytes: &[u8],
        cx: &mut Context<Self>,
    ) -> bool {
        // Pane::input also returns None when disconnected: preflight the exact
        // pane before interpreting its queue result. This is not an agent ACK.
        if self.paste_queue.has_pending_for(pane)
            || self.layout.file(pane).is_some()
            || self.layout.session(pane) != Some(session)
            || self.live.get(&pane).is_none_or(|live| {
                live.target.as_ref().is_none_or(|target| {
                    target.host() != self.layout.host(pane) || !target.is_current()
                }) || live.pane.session() != Some(session)
                    || !live.pane.is_linked()
                    || live.pane.is_closed()
                    || live.pane.is_cold()
                    || live.pane.account_switching()
                    || live.suspended
            })
        {
            return false;
        }
        #[cfg(test)]
        if let Some(send) = self.chat.send_input.as_mut() {
            return send(bytes.to_vec());
        }
        let Some(live) = self.live.get_mut(&pane) else {
            return false;
        };
        live.clear_selection();
        live.echo.key_sent(std::time::Instant::now());
        let notice = live.pane.input(bytes, super::Now::get());
        self.show(notice.as_ref(), cx);
        cx.notify();
        notice.is_none()
    }

    fn ensure_chat_input(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<InputState> {
        if let Some((input, _)) = self.chat.inputs.get(&session) {
            return input.clone();
        }
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder(ctext::PROMPT_PLACEHOLDER.get()));
        let sub = cx.subscribe_in(
            &input,
            window,
            move |this, _state, ev: &InputEvent, window, cx| {
                if matches!(
                    ev,
                    InputEvent::PressEnter {
                        secondary: false,
                        ..
                    }
                ) {
                    this.submit_chat_prompt(session, window, cx);
                }
            },
        );
        self.chat.inputs.insert(session, (input.clone(), sub));
        input
    }

    pub(super) fn submit_chat_prompt(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((input, _)) = self.chat.inputs.get(&session) else {
            return;
        };
        let input = input.clone();
        let text = input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(b'\r');
        if self.send_to_session(session, &bytes, cx) {
            Self::clear_sent_chat_draft(&input, &text, window, cx);
            self.chat.prompt_failed.remove(&session);
        } else {
            self.chat.prompt_failed.insert(session);
        }
        cx.notify();
    }

    fn clear_sent_chat_draft(
        input: &gpui::Entity<InputState>,
        sent: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if input.read(cx).value().as_ref() == sent {
            input.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        }
    }

    fn toggle_chat_multi_option(
        &mut self,
        request: &RequestIdentity,
        opt_idx: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.chat_request_is_current(request, cx) || opt_idx >= request.item.options.len() {
            return;
        }
        let answer_key = request.key.clone();
        let Some(log) = self.chat.logs.get_mut(&request.session) else {
            return;
        };
        if log
            .submissions
            .get(&answer_key)
            .is_some_and(Submission::blocks_input)
        {
            return;
        }
        let set = log.multi_selected.entry(answer_key).or_default();
        if !set.remove(&opt_idx) {
            set.insert(opt_idx);
        }
        cx.notify();
    }

    fn pane_live_status_line(&self, pane: super::PaneId) -> Option<String> {
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

    fn render_chat_item(
        &mut self,
        pane: super::PaneId,
        session: SessionId,
        abs_idx: usize,
        item: &ChatItem,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let panel_bg = theme.muted;
        let border = theme.border;
        let fg = theme.foreground;
        let muted_fg = theme.muted_foreground;
        let warning = theme.warning;
        let role_label = match item.kind {
            ChatItemKind::User => "YOU",
            ChatItemKind::Assistant => "AGENT",
            ChatItemKind::Reasoning => "REASONING",
            ChatItemKind::ToolCall => "TOOL CALL",
            ChatItemKind::ToolResult => "TOOL RESULT",
            ChatItemKind::Question => "QUESTION",
            ChatItemKind::Permission => "PERMISSION",
        };
        let card_border = if matches!(item.kind, ChatItemKind::Question | ChatItemKind::Permission)
        {
            warning
        } else {
            border
        };
        let item_selector = format!("chat-item-{}-{abs_idx}", pane.get());
        let debug_selector = item_selector.clone();
        let mut card = div()
            .id(SharedString::from(item_selector))
            .debug_selector(move || debug_selector.clone())
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
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .text_color(muted_fg)
                            .child(if item.title.is_empty() {
                                SharedString::from(role_label)
                            } else {
                                SharedString::from(format!("{role_label} • {}", item.title))
                            }),
                    )
                    .children((!item.file_path.is_empty()).then(|| {
                        let raw_path = std::path::PathBuf::from(&item.file_path);
                        let path = if raw_path.is_relative() {
                            self.pane_cwd(pane, cx)
                                .map_or_else(|| raw_path.clone(), |cwd| cwd.join(&raw_path))
                        } else {
                            raw_path
                        };
                        Button::new(SharedString::from(format!(
                            "chat-open-file-{}-{abs_idx}",
                            pane.get()
                        )))
                        .label(
                            std::path::Path::new(&item.file_path)
                                .file_name()
                                .map_or_else(
                                    || item.file_path.clone(),
                                    |name| name.to_string_lossy().into_owned(),
                                ),
                        )
                        .max_w(gpui::relative(0.45))
                        .overflow_hidden()
                        .tooltip(item.file_path.clone())
                        .accessibility_label(item.file_path.clone())
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

        let request = self.chat_request_identity(pane, session, abs_idx, item);
        let answer_key = request.key.clone();
        let submission = self
            .chat
            .logs
            .get(&session)
            .and_then(|log| log.submissions.get(&answer_key));
        let current = self.chat_request_is_current(&request, cx);
        let disabled = !current || submission.is_some_and(Submission::blocks_input);
        if matches!(item.kind, ChatItemKind::Question | ChatItemKind::Permission) {
            let status = if !current
                && !matches!(
                    submission,
                    Some(Submission::Confirmed | Submission::ReviewRequired)
                ) {
                ctext::REQUEST_UNVERIFIED.get()
            } else {
                submission.map_or(ctext::REQUEST_READY.get(), Submission::label)
            };
            card = card.child(
                div()
                    .id(SharedString::from(format!(
                        "chat-request-status-{}-{abs_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-request-status".into())
                    .text_size(ui::TEXT_XS)
                    .text_color(muted_fg)
                    .child(status),
            );
        }
        if item.kind == ChatItemKind::Question && !item.options.is_empty() {
            let multi_select = item.multi_select;
            let selected_set = self
                .chat
                .logs
                .get(&session)
                .and_then(|l| l.multi_selected.get(&answer_key))
                .cloned()
                .unwrap_or_default();
            let mut opts_row = div().flex().flex_wrap().gap_1p5();
            for (opt_idx, opt) in item.options.iter().enumerate() {
                let request_clone = request.clone();
                let is_checked = selected_set.contains(&opt_idx);
                let prefix = if multi_select {
                    if is_checked { "[✓] " } else { "[ ] " }
                } else if is_checked {
                    "✓ "
                } else {
                    ""
                };
                let label = if opt.description.is_empty() {
                    format!("{prefix}{}", opt.label)
                } else {
                    format!("{prefix}{} — {}", opt.label, opt.description)
                };
                opts_row = opts_row.child(
                    Button::new(SharedString::from(format!(
                        "chat-q-{}-{abs_idx}-{opt_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-question-option".into())
                    .label(label)
                    .small()
                    .ghost()
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if multi_select {
                            this.toggle_chat_multi_option(&request_clone, opt_idx, cx);
                        } else {
                            this.answer_chat_question(
                                request_clone.clone(),
                                vec![opt_idx],
                                false,
                                cx,
                            );
                        }
                    })),
                );
            }
            if multi_select {
                let request_submit = request.clone();
                let picks: Vec<usize> = selected_set.into_iter().collect();
                opts_row = opts_row.child(
                    Button::new(SharedString::from(format!(
                        "chat-q-submit-{}-{abs_idx}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-question-submit".into())
                    .label(ctext::SUBMIT.format(&[("n", &picks.len().to_string())]))
                    .small()
                    .primary()
                    .disabled(disabled || picks.is_empty())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.answer_chat_question(request_submit.clone(), picks.clone(), true, cx);
                    })),
                );
            }
            card = card.child(opts_row);
        } else if item.kind == ChatItemKind::Permission {
            let yes_request = request.clone();
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!(
                            "chat-perm-yes-{}-{abs_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-permission-approve".into())
                        .disabled(disabled)
                        .label(ctext::APPROVE.get())
                        .small()
                        .primary()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.approve_chat_permission(yes_request.clone(), true, cx);
                        })),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "chat-perm-no-{}-{abs_idx}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-permission-deny".into())
                        .disabled(disabled)
                        .label(ctext::DENY.get())
                        .small()
                        .ghost()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.approve_chat_permission(request.clone(), false, cx);
                        })),
                    ),
            );
        }

        div().pb_2().child(card).into_any_element()
    }

    pub(super) fn render_pane_chat_overlay(
        &mut self,
        pane: super::PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.layout.file(pane).is_some() {
            return None;
        }
        let session = self.layout.session(pane)?;
        if !self.chat.active_sessions.contains(&session) {
            return None;
        }
        let input_state = self.ensure_chat_input(session, window, cx);
        let theme = cx.theme();
        let bg = theme.background;
        let panel_bg = theme.muted;
        let border = theme.border;
        let muted_fg = theme.muted_foreground;

        let log = self.chat.logs.get(&session);
        let agent_label = log
            .and_then(|l| (!l.agent.is_empty()).then_some(l.agent.as_str()))
            .unwrap_or("agent")
            .to_owned();
        let bound = log.is_some_and(|l| l.bound);
        let base_index = log.map_or(0, |l| l.base_index);
        let revision = log.map_or(0, |l| l.revision);
        let items = log.map_or_else(Vec::new, |l| l.items.clone());

        let mut feed = div()
            .id(SharedString::from(format!(
                "desktop-chat-feed-{}",
                pane.get()
            )))
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .p_3()
            .flex()
            .flex_col()
            .gap_2();

        if items.is_empty() {
            let empty_msg = if bound {
                ctext::EMPTY_BOUND.get()
            } else {
                ctext::EMPTY_UNBOUND.get()
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
            let count = items.len();
            let list_id =
                SharedString::from(format!("desktop-chat-list-{}-{}", pane.get(), session.0));
            let state = window.use_keyed_state(list_id, cx, |_, _| ChatList {
                base_index,
                count,
                revision,
                state: gpui::ListState::new(count, gpui::ListAlignment::Top, ui::SPACE_4),
            });
            let list_state = state.update(cx, |list, _| {
                let removed = base_index.saturating_sub(list.base_index);
                if base_index < list.base_index || removed > list.count {
                    list.state.reset(count);
                } else {
                    if removed != 0 {
                        list.state.splice(0..removed, 0);
                    }
                    let kept = list.count - removed;
                    if count >= kept {
                        list.state.splice(kept..kept, count - kept);
                    } else {
                        list.state.splice(count..kept, 0);
                    }
                    if revision != list.revision {
                        list.state.remeasure();
                    }
                }
                list.base_index = base_index;
                list.count = count;
                list.revision = revision;
                list.state.clone()
            });
            let owner = cx.weak_entity();
            feed = feed.child(
                gpui::list(list_state, move |index, _, cx| {
                    let Some(item) = items.get(index) else {
                        return div().into_any_element();
                    };
                    owner
                        .update(cx, |this, cx| {
                            this.render_chat_item(pane, session, base_index + index, item, cx)
                        })
                        .unwrap_or_else(|_| div().into_any_element())
                })
                .size_full(),
            );
        }

        let composer = div()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(border)
            .bg(bg)
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&input_state).small().cleanable(true)),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "desktop-chat-send-{}",
                    pane.get()
                )))
                .debug_selector(|| "desktop-chat-send".into())
                .label(ctext::SEND.get())
                .small()
                .primary()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.submit_chat_prompt(session, window, cx);
                })),
            );

        let live_line = self.pane_live_status_line(pane);
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
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(strip_label),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .flex_none()
                    .child(
                        Button::new(SharedString::from(format!(
                            "desktop-chat-refresh-{}",
                            pane.get()
                        )))
                        .label(ctext::REFRESH.get())
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.poll_session_chat(pane, session, cx);
                        })),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "desktop-chat-switch-term-{}",
                            pane.get()
                        )))
                        .debug_selector(|| "chat-switch-terminal".into())
                        .label(ctext::TERM.get())
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.chat.active_sessions.remove(&session);
                            cx.notify();
                        })),
                    ),
            );

        Some(
            div()
                .id(SharedString::from(format!(
                    "desktop-chat-overlay-{}",
                    pane.get()
                )))
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
                .children(self.chat.prompt_failed.contains(&session).then(|| {
                    ui::error_banner(ui::palette(cx), ctext::PROMPT_FAILED.get())
                        .id("chat-prompt-error")
                        .debug_selector(|| "chat-prompt-error".into())
                }))
                .child(composer)
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
        assert!(cx.debug_bounds("desktop-chat-send").is_some());

        shell.update(cx, |this, cx| {
            assert!(this.is_active_chat_view());
            assert_eq!(this.chat.logs[&SessionId(9)].items.len(), 2);
            this.toggle_chat_view(cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("desktop-chat-overlay").is_none());
    }

    #[cfg(unix)]
    #[gpui::test]
    fn long_chat_renders_viewport_rows_and_shares_message_text(cx: &mut TestAppContext) {
        use crate::shell::tests::{open, state_path};
        let (shell, cx) = open(cx, state_path("virtual-chat"));
        let pane = shell.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            shell.layout.set_session(pane, SessionId(444));
            shell.chat.active_sessions.insert(SessionId(444));
            shell.apply_chat_poll(
                SessionId(444),
                ChatPollSnapshot {
                    bound: true,
                    reset: true,
                    items: (0..250)
                        .map(|index| ChatItem {
                            kind: ChatItemKind::Assistant,
                            text: format!("message {index}\n{}", "long answer\n".repeat(16)),
                            ..ChatItem::default()
                        })
                        .collect(),
                    ..ChatPollSnapshot::default()
                },
            );
            let messages = shell.chat.logs[&SessionId(444)].items.clone();
            assert!(Arc::ptr_eq(
                &messages[0],
                &shell.chat.logs[&SessionId(444)].items[0]
            ));
            cx.notify();
            pane
        });
        cx.run_until_parked();
        // The GPUI test selector API requires static strings; these two
        // fixture-only selectors live for the test process.
        let first = Box::leak(format!("chat-item-{}-0", pane.get()).into_boxed_str());
        let last = Box::leak(format!("chat-item-{}-249", pane.get()).into_boxed_str());
        assert!(cx.debug_bounds(first).is_some());
        assert!(cx.debug_bounds(last).is_none());
        assert!(cx.debug_bounds("desktop-chat-send").is_some());
    }

    #[test]
    fn base_index_preserves_answer_key_after_250_item_drain() {
        let mut log = SessionChatLog::default();
        let items: Vec<ChatItem> = (0..260)
            .map(|i| ChatItem {
                kind: ChatItemKind::Question,
                tool_id: format!("tool-{i}"),
                ..ChatItem::default()
            })
            .collect();
        log.items.extend(items.into_iter().map(Arc::new));
        let excess = log.items.len() - 250;
        log.items.drain(0..excess);
        log.base_index = log.base_index.saturating_add(excess);

        // Item originally at index 200 is now at relative index 190,
        // and base_index (10) + 190 == 200 preserves the original answer_key.
        let rel_idx = 190;
        let abs_idx = log.base_index + rel_idx;
        assert_eq!(abs_idx, 200);
        assert_eq!(log.items[rel_idx].tool_id, "tool-200");
    }

    #[test]
    fn detect_question_cursor_and_bidirectional_steps() {
        let make_row = |s: &str| -> Vec<Cell> {
            s.chars()
                .map(|ch| Cell {
                    ch,
                    ..Cell::default()
                })
                .collect()
        };
        let rows = vec![make_row("  Fast"), make_row("❯ Safe"), make_row("  Custom")];
        let options = vec!["Fast".to_owned(), "Safe".to_owned(), "Custom".to_owned()];
        assert_eq!(detect_question_cursor_index(&rows, &options), Some(1));

        let mut cursor = 1;
        let mut steps = Vec::new();
        step_cursor(&mut steps, &mut cursor, 0);
        assert_eq!(cursor, 0);
        assert_eq!(steps, vec![b"\x1b[A".to_vec()]);
    }
    #[cfg(unix)]
    #[gpui::test]
    fn slow_chat_poll_is_coalesced_and_rejects_changed_host(cx: &mut gpui::TestAppContext) {
        use crate::shell::tests::{open, state_path};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let (view, cx) = open(cx, state_path("async-chat-poll"));
        let calls = Arc::new(AtomicUsize::new(0));
        let (release, gate) = tokio::sync::oneshot::channel();
        view.update(cx, |shell, cx| {
            let pane = shell.layout.active_tab().focused;
            shell.layout.set_session(pane, SessionId(321));
            shell.paths = Some(krill_proto::daemon::Paths {
                socket: "/unused".into(),
                holder: "/unused".into(),
                krilld: "/unused".into(),
            });
            shell.chat.active_sessions.insert(SessionId(321));
            let recorded = calls.clone();
            shell.chat.poll_call = Some(Arc::new(move |_| {
                recorded.fetch_add(1, Ordering::SeqCst);
                Ok(Reply::AgentHistory(Response::ChatPolled(
                    ChatPollSnapshot {
                        next_offset: 7,
                        bound: true,
                        ..Default::default()
                    },
                )))
            }));
            shell.chat.next_poll_io = Some(gate);
            for _ in 0..20 {
                shell.poll_active_chat(cx);
            }
        });
        cx.run_until_parked();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        view.update(cx, |shell, cx| {
            for _ in 0..20 {
                shell.poll_active_chat(cx);
            }
            let pane = shell.layout.active_tab().focused;
            shell
                .layout
                .set_host(pane, krill_app::hosts::HostId::Remote("changed".into()));
        });
        release.send(()).unwrap();
        cx.run_until_parked();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        view.read_with(cx, |shell, _| {
            assert!(shell.chat.task.is_none());
            assert!(!shell.chat.logs.contains_key(&SessionId(321)));
        });
    }
}
