#![allow(
    clippy::expect_used,
    reason = "Real PTY fixture setup must fail the test on any OS error"
)]
use super::*;
use crate::appearance::Appearance;
use crate::session::{Session, lock};
use krill_proto::{SessionId, holder::WinSize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn fixture() -> (Session, std::fs::File) {
    let pty = rustix_openpty::openpty(None, None).expect("PTY fixture operation");
    let mut termios = rustix::termios::tcgetattr(&pty.user).expect("PTY fixture operation");
    termios.make_raw();
    rustix::termios::tcsetattr(&pty.user, rustix::termios::OptionalActions::Now, &termios)
        .expect("PTY fixture operation");
    rustix::fs::fcntl_setfl(
        &pty.user,
        rustix::fs::fcntl_getfl(&pty.user).expect("PTY fixture operation")
            | rustix::fs::OFlags::NONBLOCK,
    )
    .expect("PTY fixture operation");
    let session = Session::start(
        SessionId(91),
        pty.controller,
        WinSize { cols: 80, rows: 24 },
        Arc::new(Appearance::load(PathBuf::new())),
        None,
    )
    .expect("PTY fixture operation");
    let mut user = std::fs::File::from(pty.user);
    user.write_all(b"EMPTY").expect("PTY fixture operation");
    let started = Instant::now();
    while !session.screen_text().contains("EMPTY") {
        assert!(started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(110));
    (session, user)
}

fn text(screen: &krill_proto::screen::Screen) -> String {
    screen.rows.iter().flatten().map(|c| c.ch).collect()
}
fn read_until(user: &mut std::fs::File, expected: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let started = Instant::now();
    while bytes.len() < expected {
        let mut chunk = [0; 512];
        match user.read(&mut chunk) {
            Ok(n) => bytes.extend_from_slice(chunk.get(..n).expect("read count within buffer")),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
            }
            other => {
                assert!(matches!(other, Ok(0)), "unexpected PTY read: {other:?}");
            }
        }
        assert!(started.elapsed() < Duration::from_secs(3));
    }
    bytes
}

#[test]
fn stale_busy_draft_and_incomplete_paste_write_nothing() {
    let (session, mut user) = fixture();
    let tap = session.tap();
    let (_, revision, _) = tap.capture();
    assert!(
        guarded_command(
            &tap,
            revision.0.saturating_add(1),
            b"/model actual",
            |_, _| true
        )
        .is_err()
    );
    assert!(guarded_command(&tap, revision.0, b"/model actual", |_, _| false).is_err());
    {
        let _busy = lock(&tap.0.input);
        assert!(guarded_command(&tap, revision.0, b"/model actual", |_, _| true).is_err());
    }
    lock(&tap.0.input).paste = true;
    assert!(guarded_command(&tap, revision.0, b"/model actual", |_, _| true).is_err());
    lock(&tap.0.input).paste = false;
    let mut b = [0; 512];
    assert_eq!(
        user.read(&mut b)
            .expect_err("write must be rejected")
            .kind(),
        io::ErrorKind::WouldBlock
    );
    drop(user);
    session.wait().expect("PTY fixture operation");
}

#[test]
fn echo_precedes_enter_and_other_input_cannot_interleave() {
    let (session, mut user) = fixture();
    let tap = session.tap();
    let (_, revision, _) = tap.capture();
    let writer = tap.clone();
    let command = b"/model actual";
    let worker = thread::spawn(move || {
        guarded_command(&writer, revision.0, command, |s, pasted| {
            text(s).contains(if pasted { "/model actual" } else { "EMPTY" })
        })
    });
    assert_eq!(read_until(&mut user, command.len()), command);
    let mut b = [0; 512];
    assert_eq!(
        user.read(&mut b)
            .expect_err("write must be rejected")
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let other = tap.clone();
    let competing = thread::spawn(move || other.write_input(b"other", None));
    user.write_all(b"\x1b[2J\x1b[H/model actual")
        .expect("PTY fixture operation");
    assert_eq!(read_until(&mut user, 6), b"\rother");
    worker
        .join()
        .expect("PTY fixture operation")
        .expect("PTY fixture operation");
    competing
        .join()
        .expect("PTY fixture operation")
        .expect("PTY fixture operation");
    drop(user);
    session.wait().expect("PTY fixture operation");
}

#[test]
fn unknown_echo_never_submits_and_releases_input_gate() {
    let (session, mut user) = fixture();
    let tap = session.tap();
    let (_, revision, _) = tap.capture();
    let writer = tap.clone();
    let worker = thread::spawn(move || {
        guarded_command(&writer, revision.0, b"/model actual", |_, pasted| !pasted)
    });
    assert_eq!(read_until(&mut user, 13), b"/model actual");
    assert!(worker.join().expect("PTY fixture operation").is_err());
    tap.write_input(b"other", None)
        .expect("PTY fixture operation");
    assert_eq!(read_until(&mut user, 5), b"other");
    drop(user);
    session.wait().expect("PTY fixture operation");
}
