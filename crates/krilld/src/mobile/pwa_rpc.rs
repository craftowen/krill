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
    control::{self, CaptureFormat, Create, Program, Reply, Request},
    input::{Input, InputAck, InputId, InputResult},
    ipc::{self, ClientRole, ServerHello},
    orchestration::{GateStatus, MessageType, OrchestrationStore},
    screen::{Attach, Msg, Seq, Stream},
    usage::{ProviderKind, UsageReply, UsageRequest},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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
    git_cache: Mutex<HashMap<PathBuf, (Instant, Value)>>,
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
    CreateTerminal {
        cwd_session: u64,
        #[serde(default)]
        agent: Option<String>,
    },
    CloseTerminal {
        session: u64,
    },
    Prompt {
        session: u64,
        text: String,
    },
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
    },
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
impl Backend {
    pub(super) fn new(registry: Weak<Registry>, dir: Option<&Path>, hello: ServerHello) -> Self {
        Self {
            registry,
            dir: dir.map(Path::to_path_buf),
            hello,
            host: host_metadata(),
            git_cache: Mutex::default(),
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
            Action::Snapshot { since } => {
                let mut data = self.snapshot()?;
                let bytes = serde_json::to_vec(&data).map_err(io::Error::other)?;
                let etag: String =
                    hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref())
                        .chars()
                        .take(16)
                        .collect();
                if since.as_deref() == Some(etag.as_str()) {
                    return Ok(json!({"unchanged":true,"etag":etag}));
                }
                if let Some(object) = data.as_object_mut() {
                    object.insert("etag".into(), etag.into());
                }
                Ok(data)
            }
            Action::CreateTerminal { cwd_session, agent } => {
                let program = match agent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    None => Program::Login,
                    Some(id) if crate::agent::control::valid_session_id(id) => {
                        Program::Agent { id: id.to_owned() }
                    }
                    Some(_) => return Err(invalid()),
                };
                let cwd = self.cwd(cwd_session)?;
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
            Action::CloseTerminal { session } => {
                match self.control(&Request::Close {
                    session: SessionId(session),
                })? {
                    Reply::Closed => Ok(json!({"closed":true})),
                    Reply::Failed(error) => Err(io::Error::other(error.to_string())),
                    _ => Err(invalid()),
                }
            }
            Action::Prompt { session, text } => {
                if text.len() >= krill_proto::input::MAX_INPUT {
                    return Err(invalid());
                }
                if text.is_empty() {
                    self.input(session, b"\r".to_vec())?;
                } else {
                    let bracketed =
                        self.session_bracketed_paste(session) || text.contains(['\n', '\r']);
                    let payload = if bracketed {
                        format_bracketed_paste(&text)
                    } else {
                        sanitize_plain_input(&text)
                    };
                    if payload.len() > krill_proto::input::MAX_INPUT {
                        return Err(invalid());
                    }
                    self.input_paced(
                        session,
                        &[payload, b"\r".to_vec()],
                        Duration::from_millis(60),
                    )?;
                }
                Ok(json!({"sent":true}))
            }
            Action::Approve { session, value } => {
                if value.is_empty() || value.len() > 128 || value.contains(['\r', '\n']) {
                    return Err(invalid());
                }
                self.input(session, format!("{value}\r").into_bytes())?;
                Ok(json!({"sent":true}))
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
            Action::ChatPoll { session, offset } => {
                let registry = self.registry.upgrade().ok_or_else(invalid)?;
                to_value(registry.chat_poll(SessionId(session), offset))
            }
            Action::AnswerQuestion {
                session,
                selected,
                option_count,
                multi_select,
                custom_text,
            } => {
                let (screen_text, bracketed) = self.session_screen_and_bracketed_paste(session);
                let initial_cursor = detect_question_cursor_index(&screen_text, option_count);
                let steps = plan_question_keystrokes_from(
                    &selected,
                    option_count,
                    multi_select,
                    custom_text.as_deref(),
                    bracketed,
                    initial_cursor,
                )?;
                self.input_paced(session, &steps, Duration::from_millis(60))?;
                Ok(json!({"sent":true,"steps":steps.len()}))
            }
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
                    store.resolve_gate(&id, &decision, None, "mobile")
                })?)
            }
            Action::QuestionReply { id, body } => {
                if body.len() > 16 * 1024 {
                    return Err(invalid());
                }
                let dir = self.dir.as_deref().ok_or_else(invalid)?;
                to_value(OrchestrationStore::mutate(dir, |store| {
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
    fn session_screen_and_bracketed_paste(&self, session: u64) -> (String, bool) {
        self.registry
            .upgrade()
            .map_or((String::new(), false), |reg| {
                reg.session_screen_and_bracketed_paste(SessionId(session))
            })
    }
    fn wire(&self) -> io::Result<Wire> {
        let registry = self.registry.upgrade().ok_or_else(invalid)?;
        let hello = self.hello.clone();
        let (mut socket, server) = UnixStream::pair()?;
        socket.set_read_timeout(Some(Duration::from_secs(20)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        let worker = thread::Builder::new()
            .name("mobile-rpc".into())
            .stack_size(128 * 1024)
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
            .ok_or_else(invalid)?;
        if directory.cwd.is_empty() {
            return Err(io::Error::other("session working directory unavailable"));
        }
        Ok(PathBuf::from(std::ffi::OsString::from_vec(directory.cwd)))
    }
    fn snapshot(&self) -> io::Result<Value> {
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
            },
        };
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
                let group = session_group(info.session, cwd, &sidebar.repositories);
                (info, cwd, group)
            })
            .collect();
        let git = self.git_summaries(
            rows.iter()
                .filter_map(|(_, cwd, group)| git_cwd(cwd, group)),
        );
        let mut cards = Vec::new();
        for (info, cwd_bytes, group) in rows {
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
            let cwd = String::from_utf8_lossy(cwd_bytes);
            let text = match control(&Request::Capture {
                session: info.session,
                format: CaptureFormat::Plain,
            })? {
                Reply::Captured { bytes, .. } => String::from_utf8_lossy(&bytes).into_owned(),
                _ => String::new(),
            };
            let mut lines: Vec<_> = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .rev()
                .take(3)
                .map(|l| l.chars().take(500).collect::<String>())
                .collect();
            lines.reverse();
            let git = git_cwd(cwd_bytes, &group)
                .and_then(|path| git.get(&path))
                .unwrap_or(&Value::Null);
            cards.push(json!({"id":info.session.0,"state":state.state,"agent":agent,"cwd":cwd,"lines":lines,"exited":info.status.is_some(),"branch":git.get("branch"),"added":git.get("added"),"deleted":git.get("deleted"),
                "repo_key":group.repo_key,"worktree_key":group.worktree_key,
                "group_label":group.label,"group_path":group.path,
                "state_source":state.source,"state_confidence":state.confidence,
                "state_since_unix_ns":state.since_unix_ns.to_string()}));
        }
        Ok(
            json!({"sessions":cards,"orchestration":self.orchestration()?,
            "repositories":repositories_json(&sidebar.repositories),"truncated":truncated,
            "capabilities":{"snapshot_groups":true,"session_create":true,"session_close":true,"hybrid_chat":true},
            "host":self.host,"agents":agents_json(self.dir.as_deref())}),
        )
    }
    fn orchestration(&self) -> io::Result<Value> {
        let store = self
            .dir
            .as_deref()
            .map(OrchestrationStore::load)
            .transpose()?
            .unwrap_or_default();
        let questions: Vec<_> = store
            .messages
            .iter()
            .filter(|m| m.msg_type == MessageType::Question && !m.acked)
            .collect();
        let gates: Vec<_> = store
            .gates
            .iter()
            .filter(|g| g.status == GateStatus::Pending)
            .collect();
        Ok(json!({"runs":store.runs,"tasks":store.tasks,"gates":gates,"questions":questions}))
    }
    /// 카드용 git 요약을 cwd별로 모은다. 캐시에 없는 cwd는 큐에 전부 먼저 넣고 나서 받는다
    /// (큐 워커 [`crate::git::MAX_CONCURRENT`]개가 서로 다른 cwd를 동시에 돈다. 같은 키만 직렬).
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
            crate::session::lock(&self.git_cache)
                .insert(cwd.clone(), (Instant::now(), value.clone()));
            found.insert(cwd, value);
        }
        found
    }
    fn git(&self, session: u64, path: Option<&str>) -> io::Result<Value> {
        let cwd = self.cwd(session)?;
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
        let patch = if let Some(path) = path {
            if !files.iter().any(|p| p == path)
                || Path::new(path)
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(invalid());
            }
            read_patch(&cwd, path)?
        } else {
            String::new()
        };
        Ok(json!({"branch":branch,"added":added,"deleted":deleted,"files":files,"patch":patch}))
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
fn repositories_json(repositories: &[control::RepositoryTrees]) -> Vec<Value> {
    repositories.iter().map(|repo| json!({
        "key":hex(&repo.repo),"label":path_label(&repo.repo),"path":String::from_utf8_lossy(&repo.repo),
        "worktree_keys":repo.worktrees.iter().map(|tree| hex(tree)).collect::<Vec<_>>()
    })).collect()
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
    let dir = std::env::temp_dir().join("minikrill-paste");
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
fn read_patch(cwd: &Path, path: &str) -> io::Result<String> {
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
    command.args([
        "--literal-pathspecs",
        "-c",
        "diff.maxFiles=1",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "HEAD",
        "--",
        path,
    ]);
    Ok(String::from_utf8_lossy(&bounded_command(
        &mut command,
        Duration::from_secs(5),
        128 * 1024,
    )?)
    .into_owned())
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
        assert_eq!(
            repositories_json(&repositories)[1]["worktree_keys"],
            json!([])
        );
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
            r#"{"action":"create_terminal"}"#,
            r#"{"action":"close_terminal","session":1,"extra":true}"#,
        ] {
            assert!(
                serde_json::from_str::<Action>(request).is_err(),
                "{request}"
            );
        }
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
