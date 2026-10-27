//! Blocking `ConPTY` reads feed the engine through a bounded queue of reusable buffers.
//! The engine can wait on DEC 2026 deadlines without polling a Windows pipe.
//!
//! Blocking `ConPTY` writes run on one writer thread per session (daemon-02): the engine lock only queues
//! query replies and never waits on a full input pipe, and a `WriteFile` blocked by backpressure is
//! cancelled with `CancelSynchronousIo` so a disconnected client or a closing session cannot pin the
//! input lock. Both workers use the same [`Stop`] handle as the reader cancellation.
use std::cell::Cell;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::windows::io::{AsHandle as _, OwnedHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{BATCH, HALTED, Shared, lock};
use crate::holder::Master;
use krill_proto::holder::{FromHolder, ToHolder, WinSize};

static READERS: Mutex<Vec<Weak<Stop>>> = Mutex::new(Vec::new());

/// A pipe worker thread and its stop flag. Cancellation aborts the synchronous IO it is blocked in.
struct Stop {
    stopped: AtomicBool,
    thread: OwnedHandle,
}

impl Stop {
    fn cancel(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.cancel_io();
    }

    /// Abort the worker's in-flight `ReadFile`/`WriteFile` without stopping the worker.
    fn cancel_io(&self) {
        crate::sys::cancel_sync_io(self.thread.as_handle());
    }

    fn finished(&self) -> bool {
        crate::sys::thread_finished(self.thread.as_handle())
    }
}

type ReadMessage = io::Result<(Vec<u8>, usize)>;
// Bound read-ahead while allowing the pipe worker and engine to run independently.
const QUEUED_READS: usize = 8;

pub(super) struct Reader {
    rx: Option<mpsc::Receiver<ReadMessage>>,
    recycle: mpsc::SyncSender<Vec<u8>>,
    pending: Cell<Option<ReadMessage>>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<Stop>,
}

impl Reader {
    pub(super) fn start(master: &Master) -> io::Result<Self> {
        let mut pipe = File::from(master.output.try_clone()?);
        let (tx, rx) = mpsc::sync_channel(QUEUED_READS);
        let (recycle, recycled) = mpsc::sync_channel::<Vec<u8>>(QUEUED_READS + 2);
        // Gate startup until cancellation is registered, including concurrent halt().
        let (ready, gate) = mpsc::sync_channel::<Arc<Stop>>(0);
        let worker = thread::Builder::new()
            .name("conpty-read".to_owned())
            .spawn(move || {
                let Ok(stop) = gate.recv() else { return };
                while !stop.stopped.load(Ordering::SeqCst) {
                    let mut bytes = recycled.try_recv().unwrap_or_else(|_| vec![0; BATCH]);
                    let result = match pipe.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(n) => Ok((bytes, n)),
                        Err(e) if matches!(e.raw_os_error(), Some(109 | 232 | 995)) => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => Err(e),
                    };
                    // A read already taken from the pipe is never dropped: it waits for the engine loop
                    // (handover is lossless). `halt` bounds its own wait with a deadline instead.
                    if tx.send(result).is_err() {
                        break;
                    }
                }
            })?;
        let stop = Arc::new(Stop {
            stopped: AtomicBool::new(false),
            thread: worker.as_handle().try_clone_to_owned()?,
        });
        {
            let mut readers = lock(&READERS);
            readers.retain(|r| r.strong_count() != 0);
            readers.push(Arc::downgrade(&stop));
            if HALTED.load(Ordering::SeqCst) {
                stop.cancel();
            }
        }
        ready
            .send(Arc::clone(&stop))
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        Ok(Self {
            rx: Some(rx),
            recycle,
            pending: Cell::new(None),
            worker: Some(worker),
            stop,
        })
    }

    /// The worker was cancelled ([`halt`]) rather than reaching EOF.
    pub(super) fn halted(&self) -> bool {
        self.stop.stopped.load(Ordering::SeqCst)
    }

    pub(super) fn read(
        &self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<Option<usize>> {
        let rx = self.rx.as_ref().ok_or(io::ErrorKind::BrokenPipe)?;
        let mut message = match self.pending.take() {
            Some(message) => message,
            None => match timeout {
                Some(t) => match rx.recv_timeout(t) {
                    Ok(r) => r,
                    Err(mpsc::RecvTimeoutError::Timeout) => return Ok(Some(0)),
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
                },
                None => match rx.recv() {
                    Ok(r) => r,
                    Err(_) => return Ok(None),
                },
            },
        };
        let mut used = 0;
        loop {
            match message {
                Ok((bytes, n)) => {
                    let Some(dest) = buf.get_mut(used..).and_then(|rest| rest.get_mut(..n)) else {
                        if used == 0 {
                            return Err(io::ErrorKind::InvalidData.into());
                        }
                        self.pending.set(Some(Ok((bytes, n))));
                        return Ok(Some(used));
                    };
                    dest.copy_from_slice(bytes.get(..n).ok_or(io::ErrorKind::InvalidData)?);
                    self.recycle.try_send(bytes).ok();
                    used += n;
                }
                Err(e) if used == 0 => return Err(e),
                Err(e) => {
                    self.pending.set(Some(Err(e)));
                    return Ok(Some(used));
                }
            }
            if used == buf.len() {
                return Ok(Some(used));
            }
            // Drain only already queued reads: no timer or sleep on interactive output.
            message = match rx.try_recv() {
                Ok(r) => r,
                Err(_) => return Ok(Some(used)),
            };
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // Disconnect first so a worker blocked on the bounded queue can exit too.
        self.rx.take();
        if let Some(worker) = self.worker.take() {
            while !worker.is_finished() {
                // Repeat to close the check-stopped / enter-ReadFile race.
                self.stop.cancel();
                thread::sleep(Duration::from_millis(1));
            }
            worker.join().ok();
        }
    }
}

/// Cancel every read worker and wait for all of them to exit. Cancellation may precede `ReadFile`, so it
/// repeats until each worker has exited. A worker whose engine loop stalled keeps its last read queued
/// (never dropped) and `TimedOut` returns at `deadline`; the caller proceeds as crash scope (daemon-02).
pub(super) fn halt(deadline: Instant) -> io::Result<()> {
    let readers: Vec<_> = lock(&READERS).iter().filter_map(Weak::upgrade).collect();
    loop {
        for reader in &readers {
            reader.cancel();
        }
        if readers.iter().all(|r| r.finished()) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// One `WriteFile` never exceeds this. Win32 does not report the bytes of an aborted write
/// (`lpNumberOfBytesWritten` is undefined after `ERROR_OPERATION_ABORTED`), so a cancelled chunk bounds
/// the prefix the input ledger may miss.
const WRITE_CHUNK: usize = 1024;
/// Queued query replies before further replies are dropped (the Unix EAGAIN policy).
const QUEUED_WRITES: usize = 32;
/// How often a blocked input write re-checks session closure and peer HUP.
const WRITE_WAIT: Duration = Duration::from_millis(20);
const OPERATION_ABORTED: i32 = 995;

enum Job {
    /// Query reply: fire and forget.
    Reply(Vec<u8>),
    /// User input: the submitter waits for the bytes actually written and may abort the rest.
    Input {
        bytes: Vec<u8>,
        abort: Arc<AtomicBool>,
        done: mpsc::SyncSender<(usize, io::Result<()>)>,
    },
}

/// Per-session PTY input writer (daemon-02). Owns a duplicate of the input handle; the holder's copy is
/// never closed.
pub(super) struct Writer {
    tx: Option<mpsc::SyncSender<Job>>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<Stop>,
}

impl Writer {
    pub(super) fn start(master: &Master) -> io::Result<Self> {
        let mut pipe = File::from(master.input.try_clone()?);
        let (tx, rx) = mpsc::sync_channel::<Job>(QUEUED_WRITES);
        let (ready, gate) = mpsc::sync_channel::<Arc<Stop>>(0);
        let worker = thread::Builder::new()
            .name("conpty-write".to_owned())
            .spawn(move || {
                let Ok(stop) = gate.recv() else { return };
                for job in rx {
                    if stop.stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    match job {
                        Job::Reply(bytes) => {
                            // Query replies are best effort (Unix drops them on EAGAIN too).
                            write_chunks(&mut pipe, &bytes, &stop, None).1.ok();
                        }
                        Job::Input { bytes, abort, done } => {
                            done.send(write_chunks(&mut pipe, &bytes, &stop, Some(&abort)))
                                .ok();
                        }
                    }
                }
            })?;
        let stop = Arc::new(Stop {
            stopped: AtomicBool::new(false),
            thread: worker.as_handle().try_clone_to_owned()?,
        });
        ready
            .send(Arc::clone(&stop))
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        Ok(Self {
            tx: Some(tx),
            worker: Some(worker),
            stop,
        })
    }

    /// Queue a query reply from inside the engine lock: never waits on the pipe. A full queue drops the
    /// reply like the Unix EAGAIN policy.
    pub(super) fn reply(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(tx) = &self.tx {
            tx.try_send(Job::Reply(bytes.to_vec())).ok();
        }
    }

    /// Write user input. `alive` runs every [`WRITE_WAIT`] while the pipe does not drain; once it errs,
    /// the in-flight `WriteFile` is cancelled and that error returns with the prefix confirmed written.
    /// Bytes a cancelled chunk may still have delivered are not counted (at most [`WRITE_CHUNK`]).
    pub(super) fn write(
        &self,
        bytes: &[u8],
        mut alive: impl FnMut() -> io::Result<()>,
    ) -> (usize, io::Result<()>) {
        let Some(tx) = &self.tx else {
            return (0, Err(io::ErrorKind::BrokenPipe.into()));
        };
        let (done, finished) = mpsc::sync_channel(1);
        let abort = Arc::new(AtomicBool::new(false));
        let mut job = Job::Input {
            bytes: bytes.to_vec(),
            abort: Arc::clone(&abort),
            done,
        };
        // Queue behind pending replies without waiting forever on a stalled pipe.
        loop {
            match tx.try_send(job) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(pending)) => {
                    if let Err(e) = alive() {
                        return (0, Err(e));
                    }
                    job = pending;
                    thread::sleep(WRITE_WAIT);
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return (0, Err(io::ErrorKind::BrokenPipe.into()));
                }
            }
        }
        loop {
            match finished.recv_timeout(WRITE_WAIT) {
                Ok(outcome) => return outcome,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let Err(e) = alive() else { continue };
                    abort.store(true, Ordering::SeqCst);
                    // Cancellation may precede WriteFile: repeat until the worker reports this job.
                    loop {
                        self.stop.cancel_io();
                        match finished.recv_timeout(Duration::from_millis(1)) {
                            Ok((written, _)) => return (written, Err(e)),
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            Err(mpsc::RecvTimeoutError::Disconnected) => return (0, Err(e)),
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return (0, Err(io::ErrorKind::BrokenPipe.into()));
                }
            }
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            while !worker.is_finished() {
                // Abort a WriteFile blocked on a full pipe; repeat to close the check-stopped / enter race.
                self.stop.cancel();
                thread::sleep(Duration::from_millis(1));
            }
            worker.join().ok();
        }
    }
}

/// Write in [`WRITE_CHUNK`] pieces; stop before a piece once the writer stops or the submitter aborts.
/// Returns the bytes confirmed by completed `WriteFile` calls and the error that ended the write.
fn write_chunks(
    pipe: &mut File,
    bytes: &[u8],
    stop: &Stop,
    abort: Option<&AtomicBool>,
) -> (usize, io::Result<()>) {
    let mut written = 0;
    for chunk in bytes.chunks(WRITE_CHUNK) {
        let mut rest = chunk;
        while !rest.is_empty() {
            if stop.stopped.load(Ordering::SeqCst)
                || abort.is_some_and(|abort| abort.load(Ordering::SeqCst))
            {
                return (written, Err(io::ErrorKind::Interrupted.into()));
            }
            match pipe.write(rest) {
                Ok(0) => return (written, Err(io::ErrorKind::WriteZero.into())),
                Ok(n) => {
                    written = written.saturating_add(n);
                    rest = rest.get(n..).unwrap_or_default();
                }
                Err(e)
                    if e.kind() == io::ErrorKind::Interrupted
                        && e.raw_os_error() != Some(OPERATION_ABORTED) => {}
                Err(e) => return (written, Err(e)),
            }
        }
    }
    (written, Ok(()))
}

pub(super) fn resize(shared: &Shared, size: WinSize) -> io::Result<()> {
    let holder = shared.holder.as_ref().ok_or(io::ErrorKind::NotConnected)?;
    if holder.version() < 4 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    match holder.request(|req| ToHolder::Resize {
        req,
        id: shared.id,
        size,
    })? {
        (FromHolder::Done { .. }, _) => Ok(()),
        (FromHolder::Failed { errno, .. }, _) => Err(io::Error::from_raw_os_error(errno)),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{end_halt, halt};
    use super::*;
    use std::os::windows::io::AsSocket as _;

    fn pipes() -> (Master, std::io::PipeReader, std::io::PipeWriter) {
        let (input, to_shell) = io::pipe().unwrap();
        let (from_shell, output) = io::pipe().unwrap();
        let signal = to_shell.as_handle().try_clone_to_owned().unwrap();
        let master = Master {
            input: to_shell.into(),
            output: from_shell.into(),
            signal,
            reference: None,
            process: None,
            recovery: Vec::new(),
        };
        (master, input, output)
    }

    fn session(id: u64, master: Master) -> crate::Session {
        crate::Session::start(
            krill_proto::SessionId(id),
            master,
            None,
            WinSize { cols: 80, rows: 25 },
            Arc::new(crate::appearance::Appearance::load(
                std::path::PathBuf::new(),
            )),
            None,
        )
        .unwrap()
    }

    /// A connected TCP pair stands in for the client socket whose HUP cancels input.
    fn loopback_pair() -> (std::net::TcpStream, std::net::TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    /// Reads `input` until it ends with `until` or `deadline` passes, never blocking inside `ReadFile`.
    fn drain_until(input: &mut std::io::PipeReader, until: &[u8], deadline: Instant) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = [0u8; 8192];
        while !got.ends_with(until) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {:?} after {} bytes",
                String::from_utf8_lossy(until),
                got.len()
            );
            match crate::sys::pipe_available(input.as_handle()).unwrap() {
                Some(0) => thread::sleep(Duration::from_millis(5)),
                Some(_) => {
                    let n = input.read(&mut buf).unwrap();
                    got.extend_from_slice(&buf[..n]);
                }
                None => break,
            }
        }
        got
    }

    #[test]
    fn idle_reader_drop_cancels_and_joins_without_closing_holder_pipe() {
        let (master, _input, mut output) = pipes();
        // The holder retains its own output read handle throughout cancellation.
        let reader = Reader::start(&master).unwrap();
        drop(reader);
        output.write_all(b"after cancellation").unwrap();
        let reader = Reader::start(&master).unwrap();
        let mut buf = [0; 64];
        let n = reader
            .read(&mut buf, Some(Duration::from_secs(2)))
            .unwrap()
            .unwrap();
        assert_eq!(buf.get(..n), Some(b"after cancellation".as_slice()));
    }

    #[test]
    fn queued_reads_preserve_bytes_through_eof() {
        let (master, _input, mut output) = pipes();
        let reader = Reader::start(&master).unwrap();
        let expected: Vec<_> = (0u8..=255)
            .cycle()
            .take(BATCH * (QUEUED_READS + 3) + 37)
            .collect();
        let bytes = expected.clone();
        let writer = thread::spawn(move || {
            for chunk in bytes.chunks(997) {
                output.write_all(chunk).unwrap();
            }
        });
        let mut actual = Vec::new();
        let mut buf = vec![0; BATCH];
        while let Some(n) = reader.read(&mut buf, Some(Duration::from_secs(5))).unwrap() {
            assert_ne!(n, 0, "reader stalled before EOF");
            actual.extend_from_slice(buf.get(..n).unwrap());
        }
        writer.join().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn da1_input_and_eof_use_the_common_engine() {
        let (master, mut input, mut output) = pipes();
        let session = session(1, master);
        output.write_all(b"\x1b[c").unwrap();
        let mut reply = [0; 2];
        input.read_exact(&mut reply).unwrap();
        assert_eq!(reply, *b"\x1b[");
        session.tap().write_input(b"user input", None).unwrap();
        drop(output);
        session.wait().unwrap();
    }

    /// daemon-02: an input write blocked by a full pipe observes the client's HUP, cancels the blocked
    /// `WriteFile`, supplies the paste end marker for the prefix that really reached the pipe and does
    /// not replay the abandoned body.
    #[test]
    fn blocked_input_write_is_cancelled_by_peer_hup_and_reports_written_prefix() {
        let (master, mut input, _output) = pipes();
        let session = session(2, master);
        let tap = session.tap();
        let (mut hup, peer) = loopback_pair();
        let mut peer_reader = peer.try_clone().unwrap();
        let mut body = b"\x1b[200~".to_vec();
        body.extend(vec![b'x'; 1 << 20]);
        body.extend_from_slice(b"\x1b[201~");
        let (done, result) = mpsc::channel();
        let worker = thread::spawn({
            let tap = tap.clone();
            let body = body.clone();
            move || {
                done.send(tap.write_input(&body, Some(peer.as_socket())))
                    .unwrap();
            }
        });
        // Nobody reads `input`: the write blocks once the pipe buffer is full.
        thread::sleep(Duration::from_millis(300));
        assert!(
            matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "write must block on a full pipe"
        );
        // Frames the client queues while connected are not a disconnect, and the probe leaves them unread.
        hup.write_all(b"queued frame").unwrap();
        thread::sleep(Duration::from_millis(300));
        assert!(
            matches!(result.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "queued client frames must not cancel the write"
        );
        let mut queued = [0u8; 12];
        peer_reader.read_exact(&mut queued).unwrap();
        assert_eq!(&queued, b"queued frame");
        drop(hup);
        let got = drain_until(
            &mut input,
            b"\x1b[201~",
            Instant::now() + Duration::from_secs(10),
        );
        let err = result
            .recv_timeout(Duration::from_secs(5))
            .expect("HUP must cancel the blocked write")
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionAborted);
        worker.join().unwrap();
        assert!(got.starts_with(b"\x1b[200~"));
        assert!(
            got.len() < body.len(),
            "abandoned paste body must be discarded"
        );
        // The writer is free again and nothing of the abandoned body is replayed.
        tap.write_input(b"after-cancel", None).unwrap();
        let tail = drain_until(
            &mut input,
            b"after-cancel",
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(tail, b"after-cancel");
        drop(session);
    }

    /// daemon-02: query replies are queued, never written under the engine lock, so a full input pipe
    /// cannot stall the reader, capture or resize.
    #[test]
    fn query_replies_never_block_the_engine_on_a_full_input_pipe() {
        let (master, mut input, mut output) = pipes();
        let session = session(3, master);
        let tap = session.tap();
        // Nobody reads `input`: replies overflow the pipe buffer; the engine must keep feeding.
        let queries = 4096_usize;
        for _ in 0..queries {
            output.write_all(b"\x1b[c").unwrap();
        }
        let fed = (3 * queries) as u64;
        let deadline = Instant::now() + Duration::from_secs(10);
        while tap.pulse().end < fed {
            assert!(
                Instant::now() < deadline,
                "engine stalled behind a full input pipe at {}",
                tap.pulse().end
            );
            thread::sleep(Duration::from_millis(5));
        }
        let started = Instant::now();
        let _ = tap.capture();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "live lock pinned"
        );
        // The replies that fit were written; the overflow was dropped rather than queued forever.
        let mut got = Vec::new();
        let mut buf = [0u8; 8192];
        while crate::sys::pipe_available(input.as_handle())
            .unwrap()
            .is_some_and(|n| n > 0)
        {
            let n = input.read(&mut buf).unwrap();
            got.extend_from_slice(&buf[..n]);
        }
        assert!(got.starts_with(b"\x1b["), "{:?}", got.get(..8));
        drop(output);
        session.wait().unwrap();
        drop(tap);
    }

    /// daemon-02 + daemon-04: `halt` returns `TimedOut` at its deadline while the engine loop is stalled and
    /// the read worker is blocked on the bounded queue (its reads are never dropped); once the engine
    /// drains, halt completes, the reader ends without reporting the session closed, and `end_halt`
    /// starts one replacement reader on the same engine that keeps reading until EOF.
    #[test]
    #[ignore = "global halt(): run through halt_deadline_and_resume_run_in_an_isolated_process"]
    fn isolated_halt_deadline_and_resume() {
        let (master, _input, mut output) = pipes();
        let session = session(4, master);
        let tap = session.tap();
        let payload = vec![b'y'; BATCH * (QUEUED_READS + 3)];
        let len = payload.len() as u64;
        // Stall the engine loop: the worker fills its queue and blocks in `send`.
        let stall = session.shared.live();
        let feeder = thread::spawn(move || {
            output.write_all(&payload).unwrap();
            output.write_all(b"after-resume").unwrap();
            output
        });
        thread::sleep(Duration::from_millis(500));
        let started = Instant::now();
        assert_eq!(
            halt(Instant::now() + Duration::from_millis(300))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "halt must honour its deadline"
        );
        assert!(!session.is_closed(), "stalled reader must not be abandoned");
        drop(stall);
        halt(Instant::now() + Duration::from_secs(10)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !session.is_closed() {
            assert!(Instant::now() < deadline, "reader did not halt after drain");
            thread::sleep(Duration::from_millis(5));
        }
        let pulse = tap.pulse();
        assert!(
            pulse.end >= (QUEUED_READS * BATCH) as u64,
            "queued reads must reach the engine, not be dropped: {}",
            pulse.end
        );
        assert!(
            !pulse.closed,
            "a halted reader must not report the session closed"
        );
        assert_eq!(end_halt(), 0);
        let deadline = Instant::now() + Duration::from_secs(10);
        while tap.pulse().end < len + 12 {
            assert!(
                Instant::now() < deadline,
                "resumed reader stalled at {}",
                tap.pulse().end
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(tap.pulse().end, len + 12, "bytes must not be duplicated");
        assert!(!tap.pulse().closed);
        let output = feeder.join().unwrap();
        drop(output);
        session.wait().unwrap();
        assert!(
            tap.pulse().closed,
            "EOF after resume must close the session"
        );
    }

    #[test]
    fn halt_deadline_and_resume_run_in_an_isolated_process() {
        run_isolated("session::windows::tests::isolated_halt_deadline_and_resume");
    }

    /// Runs one `#[ignore]` test of this binary in a child process (process-global `halt()` must not touch
    /// the other tests' sessions) with a bounded wait.
    fn run_isolated(name: &str) {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                name,
                "--exact",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let out = thread::spawn(move || {
            let mut s = String::new();
            stdout.read_to_string(&mut s).ok();
            s
        });
        let err = thread::spawn(move || {
            let mut s = String::new();
            stderr.read_to_string(&mut s).ok();
            s
        });
        let deadline = Instant::now() + Duration::from_secs(120);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().ok();
                child.wait().ok();
                panic!("{name} did not finish within 120s");
            }
            thread::sleep(Duration::from_millis(50));
        };
        let (out, err) = (out.join().unwrap(), err.join().unwrap());
        assert!(
            status.success(),
            "{name} failed: {status}\nstdout:\n{out}\nstderr:\n{err}"
        );
    }
}
