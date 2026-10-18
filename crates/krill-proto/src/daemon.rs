//! 앱·`krill`가 krilld에 붙는 길(MK-025): 소켓 위치, 없으면 데몬 기동, Hello 협상. launchd plist 생성도 여기 있다.
//!
//! # 기동 경로
//!
//! - **개발 빌드(ad hoc 서명)**: 실행 파일 옆의 `krill-holder --detach <krilld> <소켓>`을 std `Command`(macOS에서는
//!   `posix_spawn`)로 띄운다. holder가 `--detach`를 보고 스스로 `setsid`해 띄운 쪽의 제어 터미널·프로세스
//!   그룹에서 떨어진다. TCC 귀속은 띄운 앱을 따라가 불안정하다(T2 §3.4 기동 경로 4).
//! - **서명 빌드**: [`holder_plist`]로 만든 launchd 비지속 잡(MK-106에서 전환·검증). launchd가 띄운 holder는
//!   자기 `setsid` 금지(CONVENTIONS A1)라 plist에는 `--detach`가 없다.
//!
//! 단일 인스턴스는 krilld의 소켓 락(MK-022)과 holder가 쥔 listen 소켓(OWE-491)이 보장한다. holder가 살아 있으면
//! krilld가 재기동 중이어도 소켓이 연결을 받아 두므로(`ConnectionRefused`가 아님) holder를 새로 띄우지 않는다.
//! 여기서는 앱과 `krill`가 동시에 기동하다 holder를 둘 띄우지 않도록 `<소켓>.launch` flock 아래에서 다시 확인하고 띄운다.
//!
//! # 업그레이드 (MK-062)
//!
//! 앱은 붙은 krilld의 `ServerHello.build`가 자기 빌드보다 오래됐으면 [`upgrade`]로 교체를 요청한다. krilld는 holder를
//! 새 바이너리로 exec 핸드오프하고 다음 krilld 경로를 바꾼 뒤 끝난다. holder가 새 krilld를 바로 띄워 세션을 넘긴다
//! (세대 공존 없음 → 고아 데몬 0). 새 빌드의 krilld에 구 앱(N-1)이 붙으면 아무것도 요청하지 않는다(다운그레이드 금지).
//!
//! # 끊김과 재연결 (MK-026)
//!
//! - 앱은 [`heartbeat_hello`]로 붙은 감시 연결 하나를 들고 [`wait_responsive`]로 막혀 있다. 2초마다 ping을
//!   보내 EOF 없이 멈춘 krilld도 안다(미지원 데몬에는 기존 EOF 감시로 동작한다). krilld가 죽으면 커널이 소켓을
//!   닫아 바로 EOF가 난다. holder만 죽으면 재결합 소켓 준비 뒤 `Orphan` 프레임으로 깨운다(유휴 웨이크업 0).
//! - 끊기면 [`reconnect_until`]이 50ms에서 두 배씩(상한 500ms) 간격으로 [`connect`]한다(포기하지 않음). holder가
//!   살아 있으면 연결은 backlog에서 krilld를 기다렸다 붙는다. 고아 상태도 초기 감시 프레임으로 확인해 holder를
//!   다시 띄운다. 앱 없이 실행한 `krill`도 같은 경로로 복구한다.

use crate::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use crate::os::net::UnixStream;
use std::ffi::OsString;
use std::fmt;
use std::fs::{DirBuilder, OpenOptions, TryLockError};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::control::{FEAT_UPGRADE, Reply, Request, Upgrade, Upgraded, is_newer};
use crate::ipc::{self, ClientHello, ClientRole, Negotiated, ServerHello};
use crate::screen::Msg as _;

pub mod migrate;
pub use migrate::{LINUX_STATE_PARENT, state_dir_in};

/// 감시 연결에만 초기 holder 상태와 상실 통보를 보낸다. 기존 Hello DTO는 바꾸지 않는다.
pub const FEAT_HOLDER_WATCH: u64 = 1 << 7;
/// holder 아래에서 뜬 krilld가 소켓 락을 못 잡고 인계 상대(고아 krilld)도 없을 때의 종료 코드(OWE-589).
/// 이미 건강한 데몬이 떠 있는 중복 기동이다. 정상(0)·실패(1)·인자 오류(2)와 구별해 holder가 재기동 없이
/// 끝낸다. 직접 실행한 krilld는 MK-022대로 0으로 끝낸다.
pub const KRILLD_DUPLICATE: u8 = 3;
/// EOF 없이 멈춘 데몬을 감지하는 감시 연결 확장(OWE-586, ARCHITECTURE §4.3).
pub const FEAT_HEARTBEAT: u64 = 1 << 45;
/// 데몬→클라이언트 holder 상태 프레임(ARCHITECTURE §4.3).
pub const HOLDER_STATUS: u8 = 0x29;
/// 감시 연결 ping/pong kind. 값은 영구다.
pub const PING: u8 = 0x43;
pub const PONG: u8 = 0x63;
/// 정상 응답 뒤 다음 ping까지의 간격. 세션 수와 무관하게 앱당 감시 연결 하나다.
pub const INTERVAL: Duration = Duration::from_secs(2);

/// 감시 연결용 Hello에 ping/pong 지원을 더한다. 구 Hello fixture는 유지한다.
pub fn heartbeat_hello(role: ClientRole, name: &str) -> ClientHello {
    ClientHello {
        features: FEAT_HOLDER_WATCH | FEAT_HEARTBEAT,
        ..watch_hello(role, name)
    }
}

/// 감시 연결인지(화면·제어 연결이 아님). holder 상태 읽기와 감시 전용 판정에 쓴다.
fn is_watch(features: u64) -> bool {
    features & FEAT_HOLDER_WATCH != 0 && features & !(FEAT_HOLDER_WATCH | FEAT_HEARTBEAT) == 0
}

/// 감시 연결 PING 한 프레임. `nonce`는 PONG 되돌림 확인용이다.
pub fn ping(nonce: u64) -> Result<Vec<u8>, ipc::Error> {
    ipc::encode(PING, &nonce)
}

/// 받은 PING의 nonce. kind가 다르면 오류다.
pub fn ping_nonce(kind: u8, body: &[u8]) -> Result<u64, ipc::Error> {
    ipc::decode(PING, kind, body)
}

/// PING에 대한 답 한 프레임. `nonce`는 받은 그대로 되돌린다.
pub fn pong(nonce: u64) -> Result<Vec<u8>, ipc::Error> {
    ipc::encode(PONG, &nonce)
}

/// 받은 PONG의 nonce. kind가 다르면 오류다.
pub fn pong_nonce(kind: u8, body: &[u8]) -> Result<u64, ipc::Error> {
    ipc::decode(PONG, kind, body)
}

/// 협상한 감시 연결의 응답을 확인한다. 미지원 데몬에는 ping을 보내지 않고 기존 EOF 감시를 쓴다.
/// 응답 제한은 입력 ACK와 같은 PROBE이며, 실패한 연결은 호출자가 버리고 다시 붙어야 한다.
pub fn wait_responsive(stream: &mut UnixStream, features: u64) -> io::Result<()> {
    if features & FEAT_HEARTBEAT == 0 {
        wait_closed(stream);
        return Ok(());
    }
    watch_for(stream, INTERVAL, crate::input::PROBE)
}

fn watch_for(stream: &mut UnixStream, interval: Duration, probe: Duration) -> io::Result<()> {
    stream.set_write_timeout(Some(probe))?;
    let mut nonce = 0u64;
    stream.write_all(&ping(nonce)?)?;
    let mut pending = true;
    let mut deadline = Instant::now()
        .checked_add(probe)
        .ok_or(io::ErrorKind::TimedOut)?;
    loop {
        let mut reader = DeadlineRead {
            stream,
            deadline,
            read: 0,
        };
        let frame = ipc::read_frame(&mut reader);
        let consumed = reader.read;
        match frame {
            Ok(Some((kind, body))) if kind == HOLDER_STATUS => {
                if HolderStatus::decode(kind, &body)? == HolderStatus::Orphan {
                    return Ok(());
                }
            }
            Ok(Some((kind, body))) if pending && kind == PONG => {
                let echoed = pong_nonce(kind, &body)?;
                if echoed != nonce {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                pending = false;
                deadline = Instant::now()
                    .checked_add(interval)
                    .ok_or(io::ErrorKind::TimedOut)?;
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut && !pending && consumed == 0 => {
                nonce = nonce.wrapping_add(1);
                stream.write_all(&ping(nonce)?)?;
                pending = true;
                deadline = Instant::now()
                    .checked_add(probe)
                    .ok_or(io::ErrorKind::TimedOut)?;
            }
            Ok(None) => return Ok(()),
            Ok(Some(_)) => return Err(io::ErrorKind::InvalidData.into()),
            Err(e) => return Err(e),
        }
    }
}

/// [`ipc::read_frame`]의 길이·`body` 읽기 전체에 같은 마감. 조각 수신·`EINTR`로 마감을 밀지 않는다.
/// 소켓 타임아웃이 잠자기를 포함해 먼저 끝나도 깨어 있는 시계의 남은 시간만큼 다시 기다린다.
struct DeadlineRead<'a> {
    stream: &'a mut UnixStream,
    deadline: Instant,
    read: usize,
}

impl io::Read for DeadlineRead<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            self.stream.set_read_timeout(Some(remaining))?;
            match self.stream.read(buf) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Ok(n) => {
                    self.read = self.read.saturating_add(n);
                    return Ok(n);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// 감시 연결의 holder 상태. `Orphan`은 인계 소켓 준비가 끝나 새 holder를 띄워도 되는 상태다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HolderStatus {
    Linked,
    Orphan,
    /// holder 없이 직접 띄운 진단용 krilld. 자동 재결합 대상이 아니다.
    Standalone,
}

impl HolderStatus {
    pub fn encode(self) -> Result<Vec<u8>, ipc::Error> {
        ipc::encode(HOLDER_STATUS, &self)
    }

    pub fn decode(kind: u8, body: &[u8]) -> Result<Self, ipc::Error> {
        ipc::decode(HOLDER_STATUS, kind, body)
    }
}

/// 앱 번들 id. 상태 디렉터리 이름과 plist의 `AssociatedBundleIdentifiers`에 쓴다(MK-170, 2026-10-17 오웬 결정).
pub const BUNDLE_ID: &str = "dev.krill.app";
/// holder launchd 잡 이름. 헬퍼는 앱과 다른 고유 identifier(CONVENTIONS A3).
pub const HOLDER_LABEL: &str = "dev.krill.app.holder";
/// 개명 전 번들 id. 이관 전 상태 디렉터리·옛 launchd 라벨 해제·구버전 원격 소켓 탐색에만 쓴다([`migrate::run`]).
pub const LEGACY_BUNDLE_ID: &str = "dev.minikrill.app";
/// 개명 전 holder launchd 잡 이름. 이관 때 해제한다.
pub const LEGACY_HOLDER_LABEL: &str = "dev.minikrill.app.holder";
/// holder에게 "launchd 밖에서 떴으니 스스로 setsid하라"고 알리는 첫 인자. launchd 경로에서는 주지 않는다.
pub const DETACH_ARG: &str = "--detach";
/// macOS `sockaddr_un.sun_path` 크기(NUL 포함). 소켓 경로는 자르지 않고 오류로 본다(CONVENTIONS A6).
#[cfg(not(windows))]
pub const SUN_PATH_MAX: usize = 104;
/// Windows `afunix.h` `UNIX_PATH_MAX`(NUL 포함).
#[cfg(windows)]
pub const SUN_PATH_MAX: usize = 108;
/// launchd 잡 소프트 fd 한도(launchd 기본 256, C2 실측). holder가 스스로 올리는 값과 같다.
const NOFILE: u32 = 10_240;
/// 띄운 뒤 소켓이 생길 때까지 기다리는 최대 시간.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(10);
/// 붙은 뒤 `ServerHello`를 기다리는 최대 시간. krilld가 멈춰 있어도 앱·CLI가 붙잡히지 않게 한다.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// 앱·CLI의 첫 Hello에는 macOS 피어 코드서명 검증 IO가 포함된다.
const SIGNED_HELLO_TIMEOUT: Duration = Duration::from_secs(30);
/// 업그레이드 답을 기다리는 최대 시간. holder exec 핸드오프와 리더 정지(최대 2초)를 넉넉히 덮는다.
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(10);
/// 재연결 간격: 50ms에서 두 배씩, 상한 500ms. krilld가 다시 뜬 뒤 붙기까지 최대 0.5초(설계값).
const RETRY_FIRST: Duration = Duration::from_millis(50);
const RETRY_CAP: Duration = Duration::from_millis(500);
/// `<소켓>.launch` 락을 기다리는 최대 시간. 락을 쥔 쪽은 핸드셰이크 둘과 [`LAUNCH_TIMEOUT`] 안에 놓으므로
/// 그보다 길게 잡는다. 넘으면 락을 쥔 채 멈춘 프로세스가 있는 것이다.
const LOCK_TIMEOUT: Duration = Duration::from_secs(20);

/// 상태 디렉터리 재정의 환경변수(`KRILL_STATE_DIR`, 개발·테스트 격리). 상태 디렉터리 재정의 여부는 모두 이 함수로 판단한다.
pub fn state_dir_override() -> Option<OsString> {
    std::env::var_os("KRILL_STATE_DIR")
}

/// 상태 디렉터리. [`state_dir_override`]가 있으면 그것, 없으면 `~/Library/Application Support/<번들 id>`
/// (Windows는 `%LOCALAPPDATA%\Krill`).
/// 이관 전(새 디렉터리 없음 + 옛 디렉터리가 실제 디렉터리)에는 옛 번들 id 경로다([`migrate::run`]).
pub fn state_dir() -> Option<PathBuf> {
    if let Some(dir) = state_dir_override() {
        return Some(PathBuf::from(dir));
    }
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME")?;
    #[cfg(windows)]
    let home = std::env::var_os("LOCALAPPDATA")?;
    Some(state_dir_in(Path::new(&home)))
}

/// 데몬 기동에 필요한 경로.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub socket: PathBuf,
    pub holder: PathBuf,
    pub krilld: PathBuf,
}

impl Paths {
    /// 개발 빌드·앱 번들: 소켓은 `<상태 디렉터리>/run/krilld.sock`, `krill-holder`·`krilld`는
    /// 심볼릭 링크를 푼 실행 파일 옆이나 앱 번들(`Contents/Helpers`·`Contents/MacOS`)에서 찾는다.
    pub fn dev() -> io::Result<Self> {
        let state = state_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
        let exe = std::env::current_exe()?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut apps = Vec::new();
        if allow_installed_apps(&exe, home.as_deref()) {
            if let Some(home) = &home {
                apps.push(home.join("Applications/Krill.app"));
            }
            apps.push(PathBuf::from("/Applications/Krill.app"));
        }
        Self::from_exe(&exe, &state, &apps)
    }

    fn from_exe(exe: &Path, state: &Path, apps: &[PathBuf]) -> io::Result<Self> {
        let exe = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
        let bin = exe.parent().ok_or(io::ErrorKind::NotFound)?;
        Ok(Self {
            socket: state.join("run/krilld.sock"),
            holder: find_exe(bin, apps, "krill-holder"),
            krilld: find_exe(bin, apps, "krilld"),
        })
    }
}

fn is_temp_home(home: &Path) -> bool {
    let temp = std::env::temp_dir();
    home.starts_with(&temp)
        || home.starts_with("/tmp")
        || home.starts_with("/private/tmp")
        || home.starts_with("/var/folders")
        || home.starts_with("/private/var/folders")
}

/// 설치본 `Krill.app`(`~/Applications`·`/Applications`) 헬퍼 후보를 허용할지 판정한다(OWE-771).
///
/// `KRILL_STATE_DIR` 오버라이드, Cargo 테스트 실행 파일(`target/.../deps/...`), 테스트·빌드 러너 환경변수,
/// 임시 `HOME`(`HOME=/tmp/...` 등)에서는 호스트에 설치된 운영 `/Applications/Krill.app`의 헬퍼를
/// 띄워 고아 holder·krilld를 남기지 않도록 제외한다.
fn allow_installed_apps(exe: &Path, home: Option<&Path>) -> bool {
    if cfg!(test) || state_dir_override().is_some() {
        return false;
    }
    if !allow_installed_apps_for_paths(exe, home) {
        return false;
    }
    for var in [
        "KRILL_TEST_OFFLINE",
        "NEXTEST",
        "NEXTEST_EXECUTION_MODE",
        "RUST_TEST_THREADS",
        "CARGO",
        "CARGO_MANIFEST_DIR",
    ] {
        if std::env::var_os(var).is_some() {
            return false;
        }
    }
    true
}

fn allow_installed_apps_for_paths(exe: &Path, home: Option<&Path>) -> bool {
    let resolved = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    for candidate in [exe, resolved.as_path()] {
        if candidate
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "deps")
        {
            return false;
        }
    }
    if home.is_some_and(is_temp_home) {
        return false;
    }
    true
}

/// 심볼릭 링크를 푼 지금 실행 파일 옆이나 같은 앱 번들(`Contents/Helpers`·`Contents/MacOS`)에서 `name`을 찾는다.
/// 없으면 진단 메시지가 기대 위치를 가리키도록 `<실행 파일 디렉터리>/<name>`을 돌려준다.
pub fn sibling_exe(name: &str) -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    let bin = exe.parent().ok_or(io::ErrorKind::NotFound)?;
    Ok(find_exe(bin, &[], name))
}

/// `bin`(심볼릭 링크를 푼 실행 파일 디렉터리) 옆이나 같은 앱 번들(`Contents/Helpers`·`Contents/MacOS`)에서
/// `name`을 찾는다. 없으면 `<bin>/<name>`을 돌려준다.
pub fn exe_near(bin: &Path, name: &str) -> PathBuf {
    find_exe(bin, &[], name)
}

fn find_exe(bin: &Path, apps: &[PathBuf], name: &str) -> PathBuf {
    let name = &format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let direct = bin.join(name);
    if direct.is_file() {
        return direct;
    }
    if let Some(contents) = bin
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "Contents"))
        .or_else(|| bin.parent())
    {
        for sub in ["Helpers", "MacOS"] {
            let candidate = contents.join(sub).join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    for app in apps {
        for sub in ["Contents/Helpers", "Contents/MacOS"] {
            let candidate = app.join(sub).join(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    direct
}

/// 협상을 마친 연결.
#[derive(Debug)]
pub struct Connection {
    pub stream: UnixStream,
    pub server: ServerHello,
    pub negotiated: Negotiated,
    /// 이 호출이 holder를 띄웠다.
    pub launched: bool,
    /// 감시 연결에서 받은 상태. 구 데몬·일반 연결에서는 `None`.
    pub holder_status: Option<HolderStatus>,
}

/// 기동·연결 실패. 앱 배너와 `krill doctor`가 그대로 보여준다.
#[derive(Debug)]
pub enum Error {
    /// 소켓 경로(임시 이름 `.tmp` 포함)가 [`SUN_PATH_MAX`]를 넘는다.
    PathTooLong(PathBuf),
    /// 띄울 실행 파일이 없다.
    Missing(PathBuf),
    /// holder를 띄우지 못했다.
    Spawn(io::Error),
    /// holder가 소켓이 생기기 전에 끝났다.
    HolderExited(ExitStatus),
    /// 띄운 뒤 [`LAUNCH_TIMEOUT`] 안에 소켓이 생기지 않았다(krilld가 계속 죽는 경우 등).
    Timeout,
    /// 다른 기동자가 [`LOCK_TIMEOUT`] 넘게 `<소켓>.launch` 락을 쥐고 있다.
    LockTimeout,
    /// 연결·협상 실패(버전 불일치는 `InvalidData`).
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PathTooLong(p) => write!(
                f,
                "socket path too long (must be under {} bytes): {}",
                SUN_PATH_MAX.saturating_sub(4),
                p.display()
            ),
            Self::Missing(p) => write!(
                f,
                "executable not found: {} (run `cargo build --workspace --bins` first)",
                p.display()
            ),
            Self::Spawn(e) => write!(f, "failed to start krill-holder: {e}"),
            Self::HolderExited(s) => write!(f, "krill-holder exited before krilld was ready ({s})"),
            Self::Timeout => write!(
                f,
                "krilld did not publish its socket within {}s",
                LAUNCH_TIMEOUT.as_secs()
            ),
            Self::LockTimeout => write!(
                f,
                "another launcher held the launch lock for over {}s",
                LOCK_TIMEOUT.as_secs()
            ),
            Self::Io(e) => write!(f, "cannot talk to krilld: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// 이 빌드의 프로토콜 범위로 만든 [`ClientHello`].
pub fn client_hello(role: ClientRole, name: &str) -> ClientHello {
    ClientHello {
        proto_min: ipc::PROTO_MIN,
        proto_max: ipc::PROTO_MAX,
        features: ipc::FEATURES,
        role,
        name: name.to_owned(),
    }
}

/// 감시 연결용 Hello. 지원하는 krilld는 초기 holder 상태를 보내고, holder 상실 시 `Orphan`을 보낸다.
/// 구 데몬은 feature 교집합이 0이라 기존 EOF 감시로 동작한다.
pub fn watch_hello(role: ClientRole, name: &str) -> ClientHello {
    ClientHello {
        features: FEAT_HOLDER_WATCH,
        ..client_hello(role, name)
    }
}

/// krilld 종료·크래시 또는 holder 상실 통보까지 막고 기다린다(유휴 웨이크업 없음).
pub fn wait_closed(stream: &mut UnixStream) {
    while let Ok(Some((kind, body))) = ipc::read_frame(stream) {
        if HolderStatus::decode(kind, &body) == Ok(HolderStatus::Orphan) {
            return;
        }
    }
}

/// 업그레이드할 `krill-holder`·`krilld` 실행 파일을 인자 없이 한 번 띄워 사전 검증·예열한다(OWE-497).
///
/// - 새 실행 파일(새 vnode)의 첫 `execve`는 macOS `AppleSystemPolicy`(`syspolicyd`·`XProtect`·`tccd`) 평가로
///   약 60ms 막힌다. 핸드오프·리더 정지 전에 자식 프로세스에서 먼저 실행해 커널 vnode 캐시와 페이지 캐시를 데운다.
/// - Apple Silicon에서 서명·코드 페이지가 깨진 Mach-O를 그대로 `execve`하면 커널이 호출 프로세스에 `SIGKILL`을
///   보내 holder 자체가 죽는다. 자식이 시그널로 죽으면(`code() == None`) `EPERM`으로 거절한다.
///
/// # Errors
/// 절대 경로의 일반 파일이 아니면 `ENOENT`, 실행 실패는 OS 오류(`EACCES`·`ENOEXEC` 등), 시그널 종료는 `EPERM`.
pub fn preflight(path: &Path) -> io::Result<()> {
    use std::io::Read as _;

    if !path.is_absolute() || !path.is_file() {
        return Err(io::Error::from_raw_os_error(2)); // ENOENT
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if std::fs::metadata(path)?.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::from_raw_os_error(13)); // EACCES
        }
    }
    // std::process::Command는 내부에서 posix_spawnp를 써서 ENOEXEC를 만나면 /bin/sh로 폴백한다.
    // holder execve가 받는 Mach-O(단일·fat)·ELF(Linux 원격 호스트)·PE(Windows) 또는 #! 스크립트 매직인지 먼저 확인한다.
    let mut magic = [0_u8; 4];
    let n = std::fs::File::open(path)?.read(&mut magic)?;
    let valid_magic = magic.starts_with(b"#!")
        || (cfg!(target_os = "linux") && magic == *b"\x7fELF")
        || (cfg!(windows) && magic.starts_with(b"MZ"))
        || (n == 4
            && matches!(
                u32::from_ne_bytes(magic),
                0xfeed_face
                    | 0xfeed_facf
                    | 0xcefa_edfe
                    | 0xcffa_edfe
                    | 0xcafe_babe
                    | 0xbeba_feca
                    | 0xcafe_babf
                    | 0xbfba_feca
            ));
    if !valid_magic {
        return Err(io::Error::from_raw_os_error(8)); // ENOEXEC
    }
    let status = Command::new(path)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    match status.code() {
        None => Err(io::Error::from_raw_os_error(1)), // EPERM(코드서명 오류 SIGKILL 등)
        Some(126 | 127) => Err(io::Error::from_raw_os_error(8)), // ENOEXEC(posix_spawnp /bin/sh 폴백)
        Some(_) => Ok(()),
    }
}

/// 붙은 krilld가 이 빌드(`crate::BUILD`)보다 오래됐으면 `paths`의 holder·krilld로 교체를 요청하고 새 krilld가
/// 클라이언트를 받을 때까지 기다린다(MK-062). 교체했으면 새 krilld의 `ServerHello`. krilld가 업그레이드를 모르거나
/// (`FEAT_UPGRADE` 없음) 같거나 새 빌드면(다운그레이드 금지) 아무것도 하지 않고 `None`.
///
/// # Errors
/// 연결·협상 실패, krilld의 거절(`Failed`, 범위 밖·holder 없음·실행 파일 없음), 새 krilld가 제때 뜨지 않음.
pub fn upgrade(paths: &Paths, hello: &ClientHello) -> io::Result<Option<ServerHello>> {
    upgrade_as(paths, hello, crate::BUILD)
}

/// [`upgrade`]와 같고 이 빌드 대신 `build`로 요청한다(테스트·진단용). krilld도 다운그레이드를 거절한다.
///
/// # Errors
/// [`upgrade`]와 같다.
pub fn upgrade_as(
    paths: &Paths,
    hello: &ClientHello,
    build: &str,
) -> io::Result<Option<ServerHello>> {
    let control = client_hello(hello.role, &hello.name);
    let mut c = handshake(&paths.socket, &control, HELLO_TIMEOUT)?;
    if c.negotiated.features & FEAT_UPGRADE == 0 || is_newer(build, &c.server.build) != Some(true) {
        return Ok(None);
    }
    let request = Request::Upgrade(Upgrade {
        build: build.to_owned(),
        holder: paths.holder.as_os_str().as_encoded_bytes().to_vec(),
        krilld: paths.krilld.as_os_str().as_encoded_bytes().to_vec(),
    });
    c.stream.set_read_timeout(Some(UPGRADE_TIMEOUT))?;
    c.stream.write_all(&request.encode()?)?;
    let (kind, body) = ipc::read_frame(&mut c.stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    match Reply::decode(kind, &body)? {
        Reply::Upgrade(Upgraded::Started) => {}
        Reply::Upgrade(Upgraded::NotNewer) => return Ok(None),
        Reply::Failed(f) => return Err(io::Error::other(f.to_string())),
        _ => return Err(io::ErrorKind::InvalidData.into()),
    }
    let old = c.server.instance_id;
    drop(c);
    // holder가 listen 소켓을 쥐고 있어 연결은 backlog에서 새 krilld를 기다린다. 구 krilld가 아직 받으면 다시 붙는다.
    let deadline = Instant::now()
        .checked_add(LAUNCH_TIMEOUT)
        .ok_or(io::ErrorKind::TimedOut)?;
    loop {
        match handshake(&paths.socket, &control, HELLO_TIMEOUT) {
            Ok(n) if n.server.instance_id != old => return Ok(Some(n.server)),
            Ok(_) => {}
            Err(e) if is_absent(&e) || is_transition(&e) => {}
            Err(e) => return Err(e),
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        thread::sleep(POLL);
    }
}

/// 실패한 `attempt`번째(0부터) 재연결 뒤 기다릴 시간.
fn retry_delay(attempt: u32) -> Duration {
    RETRY_FIRST
        .saturating_mul(1u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX))
        .min(RETRY_CAP)
}

/// 붙을 때까지 [`connect`]를 다시 시도한다(포기하지 않음, 막힘).
// ponytail: 실패 이유는 버린다(배너는 "재연결 중"만). 오래 못 붙는 이유를 보여야 하면 콜백으로 넘긴다.
pub fn reconnect_until(paths: &Paths, hello: &ClientHello) -> Connection {
    let mut attempt = 0u32;
    loop {
        if let Ok(c) = connect(paths, hello) {
            return c;
        }
        thread::sleep(retry_delay(attempt));
        attempt = attempt.saturating_add(1);
    }
}

/// krilld에 붙는다. 소켓이 없거나 죽어 있거나 고아 상태면 holder를 띄우고 재결합까지 기다린다.
pub fn connect(paths: &Paths, hello: &ClientHello) -> Result<Connection, Error> {
    // krilld는 `<소켓>.tmp`에 bind한다.
    let mut tmp = paths.socket.clone().into_os_string();
    tmp.push(".tmp");
    if tmp.len() >= SUN_PATH_MAX {
        return Err(Error::PathTooLong(paths.socket.clone()));
    }
    match ready_handshake(&paths.socket, hello) {
        Ok(Some(c)) => return Ok(c),
        Ok(None) => {}
        Err(e) if is_absent(&e) || is_transition(&e) => {}
        Err(e) => return Err(Error::Io(e)),
    }
    for exe in [&paths.holder, &paths.krilld] {
        if !exe.is_file() {
            return Err(Error::Missing(exe.clone()));
        }
    }
    let _launch = launch_lock(&paths.socket, LOCK_TIMEOUT)?;
    // 락을 기다리는 동안 다른 클라이언트가 띄웠을 수 있다.
    match ready_handshake(&paths.socket, hello) {
        Ok(Some(c)) => return Ok(c),
        Ok(None) => {}
        Err(e) if is_absent(&e) => {}
        Err(e) => return Err(Error::Io(e)),
    }
    let mut command = Command::new(&paths.holder);
    #[cfg(unix)]
    command.arg(DETACH_ARG);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // Short-lived bootstrap only. The holder FFI boundary creates the real
        // holder with bInheritHandles=false and explicit Job breakaway.
        command
            .arg("--bootstrap")
            .creation_flags(0x0000_0008 | 0x0000_0200);
    }
    let mut holder = command
        .arg(&paths.krilld)
        .arg(&paths.socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Error::Spawn)?;
    let deadline = Instant::now()
        .checked_add(LAUNCH_TIMEOUT)
        .ok_or(Error::Timeout)?;
    loop {
        match ready_handshake(&paths.socket, hello) {
            Ok(Some(c)) => {
                // ponytail: holder는 띄운 프로세스의 자식으로 남는다(앱이 먼저 끝나면 launchd가 거둔다).
                // 앱보다 먼저 죽으면 앱이 끝날 때까지 좀비 하나가 남는다. 거슬리면 앱에서 waitpid 스레드.
                return Ok(Connection {
                    launched: true,
                    ..c
                });
            }
            Ok(None) => {}
            Err(e) if is_absent(&e) || is_transition(&e) => {}
            Err(e) => return Err(Error::Io(e)),
        }
        if let Some(status) = holder.try_wait().map_err(Error::Io)? {
            #[cfg(unix)]
            return Err(Error::HolderExited(status));
            #[cfg(windows)]
            if !status.success() {
                return Err(Error::HolderExited(status));
            }
        }
        if Instant::now() >= deadline {
            // 백오프로 krilld를 계속 다시 띄우는 holder를 남기지 않는다.
            holder.kill().ok();
            holder.wait().ok();
            return Err(Error::Timeout);
        }
        thread::sleep(POLL);
    }
}

/// 다른 기동자가 재결합 중 구 krilld를 닫은 경우. 이것만으로 띄우지 않고 기동 락 아래 다시 확인한다.
fn is_transition(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
    )
}

/// 초기 상태는 전용 감시 연결에서 읽는다. 일반 제어·화면 연결에는 새 프레임을 섞지 않는다.
/// `None`이면 고아 krilld라 소켓이 살아 있어도 holder를 띄워야 한다.
fn ready_handshake(socket: &Path, hello: &ClientHello) -> io::Result<Option<Connection>> {
    let mut watch = watch_hello(hello.role, &hello.name);
    if hello.features & FEAT_HEARTBEAT != 0 {
        watch.features |= FEAT_HEARTBEAT;
    }
    // 기한을 지정하지 않은 기본 연결은 macOS 피어 코드서명 검증 IO를 기다린다.
    // 호출자가 기한을 명시한 handshake/dial 경로와 hook(수십 ms 예산)은 그대로 둔다.
    let first_hello = default_hello_timeout(hello.role);
    let c = handshake(socket, &watch, first_hello)?;
    if c.holder_status == Some(HolderStatus::Orphan) {
        return Ok(None);
    }
    if is_watch(hello.features) {
        Ok(Some(c))
    } else {
        drop(c);
        handshake(socket, hello, first_hello).map(Some)
    }
}

/// 기본 앱·CLI 연결이 첫 `ServerHello`를 기다리는 상한. 서명 검증이 느린 머신에서도
/// 붙도록 [`HELLO_TIMEOUT`]보다 넉넉히 잡는다. hook·명시 기한 경로는 쓰지 않는다.
fn default_hello_timeout(role: ClientRole) -> Duration {
    if matches!(role, ClientRole::App | ClientRole::Cli) {
        HELLO_TIMEOUT.max(SIGNED_HELLO_TIMEOUT)
    } else {
        HELLO_TIMEOUT
    }
}

/// 소켓 파일이 없거나(처음) 죽은 소켓만 남았다(krilld가 죽음).
fn is_absent(e: &io::Error) -> bool {
    // Windows AF_UNIX reports a missing socket as WSAENETDOWN (OWE-828).
    if cfg!(windows) && e.raw_os_error() == Some(10050) {
        return true;
    }
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// `<소켓>.launch`의 flock을 `timeout`까지 기다려 잡는다. 소켓 디렉터리를 krilld와 같은 규칙(0700)으로
/// 먼저 만든다.
fn launch_lock(socket: &Path, timeout: Duration) -> Result<std::fs::File, Error> {
    if let Some(dir) = socket.parent().filter(|d| !d.as_os_str().is_empty()) {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(Error::Io)?;
    }
    let mut name = OsString::from(socket);
    name.push(".launch");
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(name)
        .map_err(Error::Io)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(Error::LockTimeout)?;
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(f),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => thread::sleep(POLL),
            Err(TryLockError::WouldBlock) => return Err(Error::LockTimeout),
            Err(TryLockError::Error(e)) => return Err(Error::Io(e)),
        }
    }
}

/// 이미 떠 있는 krilld에 붙어서 `ServerHello`를 받고 `hello`로 답한 뒤 협상한다(데몬을 띄우지 않는다). 순서는
/// [`crate::ipc`] 핸드셰이크와 같다. `timeout`은 Hello 읽기·쓰기 각각의 상한이고, 돌려준 스트림에는 타임아웃이
/// 없다. 명시한 기한은 그대로 존중한다. 서명 검증 여유가 필요한 기본 앱·CLI 연결은 [`connect`]가
/// [`SIGNED_HELLO_TIMEOUT`]으로 이 함수를 부른다. 소켓 파일이 없으면 `NotFound`,
/// krilld가 죽었으면 `ConnectionRefused`.
pub fn handshake(socket: &Path, hello: &ClientHello, timeout: Duration) -> io::Result<Connection> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let (kind, body) = ipc::read_frame(&mut stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    let server = ServerHello::decode(kind, &body)?;
    stream.write_all(&hello.encode()?)?;
    let negotiated = ipc::negotiate(&server, hello).ok_or(ipc::Error::Incompatible)?;
    let holder_status = if is_watch(hello.features) && negotiated.features & FEAT_HOLDER_WATCH != 0
    {
        let (kind, body) = ipc::read_frame(&mut stream)?.ok_or(io::ErrorKind::UnexpectedEof)?;
        Some(HolderStatus::decode(kind, &body)?)
    } else {
        None
    };
    // macOS는 피어가 이미 닫은 소켓의 setsockopt를 EINVAL로 거부한다(OWE-487). 협상은 끝났고 닫힘은 다음
    // 읽기에서 EOF로 드러나므로 실패로 보지 않는다.
    stream.set_read_timeout(None).ok();
    stream.set_write_timeout(None).ok();
    Ok(Connection {
        stream,
        server,
        negotiated,
        launched: false,
        holder_status,
    })
}

/// 서명 빌드에서 `launchctl bootstrap gui/<uid>`에 줄 holder 잡 plist(ARCHITECTURE §4 기동, MK-106에서 사용).
///
/// 비지속 잡(`RunAtLoad`, `KeepAlive=false`), 세션이 holder의 프로세스 그룹 밖으로 나가도 죽이지 않게
/// `AbandonProcessGroup`, 스로틀 없는 `ProcessType=Interactive`, 앱에 귀속되는 `AssociatedBundleIdentifiers`.
/// [`DETACH_ARG`]는 넣지 않는다(launchd가 띄운 holder는 자기 setsid 금지, CONVENTIONS A1).
pub fn holder_plist(holder: &str, krilld: &str, socket: &str) -> String {
    let mut args = String::new();
    for a in [holder, krilld, socket] {
        args.push_str("\t\t<string>");
        args.push_str(&escape(a));
        args.push_str("</string>\n");
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{HOLDER_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
{args}	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<false/>
	<key>AbandonProcessGroup</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>AssociatedBundleIdentifiers</key>
	<array>
		<string>{BUNDLE_ID}</string>
	</array>
	<key>SoftResourceLimits</key>
	<dict>
		<key>NumberOfFiles</key>
		<integer>{NOFILE}</integer>
	</dict>
</dict>
</plist>
"#
    )
}

/// plist `<string>` 안에 넣을 수 있게 XML 특수 문자를 바꾼다.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
