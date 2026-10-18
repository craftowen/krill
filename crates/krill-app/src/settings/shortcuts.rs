//! 단축키 탭(OWE-660): 기본 키맵(`Config.defaults`)과 `keys.toml`을 병합해 적용된 키맵으로 행을 만든다.
//! 키캡을 누르면 기록 모드가 되어 다음 키 조합을 `keys.toml`에 쓰고 바로 다시 적용한다(Esc 취소).
//! 행별 초기화는 그 액션의 `keys.toml` 항목을 지운다. 다른 액션과 겹치는 키는 저장 전에 확인을 받는다.
//!
//! `keys.toml`은 공용 원문 편집기로 고쳐 인라인 표·여러 줄 문자열에서도
//! 미지 키·주석·빈 줄·바꾸지 않은 항목의 순서를 보존한다.
use std::path::Path;

use gpui::{
    App, Context, KeyBinding, KeyDownEvent, Keystroke, SharedString, Window, div, prelude::*,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};

use super::{Config, KEYS_FILE, Keys, SettingsView, layout, open_file, text};
use crate::i18n::Message;

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
    ("shell::ToggleZoomPane", text::SC_ZOOM_PANE),
    ("shell::ClearPane", text::SC_CLEAR),
    ("shell::SelectAll", text::SC_SELECT_ALL),
    ("shell::Find", text::SC_FIND),
    ("shell::Copy", text::SC_COPY),
    ("shell::Paste", text::SC_PASTE),
    ("shell::ToggleSidebar", text::SC_SIDEBAR),
    ("shell::FocusSidebar", text::SC_FOCUS_SIDEBAR),
    ("shell::TogglePanel", text::SC_PANEL),
    ("shell::ToggleExplorer", text::SC_EXPLORER),
    ("shell::ToggleSearch", text::SC_WORKSPACE_SEARCH),
    ("shell::Jump", text::SC_JUMP),
    ("shell::PrevSession", text::SC_PREV_SESSION),
    ("shell::NextSession", text::SC_NEXT_SESSION),
    ("shell::NavigateBack", text::SC_NAV_BACK),
    ("shell::NavigateForward", text::SC_NAV_FORWARD),
    ("shell::NewWorktree", text::SC_WORKTREE),
    ("popout::FloatTerminal", text::SC_FLOAT_TERMINAL),
    ("popout::DashboardPopout", text::SC_DASHBOARD),
    ("minikrill::NewWindow", text::SC_NEW_WINDOW),
    ("minikrill::CloseWindow", text::SC_CLOSE_WINDOW),
    ("shell::Quit", text::SC_QUIT),
    ("settings::OpenSettings", text::SC_SETTINGS),
    ("settings::ZoomIn", text::SC_ZOOM_IN),
    ("settings::ZoomOut", text::SC_ZOOM_OUT),
    ("settings::ZoomReset", text::SC_ZOOM_RESET),
];

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
    error: Option<SharedString>,
    intercept: Option<gpui::Subscription>,
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

/// `action`의 `old` 키를 `key`로 바꾼다(`old`가 `None`이면 추가). 기본 키를 바꿀 때는 그 키를 `""`로 끈다.
// ponytail: `""`는 그 키의 모든 기본 바인딩을 끈다(⌘W는 창 닫기와 대시보드 닫기를 함께). 액션별 끄기가 필요하면 keys.toml 형식을 넓힌다.
pub(super) fn set_key(
    table: &mut toml::Table,
    defaults: &[Pair],
    action: &str,
    old: Option<&str>,
    key: &str,
) {
    table.retain(|k, _| !same(k, key));
    if let Some(old) = old.filter(|o| !same(o, key)) {
        table.retain(|k, _| !same(k, old));
        if is_default(defaults, action, old) {
            table.insert(old.to_owned(), "".into());
        }
    }
    table.insert(key.to_owned(), action.into());
}

/// `action`의 재정의(그 액션으로 가는 키, 그 액션의 기본 키를 끈 `""`)를 지워 기본 키로 되돌린다.
pub(super) fn reset_action(table: &mut toml::Table, defaults: &[Pair], action: &str) {
    table.retain(|k, v| match v.as_str() {
        Some(v) if v == action => false,
        Some("") => !is_default(defaults, action, k),
        _ => true,
    });
}

/// `original`의 `[bindings]` 항목을 `target`과 같게 고친 새 본문. 주석·빈 줄·남은 항목 순서는 그대로 둔다.
///
/// # Errors
/// 직렬화에 실패하면 오류(파일은 건드리지 않는다).
pub(super) fn rewrite(original: &str, target: &toml::Table) -> Result<String, String> {
    let updated = Keys {
        bindings: target.clone(),
    };
    krill_proto::config_document::edit_toml(original, &updated).map_err(|e| e.to_string())
}

/// `keys.toml`을 읽어 `f`로 고치고 바꿔 단다. 없으면 템플릿에서 시작하고, 틀린 파일은 덮어쓰지 않는다.
///
/// # Errors
/// 읽기·파싱·쓰기 실패.
pub(super) fn edit_keys(dir: &Path, f: impl FnOnce(&mut toml::Table)) -> Result<(), String> {
    let path = dir.join(KEYS_FILE);
    let original = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => text::KEYS_TEMPLATE.get().to_owned(),
        Err(e) => return Err(e.to_string()),
    };
    let mut table = Keys::parse(&original).map_err(|e| e.to_string())?.bindings;
    f(&mut table);
    let body = rewrite(&original, &table)?;
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
        (m.control, "⌃"),
        (m.alt, "⌥"),
        (m.shift, "⇧"),
        (m.platform, "⌘"),
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

impl SettingsView {
    /// 키캡 클릭: 다음 키 조합을 가로챈다. 바인딩 매칭 전에 가로채므로 ⌘Q·⌘W 같은 키도 실행되지 않고 기록된다.
    pub(super) fn start_recording(
        &mut self,
        action: &'static str,
        old: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 검색 입력에 글자가 들어가지 않게 포커스를 창 루트로 옮긴다.
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
        self.shortcuts = Recorder {
            target: Some(Target { action, old }),
            intercept: Some(intercept),
            ..Recorder::default()
        };
        cx.notify();
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
        let key = keystroke.unparse();
        let others = conflicts(&live_pairs(cx), &key, target.action);
        if others.is_empty() {
            self.save_key(&target, &key, cx);
        } else {
            self.shortcuts.conflict = Some((target, key, others));
        }
    }

    fn save_key(&mut self, target: &Target, key: &str, cx: &mut Context<Self>) {
        self.write_keys(cx, |t, defaults| {
            set_key(t, defaults, target.action, target.old.as_deref(), key);
        });
    }

    fn write_keys(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut toml::Table, &[Pair])) {
        let Some(c) = cx.try_global::<Config>() else {
            return;
        };
        let Some(dir) = c.dir.clone() else {
            return;
        };
        let defaults = pairs(&c.defaults);
        match edit_keys(&dir, |t| f(t, &defaults)) {
            Ok(()) => {
                self.shortcuts.error = None;
                super::reload(cx);
            }
            Err(e) => {
                self.shortcuts.error = Some(text::SC_WRITE_FAILED.format(&[("error", &e)]).into());
            }
        }
        cx.notify();
    }

    pub(super) fn confirm_conflict(&mut self, cx: &mut Context<Self>) {
        if let Some((target, key, _)) = self.shortcuts.conflict.take() {
            self.save_key(&target, &key, cx);
        }
    }

    pub(super) fn reset_shortcut(&mut self, action: &'static str, cx: &mut Context<Self>) {
        self.shortcuts.conflict = None;
        self.write_keys(cx, |t, defaults| reset_action(t, defaults, action));
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
        let cap = layout::keycap(p, label.clone());
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

    pub(super) fn render_shortcuts_tab(&self, cx: &mut Context<Self>) -> gpui::Div {
        let p = crate::ui::palette(cx);
        let live = live_pairs(cx);
        let defaults = cx
            .try_global::<Config>()
            .map_or_else(Vec::new, |c| pairs(&c.defaults));
        let mut list = div().flex().flex_col().gap_1p5();
        for &(action, _) in ACTIONS {
            let current = keys_of(&live, action);
            let default = keys_of(&defaults, action);
            if current.is_empty() && default.is_empty() {
                continue;
            }
            let mut caps = layout::controls();
            for key in &current {
                caps = caps.child(self.key_button(action, Some(key.clone()), cx));
            }
            caps = caps.child(self.key_button(action, None, cx));
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
            list = list.child(
                layout::row(p)
                    .py_2()
                    .child(layout::label(p, action_label(action), None))
                    .child(caps),
            );
        }

        let conflict = self.shortcuts.conflict.as_ref().map(|(_, key, others)| {
            let names = others
                .iter()
                .map(|a| action_label(a))
                .collect::<Vec<_>>()
                .join(", ");
            let msg = text::SC_CONFLICT.format(&[("key", &keycap_text(key)), ("actions", &names)]);
            layout::row(p)
                .debug_selector(|| "sc-conflict".into())
                .bg(p.warning)
                .text_color(p.warning_foreground)
                .child(layout::label(p, msg, None))
                .child(
                    layout::controls()
                        .child(
                            Button::new("sc-conflict-save")
                                .small()
                                .primary()
                                .label(text::SC_CONFLICT_SAVE.get())
                                .debug_selector(|| "sc-conflict-save".into())
                                .on_click(cx.listener(|this, _, _, cx| this.confirm_conflict(cx))),
                        )
                        .child(
                            layout::secondary("sc-conflict-cancel", text::SC_CANCEL.get())
                                .debug_selector(|| "sc-conflict-cancel".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.shortcuts.conflict = None;
                                    cx.notify();
                                })),
                        ),
                )
        });
        let error = self.shortcuts.error.clone().map(|e| {
            div()
                .text_size(crate::ui::TEXT_XS)
                .text_color(p.warning)
                .child(e)
        });

        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                layout::row(p)
                    .child(layout::label(
                        p,
                        text::SHORTCUTS_TITLE.get(),
                        Some(text::SHORTCUTS_DESC.get()),
                    ))
                    .child(
                        layout::controls().child(
                            layout::secondary("shortcuts-open-keys", text::OPEN_KEYS.get())
                                .debug_selector(|| "shortcuts-open-keys".into())
                                .on_click(|_, _, cx| {
                                    open_file(KEYS_FILE, text::KEYS_TEMPLATE.get(), cx);
                                }),
                        ),
                    ),
            )
            .children(conflict)
            .children(error)
            .child(list)
    }
}
