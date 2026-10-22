//! 사이드바 행과 패널 렌더(OWE-576). 색·글꼴·치수는 `crate::ui` 토큰.
//! MK-131: 호스트 섹션 헤더(`HostHeader`), 상태 점(`연결됨` / `연결 중` / `끊김`), 세션 수,
//! 끊긴 호스트 흐리게 유지 + '다시 연결' 버튼.
//! 호스트 섹션 헤더 배치 참고:
//! `.ref/orca/src/renderer/src/components/sidebar/worktree-list/rows/HostSectionHeader.tsx`
//! (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use std::path::PathBuf;

use gpui::{AnyElement, Context, Focusable as _, Window, div, prelude::*, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::Input;
use gpui_component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::SessionId;
use krill_proto::control::{AgentProfile, Program};

use super::{
    Activate, CHILD_INDENT, Collapse, Dismiss, Expand, KEY_CONTEXT, NewAgent1, NewAgent2,
    NewAgent3, NewAgent4, OpenInCursor, OpenInFinder, OpenInVsCode, OpenInZed, ROW_GROUP,
    ROW_HEIGHT, Rename, Row, SelectNext, SelectPrev, Sidebar, Target, TogglePin, basename,
    state_text, text,
};
use crate::hosts::{HostId, HostStatus};
use crate::ui;
use crate::ui::status::{self, Size};

/// 카드 제목 아래 세션 이름 공간을 확보한다. 상태 글리프는 카드 상태 칸에 맞춘다.
/// 카드 테두리 2 + 안쪽 여백 10 − 세션 행 바깥 여백 4.
const CARD_SESSION_INSET: f32 = 8.;
/// 폴더 행의 아이콘 아래에 세션 상태를 정렬한다(이름 아래로 중복 들여쓰지 않는다).
const FOLDER_SESSION_INSET: f32 = 34.;

#[derive(Clone)]
struct TreeMenuMeta {
    path: PathBuf,
    pinned: bool,
    is_primary: bool,
    current_parent: Option<PathBuf>,
    parent_candidates: Vec<(PathBuf, String)>,
    descendant_count: usize,
    sleeping: bool,
}

/// OWE-854: 세션 행 메뉴에 필요한 대상(행 위치·세션).
#[derive(Clone)]
struct SessionMenuMeta {
    index: usize,
    id: SessionId,
}

/// OWE-845: 저장소·폴더 헤더 메뉴 대상. `repo`면(로컬 저장소 헤더) '새 워크트리'도 붙인다.
#[derive(Clone)]
struct HeaderMenuMeta {
    path: PathBuf,
    repo: bool,
}

/// 호스트 헤더 상태점 색·라벨. 로컬 데몬에 다시 붙는 중의 '내 Mac' 끊김은 세션이 krill-holder에 남아 있어
/// 경고가 아니라 `preserve` 톤이다(STYLEGUIDE §7.1.1). 원격 호스트 끊김·처음부터 실패는 그대로 `destructive`.
pub(super) fn host_status_tone(
    status: HostStatus,
    local_reconnecting: bool,
    reason: Option<&'static str>,
    p: &ui::Palette,
) -> (gpui::Hsla, &'static str) {
    match status {
        HostStatus::Connected => (p.success, text::HOST_CONNECTED.get()),
        HostStatus::Connecting => (p.warning, text::HOST_CONNECTING.get()),
        HostStatus::Disconnected if local_reconnecting => {
            (p.preserve, text::HOST_RECONNECTING.get())
        }
        HostStatus::Disconnected => (
            p.destructive,
            reason.unwrap_or_else(|| text::HOST_DISCONNECTED.get()),
        ),
    }
}

/// 행 컨텍스트 메뉴: 세션 행이면 세션 제어(열기·이어하기·종료)를 먼저 두고,
/// 여기서 새 터미널·에이전트(N4), Open in(폴더가 있을 때), 워크트리면 핀·이름 변경·계보 작업(MK-176).
#[expect(
    clippy::too_many_arguments,
    reason = "context menu builder receives row state, host, session meta, worktree lineage meta, and gpui contexts"
)]
fn row_menu(
    mut menu: PopupMenu,
    this: &gpui::WeakEntity<Sidebar>,
    host: &HostId,
    folder: Option<&PathBuf>,
    session: Option<&SessionMenuMeta>,
    tree: Option<&TreeMenuMeta>,
    header: Option<&HeaderMenuMeta>,
    agents: &[AgentProfile],
    pr: Option<&crate::pull_request::Status>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    if let Some(sess) = session {
        let (open_this, open_index, open_id) = (this.clone(), sess.index, sess.id);
        menu = menu.item(
            PopupMenuItem::new(text::OPEN_SESSION.get()).on_click(move |_, _, cx| {
                open_this
                    .update(cx, |s, cx| s.select_session(open_index, open_id, cx))
                    .ok();
            }),
        );
        let (rename_this, rename_index, rename_id) = (this.clone(), sess.index, sess.id);
        menu = menu.item(
            PopupMenuItem::new(text::RENAME.get()).on_click(move |_, _, cx| {
                rename_this
                    .update(cx, |s, cx| {
                        s.request_rename_session(rename_index, rename_id, cx);
                    })
                    .ok();
            }),
        );
        let (cont_this, cont_index, cont_id) = (this.clone(), sess.index, sess.id);
        menu = menu.item(PopupMenuItem::new(text::CONTINUE_SESSION.get()).on_click(
            move |_, _, cx| {
                cont_this
                    .update(cx, |s, cx| {
                        s.request_continue_session(cont_index, cont_id, cx);
                    })
                    .ok();
            },
        ));
        let (close_this, close_index, close_id) = (this.clone(), sess.index, sess.id);
        menu = menu
            .item(
                PopupMenuItem::new(text::CLOSE_SESSION.get())
                    .icon(IconName::Close)
                    .on_click(move |_, _, cx| {
                        close_this
                            .update(cx, |s, cx| {
                                s.request_close_session(close_index, close_id, cx);
                            })
                            .ok();
                    }),
            )
            .separator();
    }
    if let Some(folder) = folder {
        let (term_this, term_host, term_folder) = (this.clone(), host.clone(), folder.clone());
        menu = menu.item(
            PopupMenuItem::new(text::NEW_TERMINAL.get())
                .icon(IconName::SquareTerminal)
                .on_click(move |_, window, cx| {
                    term_this
                        .update(cx, |s, cx| {
                            s.new_session_on(
                                term_host.clone(),
                                term_folder.clone(),
                                Program::Login,
                                Some(window),
                                cx,
                            );
                        })
                        .ok();
                }),
        );
        for profile in agents {
            let (this, host, folder, id) = (
                this.clone(),
                host.clone(),
                folder.clone(),
                profile.id.clone(),
            );
            menu = menu.item(
                PopupMenuItem::new(profile.name.clone())
                    .icon(ui::icons::agent_menu_icon(&id))
                    .on_click(move |_, window, cx| {
                        this.update(cx, |s, cx| {
                            s.new_session_on(
                                host.clone(),
                                folder.clone(),
                                Program::Agent { id: id.clone() },
                                Some(window),
                                cx,
                            );
                        })
                        .ok();
                    }),
            );
        }
        if host.is_local() && session.is_none() {
            menu = menu.separator();
            for target in Target::ALL {
                let (this, folder) = (this.clone(), folder.clone());
                menu = menu.item(
                    PopupMenuItem::new(format!("{}{}", target.label(), text::OPEN_IN.get()))
                        .icon(IconName::ExternalLink)
                        .on_click(move |_, _, cx| {
                            this.update(cx, |s, cx| s.open_in(target, folder.clone(), cx))
                                .ok();
                        }),
                );
            }
        }
    }
    if let Some(meta) = tree {
        let (pin, edit) = (meta.path.clone(), meta.path.clone());
        let (a, b) = (this.clone(), this.clone());
        let (creator, create_path) = (this.clone(), meta.path.clone());
        menu = menu
            .item(
                PopupMenuItem::new(
                    pr.and_then(crate::pull_request::Status::guidance)
                        .unwrap_or(crate::pull_request::CREATE.get()),
                )
                .disabled(pr.and_then(crate::pull_request::Status::guidance).is_some())
                .on_click(move |_, _, cx| {
                    creator
                        .update(cx, |s, cx| s.create_pr(create_path.clone(), cx))
                        .ok();
                }),
            )
            .separator()
            .item(
                PopupMenuItem::new(text::PIN.get())
                    .icon(IconName::Star)
                    .checked(meta.pinned)
                    .on_click(move |_, _, cx| {
                        a.update(cx, |s, cx| s.toggle_pin(pin.clone(), cx)).ok();
                    }),
            )
            .item(
                PopupMenuItem::new(text::RENAME.get())
                    .icon(Icon::default().data(ui::icons::PENCIL_SVG))
                    .on_click(move |_, window, cx| {
                        b.update(cx, |s, cx| s.rename(edit.clone(), window, cx))
                            .ok();
                    }),
            );

        if host.is_local() {
            let (child_this, child_parent) = (this.clone(), meta.path.clone());
            menu = menu.separator().item(
                PopupMenuItem::new(text::CREATE_CHILD_WORKTREE.get())
                    .icon(IconName::Plus)
                    .on_click(move |_, window, cx| {
                        child_this
                            .update(cx, |s, cx| {
                                s.new_child_worktree(child_parent.clone(), window, cx);
                            })
                            .ok();
                    }),
            );

            if !meta.parent_candidates.is_empty() {
                let (sub_this, sub_path, candidates, cur_parent) = (
                    this.clone(),
                    meta.path.clone(),
                    meta.parent_candidates.clone(),
                    meta.current_parent.clone(),
                );
                menu = menu.submenu(
                    text::SET_PARENT_WORKSPACE.get(),
                    window,
                    cx,
                    move |mut sub, _, _| {
                        for (cand_path, cand_label) in &candidates {
                            let (this, child, parent) =
                                (sub_this.clone(), sub_path.clone(), cand_path.clone());
                            sub = sub.item(
                                PopupMenuItem::new(cand_label.clone())
                                    .checked(cur_parent.as_ref() == Some(cand_path))
                                    .on_click(move |_, _, cx| {
                                        this.update(cx, |s, cx| {
                                            s.set_worktree_parent(
                                                child.clone(),
                                                parent.clone(),
                                                cx,
                                            );
                                        })
                                        .ok();
                                    }),
                            );
                        }
                        sub
                    },
                );
            }

            if meta.current_parent.is_some() {
                let (rm_this, rm_path) = (this.clone(), meta.path.clone());
                menu = menu.item(PopupMenuItem::new(text::REMOVE_PARENT.get()).on_click(
                    move |_, _, cx| {
                        rm_this
                            .update(cx, |s, cx| {
                                s.clear_worktree_parent(&rm_path, cx);
                            })
                            .ok();
                    },
                ));
            }

            menu = menu.separator();
            if meta.sleeping {
                let (wake_this, wake_path) = (this.clone(), meta.path.clone());
                menu = menu.item(PopupMenuItem::new(text::WAKE_WORKTREE.get()).on_click(
                    move |_, _, cx| {
                        wake_this
                            .update(cx, |s, cx| s.wake_worktree(&wake_path, cx))
                            .ok();
                    },
                ));
            } else {
                let (sleep_this, sleep_path) = (this.clone(), meta.path.clone());
                menu = menu.item(PopupMenuItem::new(text::SLEEP_WORKTREE.get()).on_click(
                    move |_, _, cx| {
                        sleep_this
                            .update(cx, |s, cx| s.sleep_worktree(sleep_path.clone(), cx))
                            .ok();
                    },
                ));
            }

            if meta.descendant_count > 0 {
                let (sleep_all_this, sleep_all_path) = (this.clone(), meta.path.clone());
                let label = text::SLEEP_WITH_DESCENDANTS
                    .format(&[("n", &meta.descendant_count.to_string())]);
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    sleep_all_this
                        .update(cx, |s, cx| {
                            s.sleep_with_descendants(sleep_all_path.clone(), cx);
                        })
                        .ok();
                }));
            }

            if !meta.is_primary || meta.descendant_count > 0 {
                menu = menu.separator();
                if !meta.is_primary {
                    // OWE-847: 즉시 삭제하지 않고 확인 모달을 연다.
                    let (confirm_this, confirm_path) = (this.clone(), meta.path.clone());
                    menu = menu.item(
                        PopupMenuItem::new(text::DELETE_WORKTREE.get())
                            .icon(Icon::default().data(ui::icons::TRASH_SVG))
                            .on_click(move |_, _, cx| {
                                confirm_this
                                    .update(cx, |s, cx| {
                                        s.request_delete_worktree(confirm_path.clone(), false, cx);
                                    })
                                    .ok();
                            }),
                    );
                }
                if meta.descendant_count > 0 {
                    let (del_all_this, del_all_path) = (this.clone(), meta.path.clone());
                    let label = text::DELETE_WITH_DESCENDANTS
                        .format(&[("n", &meta.descendant_count.to_string())]);
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .icon(Icon::default().data(ui::icons::TRASH_SVG))
                            .on_click(move |_, _, cx| {
                                del_all_this
                                    .update(cx, |s, cx| {
                                        s.request_delete_worktree(del_all_path.clone(), true, cx);
                                    })
                                    .ok();
                            }),
                    );
                }
            }
        }
    }
    // OWE-845: 저장소·폴더 헤더 우클릭 메뉴(저장소면 새 워크트리, 목록에서 제거).
    if let Some(header) = header {
        menu = menu.separator();
        if header.repo {
            let (wt_this, wt_repo) = (this.clone(), header.path.clone());
            menu = menu.item(
                PopupMenuItem::new(text::NEW_WORKTREE.get())
                    .icon(IconName::Plus)
                    .on_click(move |_, window, cx| {
                        wt_this
                            .update(cx, |s, cx| s.new_worktree(wt_repo.clone(), window, cx))
                            .ok();
                    }),
            );
        }
        let (remove_this, remove_path) = (this.clone(), header.path.clone());
        menu = menu.item(
            PopupMenuItem::new(text::REMOVE_PROJECT.get())
                .icon(Icon::default().data(ui::icons::TRASH_SVG))
                .on_click(move |_, _, cx| {
                    remove_this
                        .update(cx, |s, cx| s.remove_project(remove_path.clone(), cx))
                        .ok();
                }),
        );
    }
    menu
}

impl Sidebar {
    /// Rebuilt with the row model; lookup stays constant-time while scrolling a large card.
    pub(super) fn session_card_index(&self, index: usize) -> Option<usize> {
        self.card_parents.get(index).copied().flatten()
    }

    fn worktree_card_active(&self, index: usize, path: &std::path::Path) -> bool {
        let host = self.row_host(index);
        let snapshot = self.snapshot_for(host);
        let focused_wt = self
            .focused
            .and_then(|id| self.folder_on(host, id))
            .filter(|wt| {
                snapshot
                    .trees
                    .values()
                    .any(|list| list.iter().any(|t| t == wt))
            });
        self.cursor == Some(index)
            || focused_wt == Some(path)
            || (focused_wt.is_none()
                && self.cursor.is_none()
                && self.selected_worktree.as_deref().map_or_else(
                    || {
                        snapshot
                            .trees
                            .keys()
                            .next()
                            .is_some_and(|first| first == path)
                    },
                    |sel| sel == path,
                ))
    }

    /// 카드 머리줄과 세션을 따로 가상화하되 표면·테두리는 끊김 없이 잇는다.
    fn card_segment(
        &self,
        index: usize,
        row: &Row,
        element: gpui::Stateful<gpui::Div>,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if matches!(row, Row::Worktree(_)) {
            return element.when(
                self.session_card_index(index + 1) == Some(index)
                    && self.card_list.contains_row(index + 1),
                |d| d.mb_0().pb_0().border_b_0().rounded_b_none(),
            );
        }
        let Some(parent) = self.session_card_index(index) else {
            return element;
        };
        let Some(Row::Worktree(path)) = self.rows.get(parent) else {
            return element;
        };
        let last = self.session_card_index(index + 1) != Some(parent);
        let depth = if self.row_host(parent).is_local() {
            self.lineage_depth(path)
        } else {
            0
        };
        let p = ui::palette(cx);
        element
            .h(px(if last { ROW_HEIGHT } else { 26. }))
            .mx_0()
            .ml(px(self.worktree_margin(depth)))
            .mr_1p5()
            .pl(px(CARD_SESSION_INSET + CHILD_INDENT) + ui::SPACE_1)
            .pr_2p5()
            .rounded_none()
            .border_l_1()
            .border_r_1()
            .border_color(ui::TRANSPARENT)
            .when(last, |d| d.mb_0p5().border_b_1().rounded_b(ui::RADIUS_MD))
            .when(self.worktree_card_active(parent, path), |d| {
                d.bg(p.worktree_sidebar)
                    .border_color(p.sidebar_ring.opacity(0.65))
            })
            .when(depth > 0 && !self.worktree_card_active(parent, path), |d| {
                d.border_l_2().border_color(p.worktree_sidebar_border)
            })
    }

    /// MK-078: 행마다 스크린 리더 역할(목록 항목)·이름·"n개 중 m번째"를 붙인다. 가상 리스트라 위치는 직접 알린다.
    pub(super) fn render_row(
        &self,
        index: usize,
        row: &Row,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = ui::palette(cx);
        let selected = self.cursor == Some(index);
        let dimmed = self.is_row_dimmed(index);
        let row_host = self.row_host(index).clone();
        let host_indent = self.host_indent();
        let base = div()
            .id(super::viewport::row_id(&row_host, row))
            .group(ROW_GROUP)
            .role(gpui::Role::ListItem)
            .aria_position_in_set(index + 1)
            .aria_size_of_set(self.rows.len())
            .aria_selected(selected)
            .relative()
            .flex_none()
            .overflow_hidden()
            .when(self.card_list.pending_reveal.get() == Some(index), |d| {
                let pending = self.card_list.pending_reveal.clone();
                d.child(
                    gpui::canvas(
                        move |bounds, window, _| {
                            if pending.get() == Some(index) {
                                pending.set(None);
                                window.request_autoscroll(bounds);
                            }
                        },
                        |_, (), _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .when(dimmed, |d| d.opacity(0.45));
        let single_line = |d: gpui::Stateful<gpui::Div>| {
            d.h(px(ROW_HEIGHT))
                .px_2()
                .mx_1()
                .rounded(ui::RADIUS_SM)
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(ui::TEXT_MD)
                .when(selected, |d| {
                    d.bg(p.sidebar_accent)
                        .text_color(p.sidebar_accent_foreground)
                })
        };
        let heading = |d: gpui::Stateful<gpui::Div>| {
            single_line(d)
                .text_size(ui::TEXT_XS)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(p.muted_foreground)
                .when(selected, |d| d.text_color(p.sidebar_accent_foreground))
        };
        let repo_heading = |d: gpui::Stateful<gpui::Div>| {
            d.h(ui::SIDEBAR_HEADER_HEIGHT)
                .px_2()
                .mx_1()
                .rounded(ui::RADIUS_SM)
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(ui::TEXT_MD)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(p.sidebar_foreground)
                .when(selected, |d| {
                    d.bg(p.sidebar_accent)
                        .text_color(p.sidebar_accent_foreground)
                })
        };
        let element = match row {
            Row::HostHeader {
                host,
                label,
                status,
                sessions,
            } => {
                let info = host.remote_id().and_then(|id| self.remote_info.get(id));
                let detail = info.and_then(|i| i.detail.clone());
                let error = info.and_then(|i| i.error.clone());
                let closed = self.collapsed_hosts.contains(host);
                let reason = host
                    .remote_id()
                    .and_then(|id| self.remote_hosts.iter().find(|r| r.id == id))
                    .and_then(|r| r.error.as_deref())
                    .map(disconnect_reason);
                let (dot_color, status_label) = host_status_tone(
                    *status,
                    !host.is_remote() && self.local_reconnecting(),
                    reason,
                    p,
                );
                let fold_host = host.clone();
                let header_key = host.as_key().to_owned();
                let status_key = header_key.clone();
                let count_key = header_key.clone();
                let detail_key = header_key.clone();
                let error_key = header_key.clone();
                let a11y = format!(
                    "{label}, {status_label}, {} {sessions}",
                    text::SESSIONS.get()
                );
                heading(base)
                    .debug_selector(move || format!("host-header-{header_key}"))
                    .aria_label(a11y)
                    .aria_expanded(!closed)
                    .cursor_pointer()
                    .pl_1()
                    .pr_1p5()
                    .hover(|d| {
                        d.bg(p.sidebar_accent)
                            .text_color(p.sidebar_accent_foreground)
                    })
                    .child(
                        Button::new(("host-fold", index))
                            .ghost()
                            .xsmall()
                            .icon(if closed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .accessibility_label(if closed {
                                text::UNFOLD.get()
                            } else {
                                text::FOLD.get()
                            })
                            .on_click({
                                let fold_host = fold_host.clone();
                                cx.listener(move |s, _, _, cx| {
                                    cx.stop_propagation();
                                    s.fold_host(&fold_host, None, cx);
                                })
                            }),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("host-status-{status_key}"))
                            .size(ui::STATE_DOT)
                            .rounded_full()
                            .flex_none()
                            .bg(dot_color),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.sidebar_foreground)
                            .child(div().truncate().child(label.clone()))
                            .when_some(detail, |d, detail| {
                                d.child(
                                    div()
                                        .debug_selector(move || format!("host-route-{detail_key}"))
                                        .text_size(ui::TEXT_2XS)
                                        .text_color(p.muted_foreground)
                                        .child(detail),
                                )
                            })
                            .when_some(error, |d, error| {
                                d.child(
                                    div()
                                        .debug_selector(move || format!("host-error-{error_key}"))
                                        .truncate()
                                        .text_size(ui::TEXT_2XS)
                                        .text_color(p.destructive)
                                        .child(error),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(status_label),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("host-count-{count_key}"))
                            .flex_none()
                            .px_1()
                            .rounded(ui::RADIUS_XS)
                            .bg(p.muted)
                            .font_family(ui::FONT_MONO)
                            .text_size(ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(sessions.to_string()),
                    )
                    .when(
                        *status == HostStatus::Disconnected && host.is_remote(),
                        |d| {
                            let reconnect_host = host.clone();
                            let reconnect_key = host.as_key().to_owned();
                            d.child(
                                Button::new(("host-reconnect", index))
                                    .debug_selector(move || {
                                        format!("host-reconnect-{reconnect_key}")
                                    })
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::RefreshCw)
                                    .label(text::HOST_RECONNECT.get())
                                    .accessibility_label(text::HOST_RECONNECT.get())
                                    .tooltip(text::HOST_RECONNECT.get())
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        cx.stop_propagation();
                                        s.reconnect_host(reconnect_host.clone(), cx);
                                    })),
                            )
                        },
                    )
                    .when(*status == HostStatus::Connected && host.is_remote(), |d| {
                        let disconnect_host = host.clone();
                        let disconnect_key = host.as_key().to_owned();
                        d.child(
                            Button::new(("host-disconnect", index))
                                .debug_selector(move || format!("host-disconnect-{disconnect_key}"))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .label(text::HOST_DISCONNECT.get())
                                .accessibility_label(text::HOST_DISCONNECT.get())
                                .tooltip(text::HOST_DISCONNECT.get())
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    cx.stop_propagation();
                                    s.disconnect_host(disconnect_host.clone(), cx);
                                })),
                        )
                    })
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.cursor = Some(index);
                        s.fold_host(&fold_host, None, cx);
                    }))
            }
            Row::GroupHeader { key, count } => {
                let closed = self.collapsed_groups.contains(key);
                let slug = key.slug();
                let count_slug = slug;
                let group_key = *key;
                let label = key.label();
                heading(base)
                    .ml(px(4. + host_indent))
                    .debug_selector(move || format!("group-header-{slug}"))
                    .aria_label(format!("{label} ({count})"))
                    .aria_expanded(!closed)
                    .cursor_pointer()
                    .pl_1()
                    .pr_1p5()
                    .hover(|d| {
                        d.bg(p.sidebar_accent)
                            .text_color(p.sidebar_accent_foreground)
                    })
                    .child(
                        Button::new(("group-fold", index))
                            .ghost()
                            .xsmall()
                            .icon(if closed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .accessibility_label(if closed {
                                text::UNFOLD.get()
                            } else {
                                text::FOLD.get()
                            })
                            .on_click(cx.listener(move |s, _, _, cx| {
                                cx.stop_propagation();
                                s.fold_group(group_key, None, cx);
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.sidebar_foreground)
                            .child(label),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("group-count-{count_slug}"))
                            .flex_none()
                            .px_1()
                            .rounded(ui::RADIUS_XS)
                            .bg(p.muted)
                            .font_family(ui::FONT_MONO)
                            .text_size(ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(count.to_string()),
                    )
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.cursor = Some(index);
                        s.fold_group(group_key, None, cx);
                    }))
            }
            Row::NeedsYou => heading(base)
                .aria_label(text::NEEDS_YOU.get())
                .text_color(p.agent_question)
                .child(Icon::new(IconName::Bell).xsmall())
                .child(text::NEEDS_YOU.get()),
            Row::Loose => heading(base)
                .aria_label(text::SESSIONS.get())
                .child(Icon::new(IconName::SquareTerminal).xsmall())
                .child(text::SESSIONS.get()),
            Row::OfflineHosts(n) => {
                let label = text::HOST_OFFLINE_GROUP.format(&[("n", &n.to_string())]);
                let open = self.show_offline_hosts;
                heading(base)
                    .debug_selector(|| "host-offline-group".into())
                    .aria_label(label.clone())
                    .aria_expanded(open)
                    .cursor_pointer()
                    .pl_1()
                    .pr_1p5()
                    .text_color(p.muted_foreground)
                    .hover(|d| {
                        d.bg(p.sidebar_accent)
                            .text_color(p.sidebar_accent_foreground)
                    })
                    .child(
                        Icon::new(if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .xsmall(),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.cursor = Some(index);
                        s.toggle_offline_hosts(None, cx);
                    }))
            }
            Row::Repository(path) => {
                let repo = path.clone();
                let fold_btn = path.clone();
                let fold_row = path.clone();
                let closed = self.collapsed.contains(path);
                // OWE-900: 헤더에 워크트리 수를 표시한다(자기 자신 제외).
                // 접힌 다량 유휴 프로젝트도 규모를 알 수 있다.
                let wt_count = self
                    .snapshot_for(&row_host)
                    .trees
                    .get(path)
                    .map_or(0, |list| {
                        list.iter()
                            .filter(|t| t.as_path() != path.as_path())
                            .count()
                    });
                repo_heading(base)
                    .ml(px(4. + host_indent))
                    .debug_selector(move || format!("repo-header-{index}"))
                    .aria_label(format!(
                        "{} {} {wt_count}",
                        text::REPOSITORY.get(),
                        basename(path)
                    ))
                    .aria_expanded(!closed)
                    .cursor_pointer()
                    .hover(|d| {
                        d.bg(p.sidebar_accent)
                            .text_color(p.sidebar_accent_foreground)
                    })
                    .child(
                        Button::new(("repo-fold", index))
                            .ghost()
                            .xsmall()
                            .icon(if closed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .accessibility_label(if closed {
                                text::UNFOLD.get()
                            } else {
                                text::FOLD.get()
                            })
                            .on_click(cx.listener(move |s, _, _, cx| {
                                cx.stop_propagation();
                                s.cursor = Some(index);
                                s.fold(&fold_btn, None, cx);
                            })),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("repo-icon-{index}"))
                            .child(ui::icons::project_header_icon(
                                row_host
                                    .is_local()
                                    .then(|| self.project_icon(path))
                                    .flatten(),
                                row_host
                                    .is_local()
                                    .then(|| self.project_badge_color(path))
                                    .flatten(),
                                ui::ICON_MD,
                                p.muted_foreground,
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(if row_host.is_local() {
                                self.project_header_name(path)
                            } else {
                                basename(path)
                            }),
                    )
                    .when(wt_count > 0, |d| {
                        d.child(
                            div()
                                .debug_selector(move || format!("repo-count-{index}"))
                                .flex_none()
                                .px_1()
                                .rounded(ui::RADIUS_XS)
                                .bg(p.muted)
                                .font_family(ui::FONT_MONO)
                                .text_size(ui::TEXT_2XS)
                                .text_color(p.muted_foreground)
                                .child(wt_count.to_string()),
                        )
                    })
                    .when(row_host.is_local(), |d| {
                        d.child(
                            div()
                                .invisible()
                                .group_hover(ROW_GROUP, Styled::visible)
                                .child(
                                    Button::new(("new-worktree", index))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Plus)
                                        .accessibility_label(text::NEW_WORKTREE.get())
                                        .tooltip(text::NEW_WORKTREE.get())
                                        .on_click(cx.listener(move |s, _, window, cx| {
                                            cx.stop_propagation();
                                            s.new_worktree(repo.clone(), window, cx);
                                        })),
                                ),
                        )
                    })
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.cursor = Some(index);
                        s.fold(&fold_row, None, cx);
                    }))
                    // Orca처럼 프로젝트 머리줄을 끌어다 다른 프로젝트 위에 놓아 순서를 바꾼다(내 Mac 프로젝트만).
                    .when(row_host.is_local(), |d| {
                        let drop_to = path.clone();
                        let ring = p.ring;
                        d.on_drag(
                            ProjectDrag {
                                path: path.clone(),
                                name: basename(path).into(),
                            },
                            |drag, _, _, cx| cx.new(|_| drag.clone()),
                        )
                        .drag_over::<ProjectDrag>(move |d, _, _, _| {
                            d.border_t_2().border_color(ring)
                        })
                        .on_drop(cx.listener(
                            move |s, drag: &ProjectDrag, _, cx| {
                                cx.stop_propagation();
                                s.move_project(&drag.path, &drop_to, cx);
                            },
                        ))
                    })
            }
            Row::Folder(path) => {
                let (fold_btn, fold_row, fresh, fresh_host) =
                    (path.clone(), path.clone(), path.clone(), row_host.clone());
                let closed = self.collapsed.contains(path);
                repo_heading(base)
                    .ml(px(4. + host_indent))
                    .debug_selector(move || format!("folder-header-{index}"))
                    .aria_label(format!("{} {}", text::FOLDER.get(), basename(path)))
                    .aria_expanded(!closed)
                    .cursor_pointer()
                    .hover(|d| {
                        d.bg(p.sidebar_accent)
                            .text_color(p.sidebar_accent_foreground)
                    })
                    .child(
                        Button::new(("folder-fold", index))
                            .ghost()
                            .xsmall()
                            .icon(if closed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .accessibility_label(if closed {
                                text::UNFOLD.get()
                            } else {
                                text::FOLD.get()
                            })
                            .on_click(cx.listener(move |s, _, _, cx| {
                                cx.stop_propagation();
                                s.cursor = Some(index);
                                s.fold(&fold_btn, None, cx);
                            })),
                    )
                    .child(ui::icons::project_header_icon(
                        row_host
                            .is_local()
                            .then(|| self.project_icon(path))
                            .flatten(),
                        row_host
                            .is_local()
                            .then(|| self.project_badge_color(path))
                            .flatten(),
                        ui::ICON_MD,
                        p.muted_foreground,
                    ))
                    .child(
                        div()
                            .debug_selector(move || format!("folder-title-{index}"))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(if row_host.is_local() {
                                self.project_header_name(path)
                            } else {
                                basename(path)
                            }),
                    )
                    .child(
                        div()
                            .invisible()
                            .group_hover(ROW_GROUP, Styled::visible)
                            .child(
                                Button::new(("folder-new-session", index))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::Plus)
                                    .accessibility_label(text::NEW_TERMINAL.get())
                                    .tooltip(text::NEW_TERMINAL.get())
                                    .on_click(cx.listener(move |s, _, window, cx| {
                                        cx.stop_propagation();
                                        s.new_session_on(
                                            fresh_host.clone(),
                                            fresh.clone(),
                                            Program::Login,
                                            Some(window),
                                            cx,
                                        );
                                    })),
                            ),
                    )
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.cursor = Some(index);
                        s.fold(&fold_row, None, cx);
                    }))
            }
            Row::HiddenWorktrees(repo, count, expanded) => {
                super::external::render_hidden_row(base, repo, *count, *expanded, cx)
                    .ml(px(6. + CHILD_INDENT + host_indent))
            }
            Row::Worktree(path) => self.render_worktree(base, index, path, cx),
            Row::Session(session, nested) => {
                let id = session.id;
                let unread = row_host.is_local() && self.is_unread(id);
                let state = state_text(session.status.state);
                let (name, detail) = self.session_label(session);
                let inferred = session
                    .status
                    .badge()
                    .map(|_| text::INFERRED_STATUS_TOOLTIP.get());
                let state_tip = inferred.map_or_else(
                    || state.to_owned(),
                    |explanation| format!("{state} · {explanation}"),
                );
                // OWE-707: 실행 당시 계정 배지(라벨 축약 2자, 툴팁 전체 이름).
                // OWE-662 상태 글리프·에이전트 아이콘 옆이 아니라 행 끝 배지들과 함께 둬
                // 좁은 행에서도 이름이 밀리지 않게 한다. 이어가기 칩은 OWE-699까지 숨긴다.
                let account_label = session
                    .account
                    .as_ref()
                    .map(|a| a.label.trim())
                    .filter(|l| !l.is_empty());
                let app_settings = crate::settings::current(cx);
                let session_cache_secs = app_settings
                    .prompt_cache_timer_enabled
                    .then(|| {
                        status::session_cache_remaining_secs(
                            &session.status.agent,
                            session.status.state,
                            session.status.since_unix_ns,
                            self.now_unix_ns(),
                            app_settings.prompt_cache_ttl_secs,
                        )
                    })
                    .flatten();
                let inside_card = self.session_card_index(index).is_some();
                single_line(base)
                    .debug_selector(move || format!("sidebar-session-{index}"))
                    .h(px(if *nested { 26. } else { ROW_HEIGHT }))
                    .aria_label(format!(
                        "{name}{}, {state}{}{}{}",
                        if detail.is_empty() {
                            String::new()
                        } else {
                            format!(" · {detail}")
                        },
                        account_label
                            .map(|l| {
                                crate::i18n::catalog::sidebar::SESSION_ACCOUNT
                                    .format(&[("label", l)])
                            })
                            .map(|b| format!(", {b}"))
                            .unwrap_or_default(),
                        inferred.map(|b| format!(", {b}")).unwrap_or_default(),
                        if unread {
                            format!(", {}", text::UNREAD.get())
                        } else {
                            String::new()
                        }
                    ))
                    .when(*nested, |d| {
                        // 한 단계의 세션 들여쓰기만 더한다. 호스트·계보 여백은 카드
                        // 표면이 이미 갖고 있으므로 카드 안에서는 다시 누적하지 않는다.
                        let pl = match &session.worktree {
                            Some(tree) => {
                                let depth = if row_host.is_local() {
                                    self.lineage_depth(tree)
                                } else {
                                    0
                                };
                                self.worktree_margin(depth) + CARD_SESSION_INSET + CHILD_INDENT
                            }
                            None => host_indent + FOLDER_SESSION_INSET + CHILD_INDENT,
                        };
                        d.pl(px(pl)).pr_2()
                    })
                    .cursor_pointer()
                    .text_color(p.sidebar_foreground)
                    .hover(|d| {
                        d.bg(if inside_card {
                            p.worktree_sidebar_accent
                        } else {
                            p.sidebar_accent
                        })
                        .text_color(p.sidebar_accent_foreground)
                    })
                    .child(
                        status::status_glyph_slot(
                            session.status.state,
                            unread,
                            Size::Sm,
                            self.spinner_slots(index),
                            cx,
                        )
                        .debug_selector(move || format!("session-state-{index}"))
                        .id(("session-status", index))
                        .aria_label(state_tip.clone())
                        .tooltip(move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(state_tip.clone())
                                .build(window, cx)
                        }),
                    )
                    .child(status::agent_icon(
                        status::infer_agent_from_status(&session.status, &name),
                        ui::ICON_XS,
                        p.muted_foreground,
                    ))
                    .child(
                        div()
                            .debug_selector(move || format!("session-title-{index}"))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(name),
                    )
                    .children(
                        row_host
                            .is_local()
                            .then(|| {
                                super::external::orch_parts_from_store(
                                    &self.disk.orchestration,
                                    id.0,
                                )
                            })
                            .flatten()
                            .map(|ob| super::external::session_orch_element(&ob, p)),
                    )
                    .children(session_cache_secs.map(|secs| {
                        let tip = text::CACHE_TIMER_TOOLTIP.get().to_owned();
                        div()
                            .id(("session-cache-timer", index))
                            .aria_label(format!(
                                "{}: {}",
                                tip,
                                status::format_cache_countdown(secs)
                            ))
                            .debug_selector(move || format!("session-cache-timer-{index}"))
                            .flex_none()
                            .child(status::cache_timer_chip(secs, cx))
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                            })
                    }))
                    .children(account_label.map(|l| {
                        let short = status::abbreviate_account_label(l);
                        let full = l.to_owned();
                        let tip = text::SESSION_ACCOUNT.format(&[("label", &full)]);
                        div()
                            .id(("account-badge", index))
                            .aria_label(tip.clone())
                            .debug_selector(move || format!("account-badge-{index}"))
                            .flex_none()
                            .px_1()
                            .rounded(ui::RADIUS_XS)
                            .bg(p.muted)
                            .text_size(ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(short)
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                            })
                    }))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        cx.stop_propagation();
                        s.select_session(index, id, cx);
                    }))
            }
        };
        self.card_segment(index, row, element, cx)
    }

    /// 워크트리 카드 제목 자리(OWE-847): 이름 변경 중이면 인라인 입력창 + 저장(✓)·취소(✕) 버튼
    /// (Enter 저장·Esc 취소), 아니면 기존 제목 텍스트를 돌려준다.
    fn card_title_el(
        &self,
        path: &std::path::Path,
        card_title: String,
        title_size: gpui::Rems,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some((_, input)) = self.editing.as_ref().filter(|(p, _)| p == path) {
            let input = input.clone();
            return div()
                .id(("rename-inline", index))
                .debug_selector(move || format!("rename-inline-{index}"))
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_0p5()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(div().flex_1().min_w_0().child(Input::new(&input).small()))
                .child(
                    div()
                        .flex_none()
                        .debug_selector(move || format!("rename-inline-save-{index}"))
                        .child(
                            Button::new(("rename-inline-save", index))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Check)
                                .accessibility_label(text::SAVE.get())
                                .tooltip(text::SAVE.get())
                                .on_click(cx.listener(|s, _, window, cx| {
                                    cx.stop_propagation();
                                    s.finish_rename(Some(window), cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .debug_selector(move || format!("rename-inline-cancel-{index}"))
                        .child(
                            Button::new(("rename-inline-cancel", index))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .accessibility_label(text::CANCEL.get())
                                .tooltip(text::CANCEL.get())
                                .on_click(cx.listener(|s, _, window, cx| {
                                    cx.stop_propagation();
                                    s.cancel_rename(Some(window), cx);
                                })),
                        ),
                )
                .capture_key_down(cx.listener(|s, e: &gpui::KeyDownEvent, window, cx| {
                    if e.keystroke.key == "escape" {
                        cx.stop_propagation();
                        s.cancel_rename(Some(window), cx);
                    }
                }))
                .into_any_element();
        }
        div()
            .debug_selector(move || format!("worktree-title-{index}"))
            .min_w_0()
            .truncate()
            .text_size(title_size)
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(card_title)
            .into_any_element()
    }

    /// 워크트리 카드 왼쪽 여백(px): 프로젝트 > 워크트리 들여쓰기 + 호스트 들여쓰기 + 계보 깊이(단계당 14px).
    fn worktree_margin(&self, lineage_depth: usize) -> f32 {
        let depth = f32::from(u16::try_from(lineage_depth).unwrap_or(0));
        6. + CHILD_INDENT + self.host_indent() + depth * 14.
    }

    fn render_worktree(
        &self,
        base: gpui::Stateful<gpui::Div>,
        index: usize,
        path: &std::path::Path,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use krill_proto::control::AgentState;

        let p = ui::palette(cx);
        let row_host = self.row_host(index).clone();
        let is_default_tree = self.snapshot_for(&row_host).trees.contains_key(path);
        let is_active_worktree = self.worktree_card_active(index, path);
        let (fold, pin, edit, fresh, card_path, fresh_host) = (
            path.to_path_buf(),
            path.to_path_buf(),
            path.to_path_buf(),
            path.to_path_buf(),
            path.to_path_buf(),
            row_host.clone(),
        );
        let closed = self.collapsed.contains(path);
        let card_expanded = self.expanded_cards.contains(path);
        let pinned = self.prefs.get(path).is_some_and(|p| p.pinned);
        let name = self.name(path);
        // 이름을 바꿨으면 브랜치를 옆에 따로 보인다(스크린 리더 및 대량 행 모드).
        let branch = self
            .branch_on(&row_host, path)
            .filter(|b| *b != name)
            .map(str::to_owned);
        let custom_name = self
            .prefs
            .get(path)
            .map(|v| v.name.as_str())
            .filter(|s| !s.is_empty());
        let raw_branch = self.branch_on(&row_host, path);
        let folder_name = basename(path);
        let auto_title = row_host
            .is_local()
            .then(|| self.worktree_auto_title(path))
            .flatten();
        let card_title = if let Some(custom) =
            custom_name.filter(|n| !krill_proto::git::is_default_branch_or_worktree_name(n))
        {
            custom.to_owned()
        } else if is_default_tree {
            raw_branch.unwrap_or(&folder_name).to_owned()
        } else if let Some(b) =
            raw_branch.filter(|b| !krill_proto::git::is_default_branch_or_worktree_name(b))
        {
            b.rsplit('/').next().unwrap_or(b).replace('-', " ")
        } else if let Some(auto) = auto_title {
            auto.to_owned()
        } else if let Some(custom) = custom_name {
            custom.to_owned()
        } else if let Some(b) = raw_branch {
            b.rsplit('/').next().unwrap_or(b).replace('-', " ")
        } else {
            folder_name.replace('-', " ")
        };
        let branch_subtitle = raw_branch.map_or_else(|| folder_name, str::to_owned);
        let dirty = self
            .meta_for(&row_host)
            .get(path)
            .and_then(|m| m.dirty)
            .filter(|n| *n > 0);
        let tree_sessions: Vec<_> = self
            .snapshot_for(&row_host)
            .sessions
            .iter()
            .filter(|s| s.worktree.as_deref() == Some(path))
            .collect();
        let unread =
            row_host.is_local() && tree_sessions.iter().any(|s| self.unread.contains(&s.id));
        let summary_pairs: Vec<_> = tree_sessions
            .iter()
            .filter(|s| {
                !s.status.agent.is_empty()
                    || !matches!(s.status.state, AgentState::Exited | AgentState::Unknown)
            })
            .map(|s| {
                (
                    s.status.state,
                    status::infer_agent_from_status(
                        &s.status,
                        s.status.title.as_deref().unwrap_or_default(),
                    ),
                )
            })
            .collect();
        let summary = status::summarize(&summary_pairs);
        let tree_session_count = tree_sessions.len();
        let states = summary
            .groups
            .iter()
            .map(|g| state_text(g.state).to_owned())
            .chain(unread.then(|| text::UNREAD.get().to_owned()))
            .collect::<Vec<_>>();
        let comment = row_host.is_local().then(|| self.comment(path)).flatten();
        let checkpoint_badge = row_host
            .is_local()
            .then(|| {
                super::external::checkpoint_parts_from_store(&self.disk.store, path)
                    .and_then(|p| p.label(false))
            })
            .flatten();
        let mut label = super::external::worktree_a11y_label(text::TITLE.get(), &name, comment);
        for part in [
            branch.clone(),
            checkpoint_badge,
            dirty.map(|n| format!("{} {n}", text::DIRTY.get())),
        ]
        .into_iter()
        .flatten()
        .chain(states)
        {
            label.push_str(", ");
            label.push_str(&part);
        }
        let hover_only = |d: gpui::Div| d.invisible().group_hover(ROW_GROUP, Styled::visible);

        let left_status = if unread {
            status::status_glyph(AgentState::Done, true, Size::Md, cx).into_any_element()
        } else if let Some(top) = summary.groups.first() {
            // 완료(`Done`)는 체크 글리프, 그 외(작업 중·대기·종료·알 수 없음)는
            // 묶음 선두 상태의 글리프를 그대로 보인다. 유휴(묶음 없음)는 흐릿한 점.
            status::status_glyph_slot(top.state, false, Size::Md, self.spinner_slots(index), cx)
                .into_any_element()
        } else {
            div()
                .size(ui::STATE_DOT)
                .rounded_full()
                .bg(p.muted_foreground.opacity(0.5))
                .into_any_element()
        };

        let new_session_btn = {
            let button = Button::new(("new-session", index))
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .accessibility_label(text::NEW_TERMINAL.get())
                .tooltip(text::NEW_TERMINAL.get())
                .on_click(cx.listener(move |s, _, window, cx| {
                    cx.stop_propagation();
                    s.new_session_on(
                        fresh_host.clone(),
                        fresh.clone(),
                        Program::Login,
                        Some(window),
                        cx,
                    );
                }));
            let frame = div().flex_none();
            #[cfg(test)]
            let frame = frame.debug_selector(|| format!("worktree-new-session-{index}"));
            hover_only(frame).child(button)
        };

        let pin_btn = {
            let frame = div()
                .flex_none()
                .flex()
                .rounded(ui::RADIUS_XS)
                .when(pinned, |d| {
                    d.bg(if is_active_worktree {
                        p.worktree_sidebar
                    } else {
                        p.sidebar
                    })
                    .group_hover(ROW_GROUP, |d| {
                        d.bg(if is_active_worktree {
                            p.worktree_sidebar
                        } else {
                            p.sidebar_accent
                        })
                    })
                })
                .when(!pinned, hover_only);
            #[cfg(test)]
            let frame = frame.debug_selector(move || format!("worktree-pin-{index}"));
            frame.child(
                Button::new(("pin", index))
                    .ghost()
                    .xsmall()
                    .icon(if pinned {
                        IconName::StarFill
                    } else {
                        IconName::Star
                    })
                    .accessibility_label(text::PIN.get())
                    .toggled(pinned)
                    .tooltip(text::PIN.get())
                    .on_click(cx.listener(move |s, _, _, cx| {
                        cx.stop_propagation();
                        s.toggle_pin(pin.clone(), cx);
                    })),
            )
        };

        let rename_btn = {
            let frame = div().flex_none();
            #[cfg(test)]
            let frame = frame.debug_selector(move || format!("worktree-rename-{index}"));
            hover_only(frame).child(
                Button::new(("rename", index))
                    .ghost()
                    .xsmall()
                    .icon(Icon::default().data(ui::icons::PENCIL_SVG))
                    .accessibility_label(text::RENAME.get())
                    .tooltip(text::RENAME.get())
                    .on_click(cx.listener(move |s, _, window, cx| {
                        cx.stop_propagation();
                        s.rename(edit.clone(), window, cx);
                    })),
            )
        };

        let dirty_badge = dirty.map(|n| {
            let dirty_label = text::DIRTY_COUNT.format(&[("n", &n.to_string())]);
            div()
                .id(("dirty-badge", index))
                .aria_label(dirty_label.clone())
                .debug_selector(move || format!("dirty-badge-{index}"))
                .flex_none()
                .px_1()
                .rounded(ui::RADIUS_XS)
                .font_family(ui::FONT_MONO)
                .text_size(ui::TEXT_2XS)
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(p.warning)
                .child(format!("±{n}"))
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(dirty_label.clone()).build(window, cx)
                })
        });

        let props = self.options.card_properties;
        let show_branch_subtitle = props.branch
            && !branch_subtitle.is_empty()
            && !card_title
                .trim()
                .eq_ignore_ascii_case(branch_subtitle.trim());
        let pr_badge = props
            .pr
            .then(|| {
                self.prs
                    .get(path)
                    .and_then(crate::pull_request::Status::badge)
            })
            .flatten()
            .map(|badge| {
                div()
                    .id(("pr-badge", index))
                    .role(gpui::Role::Status)
                    .aria_label(badge)
                    .debug_selector(move || format!("pr-badge-{index}"))
                    .flex_none()
                    .px_1()
                    .rounded(ui::RADIUS_XS)
                    .bg(p.worktree_sidebar_accent)
                    .text_size(ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(badge)
            });

        let app_settings = crate::settings::current(cx);
        let worktree_cache_badge = app_settings
            .prompt_cache_timer_enabled
            .then(|| {
                status::worktree_most_urgent_cache_remaining_secs(
                    tree_sessions.iter().map(|s| {
                        (
                            s.status.agent.as_str(),
                            s.status.state,
                            s.status.since_unix_ns,
                        )
                    }),
                    self.now_unix_ns(),
                    app_settings.prompt_cache_ttl_secs,
                )
            })
            .flatten()
            .map(|secs| {
                let tip = text::CACHE_TIMER_TOOLTIP.get().to_owned();
                div()
                    .id(("worktree-cache-timer", index))
                    .aria_label(format!("{}: {}", tip, status::format_cache_countdown(secs)))
                    .debug_selector(move || format!("worktree-cache-timer-{index}"))
                    .flex_none()
                    .child(status::cache_timer_chip(secs, cx))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
            });

        let issue_text = props.issue.then(|| self.issue_for(path)).flatten();
        let has_issue_badge = issue_text.is_some();
        let issue_badge = issue_text.map(|iss| {
            div()
                .id(("issue-badge", index))
                .role(gpui::Role::Status)
                .aria_label(iss.clone())
                .debug_selector(move || format!("issue-badge-{index}"))
                .flex_none()
                .px_1()
                .rounded(ui::RADIUS_XS)
                .bg(p.worktree_sidebar_accent)
                .font_family(ui::FONT_MONO)
                .text_size(ui::TEXT_2XS)
                .text_color(p.muted_foreground)
                .child(iss)
        });
        let card_checkpoint_parts = row_host
            .is_local()
            .then(|| super::external::checkpoint_parts_from_store(&self.disk.store, path))
            .flatten();

        let ports_list = if props.ports {
            self.ports_for(&row_host, path)
        } else {
            Vec::new()
        };
        let ports_badge = (!ports_list.is_empty()).then(|| {
            let aria = ports_badge_text(&ports_list, usize::MAX);
            let text_str = ports_badge_text(&ports_list, PORTS_BADGE_MAX);
            div()
                .id(("ports-badge", index))
                .role(gpui::Role::Status)
                .aria_label(aria)
                .debug_selector(move || format!("ports-badge-{index}"))
                .flex_none()
                .px_1()
                .rounded(ui::RADIUS_XS)
                .bg(p.worktree_sidebar_accent)
                .font_family(ui::FONT_MONO)
                .text_size(ui::TEXT_2XS)
                .text_color(p.success)
                .child(text_str)
        });

        let host_badge = props
            .host
            .then(|| self.host_badge_for(&row_host, path))
            .flatten()
            .map(|hb| {
                div()
                    .id(("host-badge", index))
                    .role(gpui::Role::Status)
                    .aria_label(hb.clone())
                    .debug_selector(move || format!("host-badge-{index}"))
                    .flex_none()
                    .px_1()
                    .rounded(ui::RADIUS_XS)
                    .bg(p.worktree_sidebar_accent)
                    .text_size(ui::TEXT_2XS)
                    .text_color(p.muted_foreground)
                    .child(hb)
            });

        let visible_comment = props.comment.then_some(comment).flatten();
        let has_agent_pill = !summary.groups.is_empty() || tree_session_count > 0;
        let has_line3 = visible_comment.is_some() || has_agent_pill;

        let lineage_depth = if row_host.is_local() {
            self.lineage_depth(path)
        } else {
            0
        };
        let lineage_child_count = if row_host.is_local() {
            self.lineage_children(path).len()
        } else {
            0
        };
        let lineage_collapsed = row_host.is_local() && self.is_lineage_collapsed(path);
        let left_margin = px(self.worktree_margin(lineage_depth));
        let lineage_toggle_btn = (lineage_child_count > 0).then(|| {
            let fold_path = path.to_path_buf();
            let count_str = lineage_child_count.to_string();
            let tip = if lineage_collapsed {
                text::UNFOLD_CHILDREN.format(&[("n", &count_str)])
            } else {
                text::FOLD_CHILDREN.format(&[("n", &count_str)])
            };
            div()
                .id(("lineage-fold", index))
                .debug_selector(move || format!("worktree-lineage-toggle-{index}"))
                .aria_label(tip.clone())
                .flex_none()
                .flex()
                .items_center()
                .gap_0p5()
                .px_1()
                .py(px(1.))
                .rounded(ui::RADIUS_XS)
                .bg(p.worktree_sidebar_accent)
                .hover(|d| d.bg(p.accent).text_color(p.foreground))
                .active(|d| d.bg(p.sidebar_accent))
                .text_size(ui::TEXT_2XS)
                .font_family(ui::FONT_MONO)
                .text_color(p.muted_foreground)
                .cursor_pointer()
                .child(
                    Icon::new(if lineage_collapsed {
                        IconName::ChevronRight
                    } else {
                        IconName::ChevronDown
                    })
                    .with_size(ui::ICON_XS)
                    .text_color(p.muted_foreground),
                )
                .child(count_str)
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
                .on_click(cx.listener(move |s, _, _, cx| {
                    cx.stop_propagation();
                    s.toggle_lineage(&fold_path, cx);
                }))
        });

        let is_editing_this = self.editing.as_ref().is_some_and(|(p, _)| p == path);
        // Card headers and session rows have separate list entries at every size.
        if self.options.card_layout == super::CardLayout::Compact {
            let compact_path = card_path.clone();
            let compact_fold = fold.clone();
            let compact_host = row_host.clone();
            return base
                .debug_selector(move || format!("worktree-card-{index}"))
                .aria_label(label)
                .aria_expanded(!closed)
                .min_h(px(28.))
                .mr_1p5()
                .ml(left_margin)
                .my_0p5()
                .px_2p5()
                .rounded(ui::RADIUS_MD)
                .border_1()
                .when(is_active_worktree, |d| {
                    d.bg(p.worktree_sidebar)
                        .border_color(p.sidebar_ring.opacity(0.65))
                })
                .when(!is_active_worktree, |d| {
                    let d = d
                        .border_color(ui::TRANSPARENT)
                        .hover(|d| d.bg(p.sidebar_accent.opacity(0.45)));
                    if lineage_depth > 0 {
                        d.border_l_2().border_color(p.worktree_sidebar_border)
                    } else {
                        d
                    }
                })
                .text_color(p.worktree_sidebar_foreground)
                .cursor_pointer()
                .flex()
                .flex_col()
                .child(
                    self.with_row_menu(
                        index,
                        &Row::Worktree(path.to_path_buf()),
                        div()
                            .id(("worktree-header", index))
                            .relative()
                            .w_full()
                            .h(px(26.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .debug_selector(move || format!("worktree-state-{index}"))
                                    .w(px(14.))
                                    .h(px(14.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(left_status),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(self.card_title_el(
                                        path,
                                        card_title,
                                        ui::TEXT_SM,
                                        index,
                                        cx,
                                    ))
                                    .when(!is_editing_this, |d| {
                                        d.children(lineage_toggle_btn)
                                            .when(show_branch_subtitle, |d| {
                                                d.child(
                                                    div()
                                                        .debug_selector(move || {
                                                            format!("worktree-branch-{index}")
                                                        })
                                                        .min_w_0()
                                                        .truncate()
                                                        .text_size(ui::TEXT_2XS)
                                                        .text_color(p.muted_foreground)
                                                        .child(branch_subtitle),
                                                )
                                            })
                                            .when_some(visible_comment, |d, cm| {
                                                d.child(
                                                    div()
                                                        .debug_selector(move || {
                                                            format!("worktree-comment-{index}")
                                                        })
                                                        .min_w_0()
                                                        .truncate()
                                                        .text_size(ui::TEXT_2XS)
                                                        .text_color(p.muted_foreground)
                                                        .child(cm.to_owned()),
                                                )
                                            })
                                    }),
                            )
                            .when(!is_editing_this, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .children(host_badge)
                                        .children(issue_badge)
                                        .children(ports_badge)
                                        .children(dirty_badge)
                                        .children(pr_badge)
                                        .when(has_agent_pill, |d| {
                                            d.child(
                                                div()
                                                    .id(("fold", index))
                                                    .cursor_pointer()
                                                    .child(
                                                        Icon::new(if card_expanded {
                                                            IconName::ChevronDown
                                                        } else {
                                                            IconName::ChevronRight
                                                        })
                                                        .with_size(ui::ICON_XS)
                                                        .text_color(p.muted_foreground),
                                                    )
                                                    .on_click(cx.listener(move |s, _, _, cx| {
                                                        cx.stop_propagation();
                                                        s.toggle_card_sessions(&compact_fold, cx);
                                                    })),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .gap_0p5()
                                        .child(new_session_btn)
                                        .child(rename_btn)
                                        .child(pin_btn),
                                )
                            }),
                        cx,
                    ),
                )
                .on_click(cx.listener(move |s, _, window, cx| {
                    s.activate_worktree_card(
                        index,
                        compact_host.clone(),
                        compact_path.clone(),
                        Some(window),
                        cx,
                    );
                }));
        }

        let checkpoint_visible = card_checkpoint_parts
            .as_ref()
            .is_some_and(|parts| parts.label(has_issue_badge).is_some() || parts.unread);
        let badge_count = usize::from(checkpoint_visible)
            + usize::from(worktree_cache_badge.is_some())
            + usize::from(host_badge.is_some())
            + usize::from(issue_badge.is_some())
            + usize::from(ports_badge.is_some())
            + usize::from(dirty_badge.is_some())
            + usize::from(pr_badge.is_some());
        let has_badges = badge_count > 0;
        let has_line2 = show_branch_subtitle || has_badges;
        let dense_badges = badge_count >= 2 || checkpoint_visible;
        let checkpoint_pill = card_checkpoint_parts.as_ref().map(|parts| {
            let label = parts.label(has_issue_badge);
            let a11y = label.clone().unwrap_or_default();
            div()
                .id(("checkpoint-badge", index))
                .role(gpui::Role::Status)
                .aria_label(a11y)
                .debug_selector(move || format!("checkpoint-badge-{index}"))
                .child(super::external::checkpoint_badge_element(parts, p))
        });

        let header = div()
            .id(("worktree-header", index))
            .w_full()
            .flex()
            .flex_col()
            .gap_0p5()
            // 1행: 상태 아이콘/점 + 워크트리 표시 이름 + [기본] 배지 + 자식 접기/펼치기 버튼 + 호버 액션 버튼 오버레이
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .debug_selector(move || format!("worktree-state-{index}"))
                            .w(px(14.))
                            .h(px(14.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(left_status),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(self.card_title_el(path, card_title, ui::TEXT_MD, index, cx))
                            .when(is_default_tree && !is_editing_this, |d| {
                                d.child(
                                    div()
                                        .debug_selector(move || {
                                            format!("worktree-default-badge-{index}")
                                        })
                                        .flex_none()
                                        .px_1p5()
                                        .py(px(1.))
                                        .rounded(ui::RADIUS_XS)
                                        .border_1()
                                        .border_color(p.worktree_sidebar_border)
                                        .text_size(ui::TEXT_2XS)
                                        .text_color(p.muted_foreground)
                                        .child(crate::i18n::catalog::general::DEFAULT.get()),
                                )
                            })
                            .when(!is_editing_this, |d| d.children(lineage_toggle_btn)),
                    )
                    .when(!is_editing_this, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_0p5()
                                .child(new_session_btn)
                                .child(rename_btn)
                                .child(pin_btn),
                        )
                    }),
            )
            // 2행: 들여쓴 브랜치 이름 + 체크포인트·캐시 타이머·호스트·이슈·포트·미커밋·PR 배지
            .when(has_line2, |d| {
                if show_branch_subtitle && dense_badges {
                    d.child(
                        div()
                            .w_full()
                            .pl(px(20.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .debug_selector(move || format!("worktree-branch-{index}"))
                                    .w_full()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(branch_subtitle),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_1()
                                    .children(checkpoint_pill)
                                    .children(worktree_cache_badge)
                                    .children(host_badge)
                                    .children(issue_badge)
                                    .children(ports_badge)
                                    .children(dirty_badge)
                                    .children(pr_badge),
                            ),
                    )
                } else {
                    d.child(
                        div()
                            .w_full()
                            .min_h(px(18.))
                            .pl(px(20.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_1p5()
                            .when(show_branch_subtitle, |d| {
                                d.child(
                                    div()
                                        .debug_selector(move || format!("worktree-branch-{index}"))
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(branch_subtitle),
                                )
                            })
                            .when(has_badges, |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .items_center()
                                        .gap_1()
                                        .children(checkpoint_pill)
                                        .children(worktree_cache_badge)
                                        .children(host_badge)
                                        .children(issue_badge)
                                        .children(ports_badge)
                                        .children(dirty_badge)
                                        .children(pr_badge),
                                )
                            }),
                    )
                }
            })
            // 3행: 코멘트 또는 에이전트 상태 요약(1개 세션이면 인라인 요약 줄, 다중 세션이면 알약 바)
            .when(has_line3, |d| {
                let single_session = if tree_session_count == 1 && !card_expanded {
                    tree_sessions.first().copied()
                } else {
                    None
                };
                d.child(
                    div()
                        .w_full()
                        .pl(px(20.))
                        .pt_0p5()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(
                            super::external::render_comment(visible_comment, p.muted_foreground)
                                .map(|el| {
                                    div()
                                        .debug_selector(move || format!("worktree-comment-{index}"))
                                        .child(el)
                                }),
                        )
                        .when(has_agent_pill, |d| {
                            if let Some(sess) = single_session {
                                let (sess_name, _) = self.session_label(sess);
                                let sess_unread = self.is_unread(sess.id);
                                let sess_id = sess.id;
                                d.child(
                                    div()
                                        .id(("fold", index))
                                        .debug_selector(move || format!("card-session-{index}"))
                                        .w_full()
                                        .h(px(20.))
                                        .px_1()
                                        .rounded(ui::RADIUS_XS)
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .cursor_pointer()
                                        .hover(|d| d.bg(p.worktree_sidebar_accent))
                                        .child(status::status_glyph_slot(
                                            sess.status.state,
                                            sess_unread,
                                            Size::Sm,
                                            self.spinner_slots(index),
                                            cx,
                                        ))
                                        .child(status::agent_icon(
                                            status::infer_agent_from_status(&sess.status, &sess_name),
                                            ui::ICON_SM,
                                            p.muted_foreground,
                                        ))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(ui::TEXT_XS)
                                                .text_color(p.muted_foreground)
                                                .child(sess_name),
                                        )
                                        .on_click(cx.listener(move |s, _, _, cx| {
                                            cx.stop_propagation();
                                            // OWE-854: 1개 세션 줄은 펼치기 토글이 아니라 해당 세션 선택이다.
                                            let idx = s
                                                .rows
                                                .iter()
                                                .position(|r| {
                                                    matches!(r, Row::Session(item, _) if item.id == sess_id)
                                                })
                                                .unwrap_or(index);
                                            s.select_session_on(
                                                idx,
                                                s.row_host(idx).clone(),
                                                sess_id,
                                                cx,
                                            );
                                        })),
                                )
                            } else {
                                d.child(
                                    div()
                                        .id(("fold", index))
                                        .w_full()
                                        .h(px(25.))
                                        .px_1p5()
                                        .rounded(ui::RADIUS_XS)
                                        .border_1()
                                        .border_color(p.worktree_sidebar_border)
                                        .bg(if is_active_worktree {
                                            p.sidebar.opacity(0.75)
                                        } else {
                                            p.sidebar_accent.opacity(0.65)
                                        })
                                        .hover(|d| d.bg(p.sidebar_accent))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .cursor_pointer()
                                        .child(if summary.groups.is_empty() {
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_1()
                                                .px_1p5()
                                                .py_0p5()
                                                .rounded(ui::RADIUS_XS)
                                                .bg(p.worktree_sidebar_accent.opacity(0.65))
                                                .child(status::status_glyph(
                                                    AgentState::Unknown,
                                                    unread,
                                                    Size::Sm,
                                                    cx,
                                                ))
                                                .child(status::agent_icon(
                                                    "",
                                                    ui::ICON_SM,
                                                    p.muted_foreground,
                                                ))
                                                .when(tree_session_count > 1, |d| {
                                                    d.child(
                                                        div()
                                                            .font_family(ui::FONT_MONO)
                                                            .text_size(ui::TEXT_2XS)
                                                            .text_color(p.muted_foreground)
                                                            .child(format!(
                                                                "+{}",
                                                                tree_session_count - 1
                                                            )),
                                                    )
                                                })
                                                .into_any_element()
                                        } else {
                                            status::summary_element(
                                                &summary,
                                                self.spinner_slots(index),
                                                cx,
                                            )
                                            .into_any_element()
                                        })
                                        .child(
                                            Icon::new(if card_expanded {
                                                IconName::ChevronDown
                                            } else {
                                                IconName::ChevronRight
                                            })
                                            .with_size(ui::ICON_XS)
                                            .text_color(p.muted_foreground),
                                        )
                                        .on_click(cx.listener(move |s, _, _, cx| {
                                            cx.stop_propagation();
                                            s.toggle_card_sessions(&fold, cx);
                                        })),
                                )
                            }
                        }),
                )
            });

        base.debug_selector(move || format!("worktree-card-{index}"))
            .aria_label(label)
            .aria_expanded(!closed)
            .mr_1p5()
            .ml(left_margin)
            .my_0p5()
            .px_2p5()
            .py_1p5()
            .rounded(ui::RADIUS_MD)
            .border_1()
            .when(is_active_worktree, |d| {
                d.bg(p.worktree_sidebar)
                    .border_color(p.sidebar_ring.opacity(0.65))
            })
            .when(!is_active_worktree, |d| {
                let d = d
                    .border_color(ui::TRANSPARENT)
                    .hover(|d| d.bg(p.sidebar_accent.opacity(0.45)));
                if lineage_depth > 0 {
                    d.border_l_2().border_color(p.worktree_sidebar_border)
                } else {
                    d
                }
            })
            .text_color(p.worktree_sidebar_foreground)
            .cursor_pointer()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(self.with_row_menu(index, &Row::Worktree(path.to_path_buf()), header, cx))
            .on_click(cx.listener(move |s, _, window, cx| {
                s.activate_worktree_card(
                    index,
                    row_host.clone(),
                    card_path.clone(),
                    Some(window),
                    cx,
                );
            }))
    }

    /// 행 스피너는 자리만 적는다(셸이 덮어 그림). 흐린 행은 덮어 그리는 스피너가
    /// 행 투명도를 받지 못하므로 제자리 애니메이션(`None`)을 쓴다.
    fn spinner_slots(&self, index: usize) -> Option<&status::SpinnerSlots> {
        (!self.is_row_dimmed(index)).then_some(&self.spinner_slots)
    }

    /// 셸이 사이드바 바로 위에 두는 스피너 층.
    pub fn spinner_overlay(&self) -> impl IntoElement {
        status::spinner_overlay(&self.spinner_slots)
    }

    /// 행 + 우클릭 메뉴(세션 행은 열기·이어하기·종료 + 여기서 새 터미널·에이전트·Open in, 워크트리·폴더는 핀·이름 변경·계보 작업).
    /// 저장소·폴더 헤더는 접기 대상 경로를 `header`로 넘겨 '프로젝트 제거' 메뉴를 붙인다(OWE-845).
    fn render_row_with_menu(&self, index: usize, row: &Row, cx: &mut Context<Self>) -> AnyElement {
        let element = self.render_row(index, row, cx);
        if matches!(row, Row::Worktree(_)) {
            return element.into_any_element();
        }
        self.with_row_menu(index, row, element, cx)
    }

    /// 워크트리 메뉴는 헤더에만 붙여 하위 세션 메뉴와 겹치지 않게 한다.
    fn with_row_menu(
        &self,
        index: usize,
        row: &Row,
        element: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let host = self.row_host(index).clone();
        let row = row.clone();
        let this = cx.weak_entity();
        element
            .context_menu(move |menu, window, cx| {
                // Build menu-only lineage and labels when the menu opens, not
                // once for every visible row on every scrolling frame.
                let Ok((folder, session, tree, header, agents, pr)) = this.read_with(cx, |s, _| {
                    let (folder, session, tree, header) = match &row {
                        Row::Worktree(path) => {
                            let pinned = s.prefs.get(path).is_some_and(|p| p.pinned);
                            let is_primary =
                                super::options::is_primary_worktree(s.snapshot_for(&host), path);
                            let current_parent =
                                host.is_local().then(|| s.worktree_parent(path)).flatten();
                            let parent_candidates = if host.is_local() {
                                s.eligible_parent_worktrees(path)
                                    .into_iter()
                                    .map(|cand| {
                                        let label = s.name(&cand);
                                        (cand, label)
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            };
                            let descendant_count = if host.is_local() {
                                s.lineage_descendants(path).len()
                            } else {
                                0
                            };
                            let sleeping = host.is_local() && s.is_worktree_sleeping(path);
                            (
                                Some(path.clone()),
                                None,
                                Some(TreeMenuMeta {
                                    path: path.clone(),
                                    pinned,
                                    is_primary,
                                    current_parent,
                                    parent_candidates,
                                    descendant_count,
                                    sleeping,
                                }),
                                None,
                            )
                        }
                        Row::Session(s, _) => (
                            s.worktree.clone().or_else(|| s.cwd.clone()),
                            Some(SessionMenuMeta { index, id: s.id }),
                            None,
                            None,
                        ),
                        Row::Repository(path) | Row::Folder(path) => (
                            Some(path.clone()),
                            None,
                            None,
                            Some(HeaderMenuMeta {
                                path: path.clone(),
                                repo: matches!(row, Row::Repository(_)) && host.is_local(),
                            }),
                        ),
                        _ => (None, None, None, None),
                    };
                    let agents = s.agents.clone();
                    let pr = tree
                        .as_ref()
                        .and_then(|meta| s.prs.get(&meta.path))
                        .cloned();
                    (folder, session, tree, header, agents, pr)
                }) else {
                    return menu;
                };
                row_menu(
                    menu,
                    &this,
                    &host,
                    folder.as_ref(),
                    session.as_ref(),
                    tree.as_ref(),
                    header.as_ref(),
                    &agents,
                    pr.as_ref(),
                    window,
                    cx,
                )
            })
            .into_any_element()
    }

    /// 가상 리스트(보이는 행만 만든다). 스크린 리더에는 이름 붙은 목록 하나로 보인다.
    pub(super) fn render_list(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let editing = self
            .editing
            .as_ref()
            .map(|(path, input)| (path.as_path(), input.read(cx).focus_handle(cx)));
        self.card_list.focus_editor(editing);
        let content = gpui::list(
            self.card_list.state.clone(),
            cx.processor(|s, item: usize, _, cx| {
                let Some(index) = s.card_list.row_index(item) else {
                    return div().into_any_element();
                };
                let Some(row) = s.rows.get(index).cloned() else {
                    return div().into_any_element();
                };
                #[cfg(test)]
                {
                    s.rendered_rows += 1;
                }
                let joins_next = match &row {
                    Row::Worktree(_) => {
                        s.session_card_index(index + 1) == Some(index)
                            && s.card_list.row_index(item + 1) == Some(index + 1)
                    }
                    Row::Session(_, true) => {
                        s.session_card_index(index).is_some()
                            && s.session_card_index(index + 1) == s.session_card_index(index)
                    }
                    _ => false,
                };
                div()
                    .w_full()
                    .when(!joins_next, gpui::Styled::pb_0p5)
                    .child(s.render_row_with_menu(index, &row, cx))
                    .into_any_element()
            }),
        )
        .size_full();
        div()
            .id("worktree-list-a11y")
            .debug_selector(|| "sidebar-viewport".to_owned())
            .role(gpui::Role::List)
            .aria_label(text::PROJECTS.get())
            .flex_1()
            .min_h_0()
            .py_1()
            .child(content)
    }

    /// OWE-847: 이름 변경은 카드 제목 자리 인라인 편집이 기본이다.
    /// 편집 중인 카드가 목록에 보이지 않을 때(필터·접힘 등)만 상단 대체 입력창을 띄운다.
    fn render_rename(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let p = ui::palette(cx);
        let (editing_path, input) = self.editing.as_ref()?;
        if self
            .rows
            .iter()
            .any(|row| matches!(row, Row::Worktree(w) if w == editing_path))
        {
            return None;
        }
        let input = input.clone();
        Some(
            div()
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .border_b_1()
                .border_color(p.sidebar_border)
                .child(Input::new(&input).small())
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            ui::primary("rename-save", text::SAVE.get()).on_click(cx.listener(
                                |s, _, window, cx| {
                                    s.finish_rename(Some(window), cx);
                                },
                            )),
                        )
                        .child(
                            Button::new("rename-cancel")
                                .ghost()
                                .label(text::CANCEL.get())
                                .on_click(cx.listener(|s, _, window, cx| {
                                    s.cancel_rename(Some(window), cx);
                                })),
                        ),
                )
                .capture_key_down(cx.listener(|s, e: &gpui::KeyDownEvent, window, cx| {
                    if e.keystroke.key == "escape" {
                        cx.stop_propagation();
                        s.cancel_rename(Some(window), cx);
                    }
                })),
        )
    }

    /// OWE-847: 워크트리 삭제 확인 모달(Orca `DeleteWorktreeDialog`처럼 이름·자손 수·미커밋 경고를
    /// 보여 주고, 미커밋 변경이 있으면 강제 삭제 토글을 제공한다).
    fn render_delete_confirm(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let confirm = self.delete_confirm.clone()?;
        let p = ui::palette(cx);
        let name = self.name(&confirm.path);
        let desc = if confirm.with_descendants {
            text::DELETE_DESC_WITH_DESCENDANTS.format(&[
                ("name", &name),
                ("n", &confirm.descendant_count.to_string()),
            ])
        } else {
            text::DELETE_DESC.format(&[("name", &name)])
        };
        let card = div()
            .id("delete-worktree-dialog")
            .debug_selector(|| "delete-worktree-dialog".into())
            .occlude()
            .role(gpui::Role::AlertDialog)
            .w(ui::DIALOG_WIDTH)
            .max_w(window.viewport_size().width - ui::SPACE_4 * 2.)
            .p_6()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .capture_key_down(cx.listener(|s, e: &gpui::KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    s.cancel_delete(cx);
                }
            }))
            .on_mouse_down_out(cx.listener(|s, _, _, cx| s.cancel_delete(cx)))
            .child(
                div()
                    .debug_selector(|| "delete-worktree-title".into())
                    .text_size(ui::TEXT_LG)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(text::DELETE_TITLE.get()),
            )
            .child(
                div()
                    .debug_selector(|| "delete-worktree-desc".into())
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(desc),
            )
            .when(confirm.dirty > 0, |d| {
                d.child(
                    div()
                        .debug_selector(|| "delete-worktree-dirty".into())
                        .text_size(ui::TEXT_XS)
                        .text_color(p.warning)
                        .child(text::DELETE_DIRTY.format(&[("n", &confirm.dirty.to_string())])),
                )
                .child(
                    div()
                        .debug_selector(|| "delete-worktree-force".into())
                        .child(
                            Checkbox::new("delete-worktree-force")
                                .label(text::DELETE_FORCE.get())
                                .checked(confirm.force)
                                .on_click(cx.listener(|s, _checked: &bool, _, cx| {
                                    s.toggle_delete_force(cx);
                                })),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("delete-worktree-cancel")
                            .ghost()
                            .debug_selector(|| "delete-worktree-cancel".into())
                            .label(text::CANCEL.get())
                            .on_click(cx.listener(|s, _, _, cx| s.cancel_delete(cx))),
                    )
                    .child(
                        Button::new("delete-worktree-confirm")
                            .danger()
                            .debug_selector(|| "delete-worktree-confirm".into())
                            .label(text::DELETE_CONFIRM.get())
                            .on_click(cx.listener(|s, _, _, cx| s.confirm_delete(cx))),
                    ),
            );
        Some(ui::modal_backdrop(window.viewport_size(), card).into_any_element())
    }

    /// MK-174: Setup 스크립트 감지 인앱 프롬프트 카드(`SetupScriptPromptCard`).
    /// 참고: `.ref/orca/src/renderer/src/components/sidebar/SetupScriptPromptCard.tsx`
    /// (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).
    fn render_setup_prompt_card(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let state_dir = self.setup_prompt_dir(cx)?;
        let summary = self.setup_prompt.summary.as_ref()?;
        let script = summary.suggested_setup.clone()?;
        let source = summary
            .suggested_source
            .clone()
            .unwrap_or_else(|| "orca.yaml".to_owned());
        let repo_key = summary.repo_key.clone();
        let p = ui::palette(cx);
        let wt_text = crate::i18n::catalog::worktree::PROMPT_TITLE.format(&[("source", &source)]);
        let (enable_dir, enable_key, enable_script) =
            (state_dir.clone(), repo_key.clone(), script.clone());
        let (dismiss_dir, dismiss_key) = (state_dir, repo_key);
        Some(
            div()
                .id("setup-script-prompt-card")
                .debug_selector(|| "setup-script-prompt-card".into())
                .w_full()
                .min_w_0()
                .mx_2()
                .my_1()
                .p_2p5()
                .rounded(ui::RADIUS_MD)
                .border_1()
                .border_color(p.border)
                .bg(p.card)
                .text_color(p.card_foreground)
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(p.sidebar_foreground)
                        .child(wt_text),
                )
                .child(
                    div()
                        .text_size(ui::TEXT_2XS)
                        .text_color(p.muted_foreground)
                        .child(crate::i18n::catalog::worktree::PROMPT_DESC.get()),
                )
                .child(
                    div()
                        .id("setup-script-prompt-preview")
                        .debug_selector(|| "setup-script-prompt-preview".into())
                        .px_2()
                        .py_1()
                        .rounded(ui::RADIUS_XS)
                        .bg(p.muted)
                        .font_family(ui::FONT_MONO)
                        .text_size(ui::TEXT_2XS)
                        .truncate()
                        .child(script),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1p5()
                        .child(
                            Button::new("setup-prompt-enable")
                                .primary()
                                .xsmall()
                                .label(crate::i18n::catalog::worktree::PROMPT_ENABLE.get())
                                .debug_selector(|| "setup-prompt-enable".into())
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.save_setup_prompt(
                                        enable_dir.clone(),
                                        enable_key.clone(),
                                        Some(enable_script.clone()),
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            Button::new("setup-prompt-settings")
                                .ghost()
                                .xsmall()
                                .label(crate::i18n::catalog::worktree::PROMPT_SETTINGS.get())
                                .debug_selector(|| "setup-prompt-settings".into())
                                .on_click(cx.listener(|_, _, _, cx| {
                                    crate::settings::open_window_at_tab(
                                        crate::settings::AdminTab::Workflows,
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            Button::new("setup-prompt-dismiss")
                                .ghost()
                                .xsmall()
                                .label(crate::i18n::catalog::worktree::PROMPT_DISMISS.get())
                                .debug_selector(|| "setup-prompt-dismiss".into())
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.save_setup_prompt(
                                        dismiss_dir.clone(),
                                        dismiss_key.clone(),
                                        None,
                                        cx,
                                    );
                                })),
                        ),
                ),
        )
    }
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_external_polling(cx);
        self.ensure_setup_prompt_polling(cx);
        if self.orca_import_subscription.is_none() {
            self.orca_import_subscription = Some(
                cx.observe_global::<crate::orca_import::OrcaImportState>(|this, cx| {
                    if crate::orca_import::state(cx)
                        .is_some_and(|st| !st.importing && st.last_result.is_some())
                    {
                        this.reload_external(cx);
                    }
                }),
            );
        }
        #[cfg(test)]
        {
            self.renders += 1;
        }
        self.spinner_slots.clear();
        let p = ui::palette(cx);
        let list = self.render_list(cx);
        let setup_prompt = self.render_setup_prompt_card(cx);
        let rename = self.render_rename(cx);
        let delete_confirm = self.render_delete_confirm(window, cx);
        let default_repo = self.snapshot.trees.keys().next().cloned();
        div()
            .id("sidebar")
            .when(
                self.editing.is_none() && self.creating.is_none() && self.delete_confirm.is_none(),
                |d| d.key_context(KEY_CONTEXT),
            )
            .when_some(self.focus.as_ref(), InteractiveElement::track_focus)
            .on_action(cx.listener(|s, _: &SelectPrev, _, cx| s.step(-1, cx)))
            .on_action(cx.listener(|s, _: &SelectNext, _, cx| s.step(1, cx)))
            .on_action(cx.listener(|s, _: &Activate, window, cx| s.activate(window, cx)))
            .on_action(cx.listener(|s, _: &Collapse, _, cx| s.fold_cursor(false, cx)))
            .on_action(cx.listener(|s, _: &Expand, _, cx| s.fold_cursor(true, cx)))
            .on_action(cx.listener(|s, _: &Dismiss, window, cx| s.leave(window, cx)))
            .on_action(cx.listener(|s, _: &TogglePin, _, cx| s.toggle_pin_cursor(cx)))
            .on_action(cx.listener(|s, _: &Rename, window, cx| s.rename_cursor(window, cx)))
            .on_action(cx.listener(|s, _: &OpenInFinder, _, cx| {
                s.open_in_cursor(Target::Finder, cx);
            }))
            .on_action(cx.listener(|s, _: &OpenInVsCode, _, cx| {
                s.open_in_cursor(Target::VsCode, cx);
            }))
            .on_action(cx.listener(|s, _: &OpenInCursor, _, cx| {
                s.open_in_cursor(Target::Cursor, cx);
            }))
            .on_action(cx.listener(|s, _: &OpenInZed, _, cx| {
                s.open_in_cursor(Target::Zed, cx);
            }))
            .on_action(cx.listener(|s, _: &NewAgent1, window, cx| {
                s.new_agent_cursor(0, window, cx);
            }))
            .on_action(cx.listener(|s, _: &NewAgent2, window, cx| {
                s.new_agent_cursor(1, window, cx);
            }))
            .on_action(cx.listener(|s, _: &NewAgent3, window, cx| {
                s.new_agent_cursor(2, window, cx);
            }))
            .on_action(cx.listener(|s, _: &NewAgent4, window, cx| {
                s.new_agent_cursor(3, window, cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .text_color(p.sidebar_foreground)
            .child(
                div()
                    .id("sidebar-header")
                    .debug_selector(|| "sidebar-header".into())
                    .h(ui::SIDEBAR_HEADER_HEIGHT)
                    .pl_3()
                    .pr_1p5()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_size(ui::TEXT_XS)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.muted_foreground)
                            .child(text::PROJECTS.get()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_color(p.muted_foreground)
                            .child(
                                Button::new("sidebar-add-folder")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::default().data(ui::icons::FOLDER_PLUS_SVG))
                                    .accessibility_label(
                                        crate::i18n::catalog::shell::OPEN_FOLDER.get(),
                                    )
                                    .tooltip(crate::i18n::catalog::shell::OPEN_FOLDER.get())
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(super::OpenFolder)),
                                    ),
                            )
                            .when_some(default_repo, |d, repo| {
                                d.child(
                                    Button::new("sidebar-new-worktree")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Plus)
                                        .accessibility_label(text::NEW_WORKTREE.get())
                                        .tooltip(text::NEW_WORKTREE.get())
                                        .on_click(cx.listener(move |s, _, window, cx| {
                                            s.new_worktree(repo.clone(), window, cx);
                                        })),
                                )
                            })
                            .child(self.render_options_button(cx))
                            .child(
                                Button::new("sidebar-refresh")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::RefreshCw)
                                    .accessibility_label(text::REFRESH.get())
                                    .tooltip(text::REFRESH.get())
                                    .on_click(cx.listener(|s, _, _, cx| s.refresh(cx))),
                            ),
                    ),
            )
            .children(self.error.clone().map(|e| {
                ui::error_banner(p, crate::i18n::cached_notice(&e))
                    .id("sidebar-preference-error")
                    .debug_selector(|| "sidebar-preference-error".into())
                    .mx_2()
                    .my_0p5()
                    .cursor_pointer()
                    .hover(|d| d.bg(p.destructive.opacity(0.15)))
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.error = None;
                        cx.notify();
                    }))
            }))
            .children(self.poll_error.clone().map(|e| {
                div()
                    .mx_2()
                    .my_0p5()
                    .px_3()
                    .py_2()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(p.warning.opacity(0.3))
                    .bg(p.warning.opacity(0.1))
                    .text_size(ui::TEXT_XS)
                    .text_color(p.warning)
                    .child(crate::i18n::cached_notice(&e))
            }))
            .children(
                (!self.pr_guidance_dismissed)
                    .then(|| {
                        self.prs
                            .values()
                            .find_map(crate::pull_request::Status::guidance)
                    })
                    .flatten()
                    .map(|hint| {
                        div()
                            .id("sidebar-pr-guidance")
                            .debug_selector(|| "sidebar-pr-guidance".into())
                            .px_3()
                            .py_1()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_1p5()
                            .cursor_pointer()
                            .hover(|d| d.bg(p.sidebar_accent).text_color(p.foreground))
                            .active(|d| d.bg(p.accent))
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.pr_guidance_dismissed = true;
                                cx.notify();
                            }))
                            .child(div().flex_1().min_w_0().child(hint))
                            .child(
                                ui::button_with_tooltip(
                                    Button::new("sidebar-pr-guidance-dismiss")
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::Close),
                                    "sidebar-pr-guidance-dismiss",
                                    text::CANCEL.get(),
                                )
                                .debug_selector(|| "sidebar-pr-guidance-dismiss".into())
                                .on_click(cx.listener(
                                    |s, _, _, cx| {
                                        cx.stop_propagation();
                                        s.pr_guidance_dismissed = true;
                                        cx.notify();
                                    },
                                )),
                            )
                    }),
            )
            .children(self.notice.clone().map(|e| {
                div()
                    .id("sidebar-notice")
                    .mx_2()
                    .my_0p5()
                    .px_3()
                    .py_2()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(p.warning.opacity(0.3))
                    .bg(p.warning.opacity(0.1))
                    .cursor_pointer()
                    .hover(|d| d.bg(p.warning.opacity(0.15)))
                    .text_size(ui::TEXT_XS)
                    .text_color(p.warning)
                    .child(crate::i18n::cached_notice(&e))
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.notice = None;
                        cx.notify();
                    }))
            }))
            .children(setup_prompt)
            .children(rename)
            .children(self.creating.as_ref().map(|(dialog, _)| dialog.clone()))
            .children(delete_confirm)
            .when(self.rows.is_empty(), |d| {
                d.child(
                    div()
                        .id("sidebar-empty")
                        .debug_selector(|| "sidebar-empty".into())
                        // Orca 빈 상태(`px-4 py-6 text-center text-[11px]`): 가운데 정렬.
                        .px_4()
                        .py(ui::SIDEBAR_EMPTY_PY)
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .text_center()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(p.sidebar_foreground)
                                .child(text::EMPTY.get()),
                        )
                        .child(text::EMPTY_HINT.get())
                        .child(
                            div()
                                .mt_1()
                                .debug_selector(|| "sidebar-open-folder".into())
                                .child(
                                    Button::new("sidebar-open-folder")
                                        .outline()
                                        .small()
                                        .icon(IconName::FolderOpen)
                                        .label(crate::i18n::catalog::shell::OPEN_FOLDER.get())
                                        .on_click(
                                            cx.listener(|_, _, _, cx| cx.emit(super::OpenFolder)),
                                        ),
                                ),
                        ),
                )
            })
            .child(list)
    }
}

/// 카드 포트 배지에 보이는 최대 포트 수. 나머지는 `+N`으로 접는다.
const PORTS_BADGE_MAX: usize = 3;

/// 포트 목록을 `:3000 :8080 +2` 꼴로 만든다. 처음 `max`개만 보이고 나머지는 개수로 접는다.
pub(super) fn ports_badge_text(ports: &[u16], max: usize) -> String {
    let mut text = ports
        .iter()
        .take(max)
        .map(|port| format!(":{port}"))
        .collect::<Vec<_>>()
        .join(" ");
    let rest = ports.len().saturating_sub(max);
    if rest > 0 {
        text.push_str(" +");
        text.push_str(&rest.to_string());
    }
    text
}

/// 원격 연결 실패 메시지(ssh 표준 오류·터널 오류)를 사이드바에 보일 짧은 이유로 줄인다.
pub(super) fn disconnect_reason(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    match crate::ssh_hosts::classify_ssh_error(&lower) {
        crate::ssh_hosts::SshTestFailureKind::AuthFailed => text::HOST_REASON_AUTH.get(),
        crate::ssh_hosts::SshTestFailureKind::ConnectionRefused => text::HOST_REASON_REFUSED.get(),
        crate::ssh_hosts::SshTestFailureKind::Other => {
            if [
                "timed out",
                "no route to host",
                "host is down",
                "network is unreachable",
                "could not resolve",
            ]
            .iter()
            .any(|m| lower.contains(m))
            {
                text::HOST_REASON_OFFLINE.get()
            } else if [
                "krill is not installed",
                "failed to start krill",
                "unexpected eof",
                "early eof",
                "connection reset",
                "broken pipe",
            ]
            .iter()
            .any(|m| lower.contains(m))
            {
                text::HOST_REASON_NO_KRILL.get()
            } else {
                text::HOST_DISCONNECTED.get()
            }
        }
    }
}

/// 끌고 있는 프로젝트 머리줄(놓을 자리 표시와 드래그 미리보기).
#[derive(Clone)]
struct ProjectDrag {
    path: PathBuf,
    name: gpui::SharedString,
}

impl Render for ProjectDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        div()
            .px(ui::SPACE_3)
            .py(ui::SPACE_2)
            .rounded(ui::RADIUS_SM)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_SM)
            .flex()
            .items_center()
            .gap(ui::SPACE_1)
            .child(Icon::new(IconName::Folder).xsmall())
            .child(self.name.clone())
    }
}
