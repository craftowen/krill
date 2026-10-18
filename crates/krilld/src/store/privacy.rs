//! 기록 설정·Time Machine 제외·삭제(MK-075). 잠정 MK-024 저장소에 대한 의존을 이 모듈에 둔다.

use super::{Store, lock, mkdir};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::Deserialize;

/// 상태 디렉터리의 `history.toml`. 생략하면 기록하며 변경은 다음 데몬 기동부터 적용한다.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    #[serde(default = "enabled")]
    pub recording: bool,
    #[serde(default)]
    pub encryption: bool,
}

const fn enabled() -> bool {
    true
}

pub(super) fn config(root: &Path) -> io::Result<Config> {
    match fs::read_to_string(root.join("history.toml")) {
        Ok(text) => toml::from_str::<Config>(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config {
            recording: true,
            encryption: false,
        }),
        Err(e) => Err(e),
    }
}

/// 앱의 저장된 일시중지 설정은 데몬 재시작 뒤에도 적용한다. 나머지 UI 설정은 무시한다.
pub(super) fn initial_pause(root: &Path) -> io::Result<bool> {
    #[derive(Deserialize, Default)]
    struct Settings {
        #[serde(default)]
        recording_paused: bool,
    }
    match fs::read_to_string(root.join("settings.toml")) {
        Ok(text) => toml::from_str::<Settings>(&text)
            .map(|s| s.recording_paused)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

pub(super) fn prepare(sessions: &Path) -> io::Result<()> {
    mkdir(sessions)?;
    #[cfg(unix)]
    let file = fs::File::open(sessions)?;
    // Windows 디렉터리 핸들은 백업 의미 플래그 없이는 열리지 않는다(os error 5 → 기록 전체가 꺼졌다, OWE-836).
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt as _;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
            .open(sessions)?
    };
    #[cfg(unix)]
    let dir = std::os::fd::AsFd::as_fd(&file);
    #[cfg(windows)]
    let dir = std::os::windows::io::AsHandle::as_handle(&file);
    crate::sys::exclude_from_backup(dir)
}

// 이름은 상태 디렉터리 안에서만 쓰며 동시에 진행되는 삭제끼리 겹치지 않는다.
static DELETED: AtomicU64 = AtomicU64::new(0);

impl Store {
    /// 모든 세그먼트만 지운다. PTY·화면·메모리 링·상태·설정은 유지하고 이후 출력부터 새 기록을 시작한다.
    /// 게이트 안에서는 rename과 세대 교체만 한다. 기존 fd는 옮긴 디렉터리를 가리키므로 늦은 쓰기가 새 이력에
    /// 나타나지 않는다. 실제 삭제는 게이트 밖이다. 보안 소거·이미 만들어진 백업 삭제는 아니다.
    pub fn clear(&self) -> io::Result<()> {
        let detached = {
            let _gate = lock(&self.gate);
            let next = self
                .generation
                .load(Ordering::Acquire)
                .checked_add(1)
                .ok_or_else(|| io::Error::other("history generation exhausted"))?;
            let detached = self.detach(&self.sessions)?;
            self.generation.store(next, Ordering::Release);
            detached
        };
        remove_detached(detached)
    }

    /// 현재 세대의 경로 하나를 떼어 내고 재귀 삭제·unlink는 게이트 밖에서 한다.
    pub(super) fn discard(&self, path: &Path, generation: u64) -> io::Result<()> {
        let detached = {
            let _gate = lock(&self.gate);
            if generation != self.generation.load(Ordering::Acquire) {
                return Ok(());
            }
            self.detach(path)?
        };
        remove_detached(detached)
    }

    /// 호출자가 gate를 쥔다. sessions의 형제로 옮겨 clear와 겹쳐도 삭제 대상은 움직이지 않는다.
    fn detach(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        let detached = loop {
            let n = DELETED.fetch_add(1, Ordering::Relaxed);
            let candidate = self
                .sessions
                .with_file_name(format!(".history-deleted-{}-{n}", std::process::id()));
            if !candidate.try_exists()? {
                break candidate;
            }
        };
        match fs::rename(path, &detached) {
            Ok(()) => Ok(Some(detached)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// 정적 경고만 남긴다. 경로·OS 에러·키·터미널 본문은 파일 내용에 넣지 않는다.
pub(super) fn warn(root: &Path) {
    use krill_proto::os::fs::OpenOptionsExt as _;
    use std::io::Write as _;
    let warning = root.join(krill_proto::history::RECORDING_WARNING_FILE);
    if let Ok(mut file) = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(warning)
    {
        file.write_all(b"recording unavailable; check history settings and keychain\n")
            .ok();
    }
}

fn remove_detached(path: Option<PathBuf>) -> io::Result<()> {
    let Some(path) = path else { return Ok(()) };
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// 크래시·실패로 남은 `.history-deleted-*`를 기동 때 치운다(K-21).
/// 이름에 pid·번호가 붙어 동시에 진행되는 삭제와 겹치지 않고, 상태 디렉터리는
/// 데몬 하나만 쓰므로 살아 있는 삭제 대상과 겹치지 않는다. 최선형 정리라 실패해도 무시한다.
pub(super) fn sweep_detached(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name
            .to_str()
            .is_some_and(|n| n.starts_with(".history-deleted-"))
        {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).ok();
        } else {
            fs::remove_file(&path).ok();
        }
    }
}
