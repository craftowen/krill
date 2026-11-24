//! 앱 라이브러리: `krill-ui`와 `krill-app-core`를 재노출하고 봇 UI(`bots`)·모델 목록(`agent_models`)을 제공한다.
//! gpui 메인 셸은 바이너리(`main.rs` + `shell/`)에 있다.

pub use krill_app_core::*;

pub mod agent_models;
pub mod bots;
