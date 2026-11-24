//! 원격 서버의 공용 옵션과 플랫폼별 구현.
use crate::pair::PairRateLimiter;
use std::path::PathBuf;
use std::sync::Arc;

mod server;
pub(crate) use server::build_iroh_endpoint;
pub use server::*;

/// `krill-link serve` 실행 옵션.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// 상태 디렉터리 (`link/`, `run/` 상위).
    pub state_dir: PathBuf,
    /// 로컬 소켓 경로 (생략 시 `daemon::socket_path`로 상태별 기본 경로를 해석).
    pub krilld_socket: Option<PathBuf>,
    /// 페어링 속도 제한기 (생략 시 기본값).
    pub rate_limiter: Option<Arc<PairRateLimiter>>,
}

impl ServeOptions {
    /// 기본 상태 디렉터리 기반 서버 옵션을 만든다.
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            krilld_socket: None,
            rate_limiter: None,
        }
    }

    /// 연결할 로컬 `krilld.sock` 경로를 돌려준다.
    pub fn effective_krilld_socket(&self) -> std::io::Result<PathBuf> {
        match &self.krilld_socket {
            Some(socket) => Ok(socket.clone()),
            None => krill_proto::daemon::socket_path(&self.state_dir),
        }
    }
}

#[cfg(test)]
mod socket_tests {
    use super::*;

    #[test]
    fn default_resolves_shared_endpoint_and_explicit_socket_wins() {
        let mut opts = ServeOptions::new(std::env::temp_dir().join("link-socket-state"));
        assert_eq!(
            opts.effective_krilld_socket().unwrap(),
            krill_proto::daemon::socket_path(&opts.state_dir).unwrap()
        );
        opts.krilld_socket = Some(PathBuf::from("explicit.sock"));
        assert_eq!(
            opts.effective_krilld_socket().unwrap(),
            PathBuf::from("explicit.sock")
        );
    }
}
