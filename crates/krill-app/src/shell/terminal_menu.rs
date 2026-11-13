//! 터미널의 URL·파일 경로·OSC 8 링크 우클릭 동작. 파일 확인과 기본 앱 실행은 UI 밖에서 처리한다.

use gpui::WeakEntity;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use krill_app::i18n::catalog::{diff as dtext, security as stext};
use krill_app::terminal::path_link::{Cache, PathLink};

use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) enum FileAction {
    Open,
    OpenDefault,
    Reveal,
    CopyPath,
}

impl Shell {
    pub(super) fn activate_terminal_file(
        &mut self,
        link: PathLink,
        action: FileAction,
        cx: &mut Context<Self>,
    ) {
        let remote = match self
            .layout
            .focused()
            .map(|pane| self.file_host_for_pane(pane))
        {
            Some(super::files::FileHost::Remote(host, target)) => Some((host, *target)),
            // 전송이 없는 원격(끊김·시작 중): 같은 경로의 로컬 파일을 열거나 Finder에 보이지 않는다(APP-F01 후속).
            Some(super::files::FileHost::Unavailable(_))
                if matches!(action, FileAction::OpenDefault | FileAction::Reveal) =>
            {
                self.notice = Some(
                    krill_app::i18n::catalog::editor::REMOTE_DISCONNECTED
                        .get()
                        .to_owned(),
                );
                cx.notify();
                return;
            }
            _ => None,
        };
        match action {
            FileAction::Open => self.open_path_link(link, cx),
            FileAction::OpenDefault | FileAction::Reveal => {
                if matches!(action, FileAction::Reveal) && remote.is_none() {
                    cx.reveal_path(&link.path);
                    return;
                }
                let command = if matches!(action, FileAction::Reveal) {
                    krill_app::file_format::reveal_command
                } else {
                    krill_app::file_format::open_default_command
                };
                let weak = cx.weak_entity();
                krill_app::file_format::view::spawn_open_on_reported(
                    link.path,
                    remote,
                    command,
                    move |msg, cx| {
                        weak.update(cx, |s, cx| {
                            s.notice = Some(msg);
                            cx.notify();
                        })
                        .ok();
                    },
                    cx,
                );
            }
            FileAction::CopyPath => {
                cx.write_to_clipboard(ClipboardItem::new_string(link.path.display().to_string()));
            }
        }
    }
}

struct MenuState {
    pane: PaneId,
    is_remote: bool,
    focus: FocusHandle,
    has_selection: bool,
    fork_enabled: Option<bool>,
    can_continue: bool,
}

impl MenuState {
    fn fill(
        &self,
        mut menu: PopupMenu,
        shell: &WeakEntity<Shell>,
        link: Option<&PathLink>,
        uri: Option<&str>,
    ) -> PopupMenu {
        menu = menu.action_context(self.focus.clone());
        if let Some(uri) = uri {
            for (id, label, open) in [
                ("terminal-url-copy", stext::LINK_COPY.get(), false),
                ("terminal-url-open", stext::LINK_OPEN.get(), true),
            ] {
                let shell = shell.clone();
                let pane = self.pane;
                let uri = uri.to_owned();
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        div().id(id).debug_selector(move || id.into()).child(label)
                    })
                    .on_click(move |_, _, cx| {
                        if open {
                            if shell
                                .update(cx, |s, cx| s.open_terminal_url(pane, &uri, cx))
                                .is_err()
                            {
                                cx.open_url(&uri);
                            }
                        } else {
                            cx.write_to_clipboard(ClipboardItem::new_string(uri.clone()));
                        }
                    }),
                );
            }
            menu = menu.separator();
        }
        if let Some(link) = link {
            for (id, label, action) in [
                (
                    "terminal-file-open",
                    text::TERMINAL_OPEN_FILE.get(),
                    FileAction::Open,
                ),
                (
                    "terminal-file-open-default",
                    krill_app::file_format::view::open_action_label(self.is_remote),
                    FileAction::OpenDefault,
                ),
                (
                    "terminal-file-reveal",
                    krill_app::file_format::view::reveal_action_label(self.is_remote),
                    FileAction::Reveal,
                ),
                (
                    "terminal-file-copy-path",
                    dtext::EXPLORER_COPY_PATH.get(),
                    FileAction::CopyPath,
                ),
            ] {
                let shell = shell.clone();
                let pane = self.pane;
                let link = link.clone();
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        div().id(id).debug_selector(move || id.into()).child(label)
                    })
                    .on_click(move |_, _, cx| {
                        shell
                            .update(cx, |shell, cx| {
                                if shell.layout.focused() != Some(pane) {
                                    shell.change(true, cx, |l| l.focus(pane));
                                }
                                shell.activate_terminal_file(link.clone(), action, cx);
                            })
                            .ok();
                    }),
                );
            }
            menu = menu.separator();
        }
        menu = menu
            .menu_with_disabled(text::COPY.get(), Box::new(Copy), !self.has_selection)
            .menu_with_disabled(
                text::COPY_CLEAN.get(),
                Box::new(CopyClean),
                !self.has_selection,
            )
            .menu(text::PASTE.get(), Box::new(Paste))
            .separator()
            .menu(text::PANE_SELECT_ALL.get(), Box::new(SelectAll))
            .menu(text::PANE_CLEAR.get(), Box::new(ClearPane))
            .separator()
            .menu(text::SPLIT_RIGHT.get(), Box::new(SplitRight))
            .menu(text::SPLIT_DOWN.get(), Box::new(SplitDown))
            .menu(text::PANE_ZOOM_TOGGLE.get(), Box::new(ToggleZoomPane));
        if let Some(can_fork) = self.fork_enabled {
            menu = menu
                .separator()
                .menu_with_disabled(text::FORK_SESSION.get(), Box::new(ForkSession), !can_fork)
                .menu_with_disabled(
                    text::CONTINUE_IN_NEW_SESSION.get(),
                    Box::new(ContinueInNewSession),
                    !self.can_continue,
                );
        }
        menu.separator()
            .menu(text::CLOSE_PANE.get(), Box::new(ClosePane))
    }
}

enum Target {
    File {
        path: PathBuf,
        remote: Option<krill_app::hosts::HostTarget>,
    },
    Text {
        input: krill_app::terminal::path_link::Input,
        cwd: Option<PathBuf>,
        remote: Option<krill_app::hosts::HostTarget>,
    },
}

impl Target {
    fn resolve(self) -> Option<PathLink> {
        match self {
            Self::File { path, remote } => {
                let exists = match remote {
                    Some(target) => krill_app::remote_fs::stat(&target, &path)
                        .ok()
                        .flatten()
                        .is_some(),
                    None => path.is_file(),
                };
                exists.then_some(PathLink {
                    column: 0,
                    cols: 0,
                    path,
                    line: None,
                    col: None,
                })
            }
            Self::Text { input, cwd, remote } => match remote {
                Some(target) => input.resolve_remote(&target, cwd.as_deref()),
                None => input.resolve(&mut Cache::default(), cwd.as_deref()),
            },
        }
    }
}

pub(super) fn build(
    menu: PopupMenu,
    shell: &WeakEntity<Shell>,
    pane: PaneId,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(entity) = shell.upgrade() else {
        return menu;
    };
    let s = entity.read(cx);
    let state = MenuState {
        pane,
        is_remote: s.layout.host(pane).is_remote(),
        focus: s.focus.clone(),
        has_selection: s.live.get(&pane).is_some_and(|l| l.selection.is_some()),
        fork_enabled: s
            .peer_supports_fork(pane)
            .then(|| s.can_fork_pane(pane, cx)),
        can_continue: s.can_continue_pane(pane),
    };
    if let Some((_, uri)) = s.click_link_at(pane, window.mouse_position()) {
        return state.fill(menu, shell, None, Some(&uri));
    }
    let host = s.layout.host(pane).clone();
    let remote = if host.is_remote() {
        let Some((_, target)) = s.remote_target_for_pane(pane) else {
            return state.fill(menu, shell, None, None);
        };
        Some(target)
    } else {
        None
    };
    let target = (|| {
        let live = s.live.get(&pane)?;
        let screen = live.pane.shown();
        let position = window.mouse_position();
        let (column, line) = cell_at(&live.painted, &screen, position)?;
        if let Some(path) = file_link_target(&screen, &live.painted, position) {
            Some(Target::File { path, remote })
        } else if screen.link_at(line, column).is_none() {
            Some(Target::Text {
                input: krill_app::terminal::path_link::Input::at(&screen, line, column)?,
                cwd: s.pane_cwd(pane, cx),
                remote,
            })
        } else {
            None
        }
    })();
    if let Some(target) = target {
        let shell = shell.clone();
        let live = s.live.get(&pane);
        let session = live.and_then(|l| l.pane.session());
        let shown = live.map(|l| l.pane.shown());
        let input = match &target {
            Target::Text { input, .. } => Some(input.clone()),
            Target::File { .. } => None,
        };
        let cwd = s.pane_cwd(pane, cx);
        // 확인 중인 메뉴에 행을 끼워 넣으면 포인터 아래 동작이 바뀐다. 항목을 확정한 뒤 한 번에 연다.
        cx.spawn_in(window, async move |menu, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { target.resolve() })
                .await;
            menu.update_in(cx, |menu, window, cx| {
                let valid = shell.upgrade().is_some_and(|shell| {
                    let s = shell.read(cx);
                    *s.layout.host(pane) == host
                        && s.pane_cwd(pane, cx) == cwd
                        && s.live.get(&pane).is_some_and(|l| {
                            l.pane.session() == session
                                && input.as_ref().map_or_else(
                                    || shown.as_ref().is_some_and(|old| **old == *l.pane.shown()),
                                    |input| input.matches(&l.pane.shown()),
                                )
                        })
                });
                menu.rebuild(window, cx, |menu, _, _| {
                    state.fill(menu, &shell, found.as_ref().filter(|_| valid), None)
                });
                window.refresh();
            })
            .ok();
        })
        .detach();
        menu
    } else {
        state.fill(menu, shell, None, None)
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
