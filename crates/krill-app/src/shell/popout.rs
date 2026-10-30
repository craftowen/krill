//! MK-098: 상태 바와 종류별 단일 팝아웃. 창을 닫아도 세션 종료 요청은 보내지 않는다.
//! 잠정 결정 의존: MK-030 gpui 셸. 거부 시 이 모듈과 shell.rs의 연결만 되돌린다.
//! 플로팅 창은 기존 Shell의 입력·IME·재접속을 재사용하고 레이아웃 파일은 쓰지 않는다.

use gpui::{
    App, Bounds, Context, Entity, Subscription, WeakEntity, Window, WindowBounds, WindowHandle,
    WindowOptions, actions, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::{Disableable as _, Sizable as _};
use krill_app::{sidebar, ui};
use krill_proto::{
    SessionId,
    control::AgentState,
    screen::Screen,
    usage::{
        AccountInfo, AccountStatus, AccountsSnapshot, AutoSwitchMetric, AutoSwitchRule,
        ProviderKind, UsageRequest,
    },
};

use super::{Link, Shell, WindowRole};

mod provider;
use provider::{ProviderHostScope, provider_usage_menu};

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

/// 상태바 떼 미터 점 칸 수. 폭은 세션 수와 무관하게 이만큼 예약한다.
const STATUS_SWARM_DOTS: usize = 8;

/// 화면 객체가 그대로면 평문 변환도 생략한다. Weak로 이전 화면을 붙들지 않는다.
#[derive(Default)]
pub(super) struct PortCache {
    screen: std::rc::Weak<Screen>,
    text: String,
    entries: Vec<krill_proto::orchestration::PortEntry>,
}

impl PortCache {
    pub(super) fn matches(&self, screen: &std::rc::Rc<Screen>) -> bool {
        self.screen
            .upgrade()
            .is_some_and(|old| std::rc::Rc::ptr_eq(&old, screen))
    }

    pub(super) fn entries(&self) -> &[krill_proto::orchestration::PortEntry] {
        &self.entries
    }

    pub(super) fn into_data(self) -> (String, Vec<krill_proto::orchestration::PortEntry>) {
        (self.text, self.entries)
    }

    pub(super) fn from_result(
        screen: &std::rc::Rc<Screen>,
        text: String,
        entries: Vec<krill_proto::orchestration::PortEntry>,
    ) -> Self {
        Self {
            screen: std::rc::Rc::downgrade(screen),
            text,
            entries,
        }
    }

    #[cfg(all(test, unix))]
    fn detect(
        &mut self,
        session: u64,
        screen: &std::rc::Rc<Screen>,
    ) -> &[krill_proto::orchestration::PortEntry] {
        if !self.matches(screen) {
            (self.text, self.entries) = detect_ports(
                session,
                screen,
                Some((
                    std::mem::take(&mut self.text),
                    std::mem::take(&mut self.entries),
                )),
            );
            self.screen = std::rc::Rc::downgrade(screen);
        }
        &self.entries
    }
}

/// 평문 변환·URL 감지는 백그라운드에서 실행하고 같은 텍스트면 결과를 재사용한다.
pub(super) fn detect_ports(
    session: u64,
    screen: &Screen,
    previous: Option<(String, Vec<krill_proto::orchestration::PortEntry>)>,
) -> (String, Vec<krill_proto::orchestration::PortEntry>) {
    let text = screen_plain_text(screen);
    if let Some((old, entries)) = previous
        && old == text
    {
        return (old, entries);
    }
    let entries = krill_proto::orchestration::extract_local_urls(session, &text, false);
    (text, entries)
}

/// OWE-856: 세션 수·알림 수(상태 역할·이름 노출). 세션 수 옆에 세션별 상태 떼 미터.
fn usage_status(
    p: &ui::Palette,
    states: &[AgentState],
    needs_you: usize,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let sessions = states.len();
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
                .child(sessions_count)
                .child(
                    ui::status::swarm_meter(states, STATUS_SWARM_DOTS, cx)
                        .debug_selector(|| "status-swarm".into()),
                ),
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
        krill_app::keys::bind("cmd-shift-l", DashboardPopout, None),
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
    let provider = account.provider;
    let metric_rule = rule.clone();
    let metric_weak = weak.clone();
    menu = menu.submenu(
        format!(
            "{}: {}",
            provider_text::PROVIDER_RULE_METRIC.get(),
            rule.metric.label_for(provider)
        ),
        window,
        cx,
        move |mut menu, _, _| {
            for &metric in AutoSwitchMetric::for_provider(provider) {
                let mut next = metric_rule.clone();
                next.metric = metric;
                menu = menu.item(rule_item(
                    metric.label_for(provider),
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

/// 이름과 리셋 안내를 서로 다른 flex 항목으로 둬 좁은 창·UI 줌에서는 줄바꿈한다.
fn render_popover_usage_detail(
    window_label: &str,
    selector: &str,
    win: Option<&krill_proto::usage::Window>,
    p: &ui::Palette,
) -> gpui::Div {
    let display = krill_app::accounts::display::window(
        win,
        i64::try_from(krill_app::accounts::display::now()).unwrap_or(i64::MAX),
    );
    let percent_selector = format!("{selector}-percent");
    let reset_selector = format!("{selector}-reset");
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_x_2()
        .gap_y_0p5()
        .text_size(ui::TEXT_XS)
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .font_family(ui::FONT_MONO)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(p.muted_foreground)
                        .child(window_label.to_owned()),
                )
                .child(
                    div()
                        .debug_selector(move || percent_selector.clone())
                        .font_family(ui::FONT_MONO)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(p.foreground)
                        .child(display.text),
                ),
        )
        .children(krill_app::settings::window_reset_suffix(win).map(|reset| {
            div()
                .debug_selector(move || reset_selector.clone())
                .min_w_0()
                .whitespace_normal()
                .text_color(p.muted_foreground)
                .child(reset)
        }))
}

fn render_popover_usage_row(
    window_label: &str,
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
    let debug_sel = selector.clone();
    let detail = render_popover_usage_detail(window_label, &selector, win, p);

    div()
        .id(gpui::SharedString::from(selector))
        .debug_selector(move || debug_sel.clone())
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(detail)
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
                        .bg(brand_color),
                ),
        )
}

fn expanded_usage_fits(width: gpui::Pixels, rem_size: gpui::Pixels, providers: usize) -> bool {
    let scale = f32::from(rem_size) / 16.;
    width
        >= (ui::theme::STATUS_USAGE_CONTROLS_WIDTH
            + ui::theme::STATUS_USAGE_PROVIDER_WIDTH
                * f32::from(u8::try_from(providers).unwrap_or(u8::MAX)))
            * scale
}

// Compact duration conventions follow Orca's rate-limit-reset-format.ts
// (Copyright (c) 2026 Lovecast Inc., MIT License, .ref/orca/LICENSE).
fn status_reset_duration(left: u64) -> String {
    let minutes = left / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return if minutes.is_multiple_of(60) {
            format!("{hours}h")
        } else {
            format!("{hours}h {}m", minutes % 60)
        };
    }
    if hours.is_multiple_of(24) {
        format!("{}d", hours / 24)
    } else {
        format!("{}d {}h", hours / 24, hours % 24)
    }
}

fn status_window_label(win: Option<&krill_proto::usage::Window>, now: u64) -> String {
    let Some(win) = win else { return "—".into() };
    let reset = win
        .resets_at
        .as_deref()
        .map(|ts| krill_app::settings::classify_reset(ts, now));
    match reset {
        Some(krill_app::settings::ResetDue::Stale) => "— now".into(),
        Some(krill_app::settings::ResetDue::Future(left)) => {
            format!("{}% {}", win.percent.min(100), status_reset_duration(left))
        }
        _ => format!("{}%", win.percent.min(100)),
    }
}

/// 툴팁·접근성용 한 줄: 보이는 기간마다 `라벨 사용률 리셋`. 기간이 없으면 "—".
fn expanded_account_usage(windows: &[StatusWindow<'_>], now: u64) -> String {
    if windows.is_empty() {
        return "—".into();
    }
    windows
        .iter()
        .map(|w| format!("{} {}", w.label, status_window_label(Some(w.window), now)))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 상태바 칩이 그리는 한 기간. `label`은 언어와 무관한 짧은 표기(`5h`·`7d`·`30d`·미터 이름)다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusWindow<'a> {
    label: String,
    window: &'a krill_proto::usage::Window,
}

/// 상태바 칩에 그릴 기간(Orca `StatusBar` 규칙). 보고된 기간만 그리고, 보고되지 않은 기간을
/// 빈 막대·"—"로 채우지 않는다.
///
/// 프로바이더별 특징:
/// - Claude: 5시간·주간 두 창. 모델별 주간 풀은 경고 수준일 때만 [`status_hot_meter`]로 덧붙인다.
/// - Codex: 데몬이 창 길이(300분·10080분)로 5시간·주간을 가른다. Pro처럼 주간 한도만
///   보고하는 요금제는 주간만 보인다.
/// - Antigravity: `/usage`의 5시간·주간. 모델 그룹 풀은 hot meter로 덧붙인다.
/// - `OpenCode` Go: 5시간·주간·월간. 월간은 앞의 두 창이 모두 없을 때만 보인다.
///
/// 계정 사용량에 두 창이 모두 없으면 미터 꼬리에서 5시간·주간 → 월간 → 가장 높은 기간 한도
/// 순으로 고른다(Orca의 월간·버킷 폴백).
fn status_windows<'a>(
    snap: &'a AccountsSnapshot,
    account: &'a AccountInfo,
) -> Vec<StatusWindow<'a>> {
    let mut out: Vec<_> = [
        ("5h", account.usage.five_hour.as_ref()),
        ("7d", account.usage.seven_day.as_ref()),
    ]
    .into_iter()
    .filter_map(|(label, window)| {
        window.map(|window| StatusWindow {
            label: label.into(),
            window,
        })
    })
    .collect();
    if !out.is_empty() {
        return out;
    }
    let Some(meters) = snap.meters_for(&account.id) else {
        return out;
    };
    let windows: Vec<_> = meters
        .meters
        .iter()
        .filter(|m| m.value.is_none())
        .filter_map(|m| Some((m.label.as_str(), m.window.as_ref()?)))
        .collect();
    let pick = |label: &str| windows.iter().find(|(l, _)| *l == label);
    out.extend(
        ["5h", "7d"]
            .into_iter()
            .filter_map(pick)
            .map(|(label, window)| StatusWindow {
                label: (*label).into(),
                window,
            }),
    );
    if out.is_empty()
        && let Some((label, window)) =
            pick("30d").or_else(|| windows.iter().max_by_key(|(_, w)| w.percent))
    {
        out.push(StatusWindow {
            label: (*label).into(),
            window,
        });
    }
    out
}

/// 상태바 칩 상태(Orca `StatusBar` 분기: 불러오는 중 `···`, 조회 불가 `--`, 실패 경고).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PillState {
    /// 기간 수치를 그린다. `warn`이면 마지막 조회가 실패해 값이 오래됐을 수 있어 경고 아이콘을 붙인다.
    Usage { warn: bool },
    /// 새로고침 중이고 아직 값이 없다.
    Loading,
    /// 값도 실패 사유도 없다(모름·오래됨·계정 없음).
    Unavailable,
    /// 값 없이 실패했다. 넓은 상태바에서는 사유를 함께 쓴다.
    Failed(krill_app::i18n::Message),
}

fn pill_state(status: Option<AccountStatus>, has_windows: bool, refreshing: bool) -> PillState {
    use krill_app::i18n::catalog::settings as settings_text;
    if has_windows {
        return PillState::Usage {
            warn: matches!(
                status,
                Some(AccountStatus::NeedsLogin | AccountStatus::Error)
            ),
        };
    }
    if refreshing {
        return PillState::Loading;
    }
    match status {
        Some(AccountStatus::NeedsLogin) => PillState::Failed(settings_text::ACCOUNT_LOGIN_REQUIRED),
        Some(AccountStatus::Error) => PillState::Failed(settings_text::ACCOUNT_STATUS_ERROR),
        Some(AccountStatus::RateLimited) => {
            PillState::Failed(settings_text::ACCOUNT_STATUS_LIMITED)
        }
        _ => PillState::Unavailable,
    }
}

fn status_alert_icon(p: &ui::Palette) -> gpui_component::Icon {
    gpui_component::Icon::default()
        .data(ui::icons::TRIANGLE_ALERT_SVG)
        .with_size(gpui_component::Size::XSmall)
        .text_color(p.muted_foreground)
}

/// 한 기간의 사용률(모르면·리셋 지남이면 `None`)과 리셋까지 남은 시간(5시간은 분까지, 주간은 남은 시간까지).
fn status_window_parts(
    win: Option<&krill_proto::usage::Window>,
    now: u64,
) -> (Option<u8>, Option<String>) {
    let Some(win) = win else {
        return (None, None);
    };
    let reset = win
        .resets_at
        .as_deref()
        .map(|ts| krill_app::settings::classify_reset(ts, now));
    match reset {
        Some(krill_app::settings::ResetDue::Stale) => (None, Some("now".into())),
        Some(krill_app::settings::ResetDue::Future(left)) => (
            Some(win.percent.min(100)),
            Some(status_reset_duration(left)),
        ),
        _ => (Some(win.percent.min(100)), None),
    }
}

/// 상태바 한 기간 칸: 막대 + 사용률 + 리셋. 모르면 빈 막대와 "—"(0%로 꾸미지 않음, OWE-703).
fn status_window_segment(
    win: Option<&krill_proto::usage::Window>,
    now: u64,
    short: bool,
    p: &ui::Palette,
) -> gpui::Div {
    let (pct, reset) = status_window_parts(win, now);
    let bar = if short {
        ui::SPACE_4
    } else {
        ui::SPACE_4 + ui::SPACE_2
    };
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(
            div()
                .w(bar)
                .h(ui::SPACE_1)
                .rounded_full()
                .overflow_hidden()
                .bg(p.muted)
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(f32::from(pct.unwrap_or(0)) / 100.))
                        .bg(pct.map_or(p.muted, |_| p.muted_foreground)),
                ),
        )
        .child(
            div()
                .text_color(pct.map_or(p.muted_foreground, |_| p.foreground))
                .child(pct.map_or_else(|| "—".to_owned(), |pct| format!("{pct}%"))),
        )
        .children(reset.map(|r| div().text_color(p.muted_foreground).child(r)))
}

/// 칩에 보이는 기간 밖 한도(모델별 풀 등)가 경고 수준이고 보이는 기간보다 높으면 그 이름과 사용률.
fn status_hot_meter(
    snap: &AccountsSnapshot,
    account: &AccountInfo,
    now: u64,
) -> Option<(String, u8)> {
    let now = i64::try_from(now).unwrap_or(i64::MAX);
    let shown = status_windows(snap, account)
        .into_iter()
        .filter_map(|w| krill_app::accounts::display::window(Some(w.window), now).percent)
        .max()
        .unwrap_or(0);
    snap.meters_for(&account.id)?
        .meters
        .iter()
        .filter_map(|m| {
            let pct = krill_app::accounts::display::window(m.window.as_ref(), now).percent?;
            (pct >= krill_proto::usage::WARN_PERCENT && pct > shown).then(|| (m.label.clone(), pct))
        })
        .max_by_key(|(_, pct)| *pct)
}

fn has_provider_pill(
    snap: &AccountsSnapshot,
    provider: ProviderKind,
    remote: bool,
    cx: &App,
) -> bool {
    snap.accounts.iter().any(|a| {
        a.provider == provider
            && !krill_app::accounts::orca_managed(a)
            && (provider != ProviderKind::OpenCode
                || !a.host_default
                || !a.usage.is_empty()
                || snap.meters_for(&a.id).is_some_and(|m| !m.meters.is_empty()))
    }) && (remote || krill_app::accounts::installed::is_installed(cx, provider))
}

/// Codex banked reset 및 Claude 주간 세션·프로모션 리셋 크레딧 수. 미터가 아직 없으면 `None`.
pub(in crate::shell) fn reset_credit_count(
    snap: &AccountsSnapshot,
    account: &AccountInfo,
) -> Option<u64> {
    snap.meters_for(&account.id)?
        .meters
        .iter()
        .find(|m| m.key == krill_proto::usage::RESET_CREDITS_METER_KEY)?
        .value
        .as_deref()?
        .parse::<u64>()
        .ok()
}

/// 상태바 사용량 팝업의 "사용량 초기화" 행(Codex·Claude 2단계 확인).
/// 1단계 행을 누르면 무장하고 메뉴가 닫히고, 다시 열면 안내·실행·취소 행이 보인다.
/// 미터가 아직 조회되지 않았더라도(`None`) 소진 시점에 실시간 프로브·차감할 수 있게 활성화한다.
fn append_reset_credit_rows(
    mut menu: PopupMenu,
    snap: &AccountsSnapshot,
    active: Option<&AccountInfo>,
    provider: ProviderKind,
    reset_armed: Option<&(ProviderKind, String)>,
    weak: &WeakEntity<Shell>,
) -> PopupMenu {
    // Codex banked reset와 Claude 주간 세션 리셋·프로모션 grant가 같은 행을 쓴다.
    if provider != ProviderKind::Codex && provider != ProviderKind::Claude {
        return menu;
    }
    let Some(account) = active.filter(|a| !krill_app::accounts::orca_managed(a)) else {
        return menu;
    };
    let credits = reset_credit_count(snap, account);
    let account_id = account.id.clone();
    let armed = reset_armed.is_some_and(|(p, id)| *p == provider && *id == account_id);
    if armed {
        let (title, _) = krill_app::settings::account_heading(account);
        let info = provider_text::PROVIDER_RESET_USAGE_INFO.format(&[("account", &title)]);
        menu = menu.item(
            PopupMenuItem::element(move |_, cx| {
                let p = ui::palette(cx);
                div()
                    .id("status-provider-reset-usage-info")
                    .debug_selector(|| "status-provider-reset-usage-info".into())
                    .w(px(264.))
                    .py_0p5()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(info.clone())
            })
            .disabled(true),
        );
        let confirm_weak = weak.clone();
        let confirm_id = account_id.clone();
        menu = menu.item(
            PopupMenuItem::element(move |_, cx| {
                let p = ui::palette(cx);
                div()
                    .id("status-provider-reset-usage-confirm")
                    .debug_selector(|| "status-provider-reset-usage-confirm".into())
                    .w(px(264.))
                    .py_0p5()
                    .text_size(ui::TEXT_SM)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(p.destructive)
                    .child(provider_text::PROVIDER_RESET_USAGE_CONFIRM.get())
            })
            .on_click(move |_, _, cx| {
                confirm_weak
                    .update(cx, |shell, cx| {
                        shell.consume_reset_credit(provider, confirm_id.clone(), cx);
                    })
                    .ok();
            }),
        );
        let cancel_weak = weak.clone();
        menu = menu.item(
            PopupMenuItem::element(|_, cx| {
                let p = ui::palette(cx);
                div()
                    .id("status-provider-reset-usage-cancel")
                    .debug_selector(|| "status-provider-reset-usage-cancel".into())
                    .w(px(264.))
                    .py_0p5()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(provider_text::PROVIDER_RESET_USAGE_CANCEL.get())
            })
            .on_click(move |_, _, cx| {
                cancel_weak
                    .update(cx, |shell, _| {
                        shell.reset_armed = None;
                    })
                    .ok();
            }),
        );
    } else if credits == Some(0) {
        menu = menu.item(
            PopupMenuItem::element(|_, cx| {
                let p = ui::palette(cx);
                div()
                    .id("status-provider-reset-usage")
                    .debug_selector(|| "status-provider-reset-usage".into())
                    .w(px(264.))
                    .py_0p5()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(provider_text::PROVIDER_RESET_USAGE_NONE.get())
            })
            .disabled(true),
        );
    } else {
        let arm_weak = weak.clone();
        menu = menu.item(
            PopupMenuItem::element(move |_, cx| {
                let p = ui::palette(cx);
                div()
                    .id("status-provider-reset-usage")
                    .debug_selector(|| "status-provider-reset-usage".into())
                    .w(px(264.))
                    .py_0p5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.foreground)
                    .child(provider_text::PROVIDER_RESET_USAGE.get())
                    .children(credits.filter(|c| *c > 0).map(|c| {
                        div()
                            .font_family(ui::FONT_MONO)
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(c.to_string())
                    }))
            })
            .on_click(move |_, _, cx| {
                arm_weak
                    .update(cx, |shell, _| {
                        shell.reset_armed = Some((provider, account_id.clone()));
                    })
                    .ok();
            }),
        );
    }
    menu
}

/// 상태바 메뉴는 바 위로 열고(Orca처럼 하단바를 가리지 않음), 창 위쪽 여백까지만
/// 자라며 넘치면 스크롤한다. 앵커가 넘친 메뉴를 창 안으로 밀어 올리며 바를 덮지 않게 한다.
fn fit_above_status_bar(menu: PopupMenu, window: &Window) -> PopupMenu {
    let room = window.viewport_size().height - ui::STATUS_BAR_HEIGHT - ui::SPACE_4 * 2.;
    let width = (window.viewport_size().width - ui::SPACE_4 * 2.).max(ui::SPACE_4);
    menu.max_w(width.min(px(500.)))
        .max_h(room.max(ui::SPACE_4))
        .scrollable(true)
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
    /// 현재 실행 중인 호스트·세션에 속하는 캐시 URL만 메뉴에 표시한다.
    pub(super) fn detected_ports(
        &self,
    ) -> Vec<(
        krill_app::hosts::HostId,
        krill_app::layout::PaneId,
        krill_proto::orchestration::PortEntry,
    )> {
        let mut detected_ports = Vec::new();
        for ((host, sid), cache) in self.ports_cache.borrow().iter() {
            let Some((&pane, _)) = self.live.iter().find(|(pane, live)| {
                self.layout.host(**pane) == host
                    && live.pane.session().is_some_and(|session| session.0 == *sid)
                    && cache.matches(&live.pane.shown())
            }) else {
                continue;
            };
            for entry in cache.entries() {
                detected_ports.push((host.clone(), pane, entry.clone()));
            }
        }
        detected_ports.sort_by(|a, b| {
            (&a.0, a.2.session_id, &a.2.url).cmp(&(&b.0, b.2.session_id, &b.2.url))
        });
        detected_ports
            .dedup_by(|a, b| a.0 == b.0 && a.2.session_id == b.2.session_id && a.2.url == b.2.url);
        detected_ports
    }

    /// OWE-702: 데몬 연결 중에만 [`ACCOUNTS_POLL`]마다 `ListAccounts`를 다시 받는다. 링크가 끊기면 끝난다.
    /// 데몬에만 묻고(로컬 폴백 없음) 일시 오류는 배너 없이 다음 바퀴에 다시 시도한다.
    pub(super) fn poll_accounts(cx: &mut Context<Self>) -> gpui::Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ACCOUNTS_POLL).await;
                let Ok((local_paths, remote_targets)) = this.read_with(cx, |s, _| {
                    let local_paths = s.paths.clone().filter(|_| s.link == Link::Up);
                    let remote_targets: Vec<_> = s
                        .host_targets
                        .iter()
                        .map(|(h, t)| (h.clone(), t.clone()))
                        .collect();
                    (local_paths, remote_targets)
                }) else {
                    return;
                };
                let Some(paths) = local_paths else {
                    return;
                };
                let (local_res, remote_res) = cx
                    .background_executor()
                    .spawn(async move {
                        let local = krill_app::accounts::request_target_with_features(
                            &krill_app::hosts::HostTarget::local(paths),
                            &UsageRequest::ListAccounts,
                        );
                        let mut remotes = Vec::with_capacity(remote_targets.len());
                        for (host, target) in remote_targets {
                            if let Ok(snap) = krill_app::accounts::request_target_with_features(
                                &target,
                                &UsageRequest::ListAccounts,
                            ) {
                                remotes.push((host, snap));
                            }
                        }
                        (local, remotes)
                    })
                    .await;
                let keep_going = this
                    .update(cx, |s, cx| {
                        if let Ok((snap, features)) = local_res {
                            s.sidebar.update(cx, |sidebar, cx| {
                                sidebar.set_session_account_features(
                                    krill_app::hosts::HostId::Local,
                                    features,
                                    cx,
                                );
                            });
                            s.apply_accounts_snapshot(snap, cx);
                        }
                        for (host, (snap, features)) in remote_res {
                            s.sidebar.update(cx, |sidebar, cx| {
                                sidebar.set_session_account_features(host.clone(), features, cx);
                            });
                            s.apply_accounts_snapshot_for_host(host, snap, cx);
                        }
                    })
                    .is_ok();
                if !keep_going {
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

    /// OWE-851: 대시보드 카드·리소스 행 클릭 → 메인 창 해당 세션으로 포커스하고 메인 창을 앞으로.
    #[expect(dead_code, reason = "dashboard window navigation helper")]
    pub(super) fn focus_session_window(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.open_session(session, cx);
        if let Some(win) = self.window {
            let focus = self.focus.clone();
            win.update(cx, |_, window, cx| {
                window.activate_window();
                window.focus(&focus, cx);
            })
            .ok();
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
                    krill_proto::orchestration::OrchestrationStore::reply_via_daemon_or_disk(
                        &dir,
                        &question_id,
                        "coordinator",
                        answer,
                    )
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
                    krill_proto::orchestration::OrchestrationStore::resolve_gate_via_daemon_or_disk(
                        &dir,
                        &gate_id,
                        decision,
                        Some(note.to_owned()),
                        "coordinator",
                    )
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

    pub(super) fn render_status_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = ui::palette(cx);
        let (color, daemon_tooltip) = match &self.link {
            Link::Up => (p.success, text::DAEMON_UP.get().to_owned()),
            Link::Connecting => (p.warning, text::DAEMON_CONNECTING.get().to_owned()),
            // 세션은 krill-holder가 쥐고 있어 끊김이 아니라 보존 중이다(STYLEGUIDE §3.3).
            Link::Down => (p.preserve, text::DAEMON_CONNECTING.get().to_owned()),
            Link::Failed(err) => (
                p.destructive,
                if err.is_empty() {
                    text::DAEMON_FAILED.get().to_owned()
                } else {
                    format!("{}: {err}", text::DAEMON_FAILED.get())
                },
            ),
        };
        let (needs_you, states) = self.sidebar.read(cx).status_summary();
        // OWE-851: 메뉴가 열려 있는 동안은 상태바 호버 툴팁을 숨긴다.
        let tooltips_on = self.status_menus_open == 0;
        let session = self.layout.focused().and_then(|p| self.layout.session(p));
        let account_host = self.active_account_host();
        let snap = self.accounts_for_host(&account_host);
        let account_hosts: Vec<(
            krill_app::hosts::HostId,
            String,
            std::sync::Arc<AccountsSnapshot>,
        )> = {
            let mut list: Vec<_> = self
                .execution_locations(cx)
                .into_iter()
                .map(|(h, label)| {
                    let s = self.accounts_for_host(&h);
                    (h, label, s)
                })
                .collect();
            for (h, s) in &self.remote_accounts {
                if !list.iter().any(|(existing, _, _)| existing == h) {
                    let label = h.remote_id().unwrap_or_else(|| h.as_key()).to_owned();
                    list.push((h.clone(), label, s.clone()));
                }
            }
            list
        };
        let status_providers = [
            ProviderKind::Claude,
            ProviderKind::Codex,
            ProviderKind::Antigravity,
            ProviderKind::OpenCode,
        ];
        let status_providers: Vec<_> = status_providers
            .into_iter()
            .filter(|provider| {
                krill_app::settings::status_provider_visible(*provider, cx)
                    && (has_provider_pill(&snap, *provider, account_host.is_remote(), cx)
                        || account_hosts.iter().any(|(h, _, s)| {
                            h.is_remote() && has_provider_pill(s, *provider, true, cx)
                        }))
            })
            .collect();
        let expanded = expanded_usage_fits(
            window.viewport_size().width,
            window.rem_size(),
            status_providers.len(),
        );
        let mut provider_pills = div()
            .id("status-provider-usage")
            .debug_selector(|| "status-provider-usage".into())
            .flex()
            .items_center()
            .gap_2p5();
        // OWE-652: 계정이 하나도 없는 제공자는 알약을 그리지 않는다(Orca처럼 간결하게).
        // 활성 계정의 모름·만료를 다른 계정 수치로 덮지 않는다(OWE-703).
        for provider in status_providers {
            let active = snap
                .accounts
                .iter()
                .filter(|a| !krill_app::accounts::orca_managed(a))
                .find(|a| a.provider == provider && a.active)
                .or_else(|| {
                    snap.accounts
                        .iter()
                        .find(|a| a.provider == provider && !krill_app::accounts::orca_managed(a))
                });
            let now = krill_app::accounts::display::now();
            let percent = active.and_then(|a| {
                let now = i64::try_from(now).unwrap_or(i64::MAX);
                snap.meters_for(&a.id).map_or_else(
                    || AutoSwitchMetric::Max.effective_percent(&a.usage, now),
                    |m| m.effective_max(now),
                )
            });
            let status = active.map(|a| {
                krill_app::accounts::display::account(
                    a,
                    snap.statuses.iter().find(|s| s.account_id == a.id),
                    now,
                )
            });
            let windows = active.map_or_else(Vec::new, |a| status_windows(&snap, a));
            let state = pill_state(
                status.as_ref().map(|s| s.status),
                !windows.is_empty(),
                self.usage_refresh.is_some(),
            );
            let expanded_label = expanded_account_usage(&windows, now);
            let hot = active.and_then(|a| status_hot_meter(&snap, a, now));
            let color = if percent.is_some() {
                p.foreground
            } else {
                p.muted_foreground
            };
            let selector = format!("status-provider-pill-{}", provider.as_str());
            let debug_selector = selector.clone();
            let provider_snap = snap.clone();
            let weak = cx.weak_entity();
            let description = status.map_or_else(
                || provider.display_name().to_owned(),
                |s| {
                    format!(
                        "{} · {} · {} · {}",
                        provider.display_name(),
                        expanded_label,
                        s.label,
                        s.updated
                    )
                },
            );
            let pill_btn = Button::new(gpui::SharedString::from(selector.clone()))
                .font_family(ui::FONT_MONO)
                .text_color(color)
                .ghost()
                .xsmall()
                .h(ui::STATUS_BAR_BUTTON_HEIGHT)
                .debug_selector(move || debug_selector.clone());
            let detail_selector = format!(
                "status-provider-{}-{}",
                if expanded { "expanded" } else { "compact" },
                provider.as_str()
            );
            // 프로바이더마다 보고된 기간의 막대와 사용률·리셋(하단바 B안). 좁으면 막대를
            // 줄이고 리셋을 가장 큰 단위 하나로 접는다. 값이 없으면 Orca처럼 상태 표시만 둔다.
            let short = !expanded;
            let detail = div()
                .debug_selector(move || detail_selector.clone())
                .flex()
                .items_center()
                .gap_2();
            let detail = match state {
                PillState::Usage { warn } => detail
                    .children(
                        windows
                            .iter()
                            .map(|w| status_window_segment(Some(w.window), now, short, p)),
                    )
                    .children(hot.map(|(label, pct)| {
                        div()
                            .text_color(p.foreground)
                            .child(format!("{label} {pct}%"))
                    }))
                    .when(warn, |d| d.child(status_alert_icon(p))),
                PillState::Loading => {
                    detail.child(div().text_color(p.muted_foreground).child("···"))
                }
                PillState::Unavailable => {
                    detail.child(div().text_color(p.muted_foreground).child("--"))
                }
                PillState::Failed(reason) => {
                    detail.child(status_alert_icon(p)).when(expanded, |d| {
                        d.child(div().text_color(p.muted_foreground).child(reason.get()))
                    })
                }
            };
            let pill_btn = pill_btn
                .accessibility_label(description.clone())
                .child(ui::icons::provider_icon(provider, ui::ICON_SM, color))
                .child(detail);
            let reset_armed = self.reset_armed.clone();
            let menu_account_host = account_host.clone();
            let menu_account_hosts = account_hosts.clone();
            provider_pills = provider_pills.child(
                ui::button_with_tooltip_when(tooltips_on, pill_btn, selector, description)
                    .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, window, cx| {
                        provider_usage_menu(
                            fit_above_status_bar(menu, window),
                            &ProviderHostScope {
                                snap: &provider_snap,
                                active_host: &menu_account_host,
                                account_hosts: &menu_account_hosts,
                            },
                            provider,
                            weak.clone(),
                            window,
                            cx,
                            reset_armed.as_ref(),
                        )
                    })
                    .on_open_change(track_menu_open(cx.weak_entity())),
            );
        }
        let weak = cx.weak_entity();
        let refresh_button = ui::button_with_tooltip_when(
            tooltips_on,
            Button::new("status-usage-refresh")
                .icon(gpui_component::IconName::RefreshCw)
                .loading(self.usage_refresh.is_some())
                .ghost()
                .xsmall()
                .h(ui::STATUS_BAR_BUTTON_HEIGHT)
                .debug_selector(|| "status-usage-refresh".into()),
            "status-usage-refresh",
            provider_text::PROVIDER_REFRESH_USAGE.get(),
        )
        .on_click(cx.listener(|shell, _, _, cx| {
            shell.refresh_usage_manually(cx);
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
                .h(ui::STATUS_BAR_BUTTON_HEIGHT)
                .debug_selector(|| "status-ssh-switcher".into()),
            "status-ssh-switcher",
            ssh_tooltip,
        )
        .dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |mut menu, window, _| {
            menu = fit_above_status_bar(menu, window)
                .min_w(
                    px(240.)
                        .min((window.viewport_size().width - ui::SPACE_4 * 2.).max(ui::SPACE_4)),
                )
                .label(text::SSH_MENU_TITLE.get());
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
                            krill_app::settings::AdminTab::Machines,
                            window,
                            cx,
                        );
                        open_weak
                            .update(cx, |shell, cx| {
                                if let Some(settings) = &shell.settings {
                                    settings.update(cx, |view, cx| {
                                        view.show_machine_add(
                                            krill_app::machines::AddMethod::Ssh,
                                            cx,
                                        );
                                    });
                                }
                            })
                            .ok();
                    },
                ))
        })
        .on_open_change(track_menu_open(cx.weak_entity()));
        let state_dir = self.state_dir(cx);
        let orch_store = self.orch_snapshot.clone();
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
                .h(ui::STATUS_BAR_BUTTON_HEIGHT)
                .debug_selector(|| "status-orchestration-menu".into()),
            "status-orchestration-menu",
            orch_tooltip,
        )
        .dropdown_menu_with_anchor(gpui::Anchor::BottomRight, {
                let weak = cx.weak_entity();
                let state_dir = state_dir.clone();
                move |mut menu, window, _| {
                    menu = fit_above_status_bar(menu, window)
                        .min_w(px(300.).min((window.viewport_size().width - ui::SPACE_4 * 2.).max(ui::SPACE_4)))
                        .label(text::ORCH_MENU_TITLE.get());
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
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().flex_1().min_w_0().text_size(ui::TEXT_SM).child(summary.clone()));
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
                            let can_release = dispatch.validate_release().is_ok();
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
                                    .w_full()
                                    .min_w_0()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().flex_1().min_w_0().text_size(ui::TEXT_SM).child(label.clone()))
                                    .child(
                                        Button::new(gpui::ElementId::Name(button_id.clone().into()))
                                            .label(text::WORKER_RELEASE.get())
                                            .disabled(!can_release)
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
                            "[KRILL SESSION HANDOFF]\nSource session: s{sid}\nTarget agent: codex\n--- Recent terminal context ---\n{text_body}\n--- End context ---\n"
                        );
                        let body_claude = format!(
                            "[KRILL SESSION HANDOFF]\nSource session: s{sid}\nTarget agent: claude\n--- Recent terminal context ---\n{text_body}\n--- End context ---\n"
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
        let detected_ports = self.detected_ports();
        let ports_weak = cx.weak_entity();
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
                .h(ui::STATUS_BAR_BUTTON_HEIGHT)
                .debug_selector(|| "status-ports-menu".into()),
            "status-ports-menu",
            ports_tooltip,
        )
        .dropdown_menu_with_anchor(gpui::Anchor::BottomRight, move |mut menu, window, _| {
            menu = fit_above_status_bar(menu, window)
                .min_w(
                    px(240.)
                        .min((window.viewport_size().width - ui::SPACE_4 * 2.).max(ui::SPACE_4)),
                )
                .label(text::PORTS_MENU_TITLE.get());
            if detected_ports.is_empty() {
                menu = menu.item(PopupMenuItem::new(text::PORTS_EMPTY.get()).disabled(true));
            } else {
                for (host, pane, entry) in &detected_ports {
                    let label = format!(
                        "{host} · s{} · :{} → {}",
                        entry.session_id, entry.port, entry.url
                    );
                    let url = entry.url.clone();
                    let (host, pane, session, weak) =
                        (host.clone(), *pane, entry.session_id, ports_weak.clone());
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        weak.update(cx, |this, cx| {
                            // 메뉴가 열린 뒤 다른 소유자로 재사용된 칸으로 URL을 열지 않는다.
                            if this.layout.host(pane) == &host
                                && this.live.get(&pane).is_some_and(|live| {
                                    live.pane.session().is_some_and(|sid| sid.0 == session)
                                })
                            {
                                this.open_terminal_url(pane, &url, cx);
                            }
                        })
                        .ok();
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
                                            .xsmall()
                                            .h(ui::STATUS_BAR_BUTTON_HEIGHT),
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
                                        .xsmall()
                                        .h(ui::STATUS_BAR_BUTTON_HEIGHT),
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
                    .child(usage_status(p, &states, needs_you, cx))
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
                                    .h(ui::STATUS_BAR_BUTTON_HEIGHT)
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
                                .h(ui::STATUS_BAR_BUTTON_HEIGHT)
                                .debug_selector(|| "dashboard-popout".into()),
                            "dashboard-popout",
                            dashboard_tip,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.dashboard(cx)))
                    })
                    .child(self.history.update(cx, |h, cx| h.render_toggle(cx)))
                    .child(self.resources.clone()),
            )
            // 하-4: 상태바 우클릭 메뉴. 상태바에 이미 있는 동작과 설정 열기만 담는다.
            .context_menu(move |mut m, _, cx| {
                let weak = weak.clone();
                m = m
                    .action_context(focus.clone())
                    .item(
                        PopupMenuItem::element(|_, _| {
                            div()
                                .debug_selector(|| "status-bar-menu-refresh".into())
                                .child(provider_text::PROVIDER_REFRESH_USAGE.get())
                        })
                        .on_click(move |_, _, cx| {
                            weak.update(cx, |shell, cx| {
                                shell.refresh_usage_manually(cx);
                            })
                            .ok();
                        }),
                    )
                    .separator()
                    .label(provider_text::PROVIDER_STATUS_BAR_SECTION.get());
                for provider in ProviderKind::ALL {
                    let visible = krill_app::settings::status_provider_visible(provider, cx);
                    m = m.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .debug_selector(move || {
                                    format!("status-bar-menu-provider-{}", provider.as_str())
                                })
                                .child(provider.display_name())
                        })
                        .checked(visible)
                        .on_click(move |_, _, cx| {
                            krill_app::settings::set_status_provider_visible(
                                provider, !visible, cx,
                            );
                        }),
                    );
                }
                m.separator()
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
        .filter(|disp| !disp.is_released())
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
    read_order: Option<u64>,
}

impl SidebarCard {
    fn agent(&self) -> &str {
        ui::status::infer_agent_from_status(
            &self.session.status,
            self.session.status.title.as_deref().unwrap_or(&self.name),
        )
    }
}

type PreviewMap = std::collections::BTreeMap<(krill_app::hosts::HostId, SessionId), String>;

/// Keep animation ticks local to the glyph instead of notifying the whole dashboard.
struct DashboardStatus {
    state: AgentState,
    unread: bool,
}

impl Render for DashboardStatus {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ui::status::status_glyph(self.state, self.unread, ui::status::Size::Sm, cx)
    }
}

pub(crate) struct Dashboard {
    owner: WeakEntity<Shell>,
    sidebar: Entity<sidebar::Sidebar>,
    language: krill_app::i18n::Language,
    cards: Vec<SidebarCard>,
    lanes: [Vec<usize>; 4],
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
                this.lanes = Self::project_lanes(&next);
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
            language: krill_app::i18n::language(),
            lanes: Self::project_lanes(&cards),
            cards,
            previews,
            _changes: changes,
            _frames: frames,
            focus,
            focus_on_render: true,
        }
    }

    fn project_lanes(cards: &[SidebarCard]) -> [Vec<usize>; 4] {
        let mut lanes = std::array::from_fn(|_| Vec::new());
        for (index, card) in cards.iter().enumerate() {
            lanes[lane(card.session.status.state)].push(index);
        }
        // 완료(Done | Exited) 레인: 안 읽은 완료 카드를 상단에(최신 완료순),
        // 클릭해 읽음 처리된 카드를 하단에(읽은 순서대로) 배치한다.
        lanes[2].sort_by(|&a, &b| {
            let ca = &cards[a];
            let cb = &cards[b];
            match (ca.read_order, cb.read_order) {
                (None, Some(_)) => std::cmp::Ordering::Less,
                (Some(_), None) => std::cmp::Ordering::Greater,
                (None, None) => cb
                    .session
                    .status
                    .since_unix_ns
                    .cmp(&ca.session.status.since_unix_ns)
                    .then_with(|| a.cmp(&b)),
                (Some(sa), Some(sb)) => sa.cmp(&sb).then_with(|| a.cmp(&b)),
            }
        });
        lanes
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
                let read_order = sidebar.dashboard_read_order(&host, &session);
                let unread = sidebar.is_dashboard_unread(&host, &session);
                SidebarCard {
                    host,
                    session,
                    name,
                    detail,
                    unread,
                    read_order,
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
        self.sidebar.update(cx, |s, cx| {
            s.mark_session_read(&host, session, cx);
        });
        self.cards = Self::collect_sidebar(&self.sidebar, cx);
        self.lanes = Self::project_lanes(&self.cards);
        cx.notify();
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
        if std::mem::take(&mut self.focus_on_render) || !self.focus.contains_focused(window, cx) {
            window.focus(&self.focus, cx);
        }
        if self.language != krill_app::i18n::language() {
            self.language = krill_app::i18n::language();
            self.cards = Self::collect_sidebar(&self.sidebar, cx);
            self.lanes = Self::project_lanes(&self.cards);
        }
        let p = ui::palette(cx);
        let all_sessions = &self.cards;
        let previews = &self.previews;
        let viewport = window.viewport_size();
        let card = div()
            .id("dashboard-modal")
            .debug_selector(|| "dashboard-modal".into())
            .role(gpui::Role::Dialog)
            .aria_label(text::DASHBOARD.get())
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
                    .debug_selector(|| "dashboard-lanes".into())
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_x_scroll()
                    .flex()
                    .gap_2()
                    .p(ui::SPACE_2)
                    .children(LANES.iter().enumerate().map(|(column, title)| {
                        let cards = &self.lanes[column];
                        let lane_state = match column {
                            0 => AgentState::Waiting,
                            1 => AgentState::Working,
                            2 => AgentState::Done,
                            _ => AgentState::Unknown,
                        };
                        div()
                            .id(("dashboard-lane", column))
                            .debug_selector(move || format!("dashboard-lane-{column}"))
                            .flex_1()
                            .min_w(ui::DASHBOARD_LANE_MIN_WIDTH)
                            .min_h_0()
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
                                    .flex_none()
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
                                    .debug_selector(move || format!("dashboard-cards-{column}"))
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
                                    .children(cards.iter().map(|index| {
                                        let item = &all_sessions[*index];
                                        let session = item.session.id;
                                        let host_clone = item.host.clone();
                                        let name = item.name.clone();
                                        let detail = item.detail.clone();
                                        let agent = item.agent();
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
                                        let is_completed =
                                            matches!(state, AgentState::Done | AgentState::Exited);
                                        let is_read_completed = is_completed && !unread;
                                        let (card_border, card_bg, hover_bg) = match state {
                                            AgentState::Waiting | AgentState::Blocked => (
                                                p.agent_question.opacity(0.4),
                                                p.agent_question.opacity(0.06),
                                                p.agent_question.opacity(0.12),
                                            ),
                                            AgentState::Done if unread => (
                                                p.success.opacity(0.4),
                                                p.success.opacity(0.06),
                                                p.success.opacity(0.12),
                                            ),
                                            AgentState::Done | AgentState::Exited
                                                if is_read_completed =>
                                            {
                                                (
                                                    p.border.opacity(0.5),
                                                    p.card.opacity(0.4),
                                                    p.accent.opacity(0.5),
                                                )
                                            }
                                            _ => (p.border, p.card, p.accent),
                                        };
                                        let title_color = if is_read_completed {
                                            p.muted_foreground
                                        } else {
                                            p.foreground
                                        };
                                        let title_weight = if unread {
                                            gpui::FontWeight::SEMIBOLD
                                        } else if is_read_completed {
                                            gpui::FontWeight::NORMAL
                                        } else {
                                            gpui::FontWeight::MEDIUM
                                        };
                                        div()
                                            .id(gpui::SharedString::from(selector))
                                            .debug_selector(move || debug_sel.clone())
                                            .role(gpui::Role::Button)
                                            .aria_label(format!("{name} · {detail}"))
                                            .tab_index(0)
                                            .flex_none()
                                            .p(ui::SPACE_2)
                                            .rounded(ui::RADIUS_XS)
                                            .border_1()
                                            .border_color(card_border)
                                            .bg(card_bg)
                                            .when(is_read_completed, |d| d.opacity(0.7))
                                            .cursor_pointer()
                                            .hover(move |d| d.bg(hover_bg))
                                            .focus_visible(|d| d.border_color(p.ring))
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
                                                        ui::ICON_MD,
                                                        p.muted_foreground,
                                                    ))
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .min_w_0()
                                                            .truncate()
                                                            .text_color(title_color)
                                                            .font_weight(title_weight)
                                                            .child(name),
                                                    )
                                                    .child(cx.new(|_| DashboardStatus {
                                                        state,
                                                        unread,
                                                    })),
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
                                                            .with_size(ui::ICON_XS)
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
        ui::modal_backdrop(viewport, ui::modal_focus_scope(card, &self.focus))
    }
}

pub(crate) mod bench;

#[cfg(test)]
#[cfg(unix)]
mod tests;
