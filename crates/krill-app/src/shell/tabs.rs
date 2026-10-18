//! OWE-577: 탭 크롬·닫기·재정렬·에이전트 런처. gpui 의존은 셸 경계 안에 둔다.

use gpui_component::Icon;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::tab::{Tab, TabBar};
use krill_app::ui;
use krill_proto::control::AgentState;

use super::*;

const CLOSE_GROUP: &str = "session-tab";

#[derive(Clone)]
struct TabDrag {
    pane: PaneId,
    title: SharedString,
}

impl Render for TabDrag {
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
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_SM)
            .child(self.title.clone())
    }
}

fn agent_icon(agent: &str) -> IconName {
    match agent {
        "claude" => IconName::Asterisk,
        "codex" => IconName::Cpu,
        // OWE-605: 핀(Star/StarFill)과 겹치지 않는 기본 번들 아이콘. 초승달은 작은 탭에서도 별과 구분된다.
        "agy" | "antigravity" => IconName::Moon,
        "opencode" | "" => IconName::SquareTerminal,
        _ => IconName::Bot,
    }
}

fn state_label(state: AgentState) -> &'static str {
    match state {
        AgentState::Working => krill_app::i18n::catalog::sidebar::STATE_0.get(),
        AgentState::Waiting => krill_app::i18n::catalog::sidebar::STATE_1.get(),
        AgentState::Blocked => krill_app::i18n::catalog::sidebar::STATE_2.get(),
        AgentState::Done => krill_app::i18n::catalog::sidebar::STATE_3.get(),
        AgentState::Exited => krill_app::i18n::catalog::sidebar::STATE_4.get(),
        AgentState::Unknown => krill_app::i18n::catalog::sidebar::STATE_5.get(),
    }
}

/// 같은 이름의 탭은 두 번째부터 " 2", " 3"을 붙여 구분한다(Orca 탭과 같은 의도).
pub(super) fn number_duplicates(labels: &mut [String]) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for label in labels.iter_mut() {
        let n = seen.entry(label.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            *label = format!("{label} {n}");
        }
    }
}

/// `Program::Agent { id }`가 `"fork:<agent>:<sid>"` 또는 `"fork:<sid>"`일 때
/// 실제 에이전트 프로필 ID만 돌려준다.
pub(super) fn launch_agent_id(id: &str) -> &str {
    match id.strip_prefix("fork:") {
        Some(rest) => rest.split_once(':').map_or("", |(agent, _)| agent),
        None => id,
    }
}

impl Shell {
    /// 탭·패인 헤더 제목. 데몬 스냅샷에 세션이 있으면 사이드바와 같은 규칙(`sidebar::session_label`:
    /// 터미널 제목 → 에이전트 → 폴더 → "세션 n")을 쓰고, 아직 없으면 제목 → 실행한 에이전트 이름 → "새 터미널".
    pub(super) fn pane_label(&self, pane: PaneId, cx: &App) -> String {
        if let Some(path) = self.layout.file(pane) {
            return krill_app::editor::file_name(path);
        }
        let title = self.titles.get(&pane).map(String::as_str);
        let host = self.layout.host(pane);
        if let Some(session) = self
            .layout
            .session(pane)
            .and_then(|id| self.sidebar.read(cx).session_on(host, id))
        {
            return sidebar::session_label(session, title).0;
        }
        let launched = match self.layout.launch(pane).map(|l| &l.program) {
            Some(Program::Agent { id }) => {
                let agent_id = launch_agent_id(id);
                self.agents
                    .as_ref()
                    .and_then(|a| a.profiles.iter().find(|p| p.id == agent_id))
                    .map(|p| p.name.as_str())
            }
            _ => None,
        };
        title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .or(launched)
            .unwrap_or(krill_app::i18n::catalog::shell::NEW_TERMINAL.get())
            .to_owned()
    }

    pub(super) fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let active = self.layout.active();
        let mut labels: Vec<_> = self
            .layout
            .tabs()
            .iter()
            .map(|t| self.pane_label(t.focused, cx))
            .collect();
        number_duplicates(&mut labels);
        let tabs = self
            .layout
            .tabs()
            .iter()
            .enumerate()
            .zip(labels)
            .map(|((i, tab), label)| {
                let selected = i == active;
                let host = self.layout.host(tab.focused);
                let session = self
                    .layout
                    .session(tab.focused)
                    .and_then(|id| self.sidebar.read(cx).session_on(host, id));
                let launch_agent = match self.layout.launch(tab.focused).map(|l| &l.program) {
                    Some(Program::Agent { id }) => launch_agent_id(id),
                    _ => "",
                };
                let agent = session.map_or(launch_agent, |s| {
                    if s.status.agent.is_empty() {
                        launch_agent
                    } else {
                        &s.status.agent
                    }
                });
                let state = session.map_or(AgentState::Unknown, |s| s.status.state);
                let unread = host.is_local()
                    && self
                        .layout
                        .session(tab.focused)
                        .is_some_and(|id| self.sidebar.read(cx).is_unread(id));
                let inferred = session
                    .and_then(|s| s.status.badge())
                    .map(|_| krill_app::i18n::catalog::general::INFERRED.get())
                    .unwrap_or_default();
                // OWE-707: 실행 당시 계정 배지(좁은 탭이라 이니셜 1자, 툴팁 전체 이름).
                // 이어가기 칩은 OWE-699까지 숨기고, 원 세션 PTY는 절대 닫지 않는다.
                let account_label = session
                    .and_then(|s| s.account.as_ref())
                    .map(|a| a.label.trim())
                    .filter(|l| !l.is_empty());
                let description = format!(
                    "{label} · {agent} · {} {inferred}{}{}",
                    state_label(state),
                    account_label
                        .map(|l| {
                            krill_app::i18n::catalog::sidebar::SESSION_ACCOUNT
                                .format(&[("label", l)])
                        })
                        .map(|b| format!(" · {b}"))
                        .unwrap_or_default(),
                    if unread {
                        format!(" · {}", krill_app::i18n::catalog::sidebar::UNREAD.get())
                    } else {
                        String::new()
                    }
                );
                let pane = tab.focused;
                let file_prefix = self.file_tab_prefix(pane, cx);
                Tab::new()
                    .label(SharedString::from(label.clone()))
                    .aria_label(description)
                    .group(CLOSE_GROUP)
                    .debug_selector(move || format!("session-tab-{i}"))
                    .prefix(
                        div()
                            .flex()
                            .items_center()
                            // Orca `gap-1`(4px): 상태 글리프와 에이전트 아이콘 사이.
                            .gap(ui::SPACE_1)
                            // Orca 비활성 탭 `opacity-70`: 아이덴티티는 유지하고 강조만 낮춘다.
                            .when(!selected, |d| d.opacity(ui::TAB_INACTIVE_OPACITY))
                            .when(tab.pinned, |d| {
                                d.child(
                                    ui::icons::svg_box(ui::icons::PIN_SVG, 11., p.muted_foreground)
                                        .debug_selector(move || format!("tab-pin-{i}")),
                                )
                            })
                            .children(tab.color.map(|color| {
                                div()
                                    .debug_selector(move || format!("tab-color-{i}"))
                                    .flex_none()
                                    .size(ui::STATE_DOT)
                                    .rounded_full()
                                    .bg(ui::tab_tag_color(color))
                            }))
                            .child(if let Some(fp) = file_prefix {
                                // MK-119: 파일 탭은 [파일 아이콘][미저장 점]으로 바꾼다.
                                fp.into_any_element()
                            } else {
                                // OWE-662: [상태 글리프(미확인이면 벨)][에이전트 아이콘][제목].
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(ui::SPACE_1)
                                    .child(
                                        ui::status::status_glyph(
                                            state,
                                            unread,
                                            ui::status::Size::Md,
                                            cx,
                                        )
                                        .debug_selector(move || format!("tab-state-{i}")),
                                    )
                                    .child(if agent.is_empty() {
                                        Icon::new(agent_icon(agent)).small().into_any_element()
                                    } else {
                                        ui::icons::agent_brand_icon(agent, 13., p.foreground)
                                            .into_any_element()
                                    })
                                    .children(self.yolo_badge(tab.focused, cx))
                                    .children(account_label.map(|l| {
                                        let short = ui::status::account_initial(l);
                                        let full = l.to_owned();
                                        let tip =
                                            krill_app::i18n::catalog::sidebar::SESSION_ACCOUNT
                                                .format(&[("label", &full)]);
                                        div()
                                            .id(("tab-account-badge", i))
                                            .aria_label(tip.clone())
                                            .debug_selector(move || {
                                                format!("tab-account-badge-{i}")
                                            })
                                            .flex_none()
                                            .px_1()
                                            .rounded(ui::RADIUS_XS)
                                            .bg(p.muted)
                                            .font_family(ui::FONT_UI)
                                            .text_size(ui::TEXT_2XS)
                                            .text_color(p.muted_foreground)
                                            .child(short)
                                            .tooltip(move |window, cx| {
                                                gpui_component::tooltip::Tooltip::new(tip.clone())
                                                    .build(window, cx)
                                            })
                                    }))
                                    .children(self.host_badge(tab.focused, "tab-host-badge", cx))
                                    .into_any_element()
                            }),
                    )
                    .suffix(
                        div()
                            .invisible()
                            // Orca: 활성 탭의 닫기는 항상 보이고, 비활성은 호버 때만.
                            .when(selected, Styled::visible)
                            .group_hover(CLOSE_GROUP, Styled::visible)
                            .child(
                                Button::new(("close-tab", i))
                                    .icon(IconName::Close)
                                    .ghost()
                                    .xsmall()
                                    .debug_selector(move || format!("close-tab-{i}"))
                                    .accessibility_label(text::CLOSE_TAB.get())
                                    .tooltip(text::CLOSE_TAB.get())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.request_close_tab(i, cx);
                                    })),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.request_close_tab(i, cx);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_tab_context_menu(i, e.position, cx);
                        }),
                    )
                    .on_drag(
                        TabDrag {
                            pane,
                            title: label.into(),
                        },
                        |drag, _, _, cx| cx.new(|_| drag.clone()),
                    )
                    .on_drop(cx.listener(move |this, drag: &TabDrag, _, cx| {
                        cx.stop_propagation();
                        if let Some(from) = this
                            .layout
                            .tabs()
                            .iter()
                            .position(|t| t.focused == drag.pane)
                        {
                            this.change(true, cx, |l| l.reorder_tab(from, i));
                        }
                    }))
            })
            .collect::<Vec<_>>();
        let profiles = self
            .agents
            .as_ref()
            .map(|s| s.profiles.clone())
            .unwrap_or_default();
        let locations = self.execution_locations(cx);
        let launch_host = self.launch_host.clone();
        let weak = cx.weak_entity();
        let disabled = self.agents_busy || self.agents.is_none() || self.link != Link::Up;
        div()
            .id("titlebar")
            .debug_selector(|| "titlebar".into())
            .flex()
            .items_center()
            .h(ui::TITLEBAR_HEIGHT)
            .min_w_0()
            .bg(p.sidebar)
            .border_b_1()
            .border_color(p.border)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_SM)
            .when(self.sidebar_collapsed, |d| {
                d.child(
                    div()
                        .pl(ui::TRAFFIC_LIGHT_PAD)
                        .pr_1p5()
                        .h_full()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1()
                        .border_r_1()
                        .border_color(p.sidebar_border)
                        .child(
                            Button::new("toggle-sidebar")
                                .icon(IconName::PanelLeft)
                                .ghost()
                                .xsmall()
                                .toggled(false)
                                .accessibility_label(text::TOGGLE_SIDEBAR.get())
                                .tooltip(text::TOGGLE_SIDEBAR.get())
                                .debug_selector(|| "toggle-sidebar".into())
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                        ),
                )
            })
            .child(
                TabBar::new("tabs")
                    .flex_1()
                    .min_w_0()
                    .max_width(ui::TAB_MAX_WIDTH)
                    .menu(true)
                    .selected_index(self.layout.active())
                    .children(tabs)
                    .on_click(cx.listener(|this, i: &usize, _, cx| {
                        this.change(true, cx, |l| l.activate(*i));
                    })),
            )
            .child(
                div()
                    .id("titlebar-drag")
                    .w(px(36.))
                    .h_full()
                    .flex_none()
                    .window_control_area(gpui::WindowControlArea::Drag),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_0p5()
                    .pr_2()
                    .child(self.render_commands(cx))
                    .child(
                        Button::new("new-tab")
                            .icon(IconName::Plus)
                            .ghost()
                            .xsmall()
                            .accessibility_label(text::NEW_TAB.get())
                            .tooltip(text::NEW_TAB.get())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.new_tab_on_launch_host(cx);
                            })),
                    )
                    .child(
                        Button::new("tab-agent-menu")
                            .icon(IconName::ChevronDown)
                            .ghost()
                            .xsmall()
                            .accessibility_label(
                                krill_app::i18n::catalog::general::AGENT_CHOICE.get(),
                            )
                            .tooltip(krill_app::i18n::catalog::general::AGENT_CHOICE.get())
                            .disabled(disabled)
                            .dropdown_menu(move |mut menu, _, _| {
                                menu = menu.label(text::AGENTS.get());
                                for profile in &profiles {
                                    let id = profile.id.clone();
                                    let weak = weak.clone();
                                    let icon =
                                        Icon::default().data(ui::icons::agent_svg_bytes(&id));
                                    menu = menu.item(
                                        PopupMenuItem::new(profile.name.clone())
                                            .icon(icon)
                                            .on_click(move |_, _, cx| {
                                                weak.update(cx, |this, cx| {
                                                    this.new_agent(id.clone(), cx);
                                                })
                                                .ok();
                                            }),
                                    );
                                }
                                menu = menu.separator().label(text::EXECUTION_LOCATION.get());
                                for (host_id, label) in &locations {
                                    let weak = weak.clone();
                                    let chosen = host_id.clone();
                                    menu = menu.item(
                                        PopupMenuItem::new(label.clone())
                                            .checked(&launch_host == host_id)
                                            .on_click(move |_, _, cx| {
                                                weak.update(cx, |this, cx| {
                                                    this.set_launch_host(chosen.clone(), cx);
                                                })
                                                .ok();
                                            }),
                                    );
                                }
                                menu
                            }),
                    )
                    .child(
                        Button::new("split-right")
                            .icon(IconName::PanelRight)
                            .ghost()
                            .xsmall()
                            .accessibility_label(text::SPLIT_RIGHT.get())
                            .tooltip(text::SPLIT_RIGHT.get())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.split_active(Axis::Horizontal, cx);
                            })),
                    )
                    .child(
                        Button::new("split-down")
                            .icon(IconName::PanelBottom)
                            .ghost()
                            .xsmall()
                            .accessibility_label(text::SPLIT_DOWN.get())
                            .tooltip(text::SPLIT_DOWN.get())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.split_active(Axis::Vertical, cx);
                            })),
                    )
                    .child(
                        Button::new("titlebar-jump")
                            .icon(IconName::Search)
                            .ghost()
                            .xsmall()
                            .accessibility_label(text::JUMP_TOOLTIP.get())
                            .tooltip(text::JUMP_TOOLTIP.get())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.toggle_jump(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("toggle-panel")
                            .icon(IconName::PanelRight)
                            .ghost()
                            .xsmall()
                            .toggled(!self.panel_collapsed)
                            .accessibility_label(text::TOGGLE_PANEL.get())
                            .tooltip(text::TOGGLE_PANEL.get())
                            .debug_selector(|| "toggle-panel".into())
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_panel(cx))),
                    ),
            )
    }

    /// OWE-734: 탭 닫기 요청. 고정된 탭(`pinned`)이면 확인 모달을 띄우고, 아니면 바로 닫는다.
    pub(super) fn request_close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.layout.tabs().len() {
            return;
        }
        self.tab_context_menu = None;
        if self.layout.is_tab_pinned(index) {
            self.confirm_close_tab = Some(index);
            cx.notify();
            return;
        }
        self.record_closed_tab(index);
        self.change(true, cx, |l| l.close_tab(index));
    }

    /// OWE-734: 고정된 탭 닫기 확인 모달에서 '닫기'를 눌렀을 때 실제 탭을 닫는다.
    pub(super) fn confirm_close_pinned_tab(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.confirm_close_tab.take() else {
            return;
        };
        if index < self.layout.tabs().len() {
            self.record_closed_tab(index);
            self.change(true, cx, |l| l.close_tab(index));
        } else {
            cx.notify();
        }
    }

    /// OWE-734: 고정된 탭 닫기 확인 모달을 취소한다.
    pub(super) fn cancel_close_pinned_tab(&mut self, cx: &mut Context<Self>) {
        self.confirm_close_tab = None;
        cx.notify();
    }

    /// OWE-734: 탭 우클릭 컨텍스트 메뉴를 연다.
    pub(super) fn open_tab_context_menu(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if index < self.layout.tabs().len() {
            self.tab_context_menu = Some((index, position));
            cx.notify();
        }
    }

    /// OWE-734: `keep_index`와 고정된 탭을 제외한 나머지 탭을 일괄 닫는다.
    pub(super) fn close_other_tabs(&mut self, keep_index: usize, cx: &mut Context<Self>) {
        let closing: Vec<usize> = self
            .layout
            .tabs()
            .iter()
            .enumerate()
            .filter_map(|(i, tab)| (i != keep_index && !tab.pinned).then_some(i))
            .collect();
        for i in closing {
            self.record_closed_tab(i);
        }
        self.change(true, cx, |l| l.close_other_tabs(keep_index));
    }

    /// OWE-734: `index` 오른쪽의 고정되지 않은 탭을 일괄 닫는다.
    pub(super) fn close_tabs_to_right(&mut self, index: usize, cx: &mut Context<Self>) {
        let closing: Vec<usize> = self
            .layout
            .tabs()
            .iter()
            .enumerate()
            .skip(index + 1)
            .filter_map(|(i, tab)| (!tab.pinned).then_some(i))
            .collect();
        for i in closing {
            self.record_closed_tab(i);
        }
        self.change(true, cx, |l| l.close_tabs_to_right(index));
    }

    /// OWE-734: 고정되지 않은 에디터(파일) 탭을 일괄 닫는다.
    pub(super) fn close_all_editor_tabs(&mut self, cx: &mut Context<Self>) {
        self.change(true, cx, Layout::close_all_editor_tabs);
    }

    /// OWE-734: `Ctrl+Tab` / `Ctrl+Shift+Tab`으로 최근 사용(MRU) 순서대로 탭을 순환한다.
    pub(super) fn on_recent_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            return;
        }
        self.tab_context_menu = None;
        self.change(true, cx, |l| l.cycle_mru(delta));
    }

    /// OWE-734: `Ctrl` 키를 떼거나 스위처를 닫을 때 MRU 순서를 확정한다.
    pub(super) fn commit_mru_switcher(&mut self, cx: &mut Context<Self>) {
        if self.layout.mru_switching() {
            self.layout.commit_mru();
            self.save();
            cx.notify();
        }
    }

    // Design adapted from Orca (.ref/orca/src/renderer/src/components/tab-bar/SortableTabContextMenu.tsx),
    // MIT License — Copyright (c) 2026 Lovecast Inc.
    pub(super) fn render_tab_context_menu(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        let (index, pos) = self.tab_context_menu?;
        let tab = self.layout.tabs().get(index)?;
        let is_pinned = tab.pinned;
        let current_color = tab.color;
        let has_closable_others = self
            .layout
            .tabs()
            .iter()
            .enumerate()
            .any(|(i, t)| i != index && !t.pinned);
        let has_closable_right = self.layout.tabs().iter().skip(index + 1).any(|t| !t.pinned);
        let has_closable_editors = self
            .layout
            .tabs()
            .iter()
            .enumerate()
            .any(|(i, t)| !t.pinned && self.layout.is_editor_tab(i));
        // OWE-736: 데몬이 세션 포크를 협상했을 때만 포크·이어하기 항목을 보인다.
        let pane = tab.focused;
        let peer_supports_fork = self.peer_supports_fork(pane);
        let can_fork = self.can_fork_pane(pane, cx);
        let can_continue = self.can_continue_pane(pane);

        let p = ui::palette(cx);
        let viewport = window.viewport_size();
        let menu_w = px(224.);
        let menu_h = if peer_supports_fork {
            px(300.)
        } else {
            px(236.)
        };
        let x = pos
            .x
            .min((viewport.width - menu_w - px(8.)).max(px(8.)))
            .max(px(8.));
        let y = pos
            .y
            .min((viewport.height - menu_h - px(8.)).max(px(8.)))
            .max(px(8.));

        let menu_item = |id: &'static str,
                         label: &'static str,
                         enabled: bool,
                         shortcut: Option<&'static str>| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px_2()
                .py_1()
                .rounded(ui::RADIUS_SM)
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .text_size(ui::TEXT_SM)
                .when(enabled, |d| {
                    d.cursor_pointer()
                        .text_color(p.popover_foreground)
                        .hover(|d| d.bg(p.accent).text_color(p.accent_foreground))
                })
                .when(!enabled, |d| d.text_color(p.muted_foreground).opacity(0.5))
                .child(div().flex_1().min_w_0().truncate().child(label))
                .children(shortcut.map(|sc| ui::keycap(p, sc)))
        };

        let pin_label = if is_pinned {
            text::UNPIN_TAB.get()
        } else {
            text::PIN_TAB.get()
        };
        let pin_svg = if is_pinned {
            ui::icons::PIN_OFF_SVG
        } else {
            ui::icons::PIN_SVG
        };

        let swatches = std::iter::once({
            let selected = current_color.is_none();
            div()
                .id("tab-menu-color-none")
                .debug_selector(|| "tab-menu-color-none".into())
                .aria_label(text::COLOR_NONE.get())
                .size(px(16.))
                .rounded_full()
                .border_1()
                .border_color(if selected {
                    p.foreground
                } else {
                    p.muted_foreground
                })
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .child(div().w(px(8.)).h(px(1.)).bg(p.muted_foreground))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab_context_menu = None;
                    this.change(true, cx, |l| {
                        l.set_tab_color(index, None);
                    });
                }))
                .into_any_element()
        })
        .chain(
            krill_app::layout::TabColor::ALL
                .into_iter()
                .enumerate()
                .map(|(ci, color)| {
                    let selected = current_color == Some(color);
                    let slug = color.slug();
                    div()
                        .id(("tab-menu-color", ci))
                        .debug_selector(move || format!("tab-menu-color-{slug}"))
                        .aria_label(color_label(color))
                        .size(px(16.))
                        .rounded_full()
                        .bg(ui::tab_tag_color(color))
                        .border_2()
                        .border_color(if selected { p.foreground } else { p.popover })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tab_context_menu = None;
                            this.change(true, cx, |l| {
                                l.set_tab_color(index, Some(color));
                            });
                        }))
                        .into_any_element()
                }),
        );

        let menu =
            div()
                .id("tab-context-menu")
                .debug_selector(|| "tab-context-menu".into())
                .occlude()
                .absolute()
                .left(x)
                .top(y)
                .w(menu_w)
                .p_1()
                .rounded(ui::RADIUS_MD)
                .bg(p.popover)
                .text_color(p.popover_foreground)
                .border_1()
                .border_color(p.border)
                .font_family(ui::FONT_UI)
                .text_size(ui::TEXT_SM)
                .flex()
                .flex_col()
                .gap_0p5()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("tab-menu-pin")
                        .debug_selector(|| "tab-menu-pin".into())
                        .px_2()
                        .py_1()
                        .rounded(ui::RADIUS_SM)
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .hover(|d| d.bg(p.accent).text_color(p.accent_foreground))
                        .child(ui::icons::svg_box(pin_svg, 13., p.popover_foreground))
                        .child(pin_label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tab_context_menu = None;
                            this.change(true, cx, |l| {
                                l.toggle_pin_tab(index);
                            });
                        })),
                )
                .child(div().my_1().h(px(1.)).w_full().bg(p.border))
                .child(
                    menu_item(
                        "tab-menu-close",
                        text::CLOSE_TAB_MENU.get(),
                        true,
                        Some("⌘W"),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab_context_menu = None;
                        this.request_close_tab(index, cx);
                    })),
                )
                .child(
                    menu_item(
                        "tab-menu-close-others",
                        text::CLOSE_OTHER_TABS.get(),
                        has_closable_others,
                        None,
                    )
                    .when(has_closable_others, |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| {
                            this.tab_context_menu = None;
                            this.close_other_tabs(index, cx);
                        }))
                    }),
                )
                .child(
                    menu_item(
                        "tab-menu-close-right",
                        text::CLOSE_TABS_TO_RIGHT.get(),
                        has_closable_right,
                        None,
                    )
                    .when(has_closable_right, |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| {
                            this.tab_context_menu = None;
                            this.close_tabs_to_right(index, cx);
                        }))
                    }),
                )
                .child(
                    menu_item(
                        "tab-menu-close-editors",
                        text::CLOSE_ALL_EDITOR_TABS.get(),
                        has_closable_editors,
                        None,
                    )
                    .when(has_closable_editors, |d| {
                        d.on_click(cx.listener(|this, _, _, cx| {
                            this.tab_context_menu = None;
                            this.close_all_editor_tabs(cx);
                        }))
                    }),
                )
                .when(peer_supports_fork, |d| {
                    d.child(div().my_1().h(px(1.)).w_full().bg(p.border))
                        .child(
                            menu_item("tab-menu-fork", text::FORK_SESSION.get(), can_fork, None)
                                .when(can_fork, |d| {
                                    d.on_click(cx.listener(move |this, _, _, cx| {
                                        this.tab_context_menu = None;
                                        this.fork_pane_session(pane, cx);
                                    }))
                                }),
                        )
                        .child(
                            menu_item(
                                "tab-menu-continue",
                                text::CONTINUE_IN_NEW_SESSION.get(),
                                can_continue,
                                None,
                            )
                            .when(can_continue, |d| {
                                d.on_click(cx.listener(move |this, _, window, cx| {
                                    this.tab_context_menu = None;
                                    this.open_continue_session(pane, window, cx);
                                }))
                            }),
                        )
                })
                .child(div().my_1().h(px(1.)).w_full().bg(p.border))
                .child(
                    div()
                        .px_2()
                        .pt_1()
                        .pb_1p5()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(ui::TEXT_XS)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(p.muted_foreground)
                                .child(text::TAB_COLOR.get()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_1p5()
                                .children(swatches),
                        ),
                );

        Some(
            gpui::deferred(
                gpui::anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("tab-context-menu-backdrop")
                        .occlude()
                        .w(viewport.width)
                        .h(viewport.height)
                        .relative()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.tab_context_menu = None;
                                cx.notify();
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _, _, cx| {
                                this.tab_context_menu = None;
                                cx.notify();
                            }),
                        )
                        .child(menu),
                ),
            )
            .with_priority(2),
        )
    }

    /// OWE-734: 고정된 탭 닫기 확인 모달(`PinnedTabCloseDialog`).
    pub(super) fn render_pinned_close_dialog(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        let index = self.confirm_close_tab?;
        let tab = self.layout.tabs().get(index)?;
        let label = self.pane_label(tab.focused, cx);
        let p = ui::palette(cx);
        let card = div()
            .id("pinned-tab-close-dialog")
            .debug_selector(|| "pinned-tab-close-dialog".into())
            .occlude()
            .w(px(360.))
            .p_4()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .font_family(ui::FONT_UI)
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(text::PINNED_CLOSE_TITLE.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .font_family(ui::FONT_MONO)
                            .text_color(p.muted_foreground)
                            .truncate()
                            .child(label),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(text::PINNED_CLOSE_DETAIL.get()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::secondary("cancel-close-pinned-tab", text::PINNED_CLOSE_CANCEL.get())
                            .debug_selector(|| "cancel-close-pinned-tab".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_close_pinned_tab(cx);
                            })),
                    )
                    .child(
                        Button::new("confirm-close-pinned-tab")
                            .small()
                            .danger()
                            .label(text::PINNED_CLOSE_CONFIRM.get())
                            .debug_selector(|| "confirm-close-pinned-tab".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_close_pinned_tab(cx);
                            })),
                    ),
            );
        Some(ui::modal_backdrop(window.viewport_size(), card))
    }

    // Design adapted from Orca (.ref/orca/src/renderer/src/components/tab-bar/RecentTabSwitcher.tsx),
    // MIT License — Copyright (c) 2026 Lovecast Inc.
    pub(super) fn render_recent_tab_switcher(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        let cursor = self.layout.mru_cursor()?;
        let p = ui::palette(cx);
        let mut labels: Vec<_> = self
            .layout
            .tabs()
            .iter()
            .map(|t| self.pane_label(t.focused, cx))
            .collect();
        number_duplicates(&mut labels);

        let rows = self
            .layout
            .mru_order()
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(pos, tab_idx)| {
                let tab = self.layout.tabs().get(tab_idx)?;
                let label = labels.get(tab_idx)?.clone();
                let selected = pos == cursor;
                let is_file = self.layout.file(tab.focused).is_some();
                Some(
                    div()
                        .id(("recent-tab-item", tab_idx))
                        .debug_selector(move || format!("recent-tab-item-{tab_idx}"))
                        .role(gpui::Role::ListItem)
                        .aria_selected(selected)
                        .h(px(32.))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .text_size(ui::TEXT_SM)
                        .when(selected, |d| d.bg(p.accent).text_color(p.accent_foreground))
                        .when(!selected, |d| {
                            d.text_color(p.foreground).hover(|d| d.bg(p.muted))
                        })
                        .when(tab.pinned, |d| {
                            d.child(ui::icons::svg_box(
                                ui::icons::PIN_SVG,
                                11.,
                                p.muted_foreground,
                            ))
                        })
                        .children(tab.color.map(|color| {
                            div()
                                .flex_none()
                                .size(ui::STATE_DOT)
                                .rounded_full()
                                .bg(ui::tab_tag_color(color))
                        }))
                        .child(
                            Icon::new(if is_file {
                                IconName::FileText
                            } else {
                                IconName::SquareTerminal
                            })
                            .small(),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.change(true, cx, |l| l.activate(tab_idx));
                        })),
                )
            })
            .collect::<Vec<_>>();

        let card = div()
            .id("recent-tab-switcher")
            .debug_selector(|| "recent-tab-switcher".into())
            .occlude()
            .w(px(360.))
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .font_family(ui::FONT_UI)
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(p.border)
                    .text_size(ui::TEXT_XS)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(p.muted_foreground)
                    .child(text::RECENT_TABS_TITLE.get()),
            )
            .child(div().py_1().flex().flex_col().children(rows));

        Some(ui::modal_backdrop(window.viewport_size(), card))
    }
}

fn color_label(color: krill_app::layout::TabColor) -> &'static str {
    use krill_app::layout::TabColor;
    match color {
        TabColor::Blue => text::COLOR_BLUE.get(),
        TabColor::Purple => text::COLOR_PURPLE.get(),
        TabColor::Pink => text::COLOR_PINK.get(),
        TabColor::Red => text::COLOR_RED.get(),
        TabColor::Orange => text::COLOR_ORANGE.get(),
        TabColor::Yellow => text::COLOR_YELLOW.get(),
        TabColor::Green => text::COLOR_GREEN.get(),
        TabColor::Teal => text::COLOR_TEAL.get(),
        TabColor::Gray => text::COLOR_GRAY.get(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::IconNamed as _;

    /// Agy 탭 아이콘이 핀 고정 아이콘(Star/StarFill)과 같으면 안 된다.
    #[test]
    fn agy_icon_differs_from_pin() {
        let agy = agent_icon("agy").path();
        assert_ne!(agy, IconName::Star.path(), "agy가 핀 빈 별과 같음");
        assert_ne!(agy, IconName::StarFill.path(), "agy가 핀 채운 별과 같음");
        // 다섯 에이전트 아이콘은 서로 달라야 한다.
        let icons = [
            agent_icon("claude").path(),
            agent_icon("codex").path(),
            agent_icon("agy").path(),
            agent_icon("opencode").path(),
            agent_icon("unknown").path(),
        ];
        for (i, a) in icons.iter().enumerate() {
            for b in &icons[i + 1..] {
                assert_ne!(a, b, "에이전트 아이콘 중복");
            }
        }
    }
}
