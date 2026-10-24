//! 첫 실제 발송 시점에만 알림 권한을 요청한다. 요청 중에는 태그별 최신 알림을 보관한다.

use std::collections::HashMap;
use std::time::Duration;

use gpui::{App, Global, SharedString, SystemNotification};

use super::{FixNeeds, FixOutcome, atext, dismiss_need, perm, publish};

#[derive(Default)]
struct NotificationGate {
    pending: HashMap<SharedString, SystemNotification>,
    requesting: bool,
    requested: bool,
    granted: bool,
}
impl Global for NotificationGate {}

#[derive(Default)]
struct PermissionWatch {
    running: bool,
}
impl Global for PermissionWatch {}

/// 알림이 실제로 필요한 시점의 진입점. 허용 전 알림을 버리지 않고 요청 결과 뒤 발송한다.
pub fn show_notification(notification: SystemNotification, cx: &mut App) {
    // 테스트 플랫폼엔 OS 권한이 없다. `cfg(test)`는 bin 테스트가 링크하는 lib에서 꺼지므로 런타임으로 가른다.
    if cx.background_executor().is_test() {
        cx.show_system_notification(notification);
    } else {
        deliver_with(notification, authorize, cx);
    }
}

fn deliver_with(
    notification: SystemNotification,
    authorize: impl FnOnce(bool) -> FixOutcome + Send + 'static,
    cx: &mut App,
) {
    let gate = cx.default_global::<NotificationGate>();
    if gate.granted {
        cx.show_system_notification(notification);
        return;
    }
    gate.pending.insert(notification.tag.clone(), notification);
    if gate.requesting {
        return;
    }
    gate.requesting = true;
    let requested = std::mem::replace(&mut gate.requested, true);
    let work = cx
        .background_executor()
        .spawn(async move { authorize(requested) });
    cx.spawn(async move |cx| {
        let result = work.await;
        cx.update(|cx| {
            let gate = cx.default_global::<NotificationGate>();
            gate.requesting = false;
            gate.granted = result.is_ok();
            let pending = std::mem::take(&mut gate.pending);
            match result {
                Ok(()) => {
                    dismiss_need(perm::NOTIFICATIONS_ID, cx);
                    for (_, notification) in pending {
                        cx.show_system_notification(notification);
                    }
                }
                Err(error) => publish(perm::NOTIFICATIONS_ID, error, cx),
            }
        });
    })
    .detach();
}

/// 시스템 설정이 앞에 있어도 허용된 알림 안내를 내린다. 안내가 없으면 조회를 멈춘다.
pub(super) fn watch_permission(cx: &mut App) {
    // 테스트 플랫폼에는 OS 권한이 없다. 테스트에서는 조회 함수를 주입한다.
    if !cx.background_executor().is_test() {
        watch_permission_with(
            || perm::query_notification_status(Duration::from_secs(2)),
            cx,
        );
    }
}

fn watch_permission_with(
    query: impl Fn() -> Option<perm::NotificationStatus> + Send + Sync + 'static,
    cx: &mut App,
) {
    let watch = cx.default_global::<PermissionWatch>();
    if watch.running {
        return;
    }
    watch.running = true;
    let query = std::sync::Arc::new(query);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let visible = cx.update(|cx| {
                cx.try_global::<FixNeeds>().is_some_and(|needs| {
                    needs
                        .items
                        .iter()
                        .any(|need| need.check_id == perm::NOTIFICATIONS_ID)
                })
            });
            if !visible {
                break;
            }
            let query = query.clone();
            let status = cx.background_executor().spawn(async move { query() }).await;
            if matches!(
                status,
                Some(perm::NotificationStatus::Authorized | perm::NotificationStatus::Ephemeral)
            ) {
                cx.update(|cx| dismiss_need(perm::NOTIFICATIONS_ID, cx));
                break;
            }
        }
        cx.update(|cx| cx.default_global::<PermissionWatch>().running = false);
    })
    .detach();
}

fn authorize(requested: bool) -> FixOutcome {
    authorize_status(
        perm::query_notification_status(Duration::from_secs(2)),
        requested,
        || perm::request_notification_authorization(Duration::from_secs(60)),
    )
}

fn authorize_status(
    status: Option<perm::NotificationStatus>,
    requested: bool,
    request: impl FnOnce() -> perm::AuthRequestOutcome,
) -> FixOutcome {
    match status {
        Some(
            perm::NotificationStatus::Authorized
            | perm::NotificationStatus::Ephemeral
            | perm::NotificationStatus::Provisional,
        ) => Ok(()),
        Some(perm::NotificationStatus::NotDetermined) if !requested => match request() {
            perm::AuthRequestOutcome::Granted => Ok(()),
            perm::AuthRequestOutcome::Failed(error) => {
                Err(atext::NOTIFICATION_ERROR.format(&[("error", &error)]))
            }
            perm::AuthRequestOutcome::Denied => Err(atext::NOTIFICATION_DENIED.get().to_owned()),
            perm::AuthRequestOutcome::Timeout => Err(atext::NOTIFICATION_TIMEOUT.get().to_owned()),
            perm::AuthRequestOutcome::Unavailable => {
                Err(atext::NOTIFICATION_UNAVAILABLE.get().to_owned())
            }
        },
        Some(_) => Err(atext::NOTIFICATION_DENIED.get().to_owned()),
        None => Err(atext::NOTIFICATION_UNAVAILABLE.get().to_owned()),
    }
}

/// 앱 복귀 뒤 다음 실제 알림에서 권한 변경을 다시 확인한다.
pub(super) fn invalidate(cx: &mut App) {
    if cx.try_global::<NotificationGate>().is_some() {
        cx.global_mut::<NotificationGate>().granted = false;
    }
}

/// 동의 대기 중 취소된 알림이 나중에 다시 뜨지 않게 큐에서도 제거한다.
pub fn dismiss_notification(tag: &SharedString, cx: &mut App) {
    if cx.try_global::<NotificationGate>().is_some() {
        cx.global_mut::<NotificationGate>().pending.remove(tag);
    }
    cx.dismiss_system_notification(tag);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[gpui::test]
    fn settings_permission_change_clears_only_notification_need(cx: &mut gpui::TestAppContext) {
        let status = Arc::new(AtomicUsize::new(1));
        let queries = Arc::new(AtomicUsize::new(0));
        cx.update(|cx| {
            publish(perm::NOTIFICATIONS_ID, "notifications denied".into(), cx);
            publish(perm::FOLDERS_ID, "folder access denied".into(), cx);
            let status = status.clone();
            let queries = queries.clone();
            watch_permission_with(
                move || {
                    queries.fetch_add(1, Ordering::SeqCst);
                    match status.load(Ordering::SeqCst) {
                        1 => Some(perm::NotificationStatus::Denied),
                        2 => Some(perm::NotificationStatus::Authorized),
                        3 => Some(perm::NotificationStatus::Provisional),
                        _ => None,
                    }
                },
                cx,
            );
            watch_permission_with(|| panic!("duplicate watcher"), cx);
        });
        cx.run_until_parked();
        for value in [1, 0, 3] {
            status.store(value, Ordering::SeqCst);
            cx.executor().advance_clock(Duration::from_secs(1));
            cx.run_until_parked();
            cx.update(|cx| assert_eq!(cx.global::<FixNeeds>().items.len(), 2));
        }
        status.store(2, Ordering::SeqCst);
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(cx.global::<FixNeeds>().items.len(), 1);
            assert_eq!(cx.global::<FixNeeds>().items[0].check_id, perm::FOLDERS_ID);
            assert!(!cx.global::<PermissionWatch>().running);
        });
        assert_eq!(queries.load(Ordering::SeqCst), 4);
        cx.executor().advance_clock(Duration::from_secs(5));
        cx.run_until_parked();
        assert_eq!(queries.load(Ordering::SeqCst), 4);
    }

    #[gpui::test]
    fn closing_permission_notice_stops_checks_and_can_watch_again(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            publish(perm::NOTIFICATIONS_ID, "notifications denied".into(), cx);
            watch_permission_with(|| panic!("closed notice must not query the OS"), cx);
            dismiss_need(perm::NOTIFICATIONS_ID, cx);
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!cx.global::<PermissionWatch>().running);
            publish(
                perm::NOTIFICATIONS_ID,
                "notifications denied again".into(),
                cx,
            );
            watch_permission_with(|| Some(perm::NotificationStatus::Ephemeral), cx);
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        cx.update(|cx| assert!(cx.global::<FixNeeds>().items.is_empty()));
    }

    #[test]
    fn permission_is_requested_once_and_only_if_not_determined() {
        use perm::{AuthRequestOutcome as Outcome, NotificationStatus as Status};
        assert!(authorize_status(Some(Status::NotDetermined), false, || Outcome::Granted).is_ok());
        assert!(
            authorize_status(Some(Status::NotDetermined), true, || panic!(
                "repeated request"
            ))
            .is_err()
        );
        assert!(
            authorize_status(Some(Status::Denied), false, || panic!("denied request")).is_err()
        );
        assert!(
            authorize_status(Some(Status::Authorized), false, || panic!(
                "already authorized"
            ))
            .is_ok()
        );
        assert!(authorize_status(None, false, || panic!("unbundled request")).is_err());
        let error = authorize_status(Some(Status::NotDetermined), false, || {
            Outcome::Failed("system error".to_owned())
        })
        .unwrap_err();
        assert!(error.contains("system error"));
        assert!(authorize_status(Some(Status::NotDetermined), false, || Outcome::Timeout).is_err());
    }

    #[gpui::test]
    fn first_notification_waits_for_permission_and_then_delivers(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_app_identity(krill_proto::daemon::BUNDLE_ID, "Krill");
            publish(perm::NOTIFICATIONS_ID, "previous denial".into(), cx);
            deliver_with(
                SystemNotification {
                    tag: "first".into(),
                    title: "title".into(),
                    body: "body".into(),
                    actions: Vec::new(),
                },
                |_| Ok(()),
                cx,
            );
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(cx.global::<NotificationGate>().granted);
            assert!(cx.global::<FixNeeds>().items.is_empty());
        });
        assert_eq!(cx.delivered_system_notifications().len(), 1);
    }
    #[gpui::test]
    fn dismissed_pending_notification_is_not_delivered_after_consent(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            cx.set_app_identity(krill_proto::daemon::BUNDLE_ID, "Krill");
            deliver_with(
                SystemNotification {
                    tag: "cancelled".into(),
                    title: "title".into(),
                    body: "body".into(),
                    actions: Vec::new(),
                },
                |_| Ok(()),
                cx,
            );
            dismiss_notification(&"cancelled".into(), cx);
        });
        cx.run_until_parked();
        assert!(cx.delivered_system_notifications().is_empty());
    }

    #[gpui::test]
    fn denied_notification_publishes_reason_without_delivery(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_app_identity(krill_proto::daemon::BUNDLE_ID, "Krill");
            deliver_with(
                SystemNotification {
                    tag: "denied".into(),
                    title: "title".into(),
                    body: "body".into(),
                    actions: Vec::new(),
                },
                |_| Err("fixture denied".to_owned()),
                cx,
            );
        });
        cx.run_until_parked();
        assert!(cx.delivered_system_notifications().is_empty());
        cx.update(|cx| {
            assert_eq!(
                cx.global::<super::super::FixNeeds>().items[0].message,
                "fixture denied"
            );
        });
    }
}
