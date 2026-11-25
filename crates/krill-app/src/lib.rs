//! 앱 라이브러리: `krill-ui`·`krill-app-core`·`krill-bots-ui`를 재노출한다.
//! gpui 메인 셸은 바이너리(`main.rs` + `shell/`)에 있다.

pub use krill_app_core::*;
pub use krill_bots_ui::{agent_models, bots};
