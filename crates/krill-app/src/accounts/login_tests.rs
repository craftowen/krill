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

fn encode_b64url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let (mut bits, mut count) = (0_u32, 0_u32);
    let mut out = String::new();
    for &byte in bytes {
        bits = (bits << 8) | u32::from(byte);
        count += 8;
        while count >= 6 {
            count -= 6;
            let idx = usize::try_from((bits >> count) & 63).unwrap();
            out.push(char::from(TABLE[idx]));
        }
    }
    if count > 0 {
        let idx = usize::try_from((bits << (6 - count)) & 63).unwrap();
        out.push(char::from(TABLE[idx]));
    }
    out
}

#[test]
fn codex_browser_login_renames_auto_profile_from_auth_json_and_requests_usage_refresh() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use krill_proto::ipc::{self, ClientHello, ServerHello};
    use krill_proto::screen::Msg as _;
    use krill_proto::usage::{
        AccountInfo, AccountsSnapshot, FEAT_ACCOUNT_CONNECTION, FEAT_PROVIDER_DETAILS, FEAT_USAGE,
        FEAT_USAGE_MANAGE, FEAT_USAGE_REFRESH, FEAT_USAGE_STATUS, Snapshot, UsageReply,
        UsageRequest,
    };

    let root =
        std::env::temp_dir().join(format!("krill-codex-login-rename-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    let profile = root.join("profile");
    std::fs::create_dir_all(&profile).unwrap();

    let id_payload = encode_b64url(
        &serde_json::to_vec(&serde_json::json!({
            "email": "beta@example.test",
            "iat": 1_794_147_864_i64,
            "exp": 1_794_151_464_i64,
        }))
        .unwrap(),
    );
    let auth_json = serde_json::json!({
        "tokens": {
            "id_token": format!("eyJhbGciOiJub25lIn0.{id_payload}.sig"),
            "access_token": "acc-tok",
            "account_id": "acc-beta",
        }
    })
    .to_string();

    let fake_codex = root.join("codex");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"login\" ] && [ \"$2\" = \"status\" ]; then\n  [ -f \"$CODEX_HOME/auth.json\" ] && printf 'Logged in using ChatGPT\\n' && exit 0\n  exit 1\nfi\nif [ \"$1\" = \"login\" ]; then\n  printf '%s\\n' '{auth_json}' > \"$CODEX_HOME/auth.json\"\n  exit 0\nfi\nexit 2\n"
    );
    std::fs::write(&fake_codex, script).unwrap();
    std::fs::set_permissions(&fake_codex, std::fs::Permissions::from_mode(0o700)).unwrap();

    let paths = krill_proto::daemon::Paths {
        socket: root.join("daemon.sock"),
        holder: root.join("unused-holder"),
        krilld: root.join("unused-daemon"),
    };
    let listener = std::os::unix::net::UnixListener::bind(&paths.socket).unwrap();
    listener.set_nonblocking(true).unwrap();

    let done = Arc::new(AtomicBool::new(false));
    let done_bg = done.clone();
    let server = std::thread::spawn(move || {
        let hello = ServerHello {
            proto_min: ipc::PROTO_MIN,
            proto_max: ipc::PROTO_MAX,
            features: FEAT_USAGE
                | FEAT_USAGE_MANAGE
                | FEAT_USAGE_REFRESH
                | FEAT_USAGE_STATUS
                | FEAT_ACCOUNT_CONNECTION
                | FEAT_PROVIDER_DETAILS,
            build: "test".into(),
            instance_id: 1,
        };
        let encoded_hello = hello.encode().unwrap();
        let mut requests = Vec::new();
        loop {
            match listener.accept() {
                Ok((mut conn, _)) => {
                    conn.set_nonblocking(false).unwrap();
                    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    conn.write_all(&encoded_hello).unwrap();
                    let (kind, body) = ipc::read_frame(&mut conn).unwrap().unwrap();
                    let client = ClientHello::decode(kind, &body).unwrap();
                    let features = ipc::negotiate(&hello, &client).unwrap().features;
                    if let Some((kind, body)) = ipc::read_frame(&mut conn).unwrap() {
                        let req = UsageRequest::decode(kind, &body).unwrap();
                        requests.push(req);
                        let reply = UsageReply::Accounts(AccountsSnapshot::default())
                            .for_features(features);
                        conn.write_all(&reply.encode().unwrap()).unwrap();
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if done_bg.load(Ordering::Acquire) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(err) => panic!("accept failed: {err}"),
            }
        }
        requests
    });

    let account = AccountInfo {
        id: "codex-codex-profile-1".into(),
        provider: ProviderKind::Codex,
        label: "Codex Profile #1".into(),
        email: None,
        profile_dir: profile.display().to_string(),
        active: false,
        host_default: false,
        stale: false,
        usage: Snapshot::default(),
    };
    let mut command =
        krill_proto::account_login::login_command(&fake_codex, ProviderKind::Codex, Some(&profile))
            .unwrap();
    command.env(
        "PATH",
        std::env::join_paths([root.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap(),
    );

    run_browser_login_with_mode(
        &paths,
        &account,
        true,
        command,
        &AtomicBool::new(false),
        false,
    );

    done.store(true, Ordering::Release);
    let requests = server.join().unwrap();
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(
        take_login_outcome(&account.id),
        Some(LoginOutcome::Succeeded)
    );
    let expected_rename = UsageRequest::RenameAccount {
        provider: ProviderKind::Codex,
        account_id: "codex-codex-profile-1".into(),
        label: "beta@example.test".into(),
        email: Some("beta@example.test".into()),
    };
    let rename_pos = requests.iter().position(|r| r == &expected_rename);
    let refresh_pos = requests
        .iter()
        .position(|r| r == &UsageRequest::RefreshUsage);
    assert!(
        rename_pos.is_some(),
        "expected RenameAccount in {requests:?}"
    );
    assert!(
        refresh_pos.is_some(),
        "expected RefreshUsage in {requests:?}"
    );
    assert!(
        rename_pos.unwrap() < refresh_pos.unwrap(),
        "expected RefreshUsage after RenameAccount in {requests:?}"
    );
}
