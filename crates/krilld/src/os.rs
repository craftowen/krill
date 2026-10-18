//! krilld의 Unix 전용 fd·권한 API 대역(M11 Phase 1, OWE-820). Unix는 지금 동작 그대로다.
//!
//! Windows는 컴파일 전용이다.
//! - fd: krilld가 fd로 다루는 것(holder 링크·클라이언트 소켓·PTY master) 중 Windows에서 지금 실제로 있는 것은
//!   `AF_UNIX` 소켓은 소켓 타입을 쓴다. `ConPTY`는 [`crate::holder::Master`]로 별도 구분하며
//!   `ReadFile` 세션 리더 연결은 OWE-833-8에서 진행한다.
//! - 권한: 소유자·모드 검사는 없다(`krill_proto::os`처럼 `%LOCALAPPDATA%` ACL 상속에 맡김, 추정). DACL 검사는 후속.
//!   `O_NOFOLLOW` 대신 `FILE_FLAG_OPEN_REPARSE_POINT`로 열고 심볼릭 링크·정션이면 거부한다([`open_no_follow`]).

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

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
#[expect(
    clippy::unnecessary_wraps,
    reason = "재결합 호출부의 Result 계약을 유지한다"
)]
pub(crate) fn link_fd(s: krill_proto::os::net::UnixStream) -> std::io::Result<OwnedFd> {
    Ok(OwnedFd::from(s))
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

/// `path`를 연다. 마지막 경로 성분이 심볼릭 링크면 실패한다(`O_NOFOLLOW`, `ELOOP`).
#[cfg(unix)]
pub(crate) fn open_no_follow(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    std::os::unix::fs::OpenOptionsExt::custom_flags(options, libc::O_NOFOLLOW).open(path)
}

/// Windows: reparse point를 따라가지 않고 그 자체를 연 뒤, 이름 대리(심볼릭 링크·정션·마운트 지점)면 닫고
/// `ERROR_STOPPED_ON_SYMLINK`로 실패한다. std `is_symlink`가 그 태그 판정이다. 클라우드 자리표시자 같은 다른 reparse
/// point는 파일 자체라 그대로 쓴다(비수화 `OneDrive` 파일 읽기는 미확인).
#[cfg(windows)]
pub(crate) fn open_no_follow(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    let file = options
        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if file.metadata()?.file_type().is_symlink() {
        let code = windows_sys::Win32::Foundation::ERROR_STOPPED_ON_SYMLINK;
        return Err(io::Error::from_raw_os_error(
            i32::try_from(code).unwrap_or(i32::MAX),
        ));
    }
    Ok(file)
}

/// 디스크에 실제로 잡힌 바이트(희소 파일은 길이보다 작다). Windows는 길이.
pub(crate) fn allocated(meta: &Metadata) -> u64 {
    #[cfg(unix)]
    let bytes = std::os::unix::fs::MetadataExt::blocks(meta).saturating_mul(512);
    #[cfg(windows)]
    let bytes = meta.len();
    bytes
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn open_no_follow_refuses_symlinks() {
        let dir = std::env::temp_dir().join(format!("krilld-nofollow-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("real");
        std::fs::write(&file, b"x").unwrap();
        let link = dir.join("link");
        if let Err(e) = std::os::windows::fs::symlink_file(&file, &link) {
            // 심볼릭 링크 권한(관리자·개발자 모드)이 없는 환경.
            eprintln!("skip: symlink_file {e}");
            return;
        }
        assert!(open_no_follow(OpenOptions::new().read(true), &file).is_ok());
        let err = open_no_follow(OpenOptions::new().read(true), &link).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(681));
        let err =
            open_no_follow(OpenOptions::new().create_new(true).write(true), &link).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&file).unwrap(), b"x");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
