use super::*;

#[test]
fn classify_reads_ready_probe() {
    let probe = classify(
        "krill-os=windows\nkrill-arch=x86_64\nkrill-version=0.1.44+abc\r\nkrill-bridge=ok\r\n",
        "",
        Some(0),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        probe,
        Probe {
            os: "windows".to_owned(),
            arch: "x86_64".to_owned(),
            version: "0.1.44+abc".to_owned(),
        }
    );
}

#[test]
fn classify_explains_windows_failures_and_ignores_posix_hosts() {
    let stopped = classify("krill-bridge-error=No such file\n", "", Some(1)).unwrap_err();
    assert_eq!(stopped.kind(), io::ErrorKind::ConnectionRefused);
    assert!(
        stopped.to_string().contains("open Krill there"),
        "{stopped}"
    );

    let old = classify("", "usage: krill mobile pair/devices …", Some(2)).unwrap_err();
    assert!(old.to_string().contains("too old"), "{old}");

    let missing = classify("", "'krill' is not recognized", Some(CMD_NOT_FOUND)).unwrap_err();
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);

    // POSIX 원격에서 krill이 없을 때(127)는 중계 대상이 아니다. 호출자가 원래 오류를 쓴다.
    assert!(
        classify("", "sh: krill: not found", Some(127))
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn reached_remote_separates_ssh_failure_from_remote_command_failure() {
    use std::os::unix::process::ExitStatusExt as _;
    let code = |c: i32| std::process::ExitStatus::from_raw(c << 8);
    assert!(reached_remote(code(1)));
    assert!(!reached_remote(code(SSH_FAILED)));
    assert!(!reached_remote(code(0)));
    // 신호로 끝난 ssh는 원격에 닿았는지 모른다.
    assert!(!reached_remote(std::process::ExitStatus::from_raw(9)));
}

#[cfg(unix)]
#[test]
fn relay_args_share_quoted_control_master() {
    let mut opts = SshOptions::new("user@win-pc");
    opts.port = Some(2222);
    let control = std::path::Path::new("/Users/a b/Application Support/run/c-1-0");
    let keepalive = keepalive_args(&opts, Some(control));
    let relay = relay_args(&opts, Some(control));
    let quoted = "ControlPath=\"/Users/a b/Application Support/run/c-1-0\"".to_owned();
    assert!(keepalive.contains(&"ControlMaster=yes".to_owned()));
    assert!(keepalive.contains(&"ControlPersist=no".to_owned()));
    assert!(keepalive.contains(&quoted));
    assert_eq!(
        keepalive.last().map(String::as_str),
        Some("krill __bridge --probe")
    );
    assert!(relay.contains(&"ControlMaster=no".to_owned()));
    assert!(relay.contains(&quoted));
    assert_eq!(relay.last().map(String::as_str), Some("krill __bridge"));
    assert!(relay.contains(&"2222".to_owned()));
    assert_eq!(
        control_option(std::path::Path::new("/tmp/100%/c")),
        "ControlPath=\"/tmp/100%%/c\""
    );
    // 다중화를 못 하면(경로가 길면) 연결마다 그냥 인증한다.
    assert!(
        !relay_args(&opts, None)
            .iter()
            .any(|a| a.starts_with("Control"))
    );
}

#[cfg(unix)]
#[test]
fn control_path_leaves_room_for_ssh_temporary_suffix() {
    let short = control_path(std::path::Path::new("/tmp/run/fwd-x.sock")).unwrap();
    assert!(short.starts_with("/tmp/run"));
    let long_dir = format!("/{}/fwd-x.sock", "d".repeat(daemon::SUN_PATH_MAX - 20));
    assert!(control_path(std::path::Path::new(&long_dir)).is_none());
}
