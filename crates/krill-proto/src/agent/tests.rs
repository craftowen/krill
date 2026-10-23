use super::*;

#[test]
fn builtin_has_five_agents_with_yolo_default() {
    let a = Agents::load(None).unwrap();
    assert_eq!(a.permission, Permission::Yolo);
    let ids: Vec<_> = a.profiles.keys().map(String::as_str).collect();
    assert_eq!(ids, ["agy", "claude", "codex", "cursor", "opencode"]);
    assert!(a.profiles.values().all(|p| !p.install.is_empty()));
    let (resolved_id, agy) = a.resolve("antigravity").unwrap();
    assert_eq!(resolved_id, "agy");
    assert_eq!(agy.argv(Permission::Manual), ["agy"]);
    assert_eq!(
        agy.argv(Permission::Yolo),
        ["agy", "--dangerously-skip-permissions"]
    );
}

#[test]
fn default_permission_bypasses_approvals_and_manual_remains_available() {
    for user in [
        None,
        Some(""),
        Some("[agent.mine]\nname = 'Mine'\nlaunch = ['mine']\n"),
    ] {
        let a = Agents::load(user).unwrap();
        for (id, flag) in [
            ("claude", "--dangerously-skip-permissions"),
            ("codex", "--dangerously-bypass-approvals-and-sandbox"),
            ("agy", "--dangerously-skip-permissions"),
            ("opencode", "--auto"),
        ] {
            assert_eq!(a.profiles[id].argv(a.permission), [id, flag]);
        }
    }
    let a = Agents::load(Some("permission = 'manual'\n")).unwrap();
    assert_eq!(a.permission, Permission::Manual);
    for id in ["claude", "codex", "agy", "opencode"] {
        assert_eq!(a.profiles[id].argv(a.permission), [id]);
    }
}

#[test]
fn yolo_args_only_in_yolo_mode() {
    let a = Agents::load(None).unwrap();
    let claude = &a.profiles["claude"];
    assert_eq!(claude.argv(Permission::Manual), ["claude"]);
    assert_eq!(
        claude.argv(Permission::Yolo),
        ["claude", "--dangerously-skip-permissions"]
    );
}

#[test]
fn user_file_overrides_and_adds() {
    let user = r#"
permission = "yolo"

[agent.claude]
name = "Claude (mine)"
launch = ["claude", "--model", "opus"]

[agent.aider]
name = "Aider"
launch = ["aider"]
yolo_args = ["--yes-always"]
future_key = "ignored"

[agent.gemini]
name = "Legacy Gemini CLI"
launch = ["gemini"]
"#;
    let a = Agents::load(Some(user)).unwrap();
    assert_eq!(a.permission, Permission::Yolo);
    assert_eq!(a.profiles.len(), 6);
    assert!(!a.profiles.contains_key("gemini"));
    // 같은 id는 통째로 바뀐다(내장 yolo_args가 남지 않는다).
    assert_eq!(
        a.profiles["claude"].argv(Permission::Yolo),
        ["claude", "--model", "opus"]
    );
    assert_eq!(
        a.profiles["aider"].argv(Permission::Yolo),
        ["aider", "--yes-always"]
    );
}

#[test]
fn bad_user_file_is_error() {
    assert!(Agents::load(Some("permission = \"always\"")).is_err());
    assert!(Agents::load(Some("[agent.x]\nname = \"x\"")).is_err());
}

#[test]
#[cfg(unix)]
fn which_follows_path_and_exec_bit() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("mk037-which-{}", std::process::id()));
    let (a, b) = (dir.join("a"), dir.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("tool"), "").unwrap(); // 실행 비트 없음 → 건너뜀
    std::fs::write(b.join("tool"), "").unwrap();
    std::fs::set_permissions(b.join("tool"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths([&a, &b]).unwrap();

    assert_eq!(which("tool", &path), Some(b.join("tool")));
    assert_eq!(which("missing", &path), None);
    assert_eq!(which("", &path), None);
    let abs = b.join("tool");
    assert_eq!(which(abs.to_str().unwrap(), OsStr::new("")), Some(abs));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[cfg(unix)]
fn version_reads_first_line_and_times_out() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("mk037-ver-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = |name: &str, body: &str| {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    let ok = script("ok", "printf '\\n  2.1.0 (Claude Code)\\nmore\\n'");
    assert_eq!(version(&ok).as_deref(), Some("2.1.0 (Claude Code)"));
    let fail = script("fail", "exit 3");
    assert_eq!(version(&fail), None);
    // 출력이 없는 채 멈추면 시간 제한으로 끊긴다.
    let hang = script("hang", "exec sleep 30");
    let start = std::time::Instant::now();
    assert_eq!(version(&hang), None);
    assert!(start.elapsed() < Duration::from_secs(10));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn old_agents_toml_without_env_still_parses() {
    let user = r#"
[agent.claude]
name = "Claude"
launch = ["claude", "--model", "opus"]
"#;
    let a = Agents::load(Some(user)).unwrap();
    assert!(a.profiles["claude"].env.is_empty());
    assert!(a.profiles.values().all(|p| p.env.is_empty()));
}

#[test]
fn profile_env_roundtrips_and_is_omitted_when_empty() {
    let mut p = Agents::load(None).unwrap().profiles["codex"].clone();
    let bare = toml::to_string(&p).unwrap();
    assert!(!bare.contains("env"), "{bare}");
    p.env
        .insert("OPENAI_BASE_URL".into(), "http://x = y".into());
    let text = toml::to_string(&p).unwrap();
    assert_eq!(toml::from_str::<Profile>(&text).unwrap(), p);
}

#[test]
fn env_lines_parse_and_reject_bad_rows() {
    let env = parse_env_lines("A=1\n\n  B = two words \nC=").unwrap();
    assert_eq!(env["A"], "1");
    assert_eq!(env["B"], "two words");
    assert_eq!(env["C"], "");
    assert_eq!(parse_env_lines(&format_env_lines(&env)).unwrap(), env);
    assert_eq!(parse_env_lines("A=1\nNOEQ"), Err(2));
    assert_eq!(parse_env_lines("=v"), Err(1));
    assert_eq!(parse_env_lines("A B=v"), Err(1));
}

#[test]
fn set_user_profile_keeps_other_keys_and_clears_env() {
    let user = r#"
permission = "yolo"
[agent.mine]
name = "Mine"
launch = ["mine"]
[agent.claude]
name = "Claude"
launch = ["claude"]
future_key = 1
env = { OLD = "1" }
"#;
    let mut p = Agents::load(Some(user)).unwrap().profiles["claude"].clone();
    p.launch.push("--verbose".into());
    p.env.clear();
    let text = set_user_profile(Some(user), "claude", &p).unwrap();
    let a = Agents::load(Some(&text)).unwrap();
    assert_eq!(a.permission, Permission::Yolo);
    assert_eq!(a.profiles["mine"].launch, ["mine"]);
    assert_eq!(a.profiles["claude"], p);
    assert!(text.contains("future_key"), "{text}");
    p.env.insert("K".into(), "v".into());
    let text = set_user_profile(None, "claude", &p).unwrap();
    assert_eq!(Agents::load(Some(&text)).unwrap().profiles["claude"], p);
}

/// Windows는 실행 비트 대신 `PATHEXT`로 `claude` → `claude.cmd`를 찾는다.
#[test]
#[cfg(windows)]
fn which_appends_pathext_on_windows() {
    let dir = std::env::temp_dir().join(format!("krill-which-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("claude.cmd"), "").unwrap();
    let path = std::env::join_paths([&dir]).unwrap();
    assert_eq!(which("claude", &path), Some(dir.join("claude.cmd")));
    assert_eq!(which("claude.cmd", &path), Some(dir.join("claude.cmd")));
    assert_eq!(which("missing", &path), None);
    std::fs::remove_dir_all(&dir).ok();
}

/// Windows에서는 `bash` 파이프 대신 `PowerShell` 설치 명령을, 없으면 공용 명령을 보여 준다.
#[test]
fn install_command_prefers_windows_variant_on_windows() {
    let a = Agents::load(None).unwrap();
    let claude = &a.profiles["claude"];
    let expected = if cfg!(windows) {
        "irm https://claude.ai/install.ps1 | iex"
    } else {
        "curl -fsSL https://claude.ai/install.sh | bash"
    };
    assert_eq!(claude.install_command(), expected);
    assert_eq!(
        a.profiles["codex"].install_command(),
        "npm install -g @openai/codex"
    );
}
