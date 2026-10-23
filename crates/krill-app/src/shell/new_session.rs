//! OWE-576: 사이드바 위 "+ 새 세션" 드롭다운(터미널·에이전트·권한 Manual/YOLO·폴더 열기).

use gpui::{App, Context, actions, div, prelude::*};
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
        krill_app::keys::bind("alt-cmd-1", NewAgent1, ctx),
        krill_app::keys::bind("alt-cmd-2", NewAgent2, ctx),
        krill_app::keys::bind("alt-cmd-3", NewAgent3, ctx),
        krill_app::keys::bind("alt-cmd-4", NewAgent4, ctx),
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
    /// OWE-856: 점프 검색 행(버튼 역할·이름 노출). 클릭 배선은 호출자가 단다.
    fn jump_row(
        p: &krill_app::ui::Palette,
        yolo: bool,
        menu: impl gpui::IntoElement,
    ) -> gpui::Stateful<gpui::Div> {
        use krill_app::i18n::catalog::sidebar as sb_text;

        div()
            .id("sidebar-jump")
            .debug_selector(|| "sidebar-jump".into())
            .group("sidebar-search-row")
            .role(gpui::Role::Button)
            .aria_label(sb_text::SEARCH_SHORT.get())
            .h(krill_app::ui::SIDEBAR_HEADER_HEIGHT)
            .flex()
            .items_center()
            .gap_1()
            .text_color(p.muted_foreground)
            .child(
                div()
                    .id("sidebar-search-control")
                    .size_7()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(krill_app::ui::RADIUS_SM)
                    .cursor_pointer()
                    .hover(|d| d.bg(p.sidebar_accent).text_color(p.sidebar_foreground))
                    .child(
                        gpui_component::Icon::new(IconName::Search)
                            .with_size(krill_app::ui::ICON_MD),
                    )
                    .tooltip(|window, cx| {
                        krill_app::ui::tooltip_view(
                            "sidebar-search",
                            sb_text::SEARCH_SHORT.get(),
                            window,
                            cx,
                        )
                    }),
            )
            .when(yolo, |d| {
                d.child(
                    div()
                        .id("yolo-global")
                        .debug_selector(|| "yolo-global".into())
                        .px_1()
                        .rounded(krill_app::ui::RADIUS_XS)
                        .bg(p.warning)
                        .text_color(p.warning_foreground)
                        .text_size(krill_app::ui::TEXT_2XS)
                        .child(krill_app::i18n::catalog::general::YOLO.get()),
                )
            })
            .child(
                div()
                    .id("new-session-wrap")
                    .debug_selector(|| "new-session".into())
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(menu),
            )
    }

    /// OWE-856: 탐색 행(버튼 역할·이름 노출). 클릭 배선은 호출자가 단다.
    fn nav_row(
        p: &krill_app::ui::Palette,
        id: &'static str,
        svg: &'static [u8],
        label: &'static str,
    ) -> gpui::Stateful<gpui::Div> {
        use krill_app::ui::icons;

        let nav_icon_color = p.sidebar_foreground.opacity(0.4);
        let nav_text_color = p.sidebar_foreground.opacity(0.68);
        let nav_hover_bg = p.sidebar_accent.opacity(0.6);
        div()
            .id(id)
            .debug_selector(move || id.into())
            .role(gpui::Role::Button)
            .aria_label(label)
            .size_7()
            .rounded(krill_app::ui::RADIUS_SM)
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .text_size(krill_app::ui::TEXT_MD)
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(nav_text_color)
            .hover(|d| d.bg(nav_hover_bg).text_color(p.sidebar_foreground))
            .child(icons::svg_icon(svg, krill_app::ui::ICON_SM, nav_icon_color))
            .tooltip(move |window, cx| krill_app::ui::tooltip_view(id, label, window, cx))
    }
    pub(super) fn new_agent_with_account(
        &mut self,
        id: String,
        account: String,
        cx: &mut Context<Self>,
    ) {
        if self.link != Link::Up || self.agents_busy {
            return;
        }
        self.new_agent_with_optional_account(id, Some(account), cx);
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
        if self
            .agents
            .as_ref()
            .is_some_and(|a| visible_profiles(a).next().is_some())
        {
            self.new_agent_at(index, cx);
        } else {
            self.notice = Some(
                krill_app::i18n::catalog::checks::D_AGENTS_NONE
                    .get()
                    .to_owned(),
            );
            cx.notify();
        }
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
            text::HOST_LOCAL
                .mac_or(krill_app::i18n::catalog::other_os::HOST_LOCAL)
                .get()
                .to_owned(),
        )];
        for host in self.sidebar.read(cx).remote_hosts() {
            if host.status == krill_app::hosts::HostStatus::Connected {
                out.push((
                    krill_app::hosts::HostId::Remote(host.id.clone()),
                    host.label.clone(),
                ));
            }
        }
        for (spec, status) in &self.host_statuses {
            let id = krill_app::hosts::HostId::Remote(spec.id.clone());
            if *status == krill_app::hosts::HostStatus::Connected
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
        let menu = krill_app::ui::button_with_tooltip(
            Button::new("new-session")
                .icon(IconName::Plus)
                .ghost()
                .xsmall(),
            "new-session",
            text::NEW_SESSION.get(),
        )
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
                let icon = krill_app::ui::icons::agent_menu_icon(&profile.id);
                let click_this = this.clone();
                let mut item = PopupMenuItem::new(profile.name.clone())
                    .icon(icon.clone())
                    .disabled(disabled)
                    .on_click(move |_, _, cx| {
                        click_this.update(cx, |s, cx| s.new_agent_at(i, cx)).ok();
                    });
                if let Some(action) = shortcut(i) {
                    item = item.action(action);
                }
                let choices = session_accounts(&accounts, &profile.id);
                if disabled || choices.len() <= 1 {
                    menu = menu.item(item);
                } else {
                    let this = this.clone();
                    let profile_id = profile.id.clone();
                    menu = menu.submenu_with_icon(
                        Some(krill_app::ui::icons::agent_menu_icon(&profile.id)),
                        profile.name.clone(),
                        window,
                        cx,
                        move |mut sub, _, _| {
                            let default_this = this.clone();
                            let default_id = profile_id.clone();
                            let mut default_item = PopupMenuItem::new(text::ACCOUNT_DEFAULT.get())
                                .disabled(disabled)
                                .on_click(move |_, _, cx| {
                                    default_this
                                        .update(cx, |s, cx| {
                                            s.new_agent(default_id.clone(), cx);
                                        })
                                        .ok();
                                });
                            if let Some(action) = shortcut(i) {
                                default_item = default_item.action(action);
                            }
                            sub = sub.item(default_item);
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
                        },
                    );
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

        div()
            .id("agents")
            .role(gpui::Role::Group)
            .aria_label(text::AGENTS.get())
            .px_2()
            .h(krill_app::ui::SIDEBAR_HEADER_HEIGHT)
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Self::jump_row(p, permission == Permission::Yolo, menu)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_jump(window, cx))),
            )
            .child(
                Self::nav_row(
                    p,
                    "nav-artifacts",
                    icons::FILES_SVG,
                    krill_app::i18n::catalog::artifacts::TITLE.get(),
                )
                .when(self.artifacts.is_some(), |d| {
                    d.bg(p.sidebar_accent)
                        .text_color(p.sidebar_accent_foreground)
                })
                .on_click(cx.listener(|this, _, window, cx| this.open_artifacts(Some(window), cx))),
            )
            .child(
                Self::nav_row(
                    p,
                    "nav-agent-dashboard",
                    icons::LAYOUT_DASHBOARD_SVG,
                    sb_text::NAV_AGENT_DASHBOARD.get(),
                )
                .when(self.dashboard.is_some(), |d| {
                    d.bg(p.sidebar_accent)
                        .text_color(p.sidebar_accent_foreground)
                })
                .on_click(cx.listener(|this, _, _, cx| this.dashboard(cx))),
            )
            .child(
                Self::nav_row(
                    p,
                    "nav-mobile",
                    icons::SMARTPHONE_SVG,
                    sb_text::NAV_MOBILE.get(),
                )
                .when(self.mobile_page_open(cx), |d| {
                    d.bg(p.sidebar_accent)
                        .text_color(p.sidebar_accent_foreground)
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_mobile_page(Some(window), cx);
                })),
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

    /// OWE-856: 점프 검색·대시보드 행은 버튼 역할·이름을 노출한다.
    #[test]
    fn jump_and_dashboard_rows_expose_button_role_and_label() {
        use krill_app::i18n::catalog::sidebar as sb_text;

        let p = &krill_app::ui::DARK;
        let jump_label = sb_text::SEARCH_SHORT.get();
        for yolo in [false, true] {
            let el = Shell::jump_row(p, yolo, gpui::div());
            let mut node = gpui::accesskit::Node::new(gpui::Role::Unknown);
            gpui::Element::write_a11y_info(&el, &mut node);
            assert_eq!(gpui::Element::a11y_role(&el), Some(gpui::Role::Button));
            assert_eq!(node.label(), Some(jump_label));
        }
        let dashboard_label = sb_text::NAV_AGENT_DASHBOARD.get();
        let el = Shell::nav_row(
            p,
            "nav-agent-dashboard",
            krill_app::ui::icons::LAYOUT_DASHBOARD_SVG,
            dashboard_label,
        );
        let mut node = gpui::accesskit::Node::new(gpui::Role::Unknown);
        gpui::Element::write_a11y_info(&el, &mut node);
        assert_eq!(gpui::Element::a11y_role(&el), Some(gpui::Role::Button));
        assert_eq!(node.label(), Some(dashboard_label));
    }
}
