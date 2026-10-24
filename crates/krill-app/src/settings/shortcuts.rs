//! 단축키 탭(OWE-660 · OWE-799): 기본 키맵(`Config.defaults`)과 `keys.toml`을 병합해 적용된 키맵으로
//! 기능 영역별(워크트리·탭/창·터미널·에이전트·오케스트레이션/패널·일반) 그룹과 행을 만든다.
//! 상단 로컬 검색과 상태 필터(`전체` / `수정됨`)로 즉시 거르고, 키캡을 누르면 인라인 기록 모드가 되어
//! 다음 키 조합을 `keys.toml`에 쓰고 바로 다시 적용한다(Esc 취소).
//! 행별 초기화는 그 액션의 `keys.toml` 항목을 지우고, 전체 초기화는 모든 재정의를 지운다.
//!
//! `keys.toml`은 공용 원문 편집기로 고쳐 인라인 표·여러 줄 문자열에서도
//! 미지 키·주석·빈 줄·바꾸지 않은 항목의 순서를 보존한다.
use std::path::Path;

use gpui::{
    App, Context, FontWeight, KeyBinding, KeyDownEvent, Keystroke, SharedString, Window, div,
    prelude::*,
};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};

use super::{
    Config, KEYS_FILE, Keys, SettingsView, close_settings_page_for, layout, open_file, text,
};
use crate::i18n::{Language, Message};
use crate::ui::BadgeTone;

/// 단축키 기능 영역 카테고리(OWE-799).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Category {
    Worktree,
    Tabs,
    Terminal,
    Agents,
    Panels,
    General,
}

impl Category {
    pub(super) const ALL: &[Self] = &[
        Self::Worktree,
        Self::Tabs,
        Self::Terminal,
        Self::Agents,
        Self::Panels,
        Self::General,
    ];

    pub(super) const fn selector(self) -> &'static str {
        match self {
            Self::Worktree => "sc-group-worktree",
            Self::Tabs => "sc-group-tabs",
            Self::Terminal => "sc-group-terminal",
            Self::Agents => "sc-group-agents",
            Self::Panels => "sc-group-panels",
            Self::General => "sc-group-general",
        }
    }

    pub(super) const fn message(self) -> Message {
        match self {
            Self::Worktree => text::SC_GROUP_WORKTREE,
            Self::Tabs => text::SC_GROUP_TABS,
            Self::Terminal => text::SC_GROUP_TERMINAL,
            Self::Agents => text::SC_GROUP_AGENTS,
            Self::Panels => text::SC_GROUP_PANELS,
            Self::General => text::SC_GROUP_GENERAL,
        }
    }

    pub(super) fn label(self) -> &'static str {
        self.message().get()
    }
}

/// 단축키 상태 필터(OWE-799): 전체 또는 기본값에서 변경된 단축키만.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ShortcutFilter {
    #[default]
    All,
    Modified,
}

/// 단축키 탭 행 순서와 설명. 설명의 `{n}`은 액션 이름 끝 숫자로 채운다. 기본 키맵에도 적용된 키맵에도
/// 없는 액션(예: 셸 없는 테스트)은 행을 그리지 않는다.
pub(super) const ACTIONS: &[(&str, Message)] = &[
    ("shell::NewTab", text::SC_NEW_TAB),
    ("shell::NewAgentTab", text::SC_NEW_AGENT_TAB),
    ("shell::NewAgent1", text::SC_NEW_AGENT_N),
    ("shell::NewAgent2", text::SC_NEW_AGENT_N),
    ("shell::NewAgent3", text::SC_NEW_AGENT_N),
    ("shell::NewAgent4", text::SC_NEW_AGENT_N),
    ("shell::CloseTab", text::SC_CLOSE_TAB),
    ("shell::ReopenClosedTab", text::SC_REOPEN_TAB),
    ("shell::NextTab", text::SC_NEXT_TAB),
    ("shell::PrevTab", text::SC_PREV_TAB),
    ("shell::ActivateTab1", text::SC_ACTIVATE_TAB),
    ("shell::ActivateTab2", text::SC_ACTIVATE_TAB),
    ("shell::ActivateTab3", text::SC_ACTIVATE_TAB),
    ("shell::ActivateTab4", text::SC_ACTIVATE_TAB),
    ("shell::ActivateTab5", text::SC_ACTIVATE_TAB),
    ("shell::ActivateTab6", text::SC_ACTIVATE_TAB),
    ("shell::ActivateTab7", text::SC_ACTIVATE_TAB),
    ("shell::ActivateTab8", text::SC_ACTIVATE_TAB),
    ("shell::ActivateTab9", text::SC_ACTIVATE_TAB),
    ("shell::SplitRight", text::SC_SPLIT_RIGHT),
    ("shell::SplitDown", text::SC_SPLIT_DOWN),
    ("shell::ClosePane", text::SC_CLOSE_PANE),
    ("shell::FocusNextPane", text::SC_NEXT_PANE),
    ("shell::FocusPrevPane", text::SC_PREV_PANE),
    ("shell::FocusPaneLeft", text::SC_PANE_LEFT),
    ("shell::FocusPaneRight", text::SC_PANE_RIGHT),
    ("shell::FocusPaneUp", text::SC_PANE_UP),
    ("shell::FocusPaneDown", text::SC_PANE_DOWN),
    ("shell::ToggleZoomPane", text::SC_ZOOM_PANE),
    ("shell::ClearPane", text::SC_CLEAR),
    ("shell::SelectAll", text::SC_SELECT_ALL),
    ("shell::Find", text::SC_FIND),
    ("shell::FindNext", text::SC_FIND_NEXT),
    ("shell::FindPrev", text::SC_FIND_PREV),
    ("shell::Copy", text::SC_COPY),
    ("shell::Paste", text::SC_PASTE),
    ("shell::ToggleSidebar", text::SC_SIDEBAR),
    ("shell::FocusSidebar", text::SC_FOCUS_SIDEBAR),
    ("shell::TogglePanel", text::SC_PANEL),
    ("shell::ToggleChanges", text::SC_CHANGES),
    ("shell::ToggleExplorer", text::SC_EXPLORER),
    ("shell::ToggleSearch", text::SC_WORKSPACE_SEARCH),
    ("shell::Jump", text::SC_JUMP),
    ("shell::CommandPalette", text::SC_COMMAND_PALETTE),
    ("shell::PrevSession", text::SC_PREV_SESSION),
    ("shell::NextSession", text::SC_NEXT_SESSION),
    ("shell::NavigateBack", text::SC_NAV_BACK),
    ("shell::NavigateForward", text::SC_NAV_FORWARD),
    ("shell::NewWorktree", text::SC_WORKTREE),
    ("popout::FloatTerminal", text::SC_FLOAT_TERMINAL),
    ("popout::DashboardPopout", text::SC_DASHBOARD),
    ("krill::NewWindow", text::SC_NEW_WINDOW),
    ("krill::CloseWindow", text::SC_CLOSE_WINDOW),
    ("shell::Quit", text::SC_QUIT),
    ("settings::OpenSettings", text::SC_SETTINGS),
    ("settings::ZoomIn", text::SC_ZOOM_IN),
    ("settings::ZoomOut", text::SC_ZOOM_OUT),
    ("settings::ZoomReset", text::SC_ZOOM_RESET),
];

/// 액션을 6개 기능 영역 카테고리로 분류한다(OWE-799).
pub(super) fn action_category(action: &str) -> Category {
    match action {
        "shell::NewWorktree"
        | "shell::PrevSession"
        | "shell::NextSession"
        | "shell::Jump"
        | "shell::NavigateBack"
        | "shell::NavigateForward" => Category::Worktree,
        "shell::NewTab"
        | "shell::CloseTab"
        | "shell::ReopenClosedTab"
        | "shell::NextTab"
        | "shell::PrevTab"
        | "shell::ActivateTab1"
        | "shell::ActivateTab2"
        | "shell::ActivateTab3"
        | "shell::ActivateTab4"
        | "shell::ActivateTab5"
        | "shell::ActivateTab6"
        | "shell::ActivateTab7"
        | "shell::ActivateTab8"
        | "shell::ActivateTab9"
        | "krill::NewWindow"
        | "krill::CloseWindow" => Category::Tabs,
        "shell::SplitRight"
        | "shell::SplitDown"
        | "shell::ClosePane"
        | "shell::FocusNextPane"
        | "shell::FocusPrevPane"
        | "shell::ToggleZoomPane"
        | "shell::ClearPane"
        | "shell::SelectAll"
        | "shell::Find"
        | "shell::Copy"
        | "shell::Paste"
        | "popout::FloatTerminal" => Category::Terminal,
        "shell::NewAgentTab" | "shell::NewAgent1" | "shell::NewAgent2" | "shell::NewAgent3"
        | "shell::NewAgent4" => Category::Agents,
        "shell::ToggleSidebar"
        | "shell::FocusSidebar"
        | "shell::TogglePanel"
        | "shell::ToggleExplorer"
        | "shell::ToggleSearch"
        | "popout::DashboardPopout" => Category::Panels,
        _ => Category::General,
    }
}

/// 기록 대상: 액션과 바꿀 기존 키(`None`이면 키 추가).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Target {
    action: &'static str,
    old: Option<String>,
}

/// 단축키 탭 상태. 탭을 바꾸면 버린다(가로채기 구독도 함께 끊긴다).
#[derive(Default)]
pub(super) struct Recorder {
    pub(super) target: Option<Target>,
    /// 충돌 확인 대기: 대상, 새 키, 그 키를 이미 쓰는 액션들.
    conflict: Option<(Target, String, Vec<&'static str>)>,
    pub(super) error: Option<SharedString>,
    intercept: Option<gpui::Subscription>,
    return_focus: Option<gpui::FocusHandle>,
    pub(super) filter: ShortcutFilter,
    pub(super) search: Option<(
        gpui::Entity<gpui_component::input::InputState>,
        gpui::Subscription,
    )>,
}

impl Recorder {
    pub(super) fn search_query(&self, cx: &App) -> String {
        self.search
            .as_ref()
            .map(|(input, _)| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    pub(super) fn clear_search(&mut self, window: &mut Window, cx: &mut App) {
        if let Some((input, _)) = &self.search {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
    }
}

/// (액션 이름, 정규화한 키) 쌍.
type Pair = (&'static str, String);

fn binding_pair(b: &KeyBinding) -> Pair {
    let key = b
        .keystrokes()
        .iter()
        .map(|k| k.inner().unparse())
        .collect::<Vec<_>>()
        .join(" ");
    (b.action().name(), key)
}

fn pairs<'a>(bindings: impl IntoIterator<Item = &'a KeyBinding>) -> Vec<Pair> {
    bindings.into_iter().map(binding_pair).collect()
}

/// 지금 적용된 키맵(기본 + `keys.toml`, [`super::bindings`] 결과).
fn live_pairs(cx: &App) -> Vec<Pair> {
    pairs(cx.key_bindings().borrow().bindings())
}

/// 키 문자열을 비교 가능한 형태로 맞춘다(`alt-cmd-1`과 `cmd-alt-1`은 같은 키).
pub(super) fn norm(key: &str) -> Option<String> {
    let keys = key
        .split_whitespace()
        .map(|k| Keystroke::parse(k).ok().map(|k| k.unparse()))
        .collect::<Option<Vec<_>>>()?;
    (!keys.is_empty()).then(|| keys.join(" "))
}

fn same(a: &str, b: &str) -> bool {
    norm(a).is_some_and(|a| Some(a) == norm(b))
}

/// 기능키 이름인지(F1~F35, 대소문자 무시). 기능키는 수식키 없이도 단축키가 된다.
fn is_function_key(key: &str) -> bool {
    key.strip_prefix(['f', 'F'])
        .is_some_and(|n| !n.is_empty() && n.len() <= 2 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// 녹음한 키가 단축키로 쓰이려면 수식키(⇧ 제외, ⌘/⌃/⌥ 중 하나 이상)가 필요한지(OWE-863).
/// 기능키(F1~F12 등)는 맨몸도 되지만, 문자·공백·Enter·Backspace·방향키 등은 맨몸이나 ⇧만으로는
/// 터미널 입력과 겹치므로 저장하지 않는다(Orca 정규화와 같은 규칙).
pub(super) fn needs_modifier(keystroke: &Keystroke) -> bool {
    if is_function_key(&keystroke.key) {
        return false;
    }
    let m = &keystroke.modifiers;
    !(m.platform || m.control || m.alt)
}

/// `action`의 키들(키맵 순서, 중복 제거).
fn keys_of(pairs: &[Pair], action: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (a, k) in pairs {
        if *a == action && !out.contains(k) {
            out.push(k.clone());
        }
    }
    out
}

/// 적용된 키 목록과 기본 키 목록이 다른지 판별한다.
pub(super) fn is_modified(current: &[String], default: &[String]) -> bool {
    let (mut a, mut b) = (current.to_vec(), default.to_vec());
    a.sort();
    b.sort();
    a != b
}

/// `key`를 이미 쓰는 다른 액션들(문맥은 따지지 않는다: 같은 키가 다른 곳에서 다른 일을 하면 알린다).
pub(super) fn conflicts(pairs: &[Pair], key: &str, action: &str) -> Vec<&'static str> {
    let Some(key) = norm(key) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (a, k) in pairs {
        if *k == key && *a != action && !out.contains(a) {
            out.push(*a);
        }
    }
    out
}

fn is_default(defaults: &[Pair], action: &str, key: &str) -> bool {
    norm(key).is_some_and(|key| defaults.iter().any(|(a, k)| *a == action && *k == key))
}

/// `action`의 기본 키 `key`만 끈다(`[disabled]`, OWE-863). 같은 키를 쓰는 다른 액션은 그대로다.
fn disable_default(keys: &mut Keys, action: &str, key: &str) {
    let list = keys.disabled.entry(action.to_owned()).or_default();
    if !list.iter().any(|k| same(k, key)) {
        list.push(key.to_owned());
    }
}

/// [`disable_default`]로 끈 `action`의 `key`를 되살린다.
fn enable_default(keys: &mut Keys, action: &str, key: &str) {
    if let Some(list) = keys.disabled.get_mut(action) {
        list.retain(|k| !same(k, key));
        if list.is_empty() {
            keys.disabled.remove(action);
        }
    }
}

/// `action`의 `old` 키를 `key`로 바꾼다(`old`가 `None`이면 추가). 기본 키를 바꿀 때는 그 액션의 기본 키만 끈다.
pub(super) fn set_key(
    keys: &mut Keys,
    defaults: &[Pair],
    action: &str,
    old: Option<&str>,
    key: &str,
) {
    keys.bindings.retain(|k, _| !same(k, key));
    enable_default(keys, action, key);
    if let Some(old) = old.filter(|o| !same(o, key)) {
        keys.bindings.retain(|k, _| !same(k, old));
        if is_default(defaults, action, old) {
            disable_default(keys, action, old);
        }
    }
    keys.bindings.insert(key.to_owned(), action.into());
}

/// `action`의 재정의(그 액션으로 가는 키, 그 액션의 끈 기본 키, 구 형식 `""`)를 지워 기본 키로 되돌린다.
pub(super) fn reset_action(keys: &mut Keys, defaults: &[Pair], action: &str) {
    keys.bindings.retain(|k, v| match v.as_str() {
        Some(v) if v == action => false,
        Some("") => !is_default(defaults, action, k),
        _ => true,
    });
    keys.disabled.remove(action);
}

/// `action`의 바인딩 하나만 지운다(OWE-863). 기본 키면 그 액션의 기본 키만 끈다(같은 키의 다른 액션은 유지).
pub(super) fn remove_key(keys: &mut Keys, defaults: &[Pair], action: &str, key: &str) {
    keys.bindings.retain(|k, _| !same(k, key));
    if is_default(defaults, action, key) {
        disable_default(keys, action, key);
    }
}

/// `action`의 현재 키를 모두 꺼 단축키를 비활성화한다(OWE-863). 초기화로 기본 키를 되돌릴 수 있다.
pub(super) fn disable_action(keys: &mut Keys, defaults: &[Pair], action: &str, current: &[String]) {
    for key in current {
        remove_key(keys, defaults, action, key);
    }
}

/// `original`의 `[bindings]`·`[disabled]` 항목을 `target`과 같게 고친 새 본문. 주석·빈 줄·남은 항목 순서는 그대로 둔다.
///
/// # Errors
/// 직렬화에 실패하면 오류(파일은 건드리지 않는다).
pub(super) fn rewrite(original: &str, target: &Keys) -> Result<String, String> {
    krill_proto::config_document::edit_toml(original, target).map_err(|e| e.to_string())
}

/// `keys.toml`을 읽어 `f`로 고치고 바꿔 단다. 없으면 템플릿에서 시작하고, 틀린 파일은 덮어쓰지 않는다.
///
/// # Errors
/// 읽기·파싱·쓰기 실패.
pub(super) fn edit_keys(dir: &Path, f: impl FnOnce(&mut Keys)) -> Result<(), String> {
    let path = dir.join(KEYS_FILE);
    let original = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => text::KEYS_TEMPLATE.get().to_owned(),
        Err(e) => return Err(e.to_string()),
    };
    let mut keys = Keys::parse(&original).map_err(|e| e.to_string())?;
    f(&mut keys);
    let body = rewrite(&original, &keys)?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // 1초 폴링이 반쯤 쓴 파일을 읽지 않게 고유 임시 파일에 쓰고 바꿔 단다(AS-08).
    super::write_settings(&path, &body).map_err(|e| e.to_string())
}

/// 키를 키캡 문구로(OWE-651 형식: `⇧⌘W`, `⌥⌘←`, `⇧⌘Enter`).
pub(super) fn keycap_text(key: &str) -> String {
    key.split_whitespace()
        .map(|k| Keystroke::parse(k).map_or_else(|_| k.to_owned(), |k| keystroke_text(&k)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn keystroke_text(k: &Keystroke) -> String {
    let m = k.modifiers;
    let mut out = String::new();
    for (on, sym) in [
        (m.function, "fn"),
        (
            m.control,
            if cfg!(target_os = "macos") {
                "⌃"
            } else {
                "Ctrl+"
            },
        ),
        (
            m.alt,
            if cfg!(target_os = "macos") {
                "⌥"
            } else {
                "Alt+"
            },
        ),
        (
            m.shift,
            if cfg!(target_os = "macos") {
                "⇧"
            } else {
                "Shift+"
            },
        ),
        (
            m.platform,
            if cfg!(target_os = "macos") {
                "⌘"
            } else if cfg!(windows) {
                "Win+"
            } else {
                "Super+"
            },
        ),
    ] {
        if on {
            out.push_str(sym);
        }
    }
    let key = match k.key.as_str() {
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "backspace" => "⌫".to_owned(),
        "escape" => "Esc".to_owned(),
        key => {
            let mut chars = key.chars();
            chars.next().map_or_else(String::new, |c| {
                c.to_uppercase().chain(chars).collect::<String>()
            })
        }
    };
    out.push_str(&key);
    out
}

/// 행 설명. 표에 없는 액션은 이름 그대로.
fn action_label(action: &str) -> String {
    ACTIONS.iter().find(|(a, _)| *a == action).map_or_else(
        || action.to_owned(),
        |(_, m)| {
            let n = action.trim_start_matches(|c: char| !c.is_ascii_digit());
            m.format(&[("n", n)])
        },
    )
}

fn action_label_in(action: &str, lang: Language) -> String {
    ACTIONS.iter().find(|(a, _)| *a == action).map_or_else(
        || action.to_owned(),
        |(_, m)| {
            let n = action.trim_start_matches(|c: char| !c.is_ascii_digit());
            m.in_language(lang).replace("{n}", n)
        },
    )
}

/// 로컬 검색어(`query`)가 액션 이름(한·영·식별자), 카테고리 이름(한·영), 또는 바인딩 키(원문·키캡 표기)와 맞는지 검사한다(OWE-799).
pub(super) fn matches_query(
    action: &str,
    current: &[String],
    default: &[String],
    query: &str,
) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let cat = action_category(action).message();
    let label_ko = action_label_in(action, Language::Ko).to_lowercase();
    let label_en = action_label_in(action, Language::En).to_lowercase();
    let cat_ko = cat.in_language(Language::Ko).to_lowercase();
    let cat_en = cat.in_language(Language::En).to_lowercase();
    let action_lower = action.to_lowercase();

    query.split_whitespace().all(|token| {
        action_lower.contains(token)
            || label_ko.contains(token)
            || label_en.contains(token)
            || cat_ko.contains(token)
            || cat_en.contains(token)
            || current.iter().chain(default.iter()).any(|k| {
                k.to_lowercase().contains(token) || keycap_text(k).to_lowercase().contains(token)
            })
    })
}

impl SettingsView {
    fn shortcut_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Entity<gpui_component::input::InputState> {
        if let Some((input, _)) = &self.shortcuts.search {
            return input.clone();
        }
        let input = cx.new(|cx| {
            gpui_component::input::InputState::new(window, cx)
                .placeholder(text::SC_SEARCH_PLACEHOLDER.get())
        });
        let sub = cx.subscribe(&input, |_, _, _: &gpui_component::input::InputEvent, cx| {
            cx.notify();
        });
        self.shortcuts.search = Some((input.clone(), sub));
        input
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(super) fn set_shortcut_search(
        &mut self,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = self.shortcut_search_input(window, cx);
        input.update(cx, |s, cx| s.set_value(value, window, cx));
        cx.notify();
    }

    pub(super) fn set_shortcut_filter(&mut self, filter: ShortcutFilter, cx: &mut Context<Self>) {
        if self.shortcuts.filter != filter {
            self.shortcuts.filter = filter;
            cx.notify();
        }
    }

    /// 키캡 클릭: 다음 키 조합을 가로챈다. 바인딩 매칭 전에 가로채므로 ⌘Q·⌘W 같은 키도 실행되지 않고 기록된다.
    pub(super) fn start_recording(
        &mut self,
        action: &'static str,
        old: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 검색 입력에 글자가 들어가지 않게 포커스를 창 루트로 옮긴다.
        self.shortcuts.return_focus = window.focused(cx);
        window.focus(&self.focus, cx);
        let view = cx.weak_entity();
        let id = window.window_handle().window_id();
        let intercept = cx.intercept_keystrokes(move |ev, window, cx| {
            if window.window_handle().window_id() != id {
                return;
            }
            cx.stop_propagation();
            let keystroke = ev.keystroke.clone();
            view.update(cx, |this, cx| this.on_recorded(&keystroke, cx))
                .ok();
        });
        self.shortcuts.target = Some(Target { action, old });
        self.shortcuts.conflict = None;
        self.shortcuts.error = None;
        self.shortcuts.intercept = Some(intercept);
        cx.notify();
    }

    pub(super) fn cancel_shortcut_conflict(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.shortcuts.conflict.take().is_none() {
            return false;
        }
        if let Some(focus) = self.shortcuts.return_focus.take() {
            window.focus(&focus, cx);
        }
        cx.notify();
        true
    }

    fn on_recorded(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        self.shortcuts.intercept = None;
        let Some(target) = self.shortcuts.target.take() else {
            return;
        };
        cx.notify();
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            return;
        }
        // OWE-863: 수식키 없는 일반 키(Backspace로 지우려는 경우 포함)는 터미널 입력과 겹치므로
        // 바인딩하지 않고 오류로 알린다. 지우기는 키 옆 ×, 끄기는 사용 안 함 버튼으로 한다.
        if needs_modifier(keystroke) {
            self.shortcuts.error = Some(text::SC_NEEDS_MODIFIER.get().into());
            cx.notify();
            return;
        }
        let key = keystroke.unparse();
        let others = conflicts(&live_pairs(cx), &key, target.action);
        if others.is_empty() {
            self.save_key(&target, &key, cx);
        } else {
            self.shortcuts.conflict = Some((target, key, others));
        }
    }

    fn save_key(&mut self, target: &Target, key: &str, cx: &mut Context<Self>) {
        let target = target.clone();
        let key = key.to_owned();
        self.write_keys(cx, move |t, defaults| {
            set_key(t, defaults, target.action, target.old.as_deref(), &key);
        });
    }

    fn write_keys(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Keys, &[Pair]) + Send + 'static,
    ) {
        let Some(c) = cx.try_global::<Config>() else {
            return;
        };
        let Some(dir) = c.dir.clone() else {
            return;
        };
        let defaults = pairs(&c.defaults);
        self.file_job(
            "keys-edit",
            move || edit_keys(&dir, |keys| f(keys, &defaults)),
            |this, result, cx| match result {
                Ok(()) => {
                    this.shortcuts.error = None;
                    super::reload(cx);
                }
                Err(error) => {
                    this.shortcuts.error =
                        Some(text::SC_WRITE_FAILED.format(&[("error", &error)]).into());
                }
            },
            cx,
        );
    }

    pub(super) fn confirm_conflict(&mut self, cx: &mut Context<Self>) {
        if let Some((target, key, _)) = self.shortcuts.conflict.take() {
            self.save_key(&target, &key, cx);
        }
    }

    pub(super) fn reset_shortcut(&mut self, action: &'static str, cx: &mut Context<Self>) {
        self.shortcuts.conflict = None;
        self.write_keys(cx, move |t, defaults| reset_action(t, defaults, action));
    }

    pub(super) fn reset_all_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.shortcuts.conflict = None;
        self.write_keys(cx, |t, _| {
            t.bindings.clear();
            t.disabled.clear();
        });
    }

    /// 바인딩 하나만 지운다(OWE-863). 기본 키면 그 액션의 기본 키만 끈다.
    pub(super) fn delete_key(&mut self, action: &'static str, key: &str, cx: &mut Context<Self>) {
        self.shortcuts.conflict = None;
        let key = key.to_owned();
        self.write_keys(cx, move |t, defaults| remove_key(t, defaults, action, &key));
    }

    /// 액션의 현재 키를 모두 꺼 단축키를 비활성화한다(OWE-863). 초기화로 되돌릴 수 있다.
    pub(super) fn disable_shortcut(
        &mut self,
        action: &'static str,
        current: &[String],
        cx: &mut Context<Self>,
    ) {
        self.shortcuts.conflict = None;
        let current = current.to_vec();
        self.write_keys(cx, move |t, defaults| {
            disable_action(t, defaults, action, &current);
        });
    }

    fn key_button(
        &self,
        action: &'static str,
        old: Option<String>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = crate::ui::palette(cx);
        let target = Target {
            action,
            old: old.clone(),
        };
        let recording = self.shortcuts.target.as_ref() == Some(&target);
        let label = if recording {
            text::SC_RECORDING.get().to_owned()
        } else {
            old.as_deref()
                .map_or_else(|| text::SC_ADD_KEY.get().to_owned(), keycap_text)
        };
        let id = format!("sc-{action}-{}", old.as_deref().unwrap_or("add"));
        let selector = id.clone();
        let old_for_key = old.clone();
        let cap = layout::keycap(p, label.clone()).when(recording || old.is_none(), |cap| {
            cap.font_family(crate::ui::FONT_UI)
        });
        let cap = if recording {
            cap.border_color(p.primary).text_color(p.primary)
        } else {
            cap
        };
        div()
            .id(SharedString::from(id))
            .debug_selector(move || selector.clone())
            // OWE-860: 키캡을 키보드 Tab·접근성 트리에 노출한다. Enter로도 기록 모드를 시작한다.
            .role(gpui::Role::Button)
            .aria_label(label.clone())
            .tab_index(0)
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "enter" {
                    this.start_recording(action, old_for_key.clone(), window, cx);
                }
            }))
            .cursor_pointer()
            .child(cap)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.start_recording(action, old.clone(), window, cx);
            }))
    }

    pub(super) fn render_shortcuts_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let search_input = self.shortcut_search_input(window, cx);
        let query = self.shortcuts.search_query(cx);
        let active_filter = self.shortcuts.filter;
        let live = live_pairs(cx);
        let defaults = cx
            .try_global::<Config>()
            .map_or_else(Vec::new, |c| pairs(&c.defaults));

        let indexed = |pairs: &[Pair]| {
            let mut index: std::collections::BTreeMap<&'static str, Vec<String>> =
                std::collections::BTreeMap::new();
            for (action, key) in pairs {
                let keys = index.entry(*action).or_default();
                if !keys.contains(key) {
                    keys.push(key.clone());
                }
            }
            index
        };
        let live_index = indexed(&live);
        let default_index = indexed(&defaults);
        let mut total_count = 0usize;
        let mut modified_count = 0usize;
        let mut visible_count = 0usize;

        for &(action, _) in ACTIONS {
            let current = live_index.get(action).cloned().unwrap_or_default();
            let default = default_index.get(action).cloned().unwrap_or_default();
            if current.is_empty() && default.is_empty() {
                continue;
            }
            total_count += 1;
            if is_modified(&current, &default) {
                modified_count += 1;
            }
        }

        let mut list = div().flex().flex_col().gap_4();
        for &cat in Category::ALL {
            let mut group_rows = Vec::new();
            for &(action, _) in ACTIONS {
                if action_category(action) != cat {
                    continue;
                }
                let current = live_index.get(action).cloned().unwrap_or_default();
                let default = default_index.get(action).cloned().unwrap_or_default();
                if current.is_empty() && default.is_empty() {
                    continue;
                }
                let modified = is_modified(&current, &default);
                if active_filter == ShortcutFilter::Modified && !modified {
                    continue;
                }
                if !matches_query(action, &current, &default, &query) {
                    continue;
                }
                visible_count += 1;

                let mut caps = layout::controls().flex_wrap().flex_shrink_1().min_w_0();
                for key in &current {
                    let del_id = format!("sc-del-{action}-{key}");
                    let del_selector = del_id.clone();
                    let key_owned = key.clone();
                    caps = caps.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(self.key_button(action, Some(key.clone()), cx))
                            .child(
                                layout::ghost(SharedString::from(del_id), "×")
                                    .debug_selector(move || del_selector.clone())
                                    .accessibility_label(text::SC_DELETE_KEY.get())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete_key(action, &key_owned, cx);
                                    })),
                            ),
                    );
                }
                caps = caps.child(self.key_button(action, None, cx));
                // OWE-863: 키를 모두 끈 행은 사용 안 함 힌트를, 키가 남은 행은 끄기 버튼을 보인다.
                if current.is_empty() {
                    if !default.is_empty() {
                        caps = caps.child(
                            div()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::SC_DISABLED_HINT.get()),
                        );
                    }
                } else {
                    let id = format!("sc-disable-{action}");
                    let selector = id.clone();
                    caps = caps.child(
                        layout::ghost(SharedString::from(id), text::SC_DISABLE.get())
                            .debug_selector(move || selector.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let live = live_pairs(cx);
                                let current = keys_of(&live, action);
                                this.disable_shortcut(action, &current, cx);
                            })),
                    );
                }
                let (mut a, mut b) = (current, default);
                a.sort();
                b.sort();
                if a != b {
                    let id = format!("sc-reset-{action}");
                    let selector = id.clone();
                    caps = caps.child(
                        layout::secondary(SharedString::from(id), text::SC_RESET.get())
                            .debug_selector(move || selector.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.reset_shortcut(action, cx);
                            })),
                    );
                }

                let mut left = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .text_size(crate::ui::TEXT_SM)
                            .font_weight(FontWeight::MEDIUM)
                            .child(action_label(action)),
                    );
                if modified {
                    let mod_id = format!("sc-modified-{action}");
                    left = left.child(
                        div()
                            .id(SharedString::from(mod_id.clone()))
                            .debug_selector(move || mod_id.clone())
                            .child(crate::ui::badge(
                                p,
                                BadgeTone::Muted,
                                text::SC_MODIFIED_BADGE.get(),
                            )),
                    );
                }

                let row_id = format!("sc-row-{action}");
                group_rows.push(
                    layout::row(p)
                        .id(SharedString::from(row_id.clone()))
                        .debug_selector(move || row_id.clone())
                        .flex_col()
                        .items_start()
                        .py_2()
                        .child(left)
                        .child(caps),
                );
            }

            if !group_rows.is_empty() {
                let sel = cat.selector();
                let header_id = match cat {
                    Category::Worktree => "sc-header-worktree",
                    Category::Tabs => "sc-header-tabs",
                    Category::Terminal => "sc-header-terminal",
                    Category::Agents => "sc-header-agents",
                    Category::Panels => "sc-header-panels",
                    Category::General => "sc-header-general",
                };
                list = list.child(
                    div()
                        .id(sel)
                        .debug_selector(move || sel.into())
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(layout::section_title(p, header_id, cat.label()))
                        .children(group_rows),
                );
            }
        }

        if visible_count == 0 {
            list = list.child(
                layout::row(p)
                    .id("sc-empty")
                    .debug_selector(|| "sc-empty".into())
                    .child(
                        div()
                            .text_size(crate::ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::SC_EMPTY.get()),
                    ),
            );
        }

        let clear_btn = (!query.is_empty()).then(|| {
            layout::ghost("sc-search-clear", text::SC_SEARCH_CLEAR.get())
                .debug_selector(|| "sc-search-clear".into())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.shortcuts.clear_search(window, cx);
                    cx.notify();
                }))
        });

        let filter_bar = layout::row(p)
            .id("sc-filter-bar")
            .debug_selector(|| "sc-filter-bar".into())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("sc-search")
                            .debug_selector(|| "sc-search".into())
                            .flex_1()
                            .min_w_0()
                            .capture_action(cx.listener(
                                |this, _: &gpui_component::input::Escape, window, cx| {
                                    cx.stop_propagation();
                                    if !this.shortcuts.search_query(cx).is_empty() {
                                        this.shortcuts.clear_search(window, cx);
                                    } else if this.shortcuts.target.is_none() {
                                        close_settings_page_for(Some(cx.entity_id()), window, cx);
                                    }
                                },
                            ))
                            .child(
                                gpui_component::input::Input::new(&search_input)
                                    .small()
                                    .prefix(Icon::new(IconName::Search).xsmall()),
                            ),
                    )
                    .children(clear_btn),
            )
            .child(
                layout::controls()
                    .child(
                        div()
                            .id("sc-count")
                            .debug_selector(|| "sc-count".into())
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(text::SC_COUNT_SUMMARY.format(&[
                                ("visible", &visible_count.to_string()),
                                ("total", &total_count.to_string()),
                            ])),
                    )
                    .child(layout::segmented_control(
                        p,
                        [
                            layout::segment(
                                p,
                                "sc-filter-all",
                                text::SC_FILTER_ALL.get(),
                                active_filter == ShortcutFilter::All,
                            )
                            .debug_selector(|| "sc-filter-all".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_shortcut_filter(ShortcutFilter::All, cx);
                            })),
                            layout::segment(
                                p,
                                "sc-filter-modified",
                                text::SC_FILTER_MODIFIED.get(),
                                active_filter == ShortcutFilter::Modified,
                            )
                            .debug_selector(|| "sc-filter-modified".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_shortcut_filter(ShortcutFilter::Modified, cx);
                            })),
                        ],
                    )),
            );

        let conflict = self.shortcuts.conflict.as_ref().map(|(_, key, others)| {
            let names = others
                .iter()
                .map(|a| action_label(a))
                .collect::<Vec<_>>()
                .join(", ");
            let msg = text::SC_CONFLICT.format(&[("key", &keycap_text(key)), ("actions", &names)]);
            layout::row(p)
                .debug_selector(|| "sc-conflict".into())
                .border_color(p.warning.opacity(0.3))
                .bg(p.warning.opacity(0.1))
                .child(layout::label(p, msg, None))
                .child(
                    layout::controls()
                        .child(
                            layout::primary("sc-conflict-save", text::SC_CONFLICT_SAVE.get())
                                .debug_selector(|| "sc-conflict-save".into())
                                .on_click(cx.listener(|this, _, _, cx| this.confirm_conflict(cx))),
                        )
                        .child(
                            layout::ghost("sc-conflict-cancel", text::SC_CANCEL.get())
                                .debug_selector(|| "sc-conflict-cancel".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.shortcuts.conflict = None;
                                    cx.notify();
                                })),
                        ),
                )
        });
        let error = self
            .shortcuts
            .error
            .clone()
            .map(|e| crate::ui::error_banner(p, e));

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                layout::row(p)
                    .child(layout::label(p, text::SHORTCUTS_TITLE.get(), None))
                    .child(
                        layout::controls()
                            .child(
                                layout::secondary("shortcuts-reset-all", text::SC_RESET_ALL.get())
                                    .disabled(modified_count == 0)
                                    .debug_selector(|| "shortcuts-reset-all".into())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.reset_all_shortcuts(cx);
                                    })),
                            )
                            .child(
                                layout::secondary("shortcuts-open-keys", text::OPEN_KEYS.get())
                                    .debug_selector(|| "shortcuts-open-keys".into())
                                    .on_click(|_, _, cx| {
                                        open_file(KEYS_FILE, text::KEYS_TEMPLATE.get(), cx);
                                    }),
                            ),
                    ),
            )
            .child(filter_bar)
            .children(conflict)
            .children(error)
            .child(list)
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod platform_tests {
    #[test]
    fn keycaps_show_the_effective_windows_modifiers() {
        assert_eq!(super::keycap_text("ctrl-shift-t"), "Ctrl+Shift+T");
        assert_eq!(super::keycap_text("ctrl-alt-shift-w"), "Ctrl+Alt+Shift+W");
        assert_eq!(super::keycap_text("ctrl-alt-left"), "Ctrl+Alt+←");
    }
}
