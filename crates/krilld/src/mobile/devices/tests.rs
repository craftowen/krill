use super::*;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
#[test]
fn registry_atomic_permissions_reopen_and_revoke() {
    let dir = std::env::temp_dir().join(format!("mk-mobile-{}", hex(&random().unwrap())));
    let mut devices = Devices::open(Some(&dir)).unwrap();
    let ticket = devices.mint(100, false).unwrap();
    let (d, credential) = devices.pair(&ticket.token, "phone", 101).unwrap();
    assert!(devices.pair(&ticket.token, "again", 102).is_err());
    assert!(d.shared_secret_hash.is_empty());
    assert!(
        devices
            .list()
            .iter()
            .all(|d| d.shared_secret_hash.is_empty())
    );
    let path = dir.join("mobile-devices.json");
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    let mut reopened = Devices::open(Some(&dir)).unwrap();
    assert_eq!(
        reopened.device_secret(&d.device_id).unwrap().as_slice(),
        digest::digest(&digest::SHA256, &credential).as_ref()
    );
    reopened.seen(&d.device_id, 103).unwrap();
    assert_eq!(reopened.list().first().unwrap().last_seen_ms, 103);
    assert_eq!(reopened.revoke(Some(&d.device_id), false, 104).unwrap(), 1);
    assert!(reopened.device_secret(&d.device_id).is_err());
    assert_eq!(Devices::open(Some(&dir)).unwrap().active_count(), 0);
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Devices::open(Some(&dir)).is_err());
    fs::remove_dir_all(dir).unwrap();
}
#[test]
fn expiry_rotation_one_time_and_rate_limit() {
    let mut d = Devices::open(None).unwrap();
    let t = d.mint(10, false).unwrap();
    assert_eq!(d.mint(11, false).unwrap(), t);
    let rotated = d.mint(11, true).unwrap();
    assert!(d.pairing_secret(&t.token, 12).is_err());
    assert!(
        d.pairing_secret(&rotated.token, 11 + TICKET_TTL_MS)
            .is_err()
    );
    for _ in 0..5 {
        d.attempt(20).unwrap();
    }
    d.mint(21, true).unwrap();
    assert!(d.attempt(21).is_err());
    assert!(d.attempt(60_020).is_ok());
    let t = d.mint(60_020, true).unwrap();
    d.pair(&t.token, "phone", 60_021).unwrap();
    assert!(d.ticket(60_022).is_none());
    assert!(d.pair(&t.token, "duplicate", 60_022).is_err());
}
#[test]
fn failed_save_does_not_enroll_or_consume_ticket() {
    let dir = std::env::temp_dir().join(format!("mk-mobile-{}", hex(&random().unwrap())));
    fs::write(&dir, b"not a directory").unwrap();
    let mut d = Devices::open(None).unwrap();
    d.path = Some(dir.join("mobile-devices.json"));
    let t = d.mint(1, true).unwrap();
    assert!(d.pair(&t.token, "phone", 2).is_err());
    assert_eq!(d.active_count(), 0);
    assert!(d.ticket(2).is_some());
    fs::remove_file(dir).unwrap();
}

#[test]
fn existing_user_owned_readable_state_directory_keeps_registry_private() {
    let dir = std::env::temp_dir().join(format!("mk-mobile-{}", hex(&random().unwrap())));
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let mut d = Devices::open(Some(&dir)).unwrap();
    let t = d.mint(1, true).unwrap();
    d.pair(&t.token, "phone", 2).unwrap();
    assert_eq!(
        fs::metadata(dir.join("mobile-devices.json"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn seen_within_flush_window_stays_in_memory_until_next_save() {
    let dir = std::env::temp_dir().join(format!("mk-mobile-{}", hex(&random().unwrap())));
    let path = dir.join("mobile-devices.json");
    let mut devices = Devices::open(Some(&dir)).unwrap();
    let t = devices.mint(1_000, true).unwrap();
    let (d, _) = devices.pair(&t.token, "phone", 1_000).unwrap();
    let paired = fs::read(&path).unwrap();
    // 저장 뒤 창 안의 재접속 두 번은 파일을 건드리지 않고 메모리 값만 바꾼다.
    devices.seen(&d.device_id, 2_000).unwrap();
    devices.seen(&d.device_id, 3_000).unwrap();
    assert_eq!(fs::read(&path).unwrap(), paired);
    assert_eq!(devices.list().first().unwrap().last_seen_ms, 3_000);
    // 창이 지나면 디스크에 남긴다.
    let late = 1_000 + SEEN_FLUSH_MS;
    devices.seen(&d.device_id, late).unwrap();
    let reopened = Devices::open(Some(&dir)).unwrap();
    assert_eq!(reopened.list().first().unwrap().last_seen_ms, late);
    // 회수는 창과 무관하게 즉시 영속한다.
    devices.seen(&d.device_id, late + 1).unwrap();
    assert_eq!(
        devices.revoke(Some(&d.device_id), false, late + 2).unwrap(),
        1
    );
    let reopened = Devices::open(Some(&dir)).unwrap();
    assert_eq!(reopened.active_count(), 0);
    assert_eq!(reopened.list().first().unwrap().last_seen_ms, late + 1);
    fs::remove_dir_all(dir).unwrap();
}
