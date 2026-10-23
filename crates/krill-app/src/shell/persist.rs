//! UI-coalesced snapshots feed one replaceable pending write and durable barriers.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use krill_app::layout::Layout;

pub(super) const DELAY: Duration = Duration::from_millis(150);

/// UI requests retain no Layout. The capacity-one wake only starts an idle
/// actor; another wake resets the quiet timer without spawning another task.
pub(super) struct Requests {
    pending: Cell<bool>,
    wake: tokio::sync::mpsc::Sender<()>,
    #[cfg(test)]
    snapshots: Cell<usize>,
}

impl Requests {
    pub(super) fn new() -> (Self, tokio::sync::mpsc::Receiver<()>) {
        let (wake, recv) = tokio::sync::mpsc::channel(1);
        (
            Self {
                pending: Cell::new(false),
                wake,
                #[cfg(test)]
                snapshots: Cell::new(0),
            },
            recv,
        )
    }

    pub(super) fn request(&self) {
        self.pending.set(true);
        // An already queued wake covers the newest pending state as well.
        self.wake.try_send(()).ok();
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending.get()
    }

    pub(super) fn take(&self) -> bool {
        let pending = self.pending.replace(false);
        #[cfg(test)]
        if pending {
            self.snapshots.set(self.snapshots.get() + 1);
        }
        pending
    }

    #[cfg(all(test, unix))]
    pub(super) fn snapshots(&self) -> usize {
        self.snapshots.get()
    }
}

pub(super) fn schedule(
    mut recv: tokio::sync::mpsc::Receiver<()>,
    cx: &mut gpui::Context<super::Shell>,
) -> gpui::Task<()> {
    cx.spawn(async move |this, cx| {
        while recv.recv().await.is_some() {
            let Ok(pending) = this.read_with(cx, |shell, _| shell.layout_save_requests.is_pending()) else { return };
            if !pending { continue; }
            loop {
                tokio::select! {
                    // Drain requests before a ready timer so a same-turn final
                    // geometry/cursor update always starts another quiet period.
                    biased;
                    request = recv.recv() => {
                        if request.is_none() { return; }
                    }
                    () = cx.background_executor().timer(DELAY) => {
                        if this.update(cx, |shell, _| { shell.flush_layout_snapshot(); }).is_err() { return; }
                        break;
                    }
                }
            }
        }
    })
}

#[derive(Default)]
struct Pending {
    layout: Option<Box<Layout>>,
    generation: u64,
    durable: u64,
    retry_started: Option<Instant>,
    barriers: Vec<(u64, mpsc::Sender<bool>)>,
    closed: bool,
}

#[derive(Default)]
struct Mailbox {
    pending: Mutex<Pending>,
    wake: Condvar,
}

impl Mailbox {
    fn close(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.closed = true;
            self.wake.notify_one();
        }
    }

    fn barrier(&self, done: mpsc::Sender<bool>) -> bool {
        let Ok(mut pending) = self.pending.lock() else {
            return false;
        };
        if pending.closed {
            return false;
        }
        if pending.durable == pending.generation {
            done.send(true).ok();
        } else {
            let generation = pending.generation;
            pending.barriers.push((generation, done));
            self.wake.notify_one();
        }
        true
    }
}

#[derive(Default)]
pub(super) struct Saver {
    worker: Rc<RefCell<Option<Worker>>>,
}

struct Worker {
    mailbox: Arc<Mailbox>,
    task: Option<std::thread::JoinHandle<()>>,
}

impl Saver {
    /// The app owns the join even after its window disappears.
    pub(super) fn new(cx: &mut gpui::App) -> Self {
        let saver = Self::default();
        let worker = saver.worker.clone();
        cx.on_app_quit(move |cx| {
            let worker = worker.clone();
            let executor = cx.background_executor().clone();
            async move {
                let task = worker.borrow_mut().as_mut().and_then(|worker| {
                    worker.mailbox.close();
                    worker.task.take()
                });
                executor
                    .spawn(async move {
                        if let Some(task) = task {
                            task.join().ok();
                        }
                    })
                    .await;
            }
        })
        .detach();
        saver
    }

    /// Resolves only after this generation (or a newer snapshot) is durable.
    pub(super) fn barrier(&self, cx: &gpui::App) -> gpui::Task<bool> {
        let (send, recv) = mpsc::channel();
        let worker = self.worker.borrow();
        let Some(worker) = worker.as_ref() else {
            return gpui::Task::ready(true);
        };
        let queued = worker.mailbox.barrier(send);
        cx.background_executor()
            .spawn(async move { queued && recv.recv().unwrap_or(false) })
    }

    pub(super) fn save_snapshot(&self, path: &Path, layout: Layout) -> bool {
        let mut worker = self.worker.borrow_mut();
        if worker.is_none() {
            let path = path.to_owned();
            let mailbox = Arc::new(Mailbox::default());
            let receiver = mailbox.clone();
            match std::thread::Builder::new()
                .name("layout-save".into())
                .spawn(move || run(&path, &receiver))
            {
                Ok(task) => {
                    *worker = Some(Worker {
                        mailbox,
                        task: Some(task),
                    });
                }
                Err(err) => tracing::error!(error = %err, "layout save worker failed to start"),
            }
        }
        if let Some(worker) = worker.as_ref()
            && let Ok(mut pending) = worker.mailbox.pending.lock()
            && !pending.closed
        {
            pending.generation = pending.generation.wrapping_add(1);
            pending.layout = Some(Box::new(layout));
            pending.retry_started = None;
            worker.mailbox.wake.notify_one();
            return true;
        }
        false
    }

    #[cfg(test)]
    pub(super) fn flush(&self) {
        let (send, recv) = mpsc::channel();
        if let Some(worker) = self.worker.borrow().as_ref() {
            assert!(worker.mailbox.barrier(send));
            assert!(recv.recv_timeout(Duration::from_secs(5)).unwrap());
        }
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.borrow().as_ref() {
            worker.mailbox.close();
        }
    }
}

fn run(path: &Path, mailbox: &Mailbox) {
    loop {
        let Ok(mut pending) = mailbox.pending.lock() else {
            return;
        };
        loop {
            if pending.closed || !pending.barriers.is_empty() {
                break;
            }
            let delay = pending
                .retry_started
                .map(|at| DELAY.saturating_sub(at.elapsed()));
            match delay {
                Some(delay) if delay.is_zero() => break,
                Some(delay) => {
                    let Ok((next, _)) = mailbox.wake.wait_timeout(pending, delay) else {
                        return;
                    };
                    pending = next;
                }
                None => {
                    // Normal snapshots already passed UI quiet time. Only
                    // failed writes back off; do not add another save debounce.
                    if pending.layout.is_some() {
                        break;
                    }
                    let Ok(next) = mailbox.wake.wait(pending) else {
                        return;
                    };
                    pending = next;
                }
            }
        }
        let generation = pending.generation;
        let layout = pending.layout.take();
        pending.retry_started = None;
        drop(pending);
        // Disk IO never holds the producer lock.
        let saved = layout.as_ref().is_none_or(|layout| {
            layout
                .save(path)
                .inspect_err(|err| tracing::error!(error = %err, "layout save failed"))
                .is_ok()
        });
        let Ok(mut pending) = mailbox.pending.lock() else {
            return;
        };
        if saved {
            pending.durable = generation;
        } else if pending.layout.is_none() {
            pending.layout = layout;
            pending.retry_started = Some(Instant::now());
        }
        pending.barriers.retain(|(target, done)| {
            if *target <= generation {
                done.send(saved).ok();
                false
            } else {
                true
            }
        });
        if pending.closed && (pending.layout.is_none() || !saved) {
            return;
        }
    }
}

#[test]
fn closing_saver_flushes_latest_layout() {
    let dir = std::env::temp_dir().join(format!("mk-layout-save-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let path = dir.join("layout.bin");
    let saver = Saver::default();
    let mut layout = Layout::default();
    assert!(saver.save_snapshot(&path, layout.clone()));
    layout.new_tab();
    assert!(saver.save_snapshot(&path, layout.clone()));
    let worker = saver.worker.clone();
    drop(saver);
    worker
        .borrow_mut()
        .as_mut()
        .unwrap()
        .task
        .take()
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(Layout::load(&path).unwrap(), layout);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dropping_saver_does_not_wait_for_a_stalled_worker() {
    let (release, wait) = mpsc::channel();
    let (started, ready) = mpsc::channel();
    let task = std::thread::spawn(move || {
        started.send(()).unwrap();
        // Timeout lets an old blocking Drop fail instead of hanging the suite.
        wait.recv_timeout(Duration::from_secs(2)).ok();
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let saver = Saver::default();
    *saver.worker.borrow_mut() = Some(Worker {
        mailbox: std::sync::Arc::new(Mailbox::default()),
        task: Some(task),
    });
    let worker = saver.worker.clone();
    let began = std::time::Instant::now();
    drop(saver);
    let elapsed = began.elapsed();
    release.send(()).ok();
    worker
        .borrow_mut()
        .as_mut()
        .unwrap()
        .task
        .take()
        .unwrap()
        .join()
        .unwrap();
    assert!(
        elapsed < Duration::from_secs(1),
        "UI Drop waited for disk worker: {elapsed:?}"
    );
}

#[test]
fn burst_replaces_one_pending_snapshot_and_barrier_waits_for_latest() {
    let dir = std::env::temp_dir().join(format!("mk-layout-burst-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let path = dir.join("layout.bin");
    let mailbox = Arc::new(Mailbox::default());
    // A parked worker makes queue size and generation ordering deterministic.
    let saver = Saver::default();
    *saver.worker.borrow_mut() = Some(Worker {
        mailbox: mailbox.clone(),
        task: None,
    });
    let mut layout = Layout::default();
    for _ in 0..40 {
        layout.new_tab();
        assert!(saver.save_snapshot(&path, layout.clone()));
    }
    let (done, wait) = mpsc::channel();
    assert!(mailbox.barrier(done));
    {
        let pending = mailbox.pending.lock().unwrap();
        assert_eq!(pending.generation, 40);
        assert_eq!(pending.layout.as_deref(), Some(&layout));
        assert_eq!(pending.barriers.len(), 1);
    }
    assert!(wait.try_recv().is_err());
    mailbox.close();
    run(&path, &mailbox);
    assert!(wait.recv_timeout(Duration::from_secs(1)).unwrap());
    assert_eq!(Layout::load(&path).unwrap(), layout);
    std::fs::remove_dir_all(dir).unwrap();
}
