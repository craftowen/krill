//! 온보딩 창(MK-161, OWE-714): 첫 실행 1장 + 설정 점검 화면을 한 창이 맡는다.
//!
//! - 첫 실행(동의 단계)은 1장이다. 짧은 동의(요약 3줄 + 자세히 보기), 감지된 기본 에이전트,
//!   [시작하기]. 체크리스트는 보여 주지 않는다. 시작하면 동의를 기록하고 완료로 저장한 뒤
//!   닫는다. 백그라운드 점검에서 필수 실패가 나왔을 때만 점검 화면으로 넘어가 막는다.
//! - 점검 화면(설정 > 일반 > 설정 점검 재진입, 필수 실패)은 staged `krill doctor --json
//!   --no-start` + `app.*`를 등급별 카드로 보여 준다. 행 오른쪽에 조치 버튼과 펼침 chevron,
//!   고정 헤더(제목·요약·모두 다시 검사)와 고정 푸터(닫기). 창 재포커스 재검사(1초 디바운스).
//! - Enter = 주 버튼, Esc = 나중에/닫기. 창은 680×620 고정(크기 조절·최소화 없음).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, Entity, FocusHandle, KeyDownEvent,
    Render, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use krill_proto::agent::hooks::Agent;
use krill_proto::doctor::{Check, FixKind, Level, Status};

use super::consent::view::ConsentView;
use super::state::{OnboardingState, STATE_VERSION, Step, save};
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
/// 창 크기(고정). Orca 온보딩 모달 비율에 맞춘 값이다.
pub(super) const WINDOW_SIZE: (f32, f32) = (680., 620.);

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
    Hook(Agent),
    /// 앱 안 CLI 링크 설치.
    Link,
}

pub(super) struct Wizard {
    pub(super) env: Env,
    pub(super) step: Step,
    /// 기존 완료 사용자의 재동의(MK-127). 기본 에이전트를 묻지 않고 동의만 기록한다.
    pub(super) consent_only: bool,
    pub(super) consent: Option<Entity<ConsentView>>,
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
    /// 점검 ID별 마지막 실행기 조치 실패(충돌 안내 포함).
    pub(super) fix_errors: HashMap<String, super::actions::FixError>,
    folder_check_requested: bool,
    /// 첫 화면에서 고른 기본 에이전트(감지된 것 중).
    pub(super) agent: Option<String>,
    pub(super) last_recheck: Instant,
    focus: FocusHandle,
    task: Option<Task<()>>,
    fix_tasks: HashMap<String, Task<()>>,
    activation: Option<Subscription>,
    consent_watch: Option<Subscription>,
}

impl std::fmt::Debug for Wizard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wizard")
            .field("step", &self.step)
            .field("checks", &self.checks.as_ref().map(Vec::len))
            .field("agent", &self.agent)
            .finish_non_exhaustive()
    }
}

/// 온보딩 창을 연다. 수집은 백그라운드에서 바로 시작한다.
pub(super) fn open(env: &Env, start: Step, consent_only: bool, cx: &mut App) {
    let options = window_options(cx);
    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| Wizard::new(env.clone(), start, consent_only, cx));
        view.update(cx, |w, cx| {
            w.mount_consent(cx);
            let sub = cx.observe_window_activation(window, |w, window, cx| {
                if window.is_window_active() {
                    w.activated(cx);
                }
            });
            w.activation = Some(sub);
        });
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
        view
    });
    match opened {
        Ok(handle) => {
            handle
                .update(cx, |w, _window, cx| {
                    w.begin_collect(cx);
                })
                .ok();
        }
        Err(_) => {
            tracing::warn!("onboarding wizard window creation failed");
        }
    }
}

/// 고정 크기 창(OWE-714 (8): 첫 실행에 화면 전체 크기로 뜨던 결함). 크기 조절을 끄면
/// macOS 확대(초록) 버튼도 꺼진다.
pub(super) fn window_options(cx: &App) -> WindowOptions {
    let (w, h) = WINDOW_SIZE;
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(w), px(h)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(text::TITLE.get().into()),
            appears_transparent: true,
            traffic_light_position: Some(ui::TRAFFIC_LIGHT_INSET),
        }),
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    }
}

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

/// 감지된(실행 파일이 있는) 에이전트 `(id, 이름)`.
fn detected_agents(survey: &Survey) -> Vec<(String, String)> {
    survey
        .agents
        .iter()
        .filter(|a| a.exe.is_some())
        .map(|a| (a.id.clone(), a.name.clone()))
        .collect()
}

/// 첫 화면 기본 선택: 설정의 기본 에이전트가 감지됐으면 그것, 아니면 처음 감지된 것.
fn pick_default_agent(detected: &[(String, String)], current: &str) -> Option<String> {
    detected
        .iter()
        .find(|(id, _)| id == current)
        .or_else(|| detected.first())
        .map(|(id, _)| id.clone())
}

impl Wizard {
    fn new(env: Env, step: Step, consent_only: bool, cx: &mut Context<Self>) -> Self {
        crate::orca_import::set_env_paths(
            Some(env.home.clone()),
            None,
            Some(env.state.clone()),
            None,
            cx,
        );
        crate::orca_import::ensure_scanned(cx);
        Self {
            env,
            step,
            consent_only,
            consent: None,
            survey: None,
            staged: None,
            checks: None,
            checks_loading: false,
            toggled: HashSet::new(),
            copied: None,
            busy: HashSet::new(),
            hooks: HashMap::new(),
            fix_errors: HashMap::new(),
            folder_check_requested: false,
            agent: None,
            last_recheck: Instant::now(),
            focus: cx.focus_handle(),
            task: None,
            fix_tasks: HashMap::new(),
            activation: None,
            consent_watch: None,
        }
    }

    /// 동의 단계면 동의 뷰를 얹는다. 체크 상태가 바뀌면 푸터 [시작하기]를 다시 그린다.
    fn mount_consent(&mut self, cx: &mut Context<Self>) {
        if self.step != Step::Consent || self.consent.is_some() {
            return;
        }
        let state = self.env.state.clone();
        let view = cx.new(|_| ConsentView::new(state));
        self.consent_watch = Some(cx.observe(&view, |_, _, cx| cx.notify()));
        self.consent = Some(view);
    }

    /// [시작하기]: 동의를 기록하고 완료로 저장한다. 기록이 실패해도 닫고 다음 실행에 다시 묻는다.
    /// 백그라운드 점검에 필수 실패가 있으면 닫지 않고 점검 화면으로 넘어간다.
    fn on_consented(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) =
            super::consent::record_consent(&self.env.state, std::time::SystemTime::now())
        {
            tracing::warn!(%e, "consent record failed");
        }
        if !self.consent_only
            && let Some(agent) = self.agent.clone()
        {
            crate::settings::set_default_agent(&agent, cx);
        }
        let done = OnboardingState {
            completed: false,
            step: Step::Done,
            version: STATE_VERSION,
        }
        .completed();
        if let Err(e) = save(&self.env.state, &done) {
            tracing::warn!(%e, "onboarding completion save failed");
        }
        self.consent = None;
        self.consent_watch = None;
        let blocked = self
            .checks
            .as_deref()
            .is_some_and(|c| !required_blockers(c).is_empty());
        if blocked && !self.consent_only && !self.env.is_isolated_state() {
            // 필수 실패는 자동 준비와 재검사 뒤에도 남을 때만 막는다.
            let prepare = super::actions::auto_prepare(self.env.clone(), cx);
            let env = self.env.clone();
            let handle = window.window_handle();
            self.busy.insert("auto-prepare".to_owned());
            self.task = Some(cx.spawn(async move |this, cx| {
                prepare.await.ok();
                let collected = cx
                    .background_executor()
                    .spawn(async move { collect(&env) })
                    .await;
                this.update(cx, |w, cx| {
                    w.busy.remove("auto-prepare");
                    w.apply_collected(collected, cx);
                    if w.checks
                        .as_deref()
                        .is_some_and(|checks| !required_blockers(checks).is_empty())
                    {
                        w.step = Step::Checklist;
                    } else {
                        handle
                            .update(cx, |_, window, _| window.remove_window())
                            .ok();
                    }
                    cx.notify();
                })
                .ok();
            }));
        } else if blocked && !self.consent_only {
            self.step = Step::Checklist;
            cx.notify();
        } else {
            if !self.env.is_isolated_state() {
                super::actions::auto_prepare(self.env.clone(), cx).detach();
            }
            window.remove_window();
        }
    }

    /// 온보딩 다시 실행: 첫 화면부터 다시 시작한다(열려 있는 창을 재사용).
    /// 명시적 재실행은 전체 흐름이므로 `consent_only`를 끈다.
    pub(super) fn restart_to_consent(&mut self, cx: &mut Context<Self>) {
        self.step = Step::Consent;
        self.consent_only = false;
        self.consent = None;
        self.mount_consent(cx);
        self.persist_step();
        cx.notify();
    }

    fn persist_step(&self) {
        let state = OnboardingState {
            completed: false,
            step: self.step,
            version: STATE_VERSION,
        };
        if let Err(e) = save(&self.env.state, &state) {
            tracing::warn!(%e, "onboarding step save failed");
        }
    }

    /// 창이 처음 열릴 때 수집 시작.
    fn begin_collect(&mut self, cx: &mut Context<Self>) {
        if self.checks_loading {
            return;
        }
        self.checks_loading = true;
        cx.notify();
        let env = self.env.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let collected: Collected = cx
                .background_executor()
                .spawn(async move { collect(&env) })
                .await;
            this.update(cx, |w, cx| {
                w.apply_collected(collected, cx);
                if let Some(checks) = &w.checks {
                    super::store_snapshot(cx, checks.clone());
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn apply_collected(&mut self, collected: Collected, cx: &App) {
        self.checks_loading = false;
        self.staged = collected.staged;
        let detected = detected_agents(&collected.survey);
        if self.agent.is_none() {
            self.agent = pick_default_agent(&detected, &crate::settings::current(cx).default_agent);
        }
        self.survey = Some(collected.survey);
        self.checks = Some(merge_checks(collected.doctor, collected.app));
        self.last_recheck = Instant::now();
    }

    /// 조용한 재검사(`doctor` + `app.*`). 조치·설정 변경 뒤 반영용.
    fn recheck(&mut self, cx: &mut Context<Self>) {
        if self.checks_loading {
            return;
        }
        self.checks_loading = true;
        self.last_recheck = Instant::now();
        cx.notify();
        let env = self.env.clone();
        let folders = self.folder_check_requested;
        self.task = Some(cx.spawn(async move |this, cx| {
            let collected = cx
                .background_executor()
                .spawn(async move {
                    let mut collected = collect(&env);
                    if folders {
                        let check =
                            perm::decide_folder_probe(&perm::probe_folders(FOLDER_PROBE_TIMEOUT));
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
                w.apply_collected(collected, cx);
                if let Some(checks) = &w.checks {
                    super::store_snapshot(cx, checks.clone());
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// 창 재포커스(시스템 설정에서 돌아옴) → 디바운스 후 재검사.
    fn activated(&mut self, cx: &mut Context<Self>) {
        super::actions::recheck_needed(cx).detach();
        if self.step != Step::Consent
            && !self.checks_loading
            && self.last_recheck.elapsed() > RECHECK_DEBOUNCE
        {
            self.recheck(cx);
        }
    }

    /// `[나중에]`/`[닫기]`(Esc). 첫 화면이면 다음 실행에 다시 묻도록 단계를 남긴다.
    fn later(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        if self.step == Step::Consent {
            self.persist_step();
        }
        window.remove_window();
    }

    /// 동의 체크박스가 체크됐나. 체크 전에는 [시작하기]·Enter가 아무 일도 하지 않는다.
    pub(super) fn accepted(&self, cx: &App) -> bool {
        self.consent.as_ref().is_some_and(|v| v.read(cx).accepted())
    }

    /// 주 버튼(Enter). 첫 화면은 [시작하기](체크 전이면 무시), 점검 화면은 닫기.
    fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.step {
            Step::Consent if self.accepted(cx) => self.on_consented(window, cx),
            Step::Consent => {}
            _ => self.later(window, cx),
        }
    }

    /// [종료]: 동의 거절 = 앱 프로세스만 끝낸다. 데몬·세션은 살려 두고 아무것도 쓰지 않는다.
    fn quit(cx: &mut App) {
        cx.quit();
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
        cx.notify();
        let home = self.env.home.clone();
        let state = self.env.state.clone();
        let env = self.env.clone();
        let link = cli_link_target(self.survey.as_ref());
        let prep = (!self.env.is_isolated_state())
            .then(|| super::actions::auto_prepare(self.env.clone(), cx));
        self.task = Some(cx.spawn(async move |this, cx| {
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
        }));
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
        let perm_open = |value: &str| {
            crate::platform::permissions::open_first_available(&[
                value,
                crate::platform::permissions::SETTINGS_ROOT_URL,
            ]);
        };
        match fix {
            Fix::Runner => self.run_runner(&check.id, cx),
            Fix::Copy => self.copy_fix(&check.id, &check.fix.value, cx),
            Fix::OpenSettings => perm_open(&check.fix.value),
            Fix::Reveal => {
                crate::platform::permissions::open_first_available(&[
                    crate::platform::permissions::APPLICATIONS_URL,
                ]);
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
                    if let Err(e) = outcome {
                        w.fix_errors.insert(id, e);
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
        self.task = Some(cx.spawn(async move |this, cx| {
            let status = cx
                .background_executor()
                .spawn(async move {
                    perm::request_notification_authorization(AUTH_REQUEST_TIMEOUT);
                    perm::query_notification_status(NOTIFICATION_QUERY_TIMEOUT)
                })
                .await;
            this.update(cx, |w, cx| {
                w.busy.remove("notify-allow");
                let check = perm::decide_notifications(status, perm::bundle_id().as_deref());
                w.replace_app_check(check);
                cx.notify();
            })
            .ok();
        }));
    }

    /// `[검사]`. 폴더 프로브 묶음을 돌려 행에 반영한다.
    fn probe_folders(&mut self, cx: &mut Context<Self>) {
        use crate::platform::permissions as perm;
        if !self.busy.insert("folders-probe".to_owned()) {
            return;
        }
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
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
        }));
    }

    fn replace_app_check(&mut self, check: Check) {
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
    fn hook_install(&mut self, agent: Agent, cx: &mut Context<Self>) {
        self.hooks.insert(agent.id(), HookPhase::Working);
        cx.notify();
        let (staged, path) = self.hook_paths();
        self.task = Some(cx.spawn(async move |this, cx| {
            let dry = cx
                .background_executor()
                .spawn(async move { mk_hook(&staged, &path, agent, "--dry-run") })
                .await;
            this.update(cx, |w, cx| {
                let phase = match dry {
                    Ok(diff) => HookPhase::Review(clip(&diff, agent)),
                    Err(e) => HookPhase::Failed(e),
                };
                w.hooks.insert(agent.id(), phase);
                cx.notify();
            })
            .ok();
        }));
    }

    fn hook_confirm(&mut self, agent: Agent, cx: &mut Context<Self>) {
        self.hooks.insert(agent.id(), HookPhase::Working);
        cx.notify();
        let (staged, path) = self.hook_paths();
        self.task = Some(cx.spawn(async move |this, cx| {
            let done = cx
                .background_executor()
                .spawn(async move { mk_hook(&staged, &path, agent, "--yes") })
                .await;
            this.update(cx, |w, cx| {
                match done {
                    Ok(_) => {
                        w.hooks.remove(agent.id());
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
        }));
    }

    fn hook_cancel(&mut self, agent: Agent, cx: &mut Context<Self>) {
        self.hooks.remove(agent.id());
        cx.notify();
    }

    /// `[설치]`(CLI 링크). 격리 상태면 실제 링크를 만들지 않는다.
    fn cli_install(&mut self, cx: &mut Context<Self>) {
        let Some(link) = cli_link_target(self.survey.as_ref()) else {
            return;
        };
        if !self.busy.insert("cli".to_owned()) {
            return;
        }
        cx.notify();
        let (krill, home) = (self.env.krill.clone(), self.env.home.clone());
        let isolated = self.env.is_isolated_state();
        self.task = Some(cx.spawn(async move |this, cx| {
            let ok = cx
                .background_executor()
                .spawn(async move {
                    if isolated {
                        return true;
                    }
                    install_cli_link(&krill, &home, &link)
                        .inspect_err(|e| tracing::warn!(%e, "cli link install failed"))
                        .is_ok()
                })
                .await;
            this.update(cx, |w, cx| {
                w.busy.remove("cli");
                if ok && let Some(survey) = &mut w.survey {
                    survey.cli = Cli::Installed;
                }
                cx.notify();
            })
            .ok();
            if ok {
                this.update(cx, Self::recheck).ok();
            }
        }));
    }

    /// 행 조치와 버튼 문구. 실행기가 맡는 행이 먼저이고, 나머지는 앱 안 조치(hook·CLI 링크)를
    /// 복사보다 먼저 고른다.
    fn row_fix(&self, check: &Check) -> Option<(Fix, &'static str)> {
        use crate::platform::permissions as perm;
        if check.status == Status::Ok {
            return None;
        }
        if let Some(label) = super::actions::fix_label(check) {
            return Some((Fix::Runner, label));
        }
        if check.status == Status::Warn
            && let Some(agent) = check.id.strip_prefix("hook.").and_then(Agent::parse)
        {
            return Some((Fix::Hook(agent), text::INSTALL.get()));
        }
        if check.id == "cli.path" && cli_link_target(self.survey.as_ref()).is_some() {
            return Some((Fix::Link, text::INSTALL.get()));
        }
        match (&check.fix.kind, check.id.as_str(), check.status) {
            (FixKind::Command | FixKind::Copy, _, _) if !check.fix.value.is_empty() => {
                Some((Fix::Copy, text::FIX_COPY.get()))
            }
            (FixKind::OpenUrl, _, _) if check.fix.value == perm::APPLICATIONS_URL => {
                Some((Fix::Reveal, text::FIX_REVEAL.get()))
            }
            (FixKind::OpenUrl, _, _) => Some((Fix::OpenSettings, text::FIX_OPEN_SETTINGS.get())),
            (FixKind::Reinstall, _, _) => Some((Fix::Reveal, text::FIX_REVEAL.get())),
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
        let (title, subtitle) = match self.step {
            Step::Consent => (
                crate::i18n::catalog::consent::TITLE.get().to_owned(),
                crate::i18n::catalog::consent::INTRO.get().to_owned(),
            ),
            _ => (text::CHECKS_TITLE.get().to_owned(), self.summary_line()),
        };
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
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(subtitle),
                    ),
            )
            .children((self.step != Step::Consent).then(|| {
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div().debug_selector(|| "wizard-fix-all".into()).child(
                            ui::secondary("wizard-fix-all", text::FIX_ALL.get())
                                .primary()
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
                    )
            }))
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

    /// 고정 푸터. 왼쪽 [나중에]/[닫기](Esc), 오른쪽 주 동작.
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let first = self.step == Step::Consent;
        let accepted = self.accepted(cx);
        let mut right = div().flex().items_center().gap_2();
        if first {
            right = right
                .child(
                    div().debug_selector(|| "consent-quit".into()).child(
                        Button::new("consent-quit-btn")
                            .outline()
                            .label(crate::i18n::catalog::consent::QUIT.get())
                            .on_click(|_, _, cx| Self::quit(cx)),
                    ),
                )
                .child(
                    div().debug_selector(|| "consent-continue".into()).child(
                        Button::new("consent-continue-btn")
                            .primary()
                            .label(text::START_NOW.get())
                            .tooltip(text::START_HINT.get())
                            .disabled(!accepted)
                            .on_click(cx.listener(|s, _, window, cx| s.primary(window, cx))),
                    ),
                );
        }
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
            .justify_between()
            .child(
                div().debug_selector(|| "wizard-later".into()).child(
                    Button::new("wizard-later")
                        .ghost()
                        .label(if first {
                            text::LATER.get()
                        } else {
                            text::CLOSE.get()
                        })
                        .tooltip(text::LATER_HINT.get())
                        .on_click(cx.listener(|s, _, window, cx| s.later(window, cx))),
                ),
            )
            .child(right)
    }

    /// 첫 화면 본문: 동의 + 기본 에이전트.
    fn render_first(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = ui::palette(cx);
        let consent = match &self.consent {
            Some(view) => view.clone().into_any_element(),
            None => div().child(text::CHECKING.get()).into_any_element(),
        };
        let mut body = div()
            .flex_1()
            .min_h_0()
            .px_6()
            .pb_4()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().flex_1().min_h_0().child(consent));
        if !self.consent_only {
            let detected = self.survey.as_ref().map(detected_agents);
            let choices: gpui::AnyElement = match detected {
                None => div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_color(p.muted_foreground)
                    .child(Icon::new(IconName::LoaderCircle).small())
                    .child(text::AGENT_DETECTING.get())
                    .into_any_element(),
                Some(found) if found.is_empty() => div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::AGENT_NONE.get())
                    .into_any_element(),
                Some(found) => div()
                    .flex()
                    .flex_wrap()
                    .gap_1p5()
                    .children(found.into_iter().map(|(id, name)| {
                        let selected = self.agent.as_deref() == Some(id.as_str());
                        let selector = format!("wizard-agent-{id}");
                        let button = Button::new(gpui::SharedString::from(selector.clone()))
                            .small()
                            .label(name)
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.agent = Some(id.clone());
                                cx.notify();
                            }));
                        div()
                            .debug_selector(move || selector.clone())
                            .child(if selected {
                                button.primary()
                            } else {
                                button.outline()
                            })
                    }))
                    .into_any_element(),
            };
            body = body.child(
                ui::card(p)
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .debug_selector(|| "wizard-agent".into())
                    .child(ui::label(
                        p,
                        text::AGENT_LABEL.get(),
                        Some(text::AGENT_HINT.get()),
                    ))
                    .child(div().flex_none().max_w(px(360.)).child(choices)),
            );
            if let Some(orca_card) = crate::orca_import::render_banner("onboarding", cx) {
                body = body.child(orca_card.flex_none());
            }
        }
        body.into_any_element()
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
        let title = div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(check_title(&check.id)),
            )
            .children((check.status == Status::Skip).then(|| {
                div()
                    .debug_selector({
                        let id = id.clone();
                        move || format!("wizard-skipped-{id}")
                    })
                    .px_1p5()
                    .rounded(ui::RADIUS_XS)
                    .bg(p.muted)
                    .text_size(ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(ctext::STATUS_SKIPPED.get())
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
                            .child(raw)
                    }))
            }))
            .children(hook.map(|(agent, phase)| Self::render_hook_phase(agent, phase, cx)))
            .children(fix_error.map(|error| {
                use super::actions::FixError;
                let (selector, color, line) = match error {
                    FixError::Failed(message) => (
                        "wizard-fix-error",
                        p.destructive,
                        text::HOOK_FAILED.format(&[("error", message.as_str())]),
                    ),
                    FixError::Conflict { guide, .. } => {
                        ("wizard-fix-conflict", p.warning, guide.clone())
                    }
                };
                div()
                    .debug_selector({
                        let id = id.clone();
                        move || format!("{selector}-{id}")
                    })
                    .pl_6()
                    .text_size(ui::TEXT_XS)
                    .text_color(color)
                    .child(line)
            }))
    }

    /// hook 행 아래: 진행·diff 확인·실패. 확인 버튼은 diff 위에 둬 긴 diff에도 행 가까이 보인다.
    fn render_hook_phase(
        agent: Agent,
        phase: &HookPhase,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let p = ui::palette(cx);
        let id = agent.id();
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
                .pl_6()
                .text_size(ui::TEXT_XS)
                .text_color(p.destructive)
                .child(text::HOOK_FAILED.format(&[("error", error.as_str())]))
                .into_any_element(),
            HookPhase::Review(diff) => div()
                .pl_6()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .flex()
                        .justify_end()
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
                                    .on_click(
                                        cx.listener(move |s, _, _, cx| s.hook_cancel(agent, cx)),
                                    ),
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
                                    .on_click(
                                        cx.listener(move |s, _, _, cx| s.hook_confirm(agent, cx)),
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
                        .text_size(ui::TEXT_XS)
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
        if let Some(orca_card) = crate::orca_import::render_banner("onboarding-checks", cx) {
            body = body.child(orca_card);
        }
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
                div()
                    .debug_selector(|| "wizard-blocked".into())
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.destructive)
                    .text_size(ui::TEXT_XS)
                    .text_color(p.destructive)
                    .child(Icon::new(IconName::CircleX).small())
                    .child(
                        text::BLOCKED_REQUIRED
                            .format(&[("n", &blockers.len().to_string()), ("ids", &names)]),
                    ),
            );
        }
        let mut index = 0;
        for (level, header) in [
            (Level::Required, text::GROUP_REQUIRED.get()),
            (Level::Recommended, text::GROUP_RECOMMENDED.get()),
            (Level::Optional, text::GROUP_OPTIONAL.get()),
        ] {
            let rows: Vec<_> = checks.iter().filter(|c| c.level == level).collect();
            let Some(last) = rows.len().checked_sub(1) else {
                continue;
            };
            let mut card = div()
                .rounded(ui::RADIUS_MD)
                .border_1()
                .border_color(p.border)
                .bg(p.card)
                .flex()
                .flex_col();
            for (i, check) in rows.into_iter().enumerate() {
                card = card.child(self.render_check_row(index, i == last, check, cx));
                index += 1;
            }
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.muted_foreground)
                            .child(header),
                    )
                    .child(card),
            );
        }
        body.into_any_element()
    }
}

impl Render for Wizard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let body = match self.step {
            Step::Consent => self.render_first(cx),
            Step::Checklist | Step::Connect | Step::Done => self.render_checks(cx),
        };
        div()
            .id("onboarding-wizard")
            .track_focus(&self.focus)
            .role(gpui::Role::Dialog)
            .aria_label(text::TITLE.get())
            .on_key_down(cx.listener(|s, e: &KeyDownEvent, window, cx| {
                match e.keystroke.key.as_str() {
                    "escape" => {
                        if crate::orca_import::state(cx).is_some_and(|st| st.modal_open) {
                            crate::orca_import::close_modal(cx);
                        } else {
                            s.later(window, cx);
                        }
                    }
                    "enter" => s.primary(window, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(p.background)
            .text_color(p.foreground)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_SM)
            .child(
                div()
                    .id("onboarding-titlebar")
                    .debug_selector(|| "onboarding-titlebar".into())
                    .h(ui::TITLEBAR_HEIGHT)
                    .w_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(p.worktree_sidebar)
                    .border_b_1()
                    .border_color(p.border)
                    .window_control_area(gpui::WindowControlArea::Drag)
                    .text_size(ui::TEXT_XS)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(p.muted_foreground)
                    .child(text::TITLE.get()),
            )
            .child(self.render_header(cx))
            .child(body)
            .child(self.render_footer(cx))
            .children(crate::orca_import::render_modal(window, cx))
    }
}
