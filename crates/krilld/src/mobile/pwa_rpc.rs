//! 모바일 JSON 요청을 기존 IPC·M8 원자 저장소로 연결한다. 인증 전에는 호출하지 않는다.
use super::devices::hex;
use super::e2ee::{invalid, random};
use crate::registry::Registry;
use krill_proto::os::{
    ffi::{OsStrExt, OsStringExt},
    fs::{DirBuilderExt, OpenOptionsExt},
    net::UnixStream,
};
use krill_proto::{
    SessionId,
    control::{self, Create, Program, Reply, Request},
    input::{Input, InputAck, InputId, InputResult},
    ipc::{self, ClientRole, ServerHello},
    orchestration::OrchestrationStore,
    screen::{Attach, Msg, Seq, Stream},
    usage::{ProviderKind, UsageReply, UsageRequest},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

mod requests;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct QuickCommandEntry {
    name: String,
    text: String,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct QuickCommandsFile {
    #[serde(default)]
    quick_commands: Vec<QuickCommandEntry>,
}

fn normalize_quick_command(mut entry: QuickCommandEntry) -> QuickCommandEntry {
    entry.name = entry.name.trim().to_owned();
    entry
        .text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_matches('\n')
        .clone_into(&mut entry.text);
    entry
}

fn load_quick_commands(dir: Option<&Path>) -> Vec<QuickCommandEntry> {
    dir.and_then(|dir| fs::read_to_string(dir.join("settings.toml")).ok())
        .and_then(|content| toml::from_str::<QuickCommandsFile>(&content).ok())
        .unwrap_or_default()
        .quick_commands
        .into_iter()
        .filter(|entry| !entry.name.trim().is_empty() && !entry.text.trim().is_empty())
        .map(normalize_quick_command)
        .take(128)
        .collect()
}

fn quick_commands_json(dir: Option<&Path>) -> Value {
    load_quick_commands(dir)
        .into_iter()
        .map(|entry| json!({"name":entry.name,"text":entry.text}))
        .collect()
}

/// `create_terminal`이 받는 에이전트 프로필(`registry` create와 같은 출처) id·이름, 표준 순서. 설정을 못 읽으면 빈 목록.
/// 미설치 CLI도 싣는다 — 띄우면 로그인 셸 preflight가 설치 안내를 보여 준다.
fn agents_json(dir: Option<&Path>) -> Value {
    let mut agents: Vec<_> = crate::agent::control::Control::new(dir)
        .agents()
        .map(|a| a.profiles.into_iter().collect())
        .unwrap_or_default();
    agents.sort_by(|(a, _), (b, _)| {
        (krill_proto::agent::profile_rank(a), a).cmp(&(krill_proto::agent::profile_rank(b), b))
    });
    agents
        .into_iter()
        .map(|(id, p)| json!({"id":id,"name":p.name}))
        .collect()
}
pub(super) struct Backend {
    registry: Weak<Registry>,
    dir: Option<PathBuf>,
    hello: ServerHello,
    host: Value,
    /// 카드 git 요약(branch·added·deleted) cwd별 캐시. 실패도 `Null`로 캐시한다.
    git_cache: Arc<Mutex<HashMap<PathBuf, (Instant, Value)>>>,
    git_inflight: Arc<Mutex<HashSet<PathBuf>>>,
    git_revision: Arc<AtomicU64>,
    snapshot_cache: Mutex<Option<DashboardSnapshot>>,
    catalog_cache: Mutex<Option<CatalogSnapshot>>,
    settings_lock: Mutex<()>,
}
type FileStamp = Option<crate::agent::control::cache::Stamp>;
type SnapshotDependencies = (Vec<PathBuf>, Vec<FileStamp>);

struct DashboardSnapshot {
    revision: u64,
    paths: Vec<PathBuf>,
    stamps: Vec<FileStamp>,
    at: Instant,
    data: Value,
    etag: String,
}

struct CatalogSnapshot {
    repositories: Vec<control::RepositoryTrees>,
    paths: Vec<PathBuf>,
    stamps: Vec<FileStamp>,
    at: Instant,
    catalog: Arc<super::workspaces::Catalog>,
}

fn file_stamps(paths: &[PathBuf]) -> Vec<FileStamp> {
    paths
        .iter()
        .map(|path| crate::agent::control::cache::stamp(path))
        .collect()
}

/// 스냅샷 폴링이 같은 cwd마다 git 자식을 다시 띄우지 않게 하는 카드 요약 수명. 상세 git 화면은 캐시를 안 쓴다.
const GIT_TTL: Duration = Duration::from_secs(5);
impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish_non_exhaustive()
    }
}
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    /// `since`가 직전 응답의 `etag`와 같으면 본문 없이 `{"unchanged":true}`만 보낸다(폴링 대역폭·복호화·재렌더 절약).
    Snapshot {
        #[serde(default)]
        since: Option<String>,
    },
    SaveQuickCommands {
        commands: Vec<QuickCommandEntry>,
    },
    CreateTerminal {
        #[serde(default)]
        cwd_session: Option<u64>,
        #[serde(default)]
        workspace: Option<String>,
        #[serde(default)]
        agent: Option<String>,
    },
    CreateWorkspace {
        repository: String,
        name: String,
        #[serde(default)]
        base: Option<String>,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        setup_hash: Option<String>,
        #[serde(default)]
        skip_setup: bool,
    },
    CloseTerminal {
        session: u64,
    },
    Prompt {
        session: u64,
        text: String,
        #[serde(default)]
        photo: Option<PreparedPhoto>,
    },
    #[expect(
        dead_code,
        reason = "Legacy approval fields are decoded for compatibility but cannot authorize PTY input"
    )]
    Approve {
        session: u64,
        value: String,
    },
    Photo {
        session: u64,
        mime: String,
        data: String,
        #[serde(default)]
        prompt: Option<String>,
    },
    ChatPoll {
        session: u64,
        #[serde(default)]
        offset: u64,
        /// Open at the tail, or page backwards from `end_offset`.
        #[serde(default)]
        latest: bool,
        /// Display-only replay boundary from a previously observed page; never approval authority.
        #[serde(default)]
        end_offset: Option<u64>,
    },
    #[expect(
        dead_code,
        reason = "Legacy question fields are decoded for compatibility but cannot authorize PTY input"
    )]
    AnswerQuestion {
        session: u64,
        #[serde(default)]
        selected: Vec<usize>,
        #[serde(default)]
        option_count: usize,
        #[serde(default)]
        multi_select: bool,
        #[serde(default)]
        custom_text: Option<String>,
    },
    Accounts,
    SwitchAccount {
        provider: String,
        account_id: String,
    },
    Orchestration,
    GateReply {
        id: String,
        decision: String,
    },
    QuestionReply {
        id: String,
        body: String,
    },
    Handoff {
        session: u64,
        agent: String,
    },
    Git {
        session: u64,
        path: Option<String>,
    },
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedPhoto {
    mime: String,
    data: String,
}

fn chat_page_value(
    snapshot: krill_proto::agent_history::ChatPollSnapshot,
    offset: u64,
) -> io::Result<Value> {
    let identity = crate::agent_history::chat::generation(&snapshot);
    let generation = hex(ring::digest::digest(&ring::digest::SHA256, identity.as_bytes()).as_ref());
    let start = if snapshot.reset { 0 } else { offset };
    // `at`은 postcard 와이어에서 빠지는 필드라 직렬화 뒤 같은 순서의 항목에 붙인다.
    let times: Vec<String> = snapshot.items.iter().map(|item| item.at.clone()).collect();
    let mut value = to_value(snapshot)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("generation".into(), generation.into());
        object.insert("page_start".into(), start.into());
        object.insert("reply_mode".into(), "review_in_term".into());
        if let Some(items) = object.get_mut("items").and_then(Value::as_array_mut) {
            // These are display IDs for an observed [start, next_offset) page. Historical
            // replay must keep that byte end so later appends cannot change the partition.
            for (index, item) in items.iter_mut().enumerate() {
                if let Some(fields) = item.as_object_mut() {
                    fields.insert("id".into(), format!("{start}:{index}").into());
                    if let Some(at) = times.get(index).filter(|at| !at.is_empty()) {
                        fields.insert("at".into(), at.clone().into());
                    }
                }
            }
        }
    }
    Ok(value)
}

fn review_in_term() -> io::Error {
    io::Error::other(
        "This historical request cannot be verified against the current question. Confirm in Term.",
    )
}

impl Backend {
    pub(super) fn new(registry: Weak<Registry>, dir: Option<&Path>, hello: ServerHello) -> Self {
        Self {
            registry,
            dir: dir.map(Path::to_path_buf),
            hello,
            host: host_metadata(),
            git_cache: Arc::default(),
            git_inflight: Arc::default(),
            git_revision: Arc::default(),
            snapshot_cache: Mutex::default(),
            catalog_cache: Mutex::default(),
            settings_lock: Mutex::default(),
        }
    }
    pub(super) fn request(&self, request: &str) -> String {
        let result = (|| {
            if request.len() > 750_000 {
                return Err(invalid());
            }
            let action: Action = serde_json::from_str(request).map_err(|_| invalid())?;
            self.handle(action)
        })();
        let value = match result {
            Ok(data) => json!({"ok":true,"data":data}),
            Err(e) => json!({"ok":false,"error":e.to_string()}),
        };
        serde_json::to_string(&value)
            .ok()
            .filter(|text| text.len() < ipc::MAX_FRAME.saturating_sub(32))
            .unwrap_or_else(|| "{\"ok\":false,\"error\":\"response unavailable\"}".into())
    }
    fn handle(&self, action: Action) -> io::Result<Value> {
        match action {
            Action::Snapshot { since } => self.snapshot_since(since.as_deref()),
            Action::SaveQuickCommands { commands } => self.save_quick_commands(commands),
            Action::CreateTerminal {
                cwd_session,
                workspace,
                agent,
            } => {
                let program = match agent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    None => Program::Login,
                    Some(id) if crate::agent::control::valid_session_id(id) => {
                        Program::Agent { id: id.to_owned() }
                    }
                    Some(_) => return Err(invalid()),
                };
                let cwd = match (cwd_session, workspace) {
                    (Some(session), None) => self.cwd(session)?,
                    (None, Some(key)) => {
                        let Reply::Sidebar(sidebar) = self.control(&Request::Sidebar)? else {
                            return Err(invalid());
                        };
                        super::workspaces::Catalog::load(self.dir.as_deref(), &sidebar.repositories)
                            .cwd(&key)
                            .ok_or_else(invalid)?
                    }
                    _ => return Err(invalid()),
                };
                match self.control(&Request::Create(Create {
                    program,
                    cwd: cwd.into_os_string().into_vec(),
                    cols: 80,
                    rows: 24,
                    account: None,
                }))? {
                    Reply::Created { session, .. } => Ok(json!({"session":session.0})),
                    Reply::Failed(error) => Err(io::Error::other(error.to_string())),
                    _ => Err(invalid()),
                }
            }
            Action::CreateWorkspace {
                repository,
                name,
                base,
                agent,
                setup_hash,
                skip_setup,
            } => {
                let program = match agent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    None => Program::Login,
                    Some(id) if crate::agent::control::valid_session_id(id) => {
                        Program::Agent { id: id.to_owned() }
                    }
                    Some(_) => return Err(invalid()),
                };
                let name = name.trim();
                if name.is_empty()
                    || name.len() > 256
                    || name.chars().any(char::is_control)
                    || base
                        .as_ref()
                        .is_some_and(|b| b.len() > 256 || b.chars().any(char::is_control))
                    || skip_setup && setup_hash.is_some()
                {
                    return Err(invalid());
                }
                let Reply::Sidebar(sidebar) = self.control(&Request::Sidebar)? else {
                    return Err(invalid());
                };
                let repo =
                    super::workspaces::Catalog::load(self.dir.as_deref(), &sidebar.repositories)
                        .repository(&repository)
                        .ok_or_else(invalid)?;
                let setup = if skip_setup {
                    control::Setup::Skip
                } else if let Some(hash) = setup_hash {
                    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err(invalid());
                    }
                    let bytes = hash
                        .as_bytes()
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| {
                            std::str::from_utf8(pair)
                                .map_err(|_| invalid())
                                .and_then(|s| u8::from_str_radix(s, 16).map_err(|_| invalid()))
                        })
                        .collect::<io::Result<Vec<_>>>()?;
                    control::Setup::Approve(bytes)
                } else {
                    control::Setup::Ask
                };
                match self.control(&Request::NewWorktree(control::NewWorktree {
                    repo,
                    name: name.to_owned(),
                    base: base.filter(|b| !b.trim().is_empty()),
                    setup,
                }))? {
                    Reply::SetupUntrusted { script, hash } => {
                        Ok(json!({"needs_setup":true,"script":script,"hash":hex(&hash)}))
                    }
                    Reply::WorktreeCreated(mut tree) => {
                        let session = match self.control(&Request::Create(Create {
                            program,
                            cwd: tree.path.clone(),
                            cols: 80,
                            rows: 24,
                            account: None,
                        })) {
                            Ok(Reply::Created { session, .. }) => {
                                if let Some(dir) = self.dir.as_deref()
                                    && !krill_proto::git::is_default_branch_or_worktree_name(name)
                                {
                                    krill_proto::git::record_session_auto_title(dir, session, name)
                                        .ok();
                                }
                                Some(session.0)
                            }
                            other => {
                                tree.warnings.push(format!(
                                    "Workspace created; terminal unavailable: {other:?}"
                                ));
                                None
                            }
                        };
                        let path = PathBuf::from(std::ffi::OsStr::from_bytes(&tree.path));
                        let path = fs::canonicalize(&path).unwrap_or(path);
                        Ok(json!({"workspace":hex(path.as_os_str().as_bytes()),
                            "path":path.to_string_lossy(),"branch":tree.branch,"session":session,
                            "setup_session":tree.setup.map(|s| s.0),"warnings":tree.warnings}))
                    }
                    Reply::WorktreeFailed(error) => Err(io::Error::other(error)),
                    Reply::Failed(error) => Err(io::Error::other(error.to_string())),
                    _ => Err(invalid()),
                }
            }
            Action::CloseTerminal { session } => {
                match self.control(&Request::Close {
                    session: SessionId(session),
                })? {
                    Reply::Closed => Ok(json!({"closed":true})),
                    Reply::Failed(error) => Err(io::Error::other(error.to_string())),
                    _ => Err(invalid()),
                }
            }
            Action::Prompt {
                session,
                text,
                photo,
            } => {
                if text.len() >= krill_proto::input::MAX_INPUT {
                    return Err(invalid());
                }
                // Prepare the image before sending anything, then submit image and text in order.
                let path = photo
                    .map(|photo| save_photo(&photo.mime, &photo.data))
                    .transpose()?;
                let combined = path.as_ref().map_or_else(
                    || text.clone(),
                    |path| {
                        format!(
                            "{} {}",
                            krill_proto::agent::hooks::shell_quote(&path.to_string_lossy()),
                            text
                        )
                    },
                );
                let result = self.send_prompt(session, &combined);
                if result.is_err()
                    && let Some(path) = path
                {
                    fs::remove_file(path).ok();
                }
                result?;
                Ok(json!({"sent":true}))
            }
            Action::Approve { .. } => {
                // Transcript IDs and visible text cannot atomically prove the current PTY question.
                // Fail closed, including requests from older clients without a card identity.
                Err(review_in_term())
            }
            Action::Photo {
                session,
                mime,
                data,
                prompt,
            } => {
                let path = save_photo(&mime, &data)?;
                let quoted = krill_proto::agent::hooks::shell_quote(&path.to_string_lossy());
                let trimmed_prompt = prompt.as_deref().map(str::trim).filter(|s| !s.is_empty());
                let combined = match trimmed_prompt {
                    Some(p) => {
                        if p.len() >= krill_proto::input::MAX_INPUT {
                            fs::remove_file(&path).ok();
                            return Err(invalid());
                        }
                        format!("{quoted} {p}")
                    }
                    None => format!("{quoted} "),
                };
                let bracketed =
                    self.session_bracketed_paste(session) || combined.contains(['\n', '\r']);
                let payload = if bracketed {
                    format_bracketed_paste(&combined)
                } else {
                    sanitize_plain_input(&combined)
                };
                if payload.len() > krill_proto::input::MAX_INPUT {
                    fs::remove_file(&path).ok();
                    return Err(invalid());
                }
                let send_result = if trimmed_prompt.is_some() {
                    self.input_paced(
                        session,
                        &[payload, b"\r".to_vec()],
                        Duration::from_millis(60),
                    )
                } else {
                    self.input(session, payload)
                };
                if let Err(error) = send_result {
                    fs::remove_file(&path).ok();
                    return Err(error);
                }
                Ok(json!({"attached":true,"path":path.to_string_lossy()}))
            }
            Action::ChatPoll {
                session,
                offset,
                latest,
                end_offset,
            } => {
                let registry = self.registry.upgrade().ok_or_else(invalid)?;
                let snapshot = registry.chat_poll(SessionId(session), offset);
                if latest {
                    let (snapshot, start) =
                        crate::agent_history::chat::latest_page(snapshot, end_offset)?;
                    let mut value = chat_page_value(snapshot, start)?;
                    if let Some(object) = value.as_object_mut() {
                        object.insert("has_earlier".into(), (start > 0).into());
                    }
                    return Ok(value);
                }
                let snapshot = if let Some(end) = end_offset {
                    crate::agent_history::chat::replay_page(snapshot, offset, end)?
                } else {
                    snapshot
                };
                chat_page_value(snapshot, offset)
            }
            Action::AnswerQuestion { .. } => Err(review_in_term()),
            Action::Accounts => to_value(self.accounts(&UsageRequest::ListAccounts)?),
            Action::SwitchAccount {
                provider,
                account_id,
            } => {
                let provider = ProviderKind::parse(&provider).ok_or_else(invalid)?;
                to_value(self.accounts(&UsageRequest::SwitchAccount {
                    provider,
                    account_id,
                })?)
            }
            Action::Orchestration => self.orchestration(),
            Action::GateReply { id, decision } => {
                let dir = self.dir.as_deref().ok_or_else(invalid)?;
                to_value(OrchestrationStore::mutate(dir, |store| {
                    if !store
                        .gates
                        .iter()
                        .any(|g| g.id == id && requests::gate_pending(store, g))
                    {
                        return Err(io::Error::other("This request is no longer pending."));
                    }
                    store.resolve_gate(&id, &decision, None, "mobile")
                })?)
            }
            Action::QuestionReply { id, body } => {
                if body.len() > 16 * 1024 {
                    return Err(invalid());
                }
                let dir = self.dir.as_deref().ok_or_else(invalid)?;
                to_value(OrchestrationStore::mutate(dir, |store| {
                    if body.trim().is_empty() {
                        return Err(invalid());
                    }
                    if !store
                        .messages
                        .iter()
                        .any(|m| m.id == id && requests::question_pending(store, m))
                    {
                        return Err(io::Error::other("This request is no longer pending."));
                    }
                    store.reply_message(&id, "mobile", &body)
                })?)
            }
            Action::Handoff { session, agent } => {
                if !["codex", "claude"].contains(&agent.as_str()) {
                    return Err(invalid());
                }
                let cwd = self.cwd(session)?;
                let binary = std::env::current_exe()?
                    .parent()
                    .ok_or_else(invalid)?
                    .join("krill");
                let mut command = Command::new(binary);
                command
                    .args([
                        "handoff",
                        "--from",
                        &session.to_string(),
                        "--to-agent",
                        &agent,
                        "--lines",
                        "3",
                        "--json",
                    ])
                    .current_dir(cwd)
                    .stdin(Stdio::null());
                if let Some(dir) = &self.dir {
                    command.env("KRILL_STATE_DIR", dir);
                }
                let output = bounded_command(&mut command, Duration::from_secs(15), 128 * 1024)?;
                if output.is_empty() {
                    return Err(io::Error::other(
                        "agent handoff failed; check the desktop agent configuration",
                    ));
                }
                serde_json::from_slice(&output).map_err(io::Error::other)
            }
            Action::Git { session, path } => self.git(session, path.as_deref()),
        }
    }
    fn session_bracketed_paste(&self, session: u64) -> bool {
        self.registry
            .upgrade()
            .is_some_and(|reg| reg.session_bracketed_paste(SessionId(session)))
    }
    fn send_prompt(&self, session: u64, text: &str) -> io::Result<()> {
        if text.is_empty() {
            return self.input(session, b"\r".to_vec());
        }
        let bracketed = self.session_bracketed_paste(session) || text.contains(['\n', '\r']);
        let payload = if bracketed {
            format_bracketed_paste(text)
        } else {
            sanitize_plain_input(text)
        };
        if payload.len() > krill_proto::input::MAX_INPUT {
            return Err(invalid());
        }
        self.input_paced(
            session,
            &[payload, b"\r".to_vec()],
            Duration::from_millis(60),
        )
    }
    fn wire(&self) -> io::Result<Wire> {
        let registry = self.registry.upgrade().ok_or_else(invalid)?;
        let hello = self.hello.clone();
        let (mut socket, server) = UnixStream::pair()?;
        socket.set_read_timeout(Some(Duration::from_secs(20)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        let worker = thread::Builder::new()
            .name("mobile-rpc".into())
            .stack_size(super::gateway::RPC_STACK)
            .spawn(move || {
                crate::alloc::mobile_thread().ok();
                registry.serve_mobile(server, &hello).ok()
            })?;
        let mut wire = Wire {
            socket: socket.try_clone()?,
            worker: Some(worker),
        };
        let (kind, body) = ipc::read_frame(&mut socket)?.ok_or_else(invalid)?;
        ServerHello::decode(kind, &body)?;
        socket.write_all(
            &krill_proto::daemon::client_hello(ClientRole::App, "mobile-pwa").encode()?,
        )?;
        // wire는 위 clone과 같은 스트림이며 Drop에서 반드시 양쪽 작업을 끝낸다.
        std::mem::swap(&mut wire.socket, &mut socket);
        Ok(wire)
    }
    fn control(&self, request: &Request) -> io::Result<Reply> {
        ask(&mut self.wire()?, request)
    }
    fn accounts(&self, request: &UsageRequest) -> io::Result<krill_proto::usage::AccountsSnapshot> {
        let mut wire = self.wire()?;
        wire.socket.write_all(&request.encode()?)?;
        let (kind, body) = ipc::read_frame(&mut wire.socket)?.ok_or_else(invalid)?;
        match UsageReply::decode(kind, &body)? {
            UsageReply::Accounts(data) => Ok(data),
            UsageReply::Failed(e) => Err(io::Error::other(e.to_string())),
            _ => Err(invalid()),
        }
    }
    fn input(&self, session: u64, bytes: Vec<u8>) -> io::Result<()> {
        self.input_paced(session, &[bytes], Duration::ZERO)
    }
    fn input_paced(&self, session: u64, chunks: &[Vec<u8>], delay: Duration) -> io::Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let mut wire = self.wire()?;
        wire.socket.write_all(
            &Attach {
                session: SessionId(session),
                // 입력 전용 attach는 로그를 재생하지 않는다. 범위 밖 seq는 현재 끝으로 이동한다.
                stream: Stream::Raw {
                    from: Seq(u64::MAX),
                },
            }
            .encode()?,
        )?;
        loop {
            let (kind, _) = ipc::read_frame(&mut wire.socket)?.ok_or_else(invalid)?;
            if kind == krill_proto::screen::kind::ATTACHED {
                break;
            }
            if kind == krill_proto::screen::kind::REFUSED {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        for (idx, bytes) in chunks.iter().enumerate() {
            if idx > 0 && !delay.is_zero() {
                thread::sleep(delay);
            }
            let input_id = u64::try_from(idx).unwrap_or(0).saturating_add(1);
            wire.socket.write_all(
                &Input {
                    id: InputId(input_id),
                    bytes: bytes.clone(),
                }
                .encode()?,
            )?;
            loop {
                let (kind, body) = ipc::read_frame(&mut wire.socket)?.ok_or_else(invalid)?;
                if kind == krill_proto::input::kind::INPUT_ACK {
                    let ack = InputAck::decode(kind, &body)?;
                    if ack.result != InputResult::Applied {
                        return Err(io::Error::other("input rejected"));
                    }
                    break;
                }
            }
        }
        Ok(())
    }
    fn cwd(&self, session: u64) -> io::Result<PathBuf> {
        let Reply::Sidebar(sidebar) = self.control(&Request::Sidebar)? else {
            return Err(invalid());
        };
        let directory = sidebar
            .sessions
            .into_iter()
            .find(|s| s.session.0 == session)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Session not found"))?;
        if directory.cwd.is_empty() {
            return Err(io::Error::other("session working directory unavailable"));
        }
        Ok(PathBuf::from(std::ffi::OsString::from_vec(directory.cwd)))
    }
    fn save_quick_commands(&self, commands: Vec<QuickCommandEntry>) -> io::Result<Value> {
        if commands.len() > 128 {
            return Err(invalid());
        }
        let normalized: Vec<_> = commands.into_iter().map(normalize_quick_command).collect();
        for entry in &normalized {
            if entry.name.is_empty()
                || entry.name.len() > 120
                || entry.name.chars().any(char::is_control)
                || entry.text.trim().is_empty()
                || entry.text.len() > 16 * 1024
                || entry
                    .text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
            {
                return Err(invalid());
            }
        }
        let _guard = crate::session::lock(&self.settings_lock);
        let dir = self.dir.as_deref().ok_or_else(invalid)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let path = dir.join("settings.toml");
        let _document_lock = krill_proto::config_document::lock_toml(&path)?;
        let content = krill_proto::config_document::save_toml(
            &path,
            &QuickCommandsFile {
                quick_commands: normalized.clone(),
            },
        )
        .map_err(io::Error::other)?;
        let tmp = dir.join(format!(".settings.{}.tmp", hex(&random()?)));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, &path)
        })();
        if let Err(error) = result {
            fs::remove_file(&tmp).ok();
            return Err(error);
        }
        *crate::session::lock(&self.snapshot_cache) = None;
        Ok(json!({"quick_commands":normalized}))
    }

    fn effective_revision(&self, registry: &Registry) -> u64 {
        registry.dashboard_revision()
            ^ self
                .git_revision
                .load(Ordering::Relaxed)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    fn snapshot_since(&self, since: Option<&str>) -> io::Result<Value> {
        let registry = self.registry.upgrade().ok_or_else(invalid)?;
        // This mutex is a snapshot-only singleflight, independent of registry/session IO.
        let mut cached = crate::session::lock(&self.snapshot_cache);
        let revision = self.effective_revision(&registry);
        if let Some(snapshot) = cached.as_ref()
            && snapshot.revision == revision
            && snapshot.at.elapsed() < GIT_TTL
            && snapshot.stamps == file_stamps(&snapshot.paths)
        {
            return if since == Some(snapshot.etag.as_str()) {
                Ok(json!({"unchanged":true,"etag":snapshot.etag}))
            } else {
                Ok(snapshot.data.clone())
            };
        }
        // Drop stale terminal bodies before rebuilding (including a viewport clear).
        *cached = None;
        let base_paths = self.snapshot_paths(&Value::Null);
        let before = file_stamps(&base_paths);
        let mut catalog_used = None;
        let mut data = self.snapshot(&mut catalog_used)?;
        let bytes = serde_json::to_vec(&data).map_err(io::Error::other)?;
        let etag: String = hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref())
            .chars()
            .take(16)
            .collect();
        if let Some(object) = data.as_object_mut() {
            object.insert("etag".into(), etag.clone().into());
        }
        // Validate the dependency identity actually used by this body. A newer
        // catalog or a fresh post-build stamp cannot authenticate an older body.
        if let Some((paths, stamps)) = self.snapshot_dependencies(&data, catalog_used.as_ref())
            && before == file_stamps(&base_paths)
        {
            *cached = Some(DashboardSnapshot {
                revision,
                paths,
                stamps,
                at: Instant::now(),
                data: data.clone(),
                etag: etag.clone(),
            });
        }
        if since == Some(etag.as_str()) {
            Ok(json!({"unchanged":true,"etag":etag}))
        } else {
            Ok(data)
        }
    }

    fn snapshot_dependencies(
        &self,
        data: &Value,
        used: Option<&Arc<super::workspaces::Catalog>>,
    ) -> Option<SnapshotDependencies> {
        let used = used?;
        let (catalog_paths, catalog_stamps) = crate::session::lock(&self.catalog_cache)
            .as_ref()
            .filter(|cached| Arc::ptr_eq(&cached.catalog, used))
            .map(|cached| (cached.paths.clone(), cached.stamps.clone()))?;
        let mut paths = self.snapshot_paths(data);
        paths.extend(catalog_paths);
        let stamps = file_stamps(&paths);
        stamps.ends_with(&catalog_stamps).then_some((paths, stamps))
    }

    fn snapshot_paths(&self, data: &Value) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(dir) = &self.dir {
            for name in [
                "projects.toml",
                "settings.toml",
                "sidebar.bin",
                "worktree-comments.toml",
                "session-titles.json",
                "orchestration.json",
                "agents.toml",
                "agent-launches",
            ] {
                paths.push(dir.join(name));
            }
            // Use the actual signed rules names as well as user/profile metadata.
            paths.push(dir.join(krill_proto::agent::rules::PUBLIC_KEY_FILE_NAME));
            paths.push(dir.join(krill_proto::agent::rules::RULES_FILE_NAME));
            if let Some(sessions) = data.get("sessions").and_then(Value::as_array) {
                for id in sessions
                    .iter()
                    .filter_map(|session| session.get("id").and_then(Value::as_u64))
                {
                    paths.push(dir.join("agent-launches").join(format!("{id}.json")));
                }
            }
        }
        paths
    }

    fn catalog(
        &self,
        repositories: &[control::RepositoryTrees],
    ) -> Arc<super::workspaces::Catalog> {
        let mut cached = crate::session::lock(&self.catalog_cache);
        if let Some(old) = cached.as_ref()
            && old.repositories == repositories
            && old.at.elapsed() < GIT_TTL
            && old.stamps == file_stamps(&old.paths)
        {
            return Arc::clone(&old.catalog);
        }
        *cached = None;
        let mut paths = self.snapshot_paths(&Value::Null);
        for tree in repositories.iter().flat_map(|repo| &repo.worktrees) {
            let tree = PathBuf::from(std::ffi::OsString::from_vec(tree.clone()));
            let dot = tree.join(".git");
            paths.push(dot.clone());
            let git = if dot.is_dir() {
                dot
            } else {
                fs::read_to_string(&dot)
                    .ok()
                    .and_then(|text| {
                        text.strip_prefix("gitdir:")
                            .map(|git| tree.join(git.trim()))
                    })
                    .unwrap_or(dot)
            };
            paths.push(git.join("HEAD"));
            paths.push(git.join("commondir"));
        }
        let before = file_stamps(&paths);
        let catalog = Arc::new(super::workspaces::Catalog::load(
            self.dir.as_deref(),
            repositories,
        ));
        if before == file_stamps(&paths) {
            *cached = Some(CatalogSnapshot {
                repositories: repositories.to_vec(),
                paths,
                stamps: before,
                at: Instant::now(),
                catalog: Arc::clone(&catalog),
            });
        }
        catalog
    }

    fn snapshot(
        &self,
        catalog_used: &mut Option<Arc<super::workspaces::Catalog>>,
    ) -> io::Result<Value> {
        // 세션마다 요청이 2~3개라 요청마다 스레드·소켓·Hello를 새로 만들면 세션 수의 몇 배가 된다. 한 연결로 묶는다.
        let mut wire = self.wire()?;
        let mut control = |request: &Request| ask(&mut wire, request);
        let Reply::Sessions(sessions) = control(&Request::List)? else {
            return Err(invalid());
        };
        let states = match control(&Request::States)? {
            Reply::States(s) => s,
            _ => Vec::new(),
        };
        let sidebar = match control(&Request::Sidebar)? {
            Reply::Sidebar(s) => s,
            _ => control::SidebarData {
                sessions: Vec::new(),
                repositories: Vec::new(),
                branches: Vec::new(),
            },
        };
        let catalog = self.catalog(&sidebar.repositories);
        *catalog_used = Some(Arc::clone(&catalog));
        let truncated = sessions.len() > 64;
        let rows: Vec<_> = sessions
            .into_iter()
            .take(64)
            .map(|info| {
                let cwd = sidebar
                    .sessions
                    .iter()
                    .find(|s| s.session == info.session)
                    .map_or(&[][..], |s| s.cwd.as_slice());
                let group = session_group(info.session, cwd, &catalog.repositories);
                (info, cwd, group)
            })
            .collect();
        let git = self.git_summaries_swr(
            rows.iter()
                .filter_map(|(_, cwd, group)| git_cwd(cwd, group)),
        );
        let mut cards = Vec::new();
        let mut workspace_titles = std::collections::BTreeMap::new();
        // 데스크톱 스마트 정렬과 같은 워크트리 순위(승인대기 0 → 작업 중 2 → 완료 3 → 유휴 4).
        let mut ranks: std::collections::BTreeMap<PathBuf, u8> = std::collections::BTreeMap::new();
        // 데스크톱 "잠든 작업 공간 숨기기"의 깨어 있음 판정: 작업 중·대기·완료이거나 에이전트가 살아 있는 세션.
        let mut awake = std::collections::BTreeSet::new();
        for (info, _, group) in &rows {
            let Some(path) = group.path.as_deref() else {
                continue;
            };
            let status = states
                .iter()
                .find(|s| s.session == info.session)
                .map(|s| &s.status);
            if status.is_some_and(|s| {
                matches!(
                    s.state,
                    control::AgentState::Working
                        | control::AgentState::Waiting
                        | control::AgentState::Blocked
                        | control::AgentState::Done
                ) || !s.agent.is_empty() && s.state != control::AgentState::Exited
            }) {
                awake.insert(PathBuf::from(path));
            }
            let rank = states
                .iter()
                .find(|s| s.session == info.session)
                .map_or(4, |s| match s.status.state {
                    control::AgentState::Waiting | control::AgentState::Blocked => 0,
                    control::AgentState::Working => 2,
                    control::AgentState::Done => 3,
                    _ => 4,
                });
            ranks
                .entry(PathBuf::from(path))
                .and_modify(|r| *r = (*r).min(rank))
                .or_insert(rank);
        }
        let auto_titles = self
            .dir
            .as_deref()
            .map(krill_proto::git::load_session_auto_titles)
            .unwrap_or_default();
        for (info, cwd_bytes, group) in rows {
            if let Some(title) = auto_titles.get(&info.session)
                && let Some(path) = &group.path
            {
                workspace_titles
                    .entry(PathBuf::from(path))
                    .or_insert_with(|| title.clone());
            }
            let state = states
                .iter()
                .find(|s| s.session == info.session)
                .map_or(control::AgentStatus::UNKNOWN, |s| s.status.clone());
            let agent = if state.agent.is_empty() {
                match control(&Request::AgentLaunch {
                    session: info.session,
                }) {
                    Ok(Reply::AgentLaunch(Some(launch))) => launch.profile,
                    _ => String::new(),
                }
            } else {
                state.agent.clone()
            };
            let title = state
                .title
                .as_deref()
                .filter(|title| {
                    !krill_proto::git::is_default_branch_or_worktree_name(title)
                        && !title.eq_ignore_ascii_case("claude code")
                })
                .or_else(|| auto_titles.get(&info.session).map(String::as_str))
                .or(state.title.as_deref());
            let cwd = String::from_utf8_lossy(cwd_bytes);
            let lines = self
                .registry
                .upgrade()
                .and_then(|registry| registry.dashboard_tail(info.session))
                .unwrap_or_default();
            let git = git_cwd(cwd_bytes, &group)
                .and_then(|path| git.get(&path))
                .unwrap_or(&Value::Null);
            cards.push(json!({"id":info.session.0,"state":state.state,"agent":agent,"title":title,"foreground":state.foreground,"cwd":cwd,"lines":lines,"exited":info.status.is_some(),"branch":git.get("branch"),"added":git.get("added"),"deleted":git.get("deleted"),
                "repo_key":group.repo_key,"worktree_key":group.worktree_key,
                "group_label":group.label,"group_path":group.path,
                "state_source":state.source,"state_confidence":state.confidence,
                "state_since_unix_ns":state.since_unix_ns.to_string()}));
        }
        Ok(
            json!({"sessions":cards,"orchestration":self.orchestration()?,
            "repositories":catalog.repositories_json(),"workspaces":catalog.workspaces_json(&workspace_titles, &ranks, &awake),"truncated":truncated,
            "capabilities":{"snapshot_groups":true,"session_create":true,"session_close":true,"workspace_create":true,"worktree_create":true,"hybrid_chat":true,"chat_tail":true,"quick_commands":true},
            "quick_commands":quick_commands_json(self.dir.as_deref()),
            "host":self.host,"agents":agents_json(self.dir.as_deref())}),
        )
    }
    fn orchestration(&self) -> io::Result<Value> {
        let store = self
            .registry
            .upgrade()
            .ok_or_else(invalid)?
            .orchestration_snapshot()?;
        Ok(requests::snapshot(&store))
    }
    /// 만료된 값도 즉시 반환하고, cwd별로 하나의 백그라운드 조회만 예약한다.
    /// 최초 미캐시(cold) cwd는 총 80ms 시한 안에서만 즉시 수확하고 넘치면 백그라운드로 넘긴다.
    fn git_summaries_swr(
        &self,
        cwds: impl IntoIterator<Item = PathBuf>,
    ) -> HashMap<PathBuf, Value> {
        let mut found = HashMap::new();
        let mut cold = Vec::new();
        let mut stale = Vec::new();
        {
            let cache = crate::session::lock(&self.git_cache);
            for cwd in cwds {
                if found.contains_key(&cwd) {
                    continue;
                }
                let value = if let Some((at, value)) = cache.get(&cwd) {
                    if at.elapsed() >= GIT_TTL {
                        stale.push(cwd.clone());
                    }
                    value.clone()
                } else {
                    cold.push(cwd.clone());
                    Value::Null
                };
                found.insert(cwd, value);
            }
        }
        {
            let mut inflight = crate::session::lock(&self.git_inflight);
            cold.retain(|cwd| inflight.insert(cwd.clone()));
            stale.retain(|cwd| inflight.insert(cwd.clone()));
        }
        let mut deferred = Vec::new();
        if !cold.is_empty() {
            let queue = crate::git::queue().ok();
            let pending: Vec<_> = cold
                .into_iter()
                .map(|cwd| {
                    let rx = queue.map(|q| {
                        crate::git::changes::query_with_branch(
                            q,
                            cwd.clone(),
                            Arc::new(AtomicBool::new(false)),
                        )
                    });
                    (cwd, rx)
                })
                .collect();
            let deadline = Instant::now() + Duration::from_millis(80);
            for (cwd, rx) in pending {
                let Some(rx) = rx else {
                    cache_git_summary(&self.git_cache, cwd.clone(), Value::Null);
                    crate::session::lock(&self.git_inflight).remove(&cwd);
                    continue;
                };
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(res) => {
                        let value = res.ok().map_or(Value::Null, |(changes, branch)| {
                            let (added, deleted) = totals(&changes);
                            json!({"branch":branch,"added":added,"deleted":deleted})
                        });
                        cache_git_summary(&self.git_cache, cwd.clone(), value.clone());
                        crate::session::lock(&self.git_inflight).remove(&cwd);
                        found.insert(cwd, value);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        deferred.push((cwd, Some(rx)));
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        cache_git_summary(&self.git_cache, cwd.clone(), Value::Null);
                        crate::session::lock(&self.git_inflight).remove(&cwd);
                    }
                }
            }
        }
        if !deferred.is_empty() || !stale.is_empty() {
            let cache = Arc::clone(&self.git_cache);
            let inflight = Arc::clone(&self.git_inflight);
            let revision = Arc::clone(&self.git_revision);
            let mut scheduled: Vec<PathBuf> = deferred.iter().map(|(cwd, _)| cwd.clone()).collect();
            scheduled.extend(stale.iter().cloned());
            let spawned = thread::Builder::new()
                .name("mobile-git-swr".into())
                .spawn(move || {
                    let queue = crate::git::queue().ok();
                    let mut pending = deferred;
                    pending.extend(stale.into_iter().map(|cwd| {
                        let rx = queue.map(|q| {
                            crate::git::changes::query_with_branch(
                                q,
                                cwd.clone(),
                                Arc::new(AtomicBool::new(false)),
                            )
                        });
                        (cwd, rx)
                    }));
                    for (cwd, rx) in pending {
                        let value = rx
                            .and_then(|rx| rx.recv().ok())
                            .and_then(Result::ok)
                            .map_or(Value::Null, |(changes, branch)| {
                                let (added, deleted) = totals(&changes);
                                json!({"branch":branch,"added":added,"deleted":deleted})
                            });
                        cache_git_summary(&cache, cwd.clone(), value);
                        crate::session::lock(&inflight).remove(&cwd);
                        revision.fetch_add(1, Ordering::Relaxed);
                    }
                });
            if spawned.is_err() {
                let mut inflight = crate::session::lock(&self.git_inflight);
                for cwd in scheduled {
                    inflight.remove(&cwd);
                }
            }
        }
        found
    }
    /// 카드용 git 요약을 cwd별로 모은다. 캐시에 없는 cwd는 큐에 전부 먼저 넣고 나서 받는다
    /// (큐 워커 [`crate::git::MAX_CONCURRENT`]개가 서로 다른 cwd를 동시에 돈다. 같은 키만 직렬).
    #[cfg(all(test, unix))]
    fn git_summaries(&self, cwds: impl IntoIterator<Item = PathBuf>) -> HashMap<PathBuf, Value> {
        let mut found = HashMap::new();
        let mut missing = Vec::new();
        {
            let mut cache = crate::session::lock(&self.git_cache);
            cache.retain(|_, (at, _)| at.elapsed() < GIT_TTL);
            for cwd in cwds {
                if let Some((_, value)) = cache.get(&cwd) {
                    found.insert(cwd, value.clone());
                } else if !missing.contains(&cwd) {
                    missing.push(cwd);
                }
            }
        }
        let queue = crate::git::queue().ok();
        let pending: Vec<_> = missing
            .into_iter()
            .map(|cwd| {
                let rx = queue.map(|q| {
                    crate::git::changes::query_with_branch(
                        q,
                        cwd.clone(),
                        Arc::new(AtomicBool::new(false)),
                    )
                });
                (cwd, rx)
            })
            .collect();
        for (cwd, rx) in pending {
            let value = rx
                .and_then(|rx| rx.recv().ok())
                .and_then(Result::ok)
                .map_or(Value::Null, |(changes, branch)| {
                    let (added, deleted) = totals(&changes);
                    json!({"branch":branch,"added":added,"deleted":deleted})
                });
            cache_git_summary(&self.git_cache, cwd.clone(), value.clone());
            found.insert(cwd, value);
        }
        found
    }
    fn git(&self, session: u64, path: Option<&str>) -> io::Result<Value> {
        let cwd = self.cwd(session)?;
        let root = git_root(&cwd)?;
        let queue = crate::git::queue().map_err(io::Error::other)?;
        let (changes, branch) = crate::git::changes::query_with_branch(
            queue,
            cwd.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .recv()
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
        let (added, deleted) = totals(&changes);
        let files: Vec<_> = changes
            .files
            .iter()
            .map(|file| String::from_utf8_lossy(&file.path).into_owned())
            .collect();
        let patch = if let Some(raw_path) = path {
            let Some(rel_path) = git_relative_path(&root, &cwd, raw_path, &files) else {
                return Ok(
                    json!({"diff_status":"outside_scope","patch":"","files":files,"limited":changes.limited}),
                );
            };
            if !changes.limited && !files.iter().any(|p| p == &rel_path) {
                return Ok(
                    json!({"diff_status":"no_current_diff","patch":"","files":files,"limited":changes.limited}),
                );
            }
            read_patch(&root, &rel_path)?
        } else {
            String::new()
        };
        Ok(
            json!({"branch":branch,"added":added,"deleted":deleted,"files":files,"patch":patch,"limited":changes.limited}),
        )
    }
}
/// 표시용 lossy UTF-8과 분리해 OS 경로 바이트를 그대로 식별한다.
#[derive(Debug)]
struct SessionGroup {
    repo_key: Option<String>,
    worktree_key: String,
    label: Option<String>,
    path: Option<String>,
}
fn session_group(
    session: SessionId,
    cwd: &[u8],
    repositories: &[control::RepositoryTrees],
) -> SessionGroup {
    if cwd.is_empty() {
        return SessionGroup {
            repo_key: None,
            worktree_key: format!("unknown:{}", session.0),
            label: None,
            path: None,
        };
    }
    let cwd_path = Path::new(std::ffi::OsStr::from_bytes(cwd));
    let matched = repositories
        .iter()
        .flat_map(|repo| {
            repo.worktrees
                .iter()
                .filter(|tree| !tree.is_empty())
                .map(move |tree| (repo, tree))
        })
        .filter_map(|(repo, tree)| {
            let path = Path::new(std::ffi::OsStr::from_bytes(tree));
            cwd_path
                .starts_with(path)
                .then(|| (repo, tree, path.components().count()))
        })
        .max_by_key(|(_, _, depth)| *depth);
    let (repo_key, bytes) = matched.map_or((None, cwd), |(repo, tree, _)| {
        (Some(hex(&repo.repo)), tree.as_slice())
    });
    SessionGroup {
        repo_key,
        worktree_key: hex(bytes),
        label: Some(path_label(bytes)),
        path: Some(String::from_utf8_lossy(bytes).into_owned()),
    }
}
/// 카드 git 요약을 물을 cwd. 저장소 밖(`repo_key` 없음)·cwd 미상은 git을 부르지 않는다(값은 `null`).
fn git_cwd(cwd: &[u8], group: &SessionGroup) -> Option<PathBuf> {
    group
        .repo_key
        .as_ref()
        .map(|_| PathBuf::from(std::ffi::OsStr::from_bytes(cwd)))
}
fn totals(changes: &krill_proto::git::Changes) -> (u64, u64) {
    changes
        .files
        .iter()
        .flat_map(|file| [file.staged, file.unstaged])
        .flatten()
        .fold((0_u64, 0_u64), |(added, deleted), stat| {
            (
                added.saturating_add(stat.added.unwrap_or(0)),
                deleted.saturating_add(stat.deleted.unwrap_or(0)),
            )
        })
}
fn path_label(bytes: &[u8]) -> String {
    let path = Path::new(std::ffi::OsStr::from_bytes(bytes));
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}
fn host_metadata() -> Value {
    #[cfg(windows)]
    let display_name = crate::sys::hostname().ok();
    #[cfg(unix)]
    let display_name = {
        let mut command = Command::new("/bin/hostname");
        bounded_command(&mut command, Duration::from_secs(1), 1024)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    };
    let platform = match std::env::consts::OS {
        "macos" => Some("macos"),
        "linux" => Some("linux"),
        "windows" => Some("windows"),
        _ => None,
    };
    json!({"display_name":display_name,"platform":platform})
}
/// 제어 요청 하나를 보내고 응답을 받는다. 레지스트리 제어 연결은 요청을 여러 개 이어 받는다.
fn ask(wire: &mut Wire, request: &Request) -> io::Result<Reply> {
    wire.socket.write_all(&request.encode()?)?;
    let (kind, body) = ipc::read_frame(&mut wire.socket)?.ok_or_else(invalid)?;
    Reply::decode(kind, &body).map_err(Into::into)
}
struct Wire {
    socket: UnixStream,
    worker: Option<JoinHandle<Option<()>>>,
}
impl Drop for Wire {
    fn drop(&mut self) {
        self.socket.shutdown(Shutdown::Both).ok();
        if let Some(worker) = self.worker.take() {
            worker.join().ok();
        }
    }
}
fn to_value(v: impl serde::Serialize) -> io::Result<Value> {
    serde_json::to_value(v).map_err(io::Error::other)
}

fn save_photo(mime: &str, data: &str) -> io::Result<PathBuf> {
    let extension = match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        _ => return Err(invalid()),
    };
    let bytes = decode_base64(data)?;
    if bytes.is_empty() || bytes.len() > 512 * 1024 {
        return Err(invalid());
    }
    let dir = std::env::temp_dir().join("krill-paste");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || !crate::os::private_to_me(&meta, 0o077) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let path = dir.join(format!("mobile-{}.{extension}", hex(&random()?)));
    let mut file = crate::os::open_no_follow(
        OpenOptions::new().create_new(true).write(true).mode(0o600),
        &path,
    )?;
    file.write_all(&bytes)?;
    Ok(path)
}
fn decode_base64(data: &str) -> io::Result<Vec<u8>> {
    if data.len() > 700_000 {
        return Err(invalid());
    }
    let mut bits = 0_u32;
    let mut count = 0;
    let mut out = Vec::new();
    let mut padded = false;
    for byte in data.bytes() {
        if byte == b'=' {
            padded = true;
            continue;
        }
        if padded {
            return Err(invalid());
        }
        let n = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err(invalid()),
        };
        bits = (bits << 6) | u32::from(n);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push(u8::try_from((bits >> count) & 255).map_err(|_| invalid())?);
        }
    }
    if count >= 6 || (bits & ((1 << count) - 1)) != 0 {
        return Err(invalid());
    }
    Ok(out)
}
fn cache_git_summary(
    cache: &Mutex<HashMap<PathBuf, (Instant, Value)>>,
    cwd: PathBuf,
    value: Value,
) {
    let mut cache = crate::session::lock(cache);
    cache.insert(cwd, (Instant::now(), value));
    while cache.len() > 64 {
        let oldest = cache
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(path, _)| path.clone());
        if let Some(path) = oldest {
            cache.remove(&path);
        } else {
            break;
        }
    }
}

fn mobile_git_command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
    ] {
        command.env_remove(key);
    }
    command.arg("--literal-pathspecs");
    command
}

fn git_root(cwd: &Path) -> io::Result<PathBuf> {
    let bytes = bounded_command(
        mobile_git_command(cwd).args(["rev-parse", "--show-toplevel"]),
        Duration::from_secs(5),
        16 * 1024,
    )
    .map_err(|_| io::Error::other("Not a Git repository"))?;
    // Git terminates this path with a newline; spaces in a repository name are significant.
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    fs::canonicalize(PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec())))
}

fn canonical_missing(path: &Path) -> Option<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Some(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let name = path.file_name()?;
            Some(canonical_missing(path.parent()?)?.join(name))
        }
        Err(_) => None,
    }
}

fn git_relative_path(root: &Path, cwd: &Path, raw: &str, files: &[String]) -> Option<String> {
    use std::path::Component;
    let input = Path::new(raw);
    let normalize = |path: &Path| {
        let mut out = PathBuf::new();
        for part in path.components() {
            match part {
                Component::CurDir => (),
                Component::ParentDir => {
                    if !out.pop() {
                        return None;
                    }
                }
                other => out.push(other.as_os_str()),
            }
        }
        Some(out)
    };
    let normalized = normalize(input)?;
    let candidate = if input.is_absolute() {
        normalized
    } else if files.iter().any(|p| Path::new(p) == normalized) {
        root.join(normalized)
    } else {
        let local = normalize(&cwd.join(input))?;
        if local.exists() || !root.join(&normalized).exists() {
            local
        } else {
            root.join(normalized)
        }
    };
    // Resolve parent aliases without following a final symlink: Git diffs the link
    // itself, and untracked_patch must still be able to reject that link.
    let canonical = canonical_missing(candidate.parent()?)?.join(candidate.file_name()?);
    let relative = canonical.strip_prefix(root).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    relative.to_str().map(str::to_owned)
}

fn read_patch(cwd: &Path, path: &str) -> io::Result<String> {
    let has_head = bounded_command(
        mobile_git_command(cwd).args(["rev-parse", "--verify", "HEAD"]),
        Duration::from_secs(5),
        1024,
    )
    .is_ok();
    let base = if has_head {
        "HEAD"
    } else {
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    };
    let patch = bounded_command(
        mobile_git_command(cwd).args([
            "-c",
            "diff.maxFiles=1",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            base,
            "--",
            path,
        ]),
        Duration::from_secs(5),
        128 * 1024,
    )?;
    if !patch.is_empty() {
        return Ok(String::from_utf8_lossy(&patch).into_owned());
    }
    if bounded_command(
        mobile_git_command(cwd).args(["ls-files", "--error-unmatch", "--", path]),
        Duration::from_secs(5),
        16 * 1024,
    )
    .is_ok()
    {
        return Ok(String::new());
    }
    untracked_patch(cwd, path)
}

fn untracked_patch(root: &Path, path: &str) -> io::Result<String> {
    const LIMIT: u64 = 128 * 1024;
    let target = root.join(path);
    let meta = match fs::symlink_metadata(&target) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e),
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Ok(String::new());
    }
    if meta.len() > LIMIT {
        return Err(io::Error::other("File exceeds 128 KiB diff limit"));
    }
    let canonical_root = fs::canonicalize(root)?;
    if !fs::canonicalize(&target)?.starts_with(&canonical_root) {
        return Err(io::Error::other("File is outside the repository"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(&target)?;
    if !file.metadata()?.is_file() {
        return Ok(String::new());
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > LIMIT {
        return Err(io::Error::other("File exceeds 128 KiB diff limit"));
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(format!("Binary files /dev/null and b/{path} differ\n"));
    };
    if bytes.contains(&0) {
        return Ok(format!("Binary files /dev/null and b/{path} differ\n"));
    }
    let lines = text.split_inclusive('\n').count();
    let filename = format!("b/{path}");
    let filename = if path.chars().any(char::is_control) {
        serde_json::to_string(&filename).map_err(io::Error::other)?
    } else {
        filename
    };
    let mut patch = format!("--- /dev/null\n+++ {filename}\n@@ -0,0 +1,{lines} @@\n");
    for line in text.split_inclusive('\n') {
        patch.push('+');
        patch.push_str(line);
        if !line.ends_with('\n') {
            patch.push_str("\n\\ No newline at end of file\n");
        }
    }
    if u64::try_from(patch.len()).unwrap_or(u64::MAX) > LIMIT {
        return Err(io::Error::other("mobile command output exceeds limit"));
    }
    Ok(patch)
}
fn bounded_command(command: &mut Command, timeout: Duration, limit: usize) -> io::Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| {
        let mut pipe = child.stdout.take().ok_or_else(invalid)?;
        crate::child::nonblocking(&pipe)?;
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    out.extend(buffer.iter().take(n).copied());
                    if out.len() > limit {
                        return Err(io::Error::other("mobile command output exceeds limit"));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e),
            }
        }
        loop {
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(out)
                } else {
                    Err(io::Error::other("mobile command failed"))
                };
            }
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    child.kill().ok();
    child.wait().ok();
    result
}

/// Bracketed Paste(`\x1b[200~...\x1b[201~`)로 감싸고 내부 ESC(`0x1b`)·ETX(`0x03`) 등 제어 바이트를 제거한다(OWE-944 / MK-241).
fn format_bracketed_paste(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len().saturating_add(12));
    out.extend_from_slice(b"\x1b[200~");
    for &b in text.as_bytes() {
        if matches!(b, b'\n' | b'\r' | b'\t') || (b >= 0x20 && b != 0x7f) {
            out.push(b);
        }
    }
    out.extend_from_slice(b"\x1b[201~");
    out
}

fn sanitize_plain_input(text: &str) -> Vec<u8> {
    text.as_bytes()
        .iter()
        .copied()
        .filter(|&b| b == b'\t' || (b >= 0x20 && b != 0x7f))
        .collect()
}

/// 터미널 화면 텍스트에서 현재 선택 포인터(`❯`·`›`·`▸`·`>`)가 위치한 질문 옵션 인덱스(0-based)를 추정한다.
/// 감지할 수 없으면 기본값 `0`을 반환한다.
#[cfg(test)]
fn detect_question_cursor_index(screen_text: &str, option_count: usize) -> usize {
    let max_idx = option_count.max(16);
    let tail: Vec<&str> = screen_text
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(40)
        .collect();
    for line in &tail {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed
            .strip_prefix('❯')
            .or_else(|| trimmed.strip_prefix('›'))
            .or_else(|| trimmed.strip_prefix('▸'))
            .or_else(|| trimmed.strip_prefix('>'))
        else {
            continue;
        };
        let rest = rest.trim_start();
        let rest = rest
            .strip_prefix("[ ]")
            .or_else(|| rest.strip_prefix("[x]"))
            .or_else(|| rest.strip_prefix("[X]"))
            .or_else(|| rest.strip_prefix("[✓]"))
            .or_else(|| rest.strip_prefix("( )"))
            .or_else(|| rest.strip_prefix("(*)"))
            .unwrap_or(rest)
            .trim_start();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty()
            && let Some(after) = rest.strip_prefix(&digits)
            && (after.starts_with('.') || after.starts_with(')'))
            && let Ok(num) = digits.parse::<usize>()
            && num >= 1
            && num <= max_idx.saturating_add(1)
        {
            return num - 1;
        }
    }
    0
}

#[cfg(test)]
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

#[cfg(test)]
fn plan_question_keystrokes(
    selected: &[usize],
    option_count: usize,
    multi_select: bool,
    custom_text: Option<&str>,
    bracketed: bool,
) -> io::Result<Vec<Vec<u8>>> {
    plan_question_keystrokes_from(
        selected,
        option_count,
        multi_select,
        custom_text,
        bracketed,
        0,
    )
}

/// 인터랙티브 질문 카드(`AskUserQuestion` 등)의 응답을 현재 TUI 커서 위치(`initial_cursor`) 기준
/// 60ms 간격 키스트로크 시퀀스로 계획한다(OWE-946 / MK-243).
#[cfg(test)]
fn plan_question_keystrokes_from(
    selected: &[usize],
    option_count: usize,
    multi_select: bool,
    custom_text: Option<&str>,
    bracketed: bool,
    initial_cursor: usize,
) -> io::Result<Vec<Vec<u8>>> {
    let capped_options = option_count.min(16);
    let custom = custom_text.map(str::trim).filter(|s| !s.is_empty());
    if let Some(t) = custom
        && t.len() > 4096
    {
        return Err(invalid());
    }
    let mut steps: Vec<Vec<u8>> = Vec::new();
    let mut cursor = initial_cursor.min(capped_options.max(16));

    if multi_select {
        let mut sorted: Vec<usize> = selected
            .iter()
            .copied()
            .filter(|&idx| idx < capped_options.max(16))
            .collect();
        sorted.sort_unstable();
        sorted.dedup();
        if sorted.is_empty() && custom.is_none() {
            return Err(invalid());
        }
        for target in sorted {
            step_cursor(&mut steps, &mut cursor, target);
            steps.push(b" ".to_vec());
        }
        if let Some(text) = custom {
            step_cursor(&mut steps, &mut cursor, capped_options);
            steps.push(b" ".to_vec());
            let payload = if bracketed || text.contains(['\n', '\r']) {
                format_bracketed_paste(text)
            } else {
                sanitize_plain_input(text)
            };
            steps.push(payload);
            steps.push(b"\r".to_vec());
        } else {
            steps.push(b"\r".to_vec());
        }
        return Ok(steps);
    }

    if let Some(text) = custom {
        step_cursor(&mut steps, &mut cursor, capped_options);
        if capped_options > 0 {
            steps.push(b"\r".to_vec());
        }
        let payload = if bracketed || text.contains(['\n', '\r']) {
            format_bracketed_paste(text)
        } else {
            sanitize_plain_input(text)
        };
        steps.push(payload);
        steps.push(b"\r".to_vec());
        return Ok(steps);
    }

    let Some(&choice) = selected.first() else {
        return Err(invalid());
    };
    if choice >= 16 || (capped_options > 0 && choice >= capped_options) {
        return Err(invalid());
    }
    step_cursor(&mut steps, &mut cursor, choice);
    steps.push(b"\r".to_vec());
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick_commands_test_dir() -> PathBuf {
        std::env::temp_dir().join(format!("pwa-quick-commands-{}", hex(&random().unwrap())))
    }

    fn rpc(backend: &Backend, request: &Value) -> Value {
        serde_json::from_str(&backend.request(&request.to_string())).unwrap()
    }

    #[test]
    #[cfg(unix)]
    fn save_quick_commands_preserves_settings_and_creates_private_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = quick_commands_test_dir();
        let mut backend = test_backend();
        backend.dir = Some(dir.clone());
        let commands = json!([{"name":"  orchestration  ","text":"\r\n/krill-orchestration \r\n"}]);
        let expected = json!([{"name":"orchestration","text":"/krill-orchestration "}]);
        let saved = rpc(
            &backend,
            &json!({"action":"save_quick_commands","commands":commands}),
        );
        assert_eq!(saved["ok"], true);
        assert_eq!(saved["data"]["quick_commands"], expected);
        let path = dir.join("settings.toml");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(quick_commands_json(Some(&dir)), expected);

        let preserved = "# user preferences\nterminal_font_size = 15 # keep font\nfuture = { enabled = true }\n\n[[webhooks]]\n# keep webhook\nurl = 'https://example.test/hook'\nevents = ['done']\n";
        fs::write(&path, format!("{preserved}\n[[quick_commands]]\nname = 'old'\ntext = 'old text'\nfuture_command = 42\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let saved = rpc(
            &backend,
            &json!({"action":"save_quick_commands","commands":[{"name":"new","text":"\n  prompt\r\n\tline\rtail \n"}]}),
        );
        assert_eq!(saved["ok"], true);
        assert_eq!(
            saved["data"]["quick_commands"],
            json!([{"name":"new","text":"  prompt\n\tline\ntail "}])
        );
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.starts_with(preserved));
        assert!(content.contains("future_command = 42"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            quick_commands_json(Some(&dir)),
            saved["data"]["quick_commands"]
        );
        let cleared = rpc(
            &backend,
            &json!({"action":"save_quick_commands","commands":[]}),
        );
        assert_eq!(cleared["ok"], true);
        assert_eq!(quick_commands_json(Some(&dir)), json!([]));
        assert!(fs::read_to_string(&path).unwrap().starts_with(preserved));
        let mut names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["settings.toml", "settings.toml.lock"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn snapshot_quick_commands_refresh_after_mobile_save_and_desktop_edit() {
        let dir = quick_commands_test_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("settings.toml"),
            "[[quick_commands]]\nname = 'desktop'\ntext = '/desktop '\n",
        )
        .unwrap();
        let registry = Arc::new(Registry::new(None, None));
        let mut backend = test_backend();
        backend.dir = Some(dir.clone());
        backend.registry = Arc::downgrade(&registry);
        let first = rpc(&backend, &json!({"action":"snapshot"}));
        assert_eq!(first["ok"], true);
        assert_eq!(first["data"]["capabilities"]["quick_commands"], true);
        assert_eq!(
            first["data"]["quick_commands"],
            json!([{"name":"desktop","text":"/desktop "}])
        );
        assert!(crate::session::lock(&backend.snapshot_cache).is_some());
        let etag = first["data"]["etag"].as_str().unwrap();
        assert_eq!(
            backend.snapshot_since(Some(etag)).unwrap()["unchanged"],
            true
        );

        let saved = rpc(
            &backend,
            &json!({"action":"save_quick_commands","commands":[{"name":"mobile","text":"/mobile "}]}),
        );
        assert_eq!(saved["ok"], true);
        assert!(crate::session::lock(&backend.snapshot_cache).is_none());
        let refreshed = backend.snapshot_since(Some(etag)).unwrap();
        assert_ne!(refreshed["etag"], first["data"]["etag"]);
        assert_eq!(refreshed["quick_commands"], saved["data"]["quick_commands"]);

        let replacement = dir.join("settings.new");
        fs::write(
            &replacement,
            "[[quick_commands]]\nname = 'desktop edit'\ntext = '/desktop-updated '\n",
        )
        .unwrap();
        fs::rename(replacement, dir.join("settings.toml")).unwrap();
        let desktop = backend.snapshot_since(refreshed["etag"].as_str()).unwrap();
        assert_ne!(desktop["etag"], refreshed["etag"]);
        assert_eq!(
            desktop["quick_commands"],
            json!([{"name":"desktop edit","text":"/desktop-updated "}])
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn quick_commands_wait_for_an_external_settings_writer_and_preserve_its_changes() {
        let dir = quick_commands_test_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        fs::write(&path, "terminal_font_size = 15\n").unwrap();
        let guard = krill_proto::config_document::lock_toml(&path).unwrap();
        let mut backend = test_backend();
        backend.dir = Some(dir.clone());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = backend.save_quick_commands(vec![QuickCommandEntry {
                name: "mobile".into(),
                text: "/skill ".into(),
            }]);
            done_tx.send(result).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        fs::write(&path, "terminal_font_size = 19\nfuture_flag = true\n").unwrap();
        drop(guard);
        done_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
        let saved: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["terminal_font_size"].as_integer(), Some(19));
        assert_eq!(saved["future_flag"].as_bool(), Some(true));
        assert_eq!(saved["quick_commands"][0]["text"].as_str(), Some("/skill "));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_quick_commands_rejects_invalid_requests_without_changing_settings() {
        let dir = quick_commands_test_dir();
        fs::create_dir_all(&dir).unwrap();
        let original = "terminal_font_size = 15\n";
        let path = dir.join("settings.toml");
        fs::write(&path, original).unwrap();
        let mut backend = test_backend();
        backend.dir = Some(dir.clone());
        for commands in [
            json!([{"name":" \t ","text":"valid"}]),
            json!([{"name":"valid","text":" \r\n\t"}]),
            json!([{"name":"invalid\nname","text":"valid"}]),
            json!([{"name":"invalid\u{7f}name","text":"valid"}]),
            json!([{"name":"valid","text":"invalid\u{1b}text"}]),
            json!([{"name":"valid","text":"invalid\u{0}text"}]),
            json!([{"name":"x".repeat(121),"text":"valid"}]),
            json!([{"name":"valid","text":"x".repeat(16 * 1024 + 1)}]),
            json!(vec![json!({"name":"valid","text":"valid"}); 129]),
            json!([{"name":"missing text"}]),
        ] {
            let response = rpc(
                &backend,
                &json!({"action":"save_quick_commands","commands":commands}),
            );
            assert_eq!(response["ok"], false, "{response}");
            assert_eq!(fs::read_to_string(&path).unwrap(), original);
        }
        let maximum = json!([{"name":"x".repeat(120),"text":"x".repeat(16 * 1024)}]);
        assert_eq!(
            rpc(
                &backend,
                &json!({"action":"save_quick_commands","commands":maximum})
            )["ok"],
            true
        );
        let maximum_count = json!(vec![json!({"name":"valid","text":"valid"}); 128]);
        assert_eq!(
            rpc(
                &backend,
                &json!({"action":"save_quick_commands","commands":maximum_count})
            )["ok"],
            true
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_quick_commands_normalizes_filters_caps_and_tolerates_missing_or_invalid_files() {
        assert!(load_quick_commands(None).is_empty());
        let dir = quick_commands_test_dir();
        assert!(load_quick_commands(Some(&dir)).is_empty());
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        fs::write(&path, "invalid [").unwrap();
        assert!(load_quick_commands(Some(&dir)).is_empty());
        fs::write(&path, "terminal_font_size = 15\n").unwrap();
        assert!(load_quick_commands(Some(&dir)).is_empty());
        let mut commands = vec![
            QuickCommandEntry {
                name: "  ".into(),
                text: "valid".into(),
            },
            QuickCommandEntry {
                name: "valid".into(),
                text: " \r\n\t".into(),
            },
        ];
        commands.extend((0..130).map(|i| QuickCommandEntry {
            name: format!(" command {i} "),
            text: "\r\n  /skill \r\nnext\rtail \r\n".into(),
        }));
        fs::write(
            &path,
            toml::to_string(&QuickCommandsFile {
                quick_commands: commands,
            })
            .unwrap(),
        )
        .unwrap();
        let loaded = load_quick_commands(Some(&dir));
        assert_eq!(loaded.len(), 128);
        assert_eq!(loaded.first().unwrap().name, "command 0");
        assert_eq!(loaded.first().unwrap().text, "  /skill \nnext\ntail ");
        assert_eq!(loaded.last().unwrap().name, "command 127");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn historical_approval_and_question_requests_fail_closed_before_input() {
        let backend = test_backend();
        // No Registry is present: these errors prove no attach or PTY write was attempted.
        for request in [
            r#"{"action":"approve","session":7,"value":"y"}"#,
            r#"{"action":"approve","session":7,"value":"n"}"#,
            r#"{"action":"answer_question","session":7,"selected":[0],"option_count":2}"#,
            r#"{"action":"answer_question","session":7,"selected":[1],"option_count":2,"multi_select":true,"custom_text":"Other"}"#,
        ] {
            for _ in 0..2 {
                let response: Value = serde_json::from_str(&backend.request(request)).unwrap();
                assert_eq!(response["ok"], false);
                assert!(
                    response["error"]
                        .as_str()
                        .unwrap()
                        .contains("Confirm in Term")
                );
            }
        }
    }

    #[test]
    fn prompt_accepts_one_prepared_photo_and_rejects_bad_preparation_before_input() {
        assert!(
            serde_json::from_str::<Action>(r#"{"action":"prompt","session":7,"text":"plain"}"#)
                .is_ok()
        );
        let request = r#"{"action":"prompt","session":7,"text":"image then text","photo":{"mime":"image/jpeg","data":"Zg=="}}"#;
        let Action::Prompt {
            text,
            photo: Some(photo),
            ..
        } = serde_json::from_str::<Action>(request).unwrap()
        else {
            panic!("prepared photo must survive JSON decoding");
        };
        assert_eq!(text, "image then text");
        assert_eq!(photo.mime, "image/jpeg");
        assert_eq!(photo.data, "Zg==");
        let backend = test_backend();
        for photo in [
            json!({"mime":"text/html","data":"Zg=="}),
            json!({"mime":"image/jpeg","data":"invalid"}),
        ] {
            let response: Value = serde_json::from_str(
                &backend.request(
                    &json!({"action":"prompt","session":7,"text":"keep draft","photo":photo})
                        .to_string(),
                ),
            )
            .unwrap();
            assert_eq!(response["ok"], false);
        }
    }

    #[test]
    fn mobile_chat_small_append_pages_replay_with_original_display_ids() {
        use crate::agent_history::chat::{poll_transcript_file, replay_page};
        use krill_proto::agent_history::VaultAgent;
        let dir = std::env::temp_dir().join(format!("mobile-replay-pages-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        let mut offset = 0;
        let mut observed = Vec::new();
        for index in 0..300 {
            let text = if index == 48 || index == 49 {
                "same text".to_owned()
            } else {
                format!("message {index}")
            };
            writeln!(
                file,
                "{}",
                json!({"type":"user","message":{"role":"user","content":text}})
            )
            .unwrap();
            let snapshot = poll_transcript_file(VaultAgent::Claude, "sid", &path, offset);
            assert_eq!(snapshot.items.len(), 1);
            let end = snapshot.next_offset;
            observed.push((offset, end, chat_page_value(snapshot, offset).unwrap()));
            offset = end;
        }
        for (start, end, original) in &observed {
            // Reading after all appends would return up to 128 records; replay retains
            // the original one-record partition and ID rather than comparing text.
            let current = poll_transcript_file(VaultAgent::Claude, "sid", &path, *start);
            let replay = replay_page(current, *start, *end).unwrap();
            assert_eq!(chat_page_value(replay, *start).unwrap(), *original);
        }
        let a = observed.get(48).unwrap();
        let b = observed.get(49).unwrap();
        assert_eq!(a.2["items"][0]["text"], b.2["items"][0]["text"]);
        assert_ne!(a.2["items"][0]["id"], b.2["items"][0]["id"]);
        let action: Action = serde_json::from_value(json!({
            "action":"chat_poll","session":7,"offset":a.0,"end_offset":a.1
        }))
        .unwrap();
        assert!(matches!(action, Action::ChatPoll { end_offset: Some(end), .. } if end == a.1));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn mobile_chat_page_identity_is_additive_and_replays_without_changing_legacy_snapshot() {
        use krill_proto::agent_history::{ChatItem, ChatPollSnapshot};
        let snapshot = ChatPollSnapshot {
            items: vec![ChatItem {
                text: "stable".into(),
                ..ChatItem::default()
            }],
            next_offset: 64,
            bound: true,
            ..ChatPollSnapshot::default()
        };
        let legacy = serde_json::to_value(&snapshot).unwrap();
        assert!(legacy["items"][0].get("id").is_none());
        let page = chat_page_value(snapshot.clone(), 32).unwrap();
        assert_eq!(page, chat_page_value(snapshot.clone(), 32).unwrap());
        assert_eq!(page["items"][0]["id"], "32:0");
        assert_eq!(page["reply_mode"], "review_in_term");
        let reset = chat_page_value(
            ChatPollSnapshot {
                reset: true,
                ..snapshot
            },
            512,
        )
        .unwrap();
        assert_eq!(reset["page_start"], 0);
        assert_eq!(reset["items"][0]["id"], "0:0");
    }

    #[test]
    fn bracketed_paste_and_question_keystroke_planner_work() {
        assert_eq!(
            format_bracketed_paste("hi\x1b[201~\nthere"),
            b"\x1b[200~hi[201~\nthere\x1b[201~"
        );
        // 단일 선택 2번째(인덱스 1): Down 1번 + Enter
        let single = plan_question_keystrokes(&[1], 3, false, None, true).unwrap();
        assert_eq!(single, vec![b"\x1b[B".to_vec(), b"\r".to_vec()]);

        // TUI에서 이미 2번째(인덱스 1)로 커서가 내려간 상태에서 1번째(인덱스 0)를 고르면 Up 1번 + Enter
        let screen = "Which option?\n  1. First\n❯ 2. Second\n  3. Third\n";
        let detected = detect_question_cursor_index(screen, 3);
        assert_eq!(detected, 1);
        let relative_up =
            plan_question_keystrokes_from(&[0], 3, false, None, true, detected).unwrap();
        assert_eq!(relative_up, vec![b"\x1b[A".to_vec(), b"\r".to_vec()]);

        // 다중 선택 [0, 2]: Space, Down, Down, Space, Enter
        let multi = plan_question_keystrokes(&[2, 0], 3, true, None, true).unwrap();
        assert_eq!(
            multi,
            vec![
                b" ".to_vec(),
                b"\x1b[B".to_vec(),
                b"\x1b[B".to_vec(),
                b" ".to_vec(),
                b"\r".to_vec()
            ]
        );

        // 커스텀 텍스트 입력(옵션 2개 뒤 Other): Down, Down, Enter, Paste, Enter
        let custom = plan_question_keystrokes(&[], 2, false, Some("직접 입력"), true).unwrap();
        assert_eq!(
            custom,
            vec![
                b"\x1b[B".to_vec(),
                b"\x1b[B".to_vec(),
                b"\r".to_vec(),
                b"\x1b[200~\xec\xa7\x81\xec\xa0\x91 \xec\x9e\x85\xeb\xa0\xa5\x1b[201~".to_vec(),
                b"\r".to_vec(),
            ]
        );
    }

    #[test]
    fn snapshot_groups_use_deepest_path_components_and_keep_empty_repositories() {
        let repositories = vec![
            control::RepositoryTrees {
                repo: b"/repo".to_vec(),
                worktrees: vec![b"/repo/a".to_vec(), b"/repo/a/nested".to_vec()],
            },
            control::RepositoryTrees {
                repo: b"/empty".to_vec(),
                worktrees: vec![],
            },
        ];
        let group = session_group(SessionId(1), b"/repo/a/nested/src", &repositories);
        assert_eq!(group.repo_key, Some(hex(b"/repo")));
        assert_eq!(group.worktree_key, hex(b"/repo/a/nested"));
        assert_eq!(group.label.as_deref(), Some("nested"));
        assert_eq!(group.path.as_deref(), Some("/repo/a/nested"));
        let group = session_group(SessionId(2), b"/repo/abc", &repositories);
        assert_eq!(group.repo_key, None);
        assert_eq!(group.worktree_key, hex(b"/repo/abc"));
        let outside = session_group(SessionId(3), b"/outside/sub", &repositories);
        assert_eq!(outside.repo_key, None);
        assert_eq!(outside.path.as_deref(), Some("/outside/sub"));
        let listed =
            super::super::workspaces::Catalog::load(None, &repositories).repositories_json();
        let empty = listed
            .iter()
            .find(|repo| repo["key"] == hex(b"/empty"))
            .unwrap();
        assert_eq!(empty["worktree_keys"], json!([]));
    }

    #[test]
    fn snapshot_group_keys_preserve_non_utf8_bytes_and_unknown_sessions() {
        let first = session_group(SessionId(1), b"/tmp/\xff", &[]);
        let second = session_group(SessionId(2), b"/tmp/\xfe", &[]);
        assert_eq!(first.path, second.path);
        assert_ne!(first.worktree_key, second.worktree_key);
        let first = session_group(SessionId(1), &[], &[]);
        let second = session_group(SessionId(2), &[], &[]);
        assert_ne!(first.worktree_key, second.worktree_key);
        assert!(first.repo_key.is_none());
        assert!(first.path.is_none());
        assert!(first.label.is_none());
    }

    #[test]
    fn snapshot_agents_follow_registry_profiles_in_standard_order() {
        let ids = |v: Value| -> Vec<String> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a["id"].as_str().map(str::to_owned))
                .collect()
        };
        let builtin = agents_json(None);
        assert_eq!(builtin[0], json!({"id":"claude","name":"Claude Code"}));
        assert_eq!(
            ids(builtin),
            ["claude", "codex", "agy", "cursor", "opencode"]
        );
        let dir = std::env::temp_dir().join(format!("mk-pwa-agents-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(krill_proto::agent::FILE_NAME),
            "[agent.my-bot]\nname = \"My Bot\"\nlaunch = [\"my-bot\"]\n",
        )
        .unwrap();
        let user = agents_json(Some(&dir));
        assert_eq!(
            user.as_array().unwrap().last(),
            Some(&json!({"id":"my-bot","name":"My Bot"}))
        );
        fs::write(dir.join(krill_proto::agent::FILE_NAME), "not toml [").unwrap();
        assert_eq!(agents_json(Some(&dir)), json!([]));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mobile_git_paths_untracked_and_unborn_head_have_real_patches() {
        let dir = std::env::temp_dir().join(format!("mk-mobile-git-audit-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(dir.join("sub")).unwrap();
        let root = fs::canonicalize(&dir).unwrap();
        let cwd = root.join("sub");
        assert_eq!(
            git_root(&cwd).unwrap_err().to_string(),
            "Not a Git repository"
        );
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(cwd.join("a.txt"), "one\ntwo").unwrap();
        assert_eq!(git_root(&cwd).unwrap(), root);
        let files = vec!["sub/a.txt".to_owned()];
        for raw in [
            "sub/a.txt".to_owned(),
            "a.txt".into(),
            "./a.txt".into(),
            cwd.join("a.txt").to_string_lossy().into_owned(),
            dir.join("sub/a.txt").to_string_lossy().into_owned(),
        ] {
            assert_eq!(
                git_relative_path(&root, &cwd, &raw, &files).as_deref(),
                Some("sub/a.txt"),
                "{raw}"
            );
        }
        assert!(git_relative_path(&root, &cwd, "../../escape", &files).is_none());
        // A truncated status list still permits a valid cwd-relative path.
        assert_eq!(
            git_relative_path(&root, &cwd, "./a.txt", &[]).as_deref(),
            Some("sub/a.txt")
        );
        let patch = read_patch(&root, "sub/a.txt").unwrap();
        assert!(
            patch.starts_with("--- /dev/null\n+++ b/sub/a.txt\n@@ -0,0 +1,2 @@\n+one\n+two\n"),
            "{patch}"
        );
        assert!(patch.contains("\\ No newline at end of file"));
        assert!(
            Command::new("git")
                .args(["add", "sub/a.txt"])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        let staged = read_patch(&root, "sub/a.txt").unwrap();
        assert!(staged.contains("+one"), "{staged}");
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.email=fixture@example.com",
                    "-c",
                    "user.name=Fixture",
                    "commit",
                    "-qm",
                    "fixture"
                ])
                .current_dir(&root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(cwd.join("a.txt"), "changed\n").unwrap();
        assert!(read_patch(&root, "sub/a.txt").unwrap().contains("+changed"));
        fs::remove_file(cwd.join("a.txt")).unwrap();
        assert_eq!(
            git_relative_path(&root, &cwd, "./a.txt", &files).as_deref(),
            Some("sub/a.txt")
        );
        assert!(
            read_patch(&root, "sub/a.txt")
                .unwrap()
                .contains("--- a/sub/a.txt")
        );
        fs::write(root.join("binary"), b"a\0b").unwrap();
        assert!(
            read_patch(&root, "binary")
                .unwrap()
                .contains("Binary files")
        );
        fs::write(root.join("large"), vec![b'x'; 128 * 1024 + 1]).unwrap();
        assert!(read_patch(&root, "large").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("binary"), root.join("link")).unwrap();
            assert_eq!(
                git_relative_path(&root, &cwd, root.join("link").to_str().unwrap(), &[]).as_deref(),
                Some("link")
            );
            assert!(read_patch(&root, "link").unwrap().is_empty());
            std::os::unix::fs::symlink(&root, dir.with_extension("alias")).unwrap();
            assert_eq!(
                git_relative_path(
                    &root,
                    &cwd,
                    dir.with_extension("alias")
                        .join("sub/a.txt")
                        .to_str()
                        .unwrap(),
                    &files
                )
                .as_deref(),
                Some("sub/a.txt")
            );
            fs::remove_file(dir.with_extension("alias")).unwrap();
        }
        fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn mobile_git_cache_stays_bounded_even_for_late_background_results() {
        let backend = test_backend();
        for i in 0..100 {
            cache_git_summary(
                &backend.git_cache,
                PathBuf::from(format!("/fixture/{i}")),
                json!(i),
            );
        }
        let cache = crate::session::lock(&backend.git_cache);
        assert_eq!(cache.len(), 64);
        assert!(cache.contains_key(Path::new("/fixture/99")));
        assert!(!cache.contains_key(Path::new("/fixture/0")));
    }
    fn test_backend() -> Backend {
        Backend::new(
            Weak::new(),
            None,
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        )
    }

    #[test]
    #[cfg(unix)] // git CLI
    fn card_git_skips_non_repositories_dedups_cwds_and_expires_cache() {
        let outside = session_group(SessionId(1), b"/tmp/not-a-repo", &[]);
        assert_eq!(git_cwd(b"/tmp/not-a-repo", &outside), None);
        assert_eq!(git_cwd(&[], &session_group(SessionId(2), &[], &[])), None);

        let dir = std::env::temp_dir().join(format!("mk-pwa-git-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        };
        git(&["init", "-q", "-b", "card"]);
        fs::write(dir.join("f"), "a\n").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-qm", "f"]);
        fs::write(dir.join("f"), "a\nb\n").unwrap();
        let repositories = vec![control::RepositoryTrees {
            repo: dir.as_os_str().as_bytes().to_vec(),
            worktrees: vec![dir.as_os_str().as_bytes().to_vec()],
        }];
        let inside = session_group(SessionId(3), dir.as_os_str().as_bytes(), &repositories);
        assert_eq!(
            git_cwd(dir.as_os_str().as_bytes(), &inside),
            Some(dir.clone())
        );

        let backend = test_backend();
        let expected = json!({"branch":"card","added":1,"deleted":0});
        let first = backend.git_summaries([dir.clone(), dir.clone()]);
        assert_eq!(first.len(), 1, "same cwd shares one query");
        assert_eq!(first[&dir], expected);
        fs::write(dir.join("f"), "a\nb\nc\n").unwrap();
        assert_eq!(
            backend.git_summaries([dir.clone()])[&dir],
            expected,
            "cached"
        );
        for (at, _) in crate::session::lock(&backend.git_cache).values_mut() {
            *at = Instant::now().checked_sub(GIT_TTL).unwrap();
        }
        assert_eq!(
            backend.git_summaries([dir.clone()])[&dir],
            json!({"branch":"card","added":2,"deleted":0}),
            "expired entry is queried again"
        );
        let missing = dir.join("missing");
        assert_eq!(
            backend.git_summaries([missing.clone()])[&missing],
            Value::Null
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(unix)] // git CLI shell alias gates the shared queue for this cwd.
    fn card_git_swr_returns_cold_and_expired_values_before_background_refresh() {
        let dir = std::env::temp_dir().join(format!("mk-pwa-git-swr-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let output = Command::new("git")
            .args(["init", "-q", "-b", "swr"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let backend = test_backend();
        let expected = json!({"branch":"swr","added":0,"deleted":0});
        let stale = json!({"branch":"old","added":7,"deleted":3});
        for generation in 1..=2 {
            if generation == 2 {
                crate::session::lock(&backend.git_cache).insert(
                    dir.clone(),
                    (Instant::now().checked_sub(GIT_TTL).unwrap(), stale.clone()),
                );
            }
            // Keep the real Git query queued until both immediate responses are checked.
            let gate = crate::git::queue().unwrap().submit(dir.clone(), [
                "-c", "alias.swr-gate=!touch .git/swr-started; while test ! -e .git/swr-release; do sleep 0.01; done",
                "swr-gate",
            ]);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !dir.join(".git/swr-started").exists() {
                assert!(Instant::now() < deadline, "queue gate did not start");
                thread::sleep(Duration::from_millis(5));
            }
            let started = Instant::now();
            let found = backend.git_summaries_swr([dir.clone(), dir.clone()]);
            assert!(started.elapsed() < Duration::from_millis(500));
            assert_eq!(found.len(), 1);
            let immediate = if generation == 1 {
                Value::Null
            } else {
                stale.clone()
            };
            assert_eq!(found[&dir], immediate);
            assert_eq!(backend.git_summaries_swr([dir.clone()])[&dir], immediate);
            assert_eq!(crate::session::lock(&backend.git_inflight).len(), 1);
            assert_eq!(backend.git_revision.load(Ordering::Relaxed), generation - 1);
            fs::remove_file(dir.join(".git/swr-started")).unwrap();
            fs::write(dir.join(".git/swr-release"), "").unwrap();
            assert!(
                gate.recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap()
                    .status
                    .success()
            );
            fs::remove_file(dir.join(".git/swr-release")).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while backend.git_revision.load(Ordering::Relaxed) < generation {
                assert!(
                    Instant::now() < deadline,
                    "background refresh did not complete"
                );
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(backend.git_revision.load(Ordering::Relaxed), generation);
            assert!(crate::session::lock(&backend.git_inflight).is_empty());
            assert_eq!(crate::session::lock(&backend.git_cache)[&dir].1, expected);
            assert_eq!(backend.git_summaries_swr([dir.clone()])[&dir], expected);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn host_metadata_reports_only_actual_hostname_and_platform() {
        let host = host_metadata();
        let expected = Command::new(if cfg!(windows) {
            "hostname.exe"
        } else {
            "/bin/hostname"
        })
        .output()
        .unwrap();
        assert_eq!(
            host["display_name"],
            String::from_utf8(expected.stdout).unwrap().trim()
        );
        assert_eq!(host["platform"], std::env::consts::OS);
    }

    #[test]
    fn terminal_actions_reject_phone_supplied_paths_and_programs() {
        for request in [
            r#"{"action":"create_terminal","cwd_session":1,"cwd":"/tmp"}"#,
            r#"{"action":"create_terminal","cwd_session":1,"program":"/bin/sh"}"#,
            r#"{"action":"close_terminal","session":1,"extra":true}"#,
        ] {
            assert!(
                serde_json::from_str::<Action>(request).is_err(),
                "{request}"
            );
        }
        // 생성 출처는 기존 세션 또는 등록 워크스페이스 중 하나다.
        for request in [
            r#"{"action":"create_terminal","cwd_session":1}"#,
            r#"{"action":"create_terminal","workspace":"registered-key"}"#,
        ] {
            assert!(serde_json::from_str::<Action>(request).is_ok(), "{request}");
        }
        for request in [
            r#"{"action":"create_terminal"}"#,
            r#"{"action":"create_terminal","cwd_session":1,"workspace":"key"}"#,
        ] {
            let reply: Value = serde_json::from_str(&test_backend().request(request)).unwrap();
            assert_eq!(reply["ok"], false, "{request}");
        }
    }

    #[test]
    fn terminal_creation_requires_one_registered_workspace_or_session() {
        let backend = test_backend();
        for request in [
            r#"{"action":"create_terminal"}"#,
            r#"{"action":"create_terminal","cwd_session":1,"workspace":"key"}"#,
            r#"{"action":"create_workspace","repository":"key","name":" "}"#,
        ] {
            let reply: Value = serde_json::from_str(&backend.request(request)).unwrap();
            assert_eq!(reply["ok"], false, "{request}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn create_workspace_on_non_git_folder_project_spawns_session_and_records_title() {
        use crate::holder::{Holder, link::Link};
        use crate::os::{AsFd as _, OwnedFd};
        use krill_proto::holder::{FromHolder, ToHolder, VERSION};

        let dir = std::env::temp_dir().join(format!("mk-pwa-folder-ws-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let state_dir = dir.join("state");
        let folder = dir.join("구글드라이브").join("내 드라이브");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&folder).unwrap();
        let canonical_folder = fs::canonicalize(&folder).unwrap();
        fs::write(
            state_dir.join("projects.toml"),
            format!(
                "[[projects]]\nid = \"gdrive\"\nname = \"구글드라이브\"\npath = {:?}\nkind = \"folder\"\n",
                canonical_folder.to_string_lossy()
            ),
        )
        .unwrap();

        let (daemon_end, holder_end) = UnixStream::pair().unwrap();
        let mut holder_link = Link::new(OwnedFd::from(holder_end));
        for message in [
            FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            },
            FromHolder::Ready,
        ] {
            holder_link.send(&message.encode().unwrap(), &[]).unwrap();
        }
        let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
        let registry = Arc::new(Registry::new(Some((holder, vec![])), Some(&state_dir)));
        let reader_reg = Arc::clone(&registry);
        let reader = std::thread::spawn(move || reader_reg.run_holder(link));
        let holder_worker = std::thread::spawn(move || {
            let packet = holder_link.recv().unwrap().unwrap();
            let ToHolder::Spawn { req, id, cwd, .. } = ToHolder::decode(packet.frame()).unwrap()
            else {
                panic!("expected Spawn");
            };
            let (master, _pty) = UnixStream::pair().unwrap();
            holder_link
                .send(
                    &FromHolder::Spawned {
                        req,
                        id,
                        pid: std::process::id(),
                    }
                    .encode()
                    .unwrap(),
                    &[master.as_fd()],
                )
                .unwrap();
            cwd
        });

        let backend = Backend::new(
            Arc::downgrade(&registry),
            Some(&state_dir),
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let repo_key = hex(canonical_folder.as_os_str().as_bytes());
        let request = json!({
            "action": "create_workspace",
            "repository": repo_key,
            "name": "보험",
        })
        .to_string();
        let reply: Value = serde_json::from_str(&backend.request(&request)).unwrap();
        let spawned_cwd = holder_worker.join().unwrap();
        reader.join().unwrap();

        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(reply["data"]["workspace"], repo_key);
        assert_eq!(
            reply["data"]["path"],
            canonical_folder.to_string_lossy().as_ref()
        );
        assert_eq!(reply["data"]["branch"], "보험");
        assert_eq!(spawned_cwd, canonical_folder.as_os_str().as_bytes());
        let session_id = reply["data"]["session"].as_u64().expect("session spawned");
        let titles = krill_proto::git::load_session_auto_titles(&state_dir);
        assert_eq!(
            titles.get(&SessionId(session_id)).map(String::as_str),
            Some("보험")
        );

        drop(backend);
        drop(registry);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn photo_decoder_bounds_and_malformed_data() {
        for (encoded, plain) in [
            ("", ""),
            ("Zg==", "f"),
            ("Zm8=", "fo"),
            ("Zm9v", "foo"),
            ("8J+MjQ==", "🌍"),
        ] {
            assert_eq!(decode_base64(encoded).unwrap(), plain.as_bytes());
        }
        for encoded in ["Z", "Zh==", "Zm9=", "Z g==", "Zg==a", "☀"] {
            assert!(decode_base64(encoded).is_err(), "{encoded}");
        }
        assert!(decode_base64(&"A".repeat(700_001)).is_err());
        assert!(save_photo("text/html", "Zg==").is_err());
        assert!(save_photo("image/png", "").is_err());
        assert!(save_photo("image/png", &"A".repeat(699_996)).is_err());
    }

    #[test]
    fn workflow_errors_are_bounded_and_json() {
        let backend = test_backend();
        for request in [
            "",
            "{}",
            "{\"action\":\"unknown\"}",
            "{\"action\":\"snapshot\",\"extra\":true}",
        ] {
            let result: Value = serde_json::from_str(&backend.request(request)).unwrap();
            assert_eq!(result["ok"], false);
        }
        assert!(backend.request(&" ".repeat(750_001)).len() < 200);
    }

    #[test]
    #[cfg(unix)] // /bin/sh
    fn subprocess_output_and_time_are_bounded() {
        let mut good = Command::new("/bin/sh");
        good.args(["-c", "printf hello"]);
        assert_eq!(
            bounded_command(&mut good, Duration::from_secs(1), 5).unwrap(),
            b"hello"
        );
        let mut large = Command::new("/bin/sh");
        large.args(["-c", "printf hello"]);
        assert!(bounded_command(&mut large, Duration::from_secs(1), 4).is_err());
        let mut slow = Command::new("/bin/sh");
        slow.args(["-c", "exec sleep 1"]);
        assert_eq!(
            bounded_command(&mut slow, Duration::from_millis(10), 10)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    proptest::proptest! {
        #[test]
        fn photo_decoder_never_panics(text in ".{0,2048}") {
            decode_base64(&text).ok();
        }
    }
}

#[cfg(test)]
mod revision_tests {
    use super::*;
    #[test]
    fn git_refresh_changes_the_effective_dashboard_revision() {
        let registry = Registry::new(None, None);
        let backend = Backend::new(
            Weak::new(),
            None,
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let before = backend.effective_revision(&registry);
        assert_eq!(before, registry.dashboard_revision());
        backend.git_revision.fetch_add(1, Ordering::Relaxed);
        assert_ne!(backend.effective_revision(&registry), before);
        assert_eq!(
            backend.effective_revision(&registry),
            registry.dashboard_revision() ^ 0x9E37_79B9_7F4A_7C15
        );
    }

    #[test]
    fn near_limit_workflow_snapshot_roundtrips_on_rpc_stack() {
        let worker = std::thread::Builder::new().stack_size(super::super::gateway::RPC_STACK).spawn(|| {
            let registry = Arc::new(Registry::new(None, None));
            let backend = Backend::new(Arc::downgrade(&registry), None, ServerHello {
                proto_min: 1, proto_max: 1, features: ipc::FEATURES, build: "stack-fixture".into(), instance_id: 1,
            });
            let sessions: Vec<_> = (1..=64).map(|id| json!({"id":id,"title":"fixture","state":"Working","lines":["one","two","three"]})).collect();
            let data = json!({"sessions":sessions,"padding":"x".repeat(ipc::MAX_FRAME - 20_000),"etag":"stack"});
            *crate::session::lock(&backend.snapshot_cache) = Some(DashboardSnapshot {
                revision: backend.effective_revision(&registry), paths: Vec::new(), stamps: Vec::new(), at: Instant::now(), data, etag: "stack".into(),
            });
            let reply = backend.request("{\"action\":\"snapshot\"}");
            assert!(reply.len() > ipc::MAX_FRAME - 20_000);
            assert!(reply.len() < ipc::MAX_FRAME - 32);
            let value: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(value["ok"], true);
            assert_eq!(value["data"]["sessions"].as_array().unwrap().len(), 64);
            let encoded = krill_proto::mobile::MobileReply::Workflow(reply.clone()).encode().unwrap();
            let (kind, body) = ipc::read_frame(&mut encoded.as_slice()).unwrap().unwrap();
            assert_eq!(krill_proto::mobile::MobileReply::decode(kind, &body).unwrap(), krill_proto::mobile::MobileReply::Workflow(reply));
            // A large request is rejected against this isolated empty registry;
            // it never reaches a PTY or an external service.
            let request = json!({"action":"prompt","session":1,"text":"x".repeat(749_000)}).to_string();
            assert!(request.len() < 750_000);
            let failure: Value = serde_json::from_str(&backend.request(&request)).unwrap();
            assert_eq!(failure["ok"], false);
        }).unwrap();
        worker.join().unwrap();
    }
    #[test]
    fn unchanged_snapshot_returns_before_opening_any_control_socket() {
        let registry = Arc::new(Registry::new(None, None));
        let backend = Backend::new(
            Arc::downgrade(&registry),
            None,
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        backend.git_revision.store(7, Ordering::Relaxed);
        *crate::session::lock(&backend.snapshot_cache) = Some(DashboardSnapshot {
            revision: backend.effective_revision(&registry),
            paths: Vec::new(),
            stamps: Vec::new(),
            at: Instant::now(),
            data: json!({"sessions":[],"etag":"same"}),
            etag: "same".into(),
        });
        assert_eq!(
            backend.snapshot_since(Some("same")).unwrap(),
            json!({"unchanged":true,"etag":"same"})
        );
    }

    #[test]
    fn catalog_stamp_reuses_unchanged_source_and_observes_atomic_preferences_edit() {
        let dir = std::env::temp_dir().join(format!("catalog-stamp-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("projects.toml"), "").unwrap();
        let backend = Backend::new(
            Weak::new(),
            Some(&dir),
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        let first = backend.catalog(&[]);
        let same = backend.catalog(&[]);
        assert!(Arc::ptr_eq(&first, &same));
        assert!(
            backend
                .snapshot_dependencies(&Value::Null, Some(&first))
                .is_some()
        );
        let replacement = dir.join("projects.new");
        fs::write(&replacement, "\n").unwrap();
        fs::rename(replacement, dir.join("projects.toml")).unwrap();
        // A body built with the old catalog must not adopt the new file stamps.
        assert!(
            backend
                .snapshot_dependencies(&Value::Null, Some(&first))
                .is_none()
        );
        assert!(!Arc::ptr_eq(&first, &backend.catalog(&[])));
        assert!(
            backend
                .snapshot_dependencies(&Value::Null, Some(&first))
                .is_none()
        );
        *crate::session::lock(&backend.catalog_cache) = None;
        assert!(
            backend
                .snapshot_dependencies(&Value::Null, Some(&same))
                .is_none()
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
