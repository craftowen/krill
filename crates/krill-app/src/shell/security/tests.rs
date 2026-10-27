use std::io::Read as _;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use gpui::TestAppContext;
use krill_proto::input::{Input, Now};
use krill_proto::screen::Msg as _;

use crate::shell::tests::{open, state_path};

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
