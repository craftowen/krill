// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Restart recovery classification (chats vs terminal workers, Orca #47284) and terminal worker
//! conversation resume under `resume_work_on_restart` (Orca #46643).

use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use krill_proto::SessionId;
use krill_proto::orchestration::{
    Dispatch, DispatchReleaseState, DispatchStatus, MessageType, ORCH_FILE, OrchestrationStore,
    SendParams, TaskStatus,
};
use krill_proto::os::write_atomic_0600;
use serde::{Deserialize, Serialize};

use crate::registry::Registry;

const RESTART_RECOVERY_FILE: &str = "restart-recovery.json";
const MAX_RECOVERY_BYTES: usize = 1024 * 1024;
const MAX_RECOVERY_OFFERS: usize = 512;

/// Summary recorded when a lost terminal worker cannot be resumed (or is dismissed).
pub const WORKER_LOST_SUMMARY: &str = "Worker lost without reporting";
/// Cautious continue prompt delivered ONLY when a resumed terminal worker was mid-turn.
pub const WORKER_CONTINUE_PROMPT: &str = "Continue from where you stopped.";

/// Distinguishes regular interactive chats from orchestration terminal workers so Resume and
/// Dismiss routing never misclassifies a regular chat as a terminal worker (Orca #47284).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartOfferKind {
    Chat,
    TerminalWorker,
}

/// Persisted lifecycle stage of a restart-recovery item (Orca #46643).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartOfferStage {
    Offered,
    /// Recorded BEFORE launching the resumed agent so a crash mid-resume never loops.
    Resumed,
    Dismissed,
}

/// Identifies a restart-recovery target by offer/dispatch ID or session ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartTarget<'a> {
    OfferId(&'a str),
    Session(SessionId),
}

/// Durable restart-recovery record persisted in `<state_dir>/restart-recovery.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartOffer {
    pub id: String,
    pub kind: RestartOfferKind,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_title: Option<String>,
    #[serde(default)]
    pub was_working: bool,
    pub stage: RestartOfferStage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_session_id: Option<SessionId>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartRecoveryLedger {
    #[serde(default)]
    pub offers: Vec<RestartOffer>,
}

impl RestartRecoveryLedger {
    #[must_use]
    pub fn load(state_dir: &Path) -> Self {
        let path = state_dir.join(RESTART_RECOVERY_FILE);
        let Ok(bytes) = fs::read(&path) else {
            return Self::default();
        };
        if bytes.len() > MAX_RECOVERY_BYTES {
            return Self::default();
        }
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    pub fn save(&self, state_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(state_dir)?;
        let path = state_dir.join(RESTART_RECOVERY_FILE);
        let mut bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        bytes.push(b'\n');
        write_atomic_0600(&path, &bytes)
    }

    #[must_use]
    pub fn find_by_id(&self, id: &str) -> Option<&RestartOffer> {
        self.offers.iter().find(|o| o.id == id)
    }

    pub fn find_by_id_mut(&mut self, id: &str) -> Option<&mut RestartOffer> {
        self.offers.iter_mut().find(|o| o.id == id)
    }

    #[must_use]
    pub fn find_by_dispatch(&self, dispatch_id: &str) -> Option<&RestartOffer> {
        self.offers
            .iter()
            .find(|o| o.dispatch_id.as_deref() == Some(dispatch_id))
    }

    pub fn upsert(&mut self, offer: RestartOffer) {
        if let Some(existing) = self.offers.iter_mut().find(|o| o.id == offer.id) {
            *existing = offer;
        } else {
            self.offers.push(offer);
            if self.offers.len() > MAX_RECOVERY_OFFERS {
                let excess = self.offers.len() - MAX_RECOVERY_OFFERS;
                self.offers.drain(..excess);
            }
        }
    }
}

/// Summary of a startup terminal-worker recovery pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkerRecoveryReport {
    /// `(dispatch_id, old_session, new_session)` pairs resumed into saved agent conversations.
    pub resumed: Vec<(String, SessionId, SessionId)>,
    /// Dispatch IDs settled as failed (`"Worker lost without reporting"`).
    pub failed_lost: Vec<String>,
    /// Dispatch IDs left untouched because their PTY session is still live.
    pub still_live: Vec<String>,
}

/// Result of routing an explicit Resume action for a restart-interrupted chat or terminal worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartResumeOutcome {
    ChatResumed {
        offer_id: String,
        session_id: SessionId,
    },
    WorkerResumed {
        dispatch_id: String,
        old_session: SessionId,
        new_session: SessionId,
    },
    WorkerFallbackLost {
        dispatch_id: String,
        reason: String,
    },
    AlreadyHandled(String),
    NotFound(String),
}

/// Result of routing an explicit Dismiss action for a restart-interrupted chat or terminal worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartDismissOutcome {
    ChatDismissed { offer_id: String },
    WorkerDismissedLost { dispatch_id: String },
    AlreadyHandled(String),
}

/// Reads `resume_work_on_restart` from `<state_dir>/settings.toml` (default `false`).
#[must_use]
pub fn read_resume_work_on_restart_setting(state_dir: &Path) -> bool {
    #[derive(Deserialize, Default)]
    struct PartialSettings {
        #[serde(default)]
        resume_work_on_restart: bool,
    }
    let Ok(text) = fs::read_to_string(state_dir.join("settings.toml")) else {
        return false;
    };
    toml::from_str::<PartialSettings>(&text).is_ok_and(|s| s.resume_work_on_restart)
}

/// Reads `chat_auto_continue_after_rate_limit` from `<state_dir>/settings.toml` (default `true`,
/// while strictly respecting an explicit `false` saved by the user).
#[must_use]
pub fn read_chat_auto_continue_setting(state_dir: &Path) -> bool {
    const fn default_true() -> bool {
        true
    }
    #[derive(Deserialize)]
    struct PartialSettings {
        #[serde(default = "default_true")]
        chat_auto_continue_after_rate_limit: bool,
    }
    let Ok(text) = fs::read_to_string(state_dir.join("settings.toml")) else {
        return true;
    };
    toml::from_str::<PartialSettings>(&text).map_or(true, |s| s.chat_auto_continue_after_rate_limit)
}

/// Looks up an active, non-released terminal-worker `Dispatch` matching `target` in the real
/// [`OrchestrationStore`]. Any missing lookup returns `None` (never classified as a worker).
#[must_use]
pub fn find_owned_active_dispatch<'a>(
    store: &'a OrchestrationStore,
    target: RestartTarget<'_>,
) -> Option<&'a Dispatch> {
    store.dispatches.iter().rev().find(|d| {
        if d.status != DispatchStatus::Active || d.release_state != DispatchReleaseState::Active {
            return false;
        }
        match target {
            RestartTarget::OfferId(id) => d.id == id,
            RestartTarget::Session(sid) => d.session_id == Some(sid.0),
        }
    })
}

/// Returns `true` ONLY when `target` corresponds to a real active terminal-worker `Dispatch` in
/// `store`. Regular chats and settled/unknown IDs ALWAYS return `false` (Orca #47284).
#[must_use]
pub fn is_owned_terminal_worker(store: &OrchestrationStore, target: RestartTarget<'_>) -> bool {
    find_owned_active_dispatch(store, target).is_some()
}

/// Classifies a restart-recovery target into [`RestartOfferKind::Chat`] or
/// [`RestartOfferKind::TerminalWorker`] against the real [`OrchestrationStore`].
#[must_use]
pub fn classify_restart_target(
    store: &OrchestrationStore,
    target: RestartTarget<'_>,
) -> RestartOfferKind {
    if is_owned_terminal_worker(store, target) {
        RestartOfferKind::TerminalWorker
    } else {
        RestartOfferKind::Chat
    }
}

/// Partitions a mixed slice of restart targets into `(chats, terminal_workers)` using the real
/// [`OrchestrationStore`] (Orca #47284).
#[must_use]
pub fn partition_restart_targets<'a>(
    store: &OrchestrationStore,
    targets: &'a [RestartTarget<'a>],
) -> (Vec<RestartTarget<'a>>, Vec<RestartTarget<'a>>) {
    let mut chats = Vec::new();
    let mut workers = Vec::new();
    for &target in targets {
        match classify_restart_target(store, target) {
            RestartOfferKind::Chat => chats.push(target),
            RestartOfferKind::TerminalWorker => workers.push(target),
        }
    }
    (chats, workers)
}

/// Marks a lost terminal-worker dispatch as `Failed` (`"Worker lost without reporting"`), updates
/// its linked `Dispatched` task if still pointing at this dispatch, and sends a single coordinator
/// escalation notice (idempotent per dispatch).
pub fn fail_lost_worker_dispatch(
    state_dir: &Path,
    dispatch_id: &str,
    now_ms: u64,
) -> io::Result<bool> {
    OrchestrationStore::mutate(state_dir, |store| {
        let Some(idx) = store.dispatches.iter().position(|d| d.id == dispatch_id) else {
            return Ok(false);
        };
        let Some(disp) = store.dispatches.get_mut(idx) else {
            return Ok(false);
        };
        if disp.status != DispatchStatus::Active {
            return Ok(false);
        }
        disp.status = DispatchStatus::Failed;
        disp.summary = Some(WORKER_LOST_SUMMARY.to_owned());
        disp.ended_at_ms = Some(now_ms);
        let run_id = disp.run_id.clone();
        let task_id = disp.task_id.clone();
        let worker_name = disp.worker_name.clone();

        if let Some(ref tid) = task_id
            && let Some(task) = store.tasks.iter_mut().find(|t| &t.id == tid)
            && task.status == TaskStatus::Dispatched
            && task.active_dispatch_id.as_deref() == Some(dispatch_id)
        {
            task.status = TaskStatus::Failed;
            task.result_summary = Some(WORKER_LOST_SUMMARY.to_owned());
            task.updated_at_ms = now_ms;
        }

        let already_reported = store.messages.iter().any(|m| {
            m.dispatch_id.as_deref() == Some(dispatch_id)
                && m.to == "coordinator"
                && m.msg_type == MessageType::Escalation
                && m.summary == WORKER_LOST_SUMMARY
        });
        if !already_reported {
            let params = SendParams {
                from: worker_name,
                to: "coordinator".to_owned(),
                msg_type: MessageType::Escalation,
                summary: WORKER_LOST_SUMMARY.to_owned(),
                body: format!(
                    "Dispatch {dispatch_id} lost its terminal after restart without reporting."
                ),
                run_id: Some(run_id),
                task_id,
                dispatch_id: Some(dispatch_id.to_owned()),
                outcome: Some("failed".to_owned()),
                reply_to: None,
            };
            store.send_message(&params)?;
        }
        Ok(true)
    })
}

/// Runs startup terminal-worker recovery against the live daemon [`Registry`].
/// Fast-paths out with zero work if `orchestration.json` does not exist.
pub fn recover_lost_terminal_workers_on_boot(registry: &Registry) {
    let Some(dir) = registry.state_dir() else {
        return;
    };
    if !dir.join(ORCH_FILE).exists() || registry.holder().is_none() {
        return;
    }
    let resume_enabled = read_resume_work_on_restart_setting(&dir);
    let ts = now_ms();
    match recover_lost_terminal_workers_with(
        &dir,
        resume_enabled,
        ts,
        |sid| registry.is_session_live(sid),
        |sid| registry.has_resumable_worker_conversation(sid),
        |old_sid, disp| registry.resume_lost_worker_session(old_sid, disp),
    ) {
        Ok(report) => {
            if !report.resumed.is_empty() || !report.failed_lost.is_empty() {
                tracing::info!(
                    resumed = report.resumed.len(),
                    failed_lost = report.failed_lost.len(),
                    resume_enabled,
                    "completed startup terminal-worker restart recovery"
                );
                registry.notify_orch();
            }
        }
        Err(err) => {
            tracing::warn!(
                kind = ?err.kind(),
                "startup terminal-worker restart recovery failed"
            );
        }
    }
}

/// Core testable recovery engine for lost terminal workers (Orca #46643):
/// - Leaves live workers untouched.
/// - When `resume_work_on_restart == true`, resumable conversation metadata exists, AND this
///   dispatch has not already been resumed once, claims `RestartOfferStage::Resumed` persistently
///   BEFORE calling `resume_fn`, then rebinds `dispatch.session_id` to the new live session.
/// - Otherwise (setting off, no resumable metadata, second loss, or spawn failure), falls back
///   safely to failing the dispatch with `"Worker lost without reporting"`.
pub fn recover_lost_terminal_workers_with<L, C, R>(
    state_dir: &Path,
    resume_work_on_restart: bool,
    now_ms: u64,
    mut is_live: L,
    mut has_resumable_conversation: C,
    mut resume_fn: R,
) -> io::Result<WorkerRecoveryReport>
where
    L: FnMut(SessionId) -> bool,
    C: FnMut(SessionId) -> bool,
    R: FnMut(SessionId, &Dispatch) -> io::Result<SessionId>,
{
    let mut report = WorkerRecoveryReport::default();
    if !state_dir.join(ORCH_FILE).exists() {
        return Ok(report);
    }
    let store = OrchestrationStore::load(state_dir)?;
    let mut ledger = RestartRecoveryLedger::load(state_dir);

    let candidates: Vec<(Dispatch, SessionId, Option<String>)> = store
        .dispatches
        .iter()
        .filter(|d| {
            d.status == DispatchStatus::Active && d.release_state == DispatchReleaseState::Active
        })
        .filter_map(|d| {
            let sid = SessionId(d.session_id?);
            let title = d.task_id.as_deref().and_then(|tid| {
                store
                    .tasks
                    .iter()
                    .find(|t| t.id == tid)
                    .map(|t| t.title.clone())
            });
            Some((d.clone(), sid, title))
        })
        .collect();

    for (dispatch, old_sid, task_title) in candidates {
        if is_live(old_sid) {
            report.still_live.push(dispatch.id);
            continue;
        }

        let already_resumed_once = ledger
            .find_by_dispatch(&dispatch.id)
            .is_some_and(|o| o.stage == RestartOfferStage::Resumed);
        let resumable = has_resumable_conversation(old_sid);

        if resume_work_on_restart && resumable && !already_resumed_once {
            // Claim `Resumed` BEFORE launching so a crash mid-resume never creates a loop.
            ledger.upsert(RestartOffer {
                id: dispatch.id.clone(),
                kind: RestartOfferKind::TerminalWorker,
                session_id: old_sid,
                dispatch_id: Some(dispatch.id.clone()),
                run_id: Some(dispatch.run_id.clone()),
                worker_name: Some(dispatch.worker_name.clone()),
                task_title: task_title.clone(),
                was_working: true,
                stage: RestartOfferStage::Resumed,
                resumed_session_id: None,
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            });
            ledger.save(state_dir)?;

            match resume_fn(old_sid, &dispatch) {
                Ok(new_sid) => {
                    rebind_dispatch_session(state_dir, &dispatch.id, new_sid)?;
                    if let Some(offer) = ledger.find_by_id_mut(&dispatch.id) {
                        offer.resumed_session_id = Some(new_sid);
                        offer.updated_at_ms = now_ms;
                    }
                    ledger.save(state_dir)?;
                    report.resumed.push((dispatch.id, old_sid, new_sid));
                    continue;
                }
                Err(err) => {
                    tracing::warn!(
                        dispatch = dispatch.id.as_str(),
                        kind = ?err.kind(),
                        "terminal worker conversation resume failed; falling back to lost"
                    );
                }
            }
        }

        // Safe fallback: mark dispatch lost/failed and alert coordinator once.
        if fail_lost_worker_dispatch(state_dir, &dispatch.id, now_ms)? {
            ledger.upsert(RestartOffer {
                id: dispatch.id.clone(),
                kind: RestartOfferKind::TerminalWorker,
                session_id: old_sid,
                dispatch_id: Some(dispatch.id.clone()),
                run_id: Some(dispatch.run_id.clone()),
                worker_name: Some(dispatch.worker_name.clone()),
                task_title,
                was_working: false,
                stage: RestartOfferStage::Dismissed,
                resumed_session_id: None,
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            });
            ledger.save(state_dir)?;
            report.failed_lost.push(dispatch.id);
        }
    }

    Ok(report)
}

/// Routes an explicit Resume action for `target`, strictly separating regular chats from
/// active terminal workers using the real [`OrchestrationStore`] (Orca #47284 + #46643).
pub fn handle_restart_resume<FC, FW>(
    state_dir: &Path,
    target: RestartTarget<'_>,
    now_ms: u64,
    mut on_resume_chat: FC,
    mut on_resume_worker: FW,
) -> io::Result<RestartResumeOutcome>
where
    FC: FnMut(SessionId) -> io::Result<SessionId>,
    FW: FnMut(SessionId, &Dispatch) -> io::Result<SessionId>,
{
    let store = if state_dir.join(ORCH_FILE).exists() {
        OrchestrationStore::load(state_dir)?
    } else {
        OrchestrationStore::default()
    };
    let mut ledger = RestartRecoveryLedger::load(state_dir);

    match classify_restart_target(&store, target) {
        RestartOfferKind::Chat => {
            let (offer_id, old_sid) = match target {
                RestartTarget::OfferId(id) => {
                    if let Some(offer) = ledger.find_by_id(id) {
                        if offer.stage != RestartOfferStage::Offered {
                            return Ok(RestartResumeOutcome::AlreadyHandled(id.to_owned()));
                        }
                        (offer.id.clone(), offer.session_id)
                    } else if let Some(sid) = id
                        .strip_prefix("chat:")
                        .and_then(|raw| raw.parse::<u64>().ok())
                    {
                        (id.to_owned(), SessionId(sid))
                    } else {
                        return Ok(RestartResumeOutcome::NotFound(id.to_owned()));
                    }
                }
                RestartTarget::Session(sid) => {
                    let id = format!("chat:{}", sid.0);
                    if let Some(offer) = ledger.find_by_id(&id)
                        && offer.stage != RestartOfferStage::Offered
                    {
                        return Ok(RestartResumeOutcome::AlreadyHandled(id));
                    }
                    (id, sid)
                }
            };
            let new_sid = on_resume_chat(old_sid)?;
            ledger.upsert(RestartOffer {
                id: offer_id.clone(),
                kind: RestartOfferKind::Chat,
                session_id: old_sid,
                dispatch_id: None,
                run_id: None,
                worker_name: None,
                task_title: None,
                was_working: false,
                stage: RestartOfferStage::Resumed,
                resumed_session_id: Some(new_sid),
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            });
            ledger.save(state_dir)?;
            Ok(RestartResumeOutcome::ChatResumed {
                offer_id,
                session_id: new_sid,
            })
        }
        RestartOfferKind::TerminalWorker => {
            let Some(dispatch) = find_owned_active_dispatch(&store, target).cloned() else {
                return Ok(RestartResumeOutcome::NotFound("worker".to_owned()));
            };
            let Some(old_sid) = dispatch.session_id.map(SessionId) else {
                return Ok(RestartResumeOutcome::WorkerFallbackLost {
                    dispatch_id: dispatch.id,
                    reason: "dispatch has no session to resume".to_owned(),
                });
            };
            if ledger
                .find_by_dispatch(&dispatch.id)
                .is_some_and(|o| o.stage != RestartOfferStage::Offered)
            {
                return Ok(RestartResumeOutcome::AlreadyHandled(dispatch.id));
            }
            ledger.upsert(RestartOffer {
                id: dispatch.id.clone(),
                kind: RestartOfferKind::TerminalWorker,
                session_id: old_sid,
                dispatch_id: Some(dispatch.id.clone()),
                run_id: Some(dispatch.run_id.clone()),
                worker_name: Some(dispatch.worker_name.clone()),
                task_title: None,
                was_working: true,
                stage: RestartOfferStage::Resumed,
                resumed_session_id: None,
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            });
            ledger.save(state_dir)?;

            match on_resume_worker(old_sid, &dispatch) {
                Ok(new_sid) => {
                    rebind_dispatch_session(state_dir, &dispatch.id, new_sid)?;
                    if let Some(offer) = ledger.find_by_id_mut(&dispatch.id) {
                        offer.resumed_session_id = Some(new_sid);
                        offer.updated_at_ms = now_ms;
                    }
                    ledger.save(state_dir)?;
                    Ok(RestartResumeOutcome::WorkerResumed {
                        dispatch_id: dispatch.id,
                        old_session: old_sid,
                        new_session: new_sid,
                    })
                }
                Err(err) => {
                    fail_lost_worker_dispatch(state_dir, &dispatch.id, now_ms)?;
                    if let Some(offer) = ledger.find_by_id_mut(&dispatch.id) {
                        offer.stage = RestartOfferStage::Dismissed;
                        offer.updated_at_ms = now_ms;
                    }
                    ledger.save(state_dir)?;
                    Ok(RestartResumeOutcome::WorkerFallbackLost {
                        dispatch_id: dispatch.id,
                        reason: err.to_string(),
                    })
                }
            }
        }
    }
}

/// Routes an explicit Dismiss action for `target`, strictly separating regular chats from
/// active terminal workers so dismissing a regular chat NEVER touches `OrchestrationStore`
/// (Orca #47284 + #46643).
pub fn handle_restart_dismiss(
    state_dir: &Path,
    target: RestartTarget<'_>,
    now_ms: u64,
) -> io::Result<RestartDismissOutcome> {
    let store = if state_dir.join(ORCH_FILE).exists() {
        OrchestrationStore::load(state_dir)?
    } else {
        OrchestrationStore::default()
    };
    let mut ledger = RestartRecoveryLedger::load(state_dir);

    match classify_restart_target(&store, target) {
        RestartOfferKind::Chat => {
            let (offer_id, sid) = match target {
                RestartTarget::OfferId(id) => {
                    if let Some(existing) = ledger.find_by_id(id) {
                        if existing.stage != RestartOfferStage::Offered {
                            return Ok(RestartDismissOutcome::AlreadyHandled(id.to_owned()));
                        }
                        (existing.id.clone(), existing.session_id)
                    } else {
                        let parsed = id
                            .strip_prefix("chat:")
                            .and_then(|raw| raw.parse::<u64>().ok())
                            .unwrap_or(0);
                        (id.to_owned(), SessionId(parsed))
                    }
                }
                RestartTarget::Session(sid) => {
                    let id = format!("chat:{}", sid.0);
                    if let Some(existing) = ledger.find_by_id(&id)
                        && existing.stage != RestartOfferStage::Offered
                    {
                        return Ok(RestartDismissOutcome::AlreadyHandled(id));
                    }
                    (id, sid)
                }
            };
            ledger.upsert(RestartOffer {
                id: offer_id.clone(),
                kind: RestartOfferKind::Chat,
                session_id: sid,
                dispatch_id: None,
                run_id: None,
                worker_name: None,
                task_title: None,
                was_working: false,
                stage: RestartOfferStage::Dismissed,
                resumed_session_id: None,
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            });
            ledger.save(state_dir)?;
            Ok(RestartDismissOutcome::ChatDismissed { offer_id })
        }
        RestartOfferKind::TerminalWorker => {
            let Some(dispatch) = find_owned_active_dispatch(&store, target).cloned() else {
                return Ok(RestartDismissOutcome::AlreadyHandled("worker".to_owned()));
            };
            fail_lost_worker_dispatch(state_dir, &dispatch.id, now_ms)?;
            ledger.upsert(RestartOffer {
                id: dispatch.id.clone(),
                kind: RestartOfferKind::TerminalWorker,
                session_id: SessionId(dispatch.session_id.unwrap_or(0)),
                dispatch_id: Some(dispatch.id.clone()),
                run_id: Some(dispatch.run_id.clone()),
                worker_name: Some(dispatch.worker_name.clone()),
                task_title: None,
                was_working: false,
                stage: RestartOfferStage::Dismissed,
                resumed_session_id: None,
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            });
            ledger.save(state_dir)?;
            Ok(RestartDismissOutcome::WorkerDismissedLost {
                dispatch_id: dispatch.id,
            })
        }
    }
}

fn rebind_dispatch_session(
    state_dir: &Path,
    dispatch_id: &str,
    new_session: SessionId,
) -> io::Result<()> {
    OrchestrationStore::mutate(state_dir, |store| {
        if let Some(disp) = store.dispatches.iter_mut().find(|d| d.id == dispatch_id) {
            disp.session_id = Some(new_session.0);
            let run_id = disp.run_id.clone();
            if let Ok(sid) = u32::try_from(new_session.0) {
                store.session_runs.insert(sid, run_id);
            }
        }
        Ok(())
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(1);

    fn temp_dir(label: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "krilld-rejoin-recovery-{label}-{}-{n}",
            std::process::id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed_active_worker(dir: &Path, session_id: u64) -> (String, String, String) {
        OrchestrationStore::mutate(dir, |s| {
            let run = s.create_run("Bench wave", Some(1))?;
            let task = s.create_task(&run.id, "Implement recovery", "desc", Vec::new())?;
            let disp = s.start_worker(
                &run.id,
                Some(task.id.as_str()),
                "worker-a",
                "claude",
                Some(session_id),
                "/tmp/work",
                "do work",
            )?;
            Ok((run.id, task.id, disp.id))
        })
        .unwrap()
    }

    #[test]
    fn chat_vs_terminal_worker_classification_never_misroutes_regular_chats() {
        let dir = temp_dir("classify");
        let (_run_id, _task_id, disp_id) = seed_active_worker(&dir, 400);
        let store = OrchestrationStore::load(&dir).unwrap();

        // Real active worker matches as TerminalWorker by both dispatch ID and session ID.
        assert_eq!(
            classify_restart_target(&store, RestartTarget::OfferId(&disp_id)),
            RestartOfferKind::TerminalWorker
        );
        assert_eq!(
            classify_restart_target(&store, RestartTarget::Session(SessionId(400))),
            RestartOfferKind::TerminalWorker
        );

        // Regular chat IDs and unknown/missing dispatch IDs MUST ALWAYS classify as Chat!
        assert_eq!(
            classify_restart_target(&store, RestartTarget::OfferId("chat:99")),
            RestartOfferKind::Chat
        );
        assert_eq!(
            classify_restart_target(&store, RestartTarget::OfferId("disp-does-not-exist")),
            RestartOfferKind::Chat
        );
        assert_eq!(
            classify_restart_target(&store, RestartTarget::Session(SessionId(99))),
            RestartOfferKind::Chat
        );

        let mixed = [
            RestartTarget::OfferId("chat:99"),
            RestartTarget::OfferId(&disp_id),
            RestartTarget::Session(SessionId(123)),
            RestartTarget::Session(SessionId(400)),
        ];
        let (chats, workers) = partition_restart_targets(&store, &mixed);
        assert_eq!(chats.len(), 2);
        assert_eq!(workers.len(), 2);

        // Resume and Dismiss on a regular chat invoke chat handler and leave worker untouched.
        let mut chat_resumed = Vec::new();
        let mut worker_resumed = Vec::new();
        let res = handle_restart_resume(
            &dir,
            RestartTarget::OfferId("chat:99"),
            10_000,
            |sid| {
                chat_resumed.push(sid);
                Ok(SessionId(199))
            },
            |sid, d| {
                worker_resumed.push((sid, d.id.clone()));
                Ok(SessionId(500))
            },
        )
        .unwrap();
        assert_eq!(
            res,
            RestartResumeOutcome::ChatResumed {
                offer_id: "chat:99".to_owned(),
                session_id: SessionId(199),
            }
        );
        assert_eq!(chat_resumed, vec![SessionId(99)]);
        assert!(worker_resumed.is_empty());

        let dismiss =
            handle_restart_dismiss(&dir, RestartTarget::Session(SessionId(123)), 10_500).unwrap();
        assert_eq!(
            dismiss,
            RestartDismissOutcome::ChatDismissed {
                offer_id: "chat:123".to_owned(),
            }
        );
        // Active worker dispatch is still Active and has zero lost escalations.
        let after = OrchestrationStore::load(&dir).unwrap();
        assert_eq!(
            after
                .dispatches
                .iter()
                .find(|d| d.id == disp_id)
                .unwrap()
                .status,
            DispatchStatus::Active
        );
        assert!(
            !after
                .messages
                .iter()
                .any(|m| m.summary == WORKER_LOST_SUMMARY)
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn terminal_worker_resumes_when_setting_on_and_falls_back_safely_when_off_or_unresumable() {
        // 1. Setting ON + resumable metadata -> resumes conversation & rebinds dispatch session.
        let dir_on = temp_dir("resume-on");
        let (_run, _task, disp_id) = seed_active_worker(&dir_on, 701);
        let report = recover_lost_terminal_workers_with(
            &dir_on,
            true,
            20_000,
            |_| false,
            |sid| sid == SessionId(701),
            |old, _disp| {
                assert_eq!(old, SessionId(701));
                Ok(SessionId(702))
            },
        )
        .unwrap();
        assert_eq!(
            report.resumed,
            vec![(disp_id.clone(), SessionId(701), SessionId(702))]
        );
        assert!(report.failed_lost.is_empty());
        let store_on = OrchestrationStore::load(&dir_on).unwrap();
        let disp = store_on
            .dispatches
            .iter()
            .find(|d| d.id == disp_id)
            .unwrap();
        assert_eq!(disp.status, DispatchStatus::Active);
        assert_eq!(disp.session_id, Some(702));
        assert!(
            !store_on
                .messages
                .iter()
                .any(|m| m.summary == WORKER_LOST_SUMMARY)
        );

        // Second crash on the SAME dispatch MUST NOT loop; falls back cleanly to lost/failed!
        let second = recover_lost_terminal_workers_with(
            &dir_on,
            true,
            25_000,
            |_| false,
            |_| true,
            |_, _| panic!("second crash on already-resumed dispatch must not loop"),
        )
        .unwrap();
        assert!(second.resumed.is_empty());
        assert_eq!(second.failed_lost, vec![disp_id.clone()]);
        let store_after_second = OrchestrationStore::load(&dir_on).unwrap();
        let disp2 = store_after_second
            .dispatches
            .iter()
            .find(|d| d.id == disp_id)
            .unwrap();
        assert_eq!(disp2.status, DispatchStatus::Failed);
        assert_eq!(disp2.summary.as_deref(), Some(WORKER_LOST_SUMMARY));
        assert_eq!(
            store_after_second
                .messages
                .iter()
                .filter(|m| m.summary == WORKER_LOST_SUMMARY)
                .count(),
            1
        );

        // 2. Setting OFF -> safe fallback to lost/failed even when conversation metadata exists.
        let dir_off = temp_dir("resume-off");
        let (_r, task_off, disp_off) = seed_active_worker(&dir_off, 801);
        let report_off = recover_lost_terminal_workers_with(
            &dir_off,
            false,
            30_000,
            |_| false,
            |_| true,
            |_, _| panic!("must not resume when resume_work_on_restart is off"),
        )
        .unwrap();
        assert!(report_off.resumed.is_empty());
        assert_eq!(report_off.failed_lost, vec![disp_off.clone()]);
        let store_off = OrchestrationStore::load(&dir_off).unwrap();
        assert_eq!(
            store_off
                .dispatches
                .iter()
                .find(|d| d.id == disp_off)
                .unwrap()
                .status,
            DispatchStatus::Failed
        );
        assert_eq!(
            store_off
                .tasks
                .iter()
                .find(|t| t.id == task_off)
                .unwrap()
                .status,
            TaskStatus::Failed
        );

        // 3. Setting ON + NO resumable conversation metadata -> safe fallback to lost/failed.
        let dir_no_meta = temp_dir("no-meta");
        let (_r, _t, disp_no_meta) = seed_active_worker(&dir_no_meta, 901);
        let report_no_meta = recover_lost_terminal_workers_with(
            &dir_no_meta,
            true,
            40_000,
            |_| false,
            |_| false,
            |_, _| panic!("must not attempt resume without resumable metadata"),
        )
        .unwrap();
        assert!(report_no_meta.resumed.is_empty());
        assert_eq!(report_no_meta.failed_lost, vec![disp_no_meta]);

        // 4. Settings readers respect defaults & explicit overrides.
        assert!(!read_resume_work_on_restart_setting(&dir_on));
        assert!(read_chat_auto_continue_setting(&dir_on));
        fs::write(
            dir_on.join("settings.toml"),
            "resume_work_on_restart = true\nchat_auto_continue_after_rate_limit = false\n",
        )
        .unwrap();
        assert!(read_resume_work_on_restart_setting(&dir_on));
        assert!(!read_chat_auto_continue_setting(&dir_on));

        fs::remove_dir_all(&dir_on).ok();
        fs::remove_dir_all(&dir_off).ok();
        fs::remove_dir_all(&dir_no_meta).ok();
    }
}
