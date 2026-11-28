// Adapted from Orca native-chat worker dock & temporary worker banner (MIT).
// Copyright (c) 2026 Lovecast Inc.
//! Active worker dock strip above the chat composer, temporary orchestration worker top banner,
//! and bot-chat worker lineage detection for defaulting spawned workers to Chat view.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use gpui_component::Icon;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_kit_assets::IconName;
use krill_app_core::hosts::HostId;
use krill_app_core::i18n::catalog::chat as ctext;
use krill_app_core::ui;
use krill_proto::SessionId;
use krill_proto::control::AgentState;
use krill_proto::orchestration::{
    Dispatch, DispatchReleaseState, DispatchSessionOrigin, DispatchStatus, MessageType,
    OrchestrationStore,
};

use super::{ChatKey, PaneId, Shell};

/// At most 4 recent failed/stopped unacknowledged workers remain in the dock (Orca #47432).
pub(super) const MAX_RECENT_FAILURES: usize = 4;
/// Unacknowledged failed/stopped workers expire after 4 hours (Orca #47432).
pub(super) const FAILURE_RETENTION_MS: u64 = 4 * 60 * 60 * 1000;
/// Temporary orchestration workers close 2 minutes after finishing unless kept open (Orca #43521).
pub(super) const WORKER_AUTO_CLOSE_MS: u64 = 2 * 60 * 1000;
/// Safety depth cap when walking coordinator ancestry for bot-chat lineage.
const MAX_LINEAGE_DEPTH: usize = 16;

/// Live session observation needed by the pure worker-dock projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SessionObservation {
    pub state: AgentState,
    pub exited: bool,
    pub since_ms: u64,
}

/// Visual/semantic state of a worker row inside the chat worker dock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkerDockState {
    Working,
    Starting,
    NeedsYou,
    Idle,
    Stopped,
    Failed,
}

impl WorkerDockState {
    pub(super) fn is_actively_working(self) -> bool {
        self == Self::Working
    }

    pub(super) fn is_needs_you(self) -> bool {
        self == Self::NeedsYou
    }

    pub(super) fn is_failure(self) -> bool {
        matches!(self, Self::Stopped | Self::Failed)
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Working => ctext::WORKER_STATE_RUNNING.get(),
            Self::Starting => ctext::WORKER_STARTING.get(),
            Self::NeedsYou => ctext::WORKER_DOCK_STATE_NEEDS_YOU.get(),
            Self::Idle => ctext::WORKER_DOCK_STATE_IDLE.get(),
            Self::Stopped => ctext::WORKER_STATE_STOPPED.get(),
            Self::Failed => ctext::WORKER_STATE_FAILED.get(),
        }
    }

    fn sort_rank(self) -> u8 {
        match self {
            Self::NeedsYou => 0,
            Self::Stopped | Self::Failed => 1,
            Self::Working => 2,
            Self::Starting => 3,
            Self::Idle => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WorkerDockRow {
    pub dispatch_id: String,
    pub worker_name: String,
    pub title: String,
    pub detail: String,
    pub agent: String,
    pub session_id: Option<SessionId>,
    pub state: WorkerDockState,
    pub clearable: bool,
    pub at_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct WorkerDockSnapshot {
    pub rows: Vec<WorkerDockRow>,
    /// Count of ONLY actively working workers (never includes failed, stopped, waiting, or idle).
    pub active_count: usize,
    pub needs_you_count: usize,
    pub failed_count: usize,
    pub clearable_ids: Vec<String>,
}

impl WorkerDockSnapshot {
    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(super) fn summary_text(&self) -> String {
        let mut parts = Vec::new();
        if self.active_count == 1 {
            parts.push(ctext::WORKER_DOCK_COUNT_ONE.get().to_owned());
        } else if self.active_count > 1 {
            parts.push(
                ctext::WORKER_DOCK_COUNT_MANY.format(&[("n", &self.active_count.to_string())]),
            );
        }
        if self.needs_you_count > 0 {
            parts.push(
                ctext::WORKER_DOCK_NEEDS_YOU.format(&[("n", &self.needs_you_count.to_string())]),
            );
        }
        if self.failed_count > 0 {
            parts.push(ctext::WORKER_DOCK_FAILED.format(&[("n", &self.failed_count.to_string())]));
        }
        if parts.is_empty() && !self.rows.is_empty() {
            // Only idle live workers remain; show 0 active workers honestly.
            parts.push(ctext::WORKER_DOCK_EMPTY.get().to_owned());
        }
        parts.join(" · ")
    }
}

fn has_newer_retry(target: &Dispatch, run_dispatches: &[&Dispatch]) -> bool {
    run_dispatches.iter().any(|other| {
        if other.id == target.id {
            return false;
        }
        if other.retry_of.as_deref() == Some(target.id.as_str()) {
            return true;
        }
        let newer = other.started_at_ms > target.started_at_ms
            || (other.started_at_ms == target.started_at_ms && other.id > target.id);
        if !newer {
            return false;
        }
        if target.task_id.is_some() && other.task_id == target.task_id {
            return true;
        }
        !target.worker_name.is_empty() && other.worker_name == target.worker_name
    })
}

fn is_owner_stopped(dispatch: &Dispatch, owner_stopped: &BTreeSet<String>) -> bool {
    if owner_stopped.contains(&dispatch.id) {
        return true;
    }
    if dispatch.release_state != DispatchReleaseState::Active {
        return true;
    }
    dispatch.summary.as_deref().is_some_and(|summary| {
        let lower = summary.to_ascii_lowercase();
        lower.contains("stopped by owner") || lower.contains("owner stopped")
    })
}

fn is_acknowledged(dispatch: &Dispatch, store: &OrchestrationStore) -> bool {
    store.messages.iter().any(|msg| {
        msg.dispatch_id.as_deref() == Some(dispatch.id.as_str())
            && msg.acked
            && matches!(
                msg.msg_type,
                MessageType::WorkerDone | MessageType::Escalation
            )
    })
}

fn dispatch_row_title(dispatch: &Dispatch, store: &OrchestrationStore) -> String {
    if let Some(task_id) = dispatch.task_id.as_deref()
        && let Some(task) = store.tasks.iter().find(|t| t.id == task_id)
        && !task.title.trim().is_empty()
    {
        return task.title.trim().to_owned();
    }
    let cwd_name = std::path::Path::new(&dispatch.cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty());
    cwd_name.unwrap_or_else(|| dispatch.worker_name.clone())
}

fn dispatch_row_detail(dispatch: &Dispatch) -> String {
    dispatch
        .summary
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(dispatch.prompt.as_str())
        .trim()
        .to_owned()
}

/// Pure projection for the chat worker dock strip (Orca #47149, #47432, #47774).
pub(super) fn build_worker_dock_snapshot(
    store: &OrchestrationStore,
    coordinator_session: SessionId,
    sessions: &HashMap<SessionId, SessionObservation>,
    cleared: &BTreeSet<String>,
    owner_stopped: &BTreeSet<String>,
    now_ms: u64,
) -> WorkerDockSnapshot {
    let run_ids: BTreeSet<&str> = store
        .runs
        .iter()
        .filter(|r| r.coordinator_session_id == Some(coordinator_session.0))
        .map(|r| r.id.as_str())
        .collect();
    if run_ids.is_empty() {
        return WorkerDockSnapshot::default();
    }

    let mut by_run: HashMap<&str, Vec<&Dispatch>> = HashMap::new();
    for dispatch in &store.dispatches {
        if run_ids.contains(dispatch.run_id.as_str()) {
            by_run
                .entry(dispatch.run_id.as_str())
                .or_default()
                .push(dispatch);
        }
    }

    let mut live_rows = Vec::new();
    let mut failure_rows = Vec::new();

    for dispatches in by_run.values() {
        for &dispatch in dispatches {
            if dispatch.is_released() || cleared.contains(&dispatch.id) {
                continue;
            }
            let session_id = dispatch.session_id.map(SessionId);
            let obs = session_id.and_then(|sid| sessions.get(&sid).copied());
            let title = dispatch_row_title(dispatch, store);
            let detail = dispatch_row_detail(dispatch);
            let at_ms = obs
                .map(|o| o.since_ms)
                .filter(|&ms| ms > 0)
                .unwrap_or_else(|| dispatch.ended_at_ms.unwrap_or(dispatch.started_at_ms));

            match dispatch.status {
                DispatchStatus::Active => {
                    // Exited or cut-off sessions map to `Stopped` (never "status unavailable").
                    let cut_off = session_id.is_some()
                        && obs.is_none_or(|o| o.exited || o.state == AgentState::Exited);
                    if cut_off {
                        if has_newer_retry(dispatch, dispatches)
                            || is_acknowledged(dispatch, store)
                            || now_ms.saturating_sub(at_ms) > FAILURE_RETENTION_MS
                        {
                            continue;
                        }
                        failure_rows.push(WorkerDockRow {
                            dispatch_id: dispatch.id.clone(),
                            worker_name: dispatch.worker_name.clone(),
                            title,
                            detail,
                            agent: dispatch.agent.clone(),
                            session_id,
                            state: WorkerDockState::Stopped,
                            clearable: true,
                            at_ms,
                        });
                    } else {
                        let state = match obs.map(|o| o.state) {
                            Some(AgentState::Waiting | AgentState::Blocked) => {
                                WorkerDockState::NeedsYou
                            }
                            Some(AgentState::Done) => WorkerDockState::Idle,
                            Some(AgentState::Working) => WorkerDockState::Working,
                            Some(AgentState::Unknown) | None => {
                                if now_ms.saturating_sub(dispatch.started_at_ms) < 15_000 {
                                    WorkerDockState::Starting
                                } else {
                                    WorkerDockState::Idle
                                }
                            }
                            Some(AgentState::Exited) => WorkerDockState::Stopped,
                        };
                        live_rows.push(WorkerDockRow {
                            dispatch_id: dispatch.id.clone(),
                            worker_name: dispatch.worker_name.clone(),
                            title,
                            detail,
                            agent: dispatch.agent.clone(),
                            session_id,
                            state,
                            clearable: false,
                            at_ms,
                        });
                    }
                }
                DispatchStatus::Failed | DispatchStatus::Abandoned => {
                    let ended = dispatch.ended_at_ms.unwrap_or(dispatch.started_at_ms);
                    if has_newer_retry(dispatch, dispatches)
                        || is_acknowledged(dispatch, store)
                        || now_ms.saturating_sub(ended) > FAILURE_RETENTION_MS
                    {
                        continue;
                    }
                    failure_rows.push(WorkerDockRow {
                        dispatch_id: dispatch.id.clone(),
                        worker_name: dispatch.worker_name.clone(),
                        title,
                        detail,
                        agent: dispatch.agent.clone(),
                        session_id,
                        state: WorkerDockState::Failed,
                        clearable: true,
                        at_ms: ended,
                    });
                }
                DispatchStatus::Stopped => {
                    let ended = dispatch.ended_at_ms.unwrap_or(dispatch.started_at_ms);
                    if is_owner_stopped(dispatch, owner_stopped)
                        || has_newer_retry(dispatch, dispatches)
                        || is_acknowledged(dispatch, store)
                        || now_ms.saturating_sub(ended) > FAILURE_RETENTION_MS
                    {
                        continue;
                    }
                    failure_rows.push(WorkerDockRow {
                        dispatch_id: dispatch.id.clone(),
                        worker_name: dispatch.worker_name.clone(),
                        title,
                        detail,
                        agent: dispatch.agent.clone(),
                        session_id,
                        state: WorkerDockState::Stopped,
                        clearable: true,
                        at_ms: ended,
                    });
                }
                DispatchStatus::Succeeded | DispatchStatus::Retained | DispatchStatus::Released => {
                }
            }
        }
    }

    // Keep at most MAX_RECENT_FAILURES newest unacknowledged failures/stops.
    failure_rows.sort_by(|a, b| {
        b.at_ms
            .cmp(&a.at_ms)
            .then_with(|| b.dispatch_id.cmp(&a.dispatch_id))
    });
    failure_rows.truncate(MAX_RECENT_FAILURES);

    let mut rows = live_rows;
    rows.extend(failure_rows);
    rows.sort_by(|a, b| {
        a.state
            .sort_rank()
            .cmp(&b.state.sort_rank())
            .then_with(|| b.at_ms.cmp(&a.at_ms))
            .then_with(|| b.dispatch_id.cmp(&a.dispatch_id))
    });

    let active_count = rows
        .iter()
        .filter(|r| r.state.is_actively_working())
        .count();
    let needs_you_count = rows.iter().filter(|r| r.state.is_needs_you()).count();
    let failed_count = rows.iter().filter(|r| r.state.is_failure()).count();
    let clearable_ids = rows
        .iter()
        .filter(|r| r.clearable)
        .map(|r| r.dispatch_id.clone())
        .collect();

    WorkerDockSnapshot {
        rows,
        active_count,
        needs_you_count,
        failed_count,
        clearable_ids,
    }
}

/// Phase of a temporary orchestration worker's top banner (Orca #43521).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TemporaryWorkerPhase {
    Running,
    Settled { remaining_ms: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TemporaryWorkerBanner {
    pub dispatch_id: String,
    pub worker_name: String,
    pub phase: TemporaryWorkerPhase,
}

/// Formats remaining auto-close milliseconds as `M:SS` (ceiling to whole seconds).
pub(super) fn format_worker_countdown(remaining_ms: u64) -> String {
    let secs = remaining_ms.div_ceil(1000);
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Pure projection for whether a session shows the temporary worker top banner.
pub(super) fn temporary_worker_banner_for_session(
    store: &OrchestrationStore,
    session: SessionId,
    kept_dispatches: &BTreeSet<String>,
    now_ms: u64,
) -> Option<TemporaryWorkerBanner> {
    let dispatch = store
        .dispatches
        .iter()
        .rev()
        .find(|d| d.session_id == Some(session.0))?;
    if dispatch.session_origin != DispatchSessionOrigin::Created
        || dispatch.is_released()
        || dispatch.release_state == DispatchReleaseState::Retained
        || dispatch.status == DispatchStatus::Retained
        || kept_dispatches.contains(&dispatch.id)
    {
        return None;
    }
    let phase = match dispatch.status {
        DispatchStatus::Active => TemporaryWorkerPhase::Running,
        DispatchStatus::Succeeded
        | DispatchStatus::Failed
        | DispatchStatus::Stopped
        | DispatchStatus::Abandoned => {
            let ended_at = dispatch.ended_at_ms.unwrap_or(dispatch.started_at_ms);
            let closes_at = ended_at.saturating_add(WORKER_AUTO_CLOSE_MS);
            TemporaryWorkerPhase::Settled {
                remaining_ms: closes_at.saturating_sub(now_ms),
            }
        }
        DispatchStatus::Retained | DispatchStatus::Released => return None,
    };
    Some(TemporaryWorkerBanner {
        dispatch_id: dispatch.id.clone(),
        worker_name: dispatch.worker_name.clone(),
        phase,
    })
}

/// True when `session` is a created orchestration worker spawned from a bot chat (directly or as
/// a sub-worker at any depth), and its agent supports Chat view (Orca #47363).
pub(super) fn is_bot_chat_worker_lineage(
    store: &OrchestrationStore,
    session: SessionId,
    is_bot_session: impl Fn(SessionId) -> bool,
) -> bool {
    let mut current = session;
    let mut visited = BTreeSet::new();
    for depth in 0..MAX_LINEAGE_DEPTH {
        if !visited.insert(current) {
            return false;
        }
        let Some(dispatch) = store
            .dispatches
            .iter()
            .rev()
            .find(|d| d.session_id == Some(current.0))
        else {
            return false;
        };
        if dispatch.session_origin == DispatchSessionOrigin::Borrowed {
            return false;
        }
        if depth == 0 && is_terminal_only_agent(&dispatch.agent) {
            return false;
        }
        let Some(parent_id) = store
            .runs
            .iter()
            .find(|r| r.id == dispatch.run_id)
            .and_then(|r| r.coordinator_session_id)
        else {
            return false;
        };
        let parent = SessionId(parent_id);
        if is_bot_session(parent) {
            return true;
        }
        current = parent;
    }
    false
}

fn is_terminal_only_agent(agent: &str) -> bool {
    matches!(
        agent.trim().to_ascii_lowercase().as_str(),
        "" | "terminal" | "shell" | "bash" | "zsh" | "sh" | "fish"
    )
}

impl Shell {
    /// True when `session` on `host` is an orchestration worker whose ancestor coordinator is a
    /// bot chat, so it defaults to Chat view unless explicitly set to Terminal.
    pub(in crate::shell) fn is_bot_chat_worker_session(
        &self,
        host: &HostId,
        session: SessionId,
        cx: &gpui::App,
    ) -> bool {
        let Some(store) = self.orch_snapshot_for(host) else {
            return false;
        };
        let sidebar = self.sidebar.read(cx);
        is_bot_chat_worker_lineage(store, session, |candidate| {
            self.sidebar_bots
                .iter()
                .any(|b| &b.host == host && b.sessions.iter().any(|s| s.session_id == candidate))
                || sidebar.bot_session_meta_on(host, candidate).is_some()
                || self
                    .chat
                    .cached_bots
                    .contains_key(&(host.clone(), candidate))
        })
    }

    pub(super) fn worker_dock_snapshot(&self, key: &ChatKey, cx: &gpui::App) -> WorkerDockSnapshot {
        let Some(store) = self.orch_snapshot_for(&key.0) else {
            return WorkerDockSnapshot::default();
        };
        let sidebar = self.sidebar.read(cx);
        let mut sessions = HashMap::new();
        for dispatch in &store.dispatches {
            if let Some(sid) = dispatch.session_id {
                let id = SessionId(sid);
                if let Some(info) = sidebar.session_on(&key.0, id) {
                    sessions.insert(
                        id,
                        SessionObservation {
                            state: info.status.state,
                            exited: info.is_exited(),
                            since_ms: info.status.since_unix_ns / 1_000_000,
                        },
                    );
                }
            }
        }
        let cleared: BTreeSet<String> = self
            .chat
            .worker_dock_cleared
            .iter()
            .filter(|(h, _)| h == &key.0)
            .map(|(_, id)| id.clone())
            .collect();
        let owner_stopped: BTreeSet<String> = self
            .chat
            .worker_dock_owner_stopped
            .iter()
            .filter(|(h, _)| h == &key.0)
            .map(|(_, id)| id.clone())
            .collect();
        build_worker_dock_snapshot(
            store,
            key.1,
            &sessions,
            &cleared,
            &owner_stopped,
            self.chat_now_epoch_ms(),
        )
    }

    pub(super) fn clear_worker_dock_row(
        &mut self,
        host: HostId,
        dispatch_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.chat.worker_dock_cleared.insert((host, dispatch_id)) {
            cx.notify();
        }
    }

    pub(super) fn clear_all_worker_dock_rows(
        &mut self,
        host: &HostId,
        dispatch_ids: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        for id in dispatch_ids {
            changed |= self.chat.worker_dock_cleared.insert((host.clone(), id));
        }
        if changed {
            cx.notify();
        }
    }

    /// Marks a temporary worker as kept open (optimistically hides the banner and persists
    /// `retain_worker` to `OrchestrationStore` on the background executor for local hosts).
    pub(super) fn retain_temporary_worker(
        &mut self,
        host: &HostId,
        session: SessionId,
        dispatch_id: String,
        cx: &mut Context<Self>,
    ) {
        self.chat.worker_keep_open.insert((host.clone(), session));
        self.chat
            .worker_banner_kept
            .insert((host.clone(), dispatch_id.clone()));
        if host.is_local() {
            // Update in-memory snapshot immediately so sibling surfaces stay consistent.
            let mut next = (*self.orch_snapshot).clone();
            if next.retain_worker(&dispatch_id).is_ok() {
                self.orch_snapshot = std::sync::Arc::new(next);
            }
        }
        cx.notify();

        if !host.is_local() {
            return;
        }
        let Some(dir) = self.state_dir(cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    OrchestrationStore::mutate(&dir, |store| store.retain_worker(&dispatch_id))
                })
                .await;
            this.update(cx, |this, cx| {
                if result.is_ok() {
                    this.refresh_detached(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn ensure_worker_banner_ticker(&mut self, cx: &mut Context<Self>) {
        if self.chat.worker_banner_timer.is_some() {
            return;
        }
        self.chat.worker_banner_timer = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep_running = this
                    .update(cx, |this, cx| {
                        let now_ms = this.chat_now_epoch_ms();
                        let any_countdown =
                            this.visible_chat_panes(cx).into_iter().any(|(pane, sid)| {
                                let host = this.layout.host(pane);
                                let chat_key = (host.clone(), sid);
                                let kept: BTreeSet<String> = this
                                    .chat
                                    .worker_banner_kept
                                    .iter()
                                    .filter(|(h, _)| h == host)
                                    .map(|(_, id)| id.clone())
                                    .collect();
                                !this.chat.worker_keep_open.contains(&chat_key)
                                    && this.orch_snapshot_for(host).is_some_and(|store| {
                                        matches!(
                                            temporary_worker_banner_for_session(
                                                store,
                                                sid,
                                                &kept,
                                                now_ms,
                                            ),
                                            Some(TemporaryWorkerBanner {
                                                phase: TemporaryWorkerPhase::Settled {
                                                    remaining_ms
                                                },
                                                ..
                                            }) if remaining_ms > 0
                                        )
                                    })
                            });
                        if any_countdown {
                            cx.notify();
                            true
                        } else {
                            this.chat.worker_banner_timer = None;
                            false
                        }
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }

    /// Orca `NativeChatWorkerAutoCloseBanner` (`#43521`): in-flow top banner for temporary
    /// orchestration workers showing running or 2-minute auto-close countdown state and a
    /// `Keep open` action that persists `retain_worker` and stays visible when prompt/outage
    /// cards replace the composer.
    pub(super) fn render_worker_auto_close_banner(
        &mut self,
        pane: PaneId,
        key: &ChatKey,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.chat.worker_keep_open.contains(key) {
            return None;
        }
        let store = self.orch_snapshot_for(&key.0)?;
        let kept: BTreeSet<String> = self
            .chat
            .worker_banner_kept
            .iter()
            .filter(|(h, _)| h == &key.0)
            .map(|(_, id)| id.clone())
            .collect();
        let banner =
            temporary_worker_banner_for_session(store, key.1, &kept, self.chat_now_epoch_ms())?;
        let palette = ui::palette(cx);
        let dispatch_id = banner.dispatch_id.clone();
        let host = key.0.clone();
        let session = key.1;

        let (bg, border, icon_el, title) = match banner.phase {
            TemporaryWorkerPhase::Running => (
                palette.muted.opacity(0.45),
                palette.border,
                Icon::new(IconName::Timer)
                    .with_size(ui::ICON_MD)
                    .text_color(palette.muted_foreground)
                    .into_any_element(),
                ctext::TEMP_WORKER_RUNNING.get().to_owned(),
            ),
            TemporaryWorkerPhase::Settled { remaining_ms } => {
                if remaining_ms > 0 {
                    self.ensure_worker_banner_ticker(cx);
                }
                let text = if remaining_ms > 0 {
                    let countdown = format_worker_countdown(remaining_ms);
                    ctext::TEMP_WORKER_FINISHED.format(&[("countdown", &countdown)])
                } else {
                    ctext::TEMP_WORKER_CLOSING_SOON.get().to_owned()
                };
                (
                    palette.warning.opacity(0.1),
                    palette.warning.opacity(0.3),
                    Icon::new(IconName::Clock)
                        .with_size(ui::ICON_MD)
                        .text_color(palette.warning)
                        .into_any_element(),
                    text,
                )
            }
        };

        let full_title = if banner.worker_name.trim().is_empty() {
            title
        } else {
            format!("{} · {title}", banner.worker_name)
        };

        Some(
            div()
                .id(SharedString::from(format!(
                    "chat-worker-auto-close-banner-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-worker-auto-close-banner".into())
                .role(gpui::Role::Status)
                .font_family(ui::FONT_UI)
                .flex_none()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2p5()
                .border_b_1()
                .border_color(border)
                .bg(bg)
                .py_1p5()
                .pr_2()
                .pl_3()
                .text_size(ui::TEXT_SM)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_none().child(icon_el))
                        .child(
                            div()
                                .debug_selector(|| "chat-worker-auto-close-banner-text".into())
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(palette.foreground)
                                .child(full_title),
                        ),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "chat-worker-keep-open-{}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-worker-keep-open".into())
                    .label(ctext::WORKER_KEEP_OPEN.get())
                    .outline()
                    .xsmall()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.retain_temporary_worker(&host, session, dispatch_id.clone(), cx);
                    })),
                )
                .into_any_element(),
        )
    }

    /// Orca `#47149` + `#47432` + `#47774` + `#47359`: combined worker dock directly above
    /// the composer. Preserves Wave 2B Orca 1:1 visual tokens & layout while backing it with
    /// active-only counting, 4h/4-item unacknowledged failure retention, exited->Stopped mapping,
    /// per-row Clear + Clear all, and wrapped long task descriptions.
    pub(super) fn render_chat_worker_dock(
        &self,
        pane: PaneId,
        key: &ChatKey,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let snap = self.worker_dock_snapshot(key, cx);
        if snap.is_empty() {
            return None;
        }
        let palette = ui::palette(cx);
        let expanded = self.chat.worker_dock_expanded.contains(key);
        let summary = snap.summary_text();
        let clearable_ids = snap.clearable_ids.clone();
        let show_clear_all = expanded && !clearable_ids.is_empty();
        let toggle_key = key.clone();
        let host = key.0.clone();

        let header = div()
            .id(SharedString::from(format!(
                "chat-worker-dock-header-{}",
                pane.get()
            )))
            .debug_selector(|| "chat-worker-dock-header".into())
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .justify_between()
            .gap_1()
            .px_2()
            .py_1()
            .child(
                div()
                    .id(SharedString::from(format!(
                        "chat-worker-dock-toggle-{}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-worker-dock-toggle".into())
                    .role(gpui::Role::Button)
                    .aria_expanded(expanded)
                    .aria_label(if expanded {
                        ctext::WORKER_DOCK_COLLAPSE.get()
                    } else {
                        ctext::WORKER_DOCK_EXPAND.get()
                    })
                    .tab_index(0)
                    .focus_visible(|d| d.border_1().border_color(palette.ring))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .cursor_pointer()
                    .rounded(ui::RADIUS_SM)
                    .px_1()
                    .py_0p5()
                    .hover(|s| s.bg(palette.secondary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.chat.worker_dock_expanded.remove(&toggle_key) {
                            this.chat.worker_dock_expanded.insert(toggle_key.clone());
                        }
                        cx.notify();
                    }))
                    .child(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(px(ui::ICON_XS))
                        .text_color(palette.muted_foreground),
                    )
                    .when(snap.active_count > 0, |d| {
                        // Orca `#47774`: shared working spinner in the dock header.
                        d.child(
                            div()
                                .debug_selector(|| "chat-worker-dock-spinner".into())
                                .size(px(8.))
                                .rounded_full()
                                .bg(palette.agent_working()),
                        )
                    })
                    .when(
                        snap.active_count == 0
                            && (snap.needs_you_count > 0 || snap.failed_count > 0),
                        |d| {
                            let badge: AnyElement = if snap.needs_you_count > 0 {
                                ui::status::status_glyph(
                                    AgentState::Waiting,
                                    false,
                                    ui::status::Size::Sm,
                                    cx,
                                )
                                .into_any_element()
                            } else {
                                Icon::new(IconName::CircleAlert)
                                    .with_size(ui::ICON_SM)
                                    .text_color(palette.destructive)
                                    .into_any_element()
                            };
                            d.child(div().flex_none().child(badge))
                        },
                    )
                    .child(
                        div()
                            .debug_selector(|| "chat-worker-dock-count".into())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(palette.foreground)
                            .child(summary),
                    ),
            )
            .when(show_clear_all, |d| {
                let clear_host = host.clone();
                d.child(
                    Button::new(SharedString::from(format!(
                        "chat-worker-dock-clear-{}",
                        pane.get()
                    )))
                    .debug_selector(|| "chat-worker-dock-clear".into())
                    .label(ctext::WORKER_DOCK_CLEAR_ALL.get())
                    .ghost()
                    .xsmall()
                    .text_color(palette.muted_foreground)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.clear_all_worker_dock_rows(&clear_host, clearable_ids.clone(), cx);
                    })),
                )
            });

        let rows = expanded.then(|| {
            div()
                .id(SharedString::from(format!(
                    "chat-worker-dock-list-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-worker-dock-list".into())
                .w_full()
                .min_w_0()
                .max_h(px(180.))
                .overflow_y_scroll()
                .border_t_1()
                .border_color(palette.border.opacity(0.6))
                .py_1()
                .flex()
                .flex_col()
                .children(snap.rows.into_iter().enumerate().map(|(ix, row)| {
                    let row_id = row.dispatch_id.clone();
                    let row_sel = format!("chat-worker-dock-row-{}", row.dispatch_id);
                    let session = row.session_id;
                    let row_host = host.clone();
                    let state_glyph: AnyElement = match row.state {
                        WorkerDockState::Working => ui::status::status_glyph(
                            AgentState::Working,
                            false,
                            ui::status::Size::Sm,
                            cx,
                        )
                        .into_any_element(),
                        WorkerDockState::Starting => ui::status::status_glyph(
                            AgentState::Unknown,
                            false,
                            ui::status::Size::Sm,
                            cx,
                        )
                        .into_any_element(),
                        WorkerDockState::NeedsYou => ui::status::status_glyph(
                            AgentState::Waiting,
                            false,
                            ui::status::Size::Sm,
                            cx,
                        )
                        .into_any_element(),
                        WorkerDockState::Idle => ui::status::status_glyph(
                            AgentState::Done,
                            false,
                            ui::status::Size::Sm,
                            cx,
                        )
                        .into_any_element(),
                        WorkerDockState::Stopped | WorkerDockState::Failed => {
                            Icon::new(IconName::CircleAlert)
                                .with_size(ui::ICON_SM)
                                .text_color(palette.destructive)
                                .into_any_element()
                        }
                    };
                    let state_color = match row.state {
                        WorkerDockState::Working => palette.agent_working(),
                        WorkerDockState::NeedsYou => palette.agent_question,
                        WorkerDockState::Starting
                        | WorkerDockState::Idle
                        | WorkerDockState::Stopped => palette.muted_foreground,
                        WorkerDockState::Failed => palette.destructive,
                    };
                    let primary_label = if row.worker_name.trim().is_empty() {
                        row.agent.clone()
                    } else if row.title == row.worker_name || row.title.is_empty() {
                        row.worker_name.clone()
                    } else {
                        format!("{} · {}", row.worker_name, row.title)
                    };
                    let clear_id = row.dispatch_id.clone();

                    // Full-width row hover including trailing action column (Orca #47774).
                    div()
                        .id(SharedString::from(format!(
                            "chat-worker-dock-row-{}-{row_id}",
                            pane.get()
                        )))
                        .debug_selector(move || format!("chat-worker-dock-row-{ix}"))
                        .role(gpui::Role::Button)
                        .aria_label(format!("{primary_label} · {}", row.state.label()))
                        .w_full()
                        .min_w_0()
                        .px_3()
                        .py_1()
                        .flex()
                        .items_start()
                        .gap_2()
                        .hover(|s| s.bg(palette.secondary))
                        .when_some(session, |el, sid| {
                            el.cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_worker_session_from_dock(row_host.clone(), sid, cx);
                                }))
                        })
                        .child(
                            div()
                                .debug_selector(move || row_sel.clone())
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_start()
                                .gap_2()
                                .child(div().mt(px(2.)).flex_none().child(state_glyph))
                                .child(div().mt(px(2.)).flex_none().child(ui::status::agent_icon(
                                    &row.agent,
                                    ui::ICON_XS,
                                    palette.muted_foreground,
                                )))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .w_full()
                                                .min_w_0()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .truncate()
                                                        .text_size(ui::TEXT_XS)
                                                        .font_weight(gpui::FontWeight::MEDIUM)
                                                        .text_color(palette.foreground)
                                                        .child(primary_label),
                                                )
                                                .child(
                                                    div()
                                                        .flex_none()
                                                        .text_size(ui::TEXT_XS)
                                                        .text_color(state_color)
                                                        .child(row.state.label()),
                                                ),
                                        )
                                        .when(!row.detail.is_empty(), |el| {
                                            // Orca `#47359`: long background task descriptions wrap cleanly.
                                            el.child(
                                                div()
                                                    .w_full()
                                                    .min_w_0()
                                                    .whitespace_normal()
                                                    .text_size(ui::TEXT_XS)
                                                    .text_color(palette.muted_foreground)
                                                    .child(row.detail),
                                            )
                                        }),
                                ),
                        )
                        .when(row.clearable, |d| {
                            let clear_sel = format!("chat-worker-dock-clear-{clear_id}");
                            let clear_host = host.clone();
                            d.child(
                                Button::new(SharedString::from(format!(
                                    "chat-worker-dock-clear-{}-{clear_id}",
                                    pane.get()
                                )))
                                .debug_selector(move || clear_sel.clone())
                                .icon(IconName::Close)
                                .tooltip(ctext::WORKER_DOCK_CLEAR.get())
                                .accessibility_label(ctext::WORKER_DOCK_CLEAR.get())
                                .ghost()
                                .xsmall()
                                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.clear_worker_dock_row(
                                            clear_host.clone(),
                                            clear_id.clone(),
                                            cx,
                                        );
                                    },
                                )),
                            )
                        })
                }))
        });

        Some(
            div()
                .id(SharedString::from(format!(
                    "chat-worker-dock-{}",
                    pane.get()
                )))
                .debug_selector(|| "chat-worker-dock".into())
                .w_full()
                .max_w(ui::CHAT_CONTENT_MAX_WIDTH)
                .min_w_0()
                .mb_1()
                .rounded(ui::RADIUS_MD)
                .border_1()
                .border_color(palette.border)
                .bg(palette.muted.opacity(0.28))
                .flex()
                .flex_col()
                .child(header)
                .children(rows)
                .into_any_element(),
        )
    }
}
