//! Krill UI 디자인 시스템·다국어(i18n)·플랫폼 계층·공용 프리미티브 크레이트.

pub mod admin_tab;
pub mod bot_types;
pub mod commands;
pub mod i18n;
pub mod keys;
pub mod local_time;
pub mod platform;
pub mod process_io;
#[cfg(target_os = "macos")]
pub mod responsiveness;
pub mod tab_color;
pub mod terminal_font;
pub mod ui;

pub use local_time::chat_message_time;
