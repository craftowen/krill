use super::*;

#[test]
fn status_only_advertises_a_bound_tailnet_backup() {
    let mut panel = Panel::new(Arc::new(|_| Err("unused".into())));
    let status = |bind_addrs| MobileReply::GatewayStatus {
        listening: true,
        bind_addrs,
        mode: ConnectMode::Tunnel,
        tunnel_url: None,
        active_offer: false,
        devices: Vec::new(),
    };
    panel.accept(status(vec![
        "127.0.0.1:1234".into(),
        "100.64.1.2:1234".into(),
    ]));
    assert_eq!(panel.backup_address.as_deref(), Some("100.64.1.2:1234"));
    panel.accept(status(vec![
        "127.0.0.1:1234".into(),
        "192.168.1.2:1234".into(),
    ]));
    assert!(panel.backup_address.is_none());
}

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

/// Windows 100% 배율의 168px QR은 칸이 2·3px로 번갈았다. 모든 배율에서 칸은 정수 기기 픽셀이고 4px 이상이다.
#[test]
fn qr_side_keeps_whole_device_pixel_modules() {
    assert_eq!(qr_side(px(168.), 1.), px(260.));
    assert_eq!(qr_side(px(168.), 2.), px(162.5));
    assert_eq!(qr_side(px(220.), 2.), px(227.5));
    assert_eq!(qr_side(px(168.), 0.), px(260.));
    assert_eq!(
        px(QR_MODULES),
        px(f32::from(u16::try_from(SIZE + 8).unwrap()))
    );
    for target in [168., 220.] {
        for scale in [1., 1.25, 1.5, 1.75, 2., 3.] {
            let module = f32::from(qr_side(px(target), scale)) * scale / 65.;
            assert!(
                (module - module.round()).abs() < 1e-3 && module.round() >= 4.,
                "{target}px @ {scale}x -> {module} device px per module"
            );
        }
    }
}

#[test]
fn device_line_avoids_paired_duplication_and_handles_never_seen() {
    let now = 10_000_000_000;
    // 오프라인·접속 기록 없음: 상태는 OFFLINE 1회, 마지막 활동은 NEVER.
    let line = device_line(&device(now - 90_000, 0), false, now);
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
    assert!(line.contains(text::CONNECTED.get()), "got {line}");
    assert!(!line.contains(text::OFFLINE.get()), "got {line}");
}

/// 값이 같으면 다시 그리지 않는다. 화면에 있을 때만 카운트다운과 경과 표시가 시간을 따른다.
#[test]
fn poll_redraws_only_when_shown_values_change() {
    let mut panel = Panel::new(Arc::new(|_| Err("unused".into())));
    let start = 10_000_000_000 - 10_000_000_000 % 60_000;
    assert!(panel.take_shown_change(start, true));
    assert!(!panel.take_shown_change(start + 1_000, true));
    assert!(panel.take_shown_change(start + 60_000, true), "ago text");

    panel.offer = Some(("u".into(), start + 120_000, Qr::encode("u").unwrap()));
    assert!(panel.take_shown_change(start + 60_100, true));
    assert!(!panel.take_shown_change(start + 60_900, true));
    assert!(panel.take_shown_change(start + 61_100, true), "countdown");

    assert!(panel.take_shown_change(start + 61_100, false));
    assert!(
        !panel.take_shown_change(start + 200_000, false),
        "off screen"
    );
    panel.devices.push(device(start, 0));
    assert!(
        panel.take_shown_change(start + 200_000, false),
        "real change"
    );
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
                    MobileRequest::Workflow(_) => MobileReply::Workflow("{\"ok\":true,\"data\":{\"requires_sign_in\":false,\"signed_in\":false,\"pending\":false}}".into()),
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
                    MobileRequest::Workflow(_) => Ok(MobileReply::Workflow("{\"ok\":true,\"data\":{\"requires_sign_in\":false,\"signed_in\":false,\"pending\":false}}".into())),
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

/// 실제 티켓과 같은 길이(203바이트)·모양의 가짜 페어링 URL. 실제 토큰이 아니다.
const FIXTURE_TICKET: &str = concat!(
    "https://krill-mobile.pages.dev/m/0000000000000000/#pair=",
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    "&key=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    "&r=fixture-relay-000000000000000000"
);

fn offer_view(
    cx: &mut gpui::TestAppContext,
    page: bool,
) -> (gpui::Entity<SettingsView>, &mut gpui::VisualTestContext) {
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let mut view = SettingsView::new(cx);
        view.tab = super::super::AdminTab::Mobile;
        view.mobile_page = page;
        view.mobile = Panel::new(Arc::new(|_| Err("fixture offline".into())));
        view.mobile.accept(pairing_offer(
            FIXTURE_TICKET,
            krill_proto::mobile::client::now_ms() + 600_000,
        ));
        view
    });
    (view, vcx)
}

fn redraw(vcx: &mut gpui::VisualTestContext, width: f32, scale: f32) {
    vcx.simulate_scale_factor_change(scale);
    vcx.simulate_resize(gpui::size(px(width), px(2000.)));
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();
}

fn span(b: gpui::Bounds<gpui::Pixels>) -> (f32, f32) {
    (f32::from(b.left()), f32::from(b.right()))
}

/// 화면이 덮는 가로 범위 안에 `inner`가 다 들어가는지(세로는 스크롤된다).
fn inside(inner: (f32, f32), outer: (f32, f32)) -> bool {
    inner.0 >= outer.0 - 0.01 && inner.1 <= outer.1 + 0.01
}

/// 단독 페이지(메인 창 터미널 영역)와 설정 탭 QR이 지원 폭·배율에서 잘리지 않고,
/// 그린 QR 칸이 정수 기기 픽셀인지 실제 gpui 레이아웃 경계로 확인한다.
#[gpui::test]
fn mobile_qr_cards_fit_without_clipping_at_supported_widths(cx: &mut gpui::TestAppContext) {
    let mut failures = Vec::new();
    {
        let (_view, vcx) = offer_view(cx, true);
        // 360 = 최소 창 폭(사이드바 접힘), 959 = 기본 1200px 창 − 사이드바 240 − 구분선.
        for width in [360., 480., 720., 959., 1100.] {
            for scale in [1., 1.25, 1.5, 2.] {
                redraw(vcx, width, scale);
                let qr = vcx.debug_bounds("mobile-page-qr").expect("page qr");
                let card = vcx.debug_bounds("mobile-page-card").expect("page card");
                let copy = vcx.debug_bounds("mobile-page-copy").expect("page copy");
                let new_qr = vcx.debug_bounds("mobile-page-new-qr").expect("page new qr");
                let phone = vcx.debug_bounds("mobile-page-phone").expect("phone");
                // QR 상자는 p_2 흰 여백 안에 캔버스를 둔다.
                let canvas = (f32::from(qr.left()) + 8., f32::from(qr.right()) - 8.);
                let module = (canvas.1 - canvas.0) * scale / 65.;
                eprintln!(
                    "page w={width} @{scale}x card={:?} qr_box={:?} canvas={canvas:?} module={module} copy={:?} new_qr={:?} phone_top={} card_top={}",
                    span(card),
                    span(qr),
                    span(copy),
                    span(new_qr),
                    f32::from(phone.top()),
                    f32::from(card.top())
                );
                let view = (0., width);
                if (module - module.round()).abs() > 1e-3 || module.round() < 4. {
                    failures.push(format!("page w={width} @{scale}x module {module}"));
                }
                if width >= 959. && f32::from(phone.top()) >= f32::from(card.bottom()) {
                    failures.push(format!("page w={width} @{scale}x phone wrapped below card"));
                }
                for (name, b) in [
                    ("qr", canvas),
                    ("copy", span(copy)),
                    ("new_qr", span(new_qr)),
                ] {
                    if !inside(b, span(card)) || !inside(b, view) {
                        failures.push(format!("page w={width} @{scale}x {name} clipped"));
                    }
                }
            }
        }
    }
    {
        let (_view, vcx) = offer_view(cx, false);
        // 좁은 설정 창은 상단 탐색으로 전환해 QR이 들어갈 본문 폭을 확보한다.
        for width in [360., 560., 580., 598., 800., 1100.] {
            for scale in [1., 1.25, 1.5, 2.] {
                redraw(vcx, width, scale);
                let qr_box = vcx.debug_bounds("mobile-qr-card").expect("tab qr card");
                let copy = vcx.debug_bounds("mobile-copy-link").expect("tab copy");
                let panel = vcx
                    .debug_bounds("admin-mobile-pair-section")
                    .expect("pair panel");
                // QR 상자는 캔버스 폭 그대로다. 패널의 안쪽 여백은 12이고, 배율에 따라 테두리 픽셀이 반올림된다.
                let side = f32::from(qr_side(px(148.), scale));
                let qr = span(qr_box);
                let content = (
                    f32::from(panel.left()) + 12.,
                    f32::from(panel.right()) - 12.,
                );
                assert!((qr.1 - qr.0 - side).abs() < 0.01, "{qr:?} side={side}");
                eprintln!(
                    "tab w={width} @{scale}x content={content:?} qr={qr:?} side={side} copy={:?}",
                    span(copy)
                );
                let view = (0., width);
                for (name, b) in [("qr", qr), ("copy", span(copy))] {
                    if !inside(b, content) || !inside(b, view) {
                        failures.push(format!("tab w={width} @{scale}x {name} clipped"));
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// 두 화면의 복사 버튼은 QR에 담긴 것과 같은 티켓 URL을 복사한다.
#[gpui::test]
fn mobile_copy_buttons_copy_the_url_encoded_in_the_qr(cx: &mut gpui::TestAppContext) {
    assert_eq!(FIXTURE_TICKET.len(), 203);
    for (page, selector) in [(true, "mobile-page-copy"), (false, "mobile-copy-link")] {
        let (view, vcx) = offer_view(cx, page);
        redraw(vcx, 1100., 1.);
        let button = vcx.debug_bounds(selector).expect("copy button");
        vcx.simulate_click(button.center(), gpui::Modifiers::default());
        vcx.run_until_parked();
        let copied = vcx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("clipboard text");
        assert_eq!(copied, FIXTURE_TICKET, "{selector}");
        vcx.update(|_, cx| {
            let mobile = &view.read(cx).mobile;
            let (url, _, qr) = mobile.offer.as_ref().expect("offer");
            assert_eq!(url, &copied);
            assert_eq!(qr, &Qr::encode(&copied).unwrap(), "{selector}");
            assert_eq!(mobile.copied_url.as_deref(), Some(FIXTURE_TICKET));
        });
    }
}

/// 긴 설명·주소·기기 상태가 QR이나 옆 열과 겹치지 않고, QR 상자가 정사각형을 유지한다.
#[gpui::test]
fn mobile_page_long_details_stay_in_their_columns(cx: &mut gpui::TestAppContext) {
    let (view, vcx) = offer_view(cx, true);
    view.update(vcx, |view, _| {
        view.mobile.backup_address = Some("100.113.52.255:65535".into());
        view.mobile.account.email = Some("very-long-account-address@example.test".into());
        view.mobile.devices = (0..12)
            .map(|index| PairedMobileDevice {
                device_id: format!("device-{index}"),
                label: "긴 이름의 휴대폰 · Mobile browser iPhone / iPad (Safari)".into(),
                key_fingerprint: "fixture-fingerprint".into(),
                ..device(1, 0)
            })
            .collect();
    });
    for language in [crate::i18n::Language::Ko, crate::i18n::Language::En] {
        vcx.update(|_, cx| crate::i18n::set_language(language, cx));
        for width in [360., 480., 720., 959., 1100.] {
            for scale in [1., 1.25, 1.5, 2.] {
                redraw(vcx, width, scale);
                let card = vcx.debug_bounds("mobile-page-card").unwrap();
                let description = vcx.debug_bounds("mobile-page-description").unwrap();
                let address = vcx.debug_bounds("mobile-page-address").unwrap();
                let qr = vcx.debug_bounds("mobile-page-qr").unwrap();
                let phone = vcx.debug_bounds("mobile-page-phone").unwrap();
                let item = vcx.debug_bounds("mobile-page-device-0").unwrap();
                let status = vcx.debug_bounds("mobile-page-device-status-0").unwrap();
                assert!(
                    inside(span(description), span(card)),
                    "description {width} @{scale}"
                );
                assert!(
                    inside(span(address), span(description)),
                    "address {width} @{scale}"
                );
                assert!(inside(span(qr), span(card)), "QR box {width} @{scale}");
                assert!(
                    (f32::from(qr.size.width - qr.size.height)).abs() < 0.01,
                    "QR stretched {qr:?}"
                );
                assert!(
                    description.right() <= qr.left() || qr.top() >= description.bottom(),
                    "columns overlap {width} @{scale}"
                );
                assert!(inside(span(status), span(item)) && inside(span(item), span(phone)));
                assert!(
                    status.bottom() <= item.bottom(),
                    "status clipped {width} @{scale}"
                );
                assert!(f32::from(status.size.height) > 20., "long status must wrap");
            }
        }
    }
    vcx.update(|_, cx| crate::i18n::set_language(crate::i18n::Language::Ko, cx));
}

#[gpui::test]
fn mobile_qr_fits_small_windows_at_maximum_ui_zoom(cx: &mut gpui::TestAppContext) {
    for page in [true, false] {
        let (view, vcx) = offer_view(cx, page);
        vcx.simulate_scale_factor_change(1.);
        vcx.simulate_resize(gpui::size(px(360.), px(900.)));
        vcx.update(|window, cx| {
            window.set_rem_size(px(32.));
            view.update(cx, |_, cx| cx.notify());
        });
        vcx.run_until_parked();
        if page {
            let qr = vcx.debug_bounds("mobile-page-qr").unwrap();
            let card = vcx.debug_bounds("mobile-page-card").unwrap();
            assert!(
                inside(span(qr), span(card)) && inside(span(qr), (0., 360.)),
                "page QR {qr:?} card {card:?}"
            );
        } else {
            let card = vcx.debug_bounds("mobile-qr-card").unwrap();
            let side = qr_side(px(220.), 1.);
            let qr = (
                f32::from(card.center().x - side / 2.),
                f32::from(card.center().x + side / 2.),
            );
            assert!(
                inside(qr, span(card)) && inside(qr, (0., 360.)),
                "settings QR {qr:?} card {card:?}"
            );
        }
    }
}

#[gpui::test]
fn transient_google_approval_poll_failure_keeps_polling_after_page_is_hidden(
    cx: &mut gpui::TestAppContext,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    cx.update(gpui_component::init);
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let mut view = SettingsView::new(cx);
        view.visible = false;
        view.mobile = Panel::new(Arc::new(move |request| {
            assert!(matches!(request, MobileRequest::Workflow(_)));
            if count.fetch_add(1, Ordering::Relaxed) == 0 {
                Err("temporary IPC failure".into())
            } else {
                Ok(MobileReply::Workflow("{\"ok\":true,\"data\":{\"signed_in\":true,\"email\":\"fixture@example.test\",\"pending\":false}}".into()))
            }
        }));
        view.mobile.account.pending = true;
        view.mobile_account_poll(cx);
        view
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| assert!(view.read(cx).mobile.account.pending));
    vcx.executor().advance_clock(Duration::from_secs(5));
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        assert!(!view.read(cx).mobile.account.pending);
        assert_eq!(
            view.read(cx).mobile.account.email.as_deref(),
            Some("fixture@example.test")
        );
        assert!(view.read(cx).mobile.error.is_none());
    });
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

#[gpui::test]
fn relay_click_starts_desktop_sign_in_and_resumes_pairing_once(cx: &mut gpui::TestAppContext) {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };
    cx.update(gpui_component::init);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let approved = Arc::new(AtomicBool::new(false));
    let record = requests.clone();
    let signed_in = approved.clone();
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let mut view = SettingsView::new(cx);
        view.visible = false;
        view.mobile = Panel::new(Arc::new(move |request| {
            record.lock().unwrap().push(request.clone());
            Ok(match request {
                MobileRequest::Workflow(json) => {
                    let action: serde_json::Value = serde_json::from_str(&json).unwrap();
                    let data = if action["action"] == "relay_auth_start" {
                        serde_json::json!({"requires_sign_in":true,"signed_in":false,"pending":true})
                    } else {
                        serde_json::json!({"requires_sign_in":true,"signed_in":signed_in.load(Ordering::Relaxed),"pending":!signed_in.load(Ordering::Relaxed),"email":"desktop@example.test"})
                    };
                    MobileReply::Workflow(serde_json::json!({"ok":true,"data":data}).to_string())
                }
                MobileRequest::MintPairingTicket { .. } | MobileRequest::CurrentPairingOffer => pairing_offer("https://example.test/#pair=fixture", u64::MAX),
                MobileRequest::Status => MobileReply::GatewayStatus { listening:true, bind_addrs:Vec::new(), mode:ConnectMode::Tunnel, tunnel_url:None, active_offer:false, devices:Vec::new() },
                MobileRequest::ConnectedDevices => MobileReply::ConnectedDevices(Vec::new()),
                other => panic!("unexpected request: {other:?}"),
            })
        }));
        view.mobile_account_poll(cx);
        view
    });
    vcx.run_until_parked();
    assert!(
        requests.lock().unwrap().is_empty(),
        "opening the page must not start sign-in"
    );
    vcx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.mobile_pair(ConnectMode::Tunnel, None, cx);
        });
    });
    vcx.run_until_parked();
    assert!(
        requests.lock().unwrap().iter().any(
            |r| matches!(r, MobileRequest::Workflow(json) if json.contains("relay_auth_start"))
        )
    );
    assert!(
        !requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, MobileRequest::MintPairingTicket { .. }))
    );
    approved.store(true, Ordering::Relaxed);
    vcx.executor().advance_clock(Duration::from_secs(5));
    vcx.run_until_parked();
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| matches!(r, MobileRequest::MintPairingTicket { .. }))
            .count(),
        1
    );
    vcx.executor().advance_clock(Duration::from_secs(5));
    vcx.run_until_parked();
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| matches!(r, MobileRequest::MintPairingTicket { .. }))
            .count(),
        1
    );
}
