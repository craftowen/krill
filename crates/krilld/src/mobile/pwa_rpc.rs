//! 모바일 JSON 요청을 기존 IPC·M8 원자 저장소로 연결한다. 인증 전에는 호출하지 않는다.
use super::devices::hex;
use super::e2ee::{invalid, random};
use crate::registry::Registry;
use krill_proto::{
    SessionId,
    control::{self, CaptureFormat, Reply, Request},
    input::{Input, InputAck, InputId, InputResult},
    ipc::{self, ClientRole, ServerHello},
    orchestration::{GateStatus, MessageType, OrchestrationStore},
    screen::{Attach, Msg, Seq, Stream},
    usage::{ProviderKind, UsageReply, UsageRequest},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::{
    ffi::OsStringExt,
    fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    net::UnixStream,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Weak;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub(super) struct Backend {
    registry: Weak<Registry>,
    dir: Option<PathBuf>,
    hello: ServerHello,
}
impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish_non_exhaustive()
    }
}
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Snapshot,
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
            Action::Snapshot => self.snapshot(),
            Action::Prompt { session, text } => {
                if text.len() >= krill_proto::input::MAX_INPUT {
                    return Err(invalid());
                }
                self.input(session, format!("{text}\r").into_bytes())?;
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
            } => {
                let path = save_photo(&mime, &data)?;
                let quoted = krill_proto::agent::hooks::shell_quote(&path.to_string_lossy());
                if let Err(error) = self.input(session, format!("{quoted} ").into_bytes()) {
                    fs::remove_file(&path).ok();
                    return Err(error);
                }
                Ok(json!({"attached":true,"path":path.to_string_lossy()}))
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
        let mut wire = self.wire()?;
        wire.socket.write_all(&request.encode()?)?;
        let (kind, body) = ipc::read_frame(&mut wire.socket)?.ok_or_else(invalid)?;
        Reply::decode(kind, &body).map_err(Into::into)
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
        wire.socket.write_all(
            &Input {
                id: InputId(1),
                bytes,
            }
            .encode()?,
        )?;
        loop {
            let (kind, body) = ipc::read_frame(&mut wire.socket)?.ok_or_else(invalid)?;
            if kind == krill_proto::input::kind::INPUT_ACK {
                return if InputAck::decode(kind, &body)?.result == InputResult::Applied {
                    Ok(())
                } else {
                    Err(io::Error::other("input rejected"))
                };
            }
        }
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
        let Reply::Sessions(sessions) = self.control(&Request::List)? else {
            return Err(invalid());
        };
        let states = match self.control(&Request::States)? {
            Reply::States(s) => s,
            _ => Vec::new(),
        };
        let sidebar = match self.control(&Request::Sidebar)? {
            Reply::Sidebar(s) => s.sessions,
            _ => Vec::new(),
        };
        let mut cards = Vec::new();
        for info in sessions.into_iter().take(64) {
            let state = states
                .iter()
                .find(|s| s.session == info.session)
                .map_or(control::AgentStatus::UNKNOWN, |s| s.status.clone());
            let cwd = sidebar
                .iter()
                .find(|s| s.session == info.session)
                .map_or_else(String::new, |s| {
                    String::from_utf8_lossy(&s.cwd).into_owned()
                });
            let text = match self.control(&Request::Capture {
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
            let git = self.git(info.session.0, None).unwrap_or(Value::Null);
            cards.push(json!({"id":info.session.0,"state":state.state,"agent":state.agent,"cwd":cwd,"lines":lines,"exited":info.status.is_some(),"branch":git.get("branch"),"added":git.get("added"),"deleted":git.get("deleted")}));
        }
        Ok(json!({"sessions":cards,"orchestration":self.orchestration()?}))
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
    fn git(&self, session: u64, path: Option<&str>) -> io::Result<Value> {
        let cwd = self.cwd(session)?;
        let changes = match self.control(&Request::GitChanges {
            repo: cwd.as_os_str().as_encoded_bytes().to_vec(),
        })? {
            Reply::GitChanges(c) => c,
            Reply::GitChangesFailed(e) => return Err(io::Error::other(e)),
            _ => return Err(invalid()),
        };
        let branch = crate::git::queue()
            .map_err(io::Error::other)?
            .submit(&cwd, ["branch", "--show-current"])
            .recv()
            .map_err(io::Error::other)??;
        let mut files = Vec::new();
        let mut added = 0_u64;
        let mut deleted = 0_u64;
        for file in changes.files {
            for stat in [file.staged, file.unstaged].into_iter().flatten() {
                added = added.saturating_add(stat.added.unwrap_or(0));
                deleted = deleted.saturating_add(stat.deleted.unwrap_or(0));
            }
            files.push(String::from_utf8_lossy(&file.path).into_owned());
        }
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
        Ok(
            json!({"branch":String::from_utf8_lossy(&branch.stdout).trim(),"added":added,"deleted":deleted,"files":files,"patch":patch}),
        )
    }
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
    if !meta.is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let path = dir.join(format!("mobile-{}.{extension}", hex(&random()?)));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
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
    use std::os::fd::AsFd;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| {
        let mut pipe = child.stdout.take().ok_or_else(invalid)?;
        let flags = rustix::fs::fcntl_getfl(pipe.as_fd())?;
        rustix::fs::fcntl_setfl(pipe.as_fd(), flags | rustix::fs::OFlags::NONBLOCK)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
