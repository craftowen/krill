use super::{authorize_url, cleanup_failed_login};
use krill_proto::usage::ProviderKind;
use std::path::Path;

#[test]
fn browser_login_accepts_only_https_provider_authorize_hosts() {
    for (provider, url) in [
        (
            ProviderKind::Claude,
            "https://claude.ai/oauth/authorize?code=abc",
        ),
        (
            ProviderKind::Claude,
            "https://console.anthropic.com/oauth/authorize",
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
        "http://claude.ai/oauth/authorize",
        "https://claude.ai.evil.test/",
        "https://evil.test/",
        "https://user@claude.ai/",
        "https://claude.ai:8443/",
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
fn failed_new_login_removes_account_through_daemon_but_relogin_is_preserved() {
    use std::io::Write as _;
    use std::time::Duration;

    use krill_proto::ipc::{self, ClientHello, ServerHello};
    use krill_proto::screen::Msg as _;
    use krill_proto::usage::{AccountInfo, AccountsSnapshot, Snapshot, UsageReply, UsageRequest};

    let root = std::env::temp_dir().join(format!("owe700-cleanup-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let paths = krill_proto::daemon::Paths {
        socket: root.join("daemon.sock"),
        holder: root.join("unused-holder"),
        krilld: root.join("unused-daemon"),
    };
    let account = AccountInfo {
        id: "claude-new".into(),
        provider: ProviderKind::Claude,
        label: "New profile".into(),
        email: None,
        profile_dir: root.join("profile").display().to_string(),
        active: false,
        host_default: false,
        stale: false,
        usage: Snapshot::default(),
    };
    std::fs::create_dir_all(&account.profile_dir).unwrap();
    std::fs::write(
        Path::new(&account.profile_dir).join(".claude.json"),
        "partial",
    )
    .unwrap();
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
        assert_eq!(
            UsageRequest::decode(kind, &body).unwrap(),
            UsageRequest::RemoveAccount {
                provider: ProviderKind::Claude,
                account_id: "claude-new".into(),
            }
        );
        let reply = UsageReply::Accounts(AccountsSnapshot::default()).for_features(features);
        stream.write_all(&reply.encode().unwrap()).unwrap();
    });
    cleanup_failed_login(&paths, &account, true);
    server.join().unwrap();
    assert!(
        Path::new(&account.profile_dir)
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
    cleanup_failed_login(&paths, &account, false);
    assert!(
        Path::new(&account.profile_dir)
            .join(".claude.json")
            .is_file()
    );
    std::fs::remove_dir_all(root).unwrap();
}
