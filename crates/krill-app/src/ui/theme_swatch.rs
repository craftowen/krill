//! 모양 탭의 테마 견본. 테마의 실제 팔레트와 짝 터미널 색으로 축소판을 그린다.

use gpui::prelude::*;
use gpui::{Div, FontWeight, Hsla, SharedString, Stateful, div, px, relative};
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::theme::{Rgb, Theme as TerminalTheme};

use super::terminal_themes::builtin_terminal_theme;
use super::theme::{
    Palette, RADIUS_MD, RADIUS_XS, TEXT_XS, TRANSPARENT, rgba_bytes, terminal_surface,
};

const fn fill(color: Rgb) -> Hsla {
    rgba_bytes(color.r, color.g, color.b, 0xff)
}

/// 터미널 바탕 위에 ANSI 빨강, 초록, 노랑, 파랑, 자주, 청록을 칩으로 늘어놓는다.
fn terminal_chips(id: &str, terminal: TerminalTheme) -> Div {
    let [_, chips @ .., _, _, _, _, _, _, _, _, _] = terminal.ansi;
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .p(px(2.))
        .flex()
        .items_end()
        .gap(px(1.))
        .rounded(RADIUS_XS)
        .bg(terminal_surface(terminal))
        .children(chips.into_iter().enumerate().map(|(i, color)| {
            let selector = format!("{id}-chip-{i}");
            div()
                .debug_selector(move || selector.clone())
                .flex_1()
                .min_w(px(8.))
                .h(px(14.))
                .bg(fill(color))
        }))
}

/// 축소판 한 장. 사이드바 띠, 본문, 강조 막대, 짝 터미널 칩을 그 테마의 값으로 그린다.
fn preview(id: &str, theme: &Palette, terminal_name: &str) -> Div {
    let selector = format!("{id}-preview");
    let terminal = builtin_terminal_theme(terminal_name).map(|builtin| builtin.theme);
    div()
        .debug_selector(move || selector.clone())
        .w_full()
        .h(px(52.))
        .flex()
        .overflow_hidden()
        .rounded(RADIUS_XS)
        .child(div().flex_none().w(px(10.)).h_full().bg(theme.sidebar))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .p(px(3.))
                .flex()
                .flex_col()
                .gap(px(3.))
                .bg(theme.background)
                .child(
                    div()
                        .flex_none()
                        .w(relative(0.6))
                        .h(px(5.))
                        .rounded(RADIUS_XS)
                        .bg(theme.primary),
                )
                .children(terminal.map(|terminal| terminal_chips(id, terminal))),
        )
}

/// 테마 고르기 칸. 축소판 아래에 이름을 두고, 고른 칸은 `ring` 2px과 체크로 표시한다.
/// 바깥 테두리 자리를 늘 비워 두어 선택이 바뀌어도 칸이 움직이지 않는다.
pub fn theme_swatch(
    p: &Palette,
    id: impl Into<SharedString>,
    name: &'static str,
    theme: &Palette,
    terminal_name: &str,
    selected: bool,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.to_string();
    let check_selector = format!("{id}-check");
    let inner = preview(&id, theme, terminal_name);
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .role(gpui::Role::RadioButton)
        .aria_label(name)
        .aria_toggled(if selected {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        })
        .tab_index(0)
        .relative()
        .min_w_0()
        .p(px(1.))
        .flex()
        .flex_col()
        .gap_1()
        .rounded(RADIUS_MD)
        .cursor_pointer()
        .hover(|d| d.bg(p.accent))
        .focus_visible(|d| d.bg(p.accent))
        .child(
            div()
                .w_full()
                .p(px(1.))
                .border_2()
                .rounded(RADIUS_MD)
                .border_color(if selected { p.ring } else { TRANSPARENT })
                .child(
                    // 축소판 바탕이 모달과 같아도 경계가 보이게 글자 계열 색으로 테두리를 그린다.
                    div()
                        .w_full()
                        .border_1()
                        .rounded(RADIUS_XS)
                        .border_color(p.muted_foreground.opacity(0.45))
                        .child(inner),
                ),
        )
        .child(
            div()
                .w_full()
                .text_center()
                .text_size(TEXT_XS)
                .when(selected, |d| {
                    d.text_color(p.foreground).font_weight(FontWeight::MEDIUM)
                })
                .when(!selected, |d| d.text_color(p.muted_foreground))
                .child(name),
        )
        .when(selected, |d| {
            d.child(
                div()
                    .debug_selector(move || check_selector.clone())
                    .absolute()
                    .top(px(6.))
                    .right(px(6.))
                    .size(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(p.background)
                    .bg(p.foreground)
                    .text_color(p.background)
                    .child(Icon::new(IconName::Check).xsmall()),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::super::{DarkUiTheme, LightUiTheme};
    use super::*;

    #[test]
    fn every_app_theme_has_its_paired_terminal_palette() {
        for name in DarkUiTheme::ALL
            .map(DarkUiTheme::terminal_theme_name)
            .into_iter()
            .chain(LightUiTheme::ALL.map(LightUiTheme::terminal_theme_name))
        {
            assert!(builtin_terminal_theme(name).is_some(), "{name}");
        }
    }
}
