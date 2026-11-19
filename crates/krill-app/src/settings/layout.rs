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

/// 되돌리기 어려운 동작을 일반 동작과 떼어 놓는 점선 구역.
pub(super) fn danger_zone(p: &crate::ui::Palette) -> gpui::Div {
    use gpui::{Styled as _, div};
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_2()
        .pt_2p5()
        .border_t_1()
        .border_dashed()
        .border_color(p.destructive.opacity(0.4))
}

/// 진행 문구를 띄우기 전에 기다리는 시간(STYLEGUIDE §7.3 `1s ~ 3s`).
pub(super) const SLOW_FEEDBACK: std::time::Duration = std::time::Duration::from_secs(1);

/// 본문이 좁아 부가 설명을 접는 모달 크기인지.
pub(super) fn small_body(window: &gpui::Window) -> bool {
    crate::ui::page_modal_size(window.viewport_size()).width < window.rem_size() * 48.
}

/// 값이 오기 전 최종 모양을 잡아 두는 빈 자리(STYLEGUIDE §7.0).
pub(super) fn blank(p: &crate::ui::Palette, width: impl Into<gpui::Length> + Clone) -> gpui::Div {
    use gpui::Styled as _;
    gpui::div()
        .flex_none()
        .w(width)
        .h(gpui::px(9.))
        .rounded(crate::ui::RADIUS_XS)
        .bg(p.muted)
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

    struct GroupProbe;

    impl gpui::Render for GroupProbe {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let p = &crate::ui::DARK;
            div().w(px(300.)).child(
                crate::ui::group(
                    p,
                    ["group-row-a", "group-row-b"].map(|selector| {
                        crate::ui::group_row()
                            .debug_selector(|| selector.into())
                            .child(label(p, "Appearance", Some("Choose a theme")))
                            .child(controls().child(div().w(px(60.)).h(px(20.))))
                    }),
                )
                .debug_selector(|| "group".into()),
            )
        }
    }

    #[gpui::test]
    fn group_draws_one_divider_between_rows_inside_its_border(cx: &mut gpui::TestAppContext) {
        let (_, visual) = cx.add_window_view(|_, _| GroupProbe);
        visual.run_until_parked();
        let group = visual.debug_bounds("group").expect("group");
        let a = visual.debug_bounds("group-row-a").expect("row a");
        let b = visual.debug_bounds("group-row-b").expect("row b");
        assert_eq!(group.size.width, px(300.));
        assert_eq!(b.top() - a.bottom(), px(1.));
        assert_eq!(
            group.bottom() - b.bottom(),
            px(1.),
            "no divider after last row"
        );
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
