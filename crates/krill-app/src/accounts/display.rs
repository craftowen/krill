//! 계정 카드와 상태바가 함께 쓰는 사용량·상태 표시(OWE-703).

use crate::i18n::catalog::settings as text;
use krill_proto::usage::{
    AccountInfo, AccountStatus, AccountStatusInfo, AccountsSnapshot, ConnectionStatus, Snapshot,
    Window,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowDisplay {
    pub percent: Option<u8>,
    pub text: String,
}

pub fn window(win: Option<&Window>, now: i64) -> WindowDisplay {
    let Some(win) = win else {
        return WindowDisplay {
            percent: None,
            text: "—".into(),
        };
    };
    if win
        .resets_at
        .as_deref()
        .and_then(krill_proto::time::parse_reset_epoch)
        .is_some_and(|at| at > 0 && at <= now)
    {
        return WindowDisplay {
            percent: None,
            text: text::ACCOUNT_WINDOW_RESET.get().into(),
        };
    }
    WindowDisplay {
        percent: Some(win.percent.min(100)),
        text: format!("{}%", win.percent.min(100)),
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn summary(usage: &Snapshot, now: u64) -> String {
    let now = i64::try_from(now).unwrap_or(i64::MAX);
    format!(
        "5h {} · 7d {}",
        window(usage.five_hour.as_ref(), now).text,
        window(usage.seven_day.as_ref(), now).text
    )
}

pub fn meters_summary(meters: &krill_proto::usage::AccountMeters, now: u64) -> String {
    let now = i64::try_from(now).unwrap_or(i64::MAX);
    meters
        .meters
        .iter()
        .map(|meter| {
            let value = meter
                .value
                .clone()
                .unwrap_or_else(|| window(meter.window.as_ref(), now).text);
            format!("{} {value}", meter_label(meter))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

pub fn meter_label(meter: &krill_proto::usage::UsageMeter) -> String {
    let label = match meter.label.as_str() {
        "5h" => text::WINDOW_5H.get(),
        "7d" => text::WINDOW_7D.get(),
        "30d" => text::WINDOW_MONTHLY.get(),
        "Extra usage" => text::METER_EXTRA_USAGE.get(),
        "Credits" => text::METER_CREDITS.get(),
        "Reset credits" => text::METER_RESET_CREDITS.get(),
        "Code review" => text::METER_CODE_REVIEW.get(),
        other => other,
    };
    if let Some(group) = label.strip_suffix(" · 7d") {
        format!("{group} · {}", text::WINDOW_7D.get())
    } else if let Some(group) = label.strip_suffix(" · 5h") {
        format!("{group} · {}", text::WINDOW_5H.get())
    } else {
        label.to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDisplay {
    pub status: AccountStatus,
    pub label: &'static str,
    pub updated: String,
    pub faded: bool,
}

pub fn account(info: &AccountInfo, status: Option<&AccountStatusInfo>, now: u64) -> AccountDisplay {
    // N-1 데몬은 꼬리가 없으므로 기존 stale·사용량에서 보수적으로 표시한다.
    let kind = status.map_or_else(
        || {
            if info.stale {
                AccountStatus::Stale
            } else if info.usage.is_empty() {
                AccountStatus::Unknown
            } else {
                AccountStatus::Ok
            }
        },
        |s| s.status,
    );
    let label = match kind {
        AccountStatus::Unknown => text::ACCOUNT_STATUS_UNKNOWN,
        AccountStatus::Stale => text::ACCOUNT_STATUS_STALE,
        AccountStatus::NeedsLogin => text::ACCOUNT_LOGIN_REQUIRED,
        AccountStatus::Error => text::ACCOUNT_STATUS_ERROR,
        AccountStatus::Ok => text::ACCOUNT_STATUS_OK,
        AccountStatus::RateLimited => text::ACCOUNT_STATUS_LIMITED,
    }
    .get();
    let updated = status.and_then(|s| s.updated_at);
    AccountDisplay {
        status: kind,
        label,
        updated: updated_text(updated, now),
        faded: info.stale
            || kind == AccountStatus::Stale
            || krill_proto::usage::is_updated_stale(updated, now),
    }
}

pub fn updated_text(updated: Option<u64>, now: u64) -> String {
    let Some(updated) = updated else {
        return text::ACCOUNT_UPDATED_UNKNOWN.get().into();
    };
    let ago = now.saturating_sub(updated);
    let (key, n) = match ago {
        0..10 => return text::ACCOUNT_UPDATED_NOW.get().into(),
        10..60 => (text::ACCOUNT_UPDATED_SECONDS, ago),
        60..3600 => (text::ACCOUNT_UPDATED_MINUTES, ago / 60),
        3600..86400 => (text::ACCOUNT_UPDATED_HOURS, ago / 3600),
        _ => (text::ACCOUNT_UPDATED_DAYS, ago / 86400),
    };
    key.format(&[("n", &n.to_string())])
}

/// 계정 행 상태 점의 로그인 연결 상태(OWE-719). 데몬이 연결 꼬리를 보내면 그 값을 쓰고, 구 데몬·앱 로컬
/// 폴백(`supported: false`)이면 사용량 상태에서 확실한 것만 옮긴다. 모름·오래됨은 로그인 여부를 말해 주지 않아 `None`.
pub fn connection(
    snap: &AccountsSnapshot,
    info: &AccountInfo,
    usage: &AccountDisplay,
) -> Option<ConnectionStatus> {
    if snap.connections.supported {
        return Some(snap.connection_for(&info.id));
    }
    match usage.status {
        AccountStatus::NeedsLogin => Some(ConnectionStatus::NeedsLogin),
        AccountStatus::Ok | AccountStatus::RateLimited => Some(ConnectionStatus::Connected),
        AccountStatus::Error => Some(ConnectionStatus::CheckFailed),
        AccountStatus::Unknown | AccountStatus::Stale => None,
    }
}

pub fn connection_label(status: Option<ConnectionStatus>) -> &'static str {
    match status {
        None => text::ACCOUNT_STATUS_UNKNOWN,
        Some(ConnectionStatus::Checking) => text::CONNECTION_CHECKING,
        Some(ConnectionStatus::Connected) => text::CONNECTION_CONNECTED,
        Some(ConnectionStatus::NeedsLogin) => text::ACCOUNT_LOGIN_REQUIRED,
        Some(ConnectionStatus::NotInstalled) => text::CONNECTION_NOT_INSTALLED,
        Some(ConnectionStatus::CheckFailed) => text::CONNECTION_CHECK_FAILED,
    }
    .get()
}

/// 상태 점 색: 연결됨은 성공색, 조치가 필요한 상태는 경고색, 나머지는 흐린 색.
pub fn connection_color(status: Option<ConnectionStatus>, p: &crate::ui::Palette) -> gpui::Hsla {
    match status {
        Some(ConnectionStatus::Connected) => p.success,
        Some(ConnectionStatus::NeedsLogin | ConnectionStatus::CheckFailed) => p.warning,
        _ => p.muted_foreground,
    }
}

/// 카드·팝오버에 같은 배지와 접근성 설명을 그린다. 오래된 갱신 시각은 흐리게 한다.
pub fn render_status(
    id: String,
    info: &AccountDisplay,
    p: &crate::ui::Palette,
) -> gpui::Stateful<gpui::Div> {
    use gpui::{div, prelude::*};
    let tone = match info.status {
        AccountStatus::NeedsLogin | AccountStatus::Error | AccountStatus::RateLimited => {
            crate::ui::BadgeTone::Warning
        }
        _ => crate::ui::BadgeTone::Muted,
    };
    let debug_id = id.clone();
    div()
        .id(gpui::SharedString::from(id))
        .debug_selector(move || debug_id.clone())
        .role(gpui::Role::Status)
        .aria_label(format!("{} · {}", info.label, info.updated))
        .flex()
        .items_center()
        .gap_2()
        .text_size(crate::ui::TEXT_2XS)
        .child(crate::ui::badge(p, tone, info.label))
        .child(
            div()
                .text_color(
                    p.muted_foreground
                        .opacity(if info.faded { 0.55 } else { 1.0 }),
                )
                .child(info.updated.clone()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_expired_windows_never_look_like_available_quota() {
        assert_eq!(
            window(None, 100),
            WindowDisplay {
                percent: None,
                text: "—".into()
            }
        );
        let expired = Window {
            percent: 89,
            resets_at: Some("99".into()),
        };
        assert_eq!(
            window(Some(&expired), 100),
            WindowDisplay {
                percent: None,
                text: crate::i18n::catalog::settings::ACCOUNT_WINDOW_RESET
                    .get()
                    .into()
            }
        );
        let current = Window {
            percent: 0,
            resets_at: Some("101".into()),
        };
        assert_eq!(
            window(Some(&current), 100),
            WindowDisplay {
                percent: Some(0),
                text: "0%".into()
            }
        );
    }

    /// OWE-719: 데몬 연결 꼬리가 사용량 상태보다 우선한다(로그인된 계정이 사용량 오류로 "로그인 필요"가 되지 않게).
    /// 꼬리가 없으면 모름·오래됨을 로그인 여부로 단정하지 않는다.
    #[test]
    fn connection_prefers_daemon_tail_and_never_guesses_from_unknown_usage() {
        use krill_proto::usage::{AccountConnection, ConnectionsSnapshot};
        let info = AccountInfo {
            id: "a".into(),
            provider: krill_proto::usage::ProviderKind::Claude,
            label: "A".into(),
            email: None,
            profile_dir: "/tmp/a".into(),
            active: true,
            host_default: false,
            stale: false,
            usage: Snapshot::default(),
        };
        let display = |status| AccountDisplay {
            status,
            label: "",
            updated: String::new(),
            faded: false,
        };
        let mut snap = AccountsSnapshot::default();
        for (usage, want) in [
            (
                AccountStatus::NeedsLogin,
                Some(ConnectionStatus::NeedsLogin),
            ),
            (AccountStatus::Ok, Some(ConnectionStatus::Connected)),
            (
                AccountStatus::RateLimited,
                Some(ConnectionStatus::Connected),
            ),
            (AccountStatus::Error, Some(ConnectionStatus::CheckFailed)),
            (AccountStatus::Unknown, None),
            (AccountStatus::Stale, None),
        ] {
            assert_eq!(connection(&snap, &info, &display(usage)), want, "{usage:?}");
        }
        *snap.connections = ConnectionsSnapshot {
            supported: true,
            direct_usage: false,
            accounts: vec![AccountConnection {
                account_id: "a".into(),
                status: ConnectionStatus::Connected,
            }],
        };
        assert_eq!(
            connection(&snap, &info, &display(AccountStatus::NeedsLogin)),
            Some(ConnectionStatus::Connected)
        );
        assert_eq!(
            connection_label(Some(ConnectionStatus::Connected)),
            text::CONNECTION_CONNECTED.get()
        );
    }

    #[test]
    fn status_tail_fallback_and_update_age_are_explicit() {
        let mut info = AccountInfo {
            id: "a".into(),
            provider: krill_proto::usage::ProviderKind::Claude,
            label: "A".into(),
            email: None,
            profile_dir: "/tmp/a".into(),
            active: true,
            host_default: false,
            stale: false,
            usage: Snapshot::default(),
        };
        assert_eq!(account(&info, None, 5000).status, AccountStatus::Unknown);
        info.stale = true;
        assert_eq!(account(&info, None, 5000).status, AccountStatus::Stale);
        for kind in [
            AccountStatus::Unknown,
            AccountStatus::Stale,
            AccountStatus::NeedsLogin,
            AccountStatus::Error,
            AccountStatus::Ok,
            AccountStatus::RateLimited,
        ] {
            let status = AccountStatusInfo {
                account_id: "a".into(),
                status: kind,
                updated_at: Some(4990),
                detail: None,
            };
            let display = account(&info, Some(&status), 5000);
            assert_eq!(display.status, kind);
            assert!(!display.label.is_empty());
            assert_eq!(
                display.updated,
                text::ACCOUNT_UPDATED_SECONDS.format(&[("n", "10")])
            );
        }
        info.stale = false;
        let mut status = AccountStatusInfo {
            account_id: "a".into(),
            status: AccountStatus::Ok,
            updated_at: Some(5000),
            detail: None,
        };
        assert!(!account(&info, Some(&status), 6800).faded);
        assert!(account(&info, Some(&status), 6801).faded);
        status.updated_at = None;
        assert!(account(&info, Some(&status), 5000).faded);
        assert_eq!(
            updated_text(None, 5000),
            text::ACCOUNT_UPDATED_UNKNOWN.get()
        );
        assert_eq!(
            updated_text(Some(6000), 5000),
            text::ACCOUNT_UPDATED_NOW.get()
        );
    }
}
