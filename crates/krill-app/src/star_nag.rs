//! GitHub 별 요청 카드·랜딩 알약 버튼(OWE-905).
//! 앱에서 에이전트 세션을 시작한 횟수가 문턱을 넘거나, 첫 에이전트 작업 완료(Value Moment) 또는
//! 첫 실행 온보딩 완료에 도달하면 메인 창 우상단에 카드를 띄우고, 빈 워크스페이스 하단에서도
//! 직접 별을 줄 수 있게 한다.
//! 텔레메트리 없음 — 상태는 `star-nag.toml` 하나다.
//!
//! Logic and UI adapted from Orca (`src/main/star-nag/*`, `src/main/github/client/fetch/orca-star.ts`,
//! `src/renderer/src/components/StarNagCard.tsx`, `src/renderer/src/components/star-nag/*`,
//! `Landing.tsx`, `settings/GeneralSupportSection.tsx`,
//! MIT License — Copyright (c) 2026 Orca Contributors):
//! 업데이트 간 카운터·문턱·쿨다운 유지, 첫 문턱 35, 1회 에이전트 완료 트리거(1.2초 정적 대기 + 재검증),
//! 첫 실행 온보딩 완료 토스트, `[나중에]`·닫기·웹 열기 = 문턱 2배 + 3일 쉼,
//! 별 성공·이미 별을 준 사용자 = 영구 완료.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{AnyElement, App, Global, MouseButton, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use serde::{Deserialize, Serialize};

use crate::i18n::catalog::star_nag as text;
use crate::onboarding::state::atomic_write_0600;

/// 상태 파일 이름(상태 디렉터리 바로 아래).
pub const FILE: &str = "star-nag.toml";
pub const INITIAL_THRESHOLD: u64 = 35;
pub const COOLDOWN_SECS: u64 = 3 * 24 * 60 * 60;
pub const VALUE_MOMENT_DELAY: Duration = Duration::from_millis(1200);
pub const REPO_URL: &str = "https://github.com/craftowen/krill";
const STARRED_API: &str = "user/starred/craftowen/krill";
const GH_TIMEOUT: Duration = Duration::from_secs(15);
const THANKS_FOR: Duration = Duration::from_secs(3);

/// 저장되는 상태. `deferred_until`은 유닉스 초.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub total: u64,
    pub baseline: u64,
    pub next_threshold: u64,
    pub deferred_until: u64,
    pub completed: bool,
    pub app_version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub agent_value_moment_app_version: String,
}

impl State {
    /// 기준점과 첫 문턱이 아직 없을 때만 초기화하고, 앱 버전이 바뀌어도 누적 카운터·기준점·문턱·쿨다운을 유지한다(Orca `ensureStarNagBaseline` 패리티).
    pub fn ensure_version(&mut self, version: &str) -> bool {
        let mut changed = false;
        if self.next_threshold == 0 {
            self.baseline = self.total;
            self.next_threshold = INITIAL_THRESHOLD;
            changed = true;
        }
        if self.app_version != version {
            version.clone_into(&mut self.app_version);
            changed = true;
        }
        changed
    }

    /// 첫 에이전트 작업 완료(Value Moment) 트리거가 이미 소진되었는가(업데이트 후에도 재장전하지 않음).
    #[must_use]
    pub fn value_moment_consumed(&self) -> bool {
        !self.agent_value_moment_app_version.is_empty()
    }

    /// Value Moment 트리거를 소진 처리한다. 새로 소진되었으면 `true`.
    pub fn consume_value_moment(&mut self, version: &str) -> bool {
        if self.value_moment_consumed() {
            return false;
        }
        if version.is_empty() {
            "consumed".clone_into(&mut self.agent_value_moment_app_version);
        } else {
            version.clone_into(&mut self.agent_value_moment_app_version);
        }
        true
    }

    /// 완료·쿨다운에 걸리지 않고 카드를 띄울 수 있는 상태인가.
    #[must_use]
    pub fn can_show_prompt(&self, now: u64) -> bool {
        !self.completed && now >= self.deferred_until
    }

    /// 시작 횟수 문턱 도달로 카드를 띄울 때인가.
    #[must_use]
    pub fn due(&self, now: u64) -> bool {
        self.can_show_prompt(now)
            && self.next_threshold > 0
            && self.total.saturating_sub(self.baseline) >= self.next_threshold
    }

    /// `[나중에]`·닫기·웹 열기: 문턱 2배, 지금부터 다시 세고, 3일 쉰다.
    pub fn defer(&mut self, now: u64) {
        let base_threshold = if self.next_threshold == 0 {
            INITIAL_THRESHOLD
        } else {
            self.next_threshold
        };
        self.next_threshold = base_threshold.saturating_mul(2);
        self.baseline = self.total;
        self.deferred_until = now.saturating_add(COOLDOWN_SECS);
    }

    pub fn complete(&mut self) {
        self.completed = true;
        self.deferred_until = 0;
    }
}

/// 상태를 읽는다. 없거나 깨졌으면 새로 시작한다.
#[must_use]
pub fn load(dir: &Path) -> State {
    std::fs::read_to_string(dir.join(FILE))
        .ok()
        .and_then(|body| toml::from_str(&body).ok())
        .unwrap_or_default()
}

/// 원자 쓰기로 저장한다.
pub fn save(dir: &Path, state: &State) -> std::io::Result<()> {
    let body = toml::to_string(state).map_err(std::io::Error::other)?;
    atomic_write_0600(&dir.join(FILE), &body)
}

/// 카드 및 버튼의 GitHub 별 동작 방식.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// `gh`가 인증되어 있어 클릭 한 번으로 바로 별을 줄 수 있음.
    #[default]
    Gh,
    /// `gh` 미설치·미인증 또는 직접 별 주기 실패 시 브라우저로 저장소를 엶.
    Web,
}

/// 프롬프트 표면 종류(Orca `StarNagPromptSurface` 패리티).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Surface {
    /// 에이전트 시작 횟수 문턱 도달 또는 첫 에이전트 작업 완료(Value Moment) 카드.
    #[default]
    Card,
    /// 첫 실행 온보딩 완료 직후 띄우는 토스트 카드.
    OnboardingToast,
}

/// 빈 워크스페이스(랜딩) 알약이 쓰는 별 상태.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RepoStar {
    #[default]
    Loading,
    NotStarred,
    WebFallback,
    Starred,
}

/// 카드 표시 상태(메모리만).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Card {
    Hidden,
    /// 이미 별을 줬는지 `gh`로 확인 중.
    Checking,
    Shown,
    /// 별 주는 중(`gh api -X PUT`).
    Starring,
    Thanks,
}

/// 앱 전역 별 요청 상태. [`init`] 전에는 아무것도 하지 않는다.
#[expect(
    clippy::struct_excessive_bools,
    reason = "독립적인 초기화·조회·랜딩 메뉴·Value Moment·온보딩 상태 플래그"
)]
#[derive(Debug)]
pub struct StarNag {
    dir: PathBuf,
    pub state: State,
    pub card: Card,
    pub mode: Mode,
    pub surface: Surface,
    pub repo_star: RepoStar,
    pub repo_star_starring: bool,
    pub landing_hidden: bool,
    pub landing_menu_open: bool,
    initialized: bool,
    check_in_flight: bool,
    check_requested: bool,
    any_agent_active: bool,
    value_moment_waiting: bool,
    value_moment_preparing: bool,
    value_moment_typed_during_prepare: bool,
    onboarding_completed_requested: bool,
    pending_onboarding_completed: bool,
    pending_save: Option<gpui::Task<()>>,
    pending_value_moment: Option<gpui::Task<()>>,
    startup: tokio::sync::watch::Receiver<bool>,
    #[cfg(test)]
    pub(crate) mock_gh: Option<(Option<bool>, bool)>,
}

impl Global for StarNag {}

fn persist(cx: &mut App) {
    let (dir, state, previous) = {
        let nag = cx.global_mut::<StarNag>();
        if !nag.initialized {
            return;
        }
        (nag.dir.clone(), nag.state.clone(), nag.pending_save.take())
    };
    let task = cx.background_executor().spawn(async move {
        if let Some(previous) = previous {
            previous.await;
        }
        if let Err(error) = save(&dir, &state) {
            tracing::warn!(kind = ?error.kind(), "star-nag save failed");
        }
    });
    cx.global_mut::<StarNag>().pending_save = Some(task);
}

/// 정상 종료 전에 호출하는 비동기 저장 배리어. 이후 저장도 이 순서 뒤에 이어진다.
pub fn flush(cx: &mut App) -> gpui::Task<()> {
    if !cx.has_global::<StarNag>() {
        return gpui::Task::ready(());
    }
    let mut startup = cx.global::<StarNag>().startup.clone();
    cx.spawn(async move |cx| {
        while !*startup.borrow() {
            if startup.changed().await.is_err() {
                break;
            }
        }
        let pending = cx.update(flush_writes);
        pending.await;
    })
}

fn flush_writes(cx: &mut App) -> gpui::Task<()> {
    let previous = cx.global_mut::<StarNag>().pending_save.take();
    let (done, wait) = tokio::sync::oneshot::channel();
    let task = cx.background_executor().spawn(async move {
        if let Some(previous) = previous {
            previous.await;
        }
        done.send(()).ok();
    });
    cx.global_mut::<StarNag>().pending_save = Some(task);
    cx.background_executor().spawn(async move {
        wait.await.ok();
    })
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 앱 시작 때 한 번(메인 앱만). 테스트·`KRILL_TEST_OFFLINE`에서는 부르는 쪽이 임시 디렉터리를 준다.
pub fn init(dir: PathBuf, cx: &mut App) {
    let (loaded, startup) = tokio::sync::watch::channel(false);
    cx.set_global(StarNag {
        dir: dir.clone(),
        state: State::default(),
        card: Card::Hidden,
        mode: Mode::Gh,
        surface: Surface::Card,
        repo_star: RepoStar::Loading,
        repo_star_starring: false,
        landing_hidden: false,
        landing_menu_open: false,
        initialized: false,
        check_in_flight: false,
        check_requested: false,
        any_agent_active: false,
        value_moment_waiting: false,
        value_moment_preparing: false,
        value_moment_typed_during_prepare: false,
        onboarding_completed_requested: false,
        pending_onboarding_completed: false,
        pending_save: None,
        pending_value_moment: None,
        startup,
        #[cfg(test)]
        mock_gh: None,
    });
    cx.spawn(async move |cx| {
        let load_dir = dir.clone();
        let mut state = cx
            .background_executor()
            .spawn(async move { load(&load_dir) })
            .await;
        let version_changed = state.ensure_version(env!("CARGO_PKG_VERSION"));
        cx.update(|cx| {
            let (check_requested, onboarding_requested) = {
                let nag = cx.global_mut::<StarNag>();
                if nag.dir != dir || nag.initialized {
                    return;
                }
                let starts = nag.state.total;
                state.total = state.total.saturating_add(starts);
                nag.state = state;
                nag.initialized = true;
                (
                    nag.check_requested,
                    std::mem::take(&mut nag.onboarding_completed_requested),
                )
            };
            if version_changed || cx.global::<StarNag>().state.total > 0 {
                persist(cx);
            }
            check_due(cx);
            if onboarding_requested {
                onboarding_completed(cx);
            }
            if check_requested {
                ensure_checked(cx);
            }
        });
        loaded.send(true).ok();
    })
    .detach();
    cx.on_app_quit(|cx| {
        let pending = cx.global_mut::<StarNag>().pending_save.take();
        cx.background_executor().spawn(async move {
            if let Some(pending) = pending {
                pending.await;
            }
        })
    })
    .detach();
}

/// 앱에서 에이전트 실행(새 에이전트 탭 또는 세션 Working 전이)이 시작됐다.
/// 문턱을 넘으면 이미 별을 줬는지 확인한 뒤 카드를 띄운다.
pub fn record_agent_start(cx: &mut App) {
    crate::telemetry::record_agent_start(cx);
    if !cx.has_global::<StarNag>() {
        return;
    }
    let nag = cx.global_mut::<StarNag>();
    nag.state.total = nag.state.total.saturating_add(1);
    persist(cx);
    check_due(cx);
}

/// 첫 실행 온보딩 완료 직후 호출한다(Orca `handleStarNagOnboardingCompleted` 패리티).
/// 이미 표시 중인 문턱 카드가 있으면 온보딩 완료 토스트로 대체하고, 별 확인이 진행 중이면 뒤에 이어서 실행한다.
pub fn onboarding_completed(cx: &mut App) {
    if !cx.has_global::<StarNag>() {
        return;
    }
    let resolved = {
        let nag = cx.global_mut::<StarNag>();
        if !nag.initialized {
            nag.onboarding_completed_requested = true;
            return;
        }
        if nag.state.completed || !nag.state.can_show_prompt(now()) {
            return;
        }
        if nag.check_in_flight {
            nag.pending_onboarding_completed = true;
            return;
        }
        if !matches!(nag.card, Card::Hidden | Card::Shown) {
            return;
        }
        nag.value_moment_preparing = false;
        nag.value_moment_typed_during_prepare = false;
        nag.surface = Surface::OnboardingToast;
        nag.card = Card::Checking;
        match nag.repo_star {
            RepoStar::Starred => Some(Some(true)),
            RepoStar::NotStarred | RepoStar::WebFallback | RepoStar::Loading => None,
        }
    };
    if let Some(starred) = resolved {
        apply_star_check_result(starred, cx);
    } else {
        run_star_check(cx);
    }
}

fn is_modifier_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "meta" | "shift" | "control" | "alt" | "capslock" | "fn" | "super" | "hyper"
    )
}

/// 비수식 키 입력을 기록해 Value Moment의 1.2초 정적 대기(quiet window)를 갱신한다(Orca `StarNagAgentValueMomentObserver` 패리티).
pub fn record_key_down(key: &str, cx: &mut App) {
    if is_modifier_key(key) || !cx.has_global::<StarNag>() {
        return;
    }
    let reschedule = {
        let nag = cx.global_mut::<StarNag>();
        if nag.value_moment_preparing {
            nag.value_moment_typed_during_prepare = true;
            false
        } else {
            nag.value_moment_waiting && !nag.any_agent_active
        }
    };
    if reschedule {
        schedule_value_moment_timer(cx);
    }
}

/// 진행 중인 에이전트 완료(Value Moment) 대기 타이머를 취소한다.
pub fn cancel_agent_value_moment(cx: &mut App) {
    if cx.has_global::<StarNag>() {
        let nag = cx.global_mut::<StarNag>();
        nag.pending_value_moment = None;
        nag.value_moment_waiting = false;
        nag.value_moment_preparing = false;
        nag.value_moment_typed_during_prepare = false;
    }
}

/// 에이전트 세션 스냅샷 변화를 관찰해 1회 Value Moment(모든 에이전트가 유휴가 된 뒤 1.2초 정적 유지)를 예약한다.
pub fn observe_agent_activity(any_active: bool, transitioned_to_done: bool, cx: &mut App) {
    if !cx.has_global::<StarNag>() {
        return;
    }
    {
        let nag = cx.global_mut::<StarNag>();
        nag.any_agent_active = any_active;
        if any_active {
            nag.pending_value_moment = None;
            nag.value_moment_waiting = false;
            nag.value_moment_typed_during_prepare = false;
            return;
        }
    }
    if !transitioned_to_done {
        return;
    }
    let consumed_completed = {
        let nag = cx.global_mut::<StarNag>();
        if nag.state.value_moment_consumed()
            || nag.value_moment_waiting
            || nag.value_moment_preparing
        {
            return;
        }
        if nag.state.completed {
            if !nag.state.consume_value_moment(env!("CARGO_PKG_VERSION")) {
                return;
            }
            true
        } else {
            nag.value_moment_waiting = true;
            false
        }
    };
    if consumed_completed {
        persist(cx);
        return;
    }
    schedule_value_moment_timer(cx);
}

fn schedule_value_moment_timer(cx: &mut App) {
    let task = cx.spawn(async move |cx| {
        cx.background_executor().timer(VALUE_MOMENT_DELAY).await;
        cx.update(|cx| {
            if !cx.has_global::<StarNag>() {
                return;
            }
            let action = {
                let nag = cx.global_mut::<StarNag>();
                nag.pending_value_moment = None;
                if !nag.initialized || nag.any_agent_active || !nag.value_moment_waiting {
                    return;
                }
                if nag.state.value_moment_consumed() {
                    nag.value_moment_waiting = false;
                    return;
                }
                if nag.state.completed
                    || !nag.state.can_show_prompt(now())
                    || nag.card != Card::Hidden
                    || nag.check_in_flight
                {
                    nag.value_moment_waiting = false;
                    nag.state.consume_value_moment(env!("CARGO_PKG_VERSION"));
                    None
                } else {
                    nag.value_moment_waiting = false;
                    nag.value_moment_preparing = true;
                    nag.value_moment_typed_during_prepare = false;
                    nag.surface = Surface::Card;
                    nag.card = Card::Checking;
                    Some(match nag.repo_star {
                        RepoStar::Starred => Some(Some(true)),
                        RepoStar::NotStarred | RepoStar::WebFallback | RepoStar::Loading => None,
                    })
                }
            };
            match action {
                None => persist(cx),
                Some(Some(starred)) => apply_star_check_result(starred, cx),
                Some(None) => run_star_check(cx),
            }
        });
    });
    cx.global_mut::<StarNag>().pending_value_moment = Some(task);
}

fn check_due(cx: &mut App) {
    if !cx.has_global::<StarNag>() {
        return;
    }
    let due = {
        let nag = cx.global::<StarNag>();
        nag.initialized && nag.card == Card::Hidden && !nag.check_in_flight && nag.state.due(now())
    };
    if due {
        show_card(Surface::Card, cx);
    }
}

fn show_card(surface: Surface, cx: &mut App) {
    let resolved = {
        let nag = cx.global_mut::<StarNag>();
        if !nag.initialized
            || nag.card != Card::Hidden
            || nag.check_in_flight
            || !nag.state.can_show_prompt(now())
        {
            return;
        }
        nag.surface = surface;
        nag.card = Card::Checking;
        match nag.repo_star {
            RepoStar::Starred => Some(Some(true)),
            // 브라우저에서 별을 줬거나 gh 인증이 바뀌었을 수 있어 요청마다 다시 확인한다.
            RepoStar::NotStarred | RepoStar::WebFallback | RepoStar::Loading => None,
        }
    };
    if let Some(starred) = resolved {
        apply_star_check_result(starred, cx);
    } else {
        run_star_check(cx);
    }
}

/// 랜딩 화면이나 설정 > 일반이 열릴 때 한 번만 백그라운드에서 GitHub 별 상태를 확인한다.
pub fn ensure_checked(cx: &mut App) {
    if !cx.has_global::<StarNag>() {
        return;
    }
    {
        let nag = cx.global_mut::<StarNag>();
        nag.check_requested = true;
        if !nag.initialized || nag.repo_star != RepoStar::Loading || nag.check_in_flight {
            return;
        }
    }
    run_star_check(cx);
}

fn run_star_check(cx: &mut App) {
    let mock = {
        let nag = cx.global_mut::<StarNag>();
        if nag.check_in_flight {
            return;
        }
        nag.check_in_flight = true;
        #[cfg(test)]
        {
            nag.mock_gh.map(|(check, _)| check)
        }
        #[cfg(not(test))]
        {
            None::<Option<bool>>
        }
    };
    cx.spawn(async move |cx| {
        let starred = cx
            .background_executor()
            .spawn(async move {
                if let Some(mock) = mock {
                    mock
                } else {
                    gh_check_starred()
                }
            })
            .await;
        cx.update(|cx| {
            if !cx.has_global::<StarNag>() {
                return;
            }
            cx.global_mut::<StarNag>().check_in_flight = false;
            apply_star_check_result(starred, cx);
        });
    })
    .detach();
}

fn apply_star_check_result(starred: Option<bool>, cx: &mut App) {
    let before = cx.global::<StarNag>().state.clone();
    let mut reschedule_value_moment = false;
    let mut shown = false;
    let run_pending_onboarding = {
        let nag = cx.global_mut::<StarNag>();
        if starred == Some(true) {
            nag.repo_star = RepoStar::Starred;
            nag.mode = Mode::Gh;
            nag.state.complete();
            if nag.value_moment_preparing {
                nag.value_moment_preparing = false;
                nag.value_moment_typed_during_prepare = false;
                nag.state.consume_value_moment(env!("CARGO_PKG_VERSION"));
            }
            if nag.card == Card::Checking {
                nag.card = Card::Hidden;
            }
        } else {
            if starred == Some(false) {
                nag.repo_star = if nag.state.completed {
                    RepoStar::Starred
                } else {
                    RepoStar::NotStarred
                };
                nag.mode = Mode::Gh;
            } else {
                nag.repo_star = RepoStar::WebFallback;
                nag.mode = Mode::Web;
            }
            if nag.card == Card::Checking {
                if nag.value_moment_preparing {
                    nag.value_moment_preparing = false;
                    let typed = std::mem::take(&mut nag.value_moment_typed_during_prepare);
                    if nag.state.completed || !nag.state.can_show_prompt(now()) {
                        nag.state.consume_value_moment(env!("CARGO_PKG_VERSION"));
                        nag.card = Card::Hidden;
                    } else if nag.any_agent_active {
                        nag.card = Card::Hidden;
                    } else if typed {
                        nag.card = Card::Hidden;
                        nag.value_moment_waiting = true;
                        reschedule_value_moment = true;
                    } else {
                        nag.state.consume_value_moment(env!("CARGO_PKG_VERSION"));
                        nag.card = Card::Shown;
                        shown = true;
                    }
                } else {
                    nag.card = if nag.state.completed || !nag.state.can_show_prompt(now()) {
                        Card::Hidden
                    } else {
                        shown = true;
                        Card::Shown
                    };
                }
            }
        }
        std::mem::take(&mut nag.pending_onboarding_completed)
    };
    if shown {
        crate::telemetry::record_star_prompt(
            krill_proto::telemetry::UsageName::StarPromptShown,
            cx,
        );
    }
    if cx.global::<StarNag>().state != before {
        persist(cx);
    }
    if reschedule_value_moment {
        schedule_value_moment_timer(cx);
    }
    if run_pending_onboarding {
        onboarding_completed(cx);
    }
    cx.refresh_windows();
}

fn update(cx: &mut App, f: impl FnOnce(&mut StarNag)) {
    if !cx.has_global::<StarNag>() {
        return;
    }
    let nag = cx.global_mut::<StarNag>();
    let before = nag.state.clone();
    f(nag);
    if nag.state != before {
        persist(cx);
    }
    cx.refresh_windows();
}

/// `[나중에]`·닫기 X·Escape·웹 열기.
pub fn later(cx: &mut App) {
    let mut deferred = false;
    update(cx, |nag| {
        if nag.card == Card::Thanks {
            nag.card = Card::Hidden;
            return;
        }
        deferred = true;
        nag.state.defer(now());
        nag.card = Card::Hidden;
    });
    if deferred {
        crate::telemetry::record_star_prompt(
            krill_proto::telemetry::UsageName::StarPromptLater,
            cx,
        );
    }
}

/// 카드가 떠 있을 때 Escape 키로 닫는다(`later` 처리). 닫았으면 `true`.
pub fn dismiss_on_escape(cx: &mut App) -> bool {
    if cx
        .try_global::<StarNag>()
        .is_some_and(|n| matches!(n.card, Card::Shown | Card::Thanks))
    {
        later(cx);
        true
    } else {
        false
    }
}

/// 영구 완료 처리(별 성공·이미 별을 준 상태).
pub fn never(cx: &mut App) {
    update(cx, |nag| {
        nag.state.complete();
        nag.card = Card::Hidden;
    });
    crate::telemetry::record_star_prompt(
        krill_proto::telemetry::UsageName::StarPromptDismissed,
        cx,
    );
}

/// 카드의 주 버튼 동작:
/// - `Mode::Gh`: `gh api -X PUT`으로 바로 별을 주고, 실패하면 브라우저를 강제로 열지 않고 카드를 `Mode::Web`(`GitHub 열기`)으로 전환한다(Orca `StarNagCard` 패리티).
/// - `Mode::Web`: 브라우저에서 저장소를 열고 3일 쉼(`later`)으로 전환한다.
pub fn star(cx: &mut App) {
    let Some(mode) = cx
        .try_global::<StarNag>()
        .and_then(|n| (n.card == Card::Shown).then_some(n.mode))
    else {
        return;
    };
    if mode == Mode::Web {
        cx.open_url(REPO_URL);
        update(cx, |nag| {
            nag.state.defer(now());
            nag.card = Card::Hidden;
        });
        crate::telemetry::record_star_prompt(
            krill_proto::telemetry::UsageName::StarPromptStarred,
            cx,
        );
        return;
    }
    let mock_put = {
        #[cfg(test)]
        {
            cx.global::<StarNag>().mock_gh.map(|(_, put)| put)
        }
        #[cfg(not(test))]
        {
            None::<bool>
        }
    };
    update(cx, |nag| nag.card = Card::Starring);
    cx.spawn(async move |cx| {
        let ok = cx
            .background_executor()
            .spawn(async move {
                if let Some(put) = mock_put {
                    put
                } else {
                    gh(&["api", "-X", "PUT", STARRED_API])
                }
            })
            .await;
        cx.update(|cx| {
            if ok {
                update(cx, |nag| {
                    nag.state.complete();
                    nag.repo_star = RepoStar::Starred;
                    nag.card = Card::Thanks;
                });
                crate::telemetry::record_star_prompt(
                    krill_proto::telemetry::UsageName::StarPromptStarred,
                    cx,
                );
            } else {
                update(cx, |nag| {
                    nag.mode = Mode::Web;
                    nag.repo_star = RepoStar::WebFallback;
                    nag.card = Card::Shown;
                });
            }
        });
        if ok {
            cx.background_executor().timer(THANKS_FOR).await;
            cx.update(|cx| {
                update(cx, |nag| {
                    if nag.card == Card::Thanks {
                        nag.card = Card::Hidden;
                    }
                });
            });
        }
    })
    .detach();
}

/// 빈 워크스페이스(랜딩) 알약 버튼 또는 설정 > 일반 'Krill 지원' 섹션에서 별 주기 클릭.
pub fn star_from_surface(cx: &mut App) {
    if !cx.has_global::<StarNag>() {
        cx.open_url(REPO_URL);
        return;
    }
    let (repo_star, starring) = {
        let nag = cx.global::<StarNag>();
        (nag.repo_star, nag.repo_star_starring)
    };
    if starring {
        return;
    }
    match repo_star {
        RepoStar::WebFallback => {
            update(cx, |nag| nag.landing_menu_open = false);
            cx.open_url(REPO_URL);
            crate::telemetry::record_star_prompt(
                krill_proto::telemetry::UsageName::StarPromptStarred,
                cx,
            );
        }
        RepoStar::Starred => {
            update(cx, |nag| nag.landing_menu_open = !nag.landing_menu_open);
        }
        RepoStar::Loading | RepoStar::NotStarred => {
            let mock_put = {
                #[cfg(test)]
                {
                    cx.global::<StarNag>().mock_gh.map(|(_, put)| put)
                }
                #[cfg(not(test))]
                {
                    None::<bool>
                }
            };
            update(cx, |nag| {
                nag.repo_star_starring = true;
                nag.landing_menu_open = false;
            });
            cx.spawn(async move |cx| {
                let ok = cx
                    .background_executor()
                    .spawn(async move {
                        if let Some(put) = mock_put {
                            put
                        } else {
                            gh(&["api", "-X", "PUT", STARRED_API])
                        }
                    })
                    .await;
                cx.update(|cx| {
                    update(cx, |nag| {
                        nag.repo_star_starring = false;
                        if ok {
                            nag.repo_star = RepoStar::Starred;
                            nag.state.complete();
                            if matches!(nag.card, Card::Checking | Card::Shown) {
                                nag.card = Card::Hidden;
                            }
                        } else {
                            nag.repo_star = RepoStar::WebFallback;
                            nag.mode = Mode::Web;
                        }
                    });
                    if ok {
                        crate::telemetry::record_star_prompt(
                            krill_proto::telemetry::UsageName::StarPromptStarred,
                            cx,
                        );
                    }
                });
            })
            .detach();
        }
    }
}

/// `gh api --include user/starred/...` 출력을 3상태(`Some(true)` = 이미 별 줌, `Some(false)` = 아직 안 줌,
/// `None` = `gh` 미설치·미인증·오프라인)로 판정한다.
#[must_use]
pub(crate) fn parse_gh_starred_output(success: bool, stdout: &[u8], stderr: &[u8]) -> Option<bool> {
    if success {
        return Some(true);
    }
    let out = String::from_utf8_lossy(stdout);
    let err = String::from_utf8_lossy(stderr);
    let combined = format!("{out}\n{err}");
    for line in combined.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("HTTP/")
            .or_else(|| trimmed.strip_prefix("http/"))
            && let Some((_, status_part)) = rest.split_once(char::is_whitespace)
        {
            let code = status_part.split_whitespace().next().unwrap_or_default();
            if code == "200" || code == "204" {
                return Some(true);
            }
            if code == "404" {
                return Some(false);
            }
        }
    }
    if combined.contains("HTTP 404") || combined.contains("404 Not Found") {
        return Some(false);
    }
    None
}

fn gh_check_starred() -> Option<bool> {
    if cfg!(test) || std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        return None;
    }
    let exe =
        crate::pull_request::find_executable("gh", &["/opt/homebrew/bin/gh", "/usr/local/bin/gh"]);
    let mut cmd = crate::platform::background_command(exe);
    cmd.args(["api", "--include", STARRED_API])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    let out = crate::settings::child::output(&mut cmd, GH_TIMEOUT, 64 * 1024).ok()?;
    parse_gh_starred_output(out.status.success(), &out.stdout, &out.stderr)
}

/// `gh <args>`가 0으로 끝났는가. 테스트·`KRILL_TEST_OFFLINE`에서는 실행하지 않고 `false`.
fn gh(args: &[&str]) -> bool {
    if cfg!(test) || std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        return false;
    }
    let exe =
        crate::pull_request::find_executable("gh", &["/opt/homebrew/bin/gh", "/usr/local/bin/gh"]);
    let mut cmd = crate::platform::background_command(exe);
    cmd.args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    crate::pull_request::execute(&mut cmd, GH_TIMEOUT).is_ok()
}

/// 메인 창 우상단 카드/토스트(Orca `StarNagCard.tsx` · `StarNagToastHost.tsx` 패리티). 숨김 상태면 `None`.
pub fn card(cx: &App) -> Option<AnyElement> {
    let nag = cx.try_global::<StarNag>()?;
    let card = nag.card;
    let mode = nag.mode;
    let surface = nag.surface;
    if matches!(card, Card::Hidden | Card::Checking) {
        return None;
    }
    let p = crate::ui::palette(cx);
    let shell = div()
        .id("star-nag-card")
        .debug_selector(|| "star-nag-card".into())
        .occlude()
        .on_key_down(|e: &gpui::KeyDownEvent, _, cx| {
            if e.keystroke.key == "escape" {
                cx.stop_propagation();
                later(cx);
            }
        })
        .absolute()
        .top(crate::ui::TITLEBAR_HEIGHT + crate::ui::SPACE_4)
        .right(crate::ui::SPACE_3)
        .w(px(300.))
        .p_3()
        .rounded(crate::ui::RADIUS_MD)
        .border_1()
        .border_color(p.border)
        .bg(p.popover)
        .font_family(crate::ui::FONT_UI)
        .text_size(crate::ui::TEXT_SM)
        .shadow_lg()
        .flex()
        .flex_col()
        .gap_2p5();
    if card == Card::Thanks && surface == Surface::Card {
        return Some(
            shell
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(p.popover_foreground)
                        .child(Icon::new(IconName::Star).small().text_color(p.warning))
                        .child(text::THANKS.get()),
                )
                .into_any_element(),
        );
    }
    let starring = card == Card::Starring;
    let (cta_label, cta_icon) = if starring {
        (text::STARRING.get(), IconName::Star)
    } else if mode == Mode::Web {
        (text::OPEN_GITHUB.get(), IconName::ExternalLink)
    } else {
        (text::STAR.get(), IconName::Star)
    };
    let (header_icon, title, body) = match surface {
        Surface::Card => (
            Icon::new(IconName::Star)
                .small()
                .text_color(p.warning)
                .into_any_element(),
            text::TITLE.get(),
            text::BODY.get(),
        ),
        Surface::OnboardingToast => {
            let badge = if card == Card::Thanks {
                div()
                    .size(px(20.))
                    .rounded_full()
                    .bg(p.warning.opacity(0.14))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::Star).xsmall().text_color(p.warning))
            } else {
                div()
                    .size(px(20.))
                    .rounded_full()
                    .bg(p.success.opacity(0.14))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::Check).xsmall().text_color(p.success))
            };
            (
                badge.into_any_element(),
                text::ONBOARDING_TITLE.get(),
                text::ONBOARDING_BODY.get(),
            )
        }
    };
    let shell = shell.child(
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(header_icon)
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_SM)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.popover_foreground)
                            .child(title),
                    ),
            )
            .child(
                Button::new("star-nag-close")
                    .icon(IconName::Close)
                    .debug_selector(|| "star-nag-close".into())
                    .ghost()
                    .xsmall()
                    .disabled(starring)
                    .on_click(|_, _, cx| later(cx)),
            ),
    );
    if card == Card::Thanks {
        return Some(
            shell
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(p.warning)
                        .child(text::THANKS.get()),
                )
                .into_any_element(),
        );
    }
    Some(
        shell
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div().flex_1().child(
                            crate::ui::primary("star-nag-star", cta_label)
                                .icon(cta_icon)
                                .debug_selector(|| "star-nag-star".into())
                                .loading(starring)
                                .w_full()
                                .on_click(|_, _, cx| star(cx)),
                        ),
                    )
                    .child(
                        Button::new("star-nag-later")
                            .label(text::LATER.get())
                            .debug_selector(|| "star-nag-later".into())
                            .small()
                            .outline()
                            .disabled(starring)
                            .on_click(|_, _, cx| later(cx)),
                    ),
            )
            .into_any_element(),
    )
}

/// 빈 워크스페이스(랜딩) 하단 중앙의 GitHub 별 알약 버튼(Orca `Landing.tsx` 패리티).
/// 프로젝트가 이미 열려 있고 별도 준 상태(`Starred && has_projects`)거나 사용자가 숨긴 경우 `None`.
pub fn landing_star_pill(has_projects: bool, cx: &App) -> Option<AnyElement> {
    let nag = cx.try_global::<StarNag>()?;
    if nag.landing_hidden
        || nag.repo_star == RepoStar::Loading
        || (nag.repo_star == RepoStar::Starred && has_projects)
    {
        return None;
    }
    let p = crate::ui::palette(cx);
    let repo_star = nag.repo_star;
    let starring = nag.repo_star_starring;
    let menu_open = nag.landing_menu_open;
    let (icon_name, icon_color, label, text_color, border_color, bg_color, hover_bg) =
        match repo_star {
            RepoStar::Starred => (
                IconName::Star,
                p.warning,
                text::STARRED_ON_GITHUB.get(),
                p.warning,
                p.warning.opacity(0.30),
                p.warning.opacity(0.08),
                p.warning.opacity(0.14),
            ),
            RepoStar::WebFallback => (
                IconName::ExternalLink,
                p.muted_foreground,
                text::OPEN_GITHUB.get(),
                p.muted_foreground,
                p.border,
                p.secondary,
                p.accent,
            ),
            RepoStar::Loading | RepoStar::NotStarred => (
                IconName::Star,
                p.warning,
                if starring {
                    text::STARRING.get()
                } else {
                    text::STAR.get()
                },
                p.warning,
                p.warning.opacity(0.30),
                p.warning.opacity(0.06),
                p.warning.opacity(0.12),
            ),
        };

    let hide_menu = menu_open.then(|| {
        div()
            .id("landing-star-menu")
            .debug_selector(|| "landing-star-menu".into())
            .occlude()
            .absolute()
            .bottom(px(36.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .rounded(crate::ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.popover)
                    .p_1()
                    .shadow_lg()
                    .child(
                        crate::ui::ghost("landing-star-hide", text::HIDE.get())
                            .debug_selector(|| "landing-star-hide".into())
                            .on_click(|_, _, cx| {
                                update(cx, |nag| {
                                    nag.landing_hidden = true;
                                    nag.landing_menu_open = false;
                                });
                            }),
                    ),
            )
    });

    Some(
        div()
            .absolute()
            .bottom(px(24.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div().relative().children(hide_menu).child(
                    div()
                        .id("landing-star-pill")
                        .debug_selector(|| "landing-star-pill".into())
                        .cursor_pointer()
                        .rounded_full()
                        .border_1()
                        .border_color(border_color)
                        .bg(bg_color)
                        .hover(move |s| s.bg(hover_bg))
                        .px_3()
                        .py_1p5()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(text_color)
                        .on_mouse_down(MouseButton::Right, |_, _, cx| {
                            cx.stop_propagation();
                            update(cx, |nag| {
                                if nag.repo_star == RepoStar::Starred {
                                    nag.landing_menu_open = !nag.landing_menu_open;
                                }
                            });
                        })
                        .on_click(|_, _, cx| star_from_surface(cx))
                        .child(Icon::new(icon_name).xsmall().text_color(icon_color))
                        .child(label),
                ),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests;
