use super::*;

type Hook = (SessionId, Arc<dyn Fn() + Send + Sync>);
static BEFORE_RECORD: Mutex<Vec<Hook>> = Mutex::new(Vec::new());

/// Runs inside the reader's record step, after the engine took the batch.
pub(super) fn before_record(id: SessionId) {
    let hook = lock(&BEFORE_RECORD)
        .iter()
        .find(|(target, _)| *target == id)
        .map(|(_, hook)| Arc::clone(hook));
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use krill_proto::screen::{Attach, GapMsg, Msg as _, Stream, Update, View, kind};
    use std::io::Write as _;
    use std::sync::mpsc;

    /// Runs `hook` in every record step of `id` until dropped.
    struct Hooked(SessionId);

    impl Hooked {
        fn install(id: SessionId, hook: impl Fn() + Send + Sync + 'static) -> Self {
            lock(&BEFORE_RECORD).push((id, Arc::new(hook)));
            Self(id)
        }
    }

    impl Drop for Hooked {
        fn drop(&mut self) {
            lock(&BEFORE_RECORD).retain(|(target, _)| *target != self.0);
        }
    }

    /// Holds every record step of `id` until dropped.
    fn stall(id: SessionId) -> (Hooked, mpsc::Sender<()>) {
        let (release, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        let hooked = Hooked::install(id, move || {
            lock(&wait).recv().ok();
        });
        (hooked, release)
    }

    /// A real PTY: attach asks it for the window size.
    fn fixture(id: u64) -> (Session, std::fs::File) {
        let pty = rustix_openpty::openpty(None, None).unwrap();
        let session = Session::start(
            SessionId(id),
            pty.controller,
            WinSize { cols: 80, rows: 24 },
            Arc::new(Appearance::load(std::path::PathBuf::new())),
            None,
        )
        .unwrap();
        (session, std::fs::File::from(pty.user))
    }

    fn attach(tap: &Tap, stream: Stream) -> krill_proto::os::net::UnixStream {
        let (client, server) = krill_proto::os::net::UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let tap = tap.clone();
        let session = tap.0.id;
        thread::spawn(move || {
            crate::attach::run_as(
                server,
                Attach { session, stream },
                1,
                0,
                Some(tap),
                krill_proto::roles::Role::Observer,
                crate::attach::FRAME_MIN,
            )
        });
        client
    }

    fn frame(client: &mut krill_proto::os::net::UnixStream) -> (u8, Vec<u8>) {
        krill_proto::ipc::read_frame(client)
            .expect("frame before timeout")
            .expect("open connection")
    }

    /// Nothing arrives for a while: the raw stream is waiting, not reporting a gap.
    fn quiet(client: &mut krill_proto::os::net::UnixStream) {
        client
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let got = krill_proto::ipc::read_frame(client);
        assert!(
            got.as_ref()
                .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock),
            "unexpected frame {:?}",
            got.map(|f| f.map(|(k, _)| k))
        );
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
    }

    fn first_line(view: &View) -> String {
        view.screen
            .rows
            .first()
            .map(|row| row.iter().map(|cell| cell.ch).collect())
            .unwrap_or_default()
    }

    #[test]
    fn screen_update_does_not_wait_for_slow_recording() {
        let (session, mut output) = fixture(9101);
        let tap = session.tap();
        let stall = stall(SessionId(9101));
        let mut screen = attach(&tap, Stream::Screen { known: None });
        assert_eq!(frame(&mut screen).0, kind::ATTACHED);
        let mut view = View::default();
        let mut apply = |view: &mut View, done: &dyn Fn(&View) -> bool| {
            while !(view.revision.is_some() && done(view)) {
                let (k, body) = frame(&mut screen);
                if k == kind::UPDATE {
                    view.apply(Update::decode(k, &body).unwrap()).unwrap();
                }
            }
        };
        apply(&mut view, &|_| true);
        output.write_all(b"hello").unwrap();
        apply(&mut view, &|view| first_line(view).starts_with("hello"));
        assert_eq!(view.seq, Seq(5), "screen carries the engine's seq");
        assert_eq!(tap.pulse().end, 0, "recording is still stalled");

        let mut raw = attach(&tap, Stream::Raw { from: view.seq });
        assert_eq!(frame(&mut raw).0, kind::ATTACHED);
        quiet(&mut raw);
        drop(stall);
        output.write_all(b"!").unwrap();
        let (k, body) = frame(&mut raw);
        assert_eq!(
            k,
            kind::OUTPUT,
            "raw from a screen seq whose recording is in flight is not a crash gap"
        );
        let (seq, bytes) = krill_proto::screen::decode_output(k, &body).unwrap();
        assert_eq!((seq, bytes), (Seq(5), &b"!"[..]));
    }

    #[test]
    fn raw_gap_only_past_what_the_engine_took() {
        let (session, _output) = fixture(9102);
        let tap = session.tap();
        lock(&tap.0.live).seq = 5;
        tap.0.publish(b"abc", 0, false);

        let mut in_flight = attach(&tap, Stream::Raw { from: Seq(4) });
        let mut crashed = attach(&tap, Stream::Raw { from: Seq(6) });
        assert_eq!(frame(&mut in_flight).0, kind::ATTACHED);
        assert_eq!(frame(&mut crashed).0, kind::ATTACHED);
        quiet(&mut in_flight);
        let (k, body) = frame(&mut crashed);
        assert_eq!(
            GapMsg::decode(k, &body).unwrap(),
            GapMsg {
                start: Seq(3),
                len: 0
            },
            "a seq the engine never took is still a crash gap"
        );
        tap.0.publish(b"de", 0, false);
        let (k, body) = frame(&mut in_flight);
        assert_eq!(k, kind::OUTPUT);
        let (seq, bytes) = krill_proto::screen::decode_output(k, &body).unwrap();
        assert_eq!((seq, bytes), (Seq(4), &b"e"[..]));
    }

    /// Screen publish latency while each batch's recording takes `KRILL_BENCH_RECORD_MS` (default 100).
    /// Prints `N p50 p99` in ms and writes every applied screen to `KRILL_BENCH_OUT` for a before/after byte diff.
    #[test]
    #[ignore = "benchmark"]
    fn bench_screen_publish_under_slow_recording() {
        let delay = Duration::from_millis(
            std::env::var("KRILL_BENCH_RECORD_MS").map_or(100, |v| v.parse().unwrap()),
        );
        let n: usize = std::env::var("KRILL_BENCH_N").map_or(40, |v| v.parse().unwrap());
        let (session, mut output) = fixture(9103);
        let tap = session.tap();
        let _slow = Hooked::install(SessionId(9103), move || thread::sleep(delay));
        let mut screen = attach(&tap, Stream::Screen { known: None });
        assert_eq!(frame(&mut screen).0, kind::ATTACHED);
        screen
            .set_read_timeout(Some(delay * 4 + Duration::from_secs(2)))
            .unwrap();
        let mut view = View::default();
        let mut screens = String::new();
        let mut took = Vec::with_capacity(n);
        let text = |view: &View| -> Vec<String> {
            view.screen
                .rows
                .iter()
                .map(|row| row.iter().map(|cell| cell.ch).collect::<String>())
                .collect()
        };
        for i in 0..=n {
            let line = format!("line {i:03}");
            let started = Instant::now();
            output.write_all(format!("{line}\n").as_bytes()).unwrap();
            while !(view.revision.is_some() && text(&view).iter().any(|row| row.starts_with(&line)))
            {
                let (k, body) = frame(&mut screen);
                if k == kind::UPDATE {
                    view.apply(Update::decode(k, &body).unwrap()).unwrap();
                }
            }
            let elapsed = started.elapsed();
            let fed = tap.fed();
            let raw_started = Instant::now();
            while tap.pulse().end < fed {
                thread::sleep(Duration::from_millis(1));
            }
            assert!(raw_started.elapsed() + elapsed >= delay, "recording ran");
            assert_eq!(
                view.screen,
                tap.capture().0,
                "applied frames equal the engine screen"
            );
            screens.push_str(&text(&view).join("\n"));
            screens.push_str("\n--\n");
            if i > 0 {
                took.push(elapsed);
            }
        }
        took.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let at = |pct: usize| took[((took.len() - 1) * pct).div_ceil(100)];
        println!(
            "BENCH record_ms={} n={} p50_ms={:.3} p99_ms={:.3} max_ms={:.3}",
            delay.as_millis(),
            took.len(),
            ms(at(50)),
            ms(at(99)),
            ms(took[took.len() - 1])
        );
        if let Some(path) = std::env::var_os("KRILL_BENCH_OUT") {
            std::fs::write(path, screens).unwrap();
        }
    }
}
