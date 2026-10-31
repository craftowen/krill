//! 필요한 순간에만 뜨는 조치 알림(OWE-714). 셸 우하단 토스트가 [`FixNeeds`]의 항목마다
//! [고치기](= `run_fix`) · [닫기](= `dismiss_need`)를 제공하며, 점검 표면도 같은 내용을 쓴다.
//! 첫 실행에 할 일 목록을 띄우지 않는다 — 접근 거부·첫 에이전트 세션처럼 실제로 필요할 때만.

use gpui::{AnyElement, App, div, prelude::*};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};

use super::Env;
use super::actions::{FixNeed, FixNeeds, dismiss_need, run_fix};
use crate::i18n::catalog::onboarding as text;

/// 보여 줄 배너가 없으면 `None`. 셸이 `observe_global::<FixNeeds>`로 다시 그린다.
pub fn render(cx: &App) -> Option<AnyElement> {
    let need = cx.try_global::<FixNeeds>()?.items.first()?.clone();
    Some(render_need(need, cx))
}

/// Action content shared by the shell toast and the standalone check surface.
pub fn render_need(need: FixNeed, cx: &App) -> AnyElement {
    if need.check_id == super::error_consent::CHECK_ID {
        return render_error_consent(need, cx);
    }
    let p = crate::ui::palette(cx);
    let fix_id = need.check_id.clone();
    let close_id = need.check_id.clone();
    div()
        .id("fix-banner")
        .debug_selector(|| "fix-banner".into())
        .w_full()
        .min_w_0()
        .px_3()
        .py_1p5()
        .bg(p.popover)
        .text_color(p.popover_foreground)
        .font_family(crate::ui::FONT_UI)
        .text_size(crate::ui::TEXT_XS)
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .child(need.message),
        )
        .child(
            div()
                .flex_none()
                .debug_selector(|| "fix-banner-fix".into())
                .child(
                    Button::new("fix-banner-fix")
                        .xsmall()
                        .primary()
                        .label(text::FIX_NOW.get())
                        .on_click(move |_, _, cx| fix(&fix_id, cx)),
                ),
        )
        .child(
            div()
                .flex_none()
                .debug_selector(|| "fix-banner-close".into())
                .child(
                    Button::new("fix-banner-close")
                        .xsmall()
                        .ghost()
                        .label(text::CLOSE.get())
                        .on_click(move |_, _, cx| dismiss_need(&close_id, cx)),
                ),
        )
        .into_any_element()
}

/// 오류 보고 동의 배너. 기본 OFF이며, 어느 쪽을 눌러도 온보딩 완료·세션은 막지 않는다.
fn render_error_consent(need: FixNeed, cx: &App) -> AnyElement {
    let p = crate::ui::palette(cx);
    div()
        .id("error-consent-banner")
        .debug_selector(|| "error-consent-banner".into())
        .w_full()
        .min_w_0()
        .px_3()
        .py_1p5()
        .bg(p.popover)
        .text_color(p.popover_foreground)
        .font_family(crate::ui::FONT_UI)
        .text_size(crate::ui::TEXT_XS)
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .debug_selector(|| "error-consent-message".into())
                .child(need.message),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .debug_selector(|| "error-consent-decline".into())
                        .child(
                            Button::new("error-consent-decline")
                                .xsmall()
                                .ghost()
                                .label(text::ERROR_CONSENT_DECLINE.get())
                                .on_click(|_, _, cx| decline_consent(cx)),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .debug_selector(|| "error-consent-accept".into())
                        .child(
                            Button::new("error-consent-accept")
                                .xsmall()
                                .primary()
                                .label(text::ERROR_CONSENT_ENABLE.get())
                                .on_click(|_, _, cx| accept_consent(cx)),
                        ),
                ),
        )
        .into_any_element()
}

fn accept_consent(cx: &mut App) {
    let Some(env) = Env::current() else {
        return;
    };
    super::error_consent::accept(&env.state, cx);
}

fn decline_consent(cx: &mut App) {
    let Some(env) = Env::current() else {
        return;
    };
    super::error_consent::decline(&env.state, cx);
}

/// [고치기]: 조치를 돌리고 성공하면 배너를 내린다. 실패하면 남겨 두고 다시 누를 수 있다.
fn fix(id: &str, cx: &mut App) {
    let Some(env) = Env::current() else {
        return;
    };
    let run = run_fix(id, &env, cx);
    let id = id.to_owned();
    cx.spawn(async move |cx| match run.await {
        Ok(()) => cx.update(|cx| {
            if !super::actions::awaits_settings_return(&id) {
                dismiss_need(&id, cx);
            }
        }),
        Err(super::actions::FixError::Cancelled) => {}
        Err(e) => cx.update(|cx| super::actions::report_failure(&id, e.to_string(), cx)),
    })
    .detach();
}
