//! 플랫폼별 수신 서버 시작·종료와 사용자가 선택한 접속 허용 상태.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use krill_proto::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};

use crate::{Error, is_serve_running};

const PREFERENCE: &str = "link/incoming.enabled";
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// 저장된 선택이 없으면 수신 접속을 허용한다. 읽기 오류는 켜기로 간주하지 않는다.
pub fn incoming_enabled(state_dir: &Path) -> Result<bool, Error> {
    match fs::read_to_string(state_dir.join(PREFERENCE)) {
        Ok(value) => value
            .trim()
            .parse()
            .map_err(|_| Error::InvalidConfig("invalid incoming access preference".to_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(Error::Io(e)),
    }
}

fn save_preference(state_dir: &Path, enabled: bool) -> Result<(), Error> {
    crate::store::write_atomic_0600(
        &state_dir.join(PREFERENCE),
        if enabled { b"true" } else { b"false" },
    )
    .map_err(Error::Io)
}

fn lock(state_dir: &Path) -> Result<File, Error> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state_dir.join("link"))
        .map_err(Error::Io)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(state_dir.join("link/service.lock"))
        .map_err(Error::Io)?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(Error::Timeout("incoming access lock"));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(Error::Io(e)),
        }
    }
}

/// 앱 시작 시 기본으로 수신 서버를 시작한다. 명시적으로 끈 설정은 유지한다.
pub fn ensure_default_server(executable: &Path, state_dir: &Path) -> Result<(), Error> {
    let _lock = lock(state_dir)?;
    if incoming_enabled(state_dir)? {
        start(executable, state_dir)?;
    }
    Ok(())
}

/// 수신 서버를 시작하고 켜짐 선택을 저장한다. 이미 실행 중이면 재시작하지 않는다.
pub fn enable_server(executable: &Path, state_dir: &Path) -> Result<(), Error> {
    let _lock = lock(state_dir)?;
    start(executable, state_dir)?;
    save_preference(state_dir, true)
}

/// 수신 서버만 종료하고 꺼짐 선택을 저장한다. 로컬 데몬과 세션은 건드리지 않는다.
pub fn disable_server(state_dir: &Path) -> Result<(), Error> {
    let _lock = lock(state_dir)?;
    // 앱 시작과 경합해도 사용자의 끄기 선택이 우선한다.
    save_preference(state_dir, false)?;
    #[cfg(target_os = "macos")]
    crate::disable_launch_agent(state_dir)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Io)?;
    runtime.block_on(crate::serve::shutdown_server_command(state_dir))
}

fn start(executable: &Path, state_dir: &Path) -> Result<(), Error> {
    if is_serve_running(state_dir) {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        crate::enable_launch_agent(executable, state_dir)?;
        // 테스트 훅으로 launchctl을 건너뛰면 서버가 뜨지 않으므로 기다리지 않는다.
        if std::env::var_os("KRILL_LINK_SKIP_LAUNCHCTL").is_some() {
            return Ok(());
        }
    }
    #[cfg(windows)]
    let mut child = {
        use std::os::windows::process::CommandExt as _;
        use std::process::{Command, Stdio};
        Command::new(executable)
            .arg("serve")
            .arg("--state-dir")
            .arg(state_dir)
            // 콘솔 없이 앱과 독립된 프로세스로 실행한다.
            .creation_flags(0x0000_0008 | 0x0000_0200)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(Error::Io)?
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = executable;
        Err(crate::unsupported_platform())
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        let deadline = Instant::now() + START_TIMEOUT;
        while !is_serve_running(state_dir) {
            #[cfg(windows)]
            if let Some(status) = child.try_wait().map_err(Error::Io)? {
                return Err(Error::Transport(format!(
                    "incoming server exited: {status}"
                )));
            }
            if Instant::now() >= deadline {
                #[cfg(windows)]
                {
                    child.kill().ok();
                    child.wait().ok();
                }
                return Err(Error::Timeout("incoming server startup"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incoming_defaults_on_and_preserves_explicit_choice() {
        let dir = std::env::temp_dir().join(format!("kl-pref-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        assert!(incoming_enabled(&dir).unwrap());
        disable_server(&dir).unwrap();
        assert!(!incoming_enabled(&dir).unwrap());
        // 꺼져 있으면 존재하지 않는 실행 파일로도 자동 시작하지 않는다.
        ensure_default_server(Path::new("missing-krill-link"), &dir).unwrap();
        save_preference(&dir, true).unwrap();
        assert!(incoming_enabled(&dir).unwrap());
        fs::write(dir.join(PREFERENCE), "invalid").unwrap();
        assert!(incoming_enabled(&dir).is_err());
        fs::remove_dir_all(dir).ok();
    }
}
