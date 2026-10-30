#![deny(clippy::indexing_slicing, clippy::print_stdout, clippy::print_stderr)]
//! krilld: 세션 데몬. 세션마다 Term(핫 1,000행)과 PTY 리더 스레드를 둔다(ARCHITECTURE §4). 클라이언트는 UDS 하나로 붙는다([`ipc`]).

// 단위 테스트도 데몬과 같은 할당기에서 돌아, 힙 계측 테스트가 실제 크기 등급을 본다.
#[cfg(all(test, not(windows)))]
#[global_allocator]
static TEST_ALLOCATOR: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

pub mod agent;
mod agent_history;
pub mod alloc;
mod appearance;
pub mod attach;
mod child;
mod cli;
pub mod git;
pub mod holder;
pub mod ipc;
pub mod mobile;
pub mod monitor;
mod os;
pub mod registry;
pub mod rejoin;
pub mod remote_fs;
mod resources;
pub mod scope;
pub mod search;
mod session;
mod shell;
pub mod status;
pub mod store;
mod sys;
mod term;
mod upgrade;
pub mod usage;
mod watch;
pub mod webhook;

pub use appearance::{Appearance, CellSize};
pub use session::{Gap, PARK_GRACE, Session};
#[cfg(windows)]
pub use shell::run_windows_shell;
pub use shell::{Launch, Shell};
pub use store::{Recorder, Store};
#[cfg(windows)]
pub use sys::run_console_probe;
pub use term::HOT_LINES;
