//! 프로필별 이행 전 읽기 전용 프로세스 조회. 호출은 백그라운드 작업에서만 한다.

use std::io::{self, Read as _};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const LIMIT: u64 = 16 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(2);

pub(super) fn quiescent(profile: &Path, env_key: &str) -> bool {
    let mut ps = Command::new("/bin/ps");
    ps.args([
        "eww",
        "-U",
        &crate::os::euid().to_string(),
        "-o",
        "command=",
    ]);
    let Ok(output) = capture(&mut ps, TIMEOUT) else {
        return false;
    };
    if !output.status.success()
        || !output.stderr.is_empty()
        || !String::from_utf8(output.stdout).is_ok_and(|text| !has_writer(&text, profile, env_key))
    {
        return false;
    }
    #[cfg(target_os = "macos")]
    let lsof = "/usr/sbin/lsof";
    #[cfg(not(target_os = "macos"))]
    let lsof = "lsof";
    // +D는 디렉터리 아래 열린 fd/cwd를 조회한다. 링크 대상은 따라가지 않는다.
    let mut command = Command::new(lsof);
    command.args(["-t", "+D"]).arg(profile);
    capture(&mut command, TIMEOUT)
        .is_ok_and(|result| no_open_files(result.status.code(), &result.stdout, &result.stderr))
}

pub(super) fn no_open_files(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> bool {
    // lsof는 매칭 파일이 없으면 1이다. 경고·실패·부분 조회는 해당 프로필만 보류한다.
    code == Some(1) && stdout.is_empty() && stderr.is_empty()
}

pub(super) fn has_writer(text: &str, profile: &Path, env_key: &str) -> bool {
    let marker = format!("{env_key}={}", profile.display());
    let canonical = profile
        .canonicalize()
        .ok()
        .map(|path| format!("{env_key}={}", path.display()));
    // ps eww는 폭 제한을 해제한다. 경로에 공백이 있거나 경계가 불분명해도 보수적으로 보류한다.
    text.contains(&marker)
        || canonical.is_some_and(|marker| text.contains(&marker))
        || text.lines().any(|line| {
            let executable = line.split_whitespace().next().unwrap_or_default();
            let claude = Path::new(executable)
                .file_name()
                .is_some_and(|name| name == "claude")
                || line.contains("@anthropic-ai/claude-code")
                || line.contains("/claude/cli.js");
            // macOS는 보호 프로세스의 환경을 경고 없이 생략할 수 있다.
            // Claude 후보의 환경이 보이지 않으면 다른 계정이라고 단정하지 않는다.
            claude && !line.contains(" HOME=") && !line.contains(" PATH=")
        })
}

fn read_pipe(pipe: impl io::Read + Send + 'static) -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe.take(LIMIT + 1).read_to_end(&mut bytes).and_then(|_| {
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > LIMIT {
                Err(io::ErrorKind::FileTooLarge.into())
            } else {
                Ok(bytes)
            }
        });
        tx.send(result).ok();
    });
    rx
}

/// 자체 조회 자식만 종료·회수한다. Claude·Krill·Orca 프로세스에는 신호를 보내지 않는다.
fn capture(command: &mut Command, timeout: Duration) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = read_pipe(child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?);
    let stderr = read_pipe(child.stderr.take().ok_or(io::ErrorKind::BrokenPipe)?);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                child.kill().ok();
                child.wait().ok();
                return Err(result
                    .err()
                    .unwrap_or_else(|| io::ErrorKind::TimedOut.into()));
            }
        }
    };
    let receive = |rx: mpsc::Receiver<io::Result<Vec<u8>>>| {
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(io::Error::other)?
    };
    Ok(Output {
        status,
        stdout: receive(stdout)?,
        stderr: receive(stderr)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_probe_times_out_and_reaps_only_its_own_child() {
        let mut command = Command::new("/bin/sleep");
        command.arg("5");
        let started = Instant::now();
        assert_eq!(
            capture(&mut command, Duration::from_millis(50))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
