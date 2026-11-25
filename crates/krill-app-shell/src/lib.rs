//! Krill 데스크톱 메인 셸(3열 레이아웃·탭·분할·채팅·봇 패널·창 수명주기) 크레이트.

mod app_host;
mod shell;

#[cfg(target_os = "macos")]
pub(crate) use app_host::record_test_window_geometry;
pub use app_host::run;
pub(crate) use app_host::set_menus;

#[cfg(test)]
mod lifecycle_tests;
