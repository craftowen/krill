//! Windows SSH 원격 호스트: 아직 미지원(`io::ErrorKind::Unsupported`). 근거는 `docs/research/windows-spike-OWE-840.md`.
//!
//! - ssh.exe(OpenSSH for Windows 9.5p2)는 `-L <로컬.sock>:<원격.sock>`을 "Unix domain server sockets are not
//!   supported"로 거부한다.
//! - 루프백 TCP `-L`은 ssh.exe가 포트를 직접 리슨해 같은 PC의 다른 사용자를 막을 수 없어 쓰지 않는다.
//! - 연결마다 `ssh -W <원격.sock>`을 띄워 stdio를 잇는 안은, std `Stdio::piped()` stdout을 잡은 ssh.exe가
//!   핸드셰이크에서 멈춰 쓸 수 없다. overlapped 파이프 stdio 헬퍼(허용된 FFI 모듈)가 생기면 다시 켠다.

use std::convert::Infallible;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use super::{SshOptions, windows_unsupported};

/// 만들 수 없는 터널. Unix와 같은 API를 유지한다.
#[derive(Debug)]
pub struct Tunnel {
    unavailable: Infallible,
}

impl Tunnel {
    pub fn start_at_with_program(
        _opts: &SshOptions,
        _local: PathBuf,
        _remote: &Path,
        _ssh_bin: &OsStr,
    ) -> io::Result<Self> {
        Err(windows_unsupported())
    }

    pub fn socket(&self) -> &Path {
        match self.unavailable {}
    }

    pub fn pid(&self) -> u32 {
        match self.unavailable {}
    }

    pub fn is_alive(&mut self) -> bool {
        match self.unavailable {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_tunnel_and_home_query_fail_as_unsupported_without_spawning() {
        let opts = SshOptions::new("owner@test");
        let missing = OsStr::new("Z:\\no-such-ssh.exe");
        let local = std::env::temp_dir().join("kh-win-unsupported.sock");
        let err =
            Tunnel::start_at_with_program(&opts, local.clone(), Path::new("/r.sock"), missing)
                .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(!local.exists());
        let err = super::super::resolve_remote_socket_with_program(&opts, missing).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
