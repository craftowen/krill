#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::as_conversions
)]
//! 와이어 포맷: 프레임·메시지·키프레임. 엔진·UI 무관.

pub mod account_login;
pub mod agent;
pub mod agent_history;
pub mod artifacts;
pub mod clipboard;
pub mod config_document;
pub mod control;
pub mod daemon;
pub mod diag;
pub mod doctor;
pub mod error_reporting;
pub mod git;
pub mod history;
pub mod holder;
pub mod hook;
pub mod hook_install;
pub mod input;
pub mod ipc;
pub mod mobile;
pub mod orchestration;
pub mod os;
pub mod path;
pub mod remote_fs;
pub mod resources;
pub mod roles;
pub mod schedule;
pub mod screen;
pub mod search;
pub mod statusline;
pub mod telemetry;
pub mod term_modes;
pub mod theme;
pub mod time;
pub mod usage;
pub mod user_dirs;
pub mod worktree_location;
pub mod worktree_space;

/// krilld가 매기는 세션 식별자. holder는 그대로 되돌려 줄 뿐 해석하지 않는다.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct SessionId(pub u64);

/// Hello·업그레이드에서 공유하는 빌드 식별자. 버전이 같아도 소스가 바뀌면 데몬을 교체한다.
pub const BUILD: &str = env!("KRILL_DEV_BUILD");
