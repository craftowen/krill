//! 원격 서버의 공용 옵션과 플랫폼별 구현.
use crate::pair::PairRateLimiter;
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::build_iroh_endpoint;
#[cfg(unix)]
pub use unix::*;
#[cfg(not(unix))]
mod unsupported;
#[cfg(not(unix))]
pub use unsupported::*;

/// `krill-link serve` 실행 옵션.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// 상태 디렉터리 (`link/`, `run/` 상위).
    pub state_dir: PathBuf,
    /// 로컬 `krilld.sock` 경로 (생략 시 `<state_dir>/run/krilld.sock`).
    pub krilld_socket: Option<PathBuf>,
    /// 짝짓기 속도 제한기 (생략 시 기본값).
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
    pub fn effective_krilld_socket(&self) -> PathBuf {
        self.krilld_socket
            .clone()
            .unwrap_or_else(|| self.state_dir.join("run/krilld.sock"))
    }
}
