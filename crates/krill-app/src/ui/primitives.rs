//! Orca 디자인 시스템 기반 공용 UI 프리미티브(카드·행·라벨·컨트롤·키캡·배지·오류 배너·모달 배경).
//!
//! 구조와 시각 규칙은 `.ref/orca/docs/STYLEGUIDE.md`,
//! `.ref/orca/src/renderer/src/components/settings/SettingsFormControls.tsx`,
//! `.ref/orca/src/renderer/src/components/ShortcutKeyCombo.tsx`,
//! `.ref/orca/src/renderer/src/components/ui/{button,badge,card,dialog}.tsx`를 참고해 순수 Rust/gpui로 옮겼다.
//! 출처·저작권: Copyright (c) 2026 Lovecast Inc. (MIT License, `.ref/orca/LICENSE`).

use gpui::{
    Deferred, Div, ElementId, FontWeight, Hsla, IntoElement, Pixels, SharedString, Stateful,
    Styled as _, anchored, deferred, div, point, prelude::*, px,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};

use super::theme::{
    FONT_MONO, MODAL_BACKDROP, Palette, RADIUS_MD, RADIUS_SM, RADIUS_XS, TEXT_2XS, TEXT_SM, TEXT_XS,
};

/// 테두리 있는 카드(섹션·그룹 내용 묶음, Orca `Card` / `Inset hairline` 대응).
pub fn card(p: &Palette) -> Div {
    div()
        .p_3()
        .rounded(RADIUS_MD)
        .bg(p.card)
        .border_1()
        .border_color(p.border)
}

/// 2열 설정·폼 행(Orca `SettingsRow` 대응): 왼쪽 [`label`], 오른쪽 [`controls`].
pub fn row(p: &Palette) -> Div {
    card(p).flex().items_center().justify_between().gap_3()
}

/// 2열 행의 왼쪽 라벨 열(제목 + 선택 설명).
///
/// 행(`justify_between`) 안의 텍스트 열은 flex 기준 폭이 내용 폭이 아니라 0에 가깝게 잡히고,
/// `min_w_0`만 주면 자동 최소 폭마저 사라져 폭 0px로 무너진다. 따라서 라벨 열은 반드시
/// `flex_1().min_w_0()`로 남는 폭을 차지하고, 오른쪽 컨트롤 묶음은 [`controls`]로 `flex_none` 처리한다.
pub fn label(p: &Palette, title: impl Into<SharedString>, desc: Option<&'static str>) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_size(TEXT_SM)
                .font_weight(FontWeight::MEDIUM)
                .child(title.into()),
        )
        .children(desc.map(|desc| {
            div()
                .text_size(TEXT_XS)
                .text_color(p.muted_foreground)
                .child(desc)
        }))
}

/// 세로 스택 입력 폼(대화상자 등)에서 `<Input>` 위에 놓는 필드 라벨과 선택 꼬리표.
pub fn field_label(name: &'static str, optional: Option<&'static str>, p: &Palette) -> Div {
    div()
        .flex()
        .gap_1()
        .text_size(TEXT_XS)
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.muted_foreground)
        .child(name)
        .children(optional.map(|opt| div().opacity(0.7).child(opt)))
}

/// 2열 행의 오른쪽 컨트롤 묶음(`shrink-0` / `flex_none`). 라벨 열에 밀려 줄어들지 않는다.
pub fn controls() -> Div {
    div().flex_none().flex().items_center().gap_1p5()
}

/// 카드가 아닌 섹션 머리글(Orca `SettingsSubsectionHeader` 대응).
pub fn section_title(p: &Palette, id: &'static str, title: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .pt_2()
        .pb_1()
        .border_b_1()
        .border_color(p.border)
        .text_size(TEXT_SM)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.muted_foreground)
        .child(title)
}

/// 보조 동작 버튼(파일 열기·가져오기·복사 등, Orca `Button variant="outline" size="sm"` 대응).
pub fn secondary(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).small().outline().label(label)
}

/// 2단계 확인이 있는 파괴적 동작 버튼(Orca UX Rule 1 공간 사전 예약 적용).
///
/// 기본 문구(`label`)와 무장 문구(`confirm`)를 같은 1×1 grid 칸에 겹쳐 그려 폭을 넓은 쪽으로 고정한다.
/// 무장(`armed`) 상태가 되어도 버튼 폭이 변하지 않아 옆 버튼을 덮거나 밀어내지 않는다.
pub fn danger_confirm(
    id: impl Into<ElementId>,
    label: &'static str,
    confirm: &'static str,
    armed: bool,
) -> Button {
    let cell = |text: &'static str, shown: bool| {
        div()
            .row_start(1)
            .col_start(1)
            .flex()
            .justify_center()
            .when(!shown, Styled::invisible)
            .child(text)
    };
    Button::new(id)
        .small()
        .danger()
        .accessibility_label(if armed { confirm } else { label })
        .child(
            div()
                .grid()
                .child(cell(label, !armed))
                .child(cell(confirm, armed)),
        )
}

/// 단축키 조합을 키캡 모양으로 그린다(Orca `ShortcutKeyCombo` / `KeyCap` 대응).
pub fn keycap(p: &Palette, keys: impl Into<SharedString>) -> Div {
    div()
        .px_1p5()
        .py(px(1.))
        .rounded(RADIUS_XS)
        .bg(p.muted)
        .border_1()
        .border_color(p.border)
        .font_family(FONT_MONO)
        .text_size(TEXT_XS)
        .child(keys.into())
}

/// 상태 알약 배지 톤(Orca `SettingsBadge` / `Badge` 대응).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeTone {
    Neutral,
    Accent,
    Muted,
    Success,
    Warning,
    Destructive,
}

impl BadgeTone {
    /// 주어진 팔레트에서 `(배경색, 테두리색, 글자색)` 3원소를 돌려준다.
    #[must_use]
    pub fn colors(self, p: &Palette) -> (Hsla, Hsla, Hsla) {
        match self {
            Self::Neutral => (
                p.background.opacity(0.5),
                p.border.opacity(0.5),
                p.foreground.opacity(0.8),
            ),
            Self::Accent => (
                p.foreground.opacity(0.10),
                p.foreground.opacity(0.20),
                p.foreground,
            ),
            Self::Muted => (
                p.muted.opacity(0.30),
                p.border.opacity(0.40),
                p.muted_foreground,
            ),
            Self::Success => (p.success.opacity(0.12), p.success.opacity(0.30), p.success),
            Self::Warning => (p.warning.opacity(0.12), p.warning.opacity(0.30), p.warning),
            Self::Destructive => (
                p.destructive.opacity(0.12),
                p.destructive.opacity(0.30),
                p.destructive,
            ),
        }
    }
}

/// 시맨틱 상태 알약 배지(Orca `SettingsBadge` 대응).
pub fn badge(p: &Palette, tone: BadgeTone, text: impl Into<SharedString>) -> Div {
    let (bg, border, fg) = tone.colors(p);
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_1p5()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_size(TEXT_2XS)
        .font_weight(FontWeight::MEDIUM)
        .text_color(fg)
        .child(text.into())
}

/// 인라인 오류 상자(Orca `border-destructive/30 bg-destructive/10 text-destructive` 대응).
pub fn error_banner(p: &Palette, message: impl Into<SharedString>) -> Div {
    div()
        .px_3()
        .py_2()
        .rounded(RADIUS_SM)
        .border_1()
        .border_color(p.destructive.opacity(0.3))
        .bg(p.destructive.opacity(0.1))
        .text_size(TEXT_XS)
        .text_color(p.destructive)
        .child(message.into())
}

/// 창 전체를 덮는 중앙 정렬 모달 오버레이(Orca `DialogOverlay` `fixed inset-0 bg-black/55` 대응).
pub fn modal_backdrop(viewport: gpui::Size<Pixels>, content: impl IntoElement) -> Deferred {
    deferred(
        anchored().position(point(px(0.), px(0.))).child(
            div()
                .w(viewport.width)
                .h(viewport.height)
                .bg(MODAL_BACKDROP)
                .flex()
                .items_center()
                .justify_center()
                .child(content),
        ),
    )
    .with_priority(1)
}

/// 상태바·툴바 요소의 마우스 오버 툴팁 뷰를 생성한다(OWE-788).
///
/// `gpui_base::Root` 없이 `Shell`을 루트로 여는 창에서도 GPUI 기본 툴팁 파이프라인으로 그려지며,
/// 테스트에서는 `tooltip:{id}` 및 `tooltip-text:{id}:{text}` 선택자로 노출 여부와 문구를 검증한다.
pub fn tooltip_view(
    id: impl Into<SharedString>,
    text: impl Into<SharedString>,
    window: &mut gpui::Window,
    cx: &mut gpui::App,
) -> gpui::AnyView {
    let id: SharedString = id.into();
    let text: SharedString = text.into();
    gpui_component::tooltip::Tooltip::element(move |_, _| {
        let id = id.clone();
        let text = text.clone();
        let text_id = id.clone();
        let text_val = text.clone();
        div().debug_selector(move || format!("tooltip:{id}")).child(
            div()
                .debug_selector(move || format!("tooltip-text:{text_id}:{text_val}"))
                .child(text),
        )
    })
    .build(window, cx)
}

/// [`Button`]에 GPUI 기본 마우스 오버 툴팁과 접근성 라벨을 함께 연결한다(OWE-788).
pub fn button_with_tooltip(
    mut button: Button,
    id: impl Into<SharedString>,
    text: impl Into<SharedString>,
) -> Button {
    let id: SharedString = id.into();
    let text: SharedString = text.into();
    let tip_id = id;
    let tip_text = text.clone();
    button
        .interactivity()
        .tooltip(move |window, cx| tooltip_view(tip_id.clone(), tip_text.clone(), window, cx));
    button.tooltip(text.clone()).accessibility_label(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{DARK, LIGHT};

    #[test]
    fn badge_tones_derive_distinct_semantic_colors_in_light_and_dark() {
        for p in [&LIGHT, &DARK] {
            let (_, _, neutral_fg) = BadgeTone::Neutral.colors(p);
            let (_, _, success_fg) = BadgeTone::Success.colors(p);
            let (_, _, warning_fg) = BadgeTone::Warning.colors(p);
            let (_, _, danger_fg) = BadgeTone::Destructive.colors(p);
            assert_eq!(success_fg, p.success);
            assert_eq!(warning_fg, p.warning);
            assert_eq!(danger_fg, p.destructive);
            assert_ne!(neutral_fg, danger_fg);
        }
    }
}
