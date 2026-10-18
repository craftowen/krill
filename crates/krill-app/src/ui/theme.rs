//! Orca 수준 디자인 토큰 체계(OWE-575): 라이트/다크 색 팔레트, 타이포그래피, 간격·반경, 창 크롬 상수.
//!
//! 색 토큰과 치수 체계는 `.ref/orca/src/renderer/src/assets/main.css` (`:root` / `.dark`) 및
//! `.ref/orca/src/shared/terminal-themes/defaults.ts`에서 가져와 순수 Rust/gpui 상수로 옮겼다.
//! 출처·저작권: Copyright (c) 2026 Lovecast Inc. (MIT License, `.ref/orca/LICENSE`).

use gpui::{App, Hsla, Pixels, Point, Rems, SharedString, TitlebarOptions, point, px, rems};
use gpui_component::{ActiveTheme as _, Theme, ThemeMode};
use krill_proto::control::AgentState;
use krill_proto::theme::{Rgb, Theme as TerminalTheme};

/// UI 크롬(사이드바·탭 바·패널·대화상자·상태 바) 기본 산세리프 글꼴.
/// `gpui-component`가 macOS에서 SF Pro(`.AppleSystemUIFont`)로 해석한다.
pub const FONT_UI: &str = ".SystemUIFont";

/// 터미널 격자·코드·Diff hunk·경로·단축키 배지 전용 고정폭 글꼴.
#[cfg(not(windows))]
pub const FONT_MONO: &str = "Menlo";
/// Windows는 모든 버전에 있는 Consolas(Cascadia Mono는 Windows 10에 없을 수 있고, 없는 이름이면 gpui가 비고정폭 시스템
/// 글꼴로 떨어진다). 한글처럼 없는 글리프는 DirectWrite 시스템 폴백(맑은 고딕)이 채운다(OWE-828 실측).
#[cfg(windows)]
pub const FONT_MONO: &str = "Consolas";

/// 글자 크기 단계(Orca `text-[10px]` ~ `text-sm` 대응, 창의 `rem_size`/줌을 따른다).
pub const TEXT_2XS: Rems = rems(10. / 16.);
pub const TEXT_XS: Rems = rems(11. / 16.);
pub const TEXT_SM: Rems = rems(12. / 16.);
pub const TEXT_MD: Rems = rems(13. / 16.);
pub const TEXT_LG: Rems = rems(14. / 16.);

/// 모서리 반경 단계(Orca `--radius: 0.625rem` = 10px 기준).
pub const RADIUS_XS: Pixels = px(4.);
pub const RADIUS_SM: Pixels = px(6.);
pub const RADIUS_MD: Pixels = px(8.);
pub const RADIUS_LG: Pixels = px(10.);
pub const RADIUS_XL: Pixels = px(14.);

/// 기본 간격 단계(4px 격자).
pub const SPACE_1: Pixels = px(4.);
pub const SPACE_2: Pixels = px(8.);
pub const SPACE_3: Pixels = px(12.);
pub const SPACE_4: Pixels = px(16.);

/// 투명 타이틀바 높이(Orca `h-[36px]`).
pub const TITLEBAR_HEIGHT: Pixels = px(36.);
/// macOS 신호등(닫기·최소화·전체화면) 위치 inset.
pub const TRAFFIC_LIGHT_INSET: Point<Pixels> = point(px(12.), px(10.));
/// 신호등 오른쪽에 오는 타이틀바 첫 컨트롤의 왼쪽 여백(Orca `pl-20` 대응). 신호등이 없는 OS는 기본 간격만 둔다.
pub const TRAFFIC_LIGHT_PAD: Pixels = if cfg!(target_os = "macos") {
    px(78.)
} else {
    SPACE_2
};

/// 창 제목 표시줄(OWE-834). macOS는 투명 제목 표시줄 위에 신호등을 띄우고 앱이 상단 막대를 그린다. 그 밖의 OS는 OS 기본
/// 제목 표시줄을 쓴다 — 최소화·최대화·닫기 버튼, 드래그, 더블클릭 최대화, 스냅 레이아웃을 OS가 맡는다.
pub fn titlebar(title: impl Into<SharedString>) -> TitlebarOptions {
    TitlebarOptions {
        title: Some(title.into()),
        appears_transparent: cfg!(target_os = "macos"),
        traffic_light_position: cfg!(target_os = "macos").then_some(TRAFFIC_LIGHT_INSET),
    }
}
/// 탭 바 높이(1단 통합 상단 바와 동일하게 36px).
pub const TAB_BAR_HEIGHT: Pixels = px(36.);
/// Orca 탭 너비(`tab-width-rules.ts`: 기본 180px·넓은 화면 220px·최소 72px).
/// `gpui-component` `TabBar`의 `max_width`는 탭당 최대라 넓은 화면 값(220px)을 쓴다.
/// 내용 기반 너비라 한 탭 제목이 바뀌면 다른 탭 너비도 바뀐다(Orca는 고정폭) — API에 탭당 고정폭이 없어 고정하지 못한다.
pub const TAB_MAX_WIDTH: Pixels = px(220.);
/// 비활성 탭 앞부분(에이전트 아이콘·상태 점) 투명도(Orca 비활성 `opacity-70`).
pub const TAB_INACTIVE_OPACITY: f32 = 0.7;
/// 탭·사이드바 상태 점 지름(Orca `size-1.5`).
pub const STATE_DOT: Pixels = px(6.);
/// 미확인 완료 벨·점 색(Orca `amber-500`, OWE-662).
pub const AGENT_UNREAD: Hsla = hex(0x00f5_9e0b);
/// 모달 대화상자 스크림 배경(Orca `DialogOverlay` `bg-black/55`).
pub const MODAL_BACKDROP: Hsla = hexa(0x0000_0000, 0.55);
/// QR 코드 밝은 칸·어두운 칸. 테마와 무관하게 고정해야 카메라가 읽는다(M9 모바일 페어링).
pub const QR_LIGHT: Hsla = hex(0x00ff_ffff);
pub const QR_DARK: Hsla = hex(0x0000_0000);
/// 사이드바 헤더 높이(Orca `h-8`).
pub const SIDEBAR_HEADER_HEIGHT: Pixels = px(32.);
/// 사이드바 빈 상태 상하 여백(Orca `py-6`).
pub const SIDEBAR_EMPTY_PY: Pixels = px(24.);
/// 하단 상태 바 높이(Orca `h-6` = 24px).
pub const STATUS_BAR_HEIGHT: Pixels = px(24.);
/// 창 가운데 모달 대화상자 기본 너비(Orca `DialogContent` `sm:max-w-lg` = 512px).
pub const DIALOG_WIDTH: Pixels = px(512.);
/// 상단 Jump/커맨드 팔레트 기본 너비(520px).
pub const COMMAND_PALETTE_WIDTH: Pixels = px(520.);

/// MK-098 팝아웃 기본 크기. 두 번째 창의 drawable 예산을 제한한다.
pub const FLOATING_SIZE: gpui::Size<Pixels> = gpui::Size {
    width: px(640.),
    height: px(400.),
};
pub const DASHBOARD_SIZE: gpui::Size<Pixels> = gpui::Size {
    width: px(760.),
    height: px(480.),
};

/// 왼쪽 사이드바 기본 폭(pt).
pub const SIDEBAR_DEFAULT_WIDTH: f32 = 240.0;
/// 오른쪽 패널 기본 폭(pt).
pub const PANEL_DEFAULT_WIDTH: f32 = 280.0;
/// MK-082 세션 히스토리 팝오버와 스크롤 목록.
pub const AGENT_HISTORY_WIDTH: Pixels = px(520.);
pub const AGENT_HISTORY_MAX_HEIGHT: Pixels = px(360.);
/// 분할선 마우스 드래그 잡는 폭(pt). 시각적 선은 1pt로 그린다.
pub const DIVIDER_HIT_PX: Pixels = px(5.);

/// 0..=255 RGBA 바이트를 `Hsla` 상수로 변환한다.
const fn rgba_bytes(red: u8, green: u8, blue: u8, alpha: u8) -> Hsla {
    let max_u8 = if red >= green && red >= blue {
        red
    } else if green >= blue {
        green
    } else {
        blue
    };
    let min_u8 = if red <= green && red <= blue {
        red
    } else if green <= blue {
        green
    } else {
        blue
    };
    let rf = red as f32 / 255.0;
    let gf = green as f32 / 255.0;
    let bf = blue as f32 / 255.0;
    let af = alpha as f32 / 255.0;
    let max = max_u8 as f32 / 255.0;
    let min = min_u8 as f32 / 255.0;
    let lightness = f32::midpoint(max, min);
    if max_u8 == min_u8 {
        return gpui::hsla(0.0, 0.0, lightness, af);
    }
    let delta = max - min;
    let sat = if lightness > 0.5 {
        delta / (2.0 - max - min)
    } else {
        delta / (max + min)
    };
    let mut hue = if max_u8 == red {
        (gf - bf) / delta + if gf < bf { 6.0 } else { 0.0 }
    } else if max_u8 == green {
        (bf - rf) / delta + 2.0
    } else {
        (rf - gf) / delta + 4.0
    };
    hue /= 6.0;
    gpui::hsla(hue, sat, lightness, af)
}

/// 24비트 RGB 정수(`0x00RR_GGBB`)를 불투명 [`Hsla`]로 바꾼다.
pub const fn hex(rgb: u32) -> Hsla {
    rgba_bytes(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
        0xff,
    )
}

/// 24비트 RGB 정수(`0x00RR_GGBB`)에 투명도 `alpha`(0.0..=1.0)를 입힌 [`Hsla`]를 만든다.
pub const fn hexa(rgb: u32, alpha: f32) -> Hsla {
    let mut c = hex(rgb);
    c.a = alpha;
    c
}

const fn term_rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

/// Orca `.ref/orca/src/renderer/src/assets/main.css`의 시맨틱 색 토큰.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub background: Hsla,
    pub editor_surface: Hsla,
    pub foreground: Hsla,
    pub card: Hsla,
    pub card_foreground: Hsla,
    pub popover: Hsla,
    pub popover_foreground: Hsla,
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    pub secondary: Hsla,
    pub secondary_foreground: Hsla,
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    pub accent: Hsla,
    pub accent_foreground: Hsla,
    pub destructive: Hsla,
    pub destructive_foreground: Hsla,
    pub border: Hsla,
    pub input: Hsla,
    pub ring: Hsla,
    pub sidebar: Hsla,
    pub sidebar_foreground: Hsla,
    pub sidebar_primary: Hsla,
    pub sidebar_primary_foreground: Hsla,
    pub sidebar_accent: Hsla,
    pub sidebar_accent_foreground: Hsla,
    pub sidebar_border: Hsla,
    pub sidebar_ring: Hsla,
    pub worktree_sidebar: Hsla,
    pub worktree_sidebar_foreground: Hsla,
    pub worktree_sidebar_accent: Hsla,
    pub worktree_sidebar_border: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub warning_foreground: Hsla,
    pub agent_question: Hsla,
    pub split_divider: Hsla,
    pub split_divider_strong: Hsla,
}

/// Orca 라이트 모드(`:root`) 팔레트.
pub const LIGHT: Palette = Palette {
    background: hex(0x00ff_ffff),
    editor_surface: hex(0x00ff_ffff),
    foreground: hex(0x000a_0a0a),
    card: hex(0x00ff_ffff),
    card_foreground: hex(0x000a_0a0a),
    popover: hex(0x00ff_ffff),
    popover_foreground: hex(0x000a_0a0a),
    primary: hex(0x0017_1717),
    primary_foreground: hex(0x00fa_fafa),
    secondary: hex(0x00f5_f5f5),
    secondary_foreground: hex(0x0017_1717),
    muted: hex(0x00f5_f5f5),
    muted_foreground: hex(0x0073_7373),
    accent: hex(0x00f5_f5f5),
    accent_foreground: hex(0x0017_1717),
    destructive: hex(0x00e4_0014),
    destructive_foreground: hex(0x00fc_f3f3),
    border: hex(0x00e5_e5e5),
    input: hex(0x00e5_e5e5),
    ring: hex(0x00a1_a1a1),
    sidebar: hex(0x00fa_fafa),
    sidebar_foreground: hex(0x000a_0a0a),
    sidebar_primary: hex(0x0017_1717),
    sidebar_primary_foreground: hex(0x00fa_fafa),
    sidebar_accent: hex(0x00f0_f0f0),
    sidebar_accent_foreground: hex(0x0017_1717),
    sidebar_border: hex(0x00e5_e5e5),
    sidebar_ring: hex(0x00a1_a1a1),
    worktree_sidebar: hex(0x00f5_f5f5),
    worktree_sidebar_foreground: hex(0x000a_0a0a),
    worktree_sidebar_accent: hex(0x00ea_eaea),
    worktree_sidebar_border: hex(0x00e5_e5e5),
    success: hex(0x0015_803d),
    warning: hex(0x00ca_8a04),
    warning_foreground: hex(0x00fa_fafa),
    agent_question: hex(0x00ea_580c),
    split_divider: hex(0x0086_8690),
    split_divider_strong: hex(0x0071_717a),
};

/// Orca 다크 모드(`.dark`) 팔레트.
pub const DARK: Palette = Palette {
    background: hex(0x000a_0a0a),
    editor_surface: hex(0x001e_1e1e),
    foreground: hex(0x00fa_fafa),
    card: hex(0x0017_1717),
    card_foreground: hex(0x00fa_fafa),
    popover: hex(0x0017_1717),
    popover_foreground: hex(0x00fa_fafa),
    primary: hex(0x00e5_e5e5),
    primary_foreground: hex(0x0017_1717),
    secondary: hex(0x0026_2626),
    secondary_foreground: hex(0x00fa_fafa),
    muted: hex(0x0026_2626),
    muted_foreground: hex(0x00a1_a1a1),
    accent: hex(0x0030_3030),
    accent_foreground: hex(0x00fa_fafa),
    destructive: hex(0x00ff_6568),
    destructive_foreground: hex(0x00fa_fafa),
    border: hexa(0x00ff_ffff, 0.08),
    input: hexa(0x00ff_ffff, 0.15),
    ring: hex(0x0073_7373),
    sidebar: hex(0x0017_1717),
    sidebar_foreground: hex(0x00fa_fafa),
    sidebar_primary: hex(0x0014_47e6),
    sidebar_primary_foreground: hex(0x00fa_fafa),
    sidebar_accent: hex(0x0026_2626),
    sidebar_accent_foreground: hex(0x00fa_fafa),
    sidebar_border: hexa(0x00ff_ffff, 0.07),
    sidebar_ring: hex(0x0052_5252),
    worktree_sidebar: hex(0x002a_2a2a),
    worktree_sidebar_foreground: hex(0x00fa_fafa),
    worktree_sidebar_accent: hex(0x0035_3535),
    worktree_sidebar_border: hexa(0x00ff_ffff, 0.07),
    success: hex(0x0086_efac),
    warning: hex(0x00ea_b308),
    warning_foreground: hex(0x0017_1717),
    agent_question: hex(0x00f9_7316),
    split_divider: hex(0x0071_717a),
    split_divider_strong: hex(0x00a1_a1aa),
};

/// Orca 다크 터미널 팔레트(`Ghostty Default Style Dark`, `.ref/orca/src/shared/terminal-themes/defaults.ts`).
pub const TERMINAL_DARK: TerminalTheme = TerminalTheme {
    foreground: term_rgb(0xff, 0xff, 0xff),
    background: term_rgb(0x28, 0x2c, 0x34),
    cursor: term_rgb(0xff, 0xff, 0xff),
    ansi: [
        term_rgb(0x1d, 0x1f, 0x21),
        term_rgb(0xcc, 0x66, 0x66),
        term_rgb(0xb5, 0xbd, 0x68),
        term_rgb(0xf0, 0xc6, 0x74),
        term_rgb(0x81, 0xa2, 0xbe),
        term_rgb(0xb2, 0x94, 0xbb),
        term_rgb(0x8a, 0xbe, 0xb7),
        term_rgb(0xc5, 0xc8, 0xc6),
        term_rgb(0x66, 0x66, 0x66),
        term_rgb(0xd5, 0x4e, 0x53),
        term_rgb(0xb9, 0xca, 0x4a),
        term_rgb(0xe7, 0xc5, 0x47),
        term_rgb(0x7a, 0xa6, 0xda),
        term_rgb(0xc3, 0x97, 0xd8),
        term_rgb(0x70, 0xc0, 0xb1),
        term_rgb(0xea, 0xea, 0xea),
    ],
};

/// Orca 라이트 터미널 팔레트(`Builtin Tango Light`, `.ref/orca/src/shared/terminal-themes/defaults.ts`).
pub const TERMINAL_LIGHT: TerminalTheme = TerminalTheme {
    foreground: term_rgb(0x00, 0x00, 0x00),
    background: term_rgb(0xff, 0xff, 0xff),
    cursor: term_rgb(0x00, 0x00, 0x00),
    ansi: [
        term_rgb(0x00, 0x00, 0x00),
        term_rgb(0xcc, 0x00, 0x00),
        term_rgb(0x4e, 0x9a, 0x06),
        term_rgb(0xc4, 0xa0, 0x00),
        term_rgb(0x34, 0x65, 0xa4),
        term_rgb(0x75, 0x50, 0x7b),
        term_rgb(0x06, 0x98, 0x9a),
        term_rgb(0xd3, 0xd7, 0xcf),
        term_rgb(0x55, 0x57, 0x53),
        term_rgb(0xef, 0x29, 0x29),
        term_rgb(0x8a, 0xe2, 0x34),
        term_rgb(0xfc, 0xe9, 0x4f),
        term_rgb(0x72, 0x9f, 0xcf),
        term_rgb(0xad, 0x7f, 0xa8),
        term_rgb(0x34, 0xe2, 0xe2),
        term_rgb(0xee, 0xee, 0xec),
    ],
};

/// 터미널 칸 여백도 셀과 같은 테마 배경을 쓴다.
pub const fn terminal_surface(theme: TerminalTheme) -> Hsla {
    rgba_bytes(
        theme.background.r,
        theme.background.g,
        theme.background.b,
        0xff,
    )
}

/// 설정 불투명도(`terminal_opacity_pct`, 50..=100)를 입힌 터미널 배경.
/// 범위 밖은 `50..=100`으로 자른다. 글자·장식색은 그대로 둔다.
pub fn terminal_surface_alpha(theme: TerminalTheme, opacity_pct: u8) -> Hsla {
    terminal_surface(theme).opacity(f32::from(opacity_pct.clamp(50, 100)) / 100.)
}

pub fn terminal_theme(cx: &App) -> TerminalTheme {
    if cx
        .try_global::<Theme>()
        .is_some_and(|t| t.mode == ThemeMode::Light)
    {
        TERMINAL_LIGHT
    } else {
        TERMINAL_DARK
    }
}

/// 주어진 [`ThemeMode`]에 대응하는 Orca [`Palette`]를 돌려준다.
#[inline]
pub const fn palette_for(mode: ThemeMode) -> &'static Palette {
    if matches!(mode, ThemeMode::Dark) {
        &DARK
    } else {
        &LIGHT
    }
}

/// 현재 앱의 활성 [`ThemeMode`]에 맞는 Orca [`Palette`]를 돌려준다.
/// `gpui_component::init`를 부르지 않은 단위 테스트에서도 패닉 없이 [`DARK`]로 동작한다.
#[inline]
pub fn palette(cx: &App) -> &'static Palette {
    if cx.has_global::<Theme>() {
        palette_for(cx.theme().mode)
    } else {
        &DARK
    }
}

/// 에이전트 상태별 인디케이터 점/배지 색.
pub fn agent_state_color(state: AgentState, cx: &App) -> Hsla {
    let p = palette(cx);
    match state {
        // OWE-662: 작업 중 스피너는 Orca처럼 노랑(`yellow-500` = warning 토큰).
        AgentState::Working => p.warning,
        AgentState::Waiting => p.agent_question,
        AgentState::Blocked => p.destructive,
        AgentState::Done => p.success,
        AgentState::Exited | AgentState::Unknown => p.muted_foreground,
    }
}

/// Git 변경 상태(`porcelain v2` XY 2바이트)별 배지 색.
pub fn git_status_color(status: [u8; 2], cx: &App) -> Hsla {
    let p = palette(cx);
    match status {
        [b'A', _] | [_, b'A'] | [b'?', b'?'] => p.success,
        [b'M', _] | [_, b'M'] => p.warning,
        [b'D' | b'U', _] | [_, b'D' | b'U'] => p.destructive,
        [b'R' | b'C', _] | [_, b'R' | b'C'] => p.sidebar_primary,
        _ => p.muted_foreground,
    }
}

/// 탭 9색 태그(`TabColor`, Orca `TabBar.tsx` `TAB_COLORS` 패리티) 시맨틱 색상 토큰(OWE-734).
#[must_use]
pub const fn tab_tag_color(color: crate::layout::TabColor) -> Hsla {
    use crate::layout::TabColor;
    match color {
        TabColor::Blue => hex(0x003b_82f6),
        TabColor::Purple => hex(0x00a8_55f7),
        TabColor::Pink => hex(0x00ec_4899),
        TabColor::Red => hex(0x00ef_4444),
        TabColor::Orange => hex(0x00f9_7316),
        TabColor::Yellow => hex(0x00ea_b308),
        TabColor::Green => hex(0x0022_c55e),
        TabColor::Teal => hex(0x0014_b8a6),
        TabColor::Gray => hex(0x009c_a3af),
    }
}

/// `gpui-component`의 전역 [`Theme`]에 `mode`와 Orca 색·글꼴·반경 토큰을 적용한다.
pub fn apply(mode: ThemeMode, cx: &mut App) {
    Theme::change(mode, None, cx);
    let p = palette_for(mode);
    let rem = px(16. * crate::settings::current(cx).zoom());
    Theme::update(cx, |theme| {
        theme.font_family = FONT_UI.into();
        theme.mono_font_family = FONT_MONO.into();
        theme.font_size = TEXT_MD.to_pixels(rem);
        theme.mono_font_size = TEXT_SM.to_pixels(rem);
        theme.radius = RADIUS_SM;
        theme.radius_lg = RADIUS_LG;

        theme.colors.background = p.background;
        theme.colors.foreground = p.foreground;
        theme.colors.group_box = p.card;
        theme.colors.group_box_foreground = p.card_foreground;
        theme.colors.popover = p.popover;
        theme.colors.popover_foreground = p.popover_foreground;
        theme.colors.primary = p.primary;
        theme.colors.primary_foreground = p.primary_foreground;
        theme.colors.secondary = p.secondary;
        theme.colors.secondary_foreground = p.secondary_foreground;
        theme.colors.muted = p.muted;
        theme.colors.muted_foreground = p.muted_foreground;
        theme.colors.accent = p.accent;
        theme.colors.accent_foreground = p.accent_foreground;
        theme.colors.border = p.border;
        theme.colors.input = p.input;
        theme.colors.ring = p.ring;

        theme.colors.sidebar = p.sidebar;
        theme.colors.sidebar_foreground = p.sidebar_foreground;
        theme.colors.sidebar_primary = p.sidebar_primary;
        theme.colors.sidebar_primary_foreground = p.sidebar_primary_foreground;
        theme.colors.sidebar_accent = p.sidebar_accent;
        theme.colors.sidebar_accent_foreground = p.sidebar_accent_foreground;
        theme.colors.sidebar_border = p.sidebar_border;

        theme.colors.title_bar = p.sidebar;
        theme.colors.title_bar_border = p.border;
        theme.colors.tab_bar = p.sidebar;
        theme.colors.tab = p.sidebar;
        theme.colors.tab_foreground = p.muted_foreground;
        theme.colors.tab_active = p.background;
        theme.colors.tab_active_foreground = p.foreground;

        theme.colors.danger = p.destructive;
        theme.colors.danger_foreground = p.destructive_foreground;
        theme.colors.warning = p.warning;
        theme.colors.warning_foreground = p.warning_foreground;
        theme.colors.success = p.success;
        theme.colors.success_foreground = p.primary_foreground;

        theme.colors.list = p.sidebar;
        theme.colors.list_hover = p.sidebar_accent;
        theme.colors.list_active = p.sidebar_accent;
        theme.colors.list_active_border = p.sidebar_border;
    });
}

/// 현재 `cx.theme().mode`를 유지한 채 Orca 토큰을 [`Theme`]에 동기화한다.
pub fn sync(cx: &mut App) {
    apply(cx.theme().mode, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_conversion_matches_gpui_rgb() {
        for raw in [
            0x0000_0000,
            0x00ff_ffff,
            0x000a_0a0a,
            0x0017_1717,
            0x0014_47e6,
            0x00e4_0014,
        ] {
            let a = hex(raw);
            let b = Hsla::from(gpui::rgb(raw));
            assert!((a.h - b.h).abs() < 1e-5, "{raw:#08x}: h {} vs {}", a.h, b.h);
            assert!((a.s - b.s).abs() < 1e-5, "{raw:#08x}: s {} vs {}", a.s, b.s);
            assert!((a.l - b.l).abs() < 1e-5, "{raw:#08x}: l {} vs {}", a.l, b.l);
            assert!((a.a - 1.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn palettes_distinguish_light_and_dark_surfaces() {
        let light = palette_for(ThemeMode::Light);
        let dark = palette_for(ThemeMode::Dark);
        assert!(light.background.l > 0.95);
        assert!(dark.background.l < 0.10);
        assert!(light.foreground.l < 0.10);
        assert!(dark.foreground.l > 0.95);
        assert_eq!(light, &LIGHT);
        assert_eq!(dark, &DARK);
    }

    #[gpui::test]
    fn apply_installs_orca_tokens_on_global_theme(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            apply(ThemeMode::Dark, cx);
            assert_eq!(cx.theme().mode, ThemeMode::Dark);
            assert_eq!(cx.theme().background, DARK.background);
            assert_eq!(cx.theme().sidebar, DARK.sidebar);
            assert_eq!(cx.theme().radius, RADIUS_SM);

            apply(ThemeMode::Light, cx);
            assert_eq!(cx.theme().mode, ThemeMode::Light);
            assert_eq!(cx.theme().background, LIGHT.background);
            assert_eq!(cx.theme().sidebar, LIGHT.sidebar);
        });
    }

    /// Orca 정합 토큰 회귀 방지: 탭 최대폭 220px·비활성 투명도 0.7·상태 점 6px·사이드바 헤더 32px·빈 상태 여백 24px.
    #[test]
    fn orca_parity_tokens() {
        assert_eq!(TAB_MAX_WIDTH, px(220.));
        assert!((TAB_INACTIVE_OPACITY - 0.7).abs() < f32::EPSILON);
        assert_eq!(STATE_DOT, px(6.));
        assert_eq!(SIDEBAR_HEADER_HEIGHT, px(32.));
        assert_eq!(SIDEBAR_EMPTY_PY, px(24.));
        assert_eq!(TITLEBAR_HEIGHT, TAB_BAR_HEIGHT);
    }

    /// OWE-655: 터미널 배경 불투명도는 50..=100으로 자르고 그 외 색은 그대로 둔다.
    #[test]
    fn terminal_surface_alpha_clamps_and_scales() {
        let opaque = terminal_surface_alpha(TERMINAL_DARK, 100);
        assert!((opaque.a - 1.).abs() < 1e-6);
        let half = terminal_surface_alpha(TERMINAL_DARK, 50);
        assert!((half.a - 0.5).abs() < 1e-6);
        assert!((half.h - opaque.h).abs() < 1e-6);
        assert_eq!(terminal_surface_alpha(TERMINAL_DARK, 0), half);
        assert_eq!(terminal_surface_alpha(TERMINAL_DARK, 200), opaque);
        assert!((terminal_surface_alpha(TERMINAL_DARK, 75).a - opaque.a * 0.75).abs() < 1e-6);
    }

    fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let entries = std::fs::read_dir(dir).unwrap();
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// 주석(`//`, 중첩 가능한 `/* ... */`)과 문자열 리터럴(`"..."`, `r#"..."#`)의 내용만 공백으로 지우고
    /// 줄바꿈(`\n`)은 그대로 유지해 줄 번호가 어긋나지 않게 한다.
    fn strip_comments_and_strings(src: &str) -> String {
        let bytes = src.as_bytes();
        let mut out = String::with_capacity(src.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                let mut depth = 1usize;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        if bytes[i] == b'\n' {
                            out.push('\n');
                        }
                        i += 1;
                    }
                }
                continue;
            }
            // raw string: r"...", r#"..."#, br"...", br#"..."#
            let raw_start = if bytes[i] == b'r' {
                Some(i + 1)
            } else if bytes[i] == b'b' && bytes.get(i + 1) == Some(&b'r') {
                Some(i + 2)
            } else {
                None
            };
            if let Some(mut j) = raw_start {
                let mut hashes = 0usize;
                while bytes.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if (hashes > 0
                    || bytes
                        .get(i.wrapping_sub(1))
                        .is_none_or(|b| !b.is_ascii_alphanumeric() && *b != b'_'))
                    && bytes.get(j) == Some(&b'"')
                {
                    out.push_str("\"\"");
                    i = j + 1;
                    while i < bytes.len() {
                        if bytes[i] == b'\n' {
                            out.push('\n');
                            i += 1;
                            continue;
                        }
                        if bytes[i] == b'"'
                            && bytes
                                .get(i + 1..i + 1 + hashes)
                                .is_some_and(|s| s.iter().all(|&b| b == b'#'))
                        {
                            i += 1 + hashes;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
            }
            if bytes[i] == b'"' {
                out.push_str("\"\"");
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'\n' {
                        out.push('\n');
                    } else if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            let ch = src.get(i..).and_then(|rest| rest.chars().next()).unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }

    fn has_unqualified_call(line: &str, ident: &str) -> bool {
        let needle = format!("{ident}(");
        let mut start = 0;
        while let Some(pos) = line.get(start..).and_then(|rest| rest.find(&needle)) {
            let idx = start + pos;
            let prev = idx
                .checked_sub(1)
                .and_then(|p| line.as_bytes().get(p))
                .copied();
            let boundary_ok = prev
                .is_none_or(|b| !b.is_ascii_alphanumeric() && b != b'_' && b != b':' && b != b'.');
            let is_fn_decl = line
                .get(..idx)
                .is_some_and(|prefix| prefix.trim_end().ends_with("fn"));
            if boundary_ok && !is_fn_decl {
                return true;
            }
            start = idx + needle.len();
        }
        false
    }

    fn scan_design_system_violations(rel: &str, src: &str) -> Vec<String> {
        let stripped = strip_comments_and_strings(src);
        let forbidden = [
            "gpui::rgb(",
            "gpui::rgba(",
            "gpui::hsla(",
            "gpui::black(",
            "gpui::white(",
            "ui::hex(",
            "ui::hexa(",
            "theme::hex(",
            "theme::hexa(",
            ".text_size(px(",
            ".text_size(gpui::px(",
            ".text_size(rems(",
            ".text_size(gpui::rems(",
        ];
        let unqualified = ["rgb", "rgba", "hsla", "black", "white"];

        let mut violations = Vec::new();
        for (line_no, line) in stripped.lines().enumerate() {
            for pat in forbidden {
                if line.contains(pat) {
                    violations.push(format!(
                        "src/{rel}:{}: forbidden `{pat}` (use `crate::ui` tokens per docs/STYLEGUIDE.md)",
                        line_no + 1
                    ));
                }
            }
            for ident in unqualified {
                if has_unqualified_call(line, ident) {
                    violations.push(format!(
                        "src/{rel}:{}: forbidden `{ident}(` (use `crate::ui` tokens per docs/STYLEGUIDE.md)",
                        line_no + 1
                    ));
                }
            }
        }
        violations
    }

    /// 디자인 시스템 게이트(Orca `lint:design-system` 대응): `src/ui/`와 터미널 격자(`terminal.rs`) 밖에서
    /// 생 색상(`gpui::rgb`/`rgba`/`hsla`/`black`/`white`, 비수식 `rgb(` 등, `ui::hex`/`hexa`)이나
    /// 임의 글자 크기(`.text_size(px(..))`, `.text_size(rems(..))`)를 직접 쓰면 실패한다(`docs/STYLEGUIDE.md` §2·§4·§9).
    #[test]
    fn no_raw_colors_or_arbitrary_font_sizes_outside_ui_module() {
        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let ui_dir = src_dir.join("ui");
        let terminal_rs = src_dir.join("terminal.rs");
        let terminal_tests_rs = src_dir.join("terminal").join("tests.rs");
        let mut files = Vec::new();
        collect_rs_files(&src_dir, &mut files);
        files.sort();

        let mut violations = Vec::new();
        for file in files {
            if file.starts_with(&ui_dir) || file == terminal_rs || file == terminal_tests_rs {
                continue;
            }
            let content = std::fs::read_to_string(&file).unwrap();
            let rel = file.strip_prefix(&src_dir).unwrap_or(&file);
            violations.extend(scan_design_system_violations(
                &rel.display().to_string(),
                &content,
            ));
        }
        assert!(
            violations.is_empty(),
            "Design system violations found:\n{}",
            violations.join("\n")
        );
    }

    #[test]
    fn design_system_scanner_catches_unqualified_calls_and_ignores_comments_and_strings() {
        let clean = r##"
            // gpui::rgb(0x123456) in line comment
            /*
               gpui::rgb(0x123456) and .text_size(px(13.)) in block comment
               /* nested rgb(0x000000) */
            */
            let s = "gpui::rgb(0x123456) .text_size(px(12.)) rgb(0)";
            let r = r#"hsla(0., 0., 0., 1.) black()"#;
            div().bg(p.background).text_size(ui::TEXT_SM)
        "##;
        assert!(scan_design_system_violations("clean.rs", clean).is_empty());

        for bad in [
            "div().bg(gpui::rgb(0x112233))",
            "div().bg(rgb(0x112233))",
            "div().bg(rgba(0x112233ff))",
            "div().bg(hsla(0., 0., 0., 1.))",
            "div().bg(black())",
            "div().bg(white())",
            "div().bg(ui::hex(0x112233))",
            "div().text_size(px(13.))",
            "div().text_size(rems(0.75))",
        ] {
            let v = scan_design_system_violations("bad.rs", bad);
            assert!(!v.is_empty(), "expected violation for `{bad}`");
        }
    }

    /// `Palette` 구조체의 모든 시맨틱 색 필드와 핵심 토큰·프리미티브가 `docs/STYLEGUIDE.md`에 문서화되어 있고,
    /// 저장소 기본 개발 문서(`AGENTS.md`, `docs/CONVENTIONS.md`, `CONTRIBUTING.md`, `README.md`)가 이를 참조하는지 검사한다.
    #[test]
    fn styleguide_doc_stays_in_sync_with_palette_and_tokens() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.join("../..");
        let styleguide_path = root.join("docs/STYLEGUIDE.md");
        let styleguide = std::fs::read_to_string(&styleguide_path)
            .expect("docs/STYLEGUIDE.md must exist at workspace docs/STYLEGUIDE.md");
        let theme_src = std::fs::read_to_string(manifest.join("src/ui/theme.rs")).unwrap();

        let palette_block = theme_src
            .split("pub struct Palette {")
            .nth(1)
            .and_then(|s| s.split('}').next())
            .expect("Palette struct definition must exist in src/ui/theme.rs");

        let mut field_count = 0;
        for line in palette_block.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("pub ")
                && let Some((field, _)) = rest.split_once(':')
            {
                let field = field.trim();
                field_count += 1;
                assert!(
                    styleguide.contains(&format!("`{field}`")),
                    "Palette field `{field}` is missing from docs/STYLEGUIDE.md"
                );
            }
        }
        assert_eq!(
            field_count, 38,
            "unexpected Palette field count: {field_count}"
        );

        for required in [
            "FONT_UI",
            "FONT_MONO",
            "TEXT_2XS",
            "TEXT_XS",
            "TEXT_SM",
            "TEXT_MD",
            "TEXT_LG",
            "RADIUS_XS",
            "RADIUS_SM",
            "RADIUS_MD",
            "RADIUS_LG",
            "RADIUS_XL",
            "SPACE_1",
            "SPACE_2",
            "SPACE_3",
            "SPACE_4",
            "DIALOG_WIDTH",
            "COMMAND_PALETTE_WIDTH",
            "MODAL_BACKDROP",
            "AGENT_UNREAD",
            "ui::card",
            "ui::row",
            "ui::label",
            "ui::controls",
            "ui::keycap",
            "ui::badge",
            "ui::error_banner",
            "ui::danger_confirm",
            "ui::modal_backdrop",
        ] {
            assert!(
                styleguide.contains(required),
                "Design system token/primitive `{required}` must be documented in docs/STYLEGUIDE.md"
            );
        }

        for doc in [
            "AGENTS.md",
            "docs/CONVENTIONS.md",
            "CONTRIBUTING.md",
            "README.md",
        ] {
            let content = std::fs::read_to_string(root.join(doc))
                .unwrap_or_else(|e| panic!("failed to read {doc}: {e}"));
            assert!(
                content.contains("STYLEGUIDE.md"),
                "{doc} must reference docs/STYLEGUIDE.md by default"
            );
        }
    }
}
