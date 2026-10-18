//! 앱 설정(MK-070, MK-044): `settings.toml`(테마·UI 줌·알림·잠자기 방지)과 `keys.toml`(키 바인딩 재정의). 둘 다
//! 상태 디렉터리(`krill_proto::daemon::state_dir`)에 있고, 1초마다 수정 시각을 보고 바뀌면 재시작 없이 다시 적용한다.
//! 파일이 틀리면 직전 설정을 유지하고 오류를 설정 창에 보인다. 설정 창(⌘,)은 주요 항목을 고치고 파일 편집은 시스템 편집기에 맡긴다.
//!
//! 라이트/다크는 터미널 팔레트도 바꾸며 셸이 krilld에 전달한다(OWE-590). 줌은 rem 기반 UI 크기에 적용한다.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui::{
    App, AppContext as _, Bounds, Context, Global, KeyBinding, Keystroke, Render, SharedString,
    Subscription, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, ThemeMode};
use serde::{Deserialize, Serialize};

pub use crate::i18n::Language;

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

/// 테마 선택. 기본값은 Orca와 동일한 `Dark`이며, `System`은 적용 시점의 macOS 모양을 따른다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    System,
    Light,
    #[default]
    Dark,
}

/// 잠자기 방지 모드(MK-044, `IOPMAssertion` On/Agent/Off).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepAwake {
    /// 앱이 켜져 있는 동안 항상 유휴 잠자기 방지.
    On,
    /// `working` 상태인 세션이 하나라도 있을 때만 유휴 잠자기 방지(기본값).
    #[default]
    Agent,
    /// 잠자기 방지를 쓰지 않음.
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
    pub language: Language,
    /// UI 배율(1.0 = 100%). 적용할 때 `ZOOM_MIN..=ZOOM_MAX`로 자른다.
    pub zoom: f32,
    /// 에이전트 입력 대기·완료 알림과 Dock 배지 켜기/끄기(MK-044).
    pub notifications: bool,
    /// 잠자기 방지 모드(MK-044: `on` / `agent` / `off`).
    #[serde(alias = "awake")]
    pub keep_awake: KeepAwake,
    /// 완료 웹훅 opt-in. 기본은 빈 목록이며 URL에 비밀 토큰이 들어갈 수 있다.
    pub webhooks: Vec<crate::webhook::Endpoint>,
    /// Copy Context의 최근 출력 행 수(1..=1000으로 제한).
    pub copy_context_lines: u16,
    /// 저장 명령·프롬프트(MK-092). 클릭하면 실행 키 없이 붙여넣는다.
    pub quick_commands: Vec<crate::commands::QuickCommand>,
    /// 터미널 글꼴 크기(10..=24pt, 기본 13).
    #[serde(default = "default_terminal_font_size")]
    pub terminal_font_size: u8,
    /// 터미널 배경 불투명도(50..=100%, 기본 100).
    #[serde(default = "default_terminal_opacity_pct")]
    pub terminal_opacity_pct: u8,
    /// 터미널 글꼴 패밀리(기본 Menlo, OWE-658). 없는 이름이면 gpui가 대체 글꼴을 쓴다.
    #[serde(default = "default_terminal_font_family")]
    pub terminal_font_family: String,
    /// 프로그램이 커서 모양을 지정하지 않았을 때의 기본 커서(기본 블록, OWE-658).
    #[serde(default)]
    pub terminal_cursor_shape: TerminalCursorShape,
    /// 기본 커서 깜박임(기본 끔, OWE-658). 프로그램이 지정하지 않았을 때만 쓴다.
    #[serde(default)]
    pub terminal_cursor_blink: bool,
    /// Option+글자 키를 ESC+글자(Meta)로 보낸다(기본 끔, OWE-658).
    #[serde(default)]
    pub option_as_meta: bool,
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
    /// 터미널 벨 알림(기본 켬, OWE-659).
    #[serde(default = "default_true")]
    pub notify_on_bell: bool,
    /// 기본 에이전트 ID(기본 `"claude"`).
    #[serde(default = "default_agent_id")]
    pub default_agent: String,
    /// 새 에이전트 세션 YOLO 권한 우회 모드(기본 false).
    #[serde(default)]
    pub yolo_mode: bool,
    /// 유휴 세션 하이버네이션 opt-in(기본 false, docs/agents/hibernation.md).
    #[serde(default)]
    pub hibernation_enabled: bool,
    /// 히스토리 세그먼트 기록 일시중지(기본 false).
    #[serde(default)]
    pub recording_paused: bool,
    /// 보안 키보드 입력(Secure Keyboard Entry, 기본 false).
    #[serde(default)]
    pub secure_keyboard_entry: bool,
    /// 새 버전 스테이징 완료 시 자동으로 앱을 재시작해 적용(기본 false, MK-156).
    #[serde(default)]
    pub auto_restart_update: bool,
    /// Claude 프롬프트 캐시 카운트다운 표시 켜기/끄기(기본 true, OWE-737).
    #[serde(default = "default_true")]
    pub prompt_cache_timer_enabled: bool,
    /// Claude 프롬프트 캐시 TTL(초, 기본 300초 = 5분, OWE-737).
    #[serde(default = "default_prompt_cache_ttl_secs")]
    pub prompt_cache_ttl_secs: u32,
    /// 첫 프롬프트 기반 브랜치·세션 이름 자동 변경 켜기/끄기(기본 true, OWE-737).
    #[serde(default = "default_true")]
    pub auto_rename_branch_from_work: bool,
    /// 워크스페이스 신뢰 프리셋 자동 등록 켜기/끄기(기본 false, 동의 후 활성화, OWE-737).
    pub workspace_trust_preset_enabled: bool,
}

const fn default_terminal_font_size() -> u8 {
    13
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

fn default_agent_id() -> String {
    "claude".to_owned()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            language: Language::Ko,
            zoom: 1.0,
            notifications: true,
            keep_awake: KeepAwake::Agent,
            webhooks: Vec::new(),
            copy_context_lines: 100,
            quick_commands: Vec::new(),
            terminal_font_size: 13,
            terminal_opacity_pct: 100,
            terminal_font_family: default_terminal_font_family(),
            terminal_cursor_shape: TerminalCursorShape::Block,
            terminal_cursor_blink: false,
            option_as_meta: false,
            copy_on_select: false,
            notification_sound: true,
            notify_only_inactive: false,
            notify_on_input: true,
            notify_on_done: true,
            notify_on_bell: true,
            default_agent: "claude".to_owned(),
            yolo_mode: false,
            hibernation_enabled: false,
            recording_paused: false,
            secure_keyboard_entry: false,
            auto_restart_update: false,
            prompt_cache_timer_enabled: true,
            prompt_cache_ttl_secs: DEFAULT_PROMPT_CACHE_TTL_SECS,
            auto_rename_branch_from_work: true,
            workspace_trust_preset_enabled: false,
        }
    }
}

impl Settings {
    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        let mut parsed: Self = toml::from_str(s)?;
        parsed.terminal_font_size = parsed.terminal_font_size.clamp(10, 24);
        parsed.terminal_opacity_pct = parsed.terminal_opacity_pct.clamp(50, 100);
        parsed.prompt_cache_ttl_secs = parsed.prompt_cache_ttl_secs.clamp(60, 86_400);
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
        self.terminal_font_size.clamp(10, 24)
    }

    /// 자른 터미널 배경 불투명도(`50..=100`).
    pub fn terminal_opacity_pct(&self) -> u8 {
        self.terminal_opacity_pct.clamp(50, 100)
    }

    /// 자른 프롬프트 캐시 TTL(초, `60..=86400`).
    pub fn prompt_cache_ttl_secs(&self) -> u32 {
        self.prompt_cache_ttl_secs.clamp(60, 86_400)
    }
}

/// `keys.toml`: 키 → 액션 이름. 빈 이름은 기본 바인딩 끄기.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub bindings: toml::Table,
}

impl Keys {
    pub fn parse(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

/// 기본 바인딩(`defaults`) 위에 `keys`를 얹은 최종 바인딩. 재정의는 같은 액션의 기본 바인딩과 같은 문맥에
/// 걸어(깊이가 같아야 나중 것이 이긴다) 기본 키를 대신한다. 빈 이름은 그 키의 기본 바인딩을 뺀다.
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
    let mut user = Vec::new();
    for (key, action) in &keys.bindings {
        let Some(name) = action.as_str() else {
            return Err(text::KEY_VALUE.format(&[("file", KEYS_FILE), ("key", key)]));
        };
        let typed = key
            .split_whitespace()
            .map(Keystroke::parse)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("{KEYS_FILE}: \"{key}\": {e}"))?;
        if typed.is_empty() {
            return Err(text::EMPTY_KEY.format(&[("file", KEYS_FILE)]));
        }
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
    /// 관리자 YOLO 토글이 요청한 새 세션 권한(메인 셸 창이 꺼내 데몬에 적용).
    pending_permission: Option<krill_proto::agent::Permission>,
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
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-=", ZoomIn, None),
        KeyBinding::new("cmd--", ZoomOut, None),
        KeyBinding::new("cmd-0", ZoomReset, None),
        KeyBinding::new("cmd-f", FocusSearch, Some(SEARCH_CONTEXT)),
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
    let stamps = cx
        .try_global::<Config>()
        .and_then(|c| c.dir.as_deref().map(file_stamps));
    if cx.has_global::<Config>()
        && let Some(stamps) = stamps
    {
        cx.global_mut::<Config>().stamp = stamps;
    }
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
    let cmd = cmd.into().trim().to_owned();
    if cmd.is_empty() {
        return;
    }
    if !cx.has_global::<Config>() {
        cx.set_global(Config::default());
    }
    cx.global_mut::<Config>()
        .pending_terminal_commands
        .push_back(cmd);
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

/// 설정을 읽어 적용하고, 1초마다 파일 변경을 보는 태스크를 띄운다. 앱 기본 키를 모두 묶은 뒤에
/// 부른다(그때의 키맵이 재정의의 바탕이 된다). 첫 창 전에도 언어를 적용하고, 줌은 열린 창에 적용한다.
pub fn init(cx: &mut App) {
    init_at(krill_proto::daemon::state_dir(), cx);
    cx.on_action(|_: &OpenSettings, cx| open_window(cx));
    cx.on_action(|_: &ZoomIn, cx| edit(cx, |s| s.zoom = s.zoom_step(1)));
    cx.on_action(|_: &ZoomOut, cx| edit(cx, |s| s.zoom = s.zoom_step(-1)));
    cx.on_action(|_: &ZoomReset, cx| edit(cx, |s| s.zoom = 1.0));
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
        settings: Settings::default(),
        error: None,
        dir,
        defaults,
        stamp: [None; 7],
        pending_ssh_connect: None,
        pending_terminal_commands: std::collections::VecDeque::new(),
        pending_permission: None,
    });
    reload(cx);
}

fn reload_if_changed(cx: &mut App) {
    let c = cx.global::<Config>();
    let Some(dir) = c.dir.as_deref() else {
        return;
    };
    let now = file_stamps(dir);
    if now != c.stamp {
        reload(cx);
    }
}

/// 설정·키맵·계정·호스트 파일을 다시 읽어 적용한다. 없는 파일은 기본값, 틀린 파일은 그 부분만 직전 값을 유지한다.
pub fn reload(cx: &mut App) {
    let c = cx.global::<Config>();
    let (dir, defaults, mut settings) = (c.dir.clone(), c.defaults.clone(), c.settings.clone());
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
                Ok(s) => settings = s,
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
    crate::i18n::set_language(settings.language, cx);
    match keys.map(|k| bindings(defaults, &k, cx)) {
        Some(Ok(b)) => {
            cx.clear_key_bindings();
            cx.bind_keys(b);
        }
        Some(Err(e)) => errors.push(e),
        None => {}
    }
    if !errors.is_empty() {
        tracing::warn!(count = errors.len(), "settings reload failed");
    }
    // 사람이 직접 만든 opt-in 표식은 설정값보다 우선한다(읽기 경로에서 지우지 않는다).
    settings.hibernation_enabled |= dir
        .as_deref()
        .is_some_and(|d| d.join(HIBERNATION_FLAG).is_file());
    let c = cx.global_mut::<Config>();
    let enabled = settings.hibernation_enabled;
    let auto_rename = settings.auto_rename_branch_from_work;
    let workspace_trust = settings.workspace_trust_preset_enabled;
    c.settings = settings;
    c.stamp = stamp;
    c.error = (!errors.is_empty()).then(|| errors.join("\n").into());
    if !settings_read_failed {
        sync_hibernation_file(dir.as_deref(), enabled);
        sync_auto_rename_file(dir.as_deref(), auto_rename);
        sync_workspace_trust_file(dir.as_deref(), workspace_trust);
    }
    crate::alert::sync_settings(cx);
    cx.defer(apply);
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

/// 터미널 글꼴 패밀리(OWE-658). 비었거나 `init` 전이면 Menlo.
pub fn terminal_font_family(cx: &App) -> SharedString {
    cx.try_global::<Config>()
        .map(|c| c.settings.terminal_font_family.trim())
        .filter(|f| !f.is_empty())
        .map_or(SharedString::new_static(crate::ui::FONT_MONO), |f| {
            SharedString::from(f.to_owned())
        })
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
    cx.try_global::<Config>()
        .is_some_and(|c| c.settings.option_as_meta)
}

/// 선택을 마치면 바로 복사할지(OWE-658). `init` 전이면 false.
pub fn copy_on_select(cx: &App) -> bool {
    cx.try_global::<Config>()
        .is_some_and(|c| c.settings.copy_on_select)
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
    if matches!(state.status, CheckStatus::Checking) {
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
            match res {
                Ok(CheckStatus::Ready(staged)) => {
                    if current(cx).auto_restart_update && staged.spawn_relauncher().is_ok() {
                        cx.quit();
                        return;
                    }
                    next.staged = Some(staged.clone());
                    next.status = CheckStatus::Ready(staged);
                    next.card_collapsed = false;
                }
                Ok(status) => {
                    next.status = status;
                }
                Err(err) => {
                    next.status = CheckStatus::Error(err.to_string());
                }
            }
            cx.set_global(next);
            cx.refresh_windows();
        });
    })
    .detach();
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

/// 시스템 외관 변경은 System 선택일 때 외관을 따르고, Dark 선택일 때 다크 모드를 유지한다.
pub fn system_appearance_changed(appearance: gpui::WindowAppearance, cx: &mut App) {
    match current(cx).theme {
        ThemeChoice::System => cx.defer(move |cx| crate::ui::apply(appearance.into(), cx)),
        ThemeChoice::Dark => cx.defer(|cx| crate::ui::apply(ThemeMode::Dark, cx)),
        ThemeChoice::Light => {}
    }
}

/// 현재 설정을 테마와 열린 모든 창의 rem 크기에 반영한다. 액션·클릭 처리 중에는 그 창이 빌려져 있어
/// `update`가 실패하므로 호출자는 `cx.defer`로 부른다.
fn apply(cx: &mut App) {
    crate::alert::sync_settings(cx);
    let s = current(cx);
    crate::i18n::set_language(s.language, cx);
    crate::menu_bar::refresh_language(cx);
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
fn edit(cx: &mut App, f: impl FnOnce(&mut Settings)) {
    let c = cx.global::<Config>();
    let (mut settings, dir) = (c.settings.clone(), c.dir.clone());
    let Some(dir) = dir else {
        cx.global_mut::<Config>().error = Some(text::NO_STATE_DIR.get().into());
        return;
    };
    let path = dir.join(SETTINGS_FILE);
    let base = match std::fs::read_to_string(&path) {
        Ok(body) => {
            let Ok(disk) = Settings::parse(&body) else {
                cx.global_mut::<Config>().error =
                    Some(format!("{SETTINGS_FILE}: {}", text::INVALID_SETTINGS.get()).into());
                return;
            };
            settings = disk;
            Some(body)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            cx.global_mut::<Config>().error = Some(format!("{SETTINGS_FILE}: {e}").into());
            return;
        }
    };
    f(&mut settings);
    match save_settings(&path, &settings, base.as_deref()) {
        Ok(()) => {
            crate::i18n::set_language(settings.language, cx);
            let enabled = settings.hibernation_enabled;
            let auto_rename = settings.auto_rename_branch_from_work;
            let workspace_trust = settings.workspace_trust_preset_enabled;
            let c = cx.global_mut::<Config>();
            c.settings = settings;
            c.stamp[0] = modified(&path);
            sync_hibernation_file(Some(&dir), enabled);
            sync_auto_rename_file(Some(&dir), auto_rename);
            sync_workspace_trust_file(Some(&dir), workspace_trust);
            cx.defer(apply);
        }
        Err(e) => cx.global_mut::<Config>().error = Some(format!("{SETTINGS_FILE}: {e}").into()),
    }
}

/// 기본 에이전트를 저장한다(온보딩 첫 화면, OWE-714). 설정 초기화 전이거나 같으면 쓰지 않는다.
pub fn set_default_agent(id: &str, cx: &mut App) {
    if cx.has_global::<Config>() && current(cx).default_agent != id {
        edit(cx, |s| id.clone_into(&mut s.default_agent));
    }
}

/// Select the UI language, apply it to open windows, and persist it in settings.toml.
pub fn set_language(language: Language, cx: &mut App) {
    let previous = current(cx).language;
    edit(cx, |settings| settings.language = language);
    if current(cx).language != previous {
        reload(cx);
    }
}

/// 손상 파일을 덮어쓰지 않는다. 완성된 임시 파일만 같은 디렉터리에서 원자 교체한다.
///
/// `base`는 [`edit`]이 읽은 디스크 원문이다. 저장 직전 다시 읽은 원문과 다르면
/// 외부 편집이 낀 것이므로 덮어쓰지 않고 오류를 낸다(AS-07 lost-update 방지).
fn save_settings(path: &Path, settings: &Settings, base: Option<&str>) -> std::io::Result<()> {
    use std::io::{self, Write as _};
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    let body = match std::fs::read_to_string(path) {
        Ok(body) => {
            Settings::parse(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if base != Some(body.as_str()) {
                return Err(io::Error::other(text::SETTINGS_CONFLICT.get()));
            }
            krill_proto::config_document::edit_toml_aliases(
                &body,
                settings,
                &[("awake", "keep_awake")],
            )?
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
    if !path.exists() {
        std::fs::create_dir_all(&dir).ok();
        if name == SETTINGS_FILE {
            write_settings(&path, template).ok();
        } else if name == krill_proto::agent::FILE_NAME {
            // 프로필 env에 비밀값(API 키 등)이 들어갈 수 있어 0600 원자 쓰기로 만든다.
            write_settings(&path, template).ok();
        } else {
            std::fs::write(&path, template).ok();
        }
    }
    cx.open_with_system(&path);
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
    let mut i = handlers.len();
    while i > 0 {
        i -= 1;
        match handlers[i](cmd, cx) {
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
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(text::TITLE.get().into()),
            appears_transparent: true,
            traffic_light_position: Some(crate::ui::TRAFFIC_LIGHT_INSET),
        }),
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
    Ssh,
    Workflows,
    Shortcuts,
    Resources,
    Mobile,
}

// 기본 번들 밖 탭 아이콘. 지정한 SVG만 바이너리에 싣는다.
gpui_kit_assets::icon_assets!(TabIcons, [Keyboard, Server, Smartphone]);

impl AdminTab {
    pub const ALL: [Self; 9] = [
        Self::General,
        Self::Terminal,
        Self::Accounts,
        Self::Agents,
        Self::Ssh,
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
            Self::Ssh => "admin-tab-ssh",
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
            Self::Ssh => text::TAB_SSH.get(),
            Self::Workflows => text::TAB_WORKFLOWS.get(),
            Self::Shortcuts => text::TAB_SHORTCUTS.get(),
            Self::Resources => text::TAB_RESOURCES.get(),
            Self::Mobile => crate::i18n::catalog::mobile::TAB.get(),
        }
    }

    /// 탭 아이콘. 앱 자산(`gpui_kit_assets`)의 전체 Lucide 집합에서 고른다.
    pub fn icon(self) -> gpui_kit_assets::IconName {
        use gpui_kit_assets::IconName;
        match self {
            Self::General => IconName::Settings,
            Self::Terminal => IconName::SquareTerminal,
            Self::Accounts => IconName::User,
            Self::Agents => IconName::Bot,
            Self::Ssh => IconName::Server,
            Self::Workflows => IconName::Cpu,
            Self::Shortcuts => IconName::Keyboard,
            Self::Resources => IconName::CircleCheck,
            Self::Mobile => IconName::Smartphone,
        }
    }

    /// 그릴 탭 아이콘. `Keyboard`·`Server`·`Smartphone`은 기본 번들(`gpui_kit_assets::Assets`)에 없어 경로로 그리면
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
    let metric = if rule.metric == krill_proto::usage::AutoSwitchMetric::Max {
        "Max"
    } else {
        rule.metric.label()
    };
    let mut out = format!("{metric} ≥ {}% → {target}", rule.threshold_percent);
    if !rule.enabled {
        out.push_str(" · ");
        out.push_str(text::OFF.get());
    }
    out
}

/// 사용률 색(OWE-708). 상태바·계정 카드가 같은 기준(경고 [`WARN_PERCENT`]·위험 [`CRITICAL_PERCENT`])을 쓴다.
///
/// [`WARN_PERCENT`]: krill_proto::usage::WARN_PERCENT
/// [`CRITICAL_PERCENT`]: krill_proto::usage::CRITICAL_PERCENT
#[must_use]
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
            home.join(".config")
                .join("opencode")
                .join("plugins")
                .join("minikrill.js"),
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
    let staged = home.join(".local/share/minikrill/bin/krill");
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
    let dirs = [
        home.join(".claude/skills"),
        home.join(".codex/skills"),
        home.join(".gemini/config/skills"),
        home.join(".cursor/skills"),
        home.join(".config/opencode/skills"),
    ];
    let mut installed = Vec::new();
    for skill in krill_proto::orchestration::builtin_skills() {
        for dir in &dirs {
            if let Ok(path) = krill_proto::orchestration::install_skill(skill.name, dir) {
                installed.push(path.display().to_string());
            }
        }
    }
    installed
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
#[must_use]
pub fn agent_install_command(agent_id: &str) -> Option<String> {
    let id = krill_proto::agent::canonical_id(agent_id);
    krill_proto::agent::Agents::load(None)
        .ok()
        .and_then(|agents| agents.profiles.get(id).map(|p| p.install.clone()))
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
pub fn run_doctor_check(state: Option<&Path>) -> String {
    let state_status = match state {
        Some(d) if d.exists() => "ok",
        Some(_) => "init",
        None => "missing",
    };
    let bins_status = match krill_proto::daemon::Paths::dev() {
        Ok(p) if p.holder.exists() && p.krilld.exists() => "ok",
        Ok(_) => "missing-bins",
        Err(_) => "unavailable",
    };
    let crashes = state.map_or(0, |d| krill_proto::diag::pending(d).len());
    let rec_warns = state.map_or(0, |d| krill_proto::diag::record_warnings(d).len());
    let rec_flag = state.is_some_and(|d| {
        d.join(krill_proto::history::RECORDING_WARNING_FILE)
            .exists()
    });
    let rec_status = if rec_flag || rec_warns > 0 {
        format!("warn({rec_warns})")
    } else {
        "ok".to_owned()
    };
    format!(
        "krill doctor: state={state_status} · bins={bins_status} · crashes={crashes} · recording={rec_status}"
    )
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
    mobile: mobile::Panel,
    accounts_snapshot: krill_proto::usage::AccountsSnapshot,
    account_error: Option<SharedString>,
    /// 마지막 계정 갱신 때 데몬에 닿지 못했다. 로컬 파일을 읽기 전용으로 보이고 변경은 막는다(OWE-697).
    accounts_offline: bool,
    /// 규칙 변경 응답 전에는 다음 변경을 보내지 않는다.
    account_rule_pending: bool,
    ssh_hosts: Vec<crate::ssh_hosts::SshHost>,
    ssh_host_statuses: std::collections::BTreeMap<String, SshHostCardStatus>,
    ssh_status: Option<SharedString>,
    ssh_form: Option<SshForm>,
    tailscale_peers: Vec<crate::ssh_hosts::TailscalePeer>,
    tailscale_loaded: bool,
    ssh_auth_failed_host: Option<crate::ssh_hosts::SshHost>,
    ssh_keygen_confirm_host: Option<crate::ssh_hosts::SshHost>,
    skill_status: Option<SharedString>,
    doctor_status: Option<SharedString>,
    history_status: Option<SharedString>,
    /// 기록 즉시 삭제 IPC 진행 중. UI 스레드를 막지 않고 백그라운드에서 돌린다(AS-01).
    history_clear_pending: bool,
    /// 계정·호스트 새로고침 진행 중. 관찰자·적용 경로의 동기 스캔을 하나로 합친다(AS-06).
    accounts_refresh_pending: bool,
    /// Agents 탭 렌더용 `agents.toml` 캐시((수정 시각, 본문), 새로고침 진행 중)와 디스크 읽기 횟수(AS-06).
    agents_file_cache: AgentsFileCache,
    agents_file_reads: u64,
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
    /// 사이드바 설정 검색 입력(OWE-656). 창이 필요해 처음 그릴 때 만든다.
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
    /// 스케줄 추가/편집 모달 상태(OWE-740).
    schedule_modal: Option<schedules::ScheduleModal>,
    /// 선택된 스케줄 실행 기록 로그 뷰어(OWE-740).
    schedule_log: Option<schedules::ScheduleLogView>,
    /// 스케줄 즉시 실행·조작 오류(OWE-740).
    schedule_error: Option<SharedString>,
    /// 설정 창 루트 포커스. 아무것도 포커스되지 않으면 ⌘F 문맥([`SEARCH_CONTEXT`])이 키 경로에 없다.
    focus: gpui::FocusHandle,
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

/// SSH 호스트 추가·편집 입력 폼 상태(MK-132·OWE-800).
struct SshForm {
    label: gpui::Entity<gpui_component::input::InputState>,
    address: gpui::Entity<gpui_component::input::InputState>,
    port: gpui::Entity<gpui_component::input::InputState>,
    identity_file: gpui::Entity<gpui_component::input::InputState>,
    editing_id: Option<String>,
    error: Option<SharedString>,
}

/// 워크플로 탭의 저장소별 공유 심볼릭 링크·setup/archive 훅 입력 폼(MK-174).
struct WorkflowRepoForm {
    repo_path: gpui::Entity<gpui_component::input::InputState>,
    symlink_path: gpui::Entity<gpui_component::input::InputState>,
    setup_script: gpui::Entity<gpui_component::input::InputState>,
    archive_script: gpui::Entity<gpui_component::input::InputState>,
    loaded_repo: Option<String>,
    loaded_setup: String,
    loaded_archive: String,
    status: Option<(SharedString, bool)>,
    _subs: Vec<Subscription>,
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
        let mut view = Self {
            _observe: cx.observe_global::<Config>(|this, cx| {
                this.refresh_accounts_async(cx);
            }),
            tab: AdminTab::General,
            mobile: mobile::Panel::default(),
            accounts_snapshot: krill_proto::usage::AccountsSnapshot::default(),
            account_error: None,
            accounts_offline: false,
            account_rule_pending: false,
            ssh_hosts: Vec::new(),
            ssh_host_statuses: std::collections::BTreeMap::new(),
            ssh_status: None,
            ssh_form: None,
            tailscale_peers: Vec::new(),
            tailscale_loaded: false,
            ssh_auth_failed_host: None,
            ssh_keygen_confirm_host: None,
            skill_status: None,
            doctor_status: None,
            history_status: None,
            history_clear_pending: false,
            accounts_refresh_pending: false,
            agents_file_cache: (None, false),
            agents_file_reads: 0,
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
            search: None,
            shortcuts: shortcuts::Recorder::default(),
            workflow_form: None,
            space: space::Panel::default(),
            schedule_modal: None,
            schedule_log: None,
            schedule_error: None,
            focus: cx.focus_handle(),
        };
        view.refresh_accounts_async(cx);
        // 점검 스냅샷이 바뀌면 일반 탭 요약도 다시 그린다(구독은 앱 수명 동안 둔다).
        cx.observe_global::<crate::onboarding::ChecksSnapshot>(|_, cx| cx.notify())
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

    pub fn tab(&self) -> AdminTab {
        self.tab
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

    pub fn set_tab(&mut self, tab: AdminTab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.disarm(cx);
        self.shortcuts = shortcuts::Recorder::default();
        if tab == AdminTab::Ssh && !self.tailscale_loaded {
            self.refresh_tailscale_peers(cx);
        }
        if tab == AdminTab::Resources && self.space.report.is_none() && !self.space.scanning {
            self.scan_workspace_space(cx);
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
        let dir = state_dir(cx);
        if is_daemon_state_dir(dir.as_deref()) {
            self.spawn_account_rule_request(
                async move { crate::accounts::upsert_auto_switch_rule(rule) },
                cx,
            );
            return;
        }
        let result = crate::accounts::request_local(
            &crate::accounts::default_home(),
            dir.as_deref(),
            &krill_proto::usage::UsageRequest::UpsertAutoSwitchRule(rule),
        )
        .map_err(|e| e.to_string());
        self.finish_account_rule_request(result, cx);
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
        let dir = state_dir(cx);
        if is_daemon_state_dir(dir.as_deref()) {
            let from_account_id = from_account_id.to_owned();
            self.spawn_account_rule_request(
                async move { crate::accounts::remove_auto_switch_rule(provider, &from_account_id) },
                cx,
            );
            return;
        }
        let result = crate::accounts::request_local(
            &crate::accounts::default_home(),
            dir.as_deref(),
            &krill_proto::usage::UsageRequest::RemoveAutoSwitchRule {
                provider,
                from_account_id: from_account_id.to_owned(),
            },
        )
        .map_err(|e| e.to_string());
        self.finish_account_rule_request(result, cx);
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
                self.accounts_snapshot = snap;
                self.account_error = None;
                cx.defer(notify_external_change);
            }
            Err(error) => self.account_error = Some(error.into()),
        }
        cx.notify();
    }

    /// 렌더는 메모리 캐시만 읽고, 수정 시각 확인·본문 읽기는 백그라운드로 보낸다(AS-06).
    fn cached_agents_file(&mut self, dir: Option<&Path>, cx: &mut Context<Self>) -> Option<String> {
        let cached = self
            .agents_file_cache
            .0
            .as_ref()
            .and_then(|(_, body)| body.clone());
        let Some(dir) = dir else { return cached };
        if self.agents_file_cache.1 {
            return cached;
        }
        self.agents_file_cache.1 = true;
        let path = dir.join(krill_proto::agent::FILE_NAME);
        let previous = self.agents_file_cache.0.as_ref().map(|(stamp, _)| *stamp);
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                    if previous == Some(stamp) {
                        None
                    } else {
                        Some((stamp, std::fs::read_to_string(path).ok()))
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.agents_file_cache.1 = false;
                if let Some(loaded) = loaded {
                    this.agents_file_reads += 1;
                    this.agents_file_cache.0 = Some(loaded);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cached
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
    fn refresh_accounts_async(&mut self, cx: &mut Context<Self>) {
        if self.accounts_refresh_pending {
            return;
        }
        self.accounts_refresh_pending = true;
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
                    (snapshot, hosts, problem, offline, orphans)
                })
                .await;
            this.update(cx, |this, cx| {
                this.accounts_refresh_pending = false;
                this.accounts_offline = loaded.3;
                this.orphan_profiles = loaded.4;
                if daemon && !loaded.3 {
                    this.probe_identities(cx);
                }
                // 시작 뒤 처리된 계정 요청의 성공·거부는 오래된 갱신보다 최신이다.
                if this.accounts_snapshot == before.0 && this.account_error == before.1 {
                    this.accounts_snapshot = loaded.0;
                    this.account_error = loaded.2.map(SharedString::from);
                }
                this.ssh_hosts = loaded.1;
                cx.notify();
            })
            .ok();
        })
        .detach();
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
                if let Some(snap) = snapshot {
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
        let dir = state_dir(cx);
        if !is_daemon_state_dir(dir.as_deref()) {
            let home = crate::accounts::default_home();
            crate::accounts::request_local(
                &home,
                dir.as_deref(),
                &krill_proto::usage::UsageRequest::PurgeOrphanProfiles,
            )
            .ok();
            self.orphan_profiles = crate::accounts::orphans_local(&home, dir.as_deref());
            cx.notify();
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

    /// 추가와 재로그인 모두 백그라운드 로그인 CLI를 실행한다. 앱 스레드에서는 IPC를 기다리지 않는다.
    fn login_account(
        &mut self,
        provider: krill_proto::usage::ProviderKind,
        account: Option<krill_proto::usage::AccountInfo>,
        cx: &mut Context<Self>,
    ) {
        use krill_proto::usage::UsageRequest;
        if self.account_rule_pending {
            return;
        }
        if !matches!(
            provider,
            krill_proto::usage::ProviderKind::Claude
                | krill_proto::usage::ProviderKind::Codex
                | krill_proto::usage::ProviderKind::Antigravity
        ) {
            if provider == krill_proto::usage::ProviderKind::OpenCode {
                queue_terminal_command("opencode auth login", cx);
            }
            self.account_error = Some(text::ACCOUNT_LOGIN_UNSUPPORTED.get().into());
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
                    crate::accounts::start_login(&paths, &account, newly_added)
                })
                .await;
            this.update(cx, |this, cx| {
                this.account_rule_pending = false;
                match result {
                    Ok(()) => {}
                    Err(e) => this.account_error = Some(e.into()),
                }
                this.refresh_accounts_async(cx);
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
        let dir = state_dir(cx);
        // 테스트용 임시 상태 디렉터리에서는 데몬 없이 로컬 파일로 동기 처리한다.
        if !is_daemon_state_dir(dir.as_deref()) {
            let home = crate::accounts::default_home();
            match crate::accounts::request_local(&home, dir.as_deref(), &req) {
                Ok(snap) => {
                    self.accounts_snapshot = snap;
                    self.account_error = None;
                    cx.defer(notify_external_change);
                }
                Err(e) => self.account_error = Some(e.to_string().into()),
            }
            cx.notify();
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

    pub fn import_ssh_hosts(&mut self, cx: &mut Context<Self>) {
        let dir = state_dir(cx);
        if let Ok(hosts) = crate::ssh_hosts::import_from_ssh_config(dir.as_deref(), None) {
            self.ssh_hosts = hosts;
            cx.defer(notify_external_change);
            cx.notify();
        }
    }

    pub fn remove_ssh_host(&mut self, id: &str, cx: &mut Context<Self>) {
        let dir = state_dir(cx);
        if let Ok(hosts) = crate::ssh_hosts::remove_host(dir.as_deref(), id) {
            self.ssh_hosts = hosts;
            self.ssh_host_statuses.remove(id);
            if let Some(form) = &mut self.ssh_form
                && form.editing_id.as_deref() == Some(id)
            {
                form.editing_id = None;
                form.error = None;
            }
            if self
                .ssh_auth_failed_host
                .as_ref()
                .is_some_and(|h| h.id == id)
            {
                self.ssh_auth_failed_host = None;
            }
            if self
                .ssh_keygen_confirm_host
                .as_ref()
                .is_some_and(|h| h.id == id)
            {
                self.ssh_keygen_confirm_host = None;
            }
            cx.defer(notify_external_change);
            cx.notify();
        }
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
        self.ssh_form = Some(SshForm {
            label,
            address,
            port,
            identity_file,
            editing_id: None,
            error: None,
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

    /// 입력값(이름·주소·포트·키 파일)을 검증해 `hosts.toml`에 저장한다. 실패 시 한국어/영어 안내 오류를 설정한다.
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
        let save_res = if let Some(id) = &editing_id {
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
        };
        match save_res {
            Ok(hosts) => {
                self.ssh_hosts = hosts;
                if let Some(form) = &mut self.ssh_form {
                    form.editing_id = None;
                    form.error = None;
                }
                let status_tmpl = if editing_id.is_some() {
                    text::SSH_UPDATED
                } else {
                    text::SSH_ADDED
                };
                self.ssh_status = Some(status_tmpl.format(&[("label", &valid.label)]).into());
                cx.defer(notify_external_change);
                cx.notify();
                Ok(())
            }
            Err(e) => {
                let msg: SharedString = e.to_string().into();
                if let Some(form) = &mut self.ssh_form {
                    form.error = Some(msg.clone());
                }
                self.ssh_status = Some(msg);
                cx.notify();
                Err(crate::ssh_hosts::HostInputError::InvalidAddress)
            }
        }
    }

    fn submit_ssh_host_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.ssh_form else {
            return;
        };
        let label_input = form.label.clone();
        let addr_input = form.address.clone();
        let port_input = form.port.clone();
        let key_input = form.identity_file.clone();
        let label = label_input.read(cx).value().to_string();
        let address = addr_input.read(cx).value().to_string();
        let port = port_input.read(cx).value().to_string();
        let identity_file = key_input.read(cx).value().to_string();
        if self
            .submit_ssh_host(&label, &address, &port, &identity_file, cx)
            .is_ok()
        {
            label_input.update(cx, |s, cx| s.set_value("", window, cx));
            addr_input.update(cx, |s, cx| s.set_value("", window, cx));
            port_input.update(cx, |s, cx| s.set_value("", window, cx));
            key_input.update(cx, |s, cx| s.set_value("", window, cx));
        }
    }

    /// Tailscale 피어 목록을 백그라운드 실행기에서 조회해 갱신한다(UI 스레드 블로킹 금지).
    pub fn refresh_tailscale_peers(&mut self, cx: &mut Context<Self>) {
        self.tailscale_loaded = true;
        if cfg!(test) && std::env::var_os("KRILL_TAILSCALE_BIN").is_none() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let peers = cx
                .background_executor()
                .spawn(async move { crate::ssh_hosts::discover_tailscale_peers() })
                .await;
            this.update(cx, |this, cx| {
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
                        let msg = text::SSH_AUTH_FAILED_GUIDE
                            .format(&[("label", &host.label), ("error", &err)]);
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

    pub fn test_ssh_host(host: crate::ssh_hosts::SshHost, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let host_for_bg = host.clone();
            let res = cx
                .background_executor()
                .spawn(async move { crate::ssh_hosts::test_connection(&host_for_bg) })
                .await;
            this.update(cx, |this, cx| {
                this.apply_ssh_test_result(host, res, cx);
            })
            .ok();
        })
        .detach();
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
        if has_custom_key || crate::ssh_hosts::has_default_ssh_key(Some(&home)) {
            self.launch_ssh_copy_id(host, cx);
        } else {
            self.ssh_keygen_confirm_host = Some(host);
            cx.notify();
        }
    }

    /// 키 생성 확인 후 `~/.ssh/id_ed25519`를 생성하고 새 터미널 탭에서 `ssh-copy-id`를 실행한다.
    pub fn confirm_keygen_and_register(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.ssh_keygen_confirm_host.take() else {
            return;
        };
        let home = Self::ssh_home_dir(cx);
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { crate::ssh_hosts::generate_default_ssh_key(Some(&home)) })
                .await;
            this.update(cx, |this, cx| match res {
                Ok(_) => this.launch_ssh_copy_id(host, cx),
                Err(e) => {
                    this.ssh_status = Some(format!("{}: {e}", host.label).into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 새 터미널 탭에서 `ssh-copy-id`(비밀번호 1회 입력)를 실행한다.
    pub fn launch_ssh_copy_id(&mut self, host: crate::ssh_hosts::SshHost, cx: &mut Context<Self>) {
        let label = host.label.clone();
        self.ssh_auth_failed_host = None;
        self.ssh_keygen_confirm_host = None;
        self.ssh_status = Some(
            text::SSH_REGISTER_STARTED
                .format(&[("label", &label)])
                .into(),
        );
        let copy_host = host.with_copy_id();
        cx.defer(move |cx| {
            if cx.has_global::<Config>() {
                cx.global_mut::<Config>().pending_ssh_connect = Some(copy_host);
            }
        });
        cx.notify();
    }

    pub fn run_doctor(&mut self, cx: &mut Context<Self>) {
        let dir = state_dir(cx);
        let report = run_doctor_check(dir.as_deref());
        self.doctor_status = Some(report.into());
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
                    edit(cx, |s| s.recording_paused = paused);
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

    fn render_general_tab(s: &Settings, cx: &mut Context<Self>) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let theme_button = |id: &'static str, label: &'static str, choice: ThemeChoice| {
            layout::segment(id, label, s.theme == choice)
                .debug_selector(move || id.into())
                .on_click(move |_, _, cx| {
                    edit(cx, |s| s.theme = choice);
                })
        };
        let awake_button = |id: &'static str, label: &'static str, mode: KeepAwake| {
            layout::segment(id, label, s.keep_awake == mode)
                .debug_selector(move || id.into())
                .on_click(move |_, _, cx| {
                    edit(cx, |s| s.keep_awake = mode);
                })
        };
        let language_button = |id: &'static str, label: &'static str, language: Language| {
            layout::segment(id, label, s.language == language)
                .debug_selector(move || id.into())
                .on_click(move |_, _, cx| {
                    set_language(language, cx);
                })
        };
        let zoom_pct = format!("{:.0}%", s.zoom() * 100.);
        let row = || layout::row(p);
        let label = |title: &'static str, desc: &'static str| layout::label(p, title, Some(desc));
        div()
            .flex()
            .flex_col()
            .gap_3()
            .children(crate::orca_import::render_banner("settings-general", cx))
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
                    .child(label(text::THEME.get(), text::THEME_DESC.get()))
                    .child(
                        layout::controls().child(layout::segmented_control(
                            p,
                            [
                                ("theme-system", text::SYSTEM.get(), ThemeChoice::System),
                                ("theme-light", text::LIGHT.get(), ThemeChoice::Light),
                                ("theme-dark", text::DARK.get(), ThemeChoice::Dark),
                            ]
                            .into_iter()
                            .map(|(id, l, choice)| theme_button(id, l, choice)),
                        )),
                    ),
            )
            .child(
                row()
                    .child(label(text::ZOOM.get(), text::ZOOM_DESC.get()))
                    .child(
                        layout::controls()
                            .child(
                                layout::ghost("zoom-out", "−")
                                    .debug_selector(|| "zoom-out".into())
                                    .on_click(|_, _, cx| edit(cx, |s| s.zoom = s.zoom_step(-1))),
                            )
                            .child(
                                div()
                                    .w_12()
                                    .text_center()
                                    .font_family(crate::ui::FONT_UI)
                                    .text_size(crate::ui::TEXT_SM)
                                    .child(zoom_pct),
                            )
                            .child(
                                layout::ghost("zoom-in", "+")
                                    .debug_selector(|| "zoom-in".into())
                                    .on_click(|_, _, cx| edit(cx, |s| s.zoom = s.zoom_step(1))),
                            )
                            .child(
                                layout::ghost("zoom-reset", text::RESET.get())
                                    .debug_selector(|| "zoom-reset".into())
                                    .on_click(|_, _, cx| edit(cx, |s| s.zoom = 1.0)),
                            ),
                    ),
            )
            .child(
                row()
                    .child(label(
                        text::NOTIFICATIONS.get(),
                        text::NOTIFICATIONS_DESC.get(),
                    ))
                    .child(layout::controls().child(layout::switch(
                        p,
                        "notif-toggle",
                        s.notifications,
                        false,
                        |next, _, cx| edit(cx, |s| s.notifications = next),
                    ))),
            )
            .child(
                row()
                    .child(label(
                        text::NOTIFY_INPUT.get(),
                        text::NOTIFY_INPUT_DESC.get(),
                    ))
                    .child(layout::controls().child(layout::switch(
                        p,
                        "notify-input-toggle",
                        s.notify_on_input,
                        false,
                        |next, _, cx| edit(cx, |s| s.notify_on_input = next),
                    ))),
            )
            .child(
                row()
                    .child(label(text::NOTIFY_DONE.get(), text::NOTIFY_DONE_DESC.get()))
                    .child(layout::controls().child(layout::switch(
                        p,
                        "notify-done-toggle",
                        s.notify_on_done,
                        false,
                        |next, _, cx| edit(cx, |s| s.notify_on_done = next),
                    ))),
            )
            .child(
                row()
                    .child(label(text::NOTIFY_BELL.get(), text::NOTIFY_BELL_DESC.get()))
                    .child(layout::controls().child(layout::switch(
                        p,
                        "notify-bell-toggle",
                        s.notify_on_bell,
                        false,
                        |next, _, cx| edit(cx, |s| s.notify_on_bell = next),
                    ))),
            )
            .child(
                row()
                    .child(label(text::TEST_NOTIFY.get(), text::TEST_NOTIFY_DESC.get()))
                    .child(
                        layout::controls().child(
                            layout::secondary("send-test-notification", text::TEST_NOTIFY.get())
                                .debug_selector(|| "send-test-notification".into())
                                .on_click(|_, _, cx| crate::alert::send_test_notification(cx)),
                        ),
                    ),
            )
            .child(
                row()
                    .child(label(text::ALERT_SOUND.get(), text::ALERT_SOUND_DESC.get()))
                    .child(layout::controls().child(layout::switch(
                        p,
                        "sound-toggle",
                        s.notification_sound,
                        false,
                        |next, _, cx| edit(cx, |s| s.notification_sound = next),
                    ))),
            )
            .child(
                row()
                    .child(label(
                        text::INACTIVE_ONLY.get(),
                        text::INACTIVE_ONLY_DESC.get(),
                    ))
                    .child(layout::controls().child(layout::switch(
                        p,
                        "notify-inactive-toggle",
                        s.notify_only_inactive,
                        false,
                        |next, _, cx| edit(cx, |s| s.notify_only_inactive = next),
                    ))),
            )
            .child(
                row()
                    .child(label(text::KEEP_AWAKE.get(), text::KEEP_AWAKE_DESC.get()))
                    .child(
                        layout::controls().child(layout::segmented_control(
                            p,
                            [
                                ("awake-on", text::AWAKE_ON.get(), KeepAwake::On),
                                ("awake-agent", text::AWAKE_AGENT.get(), KeepAwake::Agent),
                                ("awake-off", text::AWAKE_OFF.get(), KeepAwake::Off),
                            ]
                            .into_iter()
                            .map(|(id, l, mode)| awake_button(id, l, mode)),
                        )),
                    ),
            )
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
            .children(Self::render_update_rows(s, cx))
    }

    fn render_update_rows(s: &Settings, cx: &mut Context<Self>) -> [gpui::Div; 2] {
        use crate::updater::{CheckStatus, UpdateState};
        let p = crate::ui::palette(cx);
        let row = || layout::row(p);
        let label = |title: &'static str, desc: &'static str| layout::label(p, title, Some(desc));
        let update_state = cx.try_global::<UpdateState>().cloned().unwrap_or_default();
        let is_checking = matches!(update_state.status, CheckStatus::Checking);
        let status_label = match &update_state.status {
            CheckStatus::Idle => match &update_state.staged {
                Some(staged) => text::UPDATES_READY.format(&[("version", &staged.to_version)]),
                None => format!("v{}", env!("CARGO_PKG_VERSION")),
            },
            CheckStatus::Checking => text::UPDATES_CHECKING.get().to_owned(),
            CheckStatus::UpToDate { version } => {
                text::UPDATES_UP_TO_DATE.format(&[("version", version)])
            }
            CheckStatus::Ready(staged) => {
                text::UPDATES_READY.format(&[("version", &staged.to_version)])
            }
            CheckStatus::Error(err) => err.clone(),
        };
        let staged_for_btn = update_state.staged.clone().or_else(|| {
            if let CheckStatus::Ready(staged) = &update_state.status {
                Some(staged.clone())
            } else {
                None
            }
        });
        let check_btn_label = if is_checking {
            text::UPDATES_CHECKING.get()
        } else {
            text::UPDATES_CHECK.get()
        };
        let check_btn = layout::secondary("settings-update-check", check_btn_label)
            .debug_selector(|| "settings-update-check".into())
            .on_click(|_, _, cx| {
                trigger_manual_update_check(cx);
            });
        let mut controls = layout::controls().child(
            div()
                .text_size(crate::ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .debug_selector(|| "settings-update-status".into())
                .child(status_label),
        );
        if let Some(staged) = staged_for_btn {
            let restart_label = text::UPDATES_RESTART.format(&[("version", &staged.to_version)]);
            controls = controls.child(
                layout::primary("settings-update-restart", restart_label)
                    .debug_selector(|| "settings-update-restart".into())
                    .on_click(move |_, _, cx| {
                        if staged.spawn_relauncher().is_ok() {
                            cx.quit();
                        }
                    }),
            );
        }
        controls = controls.child(check_btn);

        [
            row()
                .child(label(text::UPDATES_TITLE.get(), text::UPDATES_DESC.get()))
                .child(controls),
            row()
                .child(label(
                    text::UPDATES_AUTO_RESTART.get(),
                    text::UPDATES_AUTO_RESTART_DESC.get(),
                ))
                .child(layout::controls().child(layout::switch(
                    p,
                    "update-auto-restart-toggle",
                    s.auto_restart_update,
                    false,
                    |next, _, cx| edit(cx, |s| s.auto_restart_update = next),
                ))),
        ]
    }
    /// 터미널 탭(OWE-658): 글꼴·크기·투명도·기본 커서·Option 키·선택 복사.
    fn render_terminal_tab(s: &Settings, cx: &mut Context<Self>) -> gpui::Div {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        let p = crate::ui::palette(cx);
        let row = || layout::row(p);
        let label = |title: &'static str, desc: &'static str| layout::label(p, title, Some(desc));
        let font_pt = format!("{}pt", s.terminal_font_size());
        let opacity_pct = format!("{}%", s.terminal_opacity_pct());
        let family = s.terminal_font_family.clone();
        let font_family = layout::secondary("terminal-font-family", family.clone())
            .debug_selector(|| "terminal-font-family".into())
            .dropdown_menu(move |mut menu, _, cx| {
                // 설치 글꼴 전체(숨김 시스템 글꼴 `.`으로 시작하는 이름 제외). 열 때만 읽는다.
                let mut names = cx.text_system().all_font_names();
                names.retain(|n| !n.starts_with('.'));
                names.sort_unstable();
                names.dedup();
                menu = menu.scrollable(true).max_h(px(360.));
                for name in names {
                    let checked = name == family;
                    menu = menu.item(PopupMenuItem::new(name.clone()).checked(checked).on_click(
                        move |_, _, cx| edit(cx, |s| s.terminal_font_family.clone_from(&name)),
                    ));
                }
                menu
            });
        let cursor_button = |id: &'static str, title: &'static str, shape: TerminalCursorShape| {
            layout::segment(id, title, s.terminal_cursor_shape == shape)
                .debug_selector(move || id.into())
                .on_click(move |_, _, cx| edit(cx, |s| s.terminal_cursor_shape = shape))
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                row()
                    .child(label(
                        text::TERMINAL_FONT.get(),
                        text::TERMINAL_FONT_DESC.get(),
                    ))
                    .child(layout::controls().child(font_family)),
            )
            .child(
                row()
                    .child(label(text::FONT_SIZE.get(), text::FONT_SIZE_DESC.get()))
                    .child(
                        layout::controls()
                            .child(
                                layout::ghost("font-dec", "−")
                                    .debug_selector(|| "font-dec".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, |s| {
                                            s.terminal_font_size =
                                                s.terminal_font_size().saturating_sub(1).max(10);
                                        });
                                    }),
                            )
                            .child(
                                div()
                                    .w_12()
                                    .text_center()
                                    .font_family(crate::ui::FONT_UI)
                                    .text_size(crate::ui::TEXT_SM)
                                    .child(font_pt),
                            )
                            .child(
                                layout::ghost("font-inc", "+")
                                    .debug_selector(|| "font-inc".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, |s| {
                                            s.terminal_font_size =
                                                s.terminal_font_size().saturating_add(1).min(24);
                                        });
                                    }),
                            )
                            .child(
                                layout::ghost("font-reset", text::RESET.get())
                                    .tooltip(text::FONT_RESET.get())
                                    .debug_selector(|| "font-reset".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, |s| {
                                            s.terminal_font_size = default_terminal_font_size();
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
                                layout::ghost("opacity-dec", "−")
                                    .debug_selector(|| "opacity-dec".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, |s| {
                                            s.terminal_opacity_pct =
                                                s.terminal_opacity_pct().saturating_sub(5).max(50);
                                        });
                                    }),
                            )
                            .child(
                                div()
                                    .w_12()
                                    .text_center()
                                    .font_family(crate::ui::FONT_UI)
                                    .text_size(crate::ui::TEXT_SM)
                                    .child(opacity_pct),
                            )
                            .child(
                                layout::ghost("opacity-inc", "+")
                                    .debug_selector(|| "opacity-inc".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, |s| {
                                            s.terminal_opacity_pct =
                                                s.terminal_opacity_pct().saturating_add(5).min(100);
                                        });
                                    }),
                            )
                            .child(
                                layout::ghost("opacity-reset", text::RESET.get())
                                    .debug_selector(|| "opacity-reset".into())
                                    .on_click(|_, _, cx| {
                                        edit(cx, |s| s.terminal_opacity_pct = 100);
                                    }),
                            ),
                    ),
            )
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
                    .child(layout::controls().child(layout::switch(
                        p,
                        "cursor-blink-toggle",
                        s.terminal_cursor_blink,
                        false,
                        |next, _, cx| edit(cx, |s| s.terminal_cursor_blink = next),
                    ))),
            )
            .child(
                row()
                    .child(label(
                        text::OPTION_AS_META.get(),
                        text::OPTION_AS_META_DESC.get(),
                    ))
                    .child(layout::controls().child(layout::switch(
                        p,
                        "option-meta-toggle",
                        s.option_as_meta,
                        false,
                        |next, _, cx| edit(cx, |s| s.option_as_meta = next),
                    ))),
            )
            .child(
                row()
                    .child(label(
                        text::COPY_ON_SELECT.get(),
                        text::COPY_ON_SELECT_DESC.get(),
                    ))
                    .child(layout::controls().child(layout::switch(
                        p,
                        "copy-select-toggle",
                        s.copy_on_select,
                        false,
                        |next, _, cx| edit(cx, |s| s.copy_on_select = next),
                    ))),
            )
    }

    /// 계정 행의 5h·7d 미니 사용량 바(OWE-719). 리셋 시각은 묶음 툴팁([`format_account_usage`])에 있다.
    fn render_usage_bar(
        key: &'static str,
        window_label: &'static str,
        win: Option<&krill_proto::usage::Window>,
        brand_color: gpui::Hsla,
        p: &crate::ui::Palette,
    ) -> gpui::Div {
        const BAR_W: f32 = 44.;
        let display = crate::accounts::display::window(
            win,
            i64::try_from(crate::accounts::display::now()).unwrap_or(i64::MAX),
        );
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
                mono(div().w(px(28.)).flex_none().whitespace_nowrap())
                    .debug_selector(move || format!("admin-usage-label-{key}"))
                    .text_color(p.muted_foreground)
                    .child(window_label),
            )
            .child(
                div()
                    .w(px(BAR_W))
                    .h(px(4.))
                    .rounded(px(2.))
                    .bg(p.muted)
                    .overflow_hidden()
                    .child(div().h_full().w(px(fill_w)).rounded(px(2.)).bg(usage_color(
                        pct,
                        brand_color,
                        p,
                    ))),
            )
            .child(
                mono(div().w(px(34.)).flex().justify_end())
                    .text_color(usage_color(pct, p.foreground, p))
                    .child(display.text),
            )
    }

    fn render_account_rule(
        &self,
        acct: &krill_proto::usage::AccountInfo,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use krill_proto::usage::{AutoSwitchMetric, AutoSwitchRule};

        let p = crate::ui::palette(cx);
        let snap = &self.accounts_snapshot;
        let existing_rule = snap.auto_switch.rule_for(acct.provider, &acct.id).cloned();
        let rule = existing_rule.clone().unwrap_or_else(|| AutoSwitchRule {
            provider: acct.provider,
            from_account_id: acct.id.clone(),
            enabled: true,
            threshold_percent: snap.auto_switch.threshold_percent,
            metric: AutoSwitchMetric::Max,
            target_account_id: None,
        });
        let rule_button =
            |id: String, label: SharedString, selected: bool, next: AutoSwitchRule| {
                layout::segment(SharedString::from(id.clone()), label, selected)
                    .disabled(self.account_rule_pending)
                    .debug_selector(move || id.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_account_rule(next.clone(), cx);
                    }))
            };
        let enabled = existing_rule.as_ref().is_some_and(|r| r.enabled);
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
                    .text_size(crate::ui::TEXT_XS)
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
                .child(layout::switch(
                    p,
                    format!("admin-rule-toggle-{}", acct.id),
                    enabled,
                    self.account_rule_pending,
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.set_account_rule(toggled.clone(), cx);
                        });
                    },
                )),
        );
        let metric_btns = [
            AutoSwitchMetric::Max,
            AutoSwitchMetric::FiveHour,
            AutoSwitchMetric::SevenDay,
        ]
        .into_iter()
        .map(|metric| {
            let mut next = rule.clone();
            next.metric = metric;
            rule_button(
                format!("admin-rule-metric-{}-{}", acct.id, metric.short_code()),
                if metric == AutoSwitchMetric::Max {
                    "Max"
                } else {
                    metric.label()
                }
                .into(),
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
        let mut automatic = rule.clone();
        automatic.target_account_id = None;
        let mut target_btns = vec![rule_button(
            format!("admin-rule-target-auto-{}", acct.id),
            text::ACCOUNTS_RULE_TARGET_AUTO.get().into(),
            rule.target_account_id.is_none(),
            automatic,
        )];
        for other in snap
            .accounts
            .iter()
            .filter(|other| other.provider == acct.provider && other.id != acct.id)
        {
            let mut next = rule.clone();
            next.target_account_id = Some(other.id.clone());
            target_btns.push(rule_button(
                format!("admin-rule-target-{}-{}", acct.id, other.id),
                format!("→ {}", other.label).into(),
                rule.target_account_id.as_deref() == Some(other.id.as_str()),
                next,
            ));
        }
        let targets = div()
            .flex()
            .items_center()
            .gap_2()
            .flex_wrap()
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(crate::i18n::catalog::status::PROVIDER_RULE_TARGET.get()),
            )
            .child(layout::segmented_control(p, target_btns));
        controls = controls.child(targets);
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
    ) -> gpui::Div {
        use gpui_component::Sizable as _;
        let save_id = format!("admin-rename-save-{}", acc.id);
        let cancel_id = format!("admin-rename-cancel-{}", acc.id);
        let (provider, account_id, state) = (acc.provider, acc.id.clone(), input.clone());
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .w(px(220.))
                    .child(gpui_component::input::Input::new(input).small()),
            )
            .child(
                layout::primary(
                    SharedString::from(save_id.clone()),
                    text::ACCOUNT_RENAME_SAVE.get(),
                )
                .debug_selector(move || save_id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let label = state.read(cx).value().trim().to_owned();
                    this.rename = None;
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
                .on_click(cx.listener(|this, _, _, cx| {
                    this.rename = None;
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
            .and_then(|i| i.org_name.as_deref());
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
        self.rename = Some((id, input));
        cx.notify();
    }

    fn start_remove(&mut self, id: String, cx: &mut Context<Self>) {
        self.rename = None;
        self.removing = Some(id);
        cx.notify();
    }

    /// 계정 한 행(OWE-719, Orca accounts-pane 행 밀도): 아바타+상태 점, 이름·보조 줄, 5h·7d 미니 바, 주 동작 하나
    /// (전환 또는 다시 로그인)와 … 메뉴(다시 로그인·이름 변경·제거). 사용 중 표시는 행 강조와 "사용 중" 하나뿐이다.
    fn render_account_row(
        &self,
        acc: &krill_proto::usage::AccountInfo,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
        use gpui_component::{IconName, Sizable as _};
        use krill_proto::usage::{ConnectionStatus, ProviderKind, UsageRequest};

        let p = crate::ui::palette(cx);
        let provider = acc.provider;
        let orca = crate::accounts::orca_managed(acc);
        let snap = &self.accounts_snapshot;
        let usage = crate::accounts::display::account(
            acc,
            snap.statuses.iter().find(|s| s.account_id == acc.id),
            crate::accounts::display::now(),
        );
        let conn = crate::accounts::display::connection(snap, acc, &usage);
        let needs_login = conn == Some(ConnectionStatus::NeedsLogin);
        let can_login = !orca && matches!(provider, ProviderKind::Claude | ProviderKind::Codex);
        let manageable = !acc.host_default && !orca;
        let (primary, heading) = account_heading(acc);
        let detail = if needs_login && can_login {
            text::ACCOUNT_SIGN_IN_TO_USE.get().to_owned()
        } else {
            self.account_detail(acc, heading)
        };
        let duplicate =
            crate::accounts::duplicate_of(acc, &snap.accounts, &self.identities.0).map(|other| {
                if other.host_default {
                    text::ACCOUNT_DUPLICATE_HOST.get().to_owned()
                } else {
                    text::ACCOUNT_DUPLICATE.format(&[("label", &other.label)])
                }
            });
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
                gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
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
                    .absolute()
                    .right(px(-2.))
                    .bottom(px(-2.))
                    .size(px(10.))
                    .rounded_full()
                    .border_2()
                    .border_color(p.background)
                    .bg(crate::accounts::display::connection_color(conn, p)),
            );

        let renaming = self
            .rename
            .as_ref()
            .filter(|(id, _)| *id == acc.id)
            .map(|(_, input)| input.clone());
        let in_use_id = format!("admin-account-in-use-{}", acc.id);
        let name_line = div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
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
                    crate::ui::badge(p, crate::ui::BadgeTone::Neutral, text::ACCOUNT_IN_USE.get())
                        .id(SharedString::from(in_use_id.clone()))
                        .debug_selector(move || in_use_id.clone()),
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
        let usage_tip = format_account_usage(&acc.usage);
        let usage_cluster = div()
            .id(SharedString::from(usage_id.clone()))
            .debug_selector(move || usage_id.clone())
            .tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(usage_tip.clone()).build(window, cx)
            })
            .flex_none()
            .flex()
            .flex_col()
            .gap_0p5()
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

        // 주 동작 하나: 로그인이 필요하면 "다시 로그인", 아니면 비활성 계정에만 "전환".
        let mut actions = div().flex_none().flex().items_center().gap_1();
        if can_login && needs_login {
            let login_account = acc.clone();
            let login_id = format!("admin-login-account-{}", acc.id);
            actions = actions.child(
                layout::secondary(
                    gpui::ElementId::Name(login_id.clone().into()),
                    text::ACCOUNT_LOGIN.get(),
                )
                .icon(IconName::RefreshCw)
                .disabled(self.account_rule_pending || self.accounts_offline)
                .debug_selector(move || login_id.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.login_account(provider, Some(login_account.clone()), cx);
                })),
            );
        } else if !orca && !acc.active {
            let switch_id = format!("admin-switch-account-{}", acc.id);
            let acc_id = acc.id.clone();
            actions = actions.child(
                layout::secondary(
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
        if can_login || manageable {
            let menu_id = format!("admin-account-menu-{}", acc.id);
            let view = cx.entity().downgrade();
            let menu_acc = acc.clone();
            let busy = self.account_rule_pending || self.accounts_offline;
            actions = actions.child(
                Button::new(gpui::ElementId::Name(menu_id.clone().into()))
                    .small()
                    .ghost()
                    .icon(IconName::Ellipsis)
                    .tooltip(text::ACCOUNT_MORE.get())
                    .debug_selector(move || menu_id.clone())
                    .dropdown_menu(move |mut menu, _, _| {
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
                            let (view, id) = (view.clone(), menu_acc.id.clone());
                            menu = menu.separator().item(
                                PopupMenuItem::new(text::ACCOUNT_REMOVE.get()).on_click(
                                    move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.start_remove(id.clone(), cx);
                                        })
                                        .ok();
                                    },
                                ),
                            );
                        }
                        menu
                    }),
            );
        }

        let row_id = format!("admin-account-row-{}", acc.id);
        div()
            .id(SharedString::from(row_id.clone()))
            .debug_selector(move || row_id.clone())
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_2p5()
            .rounded(crate::ui::RADIUS_MD)
            .border_1()
            .map(|d| {
                if acc.active {
                    d.border_color(p.foreground.opacity(0.2))
                        .bg(p.accent.opacity(0.15))
                } else {
                    d.border_color(p.border)
                }
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
            .find(|a| a.id == id && !a.host_default && !crate::accounts::orca_managed(a))?;
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
                            .child(text::ACCOUNT_REMOVE_HINT.get()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new(gpui::ElementId::Name(cancel_id.clone().into()))
                            .small()
                            .ghost()
                            .label(text::ACCOUNT_RENAME_CANCEL.get())
                            .debug_selector(move || cancel_id.clone())
                            .on_click(cx.listener(move |this, _, _, cx| close(this, cx))),
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
        Some(crate::ui::modal_backdrop(window.viewport_size(), card))
    }

    fn render_accounts_tab(&self, cx: &mut Context<Self>) -> gpui::Div {
        use gpui_component::{IconName, Sizable as _};
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
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.warning)
                    .child(error)
            }));

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

        let mut not_installed = Vec::new();
        for (provider, title, subtitle) in providers {
            let mut accounts: Vec<_> = self
                .accounts_snapshot
                .accounts
                .iter()
                .filter(|a| a.provider == provider)
                .cloned()
                .collect();
            // 시스템 기본값(호스트) 계정을 맨 위에 둔다(Orca accounts-pane 패리티).
            accounts.sort_by_key(|a| !a.host_default);
            let add_id = format!("admin-add-account-{}", provider.as_str());

            let header = div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(crate::ui::icons::provider_icon(provider, 16., p.foreground))
                        .child(
                            div()
                                .text_size(crate::ui::TEXT_MD)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(title),
                        ),
                )
                .child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(subtitle),
                );

            let add_btn = layout::secondary(
                gpui::ElementId::Name(add_id.clone().into()),
                text::ACCOUNTS_ADD.get(),
            )
            .icon(IconName::Plus)
            .debug_selector(move || add_id.clone())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.login_account(provider, None, cx);
            }));
            let provider_installed = crate::accounts::installed::is_installed(cx, provider);
            let mut sub_controls = layout::controls();
            if !provider_installed
                && let Some(install_cmd) = agent_install_command(provider.as_str())
            {
                let install_id = format!("admin-install-provider-{}", provider.as_str());
                sub_controls = sub_controls.child(
                    layout::secondary(
                        gpui::ElementId::Name(install_id.clone().into()),
                        text::INSTALL_IN_TERMINAL.get(),
                    )
                    .icon(IconName::SquareTerminal)
                    .debug_selector(move || install_id.clone())
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(install_cmd.clone()));
                        queue_terminal_command(install_cmd.clone(), cx);
                    }),
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
            for acc in &accounts {
                list = list.child(self.render_account_row(acc, cx));
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
                .child(active_line)
                .child(list);

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
                        Button::new(gpui::ElementId::Name(adv_id.clone().into()))
                            .small()
                            .ghost()
                            .icon(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .label(text::ACCOUNTS_ADVANCED.format(&[("n", &n.to_string())]))
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

            if crate::accounts::installed::is_installed(cx, provider) {
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
                            Button::new("admin-accounts-not-installed")
                                .small()
                                .ghost()
                                .icon(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .label(
                                    text::PROVIDERS_NOT_INSTALLED
                                        .format(&[("n", &not_installed.len().to_string())]),
                                )
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
                            .text_size(crate::ui::TEXT_2XS)
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
        );
        let rl_btn = layout::segment(
            "admin-rate-limit-toggle",
            text::AUTO_SWITCH_RATE_LIMIT.get(),
            policy.switch_on_rate_limit,
        )
        .debug_selector(|| "admin-rate-limit-toggle".into())
        .on_click({
            let policy = policy.clone();
            cx.listener(move |this, _, _, cx| {
                let mut next = policy.clone();
                next.switch_on_rate_limit = !next.switch_on_rate_limit;
                this.apply_account_request(UsageRequest::SetAutoSwitch(next), cx);
            })
        });
        let preset_btns = krill_proto::usage::AUTO_SWITCH_PRESETS
            .into_iter()
            .map(|pct| {
                let id_str = format!("admin-threshold-{pct}");
                let selected = policy.threshold_percent == pct;
                let policy_cl = policy.clone();
                layout::segment(
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
            .flex_wrap()
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::AUTO_SWITCH_THRESHOLD.get()),
            )
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
                    .child(layout::controls().child(layout::switch(
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
                    ))),
            );
        }

        root.child(
            layout::card(p)
                .flex()
                .flex_col()
                .gap_2p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(layout::label(p, text::AUTO_SWITCH_TITLE.get(), None))
                        .child(layout::controls().child(toggle_btn)),
                )
                .child(threshold_row)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .flex_wrap()
                        .child(rl_btn)
                        .child(
                            layout::controls()
                                .child(
                                    layout::secondary(
                                        "admin-accounts-refresh",
                                        text::ACCOUNTS_REFRESH.get(),
                                    )
                                    .icon(IconName::RefreshCw)
                                    .debug_selector(|| "admin-accounts-refresh".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.apply_account_request(
                                                UsageRequest::ListAccounts,
                                                cx,
                                            );
                                        },
                                    )),
                                )
                                .child(
                                    layout::secondary(
                                        "admin-open-accounts-toml",
                                        text::ACCOUNTS_OPEN_TOML.get(),
                                    )
                                    .icon(IconName::ExternalLink)
                                    .debug_selector(|| "admin-open-accounts-toml".into())
                                    .on_click(|_, _, cx| {
                                        open_file("accounts.toml", "", cx);
                                    }),
                                ),
                        ),
                ),
        )
    }

    fn render_agents_tab(
        &mut self,
        s: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let row = || layout::row(p);
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
            let selected = s.default_agent == agent_id;
            layout::segment(
                gpui::ElementId::Name(btn_id.clone().into()),
                label,
                selected,
            )
            .icon(gpui_component::Icon::default().data(crate::ui::icons::agent_svg_bytes(agent_id)))
            .debug_selector(move || btn_id.clone())
            .on_click(move |_, _, cx| {
                edit(cx, |s| agent_id.clone_into(&mut s.default_agent));
            })
        });
        let default_agent_row = row()
            .child(layout::label(
                p,
                text::DEFAULT_AGENT_TITLE.get(),
                Some(text::DEFAULT_AGENT_DESC.get()),
            ))
            .child(layout::controls().child(layout::segmented_control(p, default_agent_btns)));

        // YOLO 토글은 `settings.toml` 저장과 함께 셸이 데몬에 적용할 권한 요청을 남긴다.
        let yolo_row = row()
            .child(layout::label(
                p,
                text::YOLO_TITLE.get(),
                Some(text::YOLO_DESC.get()),
            ))
            .child(layout::controls().child(layout::switch(
                p,
                "admin-yolo-toggle",
                s.yolo_mode,
                false,
                |next, _, cx| {
                    cx.global_mut::<Config>().pending_permission = Some(if next {
                        krill_proto::agent::Permission::Yolo
                    } else {
                        krill_proto::agent::Permission::Manual
                    });
                    edit(cx, |s| s.yolo_mode = next);
                },
            )));

        // 유휴 동면은 `settings.toml` + 상태 디렉터리의 `hibernation.enabled` 표식으로 데몬에
        // 전달한다(OWE-655). 표식 동기화는 `edit`·`reload`가 맡는다.
        let hib_row = row()
            .child(layout::label(
                p,
                text::HIBERNATION_TITLE.get(),
                Some(text::HIBERNATION_DESC.get()),
            ))
            .child(layout::controls().child(layout::switch(
                p,
                "hib-toggle",
                s.hibernation_enabled,
                false,
                |next, _, cx| edit(cx, |s| s.hibernation_enabled = next),
            )));

        let auto_setup_row = row()
            .child(layout::label(
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
                            if let Some(env) = &env
                                && !env.is_isolated_state()
                            {
                                drop(crate::onboarding::stage_binaries(&env.krill, &base));
                            }
                            let installed = setup_all_agents_and_skills_at(&base, &state);
                            if let Some(env) = env
                                && !env.is_isolated_state()
                            {
                                crate::onboarding::actions::auto_prepare(env.clone(), cx).detach();
                                let state = state.clone();
                                cx.background_executor()
                                    .spawn(async move {
                                        drop(
                                            crate::onboarding::install_codex_hook_with_login_path(
                                                &env, &state,
                                            ),
                                        );
                                    })
                                    .detach();
                            }
                            if !installed.is_empty() {
                                let msg = text::SKILLS_INSTALLED
                                    .format(&[("path", &installed.join(", "))]);
                                this.skill_status = Some(msg.into());
                                cx.notify();
                            }
                        })),
                ),
            );

        let mut root = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::AGENTS_DESC.get()),
            )
            .child(default_agent_row)
            .child(yolo_row)
            .child(hib_row)
            .child(auto_setup_row);

        let builtin_agents = krill_proto::agent::Agents::load(None).unwrap_or_default();
        let dir = state_dir(cx);
        let user_agents = self.cached_agents_file(dir.as_deref(), cx);
        // 초기 읽기가 끝나기 전에 편집기를 기본값으로 만들면 사용자 인자가
        // 늦게 도착해도 입력 상태에 반영되지 않는다. 로드 후 처음 만든다.
        if dir.is_some() && self.agents_file_cache.0.is_none() {
            return root.children(self.render_agent_extra_rows(s, cx));
        }
        let effective = krill_proto::agent::Agents::load(user_agents.as_deref())
            .unwrap_or_else(|_| builtin_agents.clone());
        for agent_id in ["claude", "codex", "agy", "cursor", "opencode"] {
            root = root.child(self.render_agent_profile_card(
                agent_id,
                &builtin_agents,
                &effective,
                window,
                cx,
            ));
        }
        root.child(self.render_skills_card(cx))
            .children(self.render_agent_extra_rows(s, cx))
    }

    #[inline(never)]
    fn render_agent_extra_rows(&mut self, s: &Settings, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let p = crate::ui::palette(cx);
        let row = || layout::row(p);
        let auto_rename_row = row()
            .child(layout::label(
                p,
                text::AUTO_RENAME_BRANCH.get(),
                Some(text::AUTO_RENAME_BRANCH_DESC.get()),
            ))
            .child(layout::controls().child(layout::switch(
                p,
                "auto-rename-branch-toggle",
                s.auto_rename_branch_from_work,
                false,
                |next, _, cx| edit(cx, |s| s.auto_rename_branch_from_work = next),
            )));

        let cache_timer_row = row()
            .child(layout::label(
                p,
                text::PROMPT_CACHE_TIMER.get(),
                Some(text::PROMPT_CACHE_TIMER_DESC.get()),
            ))
            .child(layout::controls().child(layout::switch(
                p,
                "prompt-cache-timer-toggle",
                s.prompt_cache_timer_enabled,
                false,
                |next, _, cx| edit(cx, |s| s.prompt_cache_timer_enabled = next),
            )));

        let is_1h = s.prompt_cache_ttl_secs >= 3_600;
        let ttl_5m = layout::segment("prompt-cache-ttl-5m", text::CACHE_TTL_5M.get(), !is_1h)
            .debug_selector(|| "prompt-cache-ttl-5m".into())
            .on_click(|_, _, cx| edit(cx, |s| s.prompt_cache_ttl_secs = 300));
        let ttl_1h = layout::segment("prompt-cache-ttl-1h", text::CACHE_TTL_1H.get(), is_1h)
            .debug_selector(|| "prompt-cache-ttl-1h".into())
            .on_click(|_, _, cx| edit(cx, |s| s.prompt_cache_ttl_secs = 3_600));
        let cache_ttl_row = row()
            .child(layout::label(
                p,
                text::PROMPT_CACHE_TTL.get(),
                Some(text::PROMPT_CACHE_TTL_DESC.get()),
            ))
            .child(layout::controls().child(layout::segmented_control(p, [ttl_5m, ttl_1h])));

        let view = cx.entity();
        let mut workspace_trust_section = div().w_full().min_w_0().flex().flex_col().gap_2().child(
            row()
                .child(layout::label(
                    p,
                    text::WORKSPACE_TRUST_PRESET.get(),
                    Some(text::WORKSPACE_TRUST_PRESET_DESC.get()),
                ))
                .child(layout::controls().child(layout::switch(
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
                                edit(cx, |s| s.workspace_trust_preset_enabled = false);
                                cx.notify();
                            }
                        });
                    },
                ))),
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
                            .text_size(crate::ui::TEXT_2XS)
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
                                        edit(cx, |s| s.workspace_trust_preset_enabled = true);
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                layout::secondary(
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
        vec![
            auto_rename_row,
            cache_timer_row,
            cache_ttl_row,
            workspace_trust_section,
        ]
    }

    #[inline(never)]
    fn render_agent_profile_card(
        &mut self,
        agent_id: &'static str,
        builtin_agents: &krill_proto::agent::Agents,
        effective: &krill_proto::agent::Agents,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use gpui_component::Sizable as _;
        use krill_proto::usage::ProviderKind;
        let p = crate::ui::palette(cx);
        let display_name = builtin_agents
            .profiles
            .get(agent_id)
            .map_or(agent_id, |prof| prof.name.as_str())
            .to_owned();
        let provider = ProviderKind::parse(agent_id);
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
        let dir_info = match active {
            Some(a) => format!(
                "{} ({}) · {} {}",
                a.label,
                a.profile_dir,
                profile_count,
                text::PROFILES_UNIT.get()
            ),
            None => format!(
                "{} · {} {}",
                agent_id,
                profile_count,
                text::PROFILES_UNIT.get()
            ),
        };
        let hook_cmd = format!("krill hook install {agent_id}");
        let hook_id = format!("admin-agent-hook-{agent_id}");
        let hook_copy = hook_cmd.clone();
        let id_owned = agent_id.to_owned();
        let install_cmd = agent_install_command(agent_id);
        let install_id = format!("admin-agent-install-{agent_id}");
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
        let save_id = format!("admin-agent-save-{agent_id}");
        let open_id = format!("admin-agent-open-toml-{agent_id}");
        let edit_section = div()
            .w_full()
            .min_w_0()
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
                            let saved = match state_dir(cx) {
                                Some(dir) => save_agent_profile(&dir, agent_id, &args, &env),
                                None => Err("no state dir".to_owned()),
                            };
                            edit.status = Some(match saved {
                                Ok(()) => (text::AGENT_SAVED.get().into(), false),
                                Err(e) => (e.into(), true),
                            });
                            cx.notify();
                        })),
                    )
                    .child(
                        layout::secondary(
                            gpui::ElementId::Name(open_id.clone().into()),
                            text::AGENTS_OPEN_TOML.get(),
                        )
                        .debug_selector(move || open_id.clone())
                        .on_click(|_, _, cx| {
                            open_file(krill_proto::agent::FILE_NAME, "", cx);
                        }),
                    ),
            );
        layout::card(p)
            .debug_selector(move || format!("admin-agent-card-{agent_id}"))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(32.))
                                    .rounded(crate::ui::RADIUS_SM)
                                    .bg(p.muted)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(crate::ui::icons::agent_brand_icon(
                                        agent_id,
                                        18.,
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
                                            .truncate()
                                            .text_size(crate::ui::TEXT_SM)
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(display_name),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(crate::ui::FONT_MONO)
                                            .text_size(crate::ui::TEXT_2XS)
                                            .text_color(p.muted_foreground)
                                            .child(dir_info),
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
                                    hook_cmd,
                                )
                                .debug_selector(move || hook_id.clone())
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
                                        let status_msg = if id_owned == "codex" {
                                            queue_terminal_command(
                                                format!("{hook_copy} --yes"),
                                                cx,
                                            );
                                            format!("{hook_copy} (queued)")
                                        } else {
                                            match install_agent_hook_at(&id_owned, &base, &state) {
                                                Ok(path) => text::SKILLS_INSTALLED.format(&[(
                                                    "path",
                                                    &path.display().to_string(),
                                                )]),
                                                Err(e) => {
                                                    queue_terminal_command(
                                                        format!("{hook_copy} --yes"),
                                                        cx,
                                                    );
                                                    format!("{id_owned} hook: {e}")
                                                }
                                            }
                                        };
                                        this.skill_status = Some(status_msg.into());
                                        cx.notify();
                                    },
                                )),
                            ),
                    ),
            )
            .child(edit_section)
    }

    #[inline(never)]
    fn render_skills_card(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let mut skills_card = layout::card(p)
            .debug_selector(|| "admin-skills-card".into())
            .flex()
            .flex_col()
            .gap_2()
            .text_size(crate::ui::TEXT_SM)
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(layout::label(p, text::SKILLS_TITLE.get(), None))
                    .child(
                        layout::controls().child(
                            layout::primary(
                                "admin-install-all-skills",
                                text::SKILLS_INSTALL_ALL.get(),
                            )
                            .debug_selector(|| "admin-install-all-skills".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let base = if cfg!(test) {
                                    state_dir(cx).unwrap_or_else(crate::accounts::default_home)
                                } else {
                                    crate::accounts::default_home()
                                };
                                let installed = install_all_skills_at(&base);
                                if !installed.is_empty() {
                                    let msg = text::SKILLS_INSTALLED
                                        .format(&[("path", &installed.join(", "))]);
                                    this.skill_status = Some(msg.into());
                                    cx.notify();
                                }
                            })),
                        ),
                    ),
            );
        for skill in krill_proto::orchestration::builtin_skills() {
            let btn_id = format!("admin-install-skill-{}", skill.name);
            let row_id = format!("admin-skill-row-{}", skill.name);
            let skill_name = skill.name;
            skills_card = skills_card.child(
                div()
                    .id(gpui::ElementId::Name(row_id.clone().into()))
                    .debug_selector(move || row_id.clone())
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
                    .child(layout::label(p, skill.name, Some(skill.description)))
                    .child(
                        layout::controls().child(
                            layout::secondary(
                                gpui::ElementId::Name(btn_id.clone().into()),
                                text::SKILLS_INSTALL.get(),
                            )
                            .debug_selector(move || btn_id.clone())
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let home = if cfg!(test) {
                                        state_dir(cx).unwrap_or_else(crate::accounts::default_home)
                                    } else {
                                        crate::accounts::default_home()
                                    };
                                    let mut installed = Vec::new();
                                    for dir in [
                                        home.join(".claude/skills"),
                                        home.join(".codex/skills"),
                                        home.join(".gemini/config/skills"),
                                        home.join(".cursor/skills"),
                                        home.join(".config/opencode/skills"),
                                    ] {
                                        if let Ok(path) = krill_proto::orchestration::install_skill(
                                            skill_name, &dir,
                                        ) {
                                            installed.push(path.display().to_string());
                                        }
                                    }
                                    if !installed.is_empty() {
                                        let msg = text::SKILLS_INSTALLED
                                            .format(&[("path", &installed.join(", "))]);
                                        this.skill_status = Some(msg.into());
                                        cx.notify();
                                    }
                                },
                            )),
                        ),
                    ),
            );
        }
        if let Some(status) = &self.skill_status {
            skills_card = skills_card.child(
                div()
                    .id("admin-skills-status")
                    .debug_selector(|| "admin-skills-status".into())
                    .w_full()
                    .min_w_0()
                    .p_2()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.card)
                    .border_1()
                    .border_color(p.border)
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_XS)
                    .child(status.clone()),
            );
        }
        skills_card
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
        let p = crate::ui::palette(cx);
        let mut root = div().flex().flex_col().gap_3().child(
            layout::row(p)
                .child(layout::label(p, text::SSH_TITLE.get(), None))
                .child(
                    layout::controls()
                        .child(
                            layout::secondary(
                                "admin-link-pairing",
                                crate::i18n::link_ui::CLIENT_TITLE.get(),
                            )
                            .debug_selector(|| "admin-link-pairing".into())
                            .on_click(|_, _, cx| crate::link_ui::open_pairing(cx)),
                        )
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
                                        .text_size(crate::ui::TEXT_XS)
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
                .child(
                    div()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_XS)
                        .child(status.clone()),
                );
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
                let summary = host.format_summary();
                let card_id = format!("admin-ssh-card-{}", host.id);
                let dot_id = format!("admin-ssh-status-dot-{}", host.id);
                let conn_id = format!("admin-ssh-connect-{}", host.id);
                let disc_id = format!("admin-ssh-disconnect-{}", host.id);
                let test_id = format!("admin-ssh-test-{}", host.id);
                let edit_id = format!("admin-ssh-edit-{}", host.id);
                let rm_id = format!("admin-ssh-remove-{}", host.id);
                let host_conn = host.clone();
                let host_disc = host.clone();
                let host_cl = host.clone();
                let host_edit = host.clone();
                let rm_host_id = host.id.clone();

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

                root = root.child(
                    layout::row(p)
                        .id(gpui::ElementId::Name(card_id.clone().into()))
                        .debug_selector(move || card_id.clone())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(crate::ui::icons::svg_box(
                                    crate::ui::icons::SERVER_SVG,
                                    16.,
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
                                                .text_size(crate::ui::TEXT_XS)
                                                .text_color(p.muted_foreground)
                                                .child(summary),
                                        ),
                                ),
                        )
                        .child(
                            layout::controls()
                                .child(connect_or_disconnect_btn)
                                .child(
                                    layout::secondary(
                                        gpui::ElementId::Name(test_id.clone().into()),
                                        text::SSH_TEST.get(),
                                    )
                                    .small()
                                    .debug_selector(move || test_id.clone())
                                    .on_click(cx.listener(
                                        move |_, _, _, cx| {
                                            Self::test_ssh_host(host_cl.clone(), cx);
                                        },
                                    )),
                                )
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
                                .child(
                                    layout::ghost(
                                        gpui::ElementId::Name(rm_id.clone().into()),
                                        text::SSH_REMOVE.get(),
                                    )
                                    .small()
                                    .debug_selector(move || rm_id.clone())
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.remove_ssh_host(&rm_host_id, cx);
                                        },
                                    )),
                                ),
                        ),
                );
            }
        }
        root
    }

    fn ensure_workflow_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workflow_form.is_some() {
            self.sync_workflow_repo_hooks(window, cx);
            return;
        }
        let default_repo = std::env::current_dir()
            .ok()
            .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let dir = state_dir(cx);
        let store = dir
            .as_deref()
            .map(krill_proto::orchestration::WorktreeStore::load_cached)
            .unwrap_or_default();
        let loaded_repo = Self::normalize_repo_key(&default_repo);
        let default_setup = store.setup_script_for(&default_repo).unwrap_or_default();
        let default_archive = store.archive_script_for(&default_repo).unwrap_or_default();
        let loaded_setup = default_setup.clone();
        let loaded_archive = default_archive.clone();
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
        let setup_obs = cx.observe(&setup_script, |this, inp, cx| {
            if let Some(form) = &mut this.workflow_form
                && Self::normalize_repo_key(&form.repo_path.read(cx).value()) == form.loaded_repo
            {
                form.loaded_setup = inp.read(cx).value().to_string();
            }
        });
        let archive_obs = cx.observe(&archive_script, |this, inp, cx| {
            if let Some(form) = &mut this.workflow_form
                && Self::normalize_repo_key(&form.repo_path.read(cx).value()) == form.loaded_repo
            {
                form.loaded_archive = inp.read(cx).value().to_string();
            }
        });
        self.workflow_form = Some(WorkflowRepoForm {
            repo_path,
            symlink_path,
            setup_script,
            archive_script,
            loaded_repo,
            loaded_setup,
            loaded_archive,
            status: None,
            _subs: vec![repo_obs, repo_sub, setup_obs, archive_obs],
        });
    }

    fn sync_workflow_repo_hooks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &self.workflow_form else {
            return;
        };
        let raw_repo = form.repo_path.read(cx).value().to_string();
        let next_repo = Self::normalize_repo_key(&raw_repo);
        if next_repo == form.loaded_repo {
            return;
        }
        let prev_setup = form.loaded_setup.clone();
        let prev_archive = form.loaded_archive.clone();
        let cur_setup = form.setup_script.read(cx).value().to_string();
        let cur_archive = form.archive_script.read(cx).value().to_string();
        let setup_inp = form.setup_script.clone();
        let archive_inp = form.archive_script.clone();
        let symlink_inp = form.symlink_path.clone();

        let store = state_dir(cx)
            .as_deref()
            .and_then(|d| krill_proto::orchestration::WorktreeStore::load(d).ok())
            .unwrap_or_default();
        let repo_setup = next_repo
            .as_deref()
            .and_then(|k| store.setup_script_for(k))
            .unwrap_or_default();
        let repo_archive = next_repo
            .as_deref()
            .and_then(|k| store.archive_script_for(k))
            .unwrap_or_default();

        let target_setup = if cur_setup == prev_setup {
            repo_setup
        } else {
            cur_setup.clone()
        };
        let target_archive = if cur_archive == prev_archive {
            repo_archive
        } else {
            cur_archive.clone()
        };

        if let Some(form) = &mut self.workflow_form {
            form.loaded_repo = next_repo;
            form.loaded_setup.clone_from(&target_setup);
            form.loaded_archive.clone_from(&target_archive);
            form.status = None;
        }
        if cur_setup != target_setup {
            setup_inp.update(cx, |inp, cx| inp.set_value(target_setup, window, cx));
        }
        if cur_archive != target_archive {
            archive_inp.update(cx, |inp, cx| inp.set_value(target_archive, window, cx));
        }
        if !symlink_inp.read(cx).value().is_empty() {
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
        let next_repo = Self::normalize_repo_key(repo_raw);
        let store = state_dir(cx)
            .as_deref()
            .and_then(|d| krill_proto::orchestration::WorktreeStore::load(d).ok())
            .unwrap_or_default();
        let repo_setup = next_repo
            .as_deref()
            .and_then(|k| store.setup_script_for(k))
            .unwrap_or_default();
        let repo_archive = next_repo
            .as_deref()
            .and_then(|k| store.archive_script_for(k))
            .unwrap_or_default();
        if let Some(form) = &mut self.workflow_form {
            form.loaded_repo = next_repo;
            form.loaded_setup.clone_from(&repo_setup);
            form.loaded_archive.clone_from(&repo_archive);
            form.status = None;
        }
        if repo_inp.read(cx).value().as_str() != repo_raw {
            repo_inp.update(cx, |inp, cx| inp.set_value(repo_raw.to_owned(), window, cx));
        }
        setup_inp.update(cx, |inp, cx| inp.set_value(repo_setup, window, cx));
        archive_inp.update(cx, |inp, cx| inp.set_value(repo_archive, window, cx));
        if !symlink_inp.read(cx).value().is_empty() {
            symlink_inp.update(cx, |inp, cx| inp.set_value("", window, cx));
        }
        cx.notify();
    }

    fn normalize_repo_key(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let path = Path::new(trimmed);
        Some(
            std::fs::canonicalize(path)
                .unwrap_or_else(|_| path.to_path_buf())
                .display()
                .to_string(),
        )
    }

    pub fn add_worktree_symlink_for(
        &mut self,
        repo_raw: &str,
        rel_path: &str,
        cx: &mut Context<Self>,
    ) -> Result<Vec<String>, String> {
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
        let result = krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
            store.add_symlink(&repo_key, rel_path)
        });
        match result {
            Ok(list) => {
                if let Some(form) = &mut self.workflow_form {
                    form.status = Some((text::HOOKS_SAVED.get().into(), false));
                }
                cx.defer(notify_external_change);
                cx.notify();
                Ok(list)
            }
            Err(err) => {
                let msg = err.to_string();
                if let Some(form) = &mut self.workflow_form {
                    form.status = Some((msg.clone().into(), true));
                }
                cx.notify();
                Err(msg)
            }
        }
    }

    pub fn remove_worktree_symlink_for(
        &mut self,
        repo_key: &str,
        rel_path: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(dir) = state_dir(cx) {
            krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
                store.remove_symlink(repo_key, rel_path);
                Ok(())
            })
            .ok();
            if let Some(form) = &mut self.workflow_form {
                form.status = Some((text::HOOKS_SAVED.get().into(), false));
            }
            cx.defer(notify_external_change);
            cx.notify();
        }
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
        let result = krill_proto::orchestration::WorktreeStore::mutate(&dir, |store| {
            store.set_setup_script(&repo_key, &setup_trimmed)?;
            store.set_archive_script(&repo_key, &archive_trimmed)?;
            Ok(())
        });
        match result {
            Ok(()) => {
                if let Some(form) = &mut self.workflow_form {
                    let cur_repo = Self::normalize_repo_key(&form.repo_path.read(cx).value());
                    if cur_repo.as_deref() == Some(repo_key.as_str()) {
                        let cur_setup = form.setup_script.read(cx).value().trim().to_owned();
                        let cur_archive = form.archive_script.read(cx).value().trim().to_owned();
                        if cur_setup == setup_trimmed && cur_archive == archive_trimmed {
                            form.loaded_repo = Some(repo_key);
                            form.loaded_setup = setup_trimmed;
                            form.loaded_archive = archive_trimmed;
                        } else {
                            form.loaded_repo = None;
                        }
                    }
                    form.status = Some((text::HOOKS_SAVED.get().into(), false));
                }
                cx.defer(notify_external_change);
                cx.notify();
                Ok(())
            }
            Err(err) => {
                let msg = err.to_string();
                if let Some(form) = &mut self.workflow_form {
                    form.status = Some((msg.clone().into(), true));
                }
                cx.notify();
                Err(msg)
            }
        }
    }

    fn render_workflows_tab(
        &mut self,
        s: &Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use gpui_component::Sizable as _;
        self.ensure_workflow_form(window, cx);
        let p = crate::ui::palette(cx);
        let qc_count =
            text::QUICK_COMMANDS_COUNT.format(&[("count", &s.quick_commands.len().to_string())]);
        let wh_count = text::WEBHOOKS_COUNT.format(&[("count", &s.webhooks.len().to_string())]);
        let dir = state_dir(cx);
        let orch = dir
            .as_deref()
            .map(krill_proto::orchestration::OrchestrationStore::load_cached)
            .unwrap_or_default();
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
        let reset = self.confirm_button("admin-orch-reset", text::ORCH_RESET.get(), cx, |_, cx| {
            if let Some(dir) = state_dir(cx) {
                krill_proto::orchestration::OrchestrationStore::mutate(&dir, |store| {
                    store.reset(None);
                    Ok(())
                })
                .ok();
                cx.defer(notify_external_change);
            }
        });
        let mut orch_card = layout::card(p).flex().flex_col().gap_2().child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    layout::label(p, text::ORCH_TITLE.get(), None).child(
                        div()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(orch_summary),
                    ),
                )
                .child(layout::controls().child(reset)),
        );
        for task in orch.tasks.iter().take(6) {
            let line = format!(
                "[{}] {} · {} ({}) deps={:?}",
                task.run_id,
                task.id,
                task.title,
                task.status.as_str(),
                task.deps
            );
            orch_card = orch_card.child(
                div()
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_XS)
                    .child(line),
            );
        }
        let lines_btns = [50u16, 100, 250, 500].into_iter().map(|lines| {
            let id = format!("admin-context-lines-{lines}");
            let label = lines.to_string();
            let active = s.copy_context_lines == lines;
            layout::segment(gpui::ElementId::Name(id.clone().into()), label, active)
                .debug_selector(move || id.clone())
                .on_click(move |_, _, cx| {
                    edit(cx, |s| s.copy_context_lines = lines);
                })
        });
        let lines_row = layout::row(p)
            .child(layout::label(
                p,
                text::CONTEXT_LINES.get(),
                Some(text::CONTEXT_LINES_DESC.get()),
            ))
            .child(layout::controls().child(layout::segmented_control(p, lines_btns)));

        let mut qc_card = layout::card(p)
            .flex()
            .flex_col()
            .gap_2()
            .child(layout::label(p, qc_count, None));
        for (idx, cmd) in s.quick_commands.iter().enumerate() {
            let rm_id = format!("admin-qc-remove-{idx}");
            let summary = format!("{} → {}", cmd.name, cmd.text);
            qc_card = qc_card.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
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
                            .text_size(crate::ui::TEXT_XS)
                            .truncate()
                            .child(summary),
                    )
                    .child(
                        layout::controls().child(
                            layout::secondary(
                                gpui::ElementId::Name(rm_id.clone().into()),
                                text::ACCOUNT_REMOVE.get(),
                            )
                            .debug_selector(move || rm_id.clone())
                            .on_click(move |_, _, cx| {
                                edit(cx, |s| {
                                    if idx < s.quick_commands.len() {
                                        s.quick_commands.remove(idx);
                                    }
                                });
                            }),
                        ),
                    ),
            );
        }

        let clear = self.confirm_button(
            "admin-webhook-clear",
            text::WEBHOOK_CLEAR.get(),
            cx,
            |_, cx| edit(cx, |s| s.webhooks.clear()),
        );
        let mut wh_card = layout::card(p).flex().flex_col().gap_2().child(
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(layout::label(p, wh_count, None))
                .child(
                    layout::controls()
                        .child(
                            layout::secondary(
                                "admin-webhook-add-local",
                                text::WEBHOOK_ADD_LOCAL.get(),
                            )
                            .debug_selector(|| "admin-webhook-add-local".into())
                            .on_click(|_, _, cx| {
                                edit(cx, |s| {
                                    let n = s.webhooks.len().saturating_add(1);
                                    s.webhooks.push(crate::webhook::Endpoint::Ntfy {
                                        url: format!("http://127.0.0.1:8080/minikrill-hook-{n}"),
                                    });
                                });
                            }),
                        )
                        .child(clear),
                ),
        );
        for (idx, ep) in s.webhooks.iter().enumerate() {
            let rm_id = format!("admin-webhook-remove-{idx}");
            let label = format!("#{} · {:?}", idx + 1, ep);
            wh_card = wh_card.child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
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
                            .truncate()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_XS)
                            .child(label),
                    )
                    .child(
                        layout::controls().child(
                            layout::secondary(
                                gpui::ElementId::Name(rm_id.clone().into()),
                                text::ACCOUNT_REMOVE.get(),
                            )
                            .debug_selector(move || rm_id.clone())
                            .on_click(move |_, _, cx| {
                                edit(cx, |s| {
                                    if idx < s.webhooks.len() {
                                        s.webhooks.remove(idx);
                                    }
                                });
                            }),
                        ),
                    ),
            );
        }
        let files_row = layout::row(p)
            .child(layout::label(
                p,
                text::FILES_TITLE.get(),
                Some(text::FILES_DESC.get()),
            ))
            .child(
                layout::controls().child(
                    layout::secondary("workflow-open-settings", text::OPEN_SETTINGS.get())
                        .debug_selector(|| "workflow-open-settings".into())
                        .on_click(|_, _, cx| {
                            let body = cx.global::<Config>().settings.to_toml();
                            open_file(SETTINGS_FILE, &body, cx);
                        }),
                ),
            );

        let wt_store = dir
            .as_deref()
            .map(krill_proto::orchestration::WorktreeStore::load_cached)
            .unwrap_or_default();

        let mut symlinks_card = layout::card(p)
            .id("admin-worktree-symlinks-card")
            .debug_selector(|| "admin-worktree-symlinks-card".into())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(layout::label(
                p,
                text::SYMLINKS_TITLE.get(),
                Some(text::SYMLINKS_DESC.get()),
            ));

        let mut hooks_card = layout::card(p)
            .id("admin-repo-hooks-card")
            .debug_selector(|| "admin-repo-hooks-card".into())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(layout::label(
                p,
                text::HOOKS_TITLE.get(),
                Some(text::HOOKS_DESC.get()),
            ));

        if let Some(form) = &self.workflow_form {
            let repo_input = form.repo_path.clone();
            let symlink_input = form.symlink_path.clone();
            let setup_input = form.setup_script.clone();
            let archive_input = form.archive_script.clone();
            let status = form.status.clone();

            symlinks_card = symlinks_card
                .child(
                    div()
                        .id("admin-workflow-repo-input")
                        .debug_selector(|| "admin-workflow-repo-input".into())
                        .child(gpui_component::input::Input::new(&repo_input).small()),
                )
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
                    let mut row = div().flex().items_center().gap_1p5();
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

            let mut symlink_rows = Vec::new();
            for (repo_key, paths) in &wt_store.symlinks {
                for rel_path in paths {
                    symlink_rows.push((repo_key.clone(), rel_path.clone()));
                }
            }
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
                for (idx, (repo_key, rel_path)) in symlink_rows.into_iter().enumerate() {
                    let rm_id = format!("admin-symlink-remove-{idx}");
                    let line = format!("{repo_key} → {rel_path}");
                    let repo_cl = repo_key.clone();
                    let rel_cl = rel_path.clone();
                    symlinks_card = symlinks_card.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .flex()
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
                                    .text_size(crate::ui::TEXT_XS)
                                    .truncate()
                                    .child(line),
                            )
                            .child(
                                layout::controls().child(
                                    layout::secondary(
                                        gpui::ElementId::Name(rm_id.clone().into()),
                                        text::ACCOUNT_REMOVE.get(),
                                    )
                                    .debug_selector(move || rm_id.clone())
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.remove_worktree_symlink_for(&repo_cl, &rel_cl, cx);
                                        },
                                    )),
                                ),
                            ),
                    );
                }
            }

            hooks_card = hooks_card
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_size(crate::ui::TEXT_XS)
                        .child(
                            div()
                                .text_color(p.muted_foreground)
                                .child(text::HOOKS_SETUP_LABEL.get()),
                        )
                        .child(
                            div()
                                .id("admin-repo-setup-input")
                                .debug_selector(|| "admin-repo-setup-input".into())
                                .child(gpui_component::input::Input::new(&setup_input).small()),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_size(crate::ui::TEXT_XS)
                        .child(
                            div()
                                .text_color(p.muted_foreground)
                                .child(text::HOOKS_ARCHIVE_LABEL.get()),
                        )
                        .child(
                            div()
                                .id("admin-repo-archive-input")
                                .debug_selector(|| "admin-repo-archive-input".into())
                                .child(gpui_component::input::Input::new(&archive_input).small()),
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
                                .on_click(cx.listener(|this, _, window, cx| {
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
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.sync_workflow_repo_hooks(window, cx);
                                    let Some(form) = &this.workflow_form else {
                                        return;
                                    };
                                    let repo = form.repo_path.read(cx).value().to_string();
                                    let setup = form.setup_script.read(cx).value().to_string();
                                    let archive = form.archive_script.read(cx).value().to_string();
                                    this.save_repo_hooks_for(
                                        &repo,
                                        Some(&setup),
                                        Some(&archive),
                                        cx,
                                    )
                                    .ok();
                                })),
                        ),
                );

            let mut hook_repos: std::collections::BTreeSet<String> =
                wt_store.setup_scripts.keys().cloned().collect();
            hook_repos.extend(wt_store.archive_scripts.keys().cloned());
            for (idx, repo_key) in hook_repos.into_iter().enumerate() {
                let setup_val = wt_store
                    .setup_script_for(&repo_key)
                    .unwrap_or_else(|| "—".to_owned());
                let archive_val = wt_store
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
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_XS)
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
            .child(files_row)
    }

    fn render_resources_tab(&self, s: &Settings, cx: &mut Context<Self>) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let row = || layout::row(p);
        let label = |title: &'static str, desc: &'static str| layout::label(p, title, Some(desc));
        // 구 데몬이면 토글 대신 업데이트 안내만 보인다(요청을 보내지 않는다).
        let recording_controls = if self.history_pause_supported == Some(false) {
            div()
                .text_size(crate::ui::TEXT_XS)
                .text_color(p.warning)
                .child(text::RECORDING_UPDATE_NEEDED.get())
        } else {
            let view = cx.entity();
            layout::controls().child(layout::switch(
                p,
                "admin-recording-paused",
                !s.recording_paused,
                false,
                move |active, _, cx| {
                    view.update(cx, |this, cx| {
                        this.set_recording_paused(!active, cx);
                    });
                },
            ))
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
            .child(
                row()
                    .child(label(
                        crate::security::text::SECURE_KEYBOARD_MENU.get(),
                        text::SECURE_KB_DESC.get(),
                    ))
                    .child(layout::controls().child(layout::switch(
                        p,
                        "admin-secure-kb-toggle",
                        s.secure_keyboard_entry,
                        false,
                        |next, _, cx| {
                            edit(cx, |s| s.secure_keyboard_entry = next);
                            crate::platform::set_secure_event_input(next);
                        },
                    ))),
            )
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
                    .text_size(crate::ui::TEXT_XS)
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
                    .font_family(crate::ui::FONT_MONO)
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
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_XS)
                    .child(status.clone()),
            );
        }
        root = root.child(self.render_space_section(cx));
        root
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_component::{Icon, IconName, Sizable as _};
        window.set_window_title(text::TITLE.get());
        let (s, error) = cx.try_global::<Config>().map_or_else(
            || (Settings::default(), None),
            |c| (c.settings.clone(), c.error.clone()),
        );
        let p = crate::ui::palette(cx);
        let current_tab = self.tab;

        let search_input = self.search_input(window, cx);
        let matches = search::search_tabs(&search_input.read(cx).value());
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
                    AdminTab::Shortcuts,
                    AdminTab::Ssh,
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
            .w(crate::ui::SETTINGS_NAV_WIDTH)
            .h_full()
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
                            if let Some((input, _)) = &this.search
                                && !input.read(cx).value().is_empty()
                            {
                                input.update(cx, |s, cx| s.set_value("", window, cx));
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
                            .suffix(
                                div()
                                    .px_1()
                                    .rounded(crate::ui::RADIUS_XS)
                                    .bg(p.muted)
                                    .font_family(crate::ui::FONT_MONO)
                                    .text_size(crate::ui::TEXT_2XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::SEARCH_HINT_CMD_F.get()),
                            ),
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
            let mut group_col = div().flex().flex_col().gap_1().child(
                div()
                    .px_2()
                    .pt_1()
                    .text_size(crate::ui::TEXT_2XS)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(p.muted_foreground)
                    .child(group_label),
            );
            for tab in tabs {
                let sel = tab.selector();
                let selected = tab == current_tab;
                group_col = group_col.child(
                    div()
                        .id(sel)
                        .debug_selector(move || sel.into())
                        .h(px(30.))
                        .px_2()
                        .rounded(crate::ui::RADIUS_SM)
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(if selected {
                            p.foreground
                        } else {
                            p.muted_foreground
                        })
                        .when(selected, |d| {
                            d.bg(p.sidebar_accent).font_weight(gpui::FontWeight::MEDIUM)
                        })
                        .hover(|d| d.bg(p.sidebar_accent).text_color(p.foreground))
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

        let host_bar = div()
            .id("admin-host-scope-bar")
            .debug_selector(|| "admin-host-scope-bar".into())
            .w_full()
            .px_6()
            .py_2p5()
            .border_b_1()
            .border_color(p.border)
            .bg(p.card)
            .flex()
            .items_center()
            .gap_3()
            .flex_wrap()
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(p.muted_foreground)
                    .child(text::HOST_SCOPE_LABEL.get()),
            )
            .child(
                div()
                    .px_2p5()
                    .py_1()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.background)
                    .border_1()
                    .border_color(p.border)
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_size(crate::ui::TEXT_XS)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(Icon::new(IconName::SquareTerminal).xsmall())
                    .child(text::HOST_LOCAL_MAC.get())
                    .child(Icon::new(IconName::ChevronDown).xsmall()),
            )
            .child(
                div()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::HOST_SCOPE_DESC.get()),
            );

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

        let body = match current_tab {
            AdminTab::General => Self::render_general_tab(&s, cx),
            AdminTab::Terminal => Self::render_terminal_tab(&s, cx),
            AdminTab::Accounts => self.render_accounts_tab(cx),
            AdminTab::Agents => self.render_agents_tab(&s, window, cx),
            AdminTab::Ssh => self.render_ssh_tab(window, cx),
            AdminTab::Workflows => self.render_workflows_tab(&s, window, cx),
            AdminTab::Shortcuts => self.render_shortcuts_tab(cx),
            AdminTab::Resources => self.render_resources_tab(&s, cx),
            AdminTab::Mobile => self.render_mobile_tab(window, cx),
        }
        .w_full()
        .min_w_0();

        let page_header = div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .justify_between()
            .pb_2()
            .border_b_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(current_tab.icon_element().small())
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(current_tab.label()),
                    ),
            );

        div()
            .id("admin-center")
            .debug_selector(|| "admin-center".into())
            .key_context(SEARCH_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.focus_search(window, cx);
            }))
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape"
                    && crate::orca_import::state(cx).is_some_and(|st| st.modal_open)
                {
                    crate::orca_import::close_modal(cx);
                    cx.stop_propagation();
                    return;
                }
                if this.schedule_modal.is_some() && ev.keystroke.key == "escape" {
                    this.close_schedule_modal(cx);
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
                    && this.search_query(cx).is_empty()
                    && this.shortcuts.target.is_none()
                {
                    cx.stop_propagation();
                    close_settings_page_for(Some(cx.entity_id()), window, cx);
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
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .child(host_bar)
                        .child(
                            div()
                                .id("admin-content")
                                .flex_1()
                                .min_w_0()
                                .overflow_y_scroll()
                                .px_8()
                                .py_6()
                                .child(
                                    div()
                                        .id("admin-content-column")
                                        .debug_selector(|| "admin-content-column".into())
                                        // OWE-719·OWE-793: 왼쪽 정렬 + 창 폭에 맞춰 유동 확장하되 초대형 화면에서 과도한 행 길이만 제한.
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
                                        })),
                                ),
                        ),
                ),
            )
    }
}

pub(crate) mod layout;
mod mobile;
pub mod schedules;
mod search;
mod shortcuts;
pub mod space;
#[cfg(test)]
mod tests;
