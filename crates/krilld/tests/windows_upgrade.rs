//! Native Windows update regression. Only temporary state, installs and fixture agents are used.
//! Bundled test: `KRILL_TEST_CONPTY_DIR=<bundle> cargo nextest run -p krilld --test windows_upgrade --run-ignored all`
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "isolated integration test helpers"
)]

use std::fs;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krill_proto::SessionId;
use krill_proto::control::{
    AgentLaunch, CaptureFormat, Create, Failure, Info, Program, Reply, Request, Upgrade, Upgraded,
};
use krill_proto::daemon;
use krill_proto::ipc::{self, ClientRole, ServerHello};
use krill_proto::os::net::UnixStream;
use krill_proto::screen::Msg as _;

const WAIT: Duration = Duration::from_secs(30);

#[test]
fn update_fixture_role() {
    if std::env::var("KRILL_UPDATE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let path = PathBuf::from(std::env::var_os("KRILL_UPDATE_TRANSCRIPT").unwrap());
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    println!("FIXTURE-READY {}", std::process::id());
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        writeln!(file, "{line}").unwrap();
        file.flush().unwrap();
        println!("FIXTURE-ECHO {line}");
        std::io::stdout().flush().unwrap();
    }
}

struct Cleanup(PathBuf, Option<std::process::Child>);

struct Rig {
    _cleanup: Cleanup,
    root: PathBuf,
    install: PathBuf,
    socket: PathBuf,
    transcript: PathBuf,
    session: SessionId,
    pid: u32,
    fixture_pid: u32,
    metadata: AgentLaunch,
}

impl Rig {
    fn new(bundled: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "krill-update-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut cleanup = Cleanup(root.clone(), None);
        let install = root.join("install");
        let state = root.join("state");
        let home = root.join("home");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&home).unwrap();
        let source = std::env::var_os("KRILL_TEST_BIN_DIR").map_or_else(
            || {
                Path::new(env!("CARGO_BIN_EXE_krilld"))
                    .parent()
                    .unwrap()
                    .to_owned()
            },
            PathBuf::from,
        );
        for name in ["krill-holder.exe", "krilld.exe"] {
            fs::copy(source.join(name), install.join(name)).unwrap();
        }
        fs::copy(
            std::env::current_exe().unwrap(),
            install.join("fixture.exe"),
        )
        .unwrap();
        if bundled {
            let bundle = PathBuf::from(std::env::var_os("KRILL_TEST_CONPTY_DIR")
                .unwrap_or_else(|| panic!("set KRILL_TEST_CONPTY_DIR to a package containing conpty.dll and x64/OpenConsole.exe")));
            fs::copy(bundle.join("conpty.dll"), install.join("conpty.dll")).unwrap();
            fs::create_dir_all(install.join("x64")).unwrap();
            fs::copy(
                bundle.join("x64/OpenConsole.exe"),
                install.join("x64/OpenConsole.exe"),
            )
            .unwrap();
        }
        let transcript = root.join("fixture-transcript.txt");
        // TOML basic strings use the same backslash and quote escapes needed for these paths.
        let quote = |path: &Path| {
            format!(
                "\"{}\"",
                path.display()
                    .to_string()
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
            )
        };
        fs::write(state.join("agents.toml"), format!(
            "[agent.update-fixture]\nname = 'Update fixture'\nlaunch = [{}, '--exact', 'update_fixture_role', '--nocapture']\nyolo_args = []\ninstall = 'fixture only'\n[agent.update-fixture.env]\nKRILL_UPDATE_FIXTURE = '1'\nKRILL_UPDATE_TRANSCRIPT = {}\n",
            quote(&install.join("fixture.exe")), quote(&transcript)
        )).unwrap();
        let socket = state.join("run/krilld.sock");
        let log = fs::File::create(root.join("holder.log")).unwrap();
        // Cargo may put this test in a Job that forbids breakaway. The fixture
        // owns resident lifetime through Cleanup, so no detached bootstrap is needed.
        cleanup.1 = Some(
            Command::new(install.join("krill-holder.exe"))
                .arg(install.join("krilld.exe"))
                .arg(&socket)
                .env("KRILL_STATE_DIR", &state)
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .env("LOCALAPPDATA", home.join("Local"))
                .env("APPDATA", home.join("Roaming"))
                .env_remove("CLAUDE_CONFIG_DIR")
                .env_remove("CODEX_HOME")
                .env_remove("KRILL_HOLDER_STATE")
                .env_remove("KRILL_HOLDER_LINK")
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        let (mut conn, _) = connect(&socket, None);
        let Reply::Created { session, pid } = request(
            &mut conn,
            &Request::Create(Create {
                account: None,
                program: Program::Agent {
                    id: "update-fixture".into(),
                },
                cwd: root.to_str().unwrap().as_bytes().to_vec(),
                cols: 120,
                rows: 30,
            }),
        ) else {
            panic!("fixture creation failed")
        };
        let metadata = AgentLaunch {
            profile: "update-fixture".into(),
            permission: krill_proto::agent::Permission::Yolo,
        };
        let screen = wait_screen(&mut conn, session, "FIXTURE-READY ");
        let fixture_pid = screen
            .lines()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("FIXTURE-READY ")?
                    .trim()
                    .parse()
                    .ok()
            })
            .unwrap();
        let rig = Self {
            _cleanup: cleanup,
            root,
            install,
            socket,
            transcript,
            session,
            pid,
            fixture_pid,
            metadata,
        };
        rig.check(&mut conn, "before-update");
        rig
    }

    fn check(&self, conn: &mut UnixStream, marker: &str) {
        let Reply::Sessions(list) = request(conn, &Request::List) else {
            panic!("list failed")
        };
        assert!(
            list.contains(&Info {
                session: self.session,
                pid: self.pid,
                status: None
            }),
            "live session or PID lost: {list:?}"
        );
        assert_eq!(
            request(
                conn,
                &Request::AgentLaunch {
                    session: self.session
                }
            ),
            Reply::AgentLaunch(Some(self.metadata.clone()))
        );
        assert_eq!(
            request(
                conn,
                &Request::SendInput {
                    session: self.session,
                    bytes: marker.as_bytes().to_vec(),
                    submit: true,
                    wait_submit: false
                }
            ),
            Reply::InputSent
        );
        let screen = wait_screen(conn, self.session, &format!("FIXTURE-ECHO {marker}"));
        assert!(
            screen.contains(&format!("FIXTURE-READY {}", self.fixture_pid)),
            "fixture process was replaced"
        );
        assert!(
            fs::read_to_string(&self.transcript)
                .unwrap()
                .lines()
                .any(|line| line == marker)
        );
    }

    fn upgrade(&self) -> Request {
        Request::Upgrade(Upgrade {
            build: "999.0.0".into(),
            holder: self
                .install
                .join("krill-holder.exe")
                .to_str()
                .unwrap()
                .as_bytes()
                .to_vec(),
            krilld: self
                .install
                .join("krilld.exe")
                .to_str()
                .unwrap()
                .as_bytes()
                .to_vec(),
        })
    }

    fn replace_files(&self, round: usize) {
        let previous = self.root.join(format!("prev-{round}"));
        fs::create_dir_all(previous.join("x64")).unwrap();
        // Match the updater: rename loaded images away, then publish new bytes at the same paths.
        for relative in [
            "krill-holder.exe",
            "krilld.exe",
            "conpty.dll",
            "x64/OpenConsole.exe",
        ] {
            let from = self.install.join(relative);
            let saved = previous.join(relative);
            fs::rename(&from, &saved).unwrap();
            fs::copy(saved, from).unwrap();
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        // Include .prev images, and exclude every process outside this fixture's private root.
        let script = "$r=$env:KRILL_UPDATE_TEST_ROOT; Get-Process krilld,krill-holder,OpenConsole,fixture -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path.StartsWith($r + '\\', [StringComparison]::OrdinalIgnoreCase) } | Stop-Process -Force -ErrorAction SilentlyContinue";
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("KRILL_UPDATE_TEST_ROOT", &self.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
        if let Some(mut holder) = self.1.take() {
            holder.kill().ok();
            holder.wait().ok();
        }
        fs::remove_dir_all(&self.0).ok();
    }
}

fn connect(socket: &Path, previous: Option<u64>) -> (UnixStream, ServerHello) {
    let deadline = Instant::now() + WAIT;
    loop {
        let hello = daemon::client_hello(ClientRole::App, "windows-update-test");
        if let Ok(connection) = daemon::handshake(socket, &hello, Duration::from_secs(2))
            && previous != Some(connection.server.instance_id)
        {
            return (connection.stream, connection.server);
        }
        assert!(Instant::now() < deadline, "successor daemon did not appear");
        thread::sleep(Duration::from_millis(10));
    }
}

fn request(conn: &mut UnixStream, request: &Request) -> Reply {
    conn.set_read_timeout(Some(WAIT)).unwrap();
    conn.set_write_timeout(Some(WAIT)).unwrap();
    conn.write_all(&request.encode().unwrap()).unwrap();
    let (kind, body) = ipc::read_frame(conn).unwrap().unwrap();
    Reply::decode(kind, &body).unwrap()
}

fn wait_screen(conn: &mut UnixStream, session: SessionId, marker: &str) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let Reply::Captured { bytes, .. } = request(
            conn,
            &Request::Capture {
                session,
                format: CaptureFormat::Plain,
            },
        ) else {
            panic!("capture failed")
        };
        let text = String::from_utf8(bytes).unwrap();
        if text.contains(marker) {
            return text;
        }
        assert!(Instant::now() < deadline, "no {marker:?} in {text:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn live_inbox_update_refusal_preserves_agent_pid_metadata_transcript_and_input() {
    let rig = Rig::new(false);
    let (mut conn, before) = connect(&rig.socket, None);
    assert_eq!(
        request(&mut conn, &rig.upgrade()),
        Reply::Failed(Failure::Os(16))
    );
    let (mut conn, after) = connect(&rig.socket, None);
    assert_eq!(before.instance_id, after.instance_id);
    rig.check(&mut conn, "after-refused-update");
}

#[test]
#[ignore = "requires packaged conpty.dll and x64/OpenConsole.exe; set KRILL_TEST_CONPTY_DIR"]
fn bundled_update_preserves_agent_pid_metadata_transcript_and_input() {
    let rig = Rig::new(true);
    let (mut conn, mut server) = connect(&rig.socket, None);
    // Natural exits remain in history while live agents continue. They must not poison handoff.
    let Reply::Created {
        session: exited, ..
    } = request(
        &mut conn,
        &Request::Create(Create {
            account: None,
            program: Program::Custom {
                program: b"C:\\Windows\\System32\\cmd.exe".to_vec(),
                args: vec![b"/d".to_vec()],
            },
            cwd: rig.root.to_str().unwrap().as_bytes().to_vec(),
            cols: 80,
            rows: 24,
        }),
    )
    else {
        panic!("exited fixture creation failed")
    };
    assert_eq!(
        request(
            &mut conn,
            &Request::SendInput {
                session: exited,
                bytes: b"exit 7".to_vec(),
                submit: true,
                wait_submit: false,
            }
        ),
        Reply::InputSent
    );
    let deadline = Instant::now() + WAIT;
    loop {
        let Reply::Sessions(list) = request(&mut conn, &Request::List) else {
            panic!("list failed")
        };
        if list
            .iter()
            .any(|info| info.session == exited && info.status == Some(7))
        {
            break;
        }
        assert!(Instant::now() < deadline, "fixture did not exit: {list:?}");
        thread::sleep(Duration::from_millis(10));
    }
    for round in 0..3 {
        rig.replace_files(round);
        assert_eq!(
            request(&mut conn, &rig.upgrade()),
            Reply::Upgrade(Upgraded::Started)
        );
        let (next, hello) = connect(&rig.socket, Some(server.instance_id));
        conn = next;
        server = hello;
        rig.check(&mut conn, &format!("after-update-{round}"));
        let Reply::Sessions(list) = request(&mut conn, &Request::List) else {
            panic!("list failed")
        };
        assert!(
            list.iter()
                .any(|info| info.session == exited && info.status == Some(7)),
            "update lost the naturally exited session history"
        );
    }
    // A failed replacement must leave the current live generation usable as well.
    let Request::Upgrade(mut missing) = rig.upgrade() else {
        unreachable!()
    };
    missing.holder = rig
        .root
        .join("missing-holder.exe")
        .to_str()
        .unwrap()
        .as_bytes()
        .to_vec();
    assert!(matches!(
        request(&mut conn, &Request::Upgrade(missing)),
        Reply::Failed(_)
    ));
    rig.check(&mut conn, "after-missing-replacement");
}
