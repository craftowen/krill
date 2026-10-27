//! OWE-577: 탭 크롬·닫기·재정렬·에이전트 런처. gpui 의존은 셸 경계 안에 둔다.

use gpui::Entity;
use gpui_component::Icon;
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::tab::{Tab, TabBar};
use krill_app::ui;
use krill_proto::control::AgentState;

use super::*;

/// 탭 안에서 편집하는 이름과 입력 이벤트 구독.
pub(super) struct TabRename {
    pub(super) pane: PaneId,
    pub(super) input: Entity<InputState>,
    pub(super) _enter: Subscription,
    _blur: Subscription,
}

/// 세션은 재사용하지 않고 새 프로세스로 열되, 사용자 탭 속성은 복원한다.
pub(super) struct ClosedTab {
    workspace: workspaces::Workspace,
    position: usize,
    launch: Create,
    file: Option<PathBuf>,
    title: Option<String>,
    color: Option<krill_app::layout::TabColor>,
}

const CLOSE_GROUP: &str = "session-tab";

#[derive(Clone)]
struct TabDrag {
    pane: PaneId,
    title: SharedString,
    agent: String,
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
            .shadow_lg()
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_SM)
            .flex()
            .items_center()
            .gap(ui::SPACE_1)
            // OWE-886: 탭과 같은 식별자(상태는 드래그 프리뷰에 두지 않고 브랜드 아이콘만, §6.5).
            .child(ui::status::agent_icon(
                &self.agent,
                ui::ICON_MD,
                p.foreground,
            ))
            .child(self.title.clone())
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
    /// 에이전트 프로필 ID의 표시 이름. 설정을 아직 못 읽었거나 없는 프로필이면 `None`.
    pub(super) fn agent_profile_name(&self, id: &str) -> Option<&str> {
        self.agents
            .as_ref()
            .and_then(|a| a.profiles.iter().find(|p| p.id == id))
            .map(|p| p.name.as_str())
    }

    /// 탭·패인 헤더 제목. 터미널 제목이 비어 있으면 사이드바의 대화·자동 제목을 쓴다.
    /// 세션이 아직 없으면 제목 → 실행한 에이전트 이름 → "새 터미널" 순서로 고른다.
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
            return self
                .sidebar
                .read(cx)
                .session_label_on_with_title(host, session, title)
                .0;
        }
        let launched = match self.layout.launch(pane).map(|l| &l.program) {
            Some(Program::Agent { id }) => self.agent_profile_name(launch_agent_id(id)),
            _ => None,
        };
        title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .or(launched)
            .unwrap_or(krill_app::i18n::catalog::shell::NEW_TERMINAL.get())
            .to_owned()
    }

    pub(super) fn render_tab_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = ui::palette(cx);
        let active = self.layout.active();
        let labels = self.workspace_tab_labels(cx);
        let visible_active = labels.iter().position(|(i, _)| *i == active).unwrap_or(0);
        let indices: Vec<_> = labels.iter().map(|(i, _)| *i).collect();
        let tabs = labels
            .into_iter()
            .enumerate()
            .map(|(position, (i, label))| {
                let tab = &self.layout.tabs()[i];
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
                let detected_agent = session.map_or("", |s| {
                    ui::status::infer_agent_from_status(&s.status, &label)
                });
                let raw_agent = if detected_agent.is_empty() {
                    launch_agent
                } else {
                    detected_agent
                };
                let agent = ui::status::infer_agent_from_title(raw_agent, &label);
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
                // OWE-851: 에이전트명이 비면 `home ·  · 알 수 없음` 같은 빈 칸을 두지 않는다.
                let agent_segment = if agent.trim().is_empty() {
                    String::new()
                } else {
                    format!(" · {agent}")
                };
                let state_text = format!("{} {inferred}", state_label(state));
                let description = format!(
                    "{label}{agent_segment} · {}{}{}",
                    state_text.trim_end(),
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
                // 파일 탭에는 에이전트 상태가 없어 스크린리더에도 이름만 읽힌다.
                let description = if self.layout.file(pane).is_some() {
                    label.clone()
                } else {
                    description
                };
                let file_prefix = self.file_tab_prefix(pane, cx);
                let rename_input = self
                    .tab_rename
                    .as_ref()
                    .filter(|r| tab.root.contains(r.pane))
                    .map(|r| r.input.clone());
                let tab_hover_spec = if self.layout.file(pane).is_none() {
                    let sidebar_ref = self.sidebar.read(cx);
                    let now_ns = sidebar_ref.now_unix_ns();
                    let prompt_history = self
                        .layout
                        .session(pane)
                        .and_then(|sid| sidebar_ref.session_prompt_history_on(host, sid));
                    let since_ns = session.map_or(0, |s| s.status.since_unix_ns);
                    let state_with_age = if since_ns > 0 {
                        format!(
                            "{} ({})",
                            state_text.trim_end(),
                            krill_app::jump::format_age_ns(since_ns, now_ns)
                        )
                    } else {
                        state_text.trim_end().to_owned()
                    };
                    Some(ui::SessionHoverCardSpec {
                        id: format!("tab-hover-{i}").into(),
                        title: label.clone().into(),
                        agent: agent.to_owned().into(),
                        state_line: state_with_age.into(),
                        total_turns: prompt_history.map_or(0, |h| h.total_turns),
                        last_prompt: prompt_history
                            .and_then(krill_proto::git::SessionPromptHistory::display_last_prompt)
                            .map(Into::into),
                        last_prompt_age: prompt_history
                            .and_then(krill_proto::git::SessionPromptHistory::latest_unix_ns)
                            .filter(|&ts| ts > 0)
                            .map(|ts| krill_app::jump::format_age_ns(ts, now_ns).into()),
                        prev_turn: prompt_history
                            .and_then(krill_proto::git::SessionPromptHistory::previous_turn)
                            .map(|t| (t.turn, t.text.clone().into())),
                    })
                } else {
                    None
                };
                Tab::new()
                    .when(rename_input.is_none(), |t| {
                        t.label(SharedString::from(label.clone()))
                    })
                    .when_some(tab_hover_spec, |t, spec| {
                        t.tooltip(move |window, cx| {
                            ui::session_hover_card_view(spec.clone(), window, cx)
                        })
                    })
                    .when_some(rename_input, |t, input| {
                        t.child(
                            div()
                                .id("tab-rename-input")
                                .debug_selector(|| "tab-rename-input".into())
                                .w(px(140.))
                                .min_w_0()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(Input::new(&input).small()),
                        )
                    })
                    .text_size(ui::TEXT_SM)
                    .aria_label(description)
                    .group(CLOSE_GROUP)
                    .debug_selector(move || format!("session-tab-{i}"))
                    // 탭 안쪽 왼쪽 여백 12px, 짧은 제목도 탭 모양이 나게 최소 폭(STYLEGUIDE §5.3 탭 규칙).
                    .pl(ui::SPACE_3)
                    .min_w(ui::TAB_MIN_WIDTH)
                    .prefix(
                        div()
                            .debug_selector(move || format!("tab-prefix-{i}"))
                            .flex()
                            .items_center()
                            // Orca `gap-1`(4px): 상태 글리프와 에이전트 아이콘 사이.
                            .gap(ui::SPACE_1)
                            // 탭 기본 간격 4px과 더해 아이콘-제목 간격 8px.
                            .pr(ui::SPACE_1)
                            // Orca 비활성 탭 `opacity-70`: 아이덴티티는 유지하고 강조만 낮춘다.
                            .when(!selected, |d| d.opacity(ui::TAB_INACTIVE_OPACITY))
                            .when(tab.pinned, |d| {
                                d.child(
                                    ui::icons::svg_box(
                                        ui::icons::PIN_SVG,
                                        ui::ICON_XS,
                                        p.muted_foreground,
                                    )
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
                                    .child(
                                        // OWE-886: 빈 에이전트도 `status::agent_icon` 하나로 통일(§6.5).
                                        ui::status::agent_icon(agent, ui::ICON_SM, p.foreground),
                                    )
                                    .into_any_element()
                            }),
                    )
                    .suffix(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::SPACE_1)
                            .when(!tab.pinned, |d| {
                                d.child(
                                    div()
                                        .invisible()
                                        // Orca: 활성 탭의 닫기는 항상 보이고, 비활성은 호버 때만.
                                        .when(selected, Styled::visible)
                                        .group_hover(CLOSE_GROUP, Styled::visible)
                                        .child(ui::button_with_tooltip(
                                            Button::new(("close-tab", i))
                                                .icon(IconName::Close)
                                                .ghost()
                                                .xsmall()
                                                .debug_selector(move || format!("close-tab-{i}"))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.request_close_tab(i, cx);
                                                })),
                                            format!("close-tab-{i}"),
                                            text::CLOSE_TAB.get(),
                                        )),
                                )
                            })
                            // 탭 사이 구분선(Orca `border-r`). 활성 탭 양옆은 활성 탭 테두리가 대신해
                            // 겹치지 않게 숨기되 자리는 지켜 탭 폭이 바뀌지 않는다.
                            .child(
                                div()
                                    .debug_selector(move || format!("tab-separator-{i}"))
                                    .flex_none()
                                    .w(px(1.))
                                    .h(ui::SPACE_4)
                                    .bg(p.border)
                                    .when(
                                        selected || position + 1 == visible_active,
                                        Styled::invisible,
                                    ),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &gpui::MouseDownEvent, window, cx| {
                            // OWE-870: 더블클릭으로 탭 이름 변경 대화상자를 연다(한 번 클릭은 탭바가 받는다).
                            if e.click_count >= 2 {
                                cx.stop_propagation();
                                this.begin_tab_rename(i, window, cx);
                            }
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Middle,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if !this.layout.is_tab_pinned(i) {
                                this.request_close_tab(i, cx);
                            }
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
                            agent: agent.to_owned(),
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
                        .pl(super::traffic_light_pad(window))
                        .pr_1p5()
                        .h_full()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1()
                        .border_r_1()
                        .border_color(p.sidebar_border)
                        .child(ui::button_with_tooltip(
                            Button::new("toggle-sidebar")
                                .icon(IconName::PanelLeft)
                                .ghost()
                                .xsmall()
                                .toggled(false)
                                .debug_selector(|| "toggle-sidebar".into())
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                            "toggle-sidebar",
                            text::TOGGLE_SIDEBAR.get(),
                        )),
                )
            })
            .child(
                TabBar::new("tabs")
                    .flex_1()
                    .min_w_0()
                    .max_width(ui::TAB_MAX_WIDTH)
                    .menu(true)
                    .reveal_selected(true)
                    .selected_index(visible_active)
                    .children(tabs)
                    .on_click(cx.listener(move |this, i: &usize, _, cx| {
                        // 가운데 diff가 열려 있으면 탭을 누르는 것으로 닫고 터미널로 돌아간다.
                        if this.diff.read(cx).has_center_diff() {
                            this.close_center_diff(cx);
                        }
                        if let Some(&index) = indices.get(*i) {
                            this.change(true, cx, |l| l.activate(index));
                            this.files.refocus = true;
                            cx.notify();
                        }
                    })),
            )
            .child(
                div()
                    .id("titlebar-drag")
                    .debug_selector(|| "titlebar-drag".into())
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
                    .child(ui::button_with_tooltip(
                        Button::new("new-tab")
                            .icon(IconName::Plus)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.new_tab_on_launch_host(cx);
                            })),
                        "new-tab",
                        text::NEW_TAB.get(),
                    ))
                    .child(
                        ui::button_with_tooltip(
                            Button::new("tab-agent-menu")
                                .icon(IconName::ChevronDown)
                                .ghost()
                                .xsmall()
                                .disabled(disabled),
                            "tab-agent-menu",
                            krill_app::i18n::catalog::general::AGENT_CHOICE.get(),
                        )
                        .dropdown_menu(move |mut menu, _, _| {
                            menu = menu.label(text::AGENTS.get());
                            for profile in &profiles {
                                let id = profile.id.clone();
                                let weak = weak.clone();
                                let icon = ui::icons::agent_menu_icon(&id);
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
                    .child(ui::button_with_tooltip(
                        Button::new("split-right")
                            .icon(IconName::PanelRight)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.split_active(Axis::Horizontal, cx);
                            })),
                        "split-right",
                        text::SPLIT_RIGHT.get(),
                    ))
                    .child(ui::button_with_tooltip(
                        Button::new("split-down")
                            .icon(IconName::PanelBottom)
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.split_active(Axis::Vertical, cx);
                            })),
                        "split-down",
                        text::SPLIT_DOWN.get(),
                    ))
                    .child(ui::button_with_tooltip(
                        Button::new("titlebar-jump")
                            .icon(IconName::Search)
                            .ghost()
                            .xsmall()
                            .on_click(
                                cx.listener(|this, _, window, cx| this.toggle_jump(window, cx)),
                            ),
                        "titlebar-jump",
                        text::JUMP_TOOLTIP.get(),
                    ))
                    .child(ui::button_with_tooltip(
                        Button::new("toggle-chat-view")
                            .label(if self.is_active_chat_view_in(cx) {
                                text::VIEW_TERM.get()
                            } else {
                                text::VIEW_CHAT.get()
                            })
                            .ghost()
                            .xsmall()
                            .toggled(self.is_active_chat_view_in(cx))
                            .debug_selector(|| "toggle-chat-view".into())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_chat_view_with_focus(window, cx);
                            })),
                        "toggle-chat-view",
                        text::TOGGLE_CHAT_VIEW.get(),
                    ))
                    .child(ui::button_with_tooltip(
                        Button::new("toggle-panel")
                            .icon(IconName::PanelRight)
                            .ghost()
                            .xsmall()
                            .toggled(!self.panel_collapsed)
                            .debug_selector(|| "toggle-panel".into())
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_panel(cx))),
                        "toggle-panel",
                        text::TOGGLE_PANEL.get(),
                    )),
            )
    }

    pub(super) fn remember_closed_tab(&mut self, index: usize, cx: &App) {
        let Some(workspace) = self.tab_workspace(index, cx) else {
            return;
        };
        let Some(tab) = self.layout.tabs().get(index) else {
            return;
        };
        let position = self
            .tabs_in_workspace(index, cx)
            .iter()
            .position(|&i| i == index)
            .unwrap_or_default();
        let closed = ClosedTab {
            workspace: workspace.clone(),
            position,
            launch: self.launch_for(tab.focused),
            file: self.layout.file(tab.focused).map(ToOwned::to_owned),
            title: tab.custom_title.clone(),
            color: tab.color,
        };
        // 각 워크스페이스의 스택만 상한을 적용한다.
        if self
            .closed_tabs
            .iter()
            .filter(|t| t.workspace == workspace)
            .count()
            >= 10
            && let Some(oldest) = self
                .closed_tabs
                .iter()
                .position(|t| t.workspace == workspace)
        {
            self.closed_tabs.remove(oldest);
        }
        self.closed_tabs.push(closed);
    }

    pub(super) fn reopen_closed_tab(&mut self, cx: &mut Context<Self>) {
        let workspace = self.tab_workspace(self.layout.active(), cx);
        let Some(index) = self
            .closed_tabs
            .iter()
            .rposition(|t| workspace.as_ref().is_none_or(|w| &t.workspace == w))
        else {
            return;
        };
        let closed = self.closed_tabs.remove(index);
        let visible = self.workspace_tabs(cx);
        let destination = visible
            .get(closed.position)
            .copied()
            .or_else(|| visible.last().map(|i| i + 1))
            .unwrap_or(self.layout.tabs().len());
        self.change(true, cx, |l| {
            if let Some(file) = closed.file {
                l.open_file(file);
            } else {
                l.new_tab();
            }
            if let Some(pane) = l.focused() {
                l.set_host(pane, closed.workspace.host);
                l.set_launch(pane, closed.launch);
            }
            let active = l.active();
            l.set_tab_title(active, closed.title.as_deref());
            l.set_tab_color(active, closed.color);
            l.reorder_tab(active, destination.min(l.tabs().len().saturating_sub(1)));
        });
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
        self.maybe_close_tab(index, cx);
    }

    /// OWE-872: 칸에서 실행 중인 전경 프로세스 이름(루트 셸 제외).
    pub(super) fn pane_running_process(&self, pane: PaneId, cx: &App) -> Option<String> {
        let host = self.layout.host(pane);
        self.layout
            .session(pane)
            .and_then(|id| self.sidebar.read(cx).session_on(host, id))
            .and_then(|s| s.status.foreground.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }

    /// OWE-872: 탭 안의 칸에 실행 중인 에이전트(Working·Waiting·Blocked)나
    /// 셸이 아닌 전경 프로세스가 있으면 true.
    pub(super) fn pane_is_running(&self, pane: PaneId, cx: &App) -> bool {
        let host = self.layout.host(pane);
        self.layout
            .session(pane)
            .and_then(|id| self.sidebar.read(cx).session_on(host, id))
            .is_some_and(|s| {
                matches!(
                    s.status.state,
                    AgentState::Working | AgentState::Waiting | AgentState::Blocked
                ) || s
                    .status
                    .foreground
                    .as_deref()
                    .is_some_and(|name| !name.trim().is_empty())
            })
    }

    /// OWE-872: 탭 안의 칸 중 실행 중인 세션이 하나라도 있으면 true.
    pub(super) fn tab_has_running(&self, index: usize, cx: &App) -> bool {
        self.layout
            .tab_panes(index)
            .iter()
            .any(|p| self.pane_is_running(*p, cx))
    }

    /// OWE-872: 탭을 닫기 전에 실행 중인지 본다. 실행 중이면 확인 대화상자를 띄우고,
    /// 아니면 바로 닫는다. 일괄 닫기(다른·오른쪽·왼쪽 탭)는 메뉴에서 고른 명시적 동작이라 묻지 않는다.
    pub(super) fn maybe_close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.layout.tabs().len()
            && self.tab_has_running(index, cx)
            && krill_app::settings::confirm_running_close(cx)
        {
            self.pending_running_close = Some((PendingRunningClose::Tab(index), false));
            cx.notify();
            return;
        }
        self.do_close_tab(index, cx);
    }

    /// OWE-872: 탭을 실제로 닫는다(탭 세션 데몬 종료 포함).
    pub(super) fn do_close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.layout.tabs().len() {
            return;
        }
        self.close_workspace_tabs(vec![index], self.layout.active(), cx);
    }

    /// OWE-872: 실행 중 닫기 확인 대화상자에서 '닫기'를 눌렀다.
    /// '다시 묻지 않기'를 골랐으면 설정에 영속하고(`settings.toml` + 설정 스위치) 닫는다.
    pub(super) fn confirm_running_close(&mut self, cx: &mut Context<Self>) {
        let Some((pending, dont_ask)) = self.pending_running_close.take() else {
            return;
        };
        if dont_ask {
            krill_app::settings::set_confirm_running_close(false, cx);
        }
        match pending {
            PendingRunningClose::Tab(index) => self.do_close_tab(index, cx),
            PendingRunningClose::Pane(pane) => self.do_close_pane(pane, cx),
        }
    }

    /// OWE-872: 실행 중 닫기 확인 대화상자를 취소한다.
    pub(super) fn cancel_running_close(&mut self, cx: &mut Context<Self>) {
        self.pending_running_close = None;
        cx.notify();
    }

    /// OWE-734: 고정된 탭 닫기 확인 모달에서 '닫기'를 눌렀을 때 실제 탭을 닫는다.
    pub(super) fn confirm_close_pinned_tab(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.confirm_close_tab.take() else {
            return;
        };
        if index < self.layout.tabs().len() {
            // 고정 탭 확인은 실행 중 세션도 포함한 한 번의 확인이다.
            self.do_close_tab(index, cx);
        } else {
            cx.notify();
        }
    }

    /// OWE-734: 고정된 탭 닫기 확인 모달을 취소한다.
    pub(super) fn cancel_close_pinned_tab(&mut self, cx: &mut Context<Self>) {
        self.confirm_close_tab = None;
        cx.notify();
    }

    /// OWE-870: 탭 표시 이름. 사용자 지정 제목이 있으면 그걸, 없으면 자동 제목을 쓴다.
    pub(super) fn tab_display_label(&self, index: usize, cx: &App) -> String {
        if let Some(custom) = self.layout.tab_title(index) {
            return custom.to_owned();
        }
        self.layout
            .tabs()
            .get(index)
            .map_or(String::new(), |t| self.pane_label(t.focused, cx))
    }

    /// OWE-870: 탭 이름 인라인 편집을 연다. 입력창에는 지금 표시 이름이 미리 채워진다.
    pub(super) fn begin_tab_rename(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= self.layout.tabs().len() {
            return;
        }
        self.tab_context_menu = None;
        self.pending_focus = PendingFocus::None;
        self.files.refocus = false;
        self.change(true, cx, |l| l.activate(index));
        let current = self.tab_display_label(index, cx);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        let enter = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if this.tab_rename.as_ref().is_none_or(|r| r.input != *input) {
                    return;
                }
                if let InputEvent::PressEnter { .. } = event {
                    let composing = input.update(cx, |s, cx| {
                        gpui::EntityInputHandler::marked_text_range(s, window, cx).is_some()
                    });
                    if !composing {
                        this.finish_tab_rename(true, cx);
                        this.focus.focus(window, cx);
                    }
                }
            },
        );
        let input_handle = gpui::Focusable::focus_handle(input.read(cx), cx);
        let input_id = input.entity_id();
        let blur = cx.on_blur(&input_handle, window, move |this, _, cx| {
            if this
                .tab_rename
                .as_ref()
                .is_some_and(|r| r.input.entity_id() == input_id)
            {
                this.finish_tab_rename(true, cx);
            }
        });
        self.tab_rename = Some(TabRename {
            pane: self.layout.tabs()[index].focused,
            input,
            _enter: enter,
            _blur: blur,
        });
        cx.notify();
    }

    /// OWE-870: 이름 인라인 편집을 끝낸다. `save`면 입력값을 제목으로 저장(비우면 자동 제목으로 복귀).
    pub(super) fn finish_tab_rename(&mut self, save: bool, cx: &mut Context<Self>) {
        let Some(rename) = self.tab_rename.take() else {
            return;
        };
        if save {
            let title = rename.input.read(cx).value().trim().to_owned();
            let Some(index) = self
                .layout
                .tabs()
                .iter()
                .position(|tab| tab.root.contains(rename.pane))
            else {
                cx.notify();
                return;
            };
            self.change(true, cx, move |l| {
                l.set_tab_title(index, (!title.is_empty()).then_some(title.as_str()));
            });
        } else {
            cx.notify();
        }
    }

    /// OWE-870: 이름 인라인 편집을 취소한다.
    pub(super) fn cancel_tab_rename(&mut self, cx: &mut Context<Self>) {
        self.finish_tab_rename(false, cx);
        self.pending_focus = PendingFocus::Workspace;
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
        let closing = self
            .tabs_in_workspace(keep_index, cx)
            .into_iter()
            .filter(|&i| i != keep_index && !self.layout.is_tab_pinned(i))
            .collect();
        self.close_workspace_tabs(closing, keep_index, cx);
    }

    /// 현재 워크스페이스에서 오른쪽의 비고정 탭만 닫는다.
    pub(super) fn close_tabs_to_right(&mut self, index: usize, cx: &mut Context<Self>) {
        let closing = self
            .tabs_in_workspace(index, cx)
            .into_iter()
            .filter(|&i| i > index && !self.layout.is_tab_pinned(i))
            .collect();
        self.close_workspace_tabs(closing, self.layout.active(), cx);
    }

    /// 현재 워크스페이스에서 왼쪽의 비고정 탭만 닫는다.
    pub(super) fn close_tabs_to_left(&mut self, index: usize, cx: &mut Context<Self>) {
        let closing = self
            .tabs_in_workspace(index, cx)
            .into_iter()
            .filter(|&i| i < index && !self.layout.is_tab_pinned(i))
            .collect();
        self.close_workspace_tabs(closing, self.layout.active(), cx);
    }

    /// 현재 워크스페이스에서 비고정 파일 탭만 닫는다.
    pub(super) fn close_all_editor_tabs(&mut self, cx: &mut Context<Self>) {
        let closing = self
            .workspace_tabs(cx)
            .into_iter()
            .filter(|&i| self.layout.is_editor_tab(i) && !self.layout.is_tab_pinned(i))
            .collect();
        self.close_workspace_tabs(closing, self.layout.active(), cx);
    }

    /// OWE-734: `Ctrl+Tab` / `Ctrl+Shift+Tab`으로 최근 사용(MRU) 순서대로 탭을 순환한다.
    pub(super) fn on_recent_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.page_open() {
            return;
        }
        self.tab_context_menu = None;
        let panes = self.recent_panes(cx);
        self.layout.cycle_pane_mru_in(delta, &panes);
        if let Some(cursor) = self.layout.pane_mru_cursor() {
            let order = self.layout.pane_mru_order();
            let row = order
                .iter()
                .take(cursor)
                .filter(|p| panes.contains(p))
                .count();
            self.recent_tab_scroll.scroll_to_item(row);
        }
        cx.notify();
    }

    fn recent_pane_workspace(&self, pane: PaneId, cx: &App) -> krill_hosts::ssh::HostPath {
        let host = self.layout.host(pane);
        let folder = self
            .layout
            .launch(pane)
            .map(|launch| PathBuf::from(std::ffi::OsString::from_vec(launch.cwd.clone())))
            .or_else(|| {
                self.layout
                    .session(pane)
                    .and_then(|id| self.sidebar.read(cx).folder_on(host, id))
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| {
                PathBuf::from(std::ffi::OsString::from_vec(self.launch_for(pane).cwd))
            });
        let folder = self
            .sidebar
            .read(cx)
            .workspace_for_path_on(host, &folder)
            .unwrap_or(folder);
        krill_hosts::ssh::HostPath::new(host.clone(), folder)
    }

    fn recent_panes(&self, cx: &App) -> Vec<PaneId> {
        let workspace = self
            .tab_workspace(self.layout.active(), cx)
            .map(|workspace| krill_hosts::ssh::HostPath::new(workspace.host, workspace.folder));
        self.layout
            .pane_mru_order()
            .into_iter()
            .filter(|&pane| {
                self.layout.mru_scope() == krill_app::layout::MruScope::AllWorkspaces
                    || workspace.as_ref() == Some(&self.recent_pane_workspace(pane, cx))
            })
            .collect()
    }

    /// OWE-734: `Ctrl` 키를 떼거나 스위처를 닫을 때 MRU 순서를 확정한다.
    pub(super) fn commit_mru_switcher(&mut self, cx: &mut Context<Self>) {
        if self.layout.mru_switching() {
            self.change(true, cx, |l| {
                l.commit_mru();
            });
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
        let visible = self.tabs_in_workspace(index, cx);
        let has_closable_others = visible
            .iter()
            .any(|&i| i != index && !self.layout.is_tab_pinned(i));
        let has_closable_right = visible
            .iter()
            .any(|&i| i > index && !self.layout.is_tab_pinned(i));
        let has_closable_left = visible
            .iter()
            .any(|&i| i < index && !self.layout.is_tab_pinned(i));
        let has_closable_editors = visible
            .iter()
            .any(|&i| !self.layout.is_tab_pinned(i) && self.layout.is_editor_tab(i));
        // OWE-736: 데몬이 세션 포크를 협상했을 때만 포크·이어하기 항목을 보인다.
        let pane = tab.focused;
        let peer_supports_fork = self.peer_supports_fork(pane);
        let can_fork = self.can_fork_pane(pane, cx);
        let can_continue = self.can_continue_pane(pane);
        // OWE-874: 파일 탭이면 경로 복사·Finder 메뉴를 맨 위에 둔다(Orca EditorFileTabContextMenu).
        let file_path = self.layout.file(pane).map(ToOwned::to_owned);

        let p = ui::palette(cx);
        let viewport = window.viewport_size();
        let menu_w = px(232.);
        let max_w = (viewport.width - ui::SPACE_2 * 2.).max(px(0.));
        let max_h = (viewport.height - ui::SPACE_2 * 2.).max(px(0.));
        let x = pos
            .x
            .min((viewport.width - menu_w - ui::SPACE_2).max(ui::SPACE_2))
            .max(ui::SPACE_2);
        // Anchor above or below the pointer, using the actual available viewport half.
        // The scrollable menu fits even when rem-sized rows exceed the window.
        let below = pos.y <= viewport.height / 2.;
        let available_h = if below {
            viewport.height - pos.y - ui::SPACE_2
        } else {
            pos.y - ui::SPACE_2
        };

        let menu_item = |id: &'static str,
                         label: &'static str,
                         enabled: bool,
                         shortcut: Option<&'static str>| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .role(gpui::Role::MenuItem)
                .aria_label(label)
                .tab_index(0)
                .tab_stop(enabled)
                .focus_visible(|d| d.bg(p.accent).border_1().border_color(p.ring))
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
                .role(gpui::Role::Button)
                .tab_index(0)
                .focus_visible(|d| d.border_1().border_color(p.ring))
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
                        .role(gpui::Role::Button)
                        .tab_index(0)
                        .focus_visible(|d| d.border_color(p.ring))
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

        // OWE-874: 파일 탭 경로 메뉴(절대·상대 복사, Finder 보기). 세션 탭에는 두지 않는다.
        let mut file_items: Vec<AnyElement> = Vec::new();
        if let Some(path) = file_path {
            let abs = path.display().to_string();
            let rel = path
                .strip_prefix(self.folder())
                .map_or_else(|_| abs.clone(), |p| p.display().to_string());
            for (id, label, value) in [
                (
                    "tab-menu-copy-path",
                    krill_app::i18n::catalog::diff::EXPLORER_COPY_PATH.get(),
                    abs,
                ),
                (
                    "tab-menu-copy-relative-path",
                    krill_app::i18n::catalog::diff::EXPLORER_COPY_REL_PATH.get(),
                    rel,
                ),
            ] {
                file_items.push(
                    menu_item(id, label, true, None)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.tab_context_menu = None;
                            cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            file_items.push(
                menu_item(
                    "tab-menu-reveal-finder",
                    krill_app::i18n::catalog::diff::EXPLORER_REVEAL_FINDER.get(),
                    true,
                    None,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab_context_menu = None;
                    cx.reveal_path(&path);
                    cx.notify();
                }))
                .into_any_element(),
            );
            file_items.push(
                div()
                    .my_1()
                    .h(px(1.))
                    .w_full()
                    .bg(p.border)
                    .into_any_element(),
            );
        }

        let menu =
            div()
                .id("tab-context-menu")
                .track_focus(&self.tab_menu_focus)
                .role(gpui::Role::Menu)
                .tab_group()
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.tab_context_menu = None;
                        cx.stop_propagation();
                        cx.notify();
                    } else if event.keystroke.key == "tab"
                        && !event.keystroke.modifiers.control
                        && !event.keystroke.modifiers.alt
                        && !event.keystroke.modifiers.platform
                    {
                        if event.keystroke.modifiers.shift {
                            window.focus_prev(cx);
                        } else {
                            window.focus_next(cx);
                        }
                        cx.stop_propagation();
                        window.prevent_default();
                    }
                }))
                .debug_selector(|| "tab-context-menu".into())
                .occlude()
                .absolute()
                .left(x)
                .when(below, |d| d.top(pos.y.max(ui::SPACE_2)))
                .when(!below, |d| {
                    d.bottom((viewport.height - pos.y).max(ui::SPACE_2))
                })
                .w(menu_w)
                .max_w(max_w)
                .max_h(available_h.max(px(0.)).min(max_h))
                .overflow_y_scroll()
                .p_1()
                .rounded(ui::RADIUS_MD)
                .bg(p.popover)
                .text_color(p.popover_foreground)
                .border_1()
                .border_color(p.border)
                .shadow_lg()
                .font_family(ui::FONT_UI)
                .text_size(ui::TEXT_SM)
                .flex()
                .flex_col()
                .gap_0p5()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .children(file_items)
                .child(
                    div()
                        .id("tab-menu-pin")
                        .role(gpui::Role::Button)
                        .tab_index(0)
                        .focus_visible(|d| d.border_1().border_color(p.ring))
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
                // OWE-870: 탭 이름 변경(더블클릭과 같은 대화상자).
                .child(
                    menu_item("tab-menu-rename", text::RENAME_TAB.get(), true, None).on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.tab_context_menu = None;
                            this.begin_tab_rename(index, window, cx);
                        }),
                    ),
                )
                .child(
                    menu_item(
                        "tab-menu-close",
                        text::CLOSE_TAB_MENU.get(),
                        !is_pinned,
                        Some(if cfg!(target_os = "macos") {
                            "⌘W"
                        } else {
                            "Ctrl+Alt+W"
                        }),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab_context_menu = None;
                        if !this.layout.is_tab_pinned(index) {
                            this.request_close_tab(index, cx);
                        }
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
                // OWE-870: 왼쪽 탭 닫기(핀 고정 탭은 유지).
                .child(
                    menu_item(
                        "tab-menu-close-left",
                        text::CLOSE_TABS_TO_LEFT.get(),
                        has_closable_left,
                        None,
                    )
                    .when(has_closable_left, |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| {
                            this.tab_context_menu = None;
                            this.close_tabs_to_left(index, cx);
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
                                .items_center()
                                .justify_between()
                                .gap_1()
                                .children(swatches),
                        ),
                );

        Some(
            gpui::deferred(
                gpui::anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("tab-context-menu-backdrop")
                        .debug_selector(|| "tab-context-menu-backdrop".into())
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
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_close_pinned_tab(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "enter" && this.tab_dialog_focus.is_focused(window)
                {
                    this.confirm_close_pinned_tab(cx);
                    cx.stop_propagation();
                }
            }))
            .role(gpui::Role::AlertDialog)
            .debug_selector(|| "pinned-tab-close-dialog".into())
            .occlude()
            .w(ui::DIALOG_WIDTH)
            .max_w((window.viewport_size().width - ui::SPACE_4 * 2.).max(px(0.)))
            .max_h((window.viewport_size().height - ui::SPACE_4 * 2.).max(px(0.)))
            .p_4()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .font_family(ui::FONT_UI)
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .id("pinned-tab-close-dialog-body")
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.popover_foreground)
                            .child(text::PINNED_CLOSE_TITLE.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .font_family(ui::FONT_UI)
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
                    .flex_none()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::ghost("cancel-close-pinned-tab", text::PINNED_CLOSE_CANCEL.get())
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
        Some(ui::modal_backdrop(
            window.viewport_size(),
            ui::modal_focus_scope(card, &self.tab_dialog_focus),
        ))
    }

    /// OWE-872: 실행 중 닫기 확인 대화상자의 본문 문구. 전경 프로세스 이름이 있으면 함께 보인다.
    pub(super) fn running_close_detail(&self, cx: &App) -> Option<String> {
        let (pending, _) = self.pending_running_close.as_ref()?;
        let (title, process) = match *pending {
            PendingRunningClose::Tab(index) => {
                if index >= self.layout.tabs().len() {
                    return None;
                }
                let process = self
                    .layout
                    .tab_panes(index)
                    .into_iter()
                    .find_map(|p| self.pane_running_process(p, cx));
                (self.tab_display_label(index, cx), process)
            }
            PendingRunningClose::Pane(pane) => {
                if !self.layout.tabs().iter().any(|t| t.root.contains(pane)) {
                    return None;
                }
                (
                    self.pane_label(pane, cx),
                    self.pane_running_process(pane, cx),
                )
            }
        };
        Some(match process {
            Some(process) => text::RUNNING_CLOSE_PROCESS_DETAIL
                .format(&[("title", title.as_str()), ("process", process.as_str())]),
            None => text::RUNNING_CLOSE_DETAIL.format(&[("title", title.as_str())]),
        })
    }

    /// OWE-872: 실행 중인 탭·칸 닫기 확인 대화상자('다시 묻지 않기' 포함).
    pub(super) fn render_running_close_dialog(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        let (_, dont_ask) = self.pending_running_close.as_ref()?;
        let checked = *dont_ask;
        // 대화상자를 기다리는 동안 대상이 사라졌으면 그리지 않는다.
        let detail = self.running_close_detail(cx)?;
        let p = ui::palette(cx);
        let card = div()
            .id("running-close-dialog")
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_running_close(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "enter" && this.tab_dialog_focus.is_focused(window)
                {
                    this.confirm_running_close(cx);
                    cx.stop_propagation();
                }
            }))
            .role(gpui::Role::AlertDialog)
            .debug_selector(|| "running-close-dialog".into())
            .occlude()
            .w(ui::DIALOG_WIDTH)
            .max_w((window.viewport_size().width - ui::SPACE_4 * 2.).max(px(0.)))
            .max_h((window.viewport_size().height - ui::SPACE_4 * 2.).max(px(0.)))
            .p_4()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .font_family(ui::FONT_UI)
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .id("running-close-dialog-body")
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(p.foreground)
                            .child(text::RUNNING_CLOSE_TITLE.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(detail),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .debug_selector(|| "dont-ask-again".into())
                    .child(
                        Checkbox::new("dont-ask-again")
                            .label(text::DONT_ASK_AGAIN.get())
                            .checked(checked)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                if let Some((_, dont_ask)) = this.pending_running_close.as_mut() {
                                    *dont_ask = *checked;
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        ui::ghost("cancel-running-close", text::RUNNING_CLOSE_CANCEL.get())
                            .debug_selector(|| "cancel-running-close".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_running_close(cx);
                            })),
                    )
                    .child(
                        Button::new("confirm-running-close")
                            .debug_selector(|| "confirm-running-close".into())
                            .small()
                            .danger()
                            .label(text::RUNNING_CLOSE_CONFIRM.get())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_running_close(cx);
                            })),
                    ),
            );
        Some(ui::modal_backdrop(
            window.viewport_size(),
            ui::modal_focus_scope(card, &self.tab_dialog_focus),
        ))
    }

    // Design adapted from Orca (.ref/orca/src/renderer/src/components/tab-bar/RecentTabSwitcher.tsx),
    // MIT License — Copyright (c) 2026 Lovecast Inc.
    pub(super) fn render_recent_tab_switcher(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Deferred> {
        let cursor = self.layout.pane_mru_cursor()?;
        let p = ui::palette(cx);
        let panes = self.recent_panes(cx);
        let all = self.layout.mru_scope() == krill_app::layout::MruScope::AllWorkspaces;
        let rows = self
            .layout
            .pane_mru_order()
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(pos, pane)| {
                if !panes.contains(&pane) {
                    return None;
                }
                let tab_idx = (0..self.layout.tabs().len())
                    .find(|&i| self.layout.tab_panes(i).contains(&pane))?;
                let tab = self.layout.tabs().get(tab_idx)?;
                let mut label = if self.layout.tab_panes(tab_idx).len() == 1 {
                    self.tab_display_label(tab_idx, cx)
                } else {
                    self.pane_label(pane, cx)
                };
                if all {
                    let workspace = self.recent_pane_workspace(pane, cx);
                    label = format!(
                        "{label} · {} · {}",
                        workspace.host,
                        workspace.path.display()
                    );
                }
                let selected = pos == cursor;
                let is_file = self.layout.file(pane).is_some();
                let host = self.layout.host(pane);
                let session = self
                    .layout
                    .session(pane)
                    .and_then(|id| self.sidebar.read(cx).session_on(host, id));
                let launch_agent = match self.layout.launch(pane).map(|l| &l.program) {
                    Some(Program::Agent { id }) => launch_agent_id(id),
                    _ => "",
                };
                let agent = session.map_or_else(
                    || launch_agent.to_owned(),
                    |s| {
                        let inferred = ui::status::infer_agent_from_status(&s.status, &label);
                        if inferred.is_empty() {
                            launch_agent.to_owned()
                        } else {
                            inferred.to_owned()
                        }
                    },
                );
                Some(
                    div()
                        .id(("recent-tab-item", pane.get()))
                        .debug_selector(move || format!("recent-tab-item-{}", pane.get()))
                        .role(gpui::Role::ListBoxOption)
                        .aria_selected(selected)
                        .when(
                            selected,
                            gpui::StatefulInteractiveElement::aria_active_descendant,
                        )
                        .h(px(32.))
                        .flex_none()
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .text_size(ui::TEXT_SM)
                        .when(selected, |d| d.bg(p.accent).text_color(p.accent_foreground))
                        .when(!selected, |d| {
                            d.text_color(p.foreground).hover(|d| d.bg(p.accent))
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
                        .child(if is_file {
                            Icon::new(IconName::FileText).small().into_any_element()
                        } else {
                            // OWE-886: 빈 에이전트도 `status::agent_icon` 하나로 통일(§6.5).
                            ui::status::agent_icon(&agent, ui::ICON_MD, p.foreground)
                        })
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.change(true, cx, |l| {
                                l.activate(tab_idx);
                                l.focus(pane);
                            });
                        })),
                )
            })
            .collect::<Vec<_>>();

        let card = div()
            .id("recent-tab-switcher")
            .debug_selector(|| "recent-tab-switcher".into())
            .occlude()
            .w(ui::DIALOG_WIDTH)
            .max_w((window.viewport_size().width - ui::SPACE_4 * 2.).max(px(0.)))
            .max_h((window.viewport_size().height - ui::SPACE_4 * 2.).max(px(0.)))
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .font_family(ui::FONT_UI)
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(p.border)
                    .text_size(ui::TEXT_XS)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(p.muted_foreground)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(text::RECENT_TABS_TITLE.get())
                    .child(
                        Checkbox::new("recent-all-workspaces")
                            .label(text::RECENT_ALL_WORKSPACES.get())
                            .checked(all)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.layout.toggle_mru_scope();
                                this.on_recent_tab(0, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .id("recent-tab-list")
                    .debug_selector(|| "recent-tab-list".into())
                    .role(gpui::Role::ListBox)
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.recent_tab_scroll)
                    .py_1()
                    .flex()
                    .flex_col()
                    .children(rows),
            );

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

    #[cfg(unix)]
    #[gpui::test]
    fn tabs_leaf_switcher_scope_keeps_remote_workspace_separate(cx: &mut gpui::TestAppContext) {
        use crate::shell::tests::{open, state_path};
        use krill_app::hosts::HostId;
        let (view, cx) = open(cx, state_path("leaf-switcher"));
        cx.update(|_, cx| {
            view.update(cx, |shell, cx| {
                let first = shell.layout.focused().unwrap();
                shell.layout.split(Axis::Horizontal);
                let second = shell.layout.focused().unwrap();
                shell.layout.new_tab();
                let remote = shell.layout.focused().unwrap();
                shell.layout.set_host(remote, HostId::Remote("peer".into()));
                shell.layout.activate(0);
                assert_eq!(shell.layout.focused(), Some(second));
                let local = shell.recent_panes(cx);
                assert!(local.contains(&first) && local.contains(&second));
                assert!(!local.contains(&remote));
                shell.on_recent_tab(1, cx);
                assert_eq!(
                    shell.layout.pane_mru_order()[shell.layout.pane_mru_cursor().unwrap()],
                    first
                );
                shell.layout.toggle_mru_scope();
                shell.on_recent_tab(1, cx);
                let all = shell.recent_panes(cx);
                assert!(all.contains(&remote));
                assert_eq!(
                    shell.layout.pane_mru_order()[shell.layout.pane_mru_cursor().unwrap()],
                    remote
                );
                shell.layout.commit_mru();
                assert_eq!(shell.layout.focused(), Some(remote));
                assert_eq!(shell.layout.host(remote), &HostId::Remote("peer".into()));
            });
        });
    }

    /// OWE-886: 탭·패인 헤더 에이전트 아이콘은 브랜드 SVG(`agent_svg_bytes`)로 통일하며,
    /// 핀 고정 아이콘(`PIN_SVG`)과 겹치지 않아야 한다.
    #[test]
    fn brand_icons_differ_from_pin() {
        let pin = ui::icons::PIN_SVG;
        let icons = [
            ("claude", ui::icons::agent_svg_bytes("claude")),
            ("codex", ui::icons::agent_svg_bytes("codex")),
            ("agy", ui::icons::agent_svg_bytes("agy")),
            ("cursor", ui::icons::agent_svg_bytes("cursor")),
        ];
        for (name, bytes) in icons {
            assert_ne!(bytes, pin, "{name}가 핀 아이콘과 같음");
        }
        for (i, (a, ba)) in icons.iter().enumerate() {
            for (b, bb) in &icons[i + 1..] {
                assert_ne!(ba, bb, "{a}와 {b} 브랜드 아이콘 중복");
            }
        }
    }
}
