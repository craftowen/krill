//! Safe Win32 console adapter. No FFI escapes winapi-util.
use std::io::{self, Read as _, Write as _};
use std::net::Shutdown;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use krill_proto::os::net::UnixStream;
use krill_proto::{ipc, screen};
use winapi_util::console;
use windows_sys::Win32::System::Console::{
    DISABLE_NEWLINE_AUTO_RETURN, ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT,
    ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT, ENABLE_QUICK_EDIT_MODE,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
};

use super::{Event, RESET, Result, fail};

pub(super) struct Host {
    input: Option<u32>,
    output: Option<u32>,
}
impl Host {
    pub(super) fn enter() -> Result<Self> {
        // Construct the guard first so a later setup failure restores earlier changes.
        let mut host = Self {
            input: None,
            output: None,
        };
        if let Ok(original) = console::mode(io::stdin()) {
            host.input = Some(original);
            console::set_mode(io::stdin(), input_mode(original)).map_err(fail)?;
        }
        if let Ok(original) = console::mode(io::stdout()) {
            host.output = Some(original);
            console::set_mode(
                io::stdout(),
                original
                    | ENABLE_PROCESSED_OUTPUT
                    | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                    | DISABLE_NEWLINE_AUTO_RETURN,
            )
            .map_err(fail)?;
            io::stdout().write_all(b"\x1b[?1049h").map_err(fail)?;
        }
        Ok(host)
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        if let Some(original) = self.input {
            console::set_mode(io::stdin(), original).ok();
        }
        if let Some(original) = self.output {
            let mut out = io::stdout().lock();
            out.write_all(RESET).ok();
            out.flush().ok();
            console::set_mode(io::stdout(), original).ok();
        }
    }
}
fn input_mode(original: u32) -> u32 {
    (original | ENABLE_EXTENDED_FLAGS | ENABLE_VIRTUAL_TERMINAL_INPUT)
        & !(ENABLE_LINE_INPUT
            | ENABLE_ECHO_INPUT
            | ENABLE_PROCESSED_INPUT
            | ENABLE_QUICK_EDIT_MODE
            | ENABLE_MOUSE_INPUT
            | ENABLE_WINDOW_INPUT)
}
pub(super) fn size() -> Option<(u16, u16)> {
    if console::mode(io::stdin()).is_err()
        && console::mode(io::stdout()).is_err()
        && console::mode(io::stderr()).is_err()
    {
        return None;
    }
    // Reopen the active buffer: inherited stdout may still identify the primary
    // buffer after entering the VT alternate screen (or may be redirected).
    let output = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONOUT$")
        .ok()?;
    let rect = console::screen_buffer_info(&output).ok()?.window_rect();
    viewport(rect.left, rect.top, rect.right, rect.bottom)
}
fn viewport(left: i16, top: i16, right: i16, bottom: i16) -> Option<(u16, u16)> {
    let cols = u16::try_from(i32::from(right) - i32::from(left) + 1).ok()?;
    let rows = u16::try_from(i32::from(bottom) - i32::from(top) + 1).ok()?;
    (cols > 0 && rows > 0).then_some((cols.min(screen::MAX_COLS), rows.min(screen::MAX_LINES)))
}

/// Windows console handles cannot be polled together with `AF_UNIX` sockets.
/// Bounded workers wake the protocol loop, which retains its ACK deadline and resize timer.
/// The stdin worker stops at Ctrl-]/EOF; on peer closure the CLI process exits, ending
/// any outstanding console read. It never owns terminal modes or daemon resources.
pub(super) struct Events {
    rx: Receiver<io::Result<Event>>,
    socket: UnixStream,
    reader: Option<JoinHandle<()>>,
    stopped: Arc<AtomicBool>,
}
impl Events {
    pub(super) fn new(stream: &UnixStream) -> Result<Self> {
        let socket = stream.try_clone().map_err(fail)?;
        let reader = stream.try_clone().map_err(fail)?;
        reader
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(fail)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let mut reader = CancellableRead {
            socket: reader,
            stopped: stopped.clone(),
        };
        let (tx, rx) = mpsc::sync_channel(16);
        let input_tx = tx.clone();
        let reader = thread::Builder::new()
            .name("attach-output".into())
            .spawn(move || {
                loop {
                    let event = ipc::read_frame(&mut reader).and_then(|frame| {
                        frame
                            .map(|(kind, body)| Event::Frame(kind, body))
                            .ok_or_else(|| {
                                io::Error::new(io::ErrorKind::UnexpectedEof, "krilld disconnected")
                            })
                    });
                    let failed = event.is_err();
                    if tx.send(event).is_err() || failed {
                        break;
                    }
                }
            })
            .map_err(fail)?;
        let events = Self {
            rx,
            socket,
            reader: Some(reader),
            stopped,
        };
        thread::Builder::new()
            .name("attach-input".into())
            .spawn(move || {
                loop {
                    let mut bytes = vec![0; 4096];
                    match io::stdin().read(&mut bytes) {
                        Ok(n) => {
                            bytes.truncate(n);
                            let done = n == 0 || bytes.contains(&0x1d);
                            if input_tx.send(Ok(Event::Input(bytes))).is_err() || done {
                                break;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => {
                            input_tx.send(Err(e)).ok();
                            break;
                        }
                    }
                }
            })
            .map_err(fail)?;
        Ok(events)
    }
    pub(super) fn next(
        &mut self,
        _: &mut UnixStream,
        reading: bool,
        _: Option<Instant>,
    ) -> Result<Event> {
        match self.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(Event::Input(_))) if !reading => Ok(Event::Tick),
            Ok(event) => event.map_err(fail),
            Err(RecvTimeoutError::Timeout) => Ok(Event::Tick),
            Err(RecvTimeoutError::Disconnected) => Err(fail("attach readers disconnected")),
        }
    }
}
impl Drop for Events {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.socket.shutdown(Shutdown::Both).ok();
        // Drain before joining: a bounded sender may be waiting for queue capacity.
        while self
            .reader
            .as_ref()
            .is_some_and(|reader| !reader.is_finished())
        {
            self.rx.recv_timeout(Duration::from_millis(10)).ok();
        }
        if let Some(reader) = self.reader.take() {
            reader.join().ok();
        }
    }
}

// Retry timeout inside Read, so read_exact retains partial frame headers/bodies.
// Windows AF_UNIX shutdown alone does not reliably wake an outstanding recv.
struct CancellableRead {
    socket: UnixStream,
    stopped: Arc<AtomicBool>,
}
impl io::Read for CancellableRead {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.stopped.load(Ordering::Relaxed) {
                return Err(io::ErrorKind::ConnectionAborted.into());
            }
            match self.socket.read(bytes) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) => {}
                result => return result,
            }
        }
    }
}

// WriteConsoleW (used by Rust stdout) requires complete UTF-8, while OUTPUT frames
// may split a scalar. Keep at most the incomplete trailing scalar between frames.
pub(super) fn output() -> Utf8Output<io::Stdout> {
    Utf8Output {
        writer: io::stdout(),
        pending: Vec::new(),
    }
}
pub(super) struct Utf8Output<W> {
    writer: W,
    pending: Vec<u8>,
}
impl<W: io::Write> io::Write for Utf8Output<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0;
        let mut text = Vec::new();
        while consumed < self.pending.len() {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(_) => {
                    text.extend_from_slice(&self.pending[consumed..]);
                    consumed = self.pending.len();
                }
                Err(error) => {
                    let valid_end = consumed + error.valid_up_to();
                    text.extend_from_slice(&self.pending[consumed..valid_end]);
                    consumed = valid_end;
                    if let Some(invalid) = error.error_len() {
                        text.extend_from_slice("\u{fffd}".as_bytes());
                        consumed += invalid;
                    } else {
                        break;
                    }
                }
            }
        }
        self.writer.write_all(&text)?;
        self.pending.drain(..consumed);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visible_window_size_is_inclusive_and_clamped() {
        assert_eq!(viewport(12, 300, 121, 332), Some((110, 33)));
        assert_eq!(
            viewport(0, 0, i16::MAX, i16::MAX),
            Some((screen::MAX_COLS, screen::MAX_LINES))
        );
        assert_eq!(viewport(1, 0, 0, 4), None);
        assert_eq!(viewport(4, 0, 0, 4), None);
    }
    #[test]
    fn output_preserves_unicode_split_across_frames() {
        let mut output = Utf8Output {
            writer: Vec::new(),
            pending: Vec::new(),
        };
        let bytes = "한글 🦐\x1b[0m".as_bytes();
        for byte in bytes {
            output.write_all(&[*byte]).unwrap();
            output.flush().unwrap();
        }
        assert_eq!(output.writer, bytes);
        assert!(output.pending.is_empty());
    }
    #[test]
    fn vt_input_disables_local_processing_and_quick_edit() {
        let mode = input_mode(
            ENABLE_LINE_INPUT
                | ENABLE_ECHO_INPUT
                | ENABLE_PROCESSED_INPUT
                | ENABLE_QUICK_EDIT_MODE
                | ENABLE_WINDOW_INPUT
                | ENABLE_MOUSE_INPUT,
        );
        assert_eq!(mode, ENABLE_EXTENDED_FLAGS | ENABLE_VIRTUAL_TERMINAL_INPUT);
    }
}
