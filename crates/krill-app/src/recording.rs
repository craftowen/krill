//! 세그먼트 기록 초기화 실패 경고. 데몬 연결이 수립될 때 백그라운드에서 확인한다.

use gpui::{Div, Stateful, div, prelude::*};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Ok,
    Unavailable,
}

pub fn check() -> Status {
    if krill_proto::daemon::state_dir().is_some_and(|root| {
        root.join(krill_proto::history::RECORDING_WARNING_FILE)
            .exists()
    }) {
        Status::Unavailable
    } else {
        Status::Ok
    }
}

pub fn warning(status: Status, cx: &gpui::App) -> Option<Stateful<Div>> {
    (status == Status::Unavailable).then(|| {
        div()
            .id("status-recording-warning")
            .debug_selector(|| "status-recording-warning".into())
            .text_color(crate::ui::palette(cx).warning)
            .child(crate::i18n::catalog::recording::WARNING.get())
    })
}
