#![deny(clippy::indexing_slicing, clippy::print_stdout, clippy::print_stderr)]
//! krill-holder: 에이전트·셸의 부모이자 PTY master fd 보관자(ARCHITECTURE §1·§4, T2 §3.4).
//!
//! 하는 일은 이것뿐이다: PTY를 열어 세션을 spawn(setsid + `TIOCSCTTY`), `waitpid`로 종료 상태 수집,
//! master fd와 krilld의 listen 소켓 보관, krilld 기동·재기동(첫 크래시 즉시, 반복 크래시만 백오프)과
//! `SCM_RIGHTS` fd 재전달, 자기 exec 핸드오프, 세션 종료 단계(탈출한 후손 스윕 포함)와 세션 상한. PTY를 읽지 않는다. panic 금지(CONVENTIONS P4).
//!
//! krilld와의 내부 프로토콜(v1, 동결)은 [`krill_proto::holder`], 전송로는 `link`(Unix 전용, Windows 계약은 `windows.rs`).

#[cfg(unix)]
mod holder;
#[cfg(unix)]
pub mod link;
#[cfg(windows)]
#[path = "link/windows.rs"]
pub mod link;
#[cfg(unix)]
mod sys;
#[cfg(windows)]
#[path = "sys/windows.rs"]
mod sys;
#[cfg(unix)]
mod teardown;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{close_inherited, run};
#[cfg(unix)]
pub use {holder::run, sys::close_inherited};
