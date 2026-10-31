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

/// 채팅 상태의 키. 세션 ID는 데몬마다 따로 세므로 로컬과 원격 세션 번호가 겹친다.
pub(super) type ChatKey = (krill_app::hosts::HostId, SessionId);

#[cfg(test)]
pub(super) fn local_key(session: SessionId) -> ChatKey {
    (krill_app::hosts::HostId::Local, session)
}

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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ComposerModelItem {
    pub id: String,
    pub label: String,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ComposerMeta {
    pub model: Option<String>,
    pub model_label: Option<String>,
    pub current_model_id: Option<String>,
    pub effort: Option<String>,
    pub context_percent: Option<u8>,
    pub models: Vec<ComposerModelItem>,
    pub efforts: Vec<String>,
    pub supported: bool,
    pub effort_supported: bool,
    pub reason: Option<String>,
    pub pending: bool,
}

#[derive(Default)]
pub(crate) struct SessionChatLog {
    pub agent: String,
    pub offset: u64,
    pub bound: bool,
    pub base_index: usize,
    pub items: Vec<Arc<ChatItem>>,
    pub revision: u64,
    pub meta: ComposerMeta,
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
    Composer(ChatKey),
    Terminal(ChatKey),
}

#[derive(Default)]
pub(crate) struct ChatState {
    poll_timer: Option<Task<()>>,
    poll_failed: BTreeSet<ChatKey>,
    lists: BTreeMap<(PaneId, SessionId), ChatList>,
    focus_pending: Option<ChatFocus>,
    pub logs: BTreeMap<ChatKey, SessionChatLog>,
    pub inputs: BTreeMap<ChatKey, (gpui::Entity<TextareaState>, Subscription)>,
    pub last_poll: Option<std::time::Instant>,
    pub task: Option<Task<()>>,
    pub poll_generation: u64,
    pub copied_item: Option<(ChatKey, usize, std::time::Instant)>,
    pub quick_menu: BTreeSet<ChatKey>,
    pub model_picker: BTreeSet<ChatKey>,
    composer_tasks: BTreeMap<ChatKey, Task<()>>,
    last_composer_poll: BTreeMap<ChatKey, std::time::Instant>,
    #[cfg(test)]
    pub next_poll_io: Option<tokio::sync::oneshot::Receiver<()>>,
    #[cfg(test)]
    pub poll_call: Option<krill_app::agent_history::Call>,
    #[cfg(test)]
    pub workflow_call: Option<krill_app::agent_history::WorkflowCall>,
    request_tasks: BTreeMap<ChatKey, Task<()>>,
    prompt_failed: BTreeSet<ChatKey>,
    prompt_tasks: BTreeMap<ChatKey, Task<()>>,
    prompt_review: BTreeSet<ChatKey>,
    multiline_blocked: BTreeSet<ChatKey>,
    #[cfg(test)]
    send_input: Option<Box<dyn FnMut(Vec<u8>) -> bool>>,
    #[cfg(test)]
    prompt_delivery: Option<krill_proto::input::Delivery>,
    #[cfg(test)]
    prompt_confirmed: Option<bool>,
}

impl ChatState {
    pub(super) fn review_pending_prompt(&mut self, key: &ChatKey) {
        if self.prompt_tasks.contains_key(key) {
            self.prompt_review.insert(key.clone());
        }
    }
}

fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_valid_token_string(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:/@[]".contains(&b))
}

fn ratio_percent(used: u64, limit: u64) -> Option<u8> {
    if limit == 0 {
        return None;
    }
    let pct = ((u128::from(used) * 100 + u128::from(limit) / 2) / u128::from(limit)).min(100);
    u8::try_from(pct).ok()
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0.0..=100.0"
)]
fn float_percent(percent: f64) -> Option<u8> {
    (percent.is_finite() && (0.0..=100.0).contains(&percent))
        .then(|| percent.round().clamp(0.0, 100.0) as u8)
}

pub(crate) fn parse_transcript_record(record: &serde_json::Value, meta: &mut ComposerMeta) {
    let payload = record.get("payload").unwrap_or(record);
    let message = record.get("message").unwrap_or(record);

    if record.get("type").and_then(serde_json::Value::as_str) == Some("turn_context") {
        if let Some(model) = payload
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|v| is_valid_token_string(v))
        {
            meta.model = Some(model.to_owned());
        }
        for field in ["effort", "reasoning_effort"] {
            if let Some(effort) = payload
                .get(field)
                .and_then(serde_json::Value::as_str)
                .filter(|v| is_valid_token_string(v))
            {
                meta.effort = Some(effort.to_owned());
            }
        }
    }

    if let Some(model) = message
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|v| is_valid_token_string(v) && !v.starts_with('<'))
    {
        meta.model = Some(model.to_owned());
    }

    if let Some(info) = payload.get("info") {
        let used = info
            .pointer("/last_token_usage/total_tokens")
            .and_then(serde_json::Value::as_u64);
        let limit = info
            .get("model_context_window")
            .and_then(serde_json::Value::as_u64);
        if let (Some(used), Some(limit)) = (used, limit)
            && let Some(pct) = ratio_percent(used, limit)
        {
            meta.context_percent = Some(pct);
        }
    }

    if let Some(usage) = message.get("usage") {
        let input = usage
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let output = usage
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let cache_create = usage
            .get("cache_creation_input_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let total = input + output + cache_read + cache_create;
        if total > 0
            && let Some(pct) = ratio_percent(total, 200_000)
        {
            meta.context_percent = Some(pct);
        }
    }
}

pub(crate) fn observe_composer_meta(
    state_dir: Option<&std::path::Path>,
    session_id: &str,
    transcript_path: &str,
) -> ComposerMeta {
    let mut meta = ComposerMeta::default();
    if !transcript_path.is_empty()
        && let Ok(mut file) = std::fs::File::open(transcript_path)
    {
        use std::io::{Read, Seek, SeekFrom};
        if let Ok(len) = file.metadata().map(|m| m.len()) {
            let tail_bytes = 64 * 1024;
            let start = len.saturating_sub(tail_bytes);
            if file.seek(SeekFrom::Start(start)).is_ok() {
                let mut buf = Vec::new();
                if file.take(tail_bytes).read_to_end(&mut buf).is_ok() {
                    let text = String::from_utf8_lossy(&buf);
                    for line in text.lines().skip(usize::from(start > 0)) {
                        if let Ok(record) = serde_json::from_str::<serde_json::Value>(line) {
                            parse_transcript_record(&record, &mut meta);
                        }
                    }
                }
            }
        }
    }

    if let Some(state_dir) = state_dir {
        if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
            let claude_profile = home.join(".claude");
            if let Some(obs) =
                krill_proto::statusline::context::read(state_dir, &claude_profile, session_id)
            {
                if let Some(model) = obs.model {
                    meta.model = Some(model);
                }
                if let Some(effort) = obs.effort {
                    meta.effort = Some(effort);
                }
                if let Some(percent) = obs.percent.and_then(float_percent) {
                    meta.context_percent = Some(percent);
                }
            }
        }
        let context_dir = state_dir.join("session-context");
        if (meta.model.is_none() || meta.context_percent.is_none())
            && context_dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(&context_dir)
        {
            for entry in entries.flatten() {
                let file_path = entry.path().join(format!("{session_id}.json"));
                if file_path.is_file()
                    && let Ok(content) = std::fs::read_to_string(&file_path)
                    && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
                {
                    if meta.model.is_none()
                        && let Some(m) = json.get("model").and_then(serde_json::Value::as_str)
                    {
                        meta.model = Some(m.to_owned());
                    }
                    if meta.effort.is_none()
                        && let Some(e) = json.get("effort").and_then(serde_json::Value::as_str)
                    {
                        meta.effort = Some(e.to_owned());
                    }
                    if meta.context_percent.is_none()
                        && let Some(p) = json
                            .get("percent")
                            .and_then(serde_json::Value::as_f64)
                            .and_then(float_percent)
                    {
                        meta.context_percent = Some(p);
                    }
                }
            }
        }
    }
    meta
}

pub(crate) fn parse_composer_items(items: &[serde_json::Value]) -> Vec<ComposerModelItem> {
    items
        .iter()
        .filter_map(|item| {
            let id = item
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())?;
            let label = item
                .get("label")
                .or_else(|| item.get("name"))
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(id);
            let efforts = item
                .get("efforts")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect();
            let default_effort = item
                .get("default_effort")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            Some(ComposerModelItem {
                id: id.to_owned(),
                label: label.to_owned(),
                efforts,
                default_effort,
            })
        })
        .collect()
}

pub(crate) fn apply_composer_status_json(raw: &str, meta: &mut ComposerMeta) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(raw).ok()?;
    if root.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let data = root.get("data")?;
    let agent = data
        .get("agent")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if let Some(model) = data
        .get("model")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        meta.model = Some(model.to_owned());
    }
    if let Some(label) = data
        .get("model_label")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        meta.model_label = Some(label.to_owned());
    }
    if let Some(effort) = data
        .get("effort")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        meta.effort = Some(effort.to_owned());
    }
    if let Some(pct) = data
        .pointer("/context/percent")
        .and_then(serde_json::Value::as_f64)
        .and_then(float_percent)
    {
        meta.context_percent = Some(pct);
    }
    if let Some(supported) = data.get("supported").and_then(serde_json::Value::as_bool) {
        meta.supported = supported;
    }
    if let Some(effort_supported) = data
        .get("effort_supported")
        .and_then(serde_json::Value::as_bool)
    {
        meta.effort_supported = effort_supported;
    }
    if let Some(models) = data.get("models").and_then(serde_json::Value::as_array) {
        let parsed = parse_composer_items(models);
        if !parsed.is_empty() {
            meta.models = parsed;
        }
    }
    if let Some(efforts) = data.get("efforts").and_then(serde_json::Value::as_array) {
        let parsed: Vec<String> = efforts
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect();
        if !parsed.is_empty() {
            meta.efforts = parsed;
        }
    }
    agent
}

pub(crate) fn apply_composer_models_json(raw: &str, meta: &mut ComposerMeta) {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    if root.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return;
    }
    let Some(data) = root.get("data") else {
        return;
    };
    if let Some(supported) = data.get("supported").and_then(serde_json::Value::as_bool) {
        meta.supported = supported;
    }
    if let Some(current) = data
        .get("current")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        meta.current_model_id = Some(current.to_owned());
    }
    if let Some(items) = data.get("items").and_then(serde_json::Value::as_array) {
        let parsed = parse_composer_items(items);
        if !parsed.is_empty() {
            meta.models = parsed;
        }
    }
    meta.reason = data
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
}

pub(crate) fn apply_composer_set_json(raw: &str, meta: &mut ComposerMeta) -> bool {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    if root.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        meta.reason = root
            .get("error")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        return false;
    }
    let Some(data) = root.get("data") else {
        return false;
    };
    let applied = data
        .get("applied")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if applied {
        if let Some(model) = data
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        {
            meta.model = Some(model.to_owned());
            meta.current_model_id = Some(model.to_owned());
        }
        if let Some(label) = data
            .get("label")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        {
            meta.model_label = Some(label.to_owned());
        }
        if let Some(effort) = data
            .get("effort")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        {
            meta.effort = Some(effort.to_owned());
        }
        meta.reason = None;
        true
    } else {
        meta.reason = data
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        false
    }
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

    pub(super) fn chat_key(&self, pane: PaneId) -> Option<ChatKey> {
        Some((self.layout.host(pane).clone(), self.layout.session(pane)?))
    }

    /// 이 호스트·세션을 보여 주는 칸. 포커스 칸을 먼저 본다.
    fn chat_pane(&self, key: &ChatKey) -> Option<PaneId> {
        let shows = |pane: &PaneId| self.chat_key(*pane).as_ref() == Some(key);
        self.layout
            .focused()
            .filter(shows)
            .or_else(|| self.layout.visible().into_iter().find(shows))
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
        let Some(pane) = self.layout.focused() else {
            return;
        };
        self.set_pane_chat_view(pane, chat, cx);
        let key = (self.layout.host(pane).clone(), session);
        self.chat.focus_pending = Some(if chat {
            ChatFocus::Composer(key)
        } else {
            ChatFocus::Terminal(key)
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
        if let Some(key) = self
            .active_session_id()
            .filter(|id| self.is_session_chat_view(*id, cx))
            .and_then(|_| self.layout.focused())
            .and_then(|pane| self.chat_key(pane))
        {
            let input = self.ensure_chat_input(key, window, cx);
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
        let key = (host.clone(), session);
        let Some(target) = self.target_for_host(&host) else {
            if self.chat.poll_failed.insert(key) {
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
        let offset = self.chat.logs.get(&key).map_or(0, |log| log.offset);
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
                    && this.chat.logs.get(&key).map_or(0, |log| log.offset) == offset
                {
                    if let Ok(Reply::AgentHistory(Response::ChatPolled(snapshot))) = reply {
                        let refresh_composer = snapshot.reset
                            || !snapshot.items.is_empty()
                            || this
                                .chat
                                .last_composer_poll
                                .get(&key)
                                .is_none_or(|t| t.elapsed() >= Duration::from_secs(5));
                        this.chat.poll_failed.remove(&key);
                        this.apply_chat_poll_for_host(session, host, snapshot);
                        if refresh_composer {
                            this.refresh_chat_composer(&key, false, cx);
                        }
                    } else {
                        this.chat.poll_failed.insert(key);
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
        let key = (host.clone(), session);
        let host = Some(host);
        let log = self.chat.logs.entry(key.clone()).or_default();
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
            log.meta = ComposerMeta::default();
            log.answered.clear();
            log.expanded.clear();
            log.multi_selected.clear();
            log.submissions.clear();
            self.chat.request_tasks.remove(&key);
        }
        log.binding = binding;
        let is_local = host
            .as_ref()
            .is_some_and(krill_app::hosts::HostId::is_local);
        log.host = host;
        if is_local && snap.bound && (reset || !snap.items.is_empty() || log.meta.model.is_none()) {
            let state_dir = krill_proto::daemon::state_dir();
            let observed =
                observe_composer_meta(state_dir.as_deref(), &log.binding.0, &log.binding.1);
            if observed.model.is_some() {
                log.meta.model = observed.model;
            }
            if observed.effort.is_some() {
                log.meta.effort = observed.effort;
            }
            if observed.context_percent.is_some() {
                log.meta.context_percent = observed.context_percent;
            }
        }
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

    fn send_to_session(&mut self, key: &ChatKey, bytes: &[u8], cx: &mut Context<Self>) -> bool {
        self.chat_pane(key)
            .is_some_and(|pane| self.send_to_chat_pane(pane, key.1, bytes, cx))
    }

    fn workflow_call_for_key(
        &self,
        key: &ChatKey,
    ) -> Option<krill_app::agent_history::WorkflowCall> {
        #[cfg(test)]
        if let Some(call) = &self.chat.workflow_call {
            return Some(call.clone());
        }
        let target = self.target_for_host(&key.0)?;
        Some(krill_app::agent_history::workflow_call_for(Some(target)))
    }

    pub(super) fn refresh_chat_composer(
        &mut self,
        key: &ChatKey,
        include_models: bool,
        cx: &mut Context<Self>,
    ) {
        if self.chat.logs.get(key).is_some_and(|log| log.meta.pending) {
            return;
        }
        let Some(call) = self.workflow_call_for_key(key) else {
            return;
        };
        let task_key = key.clone();
        let session = key.1.0;
        self.chat
            .last_composer_poll
            .insert(key.clone(), std::time::Instant::now());
        let task = cx.spawn(async move |this, cx| {
            let (status_raw, models_raw) = cx
                .background_executor()
                .spawn(async move {
                    let status_req =
                        serde_json::json!({"action": "chat_composer", "session": session})
                            .to_string();
                    let status_res = call(&status_req).ok();
                    let models_res = if include_models {
                        let models_req =
                            serde_json::json!({"action": "composer_models", "session": session})
                                .to_string();
                        call(&models_req).ok()
                    } else {
                        None
                    };
                    (status_res, models_res)
                })
                .await;
            this.update(cx, |this, cx| {
                this.chat.composer_tasks.remove(&task_key);
                let log = this.chat.logs.entry(task_key.clone()).or_default();
                if let Some(raw) = status_raw
                    && let Some(agent) = apply_composer_status_json(&raw, &mut log.meta)
                    && log.agent.is_empty()
                {
                    log.agent = agent;
                }
                if let Some(raw) = models_raw {
                    apply_composer_models_json(&raw, &mut log.meta);
                }
                cx.notify();
            })
            .ok();
        });
        self.chat.composer_tasks.insert(key.clone(), task);
    }

    pub(super) fn toggle_chat_model_picker(&mut self, key: &ChatKey, cx: &mut Context<Self>) {
        self.chat.quick_menu.remove(key);
        if self.chat.model_picker.contains(key) {
            self.chat.model_picker.remove(key);
        } else {
            if let Some(log) = self.chat.logs.get_mut(key) {
                log.meta.reason = None;
            }
            self.chat.model_picker.insert(key.clone());
            self.refresh_chat_composer(key, true, cx);
        }
        cx.notify();
    }

    pub(super) fn select_chat_model(
        &mut self,
        key: &ChatKey,
        model_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.chat.quick_menu.remove(key);
        let Some(call) = self.workflow_call_for_key(key) else {
            return;
        };
        let log = self.chat.logs.entry(key.clone()).or_default();
        if log.meta.pending {
            return;
        }
        let effort = log
            .meta
            .models
            .iter()
            .find(|item| item.id == model_id)
            .and_then(|item| {
                log.meta
                    .effort
                    .as_ref()
                    .filter(|eff| item.efforts.contains(eff))
                    .cloned()
                    .or_else(|| item.default_effort.clone())
            });
        log.meta.pending = true;
        log.meta.reason = None;
        let task_key = key.clone();
        let session = key.1.0;
        let model = model_id.to_owned();
        let req_json = serde_json::json!({
            "action": "set_model",
            "session": session,
            "model": model,
            "effort": effort,
        })
        .to_string();
        let task = cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move { call(&req_json) })
                .await;
            this.update(cx, |this, cx| {
                this.chat.composer_tasks.remove(&task_key);
                let applied = {
                    let log = this.chat.logs.entry(task_key.clone()).or_default();
                    log.meta.pending = false;
                    match reply {
                        Ok(raw) => apply_composer_set_json(&raw, &mut log.meta),
                        Err(err) => {
                            log.meta.reason = Some(err.to_string());
                            false
                        }
                    }
                };
                if applied {
                    this.chat.model_picker.remove(&task_key);
                    this.refresh_chat_composer(&task_key, false, cx);
                }
                cx.notify();
            })
            .ok();
        });
        self.chat.composer_tasks.insert(key.clone(), task);
        cx.notify();
    }

    pub(super) fn select_chat_effort(
        &mut self,
        key: &ChatKey,
        effort: &str,
        cx: &mut Context<Self>,
    ) {
        self.chat.quick_menu.remove(key);
        let Some(call) = self.workflow_call_for_key(key) else {
            return;
        };
        let log = self.chat.logs.entry(key.clone()).or_default();
        if log.meta.pending {
            return;
        }
        log.meta.pending = true;
        log.meta.reason = None;
        let task_key = key.clone();
        let session = key.1.0;
        let req_json = serde_json::json!({
            "action": "set_effort",
            "session": session,
            "effort": effort,
        })
        .to_string();
        let task = cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(async move { call(&req_json) })
                .await;
            this.update(cx, |this, cx| {
                this.chat.composer_tasks.remove(&task_key);
                let applied = {
                    let log = this.chat.logs.entry(task_key.clone()).or_default();
                    log.meta.pending = false;
                    match reply {
                        Ok(raw) => apply_composer_set_json(&raw, &mut log.meta),
                        Err(err) => {
                            log.meta.reason = Some(err.to_string());
                            false
                        }
                    }
                };
                if applied {
                    this.chat.model_picker.remove(&task_key);
                    this.refresh_chat_composer(&task_key, false, cx);
                }
                cx.notify();
            })
            .ok();
        });
        self.chat.composer_tasks.insert(key.clone(), task);
        cx.notify();
    }

    fn send_to_chat_pane(
        &mut self,
        pane: super::PaneId,
        session: SessionId,
        bytes: &[u8],
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .chat
            .prompt_tasks
            .contains_key(&(self.layout.host(pane).clone(), session))
        {
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
        key: ChatKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<TextareaState> {
        if let Some((input, _)) = self.chat.inputs.get(&key) {
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
        let active =
            self.layout.focused().and_then(|pane| self.chat_key(pane)) == Some(key.clone());
        self.chat.inputs.insert(key, (input.clone(), sub));
        if active {
            input.update(cx, |state, cx| state.focus(window, cx));
        }
        input
    }

    pub(super) fn submit_chat_prompt(
        &mut self,
        key: &ChatKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chat.prompt_tasks.contains_key(key) || self.chat.prompt_review.contains(key) {
            return;
        }
        let Some((input, _)) = self.chat.inputs.get(key) else {
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
        let Some(pane) = self.chat_pane(key) else {
            return;
        };
        let session = key.1;
        let modes = self
            .live
            .get(&pane)
            .map_or_else(Default::default, |live| live.pane.modes());
        // Use the terminal consumer's paste plan, then submit separately.
        // Unknown unbracketed consumers retain the multiline draft for review.
        let krill_app::security::PastePlan::Immediate(mut bytes) =
            self.plan_terminal_paste(pane, text.as_bytes(), modes, cx)
        else {
            self.chat.multiline_blocked.insert(key.clone());
            cx.notify();
            return;
        };
        self.chat.multiline_blocked.remove(key);
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
        if self.send_to_session(key, &bytes, cx) {
            Self::clear_sent_chat_draft(&input, &text, window, cx);
            self.chat.prompt_failed.remove(key);
        } else {
            self.chat.prompt_failed.insert(key.clone());
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
        let Some(log) = self.chat.logs.get_mut(&request.chat_key()) else {
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
