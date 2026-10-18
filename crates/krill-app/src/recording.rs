//! 세그먼트 기록 초기화 실패 경고. 데몬 연결이 수립될 때 백그라운드에서 확인한다.

use gpui::{Div, Stateful, div, prelude::*};
use gpui_component::{Icon, Sizable as _};

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
        let p = crate::ui::palette(cx);
        let tip = crate::i18n::catalog::recording::WARNING.get();
        div()
            .id("status-recording-warning")
            .debug_selector(|| "status-recording-warning".into())
            .aria_label(tip)
            .flex()
            .items_center()
            .justify_center()
            .px_1()
            .py_0p5()
            .rounded(crate::ui::RADIUS_XS)
            .text_color(p.warning)
            .child(
                Icon::default()
                    .data(crate::ui::icons::TRIANGLE_ALERT_SVG)
                    .xsmall()
                    .text_color(p.warning),
            )
            .tooltip(move |window, cx| {
                crate::ui::tooltip_view("status-recording-warning", tip, window, cx)
            })
    })
}
