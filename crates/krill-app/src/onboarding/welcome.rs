//! 점검 모달(MK-161, OWE-714, OWE-903, OWE-904). 첫 실행에는 뜨지 않고 설정 > 일반 >
//! 설정 점검·메뉴·상태바에서 연다.
//!
//! - staged `krill doctor --json --no-start` + `app.*`를 등급별 카드로 보여 준다. 행 오른쪽에
//!   조치 버튼과 펼침 chevron, 고정 헤더(제목·요약·모두 자동 설정·모두 다시 검사)와 고정
//!   푸터(닫기). 창 재포커스 재검사(1초 디바운스).
//! - Enter·Esc = 닫기. 별도 OS 창 대신 메인 창 안 `ui::modal_backdrop` 중앙 모달
//!   (`SETTINGS_MODAL_WIDTH`)로 뜨며, 닫기는 [`Event::Close`]로 호스트(`Shell`)에 알린다(OWE-903).
//!
//! 호스트가 없는 테스트에서는 [`Event::Close`]를 구독한 테스트 호스트가 창을 닫는다.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    ClipboardItem, Context, EventEmitter, FocusHandle, KeyDownEvent, Render, Subscription, Task,
    Window, actions, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use krill_proto::doctor::{Check, FixKind, Level, Status};
use krill_proto::hook_install::{Plan, TargetAgent};

use super::{
    Cli, Collected, Env, Survey, check_text, check_title, clip, collect, install_cli_link,
    is_replaceable_broken_symlink, mk_hook,
};
use crate::i18n::catalog::checks as ctext;
use crate::i18n::catalog::onboarding as text;
use crate::platform::permissions as perm;
use crate::ui;

/// 재포커스 재검사의 최소 간격.
const RECHECK_DEBOUNCE: Duration = Duration::from_secs(1);
/// 폴더 프로브 한 폴더당 상한.
const FOLDER_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// 알림 허용 대화상자 대기 상한(사람이 클릭한다).
const AUTH_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// 알림 재조회 상한.
const NOTIFICATION_QUERY_TIMEOUT: Duration = Duration::from_secs(2);

actions!(welcome, [CloseWizard]);

/// 마법사 모달의 키 컨텍스트. 메인 창 안 모달에서도 macOS 표준 ⌘W 닫기를 받는다(OWE-878).
/// Esc는 기존 `on_key_down` 처리가 맡는다.
pub(super) const KEY_CONTEXT: &str = "Wizard";

/// 마법사 모달이 외부(호스트 `Shell`)로 알리는 이벤트. 모달은 OS 창을 직접 닫지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 모달을 닫는다([닫기]·Enter·Esc·⌘W·바깥 클릭).
    Close,
}

/// 점검 행의 hook 설치 진행 상태. 없으면 대기다.
#[derive(Debug, Clone)]
pub(super) enum HookPhase {
    Working,
    /// dry-run diff를 보여 주고 확정을 기다린다.
    Review(String),
    Failed(String),
}

/// 점검 행 조치.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fix {
    /// 조치 실행기([`super::actions::run_fix`])가 맡는 행. 끝나면 다시 검사한다.
    Runner,
    Copy,
    OpenSettings,
    Reveal,
    AllowNotifications,
    ProbeFolders,
    /// 앱 안 hook 설치(dry-run diff 확인 → 설치).
    Hook(TargetAgent),
    /// 앱 안 CLI 링크 설치.
    Link,
}

pub struct Wizard {
    pub(super) env: Env,
    pub(super) survey: Option<Survey>,
    pub(super) staged: Option<PathBuf>,
    /// `doctor` + `app.*` 합본. `None`이면 검사 중.
    pub(super) checks: Option<Vec<Check>>,
    pub(super) checks_loading: bool,
    /// 사용자가 펼침을 뒤집은 행. 실패 행은 기본으로 펼쳐져 있다.
    pub(super) toggled: HashSet<String>,
    pub(super) copied: Option<String>,
    /// 진행 중인 버튼(`notify-allow`, `folders-probe`, `cli`).
    pub(super) busy: HashSet<String>,
    /// 에이전트 ID별 hook 설치 진행.
    pub(super) hooks: HashMap<&'static str, HookPhase>,
    /// 에이전트 ID별 검토 대기 중인 hook 설치 계획.
    hook_plans: HashMap<&'static str, Plan>,
    /// 점검 ID별 마지막 실행기 조치 실패(충돌 안내 포함).
    pub(super) fix_errors: HashMap<String, super::actions::FixError>,
    folder_check_requested: bool,
    pub(super) last_recheck: Instant,
    focus: FocusHandle,
    tasks: HashMap<&'static str, Task<()>>,
    checks_revision: u64,
    recheck_again: bool,
    fix_tasks: HashMap<String, Task<()>>,
    activation: Option<Subscription>,
}

impl std::fmt::Debug for Wizard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wizard")
            .field("checks", &self.checks.as_ref().map(Vec::len))
            .finish_non_exhaustive()
    }
}

impl EventEmitter<Event> for Wizard {}

/// 필수 실패 행.
fn required_blockers(checks: &[Check]) -> Vec<&Check> {
    checks
        .iter()
        .filter(|c| c.level == Level::Required && c.status == Status::Fail)
        .collect()
}

fn merge_checks(doctor: Vec<Check>, app: Vec<Check>) -> Vec<Check> {
    doctor.into_iter().chain(app).collect()
}

impl Wizard {
    pub fn new(env: Env, cx: &mut Context<Self>) -> Self {
        Self {
            env,
            survey: None,
            staged: None,
            checks: None,
            checks_loading: false,
            toggled: HashSet::new(),
            copied: None,
            busy: HashSet::new(),
            hooks: HashMap::new(),
            hook_plans: HashMap::new(),
            fix_errors: HashMap::new(),
            folder_check_requested: false,
            last_recheck: Instant::now(),
            focus: cx.focus_handle(),
            tasks: HashMap::new(),
            checks_revision: 0,
            recheck_again: false,
            fix_tasks: HashMap::new(),
            activation: None,
        }
    }

    /// 창 활성화 관찰(재포커스 재검사)을 연결한다. 호스트가 모달 생성 직후 부른다.
    pub fn observe_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sub = cx.observe_window_activation(window, |w, window, cx| {
            if window.is_window_active() {
                w.activated(cx);
            }
        });
        self.activation = Some(sub);
    }

    /// 모달의 포커스 핸들. 호스트가 모달을 열 때 포커스를 옮긴다.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// 모달이 처음 열릴 때 수집 시작.
    pub fn begin_collect(&mut self, cx: &mut Context<Self>) {
        if self.checks_loading {
            return;
        }
        self.checks_loading = true;
        cx.notify();
        let revision = self.checks_revision;
        let env = self.env.clone();
        self.tasks.insert(
            "checks",
            cx.spawn(async move |this, cx| {
                let collected: Collected = cx
                    .background_executor()
                    .spawn(async move { collect(&env) })
                    .await;
                this.update(cx, |w, cx| {
                    if w.checks_revision != revision {
                        w.checks_loading = false;
                        w.recheck(cx);
                        return;
                    }
                    w.apply_collected(collected);
                    if let Some(checks) = &w.checks {
                        super::store_snapshot(cx, checks.clone());
                    }
                    cx.notify();
                    if std::mem::take(&mut w.recheck_again) {
                        w.recheck(cx);
                    }
                })
                .ok();
            }),
        );
    }

    fn apply_collected(&mut self, collected: Collected) {
        self.checks_loading = false;
        self.staged = collected.staged;
        self.survey = Some(collected.survey);
        self.checks = Some(merge_checks(collected.doctor, collected.app));
        self.last_recheck = Instant::now();
    }

    /// 조용한 재검사(`doctor` + `app.*`). 조치·설정 변경 뒤 반영용.
    fn recheck(&mut self, cx: &mut Context<Self>) {
        if self.checks_loading {
            self.recheck_again = true;
            return;
        }
        self.checks_loading = true;
        self.last_recheck = Instant::now();
        cx.notify();
        let revision = self.checks_revision;
        let env = self.env.clone();
        let folders = self.folder_check_requested;
        self.tasks.insert(
            "checks",
            cx.spawn(async move |this, cx| {
                let collected = cx
                    .background_executor()
                    .spawn(async move {
                        let mut collected = collect(&env);
                        if folders {
                            let check = perm::decide_folder_probe(&perm::probe_folders(
                                FOLDER_PROBE_TIMEOUT,
                            ));
                            if let Some(slot) = collected
                                .app
                                .iter_mut()
                                .find(|check| check.id == perm::FOLDERS_ID)
                            {
                                *slot = check;
                            }
                        }
                        collected
                    })
                    .await;
                this.update(cx, |w, cx| {
                    if w.checks_revision != revision {
                        w.checks_loading = false;
                        w.recheck(cx);
                        return;
                    }
                    w.apply_collected(collected);
                    if let Some(checks) = &w.checks {
                        super::store_snapshot(cx, checks.clone());
                    }
                    cx.notify();
                    if std::mem::take(&mut w.recheck_again) {
                        w.recheck(cx);
                    }
                })
                .ok();
            }),
        );
    }

    /// 모달 재포커스(시스템 설정에서 돌아옴) → 디바운스 후 재검사.
    /// 호스트가 창 활성화를 관찰해 부른다.
    pub fn activated(&mut self, cx: &mut Context<Self>) {
        super::actions::recheck_needed(cx).detach();
        if !self.checks_loading && self.last_recheck.elapsed() > RECHECK_DEBOUNCE {
            self.recheck(cx);
        }
    }

    /// 모달 닫기를 호스트에 알린다.
    fn close(cx: &mut Context<Self>) {
        cx.emit(Event::Close);
    }

    // ---- 점검 행 동작 ----

    fn copy_fix(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(value.to_owned()));
        crate::settings::queue_terminal_command(value.to_owned(), cx);
        self.copied = Some(id.to_owned());
        cx.notify();
    }

    /// 원클릭 전체 자동 설정: 모든 에이전트 훅·내장 스킬·CLI 링크·보조 바이너리를 한 번에 준비하고 다시 검사한다.
    pub(super) fn fix_all(&mut self, cx: &mut Context<Self>) {
        if !self.busy.insert("fix-all".to_owned()) {
            return;
        }
        self.fix_errors.clear();
        self.hooks.clear();
        self.hook_plans.clear();
        cx.notify();
        let home = self.env.home.clone();
        let state = self.env.state.clone();
        let env = self.env.clone();
        let link = cli_link_target(self.survey.as_ref());
        let prep = (!self.env.is_isolated_state())
            .then(|| super::actions::auto_prepare(self.env.clone(), cx));
        self.tasks.insert(
            "fix-all",
            cx.spawn(async move |this, cx| {
                if let Some(prep) = prep {
                    drop(prep.await);
                }
                let link_installed = cx
                    .background_executor()
                    .spawn(async move {
                        drop(crate::settings::setup_all_agents_and_skills_at(
                            &home, &state,
                        ));
                        if !env.is_isolated_state() {
                            drop(super::install_codex_hook_with_login_path(&env, &state));
                        }
                        if let Some(link_path) = link {
                            if env.is_isolated_state() {
                                return true;
                            }
                            return install_cli_link(&env.krill, &env.home, &link_path).is_ok();
                        }
                        false
                    })
                    .await;
                this.update(cx, |w, cx| {
                    w.busy.remove("fix-all");
                    if link_installed && let Some(survey) = &mut w.survey {
                        survey.cli = Cli::Installed;
                    }
                    cx.notify();
                })
                .ok();
                this.update(cx, Self::recheck).ok();
            }),
        );
    }

    pub(super) fn toggle_expand(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.toggled.remove(id) {
            self.toggled.insert(id.to_owned());
        }
        cx.notify();
    }

    /// 행이 펼쳐져 있나. 실패 행은 원인·다음 행동을 바로 보이게 기본으로 펼친다(OWE-714 (6)).
    pub(super) fn is_open(&self, check: &Check) -> bool {
        (check.status == Status::Fail) != self.toggled.contains(&check.id)
    }

    fn run_fix(&mut self, fix: Fix, check: &Check, cx: &mut Context<Self>) {
        let perm_open = |value: &str, cx: &mut Context<Self>| {
            let value = value.to_owned();
            cx.background_spawn(async move {
                crate::platform::permissions::open_first_available(&[
                    &value,
                    crate::platform::permissions::SETTINGS_ROOT_URL,
                ]);
            })
            .detach();
        };
        match fix {
            Fix::Runner => self.run_runner(&check.id, cx),
            Fix::Copy => self.copy_fix(&check.id, &check.fix.value, cx),
            Fix::OpenSettings => perm_open(&check.fix.value, cx),
            Fix::Reveal => {
                cx.background_spawn(async {
                    crate::platform::permissions::open_first_available(&[
                        crate::platform::permissions::APPLICATIONS_URL,
                    ]);
                })
                .detach();
            }
            Fix::AllowNotifications => self.allow_notifications(cx),
            Fix::ProbeFolders => self.probe_folders(cx),
            Fix::Hook(agent) => self.hook_install(agent, cx),
            Fix::Link => self.cli_install(cx),
        }
    }

    /// 실행기 조치. 실패 문구는 행 아래에 남기고, 끝나면(성공·실패 모두) 다시 검사한다.
    fn run_runner(&mut self, id: &str, cx: &mut Context<Self>) {
        let key = format!("fix:{id}");
        if !self.busy.insert(key.clone()) {
            return;
        }
        self.fix_errors.remove(id);
        if id == perm::FOLDERS_ID {
            self.folder_check_requested = true;
        }
        cx.notify();
        let run = super::actions::run_fix(id, &self.env, cx);
        let id = id.to_owned();
        let task_key = key.clone();
        self.fix_tasks.insert(
            task_key,
            cx.spawn(async move |this, cx| {
                let outcome = run.await;
                this.update(cx, |w, cx| {
                    w.busy.remove(&key);
                    match outcome {
                        Ok(()) | Err(super::actions::FixError::Cancelled) => {}
                        Err(e) => {
                            w.fix_errors.insert(id, e);
                        }
                    }
                    cx.notify();
                })
                .ok();
                this.update(cx, Self::recheck).ok();
            }),
        );
    }

    /// `[알림 허용]`. 요청 뒤 상태를 다시 읽어 행에 반영한다.
    fn allow_notifications(&mut self, cx: &mut Context<Self>) {
        use crate::platform::permissions as perm;
        if !self.busy.insert("notify-allow".to_owned()) {
            return;
        }
        cx.notify();
        self.tasks.insert(
            "notify-allow",
            cx.spawn(async move |this, cx| {
                let status = cx
                    .background_executor()
                    .spawn(async move {
                        perm::request_notification_authorization(AUTH_REQUEST_TIMEOUT);
                        (
                            perm::query_notification_status(NOTIFICATION_QUERY_TIMEOUT),
                            perm::bundle_id(),
                        )
                    })
                    .await;
                this.update(cx, |w, cx| {
                    w.busy.remove("notify-allow");
                    let check = perm::decide_notifications(status.0, status.1.as_deref());
                    w.replace_app_check(check);
                    cx.notify();
                })
                .ok();
            }),
        );
    }

    /// `[검사]`. 폴더 프로브 묶음을 돌려 행에 반영한다.
    fn probe_folders(&mut self, cx: &mut Context<Self>) {
        use crate::platform::permissions as perm;
        if !self.busy.insert("folders-probe".to_owned()) {
            return;
        }
        cx.notify();
        self.tasks.insert(
            "folders-probe",
            cx.spawn(async move |this, cx| {
                let results = cx
                    .background_executor()
                    .spawn(async move { perm::probe_folders(FOLDER_PROBE_TIMEOUT) })
                    .await;
                this.update(cx, |w, cx| {
                    w.busy.remove("folders-probe");
                    w.replace_app_check(perm::decide_folder_probe(&results));
                    cx.notify();
                })
                .ok();
            }),
        );
    }

    fn replace_app_check(&mut self, check: Check) {
        self.checks_revision = self.checks_revision.wrapping_add(1);
        if let Some(checks) = &mut self.checks
            && let Some(slot) = checks.iter_mut().find(|c| c.id == check.id)
        {
            *slot = check;
        }
    }

    fn hook_paths(&self) -> (PathBuf, std::ffi::OsString) {
        let staged = self
            .staged
            .clone()
            .unwrap_or_else(|| self.env.krill.clone());
        let path = self
            .survey
            .as_ref()
            .map(|s| s.path.clone())
            .unwrap_or_default();
        (staged, path)
    }

    /// `[설치]`(hook): dry-run diff를 받아 행 아래에 보여 준다. 쓰기는 [설치 확정]에서만.
    fn hook_install(&mut self, agent: TargetAgent, cx: &mut Context<Self>) {
        let check_id = format!("hook.{}", agent.id());
        self.fix_errors.remove(&check_id);
        self.hook_plans.remove(agent.id());
        self.hooks.insert(agent.id(), HookPhase::Working);
        cx.notify();
        let env = self.env.clone();
        let (staged, path) = self.hook_paths();
        let task_key = format!("hook:{}", agent.id());
        self.fix_tasks.insert(
            task_key,
            cx.spawn(async move |this, cx| {
                enum Outcome {
                    Review(Box<(String, Option<Plan>)>),
                    Done,
                    Error(super::actions::FixError),
                }
                let outcome = cx
                    .background_executor()
                    .spawn(async move {
                        match super::actions::prepare_hook(agent, &env) {
                            Ok(Some((diff, plan))) => Outcome::Review(Box::new((diff, Some(plan)))),
                            Ok(None) => Outcome::Done,
                            Err(e @ super::actions::FixError::Conflict { .. }) => Outcome::Error(e),
                            Err(e) => match agent {
                                TargetAgent::Other(a) => {
                                    match mk_hook(&staged, &path, a, "--dry-run") {
                                        Ok(diff) => Outcome::Review(Box::new((diff, None))),
                                        Err(_) => Outcome::Error(e),
                                    }
                                }
                                TargetAgent::Claude => Outcome::Error(e),
                            },
                        }
                    })
                    .await;
                let done = matches!(outcome, Outcome::Done);
                this.update(cx, |w, cx| {
                    match outcome {
                        Outcome::Review(boxed) => {
                            let (diff, plan) = *boxed;
                            if let Some(plan) = plan {
                                w.hook_plans.insert(agent.id(), plan);
                            }
                            w.hooks
                                .insert(agent.id(), HookPhase::Review(clip(&diff, agent.id())));
                        }
                        Outcome::Done | Outcome::Error(super::actions::FixError::Cancelled) => {
                            w.hooks.remove(agent.id());
                        }
                        Outcome::Error(e @ super::actions::FixError::Conflict { .. }) => {
                            w.hooks.remove(agent.id());
                            w.fix_errors.insert(check_id, e);
                        }
                        Outcome::Error(super::actions::FixError::Failed(msg)) => {
                            w.hooks.insert(agent.id(), HookPhase::Failed(msg));
                        }
                    }
                    cx.notify();
                })
                .ok();
                if done {
                    this.update(cx, Self::recheck).ok();
                }
            }),
        );
    }

    fn hook_confirm(&mut self, agent: TargetAgent, cx: &mut Context<Self>) {
        let plan = self.hook_plans.remove(agent.id());
        self.hooks.insert(agent.id(), HookPhase::Working);
        cx.notify();
        let (staged, path) = self.hook_paths();
        let task_key = format!("hook:{}", agent.id());
        self.fix_tasks.insert(
            task_key,
            cx.spawn(async move |this, cx| {
                let done = cx
                    .background_executor()
                    .spawn(async move {
                        match (plan, agent) {
                            (Some(plan), _) => super::actions::apply_hook(plan),
                            (None, TargetAgent::Other(a)) => mk_hook(&staged, &path, a, "--yes")
                                .map(|_| ())
                                .map_err(|e| text::HOOK_FAILED.format(&[("error", &e)])),
                            (None, TargetAgent::Claude) => {
                                Err(text::HOOK_FAILED.format(&[("error", "missing hook plan")]))
                            }
                        }
                    })
                    .await;
                this.update(cx, |w, cx| {
                    match done {
                        Ok(()) => {
                            w.hooks.remove(agent.id());
                            w.fix_errors.remove(&format!("hook.{}", agent.id()));
                        }
                        Err(e) => {
                            w.hooks.insert(agent.id(), HookPhase::Failed(e));
                        }
                    }
                    cx.notify();
                })
                .ok();
                // 영수증이 생겼으니 점검(hook.*)을 다시 읽는다. 위 업데이트와 합치면 빌림이 꼬인다.
                this.update(cx, Self::recheck).ok();
            }),
        );
    }

    fn hook_cancel(&mut self, agent: TargetAgent, cx: &mut Context<Self>) {
        self.hooks.remove(agent.id());
        self.hook_plans.remove(agent.id());
        self.fix_errors.remove(&format!("hook.{}", agent.id()));
        self.fix_tasks.remove(&format!("hook:{}", agent.id()));
        cx.notify();
    }

    /// `[설치]`(CLI 링크). 격리 상태면 실제 링크를 만들지 않는다.
    fn cli_install(&mut self, cx: &mut Context<Self>) {
        if cli_link_target(self.survey.as_ref()).is_none() {
            return;
        }
        if !self.busy.insert("cli".to_owned()) {
            return;
        }
        self.fix_errors.remove("cli.path");
        cx.notify();
        let env = self.env.clone();
        let isolated = self.env.is_isolated_state();
        self.fix_tasks.insert(
            "cli".to_owned(),
            cx.spawn(async move |this, cx| {
                let outcome = cx
                    .background_executor()
                    .spawn(async move {
                        if isolated {
                            return Ok(());
                        }
                        super::actions::install_managed_cli(&env)
                    })
                    .await;
                let ok = outcome.is_ok();
                this.update(cx, |w, cx| {
                    w.busy.remove("cli");
                    match outcome {
                        Ok(()) => {
                            if let Some(survey) = &mut w.survey {
                                survey.cli = Cli::Installed;
                            }
                        }
                        Err(e) => {
                            w.fix_errors
                                .insert("cli.path".to_owned(), super::actions::FixError::Failed(e));
                        }
                    }
                    cx.notify();
                })
                .ok();
                if ok {
                    this.update(cx, Self::recheck).ok();
                }
            }),
        );
    }

    /// 행 조치와 버튼 문구. 앱 안 조치(hook 인라인 diff 검토·CLI 링크)를 먼저 고르고,
    /// 나머지는 조치 실행기와 복사/설정 열기로 넘긴다.
    fn row_fix(&self, check: &Check) -> Option<(Fix, &'static str)> {
        use crate::platform::permissions as perm;
        if check.status == Status::Ok {
            return None;
        }
        if check.status != Status::Skip
            && let Some(agent) = check.id.strip_prefix("hook.").and_then(TargetAgent::parse)
        {
            return Some((Fix::Hook(agent), text::INSTALL.get()));
        }
        if check.id == "cli.path" && cli_link_target(self.survey.as_ref()).is_some() {
            return Some((Fix::Link, text::INSTALL.get()));
        }
        if let Some(label) = super::actions::fix_label(check) {
            return Some((Fix::Runner, label));
        }
        match (&check.fix.kind, check.id.as_str(), check.status) {
            (FixKind::Command | FixKind::Copy, _, _) if !check.fix.value.is_empty() => {
                Some((Fix::Copy, text::FIX_COPY.get()))
            }
            (FixKind::OpenUrl, _, _) if check.fix.value == perm::APPLICATIONS_URL => Some((
                Fix::Reveal,
                text::FIX_REVEAL
                    .mac_or(crate::i18n::catalog::other_os::REVEAL)
                    .get(),
            )),
            (FixKind::OpenUrl, _, _) => Some((Fix::OpenSettings, text::FIX_OPEN_SETTINGS.get())),
            (FixKind::Reinstall, _, _) => Some((
                Fix::Reveal,
                text::FIX_REVEAL
                    .mac_or(crate::i18n::catalog::other_os::REVEAL)
                    .get(),
            )),
            (FixKind::None, perm::NOTIFICATIONS_ID, Status::Warn) => {
                Some((Fix::AllowNotifications, text::FIX_ALLOW_NOTIFICATIONS.get()))
            }
            (FixKind::None, perm::FOLDERS_ID, Status::Skip) => {
                Some((Fix::ProbeFolders, text::FIX_CHECK.get()))
            }
            _ => None,
        }
    }
}

/// 연결할 링크. `Offer`·교체 가능 끊김만 설치 대상이다.
fn cli_link_target(survey: Option<&Survey>) -> Option<PathBuf> {
    match &survey?.cli {
        Cli::Offer(link) => Some(link.clone()),
        Cli::Conflict(link) if is_replaceable_broken_symlink(link) => Some(link.clone()),
        Cli::Conflict(_) | Cli::Installed | Cli::Manual => None,
    }
}

/// 상태 아이콘. 건너뜀은 오류처럼 보이지 않게 흐린 정보 아이콘 + "건너뜀" 표시다(OWE-714 (5)).
fn status_icon(status: Status) -> IconName {
    match status {
        Status::Ok => IconName::CircleCheck,
        Status::Warn => IconName::TriangleAlert,
        Status::Fail => IconName::CircleX,
        Status::Skip => IconName::Info,
    }
}

// ---- 그리기 ----

impl Wizard {
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let fix_all_busy = self.busy.contains("fix-all");
        div()
            .id("wizard-header")
            .debug_selector(|| "wizard-header".into())
            .flex_none()
            .px_6()
            .pt_4()
            .pb_3()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(text::CHECKS_TITLE.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(self.summary_line()),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div().debug_selector(|| "wizard-fix-all".into()).child(
                            ui::primary("wizard-fix-all", text::FIX_ALL.get())
                                .loading(fix_all_busy)
                                .disabled(
                                    fix_all_busy || self.checks_loading || self.checks.is_none(),
                                )
                                .on_click(cx.listener(|s, _, _, cx| s.fix_all(cx))),
                        ),
                    )
                    .child(
                        div().debug_selector(|| "wizard-recheck".into()).child(
                            ui::secondary("wizard-recheck", text::CHECKLIST_RECHECK.get())
                                .icon(IconName::RefreshCw)
                                .loading(self.checks_loading)
                                .disabled(self.checks_loading || self.checks.is_none())
                                .on_click(cx.listener(|s, _, _, cx| s.recheck(cx))),
                        ),
                    ),
            )
    }

    /// 점검 화면 헤더의 요약(통과·주의·건너뜀·실패 개수).
    fn summary_line(&self) -> String {
        let Some(checks) = &self.checks else {
            return text::CHECKING.get().to_owned();
        };
        let count = |s: Status| checks.iter().filter(|c| c.status == s).count().to_string();
        text::DONE_SUMMARY.format(&[
            ("ok", &count(Status::Ok)),
            ("warn", &count(Status::Warn)),
            ("skip", &count(Status::Skip)),
            ("fail", &count(Status::Fail)),
        ])
    }

    /// 고정 푸터. 왼쪽 [닫기](Esc).
    fn render_footer(cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        div()
            .id("wizard-footer")
            .debug_selector(|| "wizard-footer".into())
            .flex_none()
            .px_6()
            .py_3()
            .border_t_1()
            .border_color(p.border)
            .flex()
            .items_center()
            .child(
                div().debug_selector(|| "wizard-later".into()).child(
                    ui::ghost("wizard-later", text::CLOSE.get())
                        .tooltip(text::LATER_HINT.get())
                        .on_click(cx.listener(|_, _, _, cx| Self::close(cx))),
                ),
            )
            .child(
                div()
                    .debug_selector(|| "wizard-next-steps".into())
                    .ml_3()
                    .min_w_0()
                    .truncate()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::NEXT_STEPS_HINT.get()),
            )
    }

    fn render_check_row(
        &self,
        index: usize,
        last: bool,
        check: &Check,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = ui::palette(cx);
        let color = match check.status {
            Status::Ok => p.success,
            Status::Warn => p.warning,
            Status::Fail => p.destructive,
            Status::Skip => p.muted_foreground,
        };
        let detail = check_text(&check.detail);
        let fix_error = self.fix_errors.get(&check.id);
        // 충돌 원문은 안내 아래가 아니라 펼침(자세히)에서만 보인다(OWE-714 D2).
        let conflict = match fix_error {
            Some(super::actions::FixError::Conflict { detail, .. }) => Some(detail.clone()),
            _ => None,
        };
        let expandable = !detail.is_empty() || conflict.is_some();
        let open = self.is_open(check) && expandable;
        let fix = self.row_fix(check);
        let hook = match fix {
            Some((Fix::Hook(agent), _)) => self.hooks.get(agent.id()).map(|ph| (agent, ph)),
            _ => None,
        };
        let busy = match fix {
            Some((Fix::AllowNotifications, _)) => self.busy.contains("notify-allow"),
            Some((Fix::ProbeFolders, _)) => self.busy.contains("folders-probe"),
            Some((Fix::Link, _)) => self.busy.contains("cli"),
            Some((Fix::Hook(_), _)) => hook.is_some(),
            Some((Fix::Runner, _)) => self.busy.contains(&format!("fix:{}", check.id)),
            _ => false,
        };
        let id = check.id.clone();
        let check_agent = id
            .strip_prefix("agent.")
            .or_else(|| id.strip_prefix("hook."));
        let title = div()
            .flex()
            .items_center()
            .gap_1p5()
            .children(
                check_agent.map(|ag| ui::icons::agent_brand_icon(ag, ui::ICON_SM, p.foreground)),
            )
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(check_title(&check.id)),
            )
            .children((check.status == Status::Skip).then(|| {
                ui::badge(p, ui::BadgeTone::Muted, ctext::STATUS_SKIPPED.get()).debug_selector({
                    let id = id.clone();
                    move || format!("wizard-skipped-{id}")
                })
            }));
        let line = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Icon::new(status_icon(check.status))
                    .small()
                    .text_color(color),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .debug_selector({
                        let id = id.clone();
                        move || format!("wizard-check-{id}")
                    })
                    .child(title)
                    .child(
                        div()
                            .debug_selector({
                                let id = id.clone();
                                move || format!("wizard-check-summary-{id}")
                            })
                            .truncate()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(check_text(&check.summary)),
                    ),
            )
            .children(fix.map(|(fix, label)| {
                let label = if fix == Fix::Copy && self.copied.as_deref() == Some(check.id.as_str())
                {
                    text::COPIED.get()
                } else {
                    label
                };
                let check = check.clone();
                div()
                    .flex_none()
                    .debug_selector({
                        let id = id.clone();
                        move || format!("wizard-fix-{id}")
                    })
                    .child(
                        ui::secondary(("wizard-fix", index), label)
                            .loading(busy)
                            .disabled(busy)
                            .on_click(cx.listener(move |s, _, _, cx| s.run_fix(fix, &check, cx))),
                    )
            }))
            .children(expandable.then(|| {
                div()
                    .flex_none()
                    .debug_selector({
                        let id = id.clone();
                        move || format!("wizard-expand-{id}")
                    })
                    .child(
                        Button::new(("wizard-expand", index))
                            .ghost()
                            .xsmall()
                            .icon(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .tooltip(text::DETAILS.get())
                            .accessibility_label(text::DETAILS.get())
                            .on_click({
                                let id = id.clone();
                                cx.listener(move |s, _, _, cx| s.toggle_expand(&id, cx))
                            }),
                    )
            }));
        div()
            .id(gpui::SharedString::from(format!("wizard-row-{id}")))
            .px_3()
            .py_2()
            .when(!last, |d| d.border_b_1().border_color(p.border))
            .flex()
            .flex_col()
            .gap_1()
            .child(line)
            .children(open.then(|| {
                div()
                    .debug_selector({
                        let id = id.clone();
                        move || format!("wizard-detail-{id}")
                    })
                    .pl_6()
                    .text_size(ui::TEXT_XS)
                    .text_color(if check.status == Status::Fail {
                        p.foreground
                    } else {
                        p.muted_foreground
                    })
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children((!detail.is_empty()).then_some(detail))
                    .children(conflict.map(|raw| {
                        div()
                            .debug_selector({
                                let id = id.clone();
                                move || format!("wizard-fix-conflict-detail-{id}")
                            })
                            .font_family(ui::FONT_MONO)
                            .text_size(ui::TEXT_SM)
                            .child(raw)
                    }))
            }))
            .children(hook.map(|(agent, phase)| Self::render_hook_phase(agent, phase, cx)))
            .children(fix_error.and_then(|error| {
                use super::actions::FixError;
                match error {
                    FixError::Cancelled => None,
                    FixError::Failed(message) => Some(
                        div()
                            .debug_selector({
                                let id = id.clone();
                                move || format!("wizard-fix-error-{id}")
                            })
                            .pl_6()
                            .child(ui::error_banner(p, message.clone())),
                    ),
                    // 충돌 안내는 경고 톤 그대로 둔다(경고용 배너 프리미티브는 없다).
                    FixError::Conflict { guide, .. } => Some(
                        div()
                            .debug_selector({
                                let id = id.clone();
                                move || format!("wizard-fix-conflict-{id}")
                            })
                            .pl_6()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.warning)
                            .child(guide.clone()),
                    ),
                }
            }))
    }

    /// hook 행 아래: 진행·diff 확인·실패. 확인 버튼은 diff 위에 둬 긴 diff에도 행 가까이 보인다.
    fn render_hook_phase(
        agent: TargetAgent,
        phase: &HookPhase,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let p = ui::palette(cx);
        let id = agent.id();
        let name = super::agent_name(id);
        match phase {
            HookPhase::Working => div()
                .pl_6()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(Icon::new(IconName::LoaderCircle).small())
                .child(text::HOOK_INSTALLING.get())
                .into_any_element(),
            HookPhase::Failed(error) => div()
                .debug_selector(move || format!("wizard-hook-error-{id}"))
                .pl_6()
                .child(ui::error_banner(p, error.clone()))
                .into_any_element(),
            HookPhase::Review(diff) => div()
                .pl_6()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .debug_selector(move || format!("wizard-hook-title-{id}"))
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::HOOK_TITLE.format(&[("name", name)])),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    div()
                                        .debug_selector(move || format!("wizard-hook-cancel-{id}"))
                                        .child(
                                            Button::new(gpui::SharedString::from(format!(
                                                "wizard-hook-cancel-{id}"
                                            )))
                                            .small()
                                            .ghost()
                                            .label(text::CANCEL.get())
                                            .on_click(cx.listener(move |s, _, _, cx| {
                                                s.hook_cancel(agent, cx);
                                            })),
                                        ),
                                )
                                .child(
                                    div()
                                        .debug_selector(move || format!("wizard-hook-confirm-{id}"))
                                        .child(
                                            Button::new(gpui::SharedString::from(format!(
                                                "wizard-hook-confirm-{id}"
                                            )))
                                            .small()
                                            .primary()
                                            .label(text::HOOK_CONFIRM.get())
                                            .on_click(cx.listener(move |s, _, _, cx| {
                                                s.hook_confirm(agent, cx);
                                            })),
                                        ),
                                ),
                        ),
                )
                .child(
                    div()
                        .debug_selector(move || format!("wizard-hook-diff-{id}"))
                        .p_2()
                        .rounded(ui::RADIUS_XS)
                        .bg(p.muted)
                        .font_family(ui::FONT_MONO)
                        .text_size(ui::TEXT_SM)
                        .child(diff.clone()),
                )
                .into_any_element(),
        }
    }

    /// 점검 화면 본문(스크롤). 등급별 카드.
    fn render_checks(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = ui::palette(cx);
        let mut body = div()
            .id("wizard-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_6()
            .pb_4()
            .flex()
            .flex_col()
            .gap_4();
        let Some(checks) = &self.checks else {
            return body
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_color(p.muted_foreground)
                        .child(Icon::new(IconName::LoaderCircle).small())
                        .child(text::CHECKING.get()),
                )
                .into_any_element();
        };
        let blockers = required_blockers(checks);
        if !blockers.is_empty() {
            let names = blockers
                .iter()
                .map(|c| check_title(&c.id))
                .collect::<Vec<_>>()
                .join(", ");
            body = body.child(
                ui::error_banner(
                    p,
                    text::BLOCKED_REQUIRED
                        .format(&[("n", &blockers.len().to_string()), ("ids", &names)]),
                )
                .debug_selector(|| "wizard-blocked".into()),
            );
        }
        let mut index = 0;
        for (level, id, header) in [
            (
                Level::Required,
                "wizard-group-required",
                text::GROUP_REQUIRED.get(),
            ),
            (
                Level::Recommended,
                "wizard-group-recommended",
                text::GROUP_RECOMMENDED.get(),
            ),
            (
                Level::Optional,
                "wizard-group-optional",
                text::GROUP_OPTIONAL.get(),
            ),
        ] {
            let rows: Vec<_> = checks.iter().filter(|c| c.level == level).collect();
            let Some(last) = rows.len().checked_sub(1) else {
                continue;
            };
            let mut card = ui::card(p).p_0().flex().flex_col();
            for (i, check) in rows.into_iter().enumerate() {
                card = card.child(self.render_check_row(index, i == last, check, cx));
                index += 1;
            }
            body = body.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(ui::section_title(p, id, header))
                    .child(card),
            );
        }
        body.into_any_element()
    }
}

impl Render for Wizard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let body = self.render_checks(cx);
        // OWE-903: 별도 OS 창 대신 메인 창 안 중앙 모달. 가짜 타이틀바는 그리지 않는다.
        // 카드는 내용 높이에 맞추고 최대 높이만 둔다(넘치면 본문이 스크롤).
        let card = div()
            .id("onboarding-wizard")
            .debug_selector(|| "onboarding-wizard".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .role(gpui::Role::Dialog)
            .aria_label(text::TITLE.get())
            .occlude()
            .w(ui::SETTINGS_MODAL_WIDTH)
            .max_w(window.viewport_size().width - ui::SPACE_4 * 2.)
            .max_h((window.viewport_size().height - ui::SPACE_4 * 2.).min(px(620.)))
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_SM)
            .flex()
            .flex_col()
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .on_action(cx.listener(|_, _: &CloseWizard, _, cx| Self::close(cx)))
            .on_key_down(cx.listener(|_, e: &KeyDownEvent, _, cx| {
                if e.keystroke.key != "escape" {
                    return;
                }
                Self::close(cx);
                cx.stop_propagation();
            }))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| Self::close(cx)))
            .child(self.render_header(cx))
            .child(body)
            .child(Self::render_footer(cx));
        ui::modal_backdrop(window.viewport_size(), card)
    }
}

#[cfg(all(test, unix))]
mod async_tests {
    use super::*;

    #[gpui::test]
    fn collect_and_permission_job_keep_independent_completions(cx: &mut gpui::TestAppContext) {
        let root =
            std::env::temp_dir().join(format!("krill-checks-overlap-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        cx.update(gpui_component::init);
        let env = Env {
            shell: root.join("missing-shell").into_os_string(),
            krill: root.join("missing-krill"),
            state: root.join("state"),
            default_state: Some(krill_proto::daemon::state_dir_in(&root.join("home"))),
            home: root.join("home"),
            state_overridden: true,
        };
        let wizard = cx.new(|cx| Wizard::new(env, cx));
        wizard.update(cx, |wizard, cx| {
            wizard.begin_collect(cx);
            wizard.allow_notifications(cx);
            assert!(wizard.checks_loading);
            assert!(wizard.busy.contains("notify-allow"));
            assert!(wizard.tasks.contains_key("checks"));
            assert!(wizard.tasks.contains_key("notify-allow"));
        });
        cx.run_until_parked();
        wizard.read_with(cx, |wizard, _| {
            assert!(!wizard.checks_loading);
            assert!(!wizard.busy.contains("notify-allow"));
            assert!(wizard.survey.is_some());
        });
        std::fs::remove_dir_all(root).unwrap();
    }
}
