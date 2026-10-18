//! 세션 범위 권한(MK-036, T2-D11). 연결한 프로세스가 어느 세션의 후손인지 `LOCAL_PEERPID` + ppid 체인으로 판정해,
//! 세션 안에서 온 요청(에이전트·hook·에이전트가 실행한 `krill`)은 그 세션만 다루게 한다. 거부는 감사 로그에 남긴다.
//!
//! **보안 경계가 아니라 실수 방지용 범위 제한이다**(ARCHITECTURE §3). 같은 UID 프로세스는 setsid·이중 fork로
//! 부모를 launchd로 바꿔 외부 셸처럼 보일 수 있다. 계보를 끝까지 확인하지 못하면 [`Scope::Restricted`]다.
//!
//! | 피어 | 범위 |
//! |---|---|
//! | 체인이 세션 루트에 닿지 않고 launchd(PID 1)까지 감(외부 셸·앱) | [`Scope::Full`] |
//! | 세션 X 루트 프로세스 또는 그 후손 | [`Scope::Session`]`(X)` — X와 X가 만든 하위 세션만 |
//! | 피어 PID·부모 조회 실패, 체인이 [`MAX_DEPTH`]를 넘음 | [`Scope::Restricted`] — 어떤 세션도 못 다룸 |

use crate::os::AsFd;
use krill_proto::os::fs::OpenOptionsExt;
use krill_proto::os::net::UnixStream;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use krill_proto::SessionId;

use crate::sys::{self, CodeSign};

/// ppid 체인을 따라 올라가는 최대 단계. 넘으면 조회 실패로 본다.
pub const MAX_DEPTH: usize = 128;
/// 감사 로그가 이 크기를 넘으면 `<path>.1`로 밀어내고 새로 쓴다(직전 한 벌만 보관).
const AUDIT_MAX: u64 = 1 << 20;

/// 피어가 다룰 수 있는 세션 범위.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 모든 세션(외부 셸·앱).
    Full,
    /// 이 세션과 이 세션이 만든 하위 세션(세션 후손).
    Session(SessionId),
    /// 아무 세션도(계보 조회 실패).
    Restricted,
}

impl Scope {
    /// `target` 세션을 읽거나 쓸 수 있는지. 세션을 만든 상위 세션도 허용한다.
    pub fn allows(self, target: SessionId, roots: &Roots) -> bool {
        match self {
            Self::Full => true,
            Self::Session(id) => id == target || roots.created_by(target, id),
            Self::Restricted => false,
        }
    }
}

/// 세션 루트 프로세스(holder가 띄운 셸·에이전트) 목록과 생성 계보. PID 재사용을 구분하려고 등록 시점의 시작 시각을 같이 둔다.
#[derive(Debug, Default)]
pub struct Roots {
    pids: HashMap<u32, (SessionId, u64)>,
    creators: HashMap<SessionId, SessionId>,
    path: Option<PathBuf>,
}

impl Roots {
    /// 생성 계보만 디스크에서 복원한다. PID와 시작 시각은 holder 재전달 시 새로 검증한다.
    pub fn open(path: Option<PathBuf>) -> Self {
        let creators = path
            .as_deref()
            .and_then(|p| fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<Vec<(SessionId, SessionId)>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        Self {
            creators,
            path,
            ..Self::default()
        }
    }

    fn save(&self, creators: &HashMap<SessionId, SessionId>) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut rows: Vec<_> = creators.iter().map(|(&id, &parent)| (id, parent)).collect();
        rows.sort_unstable();
        let bytes = serde_json::to_vec(&rows).map_err(io::Error::other)?;
        let tmp = path.with_extension("tmp");
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?
            .write_all(&bytes)?;
        fs::rename(tmp, path)
    }

    /// 세션 `id`의 루트 `pid`를 등록한다(holder `Spawned`·`Session`을 받은 직후).
    ///
    /// # Errors
    /// 프로세스가 이미 없으면 등록하지 않고 오류(남은 후손은 launchd에 입양돼 [`Scope::Full`]로 보인다).
    pub fn insert(&mut self, id: SessionId, pid: u32) -> io::Result<()> {
        let start = sys::start_us(pid)?;
        self.pids.insert(pid, (id, start));
        Ok(())
    }

    /// 세션 `id`를 만든 피어의 범위를 기록한다(세션 안에서 만든 하위 세션이면 만든 세션을 상위 생성자로 남긴다).
    pub fn record_creator(&mut self, id: SessionId, creator: Scope) -> io::Result<()> {
        let mut next = self.creators.clone();
        next.remove(&id);
        if let Scope::Session(parent) = creator
            && parent != id
        {
            next.insert(id, parent);
        }
        if next != self.creators {
            self.save(&next)?;
            self.creators = next;
        }
        Ok(())
    }

    /// `target`이 `ancestor` 세션(또는 그 하위 세션)에서 만들어졌는지.
    pub fn created_by(&self, mut target: SessionId, ancestor: SessionId) -> bool {
        for _ in 0..MAX_DEPTH {
            let Some(&creator) = self.creators.get(&target) else {
                return false;
            };
            if creator == ancestor {
                return true;
            }
            target = creator;
        }
        false
    }

    /// 세션 `id`의 루트를 지운다(세션 종료). 하위 세션이 남아 있으면 상위 생성자로 이어 준다.
    pub fn remove(&mut self, id: SessionId) {
        self.forget(id);
        self.save(&self.creators).ok();
    }

    /// 기동 시 `live`에 없는 자식 ID 항목을 버린다(죽은 세션의 계보 잔류 정리).
    /// 남는 하위 세션은 [`Roots::remove`]와 같이 상위 생성자로 이어 준다.
    pub fn prune_dead(&mut self, live: &[SessionId]) {
        let dead: Vec<SessionId> = self
            .creators
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        if dead.is_empty() {
            return;
        }
        for id in dead {
            self.forget(id);
        }
        self.save(&self.creators).ok();
    }

    fn forget(&mut self, id: SessionId) {
        self.pids.retain(|_, (sid, _)| *sid != id);
        let parent = self.creators.remove(&id);
        for creator in self.creators.values_mut() {
            if *creator == id
                && let Some(p) = parent
            {
                *creator = p;
            }
        }
    }

    /// `pid`가 등록된 루트이고 시작 시각도 같으면(재사용된 PID가 아니면) 그 세션.
    fn session(&self, pid: u32, start_us: impl Fn(u32) -> io::Result<u64>) -> Option<SessionId> {
        let &(id, start) = self.pids.get(&pid)?;
        (start_us(pid).ok()? == start).then_some(id)
    }
}

/// 프로세스 조회. 실제는 [`Sys`], 테스트는 가짜 트리.
trait Procs {
    fn parent(&self, pid: u32) -> io::Result<u32>;
    fn start_us(&self, pid: u32) -> io::Result<u64>;
}

struct Sys;

impl Procs for Sys {
    fn parent(&self, pid: u32) -> io::Result<u32> {
        sys::parent_pid(pid)
    }
    fn start_us(&self, pid: u32) -> io::Result<u64> {
        sys::start_us(pid)
    }
}

/// `pid`에서 ppid를 따라 올라가며 범위를 정한다.
fn walk(mut pid: u32, roots: &Roots, procs: &impl Procs) -> Scope {
    for _ in 0..MAX_DEPTH {
        if pid <= 1 {
            return Scope::Full; // launchd(1) 또는 kernel_task(0)에 닿음
        }
        if let Some(id) = roots.session(pid, |p| procs.start_us(p)) {
            return Scope::Session(id);
        }
        let Ok(ppid) = procs.parent(pid) else {
            return Scope::Restricted;
        };
        pid = ppid;
    }
    Scope::Restricted
}

/// `pid`(또는 그 조상)가 루트인 세션. 외부 프로세스이거나 계보를 확인하지 못하면 `None`.
pub fn session_of(pid: u32, roots: &Roots) -> Option<SessionId> {
    match walk(pid, roots, &Sys) {
        Scope::Session(id) => Some(id),
        Scope::Full | Scope::Restricted => None,
    }
}

/// 피어 코드서명 검증 정책 파일 이름(상태 디렉터리 아래, MK-096).
pub const SIGNING_POLICY_FILE: &str = "peer-signing.toml";

/// 피어 코드서명 검증 정책(MK-096). **기본 꺼짐**(Apple Developer 미가입·ad hoc 개발 빌드, 오웬 결정 #1).
/// `require_signed`을 켜거나 요구 식별자·팀을 지정하면 강제된다.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_field_names,
    reason = "TOML 정책 필드는 require_ 접두어로 의도적(설정 가독성)"
)]
pub struct SigningPolicy {
    /// 켜면 전체 범위 피어에 유효한 서명을 요구한다.
    #[serde(default)]
    pub require_signed: bool,
    /// 지정하면 피어 식별자가 이와 같아야 한다(다르면 거부).
    #[serde(default)]
    pub require_identifier: Option<String>,
    /// 지정하면 피어 팀 식별자가 이와 같아야 한다.
    #[serde(default)]
    pub require_team: Option<String>,
}

impl SigningPolicy {
    /// 정책 파일이 있지만 읽지 못했거나 문법·필드가 틀렸을 때의 거부 정책(fail-closed).
    pub fn deny_all() -> Self {
        Self {
            require_signed: true,
            require_identifier: Some("\0invalid-policy".to_owned()),
            require_team: None,
        }
    }

    /// 검증이 강제되는지. 요구 식별자·팀을 지정한 것만으로도 강제다.
    pub fn enforced(&self) -> bool {
        self.require_signed || self.require_identifier.is_some() || self.require_team.is_some()
    }

    /// 강제되지 않으면 항상 통과(기존 동작). 강제되면 유효한 서명 + 지정된 식별자·팀 일치가 필요하다.
    /// 검증 자체가 실패한 피어(`None`·무효)는 미서명으로 보고 거부한다.
    pub fn allows(&self, sign: Option<&CodeSign>) -> bool {
        if !self.enforced() {
            return true;
        }
        let Some(sign) = sign else {
            return false;
        };
        if !sign.valid {
            return false;
        }
        if self
            .require_identifier
            .as_ref()
            .is_some_and(|want| Some(want) != sign.identifier.as_ref())
        {
            return false;
        }
        if self
            .require_team
            .as_ref()
            .is_some_and(|want| Some(want) != sign.team.as_ref())
        {
            return false;
        }
        true
    }

    /// 상태 디렉터리에서 정책을 읽는다. 파일이 없으면 꺼짐(기본값)이고, 파일이 있는데 읽지 못하거나 깨졌으면
    /// fail-closed([`Self::deny_all`])로 거부한다. 빈 식별자·팀은 지정하지 않은 것으로 본다.
    pub fn load(dir: Option<&Path>) -> Self {
        let path = dir.map(|d| d.join(SIGNING_POLICY_FILE));
        let text = match path {
            Some(path) => match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
                Err(e) => {
                    tracing::warn!("peer signing policy unreadable ({}): {e}", path.display());
                    return Self::deny_all();
                }
            },
            None => return Self::default(),
        };
        let mut policy: Self = match toml::from_str(&text) {
            Ok(policy) => policy,
            Err(e) => {
                tracing::warn!("peer signing policy malformed, denying full peers: {e}");
                return Self::deny_all();
            }
        };
        if policy
            .require_identifier
            .as_deref()
            .is_some_and(str::is_empty)
        {
            policy.require_identifier = None;
        }
        if policy.require_team.as_deref().is_some_and(str::is_empty) {
            policy.require_team = None;
        }
        policy
    }
}

/// accept한 연결의 피어.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    /// connect 시점 PID(`LOCAL_PEERPID`). 못 읽었으면 `None`.
    pub pid: Option<u32>,
    pub scope: Scope,
}

impl Peer {
    /// 연결 `s`의 피어 범위를 판정한다. 실패는 오류가 아니라 [`Scope::Restricted`]다.
    pub fn of(s: &UnixStream, roots: &Roots) -> Self {
        let pid = sys::peer_pid(s.as_fd()).ok();
        let scope = pid.map_or(Scope::Restricted, |p| walk(p, roots, &Sys));
        Self { pid, scope }
    }

    /// `op`(정적 이름, 예: `"send"`)로 `target`을 다뤄도 되는지. 안 되면 감사 로그에 한 줄 남기고 `false`.
    /// 감사 로그 쓰기 실패는 판정을 바꾸지 않는다(거부는 그대로 거부).
    pub fn authorize(
        &self,
        op: &'static str,
        target: SessionId,
        roots: &Roots,
        audit: &Audit,
    ) -> bool {
        if self.scope.allows(target, roots) {
            return true;
        }
        audit.deny(self, op, target).ok();
        false
    }

    /// 서명 정책을 전체 범위 피어에 적용한다(MK-096). 정책이 꺼졌거나 세션·제한 피어면 그대로 두고 `false`.
    /// 정책을 못 넘긴 전체 피어는 [`Scope::Restricted`]으로 내리고 감사 로그(`op=peer-sign`)에 남긴 뒤 `true`를
    /// 돌려준다. 호출자는 `true`면 연결을 닫는다(감시 연결 같은 범위 검사가 없는 경로도 막는다).
    pub fn gate_signing(
        &mut self,
        policy: &SigningPolicy,
        sign: Option<&CodeSign>,
        audit: &Audit,
    ) -> bool {
        if self.scope != Scope::Full || policy.allows(sign) {
            return false;
        }
        audit.deny(self, "peer-sign", SessionId(0)).ok();
        self.scope = Scope::Restricted;
        true
    }

    /// 세션 무관 요청(`target=0`) 거부를 감사 로그에 남긴다.
    pub fn deny(&self, op: &'static str, audit: &Audit) {
        audit.deny(self, op, SessionId(0)).ok();
    }
}

/// 거부 감사 로그(append-only 텍스트, 0600). A8: 명령줄·경로·터미널 내용은 남기지 않는다.
#[derive(Debug, Clone)]
pub struct Audit {
    path: PathBuf,
}

impl Audit {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 한 줄: `<unix 초> deny op=<op> target=<세션> peer_pid=<pid|?> scope=<session:N|restricted|full>`.
    fn deny(&self, peer: &Peer, op: &'static str, target: SessionId) -> io::Result<()> {
        // ponytail: 크기 확인과 rename 사이 경쟁은 한 벌을 더 밀어낼 뿐이다. 거부는 드물어 단순 회전으로 충분하다.
        if fs::metadata(&self.path).is_ok_and(|m| m.len() >= AUDIT_MAX) {
            let mut old = self.path.clone().into_os_string();
            old.push(".1");
            fs::rename(&self.path, old)?;
        }
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let pid = peer.pid.map_or_else(|| "?".to_owned(), |p| p.to_string());
        let scope = match peer.scope {
            Scope::Full => "full".to_owned(),
            Scope::Session(id) => format!("session:{}", id.0),
            Scope::Restricted => "restricted".to_owned(),
        };
        let line = format!(
            "{secs} deny op={op} target={} peer_pid={pid} scope={scope}\n",
            target.0
        );
        OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&self.path)?
            .write_all(line.as_bytes()) // O_APPEND 한 번 쓰기라 연결 스레드끼리 줄이 섞이지 않는다
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
