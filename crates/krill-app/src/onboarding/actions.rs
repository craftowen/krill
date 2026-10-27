//! OWE-714: 점검 ID별 조치. 파일·시스템 작업은 백그라운드에서 수행한다.
//! 수동 훅 설치는 diff 확인 뒤 실행하며, 상태 감지가 켜져 있으면 자동으로도 설치한다.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Global, PromptLevel, Task};
use krill_proto::agent::hooks::Agent;
use krill_proto::doctor::{Check, Status};
use krill_proto::hook_install::{Action, Plan, TargetAgent};

use super::Env;
use crate::i18n::catalog::{onboarding as text, setup_actions as atext};
use crate::platform::permissions as perm;

/// 성공은 조치 요청·설치 완료를 뜻한다. 권한의 실제 허용 여부는 복귀 후 다시 검사한다.
pub type FixOutcome = Result<(), String>;

/// 행 조치의 실패. `Conflict`는 설치 계획이 기존 사용자 설정과 부딪혀 아무 파일도 바꾸지 않은
/// 경우다(OWE-714 D2). 오류가 아니라 안내라서 점검 화면이 주의 톤으로 다음 행동을 보여 준다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixError {
    /// 사용자가 동의·안내 대화상자에서 취소함. 설정을 바꾸지 않았고 오류도 아니다.
    Cancelled,
    Failed(String),
    /// `guide`는 화면 언어 안내, `detail`은 충돌 원문(펼쳐서 본다).
    Conflict {
        guide: String,
        detail: String,
    },
}

impl From<String> for FixError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl std::fmt::Display for FixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str(atext::CANCELLED.get()),
            Self::Failed(message) => f.write_str(message),
            Self::Conflict { guide, .. } => f.write_str(guide),
        }
    }
}

pub type FixResult = Result<(), FixError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixAction {
    Helpers,
    Cli,
    Hook(TargetAgent),
    Notifications,
    Settings(&'static str),
    Applications,
}

fn action_for(id: &str) -> Option<FixAction> {
    Some(match id {
        "bin.helpers" | "bin.version" => FixAction::Helpers,
        "cli.path" | "shell.path" => FixAction::Cli,
        perm::NOTIFICATIONS_ID => FixAction::Notifications,
        perm::FOLDERS_ID => FixAction::Settings(perm::FOLDERS_SETTINGS_URL),
        perm::FULL_DISK_ID => FixAction::Settings(perm::FULL_DISK_SETTINGS_URL),
        "app.automation" => FixAction::Settings(perm::AUTOMATION_SETTINGS_URL),
        perm::QUARANTINE_ID => FixAction::Settings(perm::SECURITY_SETTINGS_URL),
        perm::LOCATION_ID => FixAction::Applications,
        _ => FixAction::Hook(TargetAgent::parse(id.strip_prefix("hook.")?)?),
    })
}

/// 정상 항목은 버튼을 숨긴다. 폴더는 미검사 상태에서도 설정을 열 수 있다.
pub fn fix_label(check: &Check) -> Option<&'static str> {
    if check.status == Status::Ok || (check.status == Status::Skip && check.id != perm::FOLDERS_ID)
    {
        return None;
    }
    Some(match action_for(&check.id)? {
        FixAction::Helpers | FixAction::Cli | FixAction::Hook(_) => text::INSTALL.get(),
        FixAction::Notifications if check.fix.value.is_empty() => {
            text::FIX_ALLOW_NOTIFICATIONS.get()
        }
        FixAction::Applications => text::FIX_REVEAL.get(),
        FixAction::Notifications | FixAction::Settings(_) => text::FIX_OPEN_SETTINGS.get(),
    })
}

/// 점검 화면과 배너의 공통 진입점. 알 수 없는 ID와 doctor의 임의 명령은 실행하지 않는다.
pub fn run_fix(id: &str, env: &Env, cx: &mut App) -> Task<FixResult> {
    run_with(id, env, SystemExecutor, cx)
}

enum Prepared<R> {
    Done,
    Guidance,
    Review {
        agent: TargetAgent,
        diff: String,
        review: R,
    },
}

trait Executor: Send + Sync + 'static {
    type Review: Send + 'static;
    fn prepare(&self, action: FixAction, env: &Env) -> Result<Prepared<Self::Review>, FixError>;
    fn apply(&self, review: Self::Review) -> FixOutcome;
}

fn run_with<E: Executor>(id: &str, env: &Env, executor: E, cx: &mut App) -> Task<FixResult> {
    let Some(action) = action_for(id) else {
        return Task::ready(Err(atext::UNSUPPORTED.get().to_owned().into()));
    };
    let env = env.clone();
    let executor = Arc::new(executor);
    let prepare = executor.clone();
    let background = cx.background_executor().clone();
    let window = cx.active_window();
    cx.spawn(async move |cx| {
        let prepared = background
            .spawn(async move { prepare.prepare(action, &env) })
            .await?;
        if matches!(&prepared, Prepared::Guidance) {
            let window = window.ok_or_else(|| atext::NO_WINDOW.get().to_owned())?;
            let answer = window
                .update(cx, |_, window, cx| {
                    window.prompt(
                        PromptLevel::Info,
                        atext::MOVE_TITLE.get(),
                        Some(atext::MOVE_DETAIL.get()),
                        &[text::FIX_REVEAL.get(), atext::CANCEL.get()],
                        cx,
                    )
                })
                .map_err(|_| atext::NO_WINDOW.get().to_owned())?;
            if answer.await != Ok(0) {
                return Err(FixError::Cancelled);
            }
            return background
                .spawn(async move {
                    if perm::open_url(perm::APPLICATIONS_URL) {
                        Ok(())
                    } else {
                        Err(atext::OPEN_FAILED.get().to_owned().into())
                    }
                })
                .await;
        }
        let Prepared::Review {
            agent,
            diff,
            review,
        } = prepared
        else {
            return Ok(());
        };
        let window = window.ok_or_else(|| atext::NO_WINDOW.get().to_owned())?;
        let name = super::agent_name(agent.id());
        let clipped = super::clip(&diff, agent.id());
        let title = text::HOOK_TITLE.format(&[("name", name)]);
        let detail = text::HOOK_DETAIL.format(&[("id", agent.id()), ("diff", &clipped)]);
        let answer = window
            .update(cx, |_, window, cx| {
                window.prompt(
                    PromptLevel::Warning,
                    &title,
                    Some(&detail),
                    &[text::INSTALL.get(), atext::CANCEL.get()],
                    cx,
                )
            })
            .map_err(|_| atext::NO_WINDOW.get().to_owned())?;
        if answer.await != Ok(0) {
            return Err(FixError::Cancelled);
        }
        background
            .spawn(async move { executor.apply(review) })
            .await
            .map_err(FixError::from)
    })
}

struct SystemExecutor;

impl Executor for SystemExecutor {
    type Review = Plan;

    fn prepare(&self, action: FixAction, env: &Env) -> Result<Prepared<Plan>, FixError> {
        match action {
            FixAction::Helpers => {
                stage_helpers(env)?;
            }
            FixAction::Cli => install_managed_cli(env)?,
            FixAction::Hook(agent) => {
                if let Some((diff, plan)) = prepare_hook(agent, env)? {
                    return Ok(Prepared::Review {
                        agent,
                        diff,
                        review: plan,
                    });
                }
            }
            FixAction::Notifications => request_notifications()?,
            FixAction::Settings(url) => open_settings(url)?,
            FixAction::Applications => return Ok(Prepared::Guidance),
        }
        Ok(Prepared::Done)
    }

    fn apply(&self, review: Plan) -> FixOutcome {
        apply_hook(review)
    }
}

pub(super) fn prepare_hook(
    agent: TargetAgent,
    env: &Env,
) -> Result<Option<(String, Plan)>, FixError> {
    // Codex 훅은 `codex app-server`를 띄워 신뢰 해시를 받는다. Finder로 뜬 앱의 PATH에는
    // Homebrew·npm 경로가 없어 ENOENT가 나므로 로그인 셸 PATH로 찾는다.
    let search = (agent == TargetAgent::Other(Agent::Codex))
        .then(|| super::login_path(&env.shell))
        .flatten();
    prepare_hook_with_path(agent, env, search.as_deref())
}

fn prepare_hook_with_path(
    agent: TargetAgent,
    env: &Env,
    search: Option<&OsStr>,
) -> Result<Option<(String, Plan)>, FixError> {
    let executable = stage_helpers(env)?;
    let state = canonical_path(&env.state)?;
    let path = hook_config_path(agent, env)?;
    if let Some(plan) =
        Plan::prepare_with_path(Action::Install, agent, &path, &state, &executable, search)
            .map_err(|e| hook_prepare_error(agent, &e))?
    {
        let mut diff = Vec::new();
        plan.write_diff(&mut diff).map_err(|e| e.to_string())?;
        return Ok(Some((String::from_utf8_lossy(&diff).into_owned(), plan)));
    }
    Ok(None)
}

pub(super) fn apply_hook(review: Plan) -> FixOutcome {
    review
        .apply()
        .map(|_| ())
        .map_err(|e| atext::HOOK_ERROR.format(&[("error", &e.to_string())]))
}

/// 설치 계획 실패를 행 조치 실패로. 기존 설정과의 충돌(`AlreadyExists`)은 아무것도 바꾸지 않았으니
/// 오류가 아니라 수동 병합 안내다(OWE-714 D2).
fn hook_prepare_error(agent: TargetAgent, error: &io::Error) -> FixError {
    if error.kind() == io::ErrorKind::AlreadyExists {
        let name = super::agent_name(agent.id());
        FixError::Conflict {
            guide: atext::HOOK_CONFLICT.format(&[("name", name)]),
            detail: error.to_string(),
        }
    } else {
        FixError::Failed(atext::HOOK_ERROR.format(&[("error", &error.to_string())]))
    }
}

fn stage_helpers(env: &Env) -> Result<PathBuf, String> {
    use super::{Access, access};
    let parent = env
        .krill
        .parent()
        .ok_or_else(|| atext::HELPERS_MISSING.get().to_owned())?;
    // 번들 설치본은 krill만 Contents/MacOS에 있고 holder·krilld는 Contents/Helpers에 있다(OWE-714).
    for name in ["krill", "krill-holder", "krilld"] {
        let path = krill_proto::daemon::exe_near(parent, name);
        if !path.is_file() || access(&path, Access::EXEC_OK).is_err() {
            return Err(atext::HELPERS_MISSING.format(&[("path", &path.display().to_string())]));
        }
    }
    safe_managed_dir(
        &env.home,
        &krill_proto::user_dirs::share_dir(&env.home).join("bin"),
    )?;
    super::stage_binaries(&env.krill, &env.home)
}

pub(super) fn install_managed_cli(env: &Env) -> FixOutcome {
    let path = super::login_path(&env.shell)
        .unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
    install_managed_cli_at(env, &path)
}

fn install_managed_cli_at(env: &Env, path: &std::ffi::OsStr) -> FixOutcome {
    // The Windows installer already exposes the native CLI. Do not try to
    // replace it with a Unix-only managed symlink during automatic preparation.
    #[cfg(windows)]
    if matches!(
        super::cli(&env.krill, path, &env.home),
        super::Cli::Installed
    ) {
        return Ok(());
    }
    let managed = [
        env.home.join(".local/bin"),
        env.home.join("bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    let dirs: Vec<_> = std::env::split_paths(path).collect();
    let directory = dirs
        .iter()
        .find(|dir| managed.contains(dir) && safe_cli_dir(&env.home, dir).is_ok())
        .unwrap_or(&managed[0]);
    safe_cli_dir(&env.home, directory)?;
    let target = super::stable_mk_path(&env.krill, &env.home);
    for dir in &dirs {
        let candidate = dir.join("krill");
        if std::fs::symlink_metadata(&candidate).is_ok() {
            let ours = std::fs::canonicalize(&candidate).is_ok_and(|found| {
                std::fs::canonicalize(&target).ok().as_ref() == Some(&found)
                    || std::fs::canonicalize(&env.krill).ok().as_ref() == Some(&found)
            });
            let managed_broken = candidate == directory.join("krill")
                && std::fs::read_link(&candidate).is_ok_and(|raw| raw == target);
            if !ours && !managed_broken {
                return Err(atext::CLI_CONFLICT.get().to_owned());
            }
            break;
        }
    }
    let link = directory.join("krill");
    match std::fs::symlink_metadata(&link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let raw = std::fs::read_link(&link).map_err(|e| e.to_string())?;
            let pointed = if raw.is_absolute() {
                raw
            } else {
                directory.join(raw)
            };
            if pointed != target {
                return Err(atext::CLI_CONFLICT.get().to_owned());
            }
        }
        Ok(_) => return Err(atext::CLI_CONFLICT.get().to_owned()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let staged = stage_helpers(env)?;
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    if std::fs::symlink_metadata(&link).is_err() {
        // symlink는 기존 파일을 덮어쓰지 않는다. 동시 생성된 파일의 소유권도 추측하지 않는다.
        #[cfg(unix)]
        std::os::unix::fs::symlink(staged, &link).map_err(|e| e.to_string())?;
        #[cfg(not(unix))]
        return Err(format!(
            "symlink {}: {}",
            staged.display(),
            io::ErrorKind::Unsupported
        ));
    }
    if dirs.contains(directory) {
        Ok(())
    } else {
        Err(atext::CLI_PATH.get().to_owned())
    }
}

fn safe_cli_dir(home: &Path, directory: &Path) -> FixOutcome {
    use super::{Access, access};
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt as _;
    if directory.starts_with(home) {
        return safe_managed_dir(home, directory);
    }
    if ![Path::new("/opt/homebrew/bin"), Path::new("/usr/local/bin")].contains(&directory) {
        return Err(atext::INVALID_PATH.get().to_owned());
    }
    let meta = std::fs::symlink_metadata(directory).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    let owned = meta.uid() == rustix::process::geteuid().as_raw();
    #[cfg(not(unix))]
    let owned = true;
    if !meta.is_dir()
        || !owned
        || access(directory, Access::WRITE_OK | Access::EXEC_OK).is_err()
        || directory.ancestors().any(|path| {
            std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
        })
    {
        return Err(atext::CLI_CONFLICT.get().to_owned());
    }
    Ok(())
}

fn safe_managed_dir(home: &Path, directory: &Path) -> FixOutcome {
    let relative = directory
        .strip_prefix(home)
        .map_err(|_| atext::INVALID_PATH.get().to_owned())?;
    let mut current = home.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(atext::INVALID_PATH.get().to_owned());
        }
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                return Err(atext::CLI_CONFLICT.get().to_owned());
            }
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.to_string()),
            _ => {}
        }
    }
    Ok(())
}

fn canonical_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(atext::INVALID_PATH.get().to_owned());
    }
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(atext::INVALID_PATH.get().to_owned());
    }
    let mut ancestor = path;
    let mut tail = Vec::new();
    while !ancestor.exists() {
        tail.push(
            ancestor
                .file_name()
                .ok_or_else(|| atext::INVALID_PATH.get().to_owned())?,
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| atext::INVALID_PATH.get().to_owned())?;
    }
    let mut canonical = std::fs::canonicalize(ancestor).map_err(|e| e.to_string())?;
    for part in tail.into_iter().rev() {
        canonical.push(part);
    }
    Ok(canonical)
}

fn hook_config_path(agent: TargetAgent, env: &Env) -> Result<PathBuf, String> {
    // 테스트는 가짜 홈만 쓴다. 개발자 셸의 CLAUDE_CONFIG_DIR·CODEX_HOME을 따르면 실제 설정에 훅을 쓴다.
    let directory = |key, default| {
        if cfg!(test) {
            return default;
        }
        std::env::var_os(key).map_or(default, PathBuf::from)
    };
    let path = match agent {
        TargetAgent::Claude => {
            directory("CLAUDE_CONFIG_DIR", env.home.join(".claude")).join("settings.json")
        }
        TargetAgent::Other(Agent::Codex) => {
            directory("CODEX_HOME", env.home.join(".codex")).join("config.toml")
        }
        TargetAgent::Other(Agent::Cursor) => env.home.join(".cursor/hooks.json"),
        TargetAgent::Other(Agent::Agy) => env.home.join(".gemini/config/hooks.json"),
        TargetAgent::Other(Agent::Pi) => krill_proto::agent::hooks::pi_extension_path(&directory(
            "PI_CODING_AGENT_DIR",
            env.home.join(".pi/agent"),
        )),
        TargetAgent::Other(Agent::OpenCode) => krill_proto::user_dirs::opencode_plugin(&directory(
            "OPENCODE_CONFIG_DIR",
            directory("XDG_CONFIG_HOME", env.home.join(".config")).join("opencode"),
        )),
    };
    if !path.is_absolute() {
        return Err(atext::INVALID_PATH.get().to_owned());
    }
    canonical_path(&path)
}

fn open_settings(url: &str) -> FixOutcome {
    if perm::open_first_available(&[url, perm::SETTINGS_ROOT_URL]) {
        Ok(())
    } else {
        Err(atext::OPEN_FAILED.get().to_owned())
    }
}

fn request_notifications() -> FixOutcome {
    match perm::query_notification_status(Duration::from_secs(2)) {
        Some(perm::NotificationStatus::Authorized | perm::NotificationStatus::Ephemeral) => Ok(()),
        Some(perm::NotificationStatus::Denied | perm::NotificationStatus::Provisional) => {
            open_settings(&perm::notifications_settings_url(
                perm::bundle_id().as_deref(),
            ))
        }
        Some(perm::NotificationStatus::NotDetermined) => {
            match perm::request_notification_authorization(Duration::from_secs(60)) {
                perm::AuthRequestOutcome::Granted => Ok(()),
                perm::AuthRequestOutcome::Failed(error) => {
                    Err(atext::NOTIFICATION_ERROR.format(&[("error", &error)]))
                }
                perm::AuthRequestOutcome::Denied => {
                    Err(atext::NOTIFICATION_DENIED.get().to_owned())
                }
                perm::AuthRequestOutcome::Unavailable => {
                    Err(atext::NOTIFICATION_UNAVAILABLE.get().to_owned())
                }
                perm::AuthRequestOutcome::Timeout => {
                    Err(atext::NOTIFICATION_TIMEOUT.get().to_owned())
                }
            }
        }
        None => Err(atext::NOTIFICATION_UNAVAILABLE.get().to_owned()),
    }
}

/// UI는 이 Global을 `observe_global`로 구독한다. 실제 필요한 조치만 담는다.
#[derive(Debug, Clone, Default)]
pub struct FixNeeds {
    pub items: Vec<FixNeed>,
}
impl Global for FixNeeds {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixNeed {
    pub check_id: String,
    pub message: String,
}

fn publish(id: &str, message: String, cx: &mut App) {
    if cx.try_global::<FixNeeds>().is_none() {
        cx.set_global(FixNeeds::default());
    }
    let needs = cx.global_mut::<FixNeeds>();
    if let Some(need) = needs.items.iter_mut().find(|n| n.check_id == id) {
        need.message = message;
    } else {
        needs.items.push(FixNeed {
            check_id: id.to_owned(),
            message,
        });
    }
    if id == perm::NOTIFICATIONS_ID {
        notifications::watch_permission(cx);
    }
}

pub fn dismiss_need(id: &str, cx: &mut App) {
    if cx.try_global::<FixNeeds>().is_some() {
        cx.global_mut::<FixNeeds>()
            .items
            .retain(|n| n.check_id != id);
    }
}

/// 파일 접근이 실제로 거부됐을 때만 호출한다. 권한을 미리 요청하지 않는다.
pub fn note_access_denied(path: &Path, error: &io::Error, cx: &mut App) {
    if error.kind() != io::ErrorKind::PermissionDenied && error.raw_os_error() != Some(1) {
        return;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let folders = home.is_some_and(|home| {
        perm::FolderKind::all()
            .iter()
            .any(|kind| path.starts_with(home.join(kind.subdir())))
    });
    let id = if folders {
        perm::FOLDERS_ID
    } else {
        perm::FULL_DISK_ID
    };
    let attempts = cx.default_global::<AccessAttempts>();
    if !attempts.paths.iter().any(|(_, existing)| existing == path) {
        attempts.paths.push((id.to_owned(), path.to_path_buf()));
    }
    publish(id, atext::ACCESS_DENIED.get().to_owned(), cx);
}

/// hook 미설치 배너의 점검 ID·문구. 표시 이름 규칙은 `agent_name` 단일 출처다(OWE-877).
pub(super) fn hook_needed_content(agent: TargetAgent) -> (String, String) {
    (
        format!("hook.{}", agent.id()),
        atext::HOOK_NEEDED.format(&[("agent", super::agent_name(agent.id()))]),
    )
}

/// 상태 감지가 켜져 있으면 누락된 훅을 설치하고 실패한 경우에만 안내한다.
pub fn note_agent_session(agent: &str, cx: &mut App) {
    let Some(agent) = TargetAgent::parse(agent) else {
        return;
    };
    let Some(env) = Env::current() else {
        return;
    };
    if env.is_isolated_state() {
        return;
    }
    hook_notice::note_agent_session_at(agent, env, cx);
}

pub(crate) use hook_notice::ensure_current;

fn hook_installed(state: &Path, agent: &str) -> bool {
    std::fs::read(state.join("hooks").join(format!("{agent}.json")))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<krill_proto::hook_install::Receipt>(&bytes).ok())
        .is_some_and(|receipt| receipt.complete && receipt.agent == agent)
}

#[derive(Default)]
struct AccessAttempts {
    paths: Vec<(String, PathBuf)>,
    agents: std::collections::HashSet<String>,
    rechecking: bool,
}
impl Global for AccessAttempts {}

/// 설정·Finder에서 앱으로 돌아오면 이미 거부됐던 접근과 게시된 안내만 다시 검사한다.
pub fn recheck_needed(cx: &mut App) -> Task<()> {
    notifications::invalidate(cx);
    let needs = cx
        .try_global::<FixNeeds>()
        .map(|needs| needs.items.clone())
        .unwrap_or_default();
    if needs.is_empty() {
        return Task::ready(());
    }
    let attempts = cx.default_global::<AccessAttempts>();
    if attempts.rechecking {
        return Task::ready(());
    }
    attempts.rechecking = true;
    let paths = attempts.paths.clone();
    let state = krill_proto::daemon::state_dir();
    let work = cx.background_executor().spawn(async move {
        let mut resolved = Vec::new();
        for need in needs {
            let ready = if let Some(agent) = need.check_id.strip_prefix("hook.") {
                state
                    .as_deref()
                    .is_some_and(|state| hook_installed(state, agent))
            } else if need.check_id == perm::NOTIFICATIONS_ID {
                matches!(
                    perm::query_notification_status(Duration::from_secs(2)),
                    Some(
                        perm::NotificationStatus::Authorized | perm::NotificationStatus::Ephemeral
                    )
                )
            } else {
                let denied: Vec<_> = paths
                    .iter()
                    .filter(|(id, _)| id == &need.check_id)
                    .collect();
                !denied.is_empty()
                    && denied.iter().all(|(_, path)| {
                        if path.is_dir() {
                            std::fs::read_dir(path).is_ok()
                        } else {
                            std::fs::File::open(path).is_ok()
                        }
                    })
            };
            if ready {
                resolved.push(need.check_id);
            }
        }
        resolved
    });
    cx.spawn(async move |cx| {
        let resolved = work.await;
        cx.update(|cx| {
            cx.default_global::<AccessAttempts>().rechecking = false;
            for id in resolved {
                dismiss_need(&id, cx);
            }
        });
    })
}

pub(super) fn awaits_settings_return(id: &str) -> bool {
    matches!(
        action_for(id),
        Some(FixAction::Settings(_) | FixAction::Notifications | FixAction::Applications)
    )
}

/// 조치 실패 원인을 기존 배너에 반영한다.
pub(super) fn report_failure(id: &str, error: String, cx: &mut App) {
    publish(id, error, cx);
}

/// 시작 준비. 상태 감지가 켜져 있으면 훅도 설치하며 OS 권한은 변경하지 않는다.
pub fn auto_prepare(env: Env, cx: &mut App) -> Task<FixOutcome> {
    let background = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        let result = background
            .spawn(async move {
                drop(crate::settings::install_all_skills_at(&env.home));
                stage_helpers(&env).map_err(|e| ("bin.helpers", e))?;
                let path = super::login_path(&env.shell)
                    .unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
                if crate::settings::agent_status_hooks_enabled_at(&env.state) {
                    install_default_hooks(&env, &path);
                }
                install_managed_cli_at(&env, &path).map_err(|e| ("cli.path", e))?;
                Ok(())
            })
            .await;
        match result {
            Ok(()) => Ok(()),
            Err((id, error)) => {
                let message = atext::PREPARE_FAILED.format(&[("error", &error)]);
                cx.update(|cx| publish(id, message, cx));
                Err(error)
            }
        }
    })
}

/// 상태 감지 훅을 두는 에이전트와 로그인 셸 PATH에서 찾을 실행 파일 이름.
const HOOK_AGENTS: [(TargetAgent, &str); 5] = [
    (TargetAgent::Claude, "claude"),
    (TargetAgent::Other(Agent::Codex), "codex"),
    (TargetAgent::Other(Agent::OpenCode), "opencode"),
    (TargetAgent::Other(Agent::Cursor), "cursor-agent"),
    (TargetAgent::Other(Agent::Agy), "agy"),
];

/// 설정의 상태 감지 훅 스위치를 바꾼 직후 백그라운드에서 반영한다. 켜면 설치된 에이전트에 훅을
/// 설치하고, 끄면 Krill 설치 영수증이 있는 훅만 되돌린다(사용자가 고친 항목은 남는다).
pub fn apply_hook_preference(env: Env, enabled: bool, cx: &mut App) -> Task<()> {
    if env.is_isolated_state() {
        return Task::ready(());
    }
    cx.background_executor().spawn(async move {
        let path = super::login_path(&env.shell)
            .unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
        if enabled {
            install_default_hooks(&env, &path);
        } else {
            uninstall_default_hooks(&env, &path);
        }
    })
}

/// Krill이 설치한(영수증이 있는) 상태 감지 훅을 되돌린다. 실패는 로그만 남긴다.
fn uninstall_default_hooks(env: &Env, path: &OsStr) {
    let _guard = hook_notice::INSTALL_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = match canonical_path(&env.state) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(%error, "hook uninstall skipped: invalid state directory");
            return;
        }
    };
    for (agent, _) in HOOK_AGENTS {
        let result = hook_config_path(agent, env)
            .map_err(io::Error::other)
            .and_then(|config| {
                Plan::prepare_with_path(
                    Action::Uninstall,
                    agent,
                    &config,
                    &state,
                    &env.krill,
                    Some(path),
                )
            })
            .and_then(|plan| plan.map_or(Ok(()), |plan| plan.apply().map(drop)));
        if let Err(error) = result {
            tracing::warn!(agent = agent.id(), %error, "hook uninstall skipped");
        }
    }
}

/// 상태 감지 훅은 기본으로 켠다(Orca와 같음). `path`(로그인 셸 PATH)에서 실행 파일이 보이는
/// 에이전트만 설치하고, 이미 설치 영수증이 있으면 건너뛴다. 실패는 시작을 막지 않고 로그만 남긴다
/// — 점검 화면의 훅 행이 같은 실패를 조치와 함께 보여 준다.
fn install_default_hooks(env: &Env, path: &OsStr) {
    for (agent, program) in HOOK_AGENTS {
        if krill_proto::agent::which(program, path).is_none() {
            continue;
        }
        let result = hook_notice::install_enabled(agent, env, Some(path));
        if let Err(error) = result {
            tracing::warn!(agent = agent.id(), ?error, "default hook install skipped");
        }
    }
}

mod hook_notice;
mod notifications;
pub use notifications::{dismiss_notification, show_notification};

#[cfg(test)]
#[cfg(unix)]
mod tests;
