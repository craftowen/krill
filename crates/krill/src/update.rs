//! `krill update`: 헤드리스 `krill-app --update-cli`에 위임해 업데이트를 점검·적용한다(MK-156).
//! Linux 원격 호스트에는 앱이 없다: `install-linux.sh`가 바이너리를 바꾼 뒤 이 명령이 돌고 있는 holder·krilld를
//! 세션을 유지한 채 설치된 새 바이너리로 교체한다(exec 핸드오프, 같은 버전이면 아무것도 안 함).
//!
//! `crates/krill`은 `#![forbid(unsafe_code)]` 및 호출당 RSS < 10MB·hook 50ms 예산을 지키기 위해
//! `krill-app`(`gpui`)이나 `ring`을 직접 의존성에 추가하지 않고 형제 실행 파일 `krill-app`을 호출한다.

use std::ffi::OsString;
#[cfg(not(target_os = "linux"))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(not(target_os = "linux"))]
use std::process::Command;
use std::process::ExitCode;

const USAGE: &str = "usage: krill update [--check] [--app <path>] [--state-dir <path>] [--json]";

pub fn run(raw: &[OsString]) -> ExitCode {
    let mut app_bundle: Option<PathBuf> = None;
    let mut idx = 0;
    while idx < raw.len() {
        let arg = raw[idx].to_string_lossy();
        match arg.as_ref() {
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--check" | "--json" => {
                idx += 1;
            }
            "--app" => {
                let Some(val) = raw.get(idx + 1) else {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                };
                app_bundle = Some(PathBuf::from(val));
                idx += 2;
            }
            "--state-dir" => {
                if raw.get(idx + 1).is_none() {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
                idx += 2;
            }
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let _ = app_bundle;
        upgrade_daemon()
    }
    #[cfg(not(target_os = "linux"))]
    delegate_to_app(raw, app_bundle.as_deref())
}

#[cfg(target_os = "linux")]
fn upgrade_daemon() -> ExitCode {
    use krill_proto::daemon;
    let paths = match daemon::Paths::dev() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("krill update: {e}");
            return ExitCode::FAILURE;
        }
    };
    let hello = daemon::client_hello(krill_proto::ipc::ClientRole::Cli, "krill-update");
    match daemon::upgrade(&paths, &hello) {
        Ok(Some(server)) => println!("daemon upgraded to {}", server.build),
        Ok(None) => println!("daemon is up to date ({})", krill_proto::BUILD),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            println!("daemon is not running; it will start with the installed binaries");
        }
        Err(e) => {
            eprintln!("krill update: daemon upgrade failed: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

#[cfg(not(target_os = "linux"))]
fn delegate_to_app(raw: &[OsString], app_bundle: Option<&Path>) -> ExitCode {
    let exe = resolve_krill_app_exe(app_bundle);
    let status = match Command::new(&exe).arg("--update-cli").args(raw).status() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("krill update: failed to launch {}: {e}", exe.display());
            return ExitCode::FAILURE;
        }
    };
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(
            status
                .code()
                .and_then(|c| u8::try_from(c).ok())
                .unwrap_or(1),
        )
    }
}

#[cfg(not(target_os = "linux"))]
fn resolve_krill_app_exe(app_bundle: Option<&Path>) -> PathBuf {
    if let Some(bundle) = app_bundle {
        let candidate = bundle.join("Contents/MacOS/krill-app");
        if candidate.is_file() {
            return candidate;
        }
    }
    if let Ok(sibling) = krill_proto::daemon::sibling_exe("krill-app")
        && sibling.is_file()
    {
        return sibling;
    }
    PathBuf::from("/Applications/Krill.app/Contents/MacOS/krill-app")
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    #[cfg(not(target_os = "linux"))]
    use std::fs;
    #[cfg(not(target_os = "linux"))]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn help_and_invalid_flags_return_expected_exit_codes() {
        assert_eq!(run(&[OsString::from("--help")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("-h")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("--unknown")]), ExitCode::from(2));
        assert_eq!(run(&[OsString::from("--app")]), ExitCode::from(2));
        assert_eq!(run(&[OsString::from("--state-dir")]), ExitCode::from(2));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn delegates_to_bundle_krill_app_with_update_cli_arg() {
        let dir =
            std::env::temp_dir().join(format!("krill-update-cli-test-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        let macos = dir.join("Krill.app/Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        let marker = dir.join("invoked.txt");
        let exe = macos.join("krill-app");
        fs::write(
            &exe,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\nexit 0\n",
                marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();

        let bundle = dir.join("Krill.app");
        assert_eq!(resolve_krill_app_exe(Some(&bundle)), exe);
        let code = run(&[
            OsString::from("--check"),
            OsString::from("--json"),
            OsString::from("--app"),
            bundle.into_os_string(),
        ]);
        assert_eq!(code, ExitCode::SUCCESS);
        let logged = fs::read_to_string(&marker).unwrap();
        assert!(logged.contains("--update-cli --check --json --app"));
        fs::remove_dir_all(&dir).ok();
    }
}
