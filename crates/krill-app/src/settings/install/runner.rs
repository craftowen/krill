//! 설치 전용 실행기: 공유 관리자 직렬화, 끝부분 로그, 실제 종료 코드, 전체 시간 제한.
use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::InstallJobResult;

static INSTALL_LOCK: Mutex<()> = Mutex::new(());
const TIMEOUT: Duration = Duration::from_mins(15);

pub(super) fn powershell_command(script: &str) -> Command {
    let mut cmd = crate::platform::background_command("powershell.exe");
    cmd.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        &format!("[Console]::OutputEncoding=[Text.UTF8Encoding]::new(); {script}"),
    ]);
    cmd
}

pub(super) fn version_command(exe: &Path, args: &[&str]) -> Command {
    if cfg!(windows)
        && exe.extension().is_some_and(|s| {
            s.eq_ignore_ascii_case("cmd")
                || s.eq_ignore_ascii_case("bat")
                || s.eq_ignore_ascii_case("ps1")
        })
    {
        let quote = |s: &str| format!("'{}'", s.replace('\u{27}', "''"));
        let exe = quote(&exe.to_string_lossy());
        let args = args.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ");
        powershell_command(&format!("& {exe} {args}; exit $LASTEXITCODE"))
    } else {
        let mut cmd = crate::platform::background_command(exe);
        cmd.args(args);
        cmd
    }
}

fn powershell_script(command: &str) -> String {
    // WinGet의 '적용할 업데이트 없음' HRESULT(0x8A15002B)만 정상 종료로 정규화한다.
    let no_update = if command.starts_with("winget ") {
        "if ($LASTEXITCODE -eq -1978335189) { exit 0 }; "
    } else {
        ""
    };
    format!(
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new(); $global:LASTEXITCODE=0; & {{ {command} }}; {no_update}exit $LASTEXITCODE"
    )
}

pub(super) fn run(command: &str, home: &Path, path: &OsStr) -> InstallJobResult {
    // 다른 설정 창에서 요청해도 Homebrew/WinGet/npm을 동시에 실행하지 않는다.
    let _guard = INSTALL_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut cmd = if cfg!(windows) {
        powershell_command(&powershell_script(command))
    } else {
        // 로그인 rc가 검색 PATH를 되돌리거나 출력에 섞이지 않도록 고정 셸을 쓴다.
        let mut cmd = crate::platform::background_command("/bin/bash");
        cmd.args(["-o", "pipefail", "-c", command]);
        let user_shell = std::env::var_os("SHELL")
            .filter(|shell| !shell.is_empty())
            .unwrap_or_else(|| "/bin/zsh".into());
        cmd.env("SHELL", user_shell);
        cmd
    };
    cmd.env("PATH", path)
        .env("HOME", home)
        .env("NONINTERACTIVE", "1")
        .env("HOMEBREW_NO_ENV_HINTS", "1")
        .env("CI", "1")
        .current_dir(home);
    match capture(&mut cmd, TIMEOUT) {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let output = format!("{}\n{}", stdout.trim(), stderr.trim());
            let output = output.trim();
            InstallJobResult {
                success: out.status.success(),
                output: if output.is_empty() {
                    out.status.to_string()
                } else {
                    super::tail_lines(output, 40)
                },
            }
        }
        Err(error) => InstallJobResult {
            success: false,
            output: error.to_string(),
        },
    }
}

fn terminate(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = rustix::process::Pid::from_raw(child.id().cast_signed()) {
        rustix::process::kill_process_group(pid, rustix::process::Signal::KILL).ok();
    }
    #[cfg(windows)]
    {
        crate::platform::background_command("taskkill.exe")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut task| {
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    if task.try_wait()?.is_some() {
                        return Ok(());
                    }
                    if Instant::now() >= deadline {
                        task.kill().ok();
                        task.wait().ok();
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
            .ok();
    }
    child.kill().ok();
    child.wait().ok();
}

fn capture(cmd: &mut Command, timeout: Duration) -> io::Result<Output> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let deadline = Instant::now() + timeout;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let result = (|| {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing installer stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("missing installer stderr"))?;
        let out = crate::process_io::read_tail(stdout, 64 * 1024, deadline)?;
        let err = crate::process_io::read_tail(stderr, 64 * 1024, deadline)?;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "installer timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let stdout = out.finish(deadline)?;
        let stderr = err.finish(deadline)?;
        if stdout.timed_out || stderr.timed_out {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "installer output timed out",
            ));
        }
        Ok(Output {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        })
    })();
    if result.is_err() {
        terminate(&mut child);
    }
    result
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn verbose_installers_keep_the_tail_and_real_exit_status() {
        let out = capture(Command::new("/bin/sh").args(["-c", "head -c 300000 /dev/zero; printf stdout-end; head -c 300000 /dev/zero >&2; printf stderr-end >&2; exit 7"]), Duration::from_secs(5)).unwrap();
        assert_eq!(out.status.code(), Some(7));
        assert_eq!(out.stdout.len(), 65536);
        assert_eq!(out.stderr.len(), 65536);
        assert!(out.stdout.ends_with(b"stdout-end"));
        assert!(out.stderr.ends_with(b"stderr-end"));
    }
    #[test]
    fn download_failure_is_not_hidden_by_successful_shell() {
        let home = std::env::temp_dir();
        let result = run("false | bash", &home, OsStr::new("/bin:/usr/bin"));
        assert!(!result.success);
    }
    #[test]
    fn timeout_closes_descendant_pipes() {
        let start = Instant::now();
        let result = capture(
            Command::new("/bin/sh").args(["-c", "sleep 60 & wait"]),
            Duration::from_millis(80),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;
    #[test]
    fn only_winget_no_update_is_normalized() {
        assert!(powershell_script("winget install --id Git.Git").contains("-1978335189"));
        assert!(!powershell_script("npm.cmd install -g pnpm").contains("-1978335189"));
        assert!(powershell_script("cmd /c exit 23").ends_with("exit $LASTEXITCODE"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_native_exit_and_powershell_errors_propagate() {
        let home = std::env::temp_dir();
        let path = std::env::var_os("PATH").unwrap();
        assert!(!run("cmd.exe /c exit 23", &home, &path).success);
        assert!(!run("Write-Error 'test failure'", &home, &path).success);
        assert!(run("Write-Output 'test success'", &home, &path).success);
    }

    #[cfg(unix)]
    #[test]
    fn simultaneous_install_requests_cannot_overlap() {
        let home =
            std::env::temp_dir().join(format!("krill-install-serial-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                run(
                    "mkdir exclusive || exit 99; sleep 0.1; rmdir exclusive",
                    &home,
                    OsStr::new("/bin:/usr/bin"),
                )
            });
            let second = scope.spawn(|| {
                run(
                    "mkdir exclusive || exit 99; sleep 0.1; rmdir exclusive",
                    &home,
                    OsStr::new("/bin:/usr/bin"),
                )
            });
            assert!(first.join().unwrap().success);
            assert!(second.join().unwrap().success);
        });
        std::fs::remove_dir_all(home).unwrap();
    }
}
