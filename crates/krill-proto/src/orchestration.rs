//! Multi-Agent Orchestration, Worktree Checkpoints & References, Local Port Detection,
//! and Built-in Agent Skills Registry (M8: MK-114 ~ MK-118).
//!
//! Shared between `krill` CLI and `krill-app` GUI via `state_dir` atomic files
//! (`orchestration.json` and `worktree-comments.toml`) without extending `krilld` IPC.

#[cfg(unix)]
use crate::os::fs::{DirBuilderExt, OpenOptionsExt};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde::{Deserialize, Serialize};

pub const ORCH_FILE: &str = "orchestration.json";
pub const ORCH_LOCK: &str = "orchestration.lock";
pub const COMMENTS_FILE: &str = "worktree-comments.toml";
pub const COMMENTS_LOCK: &str = "worktree-comments.lock";
pub const MAX_ORCH_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_COMMENTS_BYTES: u64 = 256 * 1024;
/// [`OrchestrationStore::prune_messages`]가 남기는 확인済み 일시 메시지(하트비트·상태) 상한.
/// 근거: 본문 300B 하트비트 3,395건 ≈ 2MiB 실측(PH-02). 500건이면 본문만 ~150KiB라 상한에 여유가 있고,
/// 최근 하트비트 이력은 `check`로 볼 수 있다.
pub const MAX_RETAINED_EPHEMERAL: usize = 500;
/// `krill orchestration check --wait`에서 시간 인자(`--timeout-ms`·`<secs>`)를 생략했을 때의 기본 대기 상한(60초).
pub const DEFAULT_CHECK_WAIT_MS: u64 = 60_000;
/// `krill orchestration check` 대기 시간의 최대 상한(5분).
pub const MAX_CHECK_WAIT_MS: u64 = 300_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Active,
    Completed,
    Archived,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Archived => "archived",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Ready,
    Dispatched,
    Completed,
    Failed,
    Blocked,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Dispatched => "dispatched",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(raw: &str) -> io::Result<Self> {
        match raw.trim() {
            "pending" => Ok(Self::Pending),
            "ready" => Ok(Self::Ready),
            "dispatched" | "in_progress" | "in-progress" => Ok(Self::Dispatched),
            "completed" | "done" | "succeeded" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "blocked" => Ok(Self::Blocked),
            _ => Err(invalid(
                "task status must be pending|ready|dispatched|completed|failed|blocked",
            )),
        }
    }
}

pub const DEFAULT_MAX_WORKER_DEPTH: u32 = 3;

/// Retain action/status extension; older peers receive the preserved-session stopped state.
pub const FEAT_ORCH_RETAIN: u64 = 1 << 19;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchStatus {
    Active,
    Succeeded,
    Failed,
    Stopped,
    Released,
    Abandoned,
    Retained,
}

impl DispatchStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Retained => "retained",
            Self::Released => "released",
            Self::Abandoned => "abandoned",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Status,
    Dispatch,
    WorkerDone,
    Escalation,
    Question,
    Answer,
    Heartbeat,
    DecisionGate,
}

impl MessageType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Dispatch => "dispatch",
            Self::WorkerDone => "worker_done",
            Self::Escalation => "escalation",
            Self::Question => "question",
            Self::Answer => "answer",
            Self::Heartbeat => "heartbeat",
            Self::DecisionGate => "decision_gate",
        }
    }

    pub fn parse(raw: &str) -> io::Result<Self> {
        match raw.trim() {
            "status" => Ok(Self::Status),
            "dispatch" => Ok(Self::Dispatch),
            "worker_done" | "done" => Ok(Self::WorkerDone),
            "escalation" => Ok(Self::Escalation),
            "question" => Ok(Self::Question),
            "answer" => Ok(Self::Answer),
            "heartbeat" => Ok(Self::Heartbeat),
            "decision_gate" | "gate" => Ok(Self::DecisionGate),
            _ => Err(invalid(
                "message type must be status|dispatch|worker_done|escalation|question|answer|heartbeat|decision_gate",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    Pending,
    Resolved,
}

impl GateStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub coordinator_session_id: Option<u64>,
    pub status: RunStatus,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub run_id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub deps: Vec<String>,
    pub status: TaskStatus,
    #[serde(default)]
    pub active_dispatch_id: Option<String>,
    #[serde(default)]
    pub assigned_worker: Option<String>,
    #[serde(default)]
    pub result_summary: Option<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dispatch {
    pub id: String,
    pub run_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub worker_name: String,
    pub agent: String,
    #[serde(default)]
    pub session_id: Option<u64>,
    #[serde(default)]
    pub depth: u32,
    pub cwd: String,
    pub prompt: String,
    pub status: DispatchStatus,
    #[serde(default)]
    pub summary: Option<String>,
    /// Terminal output captured before stopping or releasing the worker, separate from its result.
    #[serde(default)]
    pub archived_output: Option<String>,
    pub started_at_ms: u64,
    #[serde(default)]
    pub ended_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchMessage {
    pub id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub dispatch_id: Option<String>,
    pub from: String,
    pub to: String,
    pub msg_type: MessageType,
    pub summary: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub acked: bool,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionGate {
    pub id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    pub from: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<String>,
    pub status: GateStatus,
    #[serde(default)]
    pub decision: Option<String>,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub resolved_by: Option<String>,
    pub created_at_ms: u64,
    #[serde(default)]
    pub resolved_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationStore {
    #[serde(default)]
    pub next_seq: u64,
    #[serde(default)]
    pub current_run_id: Option<String>,
    #[serde(default)]
    pub runs: Vec<Run>,
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub dispatches: Vec<Dispatch>,
    #[serde(default)]
    pub messages: Vec<OrchMessage>,
    #[serde(default)]
    pub gates: Vec<DecisionGate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendParams {
    pub from: String,
    pub to: String,
    pub msg_type: MessageType,
    pub summary: String,
    pub body: String,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub dispatch_id: Option<String>,
    pub outcome: Option<String>,
    pub reply_to: Option<String>,
}

/// `krill orchestration worker-start --spec <json>` 입력 명세(MK-227).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerStartSpec {
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub worker: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub session_id: Option<u64>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub depth: Option<u32>,
    #[serde(default)]
    pub max_depth: Option<u32>,
    #[serde(default)]
    pub no_spawn: bool,
}

/// `krilld` IPC 기반 멀티에이전트 오케스트레이션 요청(MK-227).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrchAction {
    Snapshot,
    CreateRun {
        title: String,
        coordinator_session_id: Option<u64>,
    },
    SetCurrentRun {
        run_id: Option<String>,
    },
    CreateTask {
        run_id: String,
        title: String,
        description: String,
        deps: Vec<String>,
    },
    UpdateTask {
        task_id: String,
        status: TaskStatus,
        summary: Option<String>,
    },
    StartWorker {
        run_id: String,
        task_id: Option<String>,
        worker_name: String,
        agent: String,
        session_id: Option<u64>,
        cwd: String,
        prompt: String,
        depth: u32,
        max_depth: u32,
    },
    SendMessage(SendParams),
    CheckMessages {
        recipient: Option<String>,
        types: Vec<MessageType>,
        unread_only: bool,
        ack: bool,
        wait_ms: u64,
    },
    WaitAnswer {
        question_id: String,
        wait_ms: u64,
    },
    ReplyMessage {
        question_id: String,
        from: String,
        body: String,
    },
    CreateGate {
        from: String,
        question: String,
        options: Vec<String>,
        run_id: Option<String>,
        task_id: Option<String>,
    },
    ResolveGate {
        gate_id: String,
        decision: String,
        rationale: Option<String>,
        resolved_by: String,
    },
    StopOrReleaseWorker {
        worker_or_dispatch: String,
        release: bool,
        #[serde(default)]
        archived_output: Option<String>,
    },
    AbandonWorker {
        worker_or_dispatch: String,
    },
    Reset {
        run_id: Option<String>,
    },
    RetainWorker {
        worker_or_dispatch: String,
    },
    CheckMessagesFiltered {
        recipient: Option<String>,
        run_id: Option<String>,
        types: Vec<MessageType>,
        unread_only: bool,
        ack: bool,
        wait_ms: u64,
    },
    AckMessageIds {
        recipient: Option<String>,
        run_id: Option<String>,
        types: Vec<MessageType>,
        unread_only: bool,
        ack_ids: Vec<String>,
    },
}

/// `krilld` IPC 기반 멀티에이전트 오케스트레이션 응답(MK-227).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrchResponse {
    Snapshot(OrchestrationStore),
    Run(Run),
    CurrentRun(Option<String>),
    Task(Task),
    Dispatch(Dispatch),
    Messages(Vec<OrchMessage>),
    Message(OrchMessage),
    Answer(Option<OrchMessage>),
    Gate(DecisionGate),
    Reset,
    Error(String),
}

impl OrchResponse {
    /// Keep the appended Retained variant off the wire for N-1 peers.
    #[must_use]
    pub fn for_features(mut self, features: u64) -> Self {
        if features & FEAT_ORCH_RETAIN == 0 {
            let downgrade = |dispatch: &mut Dispatch| {
                if dispatch.status == DispatchStatus::Retained {
                    dispatch.status = DispatchStatus::Stopped;
                }
            };
            match &mut self {
                Self::Snapshot(store) => store.dispatches.iter_mut().for_each(downgrade),
                Self::Dispatch(dispatch) => downgrade(dispatch),
                _ => {}
            }
        }
        self
    }
}

impl OrchestrationStore {
    pub fn load_cached(state_dir: &Path) -> Self {
        type CacheEntry = (
            PathBuf,
            std::time::Instant,
            Option<SystemTime>,
            OrchestrationStore,
        );
        static CACHE: LazyLock<std::sync::Mutex<Option<CacheEntry>>> =
            LazyLock::new(|| std::sync::Mutex::new(None));
        let now = std::time::Instant::now();
        if let Ok(guard) = CACHE.lock()
            && let Some((cached_dir, checked_at, _, cached_store)) = guard.as_ref()
            && cached_dir == state_dir
            && now.duration_since(*checked_at) < Duration::from_millis(200)
        {
            return cached_store.clone();
        }
        let mtime = fs::metadata(state_dir.join(ORCH_FILE))
            .ok()
            .and_then(|m| m.modified().ok());
        if let Ok(mut guard) = CACHE.lock()
            && let Some((cached_dir, checked_at, cached_mtime, cached_store)) = guard.as_mut()
            && cached_dir == state_dir
            && *cached_mtime == mtime
        {
            *checked_at = now;
            return cached_store.clone();
        }
        let loaded = Self::load(state_dir).unwrap_or_default();
        if let Ok(mut guard) = CACHE.lock() {
            *guard = Some((state_dir.to_path_buf(), now, mtime, loaded.clone()));
        }
        loaded
    }

    pub fn load(state_dir: &Path) -> io::Result<Self> {
        let path = state_dir.join(ORCH_FILE);
        let file = match fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let mut raw = String::new();
        file.take(MAX_ORCH_BYTES.saturating_add(1))
            .read_to_string(&mut raw)?;
        if u64::try_from(raw.len()).unwrap_or(u64::MAX) > MAX_ORCH_BYTES {
            return Err(invalid("orchestration.json exceeds size limit"));
        }
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).map_err(invalid)
    }

    pub fn save(&self, state_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(state_dir)?;
        let body = crate::config_document::save_json(&state_dir.join(ORCH_FILE), self)?;
        if u64::try_from(body.len()).unwrap_or(u64::MAX) > MAX_ORCH_BYTES {
            return Err(invalid("orchestration.json exceeds size limit"));
        }
        atomic_write(&state_dir.join(ORCH_FILE), body.as_bytes())
    }

    pub fn mutate<T, F>(state_dir: &Path, f: F) -> io::Result<T>
    where
        F: FnOnce(&mut Self) -> io::Result<T>,
    {
        fs::create_dir_all(state_dir)?;
        let _guard = LockGuard::acquire(&state_dir.join(ORCH_LOCK))?;
        let mut store = Self::load(state_dir)?;
        let out = f(&mut store)?;
        store.save(state_dir)?;
        Ok(out)
    }

    /// Sends an [`OrchAction`] to the local `krilld` socket in `state_dir` if running so that
    /// `krilld`'s `orch_signal` Condvar wakes up waiting `check --wait` / `ask --wait` callers
    /// immediately. Returns `Ok(None)` when no local daemon socket is listening.
    pub fn try_local_daemon_orch(
        state_dir: &Path,
        action: &OrchAction,
        timeout: Duration,
    ) -> io::Result<Option<OrchResponse>> {
        use crate::control::{FEAT_CLI, FEAT_CONTROL, Failure, Reply, Request};
        use crate::ipc::ClientRole;
        use crate::screen::Msg as _;

        let socket = state_dir.join("run/krilld.sock");
        let hello = crate::daemon::client_hello(ClientRole::Cli, "krill-orch");
        let Ok(mut conn) = crate::daemon::handshake(&socket, &hello, Duration::from_secs(2)) else {
            return Ok(None);
        };
        let required = FEAT_CONTROL | FEAT_CLI;
        if conn.negotiated.features & required != required {
            return Ok(None);
        }
        if matches!(action, OrchAction::RetainWorker { .. })
            && conn.negotiated.features & FEAT_ORCH_RETAIN == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "update krilld to retain workers",
            ));
        }
        let io_wait = timeout.saturating_add(Duration::from_secs(5));
        conn.stream.set_read_timeout(Some(io_wait))?;
        conn.stream
            .set_write_timeout(Some(Duration::from_secs(5)))?;
        let encoded = Request::Orchestration(action.clone())
            .encode()
            .map_err(|e| invalid(e.to_string()))?;
        // Once sending begins, failure is ambiguous: never replay a mutation on disk.
        conn.stream.write_all(&encoded)?;
        let (kind, body) =
            crate::ipc::read_frame(&mut conn.stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        let reply = Reply::decode(kind, &body).map_err(|e| invalid(e.to_string()))?;
        match reply {
            Reply::Orchestration(OrchResponse::Error(msg)) => {
                if msg.contains("not found") || msg.contains("no active run") {
                    Err(not_found(msg))
                } else {
                    Err(invalid(msg))
                }
            }
            Reply::Orchestration(resp) => Ok(Some(resp)),
            Reply::Failed(Failure::BadRequest) => Ok(None),
            Reply::Failed(f) => Err(io::Error::other(f.to_string())),
            _ => Err(invalid("unexpected orchestration response")),
        }
    }

    pub fn reply_via_daemon_or_disk(
        state_dir: &Path,
        question_id: &str,
        from: &str,
        body: &str,
    ) -> io::Result<OrchMessage> {
        if let Some(response) = Self::try_local_daemon_orch(
            state_dir,
            &OrchAction::ReplyMessage {
                question_id: question_id.to_owned(),
                from: from.to_owned(),
                body: body.to_owned(),
            },
            Duration::from_secs(2),
        )? {
            return match response {
                OrchResponse::Message(m) => Ok(m),
                _ => Err(invalid("unexpected orchestration response")),
            };
        }
        Self::mutate(state_dir, |s| s.reply_message(question_id, from, body))
    }

    pub fn resolve_gate_via_daemon_or_disk(
        state_dir: &Path,
        gate_id: &str,
        decision: &str,
        rationale: Option<String>,
        resolved_by: &str,
    ) -> io::Result<DecisionGate> {
        if let Some(response) = Self::try_local_daemon_orch(
            state_dir,
            &OrchAction::ResolveGate {
                gate_id: gate_id.to_owned(),
                decision: decision.to_owned(),
                rationale: rationale.clone(),
                resolved_by: resolved_by.to_owned(),
            },
            Duration::from_secs(2),
        )? {
            return match response {
                OrchResponse::Gate(g) => Ok(g),
                _ => Err(invalid("unexpected orchestration response")),
            };
        }
        Self::mutate(state_dir, |s| {
            s.resolve_gate(gate_id, decision, rationale, resolved_by)
        })
    }

    pub fn reset_via_daemon_or_disk(state_dir: &Path, run_id: Option<&str>) -> io::Result<()> {
        if let Some(response) = Self::try_local_daemon_orch(
            state_dir,
            &OrchAction::Reset {
                run_id: run_id.map(str::to_owned),
            },
            Duration::from_secs(2),
        )? {
            return match response {
                OrchResponse::Reset => Ok(()),
                _ => Err(invalid("unexpected orchestration response")),
            };
        }
        Self::mutate(state_dir, |s| {
            s.reset(run_id);
            Ok(())
        })
    }

    fn alloc_id(&mut self, prefix: &str) -> String {
        self.next_seq = self.next_seq.saturating_add(1);
        format!("{prefix}-{}", self.next_seq)
    }

    pub fn create_run(
        &mut self,
        title: &str,
        coordinator_session_id: Option<u64>,
    ) -> io::Result<Run> {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            return Err(invalid("run title must not be empty"));
        }
        let id = self.alloc_id("run");
        let now = now_ms();
        let run = Run {
            id: id.clone(),
            title: trimmed.to_owned(),
            coordinator_session_id,
            status: RunStatus::Active,
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.current_run_id = Some(id);
        self.runs.push(run.clone());
        Ok(run)
    }

    pub fn set_current_run(&mut self, run_id: Option<&str>) -> io::Result<Option<String>> {
        match run_id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(rid) => {
                if !self.runs.iter().any(|r| r.id == rid) {
                    return Err(not_found(format!("run not found: {rid}")));
                }
                self.current_run_id = Some(rid.to_owned());
            }
            None => {
                self.current_run_id = None;
            }
        }
        Ok(self.current_run_id.clone())
    }

    pub fn resolve_run_id(&self, given: Option<&str>) -> io::Result<String> {
        if let Some(rid) = given.map(str::trim).filter(|s| !s.is_empty()) {
            return Ok(rid.to_owned());
        }
        if let Some(ref current) = self.current_run_id
            && self.runs.iter().any(|r| &r.id == current)
        {
            return Ok(current.clone());
        }
        if let Some(active) = self
            .runs
            .iter()
            .rev()
            .find(|r| r.status == RunStatus::Active)
        {
            return Ok(active.id.clone());
        }
        if let Some(last) = self.runs.last() {
            return Ok(last.id.clone());
        }
        Err(invalid("missing --run (and no active run in store)"))
    }

    pub fn create_task(
        &mut self,
        run_id: &str,
        title: &str,
        description: &str,
        deps: Vec<String>,
    ) -> io::Result<Task> {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            return Err(invalid("task title must not be empty"));
        }
        if !self.runs.iter().any(|r| r.id == run_id) {
            return Err(not_found(format!("run not found: {run_id}")));
        }
        let mut clean_deps = Vec::new();
        for dep in deps {
            let d = dep.trim();
            if d.is_empty() {
                continue;
            }
            if !self.tasks.iter().any(|t| t.run_id == run_id && t.id == d) {
                return Err(invalid(format!(
                    "dependency task '{d}' does not exist in run '{run_id}'"
                )));
            }
            if !clean_deps.iter().any(|existing| existing == d) {
                clean_deps.push(d.to_owned());
            }
        }
        let id = self.alloc_id("task");
        let deps_done = clean_deps.iter().all(|dep_id| {
            self.tasks
                .iter()
                .any(|t| t.id == *dep_id && t.status == TaskStatus::Completed)
        });
        let status = if deps_done {
            TaskStatus::Ready
        } else {
            TaskStatus::Pending
        };
        let now = now_ms();
        let task = Task {
            id,
            run_id: run_id.to_owned(),
            title: trimmed.to_owned(),
            description: description.trim().to_owned(),
            deps: clean_deps,
            status,
            active_dispatch_id: None,
            assigned_worker: None,
            result_summary: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        self.tasks.push(task.clone());
        self.reconverge_dag();
        Ok(task)
    }

    pub fn update_task(
        &mut self,
        task_id: &str,
        status: TaskStatus,
        summary: Option<String>,
    ) -> io::Result<Task> {
        let now = now_ms();
        let Some(idx) = self.tasks.iter().position(|t| t.id == task_id) else {
            return Err(not_found(format!("task not found: {task_id}")));
        };
        if let Some(task) = self.tasks.get_mut(idx) {
            task.status = status;
            if let Some(s) = summary
                && !s.trim().is_empty()
            {
                task.result_summary = Some(s.trim().to_owned());
            }
            if matches!(status, TaskStatus::Completed | TaskStatus::Failed) {
                task.active_dispatch_id = None;
            }
            task.updated_at_ms = now;
        }
        self.reconverge_dag();
        self.tasks
            .get(idx)
            .cloned()
            .ok_or_else(|| not_found("task missing after update"))
    }

    pub fn reconverge_dag(&mut self) {
        let now = now_ms();
        let completed_ids: BTreeSet<String> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .map(|t| t.id.clone())
            .collect();
        let failed_ids: BTreeSet<String> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Failed)
            .map(|t| t.id.clone())
            .collect();

        for task in &mut self.tasks {
            if task.status == TaskStatus::Pending || task.status == TaskStatus::Blocked {
                if task.deps.iter().any(|d| failed_ids.contains(d)) {
                    if task.status != TaskStatus::Blocked {
                        task.status = TaskStatus::Blocked;
                        task.updated_at_ms = now;
                    }
                } else if task.deps.iter().all(|d| completed_ids.contains(d)) {
                    task.status = TaskStatus::Ready;
                    task.updated_at_ms = now;
                }
            }
        }

        for run in &mut self.runs {
            if run.status == RunStatus::Archived {
                continue;
            }
            let mut has_tasks = false;
            let mut all_done = true;
            for task in self.tasks.iter().filter(|t| t.run_id == run.id) {
                has_tasks = true;
                if task.status != TaskStatus::Completed {
                    all_done = false;
                    break;
                }
            }
            let next_status = if has_tasks && all_done {
                RunStatus::Completed
            } else {
                RunStatus::Active
            };
            if run.status != next_status {
                run.status = next_status;
                run.updated_at_ms = now;
            }
        }
    }

    /// Validates run/task existence and ready status before spawning a worker.
    /// An active dispatch must be explicitly stopped before another attempt can start.
    pub fn validate_worker_start(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        worker_name: &str,
    ) -> io::Result<()> {
        self.validate_worker_start_with_depth(
            run_id,
            task_id,
            worker_name,
            0,
            DEFAULT_MAX_WORKER_DEPTH,
        )
    }

    pub fn validate_worker_start_with_depth(
        &self,
        run_id: &str,
        task_id: Option<&str>,
        worker_name: &str,
        depth: u32,
        max_depth: u32,
    ) -> io::Result<()> {
        if depth > max_depth {
            return Err(invalid(format!(
                "nested worker depth {depth} exceeds max_depth {max_depth}"
            )));
        }
        if !self.runs.iter().any(|r| r.id == run_id) {
            return Err(not_found(format!("run not found: {run_id}")));
        }
        let worker_trimmed = worker_name.trim();
        if worker_trimmed.is_empty() {
            return Err(invalid("worker name must not be empty"));
        }
        if let Some(tid) = task_id {
            let Some(task) = self.tasks.iter().find(|t| t.id == tid) else {
                return Err(not_found(format!("task not found: {tid}")));
            };
            if task.run_id != run_id {
                return Err(invalid("task does not belong to specified run"));
            }
            if task.status != TaskStatus::Ready || task.active_dispatch_id.is_some() {
                return Err(invalid(format!(
                    "task '{tid}' is '{}' and cannot be dispatched until ready",
                    task.status.as_str()
                )));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn inferred_child_depth(&self, parent_session_id: Option<u64>) -> Option<u32> {
        let sid = parent_session_id?;
        self.dispatches
            .iter()
            .rev()
            .find(|d| d.session_id == Some(sid))
            .map(|d| d.depth.saturating_add(1))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "worker dispatch captures full task/session binding"
    )]
    pub fn start_worker(
        &mut self,
        run_id: &str,
        task_id: Option<&str>,
        worker_name: &str,
        agent: &str,
        session_id: Option<u64>,
        cwd: &str,
        prompt: &str,
    ) -> io::Result<Dispatch> {
        self.start_worker_with_depth(
            run_id,
            task_id,
            worker_name,
            agent,
            session_id,
            cwd,
            prompt,
            0,
            DEFAULT_MAX_WORKER_DEPTH,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "worker dispatch captures full task/session binding and depth guard"
    )]
    pub fn start_worker_with_depth(
        &mut self,
        run_id: &str,
        task_id: Option<&str>,
        worker_name: &str,
        agent: &str,
        session_id: Option<u64>,
        cwd: &str,
        prompt: &str,
        depth: u32,
        max_depth: u32,
    ) -> io::Result<Dispatch> {
        self.validate_worker_start_with_depth(run_id, task_id, worker_name, depth, max_depth)?;
        let worker_trimmed = worker_name.trim();

        let disp_id = self.alloc_id("disp");
        let now = now_ms();
        let dispatch = Dispatch {
            id: disp_id.clone(),
            run_id: run_id.to_owned(),
            task_id: task_id.map(str::to_owned),
            worker_name: worker_trimmed.to_owned(),
            agent: crate::agent::canonical_id(agent).to_owned(),
            session_id,
            depth,
            cwd: cwd.trim().to_owned(),
            prompt: prompt.trim().to_owned(),
            status: DispatchStatus::Active,
            summary: None,
            archived_output: None,
            started_at_ms: now,
            ended_at_ms: None,
        };
        self.dispatches.push(dispatch.clone());

        if let Some(tid) = task_id
            && let Some(task) = self.tasks.iter_mut().find(|t| t.id == tid)
        {
            task.status = TaskStatus::Dispatched;
            task.active_dispatch_id = Some(disp_id.clone());
            task.assigned_worker = Some(worker_trimmed.to_owned());
            task.updated_at_ms = now;
        }

        let msg_id = self.alloc_id("msg");
        self.messages.push(OrchMessage {
            id: msg_id,
            run_id: Some(run_id.to_owned()),
            task_id: task_id.map(str::to_owned),
            dispatch_id: Some(disp_id),
            from: "coordinator".to_owned(),
            to: worker_trimmed.to_owned(),
            msg_type: MessageType::Dispatch,
            summary: format!("Dispatched to {worker_trimmed}"),
            body: prompt.trim().to_owned(),
            reply_to: None,
            acked: false,
            created_at_ms: now,
        });

        self.reconverge_dag();
        Ok(dispatch)
    }

    pub fn resolve_recipients(&self, to: &str) -> Vec<String> {
        let target = to.trim();
        if !target.starts_with('@') {
            return vec![target.to_owned()];
        }
        let mut names = BTreeSet::new();
        for d in &self.dispatches {
            let include = match target {
                "@all" => matches!(d.status, DispatchStatus::Active | DispatchStatus::Succeeded),
                "@idle" => d.status != DispatchStatus::Active,
                "@active" => d.status == DispatchStatus::Active,
                "@claude" => d.agent.eq_ignore_ascii_case("claude"),
                "@codex" => d.agent.eq_ignore_ascii_case("codex"),
                "@opencode" => d.agent.eq_ignore_ascii_case("opencode"),
                "@cursor" => {
                    d.agent.eq_ignore_ascii_case("cursor")
                        || d.agent.eq_ignore_ascii_case("cursor-agent")
                }
                "@agy" | "@antigravity" => {
                    d.agent.eq_ignore_ascii_case("agy")
                        || d.agent.eq_ignore_ascii_case("antigravity")
                }
                "@pi" => d.agent.eq_ignore_ascii_case("pi"),
                _ => false,
            };
            if include {
                names.insert(d.worker_name.clone());
            }
        }
        if names.is_empty() {
            vec![target.to_owned()]
        } else {
            names.into_iter().collect()
        }
    }

    pub fn send_message(&mut self, params: &SendParams) -> io::Result<Vec<OrchMessage>> {
        let from = params.from.trim();
        let to = params.to.trim();
        if from.is_empty() || to.is_empty() {
            return Err(invalid("from and to must not be empty"));
        }
        let summary = params.summary.trim();
        if summary.is_empty() {
            return Err(invalid("message summary must not be empty"));
        }

        let now = now_ms();
        let mut resolved_task_id = params.task_id.clone();
        let mut resolved_run_id = params.run_id.clone();
        let mut resolved_dispatch_id = params.dispatch_id.clone();

        if params.msg_type == MessageType::WorkerDone {
            let failed = match params.outcome.as_deref() {
                None => false, // Preserve the pre-existing omitted-outcome success contract.
                Some(value) if value.eq_ignore_ascii_case("succeeded") => false,
                Some(value)
                    if value.eq_ignore_ascii_case("failed")
                        || value.eq_ignore_ascii_case("error") =>
                {
                    true
                }
                Some(_) => return Err(invalid("worker_done outcome must be succeeded|failed")),
            };
            // 1. If dispatch_id was not explicitly given, infer from active dispatch of `from`.
            if resolved_dispatch_id.is_none() {
                let matching: Vec<&Dispatch> = self
                    .dispatches
                    .iter()
                    .filter(|d| {
                        d.worker_name == from
                            && d.status == DispatchStatus::Active
                            && resolved_run_id.as_ref().is_none_or(|rid| &d.run_id == rid)
                            && resolved_task_id
                                .as_ref()
                                .is_none_or(|tid| d.task_id.as_ref() == Some(tid))
                    })
                    .collect();
                if matching.len() > 1 {
                    return Err(invalid(format!(
                        "worker '{from}' has multiple active dispatches; specify --dispatch or --task explicitly"
                    )));
                }
                if let Some(active_disp) = matching.first() {
                    resolved_dispatch_id = Some(active_disp.id.clone());
                    if resolved_task_id.is_none() {
                        resolved_task_id.clone_from(&active_disp.task_id);
                    }
                    if resolved_run_id.is_none() {
                        resolved_run_id = Some(active_disp.run_id.clone());
                    }
                } else {
                    return Err(invalid(format!(
                        "worker '{from}' has no matching active dispatch"
                    )));
                }
            }

            // 2. If dispatch_id is present, validate its existence, worker ownership, and task binding first.
            if let Some(ref disp_id) = resolved_dispatch_id {
                let Some(disp) = self.dispatches.iter().find(|d| d.id == *disp_id) else {
                    return Err(not_found(format!("dispatch not found: {disp_id}")));
                };
                if disp.worker_name != from {
                    return Err(invalid(format!(
                        "worker '{from}' does not own dispatch '{disp_id}' (owned by '{}')",
                        disp.worker_name
                    )));
                }
                if resolved_task_id.is_some() && resolved_task_id != disp.task_id {
                    return Err(invalid(format!(
                        "dispatch '{disp_id}' task binding does not match the supplied task"
                    )));
                }
                if let Some(ref rid) = resolved_run_id
                    && *rid != disp.run_id
                {
                    return Err(invalid(format!(
                        "dispatch '{disp_id}' belongs to run '{}', not '{rid}'",
                        disp.run_id
                    )));
                }
                if resolved_task_id.is_none() {
                    resolved_task_id.clone_from(&disp.task_id);
                }
                if resolved_run_id.is_none() {
                    resolved_run_id = Some(disp.run_id.clone());
                }
            }

            // 3. Validate task active_dispatch_id authority (both Path A and Path B).
            if let Some(ref tid) = resolved_task_id {
                let Some(task) = self.tasks.iter().find(|t| t.id == *tid) else {
                    return Err(not_found(format!("task not found: {tid}")));
                };
                let Some(ref active_id) = task.active_dispatch_id else {
                    return Err(invalid(format!(
                        "task '{tid}' has no active dispatch and cannot be settled by worker_done"
                    )));
                };
                if resolved_dispatch_id.as_ref() != Some(active_id) {
                    return Err(invalid(format!(
                        "stale dispatch authority: task '{tid}' is owned by '{active_id}', not '{resolved_dispatch_id:?}'"
                    )));
                }
                if resolved_run_id.is_none() {
                    resolved_run_id = Some(task.run_id.clone());
                }
            }

            if let Some(ref disp_id) = resolved_dispatch_id
                && let Some(disp) = self.dispatches.iter_mut().find(|d| d.id == *disp_id)
            {
                if disp.status != DispatchStatus::Active {
                    return Err(invalid(format!(
                        "dispatch '{disp_id}' is no longer active ({})",
                        disp.status.as_str()
                    )));
                }
                disp.status = if failed {
                    DispatchStatus::Failed
                } else {
                    DispatchStatus::Succeeded
                };
                disp.summary = Some(summary.to_owned());
                disp.ended_at_ms = Some(now);
            }

            if let Some(ref tid) = resolved_task_id
                && let Some(task) = self.tasks.iter_mut().find(|t| t.id == *tid)
            {
                task.status = if failed {
                    TaskStatus::Failed
                } else {
                    TaskStatus::Completed
                };
                task.result_summary = Some(summary.to_owned());
                task.active_dispatch_id = None;
                task.updated_at_ms = now;
            }

            self.reconverge_dag();
        }

        let recipients = self.resolve_recipients(to);
        let mut created = Vec::new();
        for rcpt in recipients {
            let msg_id = self.alloc_id("msg");
            let msg = OrchMessage {
                id: msg_id,
                run_id: resolved_run_id.clone(),
                task_id: resolved_task_id.clone(),
                dispatch_id: resolved_dispatch_id.clone(),
                from: from.to_owned(),
                to: rcpt,
                msg_type: params.msg_type,
                summary: summary.to_owned(),
                body: params.body.trim().to_owned(),
                reply_to: params.reply_to.clone(),
                acked: false,
                created_at_ms: now,
            };
            self.messages.push(msg.clone());
            created.push(msg);
        }
        self.prune_messages();
        Ok(created)
    }

    /// 확인済み 일시 메시지(하트비트·상태) 정리. 2MiB 상한(`MAX_ORCH_BYTES`)에 닿으면 이후 모든
    /// 쓰기(`worker_done` 포함)가 실패한다(PH-02) — 새로 쌓기 전에 오래된 것부터 버린다.
    /// 남기는 것: 미확인 메시지 전부, 진행 기록(`worker_done`·`dispatch`·`decision_gate`·
    /// `question`·`answer`·`escalation`). 버리는 것: 확인済み 하트비트·상태 중 최신
    /// [`MAX_RETAINED_EPHEMERAL`]건을 넘는 오래된 것.
    pub fn prune_messages(&mut self) {
        let ephemeral = self.messages.iter().filter(|m| is_ephemeral(m)).count();
        let mut drop = ephemeral.saturating_sub(MAX_RETAINED_EPHEMERAL);
        if drop == 0 {
            return;
        }
        self.messages.retain(|m| {
            if drop > 0 && is_ephemeral(m) {
                drop = drop.saturating_sub(1);
                false
            } else {
                true
            }
        });
    }

    pub fn check_messages(
        &mut self,
        recipient: Option<&str>,
        types: &[MessageType],
        unread_only: bool,
        ack: bool,
    ) -> Vec<OrchMessage> {
        self.check_messages_filtered(recipient, None, types, unread_only, ack)
    }

    pub fn check_messages_filtered(
        &mut self,
        recipient: Option<&str>,
        run_id: Option<&str>,
        types: &[MessageType],
        unread_only: bool,
        ack: bool,
    ) -> Vec<OrchMessage> {
        let mut out = Vec::new();
        for msg in &mut self.messages {
            if let Some(rcpt) = recipient
                && msg.to != rcpt
                && msg.to != "@all"
            {
                continue;
            }
            if let Some(rid) = run_id
                && msg.run_id.as_deref() != Some(rid)
            {
                continue;
            }
            if unread_only && msg.acked {
                continue;
            }
            if !types.is_empty() && !types.contains(&msg.msg_type) {
                continue;
            }
            if ack {
                msg.acked = true;
            }
            out.push(msg.clone());
        }
        out
    }

    pub fn ack_message_ids(
        &mut self,
        recipient: Option<&str>,
        run_id: Option<&str>,
        types: &[MessageType],
        unread_only: bool,
        ack_ids: &[String],
    ) -> io::Result<Vec<OrchMessage>> {
        let mut seen = BTreeSet::new();
        let mut targets = Vec::new();
        for raw in ack_ids {
            let id = raw.trim();
            if id.is_empty() {
                continue;
            }
            if seen.insert(id.to_owned()) {
                targets.push(id.to_owned());
            }
        }
        if targets.is_empty() {
            return Err(invalid("ack_ids must contain at least one message id"));
        }

        let mut matched_indices = Vec::with_capacity(targets.len());
        for id in &targets {
            let Some((idx, msg)) = self.messages.iter().enumerate().find(|(_, m)| &m.id == id)
            else {
                return Err(not_found(format!("message not found: {id}")));
            };
            if let Some(rcpt) = recipient
                && msg.to != rcpt
                && msg.to != "@all"
            {
                return Err(invalid(format!(
                    "message '{id}' does not belong to recipient '{rcpt}'"
                )));
            }
            if let Some(rid) = run_id
                && msg.run_id.as_deref() != Some(rid)
            {
                return Err(invalid(format!(
                    "message '{id}' does not belong to run '{rid}'"
                )));
            }
            if unread_only && msg.acked {
                return Err(invalid(format!("message '{id}' is already acknowledged")));
            }
            if !types.is_empty() && !types.contains(&msg.msg_type) {
                return Err(invalid(format!(
                    "message '{id}' does not match requested message types"
                )));
            }
            matched_indices.push(idx);
        }

        let mut out = Vec::with_capacity(matched_indices.len());
        for idx in matched_indices {
            if let Some(msg) = self.messages.get_mut(idx) {
                msg.acked = true;
                out.push(msg.clone());
            }
        }
        Ok(out)
    }

    pub fn reply_message(
        &mut self,
        question_id: &str,
        from: &str,
        body: &str,
    ) -> io::Result<OrchMessage> {
        let Some(orig) = self.messages.iter().find(|m| m.id == question_id).cloned() else {
            return Err(not_found(format!("message not found: {question_id}")));
        };
        for m in &mut self.messages {
            if m.id == question_id {
                m.acked = true;
            }
        }
        let sent = self.send_message(&SendParams {
            from: from.to_owned(),
            to: orig.from,
            msg_type: MessageType::Answer,
            summary: format!("Reply to {}", orig.id),
            body: body.to_owned(),
            run_id: orig.run_id,
            task_id: orig.task_id,
            dispatch_id: orig.dispatch_id,
            outcome: None,
            reply_to: Some(orig.id),
        })?;
        sent.into_iter()
            .next()
            .ok_or_else(|| invalid("failed to create reply message"))
    }

    pub fn create_gate(
        &mut self,
        from: &str,
        question: &str,
        options: Vec<String>,
        run_id: Option<String>,
        task_id: Option<String>,
    ) -> io::Result<DecisionGate> {
        let q = question.trim();
        if q.is_empty() {
            return Err(invalid("gate question must not be empty"));
        }
        let id = self.alloc_id("gate");
        let now = now_ms();
        let gate = DecisionGate {
            id: id.clone(),
            run_id: run_id.clone(),
            task_id: task_id.clone(),
            from: from.trim().to_owned(),
            question: q.to_owned(),
            options,
            status: GateStatus::Pending,
            decision: None,
            rationale: None,
            resolved_by: None,
            created_at_ms: now,
            resolved_at_ms: None,
        };
        self.gates.push(gate.clone());
        let msg_id = self.alloc_id("msg");
        self.messages.push(OrchMessage {
            id: msg_id,
            run_id,
            task_id,
            dispatch_id: None,
            from: from.trim().to_owned(),
            to: "coordinator".to_owned(),
            msg_type: MessageType::DecisionGate,
            summary: format!("[Gate {id}] {q}"),
            body: q.to_owned(),
            reply_to: None,
            acked: false,
            created_at_ms: now,
        });
        Ok(gate)
    }

    pub fn resolve_gate(
        &mut self,
        gate_id: &str,
        decision: &str,
        rationale: Option<String>,
        resolved_by: &str,
    ) -> io::Result<DecisionGate> {
        let dec = decision.trim();
        if dec.is_empty() {
            return Err(invalid("decision must not be empty"));
        }
        let now = now_ms();
        let Some(idx) = self.gates.iter().position(|g| g.id == gate_id) else {
            return Err(not_found(format!("gate not found: {gate_id}")));
        };
        let (from, run_id, task_id) = {
            let Some(gate) = self.gates.get_mut(idx) else {
                return Err(not_found("gate missing"));
            };
            if gate.status == GateStatus::Resolved {
                return Err(invalid(format!("gate '{gate_id}' is already resolved")));
            }
            let matched_option = if gate.options.is_empty() {
                dec.to_owned()
            } else if let Some(opt) = gate
                .options
                .iter()
                .find(|opt| opt.eq_ignore_ascii_case(dec))
            {
                opt.clone()
            } else {
                return Err(invalid(format!(
                    "decision '{dec}' is not in gate options ({})",
                    gate.options.join(", ")
                )));
            };
            gate.status = GateStatus::Resolved;
            gate.decision = Some(matched_option);
            gate.rationale.clone_from(&rationale);
            gate.resolved_by = Some(resolved_by.trim().to_owned());
            gate.resolved_at_ms = Some(now);
            (gate.from.clone(), gate.run_id.clone(), gate.task_id.clone())
        };
        let msg_id = self.alloc_id("msg");
        self.messages.push(OrchMessage {
            id: msg_id,
            run_id,
            task_id,
            dispatch_id: None,
            from: resolved_by.trim().to_owned(),
            to: from,
            msg_type: MessageType::Answer,
            summary: format!("[Gate {gate_id} Resolved] {dec}"),
            body: rationale.unwrap_or_else(|| dec.to_owned()),
            reply_to: Some(gate_id.to_owned()),
            acked: false,
            created_at_ms: now,
        });
        self.gates
            .get(idx)
            .cloned()
            .ok_or_else(|| not_found("gate missing"))
    }

    pub fn stop_or_release_worker(
        &mut self,
        worker_or_dispatch: &str,
        release: bool,
    ) -> io::Result<Dispatch> {
        let now = now_ms();
        let Some(idx) = self
            .dispatches
            .iter()
            .rposition(|d| d.id == worker_or_dispatch || d.worker_name == worker_or_dispatch)
        else {
            return Err(not_found(format!(
                "worker or dispatch not found: {worker_or_dispatch}"
            )));
        };
        let (disp_id, task_id) = {
            let Some(disp) = self.dispatches.get_mut(idx) else {
                return Err(not_found("dispatch missing"));
            };
            if release {
                disp.status = DispatchStatus::Released;
            } else if disp.status == DispatchStatus::Active {
                disp.status = DispatchStatus::Stopped;
            }
            disp.ended_at_ms = Some(now);
            (disp.id.clone(), disp.task_id.clone())
        };
        if let Some(tid) = task_id
            && let Some(task) = self.tasks.iter_mut().find(|t| t.id == tid)
            && task.active_dispatch_id.as_deref() == Some(disp_id.as_str())
        {
            task.active_dispatch_id = None;
            if task.status == TaskStatus::Dispatched {
                task.status = TaskStatus::Ready;
            }
            task.updated_at_ms = now;
        }
        self.reconverge_dag();
        self.dispatches
            .get(idx)
            .cloned()
            .ok_or_else(|| not_found("dispatch missing"))
    }

    /// Marks a worker dispatch as `Retained` so its live terminal session is kept open for
    /// inspection/debugging while clearing any active task dispatch lock.
    pub fn retain_worker(&mut self, worker_or_dispatch: &str) -> io::Result<Dispatch> {
        let now = now_ms();
        let Some(idx) = self
            .dispatches
            .iter()
            .rposition(|d| d.id == worker_or_dispatch || d.worker_name == worker_or_dispatch)
        else {
            return Err(not_found(format!(
                "worker or dispatch not found: {worker_or_dispatch}"
            )));
        };
        let (disp_id, task_id) = {
            let Some(disp) = self.dispatches.get_mut(idx) else {
                return Err(not_found("dispatch missing"));
            };
            disp.status = DispatchStatus::Retained;
            if disp.ended_at_ms.is_none() {
                disp.ended_at_ms = Some(now);
            }
            (disp.id.clone(), disp.task_id.clone())
        };
        if let Some(tid) = task_id
            && let Some(task) = self.tasks.iter_mut().find(|t| t.id == tid)
            && task.active_dispatch_id.as_deref() == Some(disp_id.as_str())
        {
            task.active_dispatch_id = None;
            if task.status == TaskStatus::Dispatched {
                task.status = TaskStatus::Ready;
            }
            task.updated_at_ms = now;
        }
        self.reconverge_dag();
        self.dispatches
            .get(idx)
            .cloned()
            .ok_or_else(|| not_found("dispatch missing"))
    }

    /// Fences a dead/unresponsive worker dispatch without waiting on its PTY session,
    /// returning the bound task to `Ready` so another worker can be dispatched while
    /// rejecting any late `worker_done` from the abandoned dispatch (MK-227).
    pub fn abandon_worker(&mut self, worker_or_dispatch: &str) -> io::Result<Dispatch> {
        let now = now_ms();
        let Some(idx) = self
            .dispatches
            .iter()
            .rposition(|d| d.id == worker_or_dispatch || d.worker_name == worker_or_dispatch)
        else {
            return Err(not_found(format!(
                "worker or dispatch not found: {worker_or_dispatch}"
            )));
        };
        let (disp_id, task_id) = {
            let Some(disp) = self.dispatches.get_mut(idx) else {
                return Err(not_found("dispatch missing"));
            };
            disp.status = DispatchStatus::Abandoned;
            disp.ended_at_ms = Some(now);
            (disp.id.clone(), disp.task_id.clone())
        };
        if let Some(tid) = task_id
            && let Some(task) = self.tasks.iter_mut().find(|t| t.id == tid)
            && task.active_dispatch_id.as_deref() == Some(disp_id.as_str())
        {
            task.active_dispatch_id = None;
            task.assigned_worker = None;
            if task.status == TaskStatus::Dispatched {
                task.status = TaskStatus::Ready;
            }
            task.updated_at_ms = now;
        }
        self.reconverge_dag();
        self.dispatches
            .get(idx)
            .cloned()
            .ok_or_else(|| not_found("dispatch missing"))
    }

    pub fn reset(&mut self, run_id: Option<&str>) {
        if let Some(rid) = run_id {
            if self.current_run_id.as_deref() == Some(rid) {
                self.current_run_id = None;
            }
            self.runs.retain(|r| r.id != rid);
            self.tasks.retain(|t| t.run_id != rid);
            self.dispatches.retain(|d| d.run_id != rid);
            self.messages.retain(|m| m.run_id.as_deref() != Some(rid));
            self.gates.retain(|g| g.run_id.as_deref() != Some(rid));
        } else {
            let next_seq = self.next_seq;
            *self = Self {
                next_seq,
                ..Self::default()
            };
        }
    }
}

pub fn worker_preamble(dispatch: &Dispatch) -> String {
    let task_flag = dispatch
        .task_id
        .as_deref()
        .map(|t| format!(" --task {t}"))
        .unwrap_or_default();
    format!(
        "[MK ORCHESTRATION PREAMBLE]\n\
         Run: {run} | Dispatch: {disp} | Worker: {worker}{task_line}\n\
         Task Prompt: {prompt}\n\
         First read the Krill worker contract: krill skills get orchestration --reference references/worker-contract.md. If KRILL_CLI is set, use that absolute executable instead of bare krill; quote it for your shell. This is the krill-orchestration skill, not Orca orchestration.\n\
         IMPORTANT: Always use the `krill orchestration` CLI via your shell/command tool to report status or ask questions; do NOT use built-in subagent tools (like `send_message`, `Task`, or `spawn_agent`).\n\
         When finished, report completion with:\n  \
         krill orchestration send --from {worker} --to coordinator --type worker_done --run {run}{task_flag} --dispatch {disp} --outcome succeeded --summary \"<result>\"\n\
         If blocked or need clarification (blocks until answered):\n  \
         krill orchestration ask --from {worker} --to coordinator --run {run} --wait --summary \"<question>\"",
        run = dispatch.run_id,
        disp = dispatch.id,
        worker = dispatch.worker_name,
        task_line = dispatch
            .task_id
            .as_deref()
            .map(|t| format!(" | Task: {t}"))
            .unwrap_or_default(),
        prompt = dispatch.prompt,
    )
}

// ============================================================================
// Worktree Checkpoints & References (MK-093 + MK-116)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkspaceStatus {
    Todo,
    InProgress,
    InReview,
    Completed,
}

impl WorkspaceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::InProgress => "in-progress",
            Self::InReview => "in-review",
            Self::Completed => "completed",
        }
    }

    pub fn badge_label(self) -> &'static str {
        match self {
            Self::Todo => "TODO",
            Self::InProgress => "WIP",
            Self::InReview => "REVIEW",
            Self::Completed => "DONE",
        }
    }

    pub fn parse(raw: &str) -> io::Result<Self> {
        match raw.trim() {
            "todo" => Ok(Self::Todo),
            "in-progress" | "in_progress" | "wip" => Ok(Self::InProgress),
            "in-review" | "in_review" | "review" => Ok(Self::InReview),
            "completed" | "done" => Ok(Self::Completed),
            _ => Err(invalid(
                "workspace status must be todo|in-progress|in-review|completed",
            )),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeStore {
    #[serde(default)]
    pub comments: BTreeMap<String, String>,
    #[serde(default)]
    pub updated_at: BTreeMap<String, u64>,
    #[serde(default)]
    pub statuses: BTreeMap<String, String>,
    #[serde(default)]
    pub unread: BTreeMap<String, bool>,
    #[serde(default)]
    pub prs: BTreeMap<String, u64>,
    #[serde(default)]
    pub issues: BTreeMap<String, String>,
    #[serde(default)]
    pub references: BTreeMap<String, Vec<String>>,
    /// Per-repository relative symlink paths shared from the main worktree (MK-174).
    #[serde(default)]
    pub symlinks: BTreeMap<String, Vec<String>>,
    /// Per-repository setup script command configured in Settings or CLI (MK-174).
    #[serde(default)]
    pub setup_scripts: BTreeMap<String, String>,
    /// Per-repository archive script command configured in Settings or CLI (MK-174).
    #[serde(default)]
    pub archive_scripts: BTreeMap<String, String>,
    /// Per-repository flag indicating the setup script prompt card was dismissed (MK-174).
    #[serde(default)]
    pub setup_prompt_dismissed: BTreeMap<String, bool>,
    #[serde(default)]
    pub parents: BTreeMap<String, String>,
    /// `krill worktree cleanup`가 읽는 워크트리별 PR 상태(`merged`·`closed`·`open`).
    #[serde(default)]
    pub pr_states: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCheckpoint {
    pub worktree: String,
    pub comment: Option<String>,
    pub workspace_status: Option<String>,
    pub unread: bool,
    pub pr: Option<u64>,
    pub issue: Option<String>,
    pub references: Vec<String>,
    pub parent: Option<String>,
    pub updated_at_ms: Option<u64>,
}

/// Validates and normalizes a repository-relative symlink path (MK-174).
///
/// Rejects empty paths, absolute paths, backslashes, control characters, `.` or `..` traversal
/// segments, and `.git` components.
pub fn validate_symlink_path(raw: &str) -> io::Result<String> {
    let trimmed = raw.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || trimmed.len() > 512 || trimmed.chars().any(char::is_control) {
        return Err(invalid(
            "symlink path must be 1..=512 printable relative path characters",
        ));
    }
    if trimmed.starts_with(['/', '\\']) || trimmed.contains('\\') {
        return Err(invalid(
            "symlink path must be a relative POSIX path inside the repository",
        ));
    }
    let mut parts = Vec::new();
    for part in trimmed.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(invalid(
                "symlink path must not contain '.' or '..' traversal segments",
            ));
        }
        if part == ".git" {
            return Err(invalid("symlink path must not target '.git'"));
        }
        parts.push(part);
    }
    Ok(parts.join("/"))
}

/// Normalizes `cwd` to its canonical git worktree/repository root if `cwd` is inside a git
/// repository (walking `ancestors()` for a `.git` directory or file) so subdirectory calls
/// share the same `WorktreeStore` key as the repo root.
#[must_use]
pub fn worktree_key(cwd: &Path) -> String {
    let path = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let root = path
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .unwrap_or(path.as_path());
    root.to_string_lossy().into_owned()
}

fn normalize_store_key(key: &str) -> String {
    worktree_key(Path::new(key))
}

type WorktreeStoreCacheEntry = (
    PathBuf,
    std::time::Instant,
    Option<SystemTime>,
    WorktreeStore,
);

static WORKTREE_STORE_CACHE: LazyLock<std::sync::Mutex<Option<WorktreeStoreCacheEntry>>> =
    LazyLock::new(|| std::sync::Mutex::new(None));

impl WorktreeStore {
    pub fn load_cached(state_dir: &Path) -> Self {
        let now = std::time::Instant::now();
        if let Ok(guard) = WORKTREE_STORE_CACHE.lock()
            && let Some((cached_dir, checked_at, _, cached_store)) = guard.as_ref()
            && cached_dir == state_dir
            && now.duration_since(*checked_at) < Duration::from_millis(200)
        {
            return cached_store.clone();
        }
        let mtime = fs::metadata(state_dir.join(COMMENTS_FILE))
            .ok()
            .and_then(|m| m.modified().ok());
        if let Ok(mut guard) = WORKTREE_STORE_CACHE.lock()
            && let Some((cached_dir, checked_at, cached_mtime, cached_store)) = guard.as_mut()
            && cached_dir == state_dir
            && *cached_mtime == mtime
        {
            *checked_at = now;
            return cached_store.clone();
        }
        let loaded = Self::load(state_dir).unwrap_or_default();
        if let Ok(mut guard) = WORKTREE_STORE_CACHE.lock() {
            *guard = Some((state_dir.to_path_buf(), now, mtime, loaded.clone()));
        }
        loaded
    }

    pub fn load(state_dir: &Path) -> io::Result<Self> {
        let path = state_dir.join(COMMENTS_FILE);
        let mut bytes = Vec::new();
        match fs::File::open(&path) {
            Ok(f) => {
                f.take(MAX_COMMENTS_BYTES.saturating_add(1))
                    .read_to_end(&mut bytes)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        }
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_COMMENTS_BYTES {
            return corrupt_comments_store(&path, "worktree-comments.toml exceeds size limit");
        }
        let Ok(raw) = std::str::from_utf8(&bytes) else {
            return corrupt_comments_store(&path, "worktree-comments.toml is not UTF-8");
        };
        toml::from_str(raw).or_else(|e| corrupt_comments_store(&path, &e.to_string()))
    }

    pub fn save(&self, state_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(state_dir)?;
        let body = crate::config_document::save_toml(&state_dir.join(COMMENTS_FILE), self)?;
        if u64::try_from(body.len()).unwrap_or(u64::MAX) > MAX_COMMENTS_BYTES {
            return Err(invalid("worktree-comments.toml exceeds size limit"));
        }
        atomic_write(&state_dir.join(COMMENTS_FILE), body.as_bytes())?;
        if let Ok(mut guard) = WORKTREE_STORE_CACHE.lock() {
            *guard = None;
        }
        Ok(())
    }

    pub fn mutate<T, F>(state_dir: &Path, f: F) -> io::Result<T>
    where
        F: FnOnce(&mut Self) -> io::Result<T>,
    {
        let mut dir = fs::DirBuilder::new();
        dir.recursive(true);
        #[cfg(unix)]
        dir.mode(0o700);
        dir.create(state_dir)?;
        let _guard = LockGuard::acquire(&state_dir.join(COMMENTS_LOCK))?;
        let mut store = Self::load(state_dir)?;
        let out = f(&mut store)?;
        store.save(state_dir)?;
        Ok(out)
    }

    pub fn checkpoint(&self, key: &str) -> WorktreeCheckpoint {
        let key = normalize_store_key(key);
        WorktreeCheckpoint {
            comment: self.comments.get(&key).cloned(),
            workspace_status: self.statuses.get(&key).cloned(),
            unread: self.unread.get(&key).copied().unwrap_or(false),
            pr: self.prs.get(&key).copied(),
            issue: self.issues.get(&key).cloned(),
            references: self.references.get(&key).cloned().unwrap_or_default(),
            parent: self.parents.get(&key).cloned(),
            updated_at_ms: self.updated_at.get(&key).copied(),
            worktree: key,
        }
    }

    pub fn can_assign_parent(&self, child: &str, candidate_parent: &str) -> bool {
        let child = normalize_store_key(child);
        let candidate = normalize_store_key(candidate_parent);
        if child.is_empty() || candidate.is_empty() || child == candidate {
            return false;
        }
        let mut cursor = Some(candidate);
        let mut visited = BTreeSet::new();
        while let Some(cur) = cursor {
            if cur == child || !visited.insert(cur.clone()) {
                return false;
            }
            cursor = self.parent_of(&cur);
        }
        true
    }

    /// 정규화된 `key`의 부모를 찾는다. 정규화 전 별칭(`/var`↔`/private/var`)으로 저장된 구 항목도 찾는다.
    /// 놓치면 모든 항목을 canonicalize하므로 쓰기 경로에서만 쓴다(`checkpoint`는 정확 일치만).
    pub fn parent_of(&self, key: &str) -> Option<String> {
        self.parents
            .get(key)
            .or_else(|| {
                self.parents
                    .iter()
                    .find(|(k, _)| normalize_store_key(k) == key)
                    .map(|(_, v)| v)
            })
            .map(|v| normalize_store_key(v))
    }

    pub fn set_parent(&mut self, child: &str, parent: Option<&str>) -> io::Result<()> {
        let child_key = normalize_store_key(child);
        if let Some(parent_raw) = parent.map(str::trim).filter(|s| !s.is_empty()) {
            let parent_key = normalize_store_key(parent_raw);
            if !self.can_assign_parent(&child_key, &parent_key) {
                return Err(invalid(
                    "cannot assign worktree as its own parent or create a lineage cycle",
                ));
            }
            self.parents
                .retain(|k, _| normalize_store_key(k) != child_key);
            self.parents.insert(child_key.clone(), parent_key);
            self.updated_at.insert(child_key, now_secs());
        } else {
            self.parents
                .retain(|k, _| normalize_store_key(k) != child_key);
            self.updated_at.insert(child_key, now_secs());
        }
        Ok(())
    }

    pub fn remove_worktree_lineage(&mut self, removed: &str) {
        let removed_key = normalize_store_key(removed);
        self.parents.retain(|c, p| {
            c != removed
                && p != removed
                && normalize_store_key(c) != removed_key
                && normalize_store_key(p) != removed_key
        });
    }

    pub fn add_reference(&mut self, key: &str, url: &str) -> io::Result<Vec<String>> {
        let key = normalize_store_key(key);
        let trimmed = url.trim();
        if trimmed.is_empty() || trimmed.len() > 1024 || trimmed.chars().any(char::is_control) {
            return Err(invalid("reference must be 1..=1024 printable characters"));
        }
        let list = self.references.entry(key.clone()).or_default();
        if !list.iter().any(|u| u == trimmed) {
            list.push(trimmed.to_owned());
        }
        self.updated_at.insert(key, now_secs());
        Ok(list.clone())
    }

    pub fn remove_reference(&mut self, key: &str, url: &str) -> Vec<String> {
        let key = normalize_store_key(key);
        let trimmed = url.trim();
        if let Some(list) = self.references.get_mut(&key) {
            list.retain(|u| u != trimmed);
            let out = list.clone();
            if list.is_empty() {
                self.references.remove(&key);
            }
            self.updated_at.insert(key, now_secs());
            out
        } else {
            Vec::new()
        }
    }

    /// 검색 결과는 저장소를 빌리며 검색어는 내부에서 소유한다. 키·URL의 저장 순서를 유지한다.
    pub fn reference_matches<'store>(
        &'store self,
        query: &str,
    ) -> impl Iterator<Item = (&'store str, &'store str)> + use<'store> {
        let q = query.trim().to_ascii_lowercase();
        self.references
            .iter()
            .flat_map(|(wt, urls)| urls.iter().map(move |url| (wt.as_str(), url.as_str())))
            .filter(move |(wt, url)| {
                q.is_empty()
                    || url.to_ascii_lowercase().contains(&q)
                    || wt.to_ascii_lowercase().contains(&q)
            })
    }

    /// 호출자가 소유 결과를 필요로 할 때 쓰는 호환 API.
    pub fn find_references(&self, query: &str) -> Vec<(String, String)> {
        self.reference_matches(query)
            .map(|(wt, url)| (wt.to_owned(), url.to_owned()))
            .collect()
    }

    pub fn symlinks_for(&self, key: &str) -> Vec<String> {
        let norm = normalize_store_key(key);
        self.symlinks
            .get(&norm)
            .or_else(|| self.symlinks.get(key))
            .cloned()
            .unwrap_or_default()
    }

    pub fn add_symlink(&mut self, key: &str, raw_path: &str) -> io::Result<Vec<String>> {
        let clean = validate_symlink_path(raw_path)?;
        let key = normalize_store_key(key);
        let list = self.symlinks.entry(key.clone()).or_default();
        if !list.iter().any(|p| p == &clean) {
            list.push(clean);
        }
        self.updated_at.insert(key, now_secs());
        Ok(list.clone())
    }

    pub fn remove_symlink(&mut self, key: &str, raw_path: &str) -> Vec<String> {
        let key = normalize_store_key(key);
        let clean = validate_symlink_path(raw_path).unwrap_or_else(|_| raw_path.trim().to_owned());
        if let Some(list) = self.symlinks.get_mut(&key) {
            list.retain(|p| p != &clean && p != raw_path.trim());
            let out = list.clone();
            if list.is_empty() {
                self.symlinks.remove(&key);
            }
            self.updated_at.insert(key, now_secs());
            out
        } else {
            Vec::new()
        }
    }

    pub fn setup_script_for(&self, key: &str) -> Option<String> {
        let norm = normalize_store_key(key);
        self.setup_scripts
            .get(&norm)
            .or_else(|| self.setup_scripts.get(key))
            .cloned()
            .filter(|s| !s.trim().is_empty())
    }

    pub fn set_setup_script(&mut self, key: &str, script: &str) -> io::Result<Option<String>> {
        let key = normalize_store_key(key);
        let trimmed = script.trim();
        if trimmed.len() > 4096 {
            return Err(invalid("setup script exceeds 4096 characters"));
        }
        self.setup_prompt_dismissed.remove(&key);
        self.updated_at.insert(key.clone(), now_secs());
        if trimmed.is_empty() {
            self.setup_scripts.remove(&key);
            Ok(None)
        } else {
            self.setup_scripts.insert(key, trimmed.to_owned());
            Ok(Some(trimmed.to_owned()))
        }
    }

    pub fn archive_script_for(&self, key: &str) -> Option<String> {
        let norm = normalize_store_key(key);
        self.archive_scripts
            .get(&norm)
            .or_else(|| self.archive_scripts.get(key))
            .cloned()
            .filter(|s| !s.trim().is_empty())
    }

    pub fn set_archive_script(&mut self, key: &str, script: &str) -> io::Result<Option<String>> {
        let key = normalize_store_key(key);
        let trimmed = script.trim();
        if trimmed.len() > 4096 {
            return Err(invalid("archive script exceeds 4096 characters"));
        }
        self.updated_at.insert(key.clone(), now_secs());
        if trimmed.is_empty() {
            self.archive_scripts.remove(&key);
            Ok(None)
        } else {
            self.archive_scripts.insert(key, trimmed.to_owned());
            Ok(Some(trimmed.to_owned()))
        }
    }

    pub fn is_setup_prompt_dismissed(&self, key: &str) -> bool {
        let norm = normalize_store_key(key);
        self.setup_prompt_dismissed
            .get(&norm)
            .or_else(|| self.setup_prompt_dismissed.get(key))
            .copied()
            .unwrap_or(false)
    }

    pub fn set_setup_prompt_dismissed(&mut self, key: &str, dismissed: bool) {
        let key = normalize_store_key(key);
        if dismissed {
            self.setup_prompt_dismissed.insert(key.clone(), true);
        } else {
            self.setup_prompt_dismissed.remove(&key);
        }
        self.updated_at.insert(key, now_secs());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoHookSummary {
    pub repo_key: String,
    pub effective_setup: Option<String>,
    pub effective_archive: Option<String>,
    pub native_setup: Option<String>,
    pub native_archive: Option<String>,
    pub native_setup_disabled: bool,
    pub native_archive_disabled: bool,
    pub orca_setup: Option<String>,
    pub orca_archive: Option<String>,
    pub store_setup: Option<String>,
    pub store_archive: Option<String>,
    pub suggested_setup: Option<String>,
    pub suggested_source: Option<String>,
    pub prompt_dismissed: bool,
    pub native_symlinks: Vec<String>,
    pub orca_symlinks: Vec<String>,
    pub store_symlinks: Vec<String>,
    pub effective_symlinks: Vec<String>,
}

impl RepoHookSummary {
    #[must_use]
    pub fn should_show_setup_prompt(&self) -> bool {
        !self.native_setup_disabled
            && self.native_setup.is_none()
            && self.store_setup.is_none()
            && !self.prompt_dismissed
            && self.suggested_setup.is_some()
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct NativeTomlDoc {
    worktree: NativeWorktreeSection,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct NativeWorktreeSection {
    setup: Option<String>,
    archive: Option<String>,
    symlinks: Vec<String>,
    shared_directories: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OrcaYamlDoc {
    scripts: OrcaScriptsSection,
    worktree: OrcaWorktreeSection,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct OrcaScriptsSection {
    setup: Option<String>,
    archive: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct OrcaWorktreeSection {
    #[serde(alias = "shared_directories")]
    shared_directories: Vec<String>,
    #[serde(default)]
    symlinks: Vec<String>,
}

fn read_small_utf8(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut raw = String::new();
    file.take(MAX_COMMENTS_BYTES.saturating_add(1))
        .read_to_string(&mut raw)
        .ok()?;
    if u64::try_from(raw.len()).unwrap_or(u64::MAX) > MAX_COMMENTS_BYTES {
        return None;
    }
    Some(raw)
}

fn normalize_symlink_list(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        let trimmed = item.trim().trim_end_matches('/').to_owned();
        if !trimmed.is_empty() && !out.contains(&trimmed) {
            out.push(trimmed);
        }
    }
    out
}

/// Inspects a repository's `krill.toml`, `orca.yaml`, `package.json`, and `WorktreeStore`
/// to summarize configured and suggested `setup` / `archive` hooks and worktree symlinks (MK-174).
#[must_use]
pub fn inspect_repo_hooks(repo: &Path, state_dir: Option<&Path>) -> RepoHookSummary {
    let repo_key = worktree_key(repo);
    let root = Path::new(&repo_key);

    let native_doc = read_small_utf8(&crate::user_dirs::repo_config(root))
        .and_then(|s| toml::from_str::<NativeTomlDoc>(&s).ok())
        .unwrap_or_default();
    let native_setup_disabled = native_doc
        .worktree
        .setup
        .as_ref()
        .is_some_and(|s| s.trim().is_empty());
    let native_archive_disabled = native_doc
        .worktree
        .archive
        .as_ref()
        .is_some_and(|s| s.trim().is_empty());
    let native_setup = native_doc
        .worktree
        .setup
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let native_archive = native_doc
        .worktree
        .archive
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let native_symlinks = normalize_symlink_list(
        native_doc
            .worktree
            .symlinks
            .into_iter()
            .chain(native_doc.worktree.shared_directories),
    );

    let orca_doc = read_small_utf8(&root.join("orca.yaml"))
        .and_then(|s| serde_saphyr::from_str::<OrcaYamlDoc>(&s).ok())
        .unwrap_or_default();
    let orca_setup = orca_doc
        .scripts
        .setup
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let orca_archive = orca_doc
        .scripts
        .archive
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let orca_symlinks = normalize_symlink_list(
        orca_doc
            .worktree
            .shared_directories
            .into_iter()
            .chain(orca_doc.worktree.symlinks),
    );

    let store = state_dir
        .map(WorktreeStore::load_cached)
        .unwrap_or_default();
    let store_setup = store.setup_script_for(&repo_key);
    let store_archive = store.archive_script_for(&repo_key);
    let store_symlinks = normalize_symlink_list(store.symlinks_for(&repo_key));
    let prompt_dismissed = store.is_setup_prompt_dismissed(&repo_key);

    let effective_setup = if native_setup_disabled {
        None
    } else {
        native_setup
            .clone()
            .or_else(|| store_setup.clone())
            .or_else(|| orca_setup.clone())
    };
    let effective_archive = if native_archive_disabled {
        None
    } else {
        native_archive
            .clone()
            .or_else(|| store_archive.clone())
            .or_else(|| orca_archive.clone())
    };

    let (suggested_setup, suggested_source) = if let Some(ref s) = orca_setup {
        (Some(s.clone()), Some("orca.yaml".to_owned()))
    } else if root.join("package.json").is_file() {
        let cmd = if root.join("pnpm-lock.yaml").is_file() {
            "pnpm install"
        } else if root.join("yarn.lock").is_file() {
            "yarn install"
        } else if root.join("bun.lockb").is_file() || root.join("bun.lock").is_file() {
            "bun install"
        } else {
            "npm install"
        };
        (Some(cmd.to_owned()), Some("package.json".to_owned()))
    } else {
        (None, None)
    };

    let effective_symlinks = normalize_symlink_list(
        native_symlinks
            .iter()
            .cloned()
            .chain(orca_symlinks.iter().cloned())
            .chain(store_symlinks.iter().cloned()),
    );

    RepoHookSummary {
        repo_key,
        effective_setup,
        effective_archive,
        native_setup,
        native_archive,
        native_setup_disabled,
        native_archive_disabled,
        orca_setup,
        orca_archive,
        store_setup,
        store_archive,
        suggested_setup,
        suggested_source,
        prompt_dismissed,
        native_symlinks,
        orca_symlinks,
        store_symlinks,
        effective_symlinks,
    }
}

// ============================================================================
// Local Listening Ports & URL Detection (MK-117)
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortEntry {
    pub session_id: u64,
    pub port: u16,
    pub url: String,
    pub reachable: bool,
}

static LOCAL_URL_REGEX: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(https?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]):(\d{2,5})(?:/[\x21\x23-\x26\x28\x2a-\x3b\x3d\x3f-\x5c\x5e-\x7e]*)?)|(?:(?:\b(?:localhost|127\.0\.0\.1|0\.0\.0\.0)|\[::1\]):(\d{2,5})\b)",
    )
    .ok()
});

pub fn extract_local_urls(session_id: u64, text: &str, probe: bool) -> Vec<PortEntry> {
    let Some(re) = LOCAL_URL_REGEX.as_ref() else {
        return Vec::new();
    };
    let mut seen_ports = BTreeSet::new();
    let mut out = Vec::new();
    for caps in re.captures_iter(text) {
        let (raw_url, port_str) = match (caps.get(1), caps.get(2), caps.get(3)) {
            (Some(u), Some(p), _) => (Some(u.as_str()), p.as_str()),
            (_, _, Some(p)) => (None, p.as_str()),
            _ => continue,
        };
        let Ok(port) = port_str.parse::<u16>() else {
            continue;
        };
        if port < 80 || !seen_ports.insert(port) {
            continue;
        }
        let url = match raw_url {
            Some(u) => u
                .trim_end_matches(['.', ',', ';', ':', '!', '?'])
                .replace("0.0.0.0", "localhost")
                .replace("[::1]", "localhost"),
            None => format!("http://localhost:{port}"),
        };
        let reachable = if probe { probe_port(port) } else { true };
        out.push(PortEntry {
            session_id,
            port,
            url,
            reachable,
        });
    }
    out
}

pub fn probe_port(port: u16) -> bool {
    let timeout = Duration::from_millis(80);
    let v4 = SocketAddr::from(([127, 0, 0, 1], port));
    if TcpStream::connect_timeout(&v4, timeout).is_ok() {
        return true;
    }
    let v6 = SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port));
    TcpStream::connect_timeout(&v6, timeout).is_ok()
}

// ============================================================================
// Built-in Agent Skills Registry (MK-115)
// ============================================================================

mod skills;
#[cfg(test)]
use skills::ORCA_SKILL_NAMES;
pub use skills::{
    BUILTIN_SKILLS, ORCHESTRATION_REFERENCES, SkillEntry, SkillSyncReport, builtin_skills,
    install_skill, skill_directories, skill_references, sync_builtin_skills,
    sync_builtin_skills_at,
};

// ============================================================================
// Atomic File & Lock Helpers
// ============================================================================

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn atomic_write(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let file_name = target
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("state");
    let tmp = parent.join(format!(".{file_name}.tmp.{}", std::process::id()));
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut f = opts.open(&tmp)?;
    if let Err(e) = f.write_all(bytes).and_then(|()| f.sync_all()) {
        fs::remove_file(&tmp).ok();
        return Err(e);
    }
    drop(f);
    if let Err(e) = fs::rename(&tmp, target) {
        fs::remove_file(&tmp).ok();
        return Err(e);
    }
    Ok(())
}

struct LockGuard {
    _file: File,
}

impl LockGuard {
    fn acquire(lock_path: &Path) -> io::Result<Self> {
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        opts.mode(0o600);
        let file = opts.open(lock_path)?;
        file.lock()?;
        Ok(Self { _file: file })
    }
}

/// 손상된 `worktree-comments.toml`의 최초 사본을 `.toml.corrupt`로 남기고 덮어쓰기를 거부한다.
fn corrupt_comments_store(path: &Path, reason: &str) -> io::Result<WorktreeStore> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    opts.mode(0o600);
    match opts.open(path.with_extension("toml.corrupt")) {
        Ok(mut out) => {
            io::copy(&mut fs::File::open(path)?, &mut out)?;
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, reason))
}

fn invalid(msg: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

/// 정리 대상 일시 메시지: 확인済み 하트비트·상태. 미확인 메시지와 진행 기록은
/// [`OrchestrationStore::prune_messages`]가 버리지 않는다.
fn is_ephemeral(m: &OrchMessage) -> bool {
    m.acked && matches!(m.msg_type, MessageType::Heartbeat | MessageType::Status)
}

fn not_found(msg: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_messages_filters_unread_and_acks_only_matching_messages() -> io::Result<()> {
        let mut store = OrchestrationStore::default();
        let params = SendParams {
            from: "worker".to_owned(),
            to: "coordinator".to_owned(),
            msg_type: MessageType::Status,
            summary: "Progress".to_owned(),
            body: String::new(),
            run_id: None,
            task_id: None,
            dispatch_id: None,
            outcome: None,
            reply_to: None,
        };
        let status = store.send_message(&params)?;
        let question = store.send_message(&SendParams {
            msg_type: MessageType::Question,
            summary: "Proceed?".to_owned(),
            ..params.clone()
        })?;
        let other = store.send_message(&SendParams {
            to: "other-worker".to_owned(),
            ..params
        })?;

        assert_eq!(
            store.check_messages(Some("coordinator"), &[MessageType::Status], true, false),
            status
        );
        let acked = store.check_messages(Some("coordinator"), &[MessageType::Status], true, true);
        assert_eq!(acked.len(), 1);
        assert_eq!(acked.first().map(|m| &m.id), status.first().map(|m| &m.id));
        assert!(acked.iter().all(|m| m.acked));
        assert!(
            store
                .check_messages(Some("coordinator"), &[MessageType::Status], true, true)
                .is_empty()
        );
        assert_eq!(
            store.check_messages(Some("coordinator"), &[], true, false),
            question
        );
        assert_eq!(
            store.check_messages(Some("other-worker"), &[], true, false),
            other
        );
        assert_eq!(
            store.check_messages(Some("coordinator"), &[MessageType::Status], false, false),
            acked
        );
        Ok(())
    }

    #[test]
    fn reply_message_acks_question_and_preserves_thread_context() -> io::Result<()> {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("Questions", Some(1))?;
        let task = store.create_task(&run.id, "Confirm approach", "", Vec::new())?;
        let dispatch = store.start_worker(
            &run.id,
            Some(&task.id),
            "worker",
            "claude",
            None,
            "/tmp",
            "p",
        )?;
        let question = store
            .send_message(&SendParams {
                from: "worker".to_owned(),
                to: "coordinator".to_owned(),
                msg_type: MessageType::Question,
                summary: "Proceed?".to_owned(),
                body: "Use the existing API?".to_owned(),
                run_id: Some(run.id),
                task_id: Some(task.id),
                dispatch_id: Some(dispatch.id),
                outcome: None,
                reply_to: None,
            })?
            .into_iter()
            .next()
            .ok_or_else(|| not_found("question"))?;
        assert!(!question.acked);

        let answer = store.reply_message(&question.id, "coordinator", "Use it")?;
        assert_eq!(answer.msg_type, MessageType::Answer);
        assert_eq!(answer.from, "coordinator");
        assert_eq!(answer.to, "worker");
        assert_eq!(answer.body, "Use it");
        assert_eq!(answer.reply_to.as_deref(), Some(question.id.as_str()));
        assert_eq!(answer.run_id, question.run_id);
        assert_eq!(answer.task_id, question.task_id);
        assert_eq!(answer.dispatch_id, question.dispatch_id);
        assert!(!answer.acked);
        assert_eq!(
            store
                .messages
                .iter()
                .find(|m| m.id == question.id)
                .map(|m| m.acked),
            Some(true)
        );
        assert!(
            store
                .check_messages(Some("coordinator"), &[MessageType::Question], true, false)
                .is_empty()
        );
        assert_eq!(
            store.check_messages(Some("worker"), &[MessageType::Answer], true, false),
            vec![answer]
        );
        Ok(())
    }

    #[test]
    fn stop_preserves_settled_outcomes_and_release_marks_released() -> io::Result<()> {
        for (outcome, dispatch_status, task_status) in [
            (
                "succeeded",
                DispatchStatus::Succeeded,
                TaskStatus::Completed,
            ),
            ("failed", DispatchStatus::Failed, TaskStatus::Failed),
        ] {
            let mut store = OrchestrationStore::default();
            let run = store.create_run("Stop settled worker", None)?;
            let task = store.create_task(&run.id, "Work", "", Vec::new())?;
            let dispatch = store.start_worker(
                &run.id,
                Some(&task.id),
                "worker",
                "claude",
                None,
                "/tmp",
                "p",
            )?;
            store.send_message(&SendParams {
                from: "worker".to_owned(),
                to: "coordinator".to_owned(),
                msg_type: MessageType::WorkerDone,
                summary: "Finished".to_owned(),
                body: String::new(),
                run_id: Some(run.id),
                task_id: Some(task.id.clone()),
                dispatch_id: Some(dispatch.id.clone()),
                outcome: Some(outcome.to_owned()),
                reply_to: None,
            })?;

            let stopped = store.stop_or_release_worker(&dispatch.id, false)?;
            assert_eq!(stopped.status, dispatch_status);
            assert_eq!(stopped.summary.as_deref(), Some("Finished"));
            let released = store.stop_or_release_worker("worker", true)?;
            assert_eq!(released.status, DispatchStatus::Released);
            assert_eq!(released.id, dispatch.id);
            assert_eq!(
                store.stop_or_release_worker(&dispatch.id, false)?.status,
                DispatchStatus::Released
            );
            let settled_task = store
                .tasks
                .iter()
                .find(|t| t.id == task.id)
                .ok_or_else(|| not_found("task"))?;
            assert_eq!(settled_task.status, task_status);
            assert!(settled_task.active_dispatch_id.is_none());
        }
        Ok(())
    }

    #[test]
    fn stop_active_worker_returns_task_to_ready() -> io::Result<()> {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("Stop active worker", None)?;
        let task = store.create_task(&run.id, "Work", "", Vec::new())?;
        let dispatch = store.start_worker(
            &run.id,
            Some(&task.id),
            "worker",
            "claude",
            None,
            "/tmp",
            "p",
        )?;

        let stopped = store.stop_or_release_worker(&dispatch.id, false)?;
        assert_eq!(stopped.status, DispatchStatus::Stopped);
        assert!(stopped.ended_at_ms.is_some());
        let ready_task = store
            .tasks
            .iter()
            .find(|t| t.id == task.id)
            .ok_or_else(|| not_found("task"))?;
        assert_eq!(ready_task.status, TaskStatus::Ready);
        assert!(ready_task.active_dispatch_id.is_none());
        Ok(())
    }

    #[test]
    fn worktree_references_add_find_remove_roundtrip() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-reference-test-{}", std::process::id()));
        let mut store = WorktreeStore::default();
        let key = "/repo/Feature";
        let first = "https://example.com/Design";
        let second = "https://example.com/review";
        assert_eq!(
            store.add_reference(key, &format!(" {first} "))?,
            vec![first]
        );
        assert_eq!(store.add_reference(key, first)?, vec![first]);
        assert_eq!(store.add_reference(key, second)?, vec![first, second]);
        store.add_reference("/repo/other", "https://elsewhere.test/notes")?;
        store.save(&dir)?;

        let mut loaded = WorktreeStore::load(&dir)?;
        assert_eq!(loaded, store);
        assert_eq!(
            loaded.find_references(" DESIGN "),
            vec![(key.to_owned(), first.to_owned())]
        );
        assert_eq!(
            loaded.find_references("FEATURE"),
            vec![
                (key.to_owned(), first.to_owned()),
                (key.to_owned(), second.to_owned()),
            ]
        );
        assert_eq!(loaded.find_references("").len(), 3);
        assert!(loaded.find_references("missing").is_empty());
        assert_eq!(
            loaded.remove_reference(key, &format!(" {first} ")),
            vec![second]
        );
        assert_eq!(loaded.remove_reference(key, "missing"), vec![second]);
        assert!(loaded.remove_reference(key, second).is_empty());
        assert!(!loaded.references.contains_key(key));
        assert!(loaded.remove_reference("/repo/missing", first).is_empty());
        assert!(loaded.find_references("feature").is_empty());
        loaded.save(&dir)?;
        assert_eq!(WorktreeStore::load(&dir)?, loaded);
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn reference_matches_borrow_strings_and_preserve_owned_search_contract() -> io::Result<()> {
        let mut store = WorktreeStore::default();
        store.add_reference("/repo/Zebra", "https://example.com/First")?;
        store.add_reference("/repo/Zebra", "https://example.com/second")?;
        store.add_reference("/repo/Alpha", "https://elsewhere.test/First")?;
        store.add_reference("/repo/한글", "https://example.com/Ä")?;
        for query in ["", "  ", " FIRST ", "ZEBRA", "missing", "한글", "Ä", "ä"] {
            let borrowed: Vec<_> = store.reference_matches(query).collect();
            let owned = store.find_references(query);
            assert_eq!(
                borrowed,
                owned
                    .iter()
                    .map(|(wt, url)| (wt.as_str(), url.as_str()))
                    .collect::<Vec<_>>()
            );
        }
        // iterator는 정규화한 검색어를 소유하므로 원래 검색어가 먼저 사라져도 된다.
        let matches = {
            let query = String::from(" zebra ");
            store.reference_matches(&query)
        };
        let found: Vec<_> = matches.collect();
        assert_eq!(
            found,
            vec![
                ("/repo/Zebra", "https://example.com/First"),
                ("/repo/Zebra", "https://example.com/second")
            ]
        );
        let (stored_key, stored_urls) = store.references.get_key_value("/repo/Zebra").unwrap();
        assert!(std::ptr::eq(found[0].0, stored_key.as_str()));
        assert!(std::ptr::eq(found[0].1, stored_urls[0].as_str()));
        let all: Vec<_> = store.reference_matches("").collect();
        assert_eq!(all[0].0, "/repo/Alpha");
        assert_eq!(all[1].1, "https://example.com/First");
        assert_eq!(all[2].1, "https://example.com/second");
        assert!(
            store.reference_matches("ä").next().is_none(),
            "ASCII 대소문자 규칙을 유지한다"
        );
        Ok(())
    }

    #[test]
    fn dag_auto_converges_and_enforces_dispatch_authority() -> io::Result<()> {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("M8 Release", Some(1))?;
        let t1 = store.create_task(&run.id, "Build core", "proto", Vec::new())?;
        assert_eq!(t1.status, TaskStatus::Ready);

        let t2 = store.create_task(&run.id, "Build UI", "gpui", vec![t1.id.clone()])?;
        assert_eq!(t2.status, TaskStatus::Pending);

        let disp1 = store.start_worker(
            &run.id,
            Some(&t1.id),
            "worker-a",
            "claude",
            Some(10),
            "/tmp",
            "Implement proto",
        )?;

        // Mismatched dispatch authority must be rejected.
        let bad = store.send_message(&SendParams {
            from: "worker-b".to_owned(),
            to: "coordinator".to_owned(),
            msg_type: MessageType::WorkerDone,
            summary: "Hijack".to_owned(),
            body: String::new(),
            run_id: Some(run.id.clone()),
            task_id: Some(t1.id.clone()),
            dispatch_id: Some("disp-999".to_owned()),
            outcome: None,
            reply_to: None,
        });
        assert!(bad.is_err());

        // Legitimate worker_done settles t1 and promotes t2 to Ready.
        store.send_message(&SendParams {
            from: "worker-a".to_owned(),
            to: "coordinator".to_owned(),
            msg_type: MessageType::WorkerDone,
            summary: "Proto done".to_owned(),
            body: String::new(),
            run_id: Some(run.id.clone()),
            task_id: Some(t1.id.clone()),
            dispatch_id: Some(disp1.id),
            outcome: Some("succeeded".to_owned()),
            reply_to: None,
        })?;

        let t2_after = store
            .tasks
            .iter()
            .find(|t| t.id == t2.id)
            .ok_or_else(|| not_found("t2"))?;
        assert_eq!(t2_after.status, TaskStatus::Ready);

        // Complete t2 -> Run auto-completes.
        store.update_task(&t2.id, TaskStatus::Completed, Some("UI done".to_owned()))?;
        let run_after = store
            .runs
            .iter()
            .find(|r| r.id == run.id)
            .ok_or_else(|| not_found("run"))?;
        assert_eq!(run_after.status, RunStatus::Completed);
        Ok(())
    }

    #[test]
    fn group_addressing_and_gates_work() -> io::Result<()> {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("Group Run", None)?;
        store.start_worker(&run.id, None, "w-claude", "claude", None, "/tmp", "p1")?;
        store.start_worker(&run.id, None, "w-codex", "codex", None, "/tmp", "p2")?;

        let msgs = store.send_message(&SendParams {
            from: "coordinator".to_owned(),
            to: "@all".to_owned(),
            msg_type: MessageType::Status,
            summary: "Sync up".to_owned(),
            body: String::new(),
            run_id: Some(run.id.clone()),
            task_id: None,
            dispatch_id: None,
            outcome: None,
            reply_to: None,
        })?;
        assert_eq!(msgs.len(), 2);

        let gate = store.create_gate(
            "w-claude",
            "Proceed with schema change?",
            vec!["approve".to_owned(), "reject".to_owned()],
            Some(run.id),
            None,
        )?;
        assert_eq!(gate.status, GateStatus::Pending);
        let resolved =
            store.resolve_gate(&gate.id, "approve", Some("LGTM".to_owned()), "coordinator")?;
        assert_eq!(resolved.status, GateStatus::Resolved);
        assert_eq!(resolved.decision.as_deref(), Some("approve"));
        Ok(())
    }

    #[test]
    fn port_extraction_finds_localhost_and_ipv6_urls() {
        let sample = "Server listening at http://localhost:3000/api and 127.0.0.1:5173 or http://0.0.0.0:8080 and http://[::1]:4000";
        let entries = extract_local_urls(7, sample, false);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries.first().map(|e| e.port), Some(3000));
        assert_eq!(
            entries.get(2).map(|e| e.url.as_str()),
            Some("http://localhost:8080")
        );
        assert_eq!(
            entries.get(3).map(|e| e.url.as_str()),
            Some("http://localhost:4000")
        );
    }

    #[test]
    fn redispatch_requires_explicit_stop_and_blocks_stale_or_unassigned_worker_done()
    -> io::Result<()> {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("Hardening Run", None)?;
        let t1 = store.create_task(&run.id, "Step 1", "", Vec::new())?;
        let t2 = store.create_task(&run.id, "Step 2", "", vec![t1.id.clone()])?;

        // Pending task cannot be dispatched before dependencies complete (M8).
        assert!(
            store
                .start_worker(&run.id, Some(&t2.id), "w2", "claude", None, "/tmp", "p")
                .is_err()
        );

        // Task with no active dispatch cannot be settled by worker_done (H3 Path B).
        assert!(
            store
                .send_message(&SendParams {
                    from: "w1".to_owned(),
                    to: "coordinator".to_owned(),
                    msg_type: MessageType::WorkerDone,
                    summary: "Premature".to_owned(),
                    body: String::new(),
                    run_id: Some(run.id.clone()),
                    task_id: Some(t1.id.clone()),
                    dispatch_id: None,
                    outcome: None,
                    reply_to: None,
                })
                .is_err()
        );

        let d1 = store.start_worker(&run.id, Some(&t1.id), "w1", "claude", None, "/tmp", "p1")?;
        let before = store.clone();
        assert!(
            store
                .start_worker(&run.id, Some(&t1.id), "w2", "codex", None, "/tmp", "p2")
                .is_err()
        );
        assert_eq!(store, before);
        store.stop_or_release_worker(&d1.id, false)?;
        let d2 = store.start_worker(&run.id, Some(&t1.id), "w2", "codex", None, "/tmp", "p2")?;

        // Stale dispatch d1 without --task must be rejected (H3 Path A).
        assert!(
            store
                .send_message(&SendParams {
                    from: "w1".to_owned(),
                    to: "coordinator".to_owned(),
                    msg_type: MessageType::WorkerDone,
                    summary: "Late d1".to_owned(),
                    body: String::new(),
                    run_id: Some(run.id.clone()),
                    task_id: None,
                    dispatch_id: Some(d1.id),
                    outcome: None,
                    reply_to: None,
                })
                .is_err()
        );

        // Active dispatch d2 succeeds.
        store.send_message(&SendParams {
            from: "w2".to_owned(),
            to: "coordinator".to_owned(),
            msg_type: MessageType::WorkerDone,
            summary: "Done d2".to_owned(),
            body: String::new(),
            run_id: Some(run.id),
            task_id: None,
            dispatch_id: Some(d2.id),
            outcome: None,
            reply_to: None,
        })?;

        // Reset preserves next_seq so IDs never collide (H5).
        let seq_before = store.next_seq;
        store.reset(None);
        assert_eq!(store.next_seq, seq_before);
        Ok(())
    }

    #[test]
    fn invalid_worker_outcomes_leave_dispatch_task_and_messages_unchanged() {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("outcome validation", None).unwrap();
        let task = store
            .create_task(&run.id, "implementation", "", vec![])
            .unwrap();
        let child = store
            .create_task(&run.id, "verification", "", vec![task.id.clone()])
            .unwrap();
        let dispatch = store
            .start_worker(
                &run.id,
                Some(&task.id),
                "worker",
                "agy",
                None,
                "/tmp",
                "work",
            )
            .unwrap();
        let snapshot = store.clone();
        let mut params = SendParams {
            from: "worker".into(),
            to: "coordinator".into(),
            msg_type: MessageType::WorkerDone,
            summary: "result".into(),
            body: String::new(),
            run_id: Some(run.id),
            task_id: Some(task.id),
            dispatch_id: Some(dispatch.id),
            outcome: None,
            reply_to: None,
        };
        for outcome in ["cancelled", "blocked", "suceeded", "", "success"] {
            params.outcome = Some(outcome.into());
            assert!(store.send_message(&params).is_err(), "accepted {outcome}");
            assert_eq!(store, snapshot);
        }
        params.outcome = Some("succeeded".into());
        store.send_message(&params).unwrap();
        assert_eq!(
            store
                .tasks
                .iter()
                .find(|t| t.id == child.id)
                .unwrap()
                .status,
            TaskStatus::Ready
        );
    }

    #[test]
    fn install_skill_preserves_foreign_skill_by_namespacing() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-skill-test-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let foreign_dir = dir.join("orchestration");
        fs::create_dir_all(&foreign_dir)?;
        fs::write(foreign_dir.join("SKILL.md"), "# Orca Orchestration\n")?;

        let installed = install_skill("orchestration", &dir)?;
        assert!(installed.ends_with("krill-orchestration/SKILL.md"));
        let namespaced = fs::read_to_string(&installed)?;
        assert!(namespaced.contains("name: krill-orchestration\n"));
        let orig = fs::read_to_string(foreign_dir.join("SKILL.md"))?;
        assert_eq!(orig, "# Orca Orchestration\n");

        // 다시 설치해도 자기 이전 설치본을 남의 스킬로 오인해 `krill-<name>`으로 새지 않는다.
        let fresh = dir.join("reinstall");
        for skill in BUILTIN_SKILLS {
            let first = install_skill(skill.name, &fresh)?;
            assert_eq!(install_skill(skill.name, &fresh)?, first, "{}", skill.name);
            let expected = if ORCA_SKILL_NAMES.contains(&skill.name) {
                format!("krill-{}/SKILL.md", skill.name)
            } else {
                format!("{}/SKILL.md", skill.name)
            };
            assert!(first.ends_with(&expected), "{}", skill.name);
        }

        // 예전 버전이 맨 이름에 깐 Krill 사본은 치우고, Orca 심링크는 건드리지 않는다.
        let legacy = dir.join("legacy");
        fs::create_dir_all(legacy.join("computer-use"))?;
        fs::write(
            legacy.join("computer-use/SKILL.md"),
            "old\n<!-- krill-managed-skill -->\n",
        )?;
        install_skill("computer-use", &legacy)?;
        assert!(!legacy.join("computer-use").exists());
        assert!(legacy.join("krill-computer-use/SKILL.md").is_file());
        // 옛 `mk-` 자리의 자기 사본도 치우고, 같은 자리의 남의 스킬은 남긴다.
        fs::create_dir_all(legacy.join("mk-orchestration"))?;
        fs::write(
            legacy.join("mk-orchestration/SKILL.md"),
            "old\n<!-- krill-managed-skill -->\n",
        )?;
        fs::create_dir_all(legacy.join("mk-krill"))?;
        fs::write(legacy.join("mk-krill/SKILL.md"), "# someone else\n")?;
        install_skill("orchestration", &legacy)?;
        install_skill("krill", &legacy)?;
        assert!(!legacy.join("mk-orchestration").exists());
        assert!(legacy.join("mk-krill/SKILL.md").is_file());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&foreign_dir, legacy.join("orchestration"))?;
            install_skill("orchestration", &legacy)?;
            assert!(fs::symlink_metadata(legacy.join("orchestration"))?.is_symlink());
            assert_eq!(
                fs::read_to_string(legacy.join("orchestration/SKILL.md"))?,
                "# Orca Orchestration\n"
            );
        }

        let mut store = OrchestrationStore::default();
        let run = store.create_run("Group mentions", None)?;
        store.start_worker(&run.id, None, "agy-w", "antigravity", None, "/tmp", "p")?;
        store.start_worker(&run.id, None, "cur-w", "cursor", None, "/tmp", "p")?;
        store.start_worker(&run.id, None, "pi-w", "pi", None, "/tmp", "p")?;
        assert_eq!(store.resolve_recipients("@agy"), vec!["agy-w".to_owned()]);
        assert_eq!(
            store.resolve_recipients("@antigravity"),
            vec!["agy-w".to_owned()]
        );
        assert_eq!(
            store.resolve_recipients("@cursor"),
            vec!["cur-w".to_owned()]
        );
        assert_eq!(store.resolve_recipients("@pi"), vec!["pi-w".to_owned()]);

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn worktree_key_normalizes_subdirectory_to_git_root() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-wt-key-test-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();

        let repo = dir.join("repo");
        let sub = repo.join("crates/krill-proto/src");
        fs::create_dir_all(repo.join(".git"))?;
        fs::create_dir_all(&sub)?;

        let root_key = worktree_key(&repo);
        let sub_key = worktree_key(&sub);
        assert_eq!(sub_key, root_key);

        let linked = dir.join("linked-wt");
        let linked_sub = linked.join("nested/module");
        fs::create_dir_all(&linked_sub)?;
        fs::write(linked.join(".git"), "gitdir: /tmp/repo/.git/worktrees/w1\n")?;
        assert_eq!(worktree_key(&linked_sub), worktree_key(&linked));

        let mut store = WorktreeStore::default();
        store.add_reference(&sub.to_string_lossy(), "https://example.com/pr/1")?;
        assert_eq!(
            store.checkpoint(&repo.to_string_lossy()).references,
            vec!["https://example.com/pr/1".to_owned()]
        );
        #[cfg(unix)]
        {
            let alias = dir.join("sub-alias");
            std::os::unix::fs::symlink(&sub, &alias)?;
            assert_eq!(
                store.checkpoint(&alias.to_string_lossy()).references,
                vec!["https://example.com/pr/1".to_owned()]
            );
        }

        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn ambiguous_worker_done_without_dispatch_or_task_is_rejected() -> io::Result<()> {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("Ambiguous WorkerDone", None)?;
        let t1 = store.create_task(&run.id, "Task 1", "", Vec::new())?;
        let t2 = store.create_task(&run.id, "Task 2", "", Vec::new())?;

        store.start_worker(
            &run.id,
            Some(&t1.id),
            "w-shared",
            "claude",
            None,
            "/tmp",
            "p1",
        )?;
        store.start_worker(
            &run.id,
            Some(&t2.id),
            "w-shared",
            "claude",
            None,
            "/tmp",
            "p2",
        )?;

        let err = store
            .send_message(&SendParams {
                from: "w-shared".to_owned(),
                to: "coordinator".to_owned(),
                msg_type: MessageType::WorkerDone,
                summary: "Ambiguous done".to_owned(),
                body: String::new(),
                run_id: Some(run.id.clone()),
                task_id: None,
                dispatch_id: None,
                outcome: None,
                reply_to: None,
            })
            .err()
            .ok_or_else(|| invalid("expected ambiguous worker_done error"))?;
        assert!(err.to_string().contains("multiple active dispatches"));
        assert!(
            store
                .tasks
                .iter()
                .all(|t| t.status == TaskStatus::Dispatched)
        );

        store.send_message(&SendParams {
            from: "w-shared".to_owned(),
            to: "coordinator".to_owned(),
            msg_type: MessageType::WorkerDone,
            summary: "Task 1 done".to_owned(),
            body: String::new(),
            run_id: Some(run.id),
            task_id: Some(t1.id.clone()),
            dispatch_id: None,
            outcome: None,
            reply_to: None,
        })?;
        assert_eq!(
            store.tasks.iter().find(|t| t.id == t1.id).map(|t| t.status),
            Some(TaskStatus::Completed)
        );
        assert_eq!(
            store.tasks.iter().find(|t| t.id == t2.id).map(|t| t.status),
            Some(TaskStatus::Dispatched)
        );
        Ok(())
    }

    #[test]
    fn worker_done_scopes_inference_to_run_and_rejects_mismatched_bindings() -> io::Result<()> {
        let mut store = OrchestrationStore::default();
        let r1 = store.create_run("First", None)?;
        let r2 = store.create_run("Second", None)?;
        let task = store.create_task(&r1.id, "Task", "", Vec::new())?;
        let d1 =
            store.start_worker(&r1.id, Some(&task.id), "shared", "codex", None, "/tmp", "p")?;
        let d2 = store.start_worker(&r2.id, None, "shared", "codex", None, "/tmp", "p")?;
        let params = SendParams {
            from: "shared".to_owned(),
            to: "coordinator".to_owned(),
            msg_type: MessageType::WorkerDone,
            summary: "Done".to_owned(),
            body: String::new(),
            run_id: Some(r2.id.clone()),
            task_id: None,
            dispatch_id: Some(d1.id.clone()),
            outcome: Some("succeeded".to_owned()),
            reply_to: None,
        };
        for bad in [
            params.clone(),
            SendParams {
                task_id: Some(task.id.clone()),
                dispatch_id: Some(d2.id.clone()),
                ..params.clone()
            },
            SendParams {
                from: "unknown".to_owned(),
                dispatch_id: None,
                ..params.clone()
            },
        ] {
            let before = store.clone();
            assert!(store.send_message(&bad).is_err());
            assert_eq!(store, before);
        }
        store.send_message(&SendParams {
            dispatch_id: None,
            ..params
        })?;
        assert_eq!(
            store
                .dispatches
                .iter()
                .find(|d| d.id == d2.id)
                .map(|d| d.status),
            Some(DispatchStatus::Succeeded)
        );
        assert_eq!(
            store
                .dispatches
                .iter()
                .find(|d| d.id == d1.id)
                .map(|d| d.status),
            Some(DispatchStatus::Active)
        );
        Ok(())
    }

    #[test]
    fn dispatch_archive_defaults_for_old_state_and_roundtrips_separately() -> io::Result<()> {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("Archive", None)?;
        let mut dispatch =
            store.start_worker(&run.id, None, "worker", "codex", None, "/tmp", "p")?;
        let mut old = serde_json::to_value(&dispatch).map_err(invalid)?;
        old.as_object_mut()
            .ok_or_else(|| invalid("dispatch object"))?
            .remove("archived_output");
        let restored: Dispatch = serde_json::from_value(old).map_err(invalid)?;
        assert_eq!(restored.archived_output, None);
        dispatch.summary = Some("Result summary".to_owned());
        dispatch.archived_output = Some("terminal output\n한글".to_owned());
        let encoded = toml::to_string(&dispatch).map_err(invalid)?;
        assert_eq!(
            toml::from_str::<Dispatch>(&encoded).map_err(invalid)?,
            dispatch
        );
        Ok(())
    }

    #[test]
    fn probe_port_detects_ipv4_and_ipv6_listeners() -> io::Result<()> {
        for addr in ["127.0.0.1:0", "[::1]:0"] {
            let listener = std::net::TcpListener::bind(addr)?;
            assert!(probe_port(listener.local_addr()?.port()), "{addr}");
        }
        Ok(())
    }

    #[test]
    fn builtin_skills_krill_uses_current_cli_flags() -> io::Result<()> {
        let skill = BUILTIN_SKILLS
            .iter()
            .find(|s| s.name == "krill")
            .ok_or_else(|| not_found("krill skill"))?;
        // `crates/krill/tests/cli.rs`가 같은 명령을 실제 krilld에 돌려 파서와 맞는지 확인한다.
        for cmd in [
            "krill ls --json",
            "krill new --cwd <dir> --agent <profile>",
            "krill send <session_id> --text \"<text>\" --enter",
            "krill read <session_id> --lines 80",
            "krill wait <session_id> --for idle --timeout 30",
            "krill kill <session_id>",
        ] {
            assert!(skill.content.contains(cmd), "{cmd}");
        }
        for stale in [
            "--format json",
            "--cmd",
            "--tail",
            "--until",
            "--timeout-ms",
        ] {
            assert!(!skill.content.contains(stale), "{stale}");
        }
        let computer = BUILTIN_SKILLS
            .iter()
            .find(|s| s.name == "computer-use")
            .ok_or_else(|| not_found("computer-use skill"))?;
        for sub in [
            "krill computer capabilities",
            "krill computer permissions",
            "krill computer list-apps",
            "krill computer list-windows",
            "krill computer get-app-state",
            "krill computer click",
            "krill computer perform-secondary-action",
            "krill computer set-value",
            "krill computer type-text",
            "krill computer press-key",
            "krill computer hotkey",
            "krill computer paste-text",
            "krill computer scroll",
            "krill computer drag",
        ] {
            assert!(computer.content.contains(sub), "{sub}");
        }
        Ok(())
    }

    #[test]
    fn acked_heartbeats_are_pruned_before_size_limit() -> io::Result<()> {
        // PH-02: 본문 300B 하트비트가 쌓여 2MiB 상한에 닿으면 이후 모든 쓰기가 실패했다.
        // 확인済み 일시 메시지는 정리되어 파일에 계속 쓸 수 있어야 한다.
        let mut store = OrchestrationStore::default();
        let run = store.create_run("Heartbeat soak", None)?;
        let task = store.create_task(&run.id, "Work", "", Vec::new())?;
        let dispatch = store.start_worker(
            &run.id,
            Some(&task.id),
            "worker",
            "claude",
            None,
            "/tmp",
            "p",
        )?;
        let beat = |i: u32| SendParams {
            from: "worker".to_owned(),
            to: "coordinator".to_owned(),
            msg_type: MessageType::Heartbeat,
            summary: format!("beat {i}"),
            body: "x".repeat(300),
            run_id: Some(run.id.clone()),
            task_id: None,
            dispatch_id: None,
            outcome: None,
            reply_to: None,
        };
        for i in 0..4000u32 {
            store.send_message(&beat(i))?;
            if i % 1000 == 999 {
                // 코디네이터가 주기적으로 읽어 확인済みに 했다고 가정한다.
                store.check_messages(Some("coordinator"), &[MessageType::Heartbeat], false, true);
            }
        }
        // 진행 기록(worker_done 전 dispatch 메시지와 settlement)은 살아 있어야 한다.
        let done = store.send_message(&SendParams {
            from: "worker".to_owned(),
            to: "coordinator".to_owned(),
            msg_type: MessageType::WorkerDone,
            summary: "Finished".to_owned(),
            body: String::new(),
            run_id: Some(run.id.clone()),
            task_id: Some(task.id.clone()),
            dispatch_id: Some(dispatch.id.clone()),
            outcome: None,
            reply_to: None,
        })?;
        assert_eq!(done.len(), 1);
        assert!(
            store.messages.len() <= MAX_RETAINED_EPHEMERAL.saturating_add(10),
            "ephemeral overflow: {}",
            store.messages.len()
        );
        assert!(
            store
                .messages
                .iter()
                .any(|m| m.msg_type == MessageType::Dispatch),
            "dispatch lifecycle record must survive pruning"
        );
        assert!(
            store
                .messages
                .iter()
                .any(|m| m.msg_type == MessageType::WorkerDone),
            "worker_done lifecycle record must survive pruning"
        );
        let dir = std::env::temp_dir().join(format!("mk-orch-prune-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        store.save(&dir)?;
        let bytes = fs::metadata(dir.join(ORCH_FILE))?.len();
        assert!(bytes <= MAX_ORCH_BYTES, "orchestration.json {bytes}B");
        fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[test]
    fn unacked_messages_survive_pruning() -> io::Result<()> {
        // 미확인 메시지는 정리 대상이 아니다.
        let mut store = OrchestrationStore::default();
        for i in 0..u32::try_from(MAX_RETAINED_EPHEMERAL)
            .unwrap_or(u32::MAX)
            .saturating_add(10)
        {
            store.send_message(&SendParams {
                from: "worker".to_owned(),
                to: "coordinator".to_owned(),
                msg_type: MessageType::Heartbeat,
                summary: format!("beat {i}"),
                body: String::new(),
                run_id: None,
                task_id: None,
                dispatch_id: None,
                outcome: None,
                reply_to: None,
            })?;
        }
        assert!(
            store
                .messages
                .iter()
                .all(|m| m.msg_type == MessageType::Heartbeat && !m.acked),
            "unacked heartbeats must all survive"
        );
        Ok(())
    }

    #[test]
    fn install_skill_preserves_foreign_fallback_and_symlink_targets() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-skill-collisions-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        for name in ["orchestration", "krill-orchestration"] {
            fs::create_dir_all(dir.join(name))?;
            fs::write(
                dir.join(name).join("SKILL.md"),
                "# External skill mentioning Krill\n",
            )?;
        }
        assert!(install_skill("orchestration", &dir).is_err());
        for name in ["orchestration", "krill-orchestration"] {
            assert_eq!(
                fs::read_to_string(dir.join(name).join("SKILL.md"))?,
                "# External skill mentioning Krill\n"
            );
        }
        #[cfg(unix)]
        {
            let fallback = dir.join("krill-orchestration");
            fs::remove_dir_all(&fallback)?;
            std::os::unix::fs::symlink(dir.join("orchestration"), &fallback)?;
            assert!(install_skill("orchestration", &dir).is_err());
            fs::remove_file(&fallback)?;
            fs::create_dir_all(&fallback)?;
            std::os::unix::fs::symlink(
                dir.join("orchestration/SKILL.md"),
                fallback.join("SKILL.md"),
            )?;
            assert!(install_skill("orchestration", &dir).is_err());
            assert_eq!(
                fs::read_to_string(dir.join("orchestration/SKILL.md"))?,
                "# External skill mentioning Krill\n"
            );
        }
        fs::remove_dir_all(dir)
    }

    #[test]
    fn worktree_symlinks_and_hooks_store_roundtrip() -> io::Result<()> {
        let dir = std::env::temp_dir().join(format!("mk-symlinks-store-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let mut store = WorktreeStore::default();
        let key = "/repo/main";

        // Valid relative paths normalize trailing slashes and deduplicate.
        assert_eq!(store.add_symlink(key, " .env ")?, vec![".env"]);
        assert_eq!(store.add_symlink(key, ".env")?, vec![".env"]);
        assert_eq!(
            store.add_symlink(key, "node_modules/")?,
            vec![".env", "node_modules"]
        );
        assert_eq!(
            store.add_symlink(key, "packages/web/.env.local")?,
            vec![".env", "node_modules", "packages/web/.env.local"]
        );

        // Path traversal, absolute paths, .git, and backslashes are rejected.
        for bad in [
            "",
            ".",
            "..",
            "../.env",
            "a/../b",
            "/etc/passwd",
            ".git",
            ".git/config",
            "a/.git/b",
            "foo\\bar",
        ] {
            assert!(
                store.add_symlink(key, bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }

        // Hooks and prompt dismissal roundtrip.
        store.set_setup_prompt_dismissed(key, true);
        assert!(store.is_setup_prompt_dismissed(key));
        assert_eq!(
            store.set_setup_script(key, " pnpm install ")?,
            Some("pnpm install".to_owned())
        );
        // Setting a setup script clears the dismissed flag.
        assert!(!store.is_setup_prompt_dismissed(key));
        assert_eq!(
            store.set_archive_script(key, " ./scripts/cleanup.sh ")?,
            Some("./scripts/cleanup.sh".to_owned())
        );

        store.save(&dir)?;
        let mut loaded = WorktreeStore::load(&dir)?;
        assert_eq!(loaded, store);
        assert_eq!(
            loaded.symlinks_for(key),
            vec![".env", "node_modules", "packages/web/.env.local"]
        );
        assert_eq!(
            loaded.setup_script_for(key).as_deref(),
            Some("pnpm install")
        );
        assert_eq!(
            loaded.archive_script_for(key).as_deref(),
            Some("./scripts/cleanup.sh")
        );

        assert_eq!(
            loaded.remove_symlink(key, "node_modules/"),
            vec![".env", "packages/web/.env.local"]
        );
        assert_eq!(loaded.set_setup_script(key, "")?, None);
        assert_eq!(loaded.set_archive_script(key, "   ")?, None);
        assert!(loaded.setup_script_for(key).is_none());
        assert!(loaded.archive_script_for(key).is_none());

        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn worktree_store_parent_lineage_rejects_self_and_cycles() -> io::Result<()> {
        let mut store = WorktreeStore::default();
        store.set_parent("/repo/b", Some("/repo/a"))?;
        store.set_parent("/repo/c", Some("/repo/b"))?;
        assert_eq!(
            store.checkpoint("/repo/b").parent.as_deref(),
            Some("/repo/a")
        );
        assert_eq!(
            store.checkpoint("/repo/c").parent.as_deref(),
            Some("/repo/b")
        );
        assert!(store.set_parent("/repo/a", Some("/repo/a")).is_err());
        assert!(store.set_parent("/repo/a", Some("/repo/b")).is_err());
        assert!(store.set_parent("/repo/a", Some("/repo/c")).is_err());

        store.set_parent("/repo/b", None)?;
        assert_eq!(store.checkpoint("/repo/b").parent, None);
        store.set_parent("/repo/a", Some("/repo/c"))?;
        assert_eq!(
            store.checkpoint("/repo/a").parent.as_deref(),
            Some("/repo/c")
        );
        store.remove_worktree_lineage("/repo/c");
        assert_eq!(store.checkpoint("/repo/a").parent, None);
        assert_eq!(store.checkpoint("/repo/c").parent, None);
        Ok(())
    }

    #[test]
    fn gate_options_retain_worker_preamble_and_skill_references_work() -> io::Result<()> {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("Gate & Retain", None)?;
        let task = store.create_task(&run.id, "Step", "", Vec::new())?;
        let disp = store.start_worker(
            &run.id,
            Some(&task.id),
            "w-ret",
            "codex",
            Some(42),
            "/tmp",
            "Check preamble",
        )?;
        let preamble = worker_preamble(&disp);
        assert!(preamble.contains("--wait"), "{preamble}");
        assert!(preamble.contains("--outcome succeeded"), "{preamble}");

        let retained = store.retain_worker(&disp.id)?;
        assert_eq!(retained.status, DispatchStatus::Retained);
        let updated_task = store
            .tasks
            .iter()
            .find(|t| t.id == task.id)
            .ok_or_else(|| not_found("task"))?;
        assert_eq!(updated_task.active_dispatch_id, None);
        assert_eq!(updated_task.status, TaskStatus::Ready);

        let gate = store.create_gate(
            "w-ret",
            "Deploy?",
            vec!["approve".to_owned(), "reject".to_owned()],
            Some(run.id),
            Some(task.id),
        )?;
        let bad = store
            .resolve_gate(&gate.id, "maybe", None, "coordinator")
            .err()
            .ok_or_else(|| invalid("expected invalid gate option error"))?;
        assert!(bad.to_string().contains("not in gate options"), "{bad}");
        let ok = store.resolve_gate(&gate.id, "APPROVE", None, "coordinator")?;
        assert_eq!(ok.decision.as_deref(), Some("approve"));

        let refs = skill_references("orchestration");
        assert!(!refs.is_empty());
        assert_eq!(skill_references("krill-orchestration").len(), refs.len());
        let orch_skill = BUILTIN_SKILLS
            .iter()
            .find(|s| s.name == "orchestration")
            .ok_or_else(|| not_found("orchestration skill"))?;
        for term in [
            "run-use",
            "run-current",
            "worker-retain",
            "worker-release",
            "worker-abandon",
            "--question-id",
            "--wait",
            "--outcome succeeded",
        ] {
            assert!(orch_skill.content.contains(term), "missing {term}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod extension_wire_tests {
    use super::*;

    #[test]
    fn retain_extension_preserves_n_minus_one_postcard_ordinals() {
        for (status, bytes) in [
            (DispatchStatus::Active, vec![0]),
            (DispatchStatus::Succeeded, vec![1]),
            (DispatchStatus::Failed, vec![2]),
            (DispatchStatus::Stopped, vec![3]),
            (DispatchStatus::Released, vec![4]),
            (DispatchStatus::Abandoned, vec![5]),
            (DispatchStatus::Retained, vec![6]),
        ] {
            assert_eq!(postcard::to_stdvec(&status).unwrap(), bytes);
            assert_eq!(
                postcard::from_bytes::<DispatchStatus>(&bytes).unwrap(),
                status
            );
        }
        for (action, bytes) in [
            (
                OrchAction::StopOrReleaseWorker {
                    worker_or_dispatch: "x".into(),
                    release: false,
                    archived_output: None,
                },
                vec![12, 1, b'x', 0, 0],
            ),
            (
                OrchAction::AbandonWorker {
                    worker_or_dispatch: "x".into(),
                },
                vec![13, 1, b'x'],
            ),
            (OrchAction::Reset { run_id: None }, vec![14, 0]),
            (
                OrchAction::RetainWorker {
                    worker_or_dispatch: "x".into(),
                },
                vec![15, 1, b'x'],
            ),
        ] {
            assert_eq!(postcard::to_stdvec(&action).unwrap(), bytes);
            assert_eq!(postcard::from_bytes::<OrchAction>(&bytes).unwrap(), action);
        }
    }

    #[test]
    fn retained_responses_are_negotiated_for_n_minus_one_peers() {
        let mut store = OrchestrationStore::default();
        let run = store.create_run("compat", None).unwrap();
        let dispatch = store
            .start_worker(&run.id, None, "w", "codex", Some(42), "/tmp", "p")
            .unwrap();
        let retained = store.retain_worker(&dispatch.id).unwrap();
        let current = OrchResponse::Dispatch(retained.clone());
        assert_eq!(current.clone().for_features(FEAT_ORCH_RETAIN), current);
        let OrchResponse::Dispatch(legacy) = current.for_features(0) else {
            panic!("dispatch response")
        };
        assert_eq!(legacy.status, DispatchStatus::Stopped);
        assert_eq!(postcard::to_stdvec(&legacy.status).unwrap(), [3]);
        let OrchResponse::Snapshot(legacy) = OrchResponse::Snapshot(store.clone()).for_features(0)
        else {
            panic!("snapshot response")
        };
        assert_eq!(legacy.dispatches[0].status, DispatchStatus::Stopped);
        assert_eq!(store.dispatches[0].status, DispatchStatus::Retained);
    }

    #[cfg(unix)]
    #[test]
    fn daemon_response_loss_never_replays_reset_on_disk() {
        use crate::control::Request;
        use crate::screen::Msg as _;
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("ko-loss-{}", std::process::id()));
        fs::create_dir_all(dir.join("run")).unwrap();
        let mut store = OrchestrationStore::default();
        let run = store
            .create_run("keep after ambiguous delivery", None)
            .unwrap();
        store.save(&dir).unwrap();
        let listener = UnixListener::bind(dir.join("run/krilld.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .write_all(
                    &crate::ipc::ServerHello {
                        proto_min: crate::ipc::PROTO_MIN,
                        proto_max: crate::ipc::PROTO_MAX,
                        features: crate::ipc::FEATURES,
                        build: "response-loss-fixture".into(),
                        instance_id: 1,
                    }
                    .encode()
                    .unwrap(),
                )
                .unwrap();
            crate::ipc::read_frame(&mut socket).unwrap().unwrap();
            let (kind, body) = crate::ipc::read_frame(&mut socket).unwrap().unwrap();
            assert_eq!(
                Request::decode(kind, &body).unwrap(),
                Request::Orchestration(OrchAction::Reset { run_id: None })
            );
            // The daemon accepted the complete request, then its response was lost.
        });
        assert!(OrchestrationStore::reset_via_daemon_or_disk(&dir, None).is_err());
        server.join().unwrap();
        assert!(
            OrchestrationStore::load(&dir)
                .unwrap()
                .runs
                .iter()
                .any(|r| r.id == run.id)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn check_messages_filtered_and_ack_message_ids_enforce_run_and_recipient() {
        let mut store = OrchestrationStore::default();
        let run1 = store.create_run("run1", None).unwrap();
        let run2 = store.create_run("run2", None).unwrap();
        let msg1 = store
            .send_message(&SendParams {
                from: "w1".into(),
                to: "coordinator".into(),
                msg_type: MessageType::Status,
                summary: "r1".into(),
                body: String::new(),
                run_id: Some(run1.id.clone()),
                task_id: None,
                dispatch_id: None,
                outcome: None,
                reply_to: None,
            })
            .unwrap()[0]
            .clone();
        let msg2 = store
            .send_message(&SendParams {
                from: "w2".into(),
                to: "coordinator".into(),
                msg_type: MessageType::Status,
                summary: "r2".into(),
                body: String::new(),
                run_id: Some(run2.id.clone()),
                task_id: None,
                dispatch_id: None,
                outcome: None,
                reply_to: None,
            })
            .unwrap()[0]
            .clone();

        let filtered =
            store.check_messages_filtered(Some("coordinator"), Some(&run1.id), &[], true, false);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, msg1.id);
        assert!(!store.messages.iter().any(|m| m.acked));

        // Wrong run filter on ack_message_ids must fail without mutating anything.
        assert!(
            store
                .ack_message_ids(
                    Some("coordinator"),
                    Some(&run1.id),
                    &[],
                    true,
                    std::slice::from_ref(&msg2.id),
                )
                .is_err()
        );
        assert!(!store.messages.iter().any(|m| m.acked));

        let acked = store
            .ack_message_ids(
                Some("coordinator"),
                Some(&run1.id),
                &[],
                true,
                std::slice::from_ref(&msg1.id),
            )
            .unwrap();
        assert_eq!(acked.len(), 1);
        assert!(acked[0].acked);
        assert!(
            !store
                .messages
                .iter()
                .find(|m| m.id == msg2.id)
                .unwrap()
                .acked
        );
    }
}
