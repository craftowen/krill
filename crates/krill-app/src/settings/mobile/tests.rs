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
        mode: ConnectMode::Tailscale,
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
        mode: ConnectMode::Tailscale,
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
    assert_eq!(panel.mode, ConnectMode::Tailscale);
    assert!(panel.offer.is_some());
}

#[gpui::test]
fn hidden_mobile_poll_skips_inventory_but_active_pairing_continues(cx: &mut gpui::TestAppContext) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    cx.update(gpui_component::init);
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let mut view = SettingsView::new(cx);
        view.visible = false;
        view.tab = super::super::AdminTab::Mobile;
        view.mobile = Panel::new(Arc::new(move |_| {
            count.fetch_add(1, Ordering::Relaxed);
            Err("fixture offline".into())
        }));
        view.mobile_poll(cx);
        view
    });
    vcx.run_until_parked();
    vcx.executor().advance_clock(Duration::from_secs(3));
    vcx.run_until_parked();
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    vcx.update(|_, cx| view.update(cx, |view, _| view.mobile.pairing = PairingState::Connecting));
    vcx.executor().advance_clock(Duration::from_secs(1));
    vcx.run_until_parked();
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

fn pairing_offer(url: &str, expires_at_ms: u64) -> MobileReply {
    MobileReply::PairingOffer {
        url: url.into(),
        host: "example.test".into(),
        port: 443,
        expires_at_ms,
        mode: ConnectMode::Tailscale,
        interfaces: Vec::new(),
    }
}

#[gpui::test]
fn mobile_enter_mints_first_device_or_standalone_page_without_rotation(
    cx: &mut gpui::TestAppContext,
) {
    use std::sync::Mutex;
    cx.update(gpui_component::init);
    for (mobile_page, has_device, already_mobile) in [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (true, true, false),
        (true, true, true),
    ] {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let record = requests.clone();
        let (view, vcx) = cx.add_window_view(|_, cx| {
            let mut view = SettingsView::new(cx);
            view.mobile = Panel::new(Arc::new(move |request| {
                record.lock().unwrap().push(request.clone());
                Ok(match request {
                    MobileRequest::Status => MobileReply::GatewayStatus {
                        listening: true,
                        bind_addrs: Vec::new(),
                        mode: ConnectMode::Tailscale,
                        tunnel_url: None,
                        active_offer: false,
                        devices: if has_device {
                            vec![device(1, 1)]
                        } else {
                            Vec::new()
                        },
                    },
                    MobileRequest::MintPairingTicket { .. }
                    | MobileRequest::CurrentPairingOffer => {
                        pairing_offer("https://example.test/#pair=fixture", u64::MAX)
                    }
                    MobileRequest::ConnectedDevices => MobileReply::ConnectedDevices(Vec::new()),
                    other => panic!("unexpected request: {other:?}"),
                })
            }));
            view.mobile.address = Some("100.64.1.2:443".into());
            if already_mobile {
                view.tab = super::super::AdminTab::Mobile;
            }
            if mobile_page {
                view.set_mobile_page(true, cx);
            } else {
                view.set_tab(super::super::AdminTab::Mobile, cx);
            }
            view
        });
        vcx.run_until_parked();
        let should_mint = mobile_page || !has_device;
        vcx.update(|_, cx| {
            let mobile = &view.read(cx).mobile;
            assert_eq!(mobile.offer.is_some(), should_mint);
            assert!(!mobile.pending);
            assert!(mobile.error.is_none());
        });
        let requests = requests.lock().unwrap();
        let minted: Vec<_> = requests
            .iter()
            .filter(|r| matches!(r, MobileRequest::MintPairingTicket { .. }))
            .collect();
        assert_eq!(minted.len(), usize::from(should_mint));
        if should_mint {
            assert!(matches!(
                minted[0],
                MobileRequest::MintPairingTicket {
                    mode: ConnectMode::Tailscale,
                    custom_address: Some(address),
                    rotate: false,
                } if address == "100.64.1.2:443"
            ));
        }
    }
}

#[gpui::test]
fn mobile_poll_renews_missing_failed_or_expired_standalone_offer(cx: &mut gpui::TestAppContext) {
    use std::sync::Mutex;
    cx.update(gpui_component::init);
    for (auto_mint, had_offer, current) in [
        (true, true, Err("consumed ticket".into())),
        (true, false, Err("missing ticket".into())),
        (true, true, Ok(MobileReply::Failed("expired ticket".into()))),
        (
            true,
            true,
            Ok(pairing_offer("https://example.test/#old", 0)),
        ),
        (false, true, Err("expired ticket".into())),
        (
            true,
            true,
            Ok(pairing_offer("https://example.test/#valid", u64::MAX)),
        ),
    ] {
        let should_mint = auto_mint
            && !matches!(&current, Ok(MobileReply::PairingOffer { expires_at_ms, .. }) if *expires_at_ms > 0);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let record = requests.clone();
        let (view, vcx) = cx.add_window_view(|_, cx| {
            let mut view = SettingsView::new(cx);
            view.tab = super::super::AdminTab::Mobile;
            view.mobile_page = auto_mint;
            view.mobile = Panel::new(Arc::new(move |request| {
                record.lock().unwrap().push(request.clone());
                match request {
                    MobileRequest::CurrentPairingOffer => current.clone(),
                    MobileRequest::MintPairingTicket { .. } => {
                        Ok(pairing_offer("https://example.test/#renewed", u64::MAX))
                    }
                    MobileRequest::ConnectedDevices => {
                        Ok(MobileReply::ConnectedDevices(Vec::new()))
                    }
                    MobileRequest::Status => Ok(MobileReply::GatewayStatus {
                        listening: true,
                        bind_addrs: Vec::new(),
                        mode: ConnectMode::Tailscale,
                        tunnel_url: None,
                        active_offer: false,
                        devices: vec![device(1, 1)],
                    }),
                    other => panic!("unexpected request: {other:?}"),
                }
            }));
            view.mobile.mode = ConnectMode::Tailscale;
            view.mobile.address = Some("100.64.1.2:443".into());
            if had_offer {
                view.mobile
                    .accept(pairing_offer("https://example.test/#old", u64::MAX));
            }
            view.mobile_poll(cx);
            view
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            let mobile = &view.read(cx).mobile;
            assert_eq!(mobile.offer.is_some(), auto_mint);
            if should_mint {
                assert_eq!(
                    mobile.offer.as_ref().unwrap().0,
                    "https://example.test/#renewed"
                );
            }
        });
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests
                .iter()
                .filter(|r| matches!(r, MobileRequest::MintPairingTicket { .. }))
                .count(),
            usize::from(should_mint)
        );
        assert!(requests.iter().all(|r| !matches!(r, MobileRequest::MintPairingTicket { mode, custom_address, rotate }
            if *mode != ConnectMode::Tailscale || custom_address.as_deref() != Some("100.64.1.2:443") || *rotate)));
    }
}
