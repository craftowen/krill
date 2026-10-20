//! 레이아웃 저장은 단일 작업자가 디바운스한 뒤 수행한다. UI는 fsync를 기다리지 않는다.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use krill_app::layout::Layout;

const DELAY: Duration = Duration::from_millis(150);

enum Message {
    Save(Box<Layout>),
    Flush(mpsc::Sender<bool>),
}

#[derive(Default)]
pub(super) struct Saver {
    worker: Rc<RefCell<Option<Worker>>>,
}

struct Worker {
    send: Option<mpsc::Sender<Message>>,
    task: Option<std::thread::JoinHandle<()>>,
}

impl Saver {
    /// The app retains the join handle even after a window closes. Shutdown
    /// awaits the final durable write on a background thread, never in Drop.
    pub(super) fn new(cx: &mut gpui::App) -> Self {
        let saver = Self::default();
        let worker = saver.worker.clone();
        cx.on_app_quit(move |cx| {
            let task = worker.borrow_mut().as_mut().and_then(|worker| {
                worker.send.take();
                worker.task.take()
            });
            cx.background_executor().spawn(async move {
                if let Some(task) = task {
                    task.join().ok();
                }
            })
        })
        .detach();
        saver
    }

    /// Ordered barrier: window close awaits this without blocking input/render.
    pub(super) fn barrier(&self, cx: &gpui::App) -> gpui::Task<bool> {
        let (send, recv) = mpsc::channel();
        let queued = self
            .worker
            .borrow()
            .as_ref()
            .and_then(|w| w.send.as_ref())
            .is_none_or(|worker| worker.send(Message::Flush(send)).is_ok());
        if self.worker.borrow().is_none() {
            return gpui::Task::ready(true);
        }
        cx.background_executor()
            .spawn(async move { queued && recv.recv().unwrap_or(false) })
    }

    pub(super) fn save(&self, path: &std::path::Path, layout: &Layout) {
        let mut worker = self.worker.borrow_mut();
        if worker.is_none() {
            let path = path.to_owned();
            let (send, recv) = mpsc::channel();
            match std::thread::Builder::new()
                .name("layout-save".into())
                .spawn(move || run(&path, &recv))
            {
                Ok(task) => {
                    *worker = Some(Worker {
                        send: Some(send),
                        task: Some(task),
                    });
                }
                Err(err) => tracing::error!(error = %err, "layout save worker failed to start"),
            }
        }
        if let Some(worker) = worker.as_ref().and_then(|w| w.send.as_ref()) {
            worker.send(Message::Save(Box::new(layout.clone()))).ok();
        }
    }

    #[cfg(test)]
    pub(super) fn flush(&self) {
        let (send, recv) = mpsc::channel();
        if let Some(worker) = self.worker.borrow().as_ref() {
            worker
                .send
                .as_ref()
                .unwrap()
                .send(Message::Flush(send))
                .unwrap();
            assert!(recv.recv_timeout(Duration::from_secs(5)).unwrap());
        }
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.borrow_mut().as_mut() {
            // Disconnect drains pending saves; the app quit hook owns the join.
            worker.send.take();
        }
    }
}

fn run(path: &Path, recv: &mpsc::Receiver<Message>) {
    let mut pending: Option<Box<Layout>> = None;
    loop {
        let message = if pending.is_some() {
            recv.recv_timeout(DELAY)
        } else {
            recv.recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        match message {
            Ok(Message::Save(layout)) => pending = Some(layout),
            Ok(Message::Flush(done)) => {
                let saved = write(path, &mut pending);
                done.send(saved).ok();
            }
            Err(error) => {
                write(path, &mut pending);
                if error == mpsc::RecvTimeoutError::Disconnected {
                    return;
                }
            }
        }
    }
}

fn write(path: &std::path::Path, pending: &mut Option<Box<Layout>>) -> bool {
    if let Some(layout) = pending.as_ref() {
        if let Err(err) = layout.save(path) {
            tracing::error!(error = %err, "layout save failed");
            return false;
        }
        pending.take();
    }
    true
}

#[test]
fn closing_saver_flushes_latest_layout() {
    let dir = std::env::temp_dir().join(format!("mk-layout-save-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let path = dir.join("layout.bin");
    let saver = Saver::default();
    let mut layout = Layout::default();
    saver.save(&path, &layout);
    layout.new_tab();
    saver.save(&path, &layout);
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
    let (send, _recv) = mpsc::channel();
    let task = std::thread::spawn(move || {
        started.send(()).unwrap();
        // Timeout lets an old blocking Drop fail instead of hanging the suite.
        wait.recv_timeout(Duration::from_secs(2)).ok();
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let saver = Saver::default();
    *saver.worker.borrow_mut() = Some(Worker {
        send: Some(send),
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
