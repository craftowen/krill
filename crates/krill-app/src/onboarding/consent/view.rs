//! 동의 화면 gpui 컴포넌트(OWE-690, MK-129, OWE-714). 판정·기록은 부모 [`super`]에 있다.
//!
//! 온보딩 첫 화면 안에 얹는 내용 뷰다: 요약 3줄 + [자세히 보기](고지 5개 전문) + 동의 체크박스.
//! 체크박스는 항상 해제 상태로 시작한다. [시작하기]·[종료]·[나중에]는 마법사 고정 푸터가 맡고,
//! 여기서는 체크 상태만 들고 있다([`ConsentView::accepted`]).

use std::path::{Path, PathBuf};

use gpui::{Context, Render, Window, div, prelude::*};
use gpui_component::IconName;
use gpui_component::checkbox::Checkbox;

use crate::i18n::{Message, catalog::consent as text};
use crate::ui;

/// 고지 항목 `(제목, 상세)`(저장·hook·네트워크·데몬·라이선스).
const SECTIONS: [(Message, Message); 5] = [
    (text::LOCAL_TITLE, text::LOCAL_DETAIL),
    (text::HOOKS_TITLE, text::HOOKS_DETAIL),
    (text::NETWORK_TITLE, text::NETWORK_DETAIL),
    (text::DAEMON_TITLE, text::DAEMON_DETAIL),
    (text::LICENSE_TITLE, text::LICENSE_DETAIL),
];

/// 첫 화면에 늘 보이는 요약 3줄.
const SUMMARY: [Message; 3] = [
    text::SUMMARY_LOCAL,
    text::SUMMARY_HOOKS,
    text::SUMMARY_DAEMON,
];

/// 항목 상세. 로컬 저장 항목에만 실제 상태 디렉터리 경로를 채워 보여준다.
fn detail(message: Message, state: &Path) -> String {
    message.format(&[("state", &state.display().to_string())])
}

#[derive(Debug)]
pub struct ConsentView {
    state: PathBuf,
    /// [자세히 보기]로 고지 전문을 펼쳤나.
    more: bool,
    accepted: bool,
}

impl ConsentView {
    pub fn new(state: PathBuf) -> Self {
        Self {
            state,
            more: false,
            accepted: false,
        }
    }

    /// 동의 체크박스 상태. 새 뷰는 항상 `false`다.
    pub fn accepted(&self) -> bool {
        self.accepted
    }
}

impl Render for ConsentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let summary = SUMMARY.iter().map(|line| {
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .mt_1p5()
                        .size_1()
                        .flex_none()
                        .rounded_full()
                        .bg(p.muted_foreground),
                )
                .child(div().flex_1().min_w_0().child(line.get()))
        });
        let more = self.more;
        div()
            .id("consent")
            .debug_selector(|| "consent-dialog".into())
            .role(gpui::Role::Group)
            .aria_label(text::TITLE.get())
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .children(summary),
            )
            .child(
                // 열 정렬에서 늘어나지 않게 글자 폭만 차지한다(전체 폭 + 가운데 정렬 방지).
                div()
                    .flex_none()
                    .self_start()
                    .debug_selector(|| "consent-more".into())
                    .child(
                        ui::ghost(
                            "consent-more",
                            if more {
                                text::LESS.get()
                            } else {
                                text::MORE.get()
                            },
                        )
                        .icon(if more {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.more = !s.more;
                            cx.notify();
                        })),
                    ),
            )
            // 접혀 있으면 자리를 차지하지 않는다(빈 공백 방지). 스크롤은 마법사 본문이 맡는다.
            .when(more, |d| {
                d.child(
                    div()
                        .id("consent-sections")
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .children(SECTIONS.iter().enumerate().map(|(index, (title, body))| {
                            div()
                                .debug_selector(move || format!("consent-detail-{index}"))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(title.get()),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .font_family(ui::FONT_MONO)
                                        .text_size(ui::TEXT_XS)
                                        .text_color(p.muted_foreground)
                                        .child(detail(*body, &self.state)),
                                )
                        })),
                )
            })
            .child(
                div()
                    .flex_none()
                    .debug_selector(|| "consent-accept".into())
                    .child(
                        Checkbox::new("consent-checkbox")
                            .label(text::ACCEPT.get())
                            .checked(self.accepted)
                            .on_click(cx.listener(|s, checked: &bool, _, cx| {
                                s.accepted = *checked;
                                cx.notify();
                            })),
                    ),
            )
    }
}
