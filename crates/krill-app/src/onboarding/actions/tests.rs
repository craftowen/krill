use super::*;

#[test]
fn every_supported_check_routes_to_an_app_action() {
    for id in [
        "bin.helpers",
        "cli.path",
        "hook.claude",
        "hook.codex",
        "hook.cursor",
        "hook.agy",
        "hook.opencode",
        "app.notifications",
        "app.folders",
        "app.full_disk",
        "app.automation",
        "app.location",
    ] {
        assert!(action_for(id).is_some(), "missing action: {id}");
    }
    assert!(action_for("hook.unknown").is_none());
    assert!(action_for("curl something | sh").is_none());
}

fn fixture(name: &str) -> (PathBuf, Env) {
    use std::os::unix::fs::PermissionsExt as _;
    let root = std::env::temp_dir().join(format!("owe714-actions-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(root.join("source")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    for bin in ["krill", "krill-holder", "krilld"] {
        let path = root.join("source").join(bin);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let env = Env {
        shell: "/bin/false".into(),
        krill: root.join("source/krill"),
        state: root.join("state"),
        home: root.join("home"),
        state_overridden: true,
    };
    (root, env)
}

#[test]
fn missing_helper_is_not_a_successful_repair() {
    let (root, env) = fixture("missing");
    std::fs::remove_file(root.join("source/krilld")).unwrap();
    assert!(stage_helpers(&env).is_err());
    assert!(!env.home.join(".local/share/krill/bin/krill").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn managed_cli_preserves_existing_regular_file() {
    let (root, env) = fixture("conflict");
    std::fs::create_dir_all(env.home.join(".local/bin")).unwrap();
    let link = env.home.join(".local/bin/krill");
    std::fs::write(&link, "user-owned").unwrap();
    // 호스트 PATH(install_managed_cli)를 쓰면 쓰기 가능한 /opt/homebrew/bin(CI 러너)에 실제로 설치돼
    // 첫 시도는 통과(Ok)하고 재시도는 그 잔재와 충돌해 FLAKY가 됐다(2026-10-18 macos-14·15-intel).
    assert!(install_managed_cli_at(&env, env.home.join(".local/bin").as_os_str()).is_err());
    assert_eq!(std::fs::read_to_string(link).unwrap(), "user-owned");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cli_link_is_idempotent_and_never_uses_system_bin() {
    let (root, env) = fixture("idempotent");
    let managed = env.home.join(".local/bin");
    let path = std::env::join_paths([Path::new("/usr/bin"), &managed]).unwrap();
    install_managed_cli_at(&env, &path).unwrap();
    install_managed_cli_at(&env, &path).unwrap();
    assert_eq!(
        std::fs::read_link(managed.join("krill")).unwrap(),
        env.home.join(".local/share/krill/bin/krill")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn managed_directory_symlink_escape_and_foreign_link_are_preserved() {
    let (root, env) = fixture("symlinks");
    std::fs::create_dir_all(root.join("outside")).unwrap();
    std::os::unix::fs::symlink(root.join("outside"), env.home.join(".local")).unwrap();
    assert!(stage_helpers(&env).is_err());
    assert!(install_managed_cli_at(&env, env.home.join(".local/bin").as_os_str()).is_err());
    assert!(!root.join("outside/share").exists());
    std::fs::remove_file(env.home.join(".local")).unwrap();
    std::fs::create_dir_all(env.home.join(".local/bin")).unwrap();
    let link = env.home.join(".local/bin/krill");
    std::os::unix::fs::symlink(root.join("outside/user-tool"), &link).unwrap();
    assert!(install_managed_cli_at(&env, env.home.join(".local/bin").as_os_str()).is_err());
    assert_eq!(
        std::fs::read_link(link).unwrap(),
        root.join("outside/user-tool")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn real_hook_plan_writes_only_after_review_and_preserves_other_settings() {
    let (root, env) = fixture("hook-plan");
    let config = env.home.join(".cursor/hooks.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "{\"user_setting\":42}").unwrap();
    let Prepared::Review { review, diff, .. } = SystemExecutor
        .prepare(FixAction::Hook(TargetAgent::Other(Agent::Cursor)), &env)
        .unwrap()
    else {
        panic!("review required");
    };
    assert!(!env.state.exists(), "preparation must not write a receipt");
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "{\"user_setting\":42}"
    );
    assert!(diff.contains("user_setting"));
    SystemExecutor.apply(review).unwrap();
    assert!(hook_installed(&env.state, "cursor"));
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
    assert_eq!(json["user_setting"], 42);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn hook_plan_detects_changes_after_review() {
    let (root, env) = fixture("hook-conflict");
    let config = env.home.join(".cursor/hooks.json");
    let Prepared::Review { review, .. } = SystemExecutor
        .prepare(FixAction::Hook(TargetAgent::Other(Agent::Cursor)), &env)
        .unwrap()
    else {
        panic!("review required");
    };
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, "{\"edited\":true}").unwrap();
    assert!(SystemExecutor.apply(review).is_err());
    assert_eq!(
        std::fs::read_to_string(config).unwrap(),
        "{\"edited\":true}"
    );
    assert!(!hook_installed(&env.state, "cursor"));
    std::fs::remove_dir_all(root).unwrap();
}

#[derive(Clone, Default)]
struct FakeExecutor {
    calls: Arc<std::sync::Mutex<Vec<FixAction>>>,
    applied: Arc<std::sync::atomic::AtomicUsize>,
    failure: Option<String>,
}
impl Executor for FakeExecutor {
    type Review = ();
    fn prepare(&self, action: FixAction, _: &Env) -> Result<Prepared<()>, FixError> {
        self.calls.lock().unwrap().push(action);
        if let Some(error) = &self.failure {
            return Err(error.clone().into());
        }
        if let FixAction::Hook(agent) = action {
            Ok(Prepared::Review {
                agent,
                diff: "+reviewed hook".to_owned(),
                review: (),
            })
        } else {
            Ok(Prepared::Done)
        }
    }
    fn apply(&self, (): ()) -> FixOutcome {
        self.applied
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

fn collect_result<T: 'static>(
    task: Task<T>,
    cx: &mut App,
) -> std::rc::Rc<std::cell::RefCell<Option<T>>> {
    let result = std::rc::Rc::new(std::cell::RefCell::new(None));
    let output = result.clone();
    cx.spawn(async move |_| {
        *output.borrow_mut() = Some(task.await);
    })
    .detach();
    result
}

#[gpui::test]
fn fake_executor_routes_each_noninteractive_fix_and_returns_failure_cause(
    cx: &mut gpui::TestAppContext,
) {
    let (root, env) = fixture("fake-actions");
    let executor = FakeExecutor::default();
    for id in [
        "bin.helpers",
        "cli.path",
        "app.notifications",
        "app.folders",
        "app.full_disk",
        "app.automation",
        "app.location",
    ] {
        let result = cx.update(|cx| {
            let task = run_with(id, &env, executor.clone(), cx);
            collect_result(task, cx)
        });
        cx.run_until_parked();
        assert_eq!(*result.borrow(), Some(Ok(())), "{id}");
        assert_eq!(
            executor.calls.lock().unwrap().last().copied(),
            action_for(id)
        );
    }
    let executor = FakeExecutor {
        failure: Some("fixture failure".to_owned()),
        ..FakeExecutor::default()
    };
    let result = cx.update(|cx| {
        let task = run_with("bin.helpers", &env, executor, cx);
        collect_result(task, cx)
    });
    cx.run_until_parked();
    assert_eq!(
        *result.borrow(),
        Some(Err(FixError::Failed("fixture failure".to_owned())))
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn hook_consent_cancellation_never_applies_and_acceptance_applies_once(
    cx: &mut gpui::TestAppContext,
) {
    let (root, env) = fixture("fake-consent");
    let executor = FakeExecutor::default();
    let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
    cx.update(|window, _| window.activate_window());
    for accepted in [false, true] {
        let result = cx.update(|_, cx| {
            let task = run_with("hook.cursor", &env, executor.clone(), cx);
            collect_result(task, cx)
        });
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        assert_eq!(
            executor.applied.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        cx.simulate_prompt_answer(if accepted {
            text::INSTALL.get()
        } else {
            atext::CANCEL.get()
        });
        cx.run_until_parked();
        if accepted {
            assert_eq!(*result.borrow(), Some(Ok(())));
        } else {
            assert_eq!(*result.borrow(), Some(Err(FixError::Cancelled)));
        }
    }
    assert_eq!(
        executor.applied.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn hook_needed_banner_uses_display_name() {
    for (id, name) in [
        ("claude", "Claude Code"),
        ("codex", "Codex"),
        ("agy", "Antigravity"),
        ("opencode", "OpenCode"),
    ] {
        let agent = TargetAgent::parse(id).unwrap_or_else(|| panic!("parse {id}"));
        let (check_id, message) = hook_needed_content(agent);
        assert_eq!(check_id, format!("hook.{id}"));
        assert_eq!(message, atext::HOOK_NEEDED.format(&[("agent", name)]));
    }
}

#[gpui::test]
fn only_real_permission_denial_publishes_a_deduplicated_need(cx: &mut gpui::TestAppContext) {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let path = home.join("Documents/private.txt");
    cx.update(|cx| {
        note_access_denied(&path, &io::Error::from(io::ErrorKind::NotFound), cx);
        assert!(cx.try_global::<FixNeeds>().is_none());
        for _ in 0..2 {
            note_access_denied(&path, &io::Error::from(io::ErrorKind::PermissionDenied), cx);
        }
        assert_eq!(cx.global::<FixNeeds>().items.len(), 1);
        assert_eq!(cx.global::<FixNeeds>().items[0].check_id, perm::FOLDERS_ID);
        dismiss_need(perm::FOLDERS_ID, cx);
        assert!(cx.global::<FixNeeds>().items.is_empty());
    });
}

/// 로그인 셸 PATH에 `cursor-agent`만 보이는 픽스처. 돌려주는 값은 그 PATH다.
fn with_cursor_on_login_path(root: &Path, env: &mut Env) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt as _;
    let agents = root.join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    let cursor = agents.join("cursor-agent");
    std::fs::write(&cursor, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&cursor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let shell = root.join("shell");
    std::fs::write(
        &shell,
        format!(
            "#!/bin/sh\nprintf '__MK_PATH__=%s:%s\\n' '{}' '{}'\n",
            env.home.join(".local/bin").display(),
            agents.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    env.shell = shell.into_os_string();
    std::env::join_paths([env.home.join(".local/bin"), agents]).unwrap()
}

#[gpui::test]
fn automatic_setup_installs_hooks_only_for_agents_on_login_path(cx: &mut gpui::TestAppContext) {
    let (root, mut env) = fixture("auto");
    // 로그인 셸 PATH에 cursor-agent만 있다. 나머지 에이전트 훅은 건드리지 않아야 한다.
    let path = with_cursor_on_login_path(&root, &mut env);
    let home = env.home.clone();
    let link = env.home.join(".local/bin/krill");
    let state = env.state.clone();
    let kept = env.clone();
    let result = cx.update(|cx| {
        let task = auto_prepare(env, cx);
        collect_result(task, cx)
    });
    cx.run_until_parked();
    assert_eq!(*result.borrow(), Some(Ok(())));
    assert!(link.is_file());
    assert!(home.join(".claude/skills/krill/SKILL.md").is_file());
    assert!(
        home.join(".codex/skills/krill-orchestration/SKILL.md")
            .is_file()
    );
    assert!(
        home.join(".gemini/config/skills/worktree-checkpoints/SKILL.md")
            .is_file()
    );
    assert!(state.join("hooks/cursor.json").is_file());
    assert!(
        std::fs::read_to_string(home.join(".cursor/hooks.json"))
            .unwrap()
            .contains("hook cursor")
    );
    assert!(!state.join("hooks/claude.json").exists());
    assert!(!state.join("hooks/codex.json").exists());
    assert!(!home.join(".gemini/config/hooks.json").exists());
    cx.update(|cx| assert!(cx.try_global::<FixNeeds>().is_none()));
    assert!(!cx.has_pending_prompt());

    // 설정에서 끄면 Krill이 넣은 훅과 영수증을 되돌린다.
    uninstall_default_hooks(&kept, &path);
    assert!(!state.join("hooks/cursor.json").exists());
    assert!(
        !std::fs::read_to_string(home.join(".cursor/hooks.json"))
            .unwrap_or_default()
            .contains("hook cursor")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn automatic_setup_skips_hooks_when_disabled_in_settings(cx: &mut gpui::TestAppContext) {
    let (root, mut env) = fixture("auto-off");
    with_cursor_on_login_path(&root, &mut env);
    std::fs::create_dir_all(&env.state).unwrap();
    std::fs::write(
        env.state.join("settings.toml"),
        "agent_status_hooks = false\n",
    )
    .unwrap();
    let home = env.home.clone();
    let state = env.state.clone();
    let result = cx.update(|cx| {
        let task = auto_prepare(env, cx);
        collect_result(task, cx)
    });
    cx.run_until_parked();
    assert_eq!(*result.borrow(), Some(Ok(())));
    assert!(!state.join("hooks").exists());
    assert!(!home.join(".cursor/hooks.json").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn only_previously_denied_access_is_rechecked_on_return(cx: &mut gpui::TestAppContext) {
    let (root, _) = fixture("recheck");
    let path = root.join("private-file");
    std::fs::write(&path, "readable after settings").unwrap();
    cx.update(|cx| {
        recheck_needed(cx).detach();
        assert!(cx.try_global::<AccessAttempts>().is_none());
        note_access_denied(&path, &io::Error::from(io::ErrorKind::PermissionDenied), cx);
        assert_eq!(cx.global::<FixNeeds>().items.len(), 1);
        recheck_needed(cx).detach();
    });
    cx.run_until_parked();
    cx.update(|cx| assert!(cx.global::<FixNeeds>().items.is_empty()));
    std::fs::remove_dir_all(root).unwrap();
}

#[gpui::test]
fn unsupported_agent_does_not_create_a_hook_need(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        note_agent_session("not-a-supported-agent", cx);
        assert!(cx.try_global::<FixNeeds>().is_none());
    });
}

#[test]
fn translocation_fix_prepares_move_guidance_without_opening_finder() {
    let (root, env) = fixture("location");
    assert!(matches!(
        SystemExecutor
            .prepare(FixAction::Applications, &env)
            .unwrap(),
        Prepared::Guidance
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cli_conflict_earlier_on_path_is_preserved() {
    let (root, env) = fixture("path-conflict");
    let foreign = root.join("foreign");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("krill"), "another program").unwrap();
    let path = std::env::join_paths([foreign.clone(), env.home.join(".local/bin")]).unwrap();
    assert!(install_managed_cli_at(&env, &path).is_err());
    assert_eq!(
        std::fs::read_to_string(foreign.join("krill")).unwrap(),
        "another program"
    );
    assert!(!env.home.join(".local/bin/krill").exists());
    std::fs::remove_dir_all(root).unwrap();
}

// OWE-714 D2: 설치 계획의 기존 설정 충돌은 실패가 아니라 한글 안내 + 원문(펼침)이다.
#[test]
fn hook_plan_conflict_is_guidance_not_failure() {
    let conflict = io::Error::new(
        io::ErrorKind::AlreadyExists,
        "OpenCode plugin path already exists; refusing to overwrite",
    );
    let FixError::Conflict { guide, detail } =
        hook_prepare_error(TargetAgent::Other(Agent::OpenCode), &conflict)
    else {
        panic!("conflict must be guidance");
    };
    assert_eq!(guide, atext::HOOK_CONFLICT.format(&[("name", "OpenCode")]));
    assert_eq!(detail, conflict.to_string());
    assert!(matches!(
        hook_prepare_error(TargetAgent::Other(Agent::Codex), &io::Error::other("boom")),
        FixError::Failed(_)
    ));
}
