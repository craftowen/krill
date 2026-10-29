use super::{
    LoginFailReason, LoginOutcome, authorize_url, login_link, login_url, run_browser_login,
    run_browser_login_with_mode, take_login_outcome,
};
use krill_proto::usage::ProviderKind;
use std::path::Path;

#[test]
fn browser_login_accepts_only_https_provider_authorize_hosts() {
    for (provider, url) in [
        (
            ProviderKind::Claude,
            "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback",
        ),
        (
            ProviderKind::Claude,
            "https://console.anthropic.com/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2Fcallback",
        ),
        (
            ProviderKind::Claude,
            "https://claude.com/cai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback",
        ),
        (ProviderKind::Codex, "https://auth.openai.com/authorize"),
        (ProviderKind::Codex, "https://chatgpt.com/auth"),
        (
            ProviderKind::Antigravity,
            "https://accounts.google.com/o/oauth2/auth",
        ),
    ] {
        assert_eq!(authorize_url(provider, url), Some(url));
    }
    for url in [
        "http://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback",
        "https://claude.ai.evil.test/?redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback",
        "https://evil.test/?redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback",
        "https://user@claude.ai/?redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback",
        "https://claude.ai:8443/?redirect_uri=http%3A%2F%2Flocalhost%3A5000%2Fcallback",
        "https://claude.ai/oauth/authorize?code=abc",
        "https://claude.ai/oauth/authorize?redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback",
        "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2Fevil.test%3A5000%2Fcallback",
    ] {
        assert_eq!(authorize_url(ProviderKind::Claude, url), None);
    }
    assert_eq!(
        authorize_url(
            ProviderKind::Antigravity,
            "https://accounts.google.com/auth"
        ),
        Some("https://accounts.google.com/auth")
    );
}

#[test]
fn failed_new_login_deletes_account_through_daemon_and_trashes_managed_profile() {
    use std::io::Write as _;
    use std::time::Duration;

    use krill_proto::ipc::{self, ClientHello, ServerHello};
    use krill_proto::screen::Msg as _;
    use krill_proto::usage::{AccountInfo, UsageReply, UsageRequest};

    let root = std::env::temp_dir().join(format!("owe700-cleanup-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let home = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    let paths = krill_proto::daemon::Paths {
        socket: root.join("daemon.sock"),
        holder: root.join("unused-holder"),
        krilld: root.join("unused-daemon"),
    };
    let added = super::request_local(
        &home,
        Some(&state),
        &UsageRequest::AddAccount {
            provider: ProviderKind::Claude,
            label: "Claude Code Profile #1".into(),
            email: None,
            profile_dir: String::new(),
        },
    )
    .unwrap();
    let account: AccountInfo = added
        .accounts
        .into_iter()
        .find(|a| a.id == "claude-claude-code-profile-1")
        .unwrap();
    std::fs::write(
        Path::new(&account.profile_dir).join(".claude.json"),
        "partial",
    )
    .unwrap();
    let server_home = home.clone();
    let server_state = state.clone();
    let listener = std::os::unix::net::UnixListener::bind(&paths.socket).unwrap();
    let server = std::thread::spawn(move || {
        let hello = ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features: krill_proto::usage::FEAT_USAGE | krill_proto::usage::FEAT_USAGE_MANAGE,
            build: "test".into(),
            instance_id: 1,
        };
        let (mut watch, _) = listener.accept().unwrap();
        watch.write_all(&hello.encode().unwrap()).unwrap();
        ipc::read_frame(&mut watch).unwrap().unwrap();
        drop(watch);
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(&hello.encode().unwrap()).unwrap();
        let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
        let client = ClientHello::decode(kind, &body).unwrap();
        let features = ipc::negotiate(&hello, &client).unwrap().features;
        let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
        let req = UsageRequest::decode(kind, &body).unwrap();
        assert_eq!(
            req,
            UsageRequest::DeleteAccount {
                provider: ProviderKind::Claude,
                account_id: "claude-claude-code-profile-1".into(),
            }
        );
        let snap = super::request_local(&server_home, Some(&server_state), &req).unwrap();
        let reply = UsageReply::Accounts(snap).for_features(features);
        stream.write_all(&reply.encode().unwrap()).unwrap();
    });
    run_browser_login(
        &paths,
        &account,
        true,
        std::process::Command::new("/krill-test-missing-login-cli"),
        &std::sync::atomic::AtomicBool::new(false),
    );
    assert_eq!(
        take_login_outcome(&account.id),
        Some(LoginOutcome::Failed(LoginFailReason::SpawnFailed))
    );
    server.join().unwrap();
    assert!(!Path::new(&account.profile_dir).exists());
    assert!(
        super::orphans_local(&home, Some(&state)).is_empty(),
        "failed new login must not leave an orphan profile"
    );
    assert!(
        home.join(".Trash")
            .join("krill-claude-claude-code-profile-1")
            .join(".claude.json")
            .is_file()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_relogin_preserves_existing_profile_without_ipc() {
    let root = std::env::temp_dir().join(format!("owe700-relogin-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let paths = krill_proto::daemon::Paths {
        socket: root.join("no-daemon.sock"),
        holder: root.join("unused-holder"),
        krilld: root.join("unused-daemon"),
    };
    let account = krill_proto::usage::AccountInfo {
        id: "claude-existing".into(),
        provider: ProviderKind::Claude,
        label: "Existing".into(),
        email: None,
        profile_dir: root.join("profile").display().to_string(),
        active: false,
        host_default: false,
        stale: false,
        usage: krill_proto::usage::Snapshot::default(),
    };
    std::fs::create_dir_all(&account.profile_dir).unwrap();
    std::fs::write(
        Path::new(&account.profile_dir).join(".claude.json"),
        "existing",
    )
    .unwrap();
    run_browser_login(
        &paths,
        &account,
        false,
        {
            let mut command = std::process::Command::new("/bin/sleep");
            command.arg("30");
            command
        },
        &std::sync::atomic::AtomicBool::new(true),
    );
    assert_eq!(
        take_login_outcome(&account.id),
        Some(LoginOutcome::Cancelled)
    );
    run_browser_login(
        &paths,
        &account,
        false,
        std::process::Command::new("/krill-test-missing-login-cli"),
        &std::sync::atomic::AtomicBool::new(false),
    );
    assert_eq!(
        take_login_outcome(&account.id),
        Some(LoginOutcome::Failed(LoginFailReason::SpawnFailed))
    );
    assert!(
        Path::new(&account.profile_dir)
            .join(".claude.json")
            .is_file()
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn agy_terminal_login_preserves_literal_profile_and_never_uses_login_subcommand() {
    use krill_proto::usage::AccountInfo;
    use std::os::unix::fs::PermissionsExt as _;

    let root = std::env::temp_dir().join(format!("krill-agy-command-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let exe = root.join("agy");
    std::fs::write(&exe, "#!/bin/sh\n[ \"$SSH_CONNECTION\" = krill ] && [ \"$#\" = 1 ] && [ \"$1\" = \"--gemini_dir=$EXPECTED_PROFILE/.gemini\" ]\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut account = AccountInfo {
        usage: krill_proto::usage::Snapshot::default(),
        id: "agy-fixture".into(),
        provider: ProviderKind::Antigravity,
        label: "Work".into(),
        email: None,
        profile_dir: root.join("profile ' ; $literal").display().to_string(),
        active: false,
        host_default: false,
        stale: false,
    };
    let command = super::terminal_login_command(&account);
    assert!(
        std::process::Command::new("/bin/sh")
            .args(["-c", &command])
            .env(
                "PATH",
                std::env::join_paths([root.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
                    .unwrap()
            )
            .env("EXPECTED_PROFILE", &account.profile_dir)
            .status()
            .unwrap()
            .success()
    );
    let paths = krill_proto::daemon::Paths {
        socket: root.join("unused.sock"),
        holder: root.join("unused-holder"),
        krilld: root.join("unused-krilld"),
    };
    assert!(
        super::start_login(&paths, &account, true).is_err(),
        "interactive agy must not run behind hidden pipes"
    );
    account.host_default = true;
    assert_eq!(super::terminal_login_command(&account), "agy");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn only_links_handed_to_browser_are_opened_by_krill() {
    let url = "https://auth.openai.com/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback";
    // Codex ignores BROWSER on macOS and opens its own tab before printing the link.
    assert_eq!(
        login_link(ProviderKind::Codex, None, Some(url)),
        Some((url, false))
    );
    assert_eq!(
        login_link(ProviderKind::Codex, Some(url), Some(url)),
        Some((url, true))
    );
    assert_eq!(
        login_link(ProviderKind::Codex, None, Some("https://evil.example/a")),
        None
    );
}

#[test]
fn copy_link_mode_captures_authorize_url_without_opening_browser() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    let root = std::env::temp_dir().join(format!("krill-copy-link-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let paths = krill_proto::daemon::Paths {
        socket: root.join("no-daemon.sock"),
        holder: root.join("unused-holder"),
        krilld: root.join("unused-daemon"),
    };
    let account = krill_proto::usage::AccountInfo {
        id: "codex-copy-link".into(),
        provider: ProviderKind::Codex,
        label: "CopyLink".into(),
        email: None,
        profile_dir: root.join("profile").display().to_string(),
        active: false,
        host_default: false,
        stale: false,
        usage: krill_proto::usage::Snapshot::default(),
    };
    std::fs::create_dir_all(&account.profile_dir).unwrap();
    let expected = "https://auth.openai.com/authorize?redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&state=copy-test";
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_bg = cancel.clone();
    let paths_bg = paths.clone();
    let account_bg = account.clone();
    let handle = std::thread::spawn(move || {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", &format!("printf '%s\\n' '{expected}' >&2; sleep 30")]);
        run_browser_login_with_mode(&paths_bg, &account_bg, false, command, &cancel_bg, false);
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let captured = loop {
        if let Some(url) = login_url(&account.id) {
            break Some(url);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(captured.as_deref(), Some(expected));
    cancel.store(true, Ordering::Relaxed);
    handle.join().unwrap();
    assert_eq!(
        take_login_outcome(&account.id),
        Some(LoginOutcome::Cancelled)
    );
    assert_eq!(
        login_url(&account.id),
        None,
        "taking the outcome must clear the cached login URL"
    );
    std::fs::remove_dir_all(root).unwrap();
}
