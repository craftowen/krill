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

fn wizard_step(cx: &mut VisualTestContext) -> state::Step {
    let w = wizard_window(cx).expect("wizard window");
    w.update(&mut cx.cx, |v, _, _| v.step).unwrap()
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

fn open<'a>(cx: &'a mut TestAppContext, env: &Env) -> &'a mut VisualTestContext {
    // 마법사는 gpui-component 버튼을 쓴다(앱은 시작 때 초기화한다).
    cx.update(gpui_component::init);
    let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
    let env = env.clone();
    cx.update(|window, cx| offer(env, window, cx));
    cx.run_until_parked();
    cx
}

/// 첫 화면에서 동의하고 [시작하기]까지 누른다(체크 전 Enter는 무시된다).
fn start(cx: &mut VisualTestContext) {
    assert_eq!(wizard_step(cx), state::Step::Consent);
    press(cx, "enter");
    assert_eq!(wizard_step(cx), state::Step::Consent);
    click(cx, "consent-accept");
    click(cx, "consent-continue");
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
    let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
    let env = env.clone();
    cx.update(|_, cx| open_checks(&env, cx));
    cx.run_until_parked();
    wait_checks(cx);
    wait_survey(cx);
    cx
}

// OWE-714: 첫 실행은 1장이다. 체크리스트 없이 동의 + 기본 에이전트 + [시작하기]로 끝난다.
#[gpui::test]
fn wizard_first_run_is_one_page_and_start_completes(cx: &mut TestAppContext) {
    let f = fake("wizard-full");
    let cx = open(cx, &f.env);
    assert_eq!(wizard_step(cx), state::Step::Consent);
    assert!(present(cx, "wizard-header"));
    assert!(present(cx, "wizard-footer"));
    assert!(
        !present(cx, "wizard-recheck"),
        "no checklist on the first page"
    );
    assert!(
        !present(cx, "wizard-body"),
        "no checklist on the first page"
    );

    // 감지된 에이전트 중 설정 기본값(claude)을 자동으로 고르고, 바꿀 수 있다.
    wait_survey(cx);
    assert_eq!(
        wizard_read(cx, |v, _| v.agent.clone()).as_deref(),
        Some("claude")
    );
    assert!(present(cx, "wizard-agent-codex"));
    click(cx, "wizard-agent-codex");
    assert_eq!(
        wizard_read(cx, |v, _| v.agent.clone()).as_deref(),
        Some("codex")
    );

    start(cx);
    assert!(wizard_window(cx).is_none(), "start closes the window");
    let stored = state::load(&f.env.state);
    assert!(stored.completed);
    assert_eq!(stored.step, state::Step::Done);
    assert!(!consent::needs_consent(&f.env.state));
    // 첫 화면은 hook·CLI를 묻지 않는다.
    assert!(!f.root.join("krill.log").exists());

    // 완료 뒤에는 창 없이 스테이징 갱신만 한다.
    cx.update(|window, cx| offer(f.env.clone(), window, cx));
    cx.run_until_parked();
    assert!(wizard_window(cx).is_none());
}

// OWE-714 (8): 창은 680×620 고정이고 크기 조절·최소화가 없다.
#[gpui::test]
fn wizard_window_is_fixed_size(cx: &mut TestAppContext) {
    let options = cx.update(|cx| welcome::window_options(cx));
    assert!(!options.is_resizable, "zoom/resize must be off");
    assert!(!options.is_minimizable);
    let Some(gpui::WindowBounds::Windowed(bounds)) = options.window_bounds else {
        panic!("windowed bounds expected");
    };
    assert_eq!(bounds.size, gpui::size(gpui::px(680.), gpui::px(620.)));
}

// OWE-861: 점검 창(온보딩 마법사)은 ⌘W로 닫힌다(Esc·⌥⌘W와 별개 경로).
#[gpui::test]
fn wizard_cmd_w_closes_window(cx: &mut TestAppContext) {
    let f = fake("wizard-cmd-w");
    cx.update(|cx| {
        gpui_component::init(cx);
        init_actions(cx);
    });
    let cx = open(cx, &f.env);
    cx.run_until_parked();
    assert!(wizard_window(cx).is_some(), "wizard opens");
    press(cx, "cmd-w");
    assert!(
        wizard_window(cx).is_none(),
        "cmd-w closes the checks window"
    );
}

// OWE-714 (9): 동의 체크박스는 처음에도, 다시 실행해도 해제 상태다.
#[gpui::test]
fn wizard_consent_checkbox_starts_unchecked_even_after_rerun(cx: &mut TestAppContext) {
    let f = fake("consent-unchecked");
    let cx = open(cx, &f.env);
    assert!(!wizard_read(cx, welcome::Wizard::accepted));
    click(cx, "consent-accept");
    assert!(wizard_read(cx, welcome::Wizard::accepted));
    cx.update(|_, cx| rerun_onboarding(&f.env, cx));
    cx.run_until_parked();
    assert!(
        !wizard_read(cx, welcome::Wizard::accepted),
        "rerun must reset consent"
    );
    // 체크 전 [시작하기]는 아무 일도 하지 않는다.
    click(cx, "consent-continue");
    assert_eq!(wizard_step(cx), state::Step::Consent);
    assert!(consent::needs_consent(&f.env.state));
}

#[gpui::test]
fn wizard_later_on_first_page_asks_again(cx: &mut TestAppContext) {
    let f = fake("wizard-later");
    let cx = open(cx, &f.env);
    press(cx, "escape");
    assert!(wizard_window(cx).is_none(), "Esc must close the dialog");
    assert!(consent::needs_consent(&f.env.state));
    assert!(!state::load(&f.env.state).completed);

    cx.update(|window, cx| offer(f.env.clone(), window, cx));
    cx.run_until_parked();
    assert_eq!(wizard_step(cx), state::Step::Consent);
}

// 필수 실패가 있으면 [시작하기] 뒤 닫지 않고 점검 화면으로 막는다.
#[gpui::test]
fn wizard_required_fail_blocks_after_start(cx: &mut TestAppContext) {
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
    let cx = open(cx, &f.env);
    wait_checks(cx);
    start(cx);
    assert_eq!(wizard_step(cx), state::Step::Checklist);
    assert!(present(cx, "wizard-blocked"));
    // 실패 행은 원인·다음 행동을 펼친 채로 보여 준다(OWE-714 (6)).
    assert!(present(cx, "wizard-detail-bin.helpers"));
    assert!(state::load(&f.env.state).completed);
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
        if cx.has_pending_prompt() {
            break;
        }
    }
    assert!(
        cx.has_pending_prompt(),
        "{:?}",
        wizard_read(cx, |w, _| w.fix_errors.clone())
    );
    assert!(
        !f.env.state.join("hooks/cursor.json").exists(),
        "review writes nothing"
    );
    // 재검사는 동의 대기 중 조치 태스크를 취소하면 안 된다.
    click(cx, "wizard-recheck");
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer(text::INSTALL.get());
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
    let cx = open(cx, &f.env);
    let checks = wait_checks(cx);
    let helpers = checks.iter().find(|c| c.id == "bin.helpers").unwrap();
    assert_eq!(helpers.status, Status::Fail);
    assert!(
        helpers.detail.contains(ctext::DOCTOR_CAUSE_MISSING.get()),
        "{}",
        helpers.detail
    );
    start(cx);
    assert_eq!(wizard_step(cx), state::Step::Checklist);
    assert!(present(cx, "wizard-blocked"));
}

#[gpui::test]
fn wizard_isolated_state_skips_cli_symlink(cx: &mut TestAppContext) {
    let mut f = fake("wizard-isolated");
    f.env.state = f.root.join("isolated-mk-state");
    let link = f.env.home.join(".local/bin/krill");

    let cx = open(cx, &f.env);
    wait_checks(cx);
    start(cx);
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
    consent::record_consent(&f.env.state, std::time::SystemTime::now()).unwrap();
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
    cx.update(|window, cx| offer(f.env.clone(), window, cx));
    let krilld = stable_dir.join("krilld");
    wait_for_file(cx, &krilld, || {
        fs::read_to_string(&krilld).is_ok_and(|b| b.contains("krilld-v2"))
    });
    assert!(wizard_window(cx).is_none());
}

#[gpui::test]
fn wizard_legacy_done_with_consent_skips_wizard(cx: &mut TestAppContext) {
    let f = fake("wizard-migrated");
    fs::create_dir_all(&f.env.state).unwrap();
    fs::write(f.env.state.join(DONE_FILE), b"").unwrap();
    consent::record_consent(&f.env.state, std::time::SystemTime::now()).unwrap();

    let cx = open(cx, &f.env);
    assert!(wizard_window(cx).is_none());
    assert!(state::load(&f.env.state).completed);
}

#[gpui::test]
fn wizard_legacy_done_without_consent_asks_consent_once(cx: &mut TestAppContext) {
    let f = fake("wizard-migrate-consent");
    fs::create_dir_all(&f.env.state).unwrap();
    fs::write(f.env.state.join(DONE_FILE), b"").unwrap();

    let cx = open(cx, &f.env);
    assert_eq!(wizard_step(cx), state::Step::Consent);
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
    wait_checks(cx);
    start(cx);
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
    consent::record_consent(&f.env.state, std::time::SystemTime::now()).unwrap();

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
        if cx.has_pending_prompt() {
            break;
        }
    }
    assert!(
        cx.has_pending_prompt(),
        "{:?}",
        wizard_read(cx, |w, _| w.fix_errors.clone())
    );
    cx.simulate_prompt_answer(text::INSTALL.get());
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

    // 동의가 있으면 새 빌드로 재실행해도 대화상자 없이 스테이징 바이너리만 갱신한다.
    script(&f.env.krill, "echo mk-v2\n");
    script(&f.root.join("krill-holder"), "echo holder-v2\n");
    script(&f.root.join("krilld"), "echo krilld-v2\n");

    cx.update(|window, cx| offer(f.env.clone(), window, cx));
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

// OWE-690 동의 판정·기록·화면 테스트. 격리 임시 디렉터리만 쓰고 실제 상태를 건드리지 않는다.

/// 동의 테스트용 격리 상태 디렉터리. 끝나면 권한을 되돌리고 지운다.
struct ConsentDir {
    path: PathBuf,
}

impl ConsentDir {
    fn fresh(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mk-consent-{}-{name}", std::process::id()));
        fs::remove_dir_all(&path).ok();
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for ConsentDir {
    fn drop(&mut self) {
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o700)).ok();
        fs::remove_dir_all(&self.path).ok();
    }
}

#[test]
fn consent_missing_file_needs_consent() {
    let dir = ConsentDir::fresh("missing");
    assert!(consent::needs_consent(&dir.path));
}

#[test]
fn consent_current_record_needs_no_consent() {
    let dir = ConsentDir::fresh("current");
    consent::record_consent(&dir.path, std::time::SystemTime::now()).unwrap();
    assert!(!consent::needs_consent(&dir.path));
}

#[test]
fn consent_old_version_needs_reconsent() {
    let dir = ConsentDir::fresh("old");
    let old = consent::CONSENT_VERSION.saturating_sub(1);
    assert!(old < consent::CONSENT_VERSION);
    fs::write(
        consent::consent_path(&dir.path),
        format!(
            "version = {old}\naccepted_at = \"2024-01-01T00:00:00Z\"\napp_version = \"0.1.0\"\n"
        ),
    )
    .unwrap();
    assert!(consent::needs_consent(&dir.path));
    // 판정만 하고 파일은 손대지 않는다(덮어쓰기는 동의 때만).
    let body = fs::read_to_string(consent::consent_path(&dir.path)).unwrap();
    assert!(body.contains(&format!("version = {old}")));
}

#[test]
fn consent_corrupt_file_needs_consent_and_stays_untouched() {
    for (name, body) in [
        ("garbage", "깨진 내용 [[["),
        (
            "wrong-type",
            "version = \"1\"\naccepted_at = 3\napp_version = \"0.1.0\"\n",
        ),
        (
            "bad-time",
            "version = 1\naccepted_at = \"어제\"\napp_version = \"0.1.0\"\n",
        ),
    ] {
        let dir = ConsentDir::fresh(name);
        fs::write(consent::consent_path(&dir.path), body).unwrap();
        assert!(consent::needs_consent(&dir.path), "{name}");
        assert_eq!(
            fs::read_to_string(consent::consent_path(&dir.path)).unwrap(),
            body,
            "{name}"
        );
    }
}

#[test]
fn consent_record_is_0600_single_file_and_roundtrips() {
    let dir = ConsentDir::fresh("record");
    consent::record_consent(&dir.path, std::time::SystemTime::now()).unwrap();
    let path = consent::consent_path(&dir.path);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let body = fs::read_to_string(&path).unwrap();
    assert!(body.contains(&format!("version = {}", consent::CONSENT_VERSION)));
    assert!(body.contains(env!("CARGO_PKG_VERSION")));
    // 다시 기록해도 파일 하나로 교체된다(임시 파일 잔재 없음).
    consent::record_consent(&dir.path, std::time::SystemTime::now()).unwrap();
    let names: Vec<_> = fs::read_dir(&dir.path)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("consent.toml")]);
    assert!(!consent::needs_consent(&dir.path));
}

#[test]
fn consent_failed_write_preserves_existing() {
    let dir = ConsentDir::fresh("failure");
    consent::record_consent(&dir.path, std::time::SystemTime::now()).unwrap();
    let before = fs::read_to_string(consent::consent_path(&dir.path)).unwrap();
    fs::set_permissions(&dir.path, fs::Permissions::from_mode(0o500)).unwrap();
    let result = consent::record_consent(&dir.path, std::time::SystemTime::now());
    fs::set_permissions(&dir.path, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(consent::consent_path(&dir.path)).unwrap(),
        before
    );
    let names: Vec<_> = fs::read_dir(&dir.path)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("consent.toml")]);
}

#[gpui::test]
fn consent_view_starts_unchecked_and_details_fold(cx: &mut TestAppContext) {
    use consent::view::ConsentView;
    let dir = ConsentDir::fresh("view-gate");
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_, _| ConsentView::new(dir.path.clone()));
    cx.run_until_parked();
    assert!(!view.read_with(cx, |v, _| v.accepted()));

    // 요약 3줄만 보이고, [자세히 보기]를 누르면 실제 상태 디렉터리 경로가 든 전문이 펼쳐진다.
    assert!(cx.debug_bounds("consent-detail-0").is_none());
    let at = cx
        .debug_bounds("consent-more")
        .expect("more toggle")
        .center();
    cx.simulate_click(at, gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("consent-detail-4").is_some());

    let at = cx
        .debug_bounds("consent-accept")
        .expect("accept checkbox")
        .center();
    cx.simulate_click(at, gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.accepted()));
    // 화면 자체는 기록하지 않는다.
    assert!(fs::metadata(consent::consent_path(&dir.path)).is_err());
}

#[gpui::test]
fn consent_quit_writes_nothing(cx: &mut TestAppContext) {
    let f = fake("consent-quit");
    let cx = open(cx, &f.env);
    click(cx, "consent-accept");
    let w = wizard_window(cx).expect("wizard window");
    let mut wcx = VisualTestContext::from_window(w.into(), &cx.cx);
    let at = wcx.debug_bounds("consent-quit").expect("quit").center();
    wcx.simulate_click(at, gpui::Modifiers::default());
    wcx.run_until_parked();
    assert!(consent::needs_consent(&f.env.state));
    assert!(!state::load(&f.env.state).completed);
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

#[gpui::test]
fn rerun_restarts_wizard_from_consent(cx: &mut TestAppContext) {
    let f = fake("checks-rerun");
    cx.update(gpui_component::init);
    cx.update(|cx| open_checks(&f.env, cx));
    cx.run_until_parked();
    let step = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find_map(|w| w.downcast::<welcome::Wizard>())
            .expect("wizard")
            .update(cx, |v, _, _| v.step)
            .unwrap()
    });
    assert_eq!(step, state::Step::Checklist);
    cx.update(|cx| rerun_onboarding(&f.env, cx));
    cx.run_until_parked();
    let step = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find_map(|w| w.downcast::<welcome::Wizard>())
            .expect("wizard")
            .update(cx, |v, _, _| v.step)
            .unwrap()
    });
    assert_eq!(step, state::Step::Consent);
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

// OWE-694 시나리오 5: 완료한 기존 사용자는 동의만 묻고 기본 에이전트·점검을 묻지 않는다.
#[gpui::test]
fn wizard_completed_user_consent_only_skips_to_done(cx: &mut TestAppContext) {
    let f = fake("consent-only");
    state::save(
        &f.env.state,
        &state::OnboardingState {
            completed: true,
            step: state::Step::Done,
            version: state::STATE_VERSION,
        },
    )
    .unwrap();
    assert!(consent::needs_consent(&f.env.state));

    let cx = open(cx, &f.env);
    assert_eq!(wizard_step(cx), state::Step::Consent);
    assert!(
        !present(cx, "wizard-agent"),
        "existing users keep their default agent"
    );
    start(cx);
    assert!(wizard_window(cx).is_none());
    assert!(!consent::needs_consent(&f.env.state));
    let stored = state::load(&f.env.state);
    assert!(stored.completed);
    assert_eq!(stored.step, state::Step::Done);

    // 완료 + 동의 최신이면 마법사를 띄우지 않는다.
    cx.update(|window, cx| offer(f.env.clone(), window, cx));
    cx.run_until_parked();
    assert!(wizard_window(cx).is_none());
}

// OWE-694 시나리오 5(구 파일): onboarding.done만 있는 기존 사용자도 동의만 1회다.
#[gpui::test]
fn wizard_legacy_done_consent_only_skips_to_done(cx: &mut TestAppContext) {
    let f = fake("consent-only-legacy");
    fs::create_dir_all(&f.env.state).unwrap();
    fs::write(f.env.state.join(DONE_FILE), b"").unwrap();

    let cx = open(cx, &f.env);
    start(cx);
    assert!(wizard_window(cx).is_none());
    assert!(state::load(&f.env.state).completed);
}

// OWE-694 시나리오 6: CONSENT_VERSION 상승(구버전 기록)은 완료 사용자도 재동의다.
#[gpui::test]
fn wizard_consent_version_bump_asks_consent_again(cx: &mut TestAppContext) {
    let f = fake("consent-bump");
    state::save(
        &f.env.state,
        &state::OnboardingState {
            completed: true,
            step: state::Step::Done,
            version: state::STATE_VERSION,
        },
    )
    .unwrap();
    consent::record_consent(&f.env.state, std::time::SystemTime::now()).unwrap();
    let old = consent::CONSENT_VERSION.saturating_sub(1);
    assert!(old < consent::CONSENT_VERSION);
    fs::write(
        consent::consent_path(&f.env.state),
        format!(
            "version = {old}\naccepted_at = \"2024-01-01T00:00:00Z\"\napp_version = \"0.1.0\"\n"
        ),
    )
    .unwrap();
    assert!(consent::needs_consent(&f.env.state));

    let cx = open(cx, &f.env);
    start(cx);
    assert!(wizard_window(cx).is_none());
    assert!(!consent::needs_consent(&f.env.state));
}

// OWE-714: 구 4단계 흐름 중간(점검·연결·완료)에서 멈춘 사용자는 동의가 있으므로 다시 묻지
// 않고 완료로 넘긴다(체크리스트는 설정 점검에서만).
#[gpui::test]
fn wizard_legacy_mid_flow_with_consent_completes_silently(cx: &mut TestAppContext) {
    for step in [
        state::Step::Checklist,
        state::Step::Connect,
        state::Step::Done,
    ] {
        let f = fake(&format!("resume-{step:?}"));
        consent::record_consent(&f.env.state, std::time::SystemTime::now()).unwrap();
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
// UI 차단은 wizard_required_fail_blocks_next(필수 fail이면 다음 비활성)가 같은
// required_blockers 경로로 보장하고, 여기서는 판정→차단 연결을 고정한다.
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

// OWE-694 시나리오 7: 설정·메뉴·팔레트(같은 open_checks/rerun 경로)는 창을 하나만 쓴다.
#[gpui::test]
fn wizard_entries_share_single_window(cx: &mut TestAppContext) {
    let f = fake("entries-single");
    cx.update(gpui_component::init);
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
    // 명시적 재실행은 같은 창을 동의부터 다시 시작한다.
    cx.update(|cx| rerun_onboarding(&f.env, cx));
    cx.run_until_parked();
    assert_eq!(count(), 1);
    let step = cx.update(|cx| {
        cx.windows()
            .into_iter()
            .find_map(|w| w.downcast::<welcome::Wizard>())
            .expect("wizard")
            .update(cx, |v, _, _| v.step)
            .unwrap()
    });
    assert_eq!(step, state::Step::Consent);
    // 재실행 뒤에도 점검 진입은 창을 복제하지 않는다.
    cx.update(|cx| open_checks(&f.env, cx));
    cx.run_until_parked();
    assert_eq!(count(), 1);
}

// OWE-694 시나리오 7(시작 경로): offer는 이미 열린 마법사 창을 복제하지 않는다.
#[gpui::test]
fn wizard_offer_does_not_duplicate_open_wizard(cx: &mut TestAppContext) {
    let f = fake("offer-single");
    let cx = open(cx, &f.env);
    assert_eq!(wizard_step(cx), state::Step::Consent);
    cx.update(|window, cx| offer(f.env.clone(), window, cx));
    cx.run_until_parked();
    let count = cx
        .cx
        .windows()
        .into_iter()
        .filter(|w| w.downcast::<welcome::Wizard>().is_some())
        .count();
    assert_eq!(count, 1);
}

// OWE-694 시나리오 7(재실행): 동의-only 사용자의 명시적 재실행은 전체 흐름으로 돌아간다.
#[gpui::test]
fn wizard_rerun_after_consent_only_restores_full_flow(cx: &mut TestAppContext) {
    let f = fake("rerun-full");
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
    assert!(!present(cx, "wizard-agent"));
    // 같은 창에서 재실행하면 consent_only가 꺼져 기본 에이전트도 묻는다.
    cx.update(|_, cx| rerun_onboarding(&f.env, cx));
    cx.run_until_parked();
    assert_eq!(wizard_step(cx), state::Step::Consent);
    assert!(present(cx, "wizard-agent"));
    start(cx);
    assert!(wizard_window(cx).is_none());
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
        "Codex hook이 설치돼 있지 않습니다"
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

// OWE-714 D1: [시작하기] 직후 점검이 데몬 소켓 준비 전에 돌아도, 잠깐 기다려 다시 점검한 뒤
// 판정한다. 준비되면 막는 화면 없이 닫힌다.
#[gpui::test]
fn wizard_waits_for_late_daemon_instead_of_blocking(cx: &mut TestAppContext) {
    let f = fake("late-daemon");
    late_daemon(&f, 2);
    let cx = open(cx, &f.env);
    let checks = wait_checks(cx);
    let daemon = checks.iter().find(|c| c.id == "daemon.connect").unwrap();
    assert_eq!(daemon.status, Status::Ok, "{daemon:?}");
    start(cx);
    assert!(wizard_window(cx).is_none(), "late daemon must not block");
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
