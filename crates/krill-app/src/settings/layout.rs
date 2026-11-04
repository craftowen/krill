//! 설정 창 공통 레이아웃 조각(OWE-651).
//!
//! 실구현은 디자인 시스템 공용 모듈([`crate::ui::primitives`])에 있으며,
//! 설정 탭과 대화상자·온보딩·사이드바가 모두 동일한 카드·행·라벨·컨트롤·키캡·위험 버튼 프리미티브를 공유한다.

pub(super) use crate::ui::primitives::{
    card, controls, danger_confirm as danger, ghost, keycap, primary, row, secondary,
    section_title, segment, segmented_control, switch,
};

/// Prefer half of a horizontal row before moving controls to the next line.
/// Percentage basis falls back to content size in an auto-height column, so the
/// same settings label can still head a vertically stacked card.
pub(super) fn label(
    p: &crate::ui::Palette,
    title: impl Into<gpui::SharedString>,
    desc: impl crate::ui::primitives::LabelDesc,
) -> gpui::Div {
    use gpui::Styled as _;
    crate::ui::primitives::label(p, title, desc).flex_basis(gpui::relative(0.5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{IntoElement, div, prelude::*, px};

    struct LayoutProbe;

    impl gpui::Render for LayoutProbe {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let p = &crate::ui::DARK;
            div()
                .w(px(300.))
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    row(p)
                        .child(
                            label(p, "Appearance", Some("Choose a theme"))
                                .debug_selector(|| "row-label".into()),
                        )
                        .child(
                            controls().child(
                                div()
                                    .w(px(240.))
                                    .h(px(28.))
                                    .debug_selector(|| "wide-control".into()),
                            ),
                        ),
                )
                .child(
                    card(p).flex().flex_col().child(
                        label(p, "Appearance", Some("Choose a theme"))
                            .debug_selector(|| "column-label".into()),
                    ),
                )
        }
    }

    #[gpui::test]
    fn settings_label_wraps_before_collapsing_and_keeps_natural_column_height(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_, visual) = cx.add_window_view(|_, _| LayoutProbe);
        visual.run_until_parked();
        let label = visual.debug_bounds("row-label").expect("row label");
        let control = visual.debug_bounds("wide-control").expect("control");
        assert!(label.size.width >= px(240.));
        assert!(label.bottom() <= control.top());
        let column = visual.debug_bounds("column-label").expect("column label");
        assert!(column.size.height < px(100.));
    }
}
