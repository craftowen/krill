//! Repeated GUI failures against one unchanged daemon and PTY incarnation.
use super::*;

const LINES: usize = 64;
const READY: &[u8] = b"SUSTAINED-READY\r\n";

#[test]
#[ignore = "actual macOS GUI; build all three release binaries first"]
fn release_same_session_survival_smoke() {
    run(2);
}

#[test]
#[ignore = "1000 actual macOS GUI restarts; build all three release binaries first"]
fn release_same_session_survives_one_thousand_app_restarts() {
    run(1_000);
}

fn bytes(round: usize) -> Vec<u8> {
    let mut out: Vec<u8> = (0..LINES)
        .flat_map(|line| {
            format!(
                "{round:04}/{line:02} 한글 \x1b[3{}m색\x1b[0m line\r\n",
                line % 8
            )
            .into_bytes()
        })
        .collect();
    out.extend_from_slice(format!("SUSTAINED-{round}\r\n").as_bytes());
    out
}

#[expect(
    clippy::assertions_on_constants,
    reason = "refuse debug execution of the explicitly selected native release fixture"
)]
fn run(rounds: usize) {
    assert!(!cfg!(debug_assertions), "requires the release profile");
    assert_eq!(
        Path::new(APP).parent().unwrap().file_name().unwrap(),
        "release"
    );
    let stop = std::env::var_os("KRILL_NATIVE_STRESS_STOP").map(PathBuf::from);
    let dir = PathBuf::from(format!("/tmp/mk031-sustained-{}", std::process::id()));
    let (paths, mut guard, instance) = start_daemon(dir);
    let generator = guard.dir.join("generator.pl");
    fs::write(
        &generator,
        format!(
            "$|=1; print \"SUSTAINED-READY\\r\\n\"; for $round (1..{rounds}) {{\n\
             while (!-e \"$ARGV[0]/go-$round\") {{ select undef,undef,undef,0.02; }}\n\
             for $line (0..{}) {{ printf \"%04d/%02d 한글 \\e[3%dm색\\e[0m line\\r\\n\", $round, $line, $line%8; }}\n\
             print \"SUSTAINED-$round\\r\\n\";\n\
             open $done, '>', \"$ARGV[0]/done-$round\" or die $!; close $done; }}\n\
             while (1) {{ select undef,undef,undef,1; }}\n",
            LINES - 1
        ),
    )
    .unwrap();
    let script = format!(
        "stty raw -echo; exec /usr/bin/perl {} {}",
        generator.display(),
        guard.dir.display()
    );
    let Reply::Created { session, pid } = request(
        &paths,
        &Request::Create(Create {
            account: None,
            program: Program::Custom {
                program: b"/bin/sh".to_vec(),
                args: vec![b"-c".to_vec(), script.into_bytes()],
            },
            cwd: Vec::new(),
            cols: 80,
            rows: 24,
        }),
    ) else {
        unreachable!("generator session creation failed")
    };
    guard.sessions.push(pid);
    let mut layout = Layout::default();
    layout.set_session(layout.visible()[0], session);
    let layout_path = guard.dir.join("layout.bin");
    layout.save(&layout_path).unwrap();
    let mut want = READY.to_vec();
    let mut got = Vec::new();
    let (mut stream, _) = raw(&paths, session, 0);
    receive(&mut stream, &mut got, want.len());
    drop(stream);
    launch_app(&mut guard);
    println!(
        "SUSTAINED START rounds {rounds} app {} session {} pid {pid} daemon {instance} holder {} state {}",
        guard.apps.last().unwrap().id(),
        session.0,
        guard.holder.id(),
        guard.dir.display()
    );
    std::io::stdout().flush().unwrap();

    for round in 1..=rounds {
        // The controller requests a stop between rounds. Panic unwinds the owned
        // process guard; killing the test runner would bypass that cleanup.
        assert!(
            !stop.as_ref().is_some_and(|path| path.exists()),
            "INCOMPLETE: stopped after {} of {rounds} rounds",
            round - 1
        );
        let app = guard.apps.last_mut().unwrap();
        let before = app.id();
        app.kill().unwrap();
        assert_eq!(app.wait().unwrap().signal(), Some(9));
        assert_eq!(info(&paths, session).pid, pid);
        let resume = u64::try_from(got.len()).unwrap();
        fs::write(guard.dir.join(format!("go-{round}")), []).unwrap();
        wait_for("output while GUI and raw observer are absent", || {
            guard.dir.join(format!("done-{round}")).exists()
        });
        want.extend(bytes(round));
        launch_app(&mut guard);
        assert_eq!(info(&paths, session).pid, pid);
        assert_eq!(connection(&paths).server.instance_id, instance);
        let restored = Layout::load(&layout_path).unwrap();
        assert_eq!(restored.session(restored.visible()[0]), Some(session));
        let (mut stream, _) = raw(&paths, session, resume);
        // receive waits for actual sequenced bytes, including output the daemon
        // may still be draining after the generator published its done file.
        receive(&mut stream, &mut got, want.len());
        drop(stream);
        assert_eq!(got, want, "output changed after restart {round}");
        println!(
            "SUSTAINED round {round}/{rounds}: app {before} SIGKILL=9 -> app {}, session {} pid {pid} daemon {instance}, bytes {} PASS",
            guard.apps.last().unwrap().id(),
            session.0,
            got.len()
        );
        std::io::stdout().flush().unwrap();
    }
    let hash = sha256(&got, &guard.dir, "got.bin");
    assert_eq!(hash, sha256(&want, &guard.dir, "want.bin"));
    println!(
        "SUSTAINED COMPLETE rounds {rounds} session {} pid {pid} daemon {instance} bytes {} sha256 {hash}",
        session.0,
        got.len()
    );
    assert_eq!(request(&paths, &Request::Close { session }), Reply::Closed);
    guard.sessions.clear();
}
