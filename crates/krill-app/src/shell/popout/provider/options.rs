//! 프로바이더별 추가 설정·사용량 상세.

use super::*;

pub(super) fn advanced_notes(snap: &AccountsSnapshot, account: &AccountInfo) -> Vec<String> {
    use krill_app::i18n::catalog::shell as text;
    if !snap.advanced.supported {
        return Vec::new();
    }
    let now = krill_app::accounts::display::now();
    let mut notes = Vec::new();
    for model in snap
        .advanced
        .model_windows
        .iter()
        .filter(|m| m.account_id == account.id)
    {
        let pct = krill_app::accounts::display::window(
            Some(&model.seven_day),
            i64::try_from(now).unwrap_or(i64::MAX),
        )
        .text;
        notes.push(format!(
            "{} · {} {pct}",
            model.model,
            account_text::WINDOW_7D.get()
        ));
    }
    for f in snap
        .advanced
        .forecasts
        .iter()
        .filter(|f| f.account_id == account.id)
    {
        notes.push(text::ACCOUNT_FORECAST.format(&[
            ("window", &f.window),
            (
                "hours",
                &f.exhausts_at.saturating_sub(now).div_ceil(3600).to_string(),
            ),
            ("rate", &f.percent_per_hour.to_string()),
        ]));
    }
    if let Some(limit) = snap
        .advanced
        .rate_limits
        .iter()
        .find(|l| l.account_id == account.id)
    {
        notes.push(
            text::ACCOUNT_LIMIT_CLEARS
                .format(&[("seconds", &limit.clears_at.saturating_sub(now).to_string())]),
        );
    }
    if let Some(cd) = snap
        .advanced
        .cooldowns
        .iter()
        .find(|c| c.provider == account.provider && c.cooldown_until > now)
    {
        notes.push(text::ACCOUNT_COOLDOWN_LEFT.format(&[(
            "seconds",
            &cd.cooldown_until.saturating_sub(now).to_string(),
        )]));
    }
    notes
}

pub(super) fn append_policy_menu(
    mut menu: PopupMenu,
    snap: &AccountsSnapshot,
    active: Option<AccountInfo>,
    weak: &WeakEntity<Shell>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
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
    if snap.advanced.supported {
        use krill_app::i18n::catalog::shell as policy_text;
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
    menu
}
