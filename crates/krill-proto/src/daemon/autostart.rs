//! 로그인·부팅 시 `krill-holder` + `krilld` 자동 기동 등록(macOS `LaunchAgent`, Windows `Run`).
//!
//! - macOS: `~/Library/LaunchAgents/dev.krill.app.holder.plist`(`RunAtLoad=true`, `KeepAlive=false`,
//!   CONVENTIONS §9 A1~A3)를 원자적으로 동기화한다. 이미 건강한 데몬이 떠 있을 때는 plist만 갱신하고
//!   `launchctl bootout`은 절대 호출하지 않는다(`SIGTERM`으로 라이브 세션이 죽는다). 콜드 스타트 시에는
//!   기동 락 아래에서 `launchctl bootstrap gui/<uid>`로 띄우고, GUI 도메인이 없으면 `--detach`로 폴백한다.
//! - Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`의 `dev.krill.app.holder` 값에
//!   `"<holder>" --bootstrap "<krilld>" "<socket>"`을 등록하고 `<state>/run/autostart.cmd` 영수증으로
//!   매 연결마다 하위 프로세스를 띄우지 않게 캐시한다.
//! - 테스트·Cargo 빌드 디렉터리·격리 상태 디렉터리·임시 `HOME`·DMG(`/Volumes`)·`Downloads`에서는
//!   호스트의 실제 자동 시작 항목을 건드리지 않는다.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
#[cfg(any(target_os = "macos", windows))]
use std::process::Stdio;
#[cfg(target_os = "macos")]
use std::sync::OnceLock;

use super::{HOLDER_LABEL, Paths};

const SKIP_ENV: &str = "KRILL_DAEMON_SKIP_AUTOSTART";
#[cfg(windows)]
const WINDOWS_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const WINDOWS_RECEIPT_REL: &str = "run/autostart.cmd";

/// 조건을 만족하는 정식 설치 경로일 때만 로그인 자동 시작 구성을 최신 상태로 맞춘다.
/// 실패해도 데몬 연결 자체는 막지 않는다.
pub fn ensure_autostart(paths: &Paths) {
    #[cfg(not(test))]
    {
        static VERIFIED: std::sync::Mutex<Option<Paths>> = std::sync::Mutex::new(None);
        if VERIFIED
            .lock()
            .is_ok_and(|cached| cached.as_ref() == Some(paths))
        {
            return;
        }
        if sync_autostart(paths)
            && let Ok(mut cached) = VERIFIED.lock()
        {
            *cached = Some(paths.clone());
        }
    }
    #[cfg(test)]
    {
        let _ = sync_autostart(paths);
    }
}

fn sync_autostart(paths: &Paths) -> bool {
    if !allowed(paths) {
        return true;
    }
    #[cfg(target_os = "macos")]
    if let (Some(plist_path), Some(expected)) = (macos_plist_path(), render_macos_plist(paths)) {
        return sync_plist_file(&plist_path, &expected).is_ok();
    }
    #[cfg(windows)]
    if let (Some(state), Some(expected)) = (
        super::default_state_dir(),
        windows_run_command(&paths.holder, &paths.krilld, &paths.socket),
    ) {
        let receipt = state.join(WINDOWS_RECEIPT_REL);
        return sync_windows_run_in(WINDOWS_RUN_KEY, HOLDER_LABEL, &receipt, &expected).is_ok();
    }
    true
}

/// `krill doctor` 등에서 현재 자동 시작 구성이 등록돼 있는지 확인한다.
#[must_use]
pub fn is_autostart_registered() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos_plist_path().is_some_and(|p| p.is_file())
    }
    #[cfg(windows)]
    {
        super::default_state_dir().is_some_and(|d| d.join(WINDOWS_RECEIPT_REL).is_file())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        false
    }
}

/// macOS 콜드 스타트: `LaunchAgent` plist를 동기화하고 `launchctl bootstrap gui/<uid>`로 띄운다.
/// 성공하면 `true`(호출자는 소켓 준비만 기다린다). 비정식 경로이거나 `gui/<uid>` 도메인이 없으면 `false`를
/// 돌려주어 호출자가 기존 `--detach` 직접 기동으로 폴백하게 한다.
#[cfg(target_os = "macos")]
pub(super) fn try_bootstrap(paths: &Paths) -> bool {
    if !allowed(paths) {
        return false;
    }
    let (Some(plist_path), Some(expected)) = (macos_plist_path(), render_macos_plist(paths)) else {
        return false;
    };
    if sync_plist_file(&plist_path, &expected).is_err() {
        return false;
    }
    let uid = rustix::process::getuid().as_raw();
    bootstrap_with(&plist_path, uid, run_launchctl)
}

/// macOS `bootstrap` 직후 소켓 준비에 실패했을 때 잡을 내려 고아 재기동 루프를 막는다(A-05).
#[cfg(target_os = "macos")]
pub(super) fn bootout() {
    let service = format!("gui/{}/{HOLDER_LABEL}", rustix::process::getuid().as_raw());
    run_launchctl(&["bootout".as_ref(), OsStr::new(&service)]);
}

#[cfg(any(target_os = "macos", test))]
fn bootstrap_with(
    plist_path: &Path,
    uid: u32,
    mut launchctl: impl FnMut(&[&OsStr]) -> bool,
) -> bool {
    let domain = format!("gui/{uid}");
    let service = format!("{domain}/{HOLDER_LABEL}");
    // 이미 종료된 이전 잡 레코드가 남아 있으면 bootstrap이 EIO/EALREADY로 실패하므로 먼저 정리한다.
    // 호출자는 `<socket>.launch` 락 아래에서 건강한 데몬이 없음을 확인한 상태다.
    launchctl(&[OsStr::new("bootout"), OsStr::new(&service)]);
    launchctl(&[
        OsStr::new("bootstrap"),
        OsStr::new(&domain),
        plist_path.as_os_str(),
    ])
}

#[cfg(target_os = "macos")]
fn run_launchctl(args: &[&OsStr]) -> bool {
    Command::new("/bin/launchctl")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(target_os = "macos")]
fn macos_plist_path() -> Option<PathBuf> {
    crate::os::home_dir().map(|home| {
        home.join("Library/LaunchAgents")
            .join(format!("{HOLDER_LABEL}.plist"))
    })
}

#[cfg(target_os = "macos")]
fn render_macos_plist(paths: &Paths) -> Option<String> {
    let holder = paths.holder.to_str()?;
    let krilld = paths.krilld.to_str()?;
    let socket = paths.socket.to_str()?;
    if [holder, krilld, socket]
        .iter()
        .any(|s| s.contains(['\0', '\r', '\n']))
    {
        return None;
    }
    Some(super::holder_plist(holder, krilld, socket))
}

#[cfg(any(target_os = "macos", test))]
fn sync_plist_file(plist_path: &Path, expected: &str) -> io::Result<bool> {
    if fs::read_to_string(plist_path).ok().as_deref() == Some(expected) {
        return Ok(false);
    }
    if let Some(parent) = plist_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        // `~/Library/LaunchAgents`가 아직 없을 때 macOS 표준(0755, umask 반영)으로 먼저 만든다.
        fs::create_dir_all(parent)?;
    }
    crate::os::write_atomic(plist_path, expected.as_bytes(), 0o644)?;
    Ok(true)
}

fn allowed(paths: &Paths) -> bool {
    if cfg!(test) || super::state_dir_isolated() || skip_env_set() {
        return false;
    }
    if !cfg!(any(target_os = "macos", windows)) {
        return false;
    }
    let Some(state) = super::default_state_dir() else {
        return false;
    };
    if super::socket_path(&state).ok().as_deref() != Some(paths.socket.as_path()) {
        return false;
    }
    let Some(home) = crate::os::home_dir() else {
        return false;
    };
    if super::is_temp_home(&home) || !verified_os_home(&home) {
        return false;
    }
    let temp = std::env::temp_dir();
    stable_executable(&paths.holder, &temp) && stable_executable(&paths.krilld, &temp)
}

fn skip_env_set() -> bool {
    if std::env::var_os(SKIP_ENV).is_some_and(|v| !v.is_empty() && v != OsStr::new("0")) {
        return true;
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
            return true;
        }
    }
    false
}

#[cfg(target_os = "macos")]
fn verified_os_home(home: &Path) -> bool {
    static ACCOUNT_HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    let account = ACCOUNT_HOME.get_or_init(|| {
        let uid = rustix::process::getuid().as_raw().to_string();
        let out = Command::new("/usr/bin/dscacheutil")
            .args(["-q", "user", "-a", "uid", &uid])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("dir: ").map(PathBuf::from))
            .and_then(|dir| dir.canonicalize().ok())
    });
    match (home.canonicalize().ok(), account.as_ref()) {
        (Some(actual), Some(expected)) => &actual == expected,
        _ => false,
    }
}

#[cfg(windows)]
fn verified_os_home(home: &Path) -> bool {
    let temp = std::env::temp_dir();
    !home.starts_with(&temp)
        && std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .is_some_and(|local| !Path::new(&local).starts_with(&temp))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn verified_os_home(_home: &Path) -> bool {
    false
}

fn stable_executable(path: &Path, temp: &Path) -> bool {
    if !path.is_absolute() || !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let Ok(meta) = fs::metadata(path) else {
            return false;
        };
        if meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    let Ok(resolved) = crate::os::canonicalize(path) else {
        return false;
    };
    is_stable_install_path(path, temp) && is_stable_install_path(&resolved, temp)
}

/// 빌드 산출물(`target/debug`·`target/release`·`targets/*/release`·`deps`), 임시 폴더, 마운트 볼륨(`/Volumes`),
/// `Downloads` 폴더의 실행 파일은 재부팅 뒤 사라지거나 개발용이므로 자동 시작 등록 대상에서 제외한다.
fn is_stable_install_path(path: &Path, temp: &Path) -> bool {
    if !path.is_absolute() || path.to_str().is_none() {
        return false;
    }
    if path.starts_with(temp)
        || path.starts_with("/tmp")
        || path.starts_with("/private/tmp")
        || path.starts_with("/var/folders")
        || path.starts_with("/private/var/folders")
    {
        return false;
    }
    #[cfg(target_os = "macos")]
    if path.starts_with("/Volumes") {
        return false;
    }
    let names: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    // 마지막 요소는 실행 파일 이름이므로 디렉터리 경로만 검사한다.
    let Some((_exe, dirs)) = names.split_last() else {
        return false;
    };
    if dirs.iter().any(|d| d == "downloads") {
        return false;
    }
    if let Some(parent) = dirs.last() {
        if parent == "deps" || parent == "debug" {
            return false;
        }
        if parent == "release" && dirs.iter().any(|d| d == "target" || d == "targets") {
            return false;
        }
    }
    true
}

#[cfg(any(windows, test))]
fn windows_run_command(holder: &Path, krilld: &Path, socket: &Path) -> Option<String> {
    let holder = holder.to_str()?;
    let krilld = krilld.to_str()?;
    let socket = socket.to_str()?;
    if [holder, krilld, socket]
        .iter()
        .any(|s| s.is_empty() || s.contains(['\0', '\r', '\n']))
    {
        return None;
    }
    Some(format!(
        "{} --bootstrap {} {}",
        quote_windows_arg(holder, true),
        quote_windows_arg(krilld, true),
        quote_windows_arg(socket, true),
    ))
}

/// Win32 `CommandLineToArgvW` 규칙에 맞춘 인자 인용. `Run` 레지스트리 경로 모호성을 막기 위해
/// 실행 파일·소켓 경로는 공백이 없어도 항상 큰따옴표로 감싼다(`always_quote = true`).
#[cfg(any(windows, test))]
fn quote_windows_arg(arg: &str, always_quote: bool) -> String {
    let needs_quotes =
        always_quote || arg.is_empty() || arg.chars().any(|c| matches!(c, ' ' | '\t' | '"'));
    if !needs_quotes {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len().saturating_add(2));
    out.push('"');
    let mut backslashes: usize = 0;
    for ch in arg.chars() {
        if ch == '\\' {
            backslashes = backslashes.saturating_add(1);
        } else if ch == '"' {
            for _ in 0..backslashes.saturating_mul(2).saturating_add(1) {
                out.push('\\');
            }
            out.push('"');
            backslashes = 0;
        } else {
            for _ in 0..backslashes {
                out.push('\\');
            }
            backslashes = 0;
            out.push(ch);
        }
    }
    for _ in 0..backslashes.saturating_mul(2) {
        out.push('\\');
    }
    out.push('"');
    out
}

#[cfg(windows)]
fn reg_exe() -> PathBuf {
    std::env::var_os("SystemRoot")
        .filter(|v| !v.is_empty())
        .map_or_else(
            || PathBuf::from(r"C:\Windows\System32\reg.exe"),
            |root| PathBuf::from(root).join(r"System32\reg.exe"),
        )
}

#[cfg(windows)]
fn sync_windows_run_in(
    key: &str,
    value_name: &str,
    receipt_path: &Path,
    expected: &str,
) -> io::Result<bool> {
    if fs::read_to_string(receipt_path).ok().as_deref() == Some(expected) {
        return Ok(false);
    }
    let status = crate::os::background_command(reg_exe())
        .args([
            "add", key, "/v", value_name, "/t", "REG_SZ", "/d", expected, "/f",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("reg.exe add failed ({status})")));
    }
    crate::os::write_atomic_0600(receipt_path, expected.as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn stable_install_path_accepts_installed_locations_and_rejects_ephemeral_builds() {
        let temp = Path::new("/tmp/custom-temp");
        assert!(is_stable_install_path(
            Path::new("/Applications/Krill.app/Contents/Helpers/krill-holder"),
            temp,
        ));
        assert!(is_stable_install_path(
            Path::new("/Users/alice/Applications/Krill.app/Contents/Helpers/krilld"),
            temp,
        ));
        assert!(is_stable_install_path(
            Path::new("/Users/alice/.local/bin/krill-holder"),
            temp,
        ));
        assert!(is_stable_install_path(
            Path::new("/Users/alice/.local/share/krill/bin/krill-holder"),
            temp,
        ));
        for rejected in [
            "/Volumes/krill-build/targets/boot-autostart/release/krill-holder",
            "/Volumes/Krill/Krill.app/Contents/Helpers/krill-holder",
            "/Users/alice/wProject/krill/target/release/krill-holder",
            "/Users/alice/wProject/krill/target/aarch64-apple-darwin/release/krill-holder",
            "/Users/alice/wProject/krill/target/debug/krill-holder",
            "/Users/alice/wProject/krill/target/debug/deps/krill-holder-1234",
            "/Users/alice/Downloads/Krill.app/Contents/Helpers/krill-holder",
            "/tmp/krill-install/bin/krill-holder",
            "/private/tmp/krill/krill-holder",
            "/var/folders/xx/yy/T/AppTranslocation/Krill.app/Contents/Helpers/krill-holder",
            "relative/bin/krill-holder",
        ] {
            #[cfg(not(target_os = "macos"))]
            if rejected.starts_with("/Volumes/") && !rejected.contains("/targets/") {
                continue;
            }
            assert!(
                !is_stable_install_path(Path::new(rejected), temp),
                "expected ephemeral/build path to be rejected: {rejected}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn stable_install_path_accepts_windows_programs_and_rejects_temp_or_target() {
        let temp = Path::new(r"C:\Users\홍 길동\AppData\Local\Temp");
        assert!(is_stable_install_path(
            Path::new(r"C:\Users\홍 길동\AppData\Local\Programs\Krill\krill-holder.exe"),
            temp,
        ));
        for rejected in [
            r"C:\Users\홍 길동\AppData\Local\Temp\krill-test\krill-holder.exe",
            r"C:\Users\홍 길동\Downloads\Krill\krill-holder.exe",
            r"C:\work\krill\target\release\krill-holder.exe",
            r"C:\work\krill\target\x86_64-pc-windows-msvc\release\krill-holder.exe",
            r"C:\work\krill\target\debug\krill-holder.exe",
        ] {
            assert!(
                !is_stable_install_path(Path::new(rejected), temp),
                "expected Windows path to be rejected: {rejected}"
            );
        }
    }

    #[test]
    fn windows_run_command_quotes_paths_and_preserves_unicode() {
        let cmd = windows_run_command(
            Path::new(r"C:\Users\홍 길동\AppData\Local\Programs\Krill\krill-holder.exe"),
            Path::new(r"C:\Users\홍 길동\AppData\Local\Programs\Krill\krilld.exe"),
            Path::new(r"C:\Users\홍 길동\AppData\Local\Krill\run\krilld.sock"),
        )
        .unwrap();
        assert_eq!(
            cmd,
            r#""C:\Users\홍 길동\AppData\Local\Programs\Krill\krill-holder.exe" --bootstrap "C:\Users\홍 길동\AppData\Local\Programs\Krill\krilld.exe" "C:\Users\홍 길동\AppData\Local\Krill\run\krilld.sock""#
        );
        assert_eq!(quote_windows_arg(r#"a"b\"#, true), r#""a\"b\\""#);
        assert!(
            windows_run_command(
                Path::new("a\nb"),
                Path::new("krilld.exe"),
                Path::new("krilld.sock"),
            )
            .is_none()
        );
    }

    #[test]
    fn sync_plist_writes_once_and_updates_when_changed() {
        let root =
            std::env::temp_dir().join(format!("krill-autostart-plist-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let plist_path = root.join("Library/LaunchAgents/dev.krill.app.holder.plist");
        let v1 = super::super::holder_plist(
            "/Users/me/Applications/Krill.app/Contents/Helpers/krill-holder",
            "/Users/me/Applications/Krill.app/Contents/Helpers/krilld",
            "/Users/me/Library/Application Support/dev.krill.app/run/krilld.sock",
        );
        assert!(sync_plist_file(&plist_path, &v1).unwrap());
        assert_eq!(fs::read_to_string(&plist_path).unwrap(), v1);
        // 같은 내용이면 다시 쓰지 않는다.
        assert!(!sync_plist_file(&plist_path, &v1).unwrap());

        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&plist_path).unwrap().permissions().mode() & 0o777,
                0o644
            );
            let lint = Command::new("/usr/bin/plutil")
                .arg("-lint")
                .arg(&plist_path)
                .output()
                .unwrap();
            assert!(lint.status.success(), "{lint:?}");
        }

        let v2 = super::super::holder_plist(
            "/Applications/Krill.app/Contents/Helpers/krill-holder",
            "/Applications/Krill.app/Contents/Helpers/krilld",
            "/Users/me/Library/Application Support/dev.krill.app/run/krilld.sock",
        );
        assert!(sync_plist_file(&plist_path, &v2).unwrap());
        assert_eq!(fs::read_to_string(&plist_path).unwrap(), v2);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn bootstrap_cleans_stale_record_before_loading_and_propagates_failure() {
        let plist = Path::new("/Users/me/Library/LaunchAgents/dev.krill.app.holder.plist");
        let mut calls: Vec<Vec<String>> = Vec::new();
        let ok = bootstrap_with(plist, 501, |args| {
            calls.push(
                args.iter()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect(),
            );
            // 첫 bootout이 실패(아직 미등록)해도 bootstrap이 성공하면 전체 성공이다.
            args.first().copied() == Some(OsStr::new("bootstrap"))
        });
        assert!(ok);
        assert_eq!(
            calls,
            vec![
                vec![
                    "bootout".to_owned(),
                    "gui/501/dev.krill.app.holder".to_owned()
                ],
                vec![
                    "bootstrap".to_owned(),
                    "gui/501".to_owned(),
                    plist.display().to_string(),
                ],
            ]
        );

        let fallback = bootstrap_with(plist, 501, |_| false);
        assert!(!fallback);
    }

    #[cfg(windows)]
    #[test]
    fn sync_windows_run_round_trips_unicode_and_caches_receipt() {
        struct RegCleanup(String);
        impl Drop for RegCleanup {
            fn drop(&mut self) {
                crate::os::background_command(reg_exe())
                    .args(["delete", &self.0, "/f"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .ok();
            }
        }

        let pid = std::process::id();
        let key = format!(r"HKCU\Software\KrillAutostartTest-{pid}");
        let _cleanup = RegCleanup(key.clone());
        let root = std::env::temp_dir().join(format!("krill-win-autostart-{pid}"));
        fs::remove_dir_all(&root).ok();
        let receipt = root.join(WINDOWS_RECEIPT_REL);
        let expected = windows_run_command(
            Path::new(r"C:\Users\홍 길동\AppData\Local\Programs\Krill\krill-holder.exe"),
            Path::new(r"C:\Users\홍 길동\AppData\Local\Programs\Krill\krilld.exe"),
            Path::new(r"C:\Users\홍 길동\AppData\Local\Krill\run\krilld.sock"),
        )
        .unwrap();

        assert!(sync_windows_run_in(&key, HOLDER_LABEL, &receipt, &expected).unwrap());
        assert!(!sync_windows_run_in(&key, HOLDER_LABEL, &receipt, &expected).unwrap());
        // `reg.exe query`는 영문 Windows 러너(CP437/CP1252) 파이프에서 한글을 `?`로 바꾸므로,
        // 항상 UTF-16LE BOM으로 내보내는 `reg.exe export`로 실제 저장된 유니코드를 검증한다.
        let exported = root.join("exported.reg");
        let export_status = crate::os::background_command(reg_exe())
            .args(["export", &key, exported.to_str().unwrap(), "/y"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(export_status.success());
        let bytes = fs::read(&exported).unwrap();
        let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(&bytes);
        let units: Vec<u16> = body
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_le_bytes)
            .collect();
        let reg_text = String::from_utf16(&units).unwrap();
        let escaped = expected.replace('\\', r"\\").replace('"', r#"\""#);
        let needle = format!("\"{HOLDER_LABEL}\"=\"{escaped}\"");
        assert!(
            reg_text.contains(&needle),
            "unexpected reg export output: {reg_text}"
        );
        fs::remove_dir_all(&root).ok();
    }
}
