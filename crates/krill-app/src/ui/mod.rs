//! UI 디자인 시스템 모듈(OWE-575).
//!
//! 색 토큰·타이포그래피·간격·반경·창 크롬 상수는 [`theme`], 공용 레이아웃·컴포넌트 프리미티브는 [`primitives`],
//! 브랜드·Lucide 아이콘은 [`icons`], 에이전트·워크트리 상태 인디케이터는 [`status`],
//! 터미널 테마 카탈로그·가져오기는 [`terminal_themes`]에 모은다.
//! 모든 UI/UX 작업은 [docs/STYLEGUIDE.md] 규범과 이 모듈의 토큰·프리미티브를 따른다.

mod app_themes;

pub use app_themes::TRENCH;

pub mod icons;
pub mod primitives;
pub mod status;
pub mod terminal_themes;
pub mod theme;

/// macOS 메뉴바 템플릿 아이콘. 앱과 같은 Lucide 자산을 쓴다.
pub const MENU_BAR_ICON: gpui_kit_assets::IconName = gpui_kit_assets::IconName::SquareTerminal;
pub const MENU_BAR_ICON_SIZE: i32 = 18;

pub use icons::{ICON_2XS, ICON_LG, ICON_MD, ICON_SM, ICON_XL, ICON_XS};
pub use primitives::{
    BadgeTone, LabelDesc, badge, button_with_reserved_label, button_with_tooltip,
    button_with_tooltip_when, card, controls, danger_confirm, error_banner, field_label, ghost,
    keycap, label, modal_backdrop, primary, row, secondary, section_title, segment,
    segmented_control, switch, tooltip_view,
};
pub use terminal_themes::{
    BUILTIN_TERMINAL_THEMES, BuiltinTerminalTheme, CustomTerminalTheme,
    DEFAULT_TERMINAL_THEME_DARK, DEFAULT_TERMINAL_THEME_LIGHT, builtin_terminal_theme,
    canonical_terminal_theme_name, discover_and_import_ghostty, discover_and_import_warp_themes,
    merge_custom_themes, parse_warp_theme_yaml, resolve_terminal_theme, terminal_rgb_hsla,
    terminal_theme_display_label,
};
pub use theme::{
    ABYSS, AGENT_HISTORY_MAX_HEIGHT, AGENT_HISTORY_WIDTH, AGENT_UNREAD, COMMAND_PALETTE_WIDTH,
    DARK, DASHBOARD_SIZE, DIALOG_WIDTH, DIVIDER_HIT_PX, DarkUiTheme, FLOATING_SIZE, FONT_MONO,
    FONT_UI, GLOW, LIGHT, LightUiTheme, MODAL_BACKDROP, PANE_HEADER_HEIGHT, PANEL_DEFAULT_WIDTH,
    PANEL_ROW_HEIGHT, PANEL_TAB_HEIGHT, PAPER, Palette, RADIUS_LG, RADIUS_MD, RADIUS_SM, RADIUS_XL,
    RADIUS_XS, SETTINGS_CONTENT_MAX_WIDTH, SETTINGS_MODAL_WIDTH, SETTINGS_NAV_WIDTH,
    SIDEBAR_DEFAULT_WIDTH, SIDEBAR_EMPTY_PY, SIDEBAR_HEADER_HEIGHT, SIDEBAR_ROW_HEIGHT, SPACE_1,
    SPACE_2, SPACE_3, SPACE_4, STATE_DOT, STATUS_BAR_HEIGHT, STATUS_GROUP_ICON,
    STATUS_GROUP_OVERLAP, SWITCH_HEIGHT, SWITCH_THUMB, SWITCH_WIDTH, TAB_BAR_HEIGHT,
    TAB_INACTIVE_OPACITY, TAB_MAX_WIDTH, TAB_MIN_WIDTH, TERMINAL_DARK, TERMINAL_GHOSTTY_DARK,
    TERMINAL_LIGHT, TEXT_2XS, TEXT_LG, TEXT_MD, TEXT_SM, TEXT_XS, TIDEPOOL, TITLEBAR_HEIGHT,
    TRAFFIC_LIGHT_INSET, TRAFFIC_LIGHT_PAD, TRANSPARENT, agent_state_color, apply,
    git_status_color, hex, hexa, palette, palette_for, sync, tab_tag_color, terminal_surface,
    terminal_surface_alpha, terminal_theme, terminal_theme_for_mode, titlebar, transparent,
};
