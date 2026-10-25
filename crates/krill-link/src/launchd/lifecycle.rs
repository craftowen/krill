//! 격리 실행의 자동 등록 방지와 소유 경로가 사라진 launchd 작업 정리.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Once};
use std::time::Duration;

use super::{LINK_LABEL, launchctl_allowed, link_label};

/// HOME 재정의만으로도 실제 사용자 도메인에 임시 작업을 만들 수 있으므로 OS 계정과 대조한다.
pub(crate) fn default_autostart_allowed() -> bool {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let account = Command::new("/usr/bin/dscacheutil")
        .args(["-q", "user", "-a", "uid"])
        .arg(rustix::process::getuid().as_raw().to_string())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|line| line.strip_prefix("dir: ").map(PathBuf::from))
        });
    same_home(home.as_deref(), account.as_deref())
}

fn same_home(home: Option<&Path>, account: Option<&Path>) -> bool {
    match (home, account) {
        (Some(home), Some(account)) => match (home.canonicalize(), account.canonicalize()) {
            (Ok(home), Ok(account)) => home == account,
            _ => false,
        },
        _ => false,
    }
}

// 배포 빌드에는 실행기 훅을 컴파일하지 않는다. debug 자식도 skip 훅을 함께 강제해야 한다.
fn test_launchctl() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    #[cfg(debug_assertions)]
    if std::env::var_os("KRILL_LINK_SKIP_LAUNCHCTL").as_deref() == Some(OsStr::new("1")) {
        return std::env::var_os("KRILL_LINK_TEST_LAUNCHCTL")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
    }
    None
}

fn run_launchctl(args: &[String]) -> io::Result<Output> {
    if let Some(executable) = test_launchctl() {
        // 주입 실행 실패 시 실제 launchctl로 폴백하지 않는다.
        return Command::new(executable).args(args).output();
    }
    if !launchctl_allowed() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "launchctl disabled",
        ));
    }
    Command::new("/bin/launchctl").args(args).output()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathState {
    Present,
    Missing,
    Unavailable,
}

fn path_state(path: &Path) -> PathState {
    match fs::metadata(path) {
        Ok(_) => PathState::Present,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // 알려진 autofs/네트워크 루트는 미마운트와 삭제를 구분할 수 없어 보존한다.
            if ["/Network", "/net", "/home"]
                .iter()
                .any(|root| path.starts_with(root))
            {
                return PathState::Unavailable;
            }
            for ancestor in path.ancestors() {
                match fs::symlink_metadata(ancestor) {
                    // 끊어진 링크를 건너뛰어 부모의 존재를 삭제 근거로 삼지 않는다.
                    Ok(_) => {
                        return ancestor
                            .canonicalize()
                            .map_or(PathState::Unavailable, |resolved| {
                                missing_path_state(&resolved)
                            });
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => return PathState::Unavailable,
                }
            }
            PathState::Unavailable
        }
        Err(_) => PathState::Unavailable,
    }
}

/// 가장 가까운 존재 조상에서 마운트 루트를 찾는다. firmlink와 대소문자도 동일시한다.
fn missing_path_state(existing: &Path) -> PathState {
    let Ok(volumes) = fs::metadata("/Volumes") else {
        return PathState::Unavailable;
    };
    let mut child: Option<fs::Metadata> = None;
    for ancestor in existing.ancestors() {
        let Ok(current) = fs::metadata(ancestor) else {
            return PathState::Unavailable;
        };
        if (current.dev(), current.ino()) == (volumes.dev(), volumes.ino()) {
            // /Volumes 자체나 같은 장치의 빈 마운트 디렉터리는 미마운트로 본다.
            return if child.is_some_and(|root| root.dev() != volumes.dev()) {
                PathState::Missing
            } else {
                PathState::Unavailable
            };
        }
        child = Some(current);
    }
    PathState::Missing
}

fn isolated_label(label: &str) -> bool {
    label
        .strip_prefix(&format!("{LINK_LABEL}."))
        .is_some_and(|hash| hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// 시작을 기다리게 하지 않고 한 프로세스에서 한 번만 과거 해시 라벨을 점검한다.
pub(crate) fn cleanup_orphans_in_background() {
    static START: Once = Once::new();
    if !launchctl_allowed() {
        return;
    }
    START.call_once(|| {
        if let Err(error) = std::thread::Builder::new()
            .name("link-agent-cleanup".into())
            .spawn(|| cleanup_orphans(&mut run_launchctl))
        {
            tracing::debug!(%error, "원격 연결 고아 작업 정리 스레드 시작 실패");
        }
    });
}

fn cleanup_orphans(run: &mut impl FnMut(&[String]) -> io::Result<Output>) {
    let Ok(list) = run(&["list".into()]) else {
        return;
    };
    if !list.status.success() {
        return;
    }
    let Ok(list_text) = std::str::from_utf8(&list.stdout) else {
        return;
    };
    let domain = format!("gui/{}", rustix::process::getuid().as_raw());
    let mut candidates = Vec::new();
    for label in list_text
        .lines()
        .filter_map(|line| line.split_whitespace().nth(2))
        .filter(|label| isolated_label(label))
    {
        let target = format!("{domain}/{label}");
        let Ok(job) = run(&["print".into(), target.clone()]) else {
            continue;
        };
        if !job.status.success() {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&job.stdout) else {
            continue;
        };
        // 디렉터리 이름을 추측하지 않는다. launchd에 기록된 최상위 path만 신뢰한다.
        if !text.starts_with(&format!("{target} = {{\n"))
            || !text.lines().any(|line| line == "\ttype = LaunchAgent")
        {
            continue;
        }
        let Some(path) = text.lines().find_map(|line| line.strip_prefix("\tpath = ")) else {
            continue;
        };
        let path = Path::new(path);
        let state = text
            .lines()
            .find_map(|line| line.strip_prefix("\t\tKRILL_STATE_DIR => "))
            .map(Path::new);
        if state.is_some_and(|state| path_state(state) == PathState::Unavailable) {
            continue;
        }
        let state_missing = state.is_some_and(|state| {
            state.is_absolute()
                && link_label(Some(state)) == label
                && path_state(state) == PathState::Missing
        });
        if path.is_absolute()
            && path.file_name() == Some(OsStr::new(&format!("{label}.plist")))
            && (path_state(path) == PathState::Missing || state_missing)
        {
            candidates.push((target, path.to_path_buf(), state.map(Path::to_path_buf)));
        }
    }
    if !candidates.is_empty() {
        std::thread::sleep(Duration::from_secs(2));
    }
    for (target, path, state) in candidates {
        // 두 관측 사이 다른 작업으로 재등록되면 이전 스냅샷으로 정리하지 않는다.
        let Ok(job) = run(&["print".into(), target.clone()]) else {
            continue;
        };
        let Ok(text) = std::str::from_utf8(&job.stdout) else {
            continue;
        };
        if !job.status.success()
            || !text.starts_with(&format!("{target} = {{\n"))
            || !text.lines().any(|line| line == "\ttype = LaunchAgent")
            || text
                .lines()
                .find_map(|line| line.strip_prefix("\tpath = "))
                .map(Path::new)
                != Some(path.as_path())
            || text
                .lines()
                .find_map(|line| line.strip_prefix("\t\tKRILL_STATE_DIR => "))
                .map(Path::new)
                != state.as_deref()
            || state
                .as_deref()
                .is_some_and(|state| path_state(state) == PathState::Unavailable)
        {
            continue;
        }
        let label = target.rsplit('/').next().unwrap_or_default();
        if (path_state(&path) == PathState::Missing
            || state.as_deref().is_some_and(|state| {
                state.is_absolute()
                    && link_label(Some(state)) == label
                    && path_state(state) == PathState::Missing
            }))
            && let Err(error) = run(&["bootout".into(), target])
        {
            tracing::debug!(%error, "원격 연결 고아 작업 bootout 실패");
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Observation {
    Healthy,
    Pending,
    Retired,
}

/// 새 plist가 넘긴 경로와 launchd 자신의 서비스 이름이 일치할 때만 자기 작업을 감시한다.
#[derive(Debug, Clone)]
pub struct ManagedAgent {
    target: String,
    state: PathBuf,
    // 열린 디렉터리 fd로 삭제 뒤 재생성된 경로와 원래 상태를 구별한다.
    directory: Option<Arc<File>>,
    plist: PathBuf,
}

impl ManagedAgent {
    /// 일반 `serve`와 skip 훅에는 소유권을 부여하지 않는다(debug 주입 실행기만 예외).
    pub fn from_env(state: &Path) -> Option<Self> {
        if !launchctl_allowed() && test_launchctl().is_none() {
            return None;
        }
        let label = std::env::var("XPC_SERVICE_NAME").ok()?;
        let plist = std::env::var_os("KRILL_LINK_AGENT_PLIST").map(PathBuf::from)?;
        Self::new(&label, state, plist)
    }

    fn new(label: &str, state: &Path, plist: PathBuf) -> Option<Self> {
        if (label != LINK_LABEL && label != link_label(Some(state)))
            || !state.is_absolute()
            || !plist.is_absolute()
            || plist.file_name() != Some(OsStr::new(&format!("{label}.plist")))
        {
            return None;
        }
        Some(Self {
            target: format!("gui/{}/{label}", rustix::process::getuid().as_raw()),
            state: state.to_path_buf(),
            directory: File::open(state).ok().map(Arc::new),
            plist,
        })
    }

    /// 시작 전에도 확인해 삭제된 상태 디렉터리를 다시 만들지 않는다.
    fn is_orphaned(&self) -> bool {
        self.state_status() != PathState::Unavailable
            && (self.state_status() == PathState::Missing
                || path_state(&self.plist) == PathState::Missing
                || self.directory_replaced())
    }

    fn directory_replaced(&self) -> bool {
        let Some(directory) = &self.directory else {
            return false;
        };
        match (directory.metadata(), fs::metadata(&self.state)) {
            (Ok(original), Ok(current)) => {
                (original.dev(), original.ino()) != (current.dev(), current.ino())
            }
            _ => false,
        }
    }

    fn retire(&self, run: &mut impl FnMut(&[String]) -> io::Result<Output>) -> bool {
        if !self.is_orphaned() {
            return false;
        }
        match run(&["bootout".into(), self.target.clone()]) {
            Ok(out) => out.status.success(),
            Err(error) => {
                tracing::debug!(%error, "원격 연결 자기 작업 bootout 실패");
                false
            }
        }
    }

    fn state_status(&self) -> PathState {
        let status = path_state(&self.state);
        // 시작 전에는 임의 위치의 미마운트와 삭제를 구분할 fd가 없다.
        // plist가 남았으면 KeepAlive로 재시도하고 진짜 고아 정리는 배경 점검에 맡긴다.
        if self.directory.is_none()
            && status == PathState::Missing
            && path_state(&self.plist) == PathState::Present
        {
            return PathState::Unavailable;
        }
        // 실행 도중 임의 위치의 마운트가 사라진 경우에도 상위 파일시스템으로의 복귀를 감지한다.
        if let Some(directory) = &self.directory
            && let Ok(original) = directory.metadata()
        {
            for ancestor in self.state.ancestors() {
                let current = fs::symlink_metadata(ancestor).and_then(|metadata| {
                    if metadata.is_symlink() {
                        fs::metadata(ancestor)
                    } else {
                        Ok(metadata)
                    }
                });
                match current {
                    Ok(current) if current.dev() != original.dev() => {
                        return PathState::Unavailable;
                    }
                    Ok(_) => break,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(_) => return PathState::Unavailable,
                }
            }
        }
        status
    }

    fn observe(
        &self,
        consecutive: &mut u8,
        run: &mut impl FnMut(&[String]) -> io::Result<Output>,
    ) -> io::Result<Observation> {
        if self.state_status() == PathState::Unavailable {
            *consecutive = 0;
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "link state temporarily unavailable",
            ));
        }
        if !self.is_orphaned() {
            *consecutive = 0;
            return Ok(Observation::Healthy);
        }
        *consecutive = consecutive.saturating_add(1);
        if *consecutive >= 2 && self.retire(run) {
            return Ok(Observation::Retired);
        }
        Ok(Observation::Pending)
    }

    /// 시작 전 순간 부재는 재확인하고 복구되면 정상 기동한다.
    /// `false`는 bootout 완료. 접근 불가는 오류로 `KeepAlive` 재시도에 맡긴다.
    pub async fn prepare_start(&self) -> io::Result<bool> {
        let mut consecutive = 0;
        loop {
            let agent = self.clone();
            let (observation, count) = tokio::task::spawn_blocking(move || {
                let observation = agent.observe(&mut consecutive, &mut run_launchctl);
                (observation, consecutive)
            })
            .await
            .map_err(io::Error::other)?;
            consecutive = count;
            match observation? {
                Observation::Healthy => return Ok(true),
                Observation::Retired => return Ok(false),
                Observation::Pending => tokio::time::sleep(Duration::from_secs(2)).await,
            }
        }
    }

    /// 연속 두 번의 소실 관측 후 bootout한다. 실패는 재시도하고 접근 불가는 오류로 종료한다.
    pub async fn wait_for_removal(self) -> io::Result<()> {
        let mut consecutive = 0;
        loop {
            let agent = self.clone();
            let (observation, count) = tokio::task::spawn_blocking(move || {
                let observation = agent.observe(&mut consecutive, &mut run_launchctl);
                (observation, consecutive)
            })
            .await
            .map_err(io::Error::other)?;
            consecutive = count;
            if observation? == Observation::Retired {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

#[cfg(test)]
mod tests;
