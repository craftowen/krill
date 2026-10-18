//! Blocking `ConPTY` reads feed the common engine loop through one bounded slot.
//! The engine can wait on DEC 2026 deadlines without polling a Windows pipe.
use std::fs::File;
use std::io::{self, Read as _};
use std::os::windows::io::{AsHandle as _, OwnedHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::{BATCH, HALTED, Shared, lock};
use crate::holder::Master;
use krill_proto::holder::{FromHolder, ToHolder, WinSize};

static READERS: Mutex<Vec<Weak<Stop>>> = Mutex::new(Vec::new());

struct Stop {
    stopped: AtomicBool,
    thread: OwnedHandle,
}

impl Stop {
    fn cancel(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        crate::sys::cancel_pipe_read(self.thread.as_handle());
    }
}

pub(super) struct Reader {
    rx: Option<mpsc::Receiver<io::Result<Vec<u8>>>>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<Stop>,
}

impl Reader {
    pub(super) fn start(master: &Master) -> io::Result<Self> {
        let mut pipe = File::from(master.output.try_clone()?);
        let (tx, rx) = mpsc::sync_channel(1);
        // Gate startup until cancellation is registered, including concurrent halt().
        let (ready, gate) = mpsc::sync_channel::<Arc<Stop>>(0);
        let worker = thread::Builder::new()
            .name("conpty-read".to_owned())
            .spawn(move || {
                let Ok(stop) = gate.recv() else { return };
                while !stop.stopped.load(Ordering::SeqCst) {
                    let mut bytes = vec![0; BATCH];
                    let result = match pipe.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(n) => {
                            bytes.truncate(n);
                            Ok(bytes)
                        }
                        Err(e) if matches!(e.raw_os_error(), Some(109 | 232 | 995)) => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => Err(e),
                    };
                    let failed = result.is_err();
                    if tx.send(result).is_err() || failed {
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
            worker: Some(worker),
            stop,
        })
    }

    pub(super) fn read(
        &self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<Option<usize>> {
        let rx = self.rx.as_ref().ok_or(io::ErrorKind::BrokenPipe)?;
        let bytes = match timeout {
            Some(t) => match rx.recv_timeout(t) {
                Ok(r) => r?,
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(Some(0)),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            },
            None => match rx.recv() {
                Ok(r) => r?,
                Err(_) => return Ok(None),
            },
        };
        let dest = buf
            .get_mut(..bytes.len())
            .ok_or(io::ErrorKind::InvalidData)?;
        dest.copy_from_slice(&bytes);
        Ok(Some(bytes.len()))
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

pub(super) fn halt() {
    let readers: Vec<_> = lock(&READERS).iter().filter_map(Weak::upgrade).collect();
    for reader in &readers {
        reader.cancel();
    }
    // Cancellation may precede ReadFile. Retry until every read worker has exited;
    // queues are drained by their engine loops, which observe HALTED at boundaries.
    while readers
        .iter()
        .any(|r| !crate::sys::thread_finished(r.thread.as_handle()))
    {
        for reader in &readers {
            reader.cancel();
        }
        thread::sleep(Duration::from_millis(1));
    }
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
