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
fn interrupt_sequence_uses_esc_for_tui_agents_and_ctrl_c_for_shells() {
    for agent in [
        "agy",
        "antigravity",
        "Antigravity",
        "claude",
        "claude-code",
        "openclaude",
        "codex",
        "opencode",
        "opencode2",
        "pi",
        "omp",
        "cursor",
        "cursor-agent",
        "gemini",
        "Gemini",
    ] {
        assert_eq!(interrupt_sequence(agent), b"\x1b", "agent={agent}");
    }
    for shell in ["", "zsh", "bash", "fish", "sh", "unknown"] {
        assert_eq!(interrupt_sequence(shell), b"\x03", "shell={shell}");
    }
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
        "& ([scriptblock]::Create((irm https://claude.ai/install.ps1))) latest"
    } else {
        "curl -fsSL https://claude.ai/install.sh | bash -s -- latest"
    };
    assert_eq!(claude.install_command(), expected);
    assert_eq!(
        a.profiles["codex"].install_command(),
        if cfg!(windows) {
            "npm.cmd install -g @openai/codex@latest"
        } else {
            "npm install -g @openai/codex@latest"
        }
    );
}

#[test]
fn launch_overrides_encode_decode_and_apply_across_providers() {
    let agents = Agents::load(None).unwrap();

    // 오버라이드가 없으면 기존 id와 동일해야 한다.
    let empty = LaunchOverrides::default();
    let encoded_plain = encode_agent_program_id("codex", &empty).unwrap();
    assert_eq!(encoded_plain, "codex");
    assert_eq!(
        decode_agent_program_id(&encoded_plain).unwrap(),
        ("codex", empty)
    );

    // Codex: --model + -c model_reasoning_effort="xhigh"
    let codex_overrides = LaunchOverrides {
        model: Some("gpt-5.4".to_owned()),
        effort: Some("XHIGH".to_owned()),
    };
    let encoded = encode_agent_program_id("codex", &codex_overrides).unwrap();
    assert!(encoded.starts_with("launch\x1f"));
    let (decoded_id, decoded_overrides) = decode_agent_program_id(&encoded).unwrap();
    assert_eq!(decoded_id, "codex");
    assert_eq!(decoded_overrides.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(decoded_overrides.effort.as_deref(), Some("xhigh"));

    let mut codex_profile = agents.profiles["codex"].clone();
    apply_launch_overrides("codex", &mut codex_profile, &decoded_overrides).unwrap();
    assert_eq!(
        codex_profile.argv(Permission::Yolo),
        [
            "codex",
            "--model",
            "gpt-5.4",
            "-c",
            "model_reasoning_effort=\"xhigh\"",
            "--dangerously-bypass-approvals-and-sandbox",
        ]
    );

    // Claude: --model + --effort max
    let mut claude_profile = agents.profiles["claude"].clone();
    apply_launch_overrides(
        "claude",
        &mut claude_profile,
        &LaunchOverrides {
            model: Some("opus".to_owned()),
            effort: Some("max".to_owned()),
        },
    )
    .unwrap();
    assert_eq!(
        claude_profile.argv(Permission::Yolo),
        [
            "claude",
            "--model",
            "opus",
            "--effort",
            "max",
            "--dangerously-skip-permissions",
        ]
    );

    // Antigravity (`agy` / `antigravity`): supports --effort (low..max) and resolves suffix conflicts.
    let mut agy_family = agents.profiles["agy"].clone();
    apply_launch_overrides(
        "antigravity",
        &mut agy_family,
        &LaunchOverrides {
            model: Some("gemini-4-neon".to_owned()),
            effort: Some("xhigh".to_owned()),
        },
    )
    .unwrap();
    assert_eq!(
        agy_family.argv(Permission::Yolo),
        [
            "agy",
            "--model",
            "gemini-4-neon",
            "--effort",
            "xhigh",
            "--dangerously-skip-permissions",
        ]
    );

    // Matching suffix omits redundant --effort.
    let mut agy_same = agents.profiles["agy"].clone();
    apply_launch_overrides(
        "agy",
        &mut agy_same,
        &LaunchOverrides {
            model: Some("gemini-3.8-flash-low".to_owned()),
            effort: Some("low".to_owned()),
        },
    )
    .unwrap();
    assert_eq!(
        agy_same.argv(Permission::Yolo),
        [
            "agy",
            "--model",
            "gemini-3.8-flash-low",
            "--dangerously-skip-permissions",
        ]
    );

    // Conflicting suffix strips old suffix to family + requested --effort.
    let mut agy_conflict = agents.profiles["agy"].clone();
    apply_launch_overrides(
        "agy",
        &mut agy_conflict,
        &LaunchOverrides {
            model: Some("gemini-3.8-flash-high".to_owned()),
            effort: Some("low".to_owned()),
        },
    )
    .unwrap();
    assert_eq!(
        agy_conflict.argv(Permission::Yolo),
        [
            "agy",
            "--model",
            "gemini-3.8-flash",
            "--effort",
            "low",
            "--dangerously-skip-permissions",
        ]
    );

    // Unsupported effort or conflicting flags are rejected.
    let agy_profile = &agents.profiles["agy"];
    assert!(
        validate_launch_overrides(
            "agy",
            agy_profile,
            &LaunchOverrides {
                model: None,
                effort: Some("minimal".to_owned()),
            }
        )
        .is_err()
    );
    let cursor_profile = &agents.profiles["cursor"];
    assert!(
        validate_launch_overrides(
            "cursor",
            cursor_profile,
            &LaunchOverrides {
                model: None,
                effort: Some("high".to_owned()),
            }
        )
        .is_err()
    );
    assert!(
        validate_launch_overrides(
            "claude",
            &claude_profile,
            &LaunchOverrides {
                model: Some("sonnet".to_owned()),
                effort: None,
            }
        )
        .is_err()
    );
    assert!(
        encode_agent_program_id(
            "codex",
            &LaunchOverrides {
                model: Some("--bad-flag".to_owned()),
                effort: None,
            }
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn version_in_resolves_env_shebang_interpreter_and_which_all_preserves_order() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = std::env::temp_dir().join(format!("krill-agent-version-in-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let interp_dir = root.join("interp");
    let first_dir = root.join("first");
    let second_dir = root.join("second");
    std::fs::create_dir_all(&interp_dir).unwrap();
    std::fs::create_dir_all(&first_dir).unwrap();
    std::fs::create_dir_all(&second_dir).unwrap();

    let interp_name = format!("krill-test-node-{}", std::process::id());
    let interp_bin = interp_dir.join(&interp_name);
    std::fs::write(&interp_bin, "#!/bin/sh\necho 'codex-cli 0.167.2'\n").unwrap();
    std::fs::set_permissions(&interp_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let first_bin = first_dir.join("codex");
    std::fs::write(&first_bin, format!("#!/usr/bin/env {interp_name}\n")).unwrap();
    std::fs::set_permissions(&first_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let second_bin = second_dir.join("codex");
    std::fs::write(&second_bin, "#!/bin/sh\necho 'codex-cli 0.160.0'\n").unwrap();
    std::fs::set_permissions(&second_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let search_path = std::env::join_paths([
        first_dir.as_path(),
        second_dir.as_path(),
        first_dir.as_path(),
        interp_dir.as_path(),
    ])
    .unwrap();
    assert_eq!(
        which_all("codex", &search_path),
        vec![first_bin.clone(), second_bin]
    );
    assert_eq!(
        version_in(&first_bin, &search_path).as_deref(),
        Some("codex-cli 0.167.2")
    );
    std::fs::remove_dir_all(&root).ok();
}

#[cfg(unix)]
#[test]
fn package_version_reads_homebrew_caskroom_and_scoped_npm_metadata() {
    use std::os::unix::fs::symlink;

    let root = std::env::temp_dir().join(format!("krill-agent-pkg-ver-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();

    let cask_bin = root.join("brew/Caskroom/codex/0.167.2/bin/codex");
    let brew_bin_dir = root.join("brew/bin");
    std::fs::create_dir_all(cask_bin.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&brew_bin_dir).unwrap();
    std::fs::write(&cask_bin, "#!/bin/sh\nexit 1\n").unwrap();
    let cask_link = brew_bin_dir.join("codex");
    symlink(&cask_bin, &cask_link).unwrap();
    assert_eq!(package_version(&cask_link).as_deref(), Some("0.167.2"));

    let npm_pkg = root.join("npm/lib/node_modules/@openai/codex");
    let npm_js = npm_pkg.join("bin/codex.js");
    let npm_bin_dir = root.join("npm/bin");
    std::fs::create_dir_all(npm_js.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&npm_bin_dir).unwrap();
    std::fs::write(&npm_js, "#!/usr/bin/env node\n").unwrap();
    std::fs::write(
        npm_pkg.join("package.json"),
        r#"{"name":"@openai/codex","version":"0.168.0"}"#,
    )
    .unwrap();
    let npm_link = npm_bin_dir.join("codex");
    symlink(&npm_js, &npm_link).unwrap();
    assert_eq!(package_version(&npm_link).as_deref(), Some("0.168.0"));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn presets_match_preset_order_with_unique_ranks_and_homepages() {
    let presets = preset_profiles();
    let expected: std::collections::BTreeSet<&str> = PRESET_ORDER.iter().copied().collect();
    assert_eq!(presets.len(), PRESET_ORDER.len());
    assert_eq!(presets.len(), 24);
    for id in PRESET_ORDER {
        assert!(presets.contains_key(*id), "missing preset {id}");
        assert!(agent_homepage(id).is_some(), "missing homepage for {id}");
    }
    for key in presets.keys() {
        assert!(
            expected.contains(key.as_str()),
            "missing {key} in PRESET_ORDER"
        );
    }
    for builtin in ["claude", "codex", "agy", "cursor", "opencode"] {
        assert!(agent_homepage(builtin).is_some());
    }
    assert!(profile_rank("claude") < profile_rank("opencode"));
    assert!(profile_rank("opencode") < profile_rank("grok"));
    assert!(profile_rank("grok") < profile_rank("muse"));
    assert!(profile_rank("muse") < profile_rank("custom-x"));
    assert_eq!(profile_rank("custom-x"), u8::MAX);
}

#[test]
fn grok_muse_and_preset_launch_overrides_and_process_recognition() {
    let agents = Agents::load(None).unwrap();
    let (grok_id, grok) = agents.resolve("grok").unwrap();
    assert_eq!(grok_id, "grok");
    assert_eq!(
        grok.argv(Permission::Yolo),
        ["grok", "--permission-mode", "bypassPermissions"]
    );

    let (muse_id, muse) = agents.resolve("muse").unwrap();
    assert_eq!(muse_id, "muse");
    assert_eq!(muse.argv(Permission::Yolo), ["muse", "--yolo"]);

    let (vibe_id, _) = agents.resolve("vibe").unwrap();
    assert_eq!(vibe_id, "mistral-vibe");

    let mut grok_profile = grok.clone();
    apply_launch_overrides(
        "grok",
        &mut grok_profile,
        &LaunchOverrides {
            model: Some("grok-4.7".to_owned()),
            effort: Some("XHIGH".to_owned()),
        },
    )
    .unwrap();
    assert_eq!(
        grok_profile.argv(Permission::Yolo),
        [
            "grok",
            "-m",
            "grok-4.7",
            "--reasoning-effort",
            "xhigh",
            "--permission-mode",
            "bypassPermissions",
        ]
    );
    assert!(
        validate_launch_overrides(
            "grok",
            &grok_profile,
            &LaunchOverrides {
                model: None,
                effort: Some("high".to_owned()),
            }
        )
        .is_err()
    );
    assert!(
        validate_launch_overrides(
            "grok",
            grok,
            &LaunchOverrides {
                model: None,
                effort: Some("max".to_owned()),
            }
        )
        .is_err()
    );

    for known in [
        "claude",
        "codex-aarch64-ap",
        "grok",
        "grok-4.7",
        "muse",
        "muse-bin-1.0.3-R",
        "kiro-cli",
        "vibe",
        "cn",
        "traecli",
        "dsh-tui",
        "zcode-cli",
        "openclaude",
        "opencode2",
    ] {
        assert!(
            is_known_agent_process(known),
            "{known} should be recognized"
        );
    }
    assert_eq!(executable_id("grok-4.7"), "grok");
    assert_eq!(executable_id("muse-bin-1.0.3"), "muse");
    assert_eq!(executable_id("vibe"), "mistral-vibe");
    assert!(!is_known_agent_process("zsh"));
    assert!(!is_known_agent_process("bash"));
}

#[test]
fn remove_user_profile_and_validate_custom_agent_id() {
    let user = r#"# top comment
permission = "yolo"

[agent.grok]
name = "Grok"
launch = ["grok"]

[agent.mine]
name = "Mine"
launch = ["mine"]
future_key = 42
"#;
    let updated = remove_user_profile(Some(user), "grok").unwrap();
    assert!(updated.contains("# top comment"), "{updated}");
    assert!(updated.contains("future_key = 42"), "{updated}");
    let parsed: Agents = toml::from_str(&updated).unwrap();
    assert!(!parsed.profiles.contains_key("grok"));
    assert_eq!(parsed.profiles["mine"].launch, ["mine"]);

    assert_eq!(
        validate_custom_agent_id(" My-Agent_1 "),
        Ok("my-agent_1".to_owned())
    );
    assert!(validate_custom_agent_id("").is_err());
    assert!(validate_custom_agent_id("-bad").is_err());
    assert!(validate_custom_agent_id("bad space").is_err());
    assert!(validate_custom_agent_id("gemini").is_err());
    assert!(validate_custom_agent_id(&"a".repeat(49)).is_err());
}

#[test]
fn antigravity_listed_family_selection_folds_only_grouped_family_level_ids() {
    // Mirrors Orca `antigravity-model-listing.test.ts` (#46941).
    let listed = vec![
        AgyListedModel {
            id: "gemini-4-argon".to_owned(),
            efforts: vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
        },
        AgyListedModel {
            id: "gemini-3.5-flash-medium".to_owned(),
            efforts: Vec::new(),
        },
        AgyListedModel {
            id: "gemini-3.5-flash".to_owned(),
            efforts: Vec::new(),
        },
        AgyListedModel {
            id: "gemini-3.5-flash-low".to_owned(),
            efforts: Vec::new(),
        },
        AgyListedModel {
            id: "gpt-oss-120b-medium".to_owned(),
            efforts: Vec::new(),
        },
    ];
    let pick = |model: &str, effort: Option<&str>| {
        antigravity_listed_family_selection(model, effort, &listed)
    };
    assert_eq!(
        pick("gemini-4-argon-medium", Some("medium")),
        Some(("gemini-4-argon".to_owned(), "medium".to_owned()))
    );
    assert_eq!(
        pick("gemini-4-argon-medium", None),
        Some(("gemini-4-argon".to_owned(), "medium".to_owned()))
    );
    assert_eq!(
        pick("gemini-4-argon-medium", Some("high")),
        Some(("gemini-4-argon".to_owned(), "high".to_owned()))
    );
    assert_eq!(pick("gemini-4-argon-medium", Some("xhigh")), None);
    assert_eq!(pick("gemini-3.5-flash-medium", Some("medium")), None);
    assert_eq!(pick("gpt-oss-120b-medium", Some("medium")), None);

    // Even when a live menu gives `gemini-3.5-flash` efforts, a listed level ID stays itself.
    let mut live = listed.clone();
    live[2].efforts = vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()];
    assert_eq!(
        antigravity_listed_family_selection("gemini-3.5-flash-medium", Some("medium"), &live),
        None
    );
}

#[test]
fn canonical_id_normalizes_mixed_case_builtins_and_presets() {
    let agents = Agents::load(None).unwrap();
    for (input, expected) in [
        ("Claude", "claude"),
        ("CODEX", "codex"),
        ("Agy", "agy"),
        ("CURSOR", "cursor"),
        ("OpenCode", "opencode"),
        ("Grok", "grok"),
        ("MUSE", "muse"),
        ("Mistral-Vibe", "mistral-vibe"),
        ("OpenCode2", "opencode2"),
        ("Pi", "pi"),
        ("OMP", "omp"),
    ] {
        assert_eq!(canonical_id(input), expected, "{input}");
        let (resolved, _) = agents.resolve(input).unwrap();
        assert_eq!(resolved, expected, "{input}");
    }
    assert_eq!(canonical_id("  CustomAgent  "), "CustomAgent");

    let mut claude = agents.profiles["claude"].clone();
    apply_launch_overrides(
        "Claude",
        &mut claude,
        &LaunchOverrides {
            model: Some("opus".to_owned()),
            effort: Some("high".to_owned()),
        },
    )
    .unwrap();
    assert_eq!(
        claude.argv(Permission::Manual),
        ["claude", "--model", "opus", "--effort", "high"]
    );
}

#[cfg(unix)]
#[test]
fn capture_rejects_nonzero_exit_bounds_stdout_and_times_out_cleanly() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = std::env::temp_dir().join(format!("krill-agent-capture-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let script = |name: &str, body: &str| {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };

    // Broken CLI prints error prose to stdout and exits non-zero -> must return None.
    let err_prose = script(
        "err-prose",
        "echo 'error: unknown option --version'; exit 1",
    );
    assert_eq!(version(&err_prose), None);
    assert_eq!(
        capture(Command::new(&err_prose), Duration::from_secs(2)),
        None
    );

    // Large stdout exceeding 64 KiB with multibyte char split across the 64 KiB boundary still succeeds boundedly.
    let filler = "a".repeat(65_529);
    let large = script(
        "large",
        &format!(
            "printf '1.2.3\\n%s가%s' '{filler}' '{}'",
            "b".repeat(8 * 1024)
        ),
    );
    let out = capture(Command::new(&large), Duration::from_secs(5)).unwrap();
    assert!(out.starts_with("1.2.3\n"));
    assert!(
        out.len() <= 64 * 1024 + 4,
        "bounded capture was {}B",
        out.len()
    );
    assert_eq!(version(&large).as_deref(), Some("1.2.3"));

    // Unbounded stdout flood must time out cleanly without OOM.
    let flood = script("flood", "while :; do printf '0123456789abcdef\\n'; done");
    let started = std::time::Instant::now();
    assert_eq!(
        capture(Command::new(&flood), Duration::from_millis(150)),
        None
    );
    assert!(started.elapsed() < Duration::from_secs(3));

    // Closing stdout early while child hangs must still time out and return None.
    let close_hang = script("close-hang", "exec >&-; exec sleep 30");
    let started = std::time::Instant::now();
    assert_eq!(
        capture(Command::new(&close_hang), Duration::from_millis(150)),
        None
    );
    assert!(started.elapsed() < Duration::from_secs(3));

    std::fs::remove_dir_all(&dir).ok();
}
