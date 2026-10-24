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
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Selectable as _, Sizable as _};

use super::theme::{
    FONT_MONO, MODAL_BACKDROP, Palette, RADIUS_MD, RADIUS_SM, RADIUS_XS, SWITCH_HEIGHT,
    SWITCH_THUMB, SWITCH_WIDTH, TEXT_2XS, TEXT_SM, TEXT_XS,
};

/// 테두리 있는 카드(섹션·그룹 내용 묶음, Orca `Card` / `Inset hairline` 대응).
///
/// `w_full().min_w_0()`을 기본 포함해 부모 열을 채우면서 내부 긴 텍스트·자식 요소 때문에
/// 부모 폭 밖으로 넘치지 않게 한다(OWE-793).
pub fn card(p: &Palette) -> Div {
    div()
        .w_full()
        .min_w_0()
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

/// [`label`]의 선택 설명 인자 변환 트레이트.
///
/// `Option<&'static str>`(`None` 또는 `Some("...")`)과 동적 문자열(`String`, `SharedString`)을
/// 모두 받아 정적 설정 행과 동적 목록 행(스킬 카드 등)이 같은 2열 라벨 프리미티브를 쓰게 한다(OWE-793).
pub trait LabelDesc {
    fn into_label_desc(self) -> Option<SharedString>;
}

impl LabelDesc for Option<&'static str> {
    fn into_label_desc(self) -> Option<SharedString> {
        self.map(SharedString::from)
    }
}

impl LabelDesc for &'static str {
    fn into_label_desc(self) -> Option<SharedString> {
        (!self.is_empty()).then(|| SharedString::from(self))
    }
}

impl LabelDesc for String {
    fn into_label_desc(self) -> Option<SharedString> {
        (!self.is_empty()).then(|| SharedString::from(self))
    }
}

impl LabelDesc for SharedString {
    fn into_label_desc(self) -> Option<SharedString> {
        (!self.is_empty()).then_some(self)
    }
}

/// 2열 행의 왼쪽 라벨 열(제목 + 선택 설명).
///
/// 행(`justify_between`) 안의 텍스트 열은 flex 기준 폭이 내용 폭이 아니라 0에 가깝게 잡히고,
/// `min_w_0`만 주면 자동 최소 폭마저 사라져 폭 0px로 무너진다. 따라서 라벨 열은 반드시
/// `flex_1().min_w_0()`로 남는 폭을 차지하고, 오른쪽 컨트롤 묶음은 [`controls`]로 `flex_none` 처리한다.
pub fn label(p: &Palette, title: impl Into<SharedString>, desc: impl LabelDesc) -> Div {
    let title: SharedString = title.into();
    let desc = desc.into_label_desc();
    let help = desc.clone().map(|d| help_dot(p, &title, &d));
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .min_w_0()
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(TEXT_SM)
                .font_weight(FontWeight::MEDIUM)
                .child(title)
                .children(help),
        )
        .children(desc.map(|desc| {
            div()
                .min_w_0()
                .text_size(TEXT_XS)
                .text_color(p.muted_foreground)
                .child(desc)
        }))
}

/// 설정 행 제목 옆의 (?) 도움말 점. 설명이 눈에 안 들어오거나 잘릴 때 호버하면
/// 같은 설명 문구를 툴팁으로 다시 보여준다.
fn help_dot(p: &Palette, title: &SharedString, desc: &SharedString) -> Stateful<Div> {
    let tip = desc.clone();
    let debug = format!("label-help:{title}");
    let element_id: ElementId = format!("label-help-{title}").into();
    div()
        .id(element_id)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(14.))
        .rounded_full()
        .border_1()
        .border_color(p.border)
        .text_size(TEXT_2XS)
        .text_color(p.muted_foreground)
        .cursor_pointer()
        .child("?")
        .debug_selector(move || debug.clone())
        .tooltip(move |window, cx| tooltip_view("label-help", tip.clone(), window, cx))
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

/// 대표 긍정 주 동작 버튼(전체 설치·모두 자동 설정·저장 등, Orca `Button variant="default" size="sm"` 대응, OWE-793).
pub fn primary(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).small().primary().label(label)
}

/// 보조 동작 버튼(개별 설치·파일 열기·가져오기·복사 등, Orca `Button variant="outline" size="sm"` 대응).
pub fn secondary(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).small().outline().label(label)
}

/// 가벼운 부가·취소·초기화 동작 버튼(Orca `Button variant="ghost" size="sm"` 대응, OWE-793).
pub fn ghost(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).small().ghost().label(label)
}

/// 불리언(켜기/끄기) 토글 스위치(Orca `SettingsSwitch` / `Switch` 대응, OWE-793).
///
/// 2버튼(`켜기`/`끄기`) 나열 대신 단일 알약 트랙(36×20px)과 원형 썸(14px)으로
/// 현재 활성 상태와 클릭 시 반전 동작을 일관되게 표현한다.
pub fn switch(
    p: &Palette,
    id: impl Into<SharedString>,
    checked: bool,
    disabled: bool,
    on_toggle: impl Fn(bool, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    let thumb_id: SharedString = format!("{id}-thumb").into();
    let thumb_selector = thumb_id.clone();
    let track_bg = if checked { p.primary } else { p.muted };
    let track_border = if checked { p.primary } else { p.border };
    let hover_bg = if checked {
        p.primary.opacity(0.88)
    } else {
        p.accent
    };
    let active_bg = if checked {
        p.primary.opacity(0.75)
    } else {
        p.accent.opacity(0.8)
    };
    let thumb_bg = if checked {
        p.primary_foreground
    } else {
        p.muted_foreground
    };
    div()
        .id(ElementId::Name(id.clone()))
        .debug_selector(move || selector.to_string())
        .role(gpui::Role::Switch)
        .aria_label(id)
        .aria_toggled(if checked {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        })
        .flex_none()
        .w(SWITCH_WIDTH)
        .h(SWITCH_HEIGHT)
        .p(px(2.))
        .rounded_full()
        .border_1()
        .border_color(track_border)
        .bg(track_bg)
        .flex()
        .items_center()
        .when(checked, Styled::justify_end)
        .when(!checked, Styled::justify_start)
        .when(disabled, |d| {
            d.opacity(0.5)
                .cursor_default()
                .aria_description(crate::i18n::catalog::general::CONTROL_UNAVAILABLE.get())
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        })
        .when(!disabled, |d| {
            d.tab_index(0)
                .cursor_pointer()
                .focus_visible(|s| s.border_color(p.ring))
                .hover(|s| {
                    s.bg(hover_bg)
                        .border_color(if checked { hover_bg } else { p.ring })
                })
                .active(|s| s.bg(active_bg))
                .on_click(move |_, window, cx| on_toggle(!checked, window, cx))
        })
        .child(
            div()
                .id(ElementId::Name(thumb_id))
                .debug_selector(move || thumb_selector.to_string())
                .size(SWITCH_THUMB)
                .rounded_full()
                .bg(thumb_bg),
        )
}

/// 다중 선택지(2~4개) 세그먼트 컨트롤 트랙(Orca `SegmentedControl` 대응, OWE-793).
///
/// 개별 버튼이 흩어져 보이지 않도록 테두리 있는 트랙 안에 선택지([`segment`])들을 묶는다.
pub fn segmented_control(p: &Palette, items: impl IntoIterator<Item = impl IntoElement>) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_0p5()
        .p(px(2.))
        .rounded(RADIUS_SM)
        .bg(p.background.opacity(0.5))
        .border_1()
        .border_color(p.border)
        .children(items)
}

/// [`segmented_control`] 안에 들어가는 개별 선택지 버튼(Orca `SettingsSegmentedControl` 대응, OWE-793).
///
/// 활성 항목은 `accent` 바탕 + `accent_foreground` + `MEDIUM`, 비활성 항목은 `muted_foreground` 글자만
/// 있는 `ghost`다. 활성 항목을 `primary`(흰 채움)로 그리면 같은 화면의 주 동작 버튼과 무게가 겹친다.
///
/// OWE-856: `toggled`는 접근성 메타데이터에만 반영되고(스타일 불변), 선택 상태를
/// 보조 기술에 알린다.
pub fn segment(
    p: &Palette,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
) -> Button {
    let btn = Button::new(id)
        .small()
        .ghost()
        .rounded(RADIUS_XS)
        .label(label)
        .toggled(active);
    if active {
        btn.selected(true)
            .bg(p.accent)
            .text_color(p.accent_foreground)
            .font_weight(FontWeight::MEDIUM)
    } else {
        btn.text_color(p.muted_foreground)
    }
}

/// 두 문구 중 넓은 쪽의 공간을 항상 예약하는 버튼(STYLEGUIDE §7.3).
///
/// 스타일·크기·아이콘은 반환한 버튼에 지정한다. 숨긴 문구는 레이아웃에만 참여하고
/// 그리지 않는다. 셀·텍스트에 접근성 역할과 ID를 주지 않아 별도 노드를 만들지 않으며,
/// 버튼의 접근성 라벨은 현재 보이는 문구와 일치한다.
pub fn button_with_reserved_label(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    alternate: impl Into<SharedString>,
    alternate_active: bool,
) -> Button {
    let label = label.into();
    let alternate = alternate.into();
    let cell = |text: SharedString, shown: bool| {
        div()
            .row_start(1)
            .col_start(1)
            .flex()
            .justify_center()
            .when(!shown, Styled::invisible)
            .child(text)
    };
    Button::new(id)
        .accessibility_label(if alternate_active {
            alternate.clone()
        } else {
            label.clone()
        })
        .child(
            div()
                .grid()
                .child(cell(label, !alternate_active))
                .child(cell(alternate, alternate_active)),
        )
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
    button_with_reserved_label(id, label, confirm, armed)
        .small()
        .danger()
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
                .occlude()
                .bg(MODAL_BACKDROP)
                .flex()
                .items_center()
                .justify_center()
                .child(content),
        ),
    )
    .with_priority(1)
}

/// `gpui_component::Root` 없이도 동작하는 모달의 Tab/Shift-Tab 포커스 범위.
/// 원래 요소를 그대로 반환하므로 대화상자의 역할·이름도 접근성 트리에 남는다.
pub fn modal_focus_scope<E>(content: E, focus: &gpui::FocusHandle) -> E
where
    E: StatefulInteractiveElement + 'static,
{
    let scope = focus.clone();
    content
        .track_focus(focus)
        .tab_group()
        .capture_key_down(move |event, window, cx| {
            let modifiers = event.keystroke.modifiers;
            if event.keystroke.key != "tab"
                || modifiers.control
                || modifiers.alt
                || modifiers.platform
                || modifiers.function
            {
                return;
            }
            cx.stop_propagation();
            window.prevent_default();
            let advance = |window: &mut gpui::Window, cx: &mut gpui::App| {
                if modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
            };
            advance(window, cx);
            let first = window.focused(cx);
            while !scope.contains_focused(window, cx) {
                advance(window, cx);
                if window.focused(cx) == first {
                    window.focus(&scope, cx);
                    break;
                }
            }
        })
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

/// OWE-851: 메뉴가 열려 있는 동안(`on == false`)은 호버 툴팁을 달지 않고 접근성
/// 라벨만 둔다. 열린 팝오버 하단 글자를 툴팁이 가리는 문제를 막는다.
pub fn button_with_tooltip_when(
    on: bool,
    button: Button,
    id: impl Into<SharedString>,
    text: impl Into<SharedString>,
) -> Button {
    if on {
        button_with_tooltip(button, id, text)
    } else {
        button.accessibility_label(text.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{DARK, LIGHT};
    use gpui_component::button::ButtonVariants;

    #[gpui::test]
    fn switch_exposes_checked_state_and_unavailable_description(cx: &mut gpui::TestAppContext) {
        use gpui::Element as _;
        for checked in [false, true] {
            for disabled in [false, true] {
                let element = switch(&DARK, "switch-probe", checked, disabled, |_, _, _| {})
                    .aria_label("Notifications");
                let mut node = gpui::accesskit::Node::new(gpui::Role::Unknown);
                element.write_a11y_info(&mut node);
                assert_eq!(element.a11y_role(), Some(gpui::Role::Switch));
                assert_eq!(node.label(), Some("Notifications"));
                assert_eq!(
                    node.toggled(),
                    Some(if checked {
                        gpui::Toggled::True
                    } else {
                        gpui::Toggled::False
                    })
                );
                assert_eq!(node.description().is_some(), disabled);
            }
        }
        cx.run_until_parked();
    }

    #[derive(Default)]
    struct SwitchProbe {
        checked: bool,
        disabled: bool,
        activations: usize,
    }

    impl gpui::Render for SwitchProbe {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let weak = cx.weak_entity();
            div().p_4().child(
                switch(
                    &DARK,
                    "keyboard-switch",
                    self.checked,
                    self.disabled,
                    move |value, _, cx| {
                        weak.update(cx, |this, cx| {
                            this.checked = value;
                            this.activations += 1;
                            cx.notify();
                        })
                        .ok();
                    },
                )
                .aria_label("Notifications"),
            )
        }
    }

    #[gpui::test]
    fn switch_accepts_keyboard_once_and_disabled_ignores_activation(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (view, visual) = cx.add_window_view(|_, _| SwitchProbe::default());
        visual.run_until_parked();
        let bounds = visual.debug_bounds("keyboard-switch").unwrap();
        visual.simulate_click(bounds.center(), gpui::Modifiers::default());
        visual.run_until_parked();
        visual.simulate_keystrokes("space");
        visual.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("space").unwrap(),
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            let state = view.read(cx);
            assert!(!state.checked);
            assert_eq!(state.activations, 2);
        });
        visual.simulate_keystrokes("enter");
        visual.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            view.update(cx, |this, cx| {
                this.disabled = true;
                cx.notify();
            });
        });
        visual.run_until_parked();
        visual.simulate_click(bounds.center(), gpui::Modifiers::default());
        visual.simulate_keystrokes("space enter");
        visual.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert!(view.read(cx).checked);
            assert_eq!(view.read(cx).activations, 3);
        });
    }

    struct ReservedLabelProbe {
        label: SharedString,
        alternate: SharedString,
        busy: bool,
        danger: bool,
    }

    struct ModalScopeProbe {
        scope: gpui::FocusHandle,
        first: gpui::FocusHandle,
        last: gpui::FocusHandle,
        outside: gpui::FocusHandle,
    }

    impl gpui::Render for ModalScopeProbe {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let control = |id, focus: &gpui::FocusHandle| {
                div()
                    .id(id)
                    .track_focus(focus)
                    .tab_index(0)
                    .on_click(|_, _, _| {})
                    .child(id)
            };
            div()
                .tab_group()
                .child(control("outside", &self.outside))
                .child(modal_focus_scope(
                    div()
                        .id("scope")
                        .role(gpui::Role::Dialog)
                        .aria_label("Test dialog")
                        .child(control("first", &self.first))
                        .child(control("last", &self.last)),
                    &self.scope,
                ))
        }
    }

    #[gpui::test]
    fn modal_scope_keeps_tab_inside_without_component_root_and_preserves_role(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::Element as _;
        let (view, visual) = cx.add_window_view(|_, cx| ModalScopeProbe {
            scope: cx.focus_handle(),
            first: cx.focus_handle().tab_stop(true),
            last: cx.focus_handle().tab_stop(true),
            outside: cx.focus_handle().tab_stop(true),
        });
        visual.run_until_parked();
        visual.update(|window, cx| {
            let state = view.read(cx);
            let element = modal_focus_scope(
                div()
                    .id("semantics")
                    .role(gpui::Role::Dialog)
                    .aria_label("Test dialog"),
                &state.scope,
            );
            let mut node = gpui::accesskit::Node::new(gpui::Role::Unknown);
            element.write_a11y_info(&mut node);
            assert_eq!(element.a11y_role(), Some(gpui::Role::Dialog));
            assert_eq!(node.label(), Some("Test dialog"));
            let first = state.first.clone();
            window.focus(&first, cx);
        });
        visual.simulate_keystrokes("tab");
        visual.update(|window, cx| assert!(view.read(cx).last.is_focused(window)));
        visual.simulate_keystrokes("tab");
        visual.update(|window, cx| assert!(view.read(cx).first.is_focused(window)));
        visual.simulate_keystrokes("shift-tab");
        visual.update(|window, cx| assert!(view.read(cx).last.is_focused(window)));
    }

    impl gpui::Render for ReservedLabelProbe {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let button = button_with_reserved_label(
                "reserved-label",
                self.label.clone(),
                self.alternate.clone(),
                self.busy,
            )
            .small()
            .when(self.danger, ButtonVariants::danger)
            .when(!self.danger, ButtonVariants::primary)
            .debug_selector(|| "reserved-label".into());
            div()
                .w(px(600.))
                .flex()
                .justify_end()
                .gap_2()
                .child(ghost("reserved-close", "닫기").debug_selector(|| "reserved-close".into()))
                .child(button)
        }
    }

    /// 짧아지는 진행 문구와 길어지는 진행 문구 모두 실제 버튼 폭과 옆 버튼 위치를 유지한다.
    #[gpui::test]
    fn reserved_label_keeps_button_and_sibling_bounds(cx: &mut gpui::TestAppContext) {
        use crate::i18n::{Language, catalog};

        cx.update(gpui_component::init);
        let pairs = [
            (
                catalog::artifacts::SHARE_BUTTON,
                catalog::artifacts::SHARING,
            ),
            (
                catalog::pull_request::AI_GENERATE,
                catalog::pull_request::AI_GENERATING,
            ),
            (
                catalog::pull_request::SUBMIT,
                catalog::pull_request::SUBMITTING,
            ),
        ];
        for language in [Language::Ko, Language::En] {
            for (label, alternate) in pairs {
                // 순서를 바꿔 넓은 쪽이 어느 상태든 같은 공간을 예약하는지 확인한다.
                for reverse in [false, true] {
                    let mut labels = [label.in_language(language), alternate.in_language(language)];
                    if reverse {
                        labels.swap(0, 1);
                    }
                    let (view, vcx) = cx.add_window_view(|_, _| ReservedLabelProbe {
                        label: labels[0].into(),
                        alternate: labels[1].into(),
                        busy: false,
                        danger: reverse,
                    });
                    vcx.run_until_parked();
                    let before = vcx.debug_bounds("reserved-label").expect("버튼 bounds");
                    let close_before = vcx.debug_bounds("reserved-close").expect("닫기 bounds");
                    assert!(before.size.width > px(0.));
                    view.update(vcx, |probe, cx| {
                        probe.busy = true;
                        cx.notify();
                    });
                    vcx.run_until_parked();
                    assert_eq!(vcx.debug_bounds("reserved-label"), Some(before));
                    assert_eq!(vcx.debug_bounds("reserved-close"), Some(close_before));
                }
            }
        }
    }

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

    struct SegmentProbe;
    impl gpui::Render for SegmentProbe {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            // `Button::render`는 렌더 중인 뷰 문맥을 쓰므로 뷰 안에서 그린다.
            gpui::div()
                .debug_selector(|| "seg-probe-root".into())
                .child(super::segmented_control(
                    &DARK,
                    [
                        super::segment(&DARK, "seg-a", "worktree", true),
                        super::segment(&DARK, "seg-b", "index", false),
                    ],
                ))
        }
    }

    /// OWE-856: 세그먼트 선택지는 보조 기술에 토글 상태로 노출된다.
    /// 토글→`aria_toggled` 매핑은 gpui-component이 보장하므로, 여기서는 활성 항목만
    /// 선택 스타일(`accent` 채움)을 갖고 두 상태가 모두 그려지는지를 잠근다.
    #[gpui::test]
    fn segment_buttons_expose_toggle_state(cx: &mut gpui::TestAppContext) {
        for active in [false, true] {
            assert_eq!(
                super::segment(&DARK, "seg-probe", "worktree", active).is_selected(),
                active
            );
        }
        cx.update(gpui_component::init);
        let window = cx.add_window(|_, _| SegmentProbe);
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        // 두 상태의 세그먼트가 한 컨트롤 안에 그려졌으면 렌더 경로가 동작한 것이다.
        assert!(vcx.debug_bounds("seg-probe-root").is_some());
    }
}
