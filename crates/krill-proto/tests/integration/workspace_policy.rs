//! 워크스페이스 정책 테스트. AGENTS 규칙 5·CONVENTIONS U1: unsafe 린트를 끄는 속성은 FFI 모듈에만 둔다.
//! 허용 목록을 넓히는 것은 하네스 변경이다(메인테이너 승인).

use std::path::Path;

/// 워크스페이스 루트 기준. holder·krilld FFI 모듈(Unix·Windows), krill-ui platform 모듈(objc2·Win32).
const UNSAFE_ALLOWED: [&str; 6] = [
    "crates/krill-holder/src/sys.rs",
    "crates/krill-holder/src/sys/windows.rs",
    "crates/krilld/src/sys.rs",
    "crates/krill-ui/src/platform.rs",
    "crates/krill-ui/src/platform/windows.rs",
    "crates/krill-ui/src/platform/windows/shell.rs",
];

/// 이 파일도 검사 대상이라 린트 이름을 글자 그대로 쓰지 않는다.
const LINT: &str = concat!("unsafe", "_code");

/// 공백을 지운 소스에서 린트 이름이 `forbid(`·`deny(` 바로 뒤가 아닌 곳(expect·allow·warn·목록 안)에 나오는지.
fn relaxes_unsafe_lint(src: &str) -> bool {
    let compact: String = src.chars().filter(|c| !c.is_whitespace()).collect();
    compact.match_indices(LINT).any(|(idx, _)| {
        let before = compact.get(..idx).unwrap_or_default();
        !(before.ends_with("forbid(") || before.ends_with("deny("))
    })
}

fn rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            rs_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

#[test]
fn unsafe_lint_is_relaxed_only_in_ffi_modules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    rs_files(&root.join("crates"), &mut files).unwrap();
    let mut seen = Vec::new();
    let mut violations = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !relaxes_unsafe_lint(&std::fs::read_to_string(&file).unwrap()) {
            continue;
        }
        if UNSAFE_ALLOWED.contains(&rel.as_str()) {
            seen.push(rel);
        } else {
            violations.push(rel);
        }
    }
    assert!(
        violations.is_empty(),
        "FFI 모듈 밖에서 unsafe 린트를 끔: {violations:?}"
    );
    // 목록이 낡으면(파일 이동·삭제) 같이 고친다 — 죽은 항목이 새 파일 경로를 미리 열어 두지 않게.
    seen.sort();
    let mut allowed = UNSAFE_ALLOWED.to_vec();
    allowed.sort_unstable();
    assert_eq!(
        seen, allowed,
        "허용 목록에 더는 unsafe 예외가 없는 파일이 있다"
    );
}

#[test]
fn detects_relaxing_attributes() {
    let lint = LINT;
    assert!(!relaxes_unsafe_lint(&format!(
        "#![forbid({lint})]\n#![deny( {lint} )]"
    )));
    for bad in [
        format!("#![expect({lint}, reason = \"x\")]"),
        format!("#[allow(\n    {lint}\n)]"),
        format!("#![cfg_attr(unix, expect(\n {lint},\n reason = \"x\"))]"),
        format!("#[expect(clippy::foo, {lint})]"),
        format!("#![warn({lint})]"),
    ] {
        assert!(relaxes_unsafe_lint(&bad), "{bad}");
    }
}

/// 비테스트 프로덕션 코드에서 `Command::new(` 직접 호출이 허용된 파일 목록.
/// Windows에서 콘솔 창 깜빡임(`CREATE_NO_WINDOW` 누락)을 막기 위해, 모든 백그라운드 명령은
/// `krill_proto::os::background_command`를 써야 한다.
/// 예외는 헬퍼 구현 자체, 인터랙티브 ConPTY/CLI 로그인/분리 홀더 기동, 그리고 Unix·macOS·Linux 전용 모듈뿐이다.
const RAW_COMMAND_ALLOWED: &[&str] = &[
    // background_command 구현 자체(+ macOS launchctl)
    "crates/krill-proto/src/os.rs",
    // 인터랙티브 CLI 로그인(백그라운드 호출자는 os::configure_background 적용)
    "crates/krill-proto/src/account_login.rs",
    // Windows krill-holder --bootstrap (DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
    "crates/krill-proto/src/daemon.rs",
    // 인터랙티브 ConPTY 세션 에이전트·폴백 셸 런처
    "crates/krilld/src/agent/windows.rs",
    // 인터랙티브 ConPTY 세션 셸 트램펄린 + Unix 전용 로그인 셸 조회
    "crates/krilld/src/shell.rs",
    // Unix/macOS/Linux 전용 호출(Windows 빌드 대상 아님)
    "crates/krill/src/doctor.rs",
    "crates/krill-holder/src/holder.rs",
    "crates/krill-hosts/src/managed.rs",
    "crates/krill-hosts/src/ssh.rs",
    "crates/krill-hosts/src/ssh/bridge.rs",
    "crates/krill-link-core/src/config.rs",
    "crates/krill-link-core/src/launchd.rs",
    "crates/krill-link-core/src/launchd/lifecycle.rs",
    "crates/krill-proto/src/daemon/autostart.rs",
    "crates/krill-ui/src/platform/audio.rs",
    "crates/krill-ui/src/responsiveness.rs",
    "crates/krilld/src/agent/claude.rs",
    "crates/krilld/src/mobile/net.rs",
    "crates/krilld/src/mobile/pwa_rpc.rs",
    "crates/krilld/src/registry/account_switch/identity/claude.rs",
    "crates/krilld/src/usage/accounts/agy_native/backend.rs",
    "crates/krilld/src/usage/accounts/profile_name.rs",
    "crates/krilld/src/usage/provision/history_probe.rs",
];

fn is_cfg_test_only_attr(trimmed: &str) -> bool {
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    compact == "#[test]"
        || compact.starts_with("#[gpui::test")
        || compact.starts_with("#[cfg(test)]")
        || compact.starts_with("#[cfg(all(test,")
        || (compact.starts_with("#[cfg(all(") && compact.contains(",test)"))
        || (compact.starts_with("#[cfg(all(") && compact.contains(",test,"))
}

fn contains_production_raw_command(src: &str) -> bool {
    let mut depth: i32 = 0;
    let mut pending_test = false;
    let mut test_depth: Option<i32> = None;
    for line in src.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        let code = trimmed.split("//").next().unwrap_or(trimmed);
        if is_cfg_test_only_attr(code) {
            pending_test = true;
        }
        if test_depth.is_none() && !pending_test && code.contains("Command::new(") {
            return true;
        }
        let opens = i32::try_from(code.matches('{').count()).unwrap_or(0);
        let closes = i32::try_from(code.matches('}').count()).unwrap_or(0);
        if opens > 0 {
            if pending_test && test_depth.is_none() {
                test_depth = Some(depth);
            }
            pending_test = false;
        } else if code.trim_end().ends_with(';') {
            pending_test = false;
        }
        depth += opens - closes;
        if let Some(td) = test_depth
            && depth <= td
        {
            test_depth = None;
        }
    }
    false
}

fn is_production_source(rel: &str) -> bool {
    rel.contains("/src/")
        && !rel.contains("/tests/")
        && !rel.ends_with("/tests.rs")
        && !rel.ends_with("_tests.rs")
}

#[test]
fn background_commands_use_no_window_helper_across_workspace() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    rs_files(&root.join("crates"), &mut files).unwrap();
    let mut seen = Vec::new();
    let mut violations = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !is_production_source(&rel) {
            continue;
        }
        let src = std::fs::read_to_string(&file).unwrap();
        if !contains_production_raw_command(&src) {
            continue;
        }
        if RAW_COMMAND_ALLOWED.contains(&rel.as_str()) {
            seen.push(rel);
        } else {
            violations.push(rel);
        }
    }
    assert!(
        violations.is_empty(),
        "Windows CREATE_NO_WINDOW 회귀 방지: 프로덕션 백그라운드 명령은 krill_proto::os::background_command를 써야 합니다: {violations:?}"
    );
    seen.sort();
    let mut allowed = RAW_COMMAND_ALLOWED.to_vec();
    allowed.sort_unstable();
    assert_eq!(
        seen, allowed,
        "RAW_COMMAND_ALLOWED에 더는 Command::new를 직접 쓰지 않는 파일이 남아 있습니다"
    );
}

#[test]
fn raw_command_detector_ignores_tests_and_flags_production_code() {
    assert!(contains_production_raw_command(
        "fn f() { let _ = std::process::Command::new(\"git\"); }"
    ));
    assert!(!contains_production_raw_command(
        "fn f() { let _ = krill_proto::os::background_command(\"git\"); }"
    ));
    assert!(!contains_production_raw_command(
        "fn f() {}\n#[cfg(test)]\nmod tests {\n    fn t() { let _ = Command::new(\"git\"); }\n}\n"
    ));
    assert!(!contains_production_raw_command(
        "fn f() {}\n#[cfg(unix)]\n#[test]\nfn t() {\n    let _ = std::process::Command::new(\"git\");\n}\n"
    ));
    assert!(contains_production_raw_command(
        "#[cfg(any(windows, test))]\nfn f() { let _ = Command::new(\"cmd.exe\"); }\n"
    ));
}
