//! `std::os::unix`의 `fs`·`net`·`ffi` 대역(M11 Windows 포팅). 하위 크레이트는 import 경로만 바꾼다.
//!
//! - Unix: `std::os::unix` 재수출이라 동작이 바이트 단위로 같다.
//! - Windows: 쓰기 측 `mode`(`OpenOptionsExt`·`DirBuilderExt`)는 no-op이다. 비공개 권한은 `%LOCALAPPDATA%`
//!   ACL 상속에 맡긴다(추정, 명시 DACL은 후속 작업). `net`은 `AF_UNIX`(`uds_windows`), `ffi`는 OS 인코딩 바이트
//!   (`as_encoded_bytes`, 올바른 유니코드면 UTF-8과 같다).
//! - 넣지 않는 것: 읽기 측(실행 비트·소유자, `PermissionsExt`·`MetadataExt`), `Permissions::from_mode`(Windows는
//!   빈 `Permissions`를 만들 수 없다), `symlink`(파일·디렉터리 구분과 권한이 다르다). 호출처가 cfg로 분기한다.

#[cfg(unix)]
pub use std::os::unix::{ffi, fs, net};

/// 쓰기 측 권한 확장 트레이트(no-op).
#[cfg(windows)]
pub mod fs {
    /// [`std::os::unix::fs::OpenOptionsExt`] 대역.
    pub trait OpenOptionsExt {
        /// Windows에서는 무시한다.
        fn mode(&mut self, mode: u32) -> &mut Self;
    }

    impl OpenOptionsExt for std::fs::OpenOptions {
        fn mode(&mut self, _mode: u32) -> &mut Self {
            self
        }
    }

    /// [`std::os::unix::fs::DirBuilderExt`] 대역.
    pub trait DirBuilderExt {
        /// Windows에서는 무시한다.
        fn mode(&mut self, mode: u32) -> &mut Self;
    }

    impl DirBuilderExt for std::fs::DirBuilder {
        fn mode(&mut self, _mode: u32) -> &mut Self {
            self
        }
    }
}

/// Windows 10 1803+ `AF_UNIX` 소켓.
#[cfg(windows)]
pub mod net {
    pub use uds_windows::{SocketAddr, UnixListener, UnixStream};
}

/// 경로·문자열 바이트 변환.
#[cfg(windows)]
pub mod ffi {
    use std::ffi::{OsStr, OsString};

    /// [`std::os::unix::ffi::OsStrExt`] 대역.
    pub trait OsStrExt {
        /// OS 인코딩 바이트(WTF-8).
        fn as_bytes(&self) -> &[u8];
        /// UTF-8이 아니면 `U+FFFD` 한 글자(어떤 실제 경로와도 겹치지 않게).
        fn from_bytes(bytes: &[u8]) -> &Self;
    }

    // ponytail: 짝 없는 서로게이트(UTF-8 아님) 경로는 왕복하지 못한다. 필요하면 와이어에 WTF-8 검증을 넣고
    // `from_encoded_bytes_unchecked`(unsafe, FFI 모듈)로 옮긴다.
    impl OsStrExt for OsStr {
        fn as_bytes(&self) -> &[u8] {
            self.as_encoded_bytes()
        }

        fn from_bytes(bytes: &[u8]) -> &Self {
            OsStr::new(std::str::from_utf8(bytes).unwrap_or("\u{FFFD}"))
        }
    }

    /// [`std::os::unix::ffi::OsStringExt`] 대역.
    pub trait OsStringExt {
        /// OS 인코딩 바이트(WTF-8).
        fn into_vec(self) -> Vec<u8>;
        /// UTF-8이 아닌 부분은 `U+FFFD`로 바꾼다.
        fn from_vec(vec: Vec<u8>) -> Self;
    }

    impl OsStringExt for OsString {
        fn into_vec(self) -> Vec<u8> {
            self.into_encoded_bytes()
        }

        fn from_vec(vec: Vec<u8>) -> Self {
            match String::from_utf8(vec) {
                Ok(s) => s.into(),
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned().into(),
            }
        }
    }
}
