//! 공개 `krill-link` API와 동기 UI 백엔드의 경계. 호출자는 백그라운드 실행기를 사용한다.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use super::backend::{
    Invite, LinkBackend, NearbyMac, PairFailure, PairOutcome, PairedDevice, ServerStatus,
};
use super::qr::QrMatrixSource;
use crate::i18n::link_ui as text;

#[derive(Debug, Clone)]
pub struct RealLinkBackend {
    state_dir: PathBuf,
    executable: PathBuf,
    hosts: std::sync::OnceLock<std::sync::Arc<crate::hosts::link::LinkHosts>>,
}

impl RealLinkBackend {
    pub fn new(state_dir: PathBuf, executable: PathBuf) -> Self {
        Self {
            state_dir,
            executable,
            hosts: std::sync::OnceLock::new(),
        }
    }

    pub fn state_dir(&self) -> &std::path::Path {
        &self.state_dir
    }
}

fn run<T>(
    future: impl Future<Output = Result<T, krill_link::Error>>,
) -> Result<T, krill_link::Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(krill_link::Error::Io)?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(20), future)
            .await
            .map_err(|_| krill_link::Error::Timeout("link operation"))?
    })
}

fn device(d: krill_link::AllowedDevice) -> PairedDevice {
    PairedDevice {
        id: d.id,
        name: d.name,
        added_at: d.added_at,
        last_seen_at: None,
    }
}

impl LinkBackend for RealLinkBackend {
    fn enable_server(&self) -> Result<ServerStatus, String> {
        krill_link::enable_launch_agent(&self.executable, &self.state_dir)
            .map_err(|_| text::ERR_SERVER_CHANGE.get().to_owned())?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !krill_link::is_serve_running(&self.state_dir) {
            if std::time::Instant::now() >= deadline {
                return Err(text::ERR_SERVER_NOT_READY.get().to_owned());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.server_status()
    }

    fn disable_server(&self) -> Result<ServerStatus, String> {
        krill_link::disable_launch_agent(&self.state_dir)
            .map_err(|_| text::ERR_SERVER_CHANGE.get().to_owned())?;
        let status = self.server_status()?;
        if status.enabled {
            return Err(text::ERR_SERVER_CHANGE.get().to_owned());
        }
        Ok(status)
    }

    fn server_status(&self) -> Result<ServerStatus, String> {
        Ok(ServerStatus {
            enabled: krill_link::is_serve_running(&self.state_dir),
        })
    }

    fn create_invite(&self) -> Result<Invite, String> {
        if !krill_link::is_serve_running(&self.state_dir)
            || krill_link::read_live_server_addr(&self.state_dir).is_none()
        {
            return Err(text::ERR_SERVER_NOT_READY.get().to_owned());
        }
        let invite = run(krill_link::create_invite_command(
            &self.state_dir,
            Some(krill_link::DEFAULT_INVITE_TTL),
        ))
        .map_err(|_| text::ERR_UNAVAILABLE.get().to_owned())?;
        Ok(Invite {
            link: invite.link,
            expires_at: invite.expires_at_ms / 1000,
        })
    }

    fn pair(&self, link: &str, device_name: &str) -> PairOutcome {
        match run(krill_link::pair_with_server(
            &self.state_dir,
            link,
            device_name,
        )) {
            Ok(host) => {
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
        krill_link::load_devices(&self.state_dir)
            .map(|list| list.into_iter().map(device).collect())
            .map_err(|_| text::ERR_UNAVAILABLE.get().to_owned())
    }

    fn revoke(&self, id: &str) -> Result<Vec<PairedDevice>, String> {
        run(krill_link::revoke_device_command(&self.state_dir, id))
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
        let matrix =
            krill_link::encode_qr(link.as_bytes()).map_err(|_| text::ERR_QR.get().to_owned())?;
        Ok((0..matrix.size())
            .map(|row| (0..matrix.size()).map(|col| matrix.get(row, col)).collect())
            .collect())
    }
}

pub fn classify_pair_error(err: &krill_link::Error) -> PairFailure {
    match err {
        krill_link::Error::InvalidPairLink(_) => PairFailure::InvalidLink,
        krill_link::Error::PairRejected(reason) if reason.contains("expired") => {
            PairFailure::ExpiredLink
        }
        krill_link::Error::PairRejected(reason)
            if reason.contains("used") || reason.contains("consumed") =>
        {
            PairFailure::AlreadyUsedLink
        }
        krill_link::Error::PairRejected(reason)
            if reason.contains("rate limit") || reason.contains("too many") =>
        {
            PairFailure::RateLimited
        }
        _ => PairFailure::Unavailable,
    }
}
