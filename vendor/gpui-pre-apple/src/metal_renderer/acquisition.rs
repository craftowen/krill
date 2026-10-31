// Krill modification (Apache-2.0 §4(b)): a slow drawable acquisition must not
// hold the foreground thread or a window mutex. At most one request is active.
use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

pub(super) struct Acquisition<I, O> {
    requests: SyncSender<I>,
    results: Receiver<O>,
    pending: bool,
    prefetch: bool,
}

impl<I: Send + 'static, O: Send + 'static> Acquisition<I, O> {
    pub(super) fn new(acquire: impl FnMut(I) -> O + Send + 'static) -> io::Result<Self> {
        Self::spawn(acquire, || {}, true)
    }

    /// Notify only after the result is readable; acquire again only on a new request.
    pub(super) fn with_ready_callback(
        acquire: impl FnMut(I) -> O + Send + 'static,
        ready: impl Fn() + Send + 'static,
    ) -> io::Result<Self> {
        Self::spawn(acquire, ready, false)
    }

    fn spawn(
        mut acquire: impl FnMut(I) -> O + Send + 'static,
        ready: impl Fn() + Send + 'static,
        prefetch: bool,
    ) -> io::Result<Self> {
        let (requests, incoming) = mpsc::sync_channel(1);
        let (finished, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("metal-drawable".into())
            .spawn(move || {
                while let Ok(input) = incoming.recv() {
                    if finished.send(acquire(input)).is_err() {
                        break;
                    }
                    ready();
                }
            })?;
        Ok(Self {
            requests,
            results,
            pending: false,
            prefetch,
        })
    }

    /// Never wait for the worker. Dropping this owner also never joins it.
    /// A readiness callback can resume the scene before the next display-link tick.
    pub(super) fn poll(&mut self, input: I) -> Result<Option<O>, ()> {
        if self.pending {
            return match self.results.try_recv() {
                Ok(output) => {
                    // Start the next bounded acquisition before returning this frame. Otherwise
                    // every successful presentation needs a request-only display tick in between.
                    if self.prefetch {
                        self.requests.try_send(input).map_err(|_| ())?;
                    } else {
                        self.pending = false;
                    }
                    Ok(Some(output))
                }
                Err(TryRecvError::Empty) => Ok(None),
                Err(TryRecvError::Disconnected) => Err(()),
            };
        }
        self.request(input)?;
        Ok(None)
    }

    /// Ensure one request exists without consuming an already completed result.
    pub(super) fn request(&mut self, input: I) -> Result<(), ()> {
        if !self.pending {
            self.requests.try_send(input).map_err(|_| ())?;
            self.pending = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn on_demand_completion_is_readable_when_notified_and_never_prefetches() {
        let (started, running) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (ready, notified) = mpsc::channel();
        let mut acquisition = Acquisition::with_ready_callback(
            move |input: u32| {
                started.send(input).unwrap();
                blocked.recv().unwrap();
                input
            },
            move || ready.send(()).unwrap(),
        )
        .unwrap();
        for (input, later) in [(1, 2), (3, 4)] {
            assert_eq!(acquisition.poll(input), Ok(None));
            assert_eq!(running.recv_timeout(Duration::from_secs(5)).unwrap(), input);
            assert_eq!(acquisition.poll(later), Ok(None));
            release.send(()).unwrap();
            notified.recv_timeout(Duration::from_secs(5)).unwrap();
            acquisition.request(later).unwrap();
            assert_eq!(acquisition.poll(later), Ok(Some(input)));
            assert!(running.try_recv().is_err(), "no unused drawable requested");
        }
        drop(acquisition);
    }

    #[test]
    fn slow_acquisition_does_not_block_poll_or_owner_drop() {
        let (started, running) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (stopped, exited) = mpsc::channel();
        let mut acquisition = Acquisition::new(move |input: u32| {
            started.send(input).unwrap();
            blocked.recv().unwrap();
            stopped.send(()).unwrap();
            input
        })
        .unwrap();
        assert_eq!(acquisition.poll(1), Ok(None));
        assert_eq!(running.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        let start = Instant::now();
        for input in 2..1000 {
            assert_eq!(acquisition.poll(input), Ok(None));
        }
        drop(acquisition);
        assert!(start.elapsed() < Duration::from_millis(250));
        assert!(running.try_recv().is_err());
        release.send(()).unwrap();
        exited.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn completion_prefetches_the_latest_input_without_a_request_only_tick() {
        let (started, running) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let mut acquisition = Acquisition::new(move |input: u32| {
            started.send(input).unwrap();
            blocked.recv().unwrap();
            input
        })
        .unwrap();
        assert_eq!(acquisition.poll(1), Ok(None));
        assert_eq!(running.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        assert_eq!(acquisition.poll(2), Ok(None));
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let completed = loop {
            if let Some(output) = acquisition.poll(3).unwrap() {
                break output;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(completed, 1);
        // No intervening poll: consuming frame 1 must already have requested frame 3.
        assert_eq!(running.recv_timeout(Duration::from_secs(5)).unwrap(), 3);
        assert_eq!(acquisition.poll(4), Ok(None));
        assert!(
            running.try_recv().is_err(),
            "only one prefetch may be active"
        );
        release.send(()).unwrap();
        let completed = loop {
            if let Some(output) = acquisition.poll(5).unwrap() {
                break output;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(completed, 3, "an in-flight acquire is never duplicated");
        assert_eq!(running.recv_timeout(Duration::from_secs(5)).unwrap(), 5);
        drop(acquisition);
        release.send(()).unwrap();
    }
}
