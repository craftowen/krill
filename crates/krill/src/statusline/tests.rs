//! OWE-701: 실행 체인·기록·설치/제거 회귀 테스트. 전부 격리 임시 디렉터리만 쓴다.

use super::*;

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("owe701-{}-{}", std::process::id(), tag));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    crate::hook_install::canonicalize(&dir).unwrap()
}

fn payload(five: u8, seven: u8) -> Vec<u8> {
    serde_json::json!({
        "session_id": "s1",
        "cwd": "/tmp/work",
        "rate_limits": {
            "five_hour": {"used_percentage": five, "resets_at": "2099-01-01T00:00:00Z"},
            "seven_day": {"used_percentage": seven, "resets_at": "2099-01-08T00:00:00Z"},
        },
    })
    .to_string()
    .into_bytes()
}

fn cache_of(dir: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(statusline::cache_path(dir)).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn run_args_parse_chain_forms_and_ignore_unknown_flags() {
    let args = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
    assert!(parse_run_args(&args(&[])).chain.is_none());
    assert_eq!(
        parse_run_args(&args(&["--chain", "user-cmd"])).chain,
        Some(OsString::from("user-cmd"))
    );
    assert_eq!(
        parse_run_args(&args(&["--chain=user-cmd"])).chain,
        Some(OsString::from("user-cmd"))
    );
    // 모르는 플래그는 틱을 깨지 않게 무시한다.
    assert!(parse_run_args(&args(&["--json"])).chain.is_none());
    assert!(parse_run_args(&args(&["--chain"])).chain.is_none());
}

#[test]
fn record_writes_cache_0600_with_cached_at_and_dedups_same_value() {
    let dir = temp_root("record");
    let body = payload(42, 67);
    record(&dir, &body, 1_000);
    let first = cache_of(&dir);
    assert_eq!(
        first
            .get("rate_limits")
            .and_then(|r| r.get("five_hour"))
            .and_then(|w| w.get("used_percentage")),
        Some(&serde_json::json!(42))
    );
    assert_eq!(
        first.get("cached_at").and_then(|c| c.as_str()),
        Some("1000")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(statusline::cache_path(&dir))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    // 같은 값 5초 후: 파일 그대로(재기록 안 함).
    record(&dir, &body, 1_005);
    assert_eq!(cache_of(&dir), first);
    // 값이 바뀌면 즉시 쓴다.
    record(&dir, &payload(43, 67), 1_005);
    let second = cache_of(&dir);
    assert_ne!(second, first);
    assert_eq!(
        second.get("cached_at").and_then(|c| c.as_str()),
        Some("1005")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn record_skips_without_rate_limits_and_missing_dir() {
    let dir = temp_root("record-skip");
    // `rate_limits` 없으면 파일을 만들지 않는다.
    record(&dir, br#"{"session_id":"s1"}"#, 1_000);
    assert!(!statusline::cache_path(&dir).exists());
    // 깨진 입력도 마찬가지다.
    record(&dir, b"not json", 1_000);
    assert!(!statusline::cache_path(&dir).exists());
    // 없는 디렉터리에는 만들지 않는다.
    record(&dir.join("no-such-dir"), &payload(1, 2), 1_000);
    assert!(!statusline::cache_path(&dir.join("no-such-dir")).exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn chain_passthrough_returns_user_output_verbatim() {
    // 사용자 명령(`cat`)이 stdin을 그대로 돌려준다.
    let input = payload(10, 20);
    let output = run_chain(OsStr::new("cat"), &input);
    assert_eq!(output, input);
    // 없는 명령은 빈 출력이다(표시만 비고 0으로 끝난다 — 호출자가 보장).
    assert!(run_chain(OsStr::new("/no/such/command-owe701"), &input).is_empty());
}

#[test]
fn resolve_chain_prefers_explicit_and_reads_settings_slot() {
    let dir = temp_root("chain");
    let exe = "/test/krill";
    let none: Option<PathBuf> = None;
    // 명시가 우선이다.
    assert_eq!(
        resolve_chain(Some(OsStr::new("explicit")), none.as_ref(), false, exe),
        Some(OsString::from("explicit"))
    );
    // 깊이 가드가 있으면 명시도 건너뛴다.
    assert_eq!(
        resolve_chain(Some(OsStr::new("explicit")), none.as_ref(), true, exe),
        None
    );
    // 슬롯 없음: 체인 없음.
    assert_eq!(
        resolve_chain(None, Some(dir.clone()).as_ref(), false, exe),
        None
    );
    // 사용자 슬롯: 그대로 체인.
    std::fs::write(
        dir.join("settings.json"),
        r#"{"statusLine": {"type": "command", "command": "user-cmd"}}"#,
    )
    .unwrap();
    assert_eq!(
        resolve_chain(None, Some(dir.clone()).as_ref(), false, exe),
        Some(OsString::from("user-cmd"))
    );
    // 우리 베어: 체인 없음. 우리 체인: 원본을 꺼내 체인.
    std::fs::write(
        dir.join("settings.json"),
        serde_json::json!({"statusLine": {"command": statusline::install_command(exe)}})
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        resolve_chain(None, Some(dir.clone()).as_ref(), false, exe),
        None
    );
    std::fs::write(
        dir.join("settings.json"),
        serde_json::json!({"statusLine": {"command": statusline::chain_command(exe, "orig-cmd")}})
            .to_string(),
    )
    .unwrap();
    assert_eq!(
        resolve_chain(None, Some(dir.clone()).as_ref(), false, exe),
        Some(OsString::from("orig-cmd"))
    );
    std::fs::remove_dir_all(&dir).ok();
}

fn install_paths(tag: &str) -> (PathBuf, InstallPaths) {
    let root = temp_root(tag);
    let paths = InstallPaths {
        settings: root.join("claude/settings.json"),
        receipt: root.join("state").join(RECEIPT_FILE),
        lock_dir: root.join("state/hooks"),
    };
    (root, paths)
}

#[test]
fn install_then_uninstall_round_trips_empty_slot() {
    let (root, paths) = install_paths("install-empty");
    let exe = "/test/krill";
    let probe = || Some("2.1.301 (Claude Code)".to_owned());
    let mut out = Vec::new();
    install_with(&paths, exe, &probe, true, false, &mut out).unwrap();
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.settings).unwrap()).unwrap();
    assert_eq!(
        after
            .get("statusLine")
            .and_then(|slot| slot.get("command"))
            .and_then(|c| c.as_str()),
        Some(statusline::install_command(exe).as_str())
    );
    // receipt가 완성 기록으로 남는다.
    let receipt: Receipt =
        serde_json::from_str(&std::fs::read_to_string(&paths.receipt).unwrap()).unwrap();
    assert!(receipt.complete);

    // 제거하면 키가 통째로 사라지고 receipt도 사라진다.
    let mut out = Vec::new();
    uninstall_with(&paths, exe, true, false, &mut out).unwrap();
    assert!(!paths.settings.exists());
    assert!(!paths.receipt.exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn install_chains_user_slot_and_uninstall_restores_it() {
    let (root, paths) = install_paths("install-user");
    let exe = "/test/krill";
    let probe = || Some("2.1.301".to_owned());
    let before = r#"{"theme": "dark", "statusLine": {"command": "user-cmd", "padding": 2}}"#;
    std::fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
    std::fs::write(&paths.settings, before).unwrap();

    let mut out = Vec::new();
    install_with(&paths, exe, &probe, true, false, &mut out).unwrap();
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.settings).unwrap()).unwrap();
    assert_eq!(
        after
            .get("statusLine")
            .and_then(|slot| slot.get("command"))
            .and_then(|c| c.as_str()),
        Some(statusline::chain_command(exe, "user-cmd").as_str())
    );
    // 사용자 키는 유지된다.
    assert_eq!(after.get("theme"), Some(&serde_json::json!("dark")));

    let mut out = Vec::new();
    uninstall_with(&paths, exe, true, false, &mut out).unwrap();
    let restored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.settings).unwrap()).unwrap();
    assert_eq!(
        restored.get("statusLine"),
        serde_json::from_str::<serde_json::Value>(before)
            .unwrap()
            .get("statusLine")
    );
    assert!(!paths.receipt.exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn install_refuses_unsupported_claude_and_dry_run_writes_nothing() {
    let (root, paths) = install_paths("install-gate");
    let exe = "/test/krill";
    // 1.0.64 미만은 주입하지 않는다.
    let old = || Some("1.0.63".to_owned());
    let mut out = Vec::new();
    assert!(install_with(&paths, exe, &old, true, false, &mut out).is_err());
    assert!(!paths.settings.exists());

    // dry-run은 파일을 쓰지 않는다.
    let current = || Some("2.1.301".to_owned());
    let mut out = Vec::new();
    install_with(&paths, exe, &current, true, true, &mut out).unwrap();
    assert!(!paths.settings.exists());
    assert!(!paths.receipt.exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn uninstall_preserves_user_taken_slot() {
    let (root, paths) = install_paths("install-takeover");
    let exe = "/test/krill";
    let probe = || Some("2.1.301".to_owned());
    std::fs::create_dir_all(paths.settings.parent().unwrap()).unwrap();
    std::fs::write(
        &paths.settings,
        r#"{"statusLine": {"command": "user-cmd"}}"#,
    )
    .unwrap();
    let mut out = Vec::new();
    install_with(&paths, exe, &probe, true, false, &mut out).unwrap();
    // 사용자가 슬롯을 통째로 가져가면 제거가 거부된다.
    std::fs::write(
        &paths.settings,
        r#"{"statusLine": {"command": "other-cmd"}}"#,
    )
    .unwrap();
    let mut out = Vec::new();
    assert!(uninstall_with(&paths, exe, true, false, &mut out).is_err());
    assert!(paths.receipt.exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn help_flag_returns_zero() {
    use std::ffi::OsString;
    assert_eq!(super::run(&[OsString::from("--help")]), ExitCode::SUCCESS);
    assert_eq!(super::run(&[OsString::from("-h")]), ExitCode::SUCCESS);
}

#[test]
fn chain_that_never_exits_is_killed_at_the_deadline() {
    let started = std::time::Instant::now();
    let out = run_chain_within(
        OsStr::new("printf partial; exec sleep 30"),
        b"{}",
        Duration::from_millis(300),
    );
    assert!(out.is_empty(), "timeout means empty output");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}
