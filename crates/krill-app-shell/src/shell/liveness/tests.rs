use std::io::Read as _;

use krill_proto::input::{Input, InputAck, InputResult};
use krill_proto::screen::Msg as _;

use super::*;
use crate::shell::tests::{open, state_path};

#[expect(
    clippy::unwrap_used,
    reason = "test fixture must establish its socket pair"
)]
fn attach(shell: &mut Shell, pane: PaneId, session: u64, cx: &mut Context<Shell>) -> UnixStream {
    let (ours, peer) = UnixStream::pair().unwrap();
    let shutdown = ours.try_clone().unwrap();
    shell.attach_test(pane, krill_proto::SessionId(session), ours, cx);
    shell.live.get_mut(&pane).unwrap().sock = Some(shutdown);
    shell
        .layout
        .set_session(pane, krill_proto::SessionId(session));
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    peer
}

#[gpui::test]
fn stalled_stream_recovers_without_render_or_replaying_uncertain_input(
    cx: &mut gpui::TestAppContext,
) {
    let (view, cx) = open(cx, state_path("stalled-stream"));
    let (pane, mut peer, sent) = cx.update(|_, cx| {
        view.update(cx, |shell, cx| {
            // Control: retain the previous behavior (no independent liveness tick).
            shell.pane_liveness = Some(Task::ready(()));
            shell.link = Link::Up; // The separate watch connection is healthy.
            let pane = shell.layout.focused().unwrap();
            let mut peer = attach(shell, pane, 91, cx);
            let old = Now {
                awake: Instant::now()
                    .checked_sub(krill_proto::input::PROBE)
                    .unwrap(),
                ..Now::get()
            };
            assert!(
                shell
                    .live
                    .get_mut(&pane)
                    .unwrap()
                    .pane
                    .input(b"once\r", old)
                    .is_none()
            );
            let (kind, body) = ipc::read_frame(&mut peer).unwrap().unwrap();
            let input = Input::decode(kind, &body).unwrap();
            (pane, peer, input)
        })
    });
    // No incoming frames, cursor blink, user input, or rendering is needed.
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    cx.update(|_, cx| {
        view.update(cx, |shell, cx| {
            assert!(shell.live[&pane].sock.is_some());
            assert!(shell.live[&pane].pane.is_linked());
            assert!(shell.live[&pane].pane.stalled(Now::get()));
            shell.pane_liveness = None;
            shell.start_pane_liveness(cx);
        });
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert!(
            view.read(cx).live[&pane].sock.is_some(),
            "one missed probe is suspect"
        );
    });
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    cx.update(|_, cx| {
        view.update(cx, |shell, _cx| {
            assert_eq!(shell.link, Link::Up);
            assert_eq!(shell.layout.session(pane), Some(krill_proto::SessionId(91)));
            let live = shell.live.get_mut(&pane).unwrap();
            assert!(live.sock.is_none(), "stuck stream must be shut down");
            assert!(!live.pane.is_linked());
            assert!(!live.pane.is_closed());
            assert!(shell.notice.is_some(), "uncertain input must be disclosed");
            assert!(shell.closed_sessions.is_empty());
            // Existing reconnect loop's link path sends only subsequently held input.
            assert!(
                shell
                    .live
                    .get_mut(&pane)
                    .unwrap()
                    .pane
                    .input(b"after\r", Now::get())
                    .is_none()
            );
            let (ours, mut replacement) = UnixStream::pair().unwrap();
            replacement
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let live = shell.live.get_mut(&pane).unwrap();
            assert!(
                live.pane
                    .link(
                        krill_proto::SessionId(91),
                        1,
                        pane::Writer::spawn(ours).unwrap(),
                        krill_proto::input::FEAT_INPUT,
                        Now::get(),
                    )
                    .is_empty()
            );
            let (kind, body) = ipc::read_frame(&mut replacement).unwrap().unwrap();
            assert_eq!(Input::decode(kind, &body).unwrap().bytes, b"after\r");
            replacement.set_nonblocking(true).unwrap();
            assert!(
                ipc::read_frame(&mut replacement).is_err(),
                "uncertain command must not replay"
            );
        });
    });
    assert_eq!(sent.bytes, b"once\r");
    assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
}

#[gpui::test]
fn liveness_leaves_acked_idle_and_account_switch_streams_connected(cx: &mut gpui::TestAppContext) {
    let (view, cx) = open(cx, state_path("healthy-stream"));
    let (pane, mut peer) = cx.update(|_, cx| {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.focused().unwrap();
            let mut peer = attach(shell, pane, 92, cx);
            let old = Now {
                awake: Instant::now()
                    .checked_sub(krill_proto::input::PROBE)
                    .unwrap(),
                ..Now::get()
            };
            shell.live.get_mut(&pane).unwrap().pane.input(b"ok\r", old);
            let (kind, body) = ipc::read_frame(&mut peer).unwrap().unwrap();
            let sent = Input::decode(kind, &body).unwrap();
            let ack = InputAck {
                id: sent.id,
                result: InputResult::Applied,
            }
            .encode()
            .unwrap();
            let (kind, body) = ipc::read_frame(&mut ack.as_slice()).unwrap().unwrap();
            assert!(shell.on_frame(pane, kind, &body, cx));
            shell.recover_stalled_panes(Now::get(), cx);
            assert!(shell.live[&pane].sock.is_some());
            // Account handoff owns its input queue and connection lifecycle.
            let live = shell.live.get_mut(&pane).unwrap();
            live.pane.input(b"switching\r", old);
            live.pane.pause_account_input();
            shell.recover_stalled_panes(Now::get(), cx);
            assert!(shell.live[&pane].sock.is_some());
            (pane, peer)
        })
    });
    cx.executor().advance_clock(Duration::from_secs(3));
    cx.run_until_parked();
    cx.update(|_, cx| assert!(view.read(cx).live[&pane].sock.is_some()));
    peer.set_nonblocking(true).unwrap();
    // The second input was sent before pausing; there is no Close request.
    let (kind, body) = ipc::read_frame(&mut peer).unwrap().unwrap();
    assert_eq!(Input::decode(kind, &body).unwrap().bytes, b"switching\r");
}

#[gpui::test]
fn lost_writer_closes_only_its_stream_and_preserves_buffered_input(cx: &mut gpui::TestAppContext) {
    let (view, cx) = open(cx, state_path("lost-writer"));
    let (broken, healthy, mut peer, _healthy_peer) = cx.update(|_, cx| {
        view.update(cx, |shell, cx| {
            let broken = shell.layout.focused().unwrap();
            let peer = attach(shell, broken, 93, cx);
            shell.layout.split(Axis::Horizontal);
            let healthy = shell.layout.focused().unwrap();
            let healthy_peer = attach(shell, healthy, 94, cx);
            // A queue admission/write error unlinks the model before the writer
            // has drained its existing queue and closed the still-open reader.
            let live = shell.live.get_mut(&broken).unwrap();
            live.pane.unlink();
            assert!(live.pane.input(b"held\r", Now::get()).is_none());
            (broken, healthy, peer, healthy_peer)
        })
    });
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    cx.update(|_, cx| {
        view.update(cx, |shell, _cx| {
            assert!(shell.live[&broken].sock.is_none());
            assert!(shell.live[&healthy].sock.is_some());
            assert!(shell.closed_sessions.is_empty());
            let (ours, mut replacement) = UnixStream::pair().unwrap();
            replacement
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let live = shell.live.get_mut(&broken).unwrap();
            assert!(
                live.pane
                    .link(
                        krill_proto::SessionId(93),
                        1,
                        pane::Writer::spawn(ours).unwrap(),
                        krill_proto::input::FEAT_INPUT,
                        Now::get(),
                    )
                    .is_empty()
            );
            let (kind, body) = ipc::read_frame(&mut replacement).unwrap().unwrap();
            assert_eq!(Input::decode(kind, &body).unwrap().bytes, b"held\r");
        });
    });
    assert_eq!(peer.read(&mut [0; 1]).unwrap(), 0);
}
