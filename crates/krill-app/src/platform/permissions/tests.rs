//! 판정 순수 함수 테스트(MK-160 완료 조건: 경로 3종·알림 상태 4종·`xattr` 유무).
//!
//! 문구 단언은 카탈로그 상대(`permissions` 그룹)로만 한다. 로케일은 프로세스
//! 전역이라 다른 테스트가 바꾸어도 깨지지 않게 한다.

use std::path::Path;
use std::time::Duration;

use crate::i18n::catalog::permissions as text;

use super::*;

fn check_of(check: &Check) -> (&str, Status, FixKind) {
    (check.id.as_str(), check.status, check.fix.kind)
}

#[test]
fn location_dev_is_skip() {
    let check = decide_location(Path::new("/Applications/Krill.app"), false);
    assert_eq!(check_of(&check), (LOCATION_ID, Status::Skip, FixKind::None));
    assert_eq!(check.level, Level::Required);
    assert_eq!(check.summary, text::LOCATION_SKIP_SUMMARY.get());
}

#[test]
fn location_translocation_is_fail() {
    let exe = Path::new("/private/var/folders/xx/T/AppTranslocation/yy/Krill.app");
    let check = decide_location(exe, true);
    assert_eq!(
        check_of(&check),
        (LOCATION_ID, Status::Fail, FixKind::OpenUrl)
    );
    assert!(check.fix.value.contains("Applications"));
    assert_eq!(check.summary, text::LOCATION_TRANSLOCATED_SUMMARY.get());
    assert!(check.detail.contains("AppTranslocation"));
}

#[test]
fn location_dmg_is_fail() {
    let exe = Path::new("/Volumes/Krill 0.1.1/Krill.app");
    let check = decide_location(exe, true);
    assert_eq!(
        check_of(&check),
        (LOCATION_ID, Status::Fail, FixKind::OpenUrl)
    );
    assert_eq!(check.summary, text::LOCATION_DMG_SUMMARY.get());
}

#[test]
fn location_normal_is_ok() {
    let exe = Path::new("/Applications/Krill.app/Contents/MacOS/krill-app");
    let check = decide_location(exe, true);
    assert_eq!(check_of(&check), (LOCATION_ID, Status::Ok, FixKind::None));
    assert_eq!(check.summary, text::LOCATION_OK_SUMMARY.get());
}

#[test]
fn notifications_unavailable_is_skip() {
    let check = decide_notifications(None, None);
    assert_eq!(
        check_of(&check),
        (NOTIFICATIONS_ID, Status::Skip, FixKind::None)
    );
}

#[test]
fn notifications_authorized_is_ok() {
    let check = decide_notifications(Some(NotificationStatus::Authorized), None);
    assert_eq!(
        check_of(&check),
        (NOTIFICATIONS_ID, Status::Ok, FixKind::None)
    );
    assert_eq!(check.summary, text::NOTIFICATIONS_OK_SUMMARY.get());
}

#[test]
fn notifications_ephemeral_is_ok() {
    let check = decide_notifications(Some(NotificationStatus::Ephemeral), None);
    assert_eq!(check.status, Status::Ok);
    assert_eq!(check.summary, text::NOTIFICATIONS_EPHEMERAL_SUMMARY.get());
}

#[test]
fn notifications_provisional_is_warn_with_settings_url() {
    let check = decide_notifications(Some(NotificationStatus::Provisional), Some("dev.krill.app"));
    assert_eq!(
        check_of(&check),
        (NOTIFICATIONS_ID, Status::Warn, FixKind::OpenUrl)
    );
    assert!(check.fix.value.contains("dev.krill.app"));
}

#[test]
fn notifications_not_determined_is_warn_with_button_contract() {
    // `[알림 허용]` 버튼 계약: `warn` + `fix.kind == none`.
    let check = decide_notifications(Some(NotificationStatus::NotDetermined), None);
    assert_eq!(
        check_of(&check),
        (NOTIFICATIONS_ID, Status::Warn, FixKind::None)
    );
    assert_eq!(
        check.summary,
        text::NOTIFICATIONS_NOTDETERMINED_SUMMARY.get()
    );
}

#[test]
fn notifications_denied_links_bundle_settings() {
    let check = decide_notifications(Some(NotificationStatus::Denied), Some("dev.krill.app"));
    assert_eq!(
        check_of(&check),
        (NOTIFICATIONS_ID, Status::Warn, FixKind::OpenUrl)
    );
    assert!(check.fix.value.contains("dev.krill.app"));
    assert_eq!(check.summary, text::NOTIFICATIONS_DENIED_SUMMARY.get());
}

#[test]
fn notifications_denied_without_bundle_uses_fallback_url() {
    let check = decide_notifications(Some(NotificationStatus::Denied), None);
    assert_eq!(check.fix.kind, FixKind::OpenUrl);
    assert!(!check.fix.value.contains("?id="));
    assert_eq!(
        check.fix.value,
        "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
    );
}

#[test]
fn notification_status_from_raw_covers_known_and_unknown() {
    assert_eq!(
        NotificationStatus::from_raw(2),
        NotificationStatus::Authorized
    );
    assert_eq!(
        NotificationStatus::from_raw(99),
        NotificationStatus::NotDetermined
    );
}

#[test]
fn quarantine_dev_is_skip() {
    let check = decide_quarantine(true, false);
    assert_eq!(
        check_of(&check),
        (QUARANTINE_ID, Status::Skip, FixKind::None)
    );
}

#[test]
fn quarantine_present_is_warn() {
    let check = decide_quarantine(true, true);
    assert_eq!(
        check_of(&check),
        (QUARANTINE_ID, Status::Warn, FixKind::None)
    );
    assert_eq!(check.summary, text::QUARANTINE_WARN_SUMMARY.get());
}

#[test]
fn quarantine_absent_is_ok() {
    let check = decide_quarantine(false, true);
    assert_eq!(check_of(&check), (QUARANTINE_ID, Status::Ok, FixKind::None));
}

#[test]
fn quarantine_attr_roundtrip_on_temp_file() {
    let path = std::env::temp_dir().join(format!("mk130-quarantine-{}", std::process::id()));
    std::fs::write(&path, b"probe").unwrap();
    assert!(!has_quarantine_attr(&path));
    rustix::fs::setxattr(
        &path,
        "com.apple.quarantine",
        b"0081;6967494d;test;00000000-0000-0000-0000-000000000000".as_slice(),
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    assert!(has_quarantine_attr(&path));
    rustix::fs::removexattr(&path, "com.apple.quarantine").unwrap();
    assert!(!has_quarantine_attr(&path));
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn folders_idle_is_skip() {
    let check = folders_idle_check();
    assert_eq!(check_of(&check), (FOLDERS_ID, Status::Skip, FixKind::None));
    assert_eq!(check.level, Level::Optional);
}

#[test]
fn folder_probe_empty_falls_back_to_idle() {
    assert_eq!(decide_folder_probe(&[]).status, Status::Skip);
}

#[test]
fn folder_probe_all_readable_is_ok() {
    let results = FolderKind::all().map(|kind| (kind, true));
    let check = decide_folder_probe(&results);
    assert_eq!(check_of(&check), (FOLDERS_ID, Status::Ok, FixKind::None));
    assert_eq!(check.summary, text::FOLDERS_OK_SUMMARY.get());
}

#[test]
fn folder_probe_partial_denial_is_fail_with_folder_name() {
    let results = [
        (FolderKind::Documents, true),
        (FolderKind::Desktop, false),
        (FolderKind::Downloads, true),
    ];
    let check = decide_folder_probe(&results);
    assert_eq!(
        check_of(&check),
        (FOLDERS_ID, Status::Fail, FixKind::OpenUrl)
    );
    assert!(check.summary.contains(text::FOLDER_DESKTOP.get()));
    assert!(check.fix.value.contains("Privacy_FilesAndFolders"));
}

#[test]
fn full_disk_allowed_is_ok() {
    let check = decide_full_disk(true);
    assert_eq!(check_of(&check), (FULL_DISK_ID, Status::Ok, FixKind::None));
}

#[test]
fn full_disk_missing_is_warn_with_guidance() {
    let check = decide_full_disk(false);
    assert_eq!(
        check_of(&check),
        (FULL_DISK_ID, Status::Warn, FixKind::OpenUrl)
    );
    assert!(check.fix.value.contains("Privacy_AllFiles"));
    assert_eq!(check.summary, text::FULLDISK_WARN_SUMMARY.get());
    assert_eq!(check.detail, text::FULLDISK_WARN_DETAIL.get());
}

#[test]
fn settings_url_builders_cover_bundle_and_fallback() {
    assert_eq!(
        notifications_settings_url(Some("dev.krill.app")),
        "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=dev.krill.app"
    );
    assert!(!FOLDERS_SETTINGS_URL.is_empty());
    assert!(!FULL_DISK_SETTINGS_URL.is_empty());
}

#[test]
fn test_runner_is_not_bundled_so_queries_short_circuit() {
    assert!(!is_bundled());
    assert_eq!(query_notification_status(Duration::from_secs(1)), None);
    assert_eq!(
        request_notification_authorization(Duration::from_secs(1)),
        AuthRequestOutcome::Unavailable
    );
}

#[test]
fn folder_probe_smoke_on_temp_dir() {
    let dir = std::env::temp_dir();
    assert!(probe_folder_readable(&dir));
    assert!(!probe_folder_readable(&dir.join("mk130-no-such-dir")));
}

#[test]
fn open_first_available_with_no_candidates_is_false() {
    assert!(!open_first_available(&[]));
}

#[test]
fn check_serializes_to_mk128_wire_shape() {
    let check = decide_full_disk(false);
    let value = serde_json::to_value(&check).unwrap();
    assert_eq!(value["id"], FULL_DISK_ID);
    assert_eq!(value["level"], "optional");
    assert_eq!(value["status"], "warn");
    assert!(value["summary"].is_string());
    assert!(value["detail"].is_string());
    assert_eq!(value["fix"]["kind"], "open_url");
    assert_eq!(value["fix"]["value"], FULL_DISK_SETTINGS_URL);
}

#[test]
fn computer_use_permission_kind_roundtrips_and_probes_smoke() {
    assert_eq!(
        ComputerUsePermissionKind::parse("accessibility"),
        Some(ComputerUsePermissionKind::Accessibility)
    );
    assert_eq!(
        ComputerUsePermissionKind::parse("screenshots"),
        Some(ComputerUsePermissionKind::Screenshots)
    );
    assert_eq!(
        ComputerUsePermissionKind::parse("screen-recording"),
        Some(ComputerUsePermissionKind::Screenshots)
    );
    assert_eq!(ComputerUsePermissionKind::parse("unknown"), None);
    assert_eq!(
        ComputerUsePermissionKind::Accessibility.id(),
        "accessibility"
    );
    assert_eq!(ComputerUsePermissionKind::Screenshots.id(), "screenshots");
    assert_eq!(ACCESSIBILITY_ID, "app.accessibility");
    assert_eq!(SCREEN_RECORDING_ID, "app.screen_recording");
    assert!(
        ComputerUsePermissionKind::Accessibility
            .settings_url()
            .contains("Privacy_Accessibility")
    );
    assert!(
        ComputerUsePermissionKind::Screenshots
            .settings_url()
            .contains("Privacy_ScreenCapture")
    );
    let perms = probe_computer_use_permissions();
    assert_eq!(
        perms.all_granted(),
        perms.is_granted(ComputerUsePermissionKind::Accessibility)
            && perms.is_granted(ComputerUsePermissionKind::Screenshots)
    );
}
