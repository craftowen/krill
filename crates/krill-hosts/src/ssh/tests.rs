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
            "-N",
            "-L",
            "/tmp/local-fwd.sock:/Users/alice/custom/krilld.sock",
            "alice@mac-studio.local",
        ]
    );

    let home_query = opts.remote_home_args();
    assert_eq!(
        home_query,
        vec![
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
        remote_socket_for_home("/Users/remoteuser\r\ndev.minikrill.app").unwrap(),
        PathBuf::from(
            "/Users/remoteuser/Library/Application Support/dev.minikrill.app/run/krilld.sock"
        )
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

/// MK-170 규칙 3: 원격 질의를 실제 `/bin/sh`로 돌려 소켓을 새 번들 → 옛 번들 순으로 찾는지 본다.
/// 원격 맥은 이관 전(옛 holder가 옛 경로)이거나 구버전일 수 있다.
#[cfg(unix)]
#[test]
fn remote_query_prefers_new_socket_then_legacy() {
    let home = PathBuf::from(format!("/tmp/mk170-remote-{}", std::process::id()));
    std::fs::remove_dir_all(&home).ok();
    let sock = |bundle: &str| {
        home.join(format!(
            "Library/Application Support/{bundle}/run/krilld.sock"
        ))
    };
    let resolve = || {
        let out = Command::new("/bin/sh")
            .args(["-c", &remote_query()])
            .env("HOME", &home)
            .output()
            .unwrap();
        assert!(out.status.success());
        remote_socket_for_home(&String::from_utf8(out.stdout).unwrap()).unwrap()
    };
    assert_eq!(resolve(), sock("dev.krill.app"), "no socket: new path");
    std::fs::create_dir_all(sock("dev.minikrill.app").parent().unwrap()).unwrap();
    let _old = UnixListener::bind(sock("dev.minikrill.app")).unwrap();
    assert_eq!(resolve(), sock("dev.minikrill.app"), "legacy remote");
    std::fs::create_dir_all(sock("dev.krill.app").parent().unwrap()).unwrap();
    let _new = UnixListener::bind(sock("dev.krill.app")).unwrap();
    assert_eq!(resolve(), sock("dev.krill.app"), "new wins");
    std::fs::remove_dir_all(&home).unwrap();
}
