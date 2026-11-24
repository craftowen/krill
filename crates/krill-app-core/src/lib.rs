//! Krill 앱 코어 도메인·패널·설정·사이드바·터미널·업데이터 크레이트.
//!
//! `krill-ui`(디자인 시스템·i18n·플랫폼) 위에 올라가고 `krill-app`(봇 UI·메인 셸·실행 파일) 아래에 놓여,
//! UI 수정 시 증분 컴파일과 릴리스 버전 범프 시 캐시 보존을 함께 확보한다.

#[cfg(target_os = "macos")]
pub use krill_ui::responsiveness;
pub use krill_ui::{
    chat_message_time, commands, i18n, keys, local_time, platform, process_io, terminal_font, ui,
};

pub mod accounts;
pub mod add_project;
pub mod agent_history;
pub mod alert;
pub mod appearance;
pub mod artifacts;
pub mod automations;
pub mod continue_session;
pub mod crash;
pub mod detached;
pub mod diff;
pub mod echo;
pub mod editor;
pub mod errors;
pub mod file_format;
pub mod hosts;
pub mod jump;
pub mod layout;
pub mod link;
pub mod link_ui;
pub mod machines;
pub mod menu_bar;
pub mod onboarding;
pub mod open_in;
pub mod pane;
pub mod pull_request;
pub mod recording;
pub mod remote_fs;
pub mod resources;
pub mod search;
pub mod security;
pub mod settings;
pub mod sidebar;
pub mod ssh_hosts;
pub mod star_nag;
pub mod telemetry;
pub mod terminal;
pub mod updater;
pub mod voice;
pub mod worktree;
