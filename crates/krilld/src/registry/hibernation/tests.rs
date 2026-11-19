use super::*;
use crate::holder::Holder;
use crate::os::OwnedFd;
use crate::scope::{Peer, Scope};
use krill_proto::agent::Permission;
use krill_proto::control::{AgentLaunch, Create, Program, Reply};
use krill_proto::holder::{FromHolder, ToHolder, VERSION};
use krill_proto::os::net::UnixStream;
use std::io::{Read as _, Write as _};

#[test]
fn unslept_session_is_never_automatically_restarted() {
    let reg = Registry::new(None, None);
    let peer = Peer {
        pid: None,
        scope: Scope::Full,
    };
    let create = Create {
        account: None,
        program: Program::Login,
        cwd: vec![],
        cols: 80,
        rows: 24,
    };
    assert_eq!(
        reg.wake_hibernated(&peer, SessionId(9), create),
        Ok(Reply::Hibernation { session: None })
    );
}

/// OWE-698: 휴면 뒤 계정을 바꿔도 깨우기는 실행 당시 계정의 `CLAUDE_CONFIG_DIR`로 띄운다.
/// holder가 받은 Spawn env를 관측하고 실패로 답해 실제 프로세스는 띄우지 않는다.
#[test]
fn wake_after_account_switch_keeps_launch_profile() {
    use crate::agent::control::LaunchAccount;
    use crate::usage::accounts;
    use krill_proto::usage::ProviderKind;
    let dir = std::env::temp_dir().join(format!("owe698-wake-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(dir.join("hibernated")).unwrap();
    let claude_exe = dir.join("claude");
    fs::write(&claude_exe, "#!/bin/sh\nprintf '2.1.299\\n'\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&claude_exe, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        dir.join(krill_proto::agent::FILE_NAME),
        format!(
            "permission = \"manual\"\n[profiles.claude]\nname = \"Claude\"\nlaunch = [{:?}]\n",
            claude_exe.to_str().unwrap()
        ),
    )
    .unwrap();
    let home = accounts::default_home();
    let add = |label: &str| {
        let profile = dir.join(label);
        fs::create_dir_all(&profile).unwrap();
        let id = accounts::add_account(
            &home,
            Some(&dir),
            ProviderKind::Claude,
            label,
            None,
            profile.to_str().unwrap(),
        )
        .unwrap()
        .accounts
        .into_iter()
        .find(|a| a.label == label)
        .unwrap()
        .id;
        (id, profile.to_str().unwrap().to_owned())
    };
    let ((a, a_dir), (b, _)) = (add("A"), add("B"));
    accounts::switch_account(&home, Some(&dir), ProviderKind::Claude, &a).unwrap();

    let (daemon_end, mut holder_end) = UnixStream::pair().unwrap();
    for m in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: std::process::id(),
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        holder_end.write_all(&m.encode().unwrap()).unwrap();
    }
    let (holder, link, _, _) = Holder::connect(OwnedFd::from(daemon_end)).unwrap();
    let reg = Registry::new(Some((holder, Vec::new())), Some(&dir));
    let old = SessionId(5);
    reg.agents
        .remember_session(
            old,
            &AgentLaunch {
                profile: "claude".into(),
                permission: Permission::Manual,
            },
            Some("11111111-2222-4333-8444-555555555555".into()),
            dir.as_os_str().as_encoded_bytes().to_vec(),
            Some(LaunchAccount {
                id: a,
                env_key: "CLAUDE_CONFIG_DIR".into(),
                dir: a_dir.clone(),
            }),
        )
        .unwrap();
    fs::write(dir.join("hibernated").join("5"), b"").unwrap();
    accounts::switch_account(&home, Some(&dir), ProviderKind::Claude, &b).unwrap();

    let observed = std::thread::spawn(move || {
        let mut header = [0; 4];
        holder_end.read_exact(&mut header).unwrap();
        let mut body = vec![0; usize::try_from(u32::from_le_bytes(header)).unwrap()];
        holder_end.read_exact(&mut body).unwrap();
        let (&[kind, fds], payload) = body.split_first_chunk::<2>().unwrap();
        let frame = krill_proto::holder::Frame {
            kind,
            fds,
            body: payload,
        };
        let ToHolder::Spawn { req, env, .. } = ToHolder::decode(frame).unwrap() else {
            panic!("expected Spawn");
        };
        let failed = FromHolder::Failed {
            req,
            errno: libc::EIO,
        };
        holder_end.write_all(&failed.encode().unwrap()).unwrap();
        (env, holder_end)
    });
    let peer = Peer {
        pid: None,
        scope: Scope::Full,
    };
    let create = Create {
        account: None,
        program: Program::Login,
        cwd: vec![],
        cols: 80,
        rows: 24,
    };
    let (env, holder_end) = std::thread::scope(|scope| {
        let holder = reg.holder.as_ref().unwrap();
        let pump = scope.spawn(move || holder.run(link, |_, _| {}));
        assert!(reg.wake_hibernated(&peer, old, create).is_err());
        let (env, holder_end) = observed.join().unwrap();
        // 링크를 닫아 `run`을 끝낸다.
        holder_end.shutdown(std::net::Shutdown::Both).unwrap();
        pump.join().unwrap();
        (env, holder_end)
    });
    let config: Vec<_> = env
        .iter()
        .filter(|(k, _)| k == b"CLAUDE_CONFIG_DIR")
        .map(|(_, v)| v.as_slice())
        .collect();
    assert_eq!(
        config,
        [a_dir.as_bytes()],
        "활성 계정 B가 아니라 실행 당시 A"
    );
    drop((holder_end, reg));
    fs::remove_dir_all(dir).unwrap();
}
