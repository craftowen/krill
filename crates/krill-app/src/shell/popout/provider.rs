//! 상태바 계정 비교표(시안 D: 로컬/원격 상단 탭 분리 + 좌우 마스터-디테일 인라인 상세 뷰).
//! 이메일 묶음은 표시만 합치며 프로필·전환 대상은 보존한다.

use super::*;
use krill_app::i18n::catalog::settings as account_text;
use krill_proto::usage::{AccountStatus, Window as UsageWindow};

mod options;
use options::{advanced_notes, append_policy_menu};

#[derive(Default, Clone)]
struct ProviderPopupState {
    selected_account: Option<(ProviderKind, String)>,
    remote_tab: Option<(ProviderKind, bool)>,
}

impl gpui::Global for ProviderPopupState {}

fn selected_account_for(
    cx: &App,
    provider: ProviderKind,
    groups: &[AccountGroup],
) -> Option<AccountGroup> {
    let wanted = cx
        .try_global::<ProviderPopupState>()
        .and_then(|s| s.selected_account.as_ref())
        .filter(|(p, _)| *p == provider)
        .map(|(_, id)| id.as_str());
    if let Some(id) = wanted
        && let Some(found) = groups
            .iter()
            .find(|g| g.selected().is_some_and(|a| a.id == id))
    {
        return Some(found.clone());
    }
    groups
        .iter()
        .find(|g| g.selected().is_some_and(|a| a.active))
        .or_else(|| groups.first())
        .cloned()
}

fn is_remote_tab_for(cx: &App, provider: ProviderKind, has_remote: bool) -> bool {
    if !has_remote {
        return false;
    }
    cx.try_global::<ProviderPopupState>()
        .and_then(|s| s.remote_tab)
        .filter(|(p, _)| *p == provider)
        .is_some_and(|(_, remote)| remote)
}

fn set_selected_account(provider: ProviderKind, account_id: String, cx: &mut App) -> bool {
    let mut state = cx
        .try_global::<ProviderPopupState>()
        .cloned()
        .unwrap_or_default();
    let changed = state
        .selected_account
        .as_ref()
        .is_none_or(|(p, id)| *p != provider || *id != account_id);
    if changed {
        state.selected_account = Some((provider, account_id));
        cx.set_global(state);
    }
    changed
}

fn set_remote_tab(provider: ProviderKind, remote: bool, cx: &mut App) {
    let mut state = cx
        .try_global::<ProviderPopupState>()
        .cloned()
        .unwrap_or_default();
    state.remote_tab = Some((provider, remote));
    cx.set_global(state);
}

#[derive(Clone)]
struct AccountGroup {
    profiles: Vec<AccountInfo>,
}

impl AccountGroup {
    fn selected(&self) -> Option<&AccountInfo> {
        self.profiles
            .iter()
            .find(|a| a.active)
            .or_else(|| self.profiles.iter().find(|a| a.host_default))
            .or_else(|| self.profiles.first())
    }
}

/// 이메일이 없는 프로필은 합치지 않는다. 같은 이메일도 상세에서 개별 선택 가능하다.
fn account_groups(snap: &AccountsSnapshot, provider: ProviderKind) -> Vec<AccountGroup> {
    let mut groups: Vec<AccountGroup> = Vec::new();
    for account in snap
        .accounts
        .iter()
        .filter(|a| a.provider == provider && !krill_app::accounts::orca_managed(a))
    {
        let email = account
            .email
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let existing = email.and_then(|email| {
            groups.iter_mut().find(|group| {
                group
                    .profiles
                    .first()
                    .and_then(|a| a.email.as_deref())
                    .is_some_and(|other| other.trim().eq_ignore_ascii_case(email))
            })
        });
        if let Some(group) = existing {
            group.profiles.push(account.clone());
        } else {
            groups.push(AccountGroup {
                profiles: vec![account.clone()],
            });
        }
    }
    groups.sort_by_key(|g| !g.profiles.iter().any(|a| a.host_default));
    groups
}

fn account_name(account: &AccountInfo) -> String {
    account
        .email
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| (!account.label.trim().is_empty()).then(|| account.label.trim()))
        .unwrap_or(&account.id)
        .to_owned()
}

/// 미터 꼬리가 있으면 해당 기간을 우선한다. 잔액·모델별 풀은 상세에 남긴다.
fn comparison_window<'a>(
    snap: &'a AccountsSnapshot,
    account: &'a AccountInfo,
    label: &str,
) -> Option<&'a UsageWindow> {
    if let Some(meters) = snap.meters_for(&account.id) {
        return meters
            .meters
            .iter()
            .find(|m| m.label == label && m.value.is_none())
            .and_then(|m| m.window.as_ref());
    }
    match label {
        "5h" => account.usage.five_hour.as_ref(),
        "7d" => account.usage.seven_day.as_ref(),
        _ => None,
    }
}

/// 기본 기간이 없는 프로바이더는 실제 보고된 풀을 비교하고 나머지는 상세에 둔다.
fn comparison_columns(snap: &AccountsSnapshot, provider: ProviderKind) -> Vec<String> {
    let mut labels = Vec::new();
    for account in snap
        .accounts
        .iter()
        .filter(|a| a.provider == provider && !krill_app::accounts::orca_managed(a))
    {
        if let Some(meters) = snap.meters_for(&account.id) {
            for meter in &meters.meters {
                if meter.value.is_none() && meter.window.is_some() && !labels.contains(&meter.label)
                {
                    labels.push(meter.label.clone());
                }
            }
        } else if !account.usage.is_empty() {
            return vec!["5h".into(), "7d".into()];
        }
    }
    if labels.is_empty() || labels.iter().any(|s| matches!(s.as_str(), "5h" | "7d")) {
        vec!["5h".into(), "7d".into()]
    } else {
        labels.truncate(2);
        labels
    }
}

fn column_title(snap: &AccountsSnapshot, label: &str) -> String {
    match label {
        "5h" => provider_text::PROVIDER_USED_5H.get().into(),
        "7d" => provider_text::PROVIDER_USED_7D.get().into(),
        _ => snap
            .details
            .accounts
            .iter()
            .flat_map(|a| &a.meters)
            .find(|m| m.label == label)
            .map_or_else(
                || label.to_owned(),
                krill_app::accounts::display::meter_label,
            ),
    }
}

/// 사용률 구간별 4단계 시맨틱 색(여유: `success`, 양호: `preserve`, 주의: `warning`, 임박: `destructive`).
fn provider_usage_color(pct: u8, p: &ui::Palette) -> gpui::Hsla {
    let normal = if pct >= 55 {
        p.warning
    } else if pct >= 35 {
        p.preserve
    } else {
        p.success
    };
    krill_app::settings::usage_color(pct, normal, p)
}

fn account_peak_percent(snap: &AccountsSnapshot, account: &AccountInfo) -> Option<u8> {
    let now = i64::try_from(krill_app::accounts::display::now()).unwrap_or(i64::MAX);
    snap.meters_for(&account.id).map_or_else(
        || krill_proto::usage::AutoSwitchMetric::Max.effective_percent(&account.usage, now),
        |m| m.effective_max(now),
    )
}

fn usage_tone_badge(pct: Option<u8>) -> (ui::BadgeTone, &'static str) {
    match pct {
        Some(p) if p >= krill_proto::usage::CRITICAL_PERCENT => (
            ui::BadgeTone::Destructive,
            provider_text::PROVIDER_USAGE_CRITICAL.get(),
        ),
        Some(p) if p >= 55 => (
            ui::BadgeTone::Warning,
            provider_text::PROVIDER_USAGE_WARN.get(),
        ),
        Some(p) if p >= 35 => (
            ui::BadgeTone::Accent,
            provider_text::PROVIDER_USAGE_GOOD.get(),
        ),
        Some(_) => (
            ui::BadgeTone::Success,
            provider_text::PROVIDER_USAGE_PLENTY.get(),
        ),
        None => (ui::BadgeTone::Muted, "—"),
    }
}

fn render_cell(
    selector: String,
    title: &str,
    win: Option<&UsageWindow>,
    width: gpui::Pixels,
    p: &ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let display = krill_app::accounts::display::window(
        win,
        i64::try_from(krill_app::accounts::display::now()).unwrap_or(i64::MAX),
    );
    let color = display
        .percent
        .map_or(p.muted_foreground, |pct| provider_usage_color(pct, p));
    let number_color = if display.percent.is_some() {
        p.foreground
    } else {
        p.muted_foreground
    };
    let debug_id = selector.clone();
    div()
        .id(gpui::SharedString::from(selector))
        .debug_selector(move || debug_id.clone())
        .w(width)
        .flex_none()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(ui::TEXT_2XS)
                        .text_color(p.muted_foreground)
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(ui::FONT_MONO)
                        .text_size(ui::TEXT_XS)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(number_color)
                        .child(display.text),
                ),
        )
        .children(display.percent.map(|pct| {
            div()
                .h(ui::SPACE_1)
                .w_full()
                .rounded_full()
                .overflow_hidden()
                .bg(p.muted)
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(f32::from(pct) / 100.))
                        .bg(color),
                )
        }))
        .children(
            display
                .percent
                .and_then(|_| krill_app::settings::window_reset_suffix(win))
                .map(|reset| {
                    div()
                        .truncate()
                        .text_size(ui::TEXT_2XS)
                        .text_color(p.muted_foreground)
                        .child(reset)
                }),
        )
}

fn switch_account(weak: &WeakEntity<Shell>, account: &AccountInfo, cx: &mut App) {
    weak.update(cx, |shell, cx| {
        shell.refresh_accounts(
            UsageRequest::SwitchAccount {
                provider: account.provider,
                account_id: account.id.clone(),
            },
            cx,
        );
    })
    .ok();
}

fn use_button(
    account: &AccountInfo,
    weak: WeakEntity<Shell>,
    menu: WeakEntity<PopupMenu>,
) -> Button {
    let id = format!("status-provider-use-{}", account.id);
    let selector = id.clone();
    let account = account.clone();
    Button::new(gpui::SharedString::from(id))
        .small()
        .primary()
        .label(provider_text::PROVIDER_USE.get())
        .accessibility_label(format!(
            "{} · {}",
            provider_text::PROVIDER_USE.get(),
            account_name(&account)
        ))
        .debug_selector(move || selector.clone())
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            switch_account(&weak, &account, cx);
            menu.update(cx, |_, cx| cx.emit(gpui::DismissEvent)).ok();
        })
}

fn detail_use_button(
    account: &AccountInfo,
    weak: WeakEntity<Shell>,
    menu: WeakEntity<PopupMenu>,
) -> Button {
    let id = format!("status-provider-detail-use-{}", account.id);
    let selector = id.clone();
    let account = account.clone();
    Button::new(gpui::SharedString::from(id))
        .small()
        .primary()
        .label(provider_text::PROVIDER_SWITCH_NOW.get())
        .accessibility_label(format!(
            "{} · {}",
            provider_text::PROVIDER_SWITCH_NOW.get(),
            account_name(&account)
        ))
        .debug_selector(move || selector.clone())
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            switch_account(&weak, &account, cx);
            menu.update(cx, |_, cx| cx.emit(gpui::DismissEvent)).ok();
        })
}

fn render_detail_reset_controls(
    snap: &AccountsSnapshot,
    account: &AccountInfo,
    reset_armed: Option<&(ProviderKind, String)>,
    weak: &WeakEntity<Shell>,
    menu: &WeakEntity<PopupMenu>,
    detail_menu: &WeakEntity<PopupMenu>,
    p: &ui::Palette,
) -> Option<gpui::Div> {
    if !matches!(account.provider, ProviderKind::Codex | ProviderKind::Claude)
        || krill_app::accounts::orca_managed(account)
    {
        return None;
    }
    let credits = super::reset_credit_count(snap, account);
    let armed =
        reset_armed.is_some_and(|(prov, id)| *prov == account.provider && *id == account.id);
    if armed {
        let (title, _) = krill_app::settings::account_heading(account);
        let info = provider_text::PROVIDER_RESET_USAGE_INFO.format(&[("account", &title)]);
        let confirm_id = format!("status-provider-detail-reset-confirm-{}", account.id);
        let confirm_selector = confirm_id.clone();
        let confirm_weak = weak.clone();
        let confirm_menu = menu.clone();
        let confirm_provider = account.provider;
        let confirm_acc_id = account.id.clone();

        let cancel_id = format!("status-provider-detail-reset-cancel-{}", account.id);
        let cancel_selector = cancel_id.clone();
        let cancel_weak = weak.clone();
        let cancel_detail_menu = detail_menu.clone();
        return Some(
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .pt_1()
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(info),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            Button::new(gpui::SharedString::from(confirm_id))
                                .small()
                                .danger()
                                .label(provider_text::PROVIDER_RESET_USAGE_CONFIRM.get())
                                .debug_selector(move || confirm_selector.clone())
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    confirm_weak
                                        .update(cx, |shell, cx| {
                                            shell.consume_reset_credit(
                                                confirm_provider,
                                                confirm_acc_id.clone(),
                                                cx,
                                            );
                                        })
                                        .ok();
                                    confirm_menu
                                        .update(cx, |_, cx| cx.emit(gpui::DismissEvent))
                                        .ok();
                                }),
                        )
                        .child(
                            Button::new(gpui::SharedString::from(cancel_id))
                                .small()
                                .outline()
                                .label(provider_text::PROVIDER_RESET_USAGE_CANCEL.get())
                                .debug_selector(move || cancel_selector.clone())
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    cancel_weak
                                        .update(cx, |shell, cx| {
                                            shell.reset_armed = None;
                                            cx.notify();
                                        })
                                        .ok();
                                    cancel_detail_menu.update(cx, |_, cx| cx.notify()).ok();
                                }),
                        ),
                ),
        );
    }
    let btn_id = format!("status-provider-detail-reset-{}", account.id);
    let selector = btn_id.clone();
    if credits == Some(0) {
        return Some(
            div().pt_1().child(
                Button::new(gpui::SharedString::from(btn_id))
                    .small()
                    .outline()
                    .disabled(true)
                    .label(provider_text::PROVIDER_RESET_USAGE_NONE.get())
                    .debug_selector(move || selector.clone()),
            ),
        );
    }
    let arm_weak = weak.clone();
    let arm_detail_menu = detail_menu.clone();
    let provider = account.provider;
    let account_id = account.id.clone();
    let label = match credits {
        Some(c) if c > 0 => format!("{} ({c})", provider_text::PROVIDER_RESET_USAGE.get()),
        _ => provider_text::PROVIDER_RESET_USAGE.get().to_owned(),
    };
    Some(
        div().pt_1().child(
            Button::new(gpui::SharedString::from(btn_id))
                .small()
                .outline()
                .label(label)
                .debug_selector(move || selector.clone())
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    arm_weak
                        .update(cx, |shell, cx| {
                            shell.reset_armed = Some((provider, account_id.clone()));
                            cx.notify();
                        })
                        .ok();
                    arm_detail_menu.update(cx, |_, cx| cx.notify()).ok();
                }),
        ),
    )
}

fn window_bar_color(win: Option<&UsageWindow>, p: &ui::Palette) -> gpui::Hsla {
    let now = i64::try_from(krill_app::accounts::display::now()).unwrap_or(i64::MAX);
    krill_app::accounts::display::window(win, now)
        .percent
        .map_or(p.muted_foreground, |pct| provider_usage_color(pct, p))
}

fn render_details(
    snap: &AccountsSnapshot,
    group: &AccountGroup,
    reset_armed: Option<&(ProviderKind, String)>,
    weak: &WeakEntity<Shell>,
    menu: &WeakEntity<PopupMenu>,
    detail_menu: &WeakEntity<PopupMenu>,
    p: &ui::Palette,
) -> gpui::Div {
    let mut detail = div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .text_size(ui::TEXT_XS)
        .text_color(p.muted_foreground);
    if group.profiles.len() > 1 {
        detail = detail.child(provider_text::PROVIDER_GROUP_PROFILES.get());
    }
    for account in &group.profiles {
        let mut profile = div()
            .w_full()
            .min_w_0()
            .pt_2()
            .border_t_1()
            .border_color(p.border)
            .flex()
            .flex_col()
            .gap_2();
        if group.profiles.len() > 1 {
            let label = if account.host_default {
                account_text::SYSTEM_DEFAULT.get().to_owned()
            } else if account.label.trim().is_empty() {
                account.id.clone()
            } else {
                account.label.clone()
            };
            let selector = format!("status-provider-profile-{}", account.id);
            let debug_id = selector.clone();
            let path = account.profile_dir.clone();
            profile = profile.child(
                div()
                    .id(gpui::SharedString::from(selector))
                    .debug_selector(move || debug_id.clone())
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(if account.active {
                                p.foreground
                            } else {
                                p.muted_foreground
                            })
                            .font_weight(if account.active {
                                gpui::FontWeight::SEMIBOLD
                            } else {
                                gpui::FontWeight::NORMAL
                            })
                            .child(label),
                    )
                    .child(if account.active {
                        ui::badge(
                            p,
                            ui::BadgeTone::Success,
                            account_text::ACCOUNT_IN_USE.get(),
                        )
                        .into_any_element()
                    } else {
                        use_button(account, weak.clone(), menu.clone()).into_any_element()
                    })
                    .tooltip(move |window, cx| {
                        ui::tooltip_view("status-profile-path", path.clone(), window, cx)
                    }),
            );
        }
        let status = krill_app::accounts::display::account(
            account,
            snap.status_for(&account.id),
            krill_app::accounts::display::now(),
        );
        profile = profile.child(krill_app::accounts::display::render_status(
            format!("status-provider-detail-status-{}", account.id),
            &status,
            p,
        ));
        if let Some(meters) = snap.meters_for(&account.id) {
            profile = profile.children(meters.plan.as_ref().map(|plan| div().child(plan.clone())));
            for meter in &meters.meters {
                let label = krill_app::accounts::display::meter_label(meter);
                profile = profile.child(if let Some(value) = &meter.value {
                    div()
                        .flex()
                        .justify_between()
                        .gap_2()
                        .child(label)
                        .child(
                            div()
                                .font_family(ui::FONT_MONO)
                                .text_color(p.foreground)
                                .child(value.clone()),
                        )
                        .into_any_element()
                } else {
                    render_popover_usage_row(
                        &label,
                        format!("status-provider-detail-{}-{}", account.id, meter.key),
                        meter.window.as_ref(),
                        window_bar_color(meter.window.as_ref(), p),
                        p,
                    )
                    .into_any_element()
                });
            }
        } else {
            for (label, window) in [
                (
                    provider_text::PROVIDER_USED_5H.get(),
                    account.usage.five_hour.as_ref(),
                ),
                (
                    provider_text::PROVIDER_USED_7D.get(),
                    account.usage.seven_day.as_ref(),
                ),
            ] {
                profile = profile.child(render_popover_usage_row(
                    label,
                    format!("status-provider-detail-{}-{label}", account.id),
                    window,
                    window_bar_color(window, p),
                    p,
                ));
            }
        }
        if let Some(rule) = snap.auto_switch.rule_for(account.provider, &account.id) {
            profile = profile.child(format!(
                "{} · {}",
                provider_text::PROVIDER_AUTO_SWITCH_HEADER.get(),
                krill_app::settings::rule_summary(rule, &snap.accounts)
            ));
        }
        profile = profile.children(
            advanced_notes(snap, account)
                .into_iter()
                .map(|note| div().child(note)),
        );
        if let Some(reset_box) =
            render_detail_reset_controls(snap, account, reset_armed, weak, menu, detail_menu, p)
        {
            profile = profile.child(reset_box);
        }
        if group.profiles.len() == 1 {
            profile = profile.child(
                div()
                    .pt_1p5()
                    .border_t_1()
                    .border_color(p.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(if account.active {
                        ui::badge(
                            p,
                            ui::BadgeTone::Success,
                            provider_text::PROVIDER_ACTIVE_NOW.get(),
                        )
                        .into_any_element()
                    } else {
                        detail_use_button(account, weak.clone(), menu.clone()).into_any_element()
                    }),
            );
        }
        detail = detail.child(profile);
    }
    detail
}

pub(super) struct ProviderHostScope<'a> {
    pub(super) snap: &'a AccountsSnapshot,
    pub(super) active_host: &'a krill_app::hosts::HostId,
    pub(super) account_hosts: &'a [(
        krill_app::hosts::HostId,
        String,
        std::sync::Arc<AccountsSnapshot>,
    )],
}

fn render_remote_hosts_tab(
    provider: ProviderKind,
    other_hosts: &[(
        krill_app::hosts::HostId,
        String,
        std::sync::Arc<AccountsSnapshot>,
    )],
    content_width: gpui::Pixels,
    weak: &WeakEntity<Shell>,
    menu_weak: &WeakEntity<PopupMenu>,
    p: &ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let mut container = div()
        .id("status-provider-remote-hosts-view")
        .debug_selector(|| "status-provider-remote-hosts-view".into())
        .w(content_width)
        .py_1()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(provider_text::PROVIDER_REMOTE_HINT.get()),
        );

    for (other_host, other_label, other_snap) in other_hosts {
        let profiles: Vec<AccountInfo> = other_snap
            .accounts
            .iter()
            .filter(|a| a.provider == provider && !krill_app::accounts::orca_managed(a))
            .cloned()
            .collect();
        let active_profile = profiles
            .iter()
            .find(|a| a.active)
            .or_else(|| profiles.first())
            .cloned();
        let peak_pct = active_profile
            .as_ref()
            .and_then(|acc| account_peak_percent(other_snap, acc));
        let (tone, tone_label) = usage_tone_badge(peak_pct);
        let select_weak = weak.clone();
        let select_host = other_host.clone();
        let select_menu = menu_weak.clone();
        let host_btn_id = format!("status-provider-select-host-{}", other_host.as_key());
        let host_btn_sel = host_btn_id.clone();

        let mut host_card = div()
            .p_2p5()
            .rounded(ui::RADIUS_SM)
            .bg(p.secondary.opacity(0.55))
            .border_1()
            .border_color(p.border)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .size(ui::STATE_DOT)
                                    .rounded_full()
                                    .flex_none()
                                    .bg(p.success),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(p.foreground)
                                    .child(other_label.clone()),
                            )
                            .children(
                                peak_pct
                                    .map(|pct| ui::badge(p, tone, format!("{tone_label} {pct}%"))),
                            ),
                    )
                    .child(
                        Button::new(gpui::SharedString::from(host_btn_id))
                            .xsmall()
                            .outline()
                            .label(format!(
                                "{}: {other_label}",
                                account_text::ACCOUNTS_HOST_TITLE.get()
                            ))
                            .debug_selector(move || host_btn_sel.clone())
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                select_weak
                                    .update(cx, |shell, cx| {
                                        shell.set_launch_host(select_host.clone(), cx);
                                    })
                                    .ok();
                                select_menu
                                    .update(cx, |_, cx| cx.emit(gpui::DismissEvent))
                                    .ok();
                            }),
                    ),
            );

        if !profiles.is_empty() {
            let mut profile_list = div().flex().flex_col().gap_1();
            for profile in &profiles {
                let switch_weak = weak.clone();
                let switch_host = other_host.clone();
                let switch_menu = menu_weak.clone();
                let account_id = profile.id.clone();
                let item_label = if profile.label.trim().is_empty() {
                    account_name(profile)
                } else if let Some(email) =
                    profile.email.as_deref().filter(|s| !s.trim().is_empty())
                    && !profile.label.trim().eq_ignore_ascii_case(email.trim())
                {
                    format!("{} ({})", profile.label.trim(), email.trim())
                } else {
                    profile.label.trim().to_owned()
                };
                let remote_use_id = format!(
                    "status-provider-remote-use-{}-{}",
                    other_host.as_key(),
                    profile.id
                );
                let remote_use_sel = remote_use_id.clone();
                profile_list = profile_list.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(ui::RADIUS_XS)
                        .bg(p.background.opacity(0.35))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(ui::TEXT_XS)
                                .text_color(if profile.active {
                                    p.foreground
                                } else {
                                    p.muted_foreground
                                })
                                .font_weight(if profile.active {
                                    gpui::FontWeight::SEMIBOLD
                                } else {
                                    gpui::FontWeight::NORMAL
                                })
                                .child(item_label),
                        )
                        .child(if profile.active {
                            ui::badge(
                                p,
                                ui::BadgeTone::Success,
                                account_text::ACCOUNT_IN_USE.get(),
                            )
                            .into_any_element()
                        } else {
                            Button::new(gpui::SharedString::from(remote_use_id))
                                .xsmall()
                                .primary()
                                .label(provider_text::PROVIDER_USE.get())
                                .debug_selector(move || remote_use_sel.clone())
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    switch_weak
                                        .update(cx, |shell, cx| {
                                            shell.refresh_accounts_for_host(
                                                switch_host.clone(),
                                                UsageRequest::SwitchAccount {
                                                    provider,
                                                    account_id: account_id.clone(),
                                                },
                                                cx,
                                            );
                                        })
                                        .ok();
                                    switch_menu
                                        .update(cx, |_, cx| cx.emit(gpui::DismissEvent))
                                        .ok();
                                })
                                .into_any_element()
                        }),
                );
            }
            host_card = host_card.child(profile_list);
        }
        container = container.child(host_card);
    }
    container
}

pub(super) fn provider_usage_menu(
    mut menu: PopupMenu,
    scope: &ProviderHostScope<'_>,
    provider: ProviderKind,
    weak: WeakEntity<Shell>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
    reset_armed: Option<&(ProviderKind, String)>,
) -> PopupMenu {
    let snap = scope.snap;
    let active_host = scope.active_host;
    let account_hosts = scope.account_hosts;
    let groups = account_groups(snap, provider);
    let local_host_label = account_hosts
        .iter()
        .find(|(h, _, _)| h == active_host)
        .map_or_else(
            || {
                krill_app::i18n::catalog::shell::HOST_LOCAL
                    .mac_or(krill_app::i18n::catalog::other_os::HOST_LOCAL)
                    .get()
                    .to_owned()
            },
            |(_, label, _)| label.clone(),
        );
    let active_host_badge = (account_hosts.len() > 1).then(|| local_host_label.clone());
    let other_hosts: Vec<_> = account_hosts
        .iter()
        .filter(|(h, _, _)| h != active_host)
        .cloned()
        .collect();
    let has_remote_hosts = !other_hosts.is_empty();

    let available = window.bounds().size.width - ui::SPACE_4 * 2.;
    let width = px(580.).min(available);
    let content_width = width - ui::SPACE_4 * 2.;
    let master_width = ((content_width - ui::SPACE_3) * 0.52).floor();
    let detail_panel_width = (content_width - master_width - ui::SPACE_3).max(px(180.));
    let columns = comparison_columns(snap, provider);
    let col_count = u8::try_from(columns.len().max(1)).unwrap_or(2);
    let master_inner = (master_width - ui::SPACE_2 * 2.).max(px(120.));
    let column_width = ((master_inner - ui::SPACE_2 * f32::from(col_count.saturating_sub(1)))
        / f32::from(col_count))
    .floor();
    let headers: Vec<_> = columns
        .iter()
        .map(|label| column_title(snap, label))
        .collect();
    let active = groups
        .iter()
        .filter_map(AccountGroup::selected)
        .find(|a| a.active)
        .or_else(|| groups.first().and_then(AccountGroup::selected))
        .cloned();

    // 팝오버를 새로 열 때는 현재 호스트 탭과 활성 계정을 기본 선택한다.
    cx.set_global(ProviderPopupState {
        selected_account: active.as_ref().map(|a| (provider, a.id.clone())),
        remote_tab: Some((provider, false)),
    });

    let menu_weak = cx.weak_entity();
    let header_menu = menu_weak.clone();
    let group_count = groups.len();
    let remote_count = other_hosts.len();

    menu = menu
        .min_w(width)
        .max_w(width)
        .check_side(gpui_component::Side::Right);
    menu = menu.item(
        PopupMenuItem::element(move |_, cx| {
            let p = ui::palette(cx);
            let remote_tab = is_remote_tab_for(cx, provider, has_remote_hosts);
            let local_tab_menu = header_menu.clone();
            let remote_tab_menu = header_menu.clone();
            div()
                .id("status-provider-table-header")
                .debug_selector(|| "status-provider-table-header".into())
                .w(content_width)
                .py_1p5()
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
                                .gap_2()
                                .child(ui::icons::provider_icon(
                                    provider,
                                    ui::ICON_MD,
                                    p.foreground,
                                ))
                                .child(
                                    div()
                                        .text_size(ui::TEXT_LG)
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(p.foreground)
                                        .child(provider.display_name()),
                                )
                                .children(active_host_badge.as_ref().map(|host_label| {
                                    div()
                                        .id("status-provider-active-host")
                                        .debug_selector(|| "status-provider-active-host".into())
                                        .child(ui::badge(
                                            p,
                                            ui::BadgeTone::Accent,
                                            host_label.clone(),
                                        ))
                                })),
                        )
                        .child(if has_remote_hosts {
                            ui::segmented_control(
                                p,
                                [
                                    ui::segment(
                                        p,
                                        "status-provider-tab-local",
                                        provider_text::PROVIDER_TAB_LOCAL.format(&[
                                            ("host", &local_host_label),
                                            ("count", &group_count.to_string()),
                                        ]),
                                        !remote_tab,
                                    )
                                    .debug_selector(|| "status-provider-tab-local".into())
                                    .on_click(
                                        move |_, _, cx| {
                                            cx.stop_propagation();
                                            set_remote_tab(provider, false, cx);
                                            local_tab_menu.update(cx, |_, cx| cx.notify()).ok();
                                        },
                                    ),
                                    ui::segment(
                                        p,
                                        "status-provider-tab-remote",
                                        provider_text::PROVIDER_TAB_REMOTE
                                            .format(&[("count", &remote_count.to_string())]),
                                        remote_tab,
                                    )
                                    .debug_selector(|| "status-provider-tab-remote".into())
                                    .on_click(
                                        move |_, _, cx| {
                                            cx.stop_propagation();
                                            set_remote_tab(provider, true, cx);
                                            remote_tab_menu.update(cx, |_, cx| cx.notify()).ok();
                                        },
                                    ),
                                ],
                            )
                            .into_any_element()
                        } else {
                            ui::badge(
                                p,
                                ui::BadgeTone::Muted,
                                account_text::PROFILES_COUNT
                                    .format(&[("count", &group_count.to_string())]),
                            )
                            .into_any_element()
                        }),
                )
                .when(!remote_tab, |d| {
                    d.child(
                        div()
                            .w(master_width)
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(provider_text::PROVIDER_ACCOUNT.get()),
                            )
                            .children(headers.iter().map(|label| {
                                div()
                                    .w(column_width)
                                    .flex_none()
                                    .truncate()
                                    .child(label.clone())
                            })),
                    )
                })
        })
        .disabled(true),
    );

    let body_groups = groups;
    let body_snap = snap.clone();
    let body_columns = columns;
    let body_weak = weak.clone();
    let body_menu = menu_weak;
    let body_other_hosts = other_hosts;
    menu = menu.separator().item(
        PopupMenuItem::element(move |_, cx| {
            let p = ui::palette(cx);
            if is_remote_tab_for(cx, provider, has_remote_hosts) {
                return render_remote_hosts_tab(
                    provider,
                    &body_other_hosts,
                    content_width,
                    &body_weak,
                    &body_menu,
                    p,
                )
                .into_any_element();
            }

            let selected_group = selected_account_for(cx, provider, &body_groups);
            let selected_id = selected_group
                .as_ref()
                .and_then(AccountGroup::selected)
                .map(|a| a.id.clone());

            let mut master_col = div()
                .w(master_width)
                .flex_none()
                .flex()
                .flex_col()
                .gap_1p5();

            for group in &body_groups {
                let Some(account) = group.selected() else {
                    continue;
                };
                let is_selected = selected_id.as_deref() == Some(account.id.as_str());
                let row_id = format!(
                    "status-provider-account-{}-{}",
                    provider.as_str(),
                    account.id
                );
                let debug_id = row_id.clone();
                let status = krill_app::accounts::display::account(
                    account,
                    body_snap.status_for(&account.id),
                    krill_app::accounts::display::now(),
                );
                let status_visible =
                    !matches!(status.status, AccountStatus::Ok | AccountStatus::Unknown);

                let hover_id = account.id.clone();
                let hover_menu = body_menu.clone();
                let click_id = account.id.clone();
                let click_menu = body_menu.clone();

                let card_bg = if account.active {
                    p.success.opacity(0.10)
                } else if is_selected {
                    p.accent
                } else {
                    p.secondary.opacity(0.45)
                };
                let card_border = if account.active {
                    if is_selected {
                        p.success
                    } else {
                        p.success.opacity(0.45)
                    }
                } else if is_selected {
                    p.ring
                } else {
                    p.border
                };
                let dot_color = if account.active {
                    p.success
                } else if is_selected {
                    p.preserve
                } else {
                    p.muted_foreground.opacity(0.45)
                };

                master_col = master_col.child(
                    div()
                        .id(gpui::SharedString::from(row_id.clone()))
                        .debug_selector(move || debug_id.clone())
                        .w_full()
                        .px_2()
                        .py_2()
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(card_border)
                        .bg(card_bg)
                        .cursor_pointer()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .on_mouse_move(move |_, _, cx| {
                            if set_selected_account(provider, hover_id.clone(), cx) {
                                hover_menu.update(cx, |_, cx| cx.notify()).ok();
                            }
                        })
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            set_selected_account(provider, click_id.clone(), cx);
                            click_menu.update(cx, |_, cx| cx.notify()).ok();
                        })
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_1p5()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .child(
                                            div()
                                                .size(ui::STATE_DOT)
                                                .rounded_full()
                                                .flex_none()
                                                .bg(dot_color),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(ui::TEXT_SM)
                                                .text_color(if account.active || is_selected {
                                                    p.foreground
                                                } else {
                                                    p.muted_foreground
                                                })
                                                .font_weight(if account.active || is_selected {
                                                    gpui::FontWeight::SEMIBOLD
                                                } else {
                                                    gpui::FontWeight::MEDIUM
                                                })
                                                .child(account_name(account)),
                                        ),
                                )
                                .child(div().flex_none().flex().items_center().gap_1().child(
                                    if account.active {
                                        ui::badge(
                                            p,
                                            ui::BadgeTone::Success,
                                            account_text::ACCOUNT_IN_USE.get(),
                                        )
                                        .into_any_element()
                                    } else if group.profiles.len() > 1 {
                                        ui::badge(
                                            p,
                                            ui::BadgeTone::Muted,
                                            account_text::PROFILES_COUNT.format(&[(
                                                "count",
                                                &group.profiles.len().to_string(),
                                            )]),
                                        )
                                        .into_any_element()
                                    } else {
                                        use_button(account, body_weak.clone(), body_menu.clone())
                                            .into_any_element()
                                    },
                                )),
                        )
                        .when(account.host_default || group.profiles.len() > 1, |d| {
                            d.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .when(account.host_default, |d| {
                                        d.child(ui::badge(
                                            p,
                                            ui::BadgeTone::Muted,
                                            provider_text::PROVIDER_DEFAULT_LOGIN.get(),
                                        ))
                                    })
                                    .when(group.profiles.len() > 1, |d| {
                                        d.child(ui::badge(
                                            p,
                                            ui::BadgeTone::Muted,
                                            account_text::PROFILES_COUNT.format(&[(
                                                "count",
                                                &group.profiles.len().to_string(),
                                            )]),
                                        ))
                                    }),
                            )
                        })
                        .child(div().w_full().flex().items_start().gap_2().children(
                            body_columns.iter().map(|label| {
                                render_cell(
                                    format!("{row_id}-{label}"),
                                    &column_title(&body_snap, label),
                                    comparison_window(&body_snap, account, label),
                                    column_width,
                                    p,
                                )
                            }),
                        ))
                        .when(status_visible, |d| {
                            d.child(krill_app::accounts::display::render_status(
                                format!("{row_id}-status"),
                                &status,
                                p,
                            ))
                        }),
                );
            }

            let detail_col = if let Some(panel_group) = selected_group
                && let Some(panel_account) = panel_group.selected().cloned()
            {
                let armed = body_weak
                    .read_with(cx, |shell, _| shell.reset_armed.clone())
                    .ok()
                    .flatten();
                let selector = format!("status-provider-details-{}", panel_account.id);
                let debug_id = selector.clone();
                let peak = account_peak_percent(&body_snap, &panel_account);
                let (tone, tone_label) = usage_tone_badge(peak);
                div()
                    .id(gpui::SharedString::from(selector))
                    .debug_selector(move || debug_id.clone())
                    .w(detail_panel_width)
                    .flex_none()
                    .p_2p5()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(if panel_account.active {
                        p.success.opacity(0.45)
                    } else {
                        p.border
                    })
                    .bg(p.secondary.opacity(0.55))
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
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(p.popover_foreground)
                                    .child(account_name(&panel_account)),
                            )
                            .child(ui::badge(p, tone, tone_label)),
                    )
                    .child(render_details(
                        &body_snap,
                        &panel_group,
                        armed.as_ref(),
                        &body_weak,
                        &body_menu,
                        &body_menu,
                        p,
                    ))
                    .into_any_element()
            } else {
                div().w(detail_panel_width).flex_none().into_any_element()
            };

            div()
                .w(content_width)
                .py_1()
                .flex()
                .items_start()
                .gap_3()
                .child(master_col)
                .child(detail_col)
                .into_any_element()
        })
        .disabled(true),
    );

    let policy = snap.auto_switch.clone();
    let toggle_weak = weak.clone();
    menu = menu.separator().item(
        PopupMenuItem::new(provider_text::PROVIDER_GLOBAL_AUTO_SWITCH.get())
            .checked(policy.enabled)
            .on_click(move |_, _, cx| {
                let mut next = policy.clone();
                next.enabled = !next.enabled;
                toggle_weak
                    .update(cx, |shell, cx| {
                        shell.refresh_accounts(UsageRequest::SetAutoSwitch(next.clone()), cx);
                    })
                    .ok();
            }),
    );
    let show_in_bar = krill_app::settings::status_provider_visible(provider, cx);
    menu = menu.item(
        PopupMenuItem::element(move |_, _| {
            div()
                .id(gpui::SharedString::from(format!(
                    "status-provider-toggle-bar-{}",
                    provider.as_str()
                )))
                .debug_selector(move || format!("status-provider-toggle-bar-{}", provider.as_str()))
                .child(provider_text::PROVIDER_SHOW_IN_STATUS_BAR.get())
        })
        .checked(show_in_bar)
        .on_click(move |_, _, cx| {
            krill_app::settings::set_status_provider_visible(provider, !show_in_bar, cx);
        }),
    );
    let options_snap = snap.clone();
    let options_weak = weak.clone();
    let armed = reset_armed.cloned();
    menu = menu.submenu(
        provider_text::PROVIDER_OPTIONS.get(),
        window,
        cx,
        move |menu, window, cx| {
            let menu = append_policy_menu(
                menu,
                &options_snap,
                active.clone(),
                &options_weak,
                window,
                cx,
            );
            append_reset_credit_rows(
                menu,
                &options_snap,
                active.as_ref(),
                provider,
                armed.as_ref(),
                &options_weak,
            )
        },
    );
    let refresh_weak = weak.clone();
    menu = menu.item(
        PopupMenuItem::new(provider_text::PROVIDER_REFRESH_USAGE.get()).on_click(
            move |_, _, cx| {
                refresh_weak.update(cx, Shell::refresh_usage_manually).ok();
            },
        ),
    );
    menu.item(
        PopupMenuItem::element(|_, cx| {
            let p = ui::palette(cx);
            div()
                .id("status-provider-open-accounts")
                .debug_selector(|| "status-provider-open-accounts".into())
                .text_size(ui::TEXT_SM)
                .text_color(p.foreground)
                .child(provider_text::PROVIDER_OPEN_SETTINGS.get())
        })
        .on_click(move |_, window, cx| {
            open_settings_from_menu(&weak, krill_app::settings::AdminTab::Accounts, window, cx);
        }),
    )
}

#[cfg(all(test, unix))]
mod tests;
