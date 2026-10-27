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
                    MobileRequest::Workflow(_) => {
                        MobileReply::Workflow("{\"ok\":true,\"data\":{\"signed_in\":false}}".into())
                    }
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
                    MobileRequest::Workflow(_) => Ok(MobileReply::Workflow(
                        "{\"ok\":true,\"data\":{\"signed_in\":false}}".into(),
                    )),
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
        // 설정은 왼쪽 탐색 248px가 고정이라 360px 창에서는 본문 열이 48px뿐이다(기존부터, 측정만 한다).
        for width in [360., 560., 580., 598., 800., 1100.] {
            for scale in [1., 1.25, 1.5, 2.] {
                redraw(vcx, width, scale);
                let card = vcx.debug_bounds("mobile-qr-card").expect("tab qr card");
                let copy = vcx.debug_bounds("mobile-copy-link").expect("tab copy");
                // QR 캔버스는 선택자가 없다. 카드(p_3 + 테두리 1)가 가운데 정렬하므로 실제 카드 경계와 렌더 크기로 위치를 구한다.
                let side = f32::from(qr_side(px(220.), scale));
                let center = f32::from(card.center().x);
                let qr = (center - side / 2., center + side / 2.);
                let content = (f32::from(card.left()) + 13., f32::from(card.right()) - 13.);
                eprintln!(
                    "tab w={width} @{scale}x card={:?} content={content:?} qr~{qr:?} side={side} fits_before_220={} copy={:?}",
                    span(card),
                    content.1 - content.0 >= 220.,
                    span(copy)
                );
                if width < 598. {
                    continue;
                }
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

#[gpui::test]
fn relay_account_states_keep_card_and_buttons_in_place(cx: &mut gpui::TestAppContext) {
    let (view, vcx) = offer_view(cx, true);
    redraw(vcx, 480., 1.);
    let bounds = |vcx: &mut gpui::VisualTestContext| {
        [
            "mobile-relay-account",
            "mobile-relay-login",
            "mobile-relay-logout",
        ]
        .map(|id| vcx.debug_bounds(id).expect("account control"))
    };
    let original = bounds(vcx);
    for (pending, email) in [
        (true, None),
        (false, Some("a-long-account-email@example.test".to_owned())),
        (false, None),
    ] {
        view.update(vcx, |view, cx| {
            view.mobile.account.pending = pending;
            view.mobile.account.email = email;
            cx.notify();
        });
        vcx.run_until_parked();
        assert_eq!(bounds(vcx), original);
    }
    redraw(vcx, 360., 1.);
    let [card, login, logout] = bounds(vcx);
    assert!(inside(span(login), span(card)));
    assert!(inside(span(logout), span(card)));
}
