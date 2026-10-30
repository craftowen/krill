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

    // 3. 연결 클릭 → 페어링 성공(Paired).
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

    // 6. 상대 컴퓨터가 꺼져 있음 → 플랫폼에 중립적인 오프라인 실패 사유.
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
    assert_eq!(
        PairFailure::RemoteMacOffline.text(),
        "상대 컴퓨터가 꺼져 있거나 접속 허용이 꺼져 있습니다. 상대 컴퓨터를 확인한 뒤 다시 시도해 주세요."
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
            d.set_link_value("https://not-krill.example", window, cx);
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
    // 페이지 본문은 예전 모달 폭에 갇히지 않고 사용할 수 있는 폭을 채운다.
    let page = vcx.debug_bounds("link-pairing-view").expect("pairing page");
    assert!(
        page.size.width > crate::ui::SETTINGS_MODAL_WIDTH,
        "{page:?}"
    );
    assert!(vcx.debug_bounds("link-pairing-modal").is_none());
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
        });
    });
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        let client = view.read(cx).client.clone();
        client.update(cx, ClientDialog::submit_pair);
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

/// OWE-669: `krill://` 링크를 받고, 접두어만 있는 값은 거절한다.
#[test]
fn invite_link_accepts_krill_scheme() {
    assert!(is_valid_invite_link("krill://pair?t=00&k=11"));
    assert!(is_valid_invite_link("  krill://pair?t=00&k=11\n"));
    assert!(!is_valid_invite_link("krill://pair?"));
    assert!(!is_valid_invite_link("krill://pairing"));
    assert!(!is_valid_invite_link("https://krill.example/pair?t=00"));
    assert_eq!(
        extract_clipboard_invite(" krill://pair?t=00&k=11 ").as_deref(),
        Some("krill://pair?t=00&k=11")
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
    // 사용자가 [연결]을 눌러야 페어링이 시작된다.
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
            d.set_link_value("https://not-krill.example", window, cx);
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

/// 연결할 Mac 이름을 적으면 상대가 알려준 이름 대신 그 이름으로 저장하고, [지우기]로 비운다.
#[gpui::test]
fn client_host_name_overrides_remote_name(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let mock = Arc::new(MockLinkBackend::new());
    let invite = mock.create_invite().unwrap();
    let backend: Arc<dyn LinkBackend> = mock;
    let (dialog, vcx) = cx.add_window_view(|window, cx| ClientDialog::new(backend, window, cx));
    vcx.simulate_resize(size(px(1000.), px(1600.)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("link-client-input-host-name").is_some());
    vcx.update(|window, cx| {
        dialog.update(cx, |d, cx| {
            d.set_link_value(invite.link.clone(), window, cx);
            d.set_host_name_value(" 세무사무소 ", window, cx);
        });
        window.draw(cx).clear(cx);
    });
    click(vcx, "link-client-connect-btn");
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        let ClientPhase::Paired(dev) = dialog.read(cx).phase() else {
            panic!("expected paired");
        };
        assert_eq!(dev.name, "세무사무소");
    });

    click(vcx, "link-client-close-btn");
    vcx.update(|window, cx| {
        window.draw(cx).clear(cx);
        assert!(dialog.read(cx).host_name_value(cx).is_empty());
    });
}

/// 연결 설정은 모달 장식 없이 페이지 본문으로 렌더한다.
#[gpui::test]
fn pairing_page_renders_panels_without_modal(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let backend: Arc<dyn LinkBackend> = Arc::new(MockLinkBackend::new());
    let (_view, vcx) = cx.add_window_view(|window, cx| PairingView::new(backend, window, cx));
    vcx.simulate_resize(size(px(1100.), px(1800.)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("link-pairing-modal").is_none());
    assert!(vcx.debug_bounds("link-pairing-view").is_some());
    assert!(vcx.debug_bounds("link-server-panel").is_some());
    assert!(vcx.debug_bounds("link-client-input-link").is_some());
}

/// OWE-903: 페어링 진입점은 호스트에 위임하고, 호스트가 없으면 조용히 무시한다(별도 창 없음).
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

/// A client can have registered remote Macs without permitting any inbound devices.
/// The settings panel must show the same remote registrations as the sidebar.
#[gpui::test]
fn remote_macs_disconnect_with_confirmation_without_inbound_devices(cx: &mut TestAppContext) {
    use super::real::RealLinkBackend;
    cx.update(gpui_component::init);
    let dir = std::env::temp_dir().join(format!("krill-remote-list-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let host = krill_link::RemoteHost {
        id: "studio".into(),
        name: "Studio Mac".into(),
        node_id: "aa".repeat(32),
        relay_url: None,
        addr_hints: Vec::new(),
        added_at: 1000,
    };
    krill_link::save_hosts(&dir, &[host]).unwrap();
    let backend = Arc::new(RealLinkBackend::new(
        dir.clone(),
        std::path::PathBuf::from("unused-link"),
    ));
    assert!(backend.devices().unwrap().is_empty());
    assert_eq!(backend.remote_hosts().unwrap()[0].name, "Studio Mac");
    let (panel, vcx) = cx.add_window_view(|_, cx| ServerPanel::new(backend.clone(), cx));
    vcx.simulate_resize(size(px(1000.), px(1600.)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("link-remote-host-studio").is_some());
    vcx.update(|_, cx| {
        assert_eq!(
            panel.read(cx).remote_hosts[0].status,
            krill_hosts::HostStatus::Disconnected
        );
        assert!(!panel.read(cx).status.enabled);
    });
    click(vcx, "link-remote-disconnect-studio");
    assert_eq!(krill_link::load_hosts(&dir).unwrap().len(), 1);
    vcx.update(|_, cx| {
        assert!(panel.read(cx).confirm_disconnect.is_some());
        panel.update(cx, ServerPanel::cancel_disconnect);
        assert!(panel.read(cx).confirm_disconnect.is_none());
    });
    click(vcx, "link-remote-disconnect-studio");
    click(vcx, "link-remote-disconnect-studio");
    assert!(backend.remote_hosts().unwrap().is_empty());
    assert!(backend.devices().unwrap().is_empty());
    assert!(vcx.debug_bounds("link-remote-host-studio").is_none());
    // 오래된 조회 완료나 다음 주기 갱신이 삭제한 행을 되살리지 않는다.

    krill_link::save_hosts(&dir, &[]).unwrap();
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("link-remote-host-studio").is_none());
    std::fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn remote_macs_rename_from_the_list(cx: &mut TestAppContext) {
    use super::real::RealLinkBackend;
    cx.update(gpui_component::init);
    let dir = std::env::temp_dir().join(format!("krill-remote-rename-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let host = krill_link::RemoteHost {
        id: "studio".into(),
        name: "Studio Mac".into(),
        node_id: "aa".repeat(32),
        relay_url: None,
        addr_hints: Vec::new(),
        added_at: 1000,
    };
    krill_link::save_hosts(&dir, &[host]).unwrap();
    let backend = Arc::new(RealLinkBackend::new(
        dir.clone(),
        std::path::PathBuf::from("unused-link"),
    ));
    assert!(backend.devices().unwrap().is_empty());
    assert_eq!(backend.remote_hosts().unwrap()[0].name, "Studio Mac");
    let (panel, vcx) = cx.add_window_view(|_, cx| ServerPanel::new(backend.clone(), cx));
    vcx.simulate_resize(size(px(1000.), px(1600.)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("link-remote-host-studio").is_some());
    // 목록에서 바로 이름을 바꾼다: Escape는 취소, [저장]은 이 Mac의 hosts.toml에 반영.
    click(vcx, "link-remote-rename-studio");
    vcx.update(|_, cx| assert_eq!(panel.read(cx).renaming_host(), Some("studio")));
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    vcx.update(|_, cx| assert_eq!(panel.read(cx).renaming_host(), None));

    click(vcx, "link-remote-rename-studio");
    vcx.update(|window, cx| {
        let input = panel.read(cx).renaming.as_ref().unwrap().input.clone();
        input.update(cx, |input, cx| input.set_value("세무사무소", window, cx));
    });
    click(vcx, "link-remote-rename-save");
    vcx.update(|_, cx| {
        let panel = panel.read(cx);
        assert_eq!(panel.renaming_host(), None);
        assert_eq!(panel.remote_hosts[0].name, "세무사무소");
    });
    assert_eq!(krill_link::load_hosts(&dir).unwrap()[0].name, "세무사무소");
    std::fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn remote_disconnect_failure_keeps_the_row_and_allows_retry(cx: &mut TestAppContext) {
    use super::real::RealLinkBackend;
    cx.update(gpui_component::init);
    let dir = std::env::temp_dir().join(format!("krill-remote-failure-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let host = krill_link::RemoteHost {
        id: "studio".into(),
        name: "Studio Mac".into(),
        node_id: "aa".repeat(32),
        relay_url: None,
        addr_hints: Vec::new(),
        added_at: 1000,
    };
    krill_link::save_hosts(&dir, std::slice::from_ref(&host)).unwrap();
    let backend = Arc::new(RealLinkBackend::new(dir.clone(), "unused-link".into()));
    let (panel, vcx) = cx.add_window_view(|_, cx| ServerPanel::new(backend, cx));
    vcx.simulate_resize(size(px(1000.), px(1600.)));
    vcx.run_until_parked();
    // 손상된 저장 파일에서는 메모리 목록만 지워 성공처럼 보이지 않는다.
    std::fs::write(dir.join("link/hosts.toml"), "invalid = [").unwrap();
    click(vcx, "link-remote-disconnect-studio");
    click(vcx, "link-remote-disconnect-studio");
    assert!(vcx.debug_bounds("link-remote-host-studio").is_some());
    assert!(panel.read_with(vcx, |p, _| p.error.is_some()));
    krill_link::save_hosts(&dir, &[host]).unwrap();
    click(vcx, "link-remote-disconnect-studio");
    click(vcx, "link-remote-disconnect-studio");
    assert!(vcx.debug_bounds("link-remote-host-studio").is_none());
    assert!(panel.read_with(vcx, |p, _| p.error.is_none()));
    std::fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn hidden_server_panel_clock_pauses_without_canceling_active_invite(cx: &mut TestAppContext) {
    let backend = Arc::new(MockLinkBackend::new());
    let (panel, vcx) = cx.add_window_view(|_, cx| ServerPanel::new(backend, cx));
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        panel.update(cx, |panel, cx| {
            panel.set_visible(false, cx);
            panel.now_secs = 1;
        });
    });
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(3));
    vcx.run_until_parked();
    vcx.update(|_, cx| assert_eq!(panel.read(cx).now_secs, 1));
    vcx.update(|_, cx| {
        panel.update(cx, |panel, _| {
            panel.invite = Some(Invite {
                link: "krill://pair?code=fixture".into(),
                expires_at: current_epoch_secs() + 60,
            });
        });
    });
    vcx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        assert!(panel.read(cx).now_secs > 1);
        assert!(panel.read(cx).invite.is_some());
    });
}

#[gpui::test]
fn ui_audit_remote_confirmation_expires_and_clears_on_hide(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let mock = Arc::new(MockLinkBackend::new());
    let device = PairedDevice {
        id: "audit-device".into(),
        name: "Fixture Mac".into(),
        added_at: 1,
        last_seen_at: None,
    };
    mock.set_devices(vec![device.clone()]);
    let (panel, vcx) = cx.add_window_view(|_, cx| ServerPanel::new(mock.clone(), cx));
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        panel.update(cx, |p, cx| {
            p.request_revoke(device.clone(), cx);
            p.set_visible(false, cx);
            assert!(p.confirm_revoke.is_none());
            p.set_visible(true, cx);
        });
    });
    // Showing the panel starts a refresh; wait for its busy guard before arming.
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        panel.update(cx, |p, cx| {
            assert!(!p.busy);
            p.request_revoke(device.clone(), cx);
            p.confirmation_deadline =
                std::time::Instant::now().checked_sub(std::time::Duration::from_secs(1));
            p.confirm_revoke_now(cx);
            assert!(p.confirm_revoke.is_none());
        });
    });
    vcx.run_until_parked();
    assert_eq!(
        mock.devices().unwrap().len(),
        1,
        "expired confirmation cannot revoke"
    );
}

#[gpui::test]
fn remote_rename_all_closures_restore_initiating_focus(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let mock = Arc::new(MockLinkBackend::new());
    let host = PairedHost {
        id: "focus-fixture".into(),
        name: "Fixture Mac".into(),
        status: krill_hosts::HostStatus::Disconnected,
        version: None,
    };
    let (panel, vcx) = cx.add_window_view(|_, cx| ServerPanel::new(mock.clone(), cx));
    vcx.run_until_parked();
    for save in [false, true] {
        vcx.update(|window, cx| {
            panel.update(cx, |p, cx| {
                window.focus(&p.focus, cx);
                p.remote_hosts = vec![host.clone()];
                p.start_rename(&host, window, cx);
                assert!(p.rename_return_focus.is_some());
                if save {
                    p.save_rename(window, cx);
                } else {
                    p.cancel_rename(window, cx);
                }
                assert!(p.renaming.is_none());
                assert!(p.rename_return_focus.is_none());
                assert!(p.focus.is_focused(window));
            });
        });
        vcx.run_until_parked();
    }
}
