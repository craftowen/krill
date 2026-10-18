//! OWE-662: 에이전트 상태 글리프·미확인 벨·워크트리 상태 묶음. 탭·사이드바 세션 행·워크트리 행이 같이 쓴다.
//! OWE-707: 세션별 사용 계정 배지(라벨 축약·툴팁 전체 이름). 원 세션 PTY는 절대 닫지 않는다.
//!
//! 크기·순서·묶음 규칙은 Orca(`.ref/orca`, Copyright (c) 2026 Lovecast Inc., MIT License)의
//! `AgentStateDot.tsx`·`AgentWorkingSpinner.tsx`·`worktree-card-compact-agents.tsx`를 참고했다(코드 복사 없음).
//! 계정 배지 모양만 Orca `CodexRestartChip.tsx:140-155`의 칩 의도를 따르되,
//! 이어가기는 OWE-699(대화 기록 공유, 오웬 결정 대기)까지 숨긴다.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Hsla, IntoElement as _, ParentElement as _,
    Styled as _, Transformation, div, percentage, prelude::FluentBuilder as _, px,
};
use gpui_component::{Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::control::AgentState;

use super::{
    FONT_MONO, RADIUS_XS, STATE_DOT, TEXT_2XS, agent_state_color, icons, palette,
    theme::{AGENT_UNREAD, STATUS_GROUP_ICON, STATUS_GROUP_OVERLAP},
};

/// 3/4 링(위쪽이 빈 회전 스피너). 선 굵기는 6px 링에서 Orca `border-2`에 가깝게 잡았다.
const RING_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="7" xmlns="http://www.w3.org/2000/svg"><path d="M20.5 12A8.5 8.5 0 1 1 12 3.5"/></svg>"#;
/// 접근성 감속(reduce motion)에서는 멈춘 3/4 링이 고장처럼 보여 닫힌 링을 그린다(Orca와 같은 의도).
const RING_FULL_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="7" xmlns="http://www.w3.org/2000/svg"><circle cx="12" cy="12" r="8.5"/></svg>"#;
/// Lucide `bell`(ISC License)을 채운 모양. 미확인 완료 표시.
const BELL_FILLED_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" fill="currentColor" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" xmlns="http://www.w3.org/2000/svg"><path d="M10.268 21a2 2 0 0 0 3.464 0"/><path d="M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326"/></svg>"#;

/// 1초에 한 바퀴, 12스텝. 모든 스피너가 앱 공용 시계(`repeat_synced`)를 따라 같은 위상으로 돈다.
const SPIN_STEPS: f32 = 12.;

/// 워크트리 행에 보이는 상태 묶음 수와 묶음마다 겹쳐 그리는 에이전트 아이콘 수.
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
        Glyph::Spinner => icon(RING_SVG, size.inner(), color)
            .with_animation(
                "agent-spinner",
                Animation::new(Duration::from_secs(1))
                    .repeat_synced()
                    .with_max_fps(SPIN_STEPS)
                    .with_easing(|t| (t * SPIN_STEPS).floor() / SPIN_STEPS),
                |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
            )
            .into_any_element(),
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
    if agent.is_empty() {
        Icon::new(IconName::SquareTerminal)
            .with_size(px(size_px))
            .text_color(fg)
            .into_any_element()
    } else {
        icons::agent_brand_icon(agent, size_px, fg).into_any_element()
    }
}

/// 같은 상태의 세션 묶음.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group<'a> {
    pub state: AgentState,
    /// 겹쳐 그릴 서로 다른 에이전트(많은 순, 같으면 먼저 나온 순, 최대 [`MAX_GROUP_ICONS`]).
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

/// 워크트리 행의 상태 묶음: [글리프][겹친 에이전트 아이콘][+N] … [+N].
pub fn summary_element(summary: &Summary<'_>, cx: &App) -> gpui::Div {
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
        .children(summary.groups.iter().map(|g| {
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .px_1p5()
                .py_0p5()
                .rounded(RADIUS_XS)
                .bg(p.worktree_sidebar_accent.opacity(0.65))
                .child(status_glyph(g.state, false, Size::Sm, cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .children(g.icons.iter().enumerate().map(|(i, a)| {
                            div()
                                .size(STATUS_GROUP_ICON)
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .when(i > 0, |d| d.ml(STATUS_GROUP_OVERLAP).bg(p.worktree_sidebar))
                                .child(agent_icon(a, super::ICON_SM, p.muted_foreground))
                        })),
                )
                .when(g.more > 0, |d| d.child(more(g.more)))
        }))
        .when(summary.hidden > 0, |d| d.child(more(summary.hidden)))
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
    fn state_maps_to_glyph() {
        assert_eq!(glyph(AgentState::Working), Glyph::Spinner);
        assert_eq!(glyph(AgentState::Waiting), Glyph::Question);
        assert_eq!(glyph(AgentState::Blocked), Glyph::Question);
        assert_eq!(glyph(AgentState::Done), Glyph::Check);
        assert_eq!(glyph(AgentState::Exited), Glyph::Dot);
        assert_eq!(glyph(AgentState::Unknown), Glyph::Dot);
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
