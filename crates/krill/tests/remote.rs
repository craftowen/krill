//! MK-099: 원격 krilld를 SSH 소켓 포워딩으로 조작한다. 실제 외부 SSH 대신 가짜 포워더
//! (`KRILL_SSH_BIN`이 `krill __forward`로 unix 소켓 프록시를 띄우는 셸 스크립트)로 결정론적으로 검증한다.
//!
//! - 전 명령(ls/new/send/read/wait/attach)이 포워딩 소켓으로 간다.
//! - 호출마다 터널이 맺어지고 끊겨도(포워더 교체·실패 후 복구) 원격 세션·출력이 유지된다.
//! - 포워더 실패가 로컬 데몬 기동으로 조용히 대체되지 않는다.
//! - 새 IPC 비트 없이 기존 Hello·화면·입력 프로토콜만 쓴다.
//!
//! 재현: `cargo nextest run -p krill --test remote` (자원 제한: `CARGO_BUILD_JOBS=2 KRILL_TEST_SLOTS=1 NEXTEST_TEST_THREADS=4`).

#![cfg_attr(unix, expect(clippy::unwrap_used, reason = "integration test helpers"))]
#![cfg(unix)]

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use krill_proto::control::{Reply, Request};
use krill_proto::daemon::{client_hello, handshake};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;

const MK: &str = env!("CARGO_BIN_EXE_krill");
const WAIT: Duration = Duration::from_secs(10);
const HOST: &str = "test-remote";

/// "원격 맥"의 holder·krilld와, 로컬 측 포워딩 환경(가짜 ssh·독립 `KRILL_STATE_DIR`).
struct Remote {
    holder: Child,
    krilld: u32,
    daemon_dir: PathBuf,
    local_dir: PathBuf,
    ssh: PathBuf,
    fail_ssh: PathBuf,
}

impl Remote {
    fn new(tag: &str) -> Self {
        // UDS 경로 104바이트 제한 안에 들도록 디렉터리명을 짧게 둔다(macOS TMPDIR이 길다).
        let base = std::env::temp_dir().join(format!("m99-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&base).ok();
        let daemon_dir = base.join("r");
        let local_dir = base.join("l");
        fs::create_dir_all(&daemon_dir).unwrap();
        fs::create_dir_all(&local_dir).unwrap();
        let bin = Path::new(MK).parent().unwrap();
        let socket = daemon_dir.join("run/krilld.sock");
        let holder = Command::new(bin.join("krill-holder"))
            .arg(bin.join("krilld"))
            .arg(&socket)
            .env("KRILL_STATE_DIR", &daemon_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + WAIT;
        let krilld = loop {
            if let Ok(c) = handshake(&socket, &client_hello(ClientRole::Cli, "mk099"), WAIT) {
                break u32::try_from(c.server.instance_id).unwrap();
            }
            assert!(Instant::now() < deadline, "remote daemon startup timed out");
            thread::sleep(Duration::from_millis(20));
        };
        // 가짜 ssh: `-L <local>:<remote>`를 받아 `krill __forward`로 프록시한다.
        // 진짜 ssh와 같은 argv 모양이라 `-o ExitOnForwardFailure=yes -N -L` 전달을 검증한다.
        let ssh = local_dir.join("fake-ssh.sh");
        fs::write(
            &ssh,
            format!(
                "#!/bin/sh\n\
                 fwd=\"\"\n\
                 prev=\"\"\n\
                 for a in \"$@\"; do\n\
                   if [ \"$prev\" = \"-L\" ]; then fwd=\"$a\"; fi\n\
                   prev=\"$a\"\n\
                 done\n\
                 echo \"$@\" >>\"$MK099_LOG\"\n\
                 local=\"${{fwd%%:*}}\"\n\
                 remote=\"${{fwd#*:}}\"\n\
                 if [ -z \"$fwd\" ] || [ \"$local\" = \"$fwd\" ]; then exit 2; fi\n\
                 exec '{MK}' __forward \"$local\" \"$remote\"\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
        let fail_ssh = local_dir.join("fail-ssh.sh");
        fs::write(&fail_ssh, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&fail_ssh, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            holder,
            krilld,
            daemon_dir,
            local_dir,
            ssh,
            fail_ssh,
        }
    }

    fn remote_socket(&self) -> PathBuf {
        self.daemon_dir.join("run/krilld.sock")
    }

    fn ssh_log(&self) -> PathBuf {
        self.local_dir.join("ssh-args.log")
    }

    fn run_with(&self, ssh: &Path, args: &[&str]) -> Output {
        Command::new(MK)
            .args(args)
            .env("KRILL_STATE_DIR", &self.local_dir)
            .env("KRILL_SSH_BIN", ssh)
            .env("KRILL_REMOTE_SOCK", self.remote_socket())
            .env("MK099_LOG", self.ssh_log())
            .output()
            .unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        let ssh = self.ssh.clone();
        self.run_with(&ssh, args)
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn wait_text(&self, id: &str, text: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let out = self.ok(&["--host", HOST, "read", id]);
            if out.contains(text) {
                return out;
            }
            assert!(Instant::now() < deadline, "missing {text:?}: {out:?}");
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// 원격 데몬에 직접 물어 세션이 살아 있는지 확인한다(포워더를 거치지 않음).
    fn remote_sessions(&self) -> usize {
        let mut c = handshake(
            &self.remote_socket(),
            &client_hello(ClientRole::Cli, "mk099-direct"),
            WAIT,
        )
        .unwrap();
        c.stream
            .write_all(&Request::List.encode().unwrap())
            .unwrap();
        let (kind, body) = ipc::read_frame(&mut c.stream).unwrap().unwrap();
        let reply = Reply::decode(kind, &body).unwrap();
        assert!(
            matches!(reply, Reply::Sessions(_)),
            "unexpected reply: {reply:?}"
        );
        match reply {
            Reply::Sessions(sessions) => sessions.len(),
            _ => 0,
        }
    }

    fn ssh_argv(&self) -> String {
        fs::read_to_string(self.ssh_log()).unwrap_or_default()
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        // 세션을 먼저 닫아야 `exec sleep` 같은 자식이 고아로 남지 않는다.
        if let Ok(mut c) = handshake(
            &self.remote_socket(),
            &client_hello(ClientRole::Cli, "mk099-cleanup"),
            WAIT,
        ) {
            c.stream.write_all(&Request::List.encode().unwrap()).ok();
            if let Ok(Some((kind, body))) = ipc::read_frame(&mut c.stream)
                && let Ok(Reply::Sessions(sessions)) = Reply::decode(kind, &body)
            {
                for info in sessions {
                    c.stream
                        .write_all(
                            &Request::Close {
                                session: info.session,
                            }
                            .encode()
                            .unwrap(),
                        )
                        .ok();
                    ipc::read_frame(&mut c.stream).ok();
                }
                thread::sleep(Duration::from_millis(150));
            }
        }
        self.holder.kill().ok();
        self.holder.wait().ok();
        Command::new("/bin/kill")
            .args(["-9", &self.krilld.to_string()])
            .status()
            .ok();
        fs::remove_dir_all(self.local_dir.parent().unwrap()).ok();
    }
}

#[test]
fn remote_commands_use_forwarded_socket() {
    let r = Remote::new("c");
    let id = r.ok(&[
        "--host",
        HOST,
        "new",
        "--cwd",
        r.daemon_dir.to_str().unwrap(),
        "--",
        "/bin/sh",
        "-c",
        "stty -echo; printf 'remote-ready\\n'; IFS= read -r line; printf 'got:%s\\n' \"$line\"",
    ]);
    let id = id.trim();
    // 진짜 ssh argv 모양으로 포워더가 불렸는지 확인한다.
    let argv = r.ssh_argv();
    assert_eq!(
        argv.lines().count(),
        1,
        "explicit KRILL_REMOTE_SOCK skips HOME query: {argv}"
    );
    assert!(!argv.contains("$HOME"), "{argv}");
    assert!(
        argv.contains("-o ServerAliveInterval=15")
            && argv.contains("-o ServerAliveCountMax=3")
            && argv.contains("-o ConnectTimeout=10")
            && argv.contains("-o ExitOnForwardFailure=yes")
            && argv.contains("-o StreamLocalBindUnlink=yes")
            && argv.contains("-T")
            && argv.contains("-L")
            && argv.trim_end().ends_with("cat >/dev/null")
            && argv.contains(HOST),
        "unexpected ssh argv: {argv}"
    );
    // 로컬 데몬은 절대 뜨지 않는다. 포워더 실패가 로컬로 대체되면 krilld.sock이 생긴다.
    assert!(
        !r.local_dir.join("run/krilld.sock").exists(),
        "a local daemon was launched for --host"
    );
    // 명령 뒤에는 터널 소켓도 치워진다(프로세스 누수 없음).
    assert!(
        fs::read_dir(r.local_dir.join("run")).map_or(true, |mut d| d.next().is_none()),
        "forward socket leaked"
    );
    r.wait_text(id, "remote-ready");
    assert!(r.ok(&["--host", HOST, "ls"]).contains(id));
    // 뒤쪽 --host 형태도 받는다.
    r.ok([
        "wait",
        id,
        "--for",
        "pattern:remote-ready",
        "--timeout",
        "5",
        "--host",
        HOST,
    ]
    .as_slice());
    r.ok(&["--host", HOST, "send", id, "--enter", "--", "hello-remote"]);
    r.ok(&["--host", HOST, "wait", id, "--timeout", "5"]);
    let text = r.wait_text(id, "got:hello-remote");
    assert!(text.contains("got:hello-remote"), "{text}");
    assert_eq!(r.remote_sessions(), 1);
}

#[test]
fn remote_uses_hosts_toml_port_key_and_queries_remote_home() {
    let r = Remote::new("h");
    fs::write(
        r.local_dir.join("hosts.toml"),
        r#"
[[hosts]]
id = "ssh-studio"
label = "studio"
destination = "diffuser@test-remote"
port = 2222
identity_file = "~/.ssh/id_studio"
"#,
    )
    .unwrap();

    // KRILL_REMOTE_SOCK 없이 원격 질의(`remote_query`)와 `-L` 포워딩을 모두 처리하는 가짜 ssh. 질의는 원격 로그인
    // 셸처럼 실제 `/bin/sh`로 가짜 원격 HOME에서 돌린다(MK-170 규칙 3: 새 번들 → 옛 번들 소켓 순).
    // 포워더가 이 경로로 connect하므로 UDS 104바이트 안에 들게 `/tmp` 아래에 둔다.
    let home = PathBuf::from(format!("/tmp/m99h-{}", std::process::id()));
    fs::remove_dir_all(&home).ok();
    let sock = |bundle: &str| {
        home.join(format!(
            "Library/Application Support/{bundle}/run/krilld.sock"
        ))
    };
    let home_ssh = r.local_dir.join("home-ssh.sh");
    fs::write(
        &home_ssh,
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >>\"$MK099_LOG\"\n\
             fwd=\"\"\n\
             prev=\"\"\n\
             last=\"\"\n\
             for a in \"$@\"; do\n\
               if [ \"$prev\" = \"-L\" ]; then fwd=\"$a\"; fi\n\
               prev=\"$a\"\n\
               last=\"$a\"\n\
             done\n\
             if [ -z \"$fwd\" ]; then\n\
               HOME='{}' exec /bin/sh -c \"$last\"\n\
             fi\n\
             local=\"${{fwd%%:*}}\"\n\
             remote=\"${{fwd#*:}}\"\n\
             if [ \"$remote\" != \"$EXPECT_REMOTE\" ]; then\n\
               echo \"unexpected remote socket: $remote\" >&2\n\
               exit 3\n\
             fi\n\
             exec '{MK}' __forward \"$local\" \"$remote\"\n",
            home.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&home_ssh, fs::Permissions::from_mode(0o755)).unwrap();

    // 구버전(또는 이관 전) 원격: 옛 번들 경로에만 소켓이 있다. 다음에는 새 경로가 이긴다.
    for bundle in ["dev.minikrill.app", "dev.krill.app"] {
        fs::create_dir_all(sock(bundle).parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(r.remote_socket(), sock(bundle)).unwrap();
        fs::remove_file(r.ssh_log()).ok();
        let out = Command::new(MK)
            .args(["--host", "studio", "ls"])
            .env("KRILL_STATE_DIR", &r.local_dir)
            .env("KRILL_SSH_BIN", &home_ssh)
            .env_remove("KRILL_REMOTE_SOCK")
            .env("EXPECT_REMOTE", sock(bundle))
            .env("MK099_LOG", r.ssh_log())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{bundle} stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let log = r.ssh_argv();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2, "expected HOME query + tunnel spawn: {log}");
        assert!(
            lines[0].contains("-p 2222")
                && lines[0].contains("-i ~/.ssh/id_studio")
                && lines[0].contains("diffuser@test-remote")
                && lines[0].contains(&krill_hosts::ssh::remote_query()),
            "unexpected HOME query argv: {}",
            lines[0]
        );
        assert!(
            lines[1].contains("-p 2222")
                && lines[1].contains("-i ~/.ssh/id_studio")
                && lines[1].contains("-o ExitOnForwardFailure=yes")
                && lines[1].contains("-o StreamLocalBindUnlink=yes")
                && lines[1].contains(&format!(":{}", sock(bundle).display()))
                && lines[1].contains("diffuser@test-remote"),
            "unexpected tunnel argv: {}",
            lines[1]
        );
    }
    fs::remove_dir_all(&home).ok();
}

#[test]
fn remote_session_survives_forwarder_outage_and_reconnect() {
    let r = Remote::new("o");
    let id = r.ok(&[
        "--host",
        HOST,
        "new",
        "--cwd",
        r.daemon_dir.to_str().unwrap(),
        "--",
        "/bin/sh",
        "-c",
        "stty -echo; printf 'before-outage\\n'; IFS= read -r line; printf 'after:%s\\n' \"$line\"",
    ]);
    let id = id.trim();
    r.wait_text(id, "before-outage");
    // 호출마다 터널이 새로 맺어진다: 두 번째 호출도 같은 세션을 본다.
    assert!(
        r.ok(&["--host", HOST, "read", id])
            .contains("before-outage")
    );
    // 네트워크 끊김: 포워더가 죽으면 실패로 드러나고 로컬 데몬으로 대체하지 않는다.
    let fail = r.fail_ssh.clone();
    let out = r.run_with(&fail, &["--host", HOST, "read", id]);
    assert!(!out.status.success(), "forward failure was hidden");
    assert!(
        !r.local_dir.join("run/krilld.sock").exists(),
        "forward failure launched a local daemon"
    );
    // 그 사이 원격 세션은 그대로 살아 있다.
    assert_eq!(r.remote_sessions(), 1);
    // 포워더 복구: 다음 호출이 터널을 다시 맺고 세션·출력이 이어진다.
    r.ok(&["--host", HOST, "send", id, "--enter", "--", "reconnected"]);
    r.ok(&["--host", HOST, "wait", id, "--timeout", "5"]);
    let text = r.wait_text(id, "after:reconnected");
    assert!(text.contains("before-outage"), "{text}");
    assert_eq!(r.remote_sessions(), 1);
}

#[test]
fn remote_attach_detaches_over_forward() {
    let r = Remote::new("a");
    let id = r.ok(&[
        "--host",
        HOST,
        "new",
        "--cwd",
        r.daemon_dir.to_str().unwrap(),
        "--",
        "/bin/sh",
        "-c",
        "stty -echo; printf 'attach-marker\\n'; exec sleep 600",
    ]);
    let id = id.trim().to_owned();
    let mut child = Command::new(MK)
        .args(["--host", HOST, "attach", &id])
        .env("KRILL_STATE_DIR", &r.local_dir)
        .env("KRILL_SSH_BIN", &r.ssh)
        .env("KRILL_REMOTE_SOCK", r.remote_socket())
        .env("MK099_LOG", r.ssh_log())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut seen = Vec::new();
    let deadline = Instant::now() + WAIT;
    loop {
        let mut buf = [0u8; 4096];
        let n = stdout.read(&mut buf).unwrap();
        seen.extend_from_slice(&buf[..n]);
        if String::from_utf8_lossy(&seen).contains("attach-marker") {
            break;
        }
        assert!(Instant::now() < deadline, "attach showed no remote output");
    }
    // Ctrl-] 하나로 떼고 나간다. 세션은 원격에 남는다.
    child.stdin.take().unwrap().write_all(b"\x1d").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(r.remote_sessions(), 1);
    assert!(
        r.ok(&["--host", HOST, "read", &id])
            .contains("attach-marker")
    );
}

#[test]
fn remote_new_rejects_relative_cwd_and_worktree() {
    let r = Remote::new("l");
    assert_eq!(
        r.run(&["--host", HOST, "new", "--", "/bin/sh"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        r.run(&[
            "--host",
            HOST,
            "new",
            "--cwd",
            "relative/path",
            "--",
            "/bin/sh"
        ])
        .status
        .code(),
        Some(2)
    );
    // OWE-920부터 원격 --worktree는 원격 데몬에 워크트리 생성을 요청한다. 저장소가 아닌 --cwd면
    // 사용법 오류(2)가 아니라 실행 실패(1)이고, 세션을 만들지 않는다.
    assert_eq!(
        r.run(&[
            "--host",
            HOST,
            "new",
            "--cwd",
            r.daemon_dir.to_str().unwrap(),
            "--worktree",
            "wt",
            "--",
            "/bin/sh"
        ])
        .status
        .code(),
        Some(1)
    );
    assert_eq!(r.remote_sessions(), 0);
}

#[test]
fn remote_fork_with_prompt_delivers_to_remote_and_never_touches_local() {
    let r = Remote::new("f");
    fs::write(
        r.daemon_dir.join("agents.toml"),
        r#"
[agent.fork_probe]
name = "Fork probe"
launch = ["/bin/sh", "-c", "stty -echo; printf 'fork-probe-ready\n> '; IFS= read -r line; printf 'fork-probe-got:%s\n' \"$line\"; exec sleep 60"]
"#,
    )
    .unwrap();
    let orig = r.ok(&[
        "--host",
        HOST,
        "new",
        "--cwd",
        r.daemon_dir.to_str().unwrap(),
        "--agent",
        "fork_probe",
    ]);
    let orig = orig.trim();
    r.wait_text(orig, "fork-probe-ready");

    let forked_raw = r.ok(&[
        "--host",
        HOST,
        "fork",
        orig,
        "--prompt",
        "이어서 진행해",
        "--json",
    ]);
    let parsed: serde_json::Value = serde_json::from_str(&forked_raw).unwrap();
    let forked_id = parsed["session"].as_u64().unwrap().to_string();
    assert_eq!(parsed["from"].as_u64().unwrap().to_string().as_str(), orig);
    assert_ne!(forked_id.as_str(), orig);

    // 로컬 데몬은 절대 기동되거나 참조되지 않아야 한다(OWE-747).
    assert!(
        !r.local_dir.join("run/krilld.sock").exists(),
        "remote fork --prompt must not touch or start local daemon"
    );

    let forked_screen = r.wait_text(&forked_id, "fork-probe-got:이어서 진행해");
    assert!(
        forked_screen.contains("fork-probe-got:이어서 진행해"),
        "{forked_screen}"
    );
    let orig_screen = r.ok(&["--host", HOST, "read", orig]);
    assert!(
        !orig_screen.contains("fork-probe-got:"),
        "original session must not receive forked prompt: {orig_screen}"
    );
    assert_eq!(r.remote_sessions(), 2);
}
