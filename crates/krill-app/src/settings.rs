//! 앱 설정(MK-070, MK-044): `settings.toml`(테마·UI 줌·알림·잠자기 방지)과 `keys.toml`(키 바인딩 재정의). 둘 다
//! 상태 디렉터리(`krill_proto::daemon::state_dir`)에 있고, 1초마다 수정 시각을 보고 바뀌면 재시작 없이 다시 적용한다.
//! 파일이 틀리면 직전 설정을 유지하고 오류를 설정 창에 보인다. 설정 창(⌘,)은 주요 항목을 고치고 파일 편집은 시스템 편집기에 맡긴다.
//!
//! 라이트/다크는 터미널 팔레트도 바꾸며 셸이 krilld에 전달한다(OWE-590). 줌은 rem 기반 UI 크기에 적용한다.

// Automations design reference: Orca (MIT, Copyright (c) 2026 Lovecast Inc.).
use krill_proto::os::fs::OpenOptionsExt as _;
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui::{
    App, AppContext as _, Bounds, Context, Global, KeyBinding, Keystroke, Render, SharedString,
    Subscription, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Sizable as _, ThemeMode};
use serde::{Deserialize, Serialize};

use crate::ui::{DarkUiTheme, LightUiTheme};

pub use crate::i18n::Language;

mod locale;
mod location;

const SETTINGS_FILE: &str = "settings.toml";
const KEYS_FILE: &str = "keys.toml";
/// gpui 창의 기본 rem 크기. 줌 1.0일 때 값.
const BASE_REM: f32 = 16.;
const ZOOM_STEP: f32 = 0.1;
const ZOOM_MIN: f32 = 0.5;
const ZOOM_MAX: f32 = 2.0;
const POLL: Duration = Duration::from_secs(1);

/// 사용자에게 보이는 ko/en 문자열 키(MK-094).
use crate::i18n::catalog::settings as text;

actions!(
    settings,
    [OpenSettings, ZoomIn, ZoomOut, ZoomReset, FocusSearch]
);

/// 테마 선택. 기본값은 Orca와 동일한 `System`이며, `System`은 적용 시점의 macOS 모양을 따른다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

/// 잠자기 방지 모드(MK-044, `IOPMAssertion` On/Agent/Off).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepAwake {
    /// 앱이 켜져 있는 동안 항상 유휴 잠자기 방지.
    On,
    /// `working` 상태인 세션이 하나라도 있을 때만 유휴 잠자기 방지.
    Agent,
    /// 잠자기 방지를 쓰지 않음(기본값).
    #[default]
    Off,
}

/// `settings.toml`. 빠진 키는 기본값.
#[expect(
    clippy::struct_excessive_bools,
    reason = "settings.toml 플랫 스키마의 독립 토글 필드들"
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    /// 다크 모드에서 쓸 앱 색 조합.
    pub ui_theme_dark: crate::ui::DarkUiTheme,
    /// 라이트 모드에서 쓸 앱 색 조합.
    pub ui_theme_light: crate::ui::LightUiTheme,
    /// Saved choices win; an absent key uses the OS preferred language (ko, otherwise en).
    pub language: Language,
    /// UI 배율(1.0 = 100%). 적용할 때 `ZOOM_MIN..=ZOOM_MAX`로 자른다.
    /// 레거시 `zoom` 키는 첫 로드 시 `terminal_font_size`로 이관하고 `ui_zoom = 1.0`으로 초기화한다(OWE-843).
    #[serde(rename = "ui_zoom")]
    pub zoom: f32,
    /// 에이전트 입력 대기·완료 알림과 Dock 배지 켜기/끄기(MK-044).
    pub notifications: bool,
    /// 잠자기 방지 모드(MK-044: `on` / `agent` / `off`).
    #[serde(alias = "awake")]
    pub keep_awake: KeepAwake,
    /// 하단 상태바 사용량에서 숨길 프로바이더 목록(기본 빈 목록 = 모두 표시).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_status_providers: Vec<String>,
    /// 완료 웹훅 opt-in. 기본은 빈 목록이며 URL에 비밀 토큰이 들어갈 수 있다.
    pub webhooks: Vec<crate::webhook::Endpoint>,
    /// Copy Context의 최근 출력 행 수(1..=1000으로 제한).
    pub copy_context_lines: u16,
    /// 저장 명령·프롬프트(MK-092). 클릭하면 실행 키 없이 붙여넣는다.
    pub quick_commands: Vec<crate::commands::QuickCommand>,
    /// 터미널 글꼴 크기(10..=24pt, 기본 14).
    #[serde(default = "default_terminal_font_size")]
    pub terminal_font_size: u8,
    /// 코드 에디터 글꼴 크기(8..=32pt, 기본 12, OWE-843).
    #[serde(default = "default_editor_font_size")]
    pub editor_font_size: u8,
    /// 터미널 배경 불투명도(50..=100%, 기본 100).
    #[serde(default = "default_terminal_opacity_pct")]
    pub terminal_opacity_pct: u8,
    /// 터미널 글꼴 패밀리(기본 Menlo, OWE-658). 고정폭으로 해석되지 않으면 Menlo로
    /// 폴백한다(OWE-899: SF Mono처럼 비례 대체로 떨어지는 지정값이 셀을 어긋나게 했다).
    #[serde(default = "default_terminal_font_family")]
    pub terminal_font_family: String,
    /// 프로그램이 커서 모양을 지정하지 않았을 때의 기본 커서(기본 블록, OWE-658).
    #[serde(default)]
    pub terminal_cursor_shape: TerminalCursorShape,
    /// 기본 커서 깜박임(기본 켬). 프로그램이 지정하지 않았을 때만 쓴다.
    #[serde(default = "default_true")]
    pub terminal_cursor_blink: bool,
    /// Option+글자 키를 ESC+글자(Meta)로 보낸다(기본 끔, OWE-658).
    #[serde(default)]
    pub option_as_meta: bool,
    /// 미국식 키보드 레이아웃에서 Option을 Meta로 자동 설정한다. 기존 명시값은 수동으로 이관한다.
    #[serde(default = "default_true")]
    pub option_as_meta_auto: bool,
    /// 마우스로 선택을 마치면 바로 클립보드에 복사한다(기본 끔, OWE-658).
    #[serde(default)]
    pub copy_on_select: bool,
    /// 알림 사운드 켜기/끄기(기본 true).
    #[serde(default = "default_true")]
    pub notification_sound: bool,
    /// 창이 비활성 상태일 때만 알림 보내기(기본 false).
    #[serde(default)]
    pub notify_only_inactive: bool,
    /// 입력·승인 대기 알림(기본 켬, OWE-659). 마스터 `notifications`가 꺼지면 함께 꺼진다.
    #[serde(default = "default_true")]
    pub notify_on_input: bool,
    /// 작업 완료 알림(기본 켬, OWE-659).
    #[serde(default = "default_true")]
    pub notify_on_done: bool,
    /// 터미널 벨 알림(기본 끔).
    #[serde(default)]
    pub notify_on_bell: bool,
    /// 기본 에이전트 ID(기본 `"claude"`).
    #[serde(default = "default_agent_id")]
    pub default_agent: String,
    /// 세션 칸의 기본 보기 모드(기본 `Terminal`).
    #[serde(default)]
    pub default_view_mode: DefaultViewMode,
    /// 새 에이전트 세션 YOLO 권한 우회 모드(기본 true).
    #[serde(default = "default_true")]
    pub yolo_mode: bool,
    /// 유휴 세션 하이버네이션 opt-in(기본 false, docs/agents/hibernation.md).
    #[serde(default)]
    pub hibernation_enabled: bool,
    /// 설치된 에이전트의 상태 감지 훅을 앱 시작 때 자동 설치(기본 켬). 끄면 Krill이 설치한 훅을 되돌린다.
    #[serde(default = "default_true")]
    pub agent_status_hooks: bool,
    /// 히스토리 세그먼트 기록 일시중지(기본 false).
    #[serde(default)]
    pub recording_paused: bool,
    /// 보안 키보드 입력(Secure Keyboard Entry, 기본 false).
    #[serde(default)]
    pub secure_keyboard_entry: bool,
    /// 새 버전 스테이징 완료 시 자동으로 앱을 재시작해 적용(기본 false, MK-156).
    #[serde(default)]
    pub auto_restart_update: bool,
    /// Claude 프롬프트 캐시 카운트다운 표시 켜기/끄기(기본 false).
    #[serde(default)]
    pub prompt_cache_timer_enabled: bool,
    /// Claude 프롬프트 캐시 TTL(초, 기본 300초 = 5분, OWE-737).
    #[serde(default = "default_prompt_cache_ttl_secs")]
    pub prompt_cache_ttl_secs: u32,
    /// 첫 프롬프트 기반 브랜치·세션 이름 자동 변경 켜기/끄기(기본 true, OWE-737).
    #[serde(default = "default_true")]
    pub auto_rename_branch_from_work: bool,
    /// 실행 중인 탭·칸을 닫기 전에 확인할지(기본 true, OWE-872).
    /// '다시 묻지 않기'를 고르면 false로 저장된다.
    #[serde(default = "default_true")]
    pub confirm_running_close: bool,
    /// 워크스페이스 신뢰 프리셋 자동 등록 켜기/끄기(기본 true, OWE-737).
    #[serde(default = "default_true")]
    pub workspace_trust_preset_enabled: bool,
    /// 다크 모드 터미널 색상 테마 이름 또는 `custom:<id>`(기본 `"Krill Trench"`, OWE-956).
    #[serde(default = "default_terminal_theme_dark")]
    pub terminal_theme_dark: String,
    /// 라이트 모드 터미널 색상 테마 이름 또는 `custom:<id>`(기본 `"Krill Paper"`, OWE-956).
    #[serde(default = "default_terminal_theme_light")]
    pub terminal_theme_light: String,
    /// 라이트 모드에서 별도 터미널 테마를 쓸지(기본 true, Orca `terminalUseSeparateLightTheme`, OWE-956).
    /// false면 라이트 모드에서도 다크 모드 테마를 쓴다.
    #[serde(default = "default_true")]
    pub terminal_use_separate_light_theme: bool,
    /// 사이드바 자동화 진입점 표시. Workflows 설정에서 다시 켤 수 있다.
    #[serde(default = "default_true")]
    pub show_automations_button: bool,
    /// 가져온 사용자 터미널 테마(Ghostty·Warp, OWE-956).
    /// TOML 테이블 배열(`[[terminal_custom_themes]]`)은 스칼라 키 뒤에 와야 하므로 구조체 맨 끝에 둔다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminal_custom_themes: Vec<crate::ui::CustomTerminalTheme>,
}

pub const DEFAULT_TERMINAL_FONT_SIZE: u8 = 14;
pub const TERMINAL_FONT_SIZE_MIN: u8 = 10;
pub const TERMINAL_FONT_SIZE_MAX: u8 = 24;

pub const DEFAULT_EDITOR_FONT_SIZE: u8 = 12;
pub const EDITOR_FONT_SIZE_MIN: u8 = 8;
pub const EDITOR_FONT_SIZE_MAX: u8 = 32;

const fn default_terminal_font_size() -> u8 {
    DEFAULT_TERMINAL_FONT_SIZE
}

const fn default_editor_font_size() -> u8 {
    DEFAULT_EDITOR_FONT_SIZE
}

const fn default_terminal_opacity_pct() -> u8 {
    100
}

const fn default_true() -> bool {
    true
}

pub const DEFAULT_PROMPT_CACHE_TTL_SECS: u32 = 300;

const fn default_prompt_cache_ttl_secs() -> u32 {
    DEFAULT_PROMPT_CACHE_TTL_SECS
}

fn default_terminal_font_family() -> String {
    crate::ui::FONT_MONO.to_owned()
}

fn default_terminal_theme_dark() -> String {
    crate::ui::DEFAULT_TERMINAL_THEME_DARK.to_owned()
}

fn default_terminal_theme_light() -> String {
    crate::ui::DEFAULT_TERMINAL_THEME_LIGHT.to_owned()
}

/// 기본 터미널 커서 모양(OWE-658). `krill_proto::screen::CursorShape`는 와이어 타입이라 설정용 이름을 따로 둔다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCursorShape {
    #[default]
    Block,
    Bar,
    Underline,
}

impl TerminalCursorShape {
    pub fn to_shape(self) -> krill_proto::screen::CursorShape {
        use krill_proto::screen::CursorShape;
        match self {
            Self::Block => CursorShape::Block,
            Self::Bar => CursorShape::Bar,
            Self::Underline => CursorShape::Underline,
        }
    }
}

/// 세션 칸의 기본 보기 모드(`terminal` / `chat`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultViewMode {
    #[default]
    Terminal,
    Chat,
}

fn default_agent_id() -> String {
    "claude".to_owned()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::System,
            ui_theme_dark: crate::ui::DarkUiTheme::default(),
            ui_theme_light: crate::ui::LightUiTheme::default(),
            language: locale::system_language(),
            zoom: 1.0,
            notifications: true,
            keep_awake: KeepAwake::Off,
            hidden_status_providers: Vec::new(),
            webhooks: Vec::new(),
            copy_context_lines: 100,
            quick_commands: Vec::new(),
            terminal_font_size: DEFAULT_TERMINAL_FONT_SIZE,
            editor_font_size: DEFAULT_EDITOR_FONT_SIZE,
            terminal_opacity_pct: 100,
            terminal_font_family: default_terminal_font_family(),
            terminal_cursor_shape: TerminalCursorShape::Block,
            terminal_cursor_blink: true,
            option_as_meta: false,
            option_as_meta_auto: true,
            copy_on_select: false,
            notification_sound: true,
            notify_only_inactive: false,
            notify_on_input: true,
            notify_on_done: true,
            notify_on_bell: false,
            default_agent: "claude".to_owned(),
            default_view_mode: DefaultViewMode::Terminal,
            yolo_mode: true,
            hibernation_enabled: false,
            agent_status_hooks: true,
            recording_paused: false,
            secure_keyboard_entry: false,
            auto_restart_update: false,
            prompt_cache_timer_enabled: false,
            prompt_cache_ttl_secs: DEFAULT_PROMPT_CACHE_TTL_SECS,
            auto_rename_branch_from_work: true,
            confirm_running_close: true,
            workspace_trust_preset_enabled: true,
            terminal_theme_dark: default_terminal_theme_dark(),
            terminal_theme_light: default_terminal_theme_light(),
            terminal_use_separate_light_theme: true,
            show_automations_button: true,
            terminal_custom_themes: Vec::new(),
        }
    }
}

fn has_legacy_zoom(s: &str) -> bool {
    toml::from_str::<toml::Table>(s)
        .is_ok_and(|t| !t.contains_key("ui_zoom") && t.contains_key("zoom"))
}

impl Settings {
    /// 다크 앱 테마를 바꾼다. 다크 터미널 테마가 이전 앱 테마의 짝이면 새 테마의 짝으로 함께 바꾼다.
    pub fn set_ui_theme_dark(&mut self, theme: crate::ui::DarkUiTheme) {
        let paired = self.ui_theme_dark.terminal_theme_name();
        if crate::ui::canonical_terminal_theme_name(&self.terminal_theme_dark) == paired {
            theme
                .terminal_theme_name()
                .clone_into(&mut self.terminal_theme_dark);
        }
        self.ui_theme_dark = theme;
    }

    /// 라이트 앱 테마를 바꾼다. 라이트 터미널 테마가 이전 앱 테마의 짝이면 새 테마의 짝으로 함께 바꾼다.
    pub fn set_ui_theme_light(&mut self, theme: crate::ui::LightUiTheme) {
        let paired = self.ui_theme_light.terminal_theme_name();
        if crate::ui::canonical_terminal_theme_name(&self.terminal_theme_light) == paired {
            theme
                .terminal_theme_name()
                .clone_into(&mut self.terminal_theme_light);
        }
        self.ui_theme_light = theme;
    }

    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        Self::parse_with_language(s, locale::system_language())
    }

    /// Inject the first-launch choice so parsing regressions do not depend on the host locale.
    fn parse_with_language(s: &str, default_language: Language) -> Result<Self, toml::de::Error> {
        let mut parsed: Self = toml::from_str(s)?;
        parsed.terminal_font_size = parsed
            .terminal_font_size
            .clamp(TERMINAL_FONT_SIZE_MIN, TERMINAL_FONT_SIZE_MAX);
        parsed.editor_font_size = parsed
            .editor_font_size
            .clamp(EDITOR_FONT_SIZE_MIN, EDITOR_FONT_SIZE_MAX);
        parsed.terminal_opacity_pct = parsed.terminal_opacity_pct.clamp(50, 100);
        parsed.prompt_cache_ttl_secs = parsed.prompt_cache_ttl_secs.clamp(60, 86_400);
        if parsed.terminal_theme_dark.trim().is_empty() {
            parsed.terminal_theme_dark = default_terminal_theme_dark();
        }
        if parsed.terminal_theme_light.trim().is_empty() {
            parsed.terminal_theme_light = default_terminal_theme_light();
        }
        let mut hidden_providers = Vec::new();
        for raw in &parsed.hidden_status_providers {
            if let Some(p) = krill_proto::usage::ProviderKind::parse(raw) {
                let key = p.as_str();
                if !key.is_empty() && !hidden_providers.iter().any(|k| k == key) {
                    hidden_providers.push(key.to_owned());
                }
            }
        }
        parsed.hidden_status_providers = hidden_providers;
        let table: toml::Table = toml::from_str(s)?;
        if !table.contains_key("terminal_theme_dark") {
            parsed
                .ui_theme_dark
                .terminal_theme_name()
                .clone_into(&mut parsed.terminal_theme_dark);
        }
        if !table.contains_key("terminal_theme_light") {
            parsed
                .ui_theme_light
                .terminal_theme_name()
                .clone_into(&mut parsed.terminal_theme_light);
        }
        if !table.contains_key("language") {
            parsed.language = default_language;
        }
        if table.contains_key("option_as_meta") && !table.contains_key("option_as_meta_auto") {
            parsed.option_as_meta_auto = false;
        }
        if !table.contains_key("ui_zoom")
            && let Some(val) = table.get("zoom")
        {
            #[expect(
                clippy::cast_precision_loss,
                reason = "TOML 정수 배율은 소수 범위(0.5..=2.0)로 자르므로 정밀도 손실 무관"
            )]
            let raw = val
                .as_float()
                .or_else(|| val.as_integer().map(|n| n as f64))
                .ok_or_else(|| <toml::de::Error as serde::de::Error>::custom("invalid zoom"))?;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "ZOOM_MIN..=ZOOM_MAX(0.5..=2.0)로 자르므로 f32 범위 내"
            )]
            let z = (raw as f32).clamp(ZOOM_MIN, ZOOM_MAX);
            if z.is_finite() && (z - 1.0).abs() > f32::EPSILON {
                // 옛 zoom의 기준은 13pt다. 기본값 변경으로 기존 사용자 크기를 다시 바꾸지 않는다.
                let legacy_font = if table.contains_key("terminal_font_size") {
                    parsed.terminal_font_size
                } else {
                    13
                };
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "10..=24 * 0.5..=2.0 반올림은 양수 u8 범위 내"
                )]
                let migrated = (f32::from(legacy_font) * z).round() as u8;
                parsed.terminal_font_size =
                    migrated.clamp(TERMINAL_FONT_SIZE_MIN, TERMINAL_FONT_SIZE_MAX);
            }
            parsed.zoom = 1.0;
        }
        Ok(parsed)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// 자른 줌 값. `NaN`이면 1.0.
    pub fn zoom(&self) -> f32 {
        if self.zoom.is_nan() {
            1.0
        } else {
            self.zoom.clamp(ZOOM_MIN, ZOOM_MAX)
        }
    }

    /// 한 단계 키우거나(`+1`) 줄인(`-1`) 줌. 0.1 단위로 반올림해 부동소수 누적을 막는다.
    pub fn zoom_step(&self, dir: i8) -> f32 {
        let z = self.zoom() + f32::from(dir) * ZOOM_STEP;
        ((z * 10.).round() / 10.).clamp(ZOOM_MIN, ZOOM_MAX)
    }

    /// 자른 터미널 글꼴 크기(`10..=24`).
    pub fn terminal_font_size(&self) -> u8 {
        self.terminal_font_size
            .clamp(TERMINAL_FONT_SIZE_MIN, TERMINAL_FONT_SIZE_MAX)
    }

    /// 한 단계 키우거나(`+1`) 줄인(`-1`) 터미널 글꼴 크기(`10..=24`).
    pub fn terminal_font_size_step(&self, dir: i8) -> u8 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "10..=24 범위로 clamp된 i16을 u8로 변환"
        )]
        let next = (i16::from(self.terminal_font_size()) + i16::from(dir)).clamp(
            i16::from(TERMINAL_FONT_SIZE_MIN),
            i16::from(TERMINAL_FONT_SIZE_MAX),
        ) as u8;
        next
    }

    /// 자른 코드 에디터 글꼴 크기(`8..=32`).
    pub fn editor_font_size(&self) -> u8 {
        self.editor_font_size
            .clamp(EDITOR_FONT_SIZE_MIN, EDITOR_FONT_SIZE_MAX)
    }

    /// 한 단계 키우거나(`+1`) 줄인(`-1`) 코드 에디터 글꼴 크기(`8..=32`).
    pub fn editor_font_size_step(&self, dir: i8) -> u8 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "8..=32 범위로 clamp된 i16을 u8로 변환"
        )]
        let next = (i16::from(self.editor_font_size()) + i16::from(dir)).clamp(
            i16::from(EDITOR_FONT_SIZE_MIN),
            i16::from(EDITOR_FONT_SIZE_MAX),
        ) as u8;
        next
    }

    /// 자른 터미널 배경 불투명도(`50..=100`).
    pub fn terminal_opacity_pct(&self) -> u8 {
        self.terminal_opacity_pct.clamp(50, 100)
    }

    /// 자른 프롬프트 캐시 TTL(초, `60..=86400`).
    pub fn prompt_cache_ttl_secs(&self) -> u32 {
        self.prompt_cache_ttl_secs.clamp(60, 86_400)
    }

    /// 해당 프로바이더를 하단 상태바에 표시할지 여부(기본 true).
    #[must_use]
    pub fn status_provider_visible(&self, provider: krill_proto::usage::ProviderKind) -> bool {
        !self
            .hidden_status_providers
            .iter()
            .any(|s| krill_proto::usage::ProviderKind::parse(s) == Some(provider))
    }

    /// 하단 상태바 프로바이더 표시 여부를 바꾼다.
    pub fn set_status_provider_visible(
        &mut self,
        provider: krill_proto::usage::ProviderKind,
        visible: bool,
    ) {
        self.hidden_status_providers
            .retain(|s| krill_proto::usage::ProviderKind::parse(s) != Some(provider));
        if !visible {
            let key = provider.as_str();
            if !key.is_empty() {
                self.hidden_status_providers.push(key.to_owned());
            }
        }
    }
}

/// `keys.toml`: 키 → 액션 이름. 빈 이름은 그 키의 기본 바인딩을 모두 끈다.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub bindings: toml::Table,
    /// 액션 → 끈 기본 키들(OWE-863). 같은 키를 쓰는 다른 액션의 기본 바인딩은 그대로 둔다.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub disabled: std::collections::BTreeMap<String, Vec<String>>,
}

impl Keys {
    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

/// 기본 바인딩(`defaults`) 위에 `keys`를 얹은 최종 바인딩. 재정의는 같은 액션의 기본 바인딩과 같은 문맥에
/// 걸어(깊이가 같아야 나중 것이 이긴다) 기본 키를 대신한다. 빈 이름은 그 키의 기본 바인딩을 모두 빼고,
/// `[disabled]`는 그 액션의 기본 바인딩만 뺀다.
///
/// # Errors
/// 키 문법이나 액션 이름이 틀리면 첫 오류를 돌려준다(이때는 아무것도 적용하지 않는다).
pub fn bindings(
    defaults: Vec<KeyBinding>,
    keys: &Keys,
    cx: &App,
) -> Result<Vec<KeyBinding>, String> {
    // 문맥은 끄기 전 기본 바인딩에서 찾는다(키를 끄고 같은 액션을 다른 키로 옮기는 경우).
    let contexts: Vec<_> = defaults
        .iter()
        .map(|b| (b.action().name(), b.predicate()))
        .collect();
    let mut out = defaults;
    let parse = |key: &str| {
        let typed = key
            .split_whitespace()
            .map(Keystroke::parse)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("{KEYS_FILE}: \"{key}\": {e}"))?;
        if typed.is_empty() {
            return Err(text::EMPTY_KEY.format(&[("file", KEYS_FILE)]));
        }
        Ok(typed)
    };
    for (name, disabled) in &keys.disabled {
        for key in disabled {
            let typed = parse(key)?;
            out.retain(|b| b.action().name() != name || b.match_keystrokes(&typed) != Some(false));
        }
    }
    let mut user = Vec::new();
    for (key, action) in &keys.bindings {
        let Some(name) = action.as_str() else {
            return Err(text::KEY_VALUE.format(&[("file", KEYS_FILE), ("key", key)]));
        };
        let typed = parse(key)?;
        if name.is_empty() {
            out.retain(|b| b.match_keystrokes(&typed) != Some(false));
            continue;
        }
        let action = cx
            .build_action(name, None)
            .map_err(|e| format!("{KEYS_FILE}: \"{key}\" = \"{name}\": {e}"))?;
        let context = contexts
            .iter()
            .find(|(n, _)| *n == action.name())
            .and_then(|(_, p)| p.clone());
        let binding = KeyBinding::load(
            key,
            action,
            context,
            false,
            None,
            &gpui::DummyKeyboardMapper,
        )
        .map_err(|e| format!("{KEYS_FILE}: \"{key}\": {e}"))?;
        user.push(binding);
    }
    out.extend(user);
    Ok(out)
}

const ACCOUNTS_FILE: &str = "accounts.toml";
const HOSTS_FILE: &str = "hosts.toml";

/// 앱 전역 설정 상태.
#[derive(Default)]
pub struct Config {
    settings: Settings,
    initial_loaded: bool,
    durable: Settings,
    epoch: u64,
    revision: u64,
    /// 마지막 읽기 오류. 설정 창에 보인다.
    error: Option<SharedString>,
    dir: Option<PathBuf>,
    /// 재정의 전 기본 키맵: 초기화 시점에 묶여 있던 모든 바인딩(셸·gpui-component) + 설정 액션.
    defaults: Vec<KeyBinding>,
    /// 설정·키맵·계정·호스트 네 파일의 마지막 수정 시각(바뀌었는지 보는 용도).
    stamp: [Option<SystemTime>; 7],
    /// 관리자 창에서 요청한 원격 터미널 열기 호스트(메인 셸 창이 꺼내 실행).
    pending_ssh_connect: Option<crate::ssh_hosts::SshHost>,
    /// 설정·점검 창에서 요청한 터미널 명령 자동 입력 큐(메인 셸 창이 순서대로 꺼내 실행).
    pending_terminal_commands: std::collections::VecDeque<String>,
    /// 설정·점검 창에서 요청한 터미널 명령의 실행 대상 호스트(`None`이면 로컬).
    pending_terminal_host: Option<crate::hosts::HostId>,
    /// 관리자 YOLO 토글이 요청한 새 세션 권한(메인 셸 창이 꺼내 데몬에 적용).
    pending_permission: Option<krill_proto::agent::Permission>,
    /// `ssh-copy-id` 실행 안내처럼 설정 창이 닫힌 뒤에도 보여야 할 문구(메인 셸 창이 꺼내 배너로 표시).
    pending_notice: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("settings", &self.settings)
            .field("error", &self.error)
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Global for Config {}

fn own_bindings() -> Vec<KeyBinding> {
    vec![
        crate::keys::bind("cmd-,", OpenSettings, None),
        crate::keys::bind("cmd-=", ZoomIn, None),
        crate::keys::bind("cmd-+", ZoomIn, None),
        crate::keys::bind("cmd--", ZoomOut, None),
        crate::keys::bind("cmd-0", ZoomReset, None),
        crate::keys::bind("cmd-f", FocusSearch, Some(SEARCH_CONTEXT)),
    ]
}

fn modified(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn file_stamps(dir: &Path) -> [Option<SystemTime>; 7] {
    [
        modified(&dir.join(SETTINGS_FILE)),
        modified(&dir.join(KEYS_FILE)),
        modified(&dir.join(ACCOUNTS_FILE)),
        modified(&dir.join(HOSTS_FILE)),
        modified(&dir.join("link/hosts.toml")),
        modified(&dir.join("link/devices.toml")),
        // 터미널의 `krill import orca`가 쓴 projects.toml도 `on_config_changed` → 사이드바 재로드로 잇는다(OWE-744).
        modified(&dir.join("projects.toml")),
    ]
}

/// 설정 디렉터리 경로. `init_at`으로 지정된 경로가 있으면 우선하고, 비테스트 실행에서만 기본 상태 디렉터리를 돌려준다.
pub fn state_dir(cx: &App) -> Option<PathBuf> {
    let configured = cx.try_global::<Config>().and_then(|c| c.dir.clone());
    #[cfg(test)]
    {
        configured
    }
    #[cfg(not(test))]
    {
        configured.or_else(krill_proto::daemon::state_dir)
    }
}

/// 지정된 설정 디렉터리가 실제 데몬이 관리하는 기본 상태 디렉터리인지 판정한다(OWE-771).
/// 단위 테스트(`#[cfg(test)]`)에서는 전역 데몬(`Paths::dev()`)을 자동 기동하지 않도록 항상 `false`를 돌려준다.
fn is_daemon_state_dir(dir: Option<&Path>) -> bool {
    #[cfg(test)]
    {
        let _ = dir;
        false
    }
    #[cfg(not(test))]
    {
        dir.is_some() && dir == krill_proto::daemon::state_dir().as_deref()
    }
}

/// `accounts.toml`·`hosts.toml` 등이 바뀌었음을 전역 설정 관찰자(`Shell`·`SettingsView`)에 알린다.
pub fn notify_external_change(cx: &mut App) {
    if !cx.has_global::<Config>() {
        return;
    }
    // Notify observers using memory only; even metadata may block on a remote volume.
    cx.global_mut::<Config>().stamp = [None; 7];
    reload(cx);
}

/// 관리자 창에서 요청한 SSH 호스트 연결 요청을 꺼낸다.
pub fn take_pending_ssh_connect(cx: &mut App) -> Option<crate::ssh_hosts::SshHost> {
    if cx
        .try_global::<Config>()
        .is_none_or(|c| c.pending_ssh_connect.is_none())
    {
        return None;
    }
    cx.global_mut::<Config>().pending_ssh_connect.take()
}

/// 설정·점검 화면에서 메인 셸 터미널 탭에 자동 입력·실행할 명령을 예약한다.
pub fn queue_terminal_command(cmd: impl Into<String>, cx: &mut App) {
    queue_terminal_command_on(crate::hosts::HostId::Local, cmd, cx);
}

/// 지정한 호스트의 터미널 탭에 자동 입력·실행할 명령을 예약한다.
pub fn queue_terminal_command_on(host: crate::hosts::HostId, cmd: impl Into<String>, cx: &mut App) {
    let cmd = cmd.into().trim().to_owned();
    if cmd.is_empty() {
        return;
    }
    if !cx.has_global::<Config>() {
        cx.set_global(Config::default());
    }
    let config = cx.global_mut::<Config>();
    config.pending_terminal_host = host.is_remote().then_some(host);
    config.pending_terminal_commands.push_back(cmd);
}

/// 예약된 터미널 자동 입력 명령의 대상 호스트를 꺼낸다(`None`이면 로컬).
pub fn take_pending_terminal_host(cx: &mut App) -> Option<crate::hosts::HostId> {
    cx.try_global::<Config>()
        .and_then(|c| c.pending_terminal_host.clone())
        .and_then(|_| cx.global_mut::<Config>().pending_terminal_host.take())
}

/// 예약된 터미널 자동 입력 명령을 순서대로 꺼낸다.
pub fn take_pending_terminal_command(cx: &mut App) -> Option<String> {
    if cx
        .try_global::<Config>()
        .is_none_or(|c| c.pending_terminal_commands.is_empty())
    {
        return None;
    }
    cx.global_mut::<Config>()
        .pending_terminal_commands
        .pop_front()
}

/// OWE-897: 관리형 설치 전체 수행(백그라운드 전용).
/// 로컬 런타임 탐색 → 원격 프로브 → 전송 → 원격 설정 순이며,
/// 어떤 단계도 사용자 파일·셸 설정을 건드리지 않는다.
fn run_managed_install(
    host: &crate::ssh_hosts::SshHost,
) -> Result<krill_hosts::managed::InstallReport, krill_hosts::managed::InstallError> {
    use krill_hosts::managed::{
        InstallBlob, InstallError, InstallFailureKind, InstallPayload, InstallSpec,
    };
    let local = |kind: InstallFailureKind, step: &'static str, detail: String| InstallError {
        kind,
        step,
        detail,
    };
    let runtime = crate::ssh_hosts::find_local_runtime().ok_or_else(|| {
        local(
            InstallFailureKind::Other,
            "local",
            "local runtime missing".to_owned(),
        )
    })?;
    let opts = host.to_ssh_options();
    let remote = krill_hosts::managed::query_remote_info(&opts).map_err(|e| {
        local(
            InstallFailureKind::Network,
            "probe",
            short_error(&e.to_string()),
        )
    })?;
    let spec = InstallSpec::new(&remote.home, &runtime.version).map_err(|e| {
        local(
            InstallFailureKind::Protocol,
            "spec",
            short_error(&e.to_string()),
        )
    })?;
    let read_blob = |path: &std::path::Path, name: &'static str| {
        std::fs::read(path).map_err(|e| {
            local(
                InstallFailureKind::Other,
                "read-local",
                format!("cannot read local {name}: {e}"),
            )
        })
    };
    let krill = read_blob(&runtime.krill, "krill")?;
    let krilld = read_blob(&runtime.krilld, "krilld")?;
    let holder = read_blob(&runtime.holder, "krill-holder")?;
    let blobs = [
        InstallBlob {
            name: "krill",
            bytes: &krill,
        },
        InstallBlob {
            name: "krilld",
            bytes: &krilld,
        },
        InstallBlob {
            name: "krill-holder",
            bytes: &holder,
        },
    ];
    let payload = InstallPayload {
        blobs: &blobs,
        plist_xml: &spec.render_plist(),
    };
    krill_hosts::managed::install_remote(
        &opts,
        &spec,
        std::env::consts::ARCH,
        payload.blobs,
        payload.plist_xml,
        &remote,
    )
}

/// OWE-897: 설치 실패 원인을 설정 화면 안내문으로 바꾼다.
fn install_failure_guide(e: &krill_hosts::managed::InstallError) -> String {
    use krill_hosts::managed::InstallFailureKind as Kind;
    let short = short_error(&e.detail);
    match (e.kind, e.step) {
        (_, "local") => text::SSH_REMOTE_RUNTIME_MISSING.get().to_owned(),
        (Kind::Auth, _) => text::SSH_REMOTE_GUIDE_AUTH.format(&[("error", &short)]),
        (Kind::Permission, _) => text::SSH_REMOTE_GUIDE_PERMISSION.format(&[("error", &short)]),
        (Kind::Network, _) => text::SSH_REMOTE_GUIDE_NETWORK.format(&[("error", &short)]),
        (Kind::DiskFull, _) => text::SSH_REMOTE_GUIDE_DISK.format(&[("error", &short)]),
        (Kind::UnsupportedOs | Kind::UnsupportedArch, _) => {
            text::SSH_REMOTE_GUIDE_UNSUPPORTED.format(&[("os", &e.detail)])
        }
        (Kind::ArchMismatch, _) => text::SSH_REMOTE_GUIDE_ARCH.format(&[("os", &e.detail)]),
        (Kind::Bootstrap, _) => text::SSH_REMOTE_GUIDE_BOOTSTRAP.format(&[("error", &short)]),
        (Kind::Protocol | Kind::Other, _) => {
            text::SSH_REMOTE_GUIDE_OTHER.format(&[("error", &short)])
        }
    }
}

/// OWE-897: 호스트 카드의 원격 상태 행 텍스트와 오류 여부.
fn remote_line_text(state: Option<&RemoteCardState>) -> (String, bool) {
    use krill_hosts::managed::{RemoteVersion, VersionStatus};
    let Some(state) = state else {
        return (text::SSH_REMOTE_BIN.get().to_owned(), false);
    };
    // 설치 결과는 새 프로브가 없을 때만 행에 남긴다(배너가 다른 동작에 덮여도 카드에 유지).
    if !matches!(state.probe, RemoteProbeState::Ready(_)) {
        match &state.install {
            RemoteInstallState::Running => {
                return (text::SSH_REMOTE_INSTALLING.get().to_owned(), false);
            }
            RemoteInstallState::Done(version) => {
                return (text::SSH_REMOTE_DONE.format(&[("version", version)]), false);
            }
            RemoteInstallState::Failed(msg) => {
                return (
                    text::SSH_REMOTE_PROBE_FAILED.format(&[("error", msg)]),
                    true,
                );
            }
            RemoteInstallState::Idle => {}
        }
    }
    match &state.probe {
        RemoteProbeState::Unknown => (text::SSH_REMOTE_BIN.get().to_owned(), false),
        RemoteProbeState::Loading => (text::SSH_REMOTE_CHECKING.get().to_owned(), false),
        RemoteProbeState::Failed(msg) => (
            text::SSH_REMOTE_PROBE_FAILED.format(&[("error", msg)]),
            true,
        ),
        RemoteProbeState::Ready(info) => {
            let os = info.platform.describe();
            let short = |v: &str| v.split('+').next().unwrap_or(v).to_owned();
            // Windows 원격은 원격 krill 중계로 붙는다. Mac 바이너리를 보낼 수 없어 설치·업데이트는 그 PC에서 한다.
            if info.platform.os == krill_hosts::managed::RemoteOs::Windows {
                let version = match &info.version {
                    RemoteVersion::Known(v) => format!("krill {}", short(v)),
                    _ => text::SSH_REMOTE_UNKNOWN_VERSION.get().to_owned(),
                };
                return (
                    text::SSH_REMOTE_WINDOWS.format(&[("os", &os), ("version", &version)]),
                    false,
                );
            }
            if !info.platform.is_supported() {
                return (text::SSH_REMOTE_UNSUPPORTED.format(&[("os", &os)]), false);
            }
            match &info.version {
                RemoteVersion::Missing => (
                    format!("{os} · {}", text::SSH_REMOTE_NOT_INSTALLED.get()),
                    false,
                ),
                RemoteVersion::Unmanaged => (
                    format!("{os} · {}", text::SSH_REMOTE_UNKNOWN_VERSION.get()),
                    false,
                ),
                RemoteVersion::Known(v) => {
                    let version_label = format!("krill {}", short(v));
                    match info.version_status(krill_proto::BUILD) {
                        VersionStatus::UpToDate => (
                            text::SSH_REMOTE_LINE.format(&[
                                ("os", &os),
                                ("version", &version_label),
                                ("state", text::SSH_REMOTE_UP_TO_DATE.get()),
                            ]),
                            false,
                        ),
                        VersionStatus::UpdateAvailable { local, .. } => {
                            let state_label = text::SSH_REMOTE_UPDATE_AVAILABLE
                                .format(&[("local", &short(&local))]);
                            (
                                text::SSH_REMOTE_LINE.format(&[
                                    ("os", &os),
                                    ("version", &version_label),
                                    ("state", &state_label),
                                ]),
                                false,
                            )
                        }
                        VersionStatus::NotInstalled | VersionStatus::Unknown => {
                            (format!("{os} · {version_label}"), false)
                        }
                    }
                }
            }
        }
    }
}

/// OWE-897: 상태 표시용 오류 축약(200자, 제어문자 제거).
fn short_error(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    let trimmed = clean.trim();
    if trimmed.chars().count() > 200 {
        format!("{}…", trimmed.chars().take(200).collect::<String>())
    } else {
        trimmed.to_owned()
    }
}

/// 관리자 YOLO 토글이 요청한 새 세션 권한을 꺼낸다.
pub fn take_pending_permission(cx: &mut App) -> Option<krill_proto::agent::Permission> {
    if cx
        .try_global::<Config>()
        .is_none_or(|c| c.pending_permission.is_none())
    {
        return None;
    }
    cx.global_mut::<Config>().pending_permission.take()
}

/// 설정 창이 닫힌 뒤 메인 셸 배너로 보여줄 문구를 예약한다.
pub fn queue_notice(msg: impl Into<String>, cx: &mut App) {
    let msg = msg.into();
    if msg.trim().is_empty() {
        return;
    }
    if !cx.has_global::<Config>() {
        cx.set_global(Config::default());
    }
    cx.global_mut::<Config>().pending_notice = Some(msg);
}

/// 예약된 메인 셸 배너 문구를 꺼낸다.
pub fn take_pending_notice(cx: &mut App) -> Option<String> {
    if cx
        .try_global::<Config>()
        .is_none_or(|c| c.pending_notice.is_none())
    {
        return None;
    }
    cx.global_mut::<Config>().pending_notice.take()
}

/// 설정을 읽어 적용하고, 1초마다 파일 변경을 보는 태스크를 띄운다. 앱 기본 키를 모두 묶은 뒤에
/// 부른다(그때의 키맵이 재정의의 바탕이 된다). 첫 창 전에도 언어를 적용하고, 줌은 열린 창에 적용한다.
/// `ZoomIn`/`ZoomOut`/`ZoomReset`은 전역 UI 배율이 아니라 `Shell`이 포커스된 터미널·에디터 글꼴 크기를
/// 조절하도록 `Shell` 루트에 묶는다(OWE-843).
pub fn init(cx: &mut App) {
    init_at(krill_proto::daemon::state_dir(), cx);
    cx.on_action(|_: &OpenSettings, cx| open_window(cx));
    cx.on_app_quit(|cx| {
        let pending = cx
            .try_global::<FileIoQueue>()
            .is_some()
            .then(|| cx.global_mut::<FileIoQueue>().0.take())
            .flatten();
        cx.background_spawn(async move {
            if let Some(pending) = pending {
                pending.await.ok();
            }
        })
    })
    .detach();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(POLL).await;
            cx.update(reload_if_changed);
        }
    })
    .detach();
}

/// `dir`의 설정으로 초기화한다(테스트는 임시 디렉터리를 준다).
pub fn init_at(dir: Option<PathBuf>, cx: &mut App) {
    init_at_with_language(dir, locale::system_language(), cx);
}

fn init_at_with_language(dir: Option<PathBuf>, language: Language, cx: &mut App) {
    let settings = Settings {
        language,
        ..Settings::default()
    };
    // The disk read is asynchronous; render the first window in the OS language
    // until a saved selection arrives, rather than the catalog's bootstrap language.
    crate::i18n::set_language(settings.language, cx);
    let mut defaults: Vec<_> = cx.key_bindings().borrow().bindings().cloned().collect();
    for b in own_bindings() {
        if !defaults
            .iter()
            .any(|d| d.action().name() == b.action().name() && d.keystrokes() == b.keystrokes())
        {
            defaults.push(b);
        }
    }
    cx.set_global(Config {
        settings: settings.clone(),
        initial_loaded: false,
        durable: settings,
        epoch: next_revision(),
        revision: next_revision(),
        error: None,
        dir,
        defaults,
        stamp: [None; 7],
        pending_ssh_connect: None,
        pending_terminal_commands: std::collections::VecDeque::new(),
        pending_terminal_host: None,
        pending_permission: None,
        pending_notice: None,
    });
    reload(cx);
}

fn next_revision() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Await queued configuration writes before an explicit application quit.
pub fn flush_file_io(cx: &mut App) -> gpui::Task<()> {
    background_file_io(cx, || ())
}

#[derive(Default)]
struct ReloadState {
    pending_epoch: Option<u64>,
    again: bool,
}
impl Global for ReloadState {}

/// File mutations are serialized without making the event loop wait on a lock.
#[derive(Default)]
struct FileIoQueue(Option<tokio::sync::oneshot::Receiver<()>>);
impl Global for FileIoQueue {}

pub(crate) fn background_file_io<R: Send + 'static>(
    cx: &mut App,
    work: impl FnOnce() -> R + Send + 'static,
) -> gpui::Task<R> {
    if !cx.has_global::<FileIoQueue>() {
        cx.set_global(FileIoQueue::default());
    }
    let (done, next) = tokio::sync::oneshot::channel();
    let previous = cx.global_mut::<FileIoQueue>().0.replace(next);
    cx.background_spawn(async move {
        if let Some(previous) = previous {
            previous.await.ok();
        }
        let result = work();
        done.send(()).ok();
        result
    })
}

fn reload_if_changed(cx: &mut App) {
    start_reload(false, cx);
}

/// Load files on the background executor; completion only applies the current revision.
pub fn reload(cx: &mut App) {
    start_reload(true, cx);
}

fn start_reload(force: bool, cx: &mut App) {
    let c = cx.global::<Config>();
    let epoch = c.epoch;
    let (dir, settings, revision, previous) =
        (c.dir.clone(), c.settings.clone(), c.revision, c.stamp);
    if !cx.has_global::<ReloadState>() {
        cx.set_global(ReloadState::default());
    }
    let pending = cx.global_mut::<ReloadState>();
    if pending.pending_epoch == Some(epoch) {
        pending.again |= force;
        return;
    }
    pending.pending_epoch = Some(epoch);
    pending.again = false;
    let work = background_file_io(cx, move || {
        if !force && dir.as_deref().map_or([None; 7], file_stamps) == previous {
            return None;
        }
        Some(read_config(dir.as_deref(), settings))
    });
    cx.spawn(async move |cx| {
        let loaded = work.await;
        cx.update(|cx| {
            if cx.global::<Config>().epoch != epoch {
                return;
            }
            let pending = cx.global_mut::<ReloadState>();
            pending.pending_epoch = None;
            let again = std::mem::take(&mut pending.again);
            let c = cx.global::<Config>();
            if c.revision == revision
                && let Some((settings, keys, stamp, mut errors)) = loaded
            {
                let defaults = c.defaults.clone();
                crate::i18n::set_language(settings.language, cx);
                match keys.map(|keys| bindings(defaults, &keys, cx)) {
                    Some(Ok(bindings)) => {
                        cx.clear_key_bindings();
                        cx.bind_keys(bindings);
                    }
                    Some(Err(error)) => errors.push(error),
                    None => {}
                }
                let c = cx.global_mut::<Config>();
                c.initial_loaded = true;
                c.durable = settings.clone();
                c.settings = settings;
                c.stamp = stamp;
                c.error = (!errors.is_empty()).then(|| errors.join("\n").into());
                crate::alert::sync_settings(cx);
                cx.defer(apply);
            }
            if again {
                reload(cx);
            }
        });
    })
    .detach();
}

type LoadedConfig = (Settings, Option<Keys>, [Option<SystemTime>; 7], Vec<String>);

fn read_config(dir: Option<&Path>, mut settings: Settings) -> LoadedConfig {
    let mut errors = Vec::new();
    // 틀린 keys.toml이면 `None`: 키맵을 건드리지 않는다(직전 재정의 유지).
    let mut keys = Some(Keys::default());
    let mut stamp = [None; 7];
    let mut settings_read_failed = false;
    if let Some(dir) = &dir {
        stamp = file_stamps(dir);
        // 일시 IO 오류(EACCES·EMFILE 등)는 NotFound와 다르다. 전부 기본값으로
        // 리셋하면 테마·줌·알림이 한 틱 뒤집히고 동면 표식이 지워진다(AS-02).
        // 오류면 직전 설정을 유지하고 표식 동기화를 건너뛴다.
        match std::fs::read_to_string(dir.join(SETTINGS_FILE)) {
            Ok(s) => match Settings::parse(&s) {
                Ok(parsed) => {
                    if has_legacy_zoom(&s)
                        && save_settings(&dir.join(SETTINGS_FILE), &parsed, Some(&s)).is_ok()
                    {
                        stamp = file_stamps(dir);
                    }
                    settings = parsed;
                }
                // TOML 원문 진단에는 웹훅 토큰이 섞일 수 있어 원문을 표시하지 않는다.
                Err(_) => {
                    errors.push(format!("{SETTINGS_FILE}: {}", text::INVALID_SETTINGS.get()));
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                settings = Settings::default();
            }
            Err(e) => {
                settings_read_failed = true;
                errors.push(format!("{SETTINGS_FILE}: {e}"));
            }
        }
        match std::fs::read_to_string(dir.join(KEYS_FILE)) {
            Ok(s) => match Keys::parse(&s) {
                Ok(k) => keys = Some(k),
                Err(e) => {
                    keys = None;
                    errors.push(format!("{KEYS_FILE}: {e}"));
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                keys = None;
                errors.push(format!("{KEYS_FILE}: {e}"));
            }
        }
    }
    settings.hibernation_enabled |= dir.is_some_and(|d| d.join(HIBERNATION_FLAG).is_file());
    if !settings_read_failed {
        sync_hibernation_file(dir, settings.hibernation_enabled);
        sync_auto_rename_file(dir, settings.auto_rename_branch_from_work);
        sync_workspace_trust_file(dir, settings.workspace_trust_preset_enabled);
    }
    (settings, keys, stamp, errors)
}

/// `dir`의 `settings.toml`에서 상태 감지 훅 자동 설치가 켜져 있는지. 파일이 없으면 기본값(켬),
/// 읽기·파싱에 실패하면 사용자 의도를 모르므로 끈 것으로 본다.
#[must_use]
pub fn agent_status_hooks_enabled_at(dir: &Path) -> bool {
    match std::fs::read_to_string(dir.join(SETTINGS_FILE)) {
        Ok(s) => Settings::parse(&s).is_ok_and(|s| s.agent_status_hooks),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Whether the initial disk read has completed; absent configuration uses normal defaults.
pub fn is_loaded(cx: &App) -> bool {
    cx.try_global::<Config>()
        .is_none_or(|config| config.initial_loaded || config.dir.is_none())
}

/// 현재 적용된 설정. `init` 전(단위 테스트 등)이면 기본값을 돌려준다.
pub fn current(cx: &App) -> Settings {
    cx.try_global::<Config>()
        .map_or_else(Settings::default, |c| c.settings.clone())
}

/// 터미널 글꼴 크기(pt, `10..=24`). 매 프레임 렌더에서 부르므로 전체 복제 없이 읽는다.
/// `init` 전이면 기본값 13.
pub fn terminal_font_pt(cx: &App) -> u8 {
    cx.try_global::<Config>()
        .map_or_else(default_terminal_font_size, |c| {
            c.settings.terminal_font_size()
        })
}

/// 코드 에디터 글꼴 크기(pt, `8..=32`). `init` 전이면 기본값 12(OWE-843).
pub fn editor_font_pt(cx: &App) -> u8 {
    cx.try_global::<Config>()
        .map_or_else(default_editor_font_size, |c| c.settings.editor_font_size())
}

/// 세션 칸 기본 보기 모드(`Terminal` / `Chat`). 매 프레임 렌더에서 부르므로 전체 복제 없이 읽는다.
pub fn default_view_mode(cx: &App) -> DefaultViewMode {
    cx.try_global::<Config>()
        .map_or(DefaultViewMode::Terminal, |c| c.settings.default_view_mode)
}

fn edit_or_in_memory(cx: &mut App, f: impl Fn(&mut Settings) + Send + 'static) {
    if !cx.has_global::<Config>() {
        cx.set_global(Config::default());
    }
    if cx.global::<Config>().dir.is_some() {
        edit(cx, f);
    } else {
        f(&mut cx.global_mut::<Config>().settings);
        cx.defer(apply);
    }
}

/// 포커스된 터미널에서 `⌘=`/`⌘+`/`⌘-`로 터미널 글꼴 크기를 한 단계 조절하고 `settings.toml`에 저장한다(OWE-843).
pub fn step_terminal_font_size(dir: i8, cx: &mut App) {
    edit_or_in_memory(cx, move |s| {
        s.terminal_font_size = s.terminal_font_size_step(dir);
    });
}

/// 포커스된 터미널에서 `⌘0`으로 터미널 글꼴 크기를 기본값(13pt)으로 되돌리고 저장한다(OWE-843).
pub fn reset_terminal_font_size(cx: &mut App) {
    edit_or_in_memory(cx, move |s| {
        s.terminal_font_size = DEFAULT_TERMINAL_FONT_SIZE;
    });
}

/// 포커스된 코드 에디터 탭에서 `⌘=`/`⌘+`/`⌘-`로 에디터 글꼴 크기를 한 단계 조절하고 `settings.toml`에 저장한다(OWE-843).
pub fn step_editor_font_size(dir: i8, cx: &mut App) {
    edit_or_in_memory(cx, move |s| {
        s.editor_font_size = s.editor_font_size_step(dir);
    });
}

/// 포커스된 코드 에디터 탭에서 `⌘0`으로 에디터 글꼴 크기를 기본값(12pt)으로 되돌리고 저장한다(OWE-843).
pub fn reset_editor_font_size(cx: &mut App) {
    edit_or_in_memory(cx, move |s| s.editor_font_size = DEFAULT_EDITOR_FONT_SIZE);
}

/// 터미널 배경 불투명도(`50..=100`). 매 프레임 렌더에서 부르므로 전체 복제 없이 읽는다.
/// `init` 전이면 100.
pub fn terminal_opacity_pct(cx: &App) -> u8 {
    cx.try_global::<Config>()
        .map_or_else(default_terminal_opacity_pct, |c| {
            c.settings.terminal_opacity_pct()
        })
}

/// 터미널 배경 알파(`0.5..=1.0`). `init` 전이면 1.0.
pub fn terminal_opacity_alpha(cx: &App) -> f32 {
    f32::from(terminal_opacity_pct(cx)) / 100.
}

/// 터미널 글꼴 패밀리(OWE-658, OWE-899). 비었거나 `init` 전이면 Menlo.
/// 지정값이 고정폭으로 해석되지 않으면 Menlo로 폴백한다(SF Mono 등).
/// 셀 폭과 그리기가 같은 이름을 쓰므로 셀이 어긋나지 않는다.
pub fn terminal_font_family(cx: &App) -> SharedString {
    let raw = cx
        .try_global::<Config>()
        .map(|c| c.settings.terminal_font_family.trim())
        .filter(|f| !f.is_empty());
    let Some(raw) = raw else {
        return SharedString::new_static(crate::ui::FONT_MONO);
    };
    crate::terminal_font::effective_terminal_font(cx.text_system(), raw).family
}

/// 지정 글꼴이 고정폭으로 해석되지 않아 Menlo로 대신 그릴 때 (지정값, 실제값).
/// 설정 화면 경고용(OWE-899). `init` 전이거나 대체가 없으면 None.
pub fn terminal_font_fallback(cx: &App) -> Option<(String, SharedString)> {
    let raw = cx
        .try_global::<Config>()
        .map(|c| c.settings.terminal_font_family.trim().to_owned())
        .filter(|f| !f.is_empty())?;
    let effective = crate::terminal_font::effective_terminal_font(cx.text_system(), &raw);
    effective.replaced.then_some((raw, effective.family))
}

/// 프로그램이 지정하지 않았을 때 쓸 기본 커서(OWE-658). `init` 전이면 깜박이지 않는 블록.
pub fn terminal_cursor(cx: &App) -> krill_proto::screen::CursorStyle {
    cx.try_global::<Config>()
        .map_or_else(Default::default, |c| krill_proto::screen::CursorStyle {
            shape: c.settings.terminal_cursor_shape.to_shape(),
            blink: c.settings.terminal_cursor_blink,
        })
}

/// Option+키를 Meta(ESC+키)로 보낼지(OWE-658). `init` 전이면 false.
pub fn option_as_meta(cx: &App) -> bool {
    let Some(config) = cx.try_global::<Config>() else {
        return false;
    };
    config.settings.option_as_meta
        || (config.settings.option_as_meta_auto && us_keyboard_layout(cx.keyboard_layout().id()))
}

fn us_keyboard_layout(id: &str) -> bool {
    matches!(
        id,
        "com.apple.keylayout.US"
            | "com.apple.keylayout.ABC"
            | "com.apple.keylayout.USExtended"
            | "00000409"
            | "us"
    )
}

/// 선택을 마치면 바로 복사할지(OWE-658). `init` 전이면 false.
pub fn copy_on_select(cx: &App) -> bool {
    cx.try_global::<Config>()
        .is_some_and(|c| c.settings.copy_on_select)
}

/// 실행 중인 탭·칸을 닫기 전에 확인할지(OWE-872). `init` 전이면 기본값 true.
pub fn confirm_running_close(cx: &App) -> bool {
    cx.try_global::<Config>()
        .is_none_or(|c| c.settings.confirm_running_close)
}

/// 실행 중 닫기 확인을 켜고 끄고 `settings.toml`에 저장한다(OWE-872 '다시 묻지 않기'·설정 스위치).
pub fn set_confirm_running_close(enabled: bool, cx: &mut App) {
    if cx.has_global::<Config>() && current(cx).confirm_running_close != enabled {
        edit(cx, move |s| s.confirm_running_close = enabled);
    }
}

/// 해당 프로바이더를 하단 상태바에 표시할지 여부(기본 true).
#[must_use]
pub fn status_provider_visible(provider: krill_proto::usage::ProviderKind, cx: &App) -> bool {
    cx.try_global::<Config>()
        .is_none_or(|c| c.settings.status_provider_visible(provider))
}

/// 하단 상태바 프로바이더 표시 여부를 바꾸고 `settings.toml`에 저장한다.
pub fn set_status_provider_visible(
    provider: krill_proto::usage::ProviderKind,
    visible: bool,
    cx: &mut App,
) {
    edit_or_in_memory(cx, move |s| {
        s.set_status_provider_visible(provider, visible);
    });
    cx.refresh_windows();
    cx.defer(gpui::App::refresh_windows);
}

/// 알림 사운드 켜짐 여부. `init` 전이면 기본값 true.
pub fn notification_sound(cx: &App) -> bool {
    cx.try_global::<Config>()
        .is_none_or(|c| c.settings.notification_sound)
}

/// 설정 화면이나 명령에서 수동 업데이트 점검을 시작한다(MK-156).
pub fn trigger_manual_update_check(cx: &mut App) {
    use crate::updater::{CheckStatus, UpdateState, check_and_stage_current_app};
    let mut state = cx.try_global::<UpdateState>().cloned().unwrap_or_default();
    if matches!(state.status, CheckStatus::Checking) || state.restart.is_starting() {
        return;
    }
    state.status = CheckStatus::Checking;
    cx.set_global(state);
    cx.refresh_windows();

    let Some(dir) = state_dir(cx) else {
        let mut s = cx.try_global::<UpdateState>().cloned().unwrap_or_default();
        s.status = CheckStatus::Error(text::NO_STATE_DIR.get().to_owned());
        cx.set_global(s);
        cx.refresh_windows();
        return;
    };

    let work = cx.background_spawn(async move { check_and_stage_current_app(&dir) });
    cx.spawn(async move |cx| {
        let res = work.await;
        cx.update(|cx| {
            let mut next = cx.try_global::<UpdateState>().cloned().unwrap_or_default();
            if next.restart.is_starting() {
                return;
            }
            let automatic = current(cx).auto_restart_update;
            let mut restart = None;
            match res {
                Ok(CheckStatus::Ready(staged)) => {
                    next.restart = crate::updater::RestartStatus::Idle;
                    next.staged = Some(staged.clone());
                    next.status = CheckStatus::Ready(staged);
                    next.card_collapsed = false;
                    if automatic {
                        restart.clone_from(&next.staged);
                    }
                }
                Ok(status) => {
                    next.status = status;
                }
                Err(err) => {
                    // OWE-883: 영문 Display 대신 사용자용 문구를 보인다.
                    next.status = CheckStatus::Error(err.user_message());
                }
            }
            cx.set_global(next);
            cx.refresh_windows();
            if let Some(staged) = restart {
                restart_staged_update(staged, cx);
            }
        });
    })
    .detach();
}

/// Prepare the relauncher off the event loop, then use the application's durable quit action.
fn restart_staged_update(staged: crate::updater::StagedUpdate, cx: &mut App) {
    crate::updater::restart_staged_update(staged, cx);
}

/// 동면 opt-in 표식 파일명(krilld `hibernate_idle`이 존재 여부로 읽는다).
pub const HIBERNATION_FLAG: &str = "hibernation.enabled";

/// `hibernation_enabled` 값을 상태 디렉터리의 opt-in 파일에 반영한다(OWE-655).
/// 켜면 빈 파일을 만들고(있으면 두드리지 않는다) 끄면 지운다(없으면 무시).
/// 데몬은 다음 검사 주기부터 따른다. 디스크 오류는 조용히 둔다(표시·동작은 설정값 기준).
pub fn sync_hibernation_file(dir: Option<&Path>, enabled: bool) {
    let Some(dir) = dir else { return };
    let path = dir.join(HIBERNATION_FLAG);
    if enabled {
        if path.is_file() {
            return;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, "").ok();
    } else {
        std::fs::remove_file(&path).ok();
    }
}

/// `auto_rename_branch_from_work` 값을 상태 디렉터리의 비활성화 표식(`auto-rename-branch.disabled`)에 반영한다(OWE-737).
/// 기본값이 `true`이므로 켜져 있으면 파일을 지우고, 꺼져 있을 때만 빈 파일을 남긴다.
pub fn sync_auto_rename_file(dir: Option<&Path>, enabled: bool) {
    let Some(dir) = dir else { return };
    let path = dir.join(krill_proto::git::AUTO_RENAME_DISABLED_FLAG);
    if enabled {
        std::fs::remove_file(&path).ok();
    } else {
        if path.is_file() {
            return;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, "").ok();
    }
}

/// `workspace_trust_preset_enabled` 값을 상태 디렉터리의 opt-in 표식(`workspace-trust-preset.enabled`)에 반영한다(OWE-737).
/// 기본값이 `false`이므로 동의 후 켜졌을 때만 빈 파일을 남기고, 꺼져 있으면 파일을 지운다.
pub fn sync_workspace_trust_file(dir: Option<&Path>, enabled: bool) {
    let Some(dir) = dir else { return };
    let path = dir.join(krill_proto::git::WORKSPACE_TRUST_PRESET_ENABLED_FLAG);
    if enabled {
        if path.is_file() {
            return;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&path, "").ok();
    } else {
        std::fs::remove_file(&path).ok();
    }
}

/// 시스템 외관 변경은 System 선택일 때 외관을 따르고, Dark/Light 고정 선택일 때는 고정 모드를 유지한다.
pub fn system_appearance_changed(appearance: gpui::WindowAppearance, cx: &mut App) {
    match current(cx).theme {
        ThemeChoice::System => cx.defer(move |cx| crate::ui::apply(appearance.into(), cx)),
        ThemeChoice::Dark => cx.defer(|cx| crate::ui::apply(ThemeMode::Dark, cx)),
        ThemeChoice::Light => cx.defer(|cx| crate::ui::apply(ThemeMode::Light, cx)),
    }
}

/// 현재 설정을 테마와 열린 모든 창의 rem 크기에 반영한다. 액션·클릭 처리 중에는 그 창이 빌려져 있어
/// `update`가 실패하므로 호출자는 `cx.defer`로 부른다.
#[derive(Default)]
struct AppliedAppearance(
    Option<(
        ThemeChoice,
        Language,
        f32,
        u8,
        crate::ui::DarkUiTheme,
        crate::ui::LightUiTheme,
    )>,
);
impl Global for AppliedAppearance {}

fn apply(cx: &mut App) {
    crate::alert::sync_settings(cx);
    let s = current(cx);
    if !cx.has_global::<AppliedAppearance>() {
        cx.set_global(AppliedAppearance::default());
    }
    let previous = cx.global::<AppliedAppearance>().0;
    let next = (
        s.theme,
        s.language,
        s.zoom(),
        s.editor_font_size(),
        s.ui_theme_dark,
        s.ui_theme_light,
    );
    cx.global_mut::<AppliedAppearance>().0 = Some(next);
    let language_changed = previous.is_none_or(|p| p.1 != next.1);
    let appearance_changed = previous.is_none_or(|p| {
        p.0 != next.0
            || p.2.to_bits() != next.2.to_bits()
            || p.3 != next.3
            || p.4 != next.4
            || p.5 != next.5
    });
    if language_changed {
        crate::i18n::set_language(s.language, cx);
        crate::menu_bar::refresh_language(cx);
    }
    if !language_changed && !appearance_changed {
        return;
    }
    let mode = match s.theme {
        ThemeChoice::Light => ThemeMode::Light,
        ThemeChoice::Dark => ThemeMode::Dark,
        ThemeChoice::System => cx.window_appearance().into(),
    };
    crate::ui::apply(mode, cx);
    let rem = px(BASE_REM * s.zoom());
    for w in cx.windows() {
        w.update(cx, |_, window, _| {
            window.set_rem_size(rem);
            window.refresh();
        })
        .ok();
    }
}

/// 설정을 고쳐 바로 적용하고 `settings.toml`에 쓴다. 디스크의 유효한 최신 파일을
/// 바탕으로 변경을 적용해 1초 폴링 전 외부 편집이 메모리 설정으로 덮어쓰기되지 않게 한다.
/// 디스크 파일이 손상됐거나 읽을 수 없으면 쓰지 않고 오류만 남긴다.
pub fn edit(cx: &mut App, f: impl Fn(&mut Settings) + Send + 'static) {
    let c = cx.global::<Config>();
    let Some(dir) = c.dir.clone() else {
        cx.global_mut::<Config>().error = Some(text::NO_STATE_DIR.get().into());
        return;
    };
    let before = c.settings.clone();
    let mut next = before.clone();
    f(&mut next);
    let c = cx.global_mut::<Config>();
    c.revision = next_revision();
    let revision = c.revision;
    let epoch = c.epoch;
    c.settings = next;
    c.error = None;
    cx.defer(apply);
    let work = background_file_io(cx, move || persist_settings_edit(&dir, f));
    cx.spawn(async move |cx| {
        let result = work.await;
        cx.update(|cx| {
            let c = cx.global_mut::<Config>();
            if c.epoch != epoch {
                return;
            }
            if let Ok((settings, _)) = &result {
                c.durable = settings.clone();
            }
            if c.revision != revision {
                return;
            }
            c.initial_loaded = true;
            match result {
                Ok((settings, stamp)) => {
                    c.settings = settings;
                    c.stamp[0] = stamp;
                }
                Err(error) => {
                    c.settings = c.durable.clone();
                    c.error = Some(format!("{SETTINGS_FILE}: {error}").into());
                }
            }
            cx.defer(apply);
        });
    })
    .detach();
}

fn persist_settings_edit(
    dir: &Path,
    edit: impl Fn(&mut Settings),
) -> Result<(Settings, Option<SystemTime>), String> {
    let path = dir.join(SETTINGS_FILE);
    let base = match std::fs::read_to_string(&path) {
        Ok(body) => {
            Settings::parse(&body).map_err(|_| text::INVALID_SETTINGS.get().to_owned())?;
            Some(body)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let mut settings = base
        .as_deref()
        .map(Settings::parse)
        .transpose()
        .map_err(|_| text::INVALID_SETTINGS.get().to_owned())?
        .unwrap_or_default();
    edit(&mut settings);
    save_settings(&path, &settings, base.as_deref()).map_err(|error| error.to_string())?;
    sync_hibernation_file(Some(dir), settings.hibernation_enabled);
    sync_auto_rename_file(Some(dir), settings.auto_rename_branch_from_work);
    sync_workspace_trust_file(Some(dir), settings.workspace_trust_preset_enabled);
    Ok((settings, modified(&path)))
}

/// Select the UI language, apply it to open windows, and persist it in settings.toml.
pub fn set_language(language: Language, cx: &mut App) {
    edit(cx, move |settings| settings.language = language);
}

/// 손상 파일을 덮어쓰지 않는다. 완성된 임시 파일만 같은 디렉터리에서 원자 교체한다.
///
/// `base`는 [`edit`]이 읽은 디스크 원문이다. 저장 직전 다시 읽은 원문과 다르면
/// 외부 편집이 낀 것이므로 덮어쓰지 않고 오류를 낸다(AS-07 lost-update 방지).
fn save_settings(path: &Path, settings: &Settings, base: Option<&str>) -> std::io::Result<()> {
    use krill_proto::os::fs::OpenOptionsExt as _;
    use std::io::{self, Write as _};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let _document_lock = krill_proto::config_document::lock_toml(path)?;

    let body = match std::fs::read_to_string(path) {
        Ok(body) => {
            Settings::parse(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if base != Some(body.as_str()) {
                return Err(io::Error::other(text::SETTINGS_CONFLICT.get()));
            }
            if has_legacy_zoom(&body) && (settings.zoom() - 1.0).abs() < f32::EPSILON {
                let mut temp = settings.clone();
                temp.zoom = 0.0;
                let step1 = krill_proto::config_document::edit_toml_aliases(
                    &body,
                    &temp,
                    &[("awake", "keep_awake"), ("zoom", "ui_zoom")],
                )?;
                krill_proto::config_document::edit_toml_aliases(
                    &step1,
                    settings,
                    &[("awake", "keep_awake"), ("zoom", "ui_zoom")],
                )?
            } else {
                krill_proto::config_document::edit_toml_aliases(
                    &body,
                    settings,
                    &[("awake", "keep_awake"), ("zoom", "ui_zoom")],
                )?
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if base.is_some() {
                return Err(io::Error::other(text::SETTINGS_CONFLICT.get()));
            }
            settings.to_toml()
        }
        Err(e) => return Err(e),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "toml.{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let result = (|| {
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        std::fs::remove_file(tmp).ok();
    }
    result
}

/// 웹훅 비밀 토큰을 담을 수 있으므로 0600으로 쓴다. 기존 파일을 `set_len(0)`으로
/// 비우던 방식은 중간 크래시에 0바이트 파일을 남기므로, 고유 임시 파일에 다 쓰고
/// 같은 디렉터리에서 바꿔 다는 원자 교체로 통일한다(AS-08).
fn write_settings(path: &Path, body: &str) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "toml.{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let result = (|| {
        use std::io::Write as _;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

/// 에이전트 카드의 추가 인자·환경변수를 `agents.toml`의 `[agent.<id>]`에 쓴다(OWE-657). 인자는 공백으로
/// 나누고 실행 프로그램(`launch[0]`)은 유지한다. 환경변수에 잘못된 줄이 있으면 쓰지 않는다.
// ponytail: 인자 따옴표·이스케이프 미지원(공백 분리). 공백 든 인자가 필요하면 agents.toml을 직접 고친다.
fn save_agent_profile(dir: &Path, id: &str, args: &str, env: &str) -> Result<(), String> {
    let env = krill_proto::agent::parse_env_lines(env)
        .map_err(|line| text::AGENT_ENV_BAD_LINE.format(&[("line", &line.to_string())]))?;
    let path = dir.join(krill_proto::agent::FILE_NAME);
    let user = match std::fs::read_to_string(&path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let agents = krill_proto::agent::Agents::load(user.as_deref()).map_err(|e| e.to_string())?;
    let mut profile = agents
        .profiles
        .get(id)
        .cloned()
        .ok_or_else(|| format!("{id}: unknown agent"))?;
    profile.launch.truncate(1);
    profile
        .launch
        .extend(args.split_whitespace().map(str::to_owned));
    profile.env = env;
    let body = krill_proto::agent::set_user_profile(user.as_deref(), id, &profile)?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // krilld가 읽는 도중 반쯤 쓴 파일을 보지 않게 원자 교체한다(AS-08).
    write_settings(&path, &body).map_err(|e| e.to_string())
}

/// 파일을 시스템 편집기로 연다. 없으면 `template`로 만든다.
fn open_file(name: &str, template: &str, cx: &mut App) {
    let Some(dir) = cx.try_global::<Config>().and_then(|c| c.dir.clone()) else {
        return;
    };
    let path = dir.join(name);
    let template = template.to_owned();
    let name = name.to_owned();
    let work = background_file_io(cx, move || {
        if !path.exists() {
            std::fs::create_dir_all(&dir).ok();
            if name == SETTINGS_FILE {
                write_settings(&path, &template).ok();
            } else {
                std::fs::write(&path, template).ok();
            }
        }
        path
    });
    cx.spawn(async move |cx| {
        let path = work.await;
        cx.update(|cx| cx.open_with_system(&path));
    })
    .detach();
}

/// 메인 워크스페이스 창(`Shell`)이 설정 페이지를 인앱 뷰로 열거나 닫을 때 쓰는 명령(Orca `activeView === 'settings'` 패리티).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCommand {
    Login {
        session: krill_proto::SessionId,
    },
    Open {
        tab: AdminTab,
        window: Option<gpui::AnyWindowHandle>,
    },
    Close {
        window: gpui::AnyWindowHandle,
        view: Option<gpui::EntityId>,
    },
}

type HostHandler = std::rc::Rc<dyn Fn(HostCommand, &mut App) -> Option<bool>>;

struct HostRegistry {
    handlers: Vec<HostHandler>,
}

impl Global for HostRegistry {}

/// 메인 워크스페이스 창(`Shell`)을 설정 페이지 호스트로 등록한다.
/// 콜백은 호스트 엔티티가 이미 해제됐으면 `None`, 이 창이 처리했으면 `Some(true)`, 건너뛰면 `Some(false)`를 돌려준다.
pub fn register_host(
    cx: &mut App,
    handler: impl Fn(HostCommand, &mut App) -> Option<bool> + 'static,
) {
    let handler: HostHandler = std::rc::Rc::new(handler);
    if cx.has_global::<HostRegistry>() {
        cx.global_mut::<HostRegistry>().handlers.push(handler);
    } else {
        cx.set_global(HostRegistry {
            handlers: vec![handler],
        });
    }
}

fn dispatch_host(cmd: HostCommand, cx: &mut App) -> bool {
    let Some(mut handlers) = cx.try_global::<HostRegistry>().map(|r| r.handlers.clone()) else {
        return false;
    };
    let mut handled = false;
    for i in (0..handlers.len()).rev() {
        let Some(handler) = handlers.get_mut(i) else {
            continue;
        };
        match handler(cmd, cx) {
            Some(true) => {
                handled = true;
                break;
            }
            Some(false) => {}
            None => {
                handlers.remove(i);
            }
        }
    }
    if cx.has_global::<HostRegistry>() {
        cx.global_mut::<HostRegistry>().handlers = handlers;
    }
    handled
}

/// 인앱 설정 호스트(`Shell`)가 있으면 워크스페이스로 돌아가고, 단독 설정 창이면 창을 닫는다.
pub fn close_settings_page(window: &mut Window, cx: &mut App) {
    close_settings_page_for(None, window, cx);
}

fn close_settings_page_for(view: Option<gpui::EntityId>, window: &mut Window, cx: &mut App) {
    if !dispatch_host(
        HostCommand::Close {
            window: window.window_handle(),
            view,
        },
        cx,
    ) {
        window.remove_window();
    }
}

pub fn open_window(cx: &mut App) {
    // OWE-860: ⌘, 재입력 시 이미 열린 설정 창은 현재 탭을 유지한 채 앞으로만 가져온다(일반 탭 강제 초기화 없음).
    if let Some(handle) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<SettingsView>())
    {
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .ok();
        });
        return;
    }
    open_window_at_tab(AdminTab::General, cx);
}

pub fn open_window_at_tab(tab: AdminTab, cx: &mut App) {
    if let Some(handle) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<SettingsView>())
    {
        cx.defer(move |cx| {
            handle
                .update(cx, |view, window, cx| {
                    view.set_tab(tab, cx);
                    window.activate_window();
                })
                .ok();
        });
        return;
    }
    let opened_in_host = cx.active_window().is_some_and(|w| {
        dispatch_host(
            HostCommand::Open {
                tab,
                window: Some(w),
            },
            cx,
        )
    }) || dispatch_host(HostCommand::Open { tab, window: None }, cx);
    if opened_in_host {
        cx.defer(apply);
        return;
    }
    if cx
        .open_window(window_options(cx), move |_, cx| {
            cx.new(|cx| {
                let mut v = SettingsView::new(cx);
                v.tab = tab;
                SettingsView::sync_recording_paused(cx);
                v
            })
        })
        .is_err()
    {
        tracing::warn!("settings window creation failed");
    }
    cx.defer(apply);
}

/// 설정 창 옵션. 제목 표시줄은 투명하게 두고 테마 색 제목 띠를 직접 그린다(다크 테마에서 흰 시스템 제목 표시줄 방지).
fn window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1100.), px(740.)),
            cx,
        ))),
        titlebar: Some(crate::ui::titlebar(text::TITLE.get())),
        ..Default::default()
    }
}

/// 관리자 페이지(Settings & Admin Center, MK-109)의 핵심 탭.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AdminTab {
    #[default]
    General,
    /// 터미널 글꼴·커서·키보드·선택(OWE-658).
    Terminal,
    Accounts,
    Agents,
    /// 프론티어 CLI·개발 도구 원클릭 설치 및 에일리어스 설정.
    Install,
    Ssh,
    RemoteKrill,
    Workflows,
    Shortcuts,
    Resources,
    Mobile,
}

// 기본 번들 밖 탭 아이콘. 지정한 SVG만 바이너리에 싣는다.
gpui_kit_assets::icon_assets!(TabIcons, [Keyboard, Server, Smartphone, Monitor, Package]);

fn settings_nav_tab(tab: AdminTab, selected: bool) -> gpui::Stateful<gpui::Div> {
    let selector = tab.selector();
    div()
        .id(selector)
        .debug_selector(move || selector.into())
        .role(gpui::Role::Tab)
        .aria_label(tab.label())
        .aria_selected(selected)
        .tab_index(0)
}

impl AdminTab {
    pub const ALL: [Self; 11] = [
        Self::General,
        Self::Terminal,
        Self::Accounts,
        Self::Agents,
        Self::Install,
        Self::Ssh,
        Self::RemoteKrill,
        Self::Workflows,
        Self::Shortcuts,
        Self::Mobile,
        Self::Resources,
    ];

    pub fn selector(self) -> &'static str {
        match self {
            Self::General => "admin-tab-general",
            Self::Terminal => "admin-tab-terminal",
            Self::Accounts => "admin-tab-accounts",
            Self::Agents => "admin-tab-agents",
            Self::Install => "admin-tab-install",
            Self::Ssh => "admin-tab-ssh",
            Self::RemoteKrill => "admin-tab-remote-krill",
            Self::Workflows => "admin-tab-workflows",
            Self::Shortcuts => "admin-tab-shortcuts",
            Self::Resources => "admin-tab-resources",
            Self::Mobile => "admin-tab-mobile",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::General => text::TAB_GENERAL.get(),
            Self::Terminal => text::TAB_TERMINAL.get(),
            Self::Accounts => text::TAB_ACCOUNTS.get(),
            Self::Agents => text::TAB_AGENTS.get(),
            Self::Install => text::TAB_INSTALL.get(),
            Self::Ssh => text::TAB_SSH.get(),
            Self::RemoteKrill => text::TAB_REMOTE_KRILL.get(),
            Self::Workflows => text::TAB_WORKFLOWS.get(),
            Self::Shortcuts => text::TAB_SHORTCUTS.get(),
            Self::Resources => text::TAB_RESOURCES.get(),
            Self::Mobile => crate::i18n::catalog::mobile::TAB.get(),
        }
    }

    pub fn subtitle_message(self) -> crate::i18n::Message {
        match self {
            Self::General => text::TAB_GENERAL_DESC,
            Self::Terminal => text::TAB_TERMINAL_DESC,
            Self::Accounts => text::TAB_ACCOUNTS_DESC,
            Self::Agents => text::AGENTS_DESC,
            Self::Install => text::TAB_INSTALL_DESC,
            Self::Ssh => text::TAB_SSH_DESC,
            Self::RemoteKrill => text::TAB_REMOTE_KRILL_DESC,
            Self::Workflows => text::TAB_WORKFLOWS_DESC,
            Self::Shortcuts => text::SHORTCUTS_DESC,
            Self::Resources => text::TAB_RESOURCES_DESC,
            Self::Mobile => text::TAB_MOBILE_DESC,
        }
    }

    pub fn subtitle(self) -> &'static str {
        self.subtitle_message().get()
    }

    /// 탭 아이콘. 앱 자산(`gpui_kit_assets`)의 전체 Lucide 집합에서 고른다.
    pub fn icon(self) -> gpui_kit_assets::IconName {
        use gpui_kit_assets::IconName;
        match self {
            Self::General => IconName::Settings,
            Self::Terminal => IconName::SquareTerminal,
            Self::Accounts => IconName::User,
            Self::Agents => IconName::Bot,
            Self::Install => IconName::Package,
            Self::Ssh => IconName::Server,
            Self::RemoteKrill => IconName::Monitor,
            Self::Workflows => IconName::Cpu,
            Self::Shortcuts => IconName::Keyboard,
            Self::Resources => IconName::CircleCheck,
            Self::Mobile => IconName::Smartphone,
        }
    }

    /// 그릴 탭 아이콘. `Keyboard`·`Server`·`Smartphone`·`Monitor`는 기본 번들(`gpui_kit_assets::Assets`)에 없어 경로로 그리면
    /// 빈 칸이 된다(OWE-704 실화면 QA) → 그것만 [`TabIcons`]에서 SVG를 직접 싣는다.
    pub fn icon_element(self) -> gpui_component::Icon {
        use gpui::AssetSource as _;
        let name = self.icon();
        match TabIcons.load(&name.path()) {
            Ok(Some(svg)) => gpui_component::Icon::default().data(&svg),
            _ => gpui_component::Icon::new(name),
        }
    }

    pub fn badge(self) -> Option<&'static str> {
        match self {
            Self::Workflows => Some(text::BADGE_BETA.get()),
            _ => None,
        }
    }
}

fn has_valid_reset(resets_at: Option<&str>) -> Option<&str> {
    let s = resets_at?.trim();
    if s.is_empty() || s == "0" {
        return None;
    }
    if let Ok(n) = s.parse::<i64>()
        && n <= 0
    {
        return None;
    }
    Some(s)
}

/// OWE-652: `resets_at` 분류(순수 함수). epoch 초 문자열 또는 RFC 3339
/// (`YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM]`)를 `now_secs`(epoch 초)와 비교한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetDue {
    /// 앞으로 리셋됨. 남은 초.
    Future(u64),
    /// 리셋 시각이 지남(리셋 시각 ≤ 현재).
    Stale,
    /// 빈 값·0·음수·파싱 실패(표시 숨김).
    Unknown,
}

/// OWE-652: `resets_at`을 상대 시각으로 분류한다. `now_secs`를 주입받아 테스트한다.
#[must_use]
pub fn classify_reset(resets_at: &str, now_secs: u64) -> ResetDue {
    let Some(epoch) = parse_reset_epoch_secs(resets_at.trim()) else {
        return ResetDue::Unknown;
    };
    if epoch <= now_secs {
        ResetDue::Stale
    } else {
        ResetDue::Future(epoch.saturating_sub(now_secs))
    }
}

/// 공용 파서를 써 데몬과 같은 리셋 시각을 표시한다(OWE-709).
/// UI는 epoch 0·음수를 계속 숨긴다.
fn parse_reset_epoch_secs(s: &str) -> Option<u64> {
    u64::try_from(krill_proto::time::parse_reset_epoch(s)?)
        .ok()
        .filter(|epoch| *epoch > 0)
}

/// OWE-652: `resets_at`의 상대 시각 꼬리표(앞의 " · " 제외). 미래면 카운트다운,
/// 과거면 '갱신 필요', 모르면 None(숨김).
#[must_use]
pub fn relative_reset_suffix(resets_at: Option<&str>, now_secs: u64) -> Option<String> {
    let ts = has_valid_reset(resets_at)?;
    match classify_reset(ts, now_secs) {
        ResetDue::Unknown => None,
        ResetDue::Stale => Some(text::ACCOUNT_USAGE_RESET_STALE.get().to_owned()),
        ResetDue::Future(left) => Some(format_reset_countdown(left)),
    }
}

/// 창(`Window`)의 `resets_at`을 현재 시각 기준 상대 시각 문자열로 포맷한다.
#[must_use]
pub fn window_reset_suffix(win: Option<&krill_proto::usage::Window>) -> Option<String> {
    let ts = win.and_then(|w| has_valid_reset(w.resets_at.as_deref()))?;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    relative_reset_suffix(Some(ts), now_secs)
}

fn format_reset_countdown(left_secs: u64) -> String {
    if left_secs < 60 {
        return text::ACCOUNT_USAGE_RESET_SOON.get().to_owned();
    }
    let minutes = left_secs / 60;
    if minutes < 60 {
        return text::ACCOUNT_USAGE_RESET_M.format(&[("m", &minutes.to_string())]);
    }
    let hours = minutes / 60;
    if hours < 24 {
        return text::ACCOUNT_USAGE_RESET_HM.format(&[
            ("h", &hours.to_string()),
            ("m", &(minutes % 60).to_string()),
        ]);
    }
    text::ACCOUNT_USAGE_RESET_DH.format(&[
        ("d", &(hours / 24).to_string()),
        ("h", &(hours % 24).to_string()),
    ])
}

/// 규칙의 계정 id를 라벨로 바꾼다. 목록에 없으면 id 그대로.
fn account_label(
    accounts: &[krill_proto::usage::AccountInfo],
    rule: &krill_proto::usage::AutoSwitchRule,
    id: &str,
) -> String {
    accounts
        .iter()
        .find(|a| a.provider == rule.provider && a.id == id)
        .map_or_else(|| id.to_owned(), |a| a.label.clone())
}

/// 계정별 규칙 요약 한 줄(`Max ≥ 90% → 자동`, 꺼져 있으면 ` · 끄기`). 접힌 카드와 전역 요약이 같이 쓴다(OWE-708).
#[must_use]
pub fn rule_summary(
    rule: &krill_proto::usage::AutoSwitchRule,
    accounts: &[krill_proto::usage::AccountInfo],
) -> String {
    let target = rule.target_account_id.as_deref().map_or_else(
        || text::ACCOUNTS_RULE_TARGET_AUTO.get().to_owned(),
        |id| account_label(accounts, rule, id),
    );
    let metric = rule.metric.short_label_for(rule.provider);
    let mut out = format!("{metric} ≥ {}% → {target}", rule.threshold_percent);
    if !rule.enabled {
        out.push_str(" · ");
        out.push_str(text::OFF.get());
    }
    out
}

/// 계정 카드의 제목·보조 줄. 시스템 기본값(호스트) 계정은 `시스템 기본값[: 이메일]`과 안내문, 저장된 계정은 이메일(없으면 라벨)과 프로필 경로.
#[must_use]
pub fn account_heading(acc: &krill_proto::usage::AccountInfo) -> (String, String) {
    let email = acc.email.as_deref().filter(|s| !s.is_empty());
    if acc.host_default {
        (
            email.map_or_else(
                || text::SYSTEM_DEFAULT.get().to_owned(),
                |email| text::SYSTEM_DEFAULT_EMAIL.format(&[("email", email)]),
            ),
            text::SYSTEM_DEFAULT_DESC.format(&[("provider", acc.provider.display_name())]),
        )
    } else if crate::accounts::orca_managed(acc) {
        (
            email.unwrap_or(&acc.label).to_owned(),
            text::ACCOUNT_ORCA_MANAGED.get().to_owned(),
        )
    } else {
        (
            email.unwrap_or(&acc.label).to_owned(),
            acc.profile_dir.clone(),
        )
    }
}

/// 계정의 5h/7d 사용량 문자열을 포맷한다. `resets_at`이 있으면 상대 시각 꼬리표를 덧붙인다.
pub fn format_account_usage(usage: &krill_proto::usage::Snapshot) -> String {
    if usage.is_empty() {
        return text::ACCOUNT_USAGE_EMPTY.get().to_owned();
    }
    let mut out = crate::accounts::display::summary(usage, crate::accounts::display::now());
    let reset_ts = usage
        .five_hour
        .as_ref()
        .and_then(|w| has_valid_reset(w.resets_at.as_deref()))
        .or_else(|| {
            usage
                .seven_day
                .as_ref()
                .and_then(|w| has_valid_reset(w.resets_at.as_deref()))
        });
    if let Some(ts) = reset_ts {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if let Some(suffix) = relative_reset_suffix(Some(ts), now_secs) {
            out.push_str(" · ");
            out.push_str(&suffix);
        }
    }
    out
}

/// Claude `settings.json`에 Krill 상태 감지 훅을 병합 설치한다. 머지 규칙은
/// CLI(`krill hook install claude`)와 같은 공용 함수이며, 같은 입력이면 바이트 단위로
/// 같은 결과가 나온다. 파싱 실패·비UTF8·예상 밖 타입이면 쓰지 않고 오류를 돌려준다.
pub fn install_claude_hook(settings_path: &Path) -> std::io::Result<PathBuf> {
    install_hook_file(
        settings_path,
        krill_proto::hook_install::TargetAgent::Claude,
    )
}

/// Antigravity 훅도 CLI와 같은 설치 잠금·영수증·백업을 사용한다.
pub fn install_agy_hook(hooks_path: &Path) -> std::io::Result<PathBuf> {
    install_hook_file(
        hooks_path,
        krill_proto::hook_install::TargetAgent::Other(krill_proto::agent::hooks::Agent::Agy),
    )
}

/// 에이전트 ID별 상태 감지 훅 설정 파일 경로와 설치 대상을 돌려준다.
#[must_use]
pub fn agent_hook_target_path(
    agent_id: &str,
    home: &Path,
) -> Option<(PathBuf, krill_proto::hook_install::TargetAgent)> {
    use krill_proto::agent::hooks::Agent;
    use krill_proto::hook_install::TargetAgent;
    match krill_proto::agent::canonical_id(agent_id) {
        "claude" => Some((
            home.join(".claude").join("settings.json"),
            TargetAgent::Claude,
        )),
        "codex" => Some((
            home.join(".codex").join("config.toml"),
            TargetAgent::Other(Agent::Codex),
        )),
        "agy" => Some((
            home.join(".gemini").join("config").join("hooks.json"),
            TargetAgent::Other(Agent::Agy),
        )),
        "cursor" => Some((
            home.join(".cursor").join("hooks.json"),
            TargetAgent::Other(Agent::Cursor),
        )),
        "opencode" => Some((
            krill_proto::user_dirs::opencode_plugin(&home.join(".config/opencode")),
            TargetAgent::Other(Agent::OpenCode),
        )),
        _ => None,
    }
}

/// 지정한 홈·상태 디렉터리 기준으로 에이전트 상태 감지 훅을 설치한다.
pub fn install_agent_hook_at(
    agent_id: &str,
    home: &Path,
    state: &Path,
) -> std::io::Result<PathBuf> {
    let (target_path, target_agent) = agent_hook_target_path(agent_id, home)
        .ok_or_else(|| std::io::Error::other(format!("unsupported agent: {agent_id}")))?;
    let staged = krill_proto::user_dirs::share_dir(home).join("bin/krill");
    let exe = if staged.is_file() {
        staged.as_path()
    } else {
        Path::new("krill")
    };
    install_hook_file_with_exe(&target_path, target_agent, state, exe)
}

/// 내장 스킬 전체를 지원 에이전트 스킬 디렉터리에 한 번에 설치한다.
#[must_use]
pub fn install_all_skills_at(home: &Path) -> Vec<String> {
    let report = krill_proto::orchestration::sync_builtin_skills_at(home);
    for error in &report.failures {
        tracing::warn!(%error, "built-in skill synchronization skipped a target");
    }
    report
        .installed
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

/// 모든 지원 에이전트 상태 감지 훅과 내장 스킬을 원클릭으로 일괄 설치한다.
/// (`codex`는 외부 `codex app-server` 프로세스 왕복이 필요해 UI 스레드를 막지 않도록 호출부 백그라운드에서 처리한다.)
#[must_use]
pub fn setup_all_agents_and_skills_at(home: &Path, state: &Path) -> Vec<String> {
    let mut summary = Vec::new();
    for agent_id in ["claude", "agy", "cursor", "opencode"] {
        if let Ok(path) = install_agent_hook_at(agent_id, home, state) {
            summary.push(path.display().to_string());
        }
    }
    let skills = install_all_skills_at(home);
    summary.extend(skills);
    summary
}

/// 에이전트 기본 설치 명령(`BUILTIN` 프로필의 `install`).
/// Only a confirmed active connection receives a success badge.
fn provider_connection_badge(
    connection: Option<krill_proto::usage::ConnectionStatus>,
) -> (crate::ui::BadgeTone, &'static str) {
    use krill_proto::usage::ConnectionStatus;
    match connection {
        Some(ConnectionStatus::NeedsLogin) => (
            crate::ui::BadgeTone::Warning,
            text::ACCOUNT_LOGIN_REQUIRED.get(),
        ),
        Some(ConnectionStatus::CheckFailed) => (
            crate::ui::BadgeTone::Warning,
            text::CONNECTION_CHECK_FAILED.get(),
        ),
        Some(ConnectionStatus::Checking) => {
            (crate::ui::BadgeTone::Muted, text::CONNECTION_CHECKING.get())
        }
        Some(ConnectionStatus::NotInstalled) => (
            crate::ui::BadgeTone::Muted,
            text::CONNECTION_NOT_INSTALLED.get(),
        ),
        None => (crate::ui::BadgeTone::Muted, text::CONNECTION_CHECKING.get()),
        Some(ConnectionStatus::Connected) => (
            crate::ui::BadgeTone::Success,
            text::CONNECTION_CONNECTED.get(),
        ),
    }
}

/// Collapsed summaries expose variable names; values remain in the explicit editor.
fn agent_environment_summary(profile: Option<&krill_proto::agent::Profile>) -> String {
    profile
        .map(|profile| profile.env.keys().cloned().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

#[must_use]
pub fn agent_install_command(agent_id: &str) -> Option<String> {
    let id = krill_proto::agent::canonical_id(agent_id);
    krill_proto::agent::Agents::load(None)
        .ok()
        .and_then(|agents| {
            agents
                .profiles
                .get(id)
                .map(|p| p.install_command().to_owned())
        })
        .filter(|s| !s.trim().is_empty())
}

fn install_hook_file(
    path: &Path,
    target: krill_proto::hook_install::TargetAgent,
) -> std::io::Result<PathBuf> {
    let state = krill_proto::daemon::state_dir()
        .ok_or_else(|| std::io::Error::other("state directory unavailable"))?;
    install_hook_file_at(path, target, &state)
}

fn install_hook_file_at(
    path: &Path,
    target: krill_proto::hook_install::TargetAgent,
    state: &Path,
) -> std::io::Result<PathBuf> {
    install_hook_file_with_exe(path, target, state, Path::new("krill"))
}

fn install_hook_file_with_exe(
    path: &Path,
    target: krill_proto::hook_install::TargetAgent,
    state: &Path,
    exe: &Path,
) -> std::io::Result<PathBuf> {
    // 선택된 파일 자체의 심볼릭은 정규화 전에 거부한다.
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::other(format!(
            "refusing symlink: {}",
            path.display()
        )));
    }
    let canonical = canonical_hook_path(path)?;
    let state = canonical_hook_path(state)?;
    krill_proto::hook_install::install_hooks(target, &canonical, &state, exe)?;
    Ok(path.to_path_buf())
}

/// 존재하는 가장 긴 조상을 기준으로 심볼릭을 풀어 절대경로로 만든다.
/// 풀고 남은 경로의 심볼릭은 공용 훅 저장 계층이 거부한다.
fn canonical_hook_path(path: &Path) -> std::io::Result<PathBuf> {
    let mut ancestor = path;
    let mut tail = Vec::new();
    while !ancestor.exists() {
        tail.push(
            ancestor
                .file_name()
                .ok_or_else(|| std::io::Error::other("invalid hook path"))?
                .to_owned(),
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| std::io::Error::other("missing hook parent"))?;
    }
    let mut result = std::fs::canonicalize(ancestor)?;
    for component in tail.into_iter().rev() {
        result.push(component);
    }
    Ok(result)
}

/// `krill doctor` 핵심 항목(상태 디렉터리·데몬 바이너리·미보고 크래시·세그먼트 기록 상태)을 자가 진단한다.
/// 내부 키-값이 아니라 카탈로그 문구로 보인다(OWE-883).
pub fn run_doctor_check(state: Option<&Path>) -> String {
    let state_status = match state {
        Some(d) if d.exists() => text::DOCTOR_OK.get(),
        Some(_) => text::DOCTOR_NEEDS_INIT.get(),
        None => text::DOCTOR_MISSING.get(),
    };
    let bins_status = match krill_proto::daemon::Paths::dev() {
        Ok(p) if p.holder.exists() && p.krilld.exists() => text::DOCTOR_OK.get(),
        Ok(_) => text::DOCTOR_MISSING_BINS.get(),
        Err(_) => text::DOCTOR_UNAVAILABLE.get(),
    };
    let crashes = state.map_or(0, |d| krill_proto::diag::pending(d).len());
    let rec_warns = state.map_or(0, |d| krill_proto::diag::record_warnings(d).len());
    let rec_flag = state.is_some_and(|d| {
        d.join(krill_proto::history::RECORDING_WARNING_FILE)
            .exists()
    });
    let rec_status = if rec_flag || rec_warns > 0 {
        text::DOCTOR_WARN.format(&[("n", &rec_warns.to_string())])
    } else {
        text::DOCTOR_OK.get().to_owned()
    };
    text::DOCTOR_REPORT.format(&[
        ("state", state_status),
        ("bins", bins_status),
        (
            "crashes",
            &text::DOCTOR_CRASHES.format(&[("n", &crashes.to_string())]),
        ),
        ("recording", &rec_status),
    ])
}

/// 디스크 기록(zstd 세그먼트) 즉시 삭제(MK-075). 데몬(`krilld`)에 `HistoryRequest::Clear`를
/// 보내며, PTY·화면·상태·설정은 유지된다. 로컬 폴백은 없다 — 데몬이 살아 있을 수 있어 직접
/// 지우면 세대 가드(`Store::clear`)를 우회한다.
///
/// # Errors
/// 데몬 미연결·미지원·거부(`Scope::Full` 아님) 시 사유를 돌려준다.
pub fn request_history_clear(paths: Option<&krill_proto::daemon::Paths>) -> Result<(), String> {
    request_history_clear_on(paths.cloned().map(crate::hosts::HostTarget::from))
}

/// 대상 호스트의 디스크 기록(zstd 세그먼트) 즉시 삭제.
pub fn request_history_clear_on(target: Option<crate::hosts::HostTarget>) -> Result<(), String> {
    use krill_proto::screen::Msg as _;
    let target = target.ok_or_else(|| "history: daemon unavailable".to_owned())?;
    let mut hello =
        krill_proto::daemon::client_hello(krill_proto::ipc::ClientRole::App, "krill-app history");
    hello.features = krill_proto::history::FEAT_HISTORY;
    let mut conn = target.connect(&hello).map_err(|e| e.to_string())?;
    if conn.negotiated.features & krill_proto::history::FEAT_HISTORY == 0 {
        return Err("history: krilld does not support history clear".to_owned());
    }
    let timeout = Some(Duration::from_secs(30));
    conn.stream.set_read_timeout(timeout).ok();
    conn.stream.set_write_timeout(timeout).ok();
    conn.stream
        .write_all(
            &krill_proto::history::HistoryRequest::Clear
                .encode()
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    let (kind, body) = krill_proto::ipc::read_frame(&mut conn.stream)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "history: krilld disconnected".to_owned())?;
    match krill_proto::history::HistoryReply::decode(kind, &body).map_err(|e| e.to_string())? {
        krill_proto::history::HistoryReply::Cleared => Ok(()),
        krill_proto::history::HistoryReply::Failed(e) => Err(format!("history: {e}")),
        krill_proto::history::HistoryReply::Paused { .. } => {
            Err("history: unexpected reply to clear".to_owned())
        }
    }
}

/// 기록 일시중지 요청 결과. `Unsupported`는 구 데몬(협상 비트 없음·EOF·디코드 실패·
/// 다른 응답)이며 UI는 토글을 원복하고 업데이트 안내를 보인다.
#[derive(Debug, PartialEq, Eq)]
pub enum PauseError {
    Unsupported,
    Failed(String),
}

/// 기록 일시중지(MK-075): `SetPaused` 전송 후 같은 값의 `Paused` ACK만 성공이다.
/// `hello`에 `FEAT_HISTORY_PAUSE`를 광고하고, 데몬이 협상하지 않으면 요청을 보내지 않는다.
///
/// # Errors
/// 데몬 미연결·전송 실패는 `Failed`, 구 데몬은 `Unsupported`를 돌려준다.
pub fn request_history_pause(
    paths: Option<&krill_proto::daemon::Paths>,
    paused: bool,
) -> Result<bool, PauseError> {
    request_history_pause_on(paths.cloned().map(crate::hosts::HostTarget::from), paused)
}

/// 대상 호스트의 기록 일시중지 설정.
pub fn request_history_pause_on(
    target: Option<crate::hosts::HostTarget>,
    paused: bool,
) -> Result<bool, PauseError> {
    use krill_proto::screen::Msg as _;
    let fail = |message: String| PauseError::Failed(message);
    let target = target.ok_or_else(|| fail("history: daemon unavailable".to_owned()))?;
    let mut hello =
        krill_proto::daemon::client_hello(krill_proto::ipc::ClientRole::App, "krill-app history");
    hello.features = krill_proto::history::FEAT_HISTORY | krill_proto::history::FEAT_HISTORY_PAUSE;
    let mut conn = target.connect(&hello).map_err(|e| fail(e.to_string()))?;
    if conn.negotiated.features & krill_proto::history::FEAT_HISTORY_PAUSE == 0 {
        return Err(PauseError::Unsupported);
    }
    let timeout = Some(Duration::from_secs(30));
    conn.stream.set_read_timeout(timeout).ok();
    conn.stream.set_write_timeout(timeout).ok();
    conn.stream
        .write_all(
            &krill_proto::history::HistoryRequest::SetPaused { paused }
                .encode()
                .map_err(|e| fail(e.to_string()))?,
        )
        .map_err(|e| fail(e.to_string()))?;
    let Some((kind, body)) =
        krill_proto::ipc::read_frame(&mut conn.stream).map_err(|_| PauseError::Unsupported)?
    else {
        return Err(PauseError::Unsupported);
    };
    match krill_proto::history::HistoryReply::decode(kind, &body) {
        Ok(krill_proto::history::HistoryReply::Paused { paused: echoed }) if echoed == paused => {
            Ok(echoed)
        }
        Ok(_) | Err(_) => Err(PauseError::Unsupported),
    }
}

type AgentsFileCache = (Option<(Option<SystemTime>, Option<String>)>, bool);

/// 관리자 페이지(Settings & Admin Center, MK-109): 7개 탭(일반·계정·에이전트·SSH·워크플로·단축키·리소스).
#[expect(
    clippy::struct_excessive_bools,
    reason = "서로 독립인 요청 진행·데몬 연결 플래그"
)]
pub struct SettingsView {
    _observe: Subscription,
    tab: AdminTab,
    visible: bool,
    account_probe_pending: bool,
    account_file_stamp: Vec<Option<AdminFileStamp>>,
    mobile: mobile::Panel,
    /// 사이드바 'Krill 모바일'에서 연 단독 페이지 모드. 설정 내비 없이 페어링 화면만 그린다.
    mobile_page: bool,
    automations_page: bool,
    automations: schedules::SchedulesState,
    remote_krill: Option<gpui::Entity<crate::link_ui::PairingView>>,
    account_host: crate::hosts::HostId,
    account_hosts: Vec<(
        crate::hosts::HostId,
        String,
        Option<crate::hosts::HostTarget>,
    )>,
    local_accounts_snapshot: krill_proto::usage::AccountsSnapshot,
    remote_accounts_snapshots:
        std::collections::BTreeMap<crate::hosts::HostId, krill_proto::usage::AccountsSnapshot>,
    remote_orphan_profiles:
        std::collections::BTreeMap<crate::hosts::HostId, Vec<krill_proto::usage::OrphanProfile>>,
    accounts_snapshot: krill_proto::usage::AccountsSnapshot,
    account_error: Option<SharedString>,
    /// 마지막 계정 갱신 때 데몬에 닿지 못했다. 로컬 파일을 읽기 전용으로 보이고 변경은 막는다(OWE-697).
    accounts_offline: bool,
    /// 규칙 변경 응답 전에는 다음 변경을 보내지 않는다.
    account_rule_pending: bool,
    /// 브라우저 OAuth 로그인 진행 중(OWE-881). 있으면 해당 계정 행에 진행 표시·[취소].
    login_pending: Option<PendingLogin>,
    /// agy·OpenCode 대화형 로그인 안내를 표시할 제공자. 값이 있으면 안내 카드와
    /// [터미널에서 실행] 버튼을 보이고, 버튼을 누르기 전에는 명령을 큐에 넣지 않는다.
    terminal_login_provider: Option<krill_proto::usage::ProviderKind>,
    terminal_login_account: Option<krill_proto::usage::AccountInfo>,
    /// `OpenCode` Go API key override (Orca `OpenCodeGoCredentials` parity): input + saved badge + error.
    opencode_api_key_input: Option<gpui::Entity<gpui_component::input::InputState>>,
    opencode_api_key_configured: bool,
    opencode_api_key_pending: bool,
    opencode_api_key_error: Option<SharedString>,
    ssh_hosts: Vec<crate::ssh_hosts::SshHost>,
    /// 같은 기기 중복 등록 `(중복 id, 남길 id)`와 그것을 구한 호스트 목록(`ssh -G`, 백그라운드).
    ssh_duplicates: Vec<(String, String)>,
    ssh_duplicates_for: Option<Vec<crate::ssh_hosts::SshHost>>,
    ssh_host_statuses: std::collections::BTreeMap<String, SshHostCardStatus>,
    /// OWE-897: 호스트 카드별 원격 관리 상태(프로브·설치, 백그라운드에서만 갱신).
    ssh_remote_infos: std::collections::BTreeMap<String, RemoteCardState>,
    ssh_status: Option<SharedString>,
    /// 연결 테스트 실행 중인 호스트 id. 결과(`apply_ssh_test_result`)가 오면 빠진다(OWE-884).
    ssh_test_pending: std::collections::BTreeSet<String>,
    ssh_form: Option<SshForm>,
    tailscale_peers: Vec<crate::ssh_hosts::TailscalePeer>,
    tailscale_loaded: bool,
    tailscale_pending: bool,
    ssh_auth_failed_host: Option<crate::ssh_hosts::SshHost>,
    ssh_keygen_confirm_host: Option<crate::ssh_hosts::SshHost>,
    skill_status: Option<SharedString>,
    doctor_status: Option<SharedString>,
    error_reporting: crate::errors::Panel,
    history_status: Option<SharedString>,
    /// 기록 즉시 삭제 IPC 진행 중. UI 스레드를 막지 않고 백그라운드에서 돌린다(AS-01).
    history_clear_pending: bool,
    /// 계정·호스트 새로고침 진행 중. 관찰자·적용 경로의 동기 스캔을 하나로 합친다(AS-06).
    accounts_refresh_pending: bool,
    /// Agents 탭 렌더용 `agents.toml` 캐시((수정 시각, 본문), 새로고침 진행 중)와 디스크 읽기 횟수(AS-06).
    agents_file_cache: AgentsFileCache,
    agents_file_reads: u64,
    agents_parsed: Option<std::sync::Arc<krill_proto::agent::Agents>>,
    font_names: std::sync::Arc<Vec<String>>,
    fonts_pending: bool,
    theme_imports_pending: std::collections::BTreeSet<&'static str>,
    fonts_checked: Option<std::time::Instant>,
    /// 기록 일시중지 데몬 지원. `None`은 아직 확인 전, `Some(false)`면 토글 비활성.
    history_pause_supported: Option<bool>,
    recording_status: Option<SharedString>,
    added_counter: usize,
    /// 확인 대기 중인 파괴적 동작 버튼 ID. 같은 버튼을 한 번 더 눌러야 실행한다.
    armed: Option<SharedString>,
    /// 무장 자동 해제 타이머. 다시 무장하면 바꿔 끼워 이전 타이머를 취소한다.
    armed_timer: Option<gpui::Task<()>>,
    /// 계정별 자동 전환 규칙 "고급"을 펼친 프로바이더(OWE-719). 기본은 접힘.
    advanced_open: std::collections::BTreeSet<krill_proto::usage::ProviderKind>,
    /// 제거 확인 상자를 연 계정 id(OWE-719).
    removing: Option<String>,
    /// 등록되지 않은 Krill 프로필 폴더(OWE-704). 계정 갱신과 함께 백그라운드에서 채운다.
    orphan_profiles: Vec<krill_proto::usage::OrphanProfile>,
    /// 이름을 바꾸는 중인 계정 id와 입력(OWE-705).
    rename: Option<(String, gpui::Entity<gpui_component::input::InputState>)>,
    /// 계정 탭의 "설치 안 됨" 프로바이더 묶음을 펼쳤는지(OWE-706, 기본 접힘).
    not_installed_open: bool,
    /// 워크스페이스 신뢰 프리셋을 처음 켤 때 확인·diff 미리보기 표시 여부(OWE-737).
    workspace_trust_confirm_pending: bool,
    /// `claude auth status`로 확인한 로그인(계정 id → 신원)과 이미 물어본 계정. 화면을 연 동안 계정마다 한 번(OWE-705).
    identities: (
        std::collections::BTreeMap<String, crate::accounts::Identity>,
        std::collections::BTreeSet<String>,
    ),
    /// 에이전트 카드의 추가 인자·환경변수 편집(OWE-657). 에이전트 탭을 처음 그릴 때 만든다.
    agent_edits: std::collections::BTreeMap<&'static str, AgentEdit>,
    /// 에이전트 탭에서 인자·환경변수 편집기를 펼친 에이전트 id 집합(OWE-796). 기본 접힘.
    agent_expanded: std::collections::BTreeSet<&'static str>,
    /// 사이드바 설정 검색 입력(OWE-656). 창이 필요해 처음 그릴 때 만든다.
    search_focus: search::SearchFocus,
    search_pending: bool,
    search: Option<(
        gpui::Entity<gpui_component::input::InputState>,
        Subscription,
    )>,
    /// 단축키 탭 기록기·충돌 확인(OWE-660).
    shortcuts: shortcuts::Recorder,
    /// 워크플로 탭의 저장소별 심볼릭 링크·훅 입력 폼(MK-174).
    workflow_form: Option<WorkflowRepoForm>,
    /// 워크스페이스 디스크 용량 분석(Space) 및 워크트리 일괄 클린업 패널(MK-175·OWE-729).
    space: space::Panel,
    /// 프론티어 CLI·개발 도구 설치 및 에일리어스 패널.
    install: install::Panel,
    /// 스케줄 추가/편집 모달 상태(OWE-740).
    schedule_modal: Option<schedules::ScheduleModal>,
    schedule_form_generation: u64,
    schedule_log_generation: u64,
    /// 선택된 스케줄 실행 기록 로그 뷰어(OWE-740).
    schedule_log: Option<schedules::ScheduleLogView>,
    /// 스케줄 즉시 실행·조작 오류(OWE-740).
    schedule_error: Option<SharedString>,
    /// 터미널 탭에서 현재 편집·미리보기 중인 테마 모드(OWE-956). `None`이면 앱의 현재 모드를 따른다.
    terminal_theme_target: Option<ThemeMode>,
    /// 터미널 테마 목록 검색 입력(OWE-956).
    terminal_theme_search: Option<(
        gpui::Entity<gpui_component::input::InputState>,
        Subscription,
    )>,
    /// Ghostty·Warp 테마 가져오기 상태 메시지(OWE-956). 두 번째 값은 경고/실패 여부.
    terminal_theme_status: Option<(SharedString, bool)>,
    terminal_theme_swatches: std::collections::BTreeMap<
        usize,
        (
            crate::ui::CustomTerminalTheme,
            Option<krill_proto::theme::Theme>,
        ),
    >,
    /// 설정 창 루트 포커스. 아무것도 포커스되지 않으면 ⌘F 문맥([`SEARCH_CONTEXT`])이 키 경로에 없다.
    file_jobs: std::collections::BTreeMap<&'static str, u64>,
    file_revision: u64,
    services: ServiceCache,
    service_stamp: Option<Vec<Option<AdminFileStamp>>>,
    services_pending: bool,
    services_checked: Option<std::time::Instant>,
    services_poll: Option<gpui::Task<()>>,
    focus: gpui::FocusHandle,
    modal_focus: gpui::FocusHandle,
    modal_dismiss_focus: gpui::FocusHandle,
    modal_return_focus: Option<gpui::FocusHandle>,
    rename_return_focus: Option<gpui::FocusHandle>,
}

#[derive(Clone, Default)]
struct ServiceCache {
    worktree_locations: std::collections::BTreeMap<String, String>,
    worktrees: std::sync::Arc<krill_proto::orchestration::WorktreeStore>,
    orchestration: std::sync::Arc<krill_proto::orchestration::OrchestrationStore>,
    aliases: std::collections::BTreeMap<String, String>,
    schedules: Vec<krill_proto::schedule::Schedule>,
    runs: Vec<krill_proto::schedule::ScheduleRun>,
    schedules_error: Option<String>,
    runs_error: Option<String>,
}

impl ServiceCache {
    fn script_for(
        &self,
        scripts: &std::collections::BTreeMap<String, String>,
        raw: &str,
    ) -> Option<String> {
        let resolved = self.aliases.get(raw).map_or(raw, String::as_str);
        scripts
            .get(resolved)
            .or_else(|| scripts.get(raw))
            .cloned()
            .filter(|script| !script.trim().is_empty())
    }
    fn setup_script_for(&self, raw: &str) -> Option<String> {
        self.script_for(&self.worktrees.setup_scripts, raw)
    }
    fn archive_script_for(&self, raw: &str) -> Option<String> {
        self.script_for(&self.worktrees.archive_scripts, raw)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AdminFileStamp {
    modified: SystemTime,
    len: u64,
    identity: u64,
}
fn admin_stamp(path: &Path) -> Option<AdminFileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt as _;
        meta.ino()
    };
    #[cfg(not(unix))]
    let identity = meta
        .created()
        .ok()
        .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(AdminFileStamp {
        modified: meta.modified().ok()?,
        len: meta.len(),
        identity,
    })
}
fn account_stamps(dir: Option<&Path>) -> Vec<Option<AdminFileStamp>> {
    [
        ACCOUNTS_FILE,
        HOSTS_FILE,
        "link/hosts.toml",
        "link/devices.toml",
    ]
    .iter()
    .map(|name| dir.and_then(|dir| admin_stamp(&dir.join(name))))
    .collect()
}
fn service_stamps(dir: &Path) -> Vec<Option<AdminFileStamp>> {
    [
        krill_proto::orchestration::COMMENTS_FILE,
        krill_proto::orchestration::ORCH_FILE,
        krill_proto::schedule::FILE,
        krill_proto::schedule::RUNS,
        krill_proto::schedule::PREVIOUS_RUNS,
        crate::sidebar::external::PROJECTS_FILE,
    ]
    .iter()
    .map(|name| admin_stamp(&dir.join(name)))
    .collect()
}

fn duplicate_account_labels(
    accounts: &[krill_proto::usage::AccountInfo],
    identities: &std::collections::BTreeMap<String, crate::accounts::Identity>,
) -> std::collections::BTreeMap<String, String> {
    let mut groups = std::collections::BTreeMap::new();
    for account in accounts {
        if let Some(identity) = identities.get(&account.id) {
            groups
                .entry((
                    account.provider.as_str(),
                    identity.email.to_ascii_lowercase(),
                    identity.org_id.clone(),
                ))
                .or_insert_with(Vec::new)
                .push(account);
        }
    }
    let mut labels = std::collections::BTreeMap::new();
    for group in groups.values_mut() {
        group.sort_by_key(|account| !account.host_default);
        let mut seen = std::collections::BTreeSet::new();
        let preferred: Vec<_> = group
            .iter()
            .filter(|account| seen.insert(account.id.as_str()))
            .take(2)
            .collect();
        for account in group.iter() {
            if let Some(other) = preferred.iter().find(|other| other.id != account.id) {
                labels.insert(
                    account.id.clone(),
                    if other.host_default {
                        text::ACCOUNT_DUPLICATE_HOST.get().to_owned()
                    } else {
                        text::ACCOUNT_DUPLICATE.format(&[("label", &other.label)])
                    },
                );
            }
        }
    }
    labels
}

fn canonical_repo_key(raw: &str) -> String {
    krill_proto::orchestration::worktree_key(Path::new(raw))
}

/// ⌘F([`FocusSearch`])가 설정 창 안에서만 동작하게 하는 키 문맥. 셸의 터미널 찾기 ⌘F와 겹치지 않는다.
const SEARCH_CONTEXT: &str = "AdminSettings";

/// 파괴적 동작 버튼의 무장(첫 클릭) 유지 시간. 지나면 다시 처음부터 두 번 눌러야 한다.
const CONFIRM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// 에이전트 카드 하나의 편집 입력과 저장 결과(오류면 `true`).
struct AgentEdit {
    args: gpui::Entity<gpui_component::input::InputState>,
    env: gpui::Entity<gpui_component::input::TextareaState>,
    status: Option<(SharedString, bool)>,
}

/// SSH 호스트 카드의 최근 연결 상태(OWE-800).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SshHostCardStatus {
    Connected,
    #[default]
    Disconnected,
    Error,
}

/// 브라우저 OAuth 로그인 진행 상태(OWE-881). `account-login-browser` 스레드가
/// 최대 600초 대기하는 동안 설정 탭은 이 계정 행에 진행 표시·[링크 복사]·[취소]를 보이고,
/// 스레드가 남긴 [`crate::accounts::LoginOutcome`]으로 성공·실패를 닫는다.
struct PendingLogin {
    account_id: String,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    cancelling: bool,
    started: std::time::Instant,
    url: Option<String>,
    copy_on_ready: bool,
    copied: bool,
}

/// 브라우저 로그인 대기 상한(OWE-881). CLI 스레드의 600초 데드라인에 여유를 둔다.
const LOGIN_WAIT_TIMEOUT: Duration = Duration::from_secs(605);
/// 인증 링크가 아직 캡처되지 않았을 때의 빠른 폴링 간격.
const LOGIN_URL_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// 로그인 결과 폴링 간격(OWE-881). 데몬 `ListAccounts`가 아니라 인메모리 결과만 본다.
const LOGIN_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// OWE-897: 원격 프로브 상태(백그라운드 `ssh` 한 번 조회 결과 캐시).
#[derive(Debug, Clone, Default)]
enum RemoteProbeState {
    #[default]
    Unknown,
    Loading,
    Ready(krill_hosts::managed::RemoteInfo),
    Failed(String),
}

/// OWE-897: 원격 설치 진행 상태.
#[derive(Debug, Clone, Default)]
enum RemoteInstallState {
    #[default]
    Idle,
    Running,
    Done(String),
    Failed(String),
}

/// OWE-897: 호스트 카드별 원격 관리 상태.
#[derive(Debug, Clone, Default)]
struct RemoteCardState {
    probe: RemoteProbeState,
    install: RemoteInstallState,
}

/// SSH 호스트 추가·편집 입력 폼 상태(MK-132·OWE-800).
struct SshForm {
    label: gpui::Entity<gpui_component::input::InputState>,
    address: gpui::Entity<gpui_component::input::InputState>,
    port: gpui::Entity<gpui_component::input::InputState>,
    identity_file: gpui::Entity<gpui_component::input::InputState>,
    editing_id: Option<String>,
    error: Option<SharedString>,
    /// Enter 제출 구독. 버리면 구독이 취소되므로 폼 수명 동안 둔다(OWE-884).
    _subs: Vec<Subscription>,
}

/// 워크플로 탭의 저장소별 공유 심볼릭 링크·setup/archive 훅·빠른 명령·완료 웹훅 입력 폼(MK-174·OWE-801).
struct WorkflowRepoForm {
    repo_path: gpui::Entity<gpui_component::input::InputState>,
    symlink_path: gpui::Entity<gpui_component::input::InputState>,
    setup_script: gpui::Entity<gpui_component::input::InputState>,
    archive_script: gpui::Entity<gpui_component::input::InputState>,
    qc_name: gpui::Entity<gpui_component::input::InputState>,
    qc_text: gpui::Entity<gpui_component::input::InputState>,
    qc_editing: Option<usize>,
    webhook_url: gpui::Entity<gpui_component::input::InputState>,
    webhook_error: Option<SharedString>,
    pending_repo_path: Option<String>,
    loaded_repo: Option<String>,
    loaded_setup: String,
    loaded_archive: String,
    worktree_location: gpui::Entity<gpui_component::input::InputState>,
    loaded_location: String,
    location_status: Option<(SharedString, bool)>,
    refresh_hooks: bool,
    status: Option<(SharedString, bool)>,
    _subs: Vec<Subscription>,
}

/// 완료 웹훅 종류의 표시 이름(OWE-883). URL에는 비밀 토큰이 들어갈 수 있어
/// 종류만 보이고 원문·`{:?}`는 노출하지 않는다.
#[must_use]
pub fn webhook_kind_label(ep: &crate::webhook::Endpoint) -> &'static str {
    match ep {
        crate::webhook::Endpoint::Ntfy { .. } => text::WEBHOOK_KIND_NTFY.get(),
        crate::webhook::Endpoint::Telegram { .. } => text::WEBHOOK_KIND_TELEGRAM.get(),
        crate::webhook::Endpoint::Discord { .. } => text::WEBHOOK_KIND_DISCORD.get(),
    }
}

/// 오케스트레이션 작업 상태의 표시 이름(OWE-883). 내부 영문(`ready` 등)을 그대로 보이지 않는다.
#[must_use]
pub fn orchestration_task_status_label(
    status: krill_proto::orchestration::TaskStatus,
) -> &'static str {
    use krill_proto::orchestration::TaskStatus as S;
    match status {
        S::Pending => text::ORCH_TASK_STATUS_PENDING.get(),
        S::Ready => text::ORCH_TASK_STATUS_READY.get(),
        S::Dispatched => text::ORCH_TASK_STATUS_DISPATCHED.get(),
        S::Completed => text::ORCH_TASK_STATUS_COMPLETED.get(),
        S::Failed => text::ORCH_TASK_STATUS_FAILED.get(),
        S::Blocked => text::ORCH_TASK_STATUS_BLOCKED.get(),
    }
}

/// 오케스트레이션 작업 한 줄(OWE-883). `{:?}` 의존성·영문 상태를 사람이 읽을 수 있게 푼다.
/// 설정 탭과 상태바 메뉴가 같이 쓴다.
#[must_use]
pub fn orchestration_task_line(
    run_id: &str,
    id: &str,
    title: &str,
    status: krill_proto::orchestration::TaskStatus,
    deps: &[String],
) -> String {
    let line = text::ORCH_TASK_LINE.format(&[
        ("run", run_id),
        ("id", id),
        ("title", title),
        ("status", orchestration_task_status_label(status)),
    ]);
    if deps.is_empty() {
        line
    } else {
        format!(
            "{} · {}",
            line,
            text::ORCH_TASK_DEPS.format(&[("deps", &deps.join(", "))])
        )
    }
}

impl std::fmt::Debug for SettingsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsView")
            .field("tab", &self.tab)
            .finish_non_exhaustive()
    }
}

impl SettingsView {
    /// 계정·호스트는 UI 스레드에서 읽지 않는다(AS-06·OWE-697). 첫 스냅샷은 백그라운드 갱신이 채운다.
    pub fn new(cx: &mut Context<Self>) -> Self {
        crate::orca_import::ensure_scanned(cx);
        crate::star_nag::ensure_checked(cx);
        let mut view = Self {
            _observe: cx.observe_global::<Config>(|this, cx| {
                this.refresh_accounts_if_changed(cx);
                cx.notify();
            }),
            tab: AdminTab::General,
            visible: true,
            account_probe_pending: false,
            account_file_stamp: Vec::new(),
            mobile: mobile::Panel::default(),
            mobile_page: false,
            automations_page: false,
            automations: schedules::SchedulesState::default(),
            remote_krill: None,
            account_host: crate::hosts::HostId::Local,
            account_hosts: Vec::new(),
            local_accounts_snapshot: krill_proto::usage::AccountsSnapshot::default(),
            remote_accounts_snapshots: std::collections::BTreeMap::new(),
            remote_orphan_profiles: std::collections::BTreeMap::new(),
            accounts_snapshot: krill_proto::usage::AccountsSnapshot::default(),
            account_error: None,
            accounts_offline: false,
            account_rule_pending: false,
            login_pending: None,
            terminal_login_provider: None,
            terminal_login_account: None,
            opencode_api_key_input: None,
            opencode_api_key_configured: false,
            opencode_api_key_pending: false,
            opencode_api_key_error: None,
            ssh_hosts: Vec::new(),
            ssh_duplicates: Vec::new(),
            ssh_duplicates_for: None,
            ssh_host_statuses: std::collections::BTreeMap::new(),
            ssh_remote_infos: std::collections::BTreeMap::new(),
            ssh_status: None,
            ssh_test_pending: std::collections::BTreeSet::new(),
            ssh_form: None,
            tailscale_peers: Vec::new(),
            tailscale_loaded: false,
            tailscale_pending: false,
            ssh_auth_failed_host: None,
            ssh_keygen_confirm_host: None,
            skill_status: None,
            doctor_status: None,
            error_reporting: crate::errors::Panel::default(),
            history_status: None,
            history_clear_pending: false,
            accounts_refresh_pending: false,
            agents_file_cache: (None, false),
            agents_file_reads: 0,
            agents_parsed: None,
            font_names: std::sync::Arc::new(Vec::new()),
            fonts_pending: false,
            theme_imports_pending: std::collections::BTreeSet::new(),
            fonts_checked: None,
            history_pause_supported: None,
            recording_status: None,
            added_counter: 1,
            armed: None,
            armed_timer: None,
            advanced_open: std::collections::BTreeSet::new(),
            removing: None,
            orphan_profiles: Vec::new(),
            rename: None,
            not_installed_open: false,
            workspace_trust_confirm_pending: false,
            identities: Default::default(),
            agent_edits: std::collections::BTreeMap::new(),
            agent_expanded: std::collections::BTreeSet::new(),
            search_focus: search::SearchFocus::new(cx),
            search_pending: false,
            search: None,
            shortcuts: shortcuts::Recorder::default(),
            workflow_form: None,
            space: space::Panel::default(),
            install: install::Panel::default(),
            schedule_modal: None,
            schedule_form_generation: 0,
            schedule_log_generation: 0,
            schedule_log: None,
            schedule_error: None,
            terminal_theme_target: None,
            terminal_theme_search: None,
            terminal_theme_status: None,
            terminal_theme_swatches: std::collections::BTreeMap::new(),
            file_jobs: std::collections::BTreeMap::default(),
            file_revision: 0,
            services: ServiceCache::default(),
            service_stamp: None,
            services_pending: false,
            services_checked: None,
            services_poll: None,
            focus: cx.focus_handle(),
            modal_focus: cx.focus_handle(),
            modal_dismiss_focus: cx.focus_handle().tab_stop(true),
            modal_return_focus: None,
            rename_return_focus: None,
        };
        view.services_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this
                    .update(cx, |this, cx| {
                        if this.visible && this.tab == AdminTab::Workflows {
                            this.refresh_service_cache(cx);
                        }
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
        view.refresh_accounts_async(cx);
        // 점검 스냅샷이 바뀌면 일반 탭 요약도 다시 그린다(구독은 앱 수명 동안 둔다).
        cx.observe_global::<crate::onboarding::ChecksSnapshot>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<crate::star_nag::StarNag>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<crate::accounts::installed::Installed>(|_, cx| cx.notify())
            .detach();
        cx.observe_global::<crate::orca_import::OrcaImportState>(|this, cx| {
            if crate::orca_import::state(cx)
                .is_some_and(|st| !st.importing && st.last_result.is_some())
            {
                this.accounts_refresh_pending = false;
                this.refresh_accounts_async(cx);
            }
            cx.notify();
        })
        .detach();
        view
    }

    fn refresh_font_names(&mut self, cx: &mut Context<Self>) {
        if self.fonts_pending
            || self
                .fonts_checked
                .is_some_and(|at| at.elapsed() < Duration::from_mins(1))
        {
            return;
        }
        self.fonts_pending = true;
        let text = cx.text_system().clone();
        cx.spawn(async move |this, cx| {
            let names = cx
                .background_executor()
                .spawn(async move {
                    let mut names = text.all_font_names();
                    names.retain(|name| !name.starts_with('.'));
                    let mut names = crate::terminal_font::usable_terminal_fonts(&text, &names);
                    names.sort_unstable();
                    names.dedup();
                    std::sync::Arc::new(names)
                })
                .await;
            this.update(cx, |this, cx| {
                this.fonts_pending = false;
                this.fonts_checked = Some(std::time::Instant::now());
                this.font_names = names;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save_agent_edit(
        &mut self,
        agent_id: &'static str,
        args: String,
        env: String,
        cx: &mut Context<Self>,
    ) {
        let dir = state_dir(cx);
        self.file_job(
            "agent-edit",
            move || {
                let dir = dir.ok_or_else(|| text::NO_STATE_DIR.get().to_owned())?;
                save_agent_profile(&dir, agent_id, &args, &env)?;
                let path = dir.join(krill_proto::agent::FILE_NAME);
                let body = std::fs::read_to_string(&path).ok();
                let parsed = std::sync::Arc::new(
                    krill_proto::agent::Agents::load(body.as_deref()).unwrap_or_default(),
                );
                Ok::<_, String>(((modified(&path), body), parsed))
            },
            move |this, result, _| {
                let status = match result {
                    Ok((cache, parsed)) => {
                        this.agents_file_cache.0 = Some(cache);
                        this.agents_parsed = Some(parsed);
                        (text::AGENT_SAVED.get().into(), false)
                    }
                    Err(error) => (error.into(), true),
                };
                if let Some(edit) = this.agent_edits.get_mut(agent_id) {
                    edit.status = Some(status);
                }
            },
            cx,
        );
    }

    fn file_job<R: Send + 'static>(
        &mut self,
        key: &'static str,
        work: impl FnOnce() -> R + Send + 'static,
        finish: impl FnOnce(&mut Self, R, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.file_jobs.contains_key(key)
            && !matches!(
                key,
                "ssh-edit"
                    | "worktree-edit"
                    | "schedule-edit"
                    | "keys-edit"
                    | "account-local"
                    | "agent-edit"
            )
        {
            return;
        }
        self.file_revision = self.file_revision.wrapping_add(1);
        let revision = self.file_revision;
        self.file_jobs.insert(key, revision);
        let job = background_file_io(cx, work);
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                if this.file_jobs.get(key) != Some(&revision) {
                    return;
                }
                this.file_jobs.remove(key);
                if matches!(
                    key,
                    "worktree-edit"
                        | "worktree-location-edit"
                        | "schedule-edit"
                        | "schedule-run"
                        | "orchestration-reset"
                ) {
                    this.services_checked = None;
                }
                finish(this, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_service_cache(&mut self, cx: &mut Context<Self>) {
        if self.services_pending {
            return;
        }
        let dir = state_dir(cx);
        let revision = self.file_revision;
        let previous_stamp = self.service_stamp.clone();
        let repo = self
            .workflow_form
            .as_ref()
            .map(|form| form.repo_path.read(cx).value().trim().to_owned());
        let known_repo = repo
            .as_ref()
            .is_none_or(|r| self.services.aliases.contains_key(r));
        if known_repo && self.services_checked.is_some_and(|at| at.elapsed() < POLL) {
            return;
        }
        self.services_pending = true;
        cx.spawn(async move |this, cx| {
            let (stamp, cache) = cx
                .background_executor()
                .spawn(async move {
                    let Some(dir) = dir else {
                        return (None, None);
                    };
                    let stamp = service_stamps(&dir);
                    if previous_stamp.as_ref() == Some(&stamp) && known_repo {
                        return (Some(stamp), None);
                    }
                    let schedules = krill_proto::schedule::Schedules::load(&dir);
                    let runs = krill_proto::schedule::load_coalesced_runs(&dir);
                    let schedules_error = schedules.as_ref().err().map(ToString::to_string);
                    let runs_error = runs.as_ref().err().map(ToString::to_string);
                    (
                        Some(stamp),
                        Some(ServiceCache {
                            worktree_locations:
                                crate::sidebar::external::project_worktree_locations(
                                    &dir,
                                    repo.as_deref(),
                                ),
                            worktrees: std::sync::Arc::new(
                                krill_proto::orchestration::WorktreeStore::load(&dir)
                                    .unwrap_or_default(),
                            ),
                            orchestration: std::sync::Arc::new(
                                krill_proto::orchestration::OrchestrationStore::load(&dir)
                                    .unwrap_or_default(),
                            ),
                            aliases: repo
                                .into_iter()
                                .map(|raw| {
                                    let canonical = canonical_repo_key(&raw);
                                    (raw, canonical)
                                })
                                .collect(),
                            schedules: schedules.unwrap_or_default().schedule,
                            runs: runs.unwrap_or_default(),
                            schedules_error,
                            runs_error,
                        }),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.services_pending = false;
                if this.file_revision != revision {
                    return;
                }
                this.services_checked = Some(std::time::Instant::now());
                let worktrees_changed = this.service_stamp.as_ref().and_then(|s| s.first())
                    != stamp.as_ref().and_then(|s| s.first())
                    || this.service_stamp.as_ref().and_then(|s| s.last())
                        != stamp.as_ref().and_then(|s| s.last());
                this.service_stamp = stamp;
                if let Some(cache) = cache {
                    let aliases_changed = this.services.aliases != cache.aliases;
                    this.services = cache;
                    if (worktrees_changed || aliases_changed)
                        && let Some(form) = &mut this.workflow_form
                    {
                        form.refresh_hooks = true;
                    }
                    cx.notify();
                }
                // The repository can change while a background load is running. Resolve
                // its filesystem identity off-thread before the next form projection.
                let missing_alias = this.workflow_form.as_ref().is_some_and(|form| {
                    !this
                        .services
                        .aliases
                        .contains_key(form.repo_path.read(cx).value().trim())
                });
                if missing_alias && state_dir(cx).is_some() {
                    this.services_checked = None;
                    this.refresh_service_cache(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn tab(&self) -> AdminTab {
        self.tab
    }

    pub fn automations_page(&self) -> bool {
        self.automations_page
    }

    pub fn set_automations_page(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.automations_page == on {
            return;
        }
        self.automations_page = on;
        if on {
            self.mobile_page = false;
            self.refresh_service_cache(cx);
        }
        cx.notify();
    }

    pub fn mobile_page(&self) -> bool {
        self.mobile_page
    }

    /// 단독 모바일 페이지 모드를 켜거나 끈다. 켤 때는 모바일 탭으로 전환한다.
    pub fn set_mobile_page(&mut self, on: bool, cx: &mut Context<Self>) {
        self.mobile_page = on;
        if on && self.tab != AdminTab::Mobile {
            self.set_tab(AdminTab::Mobile, cx);
        } else if on && self.mobile.offer.is_none() {
            self.mobile_enter(cx);
        }
        cx.notify();
    }

    pub fn focus_handle(&self) -> gpui::FocusHandle {
        self.focus.clone()
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.search_input(window, cx);
        input.update(cx, |s, cx| s.focus(window, cx));
    }

    /// 사이드바 검색 입력. 처음 부를 때 만들고 창 루트에 포커스를 둬 ⌘F 문맥을 켠다.
    fn search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<gpui_component::input::InputState> {
        if let Some((input, _)) = &self.search {
            return input.clone();
        }
        let input = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::SEARCH_SETTINGS.get())
        });
        let sub = cx.subscribe(&input, |_, _, _: &gpui_component::input::InputEvent, cx| {
            cx.notify();
        });
        self.search = Some((input.clone(), sub));
        window.focus(&self.focus, cx);
        input
    }

    fn search_query(&self, cx: &App) -> String {
        self.search
            .as_ref()
            .map(|(input, _)| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// Shell calls this when its in-app settings page is hidden or foregrounded.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if let Some(view) = &self.remote_krill {
            view.update(cx, |view, cx| {
                view.set_visible(visible && self.tab == AdminTab::RemoteKrill, cx);
            });
        }
        if visible {
            if self.tab == AdminTab::Mobile {
                self.mobile_enter(cx);
            }
            if self.tab == AdminTab::Workflows {
                self.refresh_service_cache(cx);
            }
        }
    }

    pub fn set_tab(&mut self, tab: AdminTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.search_focus.title = search::first_setting(&self.search_query(cx), tab);
        self.search_pending = !self.search_query(cx).trim().is_empty();
        if let Some(view) = &self.remote_krill {
            view.update(cx, |view, cx| {
                view.set_visible(self.visible && tab == AdminTab::RemoteKrill, cx);
            });
        }
        self.disarm(cx);
        self.shortcuts = shortcuts::Recorder::default();
        if tab == AdminTab::Ssh && !self.tailscale_loaded {
            self.refresh_tailscale_peers(cx);
        }
        if tab == AdminTab::Workflows {
            self.services_checked = None;
            self.refresh_service_cache(cx);
        }
        if tab == AdminTab::Mobile {
            // 진입 시 기기 목록·활성 오퍼를 자동 조회한다(수동 새로고침 전 빈 화면 방지, OWE-880).
            self.mobile_enter(cx);
        }
        if tab == AdminTab::Resources && self.space.report.is_none() && !self.space.scanning {
            self.scan_workspace_space(cx);
        }
        if tab == AdminTab::Resources {
            self.refresh_error_reporting(cx);
        }
        cx.notify();
    }

    /// 파괴적 동작 버튼(위험 색). 첫 클릭은 "다시 눌러 확인"으로 무장만 하고, 두 번째 클릭에 실행한다.
    /// 무장은 버튼 바깥 클릭·Tab/Esc·[`CONFIRM_TIMEOUT`] 경과 시 풀린다(OWE-704 실화면 QA). 폭은 [`layout::danger`]가 고정.
    fn confirm_button(
        &self,
        id: impl Into<SharedString>,
        label: &'static str,
        cx: &mut Context<Self>,
        run: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Button {
        let id: SharedString = id.into();
        let armed = self.armed.as_ref() == Some(&id);
        let selector = id.to_string();
        let out_id = id.clone();
        layout::danger(id.clone(), label, text::CONFIRM_AGAIN.get(), armed)
            .debug_selector(move || selector.clone())
            .when(armed, |b| {
                b.on_mouse_down_out(cx.listener(move |this, _, _, cx| {
                    if this.armed.as_ref() == Some(&out_id) {
                        this.disarm(cx);
                    }
                }))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.armed.as_ref() == Some(&id) {
                    this.disarm(cx);
                    run(this, cx);
                } else {
                    this.armed = Some(id.clone());
                    // 새 타이머가 이전 타이머(Task)를 버려 취소한다.
                    this.armed_timer = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(CONFIRM_TIMEOUT).await;
                        // disarm()은 자기 Task를 버리므로 여기선 상태만 푼다.
                        this.update(cx, |this, cx| {
                            this.armed = None;
                            cx.notify();
                        })
                        .ok();
                    }));
                }
                cx.notify();
            }))
    }

    fn disarm(&mut self, cx: &mut Context<Self>) {
        if self.armed.take().is_some() {
            cx.notify();
        }
        self.armed_timer = None;
    }

    pub fn accounts(&self) -> &krill_proto::usage::AccountsSnapshot {
        &self.accounts_snapshot
    }

    #[must_use]
    pub fn account_host(&self) -> &crate::hosts::HostId {
        &self.account_host
    }

    #[must_use]
    pub fn account_hosts(
        &self,
    ) -> &[(
        crate::hosts::HostId,
        String,
        Option<crate::hosts::HostTarget>,
    )] {
        &self.account_hosts
    }

    fn selected_account_target(&self) -> Option<crate::hosts::HostTarget> {
        self.account_hosts
            .iter()
            .find(|(h, _, _)| h == &self.account_host)
            .and_then(|(_, _, target)| target.clone())
    }

    /// 메인 셸(`Shell`)에서 연결된 원격 호스트 목록·타깃·계정 스냅샷을 설정 뷰와 동기화한다.
    pub fn sync_account_hosts(
        &mut self,
        hosts: Vec<(
            crate::hosts::HostId,
            String,
            Option<crate::hosts::HostTarget>,
        )>,
        snapshots: std::collections::BTreeMap<
            crate::hosts::HostId,
            krill_proto::usage::AccountsSnapshot,
        >,
        cx: &mut Context<Self>,
    ) {
        self.account_hosts = hosts;
        for (host, snap) in snapshots {
            if host.is_local() {
                self.local_accounts_snapshot = snap.clone();
            } else {
                self.remote_accounts_snapshots
                    .insert(host.clone(), snap.clone());
            }
            if !self.account_rule_pending && self.account_host == host {
                self.accounts_snapshot = snap;
            }
        }
        if self.account_host.is_remote()
            && !self
                .account_hosts
                .iter()
                .any(|(h, _, _)| h == &self.account_host)
        {
            self.select_account_host(crate::hosts::HostId::Local, cx);
            return;
        }
        cx.notify();
    }

    /// `AI 제공자 계정` 탭에서 관리할 대상 호스트를 전환한다.
    pub fn select_account_host(&mut self, host: crate::hosts::HostId, cx: &mut Context<Self>) {
        if self.account_host != host {
            if self.account_host.is_local() {
                self.local_accounts_snapshot = self.accounts_snapshot.clone();
            } else {
                self.remote_accounts_snapshots
                    .insert(self.account_host.clone(), self.accounts_snapshot.clone());
            }
            self.account_error = None;
            self.accounts_offline = false;
            self.terminal_login_provider = None;
            self.terminal_login_account = None;
            self.removing = None;
            self.rename = None;
            self.disarm(cx);
            self.accounts_snapshot = if host.is_local() {
                self.local_accounts_snapshot.clone()
            } else {
                self.remote_accounts_snapshots
                    .get(&host)
                    .cloned()
                    .unwrap_or_default()
            };
            self.orphan_profiles = if host.is_local() {
                Vec::new()
            } else {
                self.remote_orphan_profiles
                    .get(&host)
                    .cloned()
                    .unwrap_or_default()
            };
            self.account_host = host;
        }
        self.accounts_refresh_pending = false;
        self.refresh_accounts_async(cx);
        cx.notify();
    }

    pub fn update_accounts_snapshot(
        &mut self,
        snapshot: krill_proto::usage::AccountsSnapshot,
        cx: &mut Context<Self>,
    ) {
        self.update_accounts_snapshot_for_host(&crate::hosts::HostId::Local, snapshot, cx);
    }

    pub fn update_accounts_snapshot_for_host(
        &mut self,
        host: &crate::hosts::HostId,
        snapshot: krill_proto::usage::AccountsSnapshot,
        cx: &mut Context<Self>,
    ) {
        if host.is_local() {
            self.local_accounts_snapshot = snapshot.clone();
        } else {
            self.remote_accounts_snapshots
                .insert(host.clone(), snapshot.clone());
        }
        if &self.account_host != host || self.account_rule_pending {
            return;
        }
        if let Some(error) = snapshot
            .details
            .handoffs
            .iter()
            .find_map(|h| h.error.as_ref())
        {
            self.account_error = Some(error.clone().into());
        }
        self.accounts_snapshot = snapshot;
        cx.notify();
    }

    pub fn ssh_hosts(&self) -> &[crate::ssh_hosts::SshHost] {
        &self.ssh_hosts
    }

    pub fn set_account_rule(
        &mut self,
        rule: krill_proto::usage::AutoSwitchRule,
        cx: &mut Context<Self>,
    ) {
        if self.account_rule_pending || self.daemon_required(cx) {
            return;
        }
        if self.account_host.is_remote() {
            self.apply_account_request(
                krill_proto::usage::UsageRequest::UpsertAutoSwitchRule(rule),
                cx,
            );
            return;
        }
        let dir = state_dir(cx);
        if is_daemon_state_dir(dir.as_deref()) {
            self.spawn_account_rule_request(
                async move { crate::accounts::upsert_auto_switch_rule(rule) },
                cx,
            );
            return;
        }
        self.apply_account_request(
            krill_proto::usage::UsageRequest::UpsertAutoSwitchRule(rule),
            cx,
        );
    }

    pub fn clear_account_rule(
        &mut self,
        provider: krill_proto::usage::ProviderKind,
        from_account_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self.account_rule_pending || self.daemon_required(cx) {
            return;
        }
        if self.account_host.is_remote() {
            self.apply_account_request(
                krill_proto::usage::UsageRequest::RemoveAutoSwitchRule {
                    provider,
                    from_account_id: from_account_id.to_owned(),
                },
                cx,
            );
            return;
        }
        let dir = state_dir(cx);
        if is_daemon_state_dir(dir.as_deref()) {
            let from_account_id = from_account_id.to_owned();
            self.spawn_account_rule_request(
                async move { crate::accounts::remove_auto_switch_rule(provider, &from_account_id) },
                cx,
            );
            return;
        }
        self.apply_account_request(
            krill_proto::usage::UsageRequest::RemoveAutoSwitchRule {
                provider,
                from_account_id: from_account_id.to_owned(),
            },
            cx,
        );
    }

    /// 데몬 IPC(연결·읽기 타임아웃 포함)는 UI 스레드를 막지 않게 백그라운드에서 돌린다.
    fn spawn_account_rule_request(
        &mut self,
        request: impl std::future::Future<Output = Result<krill_proto::usage::AccountsSnapshot, String>>
        + Send
        + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.account_rule_pending {
            return;
        }
        self.account_rule_pending = true;
        self.account_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(request).await;
            this.update(cx, |this, cx| this.finish_account_rule_request(result, cx))
                .ok();
        })
        .detach();
    }

    fn finish_account_rule_request(
        &mut self,
        result: Result<krill_proto::usage::AccountsSnapshot, String>,
        cx: &mut Context<Self>,
    ) {
        self.account_rule_pending = false;
        match result {
            Ok(snap) => {
                if self.account_host.is_local() {
                    self.local_accounts_snapshot = snap.clone();
                } else {
                    self.remote_accounts_snapshots
                        .insert(self.account_host.clone(), snap.clone());
                }
                self.accounts_snapshot = snap;
                self.account_error = None;
                cx.defer(notify_external_change);
            }
            Err(error) => self.account_error = Some(error.into()),
        }
        cx.notify();
    }

    /// 렌더는 메모리 캐시만 읽고, 수정 시각 확인·본문 읽기는 백그라운드로 보낸다(AS-06).
    #[cfg(all(test, unix))]
    fn cached_agents_file(&mut self, dir: Option<&Path>, cx: &mut Context<Self>) -> Option<String> {
        self.ensure_agents_file(dir, cx);
        self.agents_file_cache
            .0
            .as_ref()
            .and_then(|(_, body)| body.clone())
    }

    fn ensure_agents_file(&mut self, dir: Option<&Path>, cx: &mut Context<Self>) {
        let Some(dir) = dir else {
            return;
        };
        if self.agents_file_cache.1 {
            return;
        }
        self.agents_file_cache.1 = true;
        let path = dir.join(krill_proto::agent::FILE_NAME);
        let previous = self.agents_file_cache.0.as_ref().map(|(stamp, _)| *stamp);
        let revision = self.file_revision;
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                    if previous == Some(stamp) {
                        None
                    } else {
                        let body = std::fs::read_to_string(path).ok();
                        let parsed = std::sync::Arc::new(
                            krill_proto::agent::Agents::load(body.as_deref()).unwrap_or_default(),
                        );
                        Some(((stamp, body), parsed))
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.agents_file_cache.1 = false;
                if this.file_revision != revision {
                    return;
                }
                if let Some((loaded, parsed)) = loaded {
                    this.agents_file_reads += 1;
                    this.agents_file_cache.0 = Some(loaded);
                    this.agents_parsed = Some(parsed);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 데몬 없이 계정을 바꾸려 하면 막고 "데몬 연결 필요"를 띄운 뒤 다시 연결을 확인한다(OWE-697).
    fn daemon_required(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.accounts_offline {
            return false;
        }
        self.account_error = Some(text::ACCOUNTS_DAEMON_REQUIRED.get().into());
        cx.notify();
        self.refresh_accounts_async(cx);
        true
    }

    /// 계정·호스트 스냅샷을 백그라운드에서 갱신한다. 파일·디렉터리 스캔(`read_codex_rollout`
    /// 포함)을 UI 스레드에서 동기 수행하면 클릭 반응이 늦어진다(AS-06). 진행 중
    /// 요청은 합쳐 하나로만 돌린다. 손상된 `accounts.toml`은 오류로 알린다(AS-04).
    /// 실제 상태 디렉터리에서는 데몬 `ListAccounts` 응답만 쓰고(상태바와 같은 값), 데몬에
    /// 닿지 못하면 로컬 파일을 읽기 전용으로 보인다(OWE-697).
    fn refresh_accounts_if_changed(&mut self, cx: &mut Context<Self>) {
        if self.account_probe_pending {
            return;
        }
        self.account_probe_pending = true;
        let dir = state_dir(cx);
        cx.spawn(async move |this, cx| {
            let stamp = cx
                .background_executor()
                .spawn(async move { account_stamps(dir.as_deref()) })
                .await;
            this.update(cx, |this, cx| {
                this.account_probe_pending = false;
                if this.account_file_stamp != stamp {
                    this.account_file_stamp = stamp;
                    this.refresh_accounts_async(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn refresh_accounts_async(&mut self, cx: &mut Context<Self>) {
        if self.accounts_refresh_pending {
            return;
        }
        self.accounts_refresh_pending = true;
        if self.account_host.is_remote() {
            let host = self.account_host.clone();
            let target = self.selected_account_target();
            let before = (self.accounts_snapshot.clone(), self.account_error.clone());
            cx.notify();
            cx.spawn(async move |this, cx| {
                let loaded = cx
                    .background_executor()
                    .spawn(async move {
                        let target = target.ok_or_else(|| "remote host disconnected".to_owned())?;
                        let snapshot = crate::accounts::request_target(
                            &target,
                            &krill_proto::usage::UsageRequest::ListAccounts,
                        )?;
                        let orphans = crate::accounts::orphans_target(
                            &target,
                            &krill_proto::usage::UsageRequest::ListOrphanProfiles,
                        )
                        .unwrap_or_default();
                        Ok::<_, String>((snapshot, orphans))
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.accounts_refresh_pending = false;
                    if this.account_host != host {
                        return;
                    }
                    this.accounts_offline = false;
                    match loaded {
                        Ok((snapshot, orphans)) => {
                            this.orphan_profiles.clone_from(&orphans);
                            this.remote_orphan_profiles.insert(host.clone(), orphans);
                            this.remote_accounts_snapshots
                                .insert(host, snapshot.clone());
                            if !this.account_rule_pending
                                && this.accounts_snapshot == before.0
                                && this.account_error == before.1
                            {
                                this.accounts_snapshot = snapshot;
                                this.account_error = None;
                            }
                        }
                        Err(error) => {
                            this.account_error = Some(error.into());
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }
        let file_revision = self.file_revision;
        cx.notify();
        let before = (self.accounts_snapshot.clone(), self.account_error.clone());
        let home = crate::orca_import::state(cx)
            .and_then(|s| s.home_override.clone())
            .unwrap_or_else(crate::accounts::default_home);
        let dir = state_dir(cx);
        let daemon = is_daemon_state_dir(dir.as_deref());
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let remote = daemon
                        .then(|| krill_proto::daemon::Paths::dev().ok())
                        .flatten()
                        .and_then(|paths| {
                            crate::accounts::request_ipc(
                                &paths,
                                &krill_proto::usage::UsageRequest::ListAccounts,
                            )
                            .ok()
                        });
                    let offline = daemon && remote.is_none();
                    // OWE-704: 구 데몬(FEAT_USAGE_MANAGE 없음)이면 목록 없이 둔다.
                    let orphans = if daemon {
                        krill_proto::daemon::Paths::dev()
                            .ok()
                            .and_then(|paths| {
                                crate::accounts::orphans_ipc(
                                    &paths,
                                    &krill_proto::usage::UsageRequest::ListOrphanProfiles,
                                )
                                .ok()
                            })
                            .unwrap_or_default()
                    } else {
                        crate::accounts::orphans_local(&home, dir.as_deref())
                    };
                    let snapshot = remote
                        .unwrap_or_else(|| crate::accounts::refresh_local(&home, dir.as_deref()));
                    let hosts = crate::ssh_hosts::load_hosts(dir.as_deref());
                    let problem = crate::accounts::load_problem(&home, dir.as_deref());
                    (
                        snapshot,
                        hosts,
                        problem,
                        offline,
                        orphans,
                        account_stamps(dir.as_deref()),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                this.accounts_refresh_pending = false;
                if this.account_host.is_remote() {
                    if this.file_revision == file_revision {
                        this.ssh_hosts = loaded.1;
                    }
                    this.local_accounts_snapshot = loaded.0;
                    cx.notify();
                    return;
                }
                this.accounts_offline = loaded.3;
                this.orphan_profiles = loaded.4;
                this.account_file_stamp = loaded.5;
                if daemon && !loaded.3 {
                    this.probe_identities(cx);
                }
                // 시작 뒤 처리된 계정 요청의 성공·거부는 오래된 갱신보다 최신이다.
                if !this.account_rule_pending
                    && this.accounts_snapshot == before.0
                    && this.account_error == before.1
                {
                    let mut next = loaded.0;
                    if !daemon {
                        Self::merge_local_snapshot_tails(&this.accounts_snapshot, &mut next);
                    }
                    this.local_accounts_snapshot = next.clone();
                    this.accounts_snapshot = next;
                    this.account_error = loaded.2.map(SharedString::from);
                }
                if this.file_revision == file_revision {
                    this.ssh_hosts = loaded.1;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn refresh_accounts_manually(&mut self, cx: &mut Context<Self>) {
        let daemon = self.account_host.is_remote() || is_daemon_state_dir(state_dir(cx).as_deref());
        self.apply_account_request(krill_proto::usage::UsageRequest::ListAccounts, cx);
        if daemon {
            cx.spawn(async move |this, cx| {
                for delay_ms in [700_u64, 1_100] {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(delay_ms))
                        .await;
                    this.update(cx, |this, cx| {
                        this.refresh_accounts_async(cx);
                    })
                    .ok();
                }
            })
            .detach();
        }
    }

    fn merge_local_snapshot_tails(
        prev: &krill_proto::usage::AccountsSnapshot,
        next: &mut krill_proto::usage::AccountsSnapshot,
    ) {
        if prev.connections.supported {
            next.connections.clone_from(&prev.connections);
        }
        if prev.advanced.supported {
            next.advanced.clone_from(&prev.advanced);
        }
        if prev.details.supported {
            next.details.clone_from(&prev.details);
        }
        if !prev.statuses.is_empty() && next.statuses.is_empty() {
            next.statuses.clone_from(&prev.statuses);
        }
        for acc in &mut next.accounts {
            if acc.usage.is_empty()
                && let Some(prev_acc) = prev
                    .accounts
                    .iter()
                    .find(|a| a.id == acc.id && !a.usage.is_empty())
            {
                acc.usage.clone_from(&prev_acc.usage);
            }
        }
    }

    /// Claude 계정 로그인을 `claude auth status`로 확인한다(OWE-705). 처음 보는 계정만, 백그라운드에서.
    /// 이메일이 빈 계정은 확인된 이메일로 채우고, 자동 라벨(`Claude Code Profile #n`)은 조직 이름으로 바꾼다.
    /// 명령이 없거나 실패하면 아무것도 하지 않는다. 데몬이 있는 실제 상태 디렉터리에서만 부른다.
    fn probe_identities(&mut self, cx: &mut Context<Self>) {
        use krill_proto::usage::{ProviderKind, UsageRequest};
        let todo: Vec<_> = self
            .accounts_snapshot
            .accounts
            .iter()
            .filter(|a| {
                a.provider == ProviderKind::Claude
                    && !crate::accounts::orca_managed(a)
                    && !self.identities.1.contains(&a.id)
            })
            .cloned()
            .collect();
        if todo.is_empty() {
            return;
        }
        self.identities.1.extend(todo.iter().map(|a| a.id.clone()));
        let before = self.accounts_snapshot.clone();
        cx.spawn(async move |this, cx| {
            let (found, snapshot) = cx
                .background_executor()
                .spawn(async move {
                    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
                    let path = crate::onboarding::login_path(&shell)
                        .unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
                    let Some(exe) = krill_proto::agent::which("claude", &path) else {
                        return (Vec::new(), None);
                    };
                    let paths = krill_proto::daemon::Paths::dev().ok();
                    let mut found = Vec::new();
                    let mut snapshot = None;
                    for acc in todo {
                        let dir = (!acc.host_default).then(|| PathBuf::from(&acc.profile_dir));
                        let Some(identity) = crate::accounts::claude_identity(&exe, dir.as_deref())
                        else {
                            continue;
                        };
                        if !acc.host_default && acc.email.as_deref().is_none_or(str::is_empty) {
                            let label = if crate::accounts::auto_label(&acc) {
                                identity
                                    .org_name
                                    .clone()
                                    .unwrap_or_else(|| identity.email.clone())
                            } else {
                                acc.label.clone()
                            };
                            let req = UsageRequest::RenameAccount {
                                provider: acc.provider,
                                account_id: acc.id.clone(),
                                label,
                                email: Some(identity.email.clone()),
                            };
                            if let Some(paths) = paths.as_ref()
                                && let Ok(snap) = crate::accounts::request_ipc(paths, &req)
                            {
                                snapshot = Some(snap);
                            }
                        }
                        found.push((acc.id, identity));
                    }
                    (found, snapshot)
                })
                .await;
            this.update(cx, |this, cx| {
                this.identities.0.extend(found);
                if !this.account_rule_pending
                    && this.accounts_snapshot == before
                    && let Some(snap) = snapshot
                {
                    this.accounts_snapshot = snap;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 고아 프로필 폴더를 휴지통으로 옮긴다(OWE-704). 실제 상태 디렉터리에서는 데몬이 세션이 쓰는 폴더를 건너뛴다.
    fn purge_orphan_profiles(&mut self, cx: &mut Context<Self>) {
        if self.daemon_required(cx) {
            return;
        }
        if self.account_host.is_remote() {
            let host = self.account_host.clone();
            let target = self.selected_account_target();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let target = target.ok_or_else(|| "remote host disconnected".to_owned())?;
                        crate::accounts::orphans_target(
                            &target,
                            &krill_proto::usage::UsageRequest::PurgeOrphanProfiles,
                        )
                    })
                    .await;
                this.update(cx, |this, cx| {
                    match result {
                        Ok(left) => {
                            this.remote_orphan_profiles
                                .insert(host.clone(), left.clone());
                            if this.account_host == host {
                                this.orphan_profiles = left;
                            }
                        }
                        Err(e) => this.account_error = Some(e.into()),
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }
        let dir = state_dir(cx);
        if !is_daemon_state_dir(dir.as_deref()) {
            let home = crate::accounts::default_home();
            self.file_job(
                "account-local",
                move || {
                    let result = crate::accounts::request_local(
                        &home,
                        dir.as_deref(),
                        &krill_proto::usage::UsageRequest::PurgeOrphanProfiles,
                    );
                    let orphans = crate::accounts::orphans_local(&home, dir.as_deref());
                    (result, orphans)
                },
                |this, (result, orphans), cx| {
                    match result {
                        Ok(snapshot) => {
                            this.accounts_snapshot = snapshot;
                            this.orphan_profiles = orphans;
                        }
                        Err(error) => this.account_error = Some(error.to_string().into()),
                    }
                    cx.notify();
                },
                cx,
            );
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let paths = krill_proto::daemon::Paths::dev().map_err(|e| e.to_string())?;
                    crate::accounts::orphans_ipc(
                        &paths,
                        &krill_proto::usage::UsageRequest::PurgeOrphanProfiles,
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(left) => this.orphan_profiles = left,
                    Err(e) => this.account_error = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Claude·Codex는 백그라운드, agy·OpenCode는 안내 후 터미널에서 로그인한다.
    /// 원격 호스트를 선택 중이면 모든 제공자를 원격 터미널 안내 카드로 처리한다.
    /// 브라우저 로그인이 끝나기 전에는 `login_pending`을 유지해 진행 표시·[링크 복사]·[취소]를 보이고,
    /// 스레드가 남긴 [`crate::accounts::LoginOutcome`]으로 성공·실패·취소를 닫는다(OWE-881).
    fn login_account(
        &mut self,
        provider: krill_proto::usage::ProviderKind,
        account: Option<krill_proto::usage::AccountInfo>,
        cx: &mut Context<Self>,
    ) {
        self.login_account_with_mode(provider, account, true, cx);
    }

    fn login_account_with_mode(
        &mut self,
        provider: krill_proto::usage::ProviderKind,
        account: Option<krill_proto::usage::AccountInfo>,
        open_browser: bool,
        cx: &mut Context<Self>,
    ) {
        use krill_proto::usage::UsageRequest;
        if self.account_rule_pending || self.login_pending.is_some() {
            return;
        }
        if self.account_host.is_remote()
            || !matches!(
                provider,
                krill_proto::usage::ProviderKind::Claude | krill_proto::usage::ProviderKind::Codex
            )
        {
            // 대화형 로그인은 안내 카드에서 [터미널에서 실행]을 눌러야 명령이 간다.
            // 예전처럼 호출 즉시 큐에 넣고 설정 화면을 닫지 않는다.
            self.terminal_login_provider = Some(provider);
            self.terminal_login_account = account;
            self.account_error = None;
            cx.notify();
            return;
        }
        if self.daemon_required(cx) {
            return;
        }
        let n = self.added_counter;
        self.added_counter = self.added_counter.saturating_add(1);
        self.account_rule_pending = true;
        self.account_error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let paths = krill_proto::daemon::Paths::dev().map_err(|e| e.to_string())?;
                    let (account, newly_added) = if let Some(account) = account {
                        (account, false)
                    } else {
                        // 같은 라벨의 AddAccount는 기존 계정을 고친다(멱등). 화면 스냅샷은 비었거나 낡을 수 있어
                        // (설정을 막 열었거나 CLI로 추가) 데몬 목록을 새로 받아 라벨을 고른다.
                        let existing =
                            crate::accounts::request_ipc(&paths, &UsageRequest::ListAccounts)?
                                .accounts;
                        let mut next = n;
                        let label = loop {
                            let label = format!("{} Profile #{next}", provider.display_name());
                            if !existing
                                .iter()
                                .any(|a| a.provider == provider && a.label == label)
                            {
                                break label;
                            }
                            next = next.saturating_add(1);
                        };
                        let snap = crate::accounts::request_ipc(
                            &paths,
                            &UsageRequest::AddAccount {
                                provider,
                                label: label.clone(),
                                email: None,
                                profile_dir: String::new(),
                            },
                        )?;
                        let added = snap
                            .accounts
                            .into_iter()
                            .find(|a| a.provider == provider && a.label == label)
                            .ok_or_else(|| "new account missing from daemon reply".to_owned())?;
                        // 그사이 같은 라벨이 생겨 기존 계정이 돌아왔으면, 로그인을 취소해도 정리(삭제)하지 않는다.
                        let newly_added = !existing.iter().any(|a| a.id == added.id);
                        (added, newly_added)
                    };
                    let cancel = crate::accounts::start_login_with_mode(
                        &paths,
                        &account,
                        newly_added,
                        open_browser,
                    )?;
                    Ok::<_, String>((account, cancel))
                })
                .await;
            this.update(cx, |this, cx| {
                this.account_rule_pending = false;
                // 갱신을 먼저 예약해야 뒤이은 오류가 갱신 완료에 지워지지 않는다.
                this.refresh_accounts_async(cx);
                match result {
                    Ok((account, cancel)) => {
                        let initial_url = crate::accounts::login_url(&account.id);
                        let copied = !open_browser && initial_url.is_some();
                        if let Some(url) = initial_url.as_ref().filter(|_| !open_browser) {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
                        }
                        this.login_pending = Some(PendingLogin {
                            account_id: account.id.clone(),
                            cancel,
                            cancelling: false,
                            started: std::time::Instant::now(),
                            url: initial_url,
                            copy_on_ready: !open_browser && !copied,
                            copied,
                        });
                        // 결과 폴링: 스레드가 남긴 LoginOutcome이 있거나 타임아웃이면 닫는다.
                        cx.spawn(async move |this, cx| {
                            loop {
                                let wait = this
                                    .read_with(cx, |this, _| {
                                        if this
                                            .login_pending
                                            .as_ref()
                                            .is_some_and(|p| p.url.is_none())
                                        {
                                            LOGIN_URL_POLL_INTERVAL
                                        } else {
                                            LOGIN_POLL_INTERVAL
                                        }
                                    })
                                    .unwrap_or(LOGIN_POLL_INTERVAL);
                                cx.background_executor().timer(wait).await;
                                let settled = this
                                    .update(cx, SettingsView::poll_login_once)
                                    .unwrap_or(true);
                                if settled {
                                    break;
                                }
                            }
                        })
                        .detach();
                    }
                    Err(e) => this.account_error = Some(e.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 로그인 결과 폴링 한 걸음(OWE-881). 끝나면 `true`. 테스트에서 직접 부른다.
    fn poll_login_once(&mut self, cx: &mut Context<Self>) -> bool {
        let (account_id, timed_out) = match self.login_pending.as_ref() {
            Some(pending) => (
                pending.account_id.clone(),
                pending.started.elapsed() >= LOGIN_WAIT_TIMEOUT,
            ),
            None => return true,
        };
        if let Some(pending) = self.login_pending.as_mut()
            && pending.url.is_none()
            && let Some(url) = crate::accounts::login_url(&account_id)
        {
            if pending.copy_on_ready {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
                pending.copy_on_ready = false;
                pending.copied = true;
            }
            pending.url = Some(url);
            cx.notify();
        }
        match crate::accounts::take_login_outcome(&account_id) {
            None => {
                if !timed_out {
                    return false;
                }
                // 갱신을 먼저 예약해야 완료 시점이 오류보다 앞서 오류가 유지된다.
                // 반대로 하면 갱신 완료가 시간 초과 안내를 지워 버린다.
                self.login_pending = None;
                self.refresh_accounts_async(cx);
                self.account_error = Some(text::ACCOUNT_LOGIN_TIMED_OUT.get().into());
                cx.notify();
                true
            }
            Some(outcome) => {
                self.login_pending = None;
                self.refresh_accounts_async(cx);
                match outcome {
                    crate::accounts::LoginOutcome::Succeeded
                    | crate::accounts::LoginOutcome::Cancelled => {}
                    crate::accounts::LoginOutcome::Failed(reason) => {
                        let msg = match reason {
                            crate::accounts::LoginFailReason::TimedOut => {
                                text::ACCOUNT_LOGIN_TIMED_OUT.get()
                            }
                            crate::accounts::LoginFailReason::SpawnFailed => {
                                text::ACCOUNT_LOGIN_SPAWN_FAILED.get()
                            }
                            crate::accounts::LoginFailReason::Unsuccessful => {
                                text::ACCOUNT_LOGIN_FAILED.get()
                            }
                        };
                        self.account_error = Some(msg.into());
                    }
                }
                cx.notify();
                true
            }
        }
    }

    /// 진행 중 브라우저 로그인의 인증 링크를 클립보드에 복사한다.
    /// 아직 CLI가 링크를 내보내기 전이면 준비되는 즉시 복사하도록 예약한다.
    fn copy_pending_login_link(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.login_pending.as_mut() else {
            return;
        };
        if pending.url.is_none() {
            pending.url = crate::accounts::login_url(&pending.account_id);
        }
        if let Some(url) = pending.url.clone() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
            pending.copy_on_ready = false;
            pending.copied = true;
        } else {
            pending.copy_on_ready = true;
        }
        cx.notify();
    }

    /// 진행 중 브라우저 로그인을 취소한다(OWE-881). 스레드가 자식 CLI를 죽이고
    /// 정리한 뒤 남긴 `Cancelled`를 모니터가 보고 `login_pending`을 치운다.
    fn cancel_login(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.login_pending.as_mut() else {
            return;
        };
        if pending.cancelling {
            return;
        }
        pending.cancelling = true;
        pending
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        cx.notify();
    }

    /// 대화형 로그인 안내 카드의 [터미널에서 실행]. 사용자가 직접 누른 뒤라
    /// 설정 화면이 닫히고 활성 터미널에서 로그인 명령이 실행돼도 된다.
    fn run_terminal_login(&mut self, cx: &mut Context<Self>) {
        use krill_proto::usage::UsageRequest;
        if self.account_rule_pending || self.daemon_required(cx) {
            return;
        }
        let Some(provider) = self.terminal_login_provider.take() else {
            return;
        };
        let account = self.terminal_login_account.take();
        let host = self.account_host.clone();
        let remote_target = host.is_remote().then(|| self.selected_account_target());
        let dir = state_dir(cx);
        let daemon = is_daemon_state_dir(dir.as_deref());
        let n = self.added_counter;
        let home = crate::orca_import::state(cx)
            .and_then(|s| s.home_override.clone())
            .or_else(|| {
                cfg!(test)
                    .then(|| dir.as_ref().map(|d| d.join("test-home")))
                    .flatten()
            })
            .unwrap_or_else(crate::accounts::default_home);
        self.added_counter = n.saturating_add(1);
        let request = move || {
            let paths = if remote_target.is_none() && daemon {
                Some(krill_proto::daemon::Paths::dev().map_err(|e| e.to_string())?)
            } else {
                None
            };
            let send = |req: &UsageRequest| match &remote_target {
                Some(Some(target)) => crate::accounts::request_target(target, req),
                Some(None) => Err("remote host disconnected".to_owned()),
                None => match &paths {
                    Some(paths) => crate::accounts::request_ipc(paths, req),
                    None => crate::accounts::request_local(&home, dir.as_deref(), req)
                        .map_err(|e| e.to_string()),
                },
            };
            let snap = send(&UsageRequest::ListAccounts)?;
            let account = if let Some(account) = account {
                account
            } else {
                let mut next = n;
                let label = loop {
                    let label = format!("{} Profile #{next}", provider.display_name());
                    if !snap
                        .accounts
                        .iter()
                        .any(|a| a.provider == provider && a.label == label)
                    {
                        break label;
                    }
                    next = next.saturating_add(1);
                };
                send(&UsageRequest::AddAccount {
                    provider,
                    label: label.clone(),
                    email: None,
                    profile_dir: String::new(),
                })?
                .accounts
                .into_iter()
                .find(|a| a.provider == provider && a.label == label)
                .ok_or_else(|| "new account profile missing".to_owned())?
            };
            Ok::<_, String>((
                send(&UsageRequest::ListAccounts)?,
                crate::accounts::terminal_login_command(&account),
            ))
        };
        {
            self.account_rule_pending = true;
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { request() })
                    .await;
                this.update(cx, |this, cx| {
                    this.account_rule_pending = false;
                    match result {
                        Ok((snap, command)) => {
                            if host.is_local() {
                                this.local_accounts_snapshot = snap.clone();
                            } else {
                                this.remote_accounts_snapshots
                                    .insert(host.clone(), snap.clone());
                            }
                            if this.account_host == host {
                                this.accounts_snapshot = snap;
                            }
                            this.account_error = None;
                            queue_terminal_command_on(host, &command, cx);
                        }
                        Err(error) => this.account_error = Some(error.into()),
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        cx.notify();
    }

    fn opencode_home_and_state(cx: &App) -> (std::path::PathBuf, Option<std::path::PathBuf>) {
        let dir = state_dir(cx);
        let home = crate::orca_import::state(cx)
            .and_then(|s| s.home_override.clone())
            .or_else(|| {
                cfg!(test)
                    .then(|| dir.as_ref().map(|d| d.join("test-home")))
                    .flatten()
            })
            .unwrap_or_else(crate::accounts::default_home);
        (home, dir)
    }

    fn ensure_opencode_api_key_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<gpui_component::input::InputState> {
        if let Some(input) = self.opencode_api_key_input.clone() {
            return input;
        }
        let input = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::OPENCODE_APIKEY_PLACEHOLDER.get())
                .masked(true)
        });
        self.opencode_api_key_input = Some(input.clone());
        input
    }

    fn save_opencode_api_key(&mut self, cx: &mut Context<Self>) {
        let key = self
            .opencode_api_key_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        if key.is_empty() {
            self.opencode_api_key_error = Some(text::OPENCODE_APIKEY_EMPTY.get().into());
            cx.notify();
            return;
        }
        let (home, dir) = Self::opencode_home_and_state(cx);
        self.opencode_api_key_pending = true;
        self.opencode_api_key_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_executor()
                .spawn(async move {
                    crate::accounts::opencode_go::save_override(&home, dir.as_deref(), &key)
                })
                .await;
            this.update(cx, |this, cx| {
                this.opencode_api_key_pending = false;
                match saved {
                    Ok(()) => {
                        this.opencode_api_key_configured = true;
                        this.opencode_api_key_error = None;
                        this.opencode_api_key_input = None;
                    }
                    Err(e) => this.opencode_api_key_error = Some(e.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn clear_opencode_api_key(&mut self, cx: &mut Context<Self>) {
        let (home, dir) = Self::opencode_home_and_state(cx);
        self.opencode_api_key_pending = true;
        self.opencode_api_key_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let cleared = cx
                .background_executor()
                .spawn(async move {
                    crate::accounts::opencode_go::clear_override(&home, dir.as_deref())
                })
                .await;
            this.update(cx, |this, cx| {
                this.opencode_api_key_pending = false;
                match cleared {
                    Ok(()) => {
                        this.opencode_api_key_configured = false;
                        this.opencode_api_key_error = None;
                    }
                    Err(e) => this.opencode_api_key_error = Some(e.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn apply_account_request(
        &mut self,
        req: krill_proto::usage::UsageRequest,
        cx: &mut Context<Self>,
    ) {
        let listing = matches!(req, krill_proto::usage::UsageRequest::ListAccounts);
        if !listing && self.daemon_required(cx) {
            return;
        }
        if self.account_host.is_remote() {
            if listing {
                self.refresh_accounts_async(cx);
                return;
            }
            if self.account_rule_pending {
                return;
            }
            let Some(target) = self.selected_account_target() else {
                self.account_error = Some("remote host disconnected".into());
                cx.notify();
                return;
            };
            self.spawn_account_rule_request(
                async move { crate::accounts::request_target(&target, &req) },
                cx,
            );
            return;
        }
        let dir = state_dir(cx);
        // 테스트용 임시 상태 디렉터리에서는 데몬 없이 로컬 파일로 동기 처리한다.
        if !is_daemon_state_dir(dir.as_deref()) {
            match &req {
                krill_proto::usage::UsageRequest::SetDirectUsage(on)
                    if self.accounts_snapshot.connections.supported =>
                {
                    self.accounts_snapshot.connections.direct_usage = *on;
                    self.account_error = None;
                    cx.defer(notify_external_change);
                    cx.notify();
                    return;
                }
                krill_proto::usage::UsageRequest::SetAdvancedPolicy(policy)
                    if self.accounts_snapshot.advanced.supported =>
                {
                    self.accounts_snapshot.advanced.policy.clone_from(policy);
                    self.account_error = None;
                    cx.defer(notify_external_change);
                    cx.notify();
                    return;
                }
                _ => {}
            }
            let home = crate::accounts::default_home();
            let prev = self.accounts_snapshot.clone();
            self.account_error = None;
            self.file_job(
                "account-local",
                move || crate::accounts::request_local(&home, dir.as_deref(), &req),
                move |this, result, cx| match result {
                    Ok(mut snap) => {
                        Self::merge_local_snapshot_tails(&prev, &mut snap);
                        this.accounts_snapshot = snap;
                        this.account_error = None;
                        cx.defer(notify_external_change);
                    }
                    Err(error) => this.account_error = Some(error.to_string().into()),
                },
                cx,
            );
            return;
        }
        // 실제 상태 디렉터리에서는 데몬이 확정 경로다. 로컬을 먼저 쓰면 데몬
        // 거부와 갈라지는 split-brain이 되므로(AS-03) 데몬에만 백그라운드로 묻고,
        // 파일 스캔도 UI 스레드에서 하지 않는다(AS-06).
        if listing {
            self.refresh_accounts_async(cx);
            return;
        }
        if self.account_rule_pending {
            return;
        }
        self.account_rule_pending = true;
        self.account_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let paths = krill_proto::daemon::Paths::dev().ok();
                    crate::accounts::request(paths.as_ref(), &req)
                })
                .await;
            this.update(cx, |this, cx| {
                this.finish_account_rule_request(result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn reset_credit_count(&self, account_id: &str) -> Option<u64> {
        self.accounts_snapshot
            .meters_for(account_id)?
            .meters
            .iter()
            .find(|m| m.key == krill_proto::usage::RESET_CREDITS_METER_KEY)?
            .value
            .as_deref()?
            .parse::<u64>()
            .ok()
    }

    fn consume_reset_credit(
        &mut self,
        provider: krill_proto::usage::ProviderKind,
        account_id: String,
        cx: &mut Context<Self>,
    ) {
        use crate::i18n::catalog::status as status_text;
        use krill_proto::usage::{ResetConsumeOutcome, UsageRequest};
        if self.account_rule_pending || self.daemon_required(cx) {
            return;
        }
        let host = self.account_host.clone();
        let target = if host.is_remote() {
            let Some(target) = self.selected_account_target() else {
                self.account_error = Some("remote host disconnected".into());
                cx.notify();
                return;
            };
            target
        } else {
            let Ok(paths) = krill_proto::daemon::Paths::dev() else {
                self.account_error = Some(text::ACCOUNTS_DAEMON_REQUIRED.get().into());
                cx.notify();
                return;
            };
            crate::hosts::HostTarget::local(paths)
        };
        let req = UsageRequest::ConsumeResetCredit {
            provider,
            account_id,
        };
        self.account_rule_pending = true;
        self.account_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let consumed = crate::accounts::consume_reset_credit_target(&target, &req)?;
                    drop(crate::accounts::request_target(
                        &target,
                        &UsageRequest::RefreshUsage,
                    ));
                    Ok::<_, String>(consumed)
                })
                .await;
            this.update(cx, |this, cx| {
                this.account_rule_pending = false;
                match result {
                    Ok(consumed) => {
                        if host.is_local() {
                            this.local_accounts_snapshot = consumed.snapshot.clone();
                        } else {
                            this.remote_accounts_snapshots
                                .insert(host.clone(), consumed.snapshot.clone());
                        }
                        if this.account_host == host {
                            this.accounts_snapshot = consumed.snapshot;
                        }
                        match consumed.outcome {
                            ResetConsumeOutcome::Reset | ResetConsumeOutcome::AlreadyRedeemed => {
                                this.account_error = None;
                                cx.defer(notify_external_change);
                            }
                            ResetConsumeOutcome::NoCredit => {
                                this.account_error =
                                    Some(status_text::PROVIDER_RESET_USAGE_NO_CREDIT.get().into());
                            }
                            ResetConsumeOutcome::NothingToReset => {
                                this.account_error =
                                    Some(status_text::PROVIDER_RESET_USAGE_NOTHING.get().into());
                            }
                        }
                    }
                    Err(err) => {
                        this.account_error = Some(err.into());
                    }
                }
                this.refresh_accounts_async(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn import_ssh_hosts(&mut self, cx: &mut Context<Self>) {
        let dir = state_dir(cx);
        self.file_job(
            "ssh-edit",
            move || crate::ssh_hosts::import_from_ssh_config(dir.as_deref(), None),
            |this, result, cx| match result {
                Ok(hosts) => {
                    this.ssh_hosts = hosts;
                    cx.defer(notify_external_change);
                }
                Err(error) => this.ssh_status = Some(error.to_string().into()),
            },
            cx,
        );
    }

    /// 호스트 목록이 바뀌었으면 같은 기기 중복을 백그라운드에서 다시 구한다.
    fn refresh_ssh_duplicates(&mut self, cx: &mut Context<Self>) {
        if self.ssh_duplicates_for.as_ref() == Some(&self.ssh_hosts) {
            return;
        }
        let hosts = self.ssh_hosts.clone();
        self.ssh_duplicates_for = Some(hosts.clone());
        cx.spawn(async move |this, cx| {
            let dups = cx
                .background_executor()
                .spawn(async move { crate::ssh_hosts::duplicate_hosts(&hosts) })
                .await;
            this.update(cx, |this, cx| {
                this.ssh_duplicates = dups;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 같은 기기 중복 등록을 지운다(먼저 등록한 항목만 남긴다).
    pub fn remove_ssh_duplicates(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .ssh_duplicates
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        let dir = state_dir(cx);
        let expected = self.ssh_hosts.clone();
        self.file_job(
            "ssh-edit",
            move || crate::ssh_hosts::remove_hosts_if_current(dir.as_deref(), &ids, &expected),
            move |this, result, cx| {
                match result {
                    Ok(hosts) => {
                        let live: std::collections::BTreeSet<_> =
                            hosts.iter().map(|h| h.id.clone()).collect();
                        this.ssh_hosts = hosts;
                        this.ssh_duplicates.clear();
                        this.ssh_host_statuses.retain(|id, _| live.contains(id));
                        this.ssh_remote_infos.retain(|id, _| live.contains(id));
                        cx.defer(notify_external_change);
                    }
                    Err(error) => this.ssh_status = Some(error.to_string().into()),
                }
                cx.notify();
            },
            cx,
        );
    }

    pub fn remove_ssh_host(&mut self, id: &str, cx: &mut Context<Self>) {
        let dir = state_dir(cx);
        let id = id.to_owned();
        let bg_id = id.clone();
        self.file_job(
            "ssh-edit",
            move || crate::ssh_hosts::remove_host(dir.as_deref(), &bg_id),
            move |this, result, cx| {
                let hosts = match result {
                    Ok(hosts) => hosts,
                    Err(error) => {
                        this.ssh_status = Some(error.to_string().into());
                        return;
                    }
                };
                this.ssh_hosts = hosts;
                this.ssh_host_statuses.remove(&id);
                this.ssh_remote_infos.remove(&id);
                if let Some(form) = &mut this.ssh_form
                    && form.editing_id.as_deref() == Some(id.as_str())
                {
                    form.editing_id = None;
                    form.error = None;
                }
                if this
                    .ssh_auth_failed_host
                    .as_ref()
                    .is_some_and(|h| h.id == id)
                {
                    this.ssh_auth_failed_host = None;
                }
                if this
                    .ssh_keygen_confirm_host
                    .as_ref()
                    .is_some_and(|h| h.id == id)
                {
                    this.ssh_keygen_confirm_host = None;
                }
                cx.defer(notify_external_change);
                cx.notify();
            },
            cx,
        );
    }

    pub fn ssh_host_status(&self, id: &str) -> SshHostCardStatus {
        self.ssh_host_statuses.get(id).copied().unwrap_or_default()
    }

    pub fn editing_ssh_host_id(&self) -> Option<&str> {
        self.ssh_form.as_ref().and_then(|f| f.editing_id.as_deref())
    }

    pub fn start_edit_ssh_host(
        &mut self,
        host: &crate::ssh_hosts::SshHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_ssh_form(window, cx);
        let Some(form) = &mut self.ssh_form else {
            return;
        };
        form.editing_id = Some(host.id.clone());
        form.error = None;
        let label_input = form.label.clone();
        let addr_input = form.address.clone();
        let port_input = form.port.clone();
        let key_input = form.identity_file.clone();
        let label = host.label.clone();
        let addr = host.destination.clone();
        let port = host.port.map(|p| p.to_string()).unwrap_or_default();
        let key = host.identity_file.clone().unwrap_or_default();
        label_input.update(cx, |s, cx| s.set_value(label, window, cx));
        addr_input.update(cx, |s, cx| s.set_value(addr, window, cx));
        port_input.update(cx, |s, cx| s.set_value(port, window, cx));
        key_input.update(cx, |s, cx| s.set_value(key, window, cx));
        cx.notify();
    }

    pub fn cancel_edit_ssh_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.ssh_form else {
            return;
        };
        form.editing_id = None;
        form.error = None;
        let label_input = form.label.clone();
        let addr_input = form.address.clone();
        let port_input = form.port.clone();
        let key_input = form.identity_file.clone();
        label_input.update(cx, |s, cx| s.set_value("", window, cx));
        addr_input.update(cx, |s, cx| s.set_value("", window, cx));
        port_input.update(cx, |s, cx| s.set_value("", window, cx));
        key_input.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    fn ensure_ssh_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.ssh_form.is_some() {
            return;
        }
        let label = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::SSH_NAME_PLACEHOLDER.get())
        });
        let address = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::SSH_ADDRESS_PLACEHOLDER.get())
        });
        let port = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::SSH_PORT_PLACEHOLDER.get())
        });
        let identity_file = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::SSH_KEY_PLACEHOLDER.get())
        });
        // 입력칸 Enter 제출(Orca `SshTargetForm` form onSubmit 대응, OWE-884).
        // 네 칸 중 어디서 Enter를 눌러도 추가·저장 버튼과 같은 경로로 제출한다.
        let mut subs = Vec::with_capacity(4);
        for input in [&label, &address, &port, &identity_file] {
            subs.push(cx.subscribe_in(
                input,
                window,
                |this, _, event: &gpui_component::input::InputEvent, window, cx| {
                    if matches!(event, gpui_component::input::InputEvent::PressEnter { .. }) {
                        this.submit_ssh_host_form(window, cx);
                    }
                },
            ));
        }
        self.ssh_form = Some(SshForm {
            label,
            address,
            port,
            identity_file,
            editing_id: None,
            error: None,
            _subs: subs,
        });
    }

    fn ssh_error_message(err: crate::ssh_hosts::HostInputError) -> &'static str {
        match err {
            crate::ssh_hosts::HostInputError::EmptyAddress => text::SSH_ERR_EMPTY_ADDRESS.get(),
            crate::ssh_hosts::HostInputError::InvalidAddress => text::SSH_ERR_INVALID_ADDRESS.get(),
            crate::ssh_hosts::HostInputError::InvalidPort => text::SSH_ERR_INVALID_PORT.get(),
            crate::ssh_hosts::HostInputError::InvalidIdentityFile => {
                text::SSH_ERR_INVALID_KEY.get()
            }
        }
    }

    /// Validate immediately and queue `hosts.toml` persistence; completion reports storage errors.
    /// Input fields are cleared only after a successful write and only if the user has not edited them.
    pub fn submit_ssh_host(
        &mut self,
        label: &str,
        address: &str,
        port: &str,
        identity_file: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::ssh_hosts::HostInputError> {
        let valid = match crate::ssh_hosts::validate_host_input(label, address, port, identity_file)
        {
            Ok(v) => v,
            Err(err) => {
                let msg: SharedString = Self::ssh_error_message(err).into();
                if let Some(form) = &mut self.ssh_form {
                    form.error = Some(msg.clone());
                }
                self.ssh_status = Some(msg);
                cx.notify();
                return Err(err);
            }
        };
        let dir = state_dir(cx);
        let editing_id = self.ssh_form.as_ref().and_then(|f| f.editing_id.clone());
        let initial_form_values = self.ssh_form.as_ref().map(|form| {
            [
                form.label.read(cx).value().to_string(),
                form.address.read(cx).value().to_string(),
                form.port.read(cx).value().to_string(),
                form.identity_file.read(cx).value().to_string(),
            ]
        });
        let submitted_edit_id = editing_id.clone();
        let label = valid.label.clone();
        let editing = editing_id.is_some();
        self.file_job(
            "ssh-edit",
            move || {
                if let Some(id) = &editing_id {
                    crate::ssh_hosts::update_host(
                        dir.as_deref(),
                        id,
                        &valid.label,
                        &valid.destination,
                        valid.port,
                        valid.identity_file,
                    )
                } else {
                    crate::ssh_hosts::add_host(
                        dir.as_deref(),
                        &valid.label,
                        &valid.destination,
                        valid.port,
                        valid.identity_file,
                    )
                }
            },
            move |this, result, cx| match result {
                Ok(hosts) => {
                    this.ssh_hosts = hosts;
                    cx.spawn(async move |this, cx| {
                        this.update_in(cx, |this, window, cx| {
                            let Some(form) = &mut this.ssh_form else {
                                return;
                            };
                            let Some(submitted) = initial_form_values else {
                                return;
                            };
                            let inputs =
                                [&form.label, &form.address, &form.port, &form.identity_file];
                            if form.editing_id != submitted_edit_id
                                || inputs
                                    .iter()
                                    .zip(&submitted)
                                    .any(|(input, value)| input.read(cx).value().as_str() != value)
                            {
                                return;
                            }
                            let inputs = inputs.map(Clone::clone);
                            form.editing_id = None;
                            form.error = None;
                            for input in inputs {
                                input.update(cx, |input, cx| input.set_value("", window, cx));
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .detach();
                    let message = if editing {
                        text::SSH_UPDATED
                    } else {
                        text::SSH_ADDED
                    };
                    this.ssh_status = Some(message.format(&[("label", &label)]).into());
                    cx.defer(notify_external_change);
                }
                Err(error) => {
                    let message: SharedString = error.to_string().into();
                    if let Some(form) = &mut this.ssh_form {
                        form.error = Some(message.clone());
                    }
                    this.ssh_status = Some(message);
                }
            },
            cx,
        );
        Ok(())
    }

    fn submit_ssh_host_form(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.ssh_form else {
            return;
        };
        let label = form.label.read(cx).value().to_string();
        let address = form.address.read(cx).value().to_string();
        let port = form.port.read(cx).value().to_string();
        let identity_file = form.identity_file.read(cx).value().to_string();
        self.submit_ssh_host(&label, &address, &port, &identity_file, cx)
            .ok();
    }

    /// Tailscale 피어 목록을 백그라운드 실행기에서 조회해 갱신한다(UI 스레드 블로킹 금지).
    pub fn refresh_tailscale_peers(&mut self, cx: &mut Context<Self>) {
        if self.tailscale_pending {
            return;
        }
        self.tailscale_loaded = true;
        if cfg!(test) && std::env::var_os("KRILL_TAILSCALE_BIN").is_none() {
            return;
        }
        self.tailscale_pending = true;
        cx.spawn(async move |this, cx| {
            let peers = cx
                .background_executor()
                .spawn(async move { crate::ssh_hosts::discover_tailscale_peers() })
                .await;
            this.update(cx, |this, cx| {
                this.tailscale_pending = false;
                this.tailscale_peers = peers;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn set_tailscale_peers(
        &mut self,
        peers: Vec<crate::ssh_hosts::TailscalePeer>,
        cx: &mut Context<Self>,
    ) {
        self.tailscale_loaded = true;
        self.tailscale_peers = peers;
        cx.notify();
    }

    pub fn add_tailscale_peer(
        &mut self,
        peer: &crate::ssh_hosts::TailscalePeer,
        cx: &mut Context<Self>,
    ) {
        let prev_editing = self
            .ssh_form
            .as_mut()
            .and_then(|form| form.editing_id.take());
        self.submit_ssh_host(&peer.hostname, &peer.address, "", "", cx)
            .ok();
        if let Some(id) = prev_editing
            && let Some(form) = &mut self.ssh_form
        {
            form.editing_id = Some(id);
        }
    }

    pub fn connect_ssh_host(&mut self, host: crate::ssh_hosts::SshHost, cx: &mut Context<Self>) {
        let cmd = crate::ssh_hosts::build_connect_command(&host)
            .trim()
            .to_owned();
        self.ssh_host_statuses
            .insert(host.id.clone(), SshHostCardStatus::Connected);
        self.ssh_status = Some(format!("{}: {cmd}", host.label).into());
        cx.defer(move |cx| {
            if cx.has_global::<Config>() {
                cx.global_mut::<Config>().pending_ssh_connect = Some(host);
            }
        });
        cx.notify();
    }

    pub fn disconnect_ssh_host(
        &mut self,
        host: &crate::ssh_hosts::SshHost,
        cx: &mut Context<Self>,
    ) {
        let host_id = host.id.clone();
        self.ssh_host_statuses
            .insert(host_id.clone(), SshHostCardStatus::Disconnected);
        self.ssh_status = Some(
            text::SSH_DISCONNECTED_MSG
                .format(&[("label", &host.label)])
                .into(),
        );
        cx.defer(move |cx| {
            if cx.has_global::<Config>()
                && cx
                    .global::<Config>()
                    .pending_ssh_connect
                    .as_ref()
                    .is_some_and(|h| h.id == host_id)
            {
                cx.global_mut::<Config>().pending_ssh_connect = None;
            }
        });
        cx.notify();
    }

    /// 연결 테스트 결과를 상태 배너와 첫 연결 도우미 상태(`ssh_auth_failed_host`)에 반영한다.
    pub fn apply_ssh_test_result(
        &mut self,
        host: crate::ssh_hosts::SshHost,
        res: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        self.ssh_test_pending.remove(&host.id);
        match res {
            Ok(ok) => {
                self.ssh_host_statuses
                    .insert(host.id.clone(), SshHostCardStatus::Connected);
                self.ssh_auth_failed_host = None;
                self.ssh_keygen_confirm_host = None;
                self.ssh_status = Some(format!("{}: {ok}", host.label).into());
            }
            Err(err) => {
                self.ssh_host_statuses
                    .insert(host.id.clone(), SshHostCardStatus::Error);
                match crate::ssh_hosts::classify_ssh_error(&err) {
                    crate::ssh_hosts::SshTestFailureKind::AuthFailed => {
                        let mut msg = text::SSH_AUTH_FAILED_GUIDE
                            .format(&[("label", &host.label), ("error", &err)]);
                        if !host.destination.contains('@')
                            && let Ok(user) = std::env::var("USER")
                        {
                            msg.push_str(
                                &text::SSH_AUTH_USER_HINT
                                    .format(&[("user", &user), ("address", &host.destination)]),
                            );
                        }
                        self.ssh_auth_failed_host = Some(host);
                        self.ssh_keygen_confirm_host = None;
                        self.ssh_status = Some(msg.into());
                    }
                    crate::ssh_hosts::SshTestFailureKind::ConnectionRefused => {
                        let msg = text::SSH_CONN_REFUSED_GUIDE.format(&[("label", &host.label)]);
                        self.ssh_auth_failed_host = None;
                        self.ssh_keygen_confirm_host = None;
                        self.ssh_status = Some(msg.into());
                    }
                    crate::ssh_hosts::SshTestFailureKind::Other => {
                        self.ssh_auth_failed_host = None;
                        self.ssh_keygen_confirm_host = None;
                        self.ssh_status = Some(format!("{}: {err}", host.label).into());
                    }
                }
            }
        }
        cx.notify();
    }

    pub fn test_ssh_host(&mut self, host: crate::ssh_hosts::SshHost, cx: &mut Context<Self>) {
        // 같은 호스트의 중복 실행은 막는다. 완료·실패는 `apply_ssh_test_result`가 푼다.
        if !self.ssh_test_pending.insert(host.id.clone()) {
            return;
        }
        self.ssh_status = Some(format!("{}: {}", host.label, text::SSH_TESTING.get()).into());
        cx.notify();
        let revision = self.file_revision;
        cx.spawn(async move |this, cx| {
            let host_for_bg = host.clone();
            let res = cx
                .background_executor()
                .spawn(async move { crate::ssh_hosts::test_connection(&host_for_bg) })
                .await;
            this.update(cx, |this, cx| {
                if this.file_revision != revision {
                    this.ssh_test_pending.remove(&host.id);
                    this.ssh_remote_infos.remove(&host.id);
                    return;
                }
                this.apply_ssh_test_result(host, res, cx);
            })
            .ok();
        })
        .detach();
    }

    /// 호스트의 연결 테스트가 실행 중인지(진행 표시·버튼 비활성화용, OWE-884).
    pub fn is_ssh_test_pending(&self, id: &str) -> bool {
        self.ssh_test_pending.contains(id)
    }

    fn ssh_home_dir(cx: &App) -> PathBuf {
        if cfg!(test) {
            state_dir(cx).unwrap_or_else(crate::accounts::default_home)
        } else {
            crate::accounts::default_home()
        }
    }

    /// "이 기기 등록하기" 클릭 시: `~/.ssh/id_ed25519`(또는 호스트 지정 키)가 있으면 바로
    /// 새 터미널 탭에서 `ssh-copy-id`를 실행하고, 없으면 키 생성 확인 프롬프트를 띄운다.
    pub fn start_register_ssh_host(
        &mut self,
        host: crate::ssh_hosts::SshHost,
        cx: &mut Context<Self>,
    ) {
        let has_custom_key = host
            .identity_file
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
        let home = Self::ssh_home_dir(cx);
        let revision = self.file_revision.wrapping_add(1);
        self.file_job(
            "ssh-key-check",
            move || has_custom_key || crate::ssh_hosts::has_default_ssh_key(Some(&home)),
            move |this, has_key, cx| {
                if this.file_revision != revision {
                    return;
                }
                if has_key {
                    this.launch_ssh_copy_id(host, cx);
                } else {
                    this.ssh_keygen_confirm_host = Some(host);
                }
            },
            cx,
        );
    }

    /// 키 생성 확인 후 `~/.ssh/id_ed25519`를 생성하고 새 터미널 탭에서 `ssh-copy-id`를 실행한다.
    pub fn confirm_keygen_and_register(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.ssh_keygen_confirm_host.take() else {
            return;
        };
        let home = Self::ssh_home_dir(cx);
        let revision = self.file_revision;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { crate::ssh_hosts::generate_default_ssh_key(Some(&home)) })
                .await;
            this.update(cx, |this, cx| {
                if this.file_revision != revision {
                    return;
                }
                match res {
                    Ok(_) => this.launch_ssh_copy_id(host, cx),
                    Err(e) => {
                        this.ssh_status = Some(format!("{}: {e}", host.label).into());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// 새 터미널 탭에서 `ssh-copy-id`(비밀번호 1회 입력)를 실행한다.
    /// 설정 창은 셸이 닫으므로(`on_config_changed`), 안내 문구는 메인 창 배너로도
    /// 전달해 터미널 탭에서 비밀번호 입력을 볼 수 있게 한다(OWE-884).
    pub fn launch_ssh_copy_id(&mut self, host: crate::ssh_hosts::SshHost, cx: &mut Context<Self>) {
        let label = host.label.clone();
        self.ssh_auth_failed_host = None;
        self.ssh_keygen_confirm_host = None;
        let started: String = text::SSH_REGISTER_STARTED.format(&[("label", &label)]);
        self.ssh_status = Some(started.clone().into());
        queue_notice(started, cx);
        let copy_host = host.with_copy_id();
        cx.defer(move |cx| {
            if cx.has_global::<Config>() {
                cx.global_mut::<Config>().pending_ssh_connect = Some(copy_host);
            }
        });
        cx.notify();
    }

    /// OWE-897: 원격 OS·아키·버전·소켓을 백그라운드에서 조회한다.
    /// 네트워크 IO는 UI 스레드에서 절대 하지 않는다(CONVENTIONS T7).
    pub fn refresh_remote_info(&mut self, host: crate::ssh_hosts::SshHost, cx: &mut Context<Self>) {
        if self
            .ssh_remote_infos
            .get(&host.id)
            .is_some_and(|state| matches!(state.probe, RemoteProbeState::Loading))
        {
            return;
        }
        self.ssh_remote_infos
            .entry(host.id.clone())
            .or_default()
            .probe = RemoteProbeState::Loading;
        cx.notify();
        let revision = self.file_revision;
        cx.spawn(async move |this, cx| {
            let opts = host.to_ssh_options();
            let res = cx
                .background_executor()
                .spawn(async move { krill_hosts::managed::query_remote_info(&opts) })
                .await;
            this.update(cx, |this, cx| {
                if this.file_revision != revision {
                    this.ssh_test_pending.remove(&host.id);
                    this.ssh_remote_infos.remove(&host.id);
                    return;
                }
                this.apply_remote_probe(&host, res, cx);
            })
            .ok();
        })
        .detach();
    }

    fn apply_remote_probe(
        &mut self,
        host: &crate::ssh_hosts::SshHost,
        res: Result<krill_hosts::managed::RemoteInfo, std::io::Error>,
        cx: &mut Context<Self>,
    ) {
        let state = self.ssh_remote_infos.entry(host.id.clone()).or_default();
        match res {
            Ok(info) => state.probe = RemoteProbeState::Ready(info),
            Err(e) => {
                let mut msg = e.to_string();
                if msg.chars().count() > 200 {
                    msg = format!("{}…", msg.chars().take(200).collect::<String>());
                }
                state.probe = RemoteProbeState::Failed(msg);
            }
        }
        cx.notify();
    }

    /// OWE-897: 원격에 로컬 런타임을 설치·기동한다(전 과정 백그라운드).
    pub fn install_remote(&mut self, host: crate::ssh_hosts::SshHost, cx: &mut Context<Self>) {
        if self
            .ssh_remote_infos
            .get(&host.id)
            .is_some_and(|state| matches!(state.install, RemoteInstallState::Running))
        {
            return;
        }
        self.ssh_remote_infos
            .entry(host.id.clone())
            .or_default()
            .install = RemoteInstallState::Running;
        self.ssh_status = Some(text::SSH_REMOTE_INSTALLING.get().into());
        cx.notify();
        let bg_host = host.clone();
        let revision = self.file_revision;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { run_managed_install(&bg_host) })
                .await;
            this.update(cx, |this, cx| {
                if this.file_revision != revision {
                    this.ssh_test_pending.remove(&host.id);
                    this.ssh_remote_infos.remove(&host.id);
                    return;
                }
                this.apply_install_result(&host, res, cx);
            })
            .ok();
        })
        .detach();
    }

    fn apply_install_result(
        &mut self,
        host: &crate::ssh_hosts::SshHost,
        res: Result<krill_hosts::managed::InstallReport, krill_hosts::managed::InstallError>,
        cx: &mut Context<Self>,
    ) {
        match res {
            Ok(report) => {
                self.ssh_remote_infos
                    .entry(host.id.clone())
                    .or_default()
                    .install = RemoteInstallState::Done(report.version.clone());
                self.ssh_status = Some(
                    text::SSH_REMOTE_INSTALL_OK
                        .format(&[("label", &host.label), ("version", &report.version)])
                        .into(),
                );
                // 설치 직후 원격 버전을 다시 읽어 카드에 반영한다.
                self.refresh_remote_info(host.clone(), cx);
            }
            Err(e) => {
                let guide = install_failure_guide(&e);
                self.ssh_remote_infos
                    .entry(host.id.clone())
                    .or_default()
                    .install = RemoteInstallState::Failed(short_error(&e.to_string()));
                self.ssh_status = Some(
                    text::SSH_REMOTE_INSTALL_FAILED
                        .format(&[("label", &host.label), ("guide", &guide)])
                        .into(),
                );
                cx.notify();
            }
        }
    }

    pub fn run_doctor(&mut self, cx: &mut Context<Self>) {
        let dir = state_dir(cx);
        self.file_job(
            "doctor",
            move || run_doctor_check(dir.as_deref()),
            |this, report, _| {
                this.doctor_status = Some(report.into());
            },
            cx,
        );
    }

    fn refresh_error_reporting(&mut self, cx: &mut Context<Self>) {
        if self.error_reporting.pending || self.file_jobs.contains_key("error-reporting-read") {
            return;
        }
        let dir = state_dir(cx);
        self.error_reporting.revision = self.error_reporting.revision.wrapping_add(1);
        let revision = self.error_reporting.revision;
        self.file_job(
            "error-reporting-read",
            move || dir.as_deref().map(krill_proto::error_reporting::status),
            move |this, result, _| {
                if this.error_reporting.revision != revision {
                    return;
                }
                match result {
                    Some(Ok(status)) => {
                        this.error_reporting.status = Some(status);
                        this.error_reporting.message = None;
                    }
                    _ => {
                        this.error_reporting.message =
                            Some(text::ERROR_REPORTS_FAILED.get().into());
                    }
                }
            },
            cx,
        );
    }

    fn set_error_reporting(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.error_reporting.pending {
            return;
        }
        let Some(dir) = state_dir(cx) else {
            self.error_reporting.message = Some(text::ERROR_REPORTS_FAILED.get().into());
            cx.notify();
            return;
        };
        self.error_reporting.pending = true;
        self.error_reporting.started = Some(std::time::Instant::now());
        self.error_reporting.revision = self.error_reporting.revision.wrapping_add(1);
        self.error_reporting.message = None;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            this.update(cx, |this, cx| {
                if this.error_reporting.pending {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        self.file_job(
            "error-reporting-toggle",
            move || {
                if enabled {
                    krill_proto::error_reporting::enable(&dir)?;
                } else {
                    krill_proto::error_reporting::disable(&dir)?;
                }
                krill_proto::error_reporting::status(&dir)
            },
            |this, result, cx| {
                this.error_reporting.pending = false;
                this.error_reporting.started = None;
                match result {
                    Ok(status) => this.error_reporting.status = Some(status),
                    Err(_) => {
                        this.error_reporting.message =
                            Some(text::ERROR_REPORTS_FAILED.get().into());
                    }
                }
                crate::errors::refresh_run(cx);
            },
            cx,
        );
        cx.notify();
    }

    /// 7탭 기록 일시중지 토글. `Paused` ACK를 받은 뒤에만 `settings.toml`에 저장하고,
    /// 실패·구 데몬이면 저장하지 않아 표시가 자동으로 원복된다.
    pub fn set_recording_paused(&mut self, paused: bool, cx: &mut Context<Self>) {
        if self.history_pause_supported == Some(false) {
            self.recording_status = Some(text::RECORDING_UPDATE_NEEDED.get().into());
            cx.notify();
            return;
        }
        if !is_daemon_state_dir(state_dir(cx).as_deref()) {
            // 테스트용 임시 상태 디렉터리에서는 실제 데몬을 건드리지 않는다.
            self.recording_status = Some(text::RECORDING_DELETE_UNAVAILABLE.get().into());
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    request_history_pause(krill_proto::daemon::Paths::dev().ok().as_ref(), paused)
                })
                .await;
            this.update(cx, |this, cx| match outcome {
                Ok(echoed) if echoed == paused => {
                    this.history_pause_supported = Some(true);
                    this.recording_status = None;
                    edit(cx, move |s| s.recording_paused = paused);
                }
                Ok(_) | Err(PauseError::Unsupported) => {
                    this.history_pause_supported = Some(false);
                    this.recording_status = Some(text::RECORDING_UPDATE_NEEDED.get().into());
                    cx.notify();
                }
                Err(PauseError::Failed(message)) => {
                    this.recording_status = Some(message.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 앱 시작 시 저장된 일시중지 값을 데몬에 전달한다. 실제 상태 디렉터리에서만 보내고,
    /// 실패는 조용히 둔다(토글을 누르면 다시 확인한다).
    pub fn sync_recording_paused(cx: &mut Context<Self>) {
        if !is_daemon_state_dir(state_dir(cx).as_deref()) {
            return;
        }
        let paused = current(cx).recording_paused;
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    request_history_pause(krill_proto::daemon::Paths::dev().ok().as_ref(), paused)
                })
                .await;
            this.update(cx, |this, cx| {
                this.history_pause_supported = Some(outcome.is_ok());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 기록 즉시 삭제. 데몬 IPC(연결·읽기 타임아웃 포함)는 UI 스레드를 막지 않게
    /// 백그라운드에서 돌리고, 그 사이 in-flight 표시를 보인다(AS-01).
    pub fn clear_history(&mut self, cx: &mut Context<Self>) {
        if self.history_clear_pending {
            return;
        }
        self.history_clear_pending = true;
        self.history_status = Some(text::RECORDING_DELETING.get().into());
        cx.notify();
        let dir = state_dir(cx);
        let daemon = is_daemon_state_dir(dir.as_deref());
        cx.spawn(async move |this, cx| {
            let msg = cx
                .background_executor()
                .spawn(async move {
                    // 테스트용 임시 상태 디렉터리에서는 실제 데몬 기록을 건드리지 않는다.
                    if daemon {
                        match request_history_clear(krill_proto::daemon::Paths::dev().ok().as_ref())
                        {
                            Ok(()) => text::RECORDING_DELETED.get().to_owned(),
                            Err(e) => e,
                        }
                    } else {
                        text::RECORDING_DELETE_UNAVAILABLE.get().to_owned()
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.history_clear_pending = false;
                this.history_status = Some(msg.into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_general_tab(&self, s: &Settings, cx: &mut Context<Self>) -> gpui::Div {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let search_focus = self.search_focus.clone();
        let p = crate::ui::palette(cx);
        let theme_button = |id: &'static str, label: &'static str, choice: ThemeChoice| {
            layout::segment(p, id, label, s.theme == choice)
                .debug_selector(move || id.into())
                .on_click(move |_, _, cx| {
                    edit(cx, move |s| s.theme = choice);
                })
        };
        let selected_dark = s.ui_theme_dark;
        let dark_themes = layout::secondary("ui-theme-dark", selected_dark.label())
            .dropdown_caret(true)
            .debug_selector(|| "ui-theme-dark".into())
            .dropdown_menu(move |mut menu, _, _| {
                for theme in DarkUiTheme::ALL {
                    menu = menu.item(
                        PopupMenuItem::new(theme.label())
                            .checked(selected_dark == theme)
                            .on_click(move |_, _, cx| {
                                edit(cx, move |s| s.set_ui_theme_dark(theme));
                            }),
                    );
                }
                menu
            });
        let selected_light = s.ui_theme_light;
        let light_themes = layout::secondary("ui-theme-light", selected_light.label())
            .dropdown_caret(true)
            .debug_selector(|| "ui-theme-light".into())
            .dropdown_menu(move |mut menu, _, _| {
                for theme in LightUiTheme::ALL {
                    menu = menu.item(
                        PopupMenuItem::new(theme.label())
                            .checked(selected_light == theme)
                            .on_click(move |_, _, cx| {
                                edit(cx, move |s| s.set_ui_theme_light(theme));
                            }),
                    );
                }
                menu
            });
        let awake_button = |id: &'static str, label: &'static str, mode: KeepAwake| {
            layout::segment(p, id, label, s.keep_awake == mode)
                .debug_selector(move || id.into())
                .on_click(move |_, _, cx| {
                    edit(cx, move |s| s.keep_awake = mode);
                })
        };
        let language_button = |id: &'static str, label: &'static str, language: Language| {
            layout::segment(p, id, label, s.language == language)
                .debug_selector(move || id.into())
                .on_click(move |_, _, cx| {
                    set_language(language, cx);
                })
        };
        let zoom_val = s.zoom();
        let zoom_pct = format!("{:.0}%", zoom_val * 100.);
        let zoom_at_min = zoom_val <= ZOOM_MIN + f32::EPSILON;
        let zoom_at_max = zoom_val >= ZOOM_MAX - f32::EPSILON;
        let zoom_at_default = (zoom_val - 1.0).abs() < f32::EPSILON;
        let notif_disabled = !s.notifications;
        let row = || layout::row(p);
        let sub_notif_row = |id: &'static str| {
            layout::row(p)
                .id(id)
                .debug_selector(move || id.into())
                .when(notif_disabled, |d| d.opacity(0.5))
        };
        let label =
            |title: &'static str, desc: &'static str| search_focus.label(p, title, Some(desc));
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(crate::orca_import::render_banner("settings-general", cx))
            .child(layout::section_title(
                p,
                "general-section-appearance",
                text::SECTION_APPEARANCE.get(),
            ))
            .child(
                row()
                    .child(label(text::LANGUAGE.get(), text::LANGUAGE_DESC.get()))
                    .child(layout::controls().child(layout::segmented_control(
                        p,
                        [
                            language_button("language-ko", text::KO.get(), Language::Ko),
                            language_button("language-en", text::EN.get(), Language::En),
                        ],
                    ))),
            )
            .child(
                row()
                    .child(label(
                        text::THEME.get(),
                        text::THEME_DESC
                            .mac_or(crate::i18n::catalog::other_os::THEME_DESC)
                            .get(),
                    ))
                    .child(
                        layout::controls().child(layout::segmented_control(
                            p,
                            [
                                ("theme-dark", text::DARK.get(), ThemeChoice::Dark),
                                ("theme-light", text::LIGHT.get(), ThemeChoice::Light),
                                ("theme-system", text::SYSTEM.get(), ThemeChoice::System),
                            ]
                            .into_iter()
                            .map(|(id, l, choice)| theme_button(id, l, choice)),
                        )),
                    ),
            )
            .child(
                row()
                    .child(label(text::DARK_THEME.get(), text::DARK_THEME_DESC.get()))
                    .child(layout::controls().child(dark_themes)),
            )
            .child(
                row()
                    .child(label(text::LIGHT_THEME.get(), text::LIGHT_THEME_DESC.get()))
                    .child(layout::controls().child(light_themes)),
            )
            .child(
                row()
                    .child(label(text::ZOOM.get(), text::ZOOM_DESC.get()))
                    .child(
                        layout::controls()
                            .child(
                                layout::secondary("zoom-out", "−")
                                    .accessibility_label(
                                        text::STEP_DEC.format(&[("name", text::ZOOM.get())]),
                                    )
                                    .disabled(zoom_at_min)
                                    .debug_selector(|| "zoom-out".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| s.zoom = s.zoom_step(-1));
                                    }),
                            )
                            .child(
                                div()
                                    .id("zoom-value")
                                    .debug_selector(|| "zoom-value".into())
                                    .w(px(48.))
                                    .text_center()
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_SM)
                                    .child(zoom_pct),
                            )
                            .child(
                                layout::secondary("zoom-in", "+")
                                    .accessibility_label(
                                        text::STEP_INC.format(&[("name", text::ZOOM.get())]),
                                    )
                                    .disabled(zoom_at_max)
                                    .debug_selector(|| "zoom-in".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| s.zoom = s.zoom_step(1));
                                    }),
                            )
                            .child(
                                layout::ghost("zoom-reset", text::RESET.get())
                                    .disabled(zoom_at_default)
                                    .debug_selector(|| "zoom-reset".into())
                                    .on_click(|_, _, cx| edit(cx, move |s| s.zoom = 1.0)),
                            ),
                    ),
            )
            .child(
                row()
                    .child(label(
                        text::DEFAULT_VIEW_MODE_TITLE.get(),
                        text::DEFAULT_VIEW_MODE_DESC.get(),
                    ))
                    .child(
                        layout::controls().child(
                            layout::segmented_control(
                                p,
                                [
                                    layout::segment(
                                        p,
                                        "general-view-mode-terminal",
                                        text::DEFAULT_VIEW_MODE_TERMINAL.get(),
                                        s.default_view_mode == DefaultViewMode::Terminal,
                                    )
                                    .debug_selector(|| "general-view-mode-terminal".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| {
                                            s.default_view_mode = DefaultViewMode::Terminal;
                                        });
                                    }),
                                    layout::segment(
                                        p,
                                        "general-view-mode-chat",
                                        text::DEFAULT_VIEW_MODE_CHAT.get(),
                                        s.default_view_mode == DefaultViewMode::Chat,
                                    )
                                    .debug_selector(|| "general-view-mode-chat".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| {
                                            s.default_view_mode = DefaultViewMode::Chat;
                                        });
                                    }),
                                ],
                            )
                            .id("general-default-view-mode")
                            .debug_selector(|| "general-default-view-mode".into()),
                        ),
                    ),
            )
            .child(layout::section_title(
                p,
                "general-section-notifications",
                text::SECTION_NOTIFICATIONS.get(),
            ))
            .child(
                row()
                    .child(label(
                        text::NOTIFICATIONS.get(),
                        text::NOTIFICATIONS_DESC
                            .mac_or(crate::i18n::catalog::other_os::NOTIFICATIONS_DESC)
                            .get(),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "notif-toggle",
                                s.notifications,
                                false,
                                |next, _, cx| edit(cx, move |s| s.notifications = next),
                            )
                            .aria_label(text::NOTIFICATIONS.get()),
                        ),
                    ),
            )
            .child(
                sub_notif_row("notify-input-row")
                    .child(label(
                        text::NOTIFY_INPUT.get(),
                        text::NOTIFY_INPUT_DESC.get(),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "notify-input-toggle",
                                s.notify_on_input,
                                notif_disabled,
                                |next, _, cx| edit(cx, move |s| s.notify_on_input = next),
                            )
                            .aria_label(text::NOTIFY_INPUT.get()),
                        ),
                    ),
            )
            .child(
                sub_notif_row("notify-done-row")
                    .child(label(text::NOTIFY_DONE.get(), text::NOTIFY_DONE_DESC.get()))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "notify-done-toggle",
                                s.notify_on_done,
                                notif_disabled,
                                |next, _, cx| edit(cx, move |s| s.notify_on_done = next),
                            )
                            .aria_label(text::NOTIFY_DONE.get()),
                        ),
                    ),
            )
            .child(
                sub_notif_row("notify-bell-row")
                    .child(label(text::NOTIFY_BELL.get(), text::NOTIFY_BELL_DESC.get()))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "notify-bell-toggle",
                                s.notify_on_bell,
                                notif_disabled,
                                |next, _, cx| edit(cx, move |s| s.notify_on_bell = next),
                            )
                            .aria_label(text::NOTIFY_BELL.get()),
                        ),
                    ),
            )
            .child(
                sub_notif_row("notify-sound-row")
                    .child(label(text::ALERT_SOUND.get(), text::ALERT_SOUND_DESC.get()))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "sound-toggle",
                                s.notification_sound,
                                notif_disabled,
                                |next, _, cx| edit(cx, move |s| s.notification_sound = next),
                            )
                            .aria_label(text::ALERT_SOUND.get()),
                        ),
                    ),
            )
            .child(
                sub_notif_row("notify-inactive-row")
                    .child(label(
                        text::INACTIVE_ONLY.get(),
                        text::INACTIVE_ONLY_DESC.get(),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "notify-inactive-toggle",
                                s.notify_only_inactive,
                                notif_disabled,
                                |next, _, cx| edit(cx, move |s| s.notify_only_inactive = next),
                            )
                            .aria_label(text::INACTIVE_ONLY.get()),
                        ),
                    ),
            )
            .child(
                sub_notif_row("notify-test-row")
                    .child(label(text::TEST_NOTIFY.get(), text::TEST_NOTIFY_DESC.get()))
                    .child(
                        layout::controls().child(
                            layout::secondary("send-test-notification", text::TEST_NOTIFY.get())
                                .disabled(notif_disabled)
                                .debug_selector(|| "send-test-notification".into())
                                .on_click(|_, _, cx| crate::alert::send_test_notification(cx)),
                        ),
                    ),
            )
            .child(layout::section_title(
                p,
                "general-section-power",
                text::SECTION_POWER.get(),
            ))
            .child(
                row()
                    .child(label(
                        text::KEEP_AWAKE.get(),
                        text::KEEP_AWAKE_DESC
                            .mac_or(crate::i18n::catalog::other_os::KEEP_AWAKE_DESC)
                            .get(),
                    ))
                    .child(
                        layout::controls().child(layout::segmented_control(
                            p,
                            [
                                ("awake-off", text::AWAKE_OFF.get(), KeepAwake::Off),
                                ("awake-agent", text::AWAKE_AGENT.get(), KeepAwake::Agent),
                                ("awake-on", text::AWAKE_ON.get(), KeepAwake::On),
                            ]
                            .into_iter()
                            .map(|(id, l, mode)| awake_button(id, l, mode)),
                        )),
                    ),
            )
            .child(layout::section_title(
                p,
                "general-section-files",
                text::SECTION_FILES_UPDATES.get(),
            ))
            .child(
                row()
                    .child(label(text::FILES_TITLE.get(), text::FILES_DESC.get()))
                    .child(
                        layout::controls()
                            .child(
                                layout::secondary("open-settings", text::OPEN_SETTINGS.get())
                                    .debug_selector(|| "open-settings".into())
                                    .on_click(|_, _, cx| {
                                        let body = current(cx).to_toml();
                                        open_file(SETTINGS_FILE, &body, cx);
                                    }),
                            )
                            .child(
                                layout::secondary("open-keys", text::OPEN_KEYS.get())
                                    .debug_selector(|| "open-keys".into())
                                    .on_click(|_, _, cx| {
                                        open_file(KEYS_FILE, text::KEYS_TEMPLATE.get(), cx);
                                    }),
                            ),
                    ),
            )
            .child(
                row()
                    .child(label(text::CHECKS_TITLE.get(), text::CHECKS_DESC.get()))
                    .child(
                        layout::controls()
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_SM)
                                    .text_color(crate::ui::palette(cx).muted_foreground)
                                    .debug_selector(|| "checks-summary".into())
                                    .child(crate::onboarding::checks_summary_text(
                                        &crate::onboarding::snapshot(cx),
                                    )),
                            )
                            .child(
                                layout::secondary("open-checks", text::CHECKS_OPEN.get())
                                    .debug_selector(|| "open-checks".into())
                                    .on_click(|_, _, cx| {
                                        crate::onboarding::open_checks_current(cx);
                                    }),
                            ),
                    ),
            )
            // 자체 업데이터는 macOS 번들 전용이라 다른 OS에서는 숨긴다(규칙 10).
            .when(cfg!(target_os = "macos"), |el| {
                el.children(Self::render_update_rows(s, cx))
            })
            .child(layout::section_title(
                p,
                "general-section-support",
                text::SECTION_SUPPORT.get(),
            ))
            .child(
                row()
                    .id("settings-support-star-row")
                    .debug_selector(|| "settings-support-star-row".into())
                    .child(label(
                        text::SUPPORT_STAR_TITLE.get(),
                        text::SUPPORT_STAR_DESC.get(),
                    ))
                    .child(layout::controls().child(crate::star_nag::settings_support_control(cx))),
            )
    }

    fn render_update_rows(s: &Settings, cx: &mut Context<Self>) -> [gpui::Div; 2] {
        use gpui_component::Sizable as _;

        use crate::updater::{CheckStatus, UpdateState};
        let p = crate::ui::palette(cx);
        let row = || layout::row(p);
        let label = |title: &'static str, desc: &'static str| layout::label(p, title, Some(desc));
        let update_state = cx.try_global::<UpdateState>().cloned().unwrap_or_default();
        let is_checking = matches!(update_state.status, CheckStatus::Checking);
        let status_label = if update_state.restart.is_starting() {
            crate::i18n::catalog::updater_relaunch::CARD_RESTARTING_BTN
                .get()
                .to_owned()
        } else if let Some(error) = update_state.restart.error() {
            error.to_owned()
        } else {
            match &update_state.status {
                CheckStatus::Idle => match &update_state.staged {
                    Some(staged) => text::UPDATES_READY.format(&[("version", &staged.to_version)]),
                    None => format!("v{}", env!("CARGO_PKG_VERSION")),
                },
                CheckStatus::Checking => text::UPDATES_CHECKING.get().to_owned(),
                CheckStatus::UpToDate { version } => {
                    text::UPDATES_UP_TO_DATE.format(&[("version", version)])
                }
                CheckStatus::ChannelMismatch {
                    configured,
                    offered,
                } => text::UPDATES_CHANNEL_MISMATCH.format(&[
                    ("configured", configured.as_str()),
                    ("offered", offered.as_str()),
                ]),
                CheckStatus::Ready(staged) => {
                    text::UPDATES_READY.format(&[("version", &staged.to_version)])
                }
                CheckStatus::Error(err) => err.clone(),
            }
        };
        let staged_for_btn = update_state.staged.clone().or_else(|| {
            if let CheckStatus::Ready(staged) = &update_state.status {
                Some(staged.clone())
            } else {
                None
            }
        });
        let check_btn = crate::ui::button_with_reserved_label(
            "settings-update-check",
            text::UPDATES_CHECK.get(),
            text::UPDATES_CHECKING.get(),
            is_checking,
        )
        .small()
        .outline()
        .disabled(update_state.restart.is_starting() || is_checking)
        .debug_selector(|| "settings-update-check".into())
        .on_click(|_, _, cx| {
            trigger_manual_update_check(cx);
        });
        let status = div()
            .min_w_0()
            .text_size(crate::ui::TEXT_SM)
            .text_color(p.muted_foreground)
            .debug_selector(|| "settings-update-status".into())
            .child(status_label);
        let mut controls = layout::controls().flex_wrap().flex_shrink_1();
        if let Some(staged) = staged_for_btn {
            let restart_label = if update_state.restart.error().is_some() {
                crate::i18n::catalog::updater_relaunch::CARD_RETRY_BTN
                    .get()
                    .to_owned()
            } else {
                text::UPDATES_RESTART.format(&[("version", &staged.to_version)])
            };
            controls = controls.child(
                crate::ui::button_with_reserved_label(
                    "settings-update-restart",
                    restart_label,
                    crate::i18n::catalog::updater_relaunch::CARD_RESTARTING_BTN.get(),
                    update_state.restart.is_starting(),
                )
                .small()
                .primary()
                .disabled(update_state.restart.is_starting() || is_checking)
                .debug_selector(|| "settings-update-restart".into())
                .on_click(move |_, _, cx| {
                    restart_staged_update(staged.clone(), cx);
                }),
            );
        }
        controls = controls.child(check_btn);

        [
            row()
                .flex_col()
                .items_start()
                .child(label(text::UPDATES_TITLE.get(), text::UPDATES_DESC.get()))
                .child(status)
                .child(controls),
            row()
                .child(label(
                    text::UPDATES_AUTO_RESTART.get(),
                    text::UPDATES_AUTO_RESTART_DESC.get(),
                ))
                .child(
                    layout::controls().child(
                        layout::switch(
                            p,
                            "update-auto-restart-toggle",
                            s.auto_restart_update,
                            false,
                            |next, _, cx| edit(cx, move |s| s.auto_restart_update = next),
                        )
                        .aria_label(text::UPDATES_AUTO_RESTART.get()),
                    ),
                ),
        ]
    }
    fn terminal_theme_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<gpui_component::input::InputState> {
        if let Some((input, _)) = &self.terminal_theme_search {
            return input.clone();
        }
        let input = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::TERMINAL_THEME_SEARCH.get())
        });
        let sub = cx.subscribe(&input, |_, _, _: &gpui_component::input::InputEvent, cx| {
            cx.notify();
        });
        self.terminal_theme_search = Some((input.clone(), sub));
        input
    }

    fn import_ghostty_themes(&mut self, cx: &mut Context<Self>) {
        if !self.theme_imports_pending.insert("ghostty") {
            return;
        }
        let revision = cx.try_global::<Config>().map(|config| config.revision);
        cx.spawn(async move |this, cx| {
            let imported = cx
                .background_executor()
                .spawn(async move { crate::ui::discover_and_import_ghostty() })
                .await;
            this.update(cx, |this, cx| {
                this.theme_imports_pending.remove("ghostty");
                let select = cx.try_global::<Config>().map(|config| config.revision) == revision;
                let has_any = imported.found
                    || !imported.custom_themes.is_empty()
                    || imported.dark_theme.is_some()
                    || imported.light_theme.is_some();
                if has_any {
                    let count = imported.custom_themes.len().max(1);
                    edit(cx, move |s| {
                        crate::ui::merge_custom_themes(
                            &mut s.terminal_custom_themes,
                            imported.custom_themes.clone(),
                        );
                        if select && let Some(dark) = imported.dark_theme.clone() {
                            s.terminal_theme_dark = dark;
                        }
                        if select && let Some(light) = imported.light_theme.clone() {
                            s.terminal_theme_light = light;
                        }
                        if select && let Some(use_sep) = imported.use_separate_light_theme {
                            s.terminal_use_separate_light_theme = use_sep;
                        }
                    });
                    this.terminal_theme_status = Some((
                        text::TERMINAL_THEME_IMPORTED_OK
                            .format(&[("count", &count.to_string())])
                            .into(),
                        false,
                    ));
                } else {
                    this.terminal_theme_status =
                        Some((text::TERMINAL_THEME_IMPORT_NONE.get().into(), true));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn import_warp_themes(&mut self, cx: &mut Context<Self>) {
        if !self.theme_imports_pending.insert("warp") {
            return;
        }
        cx.spawn(async move |this, cx| {
            let report = cx
                .background_executor()
                .spawn(async move { crate::ui::discover_and_import_warp_themes() })
                .await;
            this.update(cx, |this, cx| {
                this.theme_imports_pending.remove("warp");
                if report.themes.is_empty() {
                    this.terminal_theme_status =
                        Some((text::TERMINAL_THEME_IMPORT_NONE.get().into(), true));
                } else {
                    let count = report.themes.len();
                    let themes = report.themes;
                    edit(cx, move |s| {
                        crate::ui::merge_custom_themes(
                            &mut s.terminal_custom_themes,
                            themes.clone(),
                        );
                    });
                    this.terminal_theme_status = Some((
                        text::TERMINAL_THEME_IMPORTED_OK
                            .format(&[("count", &count.to_string())])
                            .into(),
                        false,
                    ));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_terminal_theme_swatch(
        p: &crate::ui::Palette,
        theme: &krill_proto::theme::Theme,
    ) -> gpui::Div {
        let mut pill = div()
            .flex_none()
            .h(px(18.))
            .px_1p5()
            .rounded(crate::ui::RADIUS_XS)
            .border_1()
            .border_color(p.border)
            .bg(crate::ui::terminal_rgb_hsla(theme.background))
            .flex()
            .items_center()
            .gap_1();
        for idx in 1..=6 {
            pill = pill.child(
                div()
                    .w(px(6.))
                    .h(px(6.))
                    .rounded(crate::ui::RADIUS_XS)
                    .bg(crate::ui::terminal_rgb_hsla(theme.ansi[idx])),
            );
        }
        pill
    }

    fn render_terminal_theme_preview(
        p: &crate::ui::Palette,
        s: &Settings,
        target_mode: ThemeMode,
        active_label: &str,
    ) -> gpui::Stateful<gpui::Div> {
        const PREVIEW_PROMPT: &str = "krill@mac ~/krill ";
        const PREVIEW_BRANCH: &str = "(main)";
        const PREVIEW_CMD: &str = " $ cargo nextest run";
        const PREVIEW_PASS: &str = "     PASS ";
        const PREVIEW_SUITE: &str = "[ 0.042s] ";
        const PREVIEW_TEST: &str = "krill-app::ui::terminal_themes";
        const PREVIEW_KW: &str = "fn ";
        const PREVIEW_FN: &str = "apply_theme";
        const PREVIEW_ARGS: &str = "(mode: ThemeMode) -> ";
        const PREVIEW_RET: &str = "TerminalTheme ";
        const PREVIEW_COMMENT: &str = "// 30 built-in + custom themes";
        const PREVIEW_CURSOR_CHAR: &str = " ";

        let preview = crate::ui::terminal_theme_for_mode(target_mode, s);
        let bg = crate::ui::terminal_rgb_hsla(preview.background);
        let fg = crate::ui::terminal_rgb_hsla(preview.foreground);
        let cursor_col = crate::ui::terminal_rgb_hsla(preview.cursor);
        let c_red = crate::ui::terminal_rgb_hsla(preview.ansi[1]);
        let c_green = crate::ui::terminal_rgb_hsla(preview.ansi[2]);
        let c_yellow = crate::ui::terminal_rgb_hsla(preview.ansi[3]);
        let c_blue = crate::ui::terminal_rgb_hsla(preview.ansi[4]);
        let c_magenta = crate::ui::terminal_rgb_hsla(preview.ansi[5]);
        let c_cyan = crate::ui::terminal_rgb_hsla(preview.ansi[6]);
        let c_muted = crate::ui::terminal_rgb_hsla(preview.ansi[8]);

        let cursor_el = match s.terminal_cursor_shape {
            TerminalCursorShape::Block => div()
                .w(px(7.))
                .h(px(14.))
                .bg(cursor_col)
                .child(PREVIEW_CURSOR_CHAR),
            TerminalCursorShape::Bar => div()
                .w(px(2.))
                .h(px(14.))
                .bg(cursor_col)
                .child(PREVIEW_CURSOR_CHAR),
            TerminalCursorShape::Underline => div()
                .w(px(7.))
                .h(px(2.))
                .mt(px(12.))
                .bg(cursor_col)
                .child(PREVIEW_CURSOR_CHAR),
        };

        let mut normal_strip = div().flex().items_center().gap_1();
        let mut bright_strip = div().flex().items_center().gap_1();
        for idx in 0..8 {
            normal_strip = normal_strip.child(
                div()
                    .flex_1()
                    .h(px(10.))
                    .rounded(crate::ui::RADIUS_XS)
                    .border_1()
                    .border_color(p.border)
                    .bg(crate::ui::terminal_rgb_hsla(preview.ansi[idx])),
            );
            bright_strip = bright_strip.child(
                div()
                    .flex_1()
                    .h(px(10.))
                    .rounded(crate::ui::RADIUS_XS)
                    .border_1()
                    .border_color(p.border)
                    .bg(crate::ui::terminal_rgb_hsla(preview.ansi[idx + 8])),
            );
        }

        div()
            .id("terminal-theme-preview-card")
            .debug_selector(|| "terminal-theme-preview-card".into())
            .flex_1()
            .min_w_0()
            .w_full()
            .rounded(crate::ui::RADIUS_MD)
            .border_1()
            .border_color(p.border)
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .bg(p.muted)
                    .border_b_1()
                    .border_color(p.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(c_red))
                            .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(c_yellow))
                            .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(c_green))
                            .child(
                                div()
                                    .ml_1()
                                    .text_size(crate::ui::TEXT_2XS)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(p.muted_foreground)
                                    .child(text::TERMINAL_THEME_PREVIEW.get()),
                            ),
                    )
                    .child(
                        div()
                            .id("terminal-theme-active-label")
                            .debug_selector(|| "terminal-theme-active-label".into())
                            .truncate()
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(p.foreground)
                            .child(active_label.to_owned()),
                    ),
            )
            .child(
                div()
                    .p_3()
                    .bg(bg)
                    .text_color(fg)
                    .font_family(s.terminal_font_family.clone())
                    .text_size(crate::ui::TEXT_SM)
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(div().text_color(c_green).child(PREVIEW_PROMPT))
                            .child(div().text_color(c_magenta).child(PREVIEW_BRANCH))
                            .child(div().text_color(fg).child(PREVIEW_CMD)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(div().text_color(c_green).child(PREVIEW_PASS))
                            .child(div().text_color(c_cyan).child(PREVIEW_SUITE))
                            .child(div().text_color(fg).child(PREVIEW_TEST)),
                    )
                    .child(div().text_color(c_muted).child(PREVIEW_COMMENT))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(div().text_color(c_magenta).child(PREVIEW_KW))
                            .child(div().text_color(c_blue).child(PREVIEW_FN))
                            .child(div().text_color(fg).child(PREVIEW_ARGS))
                            .child(div().text_color(c_yellow).child(PREVIEW_RET))
                            .child(cursor_el),
                    )
                    .child(
                        div()
                            .mt_2()
                            .pt_2()
                            .border_t_1()
                            .border_color(p.border)
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(normal_strip)
                            .child(bright_strip),
                    ),
            )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "터미널 테마 섹션은 모드 토글·가져오기·검색·커스텀/내장 목록·실시간 미리보기를 한 카드에 그린다"
    )]
    fn render_terminal_themes_section(
        &mut self,
        s: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _};
        let p = crate::ui::palette(cx);
        let target_mode = self
            .terminal_theme_target
            .unwrap_or_else(|| cx.theme().mode);
        let want_dark = target_mode == ThemeMode::Dark;
        let match_dark = target_mode == ThemeMode::Light && !s.terminal_use_separate_light_theme;

        let active_selection = if target_mode == ThemeMode::Dark || match_dark {
            crate::ui::canonical_terminal_theme_name(&s.terminal_theme_dark)
        } else {
            crate::ui::canonical_terminal_theme_name(&s.terminal_theme_light)
        };
        let active_label =
            crate::ui::terminal_theme_display_label(active_selection, &s.terminal_custom_themes);

        let search_input = self.terminal_theme_search_input(window, cx);
        let query = search_input.read(cx).value().trim().to_ascii_lowercase();

        let mode_segment = layout::segmented_control(
            p,
            [
                (
                    "terminal-theme-mode-dark",
                    text::DARK.get(),
                    ThemeMode::Dark,
                ),
                (
                    "terminal-theme-mode-light",
                    text::LIGHT.get(),
                    ThemeMode::Light,
                ),
            ]
            .into_iter()
            .map(|(id, title, m)| {
                layout::segment(p, id, title, target_mode == m)
                    .debug_selector(move || id.into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.terminal_theme_target = Some(m);
                        cx.notify();
                    }))
            }),
        );

        let import_ghostty_btn = layout::secondary(
            "terminal-theme-import-ghostty",
            text::TERMINAL_THEME_IMPORT_GHOSTTY.get(),
        )
        .debug_selector(|| "terminal-theme-import-ghostty".into())
        .on_click(cx.listener(|this, _, _, cx| {
            this.import_ghostty_themes(cx);
        }));

        let import_warp_btn = layout::secondary(
            "terminal-theme-import-warp",
            text::TERMINAL_THEME_IMPORT_WARP.get(),
        )
        .debug_selector(|| "terminal-theme-import-warp".into())
        .on_click(cx.listener(|this, _, _, cx| {
            this.import_warp_themes(cx);
        }));

        let mut list = div()
            .id("terminal-theme-list")
            .debug_selector(|| "terminal-theme-list".into())
            .max_h(px(260.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();

        self.terminal_theme_swatches
            .retain(|index, _| *index < s.terminal_custom_themes.len());
        for (index, theme) in s.terminal_custom_themes.iter().enumerate() {
            let unchanged = self
                .terminal_theme_swatches
                .get(&index)
                .is_some_and(|(previous, _)| previous == theme);
            if !unchanged {
                self.terminal_theme_swatches
                    .insert(index, (theme.clone(), theme.to_terminal_theme()));
            }
        }
        let custom_matches: Vec<(usize, &crate::ui::CustomTerminalTheme)> = s
            .terminal_custom_themes
            .iter()
            .enumerate()
            .filter(|(_, ct)| ct.is_dark() == want_dark)
            .filter(|(_, ct)| {
                query.is_empty()
                    || ct.name.to_ascii_lowercase().contains(&query)
                    || ct.source.to_ascii_lowercase().contains(&query)
            })
            .collect();

        let builtin_matches: Vec<(usize, &crate::ui::BuiltinTerminalTheme)> =
            crate::ui::BUILTIN_TERMINAL_THEMES
                .iter()
                .enumerate()
                .filter(|(_, b)| b.is_dark == want_dark)
                .filter(|(_, b)| query.is_empty() || b.name.to_ascii_lowercase().contains(&query))
                .collect();

        if custom_matches.is_empty() && builtin_matches.is_empty() {
            list = list.child(
                div()
                    .py_4()
                    .text_center()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::TERMINAL_THEME_EMPTY.get()),
            );
        }

        if !custom_matches.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .pt_1()
                    .text_size(crate::ui::TEXT_XS)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(p.muted_foreground)
                    .child(text::TERMINAL_THEME_IMPORTED.get()),
            );
            for (idx, ct) in custom_matches {
                let sel_val = ct.selection_value();
                let selected = active_selection == sel_val;
                let row_id = SharedString::from(format!("terminal-theme-custom-{idx}"));
                let del_id = SharedString::from(format!("terminal-theme-delete-{idx}"));
                let custom_id = ct.id.clone();
                let source_badge = match ct.source.as_str() {
                    "ghostty" => text::TERMINAL_THEME_SOURCE_GHOSTTY.get(),
                    "warp" => text::TERMINAL_THEME_SOURCE_WARP.get(),
                    _ => text::TERMINAL_THEME_SOURCE_CUSTOM.get(),
                };
                let parsed_theme = self
                    .terminal_theme_swatches
                    .get(&idx)
                    .and_then(|(_, theme)| *theme)
                    .unwrap_or(if want_dark {
                        crate::ui::TERMINAL_DARK
                    } else {
                        crate::ui::TERMINAL_LIGHT
                    });
                let swatch = Self::render_terminal_theme_swatch(p, &parsed_theme);
                let row_sel = row_id.clone();
                let del_sel = del_id.clone();
                list = list.child(
                    div()
                        .id(row_id)
                        .debug_selector(move || row_sel.to_string())
                        .role(gpui::Role::RadioButton)
                        .aria_label(ct.name.clone())
                        .aria_toggled(if selected {
                            gpui::Toggled::True
                        } else {
                            gpui::Toggled::False
                        })
                        .tab_index(0)
                        .focus_visible(|style| style.border_color(p.ring))
                        .px_2()
                        .py_1p5()
                        .rounded(crate::ui::RADIUS_SM)
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .when(selected, |d| {
                            d.bg(p.sidebar_accent).font_weight(gpui::FontWeight::MEDIUM)
                        })
                        .hover(|d| d.bg(p.sidebar_accent))
                        .active(|d| d.bg(p.accent))
                        .on_click(move |_, _, cx| {
                            let val = sel_val.clone();
                            edit(cx, move |s| {
                                if want_dark {
                                    s.terminal_theme_dark.clone_from(&val);
                                } else {
                                    s.terminal_use_separate_light_theme = true;
                                    s.terminal_theme_light.clone_from(&val);
                                }
                            });
                        })
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(swatch)
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .text_size(crate::ui::TEXT_SM)
                                        .text_color(p.foreground)
                                        .child(ct.name.clone()),
                                )
                                .child(crate::ui::badge(
                                    p,
                                    crate::ui::BadgeTone::Muted,
                                    source_badge,
                                )),
                        )
                        .child(crate::ui::button_with_tooltip(
                            Button::new(del_id.clone())
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .debug_selector(move || del_sel.to_string())
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    let cid = custom_id.clone();
                                    edit(cx, move |s| {
                                        s.terminal_custom_themes.retain(|t| t.id != cid);
                                        let target_sel = format!("custom:{cid}");
                                        if s.terminal_theme_dark == target_sel {
                                            s.terminal_theme_dark = default_terminal_theme_dark();
                                        }
                                        if s.terminal_theme_light == target_sel {
                                            s.terminal_theme_light = default_terminal_theme_light();
                                        }
                                    });
                                }),
                            del_id,
                            text::TERMINAL_THEME_REMOVE.get(),
                        )),
                );
            }
        }

        if !builtin_matches.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .pt_1()
                    .text_size(crate::ui::TEXT_XS)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(p.muted_foreground)
                    .child(text::TERMINAL_THEME_BUILTIN.get()),
            );
            for (idx, entry) in builtin_matches {
                let selected = active_selection.eq_ignore_ascii_case(entry.name);
                let row_id = SharedString::from(format!("terminal-theme-builtin-{idx}"));
                let row_sel = row_id.clone();
                let theme_name = entry.name;
                let swatch = Self::render_terminal_theme_swatch(p, &entry.theme);
                list = list.child(
                    div()
                        .id(row_id)
                        .debug_selector(move || row_sel.to_string())
                        .role(gpui::Role::RadioButton)
                        .aria_label(theme_name)
                        .aria_toggled(if selected {
                            gpui::Toggled::True
                        } else {
                            gpui::Toggled::False
                        })
                        .tab_index(0)
                        .focus_visible(|style| style.border_color(p.ring))
                        .px_2()
                        .py_1p5()
                        .rounded(crate::ui::RADIUS_SM)
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(selected, |d| {
                            d.bg(p.sidebar_accent).font_weight(gpui::FontWeight::MEDIUM)
                        })
                        .hover(|d| d.bg(p.sidebar_accent))
                        .active(|d| d.bg(p.accent))
                        .on_click(move |_, _, cx| {
                            edit(cx, move |s| {
                                if want_dark {
                                    theme_name.clone_into(&mut s.terminal_theme_dark);
                                } else {
                                    s.terminal_use_separate_light_theme = true;
                                    theme_name.clone_into(&mut s.terminal_theme_light);
                                }
                            });
                        })
                        .child(swatch)
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_size(crate::ui::TEXT_SM)
                                .text_color(p.foreground)
                                .child(theme_name),
                        ),
                );
            }
        }

        let picker_col = div()
            .w_full()
            .min_w_0()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .id("terminal-theme-search")
                    .debug_selector(|| "terminal-theme-search".into())
                    .child(
                        gpui_component::input::Input::new(&search_input)
                            .small()
                            .prefix(Icon::new(IconName::Search).xsmall()),
                    ),
            )
            .child(list);

        let preview_col = Self::render_terminal_theme_preview(p, s, target_mode, &active_label);

        let mut section = div()
            .id("terminal-section-themes-wrap")
            .debug_selector(|| "terminal-section-themes-wrap".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(layout::section_title(
                p,
                "terminal-section-themes",
                text::SECTION_TERMINAL_THEMES.get(),
            ))
            .child(
                layout::row(p)
                    .child(layout::label(
                        p,
                        text::SECTION_TERMINAL_THEMES.get(),
                        Some(text::SECTION_TERMINAL_THEMES_DESC.get()),
                    ))
                    .child(
                        layout::controls()
                            .child(mode_segment)
                            .child(import_ghostty_btn)
                            .child(import_warp_btn),
                    ),
            );

        if let Some((status_msg, is_warn)) = &self.terminal_theme_status {
            section = section.child(
                div()
                    .id("terminal-theme-status")
                    .debug_selector(|| "terminal-theme-status".into())
                    .px_3()
                    .py_1p5()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.muted)
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(if *is_warn {
                        p.warning
                    } else {
                        p.muted_foreground
                    })
                    .child(status_msg.clone()),
            );
        }

        if target_mode == ThemeMode::Light {
            section = section.child(
                layout::row(p)
                    .id("terminal-theme-match-dark-row")
                    .debug_selector(|| "terminal-theme-match-dark-row".into())
                    .child(layout::label(
                        p,
                        text::TERMINAL_THEME_MATCH_DARK.get(),
                        Some(text::TERMINAL_THEME_MATCH_DARK_DESC.get()),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "terminal-theme-match-dark-toggle",
                                !s.terminal_use_separate_light_theme,
                                false,
                                |match_dark_next, _, cx| {
                                    edit(cx, move |s| {
                                        s.terminal_use_separate_light_theme = !match_dark_next;
                                    });
                                },
                            )
                            .aria_label(text::TERMINAL_THEME_MATCH_DARK.get()),
                        ),
                    ),
            );
        }

        section.child(
            layout::card(p)
                .id("terminal-theme-picker-card")
                .debug_selector(|| "terminal-theme-picker-card".into())
                .min_w_0()
                .flex()
                .flex_col()
                .items_start()
                .gap_3()
                .child(picker_col)
                .child(preview_col),
        )
    }

    /// 터미널 탭(OWE-658, OWE-798, OWE-956): 터미널 테마·미리보기·글꼴·크기·투명도·기본 커서·Option 키·선택 복사.
    fn render_terminal_tab(
        &mut self,
        s: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let themes_section = self.render_terminal_themes_section(s, window, cx);
        let search_focus = self.search_focus.clone();
        let p = crate::ui::palette(cx);
        let row = || layout::row(p);
        let label =
            |title: &'static str, desc: &'static str| search_focus.label(p, title, Some(desc));
        let font_size = s.terminal_font_size();
        let font_pt = format!("{font_size}pt");
        let editor_font_pt = format!("{}pt", s.editor_font_size());
        let opacity_val = s.terminal_opacity_pct();
        let opacity_pct = format!("{opacity_val}%");
        self.refresh_font_names(cx);
        let font_names = self.font_names.clone();
        let family = s.terminal_font_family.clone();
        let font_family = layout::secondary("terminal-font-family", family.clone())
            .debug_selector(|| "terminal-font-family".into())
            .disabled(self.fonts_pending && self.font_names.is_empty())
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.scrollable(true).max_h(px(360.));
                for name in font_names.iter().cloned() {
                    let checked = name == family;
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(checked).on_click(
                        move |_, _, cx| {
                            let name = name.clone();
                            edit(cx, move |s| s.terminal_font_family.clone_from(&name));
                        },
                    ));
                }
                menu
            });
        let cursor_button = |id: &'static str, title: &'static str, shape: TerminalCursorShape| {
            layout::segment(p, id, title, s.terminal_cursor_shape == shape)
                .debug_selector(move || id.into())
                .on_click(move |_, _, cx| edit(cx, move |s| s.terminal_cursor_shape = shape))
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(themes_section)
            .child(layout::section_title(
                p,
                "terminal-section-appearance",
                text::SECTION_TERMINAL_APPEARANCE.get(),
            ))
            .child(
                row()
                    .child(label(
                        text::TERMINAL_FONT.get(),
                        text::TERMINAL_FONT_DESC.get(),
                    ))
                    .child(layout::controls().child(font_family)),
            )
            // 지정 글꼴이 고정폭으로 해석되지 않으면 Menlo로 대신 그리고 사유를 밝힌다(OWE-899).
            .children(terminal_font_fallback(cx).map(|(requested, effective)| {
                div()
                    .id("terminal-font-fallback")
                    .debug_selector(|| "terminal-font-fallback".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.warning)
                    .child(text::TERMINAL_FONT_FALLBACK.format(&[
                        ("requested", requested.as_str()),
                        ("effective", effective.as_ref()),
                    ]))
            }))
            .child(
                row()
                    .child(label(text::FONT_SIZE.get(), text::FONT_SIZE_DESC.get()))
                    .child(
                        layout::controls()
                            .child(
                                layout::secondary("font-dec", "−")
                                    .accessibility_label(
                                        text::STEP_DEC.format(&[("name", text::FONT_SIZE.get())]),
                                    )
                                    .disabled(font_size <= 10)
                                    .debug_selector(|| "font-dec".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| {
                                            s.terminal_font_size =
                                                s.terminal_font_size().saturating_sub(1).max(10);
                                        });
                                    }),
                            )
                            .child(
                                div()
                                    .id("font-size-value")
                                    .debug_selector(|| "font-size-value".into())
                                    .w(px(48.))
                                    .text_center()
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_SM)
                                    .child(font_pt),
                            )
                            .child(
                                layout::secondary("font-inc", "+")
                                    .accessibility_label(
                                        text::STEP_INC.format(&[("name", text::FONT_SIZE.get())]),
                                    )
                                    .disabled(font_size >= 24)
                                    .debug_selector(|| "font-inc".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| {
                                            s.terminal_font_size =
                                                s.terminal_font_size().saturating_add(1).min(24);
                                        });
                                    }),
                            )
                            .child(
                                layout::ghost("font-reset", text::RESET.get())
                                    .disabled(font_size == default_terminal_font_size())
                                    .tooltip(text::FONT_RESET.get())
                                    .debug_selector(|| "font-reset".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| {
                                            s.terminal_font_size = default_terminal_font_size();
                                        });
                                    }),
                            ),
                    ),
            )
            .child(
                row()
                    .child(label(
                        text::EDITOR_FONT_SIZE.get(),
                        text::EDITOR_FONT_SIZE_DESC.get(),
                    ))
                    .child(
                        layout::controls()
                            .child(
                                Button::new("editor-font-dec")
                                    .accessibility_label(
                                        text::STEP_DEC
                                            .format(&[("name", text::EDITOR_FONT_SIZE.get())]),
                                    )
                                    .label("−")
                                    .disabled(s.editor_font_size_step(-1) == s.editor_font_size)
                                    .debug_selector(|| "editor-font-dec".into())
                                    .ghost()
                                    .small()
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| {
                                            s.editor_font_size = s.editor_font_size_step(-1);
                                        });
                                    }),
                            )
                            .child(
                                div()
                                    .w_12()
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_SM)
                                    .child(editor_font_pt),
                            )
                            .child(
                                Button::new("editor-font-inc")
                                    .accessibility_label(
                                        text::STEP_INC
                                            .format(&[("name", text::EDITOR_FONT_SIZE.get())]),
                                    )
                                    .label("+")
                                    .disabled(s.editor_font_size_step(1) == s.editor_font_size)
                                    .debug_selector(|| "editor-font-inc".into())
                                    .ghost()
                                    .small()
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| {
                                            s.editor_font_size = s.editor_font_size_step(1);
                                        });
                                    }),
                            )
                            .child(
                                Button::new("editor-font-reset")
                                    .label(text::RESET.get())
                                    .tooltip(text::EDITOR_FONT_RESET.get())
                                    .disabled(s.editor_font_size == default_editor_font_size())
                                    .debug_selector(|| "editor-font-reset".into())
                                    .ghost()
                                    .small()
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| {
                                            s.editor_font_size = default_editor_font_size();
                                        });
                                    }),
                            ),
                    ),
            )
            .child(
                row()
                    .child(label(text::OPACITY.get(), text::OPACITY_DESC.get()))
                    .child(
                        layout::controls()
                            .child(
                                layout::secondary("opacity-dec", "−")
                                    .accessibility_label(
                                        text::STEP_DEC.format(&[("name", text::OPACITY.get())]),
                                    )
                                    .disabled(opacity_val <= 50)
                                    .debug_selector(|| "opacity-dec".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| {
                                            s.terminal_opacity_pct =
                                                s.terminal_opacity_pct().saturating_sub(5).max(50);
                                        });
                                    }),
                            )
                            .child(
                                div()
                                    .id("opacity-value")
                                    .debug_selector(|| "opacity-value".into())
                                    .w(px(48.))
                                    .text_center()
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_SM)
                                    .child(opacity_pct),
                            )
                            .child(
                                layout::secondary("opacity-inc", "+")
                                    .accessibility_label(
                                        text::STEP_INC.format(&[("name", text::OPACITY.get())]),
                                    )
                                    .disabled(opacity_val >= 100)
                                    .debug_selector(|| "opacity-inc".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| {
                                            s.terminal_opacity_pct =
                                                s.terminal_opacity_pct().saturating_add(5).min(100);
                                        });
                                    }),
                            )
                            .child(
                                layout::ghost("opacity-reset", text::RESET.get())
                                    .disabled(opacity_val == default_terminal_opacity_pct())
                                    .debug_selector(|| "opacity-reset".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, move |s| s.terminal_opacity_pct = 100);
                                    }),
                            ),
                    ),
            )
            .child(layout::section_title(
                p,
                "terminal-section-cursor",
                text::SECTION_TERMINAL_CURSOR.get(),
            ))
            .child(
                row()
                    .child(label(
                        text::CURSOR_SHAPE.get(),
                        text::CURSOR_SHAPE_DESC.get(),
                    ))
                    .child(
                        layout::controls().child(layout::segmented_control(
                            p,
                            [
                                (
                                    "cursor-block",
                                    text::CURSOR_BLOCK.get(),
                                    TerminalCursorShape::Block,
                                ),
                                (
                                    "cursor-bar",
                                    text::CURSOR_BAR.get(),
                                    TerminalCursorShape::Bar,
                                ),
                                (
                                    "cursor-underline",
                                    text::CURSOR_UNDERLINE.get(),
                                    TerminalCursorShape::Underline,
                                ),
                            ]
                            .into_iter()
                            .map(|(id, l, shape)| cursor_button(id, l, shape)),
                        )),
                    ),
            )
            .child(
                row()
                    .child(label(
                        text::CURSOR_BLINK.get(),
                        text::CURSOR_BLINK_DESC.get(),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "cursor-blink-toggle",
                                s.terminal_cursor_blink,
                                false,
                                |next, _, cx| edit(cx, move |s| s.terminal_cursor_blink = next),
                            )
                            .aria_label(text::CURSOR_BLINK.get()),
                        ),
                    ),
            )
            .child(layout::section_title(
                p,
                "terminal-section-keyboard",
                text::SECTION_TERMINAL_KEYBOARD.get(),
            ))
            .child(
                row()
                    .child(label(
                        text::OPTION_AS_META.get(),
                        text::OPTION_AS_META_DESC.get(),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "option-meta-toggle",
                                s.option_as_meta,
                                false,
                                |next, _, cx| {
                                    edit(cx, move |s| {
                                        s.option_as_meta = next;
                                        s.option_as_meta_auto = false;
                                    });
                                },
                            )
                            .aria_label(text::OPTION_AS_META.get()),
                        ),
                    ),
            )
            .child(
                row()
                    .child(label(
                        text::OPTION_META_AUTO.get(),
                        text::OPTION_META_AUTO_DESC.get(),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "option-meta-auto",
                                s.option_as_meta_auto,
                                false,
                                |next, _, cx| {
                                    edit(cx, move |s| {
                                        s.option_as_meta_auto = next;
                                        if next {
                                            s.option_as_meta = false;
                                        }
                                    });
                                },
                            )
                            .aria_label(text::OPTION_META_AUTO.get()),
                        ),
                    ),
            )
            .child(
                row()
                    .child(label(
                        text::COPY_ON_SELECT.get(),
                        text::COPY_ON_SELECT_DESC.get(),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "copy-select-toggle",
                                s.copy_on_select,
                                false,
                                |next, _, cx| edit(cx, move |s| s.copy_on_select = next),
                            )
                            .aria_label(text::COPY_ON_SELECT.get()),
                        ),
                    ),
            )
            .child(
                row()
                    .child(label(
                        text::CONFIRM_RUNNING_CLOSE.get(),
                        text::CONFIRM_RUNNING_CLOSE_DESC.get(),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "confirm-running-close-toggle",
                                s.confirm_running_close,
                                false,
                                |next, _, cx| edit(cx, move |s| s.confirm_running_close = next),
                            )
                            .aria_label(text::CONFIRM_RUNNING_CLOSE.get()),
                        ),
                    ),
            )
    }

    /// 계정 행의 5h·7d 미니 사용량 바(OWE-719, OWE-797). 리셋 시각은 인라인 메타데이터와 묶음 툴팁([`format_account_usage`])에 함께 표시한다.
    fn render_usage_bar(
        key: &str,
        window_label: &str,
        win: Option<&krill_proto::usage::Window>,
        brand_color: gpui::Hsla,
        p: &crate::ui::Palette,
    ) -> gpui::Div {
        const BAR_W: f32 = 44.;
        let label_id = format!("admin-usage-label-{key}");
        let reset_id = format!("admin-usage-reset-{key}");
        let label_width = if window_label.chars().count() > 5 {
            164.
        } else {
            28.
        };
        let now_secs = crate::accounts::display::now();
        let display =
            crate::accounts::display::window(win, i64::try_from(now_secs).unwrap_or(i64::MAX));
        let reset = win
            .and_then(|w| has_valid_reset(w.resets_at.as_deref()))
            .and_then(|ts| relative_reset_suffix(Some(ts), now_secs));
        let pct = display.percent.unwrap_or(0);
        let fill_w = (f32::from(pct) / 100.0) * BAR_W;
        let mono = |d: gpui::Div| {
            d.font_family(crate::ui::FONT_MONO)
                .text_size(crate::ui::TEXT_2XS)
        };
        div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(
                // 고정폭 + 줄바꿈 금지: 20px에서는 "5시간"이 "5시/간"으로 접혔다(OWE-719 QA D3).
                mono(div().w(px(label_width)).flex_none().truncate())
                    .debug_selector(move || label_id.clone())
                    .text_color(p.muted_foreground)
                    .child(window_label.to_owned()),
            )
            .child(
                div()
                    .w(px(BAR_W))
                    .h(px(4.))
                    .rounded_full()
                    .bg(p.muted)
                    .overflow_hidden()
                    .child(div().h_full().w(px(fill_w)).rounded_full().bg(brand_color)),
            )
            .child(
                mono(div().w(px(34.)).flex().justify_end())
                    .text_color(p.foreground)
                    .child(display.text),
            )
            .children(reset.map(|reset| {
                mono(div().flex_none().whitespace_nowrap())
                    .debug_selector(move || reset_id.clone())
                    .text_color(p.muted_foreground)
                    .child(reset)
            }))
    }

    fn render_account_rule(
        &self,
        acct: &krill_proto::usage::AccountInfo,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use gpui_component::menu::DropdownMenu as _;
        use krill_proto::usage::{AutoSwitchMetric, AutoSwitchRule};

        let p = crate::ui::palette(cx);
        let snap = &self.accounts_snapshot;
        let existing_rule = snap.auto_switch.rule_for(acct.provider, &acct.id).cloned();
        let enabled = existing_rule
            .as_ref()
            .map_or(snap.auto_switch.enabled, |r| r.enabled);
        let rule = existing_rule.clone().unwrap_or_else(|| AutoSwitchRule {
            provider: acct.provider,
            from_account_id: acct.id.clone(),
            enabled,
            threshold_percent: snap.auto_switch.threshold_percent,
            metric: AutoSwitchMetric::Max,
            target_account_id: None,
        });
        let rule_button =
            |id: String, label: SharedString, selected: bool, next: AutoSwitchRule| {
                layout::segment(p, SharedString::from(id.clone()), label, selected)
                    .disabled(self.account_rule_pending)
                    .debug_selector(move || id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_account_rule(next.clone(), cx);
                    }))
            };
        // OWE-719: 규칙 편집기는 프로바이더 섹션의 "고급" 하나 아래에만 있다. 계정마다 이름과 현재 규칙 요약을 머리로 둔다.
        let summary = existing_rule.as_ref().map_or_else(
            || text::ACCOUNTS_RULE_NONE.get().to_owned(),
            |rule| rule_summary(rule, &snap.accounts),
        );
        let current_id = format!("admin-rule-current-{}", acct.id);
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(
                div()
                    .flex_none()
                    .text_size(crate::ui::TEXT_SM)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(account_heading(acct).0),
            )
            .child(
                div()
                    .id(SharedString::from(current_id.clone()))
                    .debug_selector(move || current_id.clone())
                    .min_w_0()
                    .truncate()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(summary),
            );
        let mut controls = div()
            .flex()
            .flex_col()
            .gap_2()
            .pt_2()
            .border_t_1()
            .border_color(p.border)
            .child(header);
        let mut toggled = rule.clone();
        toggled.enabled = !enabled;
        let view = cx.entity();
        controls = controls.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::ACCOUNTS_RULE_ENABLED.get()),
                )
                .child(
                    layout::switch(
                        p,
                        format!("admin-rule-toggle-{}", acct.id),
                        enabled,
                        self.account_rule_pending,
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.set_account_rule(toggled.clone(), cx);
                            });
                        },
                    )
                    .aria_label(format!(
                        "{} · {}",
                        text::ACCOUNTS_RULE_ENABLED.get(),
                        acct.label
                    )),
                ),
        );
        let provider = acct.provider;
        let metric_btns = AutoSwitchMetric::for_provider(provider)
            .iter()
            .copied()
            .map(|metric| {
                let mut next = rule.clone();
                next.metric = metric;
                rule_button(
                    format!("admin-rule-metric-{}-{}", acct.id, metric.short_code()),
                    metric.short_label_for(provider).into(),
                    rule.metric == metric,
                    next,
                )
            });
        let metrics = div()
            .flex()
            .items_center()
            .gap_2()
            .flex_wrap()
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(crate::i18n::catalog::status::PROVIDER_RULE_METRIC.get()),
            )
            .child(layout::segmented_control(p, metric_btns));
        controls = controls.child(metrics);
        let threshold_btns = krill_proto::usage::AUTO_SWITCH_PRESETS
            .into_iter()
            .map(|pct| {
                let mut next = rule.clone();
                next.threshold_percent = pct;
                rule_button(
                    format!("admin-rule-threshold-{}-{pct}", acct.id),
                    format!("{pct}%").into(),
                    rule.threshold_percent == pct,
                    next,
                )
            });
        let thresholds = div()
            .flex()
            .items_center()
            .gap_2()
            .flex_wrap()
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::AUTO_SWITCH_THRESHOLD.get()),
            )
            .child(layout::segmented_control(p, threshold_btns));
        controls = controls.child(thresholds);
        let target_accounts: Vec<_> = snap
            .accounts
            .iter()
            .filter(|other| other.provider == acct.provider && other.id != acct.id)
            .map(|other| (other.id.clone(), other.label.clone()))
            .collect();
        let target_label = target_accounts
            .iter()
            .find(|(id, _)| rule.target_account_id.as_ref() == Some(id))
            .map_or_else(
                || text::ACCOUNTS_RULE_TARGET_AUTO.get().to_owned(),
                |(_, label)| label.clone(),
            );
        let weak = cx.weak_entity();
        let current_rule = rule.clone();
        let target_selector = format!("admin-rule-target-{}", acct.id);
        controls = controls.child(
            div()
                .min_w_0()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(layout::label(
                    p,
                    crate::i18n::catalog::status::PROVIDER_RULE_TARGET.get(),
                    None,
                ))
                .child(
                    layout::secondary(
                        SharedString::from(format!("admin-rule-target-{}", acct.id)),
                        target_label,
                    )
                    .max_w(crate::ui::SETTINGS_NAV_WIDTH)
                    .disabled(self.account_rule_pending)
                    .debug_selector(move || target_selector.clone())
                    .dropdown_menu(move |mut menu, _, _| {
                        menu = menu.scrollable(true).max_h(px(360.));
                        let options = std::iter::once((
                            None,
                            text::ACCOUNTS_RULE_TARGET_AUTO.get().to_owned(),
                        ))
                        .chain(
                            target_accounts
                                .iter()
                                .map(|(id, label)| (Some(id.clone()), label.clone())),
                        );
                        for (target, label) in options {
                            let weak = weak.clone();
                            let mut next = current_rule.clone();
                            next.target_account_id.clone_from(&target);
                            menu = menu.item(
                                gpui_component::menu::PopupMenuItem::new(label)
                                    .checked(current_rule.target_account_id == target)
                                    .on_click(move |_, _, cx| {
                                        weak.update(cx, |this, cx| {
                                            this.set_account_rule(next.clone(), cx);
                                        })
                                        .ok();
                                    }),
                            );
                        }
                        menu
                    }),
                ),
        );

        if existing_rule.is_some() {
            let id = format!("admin-rule-clear-{}", acct.id);
            let provider = acct.provider;
            let from_id = acct.id.clone();
            controls = controls.child(
                div().child(
                    layout::ghost(
                        SharedString::from(id.clone()),
                        text::ACCOUNTS_RULE_CLEAR.get(),
                    )
                    .disabled(self.account_rule_pending)
                    .debug_selector(move || id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.clear_account_rule(provider, &from_id, cx);
                    })),
                ),
            );
        }
        controls
    }

    /// 이름 변경 입력과 저장·취소(OWE-705). 저장은 라벨만 바꾸고 id·프로필 폴더는 그대로다.
    fn render_rename_row(
        acc: &krill_proto::usage::AccountInfo,
        input: &gpui::Entity<gpui_component::input::InputState>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui_component::Sizable as _;
        let save_id = format!("admin-rename-save-{}", acc.id);
        let cancel_id = format!("admin-rename-cancel-{}", acc.id);
        let (provider, account_id, state) = (acc.provider, acc.id.clone(), input.clone());
        let enter_id = account_id.clone();
        let enter_state = state.clone();
        div()
            .id("admin-account-rename-row")
            .capture_action(cx.listener(
                move |this, _: &gpui_component::input::Enter, window, cx| {
                    cx.stop_propagation();
                    let label = enter_state.read(cx).value().trim().to_owned();
                    this.rename = None;
                    if let Some(focus) = this.rename_return_focus.take() {
                        window.focus(&focus, cx);
                    }
                    if !label.is_empty() {
                        this.apply_account_request(
                            krill_proto::usage::UsageRequest::RenameAccount {
                                provider,
                                account_id: enter_id.clone(),
                                label,
                                email: None,
                            },
                            cx,
                        );
                    }
                    cx.notify();
                },
            ))
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Escape, window, cx| {
                    cx.stop_propagation();
                    this.rename = None;
                    if let Some(focus) = this.rename_return_focus.take() {
                        window.focus(&focus, cx);
                    }
                    cx.notify();
                }),
            )
            .min_w_0()
            .flex_1()
            .flex_wrap()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(gpui_component::input::Input::new(input).small()),
            )
            .child(
                layout::primary(
                    SharedString::from(save_id.clone()),
                    text::ACCOUNT_RENAME_SAVE.get(),
                )
                .debug_selector(move || save_id.clone())
                .on_click(cx.listener(move |this, _, window, cx| {
                    let label = state.read(cx).value().trim().to_owned();
                    this.rename = None;
                    if let Some(focus) = this.rename_return_focus.take() {
                        window.focus(&focus, cx);
                    }
                    if !label.is_empty() {
                        this.apply_account_request(
                            krill_proto::usage::UsageRequest::RenameAccount {
                                provider,
                                account_id: account_id.clone(),
                                label,
                                email: None,
                            },
                            cx,
                        );
                    }
                    cx.notify();
                })),
            )
            .child(
                layout::ghost(
                    SharedString::from(cancel_id.clone()),
                    text::ACCOUNT_RENAME_CANCEL.get(),
                )
                .debug_selector(move || cancel_id.clone())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.rename = None;
                    if let Some(focus) = this.rename_return_focus.take() {
                        window.focus(&focus, cx);
                    }
                    cx.notify();
                })),
            )
    }

    /// 계정 행 아래 보조 줄: 시스템 기본값·Orca 설명, 로그인 필요 안내, 아니면 라벨·조직(없으면 프로필 폴더).
    fn account_detail(&self, acc: &krill_proto::usage::AccountInfo, heading: String) -> String {
        if acc.host_default || crate::accounts::orca_managed(acc) {
            return heading;
        }
        let email = acc.email.as_deref().filter(|e| !e.is_empty());
        let org = self
            .identities
            .0
            .get(&acc.id)
            .and_then(|i| i.org_name.as_deref())
            .filter(|o| !o.is_empty() && *o != acc.label);
        let parts: Vec<&str> = [
            email
                .filter(|e| *e != acc.label)
                .map(|_| acc.label.as_str()),
            org,
        ]
        .into_iter()
        .flatten()
        .collect();
        if parts.is_empty() {
            heading
        } else {
            parts.join(" · ")
        }
    }

    fn start_rename(
        &mut self,
        id: String,
        label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input =
            cx.new(|cx| gpui_component::input::InputState::new(window, cx).default_value(label));
        self.removing = None;
        self.rename_return_focus = window.focused(cx);
        input.update(cx, |input, cx| input.focus(window, cx));
        self.rename = Some((id, input));
        cx.notify();
    }

    fn start_remove(&mut self, id: String, cx: &mut Context<Self>) {
        self.rename = None;
        self.removing = Some(id);
        cx.notify();
    }

    fn render_opencode_api_key_card(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui_component::{IconName, Sizable as _};
        let p = crate::ui::palette(cx);
        let (home, dir) = Self::opencode_home_and_state(cx);
        let configured = crate::accounts::opencode_go::has_override(&home, dir.as_deref());
        self.opencode_api_key_configured = configured;
        let input = self.ensure_opencode_api_key_input(window, cx);
        let draft_empty = input.read(cx).value().trim().is_empty();
        let pending = self.opencode_api_key_pending;
        let tone = if configured {
            crate::ui::BadgeTone::Success
        } else {
            crate::ui::BadgeTone::Muted
        };
        let badge_label = if configured {
            text::OPENCODE_APIKEY_SAVED.get()
        } else {
            text::OPENCODE_APIKEY_NOT_SAVED.get()
        };
        let mut card = layout::card(p)
            .id("admin-opencode-api-key-card")
            .debug_selector(|| "admin-opencode-api-key-card".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(layout::label(
                        p,
                        text::OPENCODE_APIKEY_TITLE.get(),
                        Some(text::OPENCODE_APIKEY_DESC.get()),
                    ))
                    .child(
                        crate::ui::badge(p, tone, badge_label)
                            .id("admin-opencode-api-key-badge")
                            .debug_selector(|| "admin-opencode-api-key-badge".into()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            gpui_component::input::Input::new(&input)
                                .content_type(gpui_component::input::InputContentType::Password)
                                .small(),
                        ),
                    )
                    .child(
                        layout::controls()
                            .child(
                                layout::secondary(
                                    "admin-opencode-api-key-save",
                                    if configured {
                                        text::OPENCODE_APIKEY_REPLACE.get()
                                    } else {
                                        text::OPENCODE_APIKEY_SAVE.get()
                                    },
                                )
                                .disabled(pending || draft_empty)
                                .debug_selector(|| "admin-opencode-api-key-save".into())
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.save_opencode_api_key(cx);
                                    },
                                )),
                            )
                            .when(configured, |c| {
                                c.child(
                                    layout::ghost(
                                        "admin-opencode-api-key-clear",
                                        text::OPENCODE_APIKEY_CLEAR.get(),
                                    )
                                    .icon(IconName::Close)
                                    .disabled(pending)
                                    .debug_selector(|| "admin-opencode-api-key-clear".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.clear_opencode_api_key(cx);
                                        },
                                    )),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::OPENCODE_APIKEY_HELP.get()),
            );
        if let Some(err) = self.opencode_api_key_error.clone() {
            card = card.child(
                crate::ui::error_banner(p, err)
                    .id("admin-opencode-api-key-error")
                    .debug_selector(|| "admin-opencode-api-key-error".into()),
            );
        }
        card
    }

    /// 계정 한 행(OWE-719, Orca accounts-pane 행 밀도): 아바타+상태 점, 이름·보조 줄, 5h·7d 미니 바, 주 동작 하나
    /// (전환 또는 다시 로그인)와 … 메뉴(다시 로그인·이름 변경·제거). 사용 중 표시는 행 강조와 "사용 중" 하나뿐이다.
    fn render_account_row(
        &self,
        acc: &krill_proto::usage::AccountInfo,
        status: Option<&krill_proto::usage::AccountStatusInfo>,
        duplicate: Option<String>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        use gpui_component::{IconName, Sizable as _};
        use krill_proto::usage::{ConnectionStatus, ProviderKind, UsageRequest};

        let p = crate::ui::palette(cx);
        let provider = acc.provider;
        let orca = crate::accounts::orca_managed(acc);
        let snap = &self.accounts_snapshot;
        let usage = crate::accounts::display::account(acc, status, crate::accounts::display::now());
        let conn = crate::accounts::display::connection(snap, acc, &usage);
        let needs_login = conn == Some(ConnectionStatus::NeedsLogin);
        let can_login = !orca
            && matches!(
                provider,
                ProviderKind::Claude
                    | ProviderKind::Codex
                    | ProviderKind::Antigravity
                    | ProviderKind::OpenCode
            );
        let can_browser_login = can_login
            && !self.account_host.is_remote()
            && matches!(provider, ProviderKind::Claude | ProviderKind::Codex);
        let can_reset_usage = !orca
            && !needs_login
            && matches!(provider, ProviderKind::Claude | ProviderKind::Codex)
            && self.reset_credit_count(&acc.id) != Some(0);
        let manageable = !acc.host_default && !orca;
        // Orca 감지 계정은 실행·이름 변경은 못 해도 Krill 목록에서 지울 수는 있다.
        let removable = !acc.host_default;
        // OWE-881: 이 계정이 브라우저 로그인 대기 중이면 진행 문구와 [링크 복사]·[취소]만 보인다.
        let pending_for_row = self
            .login_pending
            .as_ref()
            .filter(|p| p.account_id == acc.id);
        let logging_in = pending_for_row.is_some();
        let cancelling = pending_for_row.is_some_and(|p| p.cancelling);
        let login_link_copied = pending_for_row.is_some_and(|p| p.copied);
        let login_link_preparing =
            pending_for_row.is_some_and(|p| p.copy_on_ready && p.url.is_none());
        let (primary, heading) = account_heading(acc);
        let switching = snap
            .details
            .handoffs
            .iter()
            .any(|h| h.account_id == acc.id && h.to.is_none() && h.error.is_none());
        let switch_failed = snap
            .details
            .handoffs
            .iter()
            .any(|h| h.account_id == acc.id && h.error.is_some());
        let detail = if switching {
            text::ACCOUNT_SESSION_SWITCHING.get().to_owned()
        } else if logging_in {
            if login_link_copied {
                text::ACCOUNT_LOGIN_LINK_COPIED_WAITING.get().to_owned()
            } else if login_link_preparing {
                text::ACCOUNT_LOGIN_LINK_PREPARING.get().to_owned()
            } else {
                text::ACCOUNT_LOGIN_WAITING.get().to_owned()
            }
        } else if needs_login && can_login {
            text::ACCOUNT_SIGN_IN_TO_USE.get().to_owned()
        } else {
            self.account_detail(acc, heading)
        };
        let brand = crate::ui::icons::provider_brand_color(provider, p.accent);

        // 아바타(이름 첫 글자) + 오른쪽 아래 상태 점. 툴팁·접근성 설명에 연결·사용량 상태와 갱신 시각.
        let tip = text::CONNECTION_TOOLTIP.format(&[
            (
                "connection",
                crate::accounts::display::connection_label(conn),
            ),
            ("usage", usage.label),
            ("updated", &usage.updated),
        ]);
        let status_id = format!("admin-account-status-{}", acc.id);
        let status_tip_id = status_id.clone();
        let dot_id = format!("admin-account-dot-{}", acc.id);
        let initial: String = primary
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_default();
        let avatar = div()
            .id(SharedString::from(status_id.clone()))
            .debug_selector(move || status_id.clone())
            .role(gpui::Role::Status)
            .aria_label(tip.clone())
            .tooltip(move |window, cx| {
                crate::ui::tooltip_view(status_tip_id.clone(), tip.clone(), window, cx)
            })
            .relative()
            .flex_none()
            .size(px(26.))
            .rounded_full()
            .bg(brand.opacity(0.16))
            .flex()
            .items_center()
            .justify_center()
            .text_size(crate::ui::TEXT_XS)
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(brand)
            .child(initial)
            .child(
                div()
                    .debug_selector(move || dot_id.clone())
                    .absolute()
                    .right(px(-1.))
                    .bottom(px(-1.))
                    .size(crate::ui::STATE_DOT)
                    .rounded_full()
                    .bg(crate::accounts::display::connection_color(conn, p)),
            );

        let renaming = self
            .rename
            .as_ref()
            .filter(|(id, _)| *id == acc.id)
            .map(|(_, input)| input.clone());
        let in_use_id = format!("admin-account-in-use-{}", acc.id);
        let needs_login_id = format!("admin-account-needs-login-{}", acc.id);
        let rate_limited_id = format!("admin-account-rate-limited-{}", acc.id);
        let is_rate_limited =
            !needs_login && usage.status == krill_proto::usage::AccountStatus::RateLimited;
        let name_line = div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(crate::ui::icons::provider_icon(
                acc.provider,
                crate::ui::ICON_XS,
                p.foreground,
            ))
            .map(|d| match renaming {
                Some(input) => d.child(Self::render_rename_row(acc, &input, cx)),
                None => d.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(crate::ui::TEXT_SM)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(primary.clone()),
                ),
            })
            .when(acc.active, |d| {
                d.child(
                    crate::ui::badge(p, crate::ui::BadgeTone::Success, text::ACCOUNT_IN_USE.get())
                        .id(SharedString::from(in_use_id.clone()))
                        .debug_selector(move || in_use_id.clone()),
                )
            })
            .when(needs_login, |d| {
                d.child(
                    crate::ui::badge(
                        p,
                        crate::ui::BadgeTone::Warning,
                        text::ACCOUNT_LOGIN_REQUIRED.get(),
                    )
                    .id(SharedString::from(needs_login_id.clone()))
                    .debug_selector(move || needs_login_id.clone()),
                )
            })
            .when(is_rate_limited, |d| {
                d.child(
                    crate::ui::badge(
                        p,
                        crate::ui::BadgeTone::Warning,
                        text::ACCOUNT_STATUS_LIMITED.get(),
                    )
                    .id(SharedString::from(rate_limited_id.clone()))
                    .debug_selector(move || rate_limited_id.clone()),
                )
            });
        let detail_line = div()
            .flex()
            .items_center()
            .gap_1p5()
            .min_w_0()
            .text_size(crate::ui::TEXT_2XS)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(if needs_login {
                        p.warning
                    } else {
                        p.muted_foreground
                    })
                    .child(detail),
            )
            .children(duplicate.map(|dup| {
                let id = format!("admin-account-duplicate-{}", acc.id);
                div()
                    .id(SharedString::from(id.clone()))
                    .debug_selector(move || id.clone())
                    .flex_none()
                    .text_color(p.warning)
                    .child(format!("· {dup}"))
            }));

        let usage_id = format!("admin-account-usage-{}", acc.id);
        let usage_tip_id = usage_id.clone();
        let meters = snap.meters_for(&acc.id);
        let usage_tip = meters.map_or_else(
            || format_account_usage(&acc.usage),
            |m| crate::accounts::display::meters_summary(m, crate::accounts::display::now()),
        );
        let mut usage_cluster = div()
            .id(SharedString::from(usage_id.clone()))
            .debug_selector(move || usage_id.clone())
            .tooltip(move |window, cx| {
                crate::ui::tooltip_view(usage_tip_id.clone(), usage_tip.clone(), window, cx)
            })
            .flex_none()
            .flex()
            .flex_col()
            .gap_0p5();
        if let Some(meters) = meters {
            for meter in &meters.meters {
                let label = crate::accounts::display::meter_label(meter);
                usage_cluster = usage_cluster.child(
                    if let Some(value) = crate::accounts::display::meter_value(
                        meter,
                        crate::accounts::display::now(),
                    ) {
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(label)
                            .child(value.clone())
                    } else {
                        Self::render_usage_bar(&meter.key, &label, meter.window.as_ref(), brand, p)
                    },
                );
            }
            if meters.meters.is_empty() {
                usage_cluster = usage_cluster.child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child("—"),
                );
            }
        } else {
            usage_cluster = usage_cluster
                .child(Self::render_usage_bar(
                    "5h",
                    text::WINDOW_5H.get(),
                    acc.usage.five_hour.as_ref(),
                    brand,
                    p,
                ))
                .child(Self::render_usage_bar(
                    "7d",
                    text::WINDOW_7D.get(),
                    acc.usage.seven_day.as_ref(),
                    brand,
                    p,
                ));
        }

        // 주 동작 하나: 로그인이 필요하면 "다시 로그인"(및 브라우저 제공자는 "링크 복사"), 아니면 비활성 계정에만 "전환"(.ghost().small()).
        // 로그인 대기 중이면 진행 표시 대신 [링크 복사]·[로그인 취소]만 둔다(OWE-881).
        let mut actions = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation());
        if logging_in {
            let copy_id = format!("admin-copy-login-link-{}", acc.id);
            let cancel_id = format!("admin-cancel-login-{}", acc.id);
            actions = actions
                .child(
                    crate::ui::button_with_reserved_label(
                        gpui::ElementId::Name(copy_id.clone().into()),
                        text::ACCOUNT_LOGIN_COPY_LINK_SHORT.get(),
                        text::ACCOUNT_LOGIN_LINK_COPIED.get(),
                        login_link_copied,
                    )
                    .small()
                    .outline()
                    .icon(if login_link_copied {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .disabled(cancelling)
                    .debug_selector(move || copy_id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy_pending_login_link(cx);
                    })),
                )
                .child(
                    crate::ui::button_with_reserved_label(
                        gpui::ElementId::Name(cancel_id.clone().into()),
                        text::ACCOUNT_LOGIN_CANCEL.get(),
                        text::ACCOUNT_LOGIN_CANCELLING.get(),
                        cancelling,
                    )
                    .small()
                    .outline()
                    .icon(IconName::Close)
                    .disabled(cancelling)
                    .debug_selector(move || cancel_id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cancel_login(cx);
                    })),
                );
        } else if can_login && needs_login {
            let login_account = acc.clone();
            let login_id = format!("admin-login-account-{}", acc.id);
            let login_disabled =
                self.account_rule_pending || self.login_pending.is_some() || self.accounts_offline;
            actions = actions.child(
                layout::ghost(
                    gpui::ElementId::Name(login_id.clone().into()),
                    text::ACCOUNT_LOGIN.get(),
                )
                .icon(IconName::RefreshCw)
                .disabled(login_disabled)
                .debug_selector(move || login_id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.login_account(provider, Some(login_account.clone()), cx);
                })),
            );
            if can_browser_login {
                let copy_login_account = acc.clone();
                let copy_login_id = format!("admin-copy-login-account-{}", acc.id);
                actions = actions.child(
                    layout::ghost(
                        gpui::ElementId::Name(copy_login_id.clone().into()),
                        text::ACCOUNT_LOGIN_COPY_LINK_SHORT.get(),
                    )
                    .icon(IconName::Copy)
                    .disabled(login_disabled)
                    .debug_selector(move || copy_login_id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.login_account_with_mode(
                            provider,
                            Some(copy_login_account.clone()),
                            false,
                            cx,
                        );
                    })),
                );
            }
        } else if !orca && (!acc.active || switch_failed) && !switching {
            let switch_id = format!("admin-switch-account-{}", acc.id);
            let acc_id = acc.id.clone();
            actions = actions.child(
                layout::ghost(
                    gpui::ElementId::Name(switch_id.clone().into()),
                    text::ACCOUNT_SWITCH.get(),
                )
                .debug_selector(move || switch_id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.apply_account_request(
                        UsageRequest::SwitchAccount {
                            provider,
                            account_id: acc_id.clone(),
                        },
                        cx,
                    );
                })),
            );
        }
        if !logging_in && can_reset_usage {
            let reset_id: SharedString = format!("admin-reset-usage-{}", acc.id).into();
            let reset_acc_id = acc.id.clone();
            let reset_disabled =
                self.account_rule_pending || self.login_pending.is_some() || self.accounts_offline;
            actions = actions.child(
                self.confirm_button(
                    reset_id,
                    crate::i18n::catalog::status::PROVIDER_RESET_USAGE.get(),
                    cx,
                    move |this, cx| {
                        this.consume_reset_credit(provider, reset_acc_id.clone(), cx);
                    },
                )
                .disabled(reset_disabled),
            );
        }
        if can_login || can_reset_usage || removable {
            let menu_id = format!("admin-account-menu-{}", acc.id);
            let view = cx.entity().downgrade();
            let menu_acc = acc.clone();
            let busy =
                self.account_rule_pending || self.login_pending.is_some() || self.accounts_offline;
            actions = actions.child(
                Button::new(gpui::ElementId::Name(menu_id.clone().into()))
                    .small()
                    .ghost()
                    .icon(IconName::Ellipsis)
                    .tooltip(text::ACCOUNT_MORE.get())
                    .accessibility_label(text::ACCOUNT_MORE.get())
                    .debug_selector(move || menu_id.clone())
                    .dropdown_menu(move |mut menu, _, _| {
                        if can_reset_usage {
                            let (view, acc) = (view.clone(), menu_acc.clone());
                            menu = menu.item(
                                PopupMenuItem::new(
                                    crate::i18n::catalog::status::PROVIDER_RESET_USAGE.get(),
                                )
                                .disabled(busy)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |this, cx| {
                                        let reset_id: SharedString =
                                            format!("admin-reset-usage-{}", acc.id).into();
                                        if this.armed.as_ref() == Some(&reset_id) {
                                            this.disarm(cx);
                                            this.consume_reset_credit(
                                                acc.provider,
                                                acc.id.clone(),
                                                cx,
                                            );
                                        } else {
                                            this.armed = Some(reset_id);
                                            cx.notify();
                                        }
                                    })
                                    .ok();
                                }),
                            );
                        }
                        if can_login {
                            let (view, acc) = (view.clone(), menu_acc.clone());
                            menu = menu.item(
                                PopupMenuItem::new(text::ACCOUNT_LOGIN.get())
                                    .disabled(busy)
                                    .on_click(move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.login_account(acc.provider, Some(acc.clone()), cx);
                                        })
                                        .ok();
                                    }),
                            );
                        }
                        if can_browser_login {
                            let (view, acc) = (view.clone(), menu_acc.clone());
                            menu = menu.item(
                                PopupMenuItem::new(text::ACCOUNT_LOGIN_COPY_LINK.get())
                                    .disabled(busy)
                                    .on_click(move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.login_account_with_mode(
                                                acc.provider,
                                                Some(acc.clone()),
                                                false,
                                                cx,
                                            );
                                        })
                                        .ok();
                                    }),
                            );
                        }
                        if manageable {
                            let (rename_view, id, label) =
                                (view.clone(), menu_acc.id.clone(), menu_acc.label.clone());
                            menu =
                                menu.item(PopupMenuItem::new(text::ACCOUNT_RENAME.get()).on_click(
                                    move |_, window, cx| {
                                        rename_view
                                            .update(cx, |this, cx| {
                                                this.start_rename(
                                                    id.clone(),
                                                    label.clone(),
                                                    window,
                                                    cx,
                                                );
                                            })
                                            .ok();
                                    },
                                ));
                        }
                        if removable {
                            let (view, id) = (view.clone(), menu_acc.id.clone());
                            if can_login || manageable {
                                menu = menu.separator();
                            }
                            menu =
                                menu.item(PopupMenuItem::new(text::ACCOUNT_REMOVE.get()).on_click(
                                    move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.start_remove(id.clone(), cx);
                                        })
                                        .ok();
                                    },
                                ));
                        }
                        menu
                    }),
            );
        }

        let can_click_switch = !(orca
            || acc.active && !switch_failed
            || switching
            || self.rename.is_some()
            || self.removing.is_some()
            || self.armed.is_some()
            || can_login && needs_login);
        let row_acc_id = acc.id.clone();
        let row_id = format!("admin-account-row-{}", acc.id);
        div()
            .id(SharedString::from(row_id.clone()))
            .debug_selector(move || row_id.clone())
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2p5()
            .rounded(crate::ui::RADIUS_MD)
            .border_1()
            .map(|d| {
                if acc.active {
                    d.bg(p.accent).border_color(p.ring)
                } else {
                    d.bg(p.card).border_color(p.border)
                }
            })
            .when(can_click_switch, |d| {
                d.cursor_pointer()
                    .hover(|s| s.bg(p.accent))
                    .active(|s| s.bg(p.sidebar_accent))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.rename.is_none() && this.removing.is_none() && this.armed.is_none()
                        {
                            this.apply_account_request(
                                UsageRequest::SwitchAccount {
                                    provider,
                                    account_id: row_acc_id.clone(),
                                },
                                cx,
                            );
                        }
                    }))
            })
            .child(avatar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(name_line)
                    .child(detail_line),
            )
            .child(usage_cluster)
            .child(actions)
    }

    /// 계정 제거 확인 대화상자(OWE-719, Orca removal dialog 대응). … 메뉴 "제거"가 열고, 창 가운데 모달에서 한 번 더
    /// 골라야 지운다. 바깥 클릭·Esc·취소는 닫기만 한다.
    fn render_remove_dialog(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        use gpui_component::Sizable as _;
        use krill_proto::usage::UsageRequest;
        let id = self.removing.as_deref()?;
        let acc = self
            .accounts_snapshot
            .accounts
            .iter()
            .find(|a| a.id == id && !a.host_default)?;
        let hint = if crate::accounts::orca_managed(acc) {
            text::ACCOUNT_REMOVE_ORCA_HINT.get()
        } else {
            text::ACCOUNT_REMOVE_HINT.get()
        };
        let p = crate::ui::palette(cx);
        let (provider, rm_acc) = (acc.provider, acc.id.clone());
        let cancel_id = format!("admin-remove-cancel-{}", acc.id);
        let rm_id = format!("admin-remove-account-{}", acc.id);
        let confirm_id = format!("admin-remove-confirm-{}", acc.id);
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.removing = None;
            cx.notify();
        };
        let card = div()
            .id(SharedString::from(confirm_id.clone()))
            .debug_selector(move || confirm_id.clone())
            .occlude()
            .role(gpui::Role::AlertDialog)
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Escape, _, cx| {
                    this.removing = None;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .capture_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.removing = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .aria_label(text::ACCOUNT_REMOVE_TITLE.format(&[("name", &account_heading(acc).0)]))
            .max_h(window.viewport_size().height - crate::ui::SPACE_4 * 2.)
            .overflow_y_scroll()
            .w(crate::ui::DIALOG_WIDTH)
            .max_w(window.viewport_size().width - crate::ui::SPACE_4 * 2.)
            .p_6()
            .rounded(crate::ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| close(this, cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(
                                text::ACCOUNT_REMOVE_TITLE
                                    .format(&[("name", &account_heading(acc).0)]),
                            ),
                    )
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(hint),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .id("admin-remove-cancel-focus")
                            .track_focus(&self.modal_dismiss_focus)
                            .tab_index(0)
                            .role(gpui::Role::Button)
                            .aria_label(text::ACCOUNT_RENAME_CANCEL.get())
                            .focus_visible(|style| style.bg(p.accent))
                            .on_click(cx.listener(move |this, _, _, cx| close(this, cx)))
                            .child(
                                layout::ghost(
                                    gpui::ElementId::Name(cancel_id.clone().into()),
                                    text::ACCOUNT_RENAME_CANCEL.get(),
                                )
                                .debug_selector(move || cancel_id.clone())
                                .tab_stop(false)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        close(this, cx);
                                    },
                                )),
                            ),
                    )
                    .child(
                        Button::new(gpui::ElementId::Name(rm_id.clone().into()))
                            .small()
                            .danger()
                            .label(text::ACCOUNT_REMOVE.get())
                            .disabled(self.accounts_offline)
                            .debug_selector(move || rm_id.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.removing = None;
                                this.apply_account_request(
                                    UsageRequest::DeleteAccount {
                                        provider,
                                        account_id: rm_acc.clone(),
                                    },
                                    cx,
                                );
                            })),
                    ),
            );
        Some(crate::ui::modal_backdrop(
            window.viewport_size(),
            crate::ui::modal_focus_scope(card, &self.modal_focus),
        ))
    }

    fn render_accounts_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        use gpui_component::IconName;
        use krill_proto::usage::{ProviderKind, UsageRequest};
        let p = crate::ui::palette(cx);
        let policy = self.accounts_snapshot.auto_switch.clone();

        let mut root = div()
            .flex()
            .flex_col()
            .gap_8()
            .children(crate::orca_import::render_banner("settings-accounts", cx))
            .when(self.accounts_offline, |d| {
                d.child(
                    div()
                        .id("admin-accounts-daemon-required")
                        .debug_selector(|| "admin-accounts-daemon-required".into())
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.warning)
                        .child(text::ACCOUNTS_DAEMON_REQUIRED.get()),
                )
            })
            .children(self.account_error.clone().map(|error| {
                crate::ui::error_banner(p, error)
                    .id("admin-account-error")
                    .debug_selector(|| "admin-account-error".into())
            }));

        if self.account_hosts.len() > 1 {
            let current_host = self.account_host.clone();
            let host_btns = self
                .account_hosts
                .iter()
                .map(|(host, label, _)| {
                    let key = match host {
                        crate::hosts::HostId::Local => "local".to_owned(),
                        crate::hosts::HostId::Remote(id) => id.clone(),
                    };
                    let id_str = format!("admin-accounts-host-{key}");
                    let selected = *host == current_host;
                    let target_host = host.clone();
                    layout::segment(
                        p,
                        gpui::ElementId::Name(id_str.clone().into()),
                        label.clone(),
                        selected,
                    )
                    .debug_selector(move || id_str.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_account_host(target_host.clone(), cx);
                    }))
                })
                .collect::<Vec<_>>();
            root = root.child(
                layout::row(p)
                    .id("admin-accounts-host-bar")
                    .debug_selector(|| "admin-accounts-host-bar".into())
                    .child(layout::label(
                        p,
                        text::ACCOUNTS_HOST_TITLE.get(),
                        Some(text::ACCOUNTS_HOST_DESC.get()),
                    ))
                    .child(layout::controls().child(layout::segmented_control(p, host_btns))),
            );
        }

        let providers = [
            (
                ProviderKind::Claude,
                "Claude",
                text::PROVIDER_CLAUDE_DESC.get(),
            ),
            (
                ProviderKind::Codex,
                "Codex",
                text::PROVIDER_CODEX_DESC.get(),
            ),
            (
                ProviderKind::Antigravity,
                "Antigravity",
                text::PROVIDER_AGY_DESC.get(),
            ),
            (
                ProviderKind::OpenCode,
                "OpenCode",
                text::PROVIDER_OPENCODE_DESC.get(),
            ),
        ];

        let duplicates =
            duplicate_account_labels(&self.accounts_snapshot.accounts, &self.identities.0);
        let mut status_index = std::collections::BTreeMap::new();
        for status in &self.accounts_snapshot.statuses {
            status_index
                .entry(status.account_id.clone())
                .or_insert_with(|| status.clone());
        }
        let mut not_installed = Vec::new();
        for (provider, title, subtitle) in providers {
            let mut accounts: Vec<_> = self
                .accounts_snapshot
                .accounts
                .iter()
                // Orca 캐시 감지 행은 실행할 프로필이 없으므로 계정 목록에 노출하지 않는다.
                .filter(|a| a.provider == provider && !crate::accounts::orca_managed(a))
                .cloned()
                .collect();
            // 시스템 기본값(호스트) 계정을 맨 위에 둔다(Orca accounts-pane 패리티).
            accounts.sort_by_key(|a| !a.host_default);
            let add_id = format!("admin-add-account-{}", provider.as_str());
            let provider_installed = if self.account_host.is_remote() {
                !accounts.is_empty()
            } else {
                crate::accounts::installed::is_installed(cx, provider)
            };
            let active_conn = accounts.iter().find(|a| a.active).and_then(|acc| {
                let usage = crate::accounts::display::account(
                    acc,
                    self.accounts_snapshot
                        .statuses
                        .iter()
                        .find(|s| s.account_id == acc.id),
                    crate::accounts::display::now(),
                );
                crate::accounts::display::connection(&self.accounts_snapshot, acc, &usage)
            });
            let (status_tone, status_label) = if provider_installed {
                provider_connection_badge(active_conn)
            } else {
                (
                    crate::ui::BadgeTone::Muted,
                    text::CONNECTION_NOT_INSTALLED.get(),
                )
            };
            let provider_status_id = format!("admin-provider-status-{}", provider.as_str());
            let show_in_bar = status_provider_visible(provider, cx);
            let status_bar_toggle_id = format!("admin-provider-status-bar-{}", provider.as_str());

            let header = div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(crate::ui::icons::provider_icon(
                                    provider,
                                    crate::ui::ICON_LG,
                                    p.foreground,
                                ))
                                .child(
                                    div()
                                        .text_size(crate::ui::TEXT_SM)
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(title),
                                )
                                .child(
                                    crate::ui::badge(p, status_tone, status_label)
                                        .id(SharedString::from(provider_status_id.clone()))
                                        .debug_selector(move || provider_status_id.clone()),
                                ),
                        )
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(subtitle),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::PROVIDER_SHOW_IN_STATUS_BAR.get()),
                        )
                        .child(
                            layout::switch(
                                p,
                                status_bar_toggle_id,
                                show_in_bar,
                                false,
                                move |on, _, cx| {
                                    set_status_provider_visible(provider, on, cx);
                                },
                            )
                            .aria_label(format!(
                                "{} · {}",
                                title,
                                text::PROVIDER_SHOW_IN_STATUS_BAR.get()
                            )),
                        ),
                );

            let add_btn = layout::secondary(
                gpui::ElementId::Name(add_id.clone().into()),
                text::ACCOUNTS_ADD.get(),
            )
            .icon(IconName::Plus)
            // OWE-881: 브라우저 로그인 진행 중에는 중복 시작을 막는다.
            .disabled(self.login_pending.is_some())
            .debug_selector(move || add_id.clone())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.login_account(provider, None, cx);
            }));
            let mut sub_controls = layout::controls();
            if !provider_installed
                && let Some(install_cmd) = agent_install_command(provider.as_str())
            {
                let install_id = format!("admin-install-provider-{}", provider.as_str());
                let install_host = self.account_host.clone();
                sub_controls = sub_controls.child(
                    layout::secondary(
                        gpui::ElementId::Name(install_id.clone().into()),
                        text::INSTALL_IN_TERMINAL.get(),
                    )
                    .icon(IconName::SquareTerminal)
                    .debug_selector(move || install_id.clone())
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(install_cmd.clone()));
                        queue_terminal_command_on(install_host.clone(), install_cmd.clone(), cx);
                    }),
                );
            }
            if !self.account_host.is_remote()
                && matches!(provider, ProviderKind::Claude | ProviderKind::Codex)
            {
                let add_copy_id = format!("admin-add-account-copy-link-{}", provider.as_str());
                sub_controls = sub_controls.child(
                    layout::secondary(
                        gpui::ElementId::Name(add_copy_id.clone().into()),
                        text::ACCOUNTS_ADD_COPY_LINK.get(),
                    )
                    .icon(IconName::Copy)
                    .disabled(self.login_pending.is_some())
                    .debug_selector(move || add_copy_id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.login_account_with_mode(provider, None, false, cx);
                    })),
                );
            }
            sub_controls = sub_controls.child(add_btn);
            let sub_id = format!("admin-accounts-subheader-{}", provider.as_str());
            let subheader = div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    layout::label(
                        p,
                        text::ACCOUNTS_SUBHEADER.get(),
                        Some(text::ACCOUNTS_SUBHEADER_DESC.get()),
                    )
                    .id(SharedString::from(sub_id.clone()))
                    .debug_selector(move || sub_id.clone()),
                )
                .child(sub_controls);

            // "활성 계정" 한 줄: 새 세션이 어느 로그인으로 뜨는지(시스템 기본값인지 추가 계정인지) 한눈에.
            let active_name = accounts.iter().find(|a| a.active).map_or_else(
                || text::SYSTEM_DEFAULT.get().to_owned(),
                |a| account_heading(a).0,
            );
            let active_id = format!("admin-accounts-active-{}", provider.as_str());
            let active_line = div()
                .id(SharedString::from(active_id.clone()))
                .debug_selector(move || active_id.clone())
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .text_size(crate::ui::TEXT_XS)
                .child(
                    div()
                        .flex_none()
                        .text_color(p.muted_foreground)
                        .child(text::ACCOUNTS_ACTIVE.get()),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(active_name),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(p.muted_foreground)
                        .child(text::ACCOUNTS_ACTIVE_HINT.get()),
                );

            let mut list = div().flex().flex_col().gap_2();
            // agy·OpenCode 로그인은 터미널에서 진행한다. [+ 계정 추가]를 눌러도
            // 명령을 바로 큐에 넣지 않고, 이 안내 카드의 [터미널에서 실행]을 눌러야 간다.
            if self.terminal_login_provider == Some(provider) {
                let run_id = format!("admin-{}-login-run", provider.as_str());
                let dismiss_id = format!("admin-{}-login-dismiss", provider.as_str());
                let guide_id = format!("admin-{}-login-guide", provider.as_str());
                list = list.child(
                    layout::card(p)
                        .id(SharedString::from(guide_id.clone()))
                        .debug_selector(move || guide_id.clone())
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(layout::label(
                            p,
                            text::ACCOUNT_LOGIN_UNSUPPORTED.get(),
                            if provider == ProviderKind::Antigravity {
                                text::AGY_LOGIN_GUIDE.get().to_owned()
                            } else if provider == ProviderKind::OpenCode {
                                text::OPENCODE_LOGIN_GUIDE.get().to_owned()
                            } else {
                                text::REMOTE_CLI_LOGIN_GUIDE.get().to_owned()
                            },
                        ))
                        .child(
                            layout::controls()
                                .child(
                                    layout::secondary(
                                        gpui::ElementId::Name(run_id.clone().into()),
                                        text::OPENCODE_LOGIN_RUN.get(),
                                    )
                                    .icon(IconName::SquareTerminal)
                                    .debug_selector(move || run_id.clone())
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.run_terminal_login(cx);
                                        },
                                    )),
                                )
                                .child(
                                    layout::ghost(
                                        gpui::ElementId::Name(dismiss_id.clone().into()),
                                        text::OPENCODE_LOGIN_DISMISS.get(),
                                    )
                                    .debug_selector(move || dismiss_id.clone())
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.terminal_login_provider = None;
                                            this.terminal_login_account = None;
                                            cx.notify();
                                        },
                                    )),
                                ),
                        ),
                );
            }
            for acc in &accounts {
                list = list.child(self.render_account_row(
                    acc,
                    status_index.get(acc.id.as_str()),
                    duplicates.get(&acc.id).cloned(),
                    cx,
                ));
            }
            // 이 기기에서 추가한 계정이 없으면 Orca처럼 점선 빈 상태 안내를 보인다.
            if !accounts
                .iter()
                .any(|a| !a.host_default && !crate::accounts::orca_managed(a))
            {
                let empty_id = format!("admin-accounts-empty-{}", provider.as_str());
                list = list.child(
                    div()
                        .id(SharedString::from(empty_id.clone()))
                        .debug_selector(move || empty_id.clone())
                        .flex()
                        .px_3()
                        .py_4()
                        .rounded(crate::ui::RADIUS_MD)
                        .border_1()
                        .border_dashed()
                        .border_color(p.border)
                        .child(layout::label(
                            p,
                            text::ACCOUNTS_EMPTY.get(),
                            Some(text::ACCOUNTS_EMPTY_DESC.get()),
                        )),
                );
            }

            let mut section = div()
                .flex()
                .flex_col()
                .gap_3()
                .child(header)
                .child(subheader)
                .child(active_line);
            if provider == ProviderKind::OpenCode && self.account_host.is_local() {
                section = section.child(self.render_opencode_api_key_card(window, cx));
            }
            section = section.child(list);

            // 계정별 자동 전환 규칙은 섹션 하단 "고급" 하나로 접는다(기본 접힘).
            let ruled: Vec<_> = accounts
                .iter()
                .filter(|a| !crate::accounts::orca_managed(a))
                .collect();
            if !ruled.is_empty() {
                let open = self.advanced_open.contains(&provider);
                let n = policy
                    .rules
                    .iter()
                    .filter(|r| r.provider == provider)
                    .count();
                let adv_id = format!("admin-accounts-advanced-{}", provider.as_str());
                section = section.child(
                    div().child(
                        layout::ghost(
                            gpui::ElementId::Name(adv_id.clone().into()),
                            text::ACCOUNTS_ADVANCED.format(&[("n", &n.to_string())]),
                        )
                        .icon(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .debug_selector(move || adv_id.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.advanced_open.remove(&provider) {
                                this.advanced_open.insert(provider);
                            }
                            cx.notify();
                        })),
                    ),
                );
                if open {
                    section = section
                        .children(ruled.into_iter().map(|a| self.render_account_rule(a, cx)));
                }
            }

            if provider_installed {
                root = root.child(section);
            } else {
                not_installed.push(section);
            }
        }
        // OWE-706: 설치되지 않은 프로바이더는 접힌 "설치 안 됨" 묶음 아래로 보낸다.
        if !not_installed.is_empty() {
            let open = self.not_installed_open;
            root = root.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_8()
                    .child(
                        div().child(
                            layout::ghost(
                                "admin-accounts-not-installed",
                                text::PROVIDERS_NOT_INSTALLED
                                    .format(&[("n", &not_installed.len().to_string())]),
                            )
                            .icon(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .debug_selector(|| "admin-accounts-not-installed".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.not_installed_open = !this.not_installed_open;
                                cx.notify();
                            })),
                        ),
                    )
                    .when(open, |d| d.children(not_installed)),
            );
        }

        // OWE-704: 이전 "제거"가 남긴 프로필 폴더 정리(휴지통 이동, 두 번 눌러 확인).
        if !self.orphan_profiles.is_empty() {
            let purge = self.confirm_button(
                "admin-purge-orphans",
                text::ORPHANS_PURGE.get(),
                cx,
                Self::purge_orphan_profiles,
            );
            root = root.child(
                layout::card(p)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .id("admin-orphan-profiles")
                    .debug_selector(|| "admin-orphan-profiles".into())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(layout::label(
                                p,
                                text::ORPHANS_TITLE
                                    .format(&[("count", &self.orphan_profiles.len().to_string())]),
                                Some(text::ORPHANS_DESC.get()),
                            ))
                            .child(layout::controls().child(purge)),
                    )
                    .children(self.orphan_profiles.iter().map(|o| {
                        div()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .truncate()
                            .child(o.dir.clone())
                    })),
            );
        }

        let view = cx.entity();
        let toggle_btn = layout::switch(
            p,
            "admin-auto-switch-toggle",
            policy.enabled,
            false,
            move |enabled, _, cx| {
                view.update(cx, |this, cx| {
                    let mut next = this.accounts_snapshot.auto_switch.clone();
                    if next.enabled != enabled {
                        next.enabled = enabled;
                        this.apply_account_request(UsageRequest::SetAutoSwitch(next), cx);
                    }
                });
            },
        )
        .aria_label(text::AUTO_SWITCH_TITLE.get());
        let view = cx.entity();
        let rl_switch = layout::switch(
            p,
            "admin-rate-limit-toggle",
            policy.switch_on_rate_limit,
            false,
            move |on, _, cx| {
                view.update(cx, |this, cx| {
                    let mut next = this.accounts_snapshot.auto_switch.clone();
                    if next.switch_on_rate_limit != on {
                        next.switch_on_rate_limit = on;
                        this.apply_account_request(UsageRequest::SetAutoSwitch(next), cx);
                    }
                });
            },
        )
        .aria_label(text::AUTO_SWITCH_RATE_LIMIT.get());
        let rate_limit_row = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(layout::label(
                p,
                text::AUTO_SWITCH_RATE_LIMIT.get(),
                Some(text::AUTO_SWITCH_RATE_LIMIT_DESC.get()),
            ))
            .child(layout::controls().child(rl_switch));
        let preset_btns = krill_proto::usage::AUTO_SWITCH_PRESETS
            .into_iter()
            .map(|pct| {
                let id_str = format!("admin-threshold-{pct}");
                let selected = policy.threshold_percent == pct;
                let policy_cl = policy.clone();
                layout::segment(
                    p,
                    gpui::ElementId::Name(id_str.clone().into()),
                    format!("{pct}%"),
                    selected,
                )
                .debug_selector(move || id_str.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let mut next = policy_cl.clone();
                    next.threshold_percent = pct;
                    this.apply_account_request(UsageRequest::SetAutoSwitch(next), cx);
                }))
            });
        let threshold_row = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(layout::label(
                p,
                text::AUTO_SWITCH_THRESHOLD.get(),
                Some(text::AUTO_SWITCH_THRESHOLD_DESC.get()),
            ))
            .child(layout::controls().child(layout::segmented_control(p, preset_btns)));

        // OWE-719 1부: 데몬이 연결 꼬리를 지원할 때만 "사용량 직접 조회" 토글을 보인다.
        let connections = &self.accounts_snapshot.connections;
        if connections.supported {
            let direct_usage = connections.direct_usage;
            let view = cx.entity();
            root = root.child(
                layout::row(p)
                    .child(layout::label(
                        p,
                        text::ACCOUNTS_DIRECT_USAGE.get(),
                        Some(text::ACCOUNTS_DIRECT_USAGE_DESC.get()),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "admin-direct-usage-toggle",
                                direct_usage,
                                false,
                                move |on, _, cx| {
                                    view.update(cx, |this, cx| {
                                        if this.accounts_snapshot.connections.direct_usage != on {
                                            this.apply_account_request(
                                                UsageRequest::SetDirectUsage(on),
                                                cx,
                                            );
                                        }
                                    });
                                },
                            )
                            .aria_label(text::ACCOUNTS_DIRECT_USAGE.get()),
                        ),
                    ),
            );
        }

        let mut auto_switch_card = layout::card(p)
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(layout::label(
                        p,
                        text::AUTO_SWITCH_TITLE.get(),
                        Some(text::AUTO_SWITCH_DESC.get()),
                    ))
                    .child(layout::controls().child(toggle_btn)),
            )
            .child(threshold_row)
            .child(rate_limit_row);

        if self.accounts_snapshot.advanced.supported {
            let adv = self.accounts_snapshot.advanced.policy.clone();
            let view_predict = cx.entity();
            let predict_switch = layout::switch(
                p,
                "admin-predict-exhaustion-toggle",
                adv.predict_exhaustion,
                false,
                move |on, _, cx| {
                    view_predict.update(cx, |this, cx| {
                        let mut next = this.accounts_snapshot.advanced.policy.clone();
                        if next.predict_exhaustion != on {
                            next.predict_exhaustion = on;
                            this.apply_account_request(UsageRequest::SetAdvancedPolicy(next), cx);
                        }
                    });
                },
            )
            .aria_label(crate::i18n::catalog::shell::ACCOUNT_PREDICT.get());
            let view_reset = cx.entity();
            let reset_switch = layout::switch(
                p,
                "admin-prefer-soonest-reset-toggle",
                adv.prefer_soonest_reset,
                false,
                move |on, _, cx| {
                    view_reset.update(cx, |this, cx| {
                        let mut next = this.accounts_snapshot.advanced.policy.clone();
                        if next.prefer_soonest_reset != on {
                            next.prefer_soonest_reset = on;
                            this.apply_account_request(UsageRequest::SetAdvancedPolicy(next), cx);
                        }
                    });
                },
            )
            .aria_label(crate::i18n::catalog::shell::ACCOUNT_RESET_FIRST.get());
            let cooldown_btns = [0_u64, 5, 15, 30].into_iter().map(|mins| {
                let secs = mins * 60;
                let id_str = format!("admin-cooldown-{mins}m");
                let selected = adv.cooldown_secs / 60 == mins;
                let adv_cl = adv.clone();
                layout::segment(
                    p,
                    gpui::ElementId::Name(id_str.clone().into()),
                    crate::i18n::catalog::shell::ACCOUNT_COOLDOWN_MINUTES
                        .format(&[("minutes", &mins.to_string())]),
                    selected,
                )
                .debug_selector(move || id_str.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let mut next = adv_cl.clone();
                    next.cooldown_secs = secs;
                    this.apply_account_request(UsageRequest::SetAdvancedPolicy(next), cx);
                }))
            });
            auto_switch_card = auto_switch_card
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(layout::label(
                            p,
                            crate::i18n::catalog::shell::ACCOUNT_PREDICT.get(),
                            Some(crate::i18n::catalog::shell::ACCOUNT_PREDICT_DESC.get()),
                        ))
                        .child(layout::controls().child(predict_switch)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(layout::label(
                            p,
                            crate::i18n::catalog::shell::ACCOUNT_RESET_FIRST.get(),
                            Some(crate::i18n::catalog::shell::ACCOUNT_RESET_FIRST_DESC.get()),
                        ))
                        .child(layout::controls().child(reset_switch)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(layout::label(
                            p,
                            crate::i18n::catalog::shell::ACCOUNT_COOLDOWN.get(),
                            Some(crate::i18n::catalog::shell::ACCOUNT_COOLDOWN_DESC.get()),
                        ))
                        .child(
                            layout::controls().child(layout::segmented_control(p, cooldown_btns)),
                        ),
                );
        }

        auto_switch_card = auto_switch_card.child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .flex_wrap()
                .child(
                    layout::controls()
                        .child(
                            layout::secondary(
                                "admin-accounts-refresh",
                                text::ACCOUNTS_REFRESH.get(),
                            )
                            .icon(IconName::RefreshCw)
                            .loading(self.accounts_refresh_pending)
                            .debug_selector(|| "admin-accounts-refresh".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_accounts_manually(cx);
                            })),
                        )
                        .when(self.account_host.is_local(), |c| {
                            c.child(
                                layout::secondary(
                                    "admin-open-accounts-toml",
                                    text::ACCOUNTS_OPEN_TOML.get(),
                                )
                                .icon(IconName::ExternalLink)
                                .debug_selector(|| "admin-open-accounts-toml".into())
                                .on_click(|_, _, cx| {
                                    open_file("accounts.toml", "", cx);
                                }),
                            )
                        }),
                ),
        );

        root.child(auto_switch_card)
    }

    fn render_agents_tab(
        &mut self,
        s: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use gpui_component::Sizable as _;
        let p = crate::ui::palette(cx);
        let default_agent_btns = [
            ("claude", "Claude Code"),
            ("codex", "Codex"),
            ("agy", "Antigravity"),
            ("cursor", "Cursor"),
            ("opencode", "OpenCode"),
        ]
        .into_iter()
        .map(|(agent_id, label)| {
            let btn_id = format!("admin-default-agent-{agent_id}");
            let check_id = format!("admin-default-agent-check-{agent_id}");
            let selected = s.default_agent == agent_id;
            div()
                .id(gpui::ElementId::Name(btn_id.clone().into()))
                .debug_selector(move || btn_id.clone())
                .role(gpui::Role::RadioButton)
                .aria_label(label)
                .aria_toggled(if selected {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .tab_index(0)
                .focus_visible(|style| style.border_color(p.ring))
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                .rounded(crate::ui::RADIUS_SM)
                .border_1()
                .cursor_pointer()
                .text_size(crate::ui::TEXT_SM)
                .when(selected, |d| {
                    d.border_color(p.muted_foreground.opacity(0.4))
                        .bg(p.accent)
                        .text_color(p.accent_foreground)
                        .font_weight(gpui::FontWeight::MEDIUM)
                })
                .when(!selected, |d| {
                    d.border_color(p.border)
                        .bg(p.background.opacity(0.5))
                        .text_color(p.muted_foreground)
                        .hover(|st| {
                            st.border_color(p.muted_foreground.opacity(0.35))
                                .bg(p.accent.opacity(0.5))
                                .text_color(p.foreground)
                        })
                        .active(|st| st.bg(p.accent))
                })
                .on_click(move |_, _, cx| {
                    edit(cx, move |s| agent_id.clone_into(&mut s.default_agent));
                })
                .child(crate::ui::icons::agent_menu_icon(agent_id))
                .child(label)
                .when(selected, |d| {
                    d.child(
                        div()
                            .id(gpui::ElementId::Name(check_id.clone().into()))
                            .debug_selector(move || check_id.clone())
                            .flex()
                            .items_center()
                            .child(
                                gpui_component::Icon::new(gpui_component::IconName::Check).xsmall(),
                            ),
                    )
                })
        });
        let default_agent_row = layout::card(p)
            .id("admin-default-agent-card")
            .debug_selector(|| "admin-default-agent-card".into())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(self.search_focus.label(
                p,
                text::DEFAULT_AGENT_TITLE.get(),
                Some(text::DEFAULT_AGENT_DESC.get()),
            ))
            .child(
                layout::controls()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .gap_2()
                    .children(default_agent_btns),
            );

        let view_mode_term = layout::segment(
            p,
            "admin-view-mode-terminal",
            text::DEFAULT_VIEW_MODE_TERMINAL.get(),
            s.default_view_mode == DefaultViewMode::Terminal,
        )
        .debug_selector(|| "admin-view-mode-terminal".into())
        .on_click(|_, _, cx| {
            edit(cx, move |s| s.default_view_mode = DefaultViewMode::Terminal);
        });
        let view_mode_chat = layout::segment(
            p,
            "admin-view-mode-chat",
            text::DEFAULT_VIEW_MODE_CHAT.get(),
            s.default_view_mode == DefaultViewMode::Chat,
        )
        .debug_selector(|| "admin-view-mode-chat".into())
        .on_click(|_, _, cx| {
            edit(cx, move |s| s.default_view_mode = DefaultViewMode::Chat);
        });
        let default_view_mode_row = layout::row(p)
            .child(self.search_focus.label(
                p,
                text::DEFAULT_VIEW_MODE_TITLE.get(),
                Some(text::DEFAULT_VIEW_MODE_DESC.get()),
            ))
            .child(
                layout::controls().child(
                    layout::segmented_control(p, [view_mode_term, view_mode_chat])
                        .id("admin-default-view-mode")
                        .debug_selector(|| "admin-default-view-mode".into()),
                ),
            );

        // YOLO 토글은 `settings.toml` 저장과 함께 셸이 데몬에 적용할 권한 요청을 남긴다(Orca AgentPermissionsSetting 세그먼트).
        let yolo_seg = layout::segment(
            p,
            "admin-yolo-yolo",
            text::YOLO_MODE_YOLO.get(),
            s.yolo_mode,
        )
        .debug_selector(|| "admin-yolo-yolo".into())
        .on_click(|_, _, cx| {
            cx.global_mut::<Config>().pending_permission =
                Some(krill_proto::agent::Permission::Yolo);
            edit(cx, move |s| s.yolo_mode = true);
        });
        let manual_seg = layout::segment(
            p,
            "admin-yolo-manual",
            text::YOLO_MODE_MANUAL.get(),
            !s.yolo_mode,
        )
        .debug_selector(|| "admin-yolo-manual".into())
        .on_click(|_, _, cx| {
            cx.global_mut::<Config>().pending_permission =
                Some(krill_proto::agent::Permission::Manual);
            edit(cx, move |s| s.yolo_mode = false);
        });
        let yolo_row = layout::row(p)
            .child(
                self.search_focus
                    .label(p, text::YOLO_TITLE.get(), Some(text::YOLO_DESC.get())),
            )
            .child(
                layout::controls().child(
                    layout::segmented_control(p, [yolo_seg, manual_seg])
                        .id("admin-yolo-toggle")
                        .debug_selector(|| "admin-yolo-toggle".into()),
                ),
            );

        // 유휴 동면은 `settings.toml` + 상태 디렉터리의 `hibernation.enabled` 표식으로 데몬에
        // 전달한다(OWE-655). 표식 동기화는 `edit`·`reload`가 맡는다.
        let hib_row = layout::row(p)
            .child(self.search_focus.label(
                p,
                text::HIBERNATION_TITLE.get(),
                Some(text::HIBERNATION_DESC.get()),
            ))
            .child(
                layout::controls().child(
                    layout::switch(
                        p,
                        "hib-toggle",
                        s.hibernation_enabled,
                        false,
                        |next, _, cx| edit(cx, move |s| s.hibernation_enabled = next),
                    )
                    .aria_label(text::HIBERNATION_TITLE.get()),
                ),
            );

        // 끄면 Krill이 설치한 훅을 되돌리고, 다시 켜면 설치된 에이전트에 바로 설치한다.
        let hooks_row = layout::row(p)
            .child(self.search_focus.label(
                p,
                text::AGENT_HOOKS_TITLE.get(),
                Some(text::AGENT_HOOKS_DESC.get()),
            ))
            .child(
                layout::controls().child(
                    layout::switch(
                        p,
                        "agent-hooks-toggle",
                        s.agent_status_hooks,
                        false,
                        |next, _, cx| {
                            edit(cx, move |s| s.agent_status_hooks = next);
                            if let Some(env) = (!cfg!(test))
                                .then(crate::onboarding::Env::current)
                                .flatten()
                            {
                                crate::onboarding::actions::apply_hook_preference(env, next, cx)
                                    .detach();
                            }
                        },
                    )
                    .aria_label(text::AGENT_HOOKS_TITLE.get()),
                ),
            );

        let auto_setup_row = layout::row(p)
            .child(self.search_focus.label(
                p,
                text::AUTO_SETUP_ALL_TITLE.get(),
                Some(text::AUTO_SETUP_ALL_DESC.get()),
            ))
            .child(
                layout::controls().child(
                    layout::primary("admin-agents-setup-all", text::AUTO_SETUP_ALL_BTN.get())
                        .debug_selector(|| "admin-agents-setup-all".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            let base = if cfg!(test) {
                                state_dir(cx).unwrap_or_else(crate::accounts::default_home)
                            } else {
                                crate::accounts::default_home()
                            };
                            let state = state_dir(cx).unwrap_or_else(|| base.clone());
                            let env = (!cfg!(test))
                                .then(crate::onboarding::Env::current)
                                .flatten();
                            let bg_env = env.clone();
                            this.file_job(
                                "setup-agents",
                                move || {
                                    if let Some(env) = &bg_env
                                        && !env.is_isolated_state()
                                    {
                                        drop(crate::onboarding::stage_binaries(&env.krill, &base));
                                    }
                                    let installed = setup_all_agents_and_skills_at(&base, &state);
                                    if let Some(env) = &bg_env
                                        && !env.is_isolated_state()
                                    {
                                        drop(
                                            crate::onboarding::install_codex_hook_with_login_path(
                                                env, &state,
                                            ),
                                        );
                                    }
                                    installed
                                },
                                move |this, installed, cx| {
                                    if let Some(env) = env
                                        && !env.is_isolated_state()
                                    {
                                        crate::onboarding::actions::auto_prepare(env, cx).detach();
                                    }
                                    if !installed.is_empty() {
                                        this.skill_status = Some(
                                            text::SKILLS_INSTALLED
                                                .format(&[("path", &installed.join(", "))])
                                                .into(),
                                        );
                                    }
                                },
                                cx,
                            );
                        })),
                ),
            );

        let mut root = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(default_agent_row)
            .child(default_view_mode_row)
            .child(yolo_row)
            .child(hib_row)
            .child(hooks_row)
            .child(auto_setup_row);

        let builtin_agents = krill_proto::agent::Agents::load(None).unwrap_or_default();
        let dir = state_dir(cx);
        self.ensure_agents_file(dir.as_deref(), cx);
        // 초기 읽기가 끝나기 전에 편집기를 기본값으로 만들면 사용자 인자가
        // 늦게 도착해도 입력 상태에 반영되지 않는다. 로드 후 처음 만든다.
        if dir.is_some() && self.agents_file_cache.0.is_none() {
            return root.children(self.render_agent_extra_rows(s, cx));
        }
        let effective = self
            .agents_parsed
            .clone()
            .unwrap_or_else(|| std::sync::Arc::new(builtin_agents.clone()));
        root = root.child(self.render_agent_catalog_card(&builtin_agents, &effective, window, cx));
        if let Some(status) = &self.skill_status {
            root = root.child(
                layout::card(p)
                    .debug_selector(|| "admin-agent-setup-status".into())
                    .child(div().text_size(crate::ui::TEXT_XS).child(status.clone())),
            );
        }
        root.children(self.render_agent_extra_rows(s, cx))
    }

    #[inline(never)]
    fn render_agent_extra_rows(&mut self, s: &Settings, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let p = crate::ui::palette(cx);
        let auto_rename_row = layout::row(p)
            .child(self.search_focus.label(
                p,
                text::AUTO_RENAME_BRANCH.get(),
                Some(text::AUTO_RENAME_BRANCH_DESC.get()),
            ))
            .child(
                layout::controls().child(
                    layout::switch(
                        p,
                        "auto-rename-branch-toggle",
                        s.auto_rename_branch_from_work,
                        false,
                        |next, _, cx| edit(cx, move |s| s.auto_rename_branch_from_work = next),
                    )
                    .aria_label(text::AUTO_RENAME_BRANCH.get()),
                ),
            );

        let is_1h = s.prompt_cache_ttl_secs >= 3_600;
        let ttl_5m = layout::segment(p, "prompt-cache-ttl-5m", text::CACHE_TTL_5M.get(), !is_1h)
            .debug_selector(|| "prompt-cache-ttl-5m".into())
            .on_click(|_, _, cx| edit(cx, move |s| s.prompt_cache_ttl_secs = 300));
        let ttl_1h = layout::segment(p, "prompt-cache-ttl-1h", text::CACHE_TTL_1H.get(), is_1h)
            .debug_selector(|| "prompt-cache-ttl-1h".into())
            .on_click(|_, _, cx| edit(cx, move |s| s.prompt_cache_ttl_secs = 3_600));
        let cache_timer_card = layout::card(p)
            .debug_selector(|| "prompt-cache-timer-card".into())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(self.search_focus.label(
                        p,
                        text::PROMPT_CACHE_TIMER.get(),
                        Some(text::PROMPT_CACHE_TIMER_DESC.get()),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "prompt-cache-timer-toggle",
                                s.prompt_cache_timer_enabled,
                                false,
                                |next, _, cx| {
                                    edit(cx, move |s| s.prompt_cache_timer_enabled = next);
                                },
                            )
                            .aria_label(text::PROMPT_CACHE_TIMER.get()),
                        ),
                    ),
            )
            .when(s.prompt_cache_timer_enabled, |d| {
                d.child(
                    div()
                        .id("prompt-cache-ttl-row")
                        .debug_selector(|| "prompt-cache-ttl-row".into())
                        .w_full()
                        .min_w_0()
                        .pl_6()
                        .pt_2()
                        .border_t_1()
                        .border_color(p.border)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(self.search_focus.label(
                            p,
                            text::PROMPT_CACHE_TTL.get(),
                            Some(text::PROMPT_CACHE_TTL_DESC.get()),
                        ))
                        .child(
                            layout::controls()
                                .child(layout::segmented_control(p, [ttl_5m, ttl_1h])),
                        ),
                )
            });

        let view = cx.entity();
        let mut workspace_trust_section = div().w_full().min_w_0().flex().flex_col().gap_2().child(
            layout::row(p)
                .child(self.search_focus.label(
                    p,
                    text::WORKSPACE_TRUST_PRESET.get(),
                    Some(text::WORKSPACE_TRUST_PRESET_DESC.get()),
                ))
                .child(
                    layout::controls().child(
                        layout::switch(
                            p,
                            "workspace-trust-preset-toggle",
                            s.workspace_trust_preset_enabled,
                            false,
                            move |next, _, cx| {
                                view.update(cx, |this, cx| {
                                    if next {
                                        if !current(cx).workspace_trust_preset_enabled {
                                            this.workspace_trust_confirm_pending = true;
                                            cx.notify();
                                        }
                                    } else {
                                        this.workspace_trust_confirm_pending = false;
                                        edit(cx, move |s| s.workspace_trust_preset_enabled = false);
                                        cx.notify();
                                    }
                                });
                            },
                        )
                        .aria_label(text::WORKSPACE_TRUST_PRESET.get()),
                    ),
                ),
        );
        if self.workspace_trust_confirm_pending && !s.workspace_trust_preset_enabled {
            workspace_trust_section = workspace_trust_section.child(
                layout::card(p)
                    .id("workspace-trust-preset-confirm")
                    .debug_selector(|| "workspace-trust-preset-confirm".into())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.foreground)
                            .child(text::WORKSPACE_TRUST_PRESET_CONFIRM_PROMPT.get()),
                    )
                    .child(
                        div()
                            .p_2()
                            .rounded(crate::ui::RADIUS_SM)
                            .bg(p.background)
                            .border_1()
                            .border_color(p.border)
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(text::WORKSPACE_TRUST_PRESET_CONFIRM_DIFF.get()),
                    )
                    .child(
                        layout::controls()
                            .child(
                                layout::primary(
                                    "workspace-trust-preset-confirm-yes",
                                    text::WORKSPACE_TRUST_PRESET_CONFIRM_YES.get(),
                                )
                                .debug_selector(|| "workspace-trust-preset-confirm-yes".into())
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.workspace_trust_confirm_pending = false;
                                        edit(cx, move |s| s.workspace_trust_preset_enabled = true);
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                layout::ghost(
                                    "workspace-trust-preset-confirm-cancel",
                                    text::WORKSPACE_TRUST_PRESET_CANCEL.get(),
                                )
                                .debug_selector(|| "workspace-trust-preset-confirm-cancel".into())
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.workspace_trust_confirm_pending = false;
                                        cx.notify();
                                    },
                                )),
                            ),
                    ),
            );
        }
        vec![auto_rename_row, cache_timer_card, workspace_trust_section]
    }

    #[inline(never)]
    fn render_agent_catalog_card(
        &mut self,
        builtin_agents: &krill_proto::agent::Agents,
        effective: &krill_proto::agent::Agents,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let mut card = layout::card(p)
            .id("admin-agents-catalog-card")
            .debug_selector(|| "admin-agents-catalog-card".into())
            .flex()
            .flex_col()
            .gap_0()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .pb_2p5()
                    .border_b_1()
                    .border_color(p.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(self.search_focus.label(
                        p,
                        text::AGENTS_PROFILES_TITLE.get(),
                        Some(text::AGENTS_PROFILES_DESC.get()),
                    ))
                    .child(
                        layout::controls().child(
                            layout::secondary(
                                "admin-agents-open-toml",
                                text::AGENTS_OPEN_TOML.get(),
                            )
                            .icon(gpui_component::IconName::ExternalLink)
                            .debug_selector(|| "admin-agents-open-toml".into())
                            .on_click(|_, _, cx| {
                                open_file(krill_proto::agent::FILE_NAME, "", cx);
                            }),
                        ),
                    ),
            );
        for (idx, agent_id) in ["claude", "codex", "agy", "cursor", "opencode"]
            .into_iter()
            .enumerate()
        {
            card = card.child(self.render_agent_catalog_row(
                idx,
                agent_id,
                builtin_agents,
                effective,
                window,
                cx,
            ));
        }
        card
    }

    #[inline(never)]
    fn render_agent_catalog_row(
        &mut self,
        idx: usize,
        agent_id: &'static str,
        builtin_agents: &krill_proto::agent::Agents,
        effective: &krill_proto::agent::Agents,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui_component::Sizable as _;
        use krill_proto::doctor::Status as DoctorStatus;
        use krill_proto::usage::ProviderKind;
        let p = crate::ui::palette(cx);
        let display_name = builtin_agents
            .profiles
            .get(agent_id)
            .map_or(agent_id, |prof| prof.name.as_str())
            .to_owned();
        let provider = ProviderKind::parse(agent_id);
        let checks = crate::onboarding::snapshot(cx).checks;
        let agent_check_id = format!("agent.{agent_id}");
        let hook_check_id = format!("hook.{agent_id}");
        let doctor_version = checks
            .iter()
            .find(|c| c.id == agent_check_id && c.status == DoctorStatus::Ok)
            .map(|c| c.detail.trim().to_owned())
            .filter(|d| !d.is_empty() && d != "version unknown");
        let doctor_not_installed = checks
            .iter()
            .any(|c| c.id == hook_check_id && c.status == DoctorStatus::Skip);
        let is_installed = if doctor_version.is_some()
            || checks
                .iter()
                .any(|c| c.id == agent_check_id && c.status == DoctorStatus::Ok)
        {
            true
        } else if doctor_not_installed {
            false
        } else {
            provider.is_none_or(|prov| crate::accounts::installed::is_installed(cx, prov))
        };
        let (badge_tone, badge_text) = if is_installed {
            let text = match &doctor_version {
                Some(ver) => format!("{} · {ver}", crate::i18n::catalog::onboarding::READY.get()),
                None => crate::i18n::catalog::onboarding::READY.get().to_owned(),
            };
            (crate::ui::BadgeTone::Success, text)
        } else {
            (
                crate::ui::BadgeTone::Muted,
                text::CONNECTION_NOT_INSTALLED.get().to_owned(),
            )
        };
        let badge_id = format!("admin-agent-installed-badge-{agent_id}");

        let profile_count = provider.map_or(1usize, |prov| {
            self.accounts_snapshot
                .accounts
                .iter()
                .filter(|a| a.provider == prov)
                .count()
                .max(1)
        });
        let active = provider.and_then(|prov| {
            self.accounts_snapshot
                .accounts
                .iter()
                .find(|a| a.provider == prov && a.active)
        });
        let profiles = text::PROFILES_COUNT.format(&[("count", &profile_count.to_string())]);
        let dir_info = match active {
            Some(a) => format!("{} ({}) · {profiles}", a.label, a.profile_dir),
            None => format!("{display_name} · {profiles}"),
        };
        let eff_prof = effective.profiles.get(agent_id);
        let cmd_name = eff_prof
            .and_then(krill_proto::agent::Profile::program)
            .unwrap_or(agent_id);
        let eff_args = eff_prof
            .map(|p| p.launch.get(1..).unwrap_or_default().join(" "))
            .unwrap_or_default();
        let eff_env = agent_environment_summary(eff_prof);
        let mut launch_summary = cmd_name.to_owned();
        if !eff_args.is_empty() {
            launch_summary.push(' ');
            launch_summary.push_str(&eff_args);
        }
        if !eff_env.is_empty() {
            launch_summary.push(' ');
            launch_summary.push_str(&eff_env);
        }
        let summary_text = format!("{launch_summary} · {dir_info}");
        let summary_id = format!("admin-agent-summary-{agent_id}");

        let hook_cmd = format!("krill hook install {agent_id}");
        let hook_id = format!("admin-agent-hook-{agent_id}");
        let hook_copy = hook_cmd.clone();
        let id_owned = agent_id.to_owned();
        let install_cmd = agent_install_command(agent_id);
        let install_id = format!("admin-agent-install-{agent_id}");
        let builtin_prof = builtin_agents.profiles.get(agent_id);
        let default_args = builtin_prof
            .map(|p| p.launch.get(1..).unwrap_or_default().join(" "))
            .unwrap_or_default();
        let default_env = builtin_prof
            .map(|p| krill_proto::agent::format_env_lines(&p.env))
            .unwrap_or_default();

        let edit_state = self.agent_edits.entry(agent_id).or_insert_with(|| {
            let prof = effective.profiles.get(agent_id);
            let args = prof
                .map(|p| p.launch.get(1..).unwrap_or_default().join(" "))
                .unwrap_or_default();
            let env = prof
                .map(|p| krill_proto::agent::format_env_lines(&p.env))
                .unwrap_or_default();
            AgentEdit {
                args: cx.new(|cx| {
                    gpui_component::input::InputState::new(window, cx).default_value(args)
                }),
                env: cx.new(|cx| {
                    gpui_component::input::TextareaState::new(window, cx)
                        .auto_grow(2, 6)
                        .default_value(env)
                }),
                status: None,
            }
        });
        let (args_input, env_input) = (edit_state.args.clone(), edit_state.env.clone());
        let status = edit_state.status.clone();
        let is_open = self.agent_expanded.contains(agent_id);
        let toggle_id = format!("admin-agent-toggle-{agent_id}");
        let header_id = format!("admin-agent-header-{agent_id}");
        let editor_id = format!("admin-agent-editor-{agent_id}");
        let save_id = format!("admin-agent-save-{agent_id}");
        let reset_id = format!("admin-agent-reset-{agent_id}");

        let edit_section = div()
            .id(gpui::ElementId::Name(editor_id.clone().into()))
            .debug_selector(move || editor_id.clone())
            .w_full()
            .min_w_0()
            .pl_9()
            .flex()
            .flex_col()
            .gap_1p5()
            .text_size(crate::ui::TEXT_XS)
            .child(
                div()
                    .text_color(p.muted_foreground)
                    .child(text::AGENT_ARGS.get()),
            )
            .child(gpui_component::input::Input::new(&args_input).small())
            .child(
                div()
                    .text_color(p.muted_foreground)
                    .child(text::AGENT_ENV.get()),
            )
            .child(gpui_component::input::Textarea::new(&env_input).small())
            .when_some(status, |d, (msg, is_err)| {
                d.child(
                    div()
                        .debug_selector(move || format!("admin-agent-status-{agent_id}"))
                        .text_color(if is_err {
                            p.destructive
                        } else {
                            p.muted_foreground
                        })
                        .child(msg),
                )
            })
            .child(
                layout::controls()
                    .child(
                        layout::primary(
                            gpui::ElementId::Name(save_id.clone().into()),
                            text::AGENT_SAVE.get(),
                        )
                        .debug_selector(move || save_id.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let Some(edit) = this.agent_edits.get_mut(agent_id) else {
                                return;
                            };
                            let args = edit.args.read(cx).value();
                            let env = edit.env.read(cx).value();
                            this.save_agent_edit(agent_id, args.to_string(), env.to_string(), cx);
                        })),
                    )
                    .child(
                        layout::ghost(
                            gpui::ElementId::Name(reset_id.clone().into()),
                            text::AGENT_RESET.get(),
                        )
                        .debug_selector(move || reset_id.clone())
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                let Some(edit) = this.agent_edits.get(agent_id) else {
                                    return;
                                };
                                let args_entity = edit.args.clone();
                                let env_entity = edit.env.clone();
                                let def_args = default_args.clone();
                                let def_env = default_env.clone();
                                args_entity.update(cx, |s, cx| {
                                    s.set_value(def_args.clone(), window, cx);
                                });
                                env_entity.update(cx, |s, cx| {
                                    s.set_value(def_env.clone(), window, cx);
                                });
                                this.save_agent_edit(agent_id, def_args, def_env, cx);
                            },
                        )),
                    ),
            );

        let card_id = format!("admin-agent-card-{agent_id}");
        div()
            .id(gpui::ElementId::Name(card_id.clone().into()))
            .debug_selector(move || card_id.clone())
            .w_full()
            .min_w_0()
            .py_2p5()
            .flex()
            .flex_col()
            .gap_2p5()
            .when(idx > 0, |d| d.border_t_1().border_color(p.border))
            .when(!is_installed, |d| d.opacity(0.75))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .id(gpui::ElementId::Name(header_id.clone().into()))
                            .debug_selector(move || header_id.clone())
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2p5()
                            .rounded(crate::ui::RADIUS_SM)
                            .cursor_pointer()
                            .hover(|d| d.opacity(0.85))
                            .active(|d| d.opacity(0.7))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.agent_expanded.remove(agent_id) {
                                    this.agent_expanded.insert(agent_id);
                                }
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(28.))
                                    .rounded(crate::ui::RADIUS_SM)
                                    .bg(p.muted)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(crate::ui::icons::agent_brand_icon(
                                        agent_id,
                                        crate::ui::ICON_XL,
                                        p.foreground,
                                    )),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_size(crate::ui::TEXT_SM)
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child(display_name),
                                            )
                                            .child(
                                                crate::ui::badge(p, badge_tone, badge_text)
                                                    .id(gpui::ElementId::Name(
                                                        badge_id.clone().into(),
                                                    ))
                                                    .debug_selector(move || badge_id.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .id(gpui::ElementId::Name(summary_id.clone().into()))
                                            .debug_selector(move || summary_id.clone())
                                            .min_w_0()
                                            .truncate()
                                            .font_family(crate::ui::FONT_MONO)
                                            .text_size(crate::ui::TEXT_SM)
                                            .text_color(p.muted_foreground)
                                            .child(summary_text),
                                    ),
                            ),
                    )
                    .child(
                        layout::controls()
                            .when_some(install_cmd, |d, cmd_str| {
                                let sel = install_id.clone();
                                d.child(
                                    layout::secondary(
                                        gpui::ElementId::Name(install_id.clone().into()),
                                        text::INSTALL_IN_TERMINAL.get(),
                                    )
                                    .debug_selector(move || sel.clone())
                                    .on_click(
                                        move |_, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                                cmd_str.clone(),
                                            ));
                                            queue_terminal_command(cmd_str.clone(), cx);
                                        },
                                    ),
                                )
                            })
                            .child(
                                layout::secondary(
                                    gpui::ElementId::Name(hook_id.clone().into()),
                                    text::HOOK_INSTALL.get(),
                                )
                                .debug_selector(move || hook_id.clone())
                                .tooltip(text::HOOK_INSTALL_TIP.format(&[("cmd", &hook_cmd)]))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            hook_copy.clone(),
                                        ));
                                        let base = if cfg!(test) {
                                            state_dir(cx)
                                                .unwrap_or_else(crate::accounts::default_home)
                                        } else {
                                            crate::accounts::default_home()
                                        };
                                        let state = state_dir(cx).unwrap_or_else(|| base.clone());
                                        if id_owned == "codex" {
                                            queue_terminal_command(
                                                format!("{hook_copy} --yes"),
                                                cx,
                                            );
                                            this.skill_status =
                                                Some(text::HOOK_QUEUED.get().into());
                                        } else {
                                            let id = id_owned.clone();
                                            let fallback = hook_copy.clone();
                                            this.file_job(
                                                "hook-install",
                                                move || install_agent_hook_at(&id, &base, &state),
                                                move |this, result, cx| {
                                                    this.skill_status = Some(match result {
                                                        Ok(path) => text::SKILLS_INSTALLED
                                                            .format(&[(
                                                                "path",
                                                                &path.display().to_string(),
                                                            )])
                                                            .into(),
                                                        Err(error) => {
                                                            queue_terminal_command(
                                                                format!("{fallback} --yes"),
                                                                cx,
                                                            );
                                                            text::HOOK_INSTALL_FAILED
                                                                .format(&[(
                                                                    "error",
                                                                    &error.to_string(),
                                                                )])
                                                                .into()
                                                        }
                                                    });
                                                },
                                                cx,
                                            );
                                        }
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                Button::new(gpui::ElementId::Name(toggle_id.clone().into()))
                                    .small()
                                    .ghost()
                                    .icon(if is_open {
                                        gpui_component::IconName::ChevronDown
                                    } else {
                                        gpui_component::IconName::ChevronRight
                                    })
                                    .accessibility_label(if is_open {
                                        text::AGENT_COLLAPSE.get()
                                    } else {
                                        text::AGENT_EXPAND.get()
                                    })
                                    .debug_selector(move || toggle_id.clone())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.agent_expanded.remove(agent_id) {
                                            this.agent_expanded.insert(agent_id);
                                        }
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .when(is_open, |d| d.child(edit_section))
    }

    fn render_remote_krill_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let Some(dir) = state_dir(cx) else {
            return div().child(crate::ui::error_banner(
                crate::ui::palette(cx),
                crate::i18n::link_ui::ERR_UNAVAILABLE.get(),
            ));
        };
        let view = self.remote_krill.get_or_insert_with(|| {
            let executable = crate::hosts::link::executable();
            let backend =
                std::sync::Arc::new(crate::link_ui::real::RealLinkBackend::new(dir, executable));
            cx.new(|cx| crate::link_ui::PairingView::new(backend, window, cx))
        });
        div().child(view.clone())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "SSH 탭은 Tailscale 발견 기기·호스트 추가/편집 2열 폼·첫 연결 키 등록 도우미·호스트 목록 카드를 한 화면에 그린다"
    )]
    fn render_ssh_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        use gpui_component::Sizable as _;

        if !self.tailscale_loaded {
            self.refresh_tailscale_peers(cx);
        }
        self.ensure_ssh_form(window, cx);
        self.refresh_ssh_duplicates(cx);
        let p = crate::ui::palette(cx);
        let mut root = div().flex().flex_col().gap_3().child(
            layout::row(p)
                .child(layout::label(
                    p,
                    text::SSH_TITLE.get(),
                    Some(text::SSH_DESC.get()),
                ))
                .child(
                    layout::controls()
                        .child(
                            layout::secondary("admin-ssh-import", text::SSH_IMPORT.get())
                                .debug_selector(|| "admin-ssh-import".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.import_ssh_hosts(cx);
                                })),
                        )
                        .child(
                            layout::secondary("admin-ssh-open-toml", text::SSH_OPEN_TOML.get())
                                .debug_selector(|| "admin-ssh-open-toml".into())
                                .on_click(|_, _, cx| {
                                    open_file("hosts.toml", "", cx);
                                }),
                        ),
                ),
        );

        // 같은 기기 중복 등록(별칭·IP·Orca 가져오기) 정리 — 두 번 눌러 확인.
        if !self.ssh_duplicates.is_empty() {
            let label_of = |id: &str| {
                self.ssh_hosts
                    .iter()
                    .find(|h| h.id == id)
                    .map_or_else(|| id.to_owned(), |h| h.label.clone())
            };
            let list = self
                .ssh_duplicates
                .iter()
                .map(|(dup, kept)| format!("{} → {}", label_of(dup), label_of(kept)))
                .collect::<Vec<_>>()
                .join(", ");
            let remove = self.confirm_button(
                "admin-ssh-dedupe",
                text::SSH_DUPES_REMOVE.get(),
                cx,
                Self::remove_ssh_duplicates,
            );
            root = root.child(
                layout::card(p)
                    .id("admin-ssh-duplicates")
                    .debug_selector(|| "admin-ssh-duplicates".into())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(layout::label(
                        p,
                        text::SSH_DUPES_TITLE
                            .format(&[("count", &self.ssh_duplicates.len().to_string())]),
                        format!("{} {list}", text::SSH_DUPES_DESC.get()),
                    ))
                    .child(remove),
            );
        }

        // 1. Tailscale 자동 감지 피어("발견된 기기") — 없거나 꺼져 있으면 조용히 숨김
        if !self.tailscale_peers.is_empty() {
            let mut peer_card = layout::card(p)
                .id("admin-ssh-discovered")
                .debug_selector(|| "admin-ssh-discovered".into())
                .flex()
                .flex_col()
                .gap_2()
                .child(layout::label(
                    p,
                    text::SSH_DISCOVERED_TITLE.get(),
                    Some(text::SSH_DISCOVERED_DESC.get()),
                ));
            for (idx, peer) in self.tailscale_peers.iter().enumerate() {
                let already_added = self.ssh_hosts.iter().any(|h| {
                    h.destination == peer.address
                        || h.destination.ends_with(&format!("@{}", peer.address))
                        || h.label == peer.hostname
                });
                let btn_id = format!("admin-ssh-add-peer-{idx}");
                let peer_cl = peer.clone();
                let addr_detail = match &peer.ipv4 {
                    Some(ip) if ip != &peer.address => format!("{} ({ip})", peer.address),
                    _ => peer.address.clone(),
                };
                let action_btn = if already_added {
                    layout::ghost(
                        gpui::ElementId::Name(btn_id.clone().into()),
                        text::SSH_DISCOVERED_ADDED.get(),
                    )
                    .debug_selector(move || btn_id.clone())
                } else {
                    layout::secondary(
                        gpui::ElementId::Name(btn_id.clone().into()),
                        text::SSH_DISCOVERED_ADD.get(),
                    )
                    .debug_selector(move || btn_id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.add_tailscale_peer(&peer_cl, cx);
                    }))
                };
                peer_card = peer_card.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .p_2p5()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.background)
                        .border_1()
                        .border_color(p.border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(crate::ui::TEXT_SM)
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(peer.hostname.clone()),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(crate::ui::FONT_MONO)
                                        .text_size(crate::ui::TEXT_SM)
                                        .text_color(p.muted_foreground)
                                        .child(addr_detail),
                                ),
                        )
                        .child(layout::controls().child(action_btn)),
                );
            }
            root = root.child(peer_card);
        }

        // 2. SSH 호스트 추가·편집 2열 그리드 폼(OWE-800): [표시 이름 | 호스트], [포트 | SSH 키 경로]
        if let Some(form) = &self.ssh_form {
            let label_input = form.label.clone();
            let addr_input = form.address.clone();
            let port_input = form.port.clone();
            let key_input = form.identity_file.clone();
            let is_editing = form.editing_id.is_some();
            let form_error = form.error.clone();
            let form_title = if is_editing {
                text::SSH_EDIT_TITLE.get()
            } else {
                text::SSH_ADD_TITLE.get()
            };
            let submit_label = if is_editing {
                text::SSH_EDIT_SUBMIT.get()
            } else {
                text::SSH_ADD_SUBMIT.get()
            };

            let mut form_card = layout::card(p)
                .id("admin-ssh-form")
                .debug_selector(|| "admin-ssh-form".into())
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    div()
                        .text_size(crate::ui::TEXT_SM)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(form_title),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .gap_2p5()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .text_size(crate::ui::TEXT_XS)
                                .child(
                                    div()
                                        .text_color(p.muted_foreground)
                                        .child(text::SSH_NAME_LABEL.get()),
                                )
                                .child(
                                    div()
                                        .id("admin-ssh-input-label")
                                        .debug_selector(|| "admin-ssh-input-label".into())
                                        .child(
                                            gpui_component::input::Input::new(&label_input).small(),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .text_size(crate::ui::TEXT_XS)
                                .child(
                                    div()
                                        .text_color(p.muted_foreground)
                                        .child(text::SSH_ADDRESS_LABEL.get()),
                                )
                                .child(
                                    div()
                                        .id("admin-ssh-input-address")
                                        .debug_selector(|| "admin-ssh-input-address".into())
                                        .child(
                                            gpui_component::input::Input::new(&addr_input).small(),
                                        ),
                                ),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .gap_2p5()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .text_size(crate::ui::TEXT_XS)
                                .child(
                                    div()
                                        .text_color(p.muted_foreground)
                                        .child(text::SSH_PORT_LABEL.get()),
                                )
                                .child(
                                    div()
                                        .id("admin-ssh-input-port")
                                        .debug_selector(|| "admin-ssh-input-port".into())
                                        .child(
                                            gpui_component::input::Input::new(&port_input).small(),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .text_size(crate::ui::TEXT_XS)
                                .child(
                                    div()
                                        .text_color(p.muted_foreground)
                                        .child(text::SSH_KEY_LABEL.get()),
                                )
                                .child(
                                    div()
                                        .id("admin-ssh-input-key")
                                        .debug_selector(|| "admin-ssh-input-key".into())
                                        .child(
                                            gpui_component::input::Input::new(&key_input).small(),
                                        ),
                                ),
                        ),
                );

            if let Some(err_msg) = form_error {
                form_card = form_card.child(
                    div()
                        .id("admin-ssh-form-error")
                        .debug_selector(|| "admin-ssh-form-error".into())
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.destructive)
                        .child(err_msg),
                );
            }

            let mut form_controls = layout::controls();
            if is_editing {
                form_controls = form_controls.child(
                    layout::ghost("admin-ssh-edit-cancel", text::SSH_EDIT_CANCEL.get())
                        .small()
                        .debug_selector(|| "admin-ssh-edit-cancel".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.cancel_edit_ssh_host(window, cx);
                        })),
                );
            }
            form_controls = form_controls.child(
                layout::primary("admin-ssh-add-submit", submit_label)
                    .small()
                    .debug_selector(|| "admin-ssh-add-submit".into())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.submit_ssh_host_form(window, cx);
                    })),
            );

            form_card = form_card.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(form_controls),
            );
            root = root.child(form_card);
        }

        // 3. 상태 배너 및 첫 연결 도우미("이 기기 등록하기" / 키 생성 확인)
        if let Some(status) = &self.ssh_status {
            let mut status_box = div()
                .id("admin-ssh-status")
                .debug_selector(|| "admin-ssh-status".into())
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .p_2p5()
                .rounded(crate::ui::RADIUS_SM)
                .bg(p.card)
                .border_1()
                .border_color(p.border)
                .child(div().text_size(crate::ui::TEXT_XS).child(status.clone()));
            if let Some(failed_host) = &self.ssh_auth_failed_host {
                let host_for_reg = failed_host.clone();
                status_box = status_box.child(
                    layout::controls().child(
                        layout::primary(
                            "admin-ssh-register-device",
                            text::SSH_REGISTER_BUTTON.get(),
                        )
                        .debug_selector(|| "admin-ssh-register-device".into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.start_register_ssh_host(host_for_reg.clone(), cx);
                        })),
                    ),
                );
            }
            if self.ssh_keygen_confirm_host.is_some() {
                status_box = status_box.child(
                    div()
                        .id("admin-ssh-keygen-confirm")
                        .debug_selector(|| "admin-ssh-keygen-confirm".into())
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_2()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.background)
                        .border_1()
                        .border_color(p.border)
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .child(text::SSH_KEYGEN_CONFIRM_PROMPT.get()),
                        )
                        .child(
                            layout::controls()
                                .child(
                                    layout::primary(
                                        "admin-ssh-keygen-confirm-yes",
                                        text::SSH_KEYGEN_CONFIRM_YES.get(),
                                    )
                                    .debug_selector(|| "admin-ssh-keygen-confirm-yes".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.confirm_keygen_and_register(cx);
                                        },
                                    )),
                                )
                                .child(
                                    layout::ghost(
                                        "admin-ssh-keygen-cancel",
                                        text::SSH_KEYGEN_CANCEL.get(),
                                    )
                                    .debug_selector(|| "admin-ssh-keygen-cancel".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.ssh_keygen_confirm_host = None;
                                            cx.notify();
                                        },
                                    )),
                                ),
                        ),
                );
            }
            root = root.child(status_box);
        }

        // 4. 등록된 호스트 목록(OWE-800 SshTargetCard 구조)
        if self.ssh_hosts.is_empty() {
            root = root.child(
                layout::card(p)
                    .text_size(crate::ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::SSH_EMPTY.get()),
            );
        } else {
            for host in &self.ssh_hosts {
                let status = self.ssh_host_status(&host.id);
                let (dot_color, status_label, status_text_color) = match status {
                    SshHostCardStatus::Connected => {
                        (p.success, text::SSH_STATUS_CONNECTED.get(), p.success)
                    }
                    SshHostCardStatus::Disconnected => (
                        p.muted_foreground.opacity(0.4),
                        text::SSH_STATUS_DISCONNECTED.get(),
                        p.muted_foreground,
                    ),
                    SshHostCardStatus::Error => {
                        (p.destructive, text::SSH_STATUS_ERROR.get(), p.destructive)
                    }
                };
                let card_id = format!("admin-ssh-card-{}", host.id);
                let dot_id = format!("admin-ssh-status-dot-{}", host.id);
                let remote_id = format!("admin-ssh-remote-{}", host.id);
                let conn_id = format!("admin-ssh-connect-{}", host.id);
                let disc_id = format!("admin-ssh-disconnect-{}", host.id);
                let test_id = format!("admin-ssh-test-{}", host.id);
                let probe_id = format!("admin-ssh-probe-{}", host.id);
                let install_id = format!("admin-ssh-install-{}", host.id);
                let edit_id = format!("admin-ssh-edit-{}", host.id);
                let rm_id = format!("admin-ssh-remove-{}", host.id);
                let host_conn = host.clone();
                let host_disc = host.clone();
                let host_cl = host.clone();
                let host_probe = host.clone();
                let host_install = host.clone();
                let host_edit = host.clone();
                let rm_host_id = host.id.clone();
                let card_state = self.ssh_remote_infos.get(&host.id);
                let (remote_text, remote_is_error) = remote_line_text(card_state);
                // OWE-897: 설치·업데이트 버튼은 프로브 완료 + 지원 플랫폼 + 구버전/미설치일 때만.
                let install_label: Option<&'static str> =
                    card_state.and_then(|st| match &st.probe {
                        RemoteProbeState::Ready(info) if info.platform.is_supported() => {
                            match info.version_status(krill_proto::BUILD) {
                                krill_hosts::managed::VersionStatus::NotInstalled
                                | krill_hosts::managed::VersionStatus::Unknown => {
                                    Some(text::SSH_REMOTE_INSTALL.get())
                                }
                                krill_hosts::managed::VersionStatus::UpdateAvailable { .. } => {
                                    Some(text::SSH_REMOTE_UPDATE.get())
                                }
                                krill_hosts::managed::VersionStatus::UpToDate => None,
                            }
                        }
                        _ => None,
                    });
                let install_running =
                    card_state.is_some_and(|st| matches!(st.install, RemoteInstallState::Running));
                let probe_loading =
                    card_state.is_some_and(|st| matches!(st.probe, RemoteProbeState::Loading));
                let probe_label = match card_state {
                    Some(st)
                        if matches!(
                            st.probe,
                            RemoteProbeState::Ready(_) | RemoteProbeState::Failed(_)
                        ) =>
                    {
                        text::SSH_REMOTE_REFRESH.get()
                    }
                    _ => text::SSH_REMOTE_PROBE.get(),
                };

                let connect_or_disconnect_btn = if status == SshHostCardStatus::Connected {
                    layout::secondary(
                        gpui::ElementId::Name(disc_id.clone().into()),
                        text::SSH_DISCONNECT.get(),
                    )
                    .small()
                    .debug_selector(move || disc_id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.disconnect_ssh_host(&host_disc, cx);
                    }))
                } else {
                    layout::secondary(
                        gpui::ElementId::Name(conn_id.clone().into()),
                        text::SSH_CONNECT.get(),
                    )
                    .small()
                    .debug_selector(move || conn_id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.connect_ssh_host(host_conn.clone(), cx);
                    }))
                };

                let probe_btn = if probe_loading {
                    layout::ghost(
                        gpui::ElementId::Name(probe_id.clone().into()),
                        text::SSH_REMOTE_CHECKING.get(),
                    )
                    .small()
                    .debug_selector(move || probe_id.clone())
                } else {
                    layout::secondary(gpui::ElementId::Name(probe_id.clone().into()), probe_label)
                        .small()
                        .debug_selector(move || probe_id.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.refresh_remote_info(host_probe.clone(), cx);
                        }))
                };

                let mut controls = layout::controls()
                    .w_full()
                    .flex_wrap()
                    .flex_shrink_1()
                    .child(connect_or_disconnect_btn)
                    .child({
                        let testing = self.is_ssh_test_pending(&host.id);
                        crate::ui::button_with_reserved_label(
                            gpui::ElementId::Name(test_id.clone().into()),
                            text::SSH_TEST.get(),
                            text::SSH_TESTING.get(),
                            testing,
                        )
                        .outline()
                        .small()
                        .disabled(testing)
                        .debug_selector(move || test_id.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.test_ssh_host(host_cl.clone(), cx);
                        }))
                    })
                    .child(probe_btn);
                if install_running {
                    let running_id = install_id.clone();
                    controls = controls.child(
                        layout::ghost(
                            gpui::ElementId::Name(install_id.clone().into()),
                            text::SSH_REMOTE_INSTALLING.get(),
                        )
                        .small()
                        .debug_selector(move || running_id.clone()),
                    );
                } else if let Some(label) = install_label {
                    let install_id_cl = install_id.clone();
                    controls = controls.child(
                        layout::secondary(gpui::ElementId::Name(install_id.clone().into()), label)
                            .small()
                            .debug_selector(move || install_id_cl.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.install_remote(host_install.clone(), cx);
                            })),
                    );
                }
                controls = controls
                    .child(
                        layout::ghost(
                            gpui::ElementId::Name(edit_id.clone().into()),
                            text::SSH_EDIT.get(),
                        )
                        .small()
                        .debug_selector(move || edit_id.clone())
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.start_edit_ssh_host(&host_edit, window, cx);
                            },
                        )),
                    )
                    // OWE-897: 삭제는 두 번 눌러 확인(파괴적 동작 규칙).
                    .child(
                        self.confirm_button(
                            rm_id.clone(),
                            text::SSH_REMOVE.get(),
                            cx,
                            move |this, cx| {
                                this.remove_ssh_host(&rm_host_id, cx);
                            },
                        )
                        .small(),
                    );

                root = root.child(
                    layout::row(p)
                        .id(gpui::ElementId::Name(card_id.clone().into()))
                        .debug_selector(move || card_id.clone())
                        .flex_col()
                        .items_start()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(crate::ui::icons::svg_box(
                                    crate::ui::icons::SERVER_SVG,
                                    crate::ui::ICON_MD,
                                    p.muted_foreground,
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .min_w_0()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .truncate()
                                                        .text_size(crate::ui::TEXT_SM)
                                                        .font_weight(gpui::FontWeight::MEDIUM)
                                                        .child(host.label.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .flex_none()
                                                        .flex()
                                                        .items_center()
                                                        .gap_1()
                                                        .child(
                                                            div()
                                                                .id(gpui::ElementId::Name(
                                                                    dot_id.clone().into(),
                                                                ))
                                                                .debug_selector(move || {
                                                                    dot_id.clone()
                                                                })
                                                                .size(crate::ui::STATE_DOT)
                                                                .rounded_full()
                                                                .bg(dot_color),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(crate::ui::TEXT_XS)
                                                                .text_color(status_text_color)
                                                                .child(status_label),
                                                        ),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .font_family(crate::ui::FONT_MONO)
                                                .text_size(crate::ui::TEXT_SM)
                                                .text_color(p.muted_foreground)
                                                .child(host.format_summary()),
                                        )
                                        .child(
                                            div()
                                                .id(gpui::ElementId::Name(remote_id.clone().into()))
                                                .debug_selector(move || remote_id.clone())
                                                .min_w_0()
                                                .truncate()
                                                .font_family(crate::ui::FONT_MONO)
                                                .text_size(crate::ui::TEXT_SM)
                                                .text_color(if remote_is_error {
                                                    p.destructive
                                                } else {
                                                    p.muted_foreground
                                                })
                                                .child(remote_text),
                                        ),
                                ),
                        )
                        .child(controls),
                );
            }
        }
        root
    }

    fn ensure_workflow_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.workflow_form {
            if let Some(pending) = form.pending_repo_path.take() {
                let repo_inp = form.repo_path.clone();
                if repo_inp.read(cx).value().as_str() != pending {
                    repo_inp.update(cx, |inp, cx| inp.set_value(pending, window, cx));
                }
            }
            self.sync_workflow_repo_hooks(window, cx);
            return;
        }
        self.refresh_service_cache(cx);
        let store = self.services.worktrees.clone();
        let default_repo = std::env::var("PWD")
            .ok()
            .filter(|pwd| Path::new(pwd).is_absolute() && Path::new(pwd).parent().is_some())
            .or_else(|| Self::workflow_repo_candidates(&store).into_iter().next())
            .unwrap_or_default();
        let loaded_repo = Self::normalize_repo_key(&default_repo);
        let default_setup = self
            .services
            .setup_script_for(&default_repo)
            .unwrap_or_default();
        let default_archive = self
            .services
            .archive_script_for(&default_repo)
            .unwrap_or_default();
        let loaded_setup = default_setup.clone();
        let loaded_archive = default_archive.clone();
        let loaded_location = self
            .services
            .script_for(&self.services.worktree_locations, &default_repo)
            .unwrap_or_default();
        let worktree_location = cx.new(|cx| {
            let mut input = gpui_component::input::InputState::new(window, cx)
                .placeholder(text::WORKTREE_LOCATION_PLACEHOLDER.get());
            input.set_value(loaded_location.clone(), window, cx);
            input
        });
        let repo_path = cx.new(|cx| {
            let mut input = gpui_component::input::InputState::new(window, cx)
                .placeholder(text::HOOKS_REPO_PLACEHOLDER.get());
            if !default_repo.is_empty() {
                input.set_value(default_repo, window, cx);
            }
            input
        });
        let symlink_path = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::SYMLINKS_PLACEHOLDER.get())
        });
        let setup_script = cx.new(|cx| {
            let mut input = gpui_component::input::InputState::new(window, cx)
                .placeholder(text::HOOKS_SETUP_PLACEHOLDER.get());
            if !default_setup.is_empty() {
                input.set_value(default_setup, window, cx);
            }
            input
        });
        let archive_script = cx.new(|cx| {
            let mut input = gpui_component::input::InputState::new(window, cx)
                .placeholder(text::HOOKS_ARCHIVE_PLACEHOLDER.get());
            if !default_archive.is_empty() {
                input.set_value(default_archive, window, cx);
            }
            input
        });
        let qc_name = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::QC_NAME_PLACEHOLDER.get())
        });
        let qc_text = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::QC_TEXT_PLACEHOLDER.get())
        });
        let webhook_url = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::WEBHOOK_URL_PLACEHOLDER.get())
        });
        let repo_obs = cx.observe_in(&repo_path, window, |this, _, window, cx| {
            this.sync_workflow_repo_hooks(window, cx);
        });
        let repo_sub = cx.subscribe_in(
            &repo_path,
            window,
            |this, _, _: &gpui_component::input::InputEvent, window, cx| {
                this.sync_workflow_repo_hooks(window, cx);
            },
        );
        // Input observers notify only; saved disk baselines must never advance with an unsent edit.
        let setup_obs = cx.observe(&setup_script, |_, _, cx| cx.notify());
        let archive_obs = cx.observe(&archive_script, |_, _, cx| cx.notify());
        let location_obs = cx.observe(&worktree_location, |_, _, cx| cx.notify());
        self.workflow_form = Some(WorkflowRepoForm {
            repo_path,
            symlink_path,
            setup_script,
            archive_script,
            qc_name,
            qc_text,
            qc_editing: None,
            webhook_url,
            webhook_error: None,
            pending_repo_path: None,
            loaded_repo,
            loaded_setup,
            loaded_archive,
            worktree_location,
            loaded_location,
            location_status: None,
            refresh_hooks: false,
            status: None,
            _subs: vec![repo_obs, repo_sub, setup_obs, archive_obs, location_obs],
        });
    }

    fn sync_workflow_repo_hooks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.workflow_form else {
            return;
        };
        let raw_repo = form.repo_path.read(cx).value().to_string();
        let next_repo = Self::normalize_repo_key(&raw_repo);
        if next_repo
            .as_ref()
            .is_some_and(|repo| !self.services.aliases.contains_key(repo))
            && state_dir(cx).is_some()
        {
            self.refresh_service_cache(cx);
            return;
        }
        let navigating = next_repo != form.loaded_repo;
        if !navigating && !form.refresh_hooks {
            return;
        }
        let prev_setup = form.loaded_setup.clone();
        let prev_archive = form.loaded_archive.clone();
        let cur_location = form.worktree_location.read(cx).value().to_string();
        let location_input = form.worktree_location.clone();
        let refresh_location = navigating || cur_location == form.loaded_location;
        let cur_setup = form.setup_script.read(cx).value().to_string();
        let cur_archive = form.archive_script.read(cx).value().to_string();
        let setup_inp = form.setup_script.clone();
        let archive_inp = form.archive_script.clone();
        let symlink_inp = form.symlink_path.clone();

        self.refresh_service_cache(cx);
        let repo_setup = next_repo
            .as_deref()
            .and_then(|key| self.services.setup_script_for(key))
            .unwrap_or_default();
        let repo_archive = next_repo
            .as_deref()
            .and_then(|key| self.services.archive_script_for(key))
            .unwrap_or_default();

        let repo_location = next_repo
            .as_deref()
            .and_then(|key| {
                self.services
                    .script_for(&self.services.worktree_locations, key)
            })
            .unwrap_or_default();
        let target_location = if refresh_location {
            repo_location.clone()
        } else {
            cur_location.clone()
        };

        let target_setup = if cur_setup == prev_setup {
            repo_setup.clone()
        } else {
            cur_setup.clone()
        };
        let target_archive = if cur_archive == prev_archive {
            repo_archive.clone()
        } else {
            cur_archive.clone()
        };

        if let Some(form) = &mut self.workflow_form {
            if navigating {
                form.pending_repo_path = None;
                form.status = None;
            }
            form.loaded_repo = next_repo;
            form.loaded_setup = repo_setup;
            form.loaded_archive = repo_archive;
            form.loaded_location = repo_location;
            if navigating {
                form.location_status = None;
            }
            form.refresh_hooks = false;
        }
        if cur_location != target_location {
            location_input.update(cx, |inp, cx| inp.set_value(target_location, window, cx));
        }
        if cur_setup != target_setup {
            setup_inp.update(cx, |inp, cx| inp.set_value(target_setup, window, cx));
        }
        if cur_archive != target_archive {
            archive_inp.update(cx, |inp, cx| inp.set_value(target_archive, window, cx));
        }
        if navigating && !symlink_inp.read(cx).value().is_empty() {
            symlink_inp.update(cx, |inp, cx| inp.set_value("", window, cx));
        }
        cx.notify();
    }

    /// 지정한 저장소 경로의 setup/archive 훅 설정을 워크플로 입력 폼에 불러온다.
    pub fn load_repo_hooks_for(
        &mut self,
        repo_raw: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_workflow_form(window, cx);
        let Some(form) = &self.workflow_form else {
            return;
        };
        let repo_inp = form.repo_path.clone();
        let setup_inp = form.setup_script.clone();
        let archive_inp = form.archive_script.clone();
        let symlink_inp = form.symlink_path.clone();
        let location_input = form.worktree_location.clone();
        let next_repo = Self::normalize_repo_key(repo_raw);
        self.refresh_service_cache(cx);
        let repo_setup = next_repo
            .as_deref()
            .and_then(|key| self.services.setup_script_for(key))
            .unwrap_or_default();
        let repo_archive = next_repo
            .as_deref()
            .and_then(|key| self.services.archive_script_for(key))
            .unwrap_or_default();
        let repo_location = next_repo
            .as_deref()
            .and_then(|key| {
                self.services
                    .script_for(&self.services.worktree_locations, key)
            })
            .unwrap_or_default();
        let resolved = next_repo
            .as_ref()
            .is_none_or(|repo| self.services.aliases.contains_key(repo));
        if let Some(form) = &mut self.workflow_form {
            form.pending_repo_path = None;
            form.loaded_repo = if resolved { next_repo } else { None };
            form.loaded_setup.clone_from(&repo_setup);
            form.loaded_archive.clone_from(&repo_archive);
            form.refresh_hooks = !resolved;
            form.status = None;
            form.loaded_location.clone_from(&repo_location);
            form.location_status = None;
        }
        if repo_inp.read(cx).value().as_str() != repo_raw {
            repo_inp.update(cx, |inp, cx| inp.set_value(repo_raw.to_owned(), window, cx));
        }
        location_input.update(cx, |inp, cx| inp.set_value(repo_location, window, cx));
        setup_inp.update(cx, |inp, cx| inp.set_value(repo_setup, window, cx));
        archive_inp.update(cx, |inp, cx| inp.set_value(repo_archive, window, cx));
        if !symlink_inp.read(cx).value().is_empty() {
            symlink_inp.update(cx, |inp, cx| inp.set_value("", window, cx));
        }
        self.refresh_service_cache(cx);
        cx.notify();
    }

    /// 워크플로 탭의 프로젝트 선택 후보. 심볼릭 링크·훅·코멘트·워크트리 부모에 등록된 저장소 중
    /// 실제 절대 경로만 남긴다(`내 드라이브::workspace:…` 같은 내부 키는 프로젝트가 아니다).
    fn workflow_repo_candidates(
        store: &krill_proto::orchestration::WorktreeStore,
    ) -> std::collections::BTreeSet<String> {
        store
            .symlinks
            .keys()
            .chain(store.setup_scripts.keys())
            .chain(store.archive_scripts.keys())
            .chain(store.comments.keys())
            .chain(store.parents.values())
            .map(|r| krill_proto::orchestration::clean_workspace_path_str(r))
            .filter(|r| Path::new(r).is_absolute() && !r.contains("::workspace:"))
            .collect()
    }

    fn workflow_hooks_ready(&self, cx: &App) -> bool {
        self.workflow_form.as_ref().is_some_and(|form| {
            let repo = form.repo_path.read(cx).value();
            let repo = repo.trim();
            !repo.is_empty()
                && self.services.aliases.contains_key(repo)
                && form.loaded_repo.as_deref() == Some(repo)
        })
    }

    fn save_workflow_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_workflow_repo_hooks(window, cx);
        if !self.workflow_hooks_ready(cx) {
            return;
        }
        let Some(form) = &self.workflow_form else {
            return;
        };
        let repo = form.repo_path.read(cx).value().to_string();
        let setup = form.setup_script.read(cx).value().to_string();
        let archive = form.archive_script.read(cx).value().to_string();
        self.save_repo_hooks_for(&repo, Some(&setup), Some(&archive), cx)
            .ok();
    }

    fn normalize_repo_key(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(trimmed.to_owned())
    }

    pub fn add_worktree_symlink_for(
        &mut self,
        repo_raw: &str,
        rel_path: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(dir) = state_dir(cx) else {
            let msg = text::NO_STATE_DIR.get().to_owned();
            if let Some(form) = &mut self.workflow_form {
                form.status = Some((msg.clone().into(), true));
            }
            cx.notify();
            return Err(msg);
        };
        let Some(repo_key) = Self::normalize_repo_key(repo_raw) else {
            let msg = text::HOOKS_REPO_PLACEHOLDER.get().to_owned();
            if let Some(form) = &mut self.workflow_form {
                form.status = Some((msg.clone().into(), true));
            }
            cx.notify();
            return Err(msg);
        };
        if let Some(form) = &mut self.workflow_form
            && Self::normalize_repo_key(&form.repo_path.read(cx).value()).as_deref()
                != Some(repo_key.as_str())
        {
            form.pending_repo_path = Some(repo_key.clone());
        }
        let rel_path = krill_proto::orchestration::validate_symlink_path(rel_path)
            .map_err(|error| error.to_string())?;
        self.file_job(
            "worktree-edit",
            move || {
                let repo_key = canonical_repo_key(&repo_key);
                krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
                    store.add_symlink(&repo_key, &rel_path)
                })
                .map_err(|error| error.to_string())
            },
            |this, result, cx| this.finish_workflow_write(result.map(|_| ()), cx),
            cx,
        );
        Ok(())
    }

    pub fn remove_worktree_symlink_for(
        &mut self,
        repo_key: &str,
        rel_path: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(dir) = state_dir(cx) {
            let repo_key = repo_key.to_owned();
            let rel_path = rel_path.to_owned();
            self.file_job(
                "worktree-edit",
                move || {
                    let repo_key = canonical_repo_key(&repo_key);
                    krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
                        store.remove_symlink(&repo_key, &rel_path);
                        Ok(())
                    })
                    .map_err(|error| error.to_string())
                },
                SettingsView::finish_workflow_write,
                cx,
            );
        }
    }

    fn finish_workflow_write(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.workflow_form {
            form.status = Some(match result {
                Ok(()) => (text::HOOKS_SAVED.get().into(), false),
                Err(error) => (error.into(), true),
            });
        }
        self.refresh_service_cache(cx);
        cx.defer(notify_external_change);
    }

    pub fn save_repo_hooks_for(
        &mut self,
        repo_raw: &str,
        setup_cmd: Option<&str>,
        archive_cmd: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let Some(dir) = state_dir(cx) else {
            let msg = text::NO_STATE_DIR.get().to_owned();
            if let Some(form) = &mut self.workflow_form {
                form.status = Some((msg.clone().into(), true));
            }
            cx.notify();
            return Err(msg);
        };
        let Some(repo_key) = Self::normalize_repo_key(repo_raw) else {
            let msg = text::HOOKS_REPO_PLACEHOLDER.get().to_owned();
            if let Some(form) = &mut self.workflow_form {
                form.status = Some((msg.clone().into(), true));
            }
            cx.notify();
            return Err(msg);
        };
        let setup_trimmed = setup_cmd.unwrap_or("").trim().to_owned();
        let archive_trimmed = archive_cmd.unwrap_or("").trim().to_owned();
        self.file_job(
            "worktree-edit",
            move || {
                let repo_key = canonical_repo_key(&repo_key);
                krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
                    store.set_setup_script(&repo_key, &setup_trimmed)?;
                    store.set_archive_script(&repo_key, &archive_trimmed)?;
                    Ok(())
                })
                .map_err(|error| error.to_string())
            },
            SettingsView::finish_workflow_write,
            cx,
        );
        Ok(())
    }

    pub fn submit_quick_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_workflow_form(window, cx);
        let Some(form) = &self.workflow_form else {
            return;
        };
        let name = form.qc_name.read(cx).value().trim().to_owned();
        let cmd_text = form.qc_text.read(cx).value().trim().to_owned();
        if name.is_empty() || cmd_text.is_empty() {
            return;
        }
        let editing_idx = form.qc_editing;
        let name_inp = form.qc_name.clone();
        let text_inp = form.qc_text.clone();
        edit(cx, move |s| {
            let entry = crate::commands::QuickCommand {
                name: name.clone(),
                text: cmd_text.clone(),
            };
            if let Some(idx) = editing_idx
                && let Some(slot) = s.quick_commands.get_mut(idx)
            {
                *slot = entry;
            } else {
                s.quick_commands.push(entry);
            }
        });
        if let Some(form) = &mut self.workflow_form {
            form.qc_editing = None;
        }
        name_inp.update(cx, |inp, cx| inp.set_value("", window, cx));
        text_inp.update(cx, |inp, cx| inp.set_value("", window, cx));
        cx.notify();
    }

    pub fn edit_quick_command(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_workflow_form(window, cx);
        let s = current(cx);
        let Some(cmd) = s.quick_commands.get(idx).cloned() else {
            return;
        };
        let Some(form) = &mut self.workflow_form else {
            return;
        };
        form.qc_editing = Some(idx);
        let name_inp = form.qc_name.clone();
        let text_inp = form.qc_text.clone();
        name_inp.update(cx, |inp, cx| inp.set_value(cmd.name, window, cx));
        text_inp.update(cx, |inp, cx| inp.set_value(cmd.text, window, cx));
        cx.notify();
    }

    pub fn cancel_quick_command_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.workflow_form else {
            return;
        };
        form.qc_editing = None;
        let name_inp = form.qc_name.clone();
        let text_inp = form.qc_text.clone();
        name_inp.update(cx, |inp, cx| inp.set_value("", window, cx));
        text_inp.update(cx, |inp, cx| inp.set_value("", window, cx));
        cx.notify();
    }

    pub fn add_webhook_url(
        &mut self,
        raw_url: &str,
        cx: &mut Context<Self>,
    ) -> Result<crate::webhook::Endpoint, String> {
        if let Ok(ep) = crate::webhook::Endpoint::from_url_input(raw_url) {
            let ep_clone = ep.clone();
            edit(cx, move |s| {
                if !s.webhooks.contains(&ep_clone) {
                    s.webhooks.push(ep_clone.clone());
                }
            });
            if let Some(form) = &mut self.workflow_form {
                form.webhook_error = None;
            }
            cx.notify();
            Ok(ep)
        } else {
            let msg = text::WEBHOOK_INVALID_URL.get().to_owned();
            if let Some(form) = &mut self.workflow_form {
                form.webhook_error = Some(msg.clone().into());
            }
            cx.notify();
            Err(msg)
        }
    }

    pub fn submit_webhook_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_workflow_form(window, cx);
        let Some(form) = &self.workflow_form else {
            return;
        };
        let raw = form.webhook_url.read(cx).value().to_string();
        let url_inp = form.webhook_url.clone();
        if self.add_webhook_url(&raw, cx).is_ok() {
            url_inp.update(cx, |inp, cx| inp.set_value("", window, cx));
        }
    }

    fn render_workflows_tab(
        &mut self,
        s: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use gpui_component::Sizable as _;
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        self.ensure_workflow_form(window, cx);
        let p = crate::ui::palette(cx);
        let qc_count =
            text::QUICK_COMMANDS_COUNT.format(&[("count", &s.quick_commands.len().to_string())]);
        let wh_count = text::WEBHOOKS_COUNT.format(&[("count", &s.webhooks.len().to_string())]);
        self.refresh_service_cache(cx);
        let orch = self.services.orchestration.clone();
        let pending_gates = orch
            .gates
            .iter()
            .filter(|g| g.status == krill_proto::orchestration::GateStatus::Pending)
            .count();
        let orch_summary = text::ORCH_SUMMARY.format(&[
            ("runs", &orch.runs.len().to_string()),
            ("tasks", &orch.tasks.len().to_string()),
            ("workers", &orch.dispatches.len().to_string()),
            ("gates", &pending_gates.to_string()),
        ]);
        let reset = self.confirm_button(
            "admin-orch-reset",
            text::ORCH_RESET.get(),
            cx,
            |this, cx| {
                if let Some(dir) = state_dir(cx) {
                    this.file_job(
                        "orchestration-reset",
                        move || {
                            krill_proto::orchestration::OrchestrationStore::reset_via_daemon_or_disk(&dir, None)
                            .map_err(|error| error.to_string())
                        },
                        |this, result, cx| {
                            if let Err(error) = result {
                                this.doctor_status = Some(error.into());
                            }
                            cx.defer(notify_external_change);
                        },
                        cx,
                    );
                }
            },
        );
        let mut orch_card = layout::card(p).flex().flex_col().gap_2().child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    layout::label(p, text::ORCH_TITLE.get(), Some(text::ORCH_DESC.get())).child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(orch_summary),
                    ),
                )
                .child(layout::controls().child(reset)),
        );
        for task in orch.tasks.iter().take(6) {
            let line = orchestration_task_line(
                &task.run_id,
                &task.id,
                &task.title,
                task.status,
                &task.deps,
            );
            orch_card = orch_card.child(div().text_size(crate::ui::TEXT_SM).child(line));
        }
        let lines_btns = [50u16, 100, 250, 500].into_iter().map(|lines| {
            let id = format!("admin-context-lines-{lines}");
            let label = lines.to_string();
            let active = s.copy_context_lines == lines;
            layout::segment(p, gpui::ElementId::Name(id.clone().into()), label, active)
                .debug_selector(move || id.clone())
                .on_click(move |_, _, cx| {
                    edit(cx, move |s| s.copy_context_lines = lines);
                })
        });
        let lines_row = layout::row(p)
            .child(layout::label(
                p,
                text::CONTEXT_LINES.get(),
                Some(text::CONTEXT_LINES_DESC.get()),
            ))
            .child(
                layout::controls().child(
                    layout::segmented_control(p, lines_btns)
                        .id("admin-context-lines-control")
                        .debug_selector(|| "admin-context-lines-control".into()),
                ),
            );

        let mut qc_card = layout::card(p)
            .id("admin-qc-card")
            .debug_selector(|| "admin-qc-card".into())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(layout::label(p, qc_count, Some(text::QC_DESC.get())));
        if let Some(form) = &self.workflow_form {
            let qc_name_input = form.qc_name.clone();
            let qc_text_input = form.qc_text.clone();
            let is_editing = form.qc_editing.is_some();
            let mut qc_controls = layout::controls();
            if is_editing {
                qc_controls = qc_controls.child(
                    layout::ghost("admin-qc-cancel", text::QC_CANCEL.get())
                        .debug_selector(|| "admin-qc-cancel".into())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.cancel_quick_command_edit(window, cx);
                        })),
                );
            }
            qc_controls = qc_controls.child(
                layout::primary(
                    "admin-qc-add",
                    if is_editing {
                        text::QC_SAVE.get()
                    } else {
                        text::QC_ADD.get()
                    },
                )
                .debug_selector(|| "admin-qc-add".into())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.submit_quick_command(window, cx);
                })),
            );
            qc_card = qc_card.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("admin-qc-name-input")
                            .debug_selector(|| "admin-qc-name-input".into())
                            .w(px(180.))
                            .flex_none()
                            .child(gpui_component::input::Input::new(&qc_name_input).small()),
                    )
                    .child(
                        div()
                            .id("admin-qc-text-input")
                            .debug_selector(|| "admin-qc-text-input".into())
                            .flex_1()
                            .min_w_0()
                            .child(gpui_component::input::Input::new(&qc_text_input).small()),
                    )
                    .child(qc_controls),
            );
        }
        if s.quick_commands.is_empty() {
            qc_card = qc_card.child(
                div()
                    .id("admin-qc-empty")
                    .debug_selector(|| "admin-qc-empty".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::QC_EMPTY.get()),
            );
        } else {
            for (idx, cmd) in s.quick_commands.iter().enumerate() {
                let row_id = format!("admin-qc-row-{idx}");
                let edit_id = format!("admin-qc-edit-{idx}");
                let rm_id = format!("admin-qc-remove-{idx}");
                let summary = format!("{} → {}", cmd.name, cmd.text);
                qc_card = qc_card.child(
                    div()
                        .id(gpui::ElementId::Name(row_id.clone().into()))
                        .debug_selector(move || row_id.clone())
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_2()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.background)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(crate::ui::FONT_MONO)
                                .text_size(crate::ui::TEXT_SM)
                                .truncate()
                                .child(summary),
                        )
                        .child(
                            layout::controls()
                                .child(
                                    layout::ghost(
                                        gpui::ElementId::Name(edit_id.clone().into()),
                                        text::QC_EDIT.get(),
                                    )
                                    .debug_selector(move || edit_id.clone())
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            this.edit_quick_command(idx, window, cx);
                                        },
                                    )),
                                )
                                .child(
                                    layout::secondary(
                                        gpui::ElementId::Name(rm_id.clone().into()),
                                        text::ACCOUNT_REMOVE.get(),
                                    )
                                    .debug_selector(move || rm_id.clone())
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            if this
                                                .workflow_form
                                                .as_ref()
                                                .is_some_and(|f| f.qc_editing == Some(idx))
                                            {
                                                this.cancel_quick_command_edit(window, cx);
                                            }
                                            edit(cx, move |s| {
                                                if idx < s.quick_commands.len() {
                                                    s.quick_commands.remove(idx);
                                                }
                                            });
                                        },
                                    )),
                                ),
                        ),
                );
            }
        }

        let clear = self.confirm_button(
            "admin-webhook-clear",
            text::WEBHOOK_CLEAR.get(),
            cx,
            |_, cx| edit(cx, move |s| s.webhooks.clear()),
        );
        let mut wh_card = layout::card(p)
            .id("admin-webhooks-card")
            .debug_selector(|| "admin-webhooks-card".into())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(layout::label(p, wh_count, Some(text::WEBHOOK_DESC.get())))
                    .child(
                        layout::controls()
                            .child(
                                layout::secondary(
                                    "admin-webhook-add-local",
                                    text::WEBHOOK_ADD_LOCAL.get(),
                                )
                                .debug_selector(|| "admin-webhook-add-local".into())
                                .on_click(|_, _, cx| {
                                    edit(cx, move |s| {
                                        let n = s.webhooks.len().saturating_add(1);
                                        s.webhooks.push(crate::webhook::Endpoint::Ntfy {
                                            url: format!("http://127.0.0.1:8080/krill-hook-{n}"),
                                        });
                                    });
                                }),
                            )
                            .child(clear),
                    ),
            );
        if let Some(form) = &self.workflow_form {
            let webhook_url_input = form.webhook_url.clone();
            let webhook_err = form.webhook_error.clone();
            wh_card = wh_card.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("admin-webhook-url-input")
                            .debug_selector(|| "admin-webhook-url-input".into())
                            .flex_1()
                            .min_w_0()
                            .child(gpui_component::input::Input::new(&webhook_url_input).small()),
                    )
                    .child(
                        layout::controls().child(
                            layout::primary("admin-webhook-add", text::WEBHOOK_ADD.get())
                                .debug_selector(|| "admin-webhook-add".into())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.submit_webhook_url(window, cx);
                                })),
                        ),
                    ),
            );
            if let Some(err) = webhook_err {
                wh_card = wh_card.child(
                    crate::ui::error_banner(p, err)
                        .id("admin-webhook-error")
                        .debug_selector(|| "admin-webhook-error".into()),
                );
            }
        }
        if s.webhooks.is_empty() {
            wh_card = wh_card.child(
                div()
                    .id("admin-webhooks-empty")
                    .debug_selector(|| "admin-webhooks-empty".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::WEBHOOK_EMPTY.get()),
            );
        } else {
            for (idx, ep) in s.webhooks.iter().enumerate() {
                let row_id = format!("admin-webhook-row-{idx}");
                let badge_id = format!("admin-webhook-badge-{idx}");
                let url_id = format!("admin-webhook-url-{idx}");
                let rm_id = format!("admin-webhook-remove-{idx}");
                let provider_label = ep.provider_badge();
                // OWE-883: URL 원문(비밀 토큰 포함 가능) 대신 종류 표시 이름만 보인다.
                let kind_label = text::WEBHOOK_LABEL.format(&[
                    ("n", &(idx + 1).to_string()),
                    ("kind", webhook_kind_label(ep)),
                ]);
                wh_card = wh_card.child(
                    div()
                        .id(gpui::ElementId::Name(row_id.clone().into()))
                        .debug_selector(move || row_id.clone())
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .p_2()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.background)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    crate::ui::badge(
                                        p,
                                        crate::ui::BadgeTone::Accent,
                                        provider_label,
                                    )
                                    .id(SharedString::from(badge_id.clone()))
                                    .debug_selector(move || badge_id.clone()),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(url_id.clone()))
                                        .debug_selector(move || url_id.clone())
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(crate::ui::TEXT_SM)
                                        .child(kind_label),
                                ),
                        )
                        .child(
                            layout::controls().child(
                                layout::secondary(
                                    gpui::ElementId::Name(rm_id.clone().into()),
                                    text::ACCOUNT_REMOVE.get(),
                                )
                                .debug_selector(move || rm_id.clone())
                                .on_click(move |_, _, cx| {
                                    edit(cx, move |s| {
                                        if idx < s.webhooks.len() {
                                            s.webhooks.remove(idx);
                                        }
                                    });
                                }),
                            ),
                        ),
                );
            }
        }
        let wt_store = self.services.worktrees.clone();

        let active_repo = self
            .workflow_form
            .as_ref()
            .and_then(|f| Self::normalize_repo_key(&f.repo_path.read(cx).value()))
            .or_else(|| {
                self.workflow_form
                    .as_ref()
                    .and_then(|f| f.loaded_repo.clone())
            });
        let active_repo =
            active_repo.map(|raw| self.services.aliases.get(&raw).cloned().unwrap_or(raw));
        let active_repo_display = active_repo
            .as_deref()
            .and_then(|r| Path::new(r).file_name().and_then(|n| n.to_str()))
            .filter(|s| !s.is_empty())
            .or(active_repo.as_deref())
            .unwrap_or("—")
            .to_owned();

        let mut known_repos = Self::workflow_repo_candidates(&wt_store);
        known_repos.extend(self.services.worktree_locations.keys().cloned());
        if let Some(cur) = active_repo.as_ref().filter(|r| Path::new(r).is_absolute()) {
            known_repos.insert(cur.clone());
        }
        let known_repos_list: Vec<String> = known_repos.into_iter().collect();

        let mut repo_context_card = layout::card(p)
            .id("admin-workflow-repo-card")
            .debug_selector(|| "admin-workflow-repo-card".into())
            .flex()
            .flex_col()
            .gap_2p5();

        let mut symlinks_card = layout::card(p)
            .id("admin-worktree-symlinks-card")
            .debug_selector(|| "admin-worktree-symlinks-card".into())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(layout::label(
                        p,
                        text::SYMLINKS_TITLE.get(),
                        Some(text::SYMLINKS_DESC.get()),
                    ))
                    .child(
                        crate::ui::badge(
                            p,
                            crate::ui::BadgeTone::Neutral,
                            active_repo_display.clone(),
                        )
                        .id("admin-symlinks-repo-badge")
                        .debug_selector(|| "admin-symlinks-repo-badge".into()),
                    ),
            );

        let mut hooks_card = layout::card(p)
            .id("admin-repo-hooks-card")
            .debug_selector(|| "admin-repo-hooks-card".into())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(layout::label(
                        p,
                        text::HOOKS_TITLE.get(),
                        Some(text::HOOKS_DESC.get()),
                    ))
                    .child(
                        crate::ui::badge(
                            p,
                            crate::ui::BadgeTone::Neutral,
                            active_repo_display.clone(),
                        )
                        .id("admin-hooks-repo-badge")
                        .debug_selector(|| "admin-hooks-repo-badge".into()),
                    ),
            );

        if let Some(form) = &self.workflow_form {
            let repo_input = form.repo_path.clone();
            let symlink_input = form.symlink_path.clone();
            let setup_input = form.setup_script.clone();
            let archive_input = form.archive_script.clone();
            let status = form.status.clone();

            let weak = cx.weak_entity();
            let dropdown_repos = known_repos_list.clone();
            let dropdown_active = active_repo.clone();
            let repo_dropdown = layout::secondary(
                "admin-workflow-repo-select",
                format!(
                    "{}{} \u{25BE}",
                    active_repo_display.chars().take(28).collect::<String>(),
                    if active_repo_display.chars().count() > 28 {
                        "…"
                    } else {
                        ""
                    }
                ),
            )
            .tooltip(active_repo.clone().unwrap_or_default())
            .max_w(crate::ui::SETTINGS_NAV_WIDTH)
            .debug_selector(|| "admin-workflow-repo-select".into())
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.scrollable(true).max_h(px(360.));
                for repo_choice in &dropdown_repos {
                    let weak = weak.clone();
                    let repo_choice = repo_choice.clone();
                    let checked = dropdown_active.as_deref() == Some(repo_choice.as_str());
                    let name = Path::new(&repo_choice)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .filter(|s| !s.is_empty())
                        .unwrap_or(repo_choice.as_str());
                    let item_label = format!("{name}  {repo_choice}");
                    menu = menu.item(PopupMenuItem::new(item_label).checked(checked).on_click(
                        move |_, window, cx| {
                            weak.update(cx, |this, cx| {
                                this.load_repo_hooks_for(&repo_choice, window, cx);
                            })
                            .ok();
                        },
                    ));
                }
                menu
            });

            repo_context_card = repo_context_card
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex_none()
                                .text_size(crate::ui::TEXT_SM)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(text::WORKFLOW_REPO_TITLE.get()),
                        )
                        .child(
                            div()
                                .id("admin-workflow-repo-input")
                                .debug_selector(|| "admin-workflow-repo-input".into())
                                .flex_1()
                                .min_w_0()
                                .child(gpui_component::input::Input::new(&repo_input).small()),
                        )
                        .child(layout::controls().child(repo_dropdown)),
                )
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::WORKFLOW_REPO_DESC.get()),
                );

            repo_context_card = repo_context_card.child(self.render_worktree_location(cx));

            symlinks_card = symlinks_card
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("admin-worktree-symlink-input")
                                .debug_selector(|| "admin-worktree-symlink-input".into())
                                .flex_1()
                                .min_w_0()
                                .child(gpui_component::input::Input::new(&symlink_input).small()),
                        )
                        .child(
                            layout::controls().child(
                                layout::primary(
                                    "admin-worktree-symlink-add",
                                    text::SYMLINKS_ADD.get(),
                                )
                                .debug_selector(|| "admin-worktree-symlink-add".into())
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        let Some(form) = &this.workflow_form else {
                                            return;
                                        };
                                        let repo = form.repo_path.read(cx).value().to_string();
                                        let rel = form.symlink_path.read(cx).value().to_string();
                                        let symlink_input = form.symlink_path.clone();
                                        if this.add_worktree_symlink_for(&repo, &rel, cx).is_ok() {
                                            symlink_input.update(cx, |inp, cx| {
                                                inp.set_value("", window, cx);
                                            });
                                        }
                                    },
                                )),
                            ),
                        ),
                )
                .child({
                    let mut row = div().flex().flex_wrap().items_center().gap_1p5();
                    for (id, preset) in [
                        ("admin-symlink-preset-env", ".env"),
                        ("admin-symlink-preset-env-local", ".env.local"),
                        ("admin-symlink-preset-node-modules", "node_modules"),
                    ] {
                        row = row.child(
                            layout::ghost(id, preset)
                                .debug_selector(move || id.into())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(form) = &this.workflow_form {
                                        let repo = form.repo_path.read(cx).value().to_string();
                                        this.add_worktree_symlink_for(&repo, preset, cx).ok();
                                    }
                                })),
                        );
                    }
                    row
                });

            let symlink_rows: Vec<(String, String)> = if let Some(repo_key) = active_repo.as_deref()
            {
                wt_store
                    .symlinks
                    .get(repo_key)
                    .into_iter()
                    .flatten()
                    .cloned()
                    .map(|rel| (repo_key.to_owned(), rel))
                    .collect()
            } else {
                let mut rows = Vec::new();
                for (repo_key, paths) in &wt_store.symlinks {
                    for rel_path in paths {
                        rows.push((repo_key.clone(), rel_path.clone()));
                    }
                }
                rows
            };
            if symlink_rows.is_empty() {
                symlinks_card = symlinks_card.child(
                    div()
                        .id("admin-worktree-symlinks-empty")
                        .debug_selector(|| "admin-worktree-symlinks-empty".into())
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::SYMLINKS_EMPTY.get()),
                );
            } else {
                let mut chips_wrap = div()
                    .id("admin-worktree-symlinks-chips")
                    .debug_selector(|| "admin-worktree-symlinks-chips".into())
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1p5();
                for (idx, (repo_key, rel_path)) in symlink_rows.into_iter().enumerate() {
                    let chip_id = format!("admin-symlink-chip-{idx}");
                    let rm_id = format!("admin-symlink-remove-{idx}");
                    let repo_cl = repo_key.clone();
                    let rel_cl = rel_path.clone();
                    chips_wrap = chips_wrap.child(
                        div()
                            .id(gpui::ElementId::Name(chip_id.clone().into()))
                            .debug_selector(move || chip_id.clone())
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_0p5()
                            .rounded(crate::ui::RADIUS_SM)
                            .border_1()
                            .border_color(p.border)
                            .bg(p.background)
                            .child(
                                div()
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_SM)
                                    .text_color(p.foreground)
                                    .child(rel_path.clone()),
                            )
                            .child(
                                Button::new(gpui::ElementId::Name(rm_id.clone().into()))
                                    .ghost()
                                    .icon(gpui_component::IconName::Close)
                                    .accessibility_label(format!(
                                        "{} · {}",
                                        text::SSH_REMOVE.get(),
                                        rel_path
                                    ))
                                    .tooltip(format!("{} · {}", text::SSH_REMOVE.get(), rel_path))
                                    .xsmall()
                                    .debug_selector(move || rm_id.clone())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_worktree_symlink_for(&repo_cl, &rel_cl, cx);
                                    })),
                            ),
                    );
                }
                symlinks_card = symlinks_card.child(chips_wrap);
            }

            hooks_card = hooks_card
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(crate::ui::field_label(
                            text::HOOKS_SETUP_LABEL.get(),
                            None,
                            p,
                        ))
                        .child(
                            div()
                                .id("admin-repo-setup-input")
                                .debug_selector(|| "admin-repo-setup-input".into())
                                .child(gpui_component::input::Input::new(&setup_input).small()),
                        )
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::HOOKS_SETUP_HINT.get()),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(crate::ui::field_label(
                            text::HOOKS_ARCHIVE_LABEL.get(),
                            None,
                            p,
                        ))
                        .child(
                            div()
                                .id("admin-repo-archive-input")
                                .debug_selector(|| "admin-repo-archive-input".into())
                                .child(gpui_component::input::Input::new(&archive_input).small()),
                        )
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::HOOKS_ARCHIVE_HINT.get()),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_2()
                        .child(
                            layout::ghost("admin-repo-hooks-clear", text::HOOKS_CLEAR.get())
                                .debug_selector(|| "admin-repo-hooks-clear".into())
                                .disabled(!self.workflow_hooks_ready(cx))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if !this.workflow_hooks_ready(cx) {
                                        return;
                                    }
                                    let Some(form) = &this.workflow_form else {
                                        return;
                                    };
                                    let repo = form.repo_path.read(cx).value().to_string();
                                    let setup_inp = form.setup_script.clone();
                                    let archive_inp = form.archive_script.clone();
                                    if this.save_repo_hooks_for(&repo, None, None, cx).is_ok() {
                                        setup_inp
                                            .update(cx, |inp, cx| inp.set_value("", window, cx));
                                        archive_inp
                                            .update(cx, |inp, cx| inp.set_value("", window, cx));
                                    }
                                })),
                        )
                        .child(
                            layout::primary("admin-repo-hooks-save", text::HOOKS_SAVE.get())
                                .debug_selector(|| "admin-repo-hooks-save".into())
                                .disabled(!self.workflow_hooks_ready(cx))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.save_workflow_form(window, cx);
                                })),
                        ),
                );

            let mut hook_repos: std::collections::BTreeSet<String> =
                wt_store.setup_scripts.keys().cloned().collect();
            hook_repos.extend(wt_store.archive_scripts.keys().cloned());
            for (idx, repo_key) in hook_repos.into_iter().enumerate() {
                let setup_val = self
                    .services
                    .setup_script_for(&repo_key)
                    .unwrap_or_else(|| "—".to_owned());
                let archive_val = self
                    .services
                    .archive_script_for(&repo_key)
                    .unwrap_or_else(|| "—".to_owned());
                let summary = format!("{repo_key} · setup={setup_val} · archive={archive_val}");
                let row_id = format!("admin-repo-hook-row-{idx}");
                let repo_for_click = repo_key.clone();
                hooks_card = hooks_card.child(
                    div()
                        .id(gpui::ElementId::Name(row_id.clone().into()))
                        .debug_selector(move || row_id.clone())
                        .w_full()
                        .min_w_0()
                        .cursor_pointer()
                        .p_2()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.background)
                        .hover(|d| d.bg(p.accent))
                        .active(|d| d.bg(p.sidebar_accent))
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_SM)
                        .truncate()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.load_repo_hooks_for(&repo_for_click, window, cx);
                        }))
                        .child(summary),
                );
            }

            if let Some((msg, is_error)) = status {
                hooks_card = hooks_card.child(
                    div()
                        .id("admin-workflow-repo-status")
                        .debug_selector(|| "admin-workflow-repo-status".into())
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(if is_error {
                            p.destructive
                        } else {
                            p.muted_foreground
                        })
                        .child(msg),
                );
            }
        }

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(orch_card)
            .child(self.render_schedules_section(cx))
            .child(repo_context_card)
            .child(symlinks_card)
            .child(hooks_card)
            .child(layout::section_title(
                p,
                "admin-section-workflows",
                text::WORKFLOWS_TITLE.get(),
            ))
            .child(lines_row)
            .child(qc_card)
            .child(wh_card)
    }

    fn render_resources_tab(&self, s: &Settings, cx: &mut Context<Self>) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let row = || layout::row(p);
        let label = |title: &'static str, desc: &'static str| layout::label(p, title, Some(desc));
        // 구 데몬이면 토글 대신 업데이트 안내만 보인다(요청을 보내지 않는다).
        let recording_controls = if self.history_pause_supported == Some(false) {
            layout::controls().child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.warning)
                    .child(text::RECORDING_UPDATE_NEEDED.get()),
            )
        } else {
            let view = cx.entity();
            layout::controls().child(
                layout::switch(
                    p,
                    "admin-recording-paused",
                    !s.recording_paused,
                    false,
                    move |active, _, cx| {
                        view.update(cx, |this, cx| {
                            this.set_recording_paused(!active, cx);
                        });
                    },
                )
                .aria_label(text::RECORDING_TITLE.get()),
            )
        };

        let clear = self
            .confirm_button(
                "admin-history-clear",
                text::RECORDING_DELETE_NOW.get(),
                cx,
                Self::clear_history,
            )
            .disabled(self.history_clear_pending);
        let info = |body: &'static str| {
            layout::card(p)
                .text_size(crate::ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(body)
        };
        let mut root = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(layout::section_title(
                p,
                "admin-section-resources",
                text::RESOURCES_TITLE.get(),
            ))
            .child(info(text::RESOURCES_BUDGET.get()))
            .child(info(text::RESOURCES_SECURITY.get()))
            .child(
                row()
                    .child(label(
                        text::ERROR_REPORTS_TITLE.get(),
                        text::ERROR_REPORTS_DESC.get(),
                    ))
                    .child(
                        layout::controls().child(
                            layout::switch(
                                p,
                                "admin-error-reporting-toggle",
                                self.error_reporting
                                    .status
                                    .as_ref()
                                    .is_some_and(|status| status.consented),
                                self.error_reporting.pending
                                    || self.error_reporting.status.is_none(),
                                {
                                    let view = cx.entity();
                                    move |enabled, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.set_error_reporting(enabled, cx);
                                        });
                                    }
                                },
                            )
                            .aria_label(text::ERROR_REPORTS_TITLE.get()),
                        ),
                    ),
            )
            .when_some(self.error_reporting.description(), |el, message| {
                el.child(
                    div()
                        .id("admin-error-reporting-status")
                        .debug_selector(|| "admin-error-reporting-status".into())
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(message),
                )
            })
            .child(
                row()
                    .child(label(
                        text::RECORDING_TITLE.get(),
                        text::RECORDING_DESC.get(),
                    ))
                    .child(recording_controls),
            )
            .child(
                row()
                    .child(label(
                        text::RECORDING_DELETE_TITLE.get(),
                        text::RECORDING_DELETE_DESC.get(),
                    ))
                    .child(layout::controls().child(clear)),
            )
            // Secure Keyboard Entry는 macOS 전용이라 다른 OS에서는 숨긴다(규칙 10).
            .when(cfg!(target_os = "macos"), |el| {
                el.child(
                    row()
                        .child(label(
                            crate::security::text::SECURE_KEYBOARD_MENU.get(),
                            text::SECURE_KB_DESC.get(),
                        ))
                        .child(
                            layout::controls().child(
                                layout::switch(
                                    p,
                                    "admin-secure-kb-toggle",
                                    s.secure_keyboard_entry,
                                    false,
                                    |next, _, cx| {
                                        edit(cx, move |s| s.secure_keyboard_entry = next);
                                        crate::platform::set_secure_event_input(next);
                                    },
                                )
                                .aria_label(crate::security::text::SECURE_KEYBOARD_MENU.get()),
                            ),
                        ),
                )
            })
            .child(
                row()
                    .child(label(text::DOCTOR_TITLE.get(), text::DOCTOR_DESC.get()))
                    .child(
                        layout::controls().child(
                            layout::secondary("admin-run-doctor", text::DOCTOR_RUN.get())
                                .debug_selector(|| "admin-run-doctor".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.run_doctor(cx);
                                })),
                        ),
                    ),
            );
        if let Some(status) = &self.doctor_status {
            root = root.child(
                div()
                    .id("admin-doctor-status")
                    .debug_selector(|| "admin-doctor-status".into())
                    .w_full()
                    .min_w_0()
                    .p_2()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.card)
                    .border_1()
                    .border_color(p.border)
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_SM)
                    .child(status.clone()),
            );
        }
        if let Some(status) = &self.history_status {
            root = root.child(
                div()
                    .id("admin-history-status")
                    .debug_selector(|| "admin-history-status".into())
                    .w_full()
                    .min_w_0()
                    .p_2()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.card)
                    .border_1()
                    .border_color(p.border)
                    .text_size(crate::ui::TEXT_XS)
                    .child(status.clone()),
            );
        }
        if let Some(status) = &self.recording_status {
            root = root.child(
                div()
                    .id("admin-recording-status")
                    .debug_selector(|| "admin-recording-status".into())
                    .w_full()
                    .min_w_0()
                    .p_2()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.card)
                    .border_1()
                    .border_color(p.border)
                    .text_size(crate::ui::TEXT_XS)
                    .child(status.clone()),
            );
        }
        root = root.child(self.render_space_section(cx));
        root
    }
}

impl SettingsView {
    /// 사이드바 검색 입력을 비우고 전체 탭 목록과 현재 탭 본문을 복원한다(OWE-795).
    pub fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_focus.title = None;
        self.search_pending = false;
        if let Some((input, _)) = &self.search {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        cx.notify();
    }

    /// 탭 페이지 헤더(아이콘·제목·배지·1줄 부제 + 우측 보조 동작 슬롯, OWE-795).
    pub(crate) fn render_page_header(
        tab: AdminTab,
        action: Option<gpui::AnyElement>,
        cx: &App,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui_component::Sizable as _;
        let p = crate::ui::palette(cx);
        div()
            .id("admin-page-header")
            .debug_selector(|| "admin-page-header".into())
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .pb_3()
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(tab.icon_element().small())
                            .child(
                                div()
                                    .id("admin-page-title")
                                    .debug_selector(|| "admin-page-title".into())
                                    .text_size(crate::ui::TEXT_LG)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(tab.label()),
                            )
                            .children(tab.badge().map(|badge_text| {
                                crate::ui::badge(p, crate::ui::BadgeTone::Muted, badge_text)
                                    .id("admin-page-badge")
                                    .debug_selector(|| "admin-page-badge".into())
                            })),
                    )
                    .child(
                        div()
                            .id("admin-page-subtitle")
                            .debug_selector(|| "admin-page-subtitle".into())
                            .min_w_0()
                            .truncate()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(tab.subtitle()),
                    ),
            )
            .child(
                div()
                    .id("admin-page-header-actions")
                    .debug_selector(|| "admin-page-header-actions".into())
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(action),
            )
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::{Icon, IconName, Sizable as _};
        if self.mobile_page {
            return self.render_mobile_page(window, cx).into_any_element();
        }
        let has_modal =
            self.removing.is_some() || self.schedule_modal.is_some() || self.space.cleanup_open;
        if has_modal && self.modal_return_focus.is_none() {
            self.modal_return_focus =
                Some(window.focused(cx).unwrap_or_else(|| self.focus.clone()));
            if let Some(modal) = &self.schedule_modal {
                let input = if modal.editing_id.is_some() {
                    &modal.cron
                } else {
                    &modal.id
                };
                input.update(cx, |input, cx| input.focus(window, cx));
            } else {
                window.focus(&self.modal_dismiss_focus, cx);
            }
        } else if !has_modal && let Some(focus) = self.modal_return_focus.take() {
            window.focus(&focus, cx);
        }
        if self.automations_page {
            return self.render_automations_page(window, cx).into_any_element();
        }
        window.set_window_title(text::TITLE.get());
        let (s, error) = cx.try_global::<Config>().map_or_else(
            || (Settings::default(), None),
            |c| {
                (
                    if matches!(
                        self.tab,
                        AdminTab::General
                            | AdminTab::Terminal
                            | AdminTab::Agents
                            | AdminTab::Workflows
                            | AdminTab::Resources
                    ) {
                        c.settings.clone()
                    } else {
                        Settings::default()
                    },
                    c.error.clone(),
                )
            },
        );
        let p = crate::ui::palette(cx);
        let current_tab = self.tab;

        let search_input = self.search_input(window, cx);
        let search_value = search_input.read(cx).value().to_string();
        let has_search_query = !search_value.is_empty();
        let matches = search::search_tabs(&search_value);
        let groups: &[(&'static str, &[AdminTab])] = &[
            (
                text::GROUP_AI.get(),
                &[AdminTab::Agents, AdminTab::Accounts],
            ),
            (
                text::GROUP_SETUP.get(),
                &[
                    AdminTab::General,
                    AdminTab::Terminal,
                    AdminTab::Install,
                    AdminTab::Shortcuts,
                    AdminTab::Ssh,
                    AdminTab::RemoteKrill,
                ],
            ),
            (
                text::GROUP_WORKFLOW.get(),
                &[AdminTab::Workflows, AdminTab::Mobile, AdminTab::Resources],
            ),
        ];

        let mut nav = div()
            .id("admin-sidebar")
            .debug_selector(|| "admin-sidebar".into())
            .tab_group()
            .w(crate::ui::SETTINGS_NAV_WIDTH)
            .h_full()
            .min_h_0()
            .overflow_y_scroll()
            .flex_none()
            .px_2p5()
            .py_3()
            .bg(p.worktree_sidebar)
            .border_r_1()
            .border_color(p.border)
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div().flex().items_center().child(
                    Button::new("admin-back-to-app")
                        .icon(IconName::ChevronLeft)
                        .label(text::BACK_TO_APP.get())
                        .small()
                        .ghost()
                        .debug_selector(|| "admin-back-to-app".into())
                        .on_click(cx.listener(|_, _, window, cx| {
                            close_settings_page_for(Some(cx.entity_id()), window, cx);
                        })),
                ),
            )
            .child(
                div()
                    .id("admin-search-pill")
                    .debug_selector(|| "admin-search-pill".into())
                    // Esc는 검색어를 지우고, 이미 비어 있으면 설정 페이지를 닫는다. Enter는 첫 결과 탭으로 간다.
                    .capture_action(cx.listener(
                        |this, _: &gpui_component::input::Escape, window, cx| {
                            cx.stop_propagation();
                            if !this.search_query(cx).is_empty() {
                                this.clear_search(window, cx);
                            } else if this.shortcuts.target.is_none() {
                                close_settings_page_for(Some(cx.entity_id()), window, cx);
                            }
                        },
                    ))
                    .capture_action(
                        cx.listener(|this, _: &gpui_component::input::Enter, _, cx| {
                            let query = this.search_query(cx);
                            if let Some(&tab) = search::search_tabs(&query)
                                .first()
                                .filter(|_| !query.trim().is_empty())
                            {
                                cx.stop_propagation();
                                this.set_tab(tab, cx);
                            }
                        }),
                    )
                    .child(
                        gpui_component::input::Input::new(&search_input)
                            .small()
                            .prefix(Icon::new(IconName::Search).xsmall())
                            .suffix(if has_search_query {
                                crate::ui::button_with_tooltip(
                                    Button::new("admin-search-clear")
                                        .icon(IconName::Close)
                                        .ghost()
                                        .xsmall()
                                        .debug_selector(|| "admin-search-clear".into())
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.clear_search(window, cx);
                                        })),
                                    "admin-search-clear",
                                    text::SEARCH_CLEAR.get(),
                                )
                                .into_any_element()
                            } else {
                                div()
                                    .id("admin-search-hint")
                                    .debug_selector(|| "admin-search-hint".into())
                                    .px_1()
                                    .rounded(crate::ui::RADIUS_XS)
                                    .bg(p.muted)
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_2XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::SEARCH_HINT_CMD_F.get())
                                    .into_any_element()
                            }),
                    ),
            );
        if matches.is_empty() {
            nav = nav.child(
                div()
                    .px_2()
                    .debug_selector(|| "admin-search-empty".into())
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::SEARCH_EMPTY.get()),
            );
        }

        for &(group_label, tabs) in groups {
            let tabs: Vec<AdminTab> = tabs
                .iter()
                .copied()
                .filter(|t| matches.contains(t))
                .collect();
            if tabs.is_empty() {
                continue;
            }
            let mut group_col = div().flex_none().flex().flex_col().gap_1().child(
                div()
                    .px_2()
                    .pt_1()
                    .text_size(crate::ui::TEXT_XS)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(p.muted_foreground)
                    .child(group_label),
            );
            for tab in tabs {
                let selected = tab == current_tab;
                group_col = group_col.child(
                    settings_nav_tab(tab, selected)
                        .focus_visible(|style| style.border_color(p.ring))
                        .min_h(px(30.))
                        .flex_none()
                        .py_1()
                        .px_2()
                        .rounded(crate::ui::RADIUS_SM)
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(crate::ui::TEXT_SM)
                        .text_color(if selected {
                            p.foreground
                        } else {
                            p.muted_foreground
                        })
                        .when(selected, |d| {
                            d.bg(p.sidebar_accent).font_weight(gpui::FontWeight::MEDIUM)
                        })
                        .hover(|d| d.bg(p.sidebar_accent).text_color(p.foreground))
                        .active(|d| d.bg(p.accent).text_color(p.foreground))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_tab(tab, cx);
                        }))
                        .child(tab.icon_element().small())
                        .child(div().flex_1().min_w_0().truncate().child(tab.label()))
                        .children(tab.badge().map(|badge_text| {
                            div()
                                .flex_none()
                                .px_1p5()
                                .py(px(1.))
                                .rounded(crate::ui::RADIUS_XS)
                                .bg(p.muted)
                                .border_1()
                                .border_color(p.border)
                                .text_size(crate::ui::TEXT_2XS)
                                .font_weight(gpui::FontWeight::NORMAL)
                                .text_color(p.muted_foreground)
                                .child(badge_text)
                        })),
                );
            }
            nav = nav.child(group_col);
        }

        // 투명 제목 표시줄 위에 그리는 테마 색 제목 띠(신호등 버튼 자리 포함, 끌어서 창 이동).
        let titlebar = div()
            .id("admin-titlebar")
            .debug_selector(|| "admin-titlebar".into())
            .h(crate::ui::TITLEBAR_HEIGHT)
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .bg(p.worktree_sidebar)
            .border_b_1()
            .border_color(p.border)
            .window_control_area(gpui::WindowControlArea::Drag)
            .text_size(crate::ui::TEXT_XS)
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(p.muted_foreground)
            .child(text::TITLE.get());

        self.search_focus.rendered.set(false);
        let content_column = if matches.is_empty() {
            div()
                .id("admin-content-column")
                .debug_selector(|| "admin-content-column".into())
                .max_w(crate::ui::SETTINGS_CONTENT_MAX_WIDTH)
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    layout::card(p)
                        .id("admin-content-search-empty")
                        .debug_selector(|| "admin-content-search-empty".into())
                        .py_8()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(
                            Icon::new(IconName::Search)
                                .small()
                                .text_color(p.muted_foreground),
                        )
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_SM)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(p.foreground)
                                .child(text::SEARCH_EMPTY_TITLE.get()),
                        )
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .text_center()
                                .child(
                                    text::SEARCH_EMPTY_DESC
                                        .format(&[("query", search_value.as_str())]),
                                ),
                        )
                        .child(
                            layout::secondary(
                                "admin-content-search-clear",
                                text::SEARCH_CLEAR.get(),
                            )
                            .debug_selector(|| "admin-content-search-clear".into())
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.clear_search(window, cx);
                                },
                            )),
                        ),
                )
        } else {
            let page_header = Self::render_page_header(current_tab, None, cx);
            let body = match current_tab {
                AdminTab::General => self.render_general_tab(&s, cx),
                AdminTab::Terminal => self.render_terminal_tab(&s, window, cx),
                AdminTab::Accounts => self.render_accounts_tab(window, cx),
                AdminTab::Agents => self.render_agents_tab(&s, window, cx),
                AdminTab::Install => self.render_install_tab(window, cx),
                AdminTab::Ssh => self.render_ssh_tab(window, cx),
                AdminTab::RemoteKrill => self.render_remote_krill_tab(window, cx),
                AdminTab::Workflows => self.render_workflows_tab(&s, window, cx),
                // OWE-799: 단축키 탭은 카테고리 그룹·로컬 검색·인라인 기록을 위해 window를 받는다.
                AdminTab::Shortcuts => self.render_shortcuts_tab(window, cx),
                AdminTab::Resources => self.render_resources_tab(&s, cx),
                AdminTab::Mobile => self.render_mobile_tab(window, cx),
            }
            .w_full()
            .min_w_0()
            .id("admin-tab-body")
            .when(
                !self.search_focus.rendered.get() && self.search_pending,
                |body| body.anchor_scroll(Some(self.search_focus.anchor.clone())),
            );

            div()
                .id("admin-content-column")
                .debug_selector(|| "admin-content-column".into())
                // OWE-719·OWE-793·OWE-795: 왼쪽 정렬 + 창 폭에 맞춰 유동 확장하되 초대형 화면에서 과도한 행 길이만 제한.
                .max_w(crate::ui::SETTINGS_CONTENT_MAX_WIDTH)
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_4()
                .child(page_header)
                .child(body)
                .children(error.map(|e| {
                    div()
                        .p_2()
                        .rounded(crate::ui::RADIUS_SM)
                        .bg(p.warning)
                        .text_color(p.warning_foreground)
                        .font_family(crate::ui::FONT_UI)
                        .text_size(crate::ui::TEXT_XS)
                        .child(crate::i18n::cached_notice(&e))
                }))
        };

        if self.search_pending {
            self.search_pending = false;
            if self.search_focus.rendered.get() {
                window.focus(&self.search_focus.focus, cx);
            } else {
                window.focus(&self.focus, cx);
            }
            self.search_focus.anchor.scroll_to(window, cx);
            window.on_next_frame(|window, _| window.refresh());
        }
        div()
            .id("admin-center")
            .debug_selector(|| "admin-center".into())
            .key_context(SEARCH_CONTEXT)
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Escape, window, cx| {
                    if crate::orca_import::state(cx).is_some_and(|st| st.modal_open) {
                        crate::orca_import::close_modal(cx);
                        cx.stop_propagation();
                        return;
                    }
                    if this.schedule_modal.is_some() {
                        this.close_schedule_modal(cx);
                        cx.stop_propagation();
                        return;
                    }
                    if this.removing.take().is_some() {
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    if this.space.cleanup_open {
                        this.close_cleanup_modal(cx);
                        cx.stop_propagation();
                        return;
                    }
                    if this.armed.is_some() {
                        this.disarm(cx);
                        cx.stop_propagation();
                        return;
                    }
                    let handled = if this.rename.take().is_some() {
                        if let Some(focus) = this.rename_return_focus.take() {
                            window.focus(&focus, cx);
                        }
                        true
                    } else if this.cancel_shortcut_conflict(window, cx) {
                        true
                    } else if this.ssh_keygen_confirm_host.take().is_some() {
                        window.focus(&this.focus, cx);
                        true
                    } else if this.editing_ssh_host_id().is_some() {
                        this.cancel_edit_ssh_host(window, cx);
                        window.focus(&this.focus, cx);
                        true
                    } else {
                        false
                    };
                    if handled {
                        cx.stop_propagation();
                        cx.notify();
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.focus_search(window, cx);
            }))
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" {
                    if this.rename.take().is_some() {
                        if let Some(focus) = this.rename_return_focus.take() {
                            window.focus(&focus, cx);
                        }
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    if this.cancel_shortcut_conflict(window, cx) {
                        cx.stop_propagation();
                        return;
                    }
                    if this.ssh_keygen_confirm_host.take().is_some() {
                        window.focus(&this.focus, cx);
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    if this.editing_ssh_host_id().is_some() {
                        this.cancel_edit_ssh_host(window, cx);
                        window.focus(&this.focus, cx);
                        cx.stop_propagation();
                        return;
                    }
                    if this.workspace_trust_confirm_pending {
                        this.workspace_trust_confirm_pending = false;
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                }
                if ev.keystroke.key == "escape"
                    && crate::orca_import::state(cx).is_some_and(|st| st.modal_open)
                {
                    if !crate::orca_import::state(cx).is_some_and(|st| st.importing) {
                        crate::orca_import::close_modal(cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                if this.schedule_modal.is_some() && ev.keystroke.key == "escape" {
                    this.close_schedule_modal(cx);
                    window.focus(&this.focus, cx);
                    cx.stop_propagation();
                    return;
                }
                // 계정 제거 확인 대화상자가 열려 있으면 Esc는 그것만 닫는다(OWE-719).
                if this.removing.is_some() && ev.keystroke.key == "escape" {
                    this.removing = None;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if this.space.cleanup_open && ev.keystroke.key == "escape" {
                    this.close_cleanup_modal(cx);
                    cx.stop_propagation();
                    return;
                }
                // 포커스 이동(Tab)·Esc는 무장한 파괴적 버튼을 먼저 푼다(Esc는 창을 닫지 않고 해제만).
                if this.armed.is_some() && matches!(ev.keystroke.key.as_str(), "tab" | "escape") {
                    this.disarm(cx);
                    if ev.keystroke.key == "escape" {
                        cx.stop_propagation();
                        return;
                    }
                }
                if ev.keystroke.key == "escape"
                    && !ev.keystroke.modifiers.modified()
                    && cx
                        .try_global::<crate::updater::UpdateState>()
                        .is_some_and(|u| u.staged.is_some() && !u.card_collapsed)
                {
                    let mut u = cx
                        .try_global::<crate::updater::UpdateState>()
                        .cloned()
                        .unwrap_or_default();
                    u.card_collapsed = true;
                    cx.set_global(u);
                    cx.stop_propagation();
                    cx.refresh_windows();
                    cx.notify();
                    return;
                }
                if ev.keystroke.key == "escape"
                    && !ev.keystroke.modifiers.modified()
                    && this.tab == AdminTab::Shortcuts
                    && !this.shortcuts.search_query(cx).is_empty()
                    && this.shortcuts.target.is_none()
                {
                    this.shortcuts.clear_search(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                if ev.keystroke.key == "escape"
                    && !ev.keystroke.modifiers.modified()
                    && this.search_query(cx).is_empty()
                    && this.shortcuts.target.is_none()
                {
                    cx.stop_propagation();
                    if this.search_query(cx).is_empty() {
                        close_settings_page_for(Some(cx.entity_id()), window, cx);
                    } else {
                        this.clear_search(window, cx);
                    }
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(p.background)
            .text_color(p.foreground)
            .font_family(crate::ui::FONT_UI)
            .text_size(crate::ui::TEXT_MD)
            .children(self.render_remove_dialog(window, cx))
            .children(self.render_cleanup_modal(window, cx))
            .children(self.render_schedule_modal(window, cx))
            .children(crate::orca_import::render_modal(window, cx))
            .child(titlebar)
            .child(
                div().flex_1().min_h_0().w_full().flex().child(nav).child(
                    div().flex_1().min_w_0().h_full().flex().flex_col().child(
                        div()
                            // 탭별 스크롤 id: 탭 전환 시 스크롤 위치를 최상단으로 되돌린다(OWE-858).
                            .id(("admin-content", current_tab as usize))
                            .debug_selector(|| "admin-content".into())
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.search_focus.scroll)
                            .px_8()
                            .py_6()
                            .child(content_column),
                    ),
                ),
            )
            .into_any_element()
    }
}

pub(crate) mod child;
pub mod install;
pub(crate) mod layout;
mod mobile;
pub mod schedules;
mod search;
mod shortcuts;
pub mod space;
#[cfg(test)]
#[cfg(unix)]
mod tests;

pub fn usage_color(pct: u8, normal: gpui::Hsla, p: &crate::ui::Palette) -> gpui::Hsla {
    match usage_tone(pct) {
        UsageTone::Critical => p.destructive,
        UsageTone::Warn => p.warning,
        UsageTone::Normal => normal,
    }
}

/// 사용률 단계. 색 판정과 테스트가 공유한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageTone {
    Normal,
    Warn,
    Critical,
}

#[must_use]
pub fn usage_tone(pct: u8) -> UsageTone {
    if pct >= krill_proto::usage::CRITICAL_PERCENT {
        UsageTone::Critical
    } else if pct >= krill_proto::usage::WARN_PERCENT {
        UsageTone::Warn
    } else {
        UsageTone::Normal
    }
}
