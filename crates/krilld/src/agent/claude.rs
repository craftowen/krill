//! 세션별 Claude `--settings`와 `UUIDv4`. 사용자 설정은 열지 않는다.
//! 파일은 launch 뒤에도 보존한다(holder와 Claude는 krilld보다 오래 산다).
//! 파일 이름은 `<세션 id>-<uuid>.json`이라 세션이 끝나면(Exited·생성 실패·닫기)
//! 그 세션의 파일만 지우고, krilld 재기동 때는 살아남은 세션의 파일만 남긴다.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use krill_proto::SessionId;
use krill_proto::agent::{Permission, Profile, claude};

use super::wrap;
use crate::shell::Launch;

/// 주입 settings 디렉터리(상태 디렉터리 아래). 0700, 파일은 0600.
const DIR: &str = "claude-settings";

#[derive(Debug)]
pub(crate) struct Settings {
    pub id: String,
    pub path: PathBuf,
}

impl Settings {
    pub(crate) fn create(
        state: &Path,
        krill: &Path,
        session: SessionId,
        version: Option<&str>,
    ) -> io::Result<Self> {
        Self::create_named(state, krill, session, version, false)
    }

    /// `--session-id`를 주입하지 않는 실행(`--fork-session`·ID 없는 `--resume`)용 설정 파일.
    /// 파일 이름을 `<세션 id>-fork-<uuid>.json`으로 두어 [`discard`]·[`sweep`]은 세션 소유로 정리하되
    /// [`session_id`] 폴백은 미주입 UUID를 반환하지 않는다(OWE-752).
    pub(crate) fn create_for_fork(
        state: &Path,
        krill: &Path,
        session: SessionId,
        version: Option<&str>,
    ) -> io::Result<Self> {
        Self::create_named(state, krill, session, version, true)
    }

    fn create_named(
        state: &Path,
        krill: &Path,
        session: SessionId,
        version: Option<&str>,
        fork_only: bool,
    ) -> io::Result<Self> {
        let dir = state.join(DIR);
        DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let state = state.canonicalize()?;
        let krill = krill.canonicalize()?;
        let command = format!(
            "env KRILL_STATE_DIR={} {} hook claude",
            quote(&state)?,
            quote(&krill)?
        );
        let hooks: serde_json::Map<String, serde_json::Value> = claude::events(version)
            .into_iter()
            .map(|event| {
                (
                    event.to_owned(),
                    serde_json::json!([{
                        "matcher": "*", "hooks": [{"type": "command", "command": command}]
                    }]),
                )
            })
            .collect();
        let mut root = serde_json::Map::new();
        root.insert("hooks".to_owned(), serde_json::Value::Object(hooks));
        // OWE-701: 프로필별 사용량 기록. 사용자 statusLine은 건드리지 않고(사용자 설정 무변경)
        // 세션별 `--settings`에만 둔다. 실행된 `krill statusline`이 `settings.json`의 사용자
        // 명령을 체인으로 보존한다. `statusLine` 키를 모르는 구버전에는 넣지 않는다.
        if krill_proto::statusline::supports_status_line(version) {
            root.insert(
                "statusLine".to_owned(),
                serde_json::json!({
                    "type": "command",
                    "command": statusline_command(&krill)?,
                }),
            );
        }
        let bytes = serde_json::to_vec(&serde_json::Value::Object(root))?;
        let id = uuid()?;
        let filename = if fork_only {
            format!("{}-fork-{id}.json", session.0)
        } else {
            format!("{}-{id}.json", session.0)
        };
        let path = state.join(DIR).join(filename);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        if let Err(e) = file.write_all(&bytes) {
            std::fs::remove_file(&path).ok();
            return Err(e);
        }
        Ok(Self { id, path })
    }

    pub(crate) fn launch(
        self,
        profile: &Profile,
        permission: Permission,
        login: Launch,
    ) -> io::Result<Launch> {
        self.check_profile(profile, permission)?;
        let mut launch = wrap(profile, permission, login);
        launch.argv.extend([
            b"--settings".to_vec(),
            self.path.as_os_str().as_bytes().to_vec(),
            b"--session-id".to_vec(),
            self.id.into_bytes(),
        ]);
        Ok(launch)
    }

    pub(crate) fn resume(
        self,
        profile: &Profile,
        permission: Permission,
        login: Launch,
        session_id: Option<&str>,
    ) -> io::Result<Launch> {
        self.check_profile(profile, permission)?;
        let mut launch = wrap(profile, permission, login);
        launch.argv.extend([
            b"--settings".to_vec(),
            self.path.as_os_str().as_bytes().to_vec(),
        ]);
        if let Some(id) = session_id.filter(|s| !s.is_empty()) {
            launch.argv.push(format!("--resume={id}").into_bytes());
        } else {
            launch.argv.push(b"--resume".to_vec());
        }
        Ok(launch)
    }

    pub(crate) fn fork(
        mut self,
        profile: &Profile,
        permission: Permission,
        login: Launch,
        session_id: Option<&str>,
    ) -> io::Result<Launch> {
        self.check_profile(profile, permission)?;
        if let Some(name) = self.path.file_name().and_then(OsStr::to_str)
            && let Some((sid, rest)) = name.strip_suffix(".json").and_then(|s| s.split_once('-'))
            && is_uuid(rest)
            && let Some(parent) = self.path.parent()
        {
            let fork_path = parent.join(format!("{sid}-fork-{rest}.json"));
            if std::fs::rename(&self.path, &fork_path).is_ok() {
                self.path = fork_path;
            }
        }
        let mut launch = wrap(profile, permission, login);
        launch.argv.extend([
            b"--settings".to_vec(),
            self.path.as_os_str().as_bytes().to_vec(),
        ]);
        if let Some(id) = session_id.filter(|s| !s.is_empty()) {
            launch.argv.push(format!("--resume={id}").into_bytes());
        } else {
            launch.argv.push(b"--continue".to_vec());
        }
        launch.argv.push(b"--fork-session".to_vec());
        Ok(launch)
    }

    fn check_profile(&self, profile: &Profile, permission: Permission) -> io::Result<()> {
        // 중복 ID·resume은 세션 계약과 충돌한다. 기존 --settings도 조용히 덮어쓰지 않는다.
        if profile.argv(permission).iter().any(|arg| {
            matches!(
                arg.split('=').next(),
                Some(
                    "--settings"
                        | "--session-id"
                        | "--resume"
                        | "--continue"
                        | "--fork-session"
                        | "-r"
                        | "-c"
                )
            )
        }) {
            std::fs::remove_file(&self.path).ok();
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "claude profile conflicts with session settings",
            ));
        }
        Ok(())
    }
}

/// 세션 `id`의 settings 파일 이름에서 사전 발급한 `<uuid>`를 찾는다.
/// `--session-id`를 주입하지 않은 `<id>-fork-<uuid>.json`은 제외한다(OWE-752).
pub(crate) fn session_id(state: &Path, id: SessionId) -> Option<String> {
    let dir = std::fs::read_dir(state.join(DIR)).ok()?;
    for entry in dir.flatten() {
        let name = entry.file_name();
        if owner(&name) == Some(id)
            && let Some((_, uuid)) = name.to_str()?.strip_suffix(".json")?.split_once('-')
            && is_uuid(uuid)
        {
            return Some(uuid.to_owned());
        }
    }
    None
}

/// 세션 `id`의 settings 파일을 지운다. 살아 있는 세션의 파일은 호출자가 지우지 않는다.
/// 파일이 없어도 성공이다. 디렉터리가 없거나 못 읽어도 성공이다(정리 실패가 세션 닫기를 막지 않는다).
pub(crate) fn discard(state: &Path, id: SessionId) {
    let Ok(dir) = std::fs::read_dir(state.join(DIR)) else {
        return;
    };
    for entry in dir.flatten() {
        if owner(&entry.file_name()) == Some(id) {
            std::fs::remove_file(entry.path()).ok();
        }
    }
}

/// 살아 있는 세션(`live`)에 속하지 않은 `<id>-<uuid>.json`을 지운다.
/// 이름이 형식에 맞지 않으면 남긴다(다른 파일·구버전 `<uuid>.json`은 건드리지 않는다).
/// 디렉터리가 없거나 못 읽으면 0을 돌려준다.
pub(crate) fn sweep(state: &Path, live: &[SessionId]) -> usize {
    let Ok(dir) = std::fs::read_dir(state.join(DIR)) else {
        return 0;
    };
    let mut removed = 0;
    for entry in dir.flatten() {
        let Some(sid) = owner(&entry.file_name()) else {
            continue;
        };
        if live.contains(&sid) {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn is_uuid(uuid: &str) -> bool {
    uuid.len() == 36
        && uuid.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

/// `<세션 id>-<uuid>.json` 또는 `<세션 id>-fork-<uuid>.json`의 세션 id.
/// uuid 자리가 36자 `8-4-4-4-12` 16진이 아니면 `None`이라
/// 첫 덩어리가 전부 숫자인 구버전 `<uuid>.json`을 세션 파일로 오인하지 않는다.
fn owner(name: &OsStr) -> Option<SessionId> {
    let (sid, rest) = name.to_str()?.strip_suffix(".json")?.split_once('-')?;
    let uuid = rest.strip_prefix("fork-").unwrap_or(rest);
    if !is_uuid(uuid) || !sid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    sid.parse().ok().map(SessionId)
}

fn quote(path: &Path) -> io::Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hook path is not utf-8"))?;
    Ok(format!("'{}'", text.replace('\'', "'\\''")))
}

/// 데몬 세션용 statusline 명령. 체인 없음(프로필 폴더에 사용자 설정이 없다).
/// 실행된 `krill statusline`이 사용자 슬롯을 보면 체인으로 보존한다.
fn statusline_command(krill: &Path) -> io::Result<String> {
    Ok(krill_proto::statusline::install_command(
        krill
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hook path is not utf-8"))?,
    ))
}

fn uuid() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let version = bytes.get_mut(6).ok_or(io::ErrorKind::InvalidData)?;
    *version = (*version & 0x0f) | 0x40;
    let variant = bytes.get_mut(8).ok_or(io::ErrorKind::InvalidData)?;
    *variant = (*variant & 0x3f) | 0x80;
    let mut id = String::new();
    for (i, b) in bytes.into_iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        write!(id, "{b:02x}").map_err(io::Error::other)?;
    }
    Ok(id)
}

/// 실제와 같은 login/interactive 셸의 PATH·환경에서 버전을 읽는다. 5초 안에 못 읽으면 보수적 집합.
pub(crate) fn probe(profile: &Profile, login: &Launch) -> Option<String> {
    let mut profile = profile.clone();
    profile.launch = vec![profile.program()?.to_owned(), "--version".into()];
    let launch = wrap(&profile, Permission::Manual, login.clone());
    let (program, args) = launch.argv.split_first()?;
    let mut child = Command::new(OsStr::from_bytes(program))
        .args(args.iter().map(|a| OsStr::from_bytes(a)))
        .env_clear()
        .envs(
            launch
                .env
                .iter()
                .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        tx.send(stdout.take(4096).read_to_string(&mut text).map(|_| text))
            .ok();
    });
    let out = rx.recv_timeout(Duration::from_secs(5));
    let status = child.try_wait().ok().flatten();
    child.kill().ok();
    child.wait().ok();
    if status.is_some_and(|s| !s.success()) {
        return None;
    }
    out.ok()?
        .ok()?
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests;
