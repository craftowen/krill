//! OWE-723 프로젝트 추가 대화상자. 사이드바 폴더 아이콘·빈 상태 '폴더 열기'로 연다.
//! 모양은 Orca `AddRepoDialog` 시작 단계(주 동작 카드 + '다른 추가 방법' 목록)를 따르되 Krill에 있는 것만 둔다.
//! Orca처럼 위에서 실행 위치(내 Mac + 연결된 원격 호스트)를 고른다. 원격 호스트를 고르면
//! 서버 파일 시스템 찾아보기 모달로 디렉터리를 탐색하거나 경로를 직접 입력할 수 있다. 실제 동작은 셸이 [`Event`]를 받아 한다.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable as _, KeyDownEvent, SharedString,
    Subscription, Window, div, prelude::*,
};
use gpui_component::Disableable as _;
use gpui_component::Icon;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_kit_assets::IconName;
use krill_proto::remote_fs::{WireBrowseEntry, WireBrowseListing};

use crate::hosts::HostId;
use crate::i18n::catalog::add_project as text;
use crate::ui;

/// 원격 호스트 디렉터리 조회 콜백(백그라운드 스레드에서 실행).
pub type RemoteBrowserFn =
    Arc<dyn Fn(&HostId, &str) -> Result<WireBrowseListing, String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 로컬 폴더를 고른다(셸이 OS 폴더 선택 패널을 띄운다).
    Browse(HostId),
    /// 호스트의 폴더를 경로로 직접 연다(원격 찾아보기·입력).
    OpenPath(HostId, PathBuf),
    /// 로컬에 복제한다(위치는 셸이 묻는다).
    Clone {
        host: HostId,
        url: String,
    },
    /// 호스트의 `parent`에 복제한다(원격, 경로 입력済み).
    CloneAt {
        host: HostId,
        url: String,
        parent: PathBuf,
    },
    /// 빈 폴더를 만들고 `git init` 한다(이름·위치는 셸이 저장 패널로 묻는다, 로컬).
    Create(HostId),
    /// 호스트의 `path`를 만들고 `git init` 한다(원격, 경로 입력済み).
    CreateAt(HostId, PathBuf),
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Start,
    Clone,
    RemotePath(RemoteMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteMode {
    Browse,
    Create,
    CloneDest,
}

pub struct Dialog {
    step: Step,
    hosts: Vec<(HostId, String)>,
    host: HostId,
    url: Entity<InputState>,
    dest: Entity<InputState>,
    remote_path: Entity<InputState>,
    remote_mode: RemoteMode,
    error: Option<&'static str>,
    remote_browser: Option<RemoteBrowserFn>,
    browse_dir: Option<String>,
    browse_is_windows: bool,
    browse_entries: Vec<WireBrowseEntry>,
    browse_cache: HashMap<(HostId, String), WireBrowseListing>,
    browse_loading: bool,
    browse_error: Option<String>,
    browse_selected: usize,
    browse_seq: u64,
    focus: FocusHandle,
    _enter_url: Subscription,
    _enter_dest: Subscription,
    _enter_path: Subscription,
    /// OWE-852: 시작 단계 3개 카드의 키보드 선택 위치(0=찾아보기, 1=복제, 2=만들기).
    selected: usize,
}

impl EventEmitter<Event> for Dialog {}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dialog")
            .field("step", &self.step)
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// `git clone`이 만들 폴더 이름(git의 기본 규칙: 마지막 경로 조각에서 `.git`·`/` 제거).
pub fn clone_dir(url: &str) -> Option<String> {
    let tail = url.trim().trim_end_matches('/').rsplit(['/', ':']).next()?;
    let name = tail.strip_suffix(".git").unwrap_or(tail);
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_owned())
}

/// 원격 호스트의 입력 경로. 이 컴퓨터의 OS와 관계없이 Unix·Windows 절대 경로를 그대로 보낸다.
fn host_path(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.contains('\0') {
        return None;
    }
    let normal = |part: &str| !part.is_empty() && !matches!(part, "." | "..");
    let unix = raw.starts_with('/') && raw.split('/').any(normal);
    let drive = matches!(raw.as_bytes(), [letter, b':', b'/' | b'\\', ..] if letter.is_ascii_alphabetic())
        && raw
            .get(3..)
            .is_some_and(|rest| rest.split(['/', '\\']).any(normal));
    let unc = raw.strip_prefix("\\\\").is_some_and(|rest| {
        // 서버·공유 이름 뒤에 실제 프로젝트 폴더가 있어야 한다(루트만 고르는 기존 거부 규칙).
        let mut parts = rest.split(['/', '\\']).filter(|part| !part.is_empty());
        parts.next().is_some_and(normal) && parts.next().is_some_and(normal) && parts.any(normal)
    });
    (unix || drive || unc).then(|| PathBuf::from(raw))
}

fn is_windows_drive_path(raw: &str) -> bool {
    matches!(
        raw.as_bytes(),
        [letter, b':'] | [letter, b':', b'/' | b'\\', ..] if letter.is_ascii_alphabetic()
    )
}

fn is_windows_drive_root(raw: &str) -> bool {
    matches!(
        raw.as_bytes(),
        [letter, b':'] | [letter, b':', b'/' | b'\\'] if letter.is_ascii_alphabetic()
    )
}

fn normalize_windows_drive_root(raw: &str) -> String {
    let letter = raw.chars().next().map_or('C', |c| c.to_ascii_uppercase());
    format!("{letter}:\\")
}

/// 원격 탐색기 상단 브레드크럼 `(표시 라벨, 이동 경로)` 목록.
fn remote_breadcrumbs(path: &str, is_windows: bool) -> Vec<(String, String)> {
    if is_windows {
        if path == "/" || path.is_empty() {
            return vec![("/".to_owned(), "/".to_owned())];
        }
        if is_windows_drive_path(path) {
            let root = normalize_windows_drive_root(path);
            let mut crumbs = vec![
                ("/".to_owned(), "/".to_owned()),
                (root.clone(), root.clone()),
            ];
            let rest = path.get(2..).unwrap_or("");
            let mut acc = root;
            for seg in rest.split(['\\', '/']).filter(|s| !s.is_empty()) {
                acc = join_remote_path(&acc, seg, true);
                crumbs.push((seg.to_owned(), acc.clone()));
            }
            return crumbs;
        }
    }
    let mut crumbs = vec![("/".to_owned(), "/".to_owned())];
    let mut acc = String::new();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        acc.push('/');
        acc.push_str(seg);
        crumbs.push((seg.to_owned(), acc.clone()));
    }
    crumbs
}

fn remote_parent_path(path: &str, is_windows: bool) -> String {
    if is_windows {
        if path == "/" || path.is_empty() || is_windows_drive_root(path) {
            return "/".to_owned();
        }
        if is_windows_drive_path(path) {
            let trimmed = path.trim_end_matches(['\\', '/']);
            if let Some((prefix, _)) = trimmed.rsplit_once(['\\', '/']) {
                if prefix.ends_with(':') {
                    return normalize_windows_drive_root(prefix);
                }
                return prefix.to_owned();
            }
            return "/".to_owned();
        }
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_owned();
    }
    match trimmed.rsplit_once('/') {
        Some(("", _)) | None => "/".to_owned(),
        Some((prefix, _)) => prefix.to_owned(),
    }
}

fn join_remote_path(base: &str, child: &str, is_windows: bool) -> String {
    if is_windows {
        if base == "/" || base.is_empty() {
            return child.to_owned();
        }
        let clean = base.trim_end_matches(['\\', '/']);
        return format!("{clean}\\{child}");
    }
    if base == "/" || base.is_empty() {
        format!("/{child}")
    } else {
        format!("{}/{child}", base.trim_end_matches('/'))
    }
}

fn looks_like_browse_path(raw: &str, is_windows: bool) -> bool {
    raw == "~"
        || raw == "."
        || raw == ".."
        || raw.contains('/')
        || (is_windows && (raw.contains('\\') || is_windows_drive_path(raw)))
}

fn resolve_typed_browse_path(raw: &str, current: Option<&str>, is_windows: bool) -> String {
    let trimmed = raw.trim();
    if trimmed == "~" || trimmed.starts_with("~/") || trimmed.starts_with("~\\") {
        return trimmed.to_owned();
    }
    if trimmed == "." {
        return current.unwrap_or("~").to_owned();
    }
    if trimmed == ".." {
        return current.map_or_else(|| "~".to_owned(), |c| remote_parent_path(c, is_windows));
    }
    if trimmed.starts_with('/') || (is_windows && is_windows_drive_path(trimmed)) {
        return trimmed.to_owned();
    }
    current.map_or_else(
        || trimmed.to_owned(),
        |c| join_remote_path(c, trimmed, is_windows),
    )
}

impl Dialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_hosts(window, cx, Vec::new(), HostId::Local)
    }

    pub fn new_with_hosts(
        window: &mut Window,
        cx: &mut Context<Self>,
        mut hosts: Vec<(HostId, String)>,
        initial: HostId,
    ) -> Self {
        if hosts.is_empty() {
            hosts.push((
                HostId::Local,
                crate::i18n::catalog::shell::HOST_LOCAL
                    .mac_or(crate::i18n::catalog::other_os::HOST_LOCAL)
                    .get()
                    .to_owned(),
            ));
        }
        let host = if hosts.iter().any(|(id, _)| id == &initial) {
            initial
        } else {
            HostId::Local
        };
        let url = cx.new(|cx| InputState::new(window, cx).placeholder(text::URL_PLACEHOLDER.get()));
        let dest = cx
            .new(|cx| InputState::new(window, cx).placeholder(text::REMOTE_PATH_PLACEHOLDER.get()));
        let remote_path = cx
            .new(|cx| InputState::new(window, cx).placeholder(text::REMOTE_PATH_PLACEHOLDER.get()));
        let enter_url =
            cx.subscribe_in(&url, window, |d, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. })
                    && !crate::jump::entity_input_is_composing(input, window, cx)
                {
                    d.submit_clone(cx);
                }
            });
        let enter_dest =
            cx.subscribe_in(&dest, window, |d, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. })
                    && !crate::jump::entity_input_is_composing(input, window, cx)
                {
                    d.submit_clone(cx);
                }
            });
        let enter_path = cx.subscribe_in(
            &remote_path,
            window,
            |d, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if d.is_remote_browser_active() {
                        d.browse_selected = 0;
                        d.error = None;
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { .. }
                    if !crate::jump::entity_input_is_composing(input, window, cx) =>
                {
                    if d.is_remote_browser_active() {
                        d.on_remote_browser_enter(window, cx);
                    } else {
                        d.submit_remote_path(cx);
                    }
                }
                _ => {}
            },
        );
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            step: Step::Start,
            hosts,
            host,
            url,
            dest,
            remote_path,
            remote_mode: RemoteMode::Browse,
            error: None,
            remote_browser: None,
            browse_dir: None,
            browse_is_windows: false,
            browse_entries: Vec::new(),
            browse_cache: HashMap::new(),
            browse_loading: false,
            browse_error: None,
            browse_selected: 0,
            browse_seq: 0,
            focus,
            _enter_url: enter_url,
            _enter_dest: enter_dest,
            _enter_path: enter_path,
            selected: 0,
        }
    }

    /// 원격 호스트 디렉터리 탐색 콜백을 연결한다(연결되면 원격 찾아보기에서 서버 파일 시스템 모달을 연다).
    #[must_use]
    pub fn with_remote_browser(mut self, browser: RemoteBrowserFn) -> Self {
        self.remote_browser = Some(browser);
        self
    }

    fn is_remote_browser_active(&self) -> bool {
        self.remote_browser.is_some()
            && matches!(
                self.step,
                Step::RemotePath(RemoteMode::Browse | RemoteMode::CloneDest)
            )
    }

    fn visible_browse_entries(&self, cx: &gpui::App) -> Vec<&WireBrowseEntry> {
        let raw = self.remote_path.read(cx).value().trim().to_owned();
        let filter = if raw.is_empty() || looks_like_browse_path(&raw, self.browse_is_windows) {
            None
        } else {
            Some(raw.to_ascii_lowercase())
        };
        self.browse_entries
            .iter()
            .filter(|entry| {
                entry.is_dir
                    && filter
                        .as_deref()
                        .is_none_or(|q| entry.name.to_ascii_lowercase().contains(q))
            })
            .collect()
    }

    fn open_remote_browser(
        &mut self,
        mode: RemoteMode,
        initial_dir: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remote_mode = mode;
        self.step = Step::RemotePath(mode);
        self.error = None;
        self.browse_error = None;
        self.browse_selected = 0;
        self.remote_path.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.navigate_remote_dir(initial_dir, window, cx);
        cx.notify();
    }

    fn navigate_remote_dir(&mut self, dir: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(browser) = self.remote_browser.clone() else {
            return;
        };
        let host = self.host.clone();
        self.error = None;
        self.browse_error = None;
        self.browse_selected = 0;
        self.remote_path
            .update(cx, |input, cx| input.set_value("", window, cx));

        if let Some(cached) = self.browse_cache.get(&(host.clone(), dir.clone())).cloned() {
            self.apply_browse_listing(&host, &dir, cached);
            cx.notify();
            return;
        }

        self.browse_seq = self.browse_seq.wrapping_add(1);
        let seq = self.browse_seq;
        self.browse_loading = true;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let host_for_rpc = host.clone();
            let dir_for_rpc = dir.clone();
            let result = cx
                .background_executor()
                .spawn(async move { browser(&host_for_rpc, &dir_for_rpc) })
                .await;
            this.update(cx, |d, cx| {
                if d.browse_seq != seq || d.host != host {
                    return;
                }
                d.browse_loading = false;
                match result {
                    Ok(listing) => d.apply_browse_listing(&host, &dir, listing),
                    Err(err) => {
                        d.browse_error = Some(err);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_browse_listing(&mut self, host: &HostId, requested: &str, listing: WireBrowseListing) {
        let resolved = listing.resolved_path.clone();
        self.browse_cache
            .insert((host.clone(), resolved.clone()), listing.clone());
        if requested != resolved {
            self.browse_cache
                .insert((host.clone(), requested.to_owned()), listing.clone());
        }
        self.browse_dir = Some(resolved);
        self.browse_is_windows = listing.is_windows;
        self.browse_entries = listing.entries;
        self.browse_loading = false;
        self.browse_error = None;
        self.browse_selected = 0;
    }

    fn on_remote_browser_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.remote_path.read(cx).value().trim().to_owned();
        if !raw.is_empty() && looks_like_browse_path(&raw, self.browse_is_windows) {
            if self.browse_error.is_some() && host_path(&raw).is_some() {
                self.confirm_remote_browser(window, cx);
                return;
            }
            let target =
                resolve_typed_browse_path(&raw, self.browse_dir.as_deref(), self.browse_is_windows);
            self.navigate_remote_dir(target, window, cx);
            return;
        }
        let target_name = {
            let visible = self.visible_browse_entries(cx);
            let idx = self.browse_selected.min(visible.len().saturating_sub(1));
            visible.get(idx).map(|entry| entry.name.clone())
        };
        if let (Some(base), Some(name)) = (self.browse_dir.as_deref(), target_name) {
            let next = join_remote_path(base, &name, self.browse_is_windows);
            self.navigate_remote_dir(next, window, cx);
        }
    }

    fn confirm_remote_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.remote_path.read(cx).value().trim().to_owned();
        let chosen = if !raw.is_empty() && looks_like_browse_path(&raw, self.browse_is_windows) {
            host_path(&raw)
        } else {
            self.browse_dir.as_deref().and_then(host_path)
        };
        let Some(path) = chosen else {
            self.error = Some(text::REMOTE_PATH_BAD.get());
            cx.notify();
            return;
        };
        match self.remote_mode {
            RemoteMode::Browse => cx.emit(Event::OpenPath(self.host.clone(), path)),
            RemoteMode::Create => cx.emit(Event::CreateAt(self.host.clone(), path)),
            RemoteMode::CloneDest => {
                let text_path = path.to_string_lossy().into_owned();
                self.dest
                    .update(cx, |input, cx| input.set_value(text_path, window, cx));
                self.step = Step::Clone;
                self.error = None;
                self.dest.update(cx, |input, cx| input.focus(window, cx));
                cx.notify();
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn host(&self) -> &HostId {
        &self.host
    }

    #[cfg(test)]
    pub(crate) fn hosts_len(&self) -> usize {
        self.hosts.len()
    }

    fn set_host(&mut self, host: HostId, window: &mut Window, cx: &mut Context<Self>) {
        if self.host != host {
            self.browse_dir = None;
            self.browse_entries.clear();
            self.browse_error = None;
            self.browse_loading = false;
            self.browse_selected = 0;
        }
        self.host = host;
        self.error = None;
        if self.step == Step::Clone && self.host.is_local() {
            // 로컬 복제는 목적지 입력이 필요 없다.
        } else if self.step == Step::Clone {
            self.dest.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    fn browse_activated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.host.is_local() {
            cx.emit(Event::Browse(self.host.clone()));
        } else if self.remote_browser.is_some() {
            let initial = self.browse_dir.clone().unwrap_or_default();
            self.open_remote_browser(RemoteMode::Browse, initial, window, cx);
        } else {
            self.remote_mode = RemoteMode::Browse;
            self.step = Step::RemotePath(RemoteMode::Browse);
            self.error = None;
            self.remote_path
                .update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
        }
    }

    fn browse_clone_dest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let initial = {
            let current = self.dest.read(cx).value().trim().to_owned();
            if current.is_empty() {
                self.browse_dir.clone().unwrap_or_default()
            } else {
                current
            }
        };
        self.open_remote_browser(RemoteMode::CloneDest, initial, window, cx);
    }

    fn create_activated(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.host.is_local() {
            cx.emit(Event::Create(self.host.clone()));
        } else {
            self.remote_mode = RemoteMode::Create;
            self.step = Step::RemotePath(RemoteMode::Create);
            self.error = None;
            self.remote_path
                .update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
        }
    }

    fn show_clone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step = Step::Clone;
        self.error = None;
        self.url.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn show_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.step = Step::Start;
        self.error = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    fn activate_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.selected {
            0 => self.browse_activated(window, cx),
            1 => self.show_clone(window, cx),
            _ => self.create_activated(window, cx),
        }
    }

    fn submit_clone(&mut self, cx: &mut Context<Self>) {
        let url = self.url.read(cx).value().trim().to_owned();
        if clone_dir(&url).is_none() {
            self.error = Some(text::BAD_URL.get());
            cx.notify();
            return;
        }
        if self.host.is_local() {
            cx.emit(Event::Clone {
                host: self.host.clone(),
                url,
            });
            return;
        }
        let raw = self.dest.read(cx).value().trim().to_owned();
        let Some(parent) = host_path(&raw) else {
            self.error = Some(text::REMOTE_PATH_BAD.get());
            cx.notify();
            return;
        };
        cx.emit(Event::CloneAt {
            host: self.host.clone(),
            url,
            parent,
        });
    }

    fn submit_remote_path(&mut self, cx: &mut Context<Self>) {
        let raw = self.remote_path.read(cx).value().trim().to_owned();
        let Some(path) = host_path(&raw) else {
            self.error = Some(text::REMOTE_PATH_BAD.get());
            cx.notify();
            return;
        };
        match self.remote_mode {
            RemoteMode::Browse => cx.emit(Event::OpenPath(self.host.clone(), path)),
            RemoteMode::Create => cx.emit(Event::CreateAt(self.host.clone(), path)),
            RemoteMode::CloneDest => {
                self.step = Step::Clone;
                cx.notify();
            }
        }
    }

    fn leave_remote_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.step == Step::RemotePath(RemoteMode::CloneDest) {
            self.step = Step::Clone;
            self.error = None;
            self.dest.update(cx, |input, cx| input.focus(window, cx));
            cx.notify();
        } else {
            self.show_start(window, cx);
        }
    }

    pub(crate) fn input_is_composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        [&self.url, &self.dest, &self.remote_path]
            .into_iter()
            .any(|input| {
                input.update(cx, |input, cx| {
                    input.focus_handle(cx).is_focused(window)
                        && gpui::EntityInputHandler::marked_text_range(input, window, cx).is_some()
                })
            })
    }

    fn on_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if (e.keystroke.key == "escape" && window.default_prevented())
            || self.input_is_composing(window, cx)
        {
            return;
        }
        match e.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                if self.step == Step::Start {
                    cx.emit(Event::Close);
                } else {
                    // OWE-852: 하위 단계 Esc는 모달을 닫지 않고 이전 단계로 복귀한다.
                    self.leave_remote_step(window, cx);
                }
            }
            "up" if self.step == Step::Start && self.focus.is_focused(window) => {
                cx.stop_propagation();
                self.selected = match self.selected {
                    0 => 2,
                    n => n - 1,
                };
                cx.notify();
            }
            "down" if self.step == Step::Start && self.focus.is_focused(window) => {
                cx.stop_propagation();
                self.selected = match self.selected {
                    0 | 1 => self.selected + 1,
                    _ => 0,
                };
                cx.notify();
            }
            "enter" if self.step == Step::Start && self.focus.is_focused(window) => {
                cx.stop_propagation();
                self.activate_selected(window, cx);
            }
            "up" if self.is_remote_browser_active() => {
                let count = self.visible_browse_entries(cx).len();
                if count > 0 {
                    cx.stop_propagation();
                    window.prevent_default();
                    self.browse_selected = if self.browse_selected == 0 {
                        count - 1
                    } else {
                        self.browse_selected - 1
                    };
                    cx.notify();
                }
            }
            "down" if self.is_remote_browser_active() => {
                let count = self.visible_browse_entries(cx).len();
                if count > 0 {
                    cx.stop_propagation();
                    window.prevent_default();
                    self.browse_selected = (self.browse_selected + 1) % count;
                    cx.notify();
                }
            }
            "backspace"
                if self.is_remote_browser_active()
                    && !self.browse_loading
                    && self.remote_path.read(cx).value().is_empty() =>
            {
                if let Some(current) = self.browse_dir.as_deref() {
                    let parent = remote_parent_path(current, self.browse_is_windows);
                    if parent != current {
                        cx.stop_propagation();
                        window.prevent_default();
                        self.navigate_remote_dir(parent, window, cx);
                    }
                }
            }
            _ => {}
        }
    }

    fn host_selector(
        &self,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if self.hosts.len() <= 1 {
            return None;
        }
        let current = self.host.clone();
        let chips = self
            .hosts
            .iter()
            .map(|(id, label)| {
                let selected = id == &current;
                let next = id.clone();
                let key = id.as_key().to_owned();
                let debug_key = key.clone();
                let chip_label = label.clone();
                let chip = div()
                    .id(SharedString::from(format!("add-project-host-{key}")))
                    .debug_selector(move || format!("add-project-host-{debug_key}"))
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .focus_visible(|d| d.border_1().border_color(p.ring))
                    .aria_label(chip_label.clone())
                    .px_3()
                    .py_1p5()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .cursor_pointer()
                    .text_size(ui::TEXT_SM)
                    .when(selected, |d| {
                        d.border_color(p.ring).bg(p.accent).text_color(p.foreground)
                    })
                    .when(!selected, |d| {
                        d.border_color(p.input)
                            .bg(p.background)
                            .text_color(p.muted_foreground)
                    })
                    .child(label.clone());
                on_activate(chip, cx, move |d, window, cx| {
                    d.set_host(next.clone(), window, cx);
                })
            })
            .collect::<Vec<_>>();
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(ui::field_label(text::LOCATION.get(), None, p))
                .child(div().flex().flex_wrap().gap_2().children(chips)),
        )
    }

    fn render_remote_breadcrumbs(
        &self,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let current = self.browse_dir.as_deref().unwrap_or("");
        let is_windows = self.browse_is_windows;
        let can_up = !current.is_empty() && remote_parent_path(current, is_windows) != current;
        let parent_target = remote_parent_path(current, is_windows);
        let crumbs = if current.is_empty() {
            Vec::new()
        } else {
            remote_breadcrumbs(current, is_windows)
        };
        let crumb_len = crumbs.len();
        let mut bar = div()
            .id("remote-browse-breadcrumbs")
            .debug_selector(|| "remote-browse-breadcrumbs".into())
            .flex_none()
            .flex()
            .items_center()
            .flex_wrap()
            .gap_1()
            .child(
                Button::new("remote-browse-up")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ArrowUp)
                    .disabled(!can_up || self.browse_loading)
                    .accessibility_label(text::SERVER_BROWSE_UP.get())
                    .on_click(cx.listener(move |d, _, window, cx| {
                        d.navigate_remote_dir(parent_target.clone(), window, cx);
                    })),
            )
            .child(
                Button::new("remote-browse-home")
                    .ghost()
                    .xsmall()
                    .icon(IconName::House)
                    .disabled(self.browse_loading)
                    .accessibility_label(text::SERVER_BROWSE_HOME.get())
                    .on_click(cx.listener(|d, _, window, cx| {
                        d.navigate_remote_dir(String::new(), window, cx);
                    })),
            );

        for (idx, (label, path)) in crumbs.into_iter().enumerate() {
            if idx > 0 {
                bar = bar.child(
                    div()
                        .flex_none()
                        .text_color(p.muted_foreground)
                        .child(Icon::new(IconName::ChevronRight).xsmall()),
                );
            }
            let is_last = idx + 1 == crumb_len;
            let seg = div()
                .id(("remote-browse-crumb", idx))
                .px_1()
                .py_0p5()
                .rounded(ui::RADIUS_SM)
                .text_size(ui::TEXT_SM)
                .when(is_last, |d| {
                    d.font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(p.foreground)
                })
                .when(!is_last, |d| {
                    d.text_color(p.muted_foreground)
                        .cursor_pointer()
                        .hover(|h| h.text_color(p.foreground).bg(p.accent))
                        .on_click(cx.listener(move |d, _, window, cx| {
                            d.navigate_remote_dir(path.clone(), window, cx);
                        }))
                })
                .child(label);
            bar = bar.child(seg);
        }
        bar
    }

    fn render_remote_browser_body(
        &self,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let visible: Vec<(String, bool)> = self
            .visible_browse_entries(cx)
            .into_iter()
            .map(|entry| (entry.name.clone(), entry.is_symlink))
            .collect();
        let selected = self.browse_selected.min(visible.len().saturating_sub(1));
        let has_filter = {
            let raw = self.remote_path.read(cx).value();
            let trimmed = raw.trim();
            !trimmed.is_empty() && !looks_like_browse_path(trimmed, self.browse_is_windows)
        };
        let current = self.browse_dir.clone().unwrap_or_default();
        let is_windows = self.browse_is_windows;

        let list_content: gpui::AnyElement = if self.browse_loading {
            div()
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .py_6()
                .text_size(ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(Icon::new(IconName::LoaderCircle).xsmall())
                .child(text::SERVER_BROWSE_LOADING.get())
                .into_any_element()
        } else if let Some(err) = self.browse_error.as_deref() {
            div()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(ui::error_banner(p, err))
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::SERVER_BROWSE_UNAVAILABLE.get()),
                )
                .into_any_element()
        } else if visible.is_empty() {
            div()
                .py_6()
                .flex()
                .justify_center()
                .text_size(ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(if has_filter {
                    text::SERVER_BROWSE_NO_MATCH.get()
                } else {
                    text::SERVER_BROWSE_EMPTY.get()
                })
                .into_any_element()
        } else {
            let rows = visible.into_iter().enumerate().map(|(idx, (name, _))| {
                let is_selected = idx == selected;
                let next_path = join_remote_path(&current, &name, is_windows);
                let label = name.clone();
                div()
                    .id(("remote-browse-row", idx))
                    .debug_selector(move || format!("remote-browse-row-{idx}"))
                    .role(gpui::Role::Button)
                    .aria_label(label.clone())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(ui::RADIUS_SM)
                    .cursor_pointer()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.foreground)
                    .when(is_selected, |d| d.bg(p.accent))
                    .when(!is_selected, |d| d.hover(|h| h.bg(p.accent)))
                    .on_click(cx.listener(move |d, _, window, cx| {
                        d.navigate_remote_dir(next_path.clone(), window, cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(p.muted_foreground)
                                    .child(Icon::new(IconName::Folder).small()),
                            )
                            .child(div().min_w_0().flex_1().truncate().child(name)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(p.muted_foreground)
                            .child(Icon::new(IconName::ChevronRight).xsmall()),
                    )
            });
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .children(rows)
                .into_any_element()
        };

        let preview_path = {
            let raw = self.remote_path.read(cx).value().trim().to_owned();
            if !raw.is_empty() && looks_like_browse_path(&raw, self.browse_is_windows) {
                raw
            } else {
                self.browse_dir.clone().unwrap_or_default()
            }
        };
        let footer_caption = (!preview_path.is_empty()).then(|| {
            let tmpl = match self.remote_mode {
                RemoteMode::CloneDest => text::SERVER_BROWSE_FOOTER_CLONE,
                RemoteMode::Browse | RemoteMode::Create => text::SERVER_BROWSE_FOOTER_OPEN,
            };
            tmpl.format(&[("path", &preview_path)])
        });

        div()
            .flex()
            .flex_col()
            .gap_2p5()
            .min_h_0()
            .child(self.render_remote_breadcrumbs(p, cx))
            .child(
                Input::new(&self.remote_path).prefix(
                    div()
                        .text_color(p.muted_foreground)
                        .child(Icon::new(IconName::Search).xsmall()),
                ),
            )
            .child(
                div()
                    .id("remote-browse-list")
                    .debug_selector(|| "remote-browse-list".into())
                    .h(gpui::px(260.))
                    .min_h(gpui::px(120.))
                    .overflow_y_scroll()
                    .rounded(ui::RADIUS_MD)
                    .border_1()
                    .border_color(p.input)
                    .bg(p.background)
                    .p_1()
                    .child(list_content),
            )
            .children(footer_caption.map(|caption| {
                div()
                    .id("remote-browse-caption")
                    .debug_selector(|| "remote-browse-caption".into())
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .truncate()
                    .child(caption)
            }))
            .children(self.error.map(|e| ui::error_banner(p, e)))
    }
}

/// 아이콘 + 제목 + 설명 한 줄(Orca `AddRepoPrimaryStartAction`/`AddRepoSecondaryStartAction`).
/// OWE-852: 스크린 리더가 버튼으로 읽도록 `Role::Button`을 단다.
fn is_activation_key(e: &KeyDownEvent) -> bool {
    matches!(e.keystroke.key.as_str(), "enter" | "space" | " ")
        && !e.keystroke.modifiers.control
        && !e.keystroke.modifiers.alt
        && !e.keystroke.modifiers.platform
        && !e.keystroke.modifiers.function
}

fn on_activate(
    el: gpui::Stateful<gpui::Div>,
    cx: &mut Context<Dialog>,
    handler: impl Fn(&mut Dialog, &mut Window, &mut Context<Dialog>) + Clone + 'static,
) -> gpui::Stateful<gpui::Div> {
    let click = handler.clone();
    el.on_click(cx.listener(move |d, _, window, cx| click(d, window, cx)))
        .on_key_down(cx.listener(move |d, e: &KeyDownEvent, window, cx| {
            if is_activation_key(e) {
                cx.stop_propagation();
                window.prevent_default();
                handler(d, window, cx);
            }
        }))
}

fn action(
    p: &ui::Palette,
    id: &'static str,
    icon: IconName,
    title: &'static str,
    desc: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .role(gpui::Role::Button)
        .tab_index(0)
        .focus_visible(|d| d.border_1().border_color(p.ring))
        .aria_label(title)
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2p5()
        .cursor_pointer()
        .hover(|d| d.bg(p.accent))
        .child(
            div()
                .flex_none()
                .size_7()
                .flex()
                .items_center()
                .justify_center()
                .rounded(ui::RADIUS_SM)
                .child(Icon::new(icon)),
        )
        .child(ui::label(p, title, Some(desc)))
}

/// OWE-852: 키보드 선택 위치를 카드 테두리·배경으로 드러낸다(선택 카드만 링+강조).
fn selected_card(
    base: gpui::Stateful<gpui::Div>,
    p: &ui::Palette,
    is_selected: bool,
) -> gpui::Stateful<gpui::Div> {
    if is_selected {
        base.rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(p.ring)
            .bg(p.accent)
    } else {
        base.rounded(ui::RADIUS_MD)
            .border_1()
            .border_color(p.input)
            .bg(p.background)
    }
}

impl Render for Dialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let remote_browser_active = self.is_remote_browser_active();
        crate::i18n::sync_input_placeholder(&self.url, text::URL_PLACEHOLDER.get(), window, cx);
        crate::i18n::sync_input_placeholder(
            &self.dest,
            text::REMOTE_PATH_PLACEHOLDER.get(),
            window,
            cx,
        );
        crate::i18n::sync_input_placeholder(
            &self.remote_path,
            if remote_browser_active {
                text::SERVER_BROWSE_FILTER_PLACEHOLDER.get()
            } else {
                text::REMOTE_PATH_PLACEHOLDER.get()
            },
            window,
            cx,
        );
        let p = ui::palette(cx);
        let title = match self.step {
            Step::Start => text::TITLE.get(),
            Step::Clone => text::CLONE.get(),
            Step::RemotePath(RemoteMode::Browse | RemoteMode::CloneDest)
                if remote_browser_active =>
            {
                text::SERVER_BROWSE_TITLE.get()
            }
            Step::RemotePath(RemoteMode::Browse | RemoteMode::CloneDest) => text::BROWSE.get(),
            Step::RemotePath(RemoteMode::Create) => text::CREATE.get(),
        };
        let subtitle = remote_browser_active.then(|| text::SERVER_BROWSE_SUBTITLE.get());
        let host_selector = self.host_selector(p, cx);
        let has_remote_browser = self.remote_browser.is_some();
        let body = div()
            .id("add-project-body")
            .debug_selector(|| "add-project-body".into())
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3();
        let body = match self.step {
            Step::Start => {
                let selected = self.selected;
                body.children(host_selector)
                    .child(selected_card(
                        on_activate(
                            action(
                                p,
                                "add-project-browse",
                                IconName::FolderOpen,
                                text::BROWSE.get(),
                                text::BROWSE_DESC.get(),
                            )
                            .child(ui::keycap(p, "↵")),
                            cx,
                            Dialog::browse_activated,
                        ),
                        p,
                        selected == 0,
                    ))
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(p.muted_foreground)
                            .child(text::OTHER.get()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(selected_card(
                                on_activate(
                                    action(
                                        p,
                                        "add-project-clone",
                                        IconName::Globe,
                                        text::CLONE.get(),
                                        text::CLONE_DESC.get(),
                                    ),
                                    cx,
                                    Dialog::show_clone,
                                ),
                                p,
                                selected == 1,
                            ))
                            .child(selected_card(
                                on_activate(
                                    action(
                                        p,
                                        "add-project-create",
                                        IconName::Plus,
                                        text::CREATE.get(),
                                        text::CREATE_DESC.get(),
                                    ),
                                    cx,
                                    Dialog::create_activated,
                                ),
                                p,
                                selected == 2,
                            )),
                    )
            }
            Step::Clone => {
                let remote = self.host.is_remote();
                body.children(host_selector)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(ui::field_label(text::URL.get(), None, p))
                            .child(Input::new(&self.url)),
                    )
                    .when(remote, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(ui::field_label(text::REMOTE_DEST.get(), None, p))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div().flex_1().min_w_0().child(Input::new(&self.dest)),
                                        )
                                        .when(has_remote_browser, |row| {
                                            row.child(
                                                Button::new("add-project-clone-browse-dest")
                                                    .ghost()
                                                    .small()
                                                    .icon(IconName::FolderOpen)
                                                    .label(text::SERVER_BROWSE_BROWSE_BTN.get())
                                                    .on_click(cx.listener(|d, _, window, cx| {
                                                        d.browse_clone_dest(window, cx);
                                                    })),
                                            )
                                        }),
                                ),
                        )
                    })
                    .children(self.error.map(|e| ui::error_banner(p, e)))
            }
            Step::RemotePath(_) if remote_browser_active => {
                body.child(self.render_remote_browser_body(p, cx))
            }
            Step::RemotePath(_) => body
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(ui::field_label(text::REMOTE_PATH.get(), None, p))
                        .child(Input::new(&self.remote_path)),
                )
                .children(self.error.map(|e| ui::error_banner(p, e))),
        };
        let back = |label: &'static str| {
            Button::new("add-project-back")
                .ghost()
                .label(label)
                .on_click(cx.listener(|d, _, window, cx| d.leave_remote_step(window, cx)))
        };
        let footer = || {
            div()
                .id("add-project-actions")
                .debug_selector(|| "add-project-actions".into())
                .flex_none()
                .flex()
                .flex_wrap()
                .justify_end()
                .gap_2()
        };
        let actions = match self.step {
            Step::Start => None,
            Step::Clone => Some(
                footer().child(back(text::BACK.get())).child(
                    Button::new("add-project-clone-submit")
                        .primary()
                        .label(if self.host.is_remote() {
                            text::CLONE_SUBMIT_REMOTE.get()
                        } else {
                            text::CLONE_SUBMIT.get()
                        })
                        .debug_selector(|| "add-project-clone-submit".into())
                        .on_click(cx.listener(|d, _, _, cx| d.submit_clone(cx))),
                ),
            ),
            Step::RemotePath(_) if remote_browser_active => Some(
                footer()
                    .child(back(text::SERVER_BROWSE_CANCEL.get()))
                    .child(
                        Button::new("add-project-remote-submit")
                            .primary()
                            .label(text::SERVER_BROWSE_SELECT.get())
                            .debug_selector(|| "add-project-remote-submit".into())
                            .on_click(cx.listener(|d, _, window, cx| {
                                d.confirm_remote_browser(window, cx);
                            })),
                    ),
            ),
            Step::RemotePath(mode) => Some(
                footer().child(back(text::BACK.get())).child(
                    Button::new("add-project-remote-submit")
                        .primary()
                        .label(match mode {
                            RemoteMode::Browse | RemoteMode::CloneDest => text::OPEN_SUBMIT.get(),
                            RemoteMode::Create => text::CREATE_SUBMIT.get(),
                        })
                        .debug_selector(|| "add-project-remote-submit".into())
                        .on_click(cx.listener(|d, _, _, cx| d.submit_remote_path(cx))),
                ),
            ),
        };
        let card = div()
            .id("add-project-dialog")
            .debug_selector(|| "add-project-dialog".into())
            .track_focus(&self.focus)
            .occlude()
            .w(ui::DIALOG_WIDTH)
            .max_w((window.viewport_size().width - ui::SPACE_4 * 2.).max(gpui::px(0.)))
            .max_h((window.viewport_size().height - ui::SPACE_4 * 2.).max(gpui::px(0.)))
            .role(gpui::Role::Dialog)
            .aria_label(title)
            .p_6()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_MD)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .capture_action(
                cx.listener(|d, _: &gpui_component::input::Escape, window, cx| {
                    if d.input_is_composing(window, cx) {
                        window.prevent_default();
                    }
                }),
            )
            .capture_action(
                cx.listener(|d, _: &gpui_component::input::Enter, window, cx| {
                    if d.input_is_composing(window, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_key_down(cx.listener(|d, e: &KeyDownEvent, window, cx| {
                d.on_key(e, window, cx);
            }))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(Event::Close)))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(ui::TEXT_LG)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(SharedString::from(title)),
                            )
                            .child(
                                Button::new("add-project-close")
                                    .ghost()
                                    .small()
                                    .icon(gpui_component::IconName::Close)
                                    .accessibility_label(text::CLOSE.get())
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Close))),
                            ),
                    )
                    .children(subtitle.map(|desc| {
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(desc)
                    })),
            )
            .child(body)
            .children(actions);
        ui::modal_backdrop(
            window.viewport_size(),
            ui::modal_focus_scope(card, &self.focus),
        )
    }
}

#[cfg(test)]
#[path = "add_project/tests.rs"]
mod tests;

#[cfg(test)]
mod ime_enter_tests {
    use std::sync::{Arc, Mutex};

    use gpui::{EntityInputHandler as _, TestAppContext};

    use super::*;

    #[gpui::test]
    fn press_enter_is_ignored_during_ime_preedit_in_all_inputs(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let remote = HostId::Remote("mini".to_owned());
        let (dialog, cx) = cx.add_window_view(|window, cx| {
            Dialog::new_with_hosts(
                window,
                cx,
                vec![
                    (HostId::Local, "local".to_owned()),
                    (remote.clone(), "mini".to_owned()),
                ],
                remote.clone(),
            )
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        cx.update(|_, cx| {
            cx.subscribe(&dialog, move |_, e: &Event, _| {
                sink.lock().unwrap().push(e.clone());
            })
            .detach();
        });
        cx.run_until_parked();

        // Clone URL input with active IME preedit must not submit on Enter.
        cx.update(|window, cx| {
            dialog.update(cx, |d, cx| {
                d.show_clone(window, cx);
                d.url.update(cx, |input, cx| {
                    input.set_value("git@github.com:o/repo.git", window, cx);
                    input.focus(window, cx);
                    input.replace_and_mark_text_in_range(None, "ㅎ", None, window, cx);
                });
                d.dest.update(cx, |input, cx| {
                    input.set_value("/srv", window, cx);
                });
            });
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            events.lock().unwrap().is_empty(),
            "Enter during url IME preedit must not submit clone"
        );

        // Clone dest input with active IME preedit must not submit on Enter.
        cx.update(|window, cx| {
            dialog.update(cx, |d, cx| {
                d.url.update(cx, |input, cx| {
                    input.unmark_text(window, cx);
                    input.set_value("git@github.com:o/repo.git", window, cx);
                });
                d.dest.update(cx, |input, cx| {
                    input.focus(window, cx);
                    input.replace_and_mark_text_in_range(None, "한", None, window, cx);
                });
            });
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            events.lock().unwrap().is_empty(),
            "Enter during dest IME preedit must not submit clone"
        );

        // RemotePath input with active IME preedit must not submit on Enter, then submits after unmark.
        cx.update(|window, cx| {
            dialog.update(cx, |d, cx| {
                d.dest.update(cx, |input, cx| input.unmark_text(window, cx));
                d.show_start(window, cx);
                d.browse_activated(window, cx);
                d.remote_path.update(cx, |input, cx| {
                    input.set_value("/srv/repo", window, cx);
                    input.focus(window, cx);
                    input.replace_and_mark_text_in_range(None, "경", None, window, cx);
                });
            });
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            events.lock().unwrap().is_empty(),
            "Enter during remote_path IME preedit must not submit remote path"
        );

        cx.update(|window, cx| {
            dialog.update(cx, |d, cx| {
                d.remote_path.update(cx, |input, cx| {
                    input.unmark_text(window, cx);
                    input.set_value("/srv/repo", window, cx);
                });
            });
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            *events.lock().unwrap(),
            [Event::OpenPath(remote, PathBuf::from("/srv/repo"))]
        );
    }
}
