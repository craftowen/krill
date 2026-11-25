//! `krill update`: 헤드리스 `krill-app --update-cli`에 위임해 업데이트를 점검·적용한다(MK-156).
//! Linux 원격 호스트에는 앱이 없다: `install-linux.sh`가 바이너리를 바꾼 뒤 이 명령이 돌고 있는 holder·krilld를
//! 세션을 유지한 채 설치된 새 바이너리로 교체한다(exec 핸드오프, 같은 버전이면 아무것도 안 함).
//!
//! `crates/krill`은 `#![forbid(unsafe_code)]` 및 호출당 RSS < 10MB·hook 50ms 예산을 지키기 위해
//! `krill-app`(`gpui`)이나 `ring`을 직접 의존성에 추가하지 않고 형제 실행 파일 `krill-app`을 호출한다.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: krill update [--check] [--app <path>] [--state-dir <path>] [--json]";

pub fn run(raw: &[OsString]) -> ExitCode {
    let mut app_bundle: Option<PathBuf> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut check = false;
    let mut json = false;
    let mut idx = 0;
    while idx < raw.len() {
        let arg = raw[idx].to_string_lossy();
        match arg.as_ref() {
            "-h" | "--help" | "help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--check" | "--json" => {
                if arg == "--check" {
                    check = true;
                } else {
                    json = true;
                }
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
                let Some(val) = raw.get(idx + 1) else {
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                };
                state_dir = Some(PathBuf::from(val));
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
        upgrade_daemon(state_dir.as_deref(), check, json)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (check, json, state_dir);
        delegate_to_app(raw, app_bundle.as_deref())
    }
}

/// `--check` 결과: 실행 중인 krilld가 이 빌드보다 오래됐는지만 보고한다(교체하지 않는다).
#[cfg(any(target_os = "linux", test))]
fn daemon_check_report(current: &str, running: &str, json: bool) -> String {
    let available = krill_proto::control::is_newer(current, running) == Some(true);
    if json {
        serde_json::json!({
            "installed": current,
            "running": running,
            "update_available": available,
        })
        .to_string()
    } else if available {
        format!("daemon {running} can be upgraded to {current}")
    } else {
        format!("daemon is up to date ({running})")
    }
}

#[cfg(any(target_os = "linux", test))]
fn daemon_paths(state_dir: Option<&Path>) -> std::io::Result<krill_proto::daemon::Paths> {
    match state_dir {
        Some(dir) => krill_proto::daemon::Paths::with_state_dir(dir),
        None => krill_proto::daemon::Paths::dev(),
    }
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy)]
enum DaemonUpgradeOutcome<'a> {
    Upgraded(&'a str),
    UpToDate(&'a str),
    NotRunning,
}

#[cfg(any(target_os = "linux", test))]
fn daemon_upgrade_report(outcome: DaemonUpgradeOutcome<'_>, json: bool) -> String {
    match outcome {
        DaemonUpgradeOutcome::Upgraded(build) => {
            if json {
                serde_json::json!({
                    "status": "upgraded",
                    "upgraded": true,
                    "running": build,
                    "installed": krill_version::BUILD,
                })
                .to_string()
            } else {
                format!("daemon upgraded to {build}")
            }
        }
        DaemonUpgradeOutcome::UpToDate(build) => {
            if json {
                serde_json::json!({
                    "status": "up_to_date",
                    "upgraded": false,
                    "running": build,
                    "installed": build,
                })
                .to_string()
            } else {
                format!("daemon is up to date ({build})")
            }
        }
        DaemonUpgradeOutcome::NotRunning => {
            if json {
                serde_json::json!({
                    "status": "not_running",
                    "upgraded": false,
                    "installed": krill_version::BUILD,
                })
                .to_string()
            } else {
                "daemon is not running; it will start with the installed binaries".to_owned()
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn upgrade_daemon(state_dir: Option<&Path>, check: bool, json: bool) -> ExitCode {
    use krill_proto::daemon;
    let paths = match daemon_paths(state_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("krill update: {e}");
            return ExitCode::FAILURE;
        }
    };
    let hello = daemon::client_hello(krill_proto::ipc::ClientRole::Cli, "krill-update");
    if check {
        return match daemon::handshake(&paths.socket, &hello, std::time::Duration::from_secs(5)) {
            Ok(c) => {
                println!(
                    "{}",
                    daemon_check_report(krill_version::BUILD, &c.server.build, json)
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("krill update: cannot reach the daemon: {e}");
                ExitCode::FAILURE
            }
        };
    }
    match daemon::upgrade(&paths, &hello) {
        Ok(Some(server)) => {
            println!(
                "{}",
                daemon_upgrade_report(DaemonUpgradeOutcome::Upgraded(&server.build), json)
            );
        }
        Ok(None) => {
            println!(
                "{}",
                daemon_upgrade_report(DaemonUpgradeOutcome::UpToDate(krill_version::BUILD), json)
            );
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            println!(
                "{}",
                daemon_upgrade_report(DaemonUpgradeOutcome::NotRunning, json)
            );
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
    let mut command = krill_proto::os::background_command(&exe);
    command.arg("--update-cli").args(raw);
    #[cfg(windows)]
    let status = match command.output() {
        Ok(output) => {
            use std::io::Write as _;
            std::io::stdout().write_all(&output.stdout).ok();
            std::io::stderr().write_all(&output.stderr).ok();
            output.status
        }
        Err(e) => {
            eprintln!("krill update: failed to launch {}: {e}", exe.display());
            return ExitCode::FAILURE;
        }
    };
    #[cfg(not(windows))]
    let status = match command.status() {
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
        let near = krill_proto::daemon::exe_near(bundle, "krill-app");
        if near.is_file() {
            return near;
        }
        if bundle.is_file() {
            return bundle.to_path_buf();
        }
    }
    if let Ok(sibling) = krill_proto::daemon::sibling_exe("krill-app")
        && sibling.is_file()
    {
        return sibling;
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join(r"Programs\Krill\krill-app.exe");
    }
    PathBuf::from("/Applications/Krill.app/Contents/MacOS/krill-app")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(target_os = "linux"))]
    use std::fs;
    #[cfg(all(unix, not(target_os = "linux")))]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn help_and_invalid_flags_return_expected_exit_codes() {
        assert_eq!(run(&[OsString::from("--help")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("-h")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("--unknown")]), ExitCode::from(2));
        assert_eq!(run(&[OsString::from("--app")]), ExitCode::from(2));
        assert_eq!(run(&[OsString::from("--state-dir")]), ExitCode::from(2));
    }

    #[test]
    fn check_report_never_upgrades_and_honors_json() {
        let json: serde_json::Value =
            serde_json::from_str(&daemon_check_report("0.2.10", "0.2.9", true)).unwrap();
        assert_eq!(json["update_available"], true);
        assert_eq!(json["running"], "0.2.9");
        assert!(daemon_check_report("0.2.9", "0.2.9", false).contains("up to date"));
        assert!(daemon_check_report("0.2.10", "0.2.9", false).contains("can be upgraded"));
    }

    #[test]
    fn daemon_paths_honors_custom_state_dir_and_upgrade_report_honors_json() {
        let custom = std::env::temp_dir().join(format!("krill-upd-state-{}", std::process::id()));
        let paths = daemon_paths(Some(&custom)).unwrap();
        assert_eq!(paths.socket, custom.join("run/krilld.sock"));

        let upg: serde_json::Value = serde_json::from_str(&daemon_upgrade_report(
            DaemonUpgradeOutcome::Upgraded("0.3.23"),
            true,
        ))
        .unwrap();
        assert_eq!(upg["status"], "upgraded");
        assert_eq!(upg["upgraded"], true);
        assert_eq!(upg["running"], "0.3.23");

        let cur: serde_json::Value = serde_json::from_str(&daemon_upgrade_report(
            DaemonUpgradeOutcome::UpToDate("0.3.23"),
            true,
        ))
        .unwrap();
        assert_eq!(cur["status"], "up_to_date");
        assert_eq!(cur["upgraded"], false);

        let off: serde_json::Value = serde_json::from_str(&daemon_upgrade_report(
            DaemonUpgradeOutcome::NotRunning,
            true,
        ))
        .unwrap();
        assert_eq!(off["status"], "not_running");
        assert_eq!(off["upgraded"], false);

        assert!(
            daemon_upgrade_report(DaemonUpgradeOutcome::Upgraded("0.3.23"), false)
                .contains("daemon upgraded to 0.3.23")
        );
        assert!(
            daemon_upgrade_report(DaemonUpgradeOutcome::UpToDate("0.3.23"), false)
                .contains("up to date (0.3.23)")
        );
        assert!(
            daemon_upgrade_report(DaemonUpgradeOutcome::NotRunning, false).contains("not running")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn resolve_krill_app_exe_supports_flat_dir_and_direct_file() {
        let dir =
            std::env::temp_dir().join(format!("krill-update-flat-test-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let flat_exe = dir.join(format!("krill-app{}", std::env::consts::EXE_SUFFIX));
        fs::write(&flat_exe, b"stub").unwrap();

        assert_eq!(resolve_krill_app_exe(Some(&dir)), flat_exe);
        assert_eq!(resolve_krill_app_exe(Some(&flat_exe)), flat_exe);
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(all(unix, not(target_os = "linux")))]
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
