//! krilld의 Unix 전용 fd·권한 API 대역(M11 Phase 1, OWE-820). Unix는 지금 동작 그대로다.
//!
//! Windows는 컴파일 전용이다.
//! - fd: krilld가 fd로 다루는 것(holder 링크·클라이언트 소켓·PTY master) 중 Windows에서 지금 실제로 있는 것은
//!   `AF_UNIX` 소켓뿐이라 소켓 타입을 쓴다. holder 링크([`crate::holder`])가 늘 실패해 PTY master는 만들어지지
//!   않는다. `ConPTY` 핸들 모델은 Phase 3(OWE-833)에서 정한다.
//! - 권한: 소유자·모드 검사와 `O_NOFOLLOW`는 없다(`krill_proto::os`처럼 `%LOCALAPPDATA%` ACL 상속에 맡김, 추정).
//!   DACL 검사와 `FILE_FLAG_OPEN_REPARSE_POINT`는 Phase 3(OWE-833).

use std::fs::{Metadata, OpenOptions};

#[cfg(unix)]
pub(crate) use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
#[cfg(windows)]
pub(crate) use std::os::windows::io::{BorrowedSocket as BorrowedFd, OwnedSocket as OwnedFd};

/// [`std::os::fd::AsFd`] 대역.
#[cfg(windows)]
pub(crate) trait AsFd {
    fn as_fd(&self) -> BorrowedFd<'_>;
}

#[cfg(windows)]
impl<T: std::os::windows::io::AsSocket> AsFd for T {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.as_socket()
    }
}

/// 연결된 소켓을 holder 링크([`crate::holder::link::Link`])용 fd로 바꾼다.
#[cfg(unix)]
#[expect(clippy::unnecessary_wraps, reason = "Windows 판과 같은 시그니처")]
pub(crate) fn link_fd(s: krill_proto::os::net::UnixStream) -> std::io::Result<OwnedFd> {
    Ok(OwnedFd::from(s))
}

/// Windows에는 holder 링크가 없다.
#[cfg(windows)]
pub(crate) fn link_fd(_s: krill_proto::os::net::UnixStream) -> std::io::Result<OwnedFd> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// 심볼릭 링크 `dst` → `src`.
#[cfg(unix)]
pub(crate) fn symlink(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

/// Windows 링크는 파일·디렉터리를 구분하고 권한이 필요하다(Phase 3).
#[cfg(windows)]
pub(crate) fn symlink(_src: &std::path::Path, _dst: &std::path::Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

/// 유효 UID.
#[cfg(unix)]
pub(crate) fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Windows에는 UID가 없다. `sys::peer_uid`가 같은 사용자 SID인 피어에게만 주는 값.
#[cfg(windows)]
pub(crate) fn euid() -> u32 {
    crate::sys::SAME_USER
}

/// 소유자가 나(유효 UID)이고 `mask` 권한 비트가 모두 꺼져 있나.
#[cfg(unix)]
pub(crate) fn private_to_me(meta: &Metadata, mask: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    meta.uid() == euid() && meta.mode() & mask == 0
}

/// Windows는 늘 참이다(모듈 설명).
#[cfg(windows)]
pub(crate) fn private_to_me(_meta: &Metadata, _mask: u32) -> bool {
    true
}

/// 마지막 경로 성분이 심볼릭 링크면 열기를 실패시킨다(`O_NOFOLLOW`). Windows는 그대로 둔다(모듈 설명).
pub(crate) fn no_follow(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(options, libc::O_NOFOLLOW);
    options
}

/// 디스크에 실제로 잡힌 바이트(희소 파일은 길이보다 작다). Windows는 길이.
pub(crate) fn allocated(meta: &Metadata) -> u64 {
    #[cfg(unix)]
    let bytes = std::os::unix::fs::MetadataExt::blocks(meta).saturating_mul(512);
    #[cfg(windows)]
    let bytes = meta.len();
    bytes
}
