//! 터미널의 파일 경로·OSC 8 링크 우클릭 동작. 파일 확인과 기본 앱 실행은 UI 밖에서 처리한다.

use gpui::WeakEntity;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use krill_app::i18n::catalog::{diff as dtext, file_format as ftext};
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
        match action {
            FileAction::Open => self.open_path_link(link, cx),
            FileAction::OpenDefault => krill_app::file_format::view::spawn_open(
                link.path,
                krill_app::file_format::open_default_command,
                cx,
            ),
            FileAction::Reveal => cx.reveal_path(&link.path),
            FileAction::CopyPath => {
                cx.write_to_clipboard(ClipboardItem::new_string(link.path.display().to_string()));
            }
        }
    }
}

struct MenuState {
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
    ) -> PopupMenu {
        menu = menu.action_context(self.focus.clone());
        if let Some(link) = link {
            for (id, label, action) in [
                (
                    "terminal-file-open",
                    text::TERMINAL_OPEN_FILE.get(),
                    FileAction::Open,
                ),
                (
                    "terminal-file-open-default",
                    ftext::OPEN_DEFAULT.get(),
                    FileAction::OpenDefault,
                ),
                (
                    "terminal-file-reveal",
                    ftext::REVEAL
                        .mac_or(krill_app::i18n::catalog::other_os::REVEAL)
                        .get(),
                    FileAction::Reveal,
                ),
                (
                    "terminal-file-copy-path",
                    dtext::EXPLORER_COPY_PATH.get(),
                    FileAction::CopyPath,
                ),
            ] {
                let shell = shell.clone();
                let link = link.clone();
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        div().id(id).debug_selector(move || id.into()).child(label)
                    })
                    .on_click(move |_, _, cx| {
                        shell
                            .update(cx, |shell, cx| {
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
    File(PathBuf),
    Text {
        row: Vec<krill_proto::screen::Cell>,
        column: u16,
        cwd: Option<PathBuf>,
    },
}

impl Target {
    fn resolve(self) -> Option<PathLink> {
        match self {
            Self::File(path) => path.is_file().then_some(PathLink {
                column: 0,
                cols: 0,
                path,
                line: None,
                col: None,
            }),
            Self::Text { row, column, cwd } => {
                Cache::default().link_at(&row, column, cwd.as_deref())
            }
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
        focus: s.focus.clone(),
        has_selection: s.live.get(&pane).is_some_and(|l| l.selection.is_some()),
        fork_enabled: s
            .peer_supports_fork(pane)
            .then(|| s.can_fork_pane(pane, cx)),
        can_continue: s.can_continue_pane(pane),
    };
    let target = (!s.layout.host(pane).is_remote())
        .then(|| {
            let live = s.live.get(&pane)?;
            let screen = live.pane.shown();
            let position = window.mouse_position();
            let (column, line) = cell_at(&live.painted, &screen, position)?;
            if let Some(path) = file_link_target(&screen, &live.painted, position) {
                Some(Target::File(path))
            } else if screen.link_at(line, column).is_none() {
                Some(Target::Text {
                    row: screen.rows.get(usize::from(line))?.clone(),
                    column,
                    cwd: s.pane_cwd(pane, cx),
                })
            } else {
                None
            }
        })
        .flatten();
    if let Some(target) = target {
        let shell = shell.clone();
        // 확인 중인 메뉴에 행을 끼워 넣으면 포인터 아래 동작이 바뀐다. 항목을 확정한 뒤 한 번에 연다.
        cx.spawn_in(window, async move |menu, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { target.resolve() })
                .await;
            menu.update_in(cx, |menu, window, cx| {
                menu.rebuild(window, cx, |menu, _, _| {
                    state.fill(menu, &shell, found.as_ref())
                });
                window.refresh();
            })
            .ok();
        })
        .detach();
        menu
    } else {
        state.fill(menu, shell, None)
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
