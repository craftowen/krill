// UI interaction patterns referenced from Orca native-chat (MIT).
// Copyright (c) 2026 Lovecast Inc.
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
use gpui_component::Disableable as _;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{InputEvent, Textarea, TextareaState};
use krill_app::i18n::catalog::chat as ctext;
use krill_app::ui;
use krill_proto::SessionId;
use krill_proto::agent_history::{Action, ChatItem, ChatItemKind, ChatPollSnapshot, Response};
use krill_proto::control::{Reply, Request};
use krill_proto::screen::Cell;

use super::{PaneId, Shell};

mod actions;
mod body;
mod composer;
mod prompt;
#[cfg(test)]
mod prompt_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod ui_tests;
mod view;
use actions::{RequestIdentity, Submission};
#[cfg(test)]
mod action_tests;
#[cfg(test)]
mod poll_tests;

#[derive(Default)]
pub(crate) struct SessionChatLog {
    pub agent: String,
    pub offset: u64,
    pub bound: bool,
    pub base_index: usize,
    pub items: Vec<Arc<ChatItem>>,
    pub revision: u64,
    expanded: BTreeSet<usize>,
    pub answered: BTreeSet<String>,
    binding: (String, String),
    host: Option<krill_app::hosts::HostId>,
    epoch: u64,
    submissions: BTreeMap<String, Submission>,
    pub multi_selected: BTreeMap<String, BTreeSet<usize>>,
}

struct ChatList {
    host: krill_app::hosts::HostId,
    base_index: usize,
    count: usize,
    revision: u64,
    epoch: u64,
    state: gpui::ListState,
}

impl ChatList {
    fn sync(
        &mut self,
        base_index: usize,
        count: usize,
        revision: u64,
        epoch: u64,
    ) -> gpui::ListState {
        let removed = base_index.saturating_sub(self.base_index);
        if epoch != self.epoch || base_index < self.base_index || removed > self.count {
            self.state.reset(count);
            self.state.set_follow_mode(gpui::FollowMode::Tail);
        } else {
            if removed != 0 {
                self.state.splice(0..removed, 0);
            }
            let kept = self.count - removed;
            if count >= kept {
                self.state.splice(kept..kept, count - kept);
            } else {
                self.state.splice(count..kept, 0);
            }
            if revision != self.revision {
                self.state.remeasure();
            }
        }
        self.base_index = base_index;
        self.count = count;
        self.revision = revision;
        self.epoch = epoch;
        self.state.clone()
    }
}

enum ChatFocus {
    Composer(SessionId),
    Terminal(SessionId),
}

#[derive(Default)]
pub(crate) struct ChatState {
    poll_timer: Option<Task<()>>,
    poll_failed: BTreeSet<SessionId>,
    lists: BTreeMap<(PaneId, SessionId), ChatList>,
    focus_pending: Option<ChatFocus>,
    pub logs: BTreeMap<SessionId, SessionChatLog>,
    pub inputs: BTreeMap<SessionId, (gpui::Entity<TextareaState>, Subscription)>,
    pub last_poll: Option<std::time::Instant>,
    pub task: Option<Task<()>>,
    pub poll_generation: u64,
    #[cfg(test)]
    pub next_poll_io: Option<tokio::sync::oneshot::Receiver<()>>,
    #[cfg(test)]
    pub poll_call: Option<krill_app::agent_history::Call>,
    request_tasks: BTreeMap<SessionId, Task<()>>,
    prompt_failed: BTreeSet<SessionId>,
    prompt_tasks: BTreeMap<SessionId, Task<()>>,
    prompt_review: BTreeSet<SessionId>,
    multiline_blocked: BTreeSet<SessionId>,
    #[cfg(test)]
    send_input: Option<Box<dyn FnMut(Vec<u8>) -> bool>>,
    #[cfg(test)]
    prompt_delivery: Option<krill_proto::input::Delivery>,
    #[cfg(test)]
    prompt_confirmed: Option<bool>,
}

impl ChatState {
    pub(super) fn review_pending_prompt(&mut self, session: SessionId) {
        if self.prompt_tasks.contains_key(&session) {
            self.prompt_review.insert(session);
        }
    }
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

use body::{ChatBodySegment, parse_chat_segments};

impl Shell {
    pub(super) fn is_session_chat_view(&self, session: SessionId, cx: &gpui::App) -> bool {
        let pane = self
            .layout
            .focused()
            .filter(|pane| self.layout.session(*pane) == Some(session));
        pane.is_some_and(|pane| self.is_pane_chat_view(pane, cx))
    }

    fn is_pane_chat_view(&self, pane: PaneId, cx: &gpui::App) -> bool {
        if self.layout.session(pane).is_none() {
            return false;
        }
        if let Some(chat) = self.layout.chat_view(pane) {
            return chat;
        }
        krill_app::settings::default_view_mode(cx) == krill_app::settings::DefaultViewMode::Chat
    }

    pub(super) fn is_active_chat_view_in(&self, cx: &gpui::App) -> bool {
        self.active_session_id()
            .is_some_and(|id| self.is_session_chat_view(id, cx))
    }

    /// 현재 포커스된 칸의 세션이 명시적 Chat 뷰 모드인지 반환한다(테스트/내부 호환용).
    #[cfg(test)]
    pub(super) fn is_active_chat_view(&self) -> bool {
        self.layout
            .focused()
            .and_then(|pane| self.layout.chat_view(pane))
            .unwrap_or(false)
    }

    pub(super) fn active_session_id(&self) -> Option<SessionId> {
        let pane = self.layout.focused()?;
        if self.layout.file(pane).is_some() {
            return None;
        }
        self.layout.session(pane)
    }

    // Term and background tabs keep their lists. Only a closed pane or a
    // reassigned terminal invalidates its cached reader anchor/follow intent.
    fn prune_chat_lists(&mut self) {
        let layout = &self.layout;
        self.chat.lists.retain(|(pane, session), list| {
            layout.pane_exists(*pane)
                && layout.file(*pane).is_none()
                && layout.session(*pane) == Some(*session)
                && layout.host(*pane) == &list.host
        });
    }

    fn set_pane_chat_view(&mut self, pane: PaneId, chat: bool, cx: &mut Context<Self>) {
        if self.layout.session(pane).is_none() {
            return;
        }
        self.layout.set_chat_view(pane, chat);
        self.save();
        cx.notify();
    }

    /// `[Chat | Term]` 모드를 토글한다(OWE-946 / MK-243).
    pub(super) fn toggle_chat_view(&mut self, cx: &mut Context<Self>) {
        self.diff
            .update(cx, krill_app::diff::Panel::clear_selection);
        let Some(session) = self.active_session_id() else {
            return;
        };
        let chat = !self.is_session_chat_view(session, cx);
        if let Some(pane) = self.layout.focused() {
            self.set_pane_chat_view(pane, chat, cx);
        }
        self.chat.focus_pending = Some(if chat {
            ChatFocus::Composer(session)
        } else {
            ChatFocus::Terminal(session)
        });
        if chat {
            self.chat.last_poll = None;
            self.poll_active_chat(cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_chat_view_with_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_chat_view(cx);
        if let Some(session) = self
            .active_session_id()
            .filter(|id| self.is_session_chat_view(*id, cx))
        {
            let input = self.ensure_chat_input(session, window, cx);
            input.update(cx, |input, cx| input.focus(window, cx));
        } else {
            window.focus(&self.focus, cx);
        }
    }

    pub(super) fn maybe_poll_active_chat(&mut self, cx: &mut Context<Self>) {
        self.prune_chat_lists();
        if !self.is_active_chat_view_in(cx) {
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

    // Transcript flushes and hook binding can happen after the final PTY redraw.
    // Keep retrying while Chat is visible, with at most one request in flight.
    fn ensure_chat_poll_timer(&mut self, cx: &mut Context<Self>) {
        if self.chat.poll_timer.is_some() {
            return;
        }
        self.chat.poll_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(800))
                    .await;
                let keep_polling = this
                    .update(cx, |this, cx| {
                        if this.page_open()
                            || this.dashboard.is_some()
                            || !this.is_active_chat_view_in(cx)
                        {
                            this.chat.poll_timer = None;
                            return false;
                        }
                        this.poll_active_chat(cx);
                        true
                    })
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        }));
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
        if self.chat.task.is_some() || !self.is_pane_chat_view(pane, cx) {
            return;
        }
        self.ensure_chat_poll_timer(cx);
        let host = self.layout.host(pane).clone();
        let Some(target) = self.target_for_host(&host) else {
            if self.chat.poll_failed.insert(session) {
                cx.notify();
            }
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
                    && this.is_pane_chat_view(pane, cx)
                    && this.chat.logs.get(&session).map_or(0, |log| log.offset) == offset
                {
                    if let Ok(Reply::AgentHistory(Response::ChatPolled(snapshot))) = reply {
                        this.chat.poll_failed.remove(&session);
                        this.apply_chat_poll_for_host(session, host, snapshot);
                    } else {
                        this.chat.poll_failed.insert(session);
                    }
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

    pub(super) fn apply_chat_poll_for_host(
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
            log.expanded.clear();
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
                log.expanded.retain(|index| *index >= log.base_index);
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
        if self.chat.prompt_tasks.contains_key(&session) {
            return false;
        }
        self.send_chat_prompt_input(pane, session, bytes, cx)
    }

    fn send_chat_prompt_input(
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
    ) -> gpui::Entity<TextareaState> {
        if let Some((input, _)) = self.chat.inputs.get(&session) {
            let input = input.clone();
            let placeholder = ctext::PROMPT_PLACEHOLDER.get();
            if input.read(cx).presentation().placeholder().as_ref() != placeholder {
                input.update(cx, |state, cx| {
                    state.set_placeholder(placeholder, window, cx);
                });
            }
            return input;
        }
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder(ctext::PROMPT_PLACEHOLDER.get())
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            move |_this, _state, ev: &InputEvent, _window, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            },
        );
        self.chat.inputs.insert(session, (input.clone(), sub));
        if self.active_session_id() == Some(session) {
            input.update(cx, |state, cx| state.focus(window, cx));
        }
        input
    }

    pub(super) fn submit_chat_prompt(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chat.prompt_tasks.contains_key(&session)
            || self.chat.prompt_review.contains(&session)
        {
            return;
        }
        let Some((input, _)) = self.chat.inputs.get(&session) else {
            return;
        };
        let input = input.clone();
        if input.update(cx, |state, cx| {
            gpui::EntityInputHandler::marked_text_range(state, window, cx).is_some()
        }) {
            return;
        }
        let text = input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        let Some(pane) = self
            .layout
            .focused()
            .filter(|&pane| self.layout.session(pane) == Some(session))
            .or_else(|| {
                self.layout
                    .visible()
                    .into_iter()
                    .find(|&pane| self.layout.session(pane) == Some(session))
            })
        else {
            return;
        };
        let modes = self
            .live
            .get(&pane)
            .map_or_else(Default::default, |live| live.pane.modes());
        // Use the terminal consumer's paste plan, then submit separately.
        // Unknown unbracketed consumers retain the multiline draft for review.
        let krill_app::security::PastePlan::Immediate(mut bytes) =
            self.plan_terminal_paste(pane, text.as_bytes(), modes, cx)
        else {
            self.chat.multiline_blocked.insert(session);
            cx.notify();
            return;
        };
        self.chat.multiline_blocked.remove(&session);
        if modes.contains(krill_proto::term_modes::Modes::BRACKETED_PASTE)
            || text.contains(['\n', '\r'])
        {
            self.start_chat_prompt(
                prompt::ChatPrompt {
                    pane,
                    session,
                    input,
                    text,
                    bytes,
                },
                window,
                cx,
            );
            return;
        }
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
        input: &gpui::Entity<TextareaState>,
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
}
