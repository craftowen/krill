//! Bounded private-pipe transport. No desktop APIs, sockets, worker restarts, or replay.

use std::io::{self, BufRead, BufReader, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

pub(super) const MAX_FRAME: usize = 1024 * 1024;
pub(super) const QUEUE: usize = 8;
pub(super) const OUTPUT_QUEUE: usize = 32;
pub(super) const STALL: Duration = Duration::from_secs(3);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(super) enum Event {
    Client(Value),
    Native(Value),
    ClientEnd,
    NativeEnd,
    InvalidClient,
    InvalidNative,
    Written,
    OutputFailed,
    NativeWriteFailed,
}

pub(super) struct Transport {
    child: Child,
    pub events: Receiver<Event>,
    pub native: SyncSender<Vec<u8>>,
    pub output: SyncSender<Vec<u8>>,
}

/// A terminated worker cannot issue more input. A posted OS input may still take effect.
impl Drop for Transport {
    fn drop(&mut self) {
        self.terminate();
    }
}

impl Transport {
    pub fn terminate(&mut self) {
        terminate_child(&mut self.child);
    }
}

fn terminate_child(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(_) => {
            // No positive ownership witness remains; never signal a potentially reused pid.
            eprintln!("krill computer serve: worker status unavailable; termination unconfirmed");
            return;
        }
    }
    child.kill().ok();
    let until = Instant::now() + Duration::from_secs(1);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(5)),
            _ => {
                eprintln!("krill computer serve: worker reap could not be confirmed within bound");
                break;
            }
        }
    }
}

pub(super) fn worker_path(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        if !path.is_absolute() || !path.is_file() {
            return Err("--worker must name an existing absolute krill-app executable".into());
        }
        return Ok(path.to_path_buf());
    }
    let exe = std::env::current_exe().map_err(|_| "cannot locate this executable")?;
    let dir = exe.parent().ok_or("cannot locate executable directory")?;
    // Packaged binaries share Contents/MacOS; development binaries share the target directory.
    // Also permit the app bundled beside an installed CLI. Never search PATH or launch services.
    for path in [
        dir.join("krill-app"),
        dir.join("Krill.app/Contents/MacOS/krill-app"),
        dir.join("../MacOS/krill-app"),
    ] {
        if path.is_file() {
            return Ok(path);
        }
    }
    Err("compatible krill-app worker not found; pass --worker <absolute krill-app path>".into())
}

impl Transport {
    pub fn spawn(path: &Path) -> Result<(Self, Value), String> {
        let mut child = Command::new(path)
            .arg("--computer-worker")
            .arg(format!("--computer-protect-pid={}", std::process::id()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|_| "could not start computer worker")?;
        let Some(input) = child.stdin.take() else {
            terminate_child(&mut child);
            return Err("worker has no private input pipe".into());
        };
        let Some(output) = child.stdout.take() else {
            terminate_child(&mut child);
            return Err("worker has no private output pipe".into());
        };
        let (events_tx, events) = mpsc::sync_channel(QUEUE);
        reader(BufReader::new(output), events_tx.clone(), false);
        let (native, native_rx) = mpsc::sync_channel(QUEUE);
        writer(input, native_rx, events_tx.clone(), false);
        // Enough slots for every pending request's final uncertainty response plus normal traffic.
        let (output_tx, output_rx) = mpsc::sync_channel(OUTPUT_QUEUE);
        let transport = Self {
            child,
            events,
            native,
            output: output_tx,
        };
        let Ok(Event::Native(hello)) = transport.events.recv_timeout(STARTUP_TIMEOUT) else {
            return Err("worker did not send a valid hello before deadline".into());
        };
        // Start client I/O only after the native handshake. The output writer is the sole stdout owner.
        // File writes bypass Stdout's process-wide lock and buffering on the supported Unix path.
        #[cfg(unix)]
        let stdout = {
            use std::os::fd::AsFd as _;
            std::fs::File::from(
                io::stdout()
                    .as_fd()
                    .try_clone_to_owned()
                    .map_err(|_| "cannot open MCP stdout")?,
            )
        };
        #[cfg(not(unix))]
        let stdout = io::stdout();
        writer(stdout, output_rx, events_tx.clone(), true);
        reader(BufReader::new(io::stdin()), events_tx, true);
        Ok((transport, hello))
    }
}

fn reader(mut input: impl BufRead + Send + 'static, events: SyncSender<Event>, client: bool) {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        loop {
            let event = match read_line(&mut input, &mut bytes) {
                Ok(false) => {
                    if client {
                        Event::ClientEnd
                    } else {
                        Event::NativeEnd
                    }
                }
                Ok(true) => match serde_json::from_slice(&bytes) {
                    Ok(value) => {
                        if client {
                            Event::Client(value)
                        } else {
                            Event::Native(value)
                        }
                    }
                    Err(_) => {
                        if client {
                            Event::InvalidClient
                        } else {
                            Event::InvalidNative
                        }
                    }
                },
                Err(_) => {
                    if client {
                        Event::InvalidClient
                    } else {
                        Event::InvalidNative
                    }
                }
            };
            let end = !matches!(event, Event::Client(_) | Event::Native(_));
            if events.send(event).is_err() || end {
                break;
            }
        }
    });
}

fn writer(
    mut output: impl Write + Send + 'static,
    input: Receiver<Vec<u8>>,
    events: SyncSender<Event>,
    client: bool,
) {
    thread::spawn(move || {
        while let Ok(bytes) = input.recv() {
            if output
                .write_all(&bytes)
                .and_then(|()| output.flush())
                .is_err()
            {
                events
                    .send(if client {
                        Event::OutputFailed
                    } else {
                        Event::NativeWriteFailed
                    })
                    .ok();
                return;
            }
            if client && events.send(Event::Written).is_err() {
                return;
            }
        }
    });
}

pub(super) fn read_line(reader: &mut impl BufRead, bytes: &mut Vec<u8>) -> io::Result<bool> {
    bytes.clear();
    let count = reader
        .by_ref()
        .take((MAX_FRAME + 1) as u64)
        .read_until(b'\n', bytes)?;
    if count == 0 {
        return Ok(false);
    }
    if bytes.len() > MAX_FRAME && bytes.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds limit",
        ));
    }
    if bytes.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "incomplete newline-delimited frame",
        ));
    }
    bytes.pop();
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(true)
}

pub(super) fn encode(value: &Value) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| "JSON encoding failed")?;
    if bytes.len() > MAX_FRAME {
        return Err("encoded frame exceeds limit".into());
    }
    bytes.push(b'\n');
    Ok(bytes)
}
