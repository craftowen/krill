//! OWE-576: 사이드바 위 "+ 새 세션" 드롭다운(터미널·에이전트·권한 Manual/YOLO·폴더 열기).

use gpui::{App, Context, KeyBinding, actions, div, prelude::*};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{IconName, Sizable as _};
use krill_app::link::Link;
use krill_proto::agent::Permission;
use krill_proto::usage::{AccountInfo, AccountStatus, ProviderKind};

use super::{KEY_CONTEXT, NewTab, Shell, text};

actions!(shell, [NewAgent1, NewAgent2, NewAgent3, NewAgent4]);

/// 프로필 목록의 앞 네 개는 ⌥⌘1~4로 바로 띄운다.
pub(super) fn bind_keys(cx: &mut App) {
    let ctx = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("alt-cmd-1", NewAgent1, ctx),
        KeyBinding::new("alt-cmd-2", NewAgent2, ctx),
        KeyBinding::new("alt-cmd-3", NewAgent3, ctx),
        KeyBinding::new("alt-cmd-4", NewAgent4, ctx),
    ]);
}

fn shortcut(index: usize) -> Option<Box<dyn gpui::Action>> {
    Some(match index {
        0 => Box::new(NewAgent1),
        1 => Box::new(NewAgent2),
        2 => Box::new(NewAgent3),
        3 => Box::new(NewAgent4),
        _ => return None,
    })
}

fn visible_profiles(
    settings: &krill_proto::control::AgentSettings,
) -> impl Iterator<Item = &krill_proto::control::AgentProfile> {
    settings.profiles.iter()
}

fn session_accounts(
    snap: &krill_proto::usage::AccountsSnapshot,
    profile: &str,
) -> Vec<AccountInfo> {
    let Some(provider) = ProviderKind::parse(profile) else {
        return Vec::new();
    };
    if !snap.advanced.supported {
        return Vec::new();
    }
    snap.accounts
        .iter()
        .filter(|a| {
            a.provider == provider
                && !a.profile_dir.is_empty()
                && !snap.status_for(&a.id).is_some_and(|s| {
                    matches!(
                        s.status,
                        AccountStatus::NeedsLogin | AccountStatus::RateLimited
                    )
                })
        })
        .cloned()
        .collect()
}

impl Shell {
    fn new_agent_with_account(&mut self, id: String, account: String, cx: &mut Context<Self>) {
        use std::os::unix::ffi::OsStrExt as _;
        if self.link != Link::Up || self.agents_busy {
            return;
        }
        let cwd = self.folder().as_os_str().as_bytes().to_vec();
        self.change(true, cx, |layout| {
            layout.new_tab();
            if let Some(pane) = layout.focused() {
                layout.set_launch(
                    pane,
                    krill_proto::control::Create {
                        program: krill_proto::control::Program::Agent { id },
                        cwd,
                        cols: krill_app::pane::SIZE.0,
                        rows: krill_app::pane::SIZE.1,
                        account: Some(account),
                    },
                );
            }
        });
    }

    /// 프로필 목록의 `index`번째 에이전트를 새 탭에 띄운다. 연결 전·목록 밖이면 아무것도 하지 않는다.
    pub(super) fn new_agent_at(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.link != Link::Up || self.agents_busy {
            return;
        }
        let Some(id) = self
            .agents
            .as_ref()
            .and_then(|a| visible_profiles(a).nth(index))
            .map(|p| p.id.clone())
        else {
            return;
        };
        self.new_agent(id, cx);
    }

    /// ⌘⌥T: 설정의 기본 에이전트를 새 탭에 띄운다. 목록에 없으면 첫 프로필로 폴백한다.
    /// 설정의 기본 에이전트를 새 탭으로 연다.
    pub(super) fn new_default_agent(&mut self, cx: &mut Context<Self>) {
        let want = krill_app::settings::current(cx).default_agent;
        let index = self
            .agents
            .as_ref()
            .and_then(|a| visible_profiles(a).position(|p| p.id == want))
            .unwrap_or(0);
        self.new_agent_at(index, cx);
    }

    /// 셸 루트에 새 세션 단축키 처리기를 붙인다.
    pub(super) fn new_session_actions(root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        root.on_action(cx.listener(|this, _: &NewAgent1, _, cx| this.new_agent_at(0, cx)))
            .on_action(cx.listener(|this, _: &NewAgent2, _, cx| this.new_agent_at(1, cx)))
            .on_action(cx.listener(|this, _: &NewAgent3, _, cx| this.new_agent_at(2, cx)))
            .on_action(cx.listener(|this, _: &NewAgent4, _, cx| this.new_agent_at(3, cx)))
    }

    /// MK-131: 새 세션 메뉴에 표시할 '실행 위치' 선택지(내 Mac 기본값 + 연결된 원격 호스트).
    pub(crate) fn execution_locations(&self, cx: &App) -> Vec<(krill_app::hosts::HostId, String)> {
        let mut out = vec![(
            krill_app::hosts::HostId::Local,
            text::HOST_LOCAL.get().to_owned(),
        )];
        for host in self.sidebar.read(cx).remote_hosts() {
            if host.status == krill_app::hosts::HostStatus::Connected {
                out.push((
                    krill_app::hosts::HostId::Remote(host.id.clone()),
                    host.label.clone(),
                ));
            }
        }
        for (spec, status) in self.host_manager.statuses() {
            let id = krill_app::hosts::HostId::Remote(spec.id.clone());
            if status == krill_app::hosts::HostStatus::Connected
                && !out.iter().any(|(h, _)| h == &id)
            {
                out.push((id, spec.display_name().to_owned()));
            }
        }
        out
    }

    pub(crate) fn set_launch_host(
        &mut self,
        host: krill_app::hosts::HostId,
        cx: &mut Context<Self>,
    ) {
        self.launch_host = host;
        cx.notify();
    }

    /// MK-078: 스크린 리더는 이 줄을 "에이전트 새 세션" 그룹으로 읽는다.
    pub(super) fn render_agents(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        use krill_app::i18n::catalog::sidebar as sb_text;
        use krill_app::ui::icons;

        let p = krill_app::ui::palette(cx);
        let settings = self.agents.clone();
        let accounts = self.accounts_snapshot.clone();
        let permission = settings.as_ref().map(|a| a.permission).unwrap_or_default();
        let disabled = settings.is_none() || self.agents_busy || self.link != Link::Up;
        let locations = self.execution_locations(cx);
        let launch_host = self.launch_host.clone();
        let (this, focus) = (cx.weak_entity(), self.focus.clone());
        let (working_count, done_count) = self.sidebar.read(cx).agent_counts();
        let menu = Button::new("new-session")
            .icon(IconName::Plus)
            .ghost()
            .xsmall()
            .tooltip(text::NEW_SESSION.get())
            .accessibility_label(text::NEW_SESSION.get())
            .dropdown_menu(move |mut menu, window, cx| {
                menu = menu
                    .action_context(focus.clone())
                    .min_w(gpui::px(220.))
                    .item(
                        PopupMenuItem::new(text::NEW_TERMINAL.get())
                            .icon(IconName::SquareTerminal)
                            .action(Box::new(NewTab)),
                    );
                for (i, profile) in settings.iter().flat_map(visible_profiles).enumerate() {
                    let item = PopupMenuItem::new(profile.name.clone())
                        .icon(
                            gpui_component::Icon::default()
                                .data(krill_app::ui::icons::agent_svg_bytes(&profile.id)),
                        )
                        .disabled(disabled);
                    let item = if let Some(action) = shortcut(i) {
                        item.action(action)
                    } else {
                        let this = this.clone();
                        item.on_click(move |_, _, cx| {
                            this.update(cx, |s, cx| s.new_agent_at(i, cx)).ok();
                        })
                    };
                    let choices = session_accounts(&accounts, &profile.id);
                    if choices.is_empty() {
                        menu = menu.item(item);
                    } else {
                        let this = this.clone();
                        let profile_id = profile.id.clone();
                        menu =
                            menu.submenu(profile.name.clone(), window, cx, move |mut sub, _, _| {
                                let default_this = this.clone();
                                let default_id = profile_id.clone();
                                sub = sub.item(
                                    PopupMenuItem::new(text::ACCOUNT_DEFAULT.get())
                                        .disabled(disabled)
                                        .on_click(move |_, _, cx| {
                                            default_this
                                                .update(cx, |s, cx| {
                                                    s.new_agent(default_id.clone(), cx);
                                                })
                                                .ok();
                                        }),
                                );
                                for account in &choices {
                                    let this = this.clone();
                                    let id = profile_id.clone();
                                    let account_id = account.id.clone();
                                    sub = sub.item(
                                        PopupMenuItem::new(account.label.clone())
                                            .checked(account.active)
                                            .disabled(disabled)
                                            .on_click(move |_, _, cx| {
                                                this.update(cx, |s, cx| {
                                                    s.new_agent_with_account(
                                                        id.clone(),
                                                        account_id.clone(),
                                                        cx,
                                                    );
                                                })
                                                .ok();
                                            }),
                                    );
                                }
                                sub
                            });
                    }
                }
                menu = menu.separator().label(text::EXECUTION_LOCATION.get());
                for (host_id, label) in &locations {
                    let this = this.clone();
                    let chosen = host_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label.clone())
                            .checked(&launch_host == host_id)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |s, cx| s.set_launch_host(chosen.clone(), cx))
                                    .ok();
                            }),
                    );
                }
                menu = menu.separator().label(text::PERMISSION.get());
                for (choice, label) in [
                    (Permission::Manual, text::MANUAL.get()),
                    (Permission::Yolo, text::YOLO.get()),
                ] {
                    let this = this.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(permission == choice)
                            .disabled(disabled)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |s, cx| {
                                    s.load_agents(Some(choice), cx);
                                    cx.notify();
                                })
                                .ok();
                            }),
                    );
                }
                let this = this.clone();
                menu.separator().item(
                    PopupMenuItem::new(text::OPEN_FOLDER.get())
                        .icon(IconName::FolderOpen)
                        .on_click(move |_, _, cx| {
                            this.update(cx, |_, cx| Self::choose_folder(cx)).ok();
                        }),
                )
            });

        let nav_icon_color = p.sidebar_foreground.opacity(0.4);
        let nav_text_color = p.sidebar_foreground.opacity(0.68);
        let nav_hover_bg = p.sidebar_accent.opacity(0.6);
        let nav_row = |id: &'static str, svg: &'static [u8], label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .w_full()
                .h(gpui::px(28.))
                .px_2()
                .rounded(krill_app::ui::RADIUS_SM)
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_2p5()
                .text_size(krill_app::ui::TEXT_MD)
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(nav_text_color)
                .hover(|d| d.bg(nav_hover_bg).text_color(p.sidebar_foreground))
                .child(icons::svg_icon(svg, 15., nav_icon_color))
                .child(div().flex_1().min_w_0().truncate().child(label))
        };

        div()
            .id("agents")
            .role(gpui::Role::Group)
            .aria_label(text::AGENTS.get())
            .px_2()
            .pt_2()
            .pb_2()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .id("sidebar-jump")
                    .group("sidebar-search-row")
                    .w_full()
                    .h(gpui::px(30.))
                    .mb_1()
                    .px_2p5()
                    .rounded(krill_app::ui::RADIUS_SM)
                    .border_1()
                    .border_color(p.sidebar_border)
                    .bg(p.sidebar_accent.opacity(0.65))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_1p5()
                    .text_size(krill_app::ui::TEXT_MD)
                    .text_color(p.sidebar_foreground.opacity(0.45))
                    .hover(|d| {
                        d.bg(p.sidebar_accent)
                            .text_color(p.sidebar_foreground.opacity(0.75))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                gpui_component::Icon::new(IconName::Search)
                                    .with_size(gpui::px(14.))
                                    .text_color(p.sidebar_foreground.opacity(0.38)),
                            )
                            .child(div().truncate().child(sb_text::SEARCH_SHORT.get())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .invisible()
                                    .group_hover("sidebar-search-row", gpui::Styled::visible)
                                    .font_family(krill_app::ui::FONT_MONO)
                                    .text_size(krill_app::ui::TEXT_2XS)
                                    .text_color(p.sidebar_foreground.opacity(0.4))
                                    .child(krill_app::i18n::catalog::general::JUMP_SHORTCUT.get()),
                            )
                            .when(permission == Permission::Yolo, |d| {
                                d.child(
                                    div()
                                        .id("yolo-global")
                                        .debug_selector(|| "yolo-global".into())
                                        .invisible()
                                        .group_hover("sidebar-search-row", gpui::Styled::visible)
                                        .px_1()
                                        .rounded(krill_app::ui::RADIUS_XS)
                                        .bg(p.warning)
                                        .text_color(p.warning_foreground)
                                        .text_size(krill_app::ui::TEXT_2XS)
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(krill_app::i18n::catalog::general::YOLO.get())
                                        .tooltip(|window, cx| {
                                            gpui_component::tooltip::Tooltip::new(
                                                text::YOLO_TOGGLE.get(),
                                            )
                                            .build(window, cx)
                                        }),
                                )
                            })
                            .child(
                                div()
                                    .id("new-session-wrap")
                                    .debug_selector(|| "new-session".into())
                                    .invisible()
                                    .group_hover("sidebar-search-row", gpui::Styled::visible)
                                    .on_click(|_, _, cx| cx.stop_propagation())
                                    .child(menu),
                            ),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_jump(window, cx))),
            )
            .child(
                nav_row(
                    "nav-agent-dashboard",
                    icons::LAYOUT_DASHBOARD_SVG,
                    sb_text::NAV_AGENT_DASHBOARD.get(),
                )
                .when(working_count > 0 || done_count > 0, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(krill_app::ui::TEXT_XS)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .when(working_count > 0, |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_color(p.sidebar_foreground.opacity(0.6))
                                        .child(
                                            div().size(gpui::px(7.)).rounded_full().bg(p.warning),
                                        )
                                        .child(working_count.to_string()),
                                )
                            })
                            .when(done_count > 0, |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .text_color(p.sidebar_foreground.opacity(0.6))
                                        .child(
                                            div().size(gpui::px(7.)).rounded_full().bg(p.success),
                                        )
                                        .child(done_count.to_string()),
                                )
                            }),
                    )
                })
                .on_click(cx.listener(|this, _, _, cx| this.dashboard(cx))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_menu_requires_capability_and_excludes_wrong_provider_and_login_required() {
        let mut snap = krill_proto::usage::AccountsSnapshot::default();
        let a = AccountInfo {
            id: "codex-work".into(),
            provider: ProviderKind::Codex,
            label: "Work".into(),
            email: None,
            profile_dir: "/tmp/work".into(),
            active: false,
            host_default: false,
            stale: false,
            usage: krill_proto::usage::Snapshot::default(),
        };
        snap.accounts.push(a);
        assert!(session_accounts(&snap, "codex").is_empty());
        snap.advanced.supported = true;
        assert_eq!(session_accounts(&snap, "codex").len(), 1);
        assert!(session_accounts(&snap, "claude").is_empty());
        snap.statuses.push(krill_proto::usage::AccountStatusInfo {
            account_id: "codex-work".into(),
            status: AccountStatus::NeedsLogin,
            updated_at: None,
            detail: None,
        });
        assert!(session_accounts(&snap, "codex").is_empty());
    }
}
