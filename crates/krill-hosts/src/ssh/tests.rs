use super::*;

#[test]
fn host_validation_rejects_flags_paths_and_blanks() {
    for good in ["mac-studio", "mac-studio.local", "owen@mac", "u@h-1.local"] {
        assert!(valid_host(good), "{good}");
    }
    for bad in [
        "",
        "-N",
        "--host",
        "a b",
        "a/b",
        "a:b",
        "a\\b",
        "@h",
        "u@",
        "u@h@x",
        "한글호스트",
    ] {
        assert!(!valid_host(bad), "{bad}");
    }
}

#[test]
fn extract_host_stops_at_literal_separator() {
    let args = ["attach", "7", "--", "--host"];
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let (host, rest) = extract_host(&args).expect("parse");
    assert_eq!(host, None);
    assert_eq!(rest, args);

    let args = ["--host", "owen@mac", "send", "7", "--", "--host", "x"];
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let (host, rest) = extract_host(&args).expect("parse");
    assert_eq!(host.as_deref(), Some("owen@mac"));
    let rest: Vec<String> = rest
        .iter()
        .map(|s| s.to_str().unwrap_or("?").to_owned())
        .collect();
    assert_eq!(rest, ["send", "7", "--", "--host", "x"]);

    let dup = ["ls", "--host", "a", "--host", "b"];
    let dup: Vec<OsString> = dup.iter().map(OsString::from).collect();
    assert!(extract_host(&dup).is_err());
    let missing = ["ls", "--host"];
    let missing: Vec<OsString> = missing.iter().map(OsString::from).collect();
    assert!(extract_host(&missing).is_err());
}

#[test]
fn slug_is_filesafe_and_bounded() {
    assert_eq!(slug("owen@mac-studio.local"), "owen@mac-studio.local");
    assert!(slug(&"h".repeat(200)).len() <= HOST_SLUG_MAX);
}

#[test]
fn ssh_options_argv_builder_includes_port_key_and_stability_flags() {
    let opts = SshOptions {
        destination: "alice@mac-studio.local".to_owned(),
        port: Some(2222),
        identity_file: Some("~/.ssh/id_ed25519_work".to_owned()),
        remote_socket: Some("/Users/alice/custom/krilld.sock".to_owned()),
    };
    opts.validate().expect("valid options");

    let local = Path::new("/tmp/local-fwd.sock");
    let remote = Path::new("/Users/alice/custom/krilld.sock");
    let tunnel = opts.tunnel_args(local, remote);
    assert_eq!(
        tunnel,
        vec![
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ConnectTimeout=10",
            "-p",
            "2222",
            "-i",
            "~/.ssh/id_ed25519_work",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "StreamLocalBindUnlink=yes",
            "-T",
            "-L",
            "/tmp/local-fwd.sock:/Users/alice/custom/krilld.sock",
            "alice@mac-studio.local",
            "cat >/dev/null",
        ]
    );

    let home_query = opts.remote_home_args();
    assert_eq!(
        home_query,
        vec![
            "-o",
            "StrictHostKeyChecking=accept-new",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-o",
            "ConnectTimeout=5",
            "-p",
            "2222",
            "-i",
            "~/.ssh/id_ed25519_work",
            "-o",
            "BatchMode=yes",
            "alice@mac-studio.local",
            &remote_query(),
        ]
    );

    let test_argv = opts.test_args();
    assert!(test_argv.contains(&"-p".to_owned()));
    assert!(test_argv.contains(&"2222".to_owned()));
    assert!(test_argv.contains(&"-i".to_owned()));
    assert!(test_argv.contains(&"~/.ssh/id_ed25519_work".to_owned()));
    assert!(test_argv.contains(&"BatchMode=yes".to_owned()));

    let connect_argv = opts.connect_args();
    assert_eq!(connect_argv.first().map(String::as_str), Some("-t"));
    assert_eq!(
        connect_argv.last().map(String::as_str),
        Some("alice@mac-studio.local")
    );
}

#[test]
fn ssh_options_validation_rejects_option_injection() {
    let mut bad_port = SshOptions::new("owen@mac");
    bad_port.port = Some(0);
    assert!(bad_port.validate().is_err());

    let mut bad_key = SshOptions::new("owen@mac");
    bad_key.identity_file = Some("-oProxyCommand=sh".to_owned());
    assert!(bad_key.validate().is_err());

    let mut bad_key_nl = SshOptions::new("owen@mac");
    bad_key_nl.identity_file = Some("/tmp/key\n-oEvil".to_owned());
    assert!(bad_key_nl.validate().is_err());

    let mut bad_sock = SshOptions::new("owen@mac");
    bad_sock.remote_socket = Some("relative/krilld.sock".to_owned());
    assert!(bad_sock.validate().is_err());

    let mut bad_sock_colon = SshOptions::new("owen@mac");
    bad_sock_colon.remote_socket = Some("/tmp/evil: target/krilld.sock".to_owned());
    assert!(bad_sock_colon.validate().is_err());
}

#[test]
fn remote_socket_for_home_computes_remote_user_path() {
    let sock = remote_socket_for_home("/Users/remoteuser\n").expect("valid home");
    assert_eq!(
        sock,
        PathBuf::from(
            "/Users/remoteuser/Library/Application Support/dev.krill.app/run/krilld.sock"
        )
    );
    assert_eq!(
        remote_socket_for_home(
            "/Users/remoteuser\r\nLibrary/Application Support/dev.minikrill.app"
        )
        .unwrap(),
        PathBuf::from(
            "/Users/remoteuser/Library/Application Support/dev.minikrill.app/run/krilld.sock"
        )
    );
    assert_eq!(
        remote_socket_for_home("/home/ubuntu\n.local/share/dev.krill.app").unwrap(),
        PathBuf::from("/home/ubuntu/.local/share/dev.krill.app/run/krilld.sock")
    );
    // 모르는 둘째 줄은 새 경로로 본다.
    assert_eq!(
        remote_socket_for_home("/Users/remoteuser\nevil/../x").unwrap(),
        sock
    );
    assert!(remote_socket_for_home("").is_err());
    assert!(remote_socket_for_home("relative/home").is_err());
    assert!(remote_socket_for_home("/Users/bad:colon").is_err());
}

#[test]
fn resolve_host_options_reads_port_key_and_socket_from_hosts_toml() {
    let dir = std::env::temp_dir().join(format!("mk132-remote-opts-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("hosts.toml"),
        r#"
[[hosts]]
id = "ssh-office-mac"
label = "office-mac"
destination = "alice@10.0.0.42"
port = 2202
identity_file = "~/.ssh/id_office"
remote_socket = "/Users/alice/custom/krilld.sock"
"#,
    )
    .expect("write hosts.toml");

    let by_dest = resolve_host_options("alice@10.0.0.42", Some(&dir)).expect("by dest");
    assert_eq!(by_dest.destination, "alice@10.0.0.42");
    assert_eq!(by_dest.port, Some(2202));
    assert_eq!(by_dest.identity_file.as_deref(), Some("~/.ssh/id_office"));
    assert_eq!(
        by_dest.remote_socket.as_deref(),
        Some("/Users/alice/custom/krilld.sock")
    );

    let by_label = resolve_host_options("office-mac", Some(&dir)).expect("by label");
    assert_eq!(by_label, by_dest);

    let fallback = resolve_host_options("bob@10.0.0.99", Some(&dir)).expect("fallback");
    assert_eq!(fallback.destination, "bob@10.0.0.99");
    assert_eq!(fallback.port, None);
    assert_eq!(fallback.identity_file, None);
    assert_eq!(fallback.remote_socket, None);

    std::fs::remove_dir_all(&dir).ok();
}

/// MK-170 규칙 3: 원격 질의를 실제 `/bin/sh`로 돌려 소켓을 새 번들 → 옛 번들 → Linux 순으로 찾는지 본다.
/// 원격 맥은 이관 전(옛 holder가 옛 경로)이거나 구버전일 수 있다. Linux는 찾기 전에 늘 원격 krill로 깨우고,
/// krill이 없거나 실패하면 표식으로 원인을 알린다. macOS는 깨우지 않는다.
#[cfg(unix)]
#[test]
fn remote_query_prefers_new_socket_then_legacy() {
    let home = PathBuf::from(format!("/tmp/mk170-remote-{}", std::process::id()));
    std::fs::remove_dir_all(&home).ok();
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let sock = |bundle: &str| {
        home.join(format!(
            "Library/Application Support/{bundle}/run/krilld.sock"
        ))
    };
    let raw = || {
        let out = Command::new("/bin/sh")
            .args(["-c", &remote_query()])
            .env("HOME", &home)
            // 이 맥의 진짜 krill로 데몬을 깨우지 않는다(`/usr/local/bin`·Homebrew 경로 제외).
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let resolve = || remote_socket_for_home(&raw());
    let linux_os = cfg!(target_os = "linux");
    let linux = home.join(".local/share/dev.krill.app/run/krilld.sock");
    // krill이 없다: Linux는 원인 오류, macOS는 새 번들 경로(깨우지 않음).
    if linux_os {
        let err = resolve().unwrap_err().to_string();
        assert!(err.contains("not installed"), "{err}");
    } else {
        assert_eq!(resolve().unwrap(), sock("dev.krill.app"));
    }
    // 실패하는 krill: Linux는 오류 첫 줄을 그대로 알린다.
    let wake = home.join(".local/bin/krill");
    std::fs::create_dir_all(wake.parent().unwrap()).unwrap();
    let script = |body: &str| {
        std::fs::write(&wake, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&wake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    };
    script("echo 'version `GLIBC_2.34` not found' >&2; exit 1");
    if linux_os {
        let err = resolve().unwrap_err().to_string();
        assert!(err.contains("GLIBC_2.34"), "{err}");
    }
    // 깨우는 krill: 소켓 유무와 무관하게 Linux는 매번 부른다(holder만 죽은 원격의 재결합).
    script("echo \"$1\" >> \"$HOME/woke\"");
    let woke = || std::fs::read_to_string(home.join("woke")).unwrap_or_default();
    if linux_os {
        assert_eq!(resolve().unwrap(), linux, "no socket on Linux: Linux path");
        assert_eq!(woke(), "ls\n");
    } else {
        assert_eq!(resolve().unwrap(), sock("dev.krill.app"));
        assert_eq!(woke(), "", "macOS 원격은 깨우지 않는다");
    }
    std::fs::create_dir_all(linux.parent().unwrap()).unwrap();
    let _linux = UnixListener::bind(&linux).unwrap();
    assert_eq!(resolve().unwrap(), linux, "Linux remote");
    std::fs::create_dir_all(sock("dev.minikrill.app").parent().unwrap()).unwrap();
    let _old = UnixListener::bind(sock("dev.minikrill.app")).unwrap();
    assert_eq!(
        resolve().unwrap(),
        sock("dev.minikrill.app"),
        "legacy remote"
    );
    std::fs::create_dir_all(sock("dev.krill.app").parent().unwrap()).unwrap();
    let _new = UnixListener::bind(sock("dev.krill.app")).unwrap();
    assert_eq!(resolve().unwrap(), sock("dev.krill.app"), "new wins");
    if linux_os {
        assert_eq!(woke(), "ls\nls\nls\nls\n");
    }
    std::fs::remove_dir_all(&home).unwrap();
}

#[cfg(unix)]
const ORPHAN_DIR: &str = "KRILL_HOSTS_ORPHAN_DIR";

/// 터널을 연 프로세스가 kill -9로 죽어도 ssh가 고아로 남지 않는다: stdin 파이프가 닫혀 원격 명령(`cat`)이 끝난다.
/// 가짜 ssh는 진짜처럼 stdin을 원격 명령에 흘리고, 끝나면 표식을 남긴다.
#[cfg(unix)]
#[test]
fn tunnel_ssh_exits_when_its_owner_is_killed() {
    let dir = PathBuf::from(format!("/tmp/krill-orphan-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let mut owner = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ssh::tests::tunnel_owner_child",
            "--ignored",
            "--nocapture",
        ])
        .env(ORPHAN_DIR, &dir)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !dir.join("ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "owner never opened the tunnel"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    owner.kill().unwrap(); // SIGKILL: Drop은 돌지 않는다
    owner.wait().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !dir.join("ssh-exited").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "tunnel ssh outlived its owner"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
#[ignore = "tunnel_ssh_exits_when_its_owner_is_killed가 자식으로 실행한다"]
fn tunnel_owner_child() {
    let Some(dir) = std::env::var_os(ORPHAN_DIR).map(PathBuf::from) else {
        return;
    };
    let ssh = dir.join("ssh");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nprev=''\nfor a in \"$@\"; do [ \"$prev\" = -L ] && fwd=\"$a\"; prev=\"$a\"; done\n\
             : > \"${{fwd%%:*}}\"\ncat >/dev/null\ntouch '{}'\n",
            dir.join("ssh-exited").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let opts = SshOptions::new("owner@test");
    let _tunnel = Tunnel::start_at_with_program(
        &opts,
        dir.join("l.sock"),
        Path::new("/tmp/r.sock"),
        ssh.as_os_str(),
    )
    .unwrap();
    std::fs::write(dir.join("ready"), "").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(60));
}

#[cfg(unix)]
#[test]
fn sweep_removes_only_dead_forward_sockets() {
    let dir = std::env::temp_dir().join(format!("krill-sweep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dead = dir.join("fwd-host-1-0.sock");
    let live = dir.join("f-2-0.sock");
    let daemon_sock = dir.join("krilld.sock");
    drop(UnixListener::bind(&dead).unwrap());
    let _live = UnixListener::bind(&live).unwrap();
    drop(UnixListener::bind(&daemon_sock).unwrap());

    sweep_stale_forwards(&dir, Duration::from_secs(3600));
    assert!(dead.exists(), "fresh sockets are left alone");

    sweep_stale_forwards(&dir, Duration::ZERO);
    assert!(!dead.exists(), "refused forward socket is removed");
    assert!(live.exists(), "listening forward socket stays");
    assert!(
        daemon_sock.exists(),
        "non-forward sockets are never touched"
    );
    std::fs::remove_dir_all(&dir).ok();
}
