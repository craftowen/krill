use super::*;

fn device(created_at_ms: u64, last_seen_ms: u64) -> PairedMobileDevice {
    PairedMobileDevice {
        device_id: "dev-1".into(),
        label: "아이폰".into(),
        key_fingerprint: String::new(),
        shared_secret_hash: String::new(),
        created_at_ms,
        last_seen_ms,
        revoked_at_ms: None,
    }
}

#[test]
fn ago_scales_from_just_now_to_days() {
    let now = 10_000_000_000;
    assert_eq!(ago(now, now), text::JUST_NOW.get());
    assert_eq!(ago(now, now + 5_000), text::JUST_NOW.get());
    assert!(
        ago(now, now - 90_000).starts_with("1 "),
        "got {}",
        ago(now, now - 90_000)
    );
    assert!(
        ago(now, now - 3 * 3_600_000).starts_with("3 "),
        "got {}",
        ago(now, now - 3 * 3_600_000)
    );
    assert!(
        ago(now, now - 2 * 86_400_000).starts_with("2 "),
        "got {}",
        ago(now, now - 2 * 86_400_000)
    );
}

#[test]
fn device_line_avoids_paired_duplication_and_handles_never_seen() {
    let now = 10_000_000_000;
    // 오프라인·접속 기록 없음: 상태는 OFFLINE 1회, 마지막 활동은 NEVER.
    let line = device_line(&device(now - 90_000, 0), false, now);
    assert!(line.starts_with("○ "), "got {line}");
    assert!(line.contains(text::OFFLINE.get()), "got {line}");
    assert!(line.contains(text::NEVER.get()), "got {line}");
    assert_eq!(
        line.matches(text::PAIRED.get()).count(),
        1,
        "PAIRED must appear exactly once (paired-at label): {line}"
    );
    // 온라인 기기: CONNECTED 1회 + 경과 시간 표기.
    let line = device_line(
        &device(now - 2 * 86_400_000, now - 3 * 3_600_000),
        true,
        now,
    );
    assert!(line.starts_with("● "), "got {line}");
    assert!(line.contains(text::CONNECTED.get()), "got {line}");
    assert!(!line.contains(text::OFFLINE.get()), "got {line}");
}

#[test]
fn obsolete_pairing_result_cannot_replace_selection_or_finish_current_job() {
    let mut panel = Panel {
        generation: 2,
        mode: ConnectMode::Lan,
        pairing: PairingState::Connecting,
        ..Panel::default()
    };
    assert!(!panel.complete(1, Err("old tunnel timed out".into())));
    assert!(panel.pairing_pending());
    assert!(panel.error.is_none());
    let reply = MobileReply::PairingOffer {
        url: "http://127.0.0.1:1234/#pair=fixture".into(),
        host: "127.0.0.1".into(),
        port: 1234,
        expires_at_ms: u64::MAX,
        mode: ConnectMode::Lan,
        interfaces: Vec::new(),
    };
    assert!(panel.complete(2, Ok(reply.clone())));
    assert!(!panel.pairing_pending());
    panel.copied = true;
    panel.accept(reply);
    assert!(
        panel.copied,
        "polling the same QR must preserve copy feedback"
    );
    let old = MobileReply::GatewayStatus {
        listening: true,
        bind_addrs: Vec::new(),
        mode: ConnectMode::Tunnel,
        tunnel_url: None,
        active_offer: true,
        devices: Vec::new(),
    };
    assert!(!panel.complete(1, Ok(old)));
    assert_eq!(panel.mode, ConnectMode::Lan);
    assert!(panel.offer.is_some());
}
