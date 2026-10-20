// Krill modification (Apache-2.0 §4(b)): serialize pasteboard server IO on one
// owning thread. A stalled owner/provider must never hold MacPlatform's UI lock.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use futures::channel::oneshot;
use gpui::{ClipboardItem, ClipboardReadError};
use parking_lot::Mutex;

use crate::pasteboard::Pasteboard;

#[derive(Clone, Copy)]
pub(crate) enum Board {
    General,
    Find,
}

enum Command {
    Read(oneshot::Sender<Option<ClipboardItem>>, u64),
    Write(Board, Arc<ClipboardItem>),
}

#[derive(Default)]
struct Snapshot {
    general: Option<Arc<ClipboardItem>>,
    find: Option<Arc<ClipboardItem>>,
    revision: u64,
}

#[derive(Clone)]
pub(crate) struct ClipboardWorker {
    send: mpsc::SyncSender<Command>,
    snapshot: Arc<Mutex<Snapshot>>,
    reading: Arc<AtomicBool>,
}

impl ClipboardWorker {
    fn channel() -> (Self, mpsc::Receiver<Command>) {
        let (send, recv) = mpsc::sync_channel(2);
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        (
            Self {
                send,
                snapshot,
                reading: Arc::new(AtomicBool::new(false)),
            },
            recv,
        )
    }

    pub(crate) fn new() -> Self {
        let (worker, recv) = Self::channel();
        let worker_snapshot = worker.snapshot.clone();
        let reading = worker.reading.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("pasteboard-io".into())
            .spawn(move || {
                // Native objects stay on this thread, with a fresh autorelease
                // pool for each command. Never move Objective-C objects via Send.
                let (general, find) =
                    objc::rc::autoreleasepool(|| (Pasteboard::general(), Pasteboard::find()));
                while let Ok(command) = recv.recv() {
                    objc::rc::autoreleasepool(|| match command {
                        Command::Read(done, revision) => {
                            if !done.is_canceled() {
                                let item = general.read();
                                store_read(&worker_snapshot, &item, revision);
                                let _ = done.send(item);
                            }
                            reading.store(false, Ordering::Release);
                        }
                        Command::Write(board, item) => match board {
                            Board::General => general.write((*item).clone()),
                            Board::Find => find.write((*item).clone()),
                        },
                    });
                }
            })
        {
            log::error!("could not start pasteboard worker: {error}");
        }
        worker
    }

    /// Legacy synchronous reads return the last completed local snapshot.
    /// Native reads must use read_async so external clipboard data stays fresh.
    pub(crate) fn cached(&self, board: Board) -> Option<ClipboardItem> {
        let item = {
            let cache = self.snapshot.lock();
            match board {
                Board::General => cache.general.clone(),
                Board::Find => cache.find.clone(),
            }
        };
        item.map(|item| (*item).clone())
    }

    pub(crate) fn read_async(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<ClipboardItem>, ClipboardReadError>>
    + Send
    + 'static {
        let (done, wait) = oneshot::channel();
        let reserved = self
            .reading
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        let queued = reserved
            && self
                .send
                .try_send(Command::Read(done, self.snapshot.lock().revision))
                .is_ok();
        if reserved && !queued {
            self.reading.store(false, Ordering::Release);
        }
        async move {
            if !queued {
                return Err(ClipboardReadError::Unavailable);
            }
            wait.await.map_err(|_| ClipboardReadError::Unavailable)
        }
    }

    pub(crate) fn write(&self, board: Board, item: ClipboardItem) {
        let item = Arc::new(item);
        let mut cache = self.snapshot.lock();
        let replaced = if self
            .send
            .try_send(Command::Write(board, item.clone()))
            .is_ok()
        {
            cache.revision = cache.revision.wrapping_add(1);
            match board {
                Board::General => cache.general.replace(item),
                Board::Find => cache.find.replace(item),
            }
        } else {
            log::warn!("pasteboard worker busy or unavailable; copy was not queued");
            None
        };
        drop(cache);
        drop(replaced);
    }
}

pub(crate) async fn with_deadline(
    read: impl std::future::Future<Output = Result<Option<ClipboardItem>, ClipboardReadError>>,
    timer: impl std::future::Future<Output = ()>,
) -> Result<Option<ClipboardItem>, ClipboardReadError> {
    match futures::future::select(Box::pin(read), Box::pin(timer)).await {
        futures::future::Either::Left((result, _)) => result,
        futures::future::Either::Right(_) => {
            log::warn!("pasteboard provider did not respond before the deadline");
            Err(ClipboardReadError::Unavailable)
        }
    }
}

fn store_read(snapshot: &Mutex<Snapshot>, item: &Option<ClipboardItem>, revision: u64) {
    // Copy large payloads before entering the short shared-state critical section.
    let item = item.clone().map(Arc::new);
    let mut cache = snapshot.lock();
    let replaced = if cache.revision == revision {
        std::mem::replace(&mut cache.general, item)
    } else {
        None
    };
    drop(cache);
    drop(replaced);
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt as _;

    #[test]
    fn pending_native_read_does_not_block_copy_and_keeps_command_order() {
        // A held reply models a pasteboard provider that is not responding.
        // No test touches the user's actual clipboard.
        let (worker, commands) = ClipboardWorker::channel();
        let first = ClipboardItem::new_string("first".into());
        worker.write(Board::General, first.clone());
        let mut read = Box::pin(worker.read_async());
        assert!(read.as_mut().now_or_never().is_none());
        assert!(matches!(
            commands.recv().unwrap(),
            Command::Write(Board::General, _)
        ));
        let Command::Read(done, revision) = commands.recv().unwrap() else {
            panic!("read must follow copy")
        };
        let latest = ClipboardItem::new_string("latest".into());
        worker.write(Board::General, latest.clone());
        assert_eq!(worker.cached(Board::General), Some(latest.clone()));
        store_read(&worker.snapshot, &Some(first.clone()), revision);
        done.send(Some(first.clone())).unwrap();
        assert_eq!(futures::executor::block_on(read).unwrap(), Some(first));
        assert_eq!(worker.cached(Board::General), Some(latest));
    }

    #[test]
    fn unavailable_worker_reports_failure_without_waiting() {
        let (worker, commands) = ClipboardWorker::channel();
        drop(commands);
        assert_eq!(
            futures::executor::block_on(worker.read_async()),
            Err(ClipboardReadError::Unavailable)
        );
    }

    #[test]
    fn stalled_provider_has_bounded_admission_and_deadline() {
        let (worker, _commands) = ClipboardWorker::channel();
        let first = worker.read_async();
        assert_eq!(
            futures::executor::block_on(worker.read_async()),
            Err(ClipboardReadError::Unavailable)
        );
        assert_eq!(
            futures::executor::block_on(with_deadline(first, futures::future::ready(()))),
            Err(ClipboardReadError::Unavailable)
        );
        // The native provider is still held, so repeated paste cannot grow the queue.
        assert_eq!(
            futures::executor::block_on(worker.read_async()),
            Err(ClipboardReadError::Unavailable)
        );
    }

    #[test]
    fn full_queue_does_not_claim_an_unaccepted_copy() {
        let (worker, _commands) = ClipboardWorker::channel();
        worker.write(Board::General, ClipboardItem::new_string("one".into()));
        worker.write(Board::General, ClipboardItem::new_string("two".into()));
        worker.write(
            Board::General,
            ClipboardItem::new_string("not queued".into()),
        );
        assert_eq!(
            worker.cached(Board::General).unwrap().text().as_deref(),
            Some("two")
        );
    }
}
