//! 상태바 계정 비교표. 이메일 묶음은 표시만 합치며 프로필·전환 대상은 보존한다.

use super::*;
use krill_app::i18n::catalog::settings as account_text;
use krill_proto::usage::{AccountStatus, Window as UsageWindow};

mod options;
use options::{advanced_notes, append_policy_menu};

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

fn usage_column(content_width: gpui::Pixels) -> gpui::Pixels {
    (ui::DIALOG_WIDTH / 4.)
        .min((content_width - action_width() - px(ui::ICON_SM) - ui::SPACE_2 * 4.) / 4.)
}

fn action_width() -> gpui::Pixels {
    ui::SPACE_4 * 4.
}

fn render_cell(
    selector: String,
    win: Option<&UsageWindow>,
    width: gpui::Pixels,
    p: &ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    let display = krill_app::accounts::display::window(
        win,
        i64::try_from(krill_app::accounts::display::now()).unwrap_or(i64::MAX),
    );
    let color = display.percent.map_or(p.muted_foreground, |pct| {
        krill_app::settings::usage_color(pct, p.muted_foreground, p)
    });
    let debug_id = selector.clone();
    div()
        .id(gpui::SharedString::from(selector))
        .debug_selector(move || debug_id.clone())
        .w(width)
        .flex_none()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .font_family(ui::FONT_MONO)
                .text_size(ui::TEXT_SM)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(color)
                .child(display.text),
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

fn render_details(
    snap: &AccountsSnapshot,
    group: &AccountGroup,
    weak: &WeakEntity<Shell>,
    menu: &WeakEntity<PopupMenu>,
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
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(if account.active {
                        ui::badge(p, ui::BadgeTone::Muted, account_text::ACCOUNT_IN_USE.get())
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
                        .child(div().font_family(ui::FONT_MONO).child(value.clone()))
                        .into_any_element()
                } else {
                    render_popover_usage_row(
                        &label,
                        format!("status-provider-detail-{}-{}", account.id, meter.key),
                        meter.window.as_ref(),
                        p.muted_foreground,
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
                    p.muted_foreground,
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
        detail = detail.child(profile);
    }
    detail
}

pub(super) fn provider_usage_menu(
    mut menu: PopupMenu,
    snap: &AccountsSnapshot,
    provider: ProviderKind,
    weak: WeakEntity<Shell>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
    reset_armed: Option<&(ProviderKind, String)>,
) -> PopupMenu {
    let groups = account_groups(snap, provider);
    let available = window.bounds().size.width - ui::SPACE_4 * 2.;
    // 비교표가 창 폭을 다 차지하지 않게 상세 패널 자리를 먼저 남긴다.
    let detail_width = (ui::DIALOG_WIDTH * 0.625).min(available / 3.);
    let width = ui::SETTINGS_MODAL_WIDTH.min(available - detail_width);
    let content_width = width - ui::SPACE_4 * 2.;
    let column_width = usage_column(content_width);
    let columns = comparison_columns(snap, provider);
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
    menu = menu
        .min_w(width)
        .max_w(width)
        .check_side(gpui_component::Side::Right);
    menu = menu.item(
        PopupMenuItem::element(move |_, cx| {
            let p = ui::palette(cx);
            div()
                .id("status-provider-table-header")
                .debug_selector(|| "status-provider-table-header".into())
                .w(content_width)
                .py_2()
                .flex()
                .flex_col()
                .gap_3()
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
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .flex()
                        .gap_2()
                        .text_size(ui::TEXT_XS)
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
                        }))
                        .child(div().w(action_width()).flex_none())
                        .child(div().w(px(ui::ICON_SM)).flex_none()),
                )
        })
        .disabled(true),
    );
    for group in groups {
        let Some(account) = group.selected().cloned() else {
            continue;
        };
        let row_account = account.clone();
        let detail_weak = weak.clone();
        let menu_weak = cx.weak_entity();
        let snapshot = snap.clone();
        let columns = columns.clone();
        let panel_group = group.clone();
        let panel_snap = snap.clone();
        let panel_weak = weak.clone();
        let parent_menu = menu_weak.clone();
        let panel_account = account.clone();
        let panel = PopupMenu::build(window, cx, move |menu, _, _| {
            menu.min_w(detail_width)
                .max_w(detail_width)
                .scrollable(true)
                .item(
                    PopupMenuItem::element(move |_, cx| {
                        let p = ui::palette(cx);
                        let selector = format!("status-provider-details-{}", panel_account.id);
                        let debug_id = selector.clone();
                        div()
                            .id(gpui::SharedString::from(selector))
                            .debug_selector(move || debug_id.clone())
                            .w(detail_width - ui::SPACE_4 * 2.)
                            .py_2()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(p.popover_foreground)
                                    .child(account_name(&panel_account)),
                            )
                            .child(render_details(
                                &panel_snap,
                                &panel_group,
                                &panel_weak,
                                &parent_menu,
                                p,
                            ))
                    })
                    .disabled(true),
                )
        });
        menu = menu.separator().item(PopupMenuItem::submenu_element(
            account_name(&account),
            panel,
            move |_, cx| {
                let p = ui::palette(cx);
                let account = &row_account;
                let row_id = format!(
                    "status-provider-account-{}-{}",
                    provider.as_str(),
                    account.id
                );
                let debug_id = row_id.clone();
                let status = krill_app::accounts::display::account(
                    account,
                    snapshot.status_for(&account.id),
                    krill_app::accounts::display::now(),
                );
                let status_visible =
                    !matches!(status.status, AccountStatus::Ok | AccountStatus::Unknown);
                div()
                    .id(gpui::SharedString::from(row_id.clone()))
                    .debug_selector(move || debug_id.clone())
                    .w(content_width)
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_start()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(ui::TEXT_SM)
                                            .text_color(p.foreground)
                                            .font_weight(if account.active {
                                                gpui::FontWeight::SEMIBOLD
                                            } else {
                                                gpui::FontWeight::NORMAL
                                            })
                                            .child(account_name(account)),
                                    )
                                    .child(
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
                                    ),
                            )
                            .children(columns.iter().map(|label| {
                                render_cell(
                                    format!("{row_id}-{label}"),
                                    comparison_window(&snapshot, account, label),
                                    column_width,
                                    p,
                                )
                            }))
                            .child(
                                div()
                                    .w(action_width())
                                    .flex_none()
                                    .flex()
                                    .justify_end()
                                    .child(if account.active {
                                        ui::badge(
                                            p,
                                            ui::BadgeTone::Muted,
                                            account_text::ACCOUNT_IN_USE.get(),
                                        )
                                        .into_any_element()
                                    } else {
                                        use_button(account, detail_weak.clone(), menu_weak.clone())
                                            .into_any_element()
                                    }),
                            )
                            .child(
                                gpui_component::Icon::new(gpui_component::IconName::ChevronRight)
                                    .size(px(ui::ICON_SM))
                                    .text_color(p.muted_foreground),
                            ),
                    )
                    .when(status_visible, |d| {
                        d.child(krill_app::accounts::display::render_status(
                            format!("{row_id}-status"),
                            &status,
                            p,
                        ))
                    })
            },
        ));
    }
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
