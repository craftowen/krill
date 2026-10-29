//! GitHub 별 요청 카드·랜딩 알약 버튼·일반 설정 지원 섹션(OWE-905).
//! 앱에서 에이전트 세션을 시작한 횟수가 문턱을 넘거나 버전당 첫 에이전트 작업 완료(Value Moment)에 도달하면
//! 메인 창 우하단에 카드를 띄우고, 빈 워크스페이스 하단과 설정 > 일반 하단에서도 직접 별을 줄 수 있게 한다.
//! 텔레메트리 없음 — 상태는 `star-nag.toml` 하나다.
//!
//! Logic and UI adapted from Orca (`src/main/star-nag/*`, `src/main/github/client/fetch/orca-star.ts`,
//! `src/renderer/src/components/StarNagCard.tsx`, `Landing.tsx`, `settings/GeneralSupportSection.tsx`,
//! MIT License — Copyright (c) 2026 Orca Contributors):
//! 버전별 기준점, 첫 문턱 35, 버전당 1회 에이전트 완료 트리거, `[나중에]`·닫기·웹 열기 = 문턱 2배 + 3일 쉼,
//! `[다시 보지 않기]`·별 성공·이미 별을 준 사용자 = 영구 완료.

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
    /// 버전이 바뀌면 지금 총계를 기준점으로 다시 센다(완료는 유지). 바뀌었으면 `true`.
    pub fn ensure_version(&mut self, version: &str) -> bool {
        if self.app_version == version {
            return false;
        }
        version.clone_into(&mut self.app_version);
        self.baseline = self.total;
        self.next_threshold = INITIAL_THRESHOLD;
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
        self.can_show_prompt(now) && self.total.saturating_sub(self.baseline) >= self.next_threshold
    }

    /// `[나중에]`·닫기·웹 열기: 문턱 2배, 지금부터 다시 세고, 3일 쉰다.
    pub fn defer(&mut self, now: u64) {
        self.next_threshold = self.next_threshold.max(1).saturating_mul(2);
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

/// 빈 워크스페이스(랜딩)와 설정 > 일반 'Krill 지원' 섹션이 공유하는 별 상태.
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
    reason = "독립적인 초기화·조회·랜딩 메뉴·별 진행 상태 플래그"
)]
#[derive(Debug)]
pub struct StarNag {
    dir: PathBuf,
    pub state: State,
    pub card: Card,
    pub mode: Mode,
    pub repo_star: RepoStar,
    pub repo_star_starring: bool,
    pub landing_hidden: bool,
    pub landing_menu_open: bool,
    initialized: bool,
    check_in_flight: bool,
    check_requested: bool,
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
        repo_star: RepoStar::Loading,
        repo_star_starring: false,
        landing_hidden: false,
        landing_menu_open: false,
        initialized: false,
        check_in_flight: false,
        check_requested: false,
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
            let check_requested = {
                let nag = cx.global_mut::<StarNag>();
                if nag.dir != dir || nag.initialized {
                    return;
                }
                let starts = nag.state.total;
                state.total = state.total.saturating_add(starts);
                nag.state = state;
                nag.initialized = true;
                nag.check_requested
            };
            if version_changed || cx.global::<StarNag>().state.total > 0 {
                persist(cx);
            }
            check_due(cx);
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

/// 앱에서 에이전트 세션을 하나 시작했다. 문턱을 넘으면 이미 별을 줬는지 확인한 뒤 카드를 띄운다.
pub fn record_agent_start(cx: &mut App) {
    if !cx.has_global::<StarNag>() {
        return;
    }
    let nag = cx.global_mut::<StarNag>();
    nag.state.total = nag.state.total.saturating_add(1);
    persist(cx);
    check_due(cx);
}

/// 진행 중인 에이전트 완료(Value Moment) 대기 타이머를 취소한다.
pub fn cancel_agent_value_moment(cx: &mut App) {
    if cx.has_global::<StarNag>() {
        cx.global_mut::<StarNag>().pending_value_moment = None;
    }
}

/// 에이전트 세션 스냅샷 변화를 관찰해 버전당 1회 Value Moment(모든 에이전트가 유휴가 된 뒤 1.2초)를 예약한다.
pub fn observe_agent_activity(any_active: bool, transitioned_to_done: bool, cx: &mut App) {
    if !cx.has_global::<StarNag>() {
        return;
    }
    if any_active {
        cx.global_mut::<StarNag>().pending_value_moment = None;
        return;
    }
    if !transitioned_to_done {
        return;
    }
    {
        let nag = cx.global_mut::<StarNag>();
        if nag.state.completed
            || nag.state.agent_value_moment_app_version == env!("CARGO_PKG_VERSION")
            || nag.pending_value_moment.is_some()
        {
            return;
        }
    }
    let task = cx.spawn(async move |cx| {
        cx.background_executor().timer(VALUE_MOMENT_DELAY).await;
        cx.update(|cx| {
            if !cx.has_global::<StarNag>() {
                return;
            }
            let should_show = {
                let nag = cx.global_mut::<StarNag>();
                nag.pending_value_moment = None;
                if !nag.initialized
                    || nag.state.agent_value_moment_app_version == env!("CARGO_PKG_VERSION")
                {
                    return;
                }
                env!("CARGO_PKG_VERSION").clone_into(&mut nag.state.agent_value_moment_app_version);
                nag.card == Card::Hidden && nag.state.can_show_prompt(now())
            };
            persist(cx);
            if should_show {
                show_card(cx);
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
        nag.initialized && nag.card == Card::Hidden && nag.state.due(now())
    };
    if due {
        show_card(cx);
    }
}

fn show_card(cx: &mut App) {
    let resolved = {
        let nag = cx.global_mut::<StarNag>();
        if !nag.initialized || nag.card != Card::Hidden || !nag.state.can_show_prompt(now()) {
            return;
        }
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
    {
        let nag = cx.global_mut::<StarNag>();
        match starred {
            Some(true) => {
                nag.repo_star = RepoStar::Starred;
                nag.mode = Mode::Gh;
                nag.state.complete();
                if nag.card == Card::Checking {
                    nag.card = Card::Hidden;
                }
            }
            Some(false) => {
                nag.repo_star = if nag.state.completed {
                    RepoStar::Starred
                } else {
                    RepoStar::NotStarred
                };
                nag.mode = Mode::Gh;
                if nag.card == Card::Checking {
                    nag.card = if nag.state.completed {
                        Card::Hidden
                    } else {
                        Card::Shown
                    };
                }
            }
            None => {
                nag.repo_star = RepoStar::WebFallback;
                nag.mode = Mode::Web;
                if nag.card == Card::Checking {
                    nag.card = if nag.state.completed {
                        Card::Hidden
                    } else {
                        Card::Shown
                    };
                }
            }
        }
    }
    if cx.global::<StarNag>().state != before {
        persist(cx);
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
    update(cx, |nag| {
        nag.state.defer(now());
        nag.card = Card::Hidden;
    });
}

/// `[다시 보지 않기]`.
pub fn never(cx: &mut App) {
    update(cx, |nag| {
        nag.state.complete();
        nag.card = Card::Hidden;
    });
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
        later(cx);
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

/// 메인 창 우상단 카드. 숨김 상태면 `None`.
pub fn card(cx: &App) -> Option<AnyElement> {
    let nag = cx.try_global::<StarNag>()?;
    let card = nag.card;
    let mode = nag.mode;
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
    if card == Card::Thanks {
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
    Some(
        shell
            .child(
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
                            .child(Icon::new(IconName::Star).small().text_color(p.warning))
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_SM)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(p.popover_foreground)
                                    .child(text::TITLE.get()),
                            ),
                    )
                    .child(
                        Button::new("star-nag-close")
                            .icon(IconName::Close)
                            .debug_selector(|| "star-nag-close".into())
                            .ghost()
                            .xsmall()
                            .on_click(|_, _, cx| later(cx)),
                    ),
            )
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::BODY.get()),
            )
            .child(
                crate::ui::primary("star-nag-star", cta_label)
                    .icon(cta_icon)
                    .debug_selector(|| "star-nag-star".into())
                    .loading(starring)
                    .w_full()
                    .on_click(|_, _, cx| star(cx)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_1()
                    .child(
                        crate::ui::ghost("star-nag-later", text::LATER.get())
                            .debug_selector(|| "star-nag-later".into())
                            .on_click(|_, _, cx| later(cx)),
                    )
                    .child(
                        crate::ui::ghost("star-nag-never", text::NEVER.get())
                            .debug_selector(|| "star-nag-never".into())
                            .on_click(|_, _, cx| never(cx)),
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
    let (icon_name, icon_color, label, text_color) = match repo_star {
        RepoStar::Starred => (
            IconName::Star,
            p.warning,
            text::STARRED_ON_GITHUB.get(),
            p.foreground,
        ),
        RepoStar::WebFallback => (
            IconName::ExternalLink,
            p.muted_foreground,
            text::OPEN_GITHUB.get(),
            p.muted_foreground,
        ),
        RepoStar::Loading | RepoStar::NotStarred => (
            IconName::Star,
            p.warning,
            if starring {
                text::STARRING.get()
            } else {
                text::STAR.get()
            },
            p.muted_foreground,
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
                        .border_color(p.border)
                        .bg(p.secondary)
                        .hover(|s| s.bg(p.accent))
                        .px_3()
                        .py_1p5()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(text_color)
                        .on_mouse_down(MouseButton::Right, |_, _, cx| {
                            cx.stop_propagation();
                            update(cx, |nag| nag.landing_menu_open = !nag.landing_menu_open);
                        })
                        .on_click(|_, _, cx| star_from_surface(cx))
                        .child(Icon::new(icon_name).xsmall().text_color(icon_color))
                        .child(label),
                ),
            )
            .into_any_element(),
    )
}

/// 설정 > 일반 > 'Krill 지원' 행 우측 컨트롤(Orca `GeneralSupportSection.tsx` 패리티).
pub fn settings_support_control(cx: &App) -> AnyElement {
    let p = crate::ui::palette(cx);
    let (repo_star, starring) = cx
        .try_global::<StarNag>()
        .map_or((RepoStar::WebFallback, false), |nag| {
            (nag.repo_star, nag.repo_star_starring)
        });
    match repo_star {
        RepoStar::Starred => div()
            .id("settings-star-thanks")
            .debug_selector(|| "settings-star-thanks".into())
            .flex()
            .items_center()
            .gap_1p5()
            .text_size(crate::ui::TEXT_SM)
            .text_color(p.muted_foreground)
            .child(Icon::new(IconName::Star).small().text_color(p.warning))
            .child(text::SUPPORT_THANKS.get())
            .into_any_element(),
        RepoStar::WebFallback => Button::new("settings-star-github")
            .label(text::OPEN_GITHUB.get())
            .icon(IconName::ExternalLink)
            .small()
            .outline()
            .debug_selector(|| "settings-star-github".into())
            .on_click(|_, _, cx| star_from_surface(cx))
            .into_any_element(),
        RepoStar::Loading | RepoStar::NotStarred => crate::ui::button_with_reserved_label(
            "settings-star-github",
            text::STAR_SHORT.get(),
            text::STARRING.get(),
            starring,
        )
        .icon(Icon::new(IconName::Star).text_color(p.warning))
        .small()
        .outline()
        .disabled(repo_star == RepoStar::Loading || starring)
        .debug_selector(|| "settings-star-github".into())
        .on_click(|_, _, cx| star_from_surface(cx))
        .into_any_element(),
    }
}

#[cfg(test)]
mod tests;
