use super::*;

#[test]
fn host_validation_rejects_flags_paths_and_blanks() {
    for good in [
        "mac-studio",
        "mac-studio.local",
        "owen@mac",
        "u@h-1.local",
        "mac_studio",
        "owen.k@100.65.102.14",
    ] {
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
        "h;id",
        "h$(id)",
        "h`id`",
        "h|x",
        "h'x",
        "h\"x",
        "h%h",
        "h*",
        "u@-oProxyCommand=x",
        "-u@h",
        "u;id@h",
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
            "BatchMode=yes",
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
            "BatchMode=yes",
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
    assert!(!sock.to_str().unwrap().contains('\\'));
    assert_eq!(
        sock,
        PathBuf::from(
            "/Users/remoteuser/Library/Application Support/dev.krill.app/run/krilld.sock"
        )
    );
    assert_eq!(
        remote_socket_for_home("/Users/remoteuser\r\nLibrary/Application Support/dev.krill.app")
            .unwrap(),
        sock
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
    std::fs::create_dir_all(sock("dev.krill.app").parent().unwrap()).unwrap();
    let _new = UnixListener::bind(sock("dev.krill.app")).unwrap();
    assert_eq!(resolve().unwrap(), sock("dev.krill.app"), "new wins");
    if linux_os {
        assert_eq!(woke(), "ls\nls\nls\n");
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

#[cfg(unix)]
#[test]
fn sweep_removes_dead_control_master_sockets_and_keeps_live_or_self_ones() {
    let dir = std::env::temp_dir().join(format!("krill-sweep-ctrl-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let dead_ctrl = dir.join("c-999999-0");
    let dead_ctrl_tmp = dir.join("c-999999-0.AbCdEf12");
    let live_ctrl = dir.join("c-888888-0");
    let self_ctrl = dir.join(format!("c-{}-999", std::process::id()));
    drop(UnixListener::bind(&dead_ctrl).unwrap());
    drop(UnixListener::bind(&dead_ctrl_tmp).unwrap());
    let _live = UnixListener::bind(&live_ctrl).unwrap();
    drop(UnixListener::bind(&self_ctrl).unwrap());

    sweep_stale_forwards(&dir, Duration::from_secs(3600));
    assert!(dead_ctrl.exists(), "fresh control sockets are left alone");

    sweep_stale_forwards(&dir, Duration::ZERO);
    assert!(!dead_ctrl.exists(), "refused control socket is removed");
    assert!(
        !dead_ctrl_tmp.exists(),
        "refused temporary control bind socket is removed"
    );
    assert!(live_ctrl.exists(), "listening control socket stays");
    assert!(self_ctrl.exists(), "current-PID control socket stays");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn machine_key_reuses_recent_result_without_running_ssh() {
    let dest = "cache-probe.invalid";
    let key = (ssh_program(), dest.to_owned(), Some(2222));
    let cached = Some("user cached\nhostname host\nport 1\n".to_owned());
    SSH_CONFIGS.lock().unwrap().insert(
        key.clone(),
        (Instant::now(), ssh_config_mtime(), cached.clone()),
    );
    assert_eq!(machine_key(dest, Some(2222)), "cached@host:1");
    // 기한이 지난 항목은 쓰지 않는다(이 대상은 `ssh -G`가 실패해도 입력 그대로가 나온다).
    // 부팅 직후라 기한 전 시각을 만들 수 없으면 이 확인은 건너뛴다.
    let Some(stale) = Instant::now().checked_sub(MACHINE_KEY_TTL) else {
        return;
    };
    SSH_CONFIGS
        .lock()
        .unwrap()
        .insert(key, (stale, ssh_config_mtime(), cached));
    assert_ne!(machine_key(dest, Some(2222)), "cached@host:1");
}

#[test]
fn host_key_checking_defers_to_an_explicit_user_setting() {
    let argv = |dest: &str, value: &str| {
        SSH_CONFIGS.lock().unwrap().insert(
            (ssh_program(), dest.to_owned(), None),
            (
                Instant::now(),
                ssh_config_mtime(),
                Some(format!(
                    "user u\nhostname {dest}\nstricthostkeychecking {value}\n"
                )),
            ),
        );
        SshOptions::new(dest).tunnel_args(Path::new("/tmp/l.sock"), Path::new("/r.sock"))
    };
    let accept_new = "StrictHostKeyChecking=accept-new".to_owned();
    for (dest, value) in [("hk-strict.invalid", "true"), ("hk-off.invalid", "false")] {
        let args = argv(dest, value);
        assert!(!args.contains(&accept_new), "{value}: {args:?}");
        assert!(args.contains(&"BatchMode=yes".to_owned()));
    }
    assert!(argv("hk-default.invalid", "ask").contains(&accept_new));
}

#[test]
fn machine_key_reads_user_host_port_from_ssh_g() {
    let cfg = "user owen\nhostname 100.65.102.14\nport 22\nidentityfile ~/.ssh/id\n";
    assert_eq!(
        machine_key_from_config(cfg).as_deref(),
        Some("owen@100.65.102.14:22")
    );
    assert_eq!(machine_key_from_config("user owen\n"), None);
    // 플래그처럼 보이는 대상은 ssh에 넘기지 않고 그대로 쓴다.
    assert_eq!(machine_key("-oProxyCommand=x", None), "-oproxycommand=x");
}

#[cfg(unix)]
#[test]
fn bounded_home_and_machine_queries_use_injected_local_program() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = PathBuf::from(format!("/tmp/krill-ssh-query-cap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("fake-ssh");
    std::fs::write(&program, "#!/bin/sh\nif [ \"$1\" = -G ]; then printf 'user local\\nhostname fixture.test\\nport 22\\n'; else printf '/Users/fixture\\n'; fi\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let opts = SshOptions::new("fixture-host");
    assert_eq!(
        query_remote_home_with_program(&opts, program.as_os_str()).unwrap(),
        "/Users/fixture\n"
    );
    assert_eq!(
        machine_key_with_program("fixture-host", None, program.as_os_str()),
        "local@fixture.test:22"
    );
    std::fs::write(&program, "#!/bin/sh\nhead -c 1048577 /dev/zero\n").unwrap();
    assert_eq!(
        query_remote_home_with_program(&opts, program.as_os_str())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(
        machine_key_with_program("fixture-host", None, program.as_os_str()),
        "fixture-host"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn valid_local_socket_and_start_at_reject_colon_and_control_chars() {
    #[cfg(unix)]
    assert!(!valid_local_socket(Path::new("/tmp/a:b.sock")));
    assert!(!valid_local_socket(Path::new("")));
    assert!(!valid_local_socket(Path::new("/tmp/bad\nname.sock")));
    assert!(valid_local_socket(Path::new("/tmp/good.sock")));

    #[cfg(unix)]
    {
        let opts = SshOptions::new("fixture-host");
        let err = Tunnel::start_at_with_program(
            &opts,
            PathBuf::from("/tmp/bad:local.sock"),
            Path::new("/remote/krilld.sock"),
            std::ffi::OsStr::new("ssh"),
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let err2 = Tunnel::start_at_with_program(
            &opts,
            PathBuf::from("/tmp/good-local.sock"),
            Path::new("/remote/bad\n.sock"),
            std::ffi::OsStr::new("ssh"),
        )
        .unwrap_err();
        assert_eq!(err2.kind(), io::ErrorKind::InvalidInput);

        for bad_remote in [
            "/remote/krilld.sock\n",
            "\n/remote/krilld.sock",
            "\t/remote/krilld.sock",
            " /remote/krilld.sock",
            "/remote/krilld.sock ",
        ] {
            assert!(!valid_remote_socket(bad_remote), "remote={bad_remote:?}");
            let err = Tunnel::start_at_with_program(
                &opts,
                PathBuf::from("/tmp/good-local.sock"),
                Path::new(bad_remote),
                std::ffi::OsStr::new("ssh"),
            )
            .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad_remote:?}");
        }
    }
}

#[test]
fn resolve_host_options_and_extract_host_reject_empty_host_with_unlabeled_entry() {
    let dir = std::env::temp_dir().join(format!("krill-r3p06-empty-host-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("hosts.toml"),
        "[[hosts]]\ndestination = \"prod@server\"\n",
    )
    .unwrap();

    for blank in ["", "   ", "\t"] {
        let err = resolve_host_options(blank, Some(&dir)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "blank={blank:?}");
    }
    let matched = resolve_host_options("prod@server", Some(&dir)).unwrap();
    assert_eq!(matched.destination, "prod@server");

    let empty_args: Vec<OsString> = ["--host", "", "ls"]
        .into_iter()
        .map(OsString::from)
        .collect();
    assert!(extract_host(&empty_args).is_err());
    let ws_args: Vec<OsString> = ["--host", "   ", "ls"]
        .into_iter()
        .map(OsString::from)
        .collect();
    assert!(extract_host(&ws_args).is_err());

    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(unix)]
#[test]
fn effective_config_fails_closed_does_not_cache_none_and_bypasses_remote_jobs_limit() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = std::env::temp_dir().join(format!("krill-r3p04-ssh-g-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();

    // 1. Fail-closed when ssh -G fails (cannot resolve effective config): must NOT inject accept-new.
    let accept_new = "StrictHostKeyChecking=accept-new".to_owned();
    let opts = SshOptions::new("fail-closed.invalid");
    assert!(
        !opts.common_args_with_config(10, None).contains(&accept_new),
        "common_args must fail closed when effective ssh config is unavailable"
    );
    assert!(
        opts.common_args_with_config(10, Some("stricthostkeychecking ask\n"))
            .contains(&accept_new)
    );
    assert!(
        !opts
            .common_args_with_config(10, Some("stricthostkeychecking true\n"))
            .contains(&accept_new)
    );

    let fail_dest = format!("fail-closed-{}.invalid", std::process::id());
    let fail_ssh = dir.join("fail-ssh");
    std::fs::write(&fail_ssh, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&fail_ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(effective_config_with_program(&fail_dest, None, fail_ssh.as_os_str()).is_none());
    // effective_config returning None must never populate SSH_CONFIGS.
    let probe_dest = format!("no-cache-none-{}.invalid", std::process::id());
    let key = (ssh_program(), probe_dest.clone(), None);
    SSH_CONFIGS.lock().unwrap().remove(&key);
    assert!(effective_config_with_program(&probe_dest, None, fail_ssh.as_os_str()).is_none());
    assert!(!SSH_CONFIGS.lock().unwrap().contains_key(&key));

    // 2. While 4 remote SSH workers saturate managed::JOBS, local `ssh -G` must still succeed.
    let sleeper = dir.join("sleep-ssh");
    let release = dir.join("release");
    std::fs::write(
        &sleeper,
        format!(
            "#!/bin/sh\ntouch \"$1\"\nwhile [ ! -f '{}' ]; do sleep 0.02; done\n",
            release.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&sleeper, std::fs::Permissions::from_mode(0o700)).unwrap();

    let fast_ssh = dir.join("fast-ssh-g");
    std::fs::write(
        &fast_ssh,
        "#!/bin/sh\nif [ \"$1\" = -G ]; then printf 'user u\\nhostname h\\nstricthostkeychecking true\\n'; exit 0; fi\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&fast_ssh, std::fs::Permissions::from_mode(0o700)).unwrap();

    let mut handles = Vec::new();
    for i in 0..4 {
        let marker = dir.join(format!("slot-{i}"));
        let prog = sleeper.clone().into_os_string();
        handles.push(std::thread::spawn(move || {
            crate::managed::run_piped_ssh_with_timeout(
                &prog,
                &[marker.to_string_lossy().into_owned()],
                None,
                Duration::from_secs(10),
            )
        }));
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while (0..4).any(|i| !dir.join(format!("slot-{i}")).exists()) {
        assert!(
            Instant::now() < deadline,
            "4 remote SSH workers did not start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // Confirm a 5th remote job is rejected by JOBS, while local `ssh -G` succeeds immediately.
    let fifth = crate::managed::run_piped_ssh_with_timeout(
        fast_ssh.as_os_str(),
        &["-G".to_owned(), "host".to_owned()],
        None,
        Duration::from_secs(2),
    );
    assert_eq!(fifth.unwrap_err().kind(), io::ErrorKind::WouldBlock);

    let cfg = effective_config_with_program("host", None, fast_ssh.as_os_str())
        .expect("local ssh -G must bypass remote JOBS semaphore");
    assert!(user_sets_host_key_checking(&cfg));

    std::fs::write(&release, "").unwrap();
    for h in handles {
        h.join().unwrap().unwrap();
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn ssh_auth_failure_classifier_distinguishes_auth_from_transient_network_errors() {
    for auth in [
        "alice@box: Permission denied (publickey).",
        "Permission denied (publickey,password).",
        "Permission denied (keyboard-interactive).",
        "Permission denied, please try again.",
        "ssh: Authentication failed for host",
        "Received disconnect from 10.0.0.5: Too many authentication failures",
        "No more authentication methods to try.",
        "sign_and_send_pubkey: signing failed for ED25519: agent refused operation",
        "The agent has no identities.",
        "Could not open a connection to your authentication agent.",
        "Enter passphrase for key '/Users/alice/.ssh/id_ed25519':",
        "Load key \"/Users/alice/.ssh/id_ed25519\": error in libcrypto",
        "Host key verification failed.",
    ] {
        assert!(is_ssh_auth_failure(auth), "must classify as auth: {auth}");
    }

    for transient in [
        "ssh: connect to host 10.0.0.5 port 22: Operation timed out",
        "ssh: connect to host 10.0.0.5 port 22: Connection refused",
        "ssh: connect to host 10.0.0.5 port 22: No route to host",
        "ssh: connect to host 10.0.0.5 port 22: Network is unreachable",
        "ssh: Could not resolve hostname box.invalid: nodename nor servname provided",
        "Connection reset by peer",
        "Broken pipe",
        "unexpected EOF",
        "mkdir: /opt/krill: Permission denied",
    ] {
        assert!(
            !is_ssh_auth_failure(transient),
            "must NOT classify as auth: {transient}"
        );
    }
}

#[test]
fn parse_remote_query_linger_reads_lines_after_socket_marker() {
    use crate::managed::LingerStatus;
    let disabled = "/home/ubuntu\n.local/share/dev.krill.app\nkrill-user=ubuntu\nkrill-linger=no\n";
    assert_eq!(
        remote_socket_for_home(disabled).unwrap(),
        PathBuf::from("/home/ubuntu/.local/share/dev.krill.app/run/krilld.sock")
    );
    assert_eq!(
        parse_remote_query_linger(disabled),
        LingerStatus::Disabled {
            user: "ubuntu".into()
        }
    );

    let enabled = "/home/ubuntu\n.local/share/dev.krill.app\nkrill-user=ubuntu\nkrill-linger=yes\n";
    assert_eq!(parse_remote_query_linger(enabled), LingerStatus::Enabled);

    let mac = "/Users/alice\nLibrary/Application Support/dev.krill.app\n";
    assert_eq!(parse_remote_query_linger(mac), LingerStatus::NotApplicable);

    let no_systemd =
        "/home/ubuntu\n.local/share/dev.krill.app\nkrill-user=ubuntu\nkrill-linger=unavailable\n";
    assert_eq!(
        parse_remote_query_linger(no_systemd),
        LingerStatus::NotApplicable
    );
}

#[cfg(unix)]
#[test]
fn tunnel_start_captures_stderr_on_auth_failure() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = PathBuf::from(format!("/tmp/krill-ssh-stderr-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("fake-ssh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'alice@box: Permission denied (publickey).' >&2\nexit 255\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let opts = SshOptions::new("alice@box");
    let err = Tunnel::start_at_with_program(
        &opts,
        dir.join("l.sock"),
        Path::new("/tmp/r.sock"),
        script.as_os_str(),
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Permission denied (publickey)"),
        "stderr lost: {msg}"
    );
    assert!(is_ssh_auth_failure(&msg));
    std::fs::remove_dir_all(&dir).ok();
}
