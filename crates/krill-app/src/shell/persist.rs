//! 레이아웃 저장은 단일 작업자가 디바운스한 뒤 수행한다. UI는 fsync를 기다리지 않는다.

use std::path::Path;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use krill_app::layout::Layout;

const DELAY: Duration = Duration::from_millis(150);

enum Message {
    Save(Layout),
    #[cfg(test)]
    Flush(mpsc::Sender<()>),
}

#[derive(Default)]
pub(super) struct Saver {
    worker: Mutex<Option<Worker>>,
}

struct Worker {
    send: mpsc::Sender<Message>,
    task: std::thread::JoinHandle<()>,
}

impl Saver {
    pub(super) fn save(&self, path: &std::path::Path, layout: &Layout) {
        let Ok(mut worker) = self.worker.lock() else {
            return;
        };
        if worker.is_none() {
            let path = path.to_owned();
            let (send, recv) = mpsc::channel();
            match std::thread::Builder::new()
                .name("layout-save".into())
                .spawn(move || run(&path, &recv))
            {
                Ok(task) => *worker = Some(Worker { send, task }),
                Err(err) => eprintln!("레이아웃 저장 작업자 시작 실패: {err}"),
            }
        }
        if let Some(worker) = worker.as_ref() {
            worker.send.send(Message::Save(layout.clone())).ok();
        }
    }

    #[cfg(test)]
    pub(super) fn flush(&self) {
        let (send, recv) = mpsc::channel();
        if let Some(worker) = self.worker.lock().unwrap().as_ref() {
            worker.send.send(Message::Flush(send)).unwrap();
            recv.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }
}

impl Drop for Saver {
    fn drop(&mut self) {
        let worker = self
            .worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(Worker { send, task }) = worker {
            // 정상 창 종료에서는 마지막 디바운스 대기분도 저장하고 끝낸다.
            drop(send);
            if task.join().is_err() {
                eprintln!("레이아웃 저장 작업자 종료 실패");
            }
        }
    }
}

fn run(path: &Path, recv: &mpsc::Receiver<Message>) {
    let mut pending = None;
    loop {
        let message = if pending.is_some() {
            recv.recv_timeout(DELAY)
        } else {
            recv.recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        match message {
            Ok(Message::Save(layout)) => pending = Some(layout),
            #[cfg(test)]
            Ok(Message::Flush(done)) => {
                write(path, &mut pending);
                done.send(()).ok();
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

fn write(path: &std::path::Path, pending: &mut Option<Layout>) {
    if let Some(layout) = pending.take()
        && let Err(err) = layout.save(path)
    {
        eprintln!("레이아웃 저장 실패: {err}");
    }
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
    drop(saver);
    assert_eq!(Layout::load(&path).unwrap(), layout);
    std::fs::remove_dir_all(dir).unwrap();
}
