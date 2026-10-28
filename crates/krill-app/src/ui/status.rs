//! OWE-662: 에이전트 상태 글리프·미확인 벨·워크트리 상태 묶음. 탭·사이드바 세션 행·워크트리 행이 같이 쓴다.
//! OWE-707: 세션별 사용 계정 배지(라벨 축약·툴팁 전체 이름). 원 세션 PTY는 절대 닫지 않는다.
//!
//! 크기·순서·묶음 규칙은 Orca(`.ref/orca`, Copyright (c) 2026 Lovecast Inc., MIT License)의
//! `AgentStateDot.tsx`·`AgentWorkingSpinner.tsx`·`worktree-card-compact-agents.tsx`를 참고했다(코드 복사 없음).
//! 계정 배지 모양만 Orca `CodexRestartChip.tsx:140-155`의 칩 의도를 따르되,
//! 이어가기는 OWE-699(대화 기록 공유, 오웬 결정 대기)까지 숨긴다.

use std::{cell::RefCell, rc::Rc, time::Duration};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, ContentMask, Hsla,
    InteractiveElement as _, IntoElement as _, ParentElement as _, Pixels, Styled as _,
    Transformation, canvas, div, percentage, prelude::FluentBuilder as _, px,
};
use gpui_component::{Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::control::AgentState;

use super::{
    FONT_MONO, RADIUS_XS, STATE_DOT, TEXT_2XS, TEXT_XS, agent_state_color, icons, palette,
    theme::{AGENT_UNREAD, STATUS_GROUP_GAP, STATUS_GROUP_ICON},
};

/// 작업 중 스피너: 120° 간격 3분절 아크(크릴 등갑 모티프, 분절마다 80° + 틈 40°). 선 굵기는 6px 링에서 읽히게 굵게 잡았다.
const RING_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="7" xmlns="http://www.w3.org/2000/svg"><path d="M14.91 4.01A8.5 8.5 0 0 1 20.37 13.48"/><path d="M17.46 18.51A8.5 8.5 0 0 1 6.54 18.51"/><path d="M3.63 13.48A8.5 8.5 0 0 1 9.09 4.01"/></svg>"#;
/// 접근성 감속(reduce motion)에서는 멈춘 분절 아크가 고장처럼 보여 닫힌 링을 그린다(Orca와 같은 의도).
const RING_FULL_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="7" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="8.5"/></svg>"#;
/// Lucide `bell`(ISC License)을 채운 모양. 미확인 완료 표시.
const BELL_FILLED_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M10.268 21a2 2 0 0 0 3.464 0"/><path d="M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326"/></svg>"#;

/// 1초에 한 바퀴, 12스텝. 모든 스피너가 앱 공용 시계(`repeat_synced`)를 따라 같은 위상으로 돈다.
const SPIN_STEPS: f32 = 12.;

/// 떼 미터 점 최대 수와 점 지름·간격·`+N` 칸 너비(px). 폭은 늘 이 값으로 예약한다(STYLEGUIDE §7.3).
pub const SWARM_MAX: usize = 12;
const SWARM_DOT: f32 = 3.;
const SWARM_GAP: f32 = 2.;
const SWARM_MORE_WIDTH: f32 = 22.;

/// 워크트리 행에 보이는 상태 묶음 수와 묶음마다 나란히 그리는 에이전트 아이콘 수.
pub const MAX_GROUPS: usize = 3;
pub const MAX_GROUP_ICONS: usize = 3;

/// 상태 하나를 그리는 모양.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Spinner,
    Question,
    Check,
    Dot,
}

/// 상태 → 글리프. 대기·승인 대기는 같은 질문 아이콘이고 색으로 구분한다.
#[must_use]
pub const fn glyph(state: AgentState) -> Glyph {
    match state {
        AgentState::Working => Glyph::Spinner,
        AgentState::Waiting | AgentState::Blocked => Glyph::Question,
        AgentState::Done => Glyph::Check,
        AgentState::Exited | AgentState::Unknown => Glyph::Dot,
    }
}

/// 글리프 크기. Orca `sm`(10px 상자)·`md`(12px 상자).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    Sm,
    Md,
}

impl Size {
    const fn outer(self) -> f32 {
        match self {
            Self::Sm => 10.,
            Self::Md => 12.,
        }
    }

    /// 스피너 링·점 지름.
    const fn inner(self) -> f32 {
        match self {
            Self::Sm => 6.,
            Self::Md => 8.,
        }
    }
}

/// 상태 글리프. `unread`면 글리프 대신 호박색 벨을 그린다.
pub fn status_glyph(state: AgentState, unread: bool, size: Size, cx: &App) -> gpui::Div {
    glyph_frame(state, unread, size, None, cx)
}

/// 캐시된 뷰(사이드바) 안의 스피너 자리. 스피너 틱마다 그 뷰 전체가 다시 렌더되지 않도록
/// 행은 자리(창 좌표·클립·색)만 적고, 회전은 부모 뷰가 [`spinner_overlay`]로 그 위에 그린다.
#[derive(Debug, Clone, Default)]
pub struct SpinnerSlots(Rc<RefCell<Vec<SpinnerSlot>>>);

#[derive(Debug)]
struct SpinnerSlot {
    bounds: Bounds<Pixels>,
    mask: ContentMask<Pixels>,
    color: Hsla,
}

impl SpinnerSlots {
    /// 뷰를 다시 렌더할 때 부른다. 재사용된 프레임에서는 지난 자리가 그대로 유효하다.
    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }
}

/// [`status_glyph`]와 같되, `slots`가 있으면 스피너는 자리만 적는다.
pub fn status_glyph_slot(
    state: AgentState,
    unread: bool,
    size: Size,
    slots: Option<&SpinnerSlots>,
    cx: &App,
) -> gpui::Div {
    glyph_frame(state, unread, size, slots, cx)
}

/// `slots`에 적힌 자리마다 도는 스피너를 그린다. 이 요소를 그리는 뷰(루트)만 틱마다 다시 렌더된다.
/// 자리를 적은 캐시 뷰보다 뒤(위)에 두고, 그 뷰의 메뉴·모달은 deferred라 이 위에 그려진다.
pub fn spinner_overlay(slots: &SpinnerSlots) -> impl gpui::IntoElement {
    let slots = slots.clone();
    canvas(
        move |_, window, cx| {
            let slots = slots.0.borrow();
            slots
                .iter()
                .enumerate()
                .map(|(i, slot)| {
                    let mut ring = div()
                        .id(("spinner-slot", i))
                        .child(spinning_ring(slot.bounds.size.width.into(), slot.color))
                        .into_any_element();
                    window.with_content_mask(Some(slot.mask), |window| {
                        ring.prepaint_as_root(
                            slot.bounds.origin,
                            slot.bounds.size.into(),
                            window,
                            cx,
                        );
                    });
                    (slot.mask, ring)
                })
                .collect::<Vec<_>>()
        },
        |_, mut rings, window, cx| {
            for (mask, ring) in &mut rings {
                window.with_content_mask(Some(*mask), |window| ring.paint(window, cx));
            }
        },
    )
    .absolute()
}

fn spinning_ring(size_px: f32, color: Hsla) -> AnyElement {
    icon(RING_SVG, size_px, color)
        .with_animation(
            "agent-spinner",
            Animation::new(Duration::from_secs(1))
                .repeat_synced()
                .with_max_fps(SPIN_STEPS)
                .with_easing(|t| (t * SPIN_STEPS).floor() / SPIN_STEPS),
            |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
        )
        .into_any_element()
}

fn glyph_frame(
    state: AgentState,
    unread: bool,
    size: Size,
    slots: Option<&SpinnerSlots>,
    cx: &App,
) -> gpui::Div {
    let frame = div()
        .size(px(size.outer()))
        .flex_none()
        .flex()
        .items_center()
        .justify_center();
    if unread {
        return frame.child(icon(BELL_FILLED_SVG, size.outer(), AGENT_UNREAD));
    }
    let color = agent_state_color(state, cx);
    frame.child(match glyph(state) {
        Glyph::Spinner if cx.reduce_motion() => {
            icon(RING_FULL_SVG, size.inner(), color).into_any_element()
        }
        Glyph::Spinner => match slots {
            Some(slots) => {
                let slots = slots.0.clone();
                canvas(
                    move |bounds, window, _| {
                        slots.borrow_mut().push(SpinnerSlot {
                            bounds,
                            mask: window.content_mask(),
                            color,
                        });
                    },
                    |_, (), _, _| {},
                )
                .size(px(size.inner()))
                .into_any_element()
            }
            None => spinning_ring(size.inner(), color),
        },
        Glyph::Question => Icon::new(IconName::MessageCircleQuestionMark)
            .with_size(px(size.outer()))
            .text_color(color)
            .into_any_element(),
        Glyph::Check => Icon::new(IconName::CircleCheck)
            .with_size(px(size.outer()))
            .text_color(color)
            .into_any_element(),
        Glyph::Dot => div()
            .size(px(size.inner()))
            .rounded_full()
            .bg(color.opacity(0.5))
            .into_any_element(),
    })
}

fn icon(bytes: &'static [u8], size_px: f32, color: Hsla) -> Icon {
    Icon::default()
        .data(bytes)
        .with_size(px(size_px))
        .text_color(color)
}

/// 에이전트 아이콘. 에이전트를 모르는 셸 세션은 터미널 아이콘.
pub fn agent_icon(agent: &str, size_px: f32, fg: Hsla) -> AnyElement {
    let agent = infer_agent_from_title(agent, "");
    if is_shell_agent(agent) {
        Icon::new(IconName::SquareTerminal)
            .with_size(px(size_px))
            .text_color(fg)
            .into_any_element()
    } else {
        icons::agent_brand_icon(agent, size_px, fg).into_any_element()
    }
}

/// 에이전트가 없거나 셸 이름만 알려졌을 때 터미널 제목에서 에이전트 종류를 추론한다.
#[must_use]
pub fn infer_agent_from_title<'a>(agent: &'a str, title: &str) -> &'a str {
    if let Some(known) = known_agent(agent) {
        return known;
    }
    if !is_shell_agent(agent) {
        return agent;
    }
    let lower = title.to_ascii_lowercase();
    if lower.contains("claude") {
        "claude"
    } else if lower.contains("codex") {
        "codex"
    } else if lower.contains("antigravity") {
        "agy"
    } else if lower.contains("opencode") {
        "opencode"
    } else if lower.contains("cursor") {
        "cursor"
    } else if lower
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .any(|word| matches!(word, "pi" | "π"))
    {
        "pi"
    } else {
        agent
    }
}

/// A recognized live foreground agent supersedes hooks left by an earlier agent in the same shell.
/// Generic runtimes (`node`, `bun`) and titles cannot override an explicit hook identity.
#[must_use]
pub fn infer_agent_from_status<'a>(
    status: &'a krill_proto::control::AgentStatus,
    title: &str,
) -> &'a str {
    if let Some(agent) = status.foreground.as_deref().and_then(known_agent) {
        return agent;
    }
    if let Some(original_title) = status.title.as_deref() {
        let original = infer_agent_from_title(&status.agent, original_title);
        if !is_shell_agent(original) {
            return original;
        }
    }
    infer_agent_from_title(&status.agent, title)
}

fn is_shell_agent(agent: &str) -> bool {
    agent.is_empty()
        || [
            "sh",
            "bash",
            "zsh",
            "fish",
            "nu",
            "pwsh",
            "powershell",
            "cmd",
            "shell",
        ]
        .iter()
        .any(|shell| agent.eq_ignore_ascii_case(shell))
}

fn known_agent(agent: &str) -> Option<&'static str> {
    let name = agent.rsplit(['/', '\\']).next().unwrap_or(agent);
    let name = name
        .rsplit_once('.')
        .filter(|(_, ext)| ext.eq_ignore_ascii_case("exe"))
        .map_or(name, |(stem, _)| stem);
    [
        ("claude", "claude"),
        ("codex", "codex"),
        ("agy", "agy"),
        ("antigravity", "agy"),
        ("opencode", "opencode"),
        ("cursor", "cursor"),
        ("cursor-agent", "cursor"),
        ("pi", "pi"),
    ]
    .into_iter()
    .find_map(|(alias, id)| name.eq_ignore_ascii_case(alias).then_some(id))
}

/// 같은 상태의 세션 묶음.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group<'a> {
    pub state: AgentState,
    /// 나란히 그릴 서로 다른 에이전트(많은 순, 같으면 먼저 나온 순, 최대 [`MAX_GROUP_ICONS`]).
    pub icons: Vec<&'a str>,
    /// 아이콘으로 못 그린 세션 수(`+N`).
    pub more: usize,
}

/// 워크트리 행 요약: 앞 [`MAX_GROUPS`]개 묶음과 나머지 묶음의 세션 수(`+N`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Summary<'a> {
    pub groups: Vec<Group<'a>>,
    pub hidden: usize,
}

/// 묶음 순서: 사람이 봐야 하는 상태가 먼저(Orca `SUMMARY_STATE_ORDER`).
const ORDER: [AgentState; 6] = [
    AgentState::Waiting,
    AgentState::Blocked,
    AgentState::Working,
    AgentState::Done,
    AgentState::Exited,
    AgentState::Unknown,
];

/// `(상태, 에이전트)` 목록을 상태별로 묶는다.
#[must_use]
pub fn summarize<'a>(sessions: &[(AgentState, &'a str)]) -> Summary<'a> {
    let mut summary = Summary::default();
    for state in ORDER {
        let agents: Vec<&str> = sessions
            .iter()
            .filter(|(s, _)| *s == state)
            .map(|(_, a)| *a)
            .collect();
        if agents.is_empty() {
            continue;
        }
        if summary.groups.len() == MAX_GROUPS {
            summary.hidden += agents.len();
            continue;
        }
        // (에이전트, 수, 처음 위치)
        let mut kinds: Vec<(&str, usize, usize)> = Vec::new();
        for (i, agent) in agents.iter().enumerate() {
            match kinds.iter_mut().find(|k| k.0 == *agent) {
                Some(k) => k.1 += 1,
                None => kinds.push((agent, 1, i)),
            }
        }
        kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
        let icons: Vec<&str> = kinds.iter().take(MAX_GROUP_ICONS).map(|k| k.0).collect();
        summary.groups.push(Group {
            state,
            more: agents.len() - icons.len(),
            icons,
        });
    }
    summary
}

/// 워크트리 행의 상태 묶음: [글리프][에이전트 아이콘][+N] … [+N].
/// `slots`는 [`status_glyph_slot`]과 같다.
pub fn summary_element(summary: &Summary<'_>, slots: Option<&SpinnerSlots>, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let more = |n: usize| {
        div()
            .flex_none()
            .font_family(FONT_MONO)
            .text_size(TEXT_2XS)
            .text_color(p.muted_foreground.opacity(0.7))
            .child(format!("+{n}"))
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .children(summary.groups.iter().enumerate().map(|(group_index, g)| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .px_1p5()
                .py_0p5()
                .rounded(RADIUS_XS)
                .bg(p.worktree_sidebar_accent.opacity(0.65))
                .child(glyph_frame(g.state, false, Size::Sm, slots, cx))
                .child(div().flex().items_center().gap(STATUS_GROUP_GAP).children(
                    g.icons.iter().enumerate().map(|(i, a)| {
                        div()
                            .debug_selector(move || {
                                format!("status-summary-icon-{group_index}-{i}")
                            })
                            .size(STATUS_GROUP_ICON)
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(agent_icon(a, super::ICON_SM, p.muted_foreground))
                    }),
                ))
                .when(g.more > 0, |d| d.child(more(g.more)))
        }))
        .when(summary.hidden > 0, |d| d.child(more(summary.hidden)))
}

/// 떼 미터에 그릴 점(상태 우선순위 순, 최대 `max`개, `max`는 [`SWARM_MAX`]로 자른다)과 넘친 세션 수(`+N`).
/// 순서는 [`summarize`]와 같다: 확인 필요(`Waiting`)·차단 → 작업 중 → 완료 → 유휴(`Exited`·`Unknown`).
#[must_use]
pub fn swarm_dots(states: &[AgentState], max: usize) -> (Vec<AgentState>, usize) {
    let max = max.min(SWARM_MAX);
    let mut dots: Vec<AgentState> = ORDER
        .iter()
        .flat_map(|o| states.iter().copied().filter(move |s| s == o))
        .collect();
    let more = dots.len().saturating_sub(max);
    dots.truncate(max);
    (dots, more)
}

/// 떼(swarm) 미터: 세션마다 3px 상태색 점, 넘치면 `+N`(`TEXT_XS` muted). 세션 수가 바뀌어도 폭이 흔들리지 않게
/// 점 `max`개 칸과 `+N` 칸을 늘 예약한다. 접근성 라벨·툴팁은 호출자가 바깥 `div().id(..)`에 붙인다.
pub fn swarm_meter(states: &[AgentState], max: usize, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let (dots, more) = swarm_dots(states, max);
    // `SWARM_MAX`(12) 이하라 u8에 늘 들어간다.
    let slots = f32::from(u8::try_from(max.min(SWARM_MAX)).unwrap_or(u8::MAX));
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(SWARM_GAP))
        .child(
            div()
                .flex_none()
                .w(px((slots * (SWARM_DOT + SWARM_GAP) - SWARM_GAP).max(0.)))
                .flex()
                .items_center()
                .gap(px(SWARM_GAP))
                .children(dots.into_iter().map(|state| {
                    let color = agent_state_color(state, cx);
                    let color = if glyph(state) == Glyph::Dot {
                        color.opacity(0.5)
                    } else {
                        color
                    };
                    div()
                        .flex_none()
                        .size(px(SWARM_DOT))
                        .rounded_full()
                        .bg(color)
                })),
        )
        .child(
            div()
                .flex_none()
                .w(px(SWARM_MORE_WIDTH))
                .font_family(FONT_MONO)
                .text_size(TEXT_XS)
                .text_color(p.muted_foreground)
                .when(more > 0, |d| d.child(format!("+{more}"))),
        )
}

/// 워크트리 행 왼쪽의 미확인 점(6px 호박색).
#[must_use]
pub fn unread_dot() -> gpui::Div {
    div().size(STATE_DOT).rounded_full().bg(AGENT_UNREAD)
}

/// OWE-707: 계정 라벨 축약(배지용, 최대 2자).
/// - 두 단어 이상이면 각 첫 글자 2개(예: "Work Max" → "WM", "claude-host" → "CH").
/// - 한 단어면 앞 2자(예: "작업용" → "작업", "Host" → "Ho").
/// - 빈 값이면 빈 문자열(호출자는 배지를 숨긴다).
#[must_use]
pub fn abbreviate_account_label(label: &str) -> String {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let words: Vec<&str> = trimmed
        .split(|c: char| c.is_whitespace() || matches!(c, '-' | '_' | '(' | ')' | '/' | '.' | '+'))
        .filter(|w| !w.is_empty())
        .collect();
    if words.len() >= 2 {
        words
            .iter()
            .take(2)
            .filter_map(|w| w.chars().next())
            .collect::<String>()
            .to_uppercase()
    } else {
        trimmed.chars().take(2).collect()
    }
}

/// OWE-707: 좁은 탭용 이니셜 1자(예: "Work Max" → "W").
#[must_use]
pub fn account_initial(label: &str) -> String {
    label
        .trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default()
}

/// OWE-699: '다른 계정으로 이어가기' 칩 노출 조건. 프로비저닝(대화 기록 공유, `accounts.toml`
/// `provisioning`)이 켜져 있고 세션 계정이 한도에 닿았을 때만 보인다. 기록을 공유하지 않으면 다른
/// 계정 프로필에서 대화를 찾지 못하기 때문이다. 칩 UI와 원 세션 보존 방식(`--fork-session` 여부)은
/// 오웬 결정 대기라 아직 그리지 않는다.
#[must_use]
pub fn continue_chip_visible(
    provisioning: bool,
    status: Option<krill_proto::usage::AccountStatus>,
) -> bool {
    provisioning && status == Some(krill_proto::usage::AccountStatus::RateLimited)
}

/// OWE-737: Claude 프롬프트 캐시 만료 임박 기준(초). 60초 이하면 경고 색으로 바꾼다.
/// (Orca `.ref/orca/src/renderer/src/components/CacheTimer.tsx`, Copyright (c) 2026 Lovecast Inc., MIT License 참고)
pub const URGENT_CACHE_THRESHOLD_SECS: u32 = 60;

/// OWE-737: 남은 초를 `M:SS` 문자열로 포맷한다(예: `300 -> "5:00"`, `59 -> "0:59"`).
#[must_use]
pub fn format_cache_countdown(remaining_secs: u32) -> String {
    let mins = remaining_secs / 60;
    let secs = remaining_secs % 60;
    format!("{mins}:{secs:02}")
}

/// OWE-737: 세션의 Claude 프롬프트 캐시 남은 시간(초)을 계산한다.
/// - `agent`가 `"claude"`(대소문자 무관)이고
/// - 상태가 유휴/대기(`Done`·`Waiting`·`Blocked`)이며
/// - `since_unix_ns > 0`, `ttl_secs > 0`이고 아직 만료 전(`remaining > 0`)일 때만 `Some(remaining_secs)`를 돌려준다.
#[must_use]
pub fn session_cache_remaining_secs(
    agent: &str,
    state: AgentState,
    since_unix_ns: u64,
    now_unix_ns: u64,
    ttl_secs: u32,
) -> Option<u32> {
    if !agent.eq_ignore_ascii_case("claude")
        || !matches!(
            state,
            AgentState::Done | AgentState::Waiting | AgentState::Blocked
        )
        || since_unix_ns == 0
        || ttl_secs == 0
    {
        return None;
    }
    let elapsed_secs = now_unix_ns.saturating_sub(since_unix_ns) / 1_000_000_000;
    let ttl = u64::from(ttl_secs);
    if elapsed_secs >= ttl {
        return None;
    }
    u32::try_from(ttl - elapsed_secs).ok()
}

/// OWE-737: 워크트리에 속한 세션들 가운데 가장 먼저 만료되는(남은 시간이 가장 짧은)
/// Claude 프롬프트 캐시 타이머의 남은 초를 돌려준다.
#[must_use]
pub fn worktree_most_urgent_cache_remaining_secs<'a>(
    sessions: impl IntoIterator<Item = (&'a str, AgentState, u64)>,
    now_unix_ns: u64,
    ttl_secs: u32,
) -> Option<u32> {
    sessions
        .into_iter()
        .filter_map(|(agent, state, since_ns)| {
            session_cache_remaining_secs(agent, state, since_ns, now_unix_ns, ttl_secs)
        })
        .min()
}

/// OWE-737: 프롬프트 캐시 카운트다운 알약(`CacheTimer`).
/// 호출자는 필요 시 바깥 `div().id(..)`로 감싸 툴팁·선택자를 붙인다.
#[must_use]
pub fn cache_timer_chip(remaining_secs: u32, cx: &App) -> gpui::Div {
    let p = palette(cx);
    let urgent = remaining_secs <= URGENT_CACHE_THRESHOLD_SECS;
    let (fg, bg) = if urgent {
        (p.warning, p.warning.opacity(0.14))
    } else {
        (p.muted_foreground, p.muted.opacity(0.6))
    };
    div()
        .flex_none()
        .px_1()
        .py(px(1.))
        .rounded(RADIUS_XS)
        .bg(bg)
        .font_family(FONT_MONO)
        .text_size(TEXT_2XS)
        .text_color(fg)
        .child(format_cache_countdown(remaining_secs))
}

// OWE-707 배지 본체는 호출자(탭·사이드바 행)가 만든다. `Div`에 `id`를 걸어
// `Stateful`로 만들어야 툴팁·접근성 라벨이 붙기 때문이다(행별 index로 구분).
// '다른 계정으로 이어가기' 칩은 OWE-699(대화 기록 공유, 오웬 결정 대기)까지 숨기고,
// 원 세션 PTY는 절대 닫지 않는다.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_identity_survives_renaming_and_shell_hooks() {
        use krill_proto::control::AgentStatus;
        for (raw, expected) in [
            ("Claude", "claude"),
            ("/opt/bin/codex", "codex"),
            ("C:\\bin\\opencode.exe", "opencode"),
            ("Antigravity", "agy"),
            ("Pi", "pi"),
            ("/opt/bin/pi", "pi"),
            ("C:\\bin\\pi.exe", "pi"),
        ] {
            assert_eq!(infer_agent_from_title(raw, "renamed task"), expected);
        }
        let mut status = AgentStatus {
            agent: "zsh".into(),
            foreground: Some("/opt/bin/codex".into()),
            ..AgentStatus::UNKNOWN
        };
        assert_eq!(
            infer_agent_from_status(&status, "custom task title"),
            "codex"
        );
        status.agent = "claude".into();
        assert_eq!(infer_agent_from_status(&status, "Codex task"), "codex");
        status.agent.clear();
        status.foreground = None;
        status.title = Some("Claude task".into());
        assert_eq!(infer_agent_from_status(&status, "Renamed task"), "claude");
        assert_eq!(infer_agent_from_status(&status, "Codex task"), "claude");
        status.title = Some("zsh".into());
        assert_eq!(
            infer_agent_from_status(&status, "OpenCode task"),
            "opencode"
        );
        assert_eq!(
            infer_agent_from_title("my-agent", "Claude task"),
            "my-agent"
        );
        assert!(is_shell_agent("zsh"));
    }

    #[test]
    fn pi_identity_uses_explicit_agent_then_foreground_then_a_whole_title_word() {
        use krill_proto::control::AgentStatus;
        let mut status = AgentStatus {
            agent: "pi".into(),
            foreground: Some("node".into()),
            ..AgentStatus::UNKNOWN
        };
        assert_eq!(infer_agent_from_status(&status, "renamed task"), "pi");
        status.agent = "zsh".into();
        status.foreground = Some("/opt/bin/pi".into());
        assert_eq!(infer_agent_from_status(&status, "renamed task"), "pi");
        status.foreground = None;
        for title in ["Pi", "pi — krill", "π coding agent"] {
            assert_eq!(infer_agent_from_status(&status, title), "pi");
        }
        for title in ["pipeline", "spin", "api", "my-pi-task"] {
            assert_eq!(infer_agent_from_status(&status, title), "zsh");
        }
        status.agent = "custom".into();
        assert_eq!(infer_agent_from_status(&status, "Pi"), "custom");
    }

    #[test]
    fn live_agent_replaces_stale_hooks_and_titles_in_a_reused_terminal() {
        use krill_proto::control::AgentStatus;
        let mut status = AgentStatus {
            agent: "claude".into(),
            title: Some("Claude Code".into()),
            ..AgentStatus::UNKNOWN
        };
        for process in ["codex", "C:\\npm\\codex.exe", "C:\\npm\\CODEX.EXE"] {
            status.foreground = Some(process.into());
            assert_eq!(infer_agent_from_status(&status, "Claude Code"), "codex");
        }
        status.agent = "codex".into();
        status.foreground = Some("claude".into());
        assert_eq!(infer_agent_from_status(&status, "Codex"), "claude");
        for process in [None, Some("node"), Some("bun"), Some("git")] {
            status.agent = "claude".into();
            status.foreground = process.map(str::to_owned);
            assert_eq!(infer_agent_from_status(&status, "Codex"), "claude");
        }
    }

    #[test]
    fn state_maps_to_glyph() {
        assert_eq!(glyph(AgentState::Working), Glyph::Spinner);
        assert_eq!(glyph(AgentState::Waiting), Glyph::Question);
        assert_eq!(glyph(AgentState::Blocked), Glyph::Question);
        assert_eq!(glyph(AgentState::Done), Glyph::Check);
        assert_eq!(glyph(AgentState::Exited), Glyph::Dot);
        assert_eq!(glyph(AgentState::Unknown), Glyph::Dot);
    }

    /// 스피너는 120° 간격 3분절 아크, 감속 대체는 닫힌 링.
    #[test]
    fn spinner_is_three_segment_arc_and_reduce_motion_is_closed_ring() {
        let ring = std::str::from_utf8(RING_SVG).unwrap();
        assert!(ring.starts_with("<svg") && ring.ends_with("</svg>"));
        assert_eq!(ring.matches("<path").count(), 3);
        assert_eq!(ring.matches("A8.5 8.5 0 0 1").count(), 3);
        let full = std::str::from_utf8(RING_FULL_SVG).unwrap();
        assert!(full.contains("<circle"));
    }

    #[test]
    fn swarm_dots_sort_by_attention_and_fold_overflow() {
        use AgentState::{Blocked, Done, Exited, Unknown, Waiting, Working};
        let (dots, more) = swarm_dots(&[Done, Unknown, Working, Exited, Blocked, Waiting], 12);
        assert_eq!(dots, [Waiting, Blocked, Working, Done, Exited, Unknown]);
        assert_eq!(more, 0);
        // 15개 → 12점 + 3. 확인 필요가 먼저 남는다.
        let mut many = vec![Done; 14];
        many.push(Waiting);
        let (dots, more) = swarm_dots(&many, 12);
        assert_eq!(dots.len(), 12);
        assert_eq!(dots[0], Waiting);
        assert_eq!(more, 3);
        // max는 SWARM_MAX로 자른다.
        let (dots, more) = swarm_dots(&many, 99);
        assert_eq!((dots.len(), more), (SWARM_MAX, 3));
        assert_eq!(swarm_dots(&[], 12), (Vec::new(), 0));
    }

    #[gpui::test]
    fn summary_provider_icons_have_space_between_them(cx: &mut gpui::TestAppContext) {
        struct SummaryView;
        impl gpui::Render for SummaryView {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                cx: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                let summary = summarize(&[
                    (AgentState::Working, "claude"),
                    (AgentState::Working, "codex"),
                    (AgentState::Working, "agy"),
                    (AgentState::Done, "claude"),
                    (AgentState::Done, "codex"),
                ]);
                div().child(summary_element(&summary, None, cx))
            }
        }
        let window = cx.add_window(|_, _| SummaryView);
        let mut cx = gpui::VisualTestContext::from_window(window.into(), cx);
        cx.run_until_parked();
        for (previous, current) in [
            ("status-summary-icon-0-0", "status-summary-icon-0-1"),
            ("status-summary-icon-0-1", "status-summary-icon-0-2"),
            ("status-summary-icon-1-0", "status-summary-icon-1-1"),
        ] {
            let previous = cx.debug_bounds(previous).unwrap();
            let current = cx.debug_bounds(current).unwrap();
            assert_eq!(previous.size.width, STATUS_GROUP_ICON);
            assert_eq!(current.origin.x - previous.right(), STATUS_GROUP_GAP);
        }
    }

    #[gpui::test]
    fn swarm_meter_reserves_width_regardless_of_count(cx: &mut gpui::TestAppContext) {
        use gpui::{InteractiveElement as _, IntoElement as _};
        struct Meters;
        impl gpui::Render for Meters {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                cx: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .child(
                        swarm_meter(&[], SWARM_MAX, cx)
                            .id("swarm-empty")
                            .debug_selector(|| "swarm-empty".to_owned()),
                    )
                    .child(
                        swarm_meter(&[AgentState::Working; 20], SWARM_MAX, cx)
                            .id("swarm-full")
                            .debug_selector(|| "swarm-full".to_owned()),
                    )
                    .into_any_element()
            }
        }
        let window = cx.add_window(|_, _| Meters);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        let empty = vcx.debug_bounds("swarm-empty").map(|b| b.size.width);
        let full = vcx.debug_bounds("swarm-full").map(|b| b.size.width);
        assert!(empty.is_some());
        assert_eq!(empty, full);
    }

    #[test]
    fn groups_by_state_in_attention_order() {
        use AgentState::{Done, Waiting, Working};
        let s = summarize(&[(Done, "codex"), (Working, "claude"), (Waiting, "claude")]);
        let states: Vec<_> = s.groups.iter().map(|g| g.state).collect();
        assert_eq!(states, [Waiting, Working, Done]);
        assert_eq!(s.hidden, 0);
        assert!(summarize(&[]).groups.is_empty());
    }

    #[test]
    fn group_icons_dedupe_and_count_more() {
        use AgentState::Working;
        // claude 3, codex 2, agy 1, opencode 1 → 많은 순 3종, 나머지 세션 수가 +N.
        let s = summarize(&[
            (Working, "codex"),
            (Working, "claude"),
            (Working, "claude"),
            (Working, "opencode"),
            (Working, "codex"),
            (Working, "claude"),
            (Working, "agy"),
        ]);
        assert_eq!(s.groups.len(), 1);
        assert_eq!(s.groups[0].icons, ["claude", "codex", "opencode"]);
        assert_eq!(s.groups[0].more, 4);
        // 같은 에이전트 5개는 아이콘 하나 + 4.
        let s = summarize(&[(Working, "claude"); 5]);
        assert_eq!(s.groups[0].icons, ["claude"]);
        assert_eq!(s.groups[0].more, 4);
    }

    #[test]
    fn groups_past_three_fold_into_hidden_count() {
        use AgentState::{Blocked, Done, Exited, Unknown, Waiting, Working};
        let s = summarize(&[
            (Unknown, ""),
            (Exited, "codex"),
            (Exited, "claude"),
            (Done, "claude"),
            (Working, "claude"),
            (Blocked, "codex"),
            (Waiting, "claude"),
        ]);
        let states: Vec<_> = s.groups.iter().map(|g| g.state).collect();
        assert_eq!(states, [Waiting, Blocked, Working]);
        // Done 1 + Exited 2 + Unknown 1
        assert_eq!(s.hidden, 4);
    }

    /// OWE-699: 칩은 프로비저닝이 켜진 한도 도달 세션에만.
    #[test]
    fn continue_chip_needs_provisioning_and_rate_limit() {
        use krill_proto::usage::AccountStatus;
        assert!(!continue_chip_visible(
            false,
            Some(AccountStatus::RateLimited)
        ));
        assert!(continue_chip_visible(
            true,
            Some(AccountStatus::RateLimited)
        ));
        assert!(!continue_chip_visible(true, Some(AccountStatus::Ok)));
        assert!(!continue_chip_visible(true, None));
    }

    /// OWE-707: 계정 배지 축약 회귀 테스트(실패 우선).
    #[test]
    fn account_label_abbreviates_for_badge() {
        assert_eq!(abbreviate_account_label(""), "");
        assert_eq!(abbreviate_account_label("   "), "");
        assert_eq!(abbreviate_account_label("Work Max"), "WM");
        assert_eq!(abbreviate_account_label("claude-host"), "CH");
        assert_eq!(abbreviate_account_label("Host (.claude)"), "HC");
        assert_eq!(abbreviate_account_label("작업용"), "작업");
        assert_eq!(abbreviate_account_label("A"), "A");
        assert_eq!(account_initial("Work Max"), "W");
        assert_eq!(account_initial("작업용"), "작");
        assert_eq!(account_initial(""), "");
        assert_eq!(account_initial("   "), "");
    }

    /// OWE-737: Claude 프롬프트 캐시 카운트다운 포맷·계산·워크트리 최긴급 타이머 선택 검증.
    #[test]
    fn claude_prompt_cache_timer_computes_and_picks_most_urgent() {
        assert_eq!(format_cache_countdown(300), "5:00");
        assert_eq!(format_cache_countdown(65), "1:05");
        assert_eq!(format_cache_countdown(59), "0:59");
        assert_eq!(format_cache_countdown(0), "0:00");

        let base_ns = 1_000_000_000_000_u64;
        let now_ns = base_ns + 120 * 1_000_000_000;
        // Claude Done(유휴) 120초 경과 → 300초 TTL 기준 180초 남음.
        assert_eq!(
            session_cache_remaining_secs("claude", AgentState::Done, base_ns, now_ns, 300),
            Some(180)
        );
        // Waiting(권한 대기)도 카운트다운 대상.
        assert_eq!(
            session_cache_remaining_secs("Claude", AgentState::Waiting, base_ns, now_ns, 300),
            Some(180)
        );
        // Working / Exited / 비-Claude 에이전트는 표시하지 않음.
        assert_eq!(
            session_cache_remaining_secs("claude", AgentState::Working, base_ns, now_ns, 300),
            None
        );
        assert_eq!(
            session_cache_remaining_secs("codex", AgentState::Done, base_ns, now_ns, 300),
            None
        );
        // 만료(300초 이상 경과) 시 None.
        let expired_ns = base_ns + 300 * 1_000_000_000;
        assert_eq!(
            session_cache_remaining_secs("claude", AgentState::Done, base_ns, expired_ns, 300),
            None
        );

        // 워크트리에 여러 세션이 있을 때 가장 먼저 만료되는(남은 초가 가장 적은) 타이머를 고른다.
        let older_idle_ns = base_ns; // 120초 경과 → 180초 남음
        let newer_idle_ns = base_ns + 90 * 1_000_000_000; // 30초 경과 → 270초 남음
        let urgent = worktree_most_urgent_cache_remaining_secs(
            [
                ("claude", AgentState::Done, newer_idle_ns),
                ("claude", AgentState::Waiting, older_idle_ns),
                ("codex", AgentState::Done, older_idle_ns),
            ],
            now_ns,
            300,
        );
        assert_eq!(urgent, Some(180));
    }
}
