//! GitHub 별 요청 카드(OWE-905). 앱에서 에이전트 세션을 시작한 횟수가 문턱을 넘으면 메인 창 우하단에
//! 카드를 한 번 띄운다. 텔레메트리 없음 — 상태는 `star-nag.toml` 하나다.
//!
//! Logic adapted from Orca (`src/main/star-nag/*`, MIT License — Copyright (c) 2026 Orca Contributors):
//! 버전별 기준점, 첫 문턱 35, `[나중에]`·닫기·웹 열기 = 문턱 2배 + 3일 쉼, `[다시 보지 않기]`·별 성공·
//! 이미 별을 준 사용자 = 영구 완료.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui::{AnyElement, App, Global, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{IconName, Sizable as _};
use serde::{Deserialize, Serialize};

use crate::i18n::catalog::star_nag as text;
use crate::onboarding::state::atomic_write_0600;

/// 상태 파일 이름(상태 디렉터리 바로 아래).
pub const FILE: &str = "star-nag.toml";
pub const INITIAL_THRESHOLD: u64 = 35;
pub const COOLDOWN_SECS: u64 = 3 * 24 * 60 * 60;
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

    /// 카드를 띄울 때인가.
    #[must_use]
    pub fn due(&self, now: u64) -> bool {
        !self.completed
            && now >= self.deferred_until
            && self.total.saturating_sub(self.baseline) >= self.next_threshold
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
#[derive(Debug)]
pub struct StarNag {
    dir: PathBuf,
    pub state: State,
    pub card: Card,
    initialized: bool,
    pending_save: Option<gpui::Task<()>>,
    startup: tokio::sync::watch::Receiver<bool>,
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
        initialized: false,
        pending_save: None,
        startup,
    });
    cx.spawn(async move |cx| {
        let load_dir = dir.clone();
        let mut state = cx
            .background_executor()
            .spawn(async move { load(&load_dir) })
            .await;
        let version_changed = state.ensure_version(env!("CARGO_PKG_VERSION"));
        cx.update(|cx| {
            let nag = cx.global_mut::<StarNag>();
            if nag.dir != dir || nag.initialized {
                return;
            }
            let starts = nag.state.total;
            state.total = state.total.saturating_add(starts);
            nag.state = state;
            nag.initialized = true;
            if version_changed || starts > 0 {
                persist(cx);
            }
            check_due(cx);
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

fn check_due(cx: &mut App) {
    let nag = cx.global_mut::<StarNag>();
    if !nag.initialized || nag.card != Card::Hidden || !nag.state.due(now()) {
        return;
    }
    nag.card = Card::Checking;
    cx.spawn(async move |cx| {
        let starred = cx
            .background_executor()
            .spawn(async { gh(&["api", STARRED_API]) })
            .await;
        cx.update(|cx| {
            let nag = cx.global_mut::<StarNag>();
            if nag.card != Card::Checking {
                return;
            }
            if starred || nag.state.completed {
                nag.state.complete();
                nag.card = Card::Hidden;
            } else {
                nag.card = Card::Shown;
            }
            persist(cx);
            cx.refresh_windows();
        });
    })
    .detach();
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

/// `[나중에]`·닫기 X.
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

/// `[GitHub에 별 주기]`: `gh`로 바로 별을 주고, 안 되면(미설치·미인증·실패) 브라우저로 넘기고 쉰다.
pub fn star(cx: &mut App) {
    if cx
        .try_global::<StarNag>()
        .is_none_or(|n| n.card != Card::Shown)
    {
        return;
    }
    update(cx, |nag| nag.card = Card::Starring);
    cx.spawn(async move |cx| {
        let ok = cx
            .background_executor()
            .spawn(async { gh(&["api", "-X", "PUT", STARRED_API]) })
            .await;
        cx.update(|cx| {
            if ok {
                update(cx, |nag| {
                    nag.state.complete();
                    nag.card = Card::Thanks;
                });
            } else {
                cx.open_url(REPO_URL);
                later(cx);
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

/// 메인 창 우하단 카드. 숨김 상태면 `None`.
pub fn card(cx: &App) -> Option<AnyElement> {
    let card = cx.try_global::<StarNag>()?.card;
    if matches!(card, Card::Hidden | Card::Checking) {
        return None;
    }
    let p = crate::ui::palette(cx);
    let shell = div()
        .id("star-nag-card")
        .debug_selector(|| "star-nag-card".into())
        .occlude()
        .absolute()
        .bottom(px(34.))
        .right(px(12.))
        .w(px(300.))
        .p_3()
        .rounded(crate::ui::RADIUS_MD)
        .border_1()
        .border_color(p.border)
        .bg(p.popover)
        .shadow_lg()
        .flex()
        .flex_col()
        .gap_2p5();
    if card == Card::Thanks {
        return Some(
            shell
                .child(
                    div()
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(p.popover_foreground)
                        .child(text::THANKS.get()),
                )
                .into_any_element(),
        );
    }
    let starring = card == Card::Starring;
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
                            .text_size(crate::ui::TEXT_SM)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.popover_foreground)
                            .child(text::TITLE.get()),
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
                crate::ui::primary(
                    "star-nag-star",
                    if starring {
                        text::STARRING.get()
                    } else {
                        text::STAR.get()
                    },
                )
                .icon(IconName::Star)
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

#[cfg(test)]
mod tests;
