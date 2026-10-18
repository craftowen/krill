use std::sync::Arc;

use gpui::{ClipboardItem, Modifiers, TestAppContext, px, size};

use super::*;
use crate::i18n::{Language, link_ui as text};

fn click(vcx: &mut gpui::VisualTestContext, selector: &'static str) {
    let bounds = vcx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing debug_selector: {selector}"));
    vcx.simulate_click(bounds.center(), Modifiers::default());
    vcx.run_until_parked();
}

#[test]
fn wording_catalog_has_no_jargon_and_has_ko_en() {
    let forbidden_words = [
        "ip", "port", "ssh", "key", "keys", "quic", "iroh", "uds", "socket",
    ];
    let forbidden_substrings = [
        "아이피",
        "포트",
        "공개키",
        "개인키",
        "암호키",
        "public key",
        "private key",
        "소켓",
        "키",
    ];
    for msg in text::ALL {
        let ko = msg.in_language(Language::Ko);
        let en = msg.in_language(Language::En);
        assert!(!ko.is_empty(), "{} ko is empty", msg.id);
        assert!(!en.is_empty(), "{} en is empty", msg.id);
        let ko_lower = ko.to_ascii_lowercase();
        let en_lower = en.to_ascii_lowercase();
        for word in forbidden_words {
            assert!(
                !ko_lower
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|w| w == word),
                "{} ko contains forbidden technical word '{word}': {ko}",
                msg.id
            );
            assert!(
                !en_lower
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|w| w == word),
                "{} en contains forbidden technical word '{word}': {en}",
                msg.id
            );
        }
        for term in forbidden_substrings {
            assert!(
                !ko_lower.contains(term),
                "{} ko contains forbidden technical term '{term}': {ko}",
                msg.id
            );
            assert!(
                !en_lower.contains(term),
                "{} en contains forbidden technical term '{term}': {en}",
                msg.id
            );
        }
    }
}

#[test]
fn qr_matrix_dimensions_finder_patterns_and_determinism() {
    let link = "krill://pair?invite=0001";
    let m1 = DefaultQrMatrix.encode(link).unwrap();
    let m2 = DefaultQrMatrix.encode(link).unwrap();
    let m3 = DefaultQrMatrix.encode("krill://pair?invite=0002").unwrap();

    assert_eq!(matrix_dimensions(&m1), (DEFAULT_QR_SIZE, DEFAULT_QR_SIZE));
    assert_eq!(m1, m2, "same link must yield identical QR matrix");
    assert_ne!(m1, m3, "different links must yield distinct QR matrices");

    // 3 corners must have 7×7 finder pattern outer top-left dark cell and center (3,3) dark cell.
    for (r0, c0) in [(0, 0), (0, DEFAULT_QR_SIZE - 7), (DEFAULT_QR_SIZE - 7, 0)] {
        assert!(m1[r0][c0]);
        assert!(m1[r0 + 3][c0 + 3]);
        assert!(!m1[r0 + 1][c0 + 1]);
    }
}

#[gpui::test]
fn server_panel_toggle_invite_copy_expiry_and_revoke_confirmation(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let mock = Arc::new(MockLinkBackend::new());
    let base_now = 1_760_000_000_u64;
    mock.set_now_secs(base_now);
    mock.set_devices(vec![
        PairedDevice {
            id: "dev-air".to_owned(),
            name: "오웬의 MacBook Air".to_owned(),
            added_at: base_now - 3_600,
            last_seen_at: Some(base_now - 120),
        },
        PairedDevice {
            id: "dev-mini".to_owned(),
            name: "집 맥미니".to_owned(),
            added_at: base_now - 86_400,
            last_seen_at: None,
        },
    ]);

    let custom_qr = Arc::new(StaticQrMatrix(vec![
        vec![true, false, true],
        vec![false, true, false],
        vec![true, false, true],
    ]));

    let backend: Arc<dyn LinkBackend> = mock.clone();
    let (panel, vcx) = cx.add_window_view(|_, cx| {
        let mut p = ServerPanel::new(backend, cx).with_qr_source(custom_qr);
        p.set_now_secs(base_now, cx);
        p
    });
    vcx.simulate_resize(size(px(1000.), px(1600.)));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));

    // 1. 초기 상태: 꺼져 있고 "아무 프로세스도 실행되지 않습니다" 안내가 표시된다.
    vcx.update(|_, cx| {
        assert!(!panel.read(cx).status().enabled);
        assert_eq!(panel.read(cx).devices().len(), 2);
    });
    assert!(vcx.debug_bounds("link-server-off-notice").is_some());
    assert!(vcx.debug_bounds("link-create-invite").is_none());

    // 2. 토글 켜기 → 안내가 사라지고 초대 링크 만들기 버튼이 나타난다.
    click(vcx, "link-server-toggle");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert!(panel.read(cx).status().enabled);
    });
    assert!(vcx.debug_bounds("link-server-off-notice").is_none());
    assert!(vcx.debug_bounds("link-create-invite").is_some());

    // 3. 초대 링크 만들기 → 링크 텍스트, 주입된 3×3 QR 행렬, 복사 버튼, 남은 유효 시간 표시.
    click(vcx, "link-create-invite");
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    let first_invite = vcx.update(|_, cx| {
        let p = panel.read(cx);
        assert_eq!(matrix_dimensions(p.qr_matrix()), (3, 3));
        assert!(!p.is_invite_expired());
        p.invite().cloned().expect("invite should be created")
    });
    assert!(first_invite.link.starts_with("krill://pair?"));
    assert!(vcx.debug_bounds("link-invite-qr").is_some());
    assert!(vcx.debug_bounds("link-invite-text").is_some());
    assert!(vcx.debug_bounds("link-invite-countdown").is_some());

    // 4. 복사 버튼 클릭 → 클립보드에 링크가 복사된다.
    click(vcx, "link-copy-invite");
    vcx.update(|_, cx| {
        assert!(panel.read(cx).copied());
        let clip = cx
            .read_from_clipboard()
            .and_then(|c| c.text())
            .expect("clipboard text");
        assert_eq!(clip, first_invite.link);
    });

    // 5. 유효 시간 만료 → 만료 안내와 "새로 만들기" 버튼이 나타난다.
    mock.set_now_secs(first_invite.expires_at + 1);
    vcx.update(|window, cx| {
        panel.update(cx, |p, cx| p.set_now_secs(first_invite.expires_at + 1, cx));
        window.draw(cx).clear(cx);
        assert!(panel.read(cx).is_invite_expired());
    });
    assert!(vcx.debug_bounds("link-invite-expired").is_some());
    assert!(vcx.debug_bounds("link-recreate-invite").is_some());

    // 6. "새로 만들기" 클릭 → 새 링크가 발급되고 다시 유효 상태가 된다.
    click(vcx, "link-recreate-invite");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let p = panel.read(cx);
        assert!(!p.is_invite_expired());
        let second = p.invite().expect("recreated invite");
        assert_ne!(second.link, first_invite.link);
    });

    // 7. 기기 연결 끊기(회수): 첫 클릭에 무장 → 같은 버튼을 다시 누르면 삭제.
    // 목록이 밀리던 별도 확인 박스 대신 행 버튼 자체가 2단계 확인이 된다.
    assert!(vcx.debug_bounds("link-revoke-btn-dev-air").is_some());
    click(vcx, "link-revoke-btn-dev-air");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert_eq!(
            panel.read(cx).confirm_revoke().map(|d| d.id.as_str()),
            Some("dev-air")
        );
    });
    // 무장 상태에서도 목록은 그대로 유지된다.
    assert!(vcx.debug_bounds("link-device-row-dev-air").is_some());
    assert!(vcx.debug_bounds("link-device-row-dev-mini").is_some());

    // 무장을 풀면 기기 목록이 그대로 유지된다.
    vcx.update(|_, cx| {
        panel.update(cx, super::ServerPanel::cancel_revoke);
        assert!(panel.read(cx).confirm_revoke().is_none());
        assert_eq!(panel.read(cx).devices().len(), 2);
    });
    vcx.update(|window, cx| window.draw(cx).clear(cx));

    // 다시 눌러 무장하고 한 번 더 누르면 dev-air가 회수되어 목록에서 사라진다.
    click(vcx, "link-revoke-btn-dev-air");
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    click(vcx, "link-revoke-btn-dev-air");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let devices = panel.read(cx).devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "dev-mini");
    });
    assert!(vcx.debug_bounds("link-device-row-dev-air").is_none());
    assert!(vcx.debug_bounds("link-device-row-dev-mini").is_some());

    // 8. 서버 끄기 → 비활성화 및 초대 초기화.
    click(vcx, "link-server-toggle");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert!(!panel.read(cx).status().enabled);
        assert!(panel.read(cx).invite().is_none());
    });
    assert!(vcx.debug_bounds("link-server-off-notice").is_some());
}

#[gpui::test]
fn client_dialog_clipboard_prefill_nearby_and_all_pair_outcomes(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.write_to_clipboard(ClipboardItem::new_string(
            "krill://pair?invite=clip-ready-1".to_owned(),
        ));
    });

    let mock = Arc::new(MockLinkBackend::new());
    mock.set_nearby(vec![
        NearbyMac {
            id: "near-1".to_owned(),
            name: "집 맥미니".to_owned(),
            invite_link: "krill://pair?nearby=home-mini".to_owned(),
        },
        NearbyMac {
            id: "near-2".to_owned(),
            name: "거실 iMac".to_owned(),
            invite_link: "krill://pair?nearby=living-imac".to_owned(),
        },
    ]);

    let backend: Arc<dyn LinkBackend> = mock.clone();
    let (dialog, vcx) = cx.add_window_view(|window, cx| ClientDialog::new(backend, window, cx));
    vcx.simulate_resize(size(px(1000.), px(1600.)));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));

    // 1. 이 기기 이름이 컴퓨터 이름으로 미리 채워져 있고, 클립보드 자동 채움 제안이 뜬다.
    vcx.update(|_, cx| {
        let d = dialog.read(cx);
        assert!(!d.device_name_value(cx).trim().is_empty());
        assert_eq!(
            d.clipboard_suggestion(),
            Some("krill://pair?invite=clip-ready-1")
        );
        assert_eq!(d.nearby().len(), 2);
    });
    assert!(vcx.debug_bounds("link-clipboard-suggestion").is_some());
    assert!(vcx.debug_bounds("link-nearby-item-0").is_some());
    assert!(vcx.debug_bounds("link-nearby-item-1").is_some());
    assert!(vcx.debug_bounds("link-nearby-connect-0").is_some());
    assert!(vcx.debug_bounds("link-nearby-select-0").is_none());

    // 2. 클립보드 자동 채움 클릭 → 링크 입력칸이 채워지고 제안 배너가 사라진다.
    click(vcx, "link-clipboard-fill");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert_eq!(
            dialog.read(cx).link_value(cx),
            "krill://pair?invite=clip-ready-1"
        );
    });
    assert!(vcx.debug_bounds("link-clipboard-suggestion").is_none());

    // 3. 연결 클릭 → 짝짓기 성공(Paired).
    click(vcx, "link-client-connect-btn");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert!(matches!(dialog.read(cx).phase(), ClientPhase::Paired(_)));
    });
    assert!(vcx.debug_bounds("link-client-status-paired").is_some());

    // 4. 같은 링크로 다시 연결 시도 → "이미 쓴 링크" 실패 사유.
    click(vcx, "link-client-connect-btn");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert_eq!(
            dialog.read(cx).phase(),
            &ClientPhase::Failed(PairFailure::AlreadyUsedLink)
        );
    });
    assert!(
        vcx.debug_bounds("link-client-failure-already-used")
            .is_some()
    );
    assert!(PairFailure::AlreadyUsedLink.text().contains("이미 쓴 링크"));

    // 5. 만료된 링크 → "만료된 링크" 실패 사유.
    vcx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.set_link_value("krill://pair?invite=expired-token", window, cx);
        });
        window.draw(cx).clear(cx);
    });
    click(vcx, "link-client-connect-btn");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert_eq!(
            dialog.read(cx).phase(),
            &ClientPhase::Failed(PairFailure::ExpiredLink)
        );
    });
    assert!(vcx.debug_bounds("link-client-failure-expired").is_some());
    assert!(PairFailure::ExpiredLink.text().contains("만료된 링크"));

    // 6. 상대 Mac이 꺼져 있음 → "상대 Mac이 꺼져 있거나" 실패 사유.
    vcx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.set_link_value("krill://pair?invite=offline-host", window, cx);
        });
        window.draw(cx).clear(cx);
    });
    click(vcx, "link-client-connect-btn");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert_eq!(
            dialog.read(cx).phase(),
            &ClientPhase::Failed(PairFailure::RemoteMacOffline)
        );
    });
    assert!(vcx.debug_bounds("link-client-failure-offline").is_some());
    assert!(
        PairFailure::RemoteMacOffline
            .text()
            .contains("상대 Mac이 꺼져")
    );

    // 7. 인터넷 연결 없음 → "인터넷 연결이 없습니다" 실패 사유.
    vcx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.set_link_value("krill://pair?invite=no-internet", window, cx);
        });
        window.draw(cx).clear(cx);
    });
    click(vcx, "link-client-connect-btn");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert_eq!(
            dialog.read(cx).phase(),
            &ClientPhase::Failed(PairFailure::NoInternet)
        );
    });
    assert!(
        vcx.debug_bounds("link-client-failure-no-internet")
            .is_some()
    );
    assert!(
        PairFailure::NoInternet
            .text()
            .contains("인터넷 연결이 없습니다")
    );

    // 8. 잘못된 링크 형식 → InvalidLink 실패 사유.
    vcx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.set_link_value("https://not-minikrill.example", window, cx);
        });
        window.draw(cx).clear(cx);
    });
    click(vcx, "link-client-connect-btn");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert_eq!(
            dialog.read(cx).phase(),
            &ClientPhase::Failed(PairFailure::InvalidLink)
        );
    });
    assert!(vcx.debug_bounds("link-client-failure-invalid").is_some());

    // 9. 근처 Mac 선택 및 바로 연결.
    click(vcx, "link-nearby-connect-0");
    vcx.update(|_, cx| {
        assert_eq!(
            dialog.read(cx).link_value(cx),
            "krill://pair?nearby=home-mini"
        );
    });
    click(vcx, "link-nearby-connect-1");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert!(matches!(dialog.read(cx).phase(), ClientPhase::Paired(_)));
    });
}

#[gpui::test]
fn pairing_view_renders_server_and_client_together(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let backend: Arc<dyn LinkBackend> = Arc::new(MockLinkBackend::with_demo_data());
    let (view, vcx) = cx.add_window_view(|window, cx| PairingView::new(backend, window, cx));
    vcx.simulate_resize(size(px(1100.), px(1800.)));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let server = view.read(cx).server.clone();
        server.update(cx, ServerPanel::create_invite);
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));

    assert!(vcx.debug_bounds("link-pairing-view").is_some());
    assert!(vcx.debug_bounds("link-server-panel").is_some());
    assert!(vcx.debug_bounds("link-client-dialog").is_some());
    assert!(vcx.debug_bounds("link-invite-qr").is_some());
    // OWE-903: 별도 창 대신 넓은 모달 규격 카드로 뜬다.
    let card = vcx.debug_bounds("link-pairing-modal").expect("modal card");
    assert!(
        (f32::from(card.size.width) - f32::from(crate::ui::SETTINGS_MODAL_WIDTH)).abs() < 1.,
        "modal card must use SETTINGS_MODAL_WIDTH: {card:?}"
    );
}

#[gpui::test]
fn paste_pairs_once_and_server_uses_one_switch(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let mock = Arc::new(MockLinkBackend::new());
    let backend: Arc<dyn LinkBackend> = mock;
    let (view, vcx) = cx.add_window_view(|window, cx| PairingView::new(backend, window, cx));
    vcx.simulate_resize(size(px(1100.), px(1800.)));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(
            "krill://pair?invite=single-paste".into(),
        ));
        let client = view.read(cx).client.clone();
        client.update(cx, |dialog, cx| {
            dialog.paste_from_clipboard(window, cx);
            dialog.submit_pair(cx);
        });
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        assert!(matches!(
            view.read(cx).client.read(cx).phase(),
            ClientPhase::Paired(_)
        ));
        window.draw(cx).clear(cx);
    });
    assert!(vcx.debug_bounds("link-server-toggle").is_some());
    assert!(vcx.debug_bounds("link-server-on").is_none());
    assert!(vcx.debug_bounds("link-server-off").is_none());
    assert_eq!(DEFAULT_INVITE_TTL_SECS, 600);
}

#[test]
fn real_qr_reports_encoding_failure_and_pair_errors_are_specific() {
    use super::real::{RealQrMatrix, classify_pair_error};
    assert!(RealQrMatrix.encode(&"x".repeat(10_000)).is_err());
    assert_eq!(
        classify_pair_error(&krill_link::Error::PairRejected("already been used".into())),
        PairFailure::AlreadyUsedLink
    );
    assert_eq!(
        classify_pair_error(&krill_link::Error::Timeout("pair")),
        PairFailure::Unavailable
    );
}

/// OWE-669: 새 `krill://` 링크와 이미 나간 옛 `minikrill://` 링크를 모두 받고, 접두어만 있는 값은 거절한다.
#[test]
fn invite_link_accepts_new_and_legacy_schemes() {
    assert!(is_valid_invite_link("krill://pair?t=00&k=11"));
    assert!(is_valid_invite_link("  minikrill://pair?t=00&k=11\n"));
    assert!(!is_valid_invite_link("krill://pair?"));
    assert!(!is_valid_invite_link("krill://pairing"));
    assert!(!is_valid_invite_link("https://krill.example/pair?t=00"));
    assert_eq!(
        extract_clipboard_invite(" minikrill://pair?t=00&k=11 ").as_deref(),
        Some("minikrill://pair?t=00&k=11")
    );
}

/// OWE-878: '붙여넣기'는 초대 링크 입력칸만 채우고, 기기 이름을 확인할 때까지
/// 연결을 시작하지 않는다. 제출은 [연결](또는 Enter)이 맡는다.
#[gpui::test]
fn paste_fills_link_without_auto_submit(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let backend: Arc<dyn LinkBackend> = Arc::new(MockLinkBackend::new());
    let (dialog, vcx) = cx.add_window_view(|window, cx| ClientDialog::new(backend, window, cx));
    vcx.simulate_resize(size(px(1000.), px(1600.)));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(
            "krill://pair?invite=paste-no-submit".into(),
        ));
        dialog.update(cx, |d, cx| d.paste_from_clipboard(window, cx));
        window.draw(cx).clear(cx);
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let d = dialog.read(cx);
        assert_eq!(d.link_value(cx), "krill://pair?invite=paste-no-submit");
        assert_eq!(d.phase(), &ClientPhase::Idle);
    });
    // 사용자가 [연결]을 눌러야 짝짓기가 시작된다.
    click(vcx, "link-client-connect-btn");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert!(matches!(dialog.read(cx).phase(), ClientPhase::Paired(_)));
    });
}

/// OWE-878: 근처 Mac 자동 찾기가 지원되지 않아 목록이 비어 있으면 섹션 자체를
/// 그리지 않는다(미구현 크롬 노출 금지). 탐색 결과가 있을 때만 행을 보여준다.
#[gpui::test]
fn nearby_section_hidden_until_discovery_returns_macs(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let mock = Arc::new(MockLinkBackend::new());
    let backend: Arc<dyn LinkBackend> = mock.clone();
    let (dialog, vcx) = cx.add_window_view(|window, cx| ClientDialog::new(backend, window, cx));
    vcx.simulate_resize(size(px(1000.), px(1600.)));
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("link-nearby-section").is_none());
    assert!(vcx.debug_bounds("link-nearby-empty").is_none());
    assert!(vcx.debug_bounds("link-nearby-refresh").is_none());

    mock.set_nearby(vec![NearbyMac {
        id: "near-1".to_owned(),
        name: "집 맥미니".to_owned(),
        invite_link: "krill://pair?nearby=home-mini".to_owned(),
    }]);
    vcx.update(|_, cx| {
        dialog.update(cx, ClientDialog::refresh_nearby);
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("link-nearby-section").is_some());
    assert!(vcx.debug_bounds("link-nearby-item-0").is_some());
}

/// OWE-878: 클라이언트 카드의 [지우기]는 OS 창을 닫지 않고 입력칸·진행 상태만
/// 초기화한다. 이 기기 이름은 유지한다.
#[gpui::test]
fn client_close_resets_form_instead_of_closing_window(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let backend: Arc<dyn LinkBackend> = Arc::new(MockLinkBackend::new());
    let (dialog, vcx) = cx.add_window_view(|window, cx| ClientDialog::new(backend, window, cx));
    vcx.simulate_resize(size(px(1000.), px(1600.)));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.set_link_value("https://not-minikrill.example", window, cx);
        });
        window.draw(cx).clear(cx);
    });
    click(vcx, "link-client-connect-btn");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert!(matches!(dialog.read(cx).phase(), ClientPhase::Failed(_)));
    });

    click(vcx, "link-client-close-btn");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let d = dialog.read(cx);
        assert_eq!(d.phase(), &ClientPhase::Idle);
        assert!(d.link_value(cx).is_empty());
        assert!(
            !d.device_name_value(cx).trim().is_empty(),
            "device name is kept"
        );
    });
}

/// OWE-903: 짝짓기 모달은 Esc로 OS 창을 닫지 않고 호스트에 닫기를 알린다.
#[gpui::test]
fn pairing_modal_emits_close_on_esc(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        super::bind_keys(cx);
    });
    let backend: Arc<dyn LinkBackend> = Arc::new(MockLinkBackend::new());
    let (view, vcx) = cx.add_window_view(|window, cx| PairingView::new(backend, window, cx));
    vcx.simulate_resize(size(px(1100.), px(1800.)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("link-pairing-modal").is_some());
    assert!(vcx.debug_bounds("link-pairing-view").is_some());
    let fired = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = fired.clone();
    let _sub = vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, _: &Event, _| {
            flag.set(true);
        })
    });
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(fired.get(), "Esc must emit Close to the modal host");
}

/// OWE-903: 짝짓기 모달은 ⌘W로 OS 창을 닫지 않고 호스트에 닫기를 알린다.
#[gpui::test]
fn pairing_modal_emits_close_on_cmd_w(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        super::bind_keys(cx);
    });
    let backend: Arc<dyn LinkBackend> = Arc::new(MockLinkBackend::new());
    let (view, vcx) = cx.add_window_view(|window, cx| PairingView::new(backend, window, cx));
    vcx.simulate_resize(size(px(1100.), px(1800.)));
    vcx.run_until_parked();
    let fired = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = fired.clone();
    let _sub = vcx.update(|_, cx| {
        cx.subscribe(&view, move |_, _: &Event, _| {
            flag.set(true);
        })
    });
    vcx.simulate_keystrokes("cmd-w");
    vcx.run_until_parked();
    assert!(fired.get(), "cmd-W must emit Close to the modal host");
}

/// OWE-903: 짝짓기 진입점은 호스트에 위임하고, 호스트가 없으면 조용히 무시한다(별도 창 없음).
#[gpui::test]
fn open_pairing_dispatches_to_registered_host(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    // 호스트 없이 호출해도 패닉·별도 창 없이 무시된다.
    cx.update(|cx| {
        let before = cx.windows().len();
        super::open_pairing(cx);
        assert_eq!(cx.windows().len(), before);
    });
    let opened = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = opened.clone();
    cx.update(|cx| {
        super::register_host(cx, move |cmd, _| {
            assert_eq!(cmd, super::HostCommand::Open);
            flag.set(true);
            Some(true)
        });
    });
    cx.update(super::open_pairing);
    assert!(opened.get(), "open_pairing must dispatch to the host");
}
