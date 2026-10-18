//! Windows holder 계약(M11 Phase 1, OWE-816) — 컴파일 전용 스텁. 이 파일은 Windows 빌드 줄 수에만 든다.
//!
//! - [`run`]은 [`io::ErrorKind::Unsupported`]. `ConPTY` holder(B안: conpty.dll + `OpenConsole` 번들,
//!   docs/research/windows-spike-OWE-826.md)는 Phase 3에서 이 자리에 들어온다.
//! - [`close_inherited`]는 `Ok`. Windows는 띄운 쪽이 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`로 상속 핸들을 고른다.
//! - `link`(`Link`·`Packet`)는 Windows에 **없다**. `OwnedFd`·`SCM_RIGHTS` 전송로라 같은 모양의 스텁이 의미가
//!   없다. krilld의 holder 링크 경로(`holder.rs`·`rejoin.rs`·`registry.rs`)는 `cfg(unix)`로 막는다(OWE-820).
//!   Windows 전송로(핸들 값 전달 + `DuplicateHandle`, krilld가 `ConPTY` signal 핸들 사본을 쥐어 holder 크래시에도
//!   세션 생존)는 Phase 3(OWE-832)에서 새 `Link`로 만든다.

use std::ffi::OsString;
use std::io;

/// Windows holder(`ConPTY`)는 아직 없다.
pub fn run(_args: impl Iterator<Item = OsString>) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// 띄운 쪽에게서 새어 들어오는 핸들이 없어 할 일이 없다.
#[expect(
    clippy::unnecessary_wraps,
    reason = "Unix 판과 같은 시그니처(main의 detach가 함께 쓴다)"
)]
pub fn close_inherited() -> io::Result<()> {
    Ok(())
}
