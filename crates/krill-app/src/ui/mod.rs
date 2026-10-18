//! UI 디자인 시스템 모듈(OWE-575).
//!
//! 색 토큰·타이포그래피·간격·반경·창 크롬 상수는 [`theme`], 공용 레이아웃·컴포넌트 프리미티브는 [`primitives`],
//! 브랜드·Lucide 아이콘은 [`icons`], 에이전트·워크트리 상태 인디케이터는 [`status`]에 모은다.
//! 모든 UI/UX 작업은 [docs/STYLEGUIDE.md] 규범과 이 모듈의 토큰·프리미티브를 따른다.

pub mod icons;
pub mod primitives;
pub mod status;
pub mod theme;

/// macOS 메뉴바 템플릿 아이콘. 앱과 같은 Lucide 자산을 쓴다.
pub const MENU_BAR_ICON: gpui_kit_assets::IconName = gpui_kit_assets::IconName::SquareTerminal;
pub const MENU_BAR_ICON_SIZE: i32 = 18;

pub use primitives::{
    BadgeTone, LabelDesc, badge, button_with_tooltip, card, controls, danger_confirm, error_banner,
    field_label, ghost, keycap, label, modal_backdrop, primary, row, secondary, section_title,
    segment, segmented_control, switch, tooltip_view,
};
pub use theme::{
    AGENT_HISTORY_MAX_HEIGHT, AGENT_HISTORY_WIDTH, AGENT_UNREAD, COMMAND_PALETTE_WIDTH, DARK,
    DASHBOARD_SIZE, DIALOG_WIDTH, DIVIDER_HIT_PX, FLOATING_SIZE, FONT_MONO, FONT_UI, LIGHT,
    MODAL_BACKDROP, PANEL_DEFAULT_WIDTH, Palette, RADIUS_LG, RADIUS_MD, RADIUS_SM, RADIUS_XL,
    RADIUS_XS, SETTINGS_CONTENT_MAX_WIDTH, SETTINGS_MODAL_WIDTH, SETTINGS_NAV_WIDTH,
    SIDEBAR_DEFAULT_WIDTH, SIDEBAR_EMPTY_PY, SIDEBAR_HEADER_HEIGHT, SPACE_1, SPACE_2, SPACE_3,
    SPACE_4, STATE_DOT, STATUS_BAR_HEIGHT, SWITCH_HEIGHT, SWITCH_THUMB, SWITCH_WIDTH,
    TAB_BAR_HEIGHT, TAB_INACTIVE_OPACITY, TAB_MAX_WIDTH, TERMINAL_DARK, TERMINAL_LIGHT, TEXT_2XS,
    TEXT_LG, TEXT_MD, TEXT_SM, TEXT_XS, TITLEBAR_HEIGHT, TRAFFIC_LIGHT_INSET, TRAFFIC_LIGHT_PAD,
    agent_state_color, apply, git_status_color, hex, hexa, palette, palette_for, sync,
    tab_tag_color, terminal_surface, terminal_surface_alpha, terminal_theme,
};
