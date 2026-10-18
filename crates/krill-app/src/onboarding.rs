//! 첫 실행 온보딩(MK-073 뼈대, MK-127·OWE-904: 첫 실행 모달 없음).
//!
//! - 첫 실행: 모달을 띄우지 않는다. [`offer`]가 완료를 남기고 비번들 스테이징·자동 준비만
//!   백그라운드로 돌린다. 꼭 필요한 설치 안내는 조치 배너([`banner`])와 빠른 점검
//!   ([`spawn_quick_check`]) → 상태바 경고 → 점검 모달로 한다.
//! - 점검 모달: staged `krill doctor --json --no-start` + `app.*`를 등급별 그룹으로 보여준다.
//!   행의 수정 버튼·모두 다시 검사·창 재포커스 재검사(1초 디바운스)는 [`welcome`]에 있다.
//!   hook 설치(diff 미리보기 30줄, [`DIFF_LINES`])와 `krill` CLI 링크도 행 안에서 한다.
//! - 완료: `onboarding.toml`([`state`])에 완료를 남긴다. 구 `done` 파일은 마이그레이션
//!   입력으로만 읽는다.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gpui::{App, KeyBinding, actions};
use krill_proto::agent::hooks::Agent;
use krill_proto::agent::{self, Agents};
use krill_proto::doctor::{Check, Fix, FixKind, Level, Status};
use rustix::fs::{Access, access};

actions!(onboarding, [OpenChecks]);

/// 끝까지 간 뒤 상태 디렉터리에 만드는 표시 파일(구 방식, 마이그레이션 입력으로만 읽는다).
pub const DONE_FILE: &str = "onboarding.done";
/// 로그인 셸이 PATH를 알려 주기까지 기다리는 최대 시간. 멈춘 rc 파일이 온보딩을 붙잡지 않게 한다.
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);
/// rc 파일이 stdout에 무엇을 찍어도 PATH 줄을 골라내기 위한 표시.
const PATH_MARK: &str = "__MK_PATH__=";
/// 대화상자에 보이는 diff 최대 줄 수. 나머지는 CLI로 본다.
const DIFF_LINES: usize = 30;
/// 리더 스레드가 파이프 EOF 없이 버티는 추가 유예. 호출자는 [`SHELL_TIMEOUT`]에
/// 먼저 끊고, 리더는 이 유예가 지나면 종료해 스레드 누수를 막는다(AS-14).
const READER_GRACE: Duration = Duration::from_secs(5);

use crate::i18n::catalog::checks as ctext;
use crate::i18n::catalog::onboarding as text;
use crate::i18n::catalog::settings as stext;

/// 온보딩이 쓰는 환경. 테스트는 가짜 셸·`krill`·디렉터리를 넣는다.
#[derive(Debug, Clone)]
pub struct Env {
    /// 로그인 셸(`$SHELL`).
    pub shell: OsString,
    /// 앱과 함께 배포된 `krill`(앱 실행 파일 옆).
    pub krill: PathBuf,
    /// 상태 디렉터리([`krill_proto::daemon::state_dir`]).
    pub state: PathBuf,
    pub home: PathBuf,
    /// 상태 디렉터리 재정의(`KRILL_STATE_DIR`) 여부. 프로세스 환경은 `current`에서만 읽고 테스트는 직접 주입한다.
    pub state_overridden: bool,
}

impl Env {
    /// 이 프로세스의 환경. HOME이나 실행 파일 경로를 모르면 `None`(온보딩을 띄우지 않는다).
    pub fn current() -> Option<Self> {
        Some(Self {
            shell: std::env::var_os("SHELL")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "/bin/zsh".into()),
            krill: krill_proto::daemon::sibling_exe("krill").ok()?,
            state: krill_proto::daemon::state_dir()?,
            home: std::env::var_os("HOME")?.into(),
            state_overridden: krill_proto::daemon::state_dir_override().is_some(),
        })
    }

    pub(crate) fn is_isolated_state(&self) -> bool {
        self.state_overridden || self.state != default_state_dir(&self.home)
    }
}

/// 감지한 에이전트 하나.
#[derive(Debug, Clone)]
pub struct Found {
    pub id: String,
    pub name: String,
    pub exe: Option<PathBuf>,
    pub version: Option<String>,
    pub install: String,
}

/// `krill` CLI 설치 상태.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cli {
    /// PATH의 `krill`가 앱의 `krill`다.
    Installed,
    /// PATH에 다른 `krill`가 있다(Homebrew 코어의 같은 이름 포뮬러 등). 덮어쓰지 않는다.
    Conflict(PathBuf),
    /// 이 경로에 링크를 만들 수 있다.
    Offer(PathBuf),
    /// PATH 안에 쓰기 가능한 후보 디렉터리가 없다.
    Manual,
}

/// 감지 결과.
#[derive(Debug, Clone)]
pub struct Survey {
    /// 로그인 셸의 PATH(실패하면 이 프로세스의 PATH).
    pub path: OsString,
    pub git: Option<String>,
    pub agents: Vec<Found>,
    pub cli: Cli,
}

/// 살아 있는 `login_path` 리더 스레드 수(AS-14 누수 감시).
static READER_THREADS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn login_path_reader_count() -> usize {
    READER_THREADS.load(Ordering::Relaxed)
}

struct ReaderGuard;

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        READER_THREADS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// `shell -l -i -c`로 로그인·대화형 셸의 PATH를 얻는다. 실패하거나 [`SHELL_TIMEOUT`]을 넘기면 `None`.
pub fn login_path(shell: &OsStr) -> Option<OsString> {
    let script = format!("printf '\\n{PATH_MARK}%s\\n' \"$PATH\"");
    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // 파이프를 쥔 좀비 자식과 함께 리더 스레드가 영원히 잠들지 않게 논블로킹
    // 조각 읽기로 바꾼다. fcntl 실패면 기존 블로킹 읽기로 둔다(호출자 타임아웃은
    // 그대로라 반환은 보장되고, 리더만 좀비 자식 수명만큼 산다).
    rustix::fs::fcntl_setfl(&stdout, rustix::fs::OFlags::NONBLOCK).ok();
    // rc가 띄운 백그라운드 프로세스가 파이프를 쥐면 읽기가 끝나지 않으므로 시간으로 끊는다.
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("mk-login-path-reader".into())
        .spawn(move || {
            let _guard = ReaderGuard;
            READER_THREADS.fetch_add(1, Ordering::Relaxed);
            let deadline = std::time::Instant::now() + SHELL_TIMEOUT + READER_GRACE;
            let mut out = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => out.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
                if out.len() > 1 << 20 || std::time::Instant::now() >= deadline {
                    break;
                }
            }
            tx.send(String::from_utf8_lossy(&out).into_owned()).ok();
        })
        .ok()?;
    let out = rx.recv_timeout(SHELL_TIMEOUT);
    child.kill().ok();
    child.wait().ok();
    out.ok()?
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix(PATH_MARK))
        .filter(|p| !p.is_empty())
        .map(OsString::from)
}

/// git·에이전트·`krill` CLI를 감지한다. 셸과 `--version`을 띄우므로 백그라운드에서 부른다.
pub fn survey(env: &Env) -> Survey {
    let path =
        login_path(&env.shell).unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
    let git = agent::which("git", &path).and_then(|g| agent::version(&g));
    let user = std::fs::read_to_string(env.state.join(agent::FILE_NAME)).ok();
    let agents = Agents::load(user.as_deref())
        .or_else(|_| Agents::load(None))
        .unwrap_or_default();
    let agents = agents
        .profiles
        .into_iter()
        .map(|(id, p)| {
            let exe = p.program().and_then(|prog| agent::which(prog, &path));
            Found {
                version: exe.as_deref().and_then(agent::version),
                exe,
                id,
                name: p.name,
                install: p.install,
            }
        })
        .collect();
    let cli = if env.is_isolated_state() {
        Cli::Installed
    } else {
        cli(&env.krill, &path, &env.home)
    };
    Survey {
        path,
        git,
        agents,
        cli,
    }
}

fn default_state_dir(home: &Path) -> PathBuf {
    krill_proto::daemon::state_dir_in(home)
}

fn in_app_bundle(krill: &Path) -> bool {
    krill
        .ancestors()
        .any(|p| p.extension().is_some_and(|ext| ext == "app"))
}

/// 심볼릭 링크가 가리킬 안정 경로. 앱 번들 안의 `krill`면 그대로 쓰고,
/// 워크트리·임시 빌드면 워크트리가 지워져도 끊기지 않게 `~/.local/share/minikrill/bin/krill`를 쓴다.
pub(super) fn stable_mk_path(krill: &Path, home: &Path) -> PathBuf {
    if in_app_bundle(krill) {
        krill.to_path_buf()
    } else {
        home.join(".local/share/minikrill/bin/krill")
    }
}

fn is_broken_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok() && std::fs::metadata(path).is_err()
}

fn is_replaceable_broken_symlink(path: &Path) -> bool {
    is_broken_symlink(path)
        && path
            .parent()
            .is_some_and(|d| access(d, Access::WRITE_OK | Access::EXEC_OK).is_ok())
}

fn cli(krill: &Path, path: &OsStr, home: &Path) -> Cli {
    let stable = stable_mk_path(krill, home);
    for dir in std::env::split_paths(path) {
        let candidate = dir.join("krill");
        if access(&candidate, Access::EXEC_OK).is_ok() && candidate.is_file() {
            let found_canon = std::fs::canonicalize(&candidate).ok();
            let same = found_canon.is_some()
                && (found_canon == std::fs::canonicalize(krill).ok()
                    || found_canon == std::fs::canonicalize(&stable).ok());
            return if same {
                Cli::Installed
            } else {
                Cli::Conflict(candidate)
            };
        }
        if is_broken_symlink(&candidate) {
            return Cli::Conflict(candidate);
        }
    }
    // 사용자가 소유하는 흔한 PATH 디렉터리만 고른다. 시스템 디렉터리에 쓰려고 권한을 올리지 않는다.
    let preferred = [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".local/bin"),
        home.join("bin"),
    ];
    std::env::split_paths(path)
        .find(|d| preferred.contains(d) && access(d, Access::WRITE_OK | Access::EXEC_OK).is_ok())
        .map_or(Cli::Manual, |d| Cli::Offer(d.join("krill")))
}

/// hook 동의를 물을 에이전트: 설치돼 있고 MK-048 대상이며 아직 영수증이 없는 것.
pub fn pending_hooks(s: &Survey, state: &Path) -> Vec<(Agent, String)> {
    s.agents
        .iter()
        .filter(|a| a.exe.is_some())
        .filter_map(|a| Agent::parse(&a.id).map(|agent| (agent, a.name.clone())))
        .filter(|(agent, _)| {
            !state
                .join("hooks")
                .join(format!("{}.json", agent.id()))
                .exists()
        })
        .collect()
}

/// `krill hook install <agent> <flag>`. 에이전트 CLI(Codex app-server 등)를 찾도록 로그인 PATH를 넘긴다.
fn mk_hook(krill: &Path, path: &OsStr, agent: Agent, flag: &str) -> Result<String, String> {
    if !krill.is_file() {
        return Err(text::MISSING_BIN
            .get()
            .replace("{path}", &krill.display().to_string()));
    }
    let out = Command::new(krill)
        .args(["hook", "install", agent.id(), flag])
        .env("PATH", path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("krill: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// 로그인 셸 `PATH`를 사용해 백그라운드에서 Codex 상태 감지 훅을 설치한다.
pub(crate) fn install_codex_hook_with_login_path(
    env: &Env,
    state: &Path,
) -> Result<String, String> {
    if state.join("hooks/codex.json").is_file() {
        return Ok(String::new());
    }
    let path =
        login_path(&env.shell).unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
    let krill = stage_binaries(&env.krill, &env.home).unwrap_or_else(|_| env.krill.clone());
    mk_hook(&krill, &path, Agent::Codex, "--yes").or_else(|_| {
        crate::settings::install_agent_hook_at("codex", &env.home, state)
            .map(|p| p.display().to_string())
            .map_err(|e| e.to_string())
    })
}

/// diff를 [`DIFF_LINES`]줄로 줄인다. 줄 단위라 한글이 잘리지 않는다.
pub(super) fn clip(diff: &str, agent_id: &str) -> String {
    let lines: Vec<&str> = diff.lines().collect();
    let mut shown = lines
        .iter()
        .take(DIFF_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(more) = lines.len().checked_sub(DIFF_LINES).filter(|n| *n > 0) {
        shown.push('\n');
        shown.push_str(
            &text::DIFF_MORE
                .get()
                .replace("{n}", &more.to_string())
                .replace("{id}", agent_id),
        );
    }
    shown
}

/// 메인 창(`Shell`)이 점검 모달을 인앱으로 열 때 쓰는 명령(OWE-903).
/// 별도 OS 창을 띄우지 않고 메인 창 안 `ui::modal_backdrop` 모달로 연다.
#[derive(Clone, Debug)]
pub enum HostCommand {
    /// 점검 모달을 연다. 이미 열려 있으면 앞으로 가져온다(상태는 유지).
    Open { env: Env },
}

type HostHandler = std::rc::Rc<dyn Fn(&HostCommand, &mut App) -> Option<bool>>;

struct HostRegistry {
    handlers: Vec<HostHandler>,
}

impl gpui::Global for HostRegistry {}

/// 메인 창(`Shell`)을 온보딩 모달 호스트로 등록한다.
/// 콜백은 호스트 엔티티가 이미 해제됐으면 `None`, 이 창이 처리했으면 `Some(true)`,
/// 건너뛰면 `Some(false)`를 돌려준다.
pub fn register_host(
    cx: &mut App,
    handler: impl Fn(&HostCommand, &mut App) -> Option<bool> + 'static,
) {
    let handler: HostHandler = std::rc::Rc::new(handler);
    if cx.has_global::<HostRegistry>() {
        cx.global_mut::<HostRegistry>().handlers.push(handler);
    } else {
        cx.set_global(HostRegistry {
            handlers: vec![handler],
        });
    }
}

/// 호스트 디스패치를 현재 업데이트가 끝난 뒤로 미룬다. 호출자가 `Shell` 업데이트 안
/// (예: `main.rs`의 `window.update` 안 `offer`)에 있어도 핸들러가 `Shell`을 다시 읽다가
/// 재진입 panic이 나지 않게 한다(OWE-903 첫 실행 크래시).
fn dispatch_host(cmd: HostCommand, what: &'static str, cx: &mut App) {
    cx.defer(move |cx| {
        if !dispatch_now(&cmd, cx) {
            tracing::warn!("onboarding modal host missing; ignoring {what}");
        }
    });
}

fn dispatch_now(cmd: &HostCommand, cx: &mut App) -> bool {
    let Some(mut handlers) = cx.try_global::<HostRegistry>().map(|r| r.handlers.clone()) else {
        return false;
    };
    let mut handled = false;
    let mut i = handlers.len();
    while i > 0 {
        i -= 1;
        match handlers[i](cmd, cx) {
            Some(true) => {
                handled = true;
                break;
            }
            Some(false) => {}
            None => {
                handlers.remove(i);
            }
        }
    }
    if cx.has_global::<HostRegistry>() {
        cx.global_mut::<HostRegistry>().handlers = handlers;
    }
    handled
}

/// 첫 실행 진입. 모달 없이 완료를 남기고, 격리 상태가 아니면 비번들 스테이징 바이너리
/// 갱신·자동 준비를 백그라운드로 돌린다(OWE-904). 준비 중 필요한 조치는 조치 배너가 알린다.
pub fn offer(env: Env, cx: &mut App) {
    let stored = state::load(&env.state);
    if !stored.completed
        && let Err(e) = state::save(&env.state, &stored.completed())
    {
        tracing::warn!(%e, "onboarding completion save failed");
    }
    if !env.is_isolated_state() {
        actions::auto_prepare(env, cx).detach();
    }
}

/// 메뉴·팔레트 액션을 등록한다. 앱 시작 때 한 번 부른다.
pub fn init_actions(cx: &mut App) {
    cx.on_action(|_: &OpenChecks, cx| open_checks_current(cx));
    // 마법사(점검) 창의 macOS 표준 ⌘W 닫기(OWE-861·878, ⌥⌘W는 전역 CloseWindow). Esc는 마법사 `on_key_down`이 맡는다.
    cx.bind_keys([KeyBinding::new(
        "cmd-w",
        welcome::CloseWizard,
        Some(welcome::KEY_CONTEXT),
    )]);
}

/// 현재 환경으로 점검 창을 연다. 메뉴·설정·상태바 진입점이 함께 쓴다.
pub fn open_checks_current(cx: &mut App) {
    if let Some(env) = Env::current() {
        open_checks(&env, cx);
    }
}

/// 점검 모달을 연다. 이미 열려 있으면 앞으로 가져온다.
pub fn open_checks(env: &Env, cx: &mut App) {
    dispatch_host(HostCommand::Open { env: env.clone() }, "open_checks", cx);
}

/// 테스트용 창 호스트(OWE-903). 프로덕션 `Shell` 대신 마법사를 별도 창 루트로 열어
/// 기존 창 단위 테스트 헬퍼를 그대로 쓴다. 닫기 이벤트를 받으면 창을 닫는다.
#[cfg(test)]
pub(crate) fn register_test_window_host(cx: &mut App) {
    use gpui::AppContext as _;

    struct TestHostSubs {
        subs: Vec<gpui::Subscription>,
    }

    impl gpui::Global for TestHostSubs {}

    if cx.has_global::<TestHostSubs>() {
        return;
    }
    cx.set_global(TestHostSubs { subs: Vec::new() });
    register_host(cx, |cmd, cx| {
        if let Some(w) = cx
            .windows()
            .into_iter()
            .find_map(|w| w.downcast::<welcome::Wizard>())
        {
            w.update(cx, |_, window, _| window.activate_window()).ok();
            return Some(true);
        }
        let HostCommand::Open { env } = cmd;
        let env = env.clone();
        let opened = cx.open_window(gpui::WindowOptions::default(), |window, cx| {
            let view = cx.new(|cx| welcome::Wizard::new(env, cx));
            let handle = window.window_handle();
            let sub = cx.subscribe(&view, move |_, _: &welcome::Event, cx| {
                handle
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            });
            cx.global_mut::<TestHostSubs>().subs.push(sub);
            view.update(cx, |w, cx| {
                w.observe_activation(window, cx);
                w.begin_collect(cx);
            });
            let focus = view.read(cx).focus_handle();
            window.focus(&focus, cx);
            view
        });
        Some(opened.is_ok())
    });
}

/// 마지막 점검 스냅샷(설정 섹션·상태바가 읽는다). 마법사 재검사·빠른 점검이 갱신한다.
#[derive(Debug, Clone, Default)]
pub struct ChecksSnapshot {
    pub at: Option<std::time::SystemTime>,
    pub checks: Vec<Check>,
}

impl gpui::Global for ChecksSnapshot {}

/// 스냅샷을 읽는다. 한 번도 검사하지 않았으면 비어 있다.
#[must_use]
pub fn snapshot(cx: &App) -> ChecksSnapshot {
    cx.try_global::<ChecksSnapshot>()
        .cloned()
        .unwrap_or_default()
}

/// 스냅샷을 지금 시각으로 저장한다. `set_global`이라 감시자도 깨운다.
pub fn store_snapshot(cx: &mut App, checks: Vec<Check>) {
    cx.set_global(ChecksSnapshot {
        at: Some(std::time::SystemTime::now()),
        checks,
    });
}

/// 필수 실패 행.
#[must_use]
pub fn required_failures(checks: &[Check]) -> Vec<&Check> {
    checks
        .iter()
        .filter(|c| c.level == Level::Required && c.status == Status::Fail)
        .collect()
}

/// 권장 주의 개수.
#[must_use]
pub fn recommended_warnings(checks: &[Check]) -> usize {
    checks
        .iter()
        .filter(|c| c.level == Level::Recommended && c.status == Status::Warn)
        .count()
}

/// 설정 섹션에 보이는 한 줄 요약.
#[must_use]
pub fn checks_summary_text(snapshot: &ChecksSnapshot) -> String {
    let Some(at) = snapshot.at else {
        return stext::CHECKS_NEVER.get().to_owned();
    };
    stext::CHECKS_SUMMARY.format(&[
        ("ago", &ago_text(at.elapsed().unwrap_or(Duration::ZERO))),
        (
            "req",
            &required_failures(&snapshot.checks).len().to_string(),
        ),
        ("rec", &recommended_warnings(&snapshot.checks).to_string()),
    ])
}

fn ago_text(elapsed: Duration) -> String {
    if elapsed.as_secs() < 60 {
        stext::CHECKS_JUST_NOW.get().to_owned()
    } else if elapsed.as_secs() < 3_600 {
        stext::CHECKS_MINUTES_AGO.format(&[("n", &(elapsed.as_secs() / 60).to_string())])
    } else {
        stext::CHECKS_HOURS_AGO.format(&[("n", &(elapsed.as_secs() / 3_600).to_string())])
    }
}

/// 시작 시 빠른 점검(프롬프트·데몬 기동 없음): `bin.*` + `state.writable` + `app.location`.
/// 백그라운드에서만 부른다. 벽시계는 호출자가 잰다.
#[must_use]
pub fn quick_checks(env: &Env) -> Vec<Check> {
    vec![
        quick_helpers(env),
        quick_version(env),
        quick_state(env),
        app_location_check(env),
    ]
}

/// 빠른 점검을 백그라운드에서 돌려 스냅샷에 저장한다. 시작 경로를 막지 않는다.
pub fn spawn_quick_check(env: &Env, cx: &mut App) {
    let env = env.clone();
    cx.spawn(async move |cx| {
        let checks = cx
            .background_executor()
            .spawn(async move { quick_checks(&env) })
            .await;
        cx.update(|cx| store_snapshot(cx, checks));
    })
    .detach();
}

fn quick_helpers(env: &Env) -> Check {
    let missing: Vec<String> = ["krill-holder", "krilld"]
        .into_iter()
        .filter(|name| {
            let path = env.krill.parent().unwrap_or(Path::new("")).join(name);
            rustix::fs::access(&path, Access::EXEC_OK).is_err()
        })
        .map(str::to_owned)
        .collect();
    if missing.is_empty() {
        Check {
            id: "bin.helpers".to_owned(),
            level: Level::Required,
            status: Status::Ok,
            summary: "helpers are present".to_owned(),
            detail: String::new(),
            fix: Fix {
                kind: FixKind::None,
                value: String::new(),
            },
        }
    } else {
        Check {
            id: "bin.helpers".to_owned(),
            level: Level::Required,
            status: Status::Fail,
            summary: "helpers are missing or not executable".to_owned(),
            detail: format!("{} (reinstall Krill)", missing.join(", ")),
            fix: Fix {
                kind: FixKind::Reinstall,
                value: String::new(),
            },
        }
    }
}

fn command_version(binary: &Path) -> Option<String> {
    let out = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| {
        let line = String::from_utf8_lossy(&out.stdout);
        line.lines().next().map(str::trim).map(str::to_owned)
    })?
}

/// `--version` 앞부분(릴리스 버전). 개발 해시(`+dev.…`)가 달라도 같은 릴리스면 일치한다.
fn version_prefix(build: &str) -> &str {
    build.split_once('+').map_or(build, |(v, _)| v)
}

fn quick_version(env: &Env) -> Check {
    let krilld = env.krill.parent().unwrap_or(Path::new("")).join("krilld");
    match (command_version(&env.krill), command_version(&krilld)) {
        (Some(a), Some(b)) if version_prefix(&a) == version_prefix(&b) => Check {
            id: "bin.version".to_owned(),
            level: Level::Required,
            status: Status::Ok,
            summary: "binaries are the same version".to_owned(),
            detail: version_prefix(&a).to_owned(),
            fix: Fix {
                kind: FixKind::None,
                value: String::new(),
            },
        },
        (Some(_), Some(_)) => Check {
            id: "bin.version".to_owned(),
            level: Level::Required,
            status: Status::Fail,
            summary: "binary versions differ".to_owned(),
            detail: "reinstall Krill".to_owned(),
            fix: Fix {
                kind: FixKind::Reinstall,
                value: String::new(),
            },
        },
        _ => Check {
            id: "bin.version".to_owned(),
            level: Level::Required,
            status: Status::Skip,
            summary: "versions not compared".to_owned(),
            detail: "a --version call failed".to_owned(),
            fix: Fix {
                kind: FixKind::None,
                value: String::new(),
            },
        },
    }
}

fn quick_state(env: &Env) -> Check {
    let writable = [env.state.clone(), env.state.join("run")]
        .into_iter()
        .filter(|dir| dir.exists())
        .all(|dir| access(&dir, Access::WRITE_OK | Access::EXEC_OK).is_ok());
    if writable {
        Check {
            id: "state.writable".to_owned(),
            level: Level::Required,
            status: Status::Ok,
            summary: "state directory is writable".to_owned(),
            detail: env.state.display().to_string(),
            fix: Fix {
                kind: FixKind::None,
                value: String::new(),
            },
        }
    } else {
        Check {
            id: "state.writable".to_owned(),
            level: Level::Required,
            status: Status::Fail,
            summary: "state directory is not writable".to_owned(),
            detail: format!("{} (check owner: ls -ld)", env.state.display()),
            fix: Fix {
                kind: FixKind::Command,
                value: format!("ls -ld {}", env.state.display()),
            },
        }
    }
}

fn app_location_check(env: &Env) -> Check {
    use crate::platform::permissions as perm;
    let exe = std::env::current_exe()
        .ok()
        .unwrap_or_else(|| env.krill.clone());
    perm::decide_location(&exe, perm::is_bundled())
}

/// 점검 ID의 표시 제목. 아는 ID는 카탈로그(ko/en)로, 모르는 ID는 원문 그대로 둔다.
#[must_use]
pub fn check_title(id: &str) -> String {
    match id {
        "bin.helpers" => ctext::TITLE_BIN_HELPERS.get().to_owned(),
        "bin.version" => ctext::TITLE_BIN_VERSION.get().to_owned(),
        "state.writable" => ctext::TITLE_STATE_WRITABLE.get().to_owned(),
        "daemon.connect" => ctext::TITLE_DAEMON_CONNECT.get().to_owned(),
        "history.key" => ctext::TITLE_HISTORY_KEY.get().to_owned(),
        "shell.path" => ctext::TITLE_SHELL_PATH.get().to_owned(),
        "git" => ctext::TITLE_GIT.get().to_owned(),
        "agent.any" => ctext::TITLE_AGENT_ANY.get().to_owned(),
        "cli.path" => ctext::TITLE_CLI_PATH.get().to_owned(),
        "app.location" => ctext::TITLE_APP_LOCATION.get().to_owned(),
        "app.notifications" => ctext::TITLE_APP_NOTIFICATIONS.get().to_owned(),
        "app.quarantine" => ctext::TITLE_APP_QUARANTINE.get().to_owned(),
        "app.folders" => ctext::TITLE_APP_FOLDERS.get().to_owned(),
        "app.full_disk" => ctext::TITLE_APP_FULL_DISK.get().to_owned(),
        _ => {
            if let Some(hook) = id.strip_prefix("hook.") {
                return ctext::TITLE_HOOK.format(&[("name", agent_name(hook))]);
            }
            if let Some(agent) = id.strip_prefix("agent.") {
                return ctext::TITLE_AGENT.format(&[("name", agent_name(agent))]);
            }
            id.to_owned()
        }
    }
}

/// `krill doctor` 문구 카탈로그. 각 키의 영어 원문이 `krill doctor --json`의 `summary`·`detail`과
/// 글자 그대로 같다(`--json` 출력은 바꾸지 않는다). 고정 문구를 틀(`{…}`)보다 먼저 둔다
/// (`no agent detected`가 `{name} detected`에 걸리지 않게).
const DOCTOR_TEXT: &[crate::i18n::Message] = &[
    ctext::D_HELPERS_OK,
    ctext::D_HELPERS_FAIL,
    ctext::D_VERSION_SKIP,
    ctext::D_VERSION_SKIP_DETAIL,
    ctext::D_VERSION_OK,
    ctext::D_VERSION_DIFF,
    ctext::D_STATE_OK,
    ctext::D_STATE_FAIL,
    ctext::D_DAEMON_OK,
    ctext::D_DAEMON_FAIL,
    ctext::D_DAEMON_SKIP,
    ctext::D_HOLDER_UNKNOWN,
    ctext::D_HOLDER_UNKNOWN_DETAIL,
    ctext::D_HOLDER_OK,
    ctext::D_HOLDER_OK_DETAIL,
    ctext::D_HOLDER_LOST,
    ctext::D_HOLDER_LOST_DETAIL,
    ctext::D_HISTORY_OFF,
    ctext::D_HISTORY_OFF_DETAIL,
    ctext::D_HISTORY_WARN,
    ctext::D_HISTORY_WARN_DETAIL,
    ctext::D_HISTORY_OK,
    ctext::D_HISTORY_OK_DETAIL,
    ctext::D_SHELL_OK,
    ctext::D_SHELL_OK_DETAIL,
    ctext::D_SHELL_FAIL,
    ctext::D_SHELL_FAIL_DETAIL,
    ctext::D_GIT_OK,
    ctext::D_GIT_WARN,
    ctext::D_GIT_WARN_DETAIL,
    ctext::D_AGENTS_NONE,
    ctext::D_AGENTS_NONE_DETAIL,
    ctext::D_VERSION_UNKNOWN,
    ctext::D_HOOK_NO_AGENT_DETAIL,
    ctext::D_HOOK_OK_DETAIL,
    ctext::D_HOOK_WARN_DETAIL,
    ctext::D_CLI_OK,
    ctext::D_CLI_CONFLICT,
    ctext::D_CLI_MISSING,
    ctext::D_HELPERS_MISSING,
    ctext::D_VERSION_DIFF_DETAIL,
    ctext::D_STATE_FAIL_DETAIL,
    ctext::D_AGENTS_FOUND,
    ctext::D_AGENT_DETECTED,
    ctext::D_HOOK_NO_AGENT,
    ctext::D_HOOK_OK,
    ctext::D_HOOK_WARN,
    ctext::D_CLI_CONFLICT_DETAIL,
];

/// `krill doctor` 점검 문구(영어 원문)를 화면 언어로 옮긴다(OWE-714). 아는 문구·틀만 옮기고
/// 경로·버전 같은 값과 모르는 문구는 원문 그대로 둔다. `{name}` 자리의 에이전트 ID는 표시 이름으로.
#[must_use]
pub fn check_text(text: &str) -> String {
    for message in DOCTOR_TEXT {
        let template = message.in_language(crate::i18n::Language::En);
        if let Some(fields) = match_template(template, text) {
            let fields: Vec<(&str, &str)> = fields
                .into_iter()
                .map(|(k, v)| {
                    if k == "name" {
                        (k, agent_name(v))
                    } else {
                        (k, v)
                    }
                })
                .collect();
            return message.format(&fields);
        }
    }
    text.to_owned()
}

/// `a {x} b {y} c` 틀에 `text`가 맞으면 자리 값들을 돌려준다. 자리 값은 비어 있으면 안 된다.
/// 마지막 자리는 끝 문구 앞까지, 나머지는 다음 문구가 처음 나오는 곳까지다.
fn match_template<'a, 't>(template: &'t str, text: &'a str) -> Option<Vec<(&'t str, &'a str)>> {
    let (lead, mut rest_template) = template.split_once('{').unwrap_or((template, ""));
    if rest_template.is_empty() {
        return (template == text).then(Vec::new);
    }
    let mut rest = text.strip_prefix(lead)?;
    let mut fields = Vec::new();
    while let Some((name, tail)) = rest_template.split_once('}') {
        let (literal, next) = tail.split_once('{').unwrap_or((tail, ""));
        let value = if next.is_empty() {
            let value = rest.strip_suffix(literal)?;
            rest = "";
            value
        } else {
            let (value, after) = rest.split_once(literal)?;
            rest = after;
            value
        };
        if value.is_empty() {
            return None;
        }
        fields.push((name, value));
        rest_template = next;
        if next.is_empty() {
            break;
        }
    }
    rest.is_empty().then_some(fields)
}

pub(super) fn agent_name(id: &str) -> &str {
    match id {
        "codex" => "Codex",
        "opencode" => "OpenCode",
        "cursor" => "Cursor",
        "claude" => "Claude Code",
        "agy" | "antigravity" => "Antigravity",
        _ => id,
    }
}

/// 마법사가 백그라운드에서 모으는 것(점검 창·연결 창이 함께 쓴다).
pub(super) struct Collected {
    pub(super) staged: Option<PathBuf>,
    pub(super) doctor: Vec<Check>,
    pub(super) app: Vec<Check>,
    pub(super) survey: Survey,
}

/// staged `krill`로 감지·버전을 모은다. 셸·`--version`을 띄우므로 백그라운드에서 부른다.
pub(super) fn collect(env: &Env) -> Collected {
    let staged = if env.is_isolated_state() {
        env.krill.is_file().then(|| env.krill.clone())
    } else {
        stage_binaries(&env.krill, &env.home).ok()
    };
    let doctor = staged.as_deref().map_or_else(doctor_failed_vec, |krill| {
        run_doctor_settled(krill, DAEMON_SETTLE)
    });
    let app = app_checks(env);
    let survey = survey(env);
    Collected {
        staged,
        doctor,
        app,
        survey,
    }
}

/// 앱이 띄운 krilld가 소켓을 열 때까지 점검이 기다리는 상한. 런처의 기동 대기(5초)와 같다.
const DAEMON_SETTLE: Duration = Duration::from_secs(5);
/// 데몬 연결 재검사 간격.
const DAEMON_SETTLE_POLL: Duration = Duration::from_millis(250);

/// 데몬 연결만 실패하면 `wait` 동안 다시 점검한다(OWE-714 D1). 앱은 시작과 함께 krilld를 띄우는데
/// 점검은 `--no-start`라, 소켓이 생기기 전에 돌면 "krilld와 통신할 수 없습니다"로 첫 화면을 막았다.
/// 기다린 뒤에도 실패면 그 결과를 그대로 돌려준다.
pub(super) fn run_doctor_settled(staged_krill: &Path, wait: Duration) -> Vec<Check> {
    let deadline = Instant::now() + wait;
    loop {
        let checks = run_doctor_checks(staged_krill);
        let daemon_down = checks
            .iter()
            .any(|c| c.id == "daemon.connect" && c.status == Status::Fail);
        if !daemon_down || Instant::now() >= deadline {
            return checks;
        }
        std::thread::sleep(DAEMON_SETTLE_POLL);
    }
}

/// staged `krill doctor --json --no-start`를 돌려 점검 목록으로 바꾼다.
/// `doctor`는 fail 점검이 있으면 종료 코드 1로 끝나므로 종료 코드와 무관하게 보고서부터
/// 읽는다(OWE-714: fail 1건이 보고서 전체를 "점검 실행 실패"로 덮던 결함). 보고서를 못 읽을
/// 때만 원인(실행 불가·종료 코드·stderr 첫 줄·형식 불일치)을 담은 `bin.helpers` fail 1건이다.
pub(super) fn run_doctor_checks(staged_krill: &Path) -> Vec<Check> {
    let output = Command::new(staged_krill)
        .args(["doctor", "--json", "--no-start"])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    let out = match output {
        Ok(out) => out,
        Err(e) => {
            return vec![doctor_failed_check(&ctext::DOCTOR_CAUSE_SPAWN.format(&[
                ("path", &staged_krill.display().to_string()),
                ("error", &e.to_string()),
            ]))];
        }
    };
    if let Some(checks) = parse_doctor_report(&out.stdout) {
        return checks;
    }
    let cause = if out.status.success() {
        ctext::DOCTOR_CAUSE_FORMAT.get().to_owned()
    } else {
        let code = out
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |c| c.to_string());
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        ctext::DOCTOR_CAUSE_EXIT.format(&[("code", &code), ("stderr", first.trim())])
    };
    vec![doctor_failed_check(&cause)]
}

fn parse_doctor_report(stdout: &[u8]) -> Option<Vec<Check>> {
    serde_json::from_slice::<krill_proto::doctor::Report>(stdout)
        .ok()
        .filter(|report| report.schema == krill_proto::doctor::SCHEMA)
        .map(|report| report.checks)
}

/// staged `krill`이 없을 때의 `bin.helpers` fail 1건(마법사 재검사 경로용).
pub(super) fn doctor_failed_vec() -> Vec<Check> {
    vec![doctor_failed_check(ctext::DOCTOR_CAUSE_MISSING.get())]
}

fn doctor_failed_check(cause: &str) -> Check {
    Check {
        id: "bin.helpers".to_owned(),
        level: Level::Required,
        status: Status::Fail,
        summary: ctext::DOCTOR_FAILED_SUMMARY.get().to_owned(),
        detail: ctext::DOCTOR_FAILED_DETAIL.format(&[("cause", cause)]),
        fix: Fix {
            kind: FixKind::Reinstall,
            value: String::new(),
        },
    }
}

/// `app.*` 점검 묶음. 폴더 프로브는 자동 실행하지 않고 대기 상태로 둔다(버튼 게이트).
pub(super) fn app_checks(env: &Env) -> Vec<Check> {
    use crate::platform::permissions as perm;
    let bundled = perm::is_bundled();
    let exe = std::env::current_exe()
        .ok()
        .unwrap_or_else(|| env.krill.clone());
    vec![
        perm::decide_location(&exe, bundled),
        perm::decide_notifications(
            perm::query_notification_status(Duration::from_secs(2)),
            perm::bundle_id().as_deref(),
        ),
        perm::decide_quarantine(
            bundled && bundle_root(&exe).is_some_and(|root| perm::has_quarantine_attr(&root)),
            bundled,
        ),
        perm::folders_idle_check(),
        perm::decide_full_disk(perm::probe_full_disk()),
    ]
}

/// 실행 파일이 속한 `.app` 번들 디렉터리. 비번들 실행이면 `None`이다.
fn bundle_root(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|p| p.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
}

/// 비번들 빌드면 `krill`와 옆의 `krill-holder`·`krilld`를 `~/.local/share/minikrill/bin/`에 복사하고
/// 안정 경로를 돌려준다. 앱 번들 안이면 `krill` 경로를 그대로 돌려준다.
pub(crate) fn stage_binaries(krill: &Path, home: &Path) -> Result<PathBuf, String> {
    if !krill.is_file() {
        return Err(text::MISSING_BIN
            .get()
            .replace("{path}", &krill.display().to_string()));
    }
    let target = stable_mk_path(krill, home);
    if !in_app_bundle(krill) {
        stage_executable(krill, &target).map_err(|e| format!("krill: {e}"))?;
        if let (Some(src_dir), Some(dst_dir)) = (krill.parent(), target.parent()) {
            for helper in ["krill-holder", "krilld"] {
                let src = src_dir.join(helper);
                if src.is_file() {
                    stage_executable(&src, &dst_dir.join(helper))
                        .map_err(|e| format!("{helper}: {e}"))?;
                }
            }
        }
    }
    Ok(target)
}

fn install_cli_link(krill: &Path, home: &Path, link: &Path) -> io::Result<()> {
    let target = stage_binaries(krill, home).map_err(io::Error::other)?;
    if is_broken_symlink(link) {
        std::fs::remove_file(link)?;
    }
    std::os::unix::fs::symlink(&target, link)
}

fn stage_executable(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    if std::fs::canonicalize(src).ok() == std::fs::canonicalize(dst).ok() && dst.is_file() {
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dst.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::copy(src, &tmp)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, dst)
}

pub mod actions;
pub mod banner;
pub mod state;
pub mod welcome;

#[cfg(test)]
mod tests;
