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
    FONT_MONO, STATE_DOT, TEXT_2XS, agent_state_color, icons, palette,
    theme::{AGENT_UNREAD, STATUS_GROUP_GAP, STATUS_GROUP_ICON},
};

/// 작업 중 스피너(B안 테이퍼드 코멧 링): 은은한 배경 트랙 원(`0.22`) 위로 꼬리(`0.04`)에서
/// 둥근 선두(`1.0`)까지 폭과 불투명도가 연속 증가하는 정적 SVG다. GPUI가 단일 채널 알파 마스크로
/// 1회 래스터라이즈해 GPU 회전 변환(`Transformation::rotate`)만으로 돌린다.
const RING_SVG: &[u8] = br##"<svg viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><defs><linearGradient id="c1" gradientUnits="userSpaceOnUse" x1="20.31" y1="9.77" x2="13.49" y2="20.47"><stop offset="0%" stop-color="#000" stop-opacity="0.04"/><stop offset="100%" stop-color="#000" stop-opacity="0.24"/></linearGradient><linearGradient id="c2" gradientUnits="userSpaceOnUse" x1="13.49" y1="20.47" x2="3.43" y2="12.75"><stop offset="0%" stop-color="#000" stop-opacity="0.24"/><stop offset="100%" stop-color="#000" stop-opacity="0.64"/></linearGradient><linearGradient id="c3" gradientUnits="userSpaceOnUse" x1="3.43" y1="12.75" x2="12.00" y2="3.40"><stop offset="0%" stop-color="#000" stop-opacity="0.64"/><stop offset="100%" stop-color="#000" stop-opacity="1"/></linearGradient></defs><circle cx="12" cy="12" r="8.6" stroke="#000" stroke-width="2.4" stroke-opacity="0.22"/><path d="M20.86 9.63C22.29 14.52 18.77 20.41 13.57 21.39L13.27 19.57C17.49 18.98 20.79 14.26 19.75 9.92Z" fill="url(#c1)"/><path d="M13.65 21.38C8.47 22.43 2.67 18.27 2.11 12.78L4.75 12.57C4.93 16.61 9.11 20.17 13.33 19.56Z" fill="url(#c2)"/><path d="M2.11 12.87C1.50 7.41 6.29 1.81 12.00 1.68L12.00 5.12C8.20 4.99 4.54 8.62 4.75 12.63Z" fill="url(#c3)"/><circle cx="12" cy="3.4" r="1.72" fill="#000"/></svg>"##;
/// 접근성 감속(reduce motion)에서는 멈춘 코멧 꼬리가 고장처럼 보이지 않도록 닫힌 정적 링을 그린다.
const RING_FULL_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="8.6"/></svg>"#;
/// Lucide `bell`(ISC License)을 채운 모양. 미확인 완료 표시.
const BELL_FILLED_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M10.268 21a2 2 0 0 0 3.464 0"/><path d="M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326"/></svg>"#;

/// 1초에 한 바퀴, 30fps 연속 회전. 모든 스피너가 앱 공용 시계(`repeat_synced`)로 같은 위상을 공유하고
/// 비활성 창에서는 0fps 정적 링으로 멈춰 GPU·배터리 부하를 억제한다.
const SPIN_FPS: f32 = 30.;
const SPIN_DURATION: Duration = Duration::from_secs(1);

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

    /// 유휴 상태 점 지름.
    const fn inner(self) -> f32 {
        match self {
            Self::Sm => 6.,
            Self::Md => 8.,
        }
    }

    /// 작업 중 코멧 스피너 지름. 질문·체크 아이콘과 시각 무게를 맞추고 소형에서도 꼬리 대비를 확보한다.
    const fn spinner(self) -> f32 {
        self.outer()
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

/// Renders the glyph for a full [`krill_proto::control::AgentStatus`]. A terminal turn that ended
/// in a provider error (e.g. Antigravity quota stop, Orca `b342cee0` / `#46999`) shows a red
/// `CircleX` instead of a green check.
pub fn status_glyph_for(
    status: &krill_proto::control::AgentStatus,
    unread: bool,
    size: Size,
    slots: Option<&SpinnerSlots>,
    cx: &App,
) -> gpui::Div {
    if status.turn_failed() {
        let p = palette(cx);
        return div()
            .size(px(size.outer()))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(
                Icon::new(IconName::CircleX)
                    .with_size(px(size.outer()))
                    .text_color(p.destructive),
            );
    }
    glyph_frame(status.state, unread, size, slots, cx)
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
    ActiveCometSpinner { size_px, color }.into_any_element()
}

/// 창 활성 상태와 감속 설정을 프레임 레이아웃 시점에 확인하는 저부하 스피너 요소.
///
/// - 활성 창 + 일반 모션: 정적 코멧 SVG(`RING_SVG`)를 30fps 공용 시계(`repeat_synced`)로 GPU 회전.
/// - 비활성 창: 애니메이션을 붙이지 않은 정적 코멧 링(0fps, 타이머·GPU 깨움 0).
///   GPUI의 창 활성화 콜백이 `window.refresh()`를 부르므로 포커스가 돌아오면 즉시 회전을 재개한다.
/// - 감속 모드(`cx.reduce_motion()`): 닫힌 정적 링(`RING_FULL_SVG`, 0fps).
struct ActiveCometSpinner {
    size_px: f32,
    color: Hsla,
}

impl gpui::IntoElement for ActiveCometSpinner {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for ActiveCometSpinner {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut gpui::Window,
        cx: &mut App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let mut child = if cx.reduce_motion() {
            icon(RING_FULL_SVG, self.size_px, self.color).into_any_element()
        } else if window.is_window_active() {
            icon(RING_SVG, self.size_px, self.color)
                .with_animation(
                    "agent-spinner",
                    Animation::new(SPIN_DURATION)
                        .repeat_synced()
                        .with_max_fps(SPIN_FPS),
                    |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
                )
                .into_any_element()
        } else {
            icon(RING_SVG, self.size_px, self.color).into_any_element()
        };
        let layout_id = child.request_layout(window, cx);
        (layout_id, child)
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        child: &mut Self::RequestLayoutState,
        window: &mut gpui::Window,
        cx: &mut App,
    ) {
        child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _global_id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        child: &mut Self::RequestLayoutState,
        (): &mut Self::PrepaintState,
        window: &mut gpui::Window,
        cx: &mut App,
    ) {
        child.paint(window, cx);
    }
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
            icon(RING_FULL_SVG, size.spinner(), color).into_any_element()
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
                .size(px(size.spinner()))
                .into_any_element()
            }
            None => spinning_ring(size.spinner(), color),
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

/// 워크트리 행 왼쪽의 미확인 점(6px 호박색).
#[must_use]
pub fn unread_dot() -> gpui::Div {
    div().size(STATE_DOT).rounded_full().bg(AGENT_UNREAD)
}

/// Projects 및 접힌 Bots 아바타에 쓰는 파란 안 읽음 점 (Orca `#47122` `unreadAlertDotClassName`:
/// `size-[6px] rounded-full bg-unread ring-2 ring-worktree-sidebar`).
/// GPUI는 `border_2()`가 박스 안쪽에 그려지므로 10px 박스에 2px 링을 둘러 6px 점을 만든다.
pub fn unread_alert_dot(ring_bg: Hsla, cx: &App) -> gpui::Div {
    div()
        .size(px(10.))
        .flex_none()
        .rounded_full()
        .border_2()
        .border_color(ring_bg)
        .bg(palette(cx).unread)
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

// OWE-707 배지 본체는 호출자(탭·사이드바 행)가 만든다. `Div`에 `id`를 걸어
// `Stateful`로 만들어야 툴팁·접근성 라벨이 붙기 때문이다(행별 index로 구분).
// '다른 계정으로 이어가기' 칩은 OWE-699(대화 기록 공유, 오웬 결정 대기)까지 숨기고,
// 원 세션 PTY는 절대 닫지 않는다.

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;

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

    /// 테이퍼드 코멧 링 SVG가 GPUI의 실제 resvg 알파 마스크에서 단조 증가하는 꼬리→선두 알파를 만들고,
    /// 감속 대체는 닫힌 정적 링인지 검증한다.
    #[test]
    fn comet_spinner_svg_has_tapered_alpha_mask_and_reduce_motion_is_closed_ring() {
        let ring = std::str::from_utf8(RING_SVG).unwrap();
        assert!(ring.starts_with("<svg") && ring.ends_with("</svg>"));
        assert_eq!(ring.matches("<linearGradient").count(), 3);
        assert!(ring.contains("stroke-opacity=\"0.22\""));
        let full = std::str::from_utf8(RING_FULL_SVG).unwrap();
        assert!(full.contains("<circle"));

        let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(()));
        let parsed = renderer.parse_svg(RING_SVG).unwrap();
        let dim: usize = 96;
        let dim_px = gpui::DevicePixels(96);
        let image = renderer
            .render_parsed(
                &parsed,
                gpui::SvgSize::ExactSize(gpui::size(dim_px, dim_px)),
            )
            .unwrap();
        let bytes = image.as_bytes(0).unwrap();
        let alpha_at =
            |buf: &[u8], px_x: usize, px_y: usize| -> u8 { buf[(px_y * dim + px_x) * 4 + 3] };

        // 96px(24px × 4) 좌표계 반지름 8.6 궤도 상 알파: 중심(0) < 빈 트랙(1:30) < 꼬리(4:30) < 중간(7:30) < 선두(12:00)
        let center = alpha_at(bytes, 48, 48);
        let track = alpha_at(bytes, 72, 24); // (18.0, 6.0) ~45 deg (track only)
        let tail = alpha_at(bytes, 72, 72); // (18.0, 18.0) ~135 deg (c1)
        let mid = alpha_at(bytes, 24, 72); // (6.0, 18.0) ~225 deg (c2)
        let head = alpha_at(bytes, 48, 14); // (12.0, 3.5) 0 deg (head cap)

        assert_eq!(center, 0, "spinner center must stay hollow");
        assert!(
            track > 30 && track < 80,
            "background track alpha expected ~56 (0.22), got {track}"
        );
        assert!(
            tail > track,
            "tail ({tail}) must exceed track alone ({track})"
        );
        assert!(mid > tail + 30, "mid ({mid}) must ramp above tail ({tail})");
        assert!(
            head >= 245,
            "head cap must reach full opacity (~255), got {head}"
        );

        let full_parsed = renderer.parse_svg(RING_FULL_SVG).unwrap();
        let full_img = renderer
            .render_parsed(
                &full_parsed,
                gpui::SvgSize::ExactSize(gpui::size(dim_px, dim_px)),
            )
            .unwrap();
        let full_bytes = full_img.as_bytes(0).unwrap();
        assert_eq!(alpha_at(full_bytes, 48, 48), 0);
        assert!(alpha_at(full_bytes, 48, 14) >= 245);
        assert!(alpha_at(full_bytes, 72, 24) >= 245);
    }

    #[gpui::test]
    fn unfocused_window_pauses_spinner_at_zero_fps_and_resumes_on_focus(
        cx: &mut gpui::TestAppContext,
    ) {
        struct SpinnerHost {
            slots: SpinnerSlots,
            renders: usize,
        }
        impl gpui::Render for SpinnerHost {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                cx: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                self.renders += 1;
                self.slots.clear();
                div()
                    .size(px(80.))
                    .child(status_glyph(AgentState::Working, false, Size::Md, cx))
                    .child(status_glyph_slot(
                        AgentState::Working,
                        false,
                        Size::Sm,
                        Some(&self.slots),
                        cx,
                    ))
                    .child(spinner_overlay(&self.slots))
            }
        }
        struct OtherWindow;
        impl gpui::Render for OtherWindow {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                _: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                div()
            }
        }

        cx.update(gpui_component::init);
        let (view, vcx) = cx.add_window_view(|_, _| SpinnerHost {
            slots: SpinnerSlots::default(),
            renders: 0,
        });
        vcx.run_until_parked();

        // 1) 초기 비활성 창: 1초 동안 0fps(추가 렌더 0회).
        let inactive_before = view.read_with(vcx, |v, _| v.renders);
        for _ in 0..10 {
            vcx.executor().advance_clock(Duration::from_millis(100));
            vcx.run_until_parked();
        }
        assert_eq!(
            view.read_with(vcx, |v, _| v.renders),
            inactive_before,
            "unfocused window must not schedule spinner frames"
        );

        // 2) 창 활성화: 즉시 30fps 회전 재개.
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        let active_before = view.read_with(vcx, |v, _| v.renders);
        for _ in 0..12 {
            vcx.executor().advance_clock(Duration::from_millis(50));
            vcx.run_until_parked();
        }
        assert!(
            view.read_with(vcx, |v, _| v.renders) >= active_before + 10,
            "active window must resume smooth 30fps ticking"
        );

        // 3) 다른 창으로 포커스 이탈: 최대 1회 잔여 틱 이후 완전 0fps 정지.
        let other = cx.add_window(|_, _| OtherWindow);
        cx.update_window(other.into(), |_, window, _| window.activate_window())
            .unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        let paused_at = view.read_with(cx, |v, _| v.renders);
        for _ in 0..10 {
            cx.executor().advance_clock(Duration::from_millis(100));
            cx.run_until_parked();
        }
        assert_eq!(
            view.read_with(cx, |v, _| v.renders),
            paused_at,
            "deactivated window must stop scheduling spinner ticks"
        );
    }

    #[gpui::test]
    fn reduce_motion_renders_static_ring_at_zero_fps(cx: &mut gpui::TestAppContext) {
        struct Host {
            renders: usize,
        }
        impl gpui::Render for Host {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                cx: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                self.renders += 1;
                div().child(status_glyph(AgentState::Working, false, Size::Md, cx))
            }
        }

        cx.update(gpui_component::init);
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, vcx) = cx.add_window_view(|_, _| Host { renders: 0 });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();

        let before = view.read_with(vcx, |v, _| v.renders);
        for _ in 0..10 {
            vcx.executor().advance_clock(Duration::from_millis(100));
            vcx.run_until_parked();
        }
        assert_eq!(
            view.read_with(vcx, |v, _| v.renders),
            before,
            "reduce_motion must keep active window spinner at 0fps"
        );
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
}
