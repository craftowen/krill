//! MK-098: 상태 바와 종류별 단일 팝아웃. 창을 닫아도 세션 종료 요청은 보내지 않는다.
//! 잠정 결정 의존: MK-030 gpui 셸. 거부 시 이 모듈과 shell.rs의 연결만 되돌린다.
//! 플로팅 창은 기존 Shell의 입력·IME·재접속을 재사용하고 레이아웃 파일은 쓰지 않는다.

use gpui::{
    App, Bounds, Context, Entity, Subscription, WeakEntity, Window, WindowBounds, WindowHandle,
    WindowOptions, actions, div, prelude::*, px,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use krill_app::{sidebar, ui};
use krill_proto::{
    SessionId,
    control::AgentState,
    screen::Screen,
    usage::{
        AccountInfo, AccountsSnapshot, AutoSwitchMetric, AutoSwitchRule, ProviderKind, UsageRequest,
    },
};

use super::{Link, Shell, WindowRole};

use krill_app::i18n::catalog::{popout as text, status as provider_text};

const LANES: [krill_app::i18n::Message; 4] = [
    text::LANE_NEEDS_YOU,
    text::LANE_WORKING,
    text::LANE_DONE,
    text::LANE_UNKNOWN,
];

fn counts(sessions: usize, needs_you: usize) -> String {
    text::COUNTS.format(&[
        ("sessions", &sessions.to_string()),
        ("needs_you", &needs_you.to_string()),
    ])
}

/// OWE-856: 데몬 연결점(상태 역할·이름 노출).
fn daemon_status_dot(tip: String, color: gpui::Hsla) -> gpui::Stateful<gpui::Div> {
    div()
        .id("status-daemon")
        .debug_selector(|| "status-daemon".into())
        .role(gpui::Role::Status)
        .aria_label(tip.clone())
        .flex()
        .items_center()
        .justify_center()
        .px_1()
        .py_0p5()
        .rounded(ui::RADIUS_XS)
        .child(div().size(ui::STATE_DOT).rounded_full().bg(color))
        .tooltip(move |window, cx| ui::tooltip_view("status-daemon", tip.clone(), window, cx))
}

/// OWE-856: 세션 수·알림 수(상태 역할·이름 노출).
fn usage_status(p: &ui::Palette, sessions: usize, needs_you: usize) -> gpui::Stateful<gpui::Div> {
    let usage_tip = counts(sessions, needs_you);
    let sessions_count = sessions.to_string();
    let needs_you_count = needs_you.to_string();
    let needs_you_color = if needs_you > 0 {
        p.agent_question
    } else {
        p.muted_foreground
    };
    div()
        .id("status-usage")
        .debug_selector(|| "status-usage".into())
        .role(gpui::Role::Status)
        .aria_label(usage_tip.clone())
        .flex()
        .items_center()
        .gap_2()
        .px_1()
        .py_0p5()
        .rounded(ui::RADIUS_XS)
        .child(
            div()
                .id("status-sessions")
                .debug_selector(|| "status-sessions".into())
                .flex()
                .items_center()
                .gap_1()
                .font_family(ui::FONT_MONO)
                .child(
                    gpui_component::Icon::default()
                        .data(ui::icons::TERMINAL_SVG)
                        .with_size(gpui_component::Size::XSmall),
                )
                .child(sessions_count),
        )
        .child(
            div()
                .id("status-needs-you")
                .debug_selector(|| "status-needs-you".into())
                .flex()
                .items_center()
                .gap_1()
                .font_family(ui::FONT_MONO)
                .text_color(needs_you_color)
                .child(
                    gpui_component::Icon::default()
                        .data(ui::icons::BELL_SVG)
                        .with_size(gpui_component::Size::XSmall)
                        .text_color(needs_you_color),
                )
                .child(needs_you_count),
        )
        .tooltip(move |window, cx| ui::tooltip_view("status-usage", usage_tip.clone(), window, cx))
}

/// OWE-851: 상태바 드롭다운 메뉴 열림·닫힘을 셸에 알려 메뉴가 열린 동안은
/// 호버 툴팁을 숨긴다(팝오버 하단 글자 가림 방지).
fn track_menu_open(weak: WeakEntity<Shell>) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    move |open, _, cx| {
        let open = *open;
        weak.update(cx, |shell, cx| {
            if open {
                shell.status_menus_open += 1;
            } else {
                shell.status_menus_open = shell.status_menus_open.saturating_sub(1);
            }
            cx.notify();
        })
        .ok();
    }
}

actions!(popout, [FloatTerminal, DashboardPopout, CloseDashboard]);

pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        krill_app::keys::bind("cmd-w", CloseDashboard, Some("Dashboard")),
        // OWE-851: 대시보드도 Esc로 닫힌다(터미널 입력이 없어 PTY 전달 걱정이 없다).
        krill_app::keys::bind("escape", CloseDashboard, Some("Dashboard")),
        krill_app::keys::bind("alt-cmd-a", FloatTerminal, Some(super::KEY_CONTEXT)),
        krill_app::keys::bind("alt-cmd-shift-a", DashboardPopout, None),
    ]);
}

fn options(title: &str, dimensions: gpui::Size<gpui::Pixels>, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None, dimensions, cx,
        ))),
        titlebar: Some(ui::titlebar(title.to_owned())),
        ..Default::default()
    }
}

fn existing<T: 'static>(cx: &App) -> Option<WindowHandle<T>> {
    cx.windows().into_iter().find_map(|w| w.downcast::<T>())
}

pub(crate) fn existing_dashboard(cx: &App) -> Option<Entity<Dashboard>> {
    cx.windows()
        .into_iter()
        .find_map(|w| w.downcast::<Shell>())
        .and_then(|w| w.read(cx).ok()?.dashboard.clone())
}

/// 별도 root 타입으로 메인 창의 레이아웃 쓰기 소유권과 구분한다.
pub(crate) struct Floating {
    pub(super) shell: Entity<Shell>,
}

impl Render for Floating {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title(text::FLOAT_LABEL.get());
        div()
            // OWE-861: 플로팅 창에서도 Shell 키 문맥이 잡혀 터미널 단축키(⌘C·⌘V·⌘A·⌘F·⌘K·⌘W)가 동작한다.
            // 실제 편집·닫기는 안쪽 Shell 핸들러(및 아래 ClosePane 갈무리)가 맡는다.
            .key_context(super::KEY_CONTEXT)
            .size_full()
            .capture_action(|_: &super::ClosePane, window, cx| {
                window.remove_window();
                cx.stop_propagation();
            })
            .child(self.shell.clone())
    }
}

fn open_floating(
    session: SessionId,
    target: Option<krill_app::hosts::HostTarget>,
    theme: krill_proto::theme::Theme,
    cx: &mut App,
) -> Result<(), String> {
    if let Some(handle) = existing::<Floating>(cx) {
        cx.defer(move |cx| {
            handle
                .update(cx, |floating, window, cx| {
                    floating.shell.update(cx, |shell, cx| {
                        if shell.layout.tabs().is_empty() {
                            shell.layout.new_tab();
                        }
                        let Some(pane) = shell.layout.focused() else {
                            return;
                        };
                        if shell.layout.session(pane) != Some(session) {
                            shell.commit_ime(pane, Some(window), cx);
                            shell.live.clear();
                            if let Some(t) = &target {
                                shell.layout.set_host(pane, t.host().clone());
                                shell.paths = t.local_paths().cloned();
                            }
                            shell.layout.set_session(pane, session);
                            shell.sync(cx);
                            cx.notify();
                        }
                    });
                    window.activate_window();
                })
                .ok();
        });
        return Ok(());
    }
    let mut opts = options(text::FLOAT_LABEL.get(), ui::FLOATING_SIZE, cx);
    opts.kind = gpui::WindowKind::Floating;
    cx.open_window(opts, |window, cx| {
        let shell = cx.new(|cx| {
            let mut shell = Shell::new(None, cx);
            shell.role = WindowRole::Floating;
            shell.sidebar_collapsed = true;
            shell.panel_collapsed = true;
            shell.sidebar.update(cx, |s, cx| s.set_visible(false, cx));
            shell.diff.update(cx, |d, cx| d.set_visible(false, cx));
            shell.terminal_theme = theme;
            if shell.layout.tabs().is_empty() {
                shell.layout.new_tab();
            }
            if let Some(pane) = shell.layout.focused() {
                if let Some(t) = &target {
                    shell.layout.set_host(pane, t.host().clone());
                    shell.paths = t.local_paths().cloned();
                }
                shell.layout.set_session(pane, session);
            }
            // AS-21: 링크를 직접 대입하지 않고 `set_link`로 연결한다. 데몬 상태를
            // "연결됨"으로 확정하고 에이전트 목록·계정·분리 세션을 함께 불러온다.
            shell.set_link(Link::Up, cx);
            shell
        });
        window.set_rem_size(px(16. * krill_app::settings::current(cx).zoom()));
        let focus = shell.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        cx.new(|_| Floating { shell })
    })
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn account_rule(snap: &AccountsSnapshot, account: &AccountInfo) -> AutoSwitchRule {
    snap.auto_switch
        .rule_for(account.provider, &account.id)
        .cloned()
        .unwrap_or_else(|| AutoSwitchRule {
            provider: account.provider,
            from_account_id: account.id.clone(),
            enabled: snap.auto_switch.enabled,
            threshold_percent: snap.auto_switch.threshold_percent.clamp(50, 99),
            metric: AutoSwitchMetric::Max,
            target_account_id: None,
        })
}

fn rule_item(
    label: impl Into<gpui::SharedString>,
    checked: bool,
    rule: AutoSwitchRule,
    weak: WeakEntity<Shell>,
) -> PopupMenuItem {
    PopupMenuItem::new(label)
        .checked(checked)
        .on_click(move |_, _, cx| {
            weak.update(cx, |shell, cx| shell.upsert_account_rule(rule.clone(), cx))
                .ok();
        })
}

fn append_rule_menu(
    mut menu: PopupMenu,
    snap: &AccountsSnapshot,
    account: &AccountInfo,
    weak: WeakEntity<Shell>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let rule = account_rule(snap, account);
    let mut toggle = rule.clone();
    toggle.enabled = !toggle.enabled;
    menu = menu.item(rule_item(
        if rule.enabled {
            provider_text::PROVIDER_RULE_ENABLED.get()
        } else {
            provider_text::PROVIDER_RULE_DISABLED.get()
        },
        rule.enabled,
        toggle,
        weak.clone(),
    ));
    let metric_rule = rule.clone();
    let metric_weak = weak.clone();
    menu = menu.submenu(
        format!(
            "{}: {}",
            provider_text::PROVIDER_RULE_METRIC.get(),
            rule.metric.label()
        ),
        window,
        cx,
        move |mut menu, _, _| {
            for metric in [
                AutoSwitchMetric::Max,
                AutoSwitchMetric::FiveHour,
                AutoSwitchMetric::SevenDay,
            ] {
                let mut next = metric_rule.clone();
                next.metric = metric;
                menu = menu.item(rule_item(
                    metric.label(),
                    metric_rule.metric == metric,
                    next,
                    metric_weak.clone(),
                ));
            }
            menu
        },
    );
    let threshold_rule = rule.clone();
    let threshold_weak = weak.clone();
    menu = menu.submenu(
        format!(
            "{}: {}%",
            provider_text::PROVIDER_RULE_THRESHOLD.get(),
            rule.threshold_percent
        ),
        window,
        cx,
        move |mut menu, _, _| {
            for threshold in [60, 70, 80, 85, 90, 95] {
                let mut next = threshold_rule.clone();
                next.threshold_percent = threshold;
                menu = menu.item(rule_item(
                    format!("{threshold}%"),
                    threshold_rule.threshold_percent == threshold,
                    next,
                    threshold_weak.clone(),
                ));
            }
            menu
        },
    );
    let targets: Vec<_> = snap
        .accounts
        .iter()
        .filter(|a| a.provider == account.provider && a.id != account.id)
        .cloned()
        .collect();
    let target_weak = weak.clone();
    menu = menu.submenu(
        provider_text::PROVIDER_RULE_TARGET.get(),
        window,
        cx,
        move |mut menu, _, _| {
            let mut automatic = rule.clone();
            automatic.target_account_id = None;
            menu = menu.item(rule_item(
                provider_text::PROVIDER_TARGET_AUTO.get(),
                rule.target_account_id.is_none(),
                automatic,
                target_weak.clone(),
            ));
            for target in &targets {
                let mut next = rule.clone();
                next.target_account_id = Some(target.id.clone());
                menu = menu.item(rule_item(
                    format!("→ {}", target.label),
                    rule.target_account_id.as_deref() == Some(target.id.as_str()),
                    next,
                    target_weak.clone(),
                ));
            }
            menu
        },
    );
    if snap
        .auto_switch
        .rule_for(account.provider, &account.id)
        .is_some()
    {
        let provider = account.provider;
        let id = account.id.clone();
        menu = menu.item(
            PopupMenuItem::new(krill_app::i18n::catalog::settings::ACCOUNTS_RULE_CLEAR.get())
                .on_click(move |_, _, cx| {
                    weak.update(cx, |shell, cx| shell.remove_account_rule(provider, &id, cx))
                        .ok();
                }),
        );
    }
    menu
}

fn render_popover_usage_row(
    window_label: &'static str,
    selector: String,
    win: Option<&krill_proto::usage::Window>,
    brand_color: gpui::Hsla,
    p: &ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let display = krill_app::accounts::display::window(
        win,
        i64::try_from(krill_app::accounts::display::now()).unwrap_or(i64::MAX),
    );
    let pct = display.percent.unwrap_or(0);
    let bar_color = krill_app::settings::usage_color(pct, brand_color, p);
    let pct_text = display.text;
    let reset_text = display
        .percent
        .and_then(|_| krill_app::settings::window_reset_suffix(win));
    let debug_sel = selector.clone();

    div()
        .id(gpui::SharedString::from(selector))
        .debug_selector(move || debug_sel.clone())
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .font_family(ui::FONT_MONO)
                        .text_size(ui::TEXT_2XS)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(p.muted_foreground)
                        .child(window_label),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .font_family(ui::FONT_MONO)
                                .text_size(ui::TEXT_2XS)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(krill_app::settings::usage_color(pct, p.foreground, p))
                                .child(pct_text),
                        )
                        .children(reset_text.map(|r| {
                            div()
                                .text_size(ui::TEXT_2XS)
                                .text_color(p.muted_foreground)
                                .child(format!("· {r}"))
                        })),
                ),
        )
        .child(
            div()
                .w_full()
                .h(px(5.))
                .rounded_full()
                .bg(p.muted)
                .overflow_hidden()
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(f32::from(pct) / 100.0))
                        .rounded_full()
                        .bg(bar_color),
                ),
        )
}

fn compact_account_usage(usage: &krill_proto::usage::Snapshot) -> String {
    krill_app::accounts::display::summary(usage, krill_app::accounts::display::now())
}

fn provider_usage_menu(
    mut menu: PopupMenu,
    snap: &AccountsSnapshot,
    provider: ProviderKind,
    weak: WeakEntity<Shell>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let mut accounts: Vec<_> = snap
        .accounts
        .iter()
        .filter(|a| a.provider == provider)
        .cloned()
        .collect();
    accounts.sort_by_key(|a| !a.host_default);
    let active = accounts
        .iter()
        .find(|a| a.active)
        .or_else(|| accounts.first())
        .cloned();

    menu = menu.min_w(px(300.)).max_w(px(340.));
    let active_card = active.clone();
    menu = menu.item(
        PopupMenuItem::element(move |_, cx| {
            let p = ui::palette(cx);
            let brand_color = ui::icons::provider_brand_color(provider, p.accent);
            let active_badge = active_card
                .as_ref()
                .map(|acc| krill_app::settings::account_heading(acc).0);
            let five_win = active_card
                .as_ref()
                .and_then(|acc| acc.usage.five_hour.as_ref());
            let seven_win = active_card
                .as_ref()
                .and_then(|acc| acc.usage.seven_day.as_ref());
            let card_id = format!("status-provider-card-{}", provider.as_str());
            let debug_id = card_id.clone();

            div()
                .id(gpui::SharedString::from(card_id))
                .debug_selector(move || debug_id.clone())
                .w(px(284.))
                .px_1()
                .py_1()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(ui::icons::provider_icon(provider, ui::ICON_MD, brand_color))
                                .child(
                                    div()
                                        .text_size(ui::TEXT_XS)
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(p.foreground)
                                        .child(provider.display_name()),
                                ),
                        )
                        .children(active_badge.map(|name| {
                            div()
                                .max_w(px(150.))
                                .truncate()
                                .px_1p5()
                                .py(px(1.))
                                .rounded(ui::RADIUS_XS)
                                .bg(p.muted)
                                .text_size(ui::TEXT_2XS)
                                .text_color(p.muted_foreground)
                                .child(name)
                        })),
                )
                .child(render_popover_usage_row(
                    krill_app::i18n::catalog::settings::WINDOW_5H.get(),
                    format!("status-provider-bar-{}-5h", provider.as_str()),
                    five_win,
                    brand_color,
                    p,
                ))
                .child(render_popover_usage_row(
                    krill_app::i18n::catalog::settings::WINDOW_7D.get(),
                    format!("status-provider-bar-{}-7d", provider.as_str()),
                    seven_win,
                    brand_color,
                    p,
                ))
        })
        .disabled(true),
    );

    menu = menu
        .separator()
        .label(provider_text::PROVIDER_ACCOUNTS_HEADER.get());
    for account in &accounts {
        let (title, _) = krill_app::settings::account_heading(account);
        let usage_summary = compact_account_usage(&account.usage);
        let status = krill_app::accounts::display::account(
            account,
            snap.statuses.iter().find(|s| s.account_id == account.id),
            krill_app::accounts::display::now(),
        );
        let rule_badge = snap
            .auto_switch
            .rule_for(provider, &account.id)
            .filter(|r| r.enabled)
            .map(|r| format!("⚡≥{}%", r.threshold_percent));
        let is_active = account.active;
        // OWE-696: Orca 감지 계정은 Krill이 실행할 폴더가 없어 고를 수 없다.
        let orca = krill_app::accounts::orca_managed(account);
        let id = account.id.clone();
        let row_id = format!(
            "status-provider-account-{}-{}",
            provider.as_str(),
            account.id
        );
        let account_weak = weak.clone();
        // 계정 한 줄: 이름(활성은 굵게)·자동전환 배지·사용량. ✓가 이름 줄에 붙어 고른 계정이 바로 보이고
        // 줄 전체를 눌러 전환한다. 상태는 정상이 아닐 때(로그인 필요·한도 등)만 둘째 줄에 보인다.
        let show_status = !matches!(
            status.status,
            krill_proto::usage::AccountStatus::Ok | krill_proto::usage::AccountStatus::Unknown
        );
        menu = menu.item(
            PopupMenuItem::element(move |_, cx| {
                let p = ui::palette(cx);
                let debug_id = row_id.clone();
                div()
                    .id(gpui::SharedString::from(row_id.clone()))
                    .debug_selector(move || debug_id.clone())
                    .w(px(264.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui::TEXT_XS)
                                    .font_weight(if is_active {
                                        gpui::FontWeight::SEMIBOLD
                                    } else {
                                        gpui::FontWeight::NORMAL
                                    })
                                    .text_color(p.foreground)
                                    .child(title.clone()),
                            )
                            .children(rule_badge.clone().map(|b| {
                                div()
                                    .flex_none()
                                    .px_1()
                                    .rounded(ui::RADIUS_XS)
                                    .bg(p.muted)
                                    .font_family(ui::FONT_MONO)
                                    .text_size(ui::TEXT_2XS)
                                    .text_color(p.muted_foreground)
                                    .child(b)
                            }))
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(ui::FONT_MONO)
                                    .text_size(ui::TEXT_2XS)
                                    .text_color(p.muted_foreground)
                                    .child(usage_summary.clone()),
                            ),
                    )
                    .when(show_status, |d| {
                        d.child(krill_app::accounts::display::render_status(
                            format!("{row_id}-status"),
                            &status,
                            p,
                        ))
                    })
            })
            .checked(is_active)
            .disabled(orca)
            .on_click(move |_, _, cx| {
                account_weak
                    .update(cx, |shell, cx| {
                        shell.refresh_accounts(
                            UsageRequest::SwitchAccount {
                                provider,
                                account_id: id.clone(),
                            },
                            cx,
                        );
                    })
                    .ok();
            }),
        );
    }

    if snap.advanced.supported {
        use krill_app::i18n::catalog::shell as policy_text;
        let now = krill_app::accounts::display::now();
        if let Some(active) = &active {
            for model in snap
                .advanced
                .model_windows
                .iter()
                .filter(|m| m.account_id == active.id)
            {
                // 모름 "—"·리셋 지남은 카드와 같은 공유 헬퍼로 현지화한다(OWE-703).
                let pct = krill_app::accounts::display::window(
                    Some(&model.seven_day),
                    i64::try_from(now).unwrap_or(i64::MAX),
                )
                .text;
                let window = krill_app::i18n::catalog::settings::WINDOW_7D.get();
                menu = menu.label(format!("{} · {window} {pct}", model.model));
            }
            for f in snap
                .advanced
                .forecasts
                .iter()
                .filter(|f| f.account_id == active.id)
            {
                menu = menu.label(policy_text::ACCOUNT_FORECAST.format(&[
                    ("window", &f.window),
                    (
                        "minutes",
                        &f.exhausts_at.saturating_sub(now).div_ceil(60).to_string(),
                    ),
                    ("rate", &f.percent_per_hour.to_string()),
                ]));
            }
            if let Some(limit) = snap
                .advanced
                .rate_limits
                .iter()
                .find(|l| l.account_id == active.id)
            {
                menu = menu.label(
                    policy_text::ACCOUNT_LIMIT_CLEARS
                        .format(&[("seconds", &limit.clears_at.saturating_sub(now).to_string())]),
                );
            }
        }
        if let Some(cd) = snap
            .advanced
            .cooldowns
            .iter()
            .find(|c| c.provider == provider && c.cooldown_until > now)
        {
            menu = menu.label(policy_text::ACCOUNT_COOLDOWN_LEFT.format(&[(
                "seconds",
                &cd.cooldown_until.saturating_sub(now).to_string(),
            )]));
        }
        let policy = snap.advanced.policy.clone();
        let weak = weak.clone();
        menu = menu.submenu(
            policy_text::ACCOUNT_POLICY.get(),
            window,
            cx,
            move |mut menu, _, _| {
                for (predict, label, checked) in [
                    (
                        true,
                        policy_text::ACCOUNT_PREDICT.get(),
                        policy.predict_exhaustion,
                    ),
                    (
                        false,
                        policy_text::ACCOUNT_RESET_FIRST.get(),
                        policy.prefer_soonest_reset,
                    ),
                ] {
                    let mut next = policy.clone();
                    if predict {
                        next.predict_exhaustion = !checked;
                    } else {
                        next.prefer_soonest_reset = !checked;
                    }
                    let weak = weak.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                        move |_, _, cx| {
                            weak.update(cx, |s, cx| {
                                s.refresh_accounts(
                                    UsageRequest::SetAdvancedPolicy(next.clone()),
                                    cx,
                                );
                            })
                            .ok();
                        },
                    ));
                }
                menu = menu.separator().label(policy_text::ACCOUNT_COOLDOWN.get());
                for minutes in [0_u64, 5, 15, 30] {
                    let mut next = policy.clone();
                    next.cooldown_secs = minutes.saturating_mul(60);
                    let weak = weak.clone();
                    menu = menu.item(
                        PopupMenuItem::new(
                            policy_text::ACCOUNT_COOLDOWN_MINUTES
                                .format(&[("minutes", &minutes.to_string())]),
                        )
                        .checked(policy.cooldown_secs == next.cooldown_secs)
                        .on_click(move |_, _, cx| {
                            weak.update(cx, |s, cx| {
                                s.refresh_accounts(
                                    UsageRequest::SetAdvancedPolicy(next.clone()),
                                    cx,
                                );
                            })
                            .ok();
                        }),
                    );
                }
                menu
            },
        );
    }
    menu = menu.separator();
    let policy = snap.auto_switch.clone();
    let weak_toggle = weak.clone();
    menu = menu.item(
        PopupMenuItem::new(provider_text::PROVIDER_GLOBAL_AUTO_SWITCH.get())
            .checked(policy.enabled)
            .on_click(move |_, _, cx| {
                let mut next = policy.clone();
                next.enabled = !next.enabled;
                weak_toggle
                    .update(cx, |shell, cx| {
                        shell.refresh_accounts(UsageRequest::SetAutoSwitch(next.clone()), cx);
                    })
                    .ok();
            }),
    );
    if let Some(active) = active {
        let snap_rule = snap.clone();
        let weak_rule = weak.clone();
        menu = menu.submenu(
            provider_text::PROVIDER_AUTO_SWITCH_HEADER.get(),
            window,
            cx,
            move |menu, window, cx| {
                append_rule_menu(menu, &snap_rule, &active, weak_rule.clone(), window, cx)
            },
        );
    }
    let refresh_weak = weak.clone();
    let open_weak = weak;
    menu.separator()
        .item(
            PopupMenuItem::new(provider_text::PROVIDER_REFRESH_USAGE.get()).on_click(
                move |_, _, cx| {
                    refresh_weak
                        .update(cx, |shell, cx| {
                            shell.refresh_accounts(UsageRequest::ListAccounts, cx);
                        })
                        .ok();
                },
            ),
        )
        .item(
            PopupMenuItem::element(|_, cx| {
                let p = ui::palette(cx);
                div()
                    .id("status-provider-open-accounts")
                    .debug_selector(|| "status-provider-open-accounts".into())
                    .text_size(ui::TEXT_XS)
                    .text_color(p.foreground)
                    .child(provider_text::PROVIDER_OPEN_SETTINGS.get())
            })
            .on_click(move |_, window, cx| {
                open_settings_from_menu(
                    &open_weak,
                    krill_app::settings::AdminTab::Accounts,
                    window,
                    cx,
                );
            }),
        )
}

fn open_settings_from_menu(
    weak: &gpui::WeakEntity<Shell>,
    tab: krill_app::settings::AdminTab,
    window: &mut Window,
    cx: &mut App,
) {
    if weak
        .update(cx, |shell, cx| shell.open_settings(tab, Some(window), cx))
        .is_err()
    {
        krill_app::settings::open_window_at_tab(tab, cx);
    }
}

/// OWE-702: 상태바·팝오버 계정 사용량 주기 갱신 간격(krilld 틱과 같은 60초).
pub(super) const ACCOUNTS_POLL: std::time::Duration = std::time::Duration::from_secs(60);

impl Shell {
    /// OWE-702: 데몬 연결 중에만 [`ACCOUNTS_POLL`]마다 `ListAccounts`를 다시 받는다. 링크가 끊기면 끝난다.
    /// 데몬에만 묻고(로컬 폴백 없음) 일시 오류는 배너 없이 다음 바퀴에 다시 시도한다.
    pub(super) fn poll_accounts(cx: &mut Context<Self>) -> gpui::Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ACCOUNTS_POLL).await;
                let Ok(Some(paths)) =
                    this.read_with(cx, |s, _| s.paths.clone().filter(|_| s.link == Link::Up))
                else {
                    return;
                };
                let res = cx
                    .background_executor()
                    .spawn(async move {
                        krill_app::accounts::request_ipc(&paths, &UsageRequest::ListAccounts)
                    })
                    .await;
                if let Ok(snap) = res
                    && this
                        .update(cx, |s, cx| s.apply_accounts_snapshot(snap, cx))
                        .is_err()
                {
                    return;
                }
            }
        })
    }

    pub(super) fn on_float_terminal(
        &mut self,
        _: &FloatTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.layout.focused() else {
            return;
        };
        self.commit_ime(pane, Some(window), cx);
        let Some(session) = self.layout.session(pane) else {
            return;
        };
        self.float_terminal(session, cx);
    }

    pub(super) fn float_terminal(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let target = if let Some(pane) = self
            .layout
            .focused()
            .filter(|&pane| self.layout.session(pane) == Some(session))
        {
            self.target_for_pane(pane)
        } else {
            self.local_target()
        };
        if let Err(error) = open_floating(session, target, self.terminal_theme, cx) {
            self.notice = Some(error);
            cx.notify();
        }
    }

    pub(super) fn on_dashboard(
        &mut self,
        _: &DashboardPopout,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dashboard(cx);
    }

    pub(super) fn dashboard(&mut self, cx: &mut Context<Self>) {
        if self.role == WindowRole::Floating
            && let Some(handle) = existing::<Shell>(cx)
        {
            if let Some(pane) = self.layout.focused() {
                self.commit_ime(pane, None, cx);
            }
            cx.defer(move |cx| {
                handle
                    .update(cx, |shell, window, cx| {
                        shell.dashboard(cx);
                        window.activate_window();
                    })
                    .ok();
            });
            return;
        }
        if let Some(dashboard) = &self.dashboard {
            dashboard.update(cx, |d, cx| {
                d.focus_on_render = true;
                cx.notify();
            });
            cx.notify();
            return;
        }
        if let Some(pane) = self.layout.focused() {
            self.commit_ime(pane, None, cx);
        }
        self.jump = None;
        self.continue_session = None;
        self.add_project = None;
        self.files.close_quick_open();
        if self.search.take().is_some()
            && let Some(l) = self.focused_live()
        {
            l.pane.clear_search();
        }
        let owner = cx.weak_entity();
        let sidebar = self.sidebar.clone();
        let previews = Dashboard::collect_previews(self);
        let dashboard = cx.new(|cx| Dashboard::new(owner, sidebar, previews, cx));
        self.dashboard = Some(dashboard);
        self.sync_sidebar_visibility(cx);
        self.sync_focused_session(cx);
        cx.notify();
    }

    pub(crate) fn upsert_account_rule(&mut self, rule: AutoSwitchRule, cx: &mut Context<Self>) {
        self.refresh_accounts(UsageRequest::UpsertAutoSwitchRule(rule), cx);
    }

    /// AS-22: 오케스트레이션 질문 응답을 백그라운드에서 저장한다. `mutate`는 파일 잠금
    /// (`flock`) + 쓰기라 UI 스레드에서 하면 CLI가 잠금을 쥐고 있을 때 멈춘다.
    /// 실패하면 배너로 알려준다(조용히 승인된 줄 아는 경우 방지).
    fn answer_question(
        state_dir: Option<std::path::PathBuf>,
        question_id: String,
        answer: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = state_dir else {
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    krill_proto::orchestration::OrchestrationStore::mutate(&dir, |st| {
                        st.reply_message(&question_id, "coordinator", answer)
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                if let Err(e) = outcome {
                    this.notice =
                        Some(text::QUESTION_ANSWER_FAILED.format(&[("reason", &e.to_string())]));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// AS-22: Gate 승인·거절도 질문 응답과 같은 경로(백그라운드 저장 + 실패 배너)로 처리한다.
    fn resolve_gate(
        state_dir: Option<std::path::PathBuf>,
        gate_id: String,
        decision: &'static str,
        cx: &mut Context<Self>,
    ) {
        let Some(dir) = state_dir else {
            cx.notify();
            return;
        };
        let note = if decision == "approve" {
            "approved in UI"
        } else {
            "rejected in UI"
        };
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    krill_proto::orchestration::OrchestrationStore::mutate(&dir, move |s| {
                        s.resolve_gate(&gate_id, decision, Some(note.to_owned()), "coordinator")
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                if let Err(e) = outcome {
                    this.notice =
                        Some(text::GATE_RESOLVE_FAILED.format(&[("reason", &e.to_string())]));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn remove_account_rule(
        &mut self,
        provider: ProviderKind,
        from_account_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.refresh_accounts(
            UsageRequest::RemoveAutoSwitchRule {
                provider,
                from_account_id: from_account_id.to_owned(),
            },
            cx,
        );
    }

    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let (color, daemon_tooltip) = match &self.link {
            Link::Up => (p.success, text::DAEMON_UP.get().to_owned()),
            Link::Connecting | Link::Down => (p.warning, text::DAEMON_CONNECTING.get().to_owned()),
            Link::Failed(err) => (
                p.destructive,
                if err.is_empty() {
                    text::DAEMON_FAILED.get().to_owned()
                } else {
                    format!("{}: {err}", text::DAEMON_FAILED.get())
                },
            ),
        };
        let items = self.sidebar.read(cx).jump_items();
        let needs_you = items.iter().filter(|i| i.needs_you).count();
        // OWE-851: 메뉴가 열려 있는 동안은 상태바 호버 툴팁을 숨긴다.
        let tooltips_on = self.status_menus_open == 0;
        let session = self.layout.focused().and_then(|p| self.layout.session(p));
        let snap = self.accounts_snapshot.clone();
        let status_providers = [
            ProviderKind::Claude,
            ProviderKind::Codex,
            ProviderKind::Antigravity,
            ProviderKind::OpenCode,
        ];
        let mut provider_pills = div()
            .id("status-provider-usage")
            .debug_selector(|| "status-provider-usage".into())
            .flex()
            .items_center()
            .gap_2p5();
        // OWE-652: 계정이 하나도 없는 제공자는 알약을 그리지 않는다(Orca처럼 간결하게).
        // 활성 계정의 모름·만료를 다른 계정 수치로 덮지 않는다(OWE-703).
        for provider in status_providers {
            // OWE-706: 설치 감지에서 빠진 프로바이더도 그리지 않는다(감지 전에는 모두 그린다).
            if !snap.accounts.iter().any(|a| a.provider == provider)
                || !krill_app::accounts::installed::is_installed(cx, provider)
            {
                continue;
            }
            let active = snap
                .accounts
                .iter()
                .find(|a| a.provider == provider && a.active)
                .or_else(|| snap.accounts.iter().find(|a| a.provider == provider));
            let now = krill_app::accounts::display::now();
            let percent = active.and_then(|a| {
                AutoSwitchMetric::Max
                    .effective_percent(&a.usage, i64::try_from(now).unwrap_or(i64::MAX))
            });
            let status = active.map(|a| {
                krill_app::accounts::display::account(
                    a,
                    snap.statuses.iter().find(|s| s.account_id == a.id),
                    now,
                )
            });
            let label = percent.map_or_else(|| "—".to_owned(), |pct| format!("{pct}%"));
            let color = match percent {
                _ if status.as_ref().is_some_and(|s| {
                    s.status == krill_proto::usage::AccountStatus::RateLimited
                }) =>
                {
                    p.warning
                }
                Some(pct) => krill_app::settings::usage_color(pct, p.foreground, p),
                None => p.muted_foreground,
            };
            let selector = format!("status-provider-pill-{}", provider.as_str());
            let debug_selector = selector.clone();
            let provider_snap = snap.clone();
            let weak = cx.weak_entity();
            let description = status.map_or_else(
                || provider.display_name().to_owned(),
                |s| format!("{} · {} · {}", provider.display_name(), s.label, s.updated),
            );
            provider_pills = provider_pills.child(
                ui::button_with_tooltip_when(
                    tooltips_on,
                    Button::new(gpui::SharedString::from(selector.clone()))
                        .label(label)
                        .font_family(ui::FONT_MONO)
                        .icon(ui::icons::provider_menu_icon(provider))
                        .text_color(color)
                        .ghost()
                        .xsmall()
                        .debug_selector(move || debug_selector.clone()),
                    selector,
                    description,
                )
                .dropdown_menu(move |menu, window, cx| {
                    provider_usage_menu(menu, &provider_snap, provider, weak.clone(), window, cx)
                })
                .on_open_change({
                    let track = track_menu_open(cx.weak_entity());
                    let shell = cx.weak_entity();
                    // 열 때마다 데몬의 활성 계정·사용량을 다시 받는다(다른 곳에서 바꾼 전환이 늦게 보이지 않게).
                    move |open: &bool, window: &mut Window, cx: &mut App| {
                        track(open, window, cx);
                        if *open {
                            shell
                                .update(cx, |s, cx| {
                                    if s.link == Link::Up {
                                        s.refresh_accounts(UsageRequest::ListAccounts, cx);
                                    }
                                })
                                .ok();
                        }
                    }
                }),
            );
        }
        let weak = cx.weak_entity();
        let refresh_button = ui::button_with_tooltip_when(
            tooltips_on,
            Button::new("status-usage-refresh")
                .icon(gpui_component::IconName::RefreshCw)
                .ghost()
                .xsmall()
                .debug_selector(|| "status-usage-refresh".into()),
            "status-usage-refresh",
            provider_text::PROVIDER_REFRESH_USAGE.get(),
        )
        .on_click(cx.listener(|shell, _, _, cx| {
            shell.refresh_accounts(UsageRequest::ListAccounts, cx);
        }));
        // 같은 기기 중복 등록은 사이드바처럼 한 번만 보인다(대표 항목만).
        let ssh_hosts: Vec<_> = self
            .ssh_hosts
            .iter()
            .filter(|h| !self.host_aliases.contains_key(&h.id))
            .cloned()
            .collect();
        let ssh_count_label = ssh_hosts.len().to_string();
        let ssh_full_label = text::SSH_MENU_BTN.format(&[("count", &ssh_count_label)]);
        let ssh_tooltip = format!("{} — {ssh_full_label}", text::SSH_MENU_TITLE.get());
        let ssh_menu = ui::button_with_tooltip_when(
            tooltips_on,
            Button::new("status-ssh-switcher")
                .icon(gpui_component::Icon::default().data(ui::icons::SERVER_SVG))
                .label(ssh_count_label)
                .font_family(ui::FONT_MONO)
                .ghost()
                .xsmall()
                .debug_selector(|| "status-ssh-switcher".into()),
            "status-ssh-switcher",
            ssh_tooltip,
        )
        .dropdown_menu(move |mut menu, _, _| {
            menu = menu.min_w(px(240.)).label(text::SSH_MENU_TITLE.get());
            for host in &ssh_hosts {
                let item_label = format!("{} ({})", host.label, host.destination);
                let host = host.clone();
                let weak = weak.clone();
                menu = menu.item(
                    PopupMenuItem::new(item_label).on_click(move |_, window, cx| {
                        weak.update(cx, |this, cx| {
                            this.connect_ssh_host(&host, Some(window), cx);
                        })
                        .ok();
                    }),
                );
            }
            let open_weak = weak.clone();
            menu.separator()
                .item(PopupMenuItem::new(text::OPEN_SSH_SETTINGS.get()).on_click(
                    move |_, window, cx| {
                        open_settings_from_menu(
                            &open_weak,
                            krill_app::settings::AdminTab::Ssh,
                            window,
                            cx,
                        );
                    },
                ))
        })
        .on_open_change(track_menu_open(cx.weak_entity()));
        let state_dir = self.state_dir(cx);
        let orch_store = state_dir
            .as_deref()
            .map(krill_proto::orchestration::OrchestrationStore::load_cached)
            .unwrap_or_default();
        let orch_compact_label = format!("{}·{}", orch_store.runs.len(), orch_store.tasks.len());
        let orch_full_label = text::ORCH_MENU_BTN.format(&[
            ("runs", &orch_store.runs.len().to_string()),
            ("tasks", &orch_store.tasks.len().to_string()),
        ]);
        let orch_tooltip = format!("{} — {orch_full_label}", text::ORCH_MENU_TITLE.get());
        let active_handoff_text = session.and_then(|sid| {
            self.live
                .values()
                .find(|l| l.pane.session() == Some(sid))
                .map(|l| (sid.0, screen_plain_text(&l.pane.shown())))
        });
        let orch_menu = ui::button_with_tooltip_when(tooltips_on,
            Button::new("status-orchestration-menu")
                .icon(gpui_component::Icon::default().data(ui::icons::WORKFLOW_SVG))
                .label(orch_compact_label)
                .font_family(ui::FONT_MONO)
                .ghost()
                .xsmall()
                .debug_selector(|| "status-orchestration-menu".into()),
            "status-orchestration-menu",
            orch_tooltip,
        )
        .dropdown_menu({
                let weak = cx.weak_entity();
                let state_dir = state_dir.clone();
                move |mut menu, _, _| {
                    menu = menu.min_w(px(300.)).label(text::ORCH_MENU_TITLE.get());
                    // OWE-652: Run이 하나도 없으면 빈 상태 안내를 보인다.
                    if !orch_has_content(&orch_store) {
                        menu = menu.item(PopupMenuItem::new(text::ORCH_EMPTY.get()).disabled(true));
                    }
                    for task in orch_store.tasks.iter().take(6) {
                        // OWE-883: 내부 영문 상태·`{:?}` 대신 카탈로그 표시 이름과 의존 목록을 보인다.
                        let row = krill_app::settings::orchestration_task_line(
                            &task.run_id,
                            &task.id,
                            &task.title,
                            task.status,
                            &task.deps,
                        );
                        menu = menu.item(PopupMenuItem::new(row).disabled(true));
                    }
                    let pending_questions: Vec<_> = orch_store
                        .messages
                        .iter()
                        .filter(|msg| {
                            msg.msg_type == krill_proto::orchestration::MessageType::Question
                                && !msg.acked
                        })
                        .take(3)
                        .collect();
                    if !pending_questions.is_empty() {
                        menu = menu.separator().label(text::PENDING_QUESTIONS.get());
                        for (idx, question) in pending_questions.into_iter().enumerate() {
                            let question_id = question.id.clone();
                            let summary = question.summary.clone();
                            let dir = state_dir.clone();
                            let weak = weak.clone();
                            menu = menu.item(PopupMenuItem::element(move |_, _| {
                                let mut row = div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().flex_1().text_size(ui::TEXT_XS).child(summary.clone()));
                                for (answer, label) in [
                                    ("yes", text::QUESTION_YES.get()),
                                    ("no", text::QUESTION_NO.get()),
                                ] {
                                    let button_id = format!("orch-question-{answer}-{idx}");
                                    let qid = question_id.clone();
                                    let dir = dir.clone();
                                    let weak = weak.clone();
                                    row = row.child(
                                        Button::new(gpui::ElementId::Name(button_id.clone().into()))
                                            .label(label)
                                            .debug_selector(move || button_id.clone())
                                            .ghost()
                                            .xsmall()
                                            .on_click(move |_, _, cx| {
                                                // AS-22: 파일 잠금+쓰기를 UI 스레드에서 하지 않는다.
                                                weak.update(cx, |_, cx| {
                                                    Shell::answer_question(
                                                        dir.clone(),
                                                        qid.clone(),
                                                        answer,
                                                        cx,
                                                    );
                                                })
                                                .ok();
                                            }),
                                    );
                                }
                                row
                            }));
                        }
                    }
                    let workers = unreleased_workers(&orch_store);
                    if !workers.is_empty() {
                        menu = menu.separator().label(text::WORKERS.get());
                        for (idx, dispatch) in workers.into_iter().enumerate() {
                            let did = dispatch.id.clone();
                            let label = format!(
                                "{} ({})",
                                dispatch.worker_name,
                                dispatch.status.as_str()
                            );
                            let dir = state_dir.clone();
                            let weak = weak.clone();
                            menu = menu.item(PopupMenuItem::element(move |_, _| {
                                let button_id = format!("orch-worker-release-{idx}");
                                let did = did.clone();
                                let dir = dir.clone();
                                let weak = weak.clone();
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().flex_1().text_size(ui::TEXT_XS).child(label.clone()))
                                    .child(
                                        Button::new(gpui::ElementId::Name(button_id.clone().into()))
                                            .label(text::WORKER_RELEASE.get())
                                            .debug_selector(move || button_id.clone())
                                            .ghost()
                                            .xsmall()
                                            .on_click(move |_, _, cx| {
                                                // R2: Released를 먼저 저장하지 않는다. 아카이빙→Close 검증→
                                                // Released 순서가 실패하면 상태는 그대로 두고 오류를 알린다.
                                                weak.update(cx, |this, cx| {
                                                    this.release_worker(did.clone(), dir.clone(), cx);
                                                })
                                                .ok();
                                            }),
                                    )
                            }));
                        }
                    }
                    let pending_gates: Vec<_> = orch_store
                        .gates
                        .iter()
                        .filter(|g| g.status == krill_proto::orchestration::GateStatus::Pending)
                        .cloned()
                        .collect();
                    if !pending_gates.is_empty() {
                        menu = menu.separator();
                        for gate in pending_gates {
                            let approve_label = text::ORCH_GATE_APPROVE
                                .format(&[("id", &gate.id), ("summary", &gate.question)]);
                            let reject_label = text::ORCH_GATE_REJECT
                                .format(&[("id", &gate.id), ("summary", &gate.question)]);
                            let gate_id_ok = gate.id.clone();
                            let dir_ok = state_dir.clone();
                            let weak_ok = weak.clone();
                            menu = menu.item(PopupMenuItem::new(approve_label).on_click(
                                move |_, _, cx| {
                                    // AS-22: 파일 잠금+쓰기를 UI 스레드에서 하지 않는다.
                                    weak_ok
                                        .update(cx, |_, cx| {
                                            Shell::resolve_gate(
                                                dir_ok.clone(),
                                                gate_id_ok.clone(),
                                                "approve",
                                                cx,
                                            );
                                        })
                                        .ok();
                                },
                            ));
                            let gate_id_no = gate.id;
                            let dir_no = state_dir.clone();
                            let weak_no = weak.clone();
                            menu = menu.item(PopupMenuItem::new(reject_label).on_click(
                                move |_, _, cx| {
                                    // AS-22: 파일 잠금+쓰기를 UI 스레드에서 하지 않는다.
                                    weak_no
                                        .update(cx, |_, cx| {
                                            Shell::resolve_gate(
                                                dir_no.clone(),
                                                gate_id_no.clone(),
                                                "reject",
                                                cx,
                                            );
                                        })
                                        .ok();
                                },
                            ));
                        }
                    }
                    if let Some((sid, ref text_body)) = active_handoff_text {
                        let body_codex = format!(
                            "[MK SESSION HANDOFF]\nSource session: s{sid}\nTarget agent: codex\n--- Recent terminal context ---\n{text_body}\n--- End context ---\n"
                        );
                        let body_claude = format!(
                            "[MK SESSION HANDOFF]\nSource session: s{sid}\nTarget agent: claude\n--- Recent terminal context ---\n{text_body}\n--- End context ---\n"
                        );
                        let weak_codex = weak.clone();
                        let weak_claude = weak.clone();
                        menu = menu
                            .separator()
                            .item(
                                PopupMenuItem::new(text::ORCH_HANDOFF_CODEX.get())
                                    .icon(ui::icons::agent_menu_icon("codex"))
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            body_codex.clone(),
                                        ));
                                        weak_codex
                                            .update(cx, |this, cx| {
                                                this.spawn_handoff_agent(
                                                    "codex",
                                                    body_codex.clone(),
                                                    cx,
                                                );
                                            })
                                            .ok();
                                    }),
                            )
                            .item(
                                PopupMenuItem::new(text::ORCH_HANDOFF_CLAUDE.get())
                                    .icon(ui::icons::agent_menu_icon("claude"))
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            body_claude.clone(),
                                        ));
                                        weak_claude
                                            .update(cx, |this, cx| {
                                                this.spawn_handoff_agent(
                                                    "claude",
                                                    body_claude.clone(),
                                                    cx,
                                                );
                                            })
                                            .ok();
                                    }),
                            );
                    }
                    let open_weak = weak.clone();
                    menu.separator().item(
                        PopupMenuItem::new(text::OPEN_WORKFLOWS_SETTINGS.get()).on_click(
                            move |_, window, cx| {
                                open_settings_from_menu(
                                    &open_weak,
                                    krill_app::settings::AdminTab::Workflows,
                                    window,
                                    cx,
                                );
                            },
                        ),
                    )
                }
            })
            .on_open_change(track_menu_open(cx.weak_entity()));
        let mut detected_ports = Vec::new();
        for live in self.live.values() {
            if let Some(sid) = live.pane.session() {
                let text_lines = screen_plain_text(&live.pane.shown());
                // AS-24: 화면 텍스트가 그대로면 정규식을 다시 돌리지 않는다.
                let hit = self
                    .ports_cache
                    .borrow()
                    .get(&sid.0)
                    .filter(|(text, _)| *text == text_lines)
                    .map(|(_, entries)| entries.clone());
                let entries = if let Some(entries) = hit {
                    entries
                } else {
                    let entries =
                        krill_proto::orchestration::extract_local_urls(sid.0, &text_lines, false);
                    self.ports_cache
                        .borrow_mut()
                        .insert(sid.0, (text_lines, entries.clone()));
                    entries
                };
                for entry in entries {
                    if !detected_ports
                        .iter()
                        .any(|e: &krill_proto::orchestration::PortEntry| e.url == entry.url)
                    {
                        detected_ports.push(entry);
                    }
                }
            }
        }
        // 사라진 세션의 메모를 비운다.
        self.ports_cache.borrow_mut().retain(|id, _| {
            self.live
                .values()
                .any(|live| live.pane.session().is_some_and(|sid| sid.0 == *id))
        });
        let ports_count_label = detected_ports.len().to_string();
        let ports_full_label = text::PORTS_MENU_BTN.format(&[("count", &ports_count_label)]);
        let ports_tooltip = format!("{} — {ports_full_label}", text::PORTS_MENU_TITLE.get());
        let ports_menu = ui::button_with_tooltip_when(
            tooltips_on,
            Button::new("status-ports-menu")
                .icon(gpui_component::Icon::default().data(ui::icons::PLUG_SVG))
                .label(ports_count_label)
                .font_family(ui::FONT_MONO)
                .ghost()
                .xsmall()
                .debug_selector(|| "status-ports-menu".into()),
            "status-ports-menu",
            ports_tooltip,
        )
        .dropdown_menu(move |mut menu, _, _| {
            menu = menu.min_w(px(240.)).label(text::PORTS_MENU_TITLE.get());
            if detected_ports.is_empty() {
                menu = menu.item(PopupMenuItem::new(text::PORTS_EMPTY.get()).disabled(true));
            } else {
                for entry in &detected_ports {
                    let label = format!("s{} · :{} → {}", entry.session_id, entry.port, entry.url);
                    let url = entry.url.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        // AS-23: 자식 프로세스로 `open`을 띄우면 회수하지 않아 좀비가 쌓인다.
                        // 같은 동작(기본 브라우저로 URL 열기)을 플랫폼에 맡긴다.
                        cx.open_url(&url);
                    }));
                }
            }
            menu
        })
        .on_open_change(track_menu_open(cx.weak_entity()));
        let weak = cx.weak_entity();
        let focus = self.focus.clone();
        let has_session = session.is_some();
        div()
            .id("status-bar")
            .debug_selector(|| "status-bar".into())
            .h(ui::STATUS_BAR_HEIGHT)
            .w_full()
            .flex_none()
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .bg(p.sidebar)
            .border_t_1()
            .border_color(p.border)
            .text_size(ui::TEXT_XS)
            .text_color(p.muted_foreground)
            .child(
                div()
                    .id("status-bar-left")
                    .debug_selector(|| "status-bar-left".into())
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_none()
                    .child(provider_pills)
                    .child(refresh_button),
            )
            .child(
                div()
                    .id("status-bar-right")
                    .debug_selector(|| "status-bar-right".into())
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(krill_app::recording::warning(self.recording, cx))
                    .children({
                        let checks = krill_app::onboarding::snapshot(cx).checks;
                        let failures = krill_app::onboarding::required_failures(&checks);
                        (!failures.is_empty()).then(|| {
                            let warn_title =
                                krill_app::i18n::catalog::onboarding::STATUS_CHECKS_WARN.get();
                            let details: Vec<&str> =
                                failures.iter().map(|c| c.summary.as_str()).collect();
                            let warn_tooltip = if details.is_empty() {
                                warn_title.to_owned()
                            } else {
                                format!("{warn_title}: {}", details.join(", "))
                            };
                            div()
                                .debug_selector(|| "status-checks-warning".into())
                                .child(
                                    ui::button_with_tooltip_when(
                                        tooltips_on,
                                        Button::new("status-checks-warning")
                                            .icon(
                                                gpui_component::Icon::default()
                                                    .data(ui::icons::TRIANGLE_ALERT_SVG),
                                            )
                                            .label(failures.len().to_string())
                                            .font_family(ui::FONT_MONO)
                                            .text_color(p.warning)
                                            .ghost()
                                            .xsmall(),
                                        "status-checks-warning",
                                        warn_tooltip,
                                    )
                                    .on_click(|_, _, cx| {
                                        krill_app::onboarding::open_checks_current(cx);
                                    }),
                                )
                        })
                    })
                    // Design adapted from Orca (.ref/orca/src/renderer/src/components/status-bar/UpdateStatusSegment.tsx),
                    // MIT License — Copyright (c) 2026 Orca Contributors.
                    .children(
                        cx.try_global::<krill_app::updater::UpdateState>()
                            .and_then(|u| u.staged.clone())
                            .map(|staged| {
                                let badge =
                                    krill_app::i18n::catalog::updater_relaunch::STATUS_READY_BADGE
                                        .format(&[("version", &staged.to_version)]);
                                let compact = format!("v{}", staged.to_version);
                                ui::button_with_tooltip_when(
                                    tooltips_on,
                                    Button::new("status-update-ready")
                                        .icon(
                                            gpui_component::Icon::default()
                                                .data(ui::icons::CIRCLE_ARROW_UP_SVG),
                                        )
                                        .label(compact)
                                        .font_family(ui::FONT_MONO)
                                        .text_color(p.success)
                                        .debug_selector(|| "status-update-ready".into())
                                        .ghost()
                                        .xsmall(),
                                    "status-update-ready",
                                    badge,
                                )
                                .on_click(cx.listener(
                                    |_, _, _, cx| {
                                        let mut u = cx
                                            .try_global::<krill_app::updater::UpdateState>()
                                            .cloned()
                                            .unwrap_or_default();
                                        u.card_collapsed = !u.card_collapsed;
                                        cx.set_global(u);
                                        cx.refresh_windows();
                                        cx.notify();
                                    },
                                ))
                            }),
                    )
                    .child(daemon_status_dot(daemon_tooltip, color))
                    .child(usage_status(p, items.len(), needs_you))
                    .child(orch_menu)
                    .child(ports_menu)
                    .child(ssh_menu)
                    .when(session.is_some(), |d| {
                        let float_tip =
                            format!("{} — {}", text::FLOAT_BUTTON.get(), text::FLOAT_LABEL.get());
                        d.child(
                            ui::button_with_tooltip_when(
                                tooltips_on,
                                Button::new("float-terminal")
                                    .icon(
                                        gpui_component::Icon::default()
                                            .data(ui::icons::PANELS_TOP_LEFT_SVG),
                                    )
                                    .ghost()
                                    .xsmall()
                                    .debug_selector(|| "float-terminal".into()),
                                "float-terminal",
                                float_tip,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.on_float_terminal(&FloatTerminal, window, cx);
                                },
                            )),
                        )
                    })
                    .child({
                        let dashboard_tip = format!(
                            "{} — {}",
                            text::DASHBOARD.get(),
                            text::DASHBOARD_LABEL.get()
                        );
                        ui::button_with_tooltip_when(
                            tooltips_on,
                            Button::new("dashboard-popout")
                                .icon(
                                    gpui_component::Icon::default()
                                        .data(ui::icons::LAYOUT_DASHBOARD_SVG),
                                )
                                .ghost()
                                .xsmall()
                                .debug_selector(|| "dashboard-popout".into()),
                            "dashboard-popout",
                            dashboard_tip,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.dashboard(cx)))
                    })
                    .child(self.history.clone())
                    .child(self.resources.clone()),
            )
            // 하-4: 상태바 우클릭 메뉴. 상태바에 이미 있는 동작과 설정 열기만 담는다.
            .context_menu(move |m, _, _| {
                let weak = weak.clone();
                m.action_context(focus.clone())
                    .item(
                        PopupMenuItem::element(|_, _| {
                            div()
                                .debug_selector(|| "status-bar-menu-refresh".into())
                                .child(provider_text::PROVIDER_REFRESH_USAGE.get())
                        })
                        .on_click(move |_, _, cx| {
                            weak.update(cx, |shell, cx| {
                                shell.refresh_accounts(UsageRequest::ListAccounts, cx);
                            })
                            .ok();
                        }),
                    )
                    .separator()
                    .menu_with_disabled(
                        text::FLOAT_MENU.get(),
                        Box::new(FloatTerminal),
                        !has_session,
                    )
                    .menu(text::DASHBOARD.get(), Box::new(DashboardPopout))
                    .separator()
                    .menu(
                        text::STATUS_OPEN_SETTINGS.get(),
                        Box::new(krill_app::settings::OpenSettings),
                    )
            })
    }
}

/// OWE-652: 오케스트레이션 메뉴에 보여줄 내용이 있는지. Run·Task가 모두 없을 때만
/// 빈 상태 안내(`ORCH_EMPTY`)를 보인다.
fn orch_has_content(store: &krill_proto::orchestration::OrchestrationStore) -> bool {
    !store.runs.is_empty() || !store.tasks.is_empty()
}

/// 오케스트레이션 메뉴에 보여줄 미해제 워커. 완료된 Run의 워커도 `Released`가 아니면
/// 보인다(OWE-633). `Released`는 숨긴다.
pub(super) fn unreleased_workers(
    store: &krill_proto::orchestration::OrchestrationStore,
) -> Vec<&krill_proto::orchestration::Dispatch> {
    store
        .dispatches
        .iter()
        .filter(|disp| disp.status != krill_proto::orchestration::DispatchStatus::Released)
        .take(3)
        .collect()
}

fn lane(state: AgentState) -> usize {
    match state {
        AgentState::Waiting | AgentState::Blocked => 0,
        AgentState::Working => 1,
        AgentState::Done | AgentState::Exited => 2,
        AgentState::Unknown => 3,
    }
}

/// 숨은 세션에 스트림을 열지 않고 이미 보이는 화면의 세 행만 빌린다.
fn preview(screen: &Screen) -> String {
    screen
        .rows
        .iter()
        .take(3)
        .map(|row| {
            row.iter()
                .take(32)
                .flat_map(|cell| std::iter::once(cell.ch).chain(cell.zerowidth.iter().copied()))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn screen_plain_text(screen: &Screen) -> String {
    screen
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .flat_map(|cell| std::iter::once(cell.ch).chain(cell.zerowidth.iter().copied()))
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Clone, PartialEq, Eq)]
struct SidebarCard {
    host: krill_app::hosts::HostId,
    session: sidebar::Session,
    name: String,
    detail: String,
    unread: bool,
}

type PreviewMap = std::collections::BTreeMap<(krill_app::hosts::HostId, SessionId), String>;

pub(crate) struct Dashboard {
    owner: WeakEntity<Shell>,
    sidebar: Entity<sidebar::Sidebar>,
    cards: Vec<SidebarCard>,
    previews: PreviewMap,
    _changes: Subscription,
    _frames: Option<Subscription>,
    focus: gpui::FocusHandle,
    focus_on_render: bool,
}

impl Dashboard {
    fn new(
        owner: WeakEntity<Shell>,
        sidebar: Entity<sidebar::Sidebar>,
        previews: PreviewMap,
        cx: &mut Context<Self>,
    ) -> Self {
        let cards = Self::collect_sidebar(&sidebar, cx);
        let changes = cx.observe(&sidebar, |this, sidebar, cx| {
            let next = Self::collect_sidebar(&sidebar, cx);
            if this.cards != next {
                this.cards = next;
                cx.notify();
            }
        });
        let frames = owner.upgrade().map(|owner| {
            cx.observe(&owner, |this, owner, cx| {
                let next = Self::collect_previews(owner.read(cx));
                if this.previews != next {
                    this.previews = next;
                    cx.notify();
                }
            })
        });
        let focus = cx.focus_handle();
        Self {
            owner,
            sidebar,
            cards,
            previews,
            _changes: changes,
            _frames: frames,
            focus,
            focus_on_render: true,
        }
    }

    fn collect_sidebar(sidebar: &Entity<sidebar::Sidebar>, cx: &App) -> Vec<SidebarCard> {
        let sidebar = sidebar.read(cx);
        sidebar
            .dashboard_sessions()
            .into_iter()
            .map(|(host, session)| {
                let (name, detail) = sidebar.session_label_on(&host, &session);
                let detail = match &host {
                    krill_app::hosts::HostId::Local => detail,
                    krill_app::hosts::HostId::Remote(id) => {
                        if detail.is_empty() {
                            id.clone()
                        } else {
                            format!("{id} · {detail}")
                        }
                    }
                };
                let unread = host.is_local() && sidebar.is_unread(session.id);
                SidebarCard {
                    host,
                    session,
                    name,
                    detail,
                    unread,
                }
            })
            .collect()
    }

    fn collect_previews(shell: &Shell) -> PreviewMap {
        let mut out = PreviewMap::new();
        for (pane, live) in &shell.live {
            if let Some(session) = live.pane.session() {
                out.insert(
                    (shell.layout.host(*pane).clone(), session),
                    preview(&live.pane.shown()),
                );
            }
        }
        out
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(owner) = self.owner.upgrade() {
            owner.update(cx, |shell, cx| {
                shell.close_dashboard(Some(window), cx);
            });
        }
    }

    fn select_session(
        &mut self,
        host: krill_app::hosts::HostId,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(owner) = self.owner.upgrade() {
            owner.update(cx, |shell, cx| {
                shell.dashboard = None;
                shell.sync_sidebar_visibility(cx);
                shell.close_settings(Some(window), cx);
                shell.open_session_on(host, session, cx);
                window.focus(&shell.focus, cx);
                cx.notify();
            });
        }
    }
}

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_on_render) {
            window.focus(&self.focus, cx);
        }
        self.cards = Self::collect_sidebar(&self.sidebar, cx);
        if let Some(owner) = self.owner.upgrade() {
            self.previews = Self::collect_previews(owner.read(cx));
        }
        let p = ui::palette(cx);
        let all_sessions = &self.cards;
        let previews = &self.previews;
        let viewport = window.viewport_size();
        let card = div()
            .id("dashboard-modal")
            .debug_selector(|| "dashboard-modal".into())
            .key_context("Dashboard")
            .track_focus(&self.focus)
            .occlude()
            .on_action(cx.listener(|this, _: &CloseDashboard, window, cx| this.close(window, cx)))
            .capture_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close(window, cx);
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .w(ui::DASHBOARD_SIZE.width)
            .max_w(viewport.width - ui::SPACE_4 * 2.)
            .h(ui::DASHBOARD_SIZE.height)
            .max_h(viewport.height - ui::SPACE_4 * 2.)
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_SM)
            .child(
                div()
                    .px_4()
                    .py_2p5()
                    .border_b_1()
                    .border_color(p.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(ui::TEXT_LG)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(text::DASHBOARD.get()),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(format!("({})", all_sessions.len())),
                            ),
                    )
                    .child(
                        Button::new("dashboard-close")
                            .debug_selector(|| "dashboard-close".into())
                            .ghost()
                            .xsmall()
                            .icon(gpui_component::IconName::Close)
                            .accessibility_label(krill_app::i18n::catalog::add_project::CLOSE.get())
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
            )
            .child(
                div()
                    .id("dashboard-lanes")
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap_2()
                    .p(ui::SPACE_2)
                    .children(LANES.iter().enumerate().map(|(column, title)| {
                        let cards: Vec<_> = all_sessions
                            .iter()
                            .filter(|item| lane(item.session.status.state) == column)
                            .collect();
                        let lane_state = match column {
                            0 => AgentState::Waiting,
                            1 => AgentState::Working,
                            2 => AgentState::Done,
                            _ => AgentState::Unknown,
                        };
                        div()
                            .id(("dashboard-lane", column))
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .p(ui::SPACE_2)
                            // OWE-876: 레인 배경에는 전경색을 쌍으로 지정한다.
                            .bg(p.sidebar)
                            .text_color(p.sidebar_foreground)
                            .rounded(ui::RADIUS_SM)
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .min_w_0()
                                            .child(
                                                div()
                                                    .size(ui::STATE_DOT)
                                                    .flex_none()
                                                    .rounded_full()
                                                    .bg(ui::agent_state_color(lane_state, cx)),
                                            )
                                            .child(
                                                div()
                                                    .truncate()
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                    .child(title.get()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .rounded_full()
                                            .bg(p.background)
                                            .px_1p5()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .child(format!("{}", cards.len())),
                                    ),
                            )
                            .child(
                                div()
                                    .id(("dashboard-cards", column))
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .when(cards.is_empty(), |d| {
                                        d.child(
                                            div()
                                                .text_color(p.muted_foreground)
                                                .child(text::EMPTY.get()),
                                        )
                                    })
                                    .children(cards.into_iter().map(|item| {
                                        let session = item.session.id;
                                        let host_clone = item.host.clone();
                                        let name = item.name.clone();
                                        let detail = item.detail.clone();
                                        let agent = item.session.status.agent.as_str();
                                        let state = item.session.status.state;
                                        let unread = item.unread;
                                        let screen =
                                            previews.get(&(item.host.clone(), session)).cloned();
                                        let selector = match &item.host {
                                            krill_app::hosts::HostId::Local => {
                                                format!("dashboard-session-{}", session.0)
                                            }
                                            krill_app::hosts::HostId::Remote(id) => {
                                                format!("dashboard-session-{id}-{}", session.0)
                                            }
                                        };
                                        let debug_sel = selector.clone();
                                        let (card_border, card_bg, hover_bg) = match state {
                                            AgentState::Waiting | AgentState::Blocked => (
                                                p.agent_question.opacity(0.4),
                                                p.agent_question.opacity(0.06),
                                                p.agent_question.opacity(0.12),
                                            ),
                                            AgentState::Done => (
                                                p.success.opacity(0.4),
                                                p.success.opacity(0.06),
                                                p.success.opacity(0.12),
                                            ),
                                            _ => (p.border, p.card, p.accent),
                                        };
                                        div()
                                            .id(gpui::SharedString::from(selector))
                                            .debug_selector(move || debug_sel.clone())
                                            .p(ui::SPACE_2)
                                            .rounded(ui::RADIUS_XS)
                                            .border_1()
                                            .border_color(card_border)
                                            .bg(card_bg)
                                            .cursor_pointer()
                                            .hover(move |d| d.bg(hover_bg))
                                            .flex()
                                            .flex_col()
                                            .gap_1p5()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_1p5()
                                                    .child(ui::status::agent_icon(
                                                        agent,
                                                        ui::ICON_SM,
                                                        p.muted_foreground,
                                                    ))
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .truncate()
                                                            .font_weight(if unread {
                                                                gpui::FontWeight::SEMIBOLD
                                                            } else {
                                                                gpui::FontWeight::MEDIUM
                                                            })
                                                            .child(name),
                                                    )
                                                    .child(
                                                        div()
                                                            .size(ui::STATE_DOT)
                                                            .flex_none()
                                                            .rounded_full()
                                                            .bg(ui::agent_state_color(state, cx)),
                                                    ),
                                            )
                                            .when_some(screen.clone(), |d, screen| {
                                                d.child(
                                                    div()
                                                        // OWE-861: 미리보기 유무 회귀 검사 대상.
                                                        .debug_selector(move || {
                                                            format!(
                                                                "dashboard-preview-{}",
                                                                session.0
                                                            )
                                                        })
                                                        .text_size(ui::TEXT_2XS)
                                                        .text_color(p.muted_foreground)
                                                        .child(text::PREVIEW.get()),
                                                )
                                                .child(
                                                    div()
                                                        .font_family(ui::FONT_MONO)
                                                        .text_size(ui::TEXT_2XS)
                                                        .overflow_hidden()
                                                        .child(screen),
                                                )
                                            })
                                            .when(screen.is_none(), |d| {
                                                d.child(
                                                    div()
                                                        // OWE-861: 미리보기 유무 회귀 검사 대상.
                                                        .debug_selector(move || {
                                                            format!(
                                                                "dashboard-no-preview-{}",
                                                                session.0
                                                            )
                                                        })
                                                        .text_size(ui::TEXT_2XS)
                                                        .text_color(p.muted_foreground)
                                                        .child(text::NO_PREVIEW.get()),
                                                )
                                            })
                                            .when(!detail.is_empty(), |d| {
                                                d.child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_1()
                                                        .text_size(ui::TEXT_XS)
                                                        .text_color(p.muted_foreground)
                                                        .child(
                                                            gpui_component::Icon::new(
                                                                gpui_component::IconName::Folder,
                                                            )
                                                            .with_size(px(11.))
                                                            .text_color(p.muted_foreground),
                                                        )
                                                        .child(
                                                            div()
                                                                .flex_1()
                                                                .min_w_0()
                                                                .truncate()
                                                                .child(detail),
                                                        ),
                                                )
                                            })
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.select_session(
                                                    host_clone.clone(),
                                                    session,
                                                    window,
                                                    cx,
                                                );
                                            }))
                                    })),
                            )
                    })),
            );
        ui::modal_backdrop(viewport, card)
    }
}

pub(crate) mod bench;

#[cfg(test)]
#[cfg(unix)]
mod tests;
