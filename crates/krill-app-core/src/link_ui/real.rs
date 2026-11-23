//! 공개 `krill-link` API와 동기 UI 백엔드의 경계. 호출자는 백그라운드 실행기를 사용한다.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use super::backend::{
    Invite, LinkBackend, NearbyMac, PairFailure, PairOutcome, PairedDevice, PairedHost,
    ServerStatus, clean_host_name,
};
use super::qr::QrMatrixSource;
use crate::i18n::link_ui as text;

#[derive(Debug, Clone)]
pub struct RealLinkBackend {
    state_dir: PathBuf,
    executable: PathBuf,
    hosts: std::sync::OnceLock<std::sync::Arc<crate::hosts::link::LinkHosts>>,
    /// 연결된 원격 호스트별 데몬 빌드. 연결이 끊기면 비워 다시 읽는다.
    versions: std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>,
}

/// 원격 Mac에서 설치된 Krill.app의 헤드리스 업데이터(`krill-app --update-cli`)를 실행한다.
const REMOTE_UPDATE_SCRIPT: &str = r#"for d in /Applications/Krill.app "$HOME/Applications/Krill.app"; do [ -x "$d/Contents/MacOS/krill-app" ] && exec "$d/Contents/MacOS/krill-app" --update-cli; done; echo 'Krill.app not found'; exit 1"#;

impl RealLinkBackend {
    pub fn new(state_dir: PathBuf, executable: PathBuf) -> Self {
        Self {
            state_dir,
            executable,
            hosts: std::sync::OnceLock::new(),
            versions: std::sync::Arc::default(),
        }
    }

    fn link_hosts(&self) -> &std::sync::Arc<crate::hosts::link::LinkHosts> {
        self.hosts.get_or_init(|| {
            crate::hosts::link::LinkHosts::shared(self.state_dir.clone(), self.executable.clone())
        })
    }

    fn remote_target(&self, id: &str) -> Result<krill_hosts::HostTarget, String> {
        let want = crate::hosts::link::host_id(id);
        self.link_hosts()
            .registrations()
            .ok()
            .and_then(|hosts| hosts.into_iter().find(|h| h.spec.id == want))
            .map(|h| h.target())
            .ok_or_else(|| text::ERR_UNAVAILABLE.get().to_owned())
    }

    /// 연결된 호스트면 데몬 빌드를 돌려준다(처음 한 번만 핸드셰이크).
    fn host_version(&self, id: &str, connected: bool) -> Option<String> {
        let mut cache = self.versions.lock().ok()?;
        if !connected {
            cache.remove(id);
            return None;
        }
        if let Some(v) = cache.get(id) {
            return Some(v.clone());
        }
        drop(cache);
        let hello = krill_proto::daemon::client_hello(
            krill_proto::ipc::ClientRole::App,
            "krill-app-remote-version",
        );
        let build = self
            .remote_target(id)
            .ok()?
            .connect(&hello)
            .ok()?
            .server
            .build;
        self.versions
            .lock()
            .ok()?
            .insert(id.to_owned(), build.clone());
        Some(build)
    }
}

fn run<T>(
    future: impl Future<Output = Result<T, krill_link_core::Error>>,
) -> Result<T, krill_link_core::Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(krill_link_core::Error::Io)?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(20), future)
            .await
            .map_err(|_| krill_link_core::Error::Timeout("link operation"))?
    })
}

fn device(d: krill_link_core::AllowedDevice) -> PairedDevice {
    PairedDevice {
        id: d.id,
        name: d.name,
        added_at: d.added_at,
        last_seen_at: None,
    }
}

impl LinkBackend for RealLinkBackend {
    fn enable_server(&self) -> Result<ServerStatus, String> {
        krill_link_core::service::enable_server(&self.executable, &self.state_dir).map_err(
            |error| {
                tracing::warn!(%error, "failed to enable incoming access");
                text::ERR_SERVER_CHANGE.get().to_owned()
            },
        )?;
        self.server_status()
    }

    fn disable_server(&self) -> Result<ServerStatus, String> {
        krill_link_core::service::disable_server(&self.state_dir)
            .map_err(|_| text::ERR_SERVER_CHANGE.get().to_owned())?;
        let status = self.server_status()?;
        if status.enabled {
            return Err(text::ERR_SERVER_CHANGE.get().to_owned());
        }
        Ok(status)
    }

    fn server_status(&self) -> Result<ServerStatus, String> {
        Ok(ServerStatus {
            enabled: krill_link_core::is_serve_running(&self.state_dir),
        })
    }

    fn create_invite(&self) -> Result<Invite, String> {
        if !krill_link_core::is_serve_running(&self.state_dir)
            || krill_link_core::read_live_server_addr(&self.state_dir).is_none()
        {
            return Err(text::ERR_SERVER_NOT_READY.get().to_owned());
        }
        let invite = run(krill_link_core::create_invite_command(
            &self.state_dir,
            Some(krill_link_core::DEFAULT_INVITE_TTL),
        ))
        .map_err(|_| text::ERR_UNAVAILABLE.get().to_owned())?;
        Ok(Invite {
            link: invite.link,
            expires_at: invite.expires_at_ms / 1000,
        })
    }

    fn pair(&self, link: &str, device_name: &str, host_name: &str) -> PairOutcome {
        match run(krill_link_core::pair_with_server(
            &self.state_dir,
            link,
            device_name,
        )) {
            Ok(mut host) => {
                if let Some(name) = clean_host_name(host_name) {
                    match krill_link_core::rename_host(&self.state_dir, &host.id, &name) {
                        Ok(renamed) => host = renamed,
                        Err(_) => return PairOutcome::Failed(PairFailure::Unavailable),
                    }
                }
                self.hosts
                    .get_or_init(|| {
                        crate::hosts::link::LinkHosts::shared(
                            self.state_dir.clone(),
                            self.executable.clone(),
                        )
                    })
                    .paired(&host.id);
                PairOutcome::Paired(PairedDevice {
                    id: host.id,
                    name: host.name,
                    added_at: host.added_at,
                    last_seen_at: None,
                })
            }
            Err(err) => PairOutcome::Failed(classify_pair_error(&err)),
        }
    }

    fn devices(&self) -> Result<Vec<PairedDevice>, String> {
        krill_link_core::load_devices(&self.state_dir)
            .map(|list| list.into_iter().map(device).collect())
            .map_err(|_| text::ERR_UNAVAILABLE.get().to_owned())
    }

    fn remote_hosts(&self) -> Result<Vec<PairedHost>, String> {
        krill_link_core::load_hosts(&self.state_dir)
            .map_err(|_| text::ERR_UNAVAILABLE.get().to_owned())?
            .into_iter()
            .map(|host| {
                let info = crate::hosts::link::connection_info(&self.state_dir, &host.id)
                    .map_err(|_| text::ERR_UNAVAILABLE.get().to_owned())?;
                let version =
                    self.host_version(&host.id, info.status == krill_hosts::HostStatus::Connected);
                Ok(PairedHost {
                    id: host.id,
                    name: host.name,
                    status: info.status,
                    version,
                })
            })
            .collect()
    }

    fn update_host(&self, id: &str) -> Result<(), String> {
        let target = self.remote_target(id)?;
        send_remote_update(&target, Duration::from_secs(10), Duration::from_secs(5))?;
        if let Ok(mut cache) = self.versions.lock() {
            cache.remove(id);
        }
        Ok(())
    }

    fn disconnect_host(&self, id: &str) -> Result<Vec<PairedHost>, String> {
        self.link_hosts()
            .disconnect_host(id)
            .map_err(|_| text::ERR_DISCONNECT.get().to_owned())?;
        self.remote_hosts()
    }

    fn rename_host(&self, id: &str, name: &str) -> Result<Vec<PairedHost>, String> {
        let name = clean_host_name(name).ok_or_else(|| text::ERR_UNAVAILABLE.get().to_owned())?;
        krill_link_core::rename_host(&self.state_dir, id, &name)
            .map_err(|_| text::ERR_UNAVAILABLE.get().to_owned())?;
        self.remote_hosts()
    }

    fn revoke(&self, id: &str) -> Result<Vec<PairedDevice>, String> {
        run(krill_link_core::revoke_device_command(&self.state_dir, id))
            .map_err(|_| text::ERR_UNAVAILABLE.get().to_owned())?;
        self.devices()
    }

    fn nearby(&self) -> Result<Vec<NearbyMac>, String> {
        Ok(Vec::new())
    }

    fn qr_matrix(&self, link: &str) -> Result<Vec<Vec<bool>>, String> {
        RealQrMatrix.encode(link)
    }
}

#[derive(Debug)]
pub struct RealQrMatrix;

impl QrMatrixSource for RealQrMatrix {
    fn encode(&self, link: &str) -> Result<Vec<Vec<bool>>, String> {
        let matrix = krill_link_core::encode_qr(link.as_bytes())
            .map_err(|_| text::ERR_QR.get().to_owned())?;
        Ok((0..matrix.size())
            .map(|row| (0..matrix.size()).map(|col| matrix.get(row, col)).collect())
            .collect())
    }
}

pub fn classify_pair_error(err: &krill_link_core::Error) -> PairFailure {
    match err {
        krill_link_core::Error::InvalidPairLink(_) => PairFailure::InvalidLink,
        krill_link_core::Error::PairRejected(reason) if reason.contains("expired") => {
            PairFailure::ExpiredLink
        }
        krill_link_core::Error::PairRejected(reason)
            if reason.contains("used") || reason.contains("consumed") =>
        {
            PairFailure::AlreadyUsedLink
        }
        krill_link_core::Error::PairRejected(reason)
            if reason.contains("rate limit") || reason.contains("too many") =>
        {
            PairFailure::RateLimited
        }
        _ => PairFailure::Unavailable,
    }
}

fn send_remote_update(
    target: &krill_hosts::HostTarget,
    read_timeout: Duration,
    write_timeout: Duration,
) -> Result<(), String> {
    use krill_proto::control::{Create, FEAT_CLI, FEAT_CONTROL, Program, Reply, Request};
    use krill_proto::screen::Msg as _;
    use std::io::Write as _;

    let err = || text::ERR_UPDATE.get().to_owned();
    let hello = krill_proto::daemon::client_hello(
        krill_proto::ipc::ClientRole::App,
        "krill-app-remote-update",
    );
    let mut conn = target.connect(&hello).map_err(|_| err())?;
    conn.stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|_| err())?;
    conn.stream
        .set_write_timeout(Some(write_timeout))
        .map_err(|_| err())?;
    let need = FEAT_CONTROL | FEAT_CLI;
    if conn.negotiated.features & need != need {
        return Err(text::ERR_UPDATE_UNSUPPORTED.get().to_owned());
    }
    let create = Request::Create(Create {
        account: None,
        program: Program::Custom {
            program: b"/bin/sh".to_vec(),
            args: vec![b"-c".to_vec(), REMOTE_UPDATE_SCRIPT.as_bytes().to_vec()],
        },
        cwd: b"/tmp".to_vec(),
        cols: 120,
        rows: 24,
    });
    conn.stream
        .write_all(&create.encode().map_err(|_| err())?)
        .map_err(|_| err())?;
    let (kind, body) = krill_proto::ipc::read_frame(&mut conn.stream)
        .ok()
        .flatten()
        .ok_or_else(err)?;
    match Reply::decode(kind, &body) {
        Ok(Reply::Created { .. }) => Ok(()),
        _ => Err(err()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn remote_update_times_out_instead_of_hanging_when_daemon_stalls() {
        use krill_proto::control::{FEAT_CLI, FEAT_CONTROL};
        use krill_proto::ipc::{self, ClientHello, ServerHello};
        use krill_proto::os::net::UnixListener;
        use std::io::Write as _;
        use std::time::Instant;

        let dir =
            std::env::temp_dir().join(format!("krill-link-update-timeout-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("remote.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .write_all(
                    &ServerHello {
                        proto_min: ipc::PROTO_MIN,
                        proto_max: ipc::PROTO_MAX,
                        features: FEAT_CONTROL | FEAT_CLI,
                        build: "test".into(),
                        instance_id: 1,
                    }
                    .encode()
                    .unwrap(),
                )
                .unwrap();
            let (kind, body) = ipc::read_frame(&mut stream).unwrap().unwrap();
            ClientHello::decode(kind, &body).unwrap();
            // Read the Create request, then intentionally withhold any reply until released.
            ipc::read_frame(&mut stream).ok();
            done_rx.recv_timeout(Duration::from_secs(3)).ok();
        });

        let target = krill_hosts::HostTarget::remote("stalled-host", socket);
        let started = Instant::now();
        let res = send_remote_update(
            &target,
            Duration::from_millis(80),
            Duration::from_millis(80),
        );
        let elapsed = started.elapsed();
        done_tx.send(()).ok();
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert!(res.is_err(), "stalled remote update must fail with timeout");
        assert!(
            elapsed < Duration::from_secs(2),
            "remote update hung for {elapsed:?} instead of respecting timeout"
        );
    }
}
