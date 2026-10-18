use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use gpui::{TestAppContext, VisualTestContext};

use super::*;

/// 새 계정 흉내: 빈 상태 디렉터리·빈 HOME, 로그인 셸 PATH에만 git·claude·codex가 있다.
struct Fake {
    root: PathBuf,
    env: Env,
}

impl Drop for Fake {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).ok();
    }
}

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn ck(
    id: &str,
    level: &str,
    status: &str,
    summary: &str,
    detail: &str,
    kind: &str,
    value: &str,
) -> serde_json::Value {
    serde_json::json!({
        "id": id, "level": level, "status": status,
        "summary": summary, "detail": detail,
        "fix": {"kind": kind, "value": value},
    })
}

/// 가짜 `krill doctor --json --no-start` 응답을 적는다.
fn write_doctor(root: &Path, checks: &[serde_json::Value]) {
    let report = serde_json::json!({"schema": 1, "checks": checks});
    fs::write(
        root.join("doctor.json"),
        serde_json::to_string(&report).unwrap(),
    )
    .unwrap();
}

/// 기본 canned 응답: 필수 전부 ok, `hook.codex` 미설치 warn, `cli.path` 제안 warn,
/// `git` 없음 warn(복사 버튼 테스트용).
fn default_checks(link: &str) -> Vec<serde_json::Value> {
    vec![
        ck(
            "bin.helpers",
            "required",
            "ok",
            "krill-holder and krilld are present",
            "staged",
            "none",
            "",
        ),
        ck(
            "bin.version",
            "required",
            "ok",
            "binaries are the same version",
            "0.1.1",
            "none",
            "",
        ),
        ck(
            "state.writable",
            "required",
            "ok",
            "state directory is writable",
            ".",
            "none",
            "",
        ),
        ck(
            "daemon.connect",
            "required",
            "ok",
            "connected to krilld",
            "instance 1",
            "none",
            "",
        ),
        ck(
            "history.key",
            "required",
            "ok",
            "segment key is accessible",
            "no recording warning",
            "none",
            "",
        ),
        ck(
            "shell.path",
            "required",
            "ok",
            "login shell PATH resolved",
            "fake",
            "none",
            "",
        ),
        ck(
            "git",
            "recommended",
            "warn",
            "git is not installed",
            "install Xcode Command Line Tools",
            "copy",
            "xcode-select --install",
        ),
        ck(
            "agent.any",
            "recommended",
            "ok",
            "2 agent(s) found",
            "Claude Code, Codex",
            "none",
            "",
        ),
        ck(
            "agent.codex",
            "recommended",
            "ok",
            "Codex detected",
            "codex-cli 0.130.0",
            "none",
            "",
        ),
        ck(
            "hook.codex",
            "recommended",
            "warn",
            "codex hook is not installed",
            "receipt not found",
            "command",
            "krill hook install codex",
        ),
        ck(
            "hook.opencode",
            "recommended",
            "skip",
            "opencode agent is not installed",
            "hook check needs the agent installed",
            "none",
            "",
        ),
        ck(
            "hook.cursor",
            "recommended",
            "skip",
            "cursor agent is not installed",
            "hook check needs the agent installed",
            "none",
            "",
        ),
        ck(
            "cli.path",
            "recommended",
            "warn",
            "krill is not on PATH",
            link,
            "command",
            link,
        ),
    ]
}

fn fake(name: &str) -> Fake {
    let root = std::env::temp_dir().join(format!("mk-onboard-{}-{name}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let (bin, home) = (root.join("bin"), root.join("home"));
    let state = default_state_dir(&home);
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(home.join(".local/bin")).unwrap();
    script(&bin.join("git"), "echo 'git version 2.50.1'\n");
    script(&bin.join("claude"), "echo '2.1.0 (Claude Code)'\n");
    script(&bin.join("codex"), "echo 'codex-cli 0.130.0'\n");
    // rc 파일이 stdout에 찍는 잡음까지 흉내 낸다.
    script(
        &root.join("shell"),
        &format!(
            "PATH='{}:{}'\nexport PATH\necho 'welcome from .zshrc'\nfor a; do c=$a; done\nexec /bin/sh -c \"$c\"\n",
            bin.display(),
            home.join(".local/bin").display()
        ),
    );
    for helper in ["krill-holder", "krilld"] {
        script(&root.join(helper), "exit 0");
    }
    let krill = root.join("krill");
    script(
        &krill,
        &format!(
            "if [ \"$1\" = doctor ]; then cat '{root}/doctor.json'; exit 0; fi\necho \"$* PATH=$PATH\" >> '{log}'\ncase \"$4\" in\n--dry-run) i=0; while [ $i -lt 40 ]; do echo \"+line $i\"; i=$((i+1)); done ;;\n--yes) /bin/mkdir -p '{state}/hooks'; : > '{state}/hooks/'\"$3\".json ;;\nesac\n",
            root = root.display(),
            log = root.join("krill.log").display(),
            state = state.display()
        ),
    );
    write_doctor(
        &root,
        &default_checks(&home.join(".local/bin/krill").display().to_string()),
    );
    Fake {
        env: Env {
            shell: root.join("shell").into(),
            krill,
            state,
            home,
            state_overridden: false,
        },
        root,
    }
}

fn wizard_window(cx: &TestAppContext) -> Option<gpui::WindowHandle<welcome::Wizard>> {
    cx.windows()
        .into_iter()
        .find_map(|w| w.downcast::<welcome::Wizard>())
}

fn wizard_checks(cx: &mut VisualTestContext) -> Option<Vec<Check>> {
    let w = wizard_window(cx).expect("wizard window");
    w.update(&mut cx.cx, |v, _, _| v.checks.clone()).unwrap()
}

/// 백그라운드 수집이 끝날 때까지 돌린다.
fn wait_checks(cx: &mut VisualTestContext) -> Vec<Check> {
    for _ in 0..200 {
        cx.run_until_parked();
        if let Some(checks) = wizard_checks(cx) {
            return checks;
        }
    }
    panic!("checks never arrived");
}

/// 마법사 창에 키를 누른다(enter = 주 버튼, escape = 나중에).
fn press(cx: &mut VisualTestContext, key: &str) {
    let w = wizard_window(cx).expect("wizard window");
    cx.cx.simulate_keystrokes(w.into(), key);
    cx.run_until_parked();
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let w = wizard_window(cx).expect("wizard window");
    let mut wcx = VisualTestContext::from_window(w.into(), &cx.cx);
    let at = wcx.debug_bounds(selector).expect(selector).center();
    wcx.simulate_click(at, gpui::Modifiers::default());
    wcx.run_until_parked();
}

/// 마법사 창에 셀렉터가 보이면 true.
fn present(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    let w = wizard_window(cx).expect("wizard window");
    let mut wcx = VisualTestContext::from_window(w.into(), &cx.cx);
    wcx.run_until_parked();
    wcx.debug_bounds(selector).is_some()
}

/// 파일 조건이 찰 때까지 돌린다(백그라운드 설치·스테이징용).
fn wait_for_file(cx: &mut VisualTestContext, path: &Path, mut done: impl FnMut() -> bool) {
    for _ in 0..200 {
        cx.run_until_parked();
        if done() {
            return;
        }
    }
    panic!("{} never ready", path.display());
}

/// 첫 실행 진입(`offer`)을 부른다. 모달이 뜨면 테스트 호스트가 창으로 연다.
fn open<'a>(cx: &'a mut TestAppContext, env: &Env) -> &'a mut VisualTestContext {
    // 마법사는 gpui-component 버튼을 쓴다(앱은 시작 때 초기화한다).
    cx.update(gpui_component::init);
    cx.update(register_test_window_host);
    let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
    let env = env.clone();
    cx.update(|_, cx| offer(env, cx));
    cx.run_until_parked();
    cx
}

/// 마법사 값 하나를 읽는다.
fn wizard_read<T>(
    cx: &mut VisualTestContext,
    f: impl FnOnce(&welcome::Wizard, &gpui::App) -> T,
) -> T {
    let w = wizard_window(cx).expect("wizard window");
    w.update(&mut cx.cx, |v, _, cx| f(v, cx)).unwrap()
}

/// 백그라운드 감지(survey)가 끝날 때까지 돌린다.
fn wait_survey(cx: &mut VisualTestContext) {
    for _ in 0..200 {
        cx.run_until_parked();
        if wizard_read(cx, |v, _| v.survey.is_some()) {
            return;
        }
    }
    panic!("survey never arrived");
}

/// 설정 점검 화면(재진입)을 열고 점검 목록을 기다린다.
fn open_checks_screen<'a>(cx: &'a mut TestAppContext, env: &Env) -> &'a mut VisualTestContext {
    cx.update(gpui_component::init);
    cx.update(register_test_window_host);
    let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
    let env = env.clone();
    cx.update(|_, cx| open_checks(&env, cx));
    cx.run_until_parked();
    wait_checks(cx);
    wait_survey(cx);
    cx
}

/// OWE-903 테스트 호스트 진입점. 실구현은 [`super::register_test_window_host`]다.
fn register_test_host(cx: &mut gpui::App) {
    register_test_window_host(cx);
}

// OWE-904: 첫 실행은 모달 없이 완료를 남기고 hook·CLI를 묻지 않는다. 다시 불러도 같다.
#[gpui::test]
fn first_run_opens_no_modal_and_completes(cx: &mut TestAppContext) {
    let f = fake("first-run");
    let cx = open(cx, &f.env);
    assert!(wizard_window(cx).is_none(), "first run opens no modal");
    let stored = state::load(&f.env.state);
    assert!(stored.completed);
    assert_eq!(stored.step, state::Step::Done);
    assert!(!f.env.state.join("consent.toml").exists());
    assert!(!f.root.join("krill.log").exists());

    cx.update(|_, cx| offer(f.env.clone(), cx));
    cx.run_until_parked();
    assert!(wizard_window(cx).is_none());
}

// OWE-903: 온보딩은 별도 OS 창 대신 메인 창 안 중앙 모달(넓은 모달 규격)로 뜬다.
#[gpui::test]
fn wizard_renders_as_in_window_modal_with_settings_modal_width(cx: &mut TestAppContext) {
    let f = fake("wizard-modal");
    let cx = open_checks_screen(cx, &f.env);
    let w = wizard_window(cx).expect("wizard window");
    let mut wcx = VisualTestContext::from_window(w.into(), &cx.cx);
    wcx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(900.)));
    wcx.run_until_parked();
    let card = wcx.debug_bounds("onboarding-wizard").expect("modal card");
    assert!(
        (f32::from(card.size.width) - f32::from(crate::ui::SETTINGS_MODAL_WIDTH)).abs() < 1.,
        "modal card must use SETTINGS_MODAL_WIDTH: {card:?}"
    );
    // 가짜 타이틀바는 그리지 않는다(메인 창의 것을 쓴다).
    assert!(wcx.debug_bounds("onboarding-titlebar").is_none());
}

// OWE-861: 점검 창(온보딩 마법사)은 ⌘W로 닫힌다(Esc·⌥⌘W와 별개 경로).
#[gpui::test]
fn wizard_cmd_w_closes_window(cx: &mut TestAppContext) {
    let f = fake("wizard-cmd-w");
    cx.update(|cx| {
        gpui_component::init(cx);
        init_actions(cx);
    });
    let cx = open_checks_screen(cx, &f.env);
    assert!(wizard_window(cx).is_some(), "wizard opens");
    press(cx, "cmd-w");
    assert!(
        wizard_window(cx).is_none(),
        "cmd-w closes the checks window"
    );
}

// 필수 실패는 점검 화면 위에 막힘 배너로 보이고, 실패 행은 펼친 채다.
#[gpui::test]
fn checks_screen_shows_required_fail_expanded(cx: &mut TestAppContext) {
    let f = fake("wizard-blocked");
    write_doctor(
        &f.root,
        &[ck(
            "bin.helpers",
            "required",
            "fail",
            "krill-holder or krilld is missing or not executable",
            "krilld missing or not executable (reinstall Krill)",
            "reinstall",
            "",
        )],
    );
    let cx = open_checks_screen(cx, &f.env);
    assert!(present(cx, "wizard-blocked"));
    // 실패 행은 원인·다음 행동을 펼친 채로 보여 준다(OWE-714 (6)).
    assert!(present(cx, "wizard-detail-bin.helpers"));
    press(cx, "escape");
    assert!(wizard_window(cx).is_none());
}

#[gpui::test]
fn wizard_copy_button_copies_command_fix(cx: &mut TestAppContext) {
    let f = fake("wizard-copy");
    // 복사 버튼이 창 안에 들어오도록 점검은 2건만 둔다.
    write_doctor(
        &f.root,
        &[
            ck(
                "bin.helpers",
                "required",
                "ok",
                "helpers present",
                "staged",
                "none",
                "",
            ),
            ck(
                "git",
                "recommended",
                "warn",
                "git is not installed",
                "install Xcode Command Line Tools",
                "copy",
                "xcode-select --install",
            ),
        ],
    );
    let cx = open_checks_screen(cx, &f.env);
    click(cx, "wizard-fix-git");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("xcode-select --install")
    );
}

/// hook·CLI 행만 둔 점검(버튼이 창 안에 들어오게).
fn connect_checks(link: &str) -> Vec<serde_json::Value> {
    vec![
        ck(
            "hook.codex",
            "recommended",
            "warn",
            "codex hook is not installed",
            "receipt not found",
            "command",
            "krill hook install codex",
        ),
        ck(
            "cli.path",
            "recommended",
            "warn",
            "krill is not on PATH",
            link,
            "command",
            link,
        ),
    ]
}

// OWE-714: hook은 점검 행에서 앱 안에서 설치한다(diff 확인 → 설치 확정).
#[gpui::test]
fn checks_screen_installs_hook_and_cli_in_app(cx: &mut TestAppContext) {
    let f = fake("checks-fix");
    let link = f.env.home.join(".local/bin/krill");
    write_doctor(&f.root, &connect_checks(&link.display().to_string()));
    let cx = open_checks_screen(cx, &f.env);

    // 실제 proto 계획은 JSON만 쓰는 Cursor로 검증한다(Codex는 별도 RPC writer).
    let checks = vec![
        ck(
            "hook.cursor",
            "recommended",
            "warn",
            "missing",
            "receipt not found",
            "command",
            "krill hook install cursor",
        ),
        ck(
            "cli.path",
            "recommended",
            "warn",
            "missing",
            "",
            "command",
            "krill install",
        ),
    ];
    write_doctor(&f.root, &checks);
    click(cx, "wizard-recheck");
    wait_checks(cx);
    wizard_window(cx)
        .unwrap()
        .update(&mut cx.cx, |_, window, _| window.activate_window())
        .unwrap();
    click(cx, "wizard-fix-hook.cursor");
    for _ in 0..200 {
        cx.run_until_parked();
        if present(cx, "wizard-hook-confirm-cursor") {
            break;
        }
    }
    assert!(
        !cx.has_pending_prompt(),
        "checks screen must render inline diff preview instead of modal prompt"
    );
    assert!(
        present(cx, "wizard-hook-diff-cursor"),
        "{:?}",
        wizard_read(cx, |w, _| w.fix_errors.clone())
    );
    assert!(present(cx, "wizard-hook-title-cursor"));
    assert!(present(cx, "wizard-hook-cancel-cursor"));
    assert!(
        !f.env.state.join("hooks/cursor.json").exists(),
        "review writes nothing"
    );
    // 재검사는 검토 대기 중인 인라인 diff 상태를 깨뜨리면 안 된다.
    click(cx, "wizard-recheck");
    wait_checks(cx);
    assert!(present(cx, "wizard-hook-confirm-cursor"));

    // 취소하면 아무 파일도 쓰지 않고 오류 배너도 남기지 않는다.
    click(cx, "wizard-hook-cancel-cursor");
    assert!(!present(cx, "wizard-hook-confirm-cursor"));
    assert!(!present(cx, "wizard-fix-error-hook.cursor"));
    assert!(!f.env.state.join("hooks/cursor.json").exists());

    // 다시 눌러 설치 확정을 누르면 실제 훅과 영수증을 기록한다.
    click(cx, "wizard-fix-hook.cursor");
    for _ in 0..200 {
        cx.run_until_parked();
        if present(cx, "wizard-hook-confirm-cursor") {
            break;
        }
    }
    click(cx, "wizard-hook-confirm-cursor");
    wait_for_file(cx, &f.env.state, || {
        f.env.state.join("hooks/cursor.json").exists()
    });
    click(cx, "wizard-fix-cli.path");
    wait_for_file(cx, &link, || fs::symlink_metadata(&link).is_ok());
    assert_eq!(
        fs::read_link(&link).unwrap(),
        f.env.home.join(".local/share/minikrill/bin/krill")
    );
}

#[gpui::test]
fn wizard_missing_binary_shows_helpers_fail(cx: &mut TestAppContext) {
    let f = fake("wizard-missing-krill");
    fs::remove_file(&f.env.krill).unwrap();
    let cx = open_checks_screen(cx, &f.env);
    let checks = wait_checks(cx);
    let helpers = checks.iter().find(|c| c.id == "bin.helpers").unwrap();
    assert_eq!(helpers.status, Status::Fail);
    assert!(
        helpers.detail.contains(ctext::DOCTOR_CAUSE_MISSING.get()),
        "{}",
        helpers.detail
    );
    assert!(present(cx, "wizard-blocked"));
}

#[gpui::test]
fn wizard_isolated_state_skips_cli_symlink(cx: &mut TestAppContext) {
    let mut f = fake("wizard-isolated");
    f.env.state = f.root.join("isolated-mk-state");
    let link = f.env.home.join(".local/bin/krill");

    let cx = open(cx, &f.env);
    assert!(wizard_window(cx).is_none());

    assert!(fs::symlink_metadata(&link).is_err());
    assert!(!f.env.home.join(".local/share/minikrill/bin").exists());
    assert!(state::load(&f.env.state).completed);
}

#[gpui::test]
fn wizard_completed_state_only_refreshes_staging(cx: &mut TestAppContext) {
    let f = fake("wizard-refresh");
    script(&f.root.join("krill-holder"), "echo holder-v1\n");
    script(&f.root.join("krilld"), "echo krilld-v1\n");
    state::save(
        &f.env.state,
        &state::OnboardingState {
            completed: true,
            step: state::Step::Done,
            version: state::STATE_VERSION,
        },
    )
    .unwrap();

    let cx = open(cx, &f.env);
    assert!(wizard_window(cx).is_none());
    // 스테이징은 돌아간다.
    let stable_dir = f.env.home.join(".local/share/minikrill/bin");
    wait_for_file(cx, &stable_dir.join("krilld"), || {
        stable_dir.join("krilld").is_file()
    });
    assert!(
        fs::read_to_string(stable_dir.join("krilld"))
            .unwrap()
            .contains("krilld-v1")
    );

    // 새 빌드로 재실행하면 대화상자 없이 스테이징 바이너리만 갱신한다.
    script(&f.root.join("krilld"), "echo krilld-v2\n");
    cx.update(|_, cx| offer(f.env.clone(), cx));
    let krilld = stable_dir.join("krilld");
    wait_for_file(cx, &krilld, || {
        fs::read_to_string(&krilld).is_ok_and(|b| b.contains("krilld-v2"))
    });
    assert!(wizard_window(cx).is_none());
}

#[gpui::test]
fn wizard_legacy_done_skips_wizard(cx: &mut TestAppContext) {
    let f = fake("wizard-migrated");
    fs::create_dir_all(&f.env.state).unwrap();
    fs::write(f.env.state.join(DONE_FILE), b"").unwrap();

    let cx = open(cx, &f.env);
    assert!(wizard_window(cx).is_none());
    assert!(state::load(&f.env.state).completed);
}

#[test]
fn cli_never_shadows_another_mk() {
    let f = fake("cli");
    let bin = f.root.join("bin");
    let path = std::env::join_paths([&bin, &f.env.home.join(".local/bin")]).unwrap();
    assert_eq!(
        cli(&f.env.krill, &path, &f.env.home),
        Cli::Offer(f.env.home.join(".local/bin/krill"))
    );
    // Homebrew 코어의 같은 이름 포뮬러 같은 다른 krill.
    script(&bin.join("krill"), "exit 0\n");
    assert_eq!(
        cli(&f.env.krill, &path, &f.env.home),
        Cli::Conflict(bin.join("krill"))
    );
    fs::remove_file(bin.join("krill")).unwrap();
    std::os::unix::fs::symlink(&f.env.krill, bin.join("krill")).unwrap();
    assert_eq!(cli(&f.env.krill, &path, &f.env.home), Cli::Installed);
    // PATH에 사용자 소유 후보 디렉터리가 없으면 수동 안내.
    assert_eq!(
        cli(&f.env.krill, OsStr::new("/usr/bin"), &f.env.home),
        Cli::Manual
    );
}

#[test]
fn login_path_skips_rc_noise_and_failures() {
    let f = fake("path");
    let path = login_path(&f.env.shell).unwrap();
    assert!(
        path.to_string_lossy()
            .starts_with(&*f.root.join("bin").to_string_lossy())
    );
    assert_eq!(login_path(OsStr::new("/nonexistent/shell")), None);
}

/// AS-14: stdout 파이프를 쥔 좀비 자식이 있어도 리더 스레드는 유예 안에
/// 반드시 끝난다(호출자는 타임아웃에 `None`으로 먼저 복귀한다).
#[test]
fn login_path_hanging_pipe_reader_thread_always_exits() {
    use std::time::Instant;
    let root = std::env::temp_dir().join(format!("mk-onboard-hang-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    // 파이프를 쥐고 잠드는 백그라운드를 남기고 먼저 끝나는 셸.
    // 잠든 `sleep`은 테스트 뒤 저절로 끝나고 실제 사용자 경로를 건드리지 않는다.
    let shell = root.join("hang-shell");
    script(
        &shell,
        "(sleep 25 &)\nfor a; do c=$a; done\nexec /bin/sh -c \"$c\"\n",
    );
    let started = Instant::now();
    assert_eq!(login_path(shell.as_os_str()), None);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "caller must give up at SHELL_TIMEOUT"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while login_path_reader_count() > 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(login_path_reader_count(), 0, "reader thread leaked");
    fs::remove_dir_all(&root).ok();
}

#[gpui::test]
fn foreign_broken_symlink_is_preserved_and_repair_reports_conflict(cx: &mut TestAppContext) {
    let f = fake("broken-link");
    let link = f.env.home.join(".local/bin/krill");
    std::os::unix::fs::symlink(f.root.join("vanished-krill"), &link).unwrap();
    let path = login_path(&f.env.shell).unwrap();
    assert_eq!(
        cli(&f.env.krill, &path, &f.env.home),
        Cli::Conflict(link.clone())
    );
    write_doctor(&f.root, &connect_checks(&link.display().to_string()));
    let cx = open_checks_screen(cx, &f.env);
    click(cx, "wizard-fix-cli.path");
    cx.run_until_parked();
    assert_eq!(fs::read_link(&link).unwrap(), f.root.join("vanished-krill"));
    assert!(wizard_read(cx, |w, _| w
        .fix_errors
        .contains_key("cli.path")));
}

#[test]
fn broken_symlink_summary_offers_replacement_instead_of_conflict() {
    let f = fake("summary-broken");
    let link = f.env.home.join(".local/bin/krill");
    std::os::unix::fs::symlink(f.root.join("vanished-krill"), &link).unwrap();
    let s = survey(&f.env);
    assert!(
        matches!(&s.cli, Cli::Conflict(p) if p == &link),
        "{:?}",
        s.cli
    );
    // 끊긴 심링크는 교체 설치 대상이다.
    assert!(is_replaceable_broken_symlink(&link));
}

#[gpui::test]
fn isolated_state_dir_skips_cli_symlink_step(cx: &mut TestAppContext) {
    let mut f = fake("explicit-state-override");
    f.env.state_overridden = true;
    assert_eq!(f.env.state, default_state_dir(&f.env.home));
    let cx = open(cx, &f.env);
    assert!(wizard_window(cx).is_none());
    // 격리 상태에서는 CLI 링크·스테이징을 만들지 않는다.
    assert!(fs::symlink_metadata(f.env.home.join(".local/bin/krill")).is_err());
    assert!(!f.env.home.join(".local/share/minikrill/bin").exists());
    assert!(state::load(&f.env.state).completed);
}

#[gpui::test]
fn stage_runs_before_hook_records_stable_path_and_refreshes_after_done(cx: &mut TestAppContext) {
    let f = fake("stage-before-hook");
    let exe_log = f.root.join("mk-exe.log");
    script(
        &f.env.krill,
        &format!(
            "if [ \"$1\" = doctor ]; then cat '{root}/doctor.json'; exit 0; fi\necho \"$0|$*\" >> '{exe_log}'\ncase \"$4\" in\n--dry-run) echo '+hook' ;;\n--yes) /bin/mkdir -p '{state}/hooks'; : > '{state}/hooks/'\"$3\".json ;;\nesac\n",
            root = f.root.display(),
            exe_log = exe_log.display(),
            state = f.env.state.display()
        ),
    );
    script(&f.root.join("krill-holder"), "echo holder-v1\n");
    script(&f.root.join("krilld"), "echo krilld-v1\n");
    write_doctor(
        &f.root,
        &connect_checks(&f.env.home.join(".local/bin/krill").display().to_string()),
    );

    // hook 명령은 스테이징된 안정 경로를 기록해야 한다.
    write_doctor(
        &f.root,
        &[ck(
            "hook.cursor",
            "recommended",
            "warn",
            "missing",
            "receipt not found",
            "command",
            "krill hook install cursor",
        )],
    );
    let cx = open_checks_screen(cx, &f.env);
    wizard_window(cx)
        .unwrap()
        .update(&mut cx.cx, |_, window, _| window.activate_window())
        .unwrap();
    click(cx, "wizard-fix-hook.cursor");
    for _ in 0..200 {
        cx.run_until_parked();
        if present(cx, "wizard-hook-confirm-cursor") {
            break;
        }
    }
    assert!(
        present(cx, "wizard-hook-confirm-cursor"),
        "{:?}",
        wizard_read(cx, |w, _| w.fix_errors.clone())
    );
    click(cx, "wizard-hook-confirm-cursor");
    wait_for_file(cx, &f.env.state, || {
        f.env.state.join("hooks/cursor.json").exists()
    });
    press(cx, "escape");
    let stable_dir = f.env.home.join(".local/share/minikrill/bin");
    let stable_mk = stable_dir.join("krill");
    for bin in ["krill", "krill-holder", "krilld"] {
        assert!(stable_dir.join(bin).is_file());
    }
    let config = fs::read_to_string(f.env.home.join(".cursor/hooks.json")).unwrap();
    assert!(
        config.contains(&stable_mk.display().to_string()),
        "{config}"
    );

    // 새 빌드로 재실행해도 대화상자 없이 스테이징 바이너리만 갱신한다.
    script(&f.env.krill, "echo mk-v2\n");
    script(&f.root.join("krill-holder"), "echo holder-v2\n");
    script(&f.root.join("krilld"), "echo krilld-v2\n");

    cx.update(|_, cx| offer(f.env.clone(), cx));
    wait_for_file(cx, &stable_mk, || {
        fs::read_to_string(&stable_mk).is_ok_and(|b| b.contains("mk-v2"))
    });
    assert!(wizard_window(cx).is_none());
    assert!(state::load(&f.env.state).completed);
    assert!(
        fs::read_to_string(stable_dir.join("krill-holder"))
            .unwrap()
            .contains("holder-v2")
    );
    assert!(
        fs::read_to_string(stable_dir.join("krilld"))
            .unwrap()
            .contains("krilld-v2")
    );
}

// MK-162 진입점·매핑·빠른 점검 테스트.

use crate::i18n::catalog::checks as ctext;
use crate::i18n::catalog::settings as stext;

fn fail_check(id: &str) -> Check {
    Check {
        id: id.to_owned(),
        level: krill_proto::doctor::Level::Required,
        status: krill_proto::doctor::Status::Fail,
        summary: "bad".to_owned(),
        detail: String::new(),
        fix: krill_proto::doctor::Fix {
            kind: krill_proto::doctor::FixKind::None,
            value: String::new(),
        },
    }
}

#[gpui::test]
fn checks_window_is_single_instance(cx: &mut TestAppContext) {
    let f = fake("checks-single");
    cx.update(gpui_component::init);
    cx.update(register_test_host);
    cx.update(|cx| open_checks(&f.env, cx));
    cx.run_until_parked();
    cx.update(|cx| open_checks(&f.env, cx));
    cx.run_until_parked();
    let count = cx
        .windows()
        .into_iter()
        .filter(|w| w.downcast::<welcome::Wizard>().is_some())
        .count();
    assert_eq!(count, 1);
}

#[test]
fn check_title_maps_known_ids_and_passes_unknown_through() {
    assert_eq!(check_title("bin.helpers"), ctext::TITLE_BIN_HELPERS.get());
    assert_eq!(check_title("app.location"), ctext::TITLE_APP_LOCATION.get());
    assert_eq!(
        check_title("hook.codex"),
        ctext::TITLE_HOOK.format(&[("name", "Codex")])
    );
    assert_eq!(
        check_title("hook.agy"),
        ctext::TITLE_HOOK.format(&[("name", "Antigravity")])
    );
    assert_eq!(
        check_title("agent.opencode"),
        ctext::TITLE_AGENT.format(&[("name", "OpenCode")])
    );
    assert_eq!(check_title("mystery.check"), "mystery.check");
}

#[test]
fn checks_summary_reports_never_without_snapshot() {
    assert_eq!(
        checks_summary_text(&ChecksSnapshot::default()),
        stext::CHECKS_NEVER.get()
    );
}

#[test]
fn checks_summary_counts_failures_and_warnings() {
    use krill_proto::doctor::{Fix, FixKind, Level, Status};
    let ok = |id: &str| Check {
        id: id.to_owned(),
        level: Level::Required,
        status: Status::Ok,
        summary: String::new(),
        detail: String::new(),
        fix: Fix {
            kind: FixKind::None,
            value: String::new(),
        },
    };
    let mut warn = ok("git");
    warn.level = Level::Recommended;
    warn.status = Status::Warn;
    let snap = ChecksSnapshot {
        at: Some(std::time::SystemTime::now()),
        checks: vec![fail_check("bin.helpers"), warn, ok("app.location")],
    };
    assert_eq!(
        checks_summary_text(&snap),
        stext::CHECKS_SUMMARY.format(&[
            ("ago", stext::CHECKS_JUST_NOW.get()),
            ("req", "1"),
            ("rec", "1"),
        ])
    );
}
#[test]
fn quick_checks_reports_missing_helpers_without_daemon() {
    let f = fake("quick");
    std::fs::create_dir_all(&f.env.state).unwrap();
    fs::remove_file(f.root.join("krill-holder")).unwrap();
    fs::remove_file(f.root.join("krilld")).unwrap();
    let start = std::time::Instant::now();
    let checks = quick_checks(&f.env);
    let wall = start.elapsed();
    assert!(
        wall < Duration::from_secs(10),
        "quick checks must stay fast: {wall:?}"
    );
    let get = |id: &str| checks.iter().find(|c| c.id == id).unwrap();
    assert_eq!(get("bin.helpers").status, krill_proto::doctor::Status::Fail);
    assert_eq!(get("bin.version").status, krill_proto::doctor::Status::Skip);
    assert_eq!(
        get("state.writable").status,
        krill_proto::doctor::Status::Ok
    );
    assert_eq!(
        get("app.location").status,
        krill_proto::doctor::Status::Skip
    );
}

#[gpui::test]
fn snapshot_roundtrip_via_global(cx: &mut TestAppContext) {
    let snap = ChecksSnapshot {
        at: Some(std::time::SystemTime::now()),
        checks: vec![fail_check("bin.helpers")],
    };
    cx.update(|cx| store_snapshot(cx, snap.checks.clone()));
    let back = cx.update(|cx| snapshot(cx));
    assert_eq!(back.checks, snap.checks);
    assert!(back.at.is_some());
    // 다른 테스트에 새지 않게 비운다.
    cx.update(|cx| store_snapshot(cx, Vec::new()));
}

// OWE-714: 구 4단계 흐름 중간(점검·연결·완료)에서 멈춘 사용자는 다시 묻지 않고 완료로
// 넘긴다(체크리스트는 설정 점검에서만).
#[gpui::test]
fn wizard_legacy_mid_flow_completes_silently(cx: &mut TestAppContext) {
    for step in [
        state::Step::Checklist,
        state::Step::Connect,
        state::Step::Done,
    ] {
        let f = fake(&format!("resume-{step:?}"));
        state::save(
            &f.env.state,
            &state::OnboardingState {
                completed: false,
                step,
                version: state::STATE_VERSION,
            },
        )
        .unwrap();
        let cx = open(cx, &f.env);
        assert!(wizard_window(cx).is_none(), "{step:?}");
        assert!(state::load(&f.env.state).completed, "{step:?}");
    }
}

// OWE-694 시나리오 8(원자 쓰기 중단): 실패한 저장은 기존 파일을 보존하고 임시 파일만 치운다.
#[test]
fn state_failed_save_preserves_existing() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = std::env::temp_dir().join(format!("mk131-state-fail-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    let before = state::OnboardingState {
        completed: false,
        step: state::Step::Connect,
        version: state::STATE_VERSION,
    };
    state::save(&root, &before).unwrap();
    let body = fs::read_to_string(state::state_path(&root)).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
    let after = state::OnboardingState {
        completed: true,
        step: state::Step::Done,
        version: state::STATE_VERSION,
    };
    let result = state::save(&root, &after);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(state::state_path(&root)).unwrap(), body);
    assert_eq!(state::load(&root), before);
    let names: Vec<_> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("onboarding.toml")]);
    fs::remove_dir_all(&root).ok();
}

// OWE-694 시나리오 8(원자 쓰기 중단): kill -9로 남을 수 있는 tmp 잔재는 읽기를 깨지 않는다.
#[test]
fn state_load_ignores_interrupted_tmp_debris() {
    let root = std::env::temp_dir().join(format!("mk131-state-debris-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    let saved = state::OnboardingState {
        completed: false,
        step: state::Step::Checklist,
        version: state::STATE_VERSION,
    };
    state::save(&root, &saved).unwrap();
    // 중단된 rename이 남긴 임시 파일 흉내. 다음 저장은 다른 고유 이름이라 덮지 않는다.
    fs::write(root.join("onboarding.tmp-999-0"), "completed = true\n").unwrap();
    assert_eq!(state::load(&root), saved);
    let done = saved.completed();
    state::save(&root, &done).unwrap();
    assert!(state::load(&root).completed);
    fs::remove_dir_all(&root).ok();
}

// OWE-694 시나리오 2: app.location이 /Volumes·AppTranslocation에서 fail이면 필수 차단이다.
// UI 표시는 checks_screen_shows_required_fail_expanded가 같은 required_blockers 경로로
// 보장하고, 여기서는 판정→차단 연결을 고정한다.
#[test]
fn app_location_fail_is_required_blocker() {
    use crate::platform::permissions as perm;
    for exe in [
        "/Volumes/Krill 0.1.1/Krill.app/Contents/MacOS/krill-app",
        "/private/var/folders/xx/T/AppTranslocation/yy/Krill.app/Contents/MacOS/krill-app",
    ] {
        let check = perm::decide_location(std::path::Path::new(exe), true);
        assert_eq!(check.id, "app.location", "{exe}");
        assert_eq!(check.level, krill_proto::doctor::Level::Required, "{exe}");
        assert_eq!(check.status, krill_proto::doctor::Status::Fail, "{exe}");
        assert_eq!(
            required_failures(std::slice::from_ref(&check)).len(),
            1,
            "{exe}"
        );
    }
    let ok = perm::decide_location(
        std::path::Path::new("/Applications/Krill.app/Contents/MacOS/krill-app"),
        true,
    );
    assert_eq!(ok.status, krill_proto::doctor::Status::Ok);
    assert!(required_failures(std::slice::from_ref(&ok)).is_empty());
}

// OWE-694 시나리오 7: 설정·메뉴·팔레트(같은 open_checks 경로)는 창을 하나만 쓴다.
#[gpui::test]
fn wizard_entries_share_single_window(cx: &mut TestAppContext) {
    let f = fake("entries-single");
    cx.update(gpui_component::init);
    cx.update(register_test_host);
    cx.update(|cx| open_checks(&f.env, cx));
    cx.run_until_parked();
    cx.update(|cx| open_checks(&f.env, cx));
    cx.run_until_parked();
    let count = || {
        cx.windows()
            .into_iter()
            .filter(|w| w.downcast::<welcome::Wizard>().is_some())
            .count()
    };
    assert_eq!(count(), 1);
}

// OWE-714 (6) 근본 원인: `krill doctor --json`은 fail 점검이 있으면 종료 코드 1이다.
// 종료 코드만 보고 보고서 전체를 "점검 실행 실패"로 덮지 않는다.
#[test]
fn doctor_exit_1_with_report_keeps_real_checks() {
    let f = fake("doctor-exit1");
    write_doctor(
        &f.root,
        &[ck(
            "shell.path",
            "required",
            "fail",
            "login shell PATH unavailable",
            "login shell did not answer within 5s; agent detection may be wrong",
            "none",
            "",
        )],
    );
    script(
        &f.env.krill,
        &format!("cat '{}/doctor.json'; exit 1\n", f.root.display()),
    );
    let checks = run_doctor_checks(&f.env.krill);
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(checks[0].id, "shell.path");
}

// OWE-714 (6): 보고서를 못 읽으면 원인(종료 코드·stderr·실행 불가·형식)과 다음 행동을 적는다.
#[test]
fn doctor_failure_names_cause_and_next_step() {
    let f = fake("doctor-cause");
    script(&f.env.krill, "echo 'boom: no daemon' >&2; exit 3\n");
    let checks = run_doctor_checks(&f.env.krill);
    assert_eq!(checks[0].id, "bin.helpers");
    let cause = ctext::DOCTOR_CAUSE_EXIT.format(&[("code", "3"), ("stderr", "boom: no daemon")]);
    assert_eq!(
        checks[0].detail,
        ctext::DOCTOR_FAILED_DETAIL.format(&[("cause", &cause)])
    );

    let missing = f.root.join("no-such-krill");
    let detail = &run_doctor_checks(&missing)[0].detail;
    assert!(detail.contains(&missing.display().to_string()), "{detail}");

    script(&f.env.krill, "echo not-json\n");
    let detail = &run_doctor_checks(&f.env.krill)[0].detail;
    assert!(
        detail.contains(ctext::DOCTOR_CAUSE_FORMAT.get()),
        "{detail}"
    );
}

// OWE-714 (2): doctor 영어 문구를 화면 언어(테스트 기본 한국어)로 옮기고 값은 그대로 둔다.
#[test]
fn check_text_localizes_doctor_messages_and_keeps_values() {
    assert_eq!(check_text("receipt not found"), "설치 영수증이 없습니다");
    assert_eq!(
        check_text("krill-holder and krilld are present"),
        "krill-holder와 krilld가 있습니다"
    );
    assert_eq!(
        check_text("another krill is on PATH"),
        "PATH에 다른 krill이 있습니다"
    );
    assert_eq!(
        check_text("codex hook is not installed"),
        "Codex 훅이 설치돼 있지 않습니다"
    );
    assert_eq!(check_text("2 agent(s) found"), "에이전트 2개 감지");
    assert_eq!(
        check_text("no agent detected"),
        "감지된 에이전트가 없습니다"
    );
    assert_eq!(check_text("Claude Code detected"), "Claude Code 감지됨");
    assert_eq!(
        check_text("/opt/homebrew/bin/krill (not overwritten)"),
        "/opt/homebrew/bin/krill(덮어쓰지 않음)"
    );
    assert_eq!(
        check_text("krill 0.1.0+dev.a != krilld 0.1.1 (reinstall Krill)"),
        "krill 0.1.0+dev.a ≠ krilld 0.1.1(Krill을 다시 설치하세요)"
    );
    // 값(경로·버전)과 모르는 문구는 원문 그대로.
    assert_eq!(check_text("/Users/x/state"), "/Users/x/state");
    assert_eq!(check_text("codex-cli 0.130.0"), "codex-cli 0.130.0");
    assert_eq!(check_text(""), "");
}

// 카탈로그의 모든 doctor 틀은 자기 영어 원문에 맞고, 앞선 틀에 가려지지 않는다.
#[test]
fn check_text_every_doctor_template_matches_itself() {
    for message in DOCTOR_TEXT {
        let en = message.in_language(crate::i18n::Language::En);
        let mut sample = String::new();
        let mut fields = Vec::new();
        let mut rest = en;
        while let Some((lead, tail)) = rest.split_once('{') {
            let (name, after) = tail.split_once('}').unwrap();
            let value = format!("v{}", fields.len());
            sample.push_str(lead);
            sample.push_str(&value);
            fields.push((name, value));
            rest = after;
        }
        sample.push_str(rest);
        let pairs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
        assert_eq!(check_text(&sample), message.format(&pairs), "{en}");
    }
}

// OWE-714 (1)(3)(4)(5): 펼침 chevron은 행 오른쪽 같은 줄, 조치 버튼은 테두리 버튼,
// 헤더·푸터는 스크롤 밖에 고정, 건너뜀은 중립 표시다.
#[gpui::test]
fn checks_screen_layout_inline_chevron_fixed_footer_and_skip(cx: &mut TestAppContext) {
    let f = fake("checks-layout");
    let cx = open_checks_screen(cx, &f.env);
    let w = wizard_window(cx).expect("wizard window");
    let mut wcx = VisualTestContext::from_window(w.into(), &cx.cx);
    wcx.run_until_parked();

    let text = wcx
        .debug_bounds("wizard-check-bin.helpers")
        .expect("row text");
    let chevron = wcx
        .debug_bounds("wizard-expand-bin.helpers")
        .expect("chevron");
    assert!(
        chevron.left() >= text.right(),
        "{chevron:?} right of {text:?}"
    );
    assert!(
        chevron.top() < text.bottom() && chevron.bottom() > text.top(),
        "chevron on the same line: {chevron:?} vs {text:?}"
    );

    // 점검 행이 창보다 많아도 헤더·푸터는 창 안에 있다.
    let viewport = wcx.update(|window, _| window.viewport_size());
    let header = wcx.debug_bounds("wizard-header").expect("header");
    let footer = wcx.debug_bounds("wizard-footer").expect("footer");
    let recheck = wcx
        .debug_bounds("wizard-recheck")
        .expect("recheck in header");
    assert!(recheck.top() >= header.top() && recheck.bottom() <= header.bottom());
    assert!(
        footer.bottom() <= viewport.height,
        "{footer:?} in {viewport:?}"
    );
    assert!(footer.top() > header.bottom());
    assert!(wcx.debug_bounds("wizard-later").is_some());

    assert!(wcx.debug_bounds("wizard-skipped-hook.opencode").is_some());
    assert!(wcx.debug_bounds("wizard-skipped-bin.helpers").is_none());

    // chevron으로 상세를 펼친다(성공 행은 기본 접힘).
    assert!(wcx.debug_bounds("wizard-detail-bin.helpers").is_none());
    let at = chevron.center();
    wcx.simulate_click(at, gpui::Modifiers::default());
    wcx.run_until_parked();
    assert!(wcx.debug_bounds("wizard-detail-bin.helpers").is_some());

    // Esc = 닫기.
    press(cx, "escape");
    assert!(wizard_window(cx).is_none());
}

/// 배너만 그리는 테스트 호스트(셸과 같은 `observe_global` 재그리기).
struct BannerHost;

impl gpui::Render for BannerHost {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{ParentElement as _, Styled as _};
        gpui::div().size_full().children(banner::render(cx))
    }
}

// OWE-714: 조치 배너는 실행기가 필요를 올렸을 때만 보이고, [닫기]로 내린다.
#[gpui::test]
fn fix_banner_shows_only_on_need_and_closes(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|_, cx| {
        cx.observe_global::<actions::FixNeeds>(|_, cx| cx.notify())
            .detach();
        BannerHost
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("fix-banner").is_none(),
        "no banner on first run"
    );

    cx.update(|_, cx| {
        cx.set_global(actions::FixNeeds {
            items: vec![actions::FixNeed {
                check_id: "app.folders".to_owned(),
                message: "denied".to_owned(),
            }],
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("fix-banner").is_some());
    assert!(cx.debug_bounds("fix-banner-fix").is_some());

    let at = cx.debug_bounds("fix-banner-close").expect("close").center();
    cx.simulate_click(at, gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("fix-banner").is_none());
}

/// `doctor`가 처음 `down`번은 데몬 연결 실패(소켓 없음)를, 그 뒤로는 기본 응답을 내는 가짜 `krill`.
/// 앱이 막 띄운 krilld가 소켓을 열기 전 상태를 흉내 낸다(OWE-714 D1).
fn late_daemon(f: &Fake, down: u32) {
    let link = f.env.home.join(".local/bin/krill").display().to_string();
    let mut checks = default_checks(&link);
    for check in &mut checks {
        if check["id"] == "daemon.connect" {
            *check = ck(
                "daemon.connect",
                "required",
                "fail",
                "cannot talk to krilld",
                "No such file or directory (os error 2)",
                "none",
                "",
            );
        }
    }
    let report = serde_json::json!({"schema": 1, "checks": checks});
    fs::write(f.root.join("doctor-down.json"), report.to_string()).unwrap();
    script(
        &f.env.krill,
        &format!(
            "if [ \"$1\" = doctor ]; then n=$(cat '{root}/doctor-calls' 2>/dev/null || echo 0); n=$((n+1)); echo $n > '{root}/doctor-calls'; if [ $n -le {down} ]; then cat '{root}/doctor-down.json'; exit 1; fi; cat '{root}/doctor.json'; exit 0; fi\n",
            root = f.root.display(),
        ),
    );
}

fn doctor_calls(f: &Fake) -> u32 {
    fs::read_to_string(f.root.join("doctor-calls")).map_or(0, |s| s.trim().parse().unwrap())
}

// OWE-714 D1: 점검이 데몬 소켓 준비 전에 돌아도, 잠깐 기다려 다시 점검한 뒤 판정한다.
#[gpui::test]
fn wizard_waits_for_late_daemon_instead_of_blocking(cx: &mut TestAppContext) {
    let f = fake("late-daemon");
    late_daemon(&f, 2);
    let cx = open_checks_screen(cx, &f.env);
    let checks = wait_checks(cx);
    let daemon = checks.iter().find(|c| c.id == "daemon.connect").unwrap();
    assert_eq!(daemon.status, Status::Ok, "{daemon:?}");
    assert!(!present(cx, "wizard-blocked"), "late daemon must not block");
}

// OWE-714 D1: 기다린 뒤에도 데몬이 없으면 실패 그대로 돌려준다(막는 화면은 그때만).
#[test]
fn doctor_settle_gives_up_after_wait_and_keeps_failure() {
    let f = fake("dead-daemon");
    late_daemon(&f, u32::MAX);
    let checks = run_doctor_settled(&f.env.krill, Duration::from_millis(300));
    let daemon = checks.iter().find(|c| c.id == "daemon.connect").unwrap();
    assert_eq!(daemon.status, Status::Fail);
    assert!(doctor_calls(&f) >= 2, "retried at least once");
}

// OWE-714 D2: 누르기 전엔 hook 행에 오류가 없고, 설치 계획이 기존 설정과 충돌하면 오류색 "실패"가
// 아니라 주의 톤 안내를 보이며 원문은 펼침(자세히)에서만 보인다.
#[gpui::test]
fn hook_conflict_shows_guidance_not_failure(cx: &mut TestAppContext) {
    let f = fake("hook-conflict-ui");
    let cx = open_checks_screen(cx, &f.env);
    assert!(present(cx, "wizard-fix-hook.codex"));
    assert!(!present(cx, "wizard-fix-error-hook.codex"));
    assert!(!present(cx, "wizard-fix-conflict-hook.codex"));
    let w = wizard_window(cx).unwrap();
    w.update(&mut cx.cx, |v, _, cx| {
        v.fix_errors.insert(
            "hook.codex".to_owned(),
            actions::FixError::Conflict {
                guide: "guide".to_owned(),
                detail: "existing Codex trust conflicts: x".to_owned(),
            },
        );
        cx.notify();
    })
    .unwrap();
    assert!(present(cx, "wizard-fix-conflict-hook.codex"));
    assert!(!present(cx, "wizard-fix-error-hook.codex"));
    assert!(!present(cx, "wizard-fix-conflict-detail-hook.codex"));
    // 행이 스크롤 밖일 수 있어 클릭 대신 펼침 상태를 직접 뒤집는다(자세히 버튼과 같은 경로).
    w.update(&mut cx.cx, |v, _, cx| v.toggle_expand("hook.codex", cx))
        .unwrap();
    assert!(present(cx, "wizard-fix-conflict-detail-hook.codex"));
}

#[gpui::test]
fn wizard_fix_all_installs_hooks_and_skills_and_copy_queues_terminal_command(
    cx: &mut TestAppContext,
) {
    let f = fake("wizard-fix-all");
    write_doctor(
        &f.root,
        &[
            ck(
                "bin.helpers",
                "required",
                "ok",
                "helpers present",
                "staged",
                "none",
                "",
            ),
            ck(
                "git",
                "recommended",
                "warn",
                "git is not installed",
                "install Xcode Command Line Tools",
                "copy",
                "xcode-select --install",
            ),
        ],
    );
    let cx = open_checks_screen(cx, &f.env);
    assert!(present(cx, "wizard-fix-all"));
    click(cx, "wizard-fix-all");
    for rel_path in [
        ".claude/settings.json",
        ".gemini/config/hooks.json",
        ".cursor/hooks.json",
        ".config/opencode/plugins/minikrill.js",
        ".claude/skills/minikrill/SKILL.md",
        ".codex/skills/orchestration/SKILL.md",
        ".gemini/config/skills/worktree-checkpoints/SKILL.md",
    ] {
        assert!(
            f.env.home.join(rel_path).is_file(),
            "wizard-fix-all should create {rel_path}"
        );
    }

    assert!(
        f.env.state.join("hooks/codex.json").is_file(),
        "wizard-fix-all should also install codex hook in background"
    );
    let claude_hook = fs::read_to_string(f.env.home.join(".claude/settings.json")).unwrap();
    let staged = f.env.home.join(".local/share/minikrill/bin/krill");
    assert!(
        claude_hook.contains(&staged.display().to_string()),
        "claude hook should record staged krill path: {claude_hook}"
    );

    // 명령어 복사 행을 누르면 클립보드 복사와 함께 터미널 자동 실행 큐에도 들어간다.
    click(cx, "wizard-fix-git");
    cx.update(|_, cx| {
        assert_eq!(
            crate::settings::take_pending_terminal_command(cx).as_deref(),
            Some("xcode-select --install")
        );
    });
}

#[gpui::test]
fn wizard_fix_all_respects_isolated_state_without_polluting_fix_needs(cx: &mut TestAppContext) {
    let mut f = fake("wizard-fix-all-isolated");
    f.env.state_overridden = true;
    // 보조 바이너리를 지워도 격리 상태에서는 auto_prepare를 돌리지 않아 FixNeeds 오류가 발생하지 않아야 한다.
    fs::remove_file(f.root.join("krill-holder")).ok();
    fs::remove_file(f.root.join("krilld")).ok();
    write_doctor(
        &f.root,
        &[ck(
            "git",
            "recommended",
            "warn",
            "git is not installed",
            "install Xcode Command Line Tools",
            "copy",
            "xcode-select --install",
        )],
    );
    let cx = open_checks_screen(cx, &f.env);
    click(cx, "wizard-fix-all");
    assert!(f.env.home.join(".claude/settings.json").is_file());
    assert!(
        f.env
            .home
            .join(".claude/skills/minikrill/SKILL.md")
            .is_file()
    );
    assert!(!f.env.home.join(".local/bin/krill").exists());
    cx.update(|_, cx| assert!(cx.try_global::<actions::FixNeeds>().is_none()));
}

#[gpui::test]
fn cancelled_fix_shows_no_error_and_failed_fix_shows_only_error_cause(cx: &mut TestAppContext) {
    let f = fake("fix-error-copy");
    let cx = open_checks_screen(cx, &f.env);
    let w = wizard_window(cx).unwrap();
    w.update(&mut cx.cx, |v, _, cx| {
        v.fix_errors
            .insert("bin.helpers".to_owned(), actions::FixError::Cancelled);
        cx.notify();
    })
    .unwrap();
    assert!(!present(cx, "wizard-fix-error-bin.helpers"));

    w.update(&mut cx.cx, |v, _, cx| {
        v.fix_errors.insert(
            "bin.helpers".to_owned(),
            actions::FixError::Failed("helper copy failed".to_owned()),
        );
        cx.notify();
    })
    .unwrap();
    assert!(present(cx, "wizard-fix-error-bin.helpers"));
}

/// 설 실기(v0.1.8): 번들 설치본은 holder·krilld가 Contents/Helpers에 있어 빠른 점검이 늘 "helpers 없음"으로
/// 실패하고 버전 비교도 건너뛰었다. 번들 배치에서 두 점검이 통과해야 한다.
#[test]
fn quick_checks_find_helpers_in_app_bundle() {
    let root = std::env::temp_dir().join(format!("mk-bundle-helpers-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let contents = root.join("Krill.app/Contents");
    let script = "#!/bin/sh\necho 'krill 0.1.8'\n";
    for rel in ["MacOS/krill", "Helpers/krill-holder", "Helpers/krilld"] {
        let path = contents.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let env = Env {
        shell: "/bin/sh".into(),
        krill: contents.join("MacOS/krill"),
        state: root.join("state"),
        home: root.join("home"),
        state_overridden: true,
    };
    let checks = super::quick_checks(&env);
    let status = |id: &str| checks.iter().find(|c| c.id == id).map(|c| c.status);
    assert_eq!(status("bin.helpers"), Some(Status::Ok));
    assert_eq!(status("bin.version"), Some(Status::Ok));
    fs::remove_dir_all(&root).ok();
}
