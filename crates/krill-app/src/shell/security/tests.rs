use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use gpui::TestAppContext;
use krill_proto::input::{Input, Now};
use krill_proto::screen::Msg as _;

use crate::shell::tests::{open, state_path};

fn link_waiting_pane(
    shell: &mut super::Shell,
    pane: krill_app::layout::PaneId,
    session: krill_proto::SessionId,
    socket: UnixStream,
    cx: &mut gpui::Context<super::Shell>,
) -> std::io::Result<()> {
    assert!(matches!(
        shell.linked(
            pane,
            session,
            krill_app::pane::Linked {
                reader: socket.try_clone()?,
                writer: krill_app::pane::Writer::spawn(socket)?,
                instance: 1,
                launch: None,
                features: krill_proto::input::FEAT_INPUT,
                search: false,
            },
            cx
        ),
        super::super::Next::Read(_)
    ));
    Ok(())
}

#[gpui::test]
fn first_attach_paste_rejects_changed_host_live_and_session(cx: &mut TestAppContext) {
    use krill_app::hosts::HostId;
    use krill_proto::SessionId;

    for change in ["none", "host", "live", "session", "focus"] {
        let (view, cx) = open(cx, state_path(&format!("early-paste-{change}")));
        let (pane, request) = cx.update(|window, cx| {
            view.update(cx, |shell, cx| {
                let pane = shell.layout.active_tab().focused;
                shell
                    .live
                    .insert(pane, super::super::Live::new(gpui::Task::ready(())));
                (pane, shell.begin_clipboard_paste(window, cx).unwrap())
            })
        });
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        cx.update(|window, cx| {
            view.update(cx, |shell, cx| {
                match change {
                    "none" => {}
                    "host" => shell.layout.set_host(pane, HostId::Remote("other".into())),
                    "live" => {
                        shell
                            .live
                            .insert(pane, super::super::Live::new(gpui::Task::ready(())));
                    }
                    "session" => {
                        // Bind the request before clipboard IO finishes, then change PTYs.
                        let (first, _peer) = UnixStream::pair().unwrap();
                        link_waiting_pane(shell, pane, SessionId(41), first, cx).unwrap();
                    }
                    "focus" => shell.on_window_active_changed(false, cx),
                    _ => unreachable!(),
                }
                link_waiting_pane(shell, pane, SessionId(42), ours, cx).unwrap();
                shell.complete_clipboard_paste(request, Some(b"stale url".to_vec()), window, cx);
            });
        });
        cx.run_until_parked();
        if change == "none" {
            theirs
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let (kind, body) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
            assert_eq!(Input::decode(kind, &body).unwrap().bytes, b"stale url");
            continue;
        }
        theirs.set_nonblocking(true).unwrap();
        assert_eq!(
            theirs.read(&mut [0]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "{change}"
        );
    }
}

#[gpui::test]
fn windows_ctrl_v_before_first_attach_keeps_url_and_following_typing(cx: &mut TestAppContext) {
    cx.update(super::super::bind_windows_paste);
    let (view, cx) = open(cx, state_path("windows-early-url-paste"));
    let pane = view.update(cx, |shell, _| {
        let pane = shell.layout.active_tab().focused;
        shell
            .live
            .insert(pane, super::super::Live::new(gpui::Task::ready(())));
        pane
    });
    let url = "https://example.com/한글?q=clipboard";
    cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.into()));
    cx.simulate_keystrokes("ctrl-v");
    cx.simulate_input("x");
    cx.run_until_parked();
    // The preparation deadline must not erase a ready paste while attach is slow.
    cx.executor()
        .advance_clock(super::PASTE_PREPARATION_TIMEOUT);
    cx.run_until_parked();
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    view.update(cx, |shell, cx| {
        link_waiting_pane(shell, pane, krill_proto::SessionId(42), ours, cx).unwrap();
    });
    let received = (0..2)
        .map(|_| {
            let (kind, body) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
            Input::decode(kind, &body).unwrap().bytes
        })
        .collect::<Vec<_>>();
    assert_eq!(received, [url.as_bytes().to_vec(), b"x".to_vec()]);
}

#[gpui::test]
fn windows_ctrl_v_pastes_url_without_relying_on_agent_clipboard_support(cx: &mut TestAppContext) {
    // Register the actual Windows binding on the in-memory Mac test platform.
    cx.update(super::super::bind_windows_paste);
    let (view, cx) = open(cx, state_path("windows-url-paste"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), ours, cx);
    });
    let url = "https://example.com/path?q=한글&from=clipboard";
    cx.update(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.to_owned())));
    cx.simulate_keystrokes("ctrl-v");
    cx.run_until_parked();
    let (kind, body) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(Input::decode(kind, &body).unwrap().bytes, url.as_bytes());
}

fn typed(bytes: &[u8]) -> krill_term::ime::Output {
    krill_term::ime::Output {
        pty: bytes.to_vec(),
        discard_ime: false,
    }
}

// `cfg(all(test, unix))` 모듈의 헬퍼는 clippy의 테스트 예외(`allow-unwrap-in-tests`)를 받지 못한다.
#[expect(clippy::unwrap_used, reason = "테스트 헬퍼")]
fn read_input(peer: &mut UnixStream) -> Vec<u8> {
    let (kind, body) = krill_proto::ipc::read_frame(peer).unwrap().unwrap();
    Input::decode(kind, &body).unwrap().bytes
}

#[expect(clippy::unwrap_used, reason = "테스트 헬퍼")]
fn assert_nothing_sent(peer: &mut UnixStream) {
    peer.set_nonblocking(true).unwrap();
    assert_eq!(
        peer.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "bytes reached the PTY too early"
    );
    peer.set_nonblocking(false).unwrap();
}

/// APP-RUNTIME-002: bracketed paste가 꺼진 세션에서 여러 줄 붙여넣기가 경고에 걸리면, 그 뒤에 친 키와 다음 붙여넣기는
/// 확인이 끝날 때까지 큐에 남아 제출 순서를 지키고, 연속 붙여넣기는 덮어쓰이지 않고 차례로 확인한다.
#[gpui::test]
fn multiline_paste_warning_holds_later_input_and_queues_the_next_paste(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("async-paste-warn-order"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), ours, cx);
    });
    let (first, second) = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let first = shell.begin_clipboard_paste(window, cx).unwrap();
            shell.send(&typed(b"x"), Some(window), cx);
            let second = shell.begin_clipboard_paste(window, cx).unwrap();
            shell.send(&typed(b"y"), Some(window), cx);
            (first, second)
        })
    });
    // 둘째가 먼저 준비돼도 첫째 뒤에서 기다린다.
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(second, Some(b"echo B\necho BB".to_vec()), window, cx);
            shell.complete_clipboard_paste(first, Some(b"echo A\necho AA".to_vec()), window, cx);
        });
    });
    cx.run_until_parked();
    let shown = |cx: &mut gpui::VisualTestContext| {
        view.read_with(cx, |shell, _| {
            shell.security.pending_paste().map(|p| p.payload.clone())
        })
    };
    assert_eq!(
        shown(cx),
        Some(b"echo A\recho AA".to_vec()),
        "the first paste awaits confirmation"
    );
    assert_nothing_sent(&mut theirs);

    cx.update(|window, cx| {
        view.update(cx, |shell, cx| shell.confirm_pending_paste(window, cx));
    });
    assert_eq!(read_input(&mut theirs), b"echo A\recho AA");
    assert_eq!(
        read_input(&mut theirs),
        b"x",
        "typed keys follow the confirmed paste"
    );
    assert_eq!(
        shown(cx),
        Some(b"echo B\recho BB".to_vec()),
        "the second paste is confirmed next instead of overwriting the first"
    );
    assert_nothing_sent(&mut theirs);

    cx.update(|_, cx| view.update(cx, super::Shell::cancel_pending_paste));
    assert_eq!(
        read_input(&mut theirs),
        b"y",
        "cancel drops only the payload"
    );
    assert_eq!(shown(cx), None);
    assert!(view.read_with(cx, |shell, _| !shell.paste_queue.has_pending_for(pane)));
}

/// APP-RUNTIME-003: 첫 세션에 붙기 전 클립보드 붙여넣기는 버리지 않고 칸 저널에 보관돼, 그 뒤에 친 키와 함께 제출
/// 순서대로 첫 세션에 전달된다(STYLEGUIDE §7.0 "기다리는 동안에도 입력을 받는다").
#[gpui::test]
fn clipboard_paste_before_the_first_session_reaches_it_in_order(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("async-paste-pre-attach"));
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    view.update(cx, |shell, _| {
        let live = crate::shell::Live::new(gpui::Task::ready(()));
        assert!(live.pane.session().is_none());
        shell.live.insert(pane, live);
    });
    let request = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let request = shell
                .begin_clipboard_paste(window, cx)
                .expect("paste is accepted before the first session");
            shell.send(&typed(b"ls\r"), Some(window), cx);
            request
        })
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(request, Some(b"hello".to_vec()), window, cx);
        });
    });
    cx.run_until_parked();
    assert!(view.read_with(cx, |shell, _| !shell.paste_queue.has_pending_for(pane)));

    // 이제 첫 세션에 붙는다: 보관된 입력이 제출 순서대로 나간다.
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    view.update(cx, |shell, cx| {
        let live = shell.live.get_mut(&pane).unwrap();
        let notices = live.pane.link(
            krill_proto::SessionId(42),
            1,
            krill_app::pane::Writer::spawn(ours).unwrap(),
            krill_proto::input::FEAT_INPUT,
            Now::get(),
        );
        assert!(notices.is_empty());
        cx.notify();
    });
    assert_eq!(read_input(&mut theirs), b"hello");
    assert_eq!(read_input(&mut theirs), b"ls\r");
}

#[gpui::test]
fn confirming_paste_does_not_reach_a_replacement_session(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("paste-confirm-session-change"));
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    let (old, mut old_peer) = UnixStream::pair().unwrap();
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), old, cx);
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.paste_bytes(b"echo old\necho old2", window, cx);
            shell.send(&typed(b"old keys"), Some(window), cx);
            assert!(shell.security.pending_paste().is_some());
        });
    });
    assert_nothing_sent(&mut old_peer);

    let (new, mut new_peer) = UnixStream::pair().unwrap();
    new_peer
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(43), new, cx);
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.send(&typed(b"new keys"), Some(window), cx);
            shell.confirm_pending_paste(window, cx);
        });
    });
    assert_eq!(read_input(&mut new_peer), b"new keys");
    assert_nothing_sent(&mut new_peer);
    assert!(
        view.read_with(cx, |shell, _| shell.security.pending_paste().is_none()
            && !shell.paste_queue.has_pending_for(pane))
    );
}

#[gpui::test]
fn stale_confirmation_banner_without_a_queued_paste_sends_nothing(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("paste-confirm-stale-banner"));
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    let (ours, mut peer) = UnixStream::pair().unwrap();
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), ours, cx);
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.paste_bytes(b"echo stale\necho stale2", window, cx);
            assert!(shell.security.pending_paste().is_some());
            shell.paste_queue.entries.clear();
            shell.confirm_pending_paste(window, cx);
            assert!(shell.security.pending_paste().is_none());
        });
    });
    peer.set_read_timeout(Some(Duration::from_millis(250)))
        .unwrap();
    assert!(matches!(
        peer.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
}

#[gpui::test]
fn slow_pastes_keep_keystrokes_in_submission_order(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("async-paste-order"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), ours, cx);
    });
    let (first, second) = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let first = shell.begin_clipboard_paste(window, cx).unwrap();
            shell.send(
                &krill_term::ime::Output {
                    pty: b"x".to_vec(),
                    discard_ime: false,
                },
                Some(window),
                cx,
            );
            let second = shell.begin_clipboard_paste(window, cx).unwrap();
            shell.send(
                &krill_term::ime::Output {
                    pty: b"y".to_vec(),
                    discard_ime: false,
                },
                Some(window),
                cx,
            );
            (first, second)
        })
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(second, Some(b"B".to_vec()), window, cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("panel-column").is_some(),
        "window keeps drawing while clipboard work is pending"
    );
    theirs.set_nonblocking(true).unwrap();
    let mut byte = [0];
    assert_eq!(
        theirs.read(&mut byte).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    theirs.set_nonblocking(false).unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(first, Some(b"A".to_vec()), window, cx);
        });
    });
    let received = (0..4)
        .map(|_| {
            let (kind, body) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
            Input::decode(kind, &body).unwrap().bytes
        })
        .collect::<Vec<_>>();
    assert_eq!(
        received,
        [b"A".to_vec(), b"x".to_vec(), b"B".to_vec(), b"y".to_vec()]
    );
}

#[gpui::test]
fn paste_result_never_targets_a_later_tab(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("async-paste-stale"));
    let (ours, _theirs) = UnixStream::pair().unwrap();
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), ours, cx);
    });
    let pending = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.begin_clipboard_paste(window, cx).unwrap()
        })
    });
    cx.simulate_keystrokes("cmd-t");
    let later = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    view.update(cx, |shell, cx| {
        shell.attach_test(later, krill_proto::SessionId(43), ours, cx);
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(pending, Some(b"stale".to_vec()), window, cx);
        });
    });
    cx.run_until_parked();
    theirs.set_nonblocking(true).unwrap();
    assert_eq!(
        theirs.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[gpui::test]
fn paste_deadline_releases_typing_and_rejects_late_image_result(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("async-paste-deadline"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), ours, cx);
    });
    let request = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let request = shell.begin_clipboard_paste(window, cx).unwrap();
            shell.send(
                &krill_term::ime::Output {
                    pty: b"typed".to_vec(),
                    discard_ime: false,
                },
                Some(window),
                cx,
            );
            request
        })
    });
    cx.run_until_parked();
    cx.executor()
        .advance_clock(super::PASTE_PREPARATION_TIMEOUT);
    cx.run_until_parked();
    let (kind, body) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(Input::decode(kind, &body).unwrap().bytes, b"typed");
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(request, Some(b"late image".to_vec()), window, cx);
        });
    });
    cx.run_until_parked();
    theirs.set_nonblocking(true).unwrap();
    assert_eq!(
        theirs.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[gpui::test]
fn pending_paste_in_another_pane_does_not_block_ready_paste(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("async-paste-independent-panes"));
    let (ours, _theirs) = UnixStream::pair().unwrap();
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(42), ours, cx);
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.begin_clipboard_paste(window, cx).unwrap();
        });
    });
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let request = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            // Keep the earlier pane's request pending to exercise independent queues.
            shell.layout.new_tab();
            let pane = shell.layout.active_tab().focused;
            shell.attach_test(pane, krill_proto::SessionId(43), ours, cx);
            shell.begin_clipboard_paste(window, cx).unwrap()
        })
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(request, Some(b"ready".to_vec()), window, cx);
        });
    });
    let (kind, body) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(Input::decode(kind, &body).unwrap().bytes, b"ready");
}

#[test]
fn remote_filename_is_sanitized() {
    assert_eq!(
        super::sanitize_remote_filename("Screen Shot 2026-10-22.png"),
        "Screen_Shot_2026-10-22.png"
    );
    assert_eq!(
        super::sanitize_remote_filename("../../etc/passwd"),
        "passwd"
    );
    assert_eq!(super::sanitize_remote_filename(""), "pasted-file");
    assert_eq!(super::sanitize_remote_filename("..."), "pasted-file");
    assert_eq!(super::sanitize_remote_filename("a/b"), "b");
}

#[test]
fn remote_text_paste_needs_no_upload() {
    let host = krill_app::ssh_hosts::SshHost {
        id: "ssh-test".to_owned(),
        label: "test".to_owned(),
        destination: "user@host.invalid".to_owned(),
        port: None,
        identity_file: None,
        remote_socket: None,
        imported: false,
        copy_id_first: false,
    };
    let item = gpui::ClipboardItem::new_string("echo hello".to_owned());
    assert_eq!(
        super::clipboard_paste_payload_for_host(&item, Some(&host)),
        Some(b"echo hello".to_vec())
    );
    assert_eq!(
        super::clipboard_paste_payload_for_host(&item, None),
        Some(b"echo hello".to_vec())
    );
}

#[test]
fn remote_image_paste_with_unusable_destination_cancels() {
    let host = krill_app::ssh_hosts::SshHost {
        id: "ssh-test".to_owned(),
        label: "test".to_owned(),
        destination: "-invalid".to_owned(),
        port: None,
        identity_file: None,
        remote_socket: None,
        imported: false,
        copy_id_first: false,
    };
    let img = gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
    );
    let item = gpui::ClipboardItem::new_image(&img);
    assert_eq!(
        super::clipboard_paste_payload_for_host(&item, Some(&host)),
        None
    );
}

#[test]
fn preparation_admission_is_held_until_backend_guard_finishes() {
    let active = std::sync::atomic::AtomicUsize::new(0);
    let first = super::PreparationPermit::acquire(&active).unwrap();
    let second = super::PreparationPermit::acquire(&active).unwrap();
    assert!(super::PreparationPermit::acquire(&active).is_none());
    // Releasing a UI request does not release these backend-owned permits.
    assert_eq!(active.load(std::sync::atomic::Ordering::Acquire), 2);
    drop(first);
    assert!(super::PreparationPermit::acquire(&active).is_some());
    drop(second);
    assert_eq!(active.load(std::sync::atomic::Ordering::Acquire), 0);
}

fn upload_test_host() -> krill_app::ssh_hosts::SshHost {
    krill_app::ssh_hosts::SshHost {
        id: "upload-test".into(),
        label: "Upload test".into(),
        destination: "test.invalid".into(),
        port: None,
        identity_file: None,
        remote_socket: None,
        imported: false,
        copy_id_first: false,
    }
}

#[test]
fn remote_upload_closes_stdin_and_uses_private_unique_paths() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = state_path("upload-eof").parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("ssh");
    std::fs::write(&script, format!(
        "#!/bin/sh\nexport TMPDIR='{}'\nfor arg in \"$@\"; do last=$arg; done\nexec /bin/sh -c \"$last\"\n",
        root.display()
    )).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let host = upload_test_host();
    let first = super::upload_bytes_with_program(
        &host,
        "image.png",
        b"one",
        script.as_os_str(),
        Duration::from_secs(3),
    )
    .unwrap();
    let second = super::upload_bytes_with_program(
        &host,
        "image.png",
        b"two",
        script.as_os_str(),
        Duration::from_secs(3),
    )
    .unwrap();
    assert_ne!(
        first, second,
        "same-named uploads must not overwrite each other"
    );
    assert_eq!(std::fs::read(&first).unwrap(), b"one");
    assert_eq!(std::fs::read(&second).unwrap(), b"two");
    assert_eq!(
        std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(std::path::Path::new(&first).parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn remote_upload_stalled_stdin_is_bounded() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = state_path("upload-timeout").parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&root).unwrap();
    let script = root.join("ssh");
    std::fs::write(&script, "#!/bin/sh\nexec /bin/sleep 10\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let start = std::time::Instant::now();
    assert!(
        super::upload_bytes_with_program(
            &upload_test_host(),
            "image.png",
            &vec![0; 1024 * 1024],
            script.as_os_str(),
            Duration::from_millis(100)
        )
        .is_none()
    );
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "a blocked pipe must not hang the paste worker"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn link_only_remote_never_receives_a_local_media_path() {
    let text = gpui::ClipboardItem::new_string("echo hello".into());
    assert_eq!(
        super::remote_text_without_upload(&text),
        Some(b"echo hello".to_vec())
    );
    let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, vec![1, 2, 3]);
    assert!(super::remote_text_without_upload(&gpui::ClipboardItem::new_image(&image)).is_none());
}

#[test]
fn remote_host_target_uploads_clipboard_bytes_without_ssh_cli() {
    use krill_proto::control::{Program, Reply, Request};
    use krill_proto::ipc::{self, ServerHello};
    use std::io::Write as _;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::net::UnixListener;

    let root = state_path("upload-via-target")
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).unwrap();
    let sock_path = root.join("remote.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();

    let server_root = root.clone();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let hello = ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features: ipc::FEATURES,
            build: "test".into(),
            instance_id: 99,
        };
        stream.write_all(&hello.encode().unwrap()).unwrap();
        drop(ipc::read_frame(&mut stream).unwrap().unwrap());
        let mut captured_stdout = Vec::new();
        while let Ok(Some((kind, body))) = ipc::read_frame(&mut stream) {
            match Request::decode(kind, &body).unwrap() {
                Request::Create(create) => {
                    if let Program::Custom { program, args } = create.program {
                        let mut cmd =
                            std::process::Command::new(std::ffi::OsStr::from_bytes(&program));
                        cmd.env("TMPDIR", &server_root);
                        for a in &args {
                            // `sleep 2` at the end is only for keeping a real PTY alive until Capture
                            let s = String::from_utf8_lossy(a).replace("sleep 2", "true");
                            cmd.arg(s);
                        }
                        let out = cmd.output().unwrap();
                        captured_stdout = out.stdout;
                    }
                    let reply = Reply::Created {
                        session: krill_proto::SessionId(1),
                        pid: 123,
                    };
                    stream.write_all(&reply.encode().unwrap()).unwrap();
                }
                Request::Capture { .. } => {
                    let reply = Reply::Captured {
                        bytes: captured_stdout.clone(),
                        seq: krill_proto::screen::Seq(1),
                    };
                    stream.write_all(&reply.encode().unwrap()).unwrap();
                }
                Request::Close { .. } => {
                    let reply = Reply::Closed {};
                    stream.write_all(&reply.encode().unwrap()).unwrap();
                    break;
                }
                _ => {}
            }
        }
    });

    let target = krill_app::hosts::HostTarget::remote("iroh:test-mac", sock_path);
    let payload = b"\x89PNG\r\n\x1a\nremote-paste-image-bytes";
    let uploaded_path = super::upload_bytes_via_target(
        &target,
        "clipboard-image.png",
        payload,
        Duration::from_secs(5),
    )
    .expect("upload via HostTarget must succeed");
    server.join().unwrap();

    assert_eq!(std::fs::read(&uploaded_path).unwrap(), payload);
    std::fs::remove_dir_all(root).unwrap();
}

#[expect(clippy::unwrap_used, reason = "테스트 헬퍼")]
fn make_test_linked(sock: UnixStream) -> krill_app::pane::Linked {
    let reader = sock.try_clone().unwrap();
    krill_app::pane::Linked {
        reader,
        writer: krill_app::pane::Writer::spawn(sock).unwrap(),
        instance: 1,
        launch: None,
        features: krill_proto::input::FEAT_INPUT,
        search: false,
    }
}

/// APP-RUNTIME-003: 첫 세션이 붙기 전(`session == None`)에 시작한 느린 붙여넣기(`Pending`)와 여러 줄 경고(`Confirming`)는
/// `Shell::linked` 시점에 첫 세션 ID로 묶이며, 이후 `restart`·세션 재바인딩으로 두 번째 세션이 붙어도 절대 새지 않는다.
#[gpui::test]
fn pre_attach_pending_and_confirming_pastes_bind_to_first_session_and_never_leak(
    cx: &mut TestAppContext,
) {
    let (view, cx) = open(cx, state_path("paste-bind-first-session"));
    let pane = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);

    // 1. 첫 세션 연결 전에 느린 클립보드 붙여넣기(Pending)와 뒤따르는 키 입력을 넣는다.
    let pending_id = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let id = shell
                .begin_clipboard_paste(window, cx)
                .expect("pre-attach clipboard paste must be accepted");
            shell.send(&typed(b"after-slow\r"), Some(window), cx);
            assert!(
                shell
                    .paste_queue
                    .entries
                    .iter()
                    .all(|e| e.pane == pane && e.session.is_none())
            );
            id
        })
    });

    // Shell::linked로 첫 세션(41)에 붙으면 아직 Pending인 항목도 즉시 Some(SessionId(41))에 묶인다.
    let (first_sock, mut first_peer) = UnixStream::pair().unwrap();
    let _first_next = view.update(cx, |shell, cx| {
        let next = shell.linked(
            pane,
            krill_proto::SessionId(41),
            make_test_linked(first_sock),
            cx,
        );
        assert!(
            shell
                .paste_queue
                .entries
                .iter()
                .all(|e| e.session == Some(krill_proto::SessionId(41))),
            "Shell::linked must promote pre-attach None entries to Some(first_session)"
        );
        next
    });
    assert_nothing_sent(&mut first_peer);

    // 1-a. restart 없이 같은 Live가 두 번째 세션(42)에 붙더라도, 첫 세션(41)으로 고정된 큐는 42에 절대 들어가지 않는다.
    let (direct_sock, mut direct_peer) = UnixStream::pair().unwrap();
    direct_peer
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let _direct_next = view.update(cx, |shell, cx| {
        shell.linked(
            pane,
            krill_proto::SessionId(42),
            make_test_linked(direct_sock),
            cx,
        )
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(pending_id, Some(b"echo leaked\r".to_vec()), window, cx);
            shell.send(&typed(b"second-only\r"), Some(window), cx);
        });
    });
    assert_eq!(read_input(&mut direct_peer), b"second-only\r");
    assert_nothing_sent(&mut direct_peer);

    // 1-b. 다시 Pending 붙여넣기가 걸린 상태에서 restart가 돌면 큐가 즉시 정리되고 다음 세션(43)은 깨끗하다.
    let pending_restart_id = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let id = shell.begin_clipboard_paste(window, cx).unwrap();
            shell.send(&typed(b"dropped-on-restart\r"), Some(window), cx);
            id
        })
    });
    let (restart_sock, mut restart_peer) = UnixStream::pair().unwrap();
    restart_peer
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let _restart_next = view.update(cx, |shell, cx| {
        shell.restart(pane, cx);
        assert!(!shell.paste_queue.has_pending_for(pane));
        let _ = shell.ensure_input_live(pane);
        shell.linked(
            pane,
            krill_proto::SessionId(43),
            make_test_linked(restart_sock),
            cx,
        )
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(
                pending_restart_id,
                Some(b"echo restart-leaked\r".to_vec()),
                window,
                cx,
            );
            shell.send(&typed(b"third-only\r"), Some(window), cx);
        });
    });
    assert_eq!(read_input(&mut restart_peer), b"third-only\r");
    assert_nothing_sent(&mut restart_peer);

    // 2. 새 탭에서 첫 세션 전에 여러 줄 붙여넣기(Confirming)가 걸린 뒤 계정/콜드 재바인딩으로 세션이 바뀌는 경로.
    let pane2 = view.update(cx, |shell, cx| {
        shell.new_tab_on_launch_host(cx);
        shell.layout.active_tab().focused
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.paste_bytes(b"echo one\necho two", window, cx);
            shell.send(&typed(b"queued-behind-warn\r"), Some(window), cx);
            assert!(shell.security.pending_paste().is_some());
        });
    });
    let (s51, mut peer51) = UnixStream::pair().unwrap();
    let _next51 = view.update(cx, |shell, cx| {
        shell.layout.set_session(pane2, krill_proto::SessionId(51));
        let next = shell.linked(pane2, krill_proto::SessionId(51), make_test_linked(s51), cx);
        assert!(
            shell
                .paste_queue
                .entries
                .iter()
                .filter(|e| e.pane == pane2)
                .all(|e| e.session == Some(krill_proto::SessionId(51)))
        );
        next
    });
    assert_nothing_sent(&mut peer51);

    // 두 번째 세션(52)으로 교체된 뒤 배너 확인을 눌러도 51의 여러 줄 페이로드는 52에 전달되지 않는다.
    let (s52, mut peer52) = UnixStream::pair().unwrap();
    peer52
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let _next52 = view.update(cx, |shell, cx| {
        for rebound in shell.layout.rebind_session(
            &krill_app::hosts::HostId::Local,
            krill_proto::SessionId(51),
            krill_proto::SessionId(52),
        ) {
            shell.drop_stale_pastes_for_pane(rebound, Some(krill_proto::SessionId(52)), cx);
        }
        shell.linked(pane2, krill_proto::SessionId(52), make_test_linked(s52), cx)
    });
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            assert!(shell.security.pending_paste().is_none());
            shell.confirm_pending_paste(window, cx);
            shell.send(&typed(b"clean-52\r"), Some(window), cx);
        });
    });
    assert_eq!(read_input(&mut peer52), b"clean-52\r");
    assert_nothing_sent(&mut peer52);
}

/// APP-RUNTIME-003 + 004: `layout_loading` 중(보이는 터미널 칸에 아직 데몬 연결이 없을 때) 입력한
/// 한글 IME·클립보드 붙여넣기·키 입력은 버려지지 않고, 레이아웃 복원 뒤에도 같은 `PaneId`의 첫 세션에
/// 제출 순서대로 도착하며 복원된 기존 세션에는 한 바이트도 섞이지 않는다(STYLEGUIDE §7.0).
#[gpui::test]
fn startup_loading_accepts_ime_paste_and_keys_and_delivers_to_first_session(
    cx: &mut TestAppContext,
) {
    let path = state_path("startup-early-input-paste-ime");
    let mut saved = krill_app::layout::Layout::default();
    let saved_pane_orig = saved.focused().unwrap();
    saved.set_session(saved_pane_orig, krill_proto::SessionId(7));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    saved.save(&path).unwrap();

    let state_dir = path.parent().map(std::path::Path::to_path_buf);
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::shell::bind_keys(cx);
        krill_app::settings::init_at(state_dir, cx);
    });
    let (release, gate) = tokio::sync::oneshot::channel();
    let (view, cx) = cx.add_window_view(|window, cx| {
        let shell = crate::shell::Shell::new_with_restore_gate(Some(path.clone()), gate, cx);
        assert!(shell.layout_loading);
        shell.focus.focus(window, cx);
        shell
    });

    // 1. 초기 placeholder 탭(PaneId(0))과 초기 Cmd+T 탭(PaneId(1)) 모두 layout_loading 중 입력을 받는다.
    let (placeholder_pane, early_new_pane, paste_req) = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            assert!(shell.layout_loading);
            let p0 = shell.layout.focused().unwrap();
            assert!(
                gpui::EntityInputHandler::selected_text_range(shell, false, window, cx).is_some(),
                "terminal text input must stay enabled during layout_loading"
            );
            gpui::EntityInputHandler::replace_and_mark_text_in_range(
                shell,
                None,
                "한",
                Some(0..1),
                window,
                cx,
            );
            gpui::EntityInputHandler::replace_text_in_range(shell, None, "한글 ", window, cx);
            shell.send(&typed(b"pwd\r"), Some(window), cx);

            shell.new_tab_on_launch_host(cx);
            let p1 = shell.layout.focused().unwrap();
            gpui::EntityInputHandler::replace_text_in_range(shell, None, "echo ", window, cx);
            let req = shell
                .begin_clipboard_paste(window, cx)
                .expect("Cmd+V during layout_loading must queue onto focused pane");
            shell.send(&typed(b" done\r"), Some(window), cx);
            (p0, p1, req)
        })
    });

    // 2. 레이아웃 복원 완료: 저장된 탭(0) + 입력이 걸린 초기 탭 두 개(1, 2)가 모두 보존되고 초기 PaneId도 그대로다.
    release.send(()).unwrap();
    cx.run_until_parked();
    let restored_saved_pane = view.read_with(cx, |shell, _| {
        assert!(!shell.layout_loading);
        assert_eq!(shell.layout.tabs().len(), 3);
        assert_eq!(shell.layout.tab_panes(1), vec![placeholder_pane]);
        assert_eq!(shell.layout.tab_panes(2), vec![early_new_pane]);
        assert_eq!(shell.layout.active(), 2);
        let saved_pane = shell.layout.tab_panes(0)[0];
        assert_eq!(
            shell.layout.session(saved_pane),
            Some(krill_proto::SessionId(7))
        );
        saved_pane
    });

    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.complete_clipboard_paste(paste_req, Some(b"world".to_vec()), window, cx);
        });
    });

    let (saved_sock, mut saved_peer) = UnixStream::pair().unwrap();
    let (p0_sock, mut p0_peer) = UnixStream::pair().unwrap();
    let (p1_sock, mut p1_peer) = UnixStream::pair().unwrap();
    p0_peer
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    p1_peer
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (_saved_next, _p0_next, _p1_next) = view.update(cx, |shell, cx| {
        let _ = shell.ensure_input_live(restored_saved_pane);
        let s_next = shell.linked(
            restored_saved_pane,
            krill_proto::SessionId(7),
            make_test_linked(saved_sock),
            cx,
        );
        let p0_next = shell.linked(
            placeholder_pane,
            krill_proto::SessionId(101),
            make_test_linked(p0_sock),
            cx,
        );
        let p1_next = shell.linked(
            early_new_pane,
            krill_proto::SessionId(102),
            make_test_linked(p1_sock),
            cx,
        );
        (s_next, p0_next, p1_next)
    });

    assert_nothing_sent(&mut saved_peer);
    let mut got_p0 = read_input(&mut p0_peer);
    got_p0.extend_from_slice(&read_input(&mut p0_peer));
    assert_eq!(got_p0, "한글 pwd\r".as_bytes());

    let mut got_p1 = read_input(&mut p1_peer);
    got_p1.extend_from_slice(&read_input(&mut p1_peer));
    got_p1.extend_from_slice(&read_input(&mut p1_peer));
    assert_eq!(got_p1, b"echo world done\r");
}

#[gpui::test]
fn hidden_confirming_paste_and_following_keys_survive_other_tab_attach(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("hidden-confirming-queue"));
    let original = view.read_with(cx, |shell, _| shell.layout.active_tab().focused);
    let (initial, _initial_peer) = UnixStream::pair().unwrap();
    view.update(cx, |shell, cx| {
        shell.attach_test(original, krill_proto::SessionId(42), initial, cx);
    });
    let second = cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.paste_bytes(b"echo first\necho second", window, cx);
            shell.send(&typed(b"following keys"), Some(window), cx);
            shell.new_tab_on_launch_host(cx);
            assert!(
                shell.live.contains_key(&original),
                "pending confirmation keeps its Live identity across navigation"
            );
            shell.layout.active_tab().focused
        })
    });
    // The hidden pane can finish a reattach while its confirmation is pending.
    let (reattach, mut original_peer) = UnixStream::pair().unwrap();
    original_peer
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    view.update(cx, |shell, cx| {
        let got = super::super::Got::Live {
            session: krill_proto::SessionId(42),
            linked: Ok(krill_app::pane::Linked {
                reader: reattach.try_clone().unwrap(),
                writer: krill_app::pane::Writer::spawn(reattach).unwrap(),
                instance: 1,
                launch: None,
                features: krill_proto::input::FEAT_INPUT,
                search: false,
            }),
            prior_cold: None,
        };
        assert!(
            matches!(
                shell.settled(original, Ok(got), cx),
                super::super::Next::Read(_)
            ),
            "settling a hidden pending confirmation must retain its connection"
        );
    });
    let (other, mut other_peer) = UnixStream::pair().unwrap();
    view.update(cx, |shell, cx| {
        shell.attach_test(second, krill_proto::SessionId(43), other, cx);
        assert!(shell.paste_queue.has_pending_for(original));
        shell.change(true, cx, |layout| layout.activate(0));
        shell.refresh_pending_paste_banner(cx);
    });
    cx.update(|window, cx| view.update(cx, |shell, cx| shell.confirm_pending_paste(window, cx)));
    assert_eq!(read_input(&mut original_peer), b"echo first\recho second");
    assert_eq!(read_input(&mut original_peer), b"following keys");
    // Returning to the original tab may release the other tab's idle connection.
    // Both EOF and an empty open connection prove that no bytes went to that PTY.
    other_peer.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_millis(250);
    loop {
        match other_peer.read(&mut [0]) {
            Ok(0) => break,
            Ok(_) => panic!("confirmation reached the other tab"),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("other peer read failed: {error}"),
        }
    }
    assert!(view.read_with(cx, |shell, _| !shell.paste_queue.has_pending_for(original)));
}

#[gpui::test]
fn cold_read_only_pane_never_accepts_clipboard_confirmation(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("cold-clipboard-guard"));
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.active_tab().focused;
            let mut live = super::super::Live::new(gpui::Task::ready(()));
            live.pane
                .show_cold(
                    krill_proto::SessionId(42),
                    krill_proto::control::Cold {
                        screen: vec![krill_proto::screen::Op::Reset {
                            cols: 80,
                            lines: 24,
                        }],
                        history: Vec::new(),
                    },
                )
                .unwrap();
            shell.live.insert(pane, live);
            assert!(shell.begin_clipboard_paste(window, cx).is_none());
            shell.paste_bytes(b"echo first\necho second", window, cx);
            assert!(!shell.paste_queue.has_pending_for(pane));
            assert!(shell.security.pending_paste().is_none());
        });
    });
}

#[gpui::test]
fn input_policy_drop_routes_explicit_shells_and_image_attachments(cx: &mut TestAppContext) {
    use krill_proto::control::{Create, Program};
    let (view, cx) = open(cx, state_path("drop-policy-routing"));
    let pane = view.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    let (ours, mut peer) = UnixStream::pair().unwrap();
    view.update(cx, |shell, cx| {
        shell.attach_test(pane, krill_proto::SessionId(713), ours, cx);
    });
    for (program, path, expected) in [
        (
            Program::Custom {
                program: b"powershell.exe".to_vec(),
                args: vec![],
            },
            r"C:\tmp\it's (a b).txt",
            br"'C:\tmp\it''s (a b).txt' ".as_slice(),
        ),
        (
            Program::Custom {
                program: b"cmd.exe".to_vec(),
                args: vec![],
            },
            r"C:\tmp\it's (a b).txt",
            br#""C:\tmp\it's (a b).txt" "#,
        ),
        (
            Program::Agent { id: "codex".into() },
            "/tmp/it's (a b).png",
            b"\x1b[200~/tmp/it\\'s\\ \\(a\\ b\\).png\x1b[201~",
        ),
    ] {
        cx.update(|window, cx| {
            view.update(cx, |shell, cx| {
                shell.layout.set_launch(
                    pane,
                    Create {
                        program,
                        cwd: vec![],
                        cols: 80,
                        rows: 24,
                        account: None,
                    },
                );
                let paths =
                    gpui::ExternalPaths([std::path::PathBuf::from(path)].into_iter().collect());
                shell.paste_dropped_paths(&paths, window, cx);
            });
        });
        cx.run_until_parked();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let (kind, body) = krill_proto::ipc::read_frame(&mut peer).unwrap().unwrap();
        assert_eq!(Input::decode(kind, &body).unwrap().bytes, expected);
    }
}

#[gpui::test]
fn input_policy_late_preparation_cannot_reuse_a_changed_shell_or_attachment_policy(
    cx: &mut TestAppContext,
) {
    use krill_proto::control::{Create, Program};
    let (view, cx) = open(cx, state_path("drop-policy-changed"));
    let (ours, mut peer) = UnixStream::pair().unwrap();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            shell.attach_test(pane, krill_proto::SessionId(714), ours, cx);
            let request = shell.begin_clipboard_paste(window, cx).unwrap();
            shell.layout.set_launch(
                pane,
                Create {
                    program: Program::Custom {
                        program: b"pwsh.exe".to_vec(),
                        args: vec![],
                    },
                    cwd: vec![],
                    cols: 80,
                    rows: 24,
                    account: None,
                },
            );
            shell.complete_attachment_paste(
                request,
                Some(b"/tmp/old\\ image.png".to_vec()),
                true,
                window,
                cx,
            );
            assert!(shell.paste_queue.entries.is_empty());
        });
    });
    cx.run_until_parked();
    peer.set_nonblocking(true).unwrap();
    assert!(peer.read(&mut [0; 64]).is_err());
}

#[gpui::test]
fn input_policy_uses_observed_remote_shell_and_os_without_local_platform_guesses(
    cx: &mut TestAppContext,
) {
    use krill_app::hosts::{HostId, HostStatus};
    use krill_proto::control::{AgentStatus, Create, Program, RootShell, ShellOs};
    use krill_term::paste::{InputPolicy, ShellFamily};
    let (view, cx) = open(cx, state_path("observed-remote-input-policy"));
    for (os, program, foreground, expected_shell, expected_input) in [
        (
            ShellOs::Windows,
            "pwsh.exe",
            "C:\\tools\\CODEX.CMD",
            ShellFamily::PowerShell,
            InputPolicy::WindowsCodex,
        ),
        (
            ShellOs::Windows,
            "cmd.exe",
            "PI.EXE",
            ShellFamily::Cmd,
            InputPolicy::WindowsPi,
        ),
        (
            ShellOs::Unix,
            "zsh",
            "codex",
            ShellFamily::Posix,
            InputPolicy::ByteStream,
        ),
        (
            ShellOs::Unix,
            "nu",
            "",
            ShellFamily::Unknown,
            InputPolicy::ByteStream,
        ),
        (
            ShellOs::Windows,
            "codex.exe",
            "powershell.exe",
            ShellFamily::PowerShell,
            InputPolicy::ByteStream,
        ),
        (
            ShellOs::Windows,
            "wsl.exe",
            "codex",
            ShellFamily::WindowsUnknown,
            InputPolicy::ByteStream,
        ),
    ] {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            let session = krill_proto::SessionId(777);
            shell.layout.set_session(pane, session);
            shell
                .layout
                .set_host(pane, HostId::Remote("observed".into()));
            shell.layout.set_launch(
                pane,
                Create {
                    program: Program::Agent { id: "codex".into() },
                    cwd: vec![],
                    cols: 80,
                    rows: 24,
                    account: None,
                },
            );
            shell.sidebar.update(cx, |sidebar, cx| {
                sidebar.set_remote_snapshot_for_test(
                    "observed",
                    HostStatus::Connected,
                    krill_app::sidebar::Snapshot {
                        sessions: vec![krill_app::sidebar::Session {
                            id: session,
                            status: AgentStatus {
                                foreground: (!foreground.is_empty()).then(|| foreground.to_owned()),
                                root_shell: Some(Box::new(RootShell {
                                    os,
                                    program: program.into(),
                                })),
                                ..AgentStatus::UNKNOWN
                            },
                            cwd: None,
                            worktree: None,
                            account: None,
                        }],
                        trees: std::collections::BTreeMap::default(),
                    },
                    cx,
                );
            });
            assert_eq!(
                shell.drop_policy_for(pane, cx).shell,
                expected_shell,
                "{program}/{foreground}"
            );
            assert_eq!(
                shell.input_policy_for(pane, cx),
                expected_input,
                "{program}/{foreground}"
            );
        });
    }
}

// Regressions for native attachment formatting, shell handoff and refused file drops.
#[expect(clippy::unwrap_used, reason = "test fixture opens one focused pane")]
fn observed_status_for_attachment_regression(
    shell: &mut super::Shell,
    root: &str,
    foreground: &str,
    cx: &mut gpui::Context<super::Shell>,
) -> krill_app::layout::PaneId {
    use krill_app::hosts::{HostId, HostStatus};
    use krill_proto::control::{AgentStatus, Create, Program, RootShell, ShellOs};
    let pane = shell.layout.focused().unwrap();
    let session = krill_proto::SessionId(979);
    shell.layout.set_session(pane, session);
    shell
        .layout
        .set_host(pane, HostId::Remote("verify-input".into()));
    shell.layout.set_launch(
        pane,
        Create {
            program: Program::Agent { id: "codex".into() },
            cwd: vec![],
            cols: 80,
            rows: 24,
            account: None,
        },
    );
    shell.sidebar.update(cx, |sidebar, cx| {
        sidebar.set_remote_snapshot_for_test(
            "verify-input",
            HostStatus::Connected,
            krill_app::sidebar::Snapshot {
                sessions: vec![krill_app::sidebar::Session {
                    id: session,
                    status: AgentStatus {
                        foreground: Some(foreground.into()),
                        root_shell: Some(Box::new(RootShell {
                            os: ShellOs::Windows,
                            program: root.into(),
                        })),
                        ..AgentStatus::UNKNOWN
                    },
                    cwd: None,
                    worktree: None,
                    account: None,
                }],
                trees: std::collections::BTreeMap::default(),
            },
            cx,
        );
    });
    pane
}

#[gpui::test]
fn native_codex_image_uses_windows_paths_under_git_bash(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("verifier-codex-git-bash-image"));
    view.update(cx, |shell, cx| {
        let pane = observed_status_for_attachment_regression(shell, "bash.exe", "codex.exe", cx);
        assert_eq!(
            shell.input_policy_for(pane, cx),
            krill_term::paste::InputPolicy::WindowsCodex
        );
        let policy = shell.drop_policy_for(pane, cx);
        let formatted =
            krill_term::paste::dropped_with_policy([br"C:\tmp\it's (a b).png".as_slice()], policy)
                .unwrap();
        assert_eq!(
            formatted, br#""C:\tmp\it's (a b).png""#,
            "Native Windows consumer must not receive POSIX escapes of path separators"
        );
    });
}

#[gpui::test]
fn foreground_shell_does_not_keep_codex_image_attachment_contract(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("verifier-codex-child-shell-image"));
    view.update(cx, |shell, cx| {
        let pane =
            observed_status_for_attachment_regression(shell, "codex.exe", "powershell.exe", cx);
        assert_eq!(
            shell.input_policy_for(pane, cx),
            krill_term::paste::InputPolicy::ByteStream
        );
        assert!(
            !shell.drop_policy_for(pane, cx).image_attachments,
            "Shell child cannot parse the provider's forced bracketed image paste"
        );
    });
}

#[gpui::test]
fn refused_shell_path_has_user_feedback(cx: &mut TestAppContext) {
    use krill_proto::control::{Create, Program};
    let (view, cx) = open(cx, state_path("verifier-rejected-drop-feedback"));
    let pane = view.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    let (ours, mut peer) = UnixStream::pair().unwrap();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.attach_test(pane, krill_proto::SessionId(976), ours, cx);
            shell.layout.set_launch(
                pane,
                Create {
                    program: Program::Custom {
                        program: b"cmd.exe".to_vec(),
                        args: vec![],
                    },
                    cwd: vec![],
                    cols: 80,
                    rows: 24,
                    account: None,
                },
            );
            shell.notice = None;
            let paths = gpui::ExternalPaths(
                [std::path::PathBuf::from(r"C:\tmp\%TEMP%.txt")]
                    .into_iter()
                    .collect(),
            );
            shell.paste_dropped_paths(&paths, window, cx);
            assert!(!shell.paste_queue.has_pending_for(pane));
            assert_nothing_sent(&mut peer);
            assert!(
                shell.notice.is_some(),
                "Refused file paths disappear without feedback"
            );
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("notice").is_some(),
        "Refused paths must show a visible notice"
    );
}
