// Krill modification (Apache-2.0 §4(b)): delayed clipboard rendering is cross-
// process IO. Read/write on one owner thread, without blocking the event thread.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, mpsc};

use futures::channel::oneshot;
use gpui::{ClipboardItem, ClipboardReadError};
use parking_lot::Mutex;

enum Command {
    Read(oneshot::Sender<Option<ClipboardItem>>, u64),
    Write(Arc<ClipboardItem>),
}

#[derive(Default)]
struct Snapshot {
    item: Option<Arc<ClipboardItem>>,
    revision: u64,
}

pub(crate) struct ClipboardWorker {
    send: mpsc::SyncSender<Command>,
    snapshot: Arc<Mutex<Snapshot>>,
    reading: Arc<AtomicBool>,
}

pub(crate) static CLIPBOARD: LazyLock<ClipboardWorker> = LazyLock::new(|| {
    let (send, recv) = mpsc::sync_channel(2);
    let snapshot = Arc::new(Mutex::new(Snapshot::default()));
    let cache = snapshot.clone();
    let reading = Arc::new(AtomicBool::new(false));
    let worker_reading = reading.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("clipboard-io".into())
        .spawn(move || {
            while let Ok(command) = recv.recv() {
                match command {
                    Command::Read(done, revision) => {
                        if !done.is_canceled() {
                            let item = crate::clipboard::read_from_clipboard();
                            let snapshot_item = item.clone().map(Arc::new);
                            let replaced = {
                                let mut cache = cache.lock();
                                if cache.revision == revision {
                                    std::mem::replace(&mut cache.item, snapshot_item)
                                } else {
                                    None
                                }
                            };
                            drop(replaced);
                            let _ = done.send(item);
                        }
                        worker_reading.store(false, Ordering::Release);
                    }
                    Command::Write(item) => crate::clipboard::write_to_clipboard((*item).clone()),
                }
            }
        })
    {
        log::error!("could not start clipboard worker: {error}");
    }
    ClipboardWorker {
        send,
        snapshot,
        reading,
    }
});

impl ClipboardWorker {
    pub(crate) fn cached(&self) -> Option<ClipboardItem> {
        let item = self.snapshot.lock().item.clone();
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

    pub(crate) fn write(&self, item: ClipboardItem) {
        let item = Arc::new(item);
        let mut cache = self.snapshot.lock();
        let replaced = if self.send.try_send(Command::Write(item.clone())).is_ok() {
            cache.revision = cache.revision.wrapping_add(1);
            cache.item.replace(item)
        } else {
            log::warn!("clipboard worker busy or unavailable; copy was not queued");
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
            log::warn!("clipboard provider did not respond before the deadline");
            Err(ClipboardReadError::Unavailable)
        }
    }
}
