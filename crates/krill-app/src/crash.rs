//! 지난 실행의 크래시 보고(MK-067). 시작할 때 확인하지 않은 크래시 로그(`krill_proto::diag`)가 있으면 진단 대화상자를
//! 띄운다. "진단 번들 만들기"는 리댁션한 번들을 상태 디렉터리에 쓰고 호출자에게 넘긴다(앱은 Finder로 보인다).
//! 어느 쪽을 골라도 다시 묻지 않는다.

use std::path::{Path, PathBuf};

use gpui::{App, PromptLevel, Window};
use krill_proto::diag;

use crate::i18n::catalog::crash as text;

/// `state`에 확인하지 않은 크래시 로그가 있으면 진단 대화상자를 띄운다. 번들을 만들면 `on_bundle`에 경로를 준다.
pub fn offer_report(
    state: PathBuf,
    on_bundle: impl FnOnce(&Path, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let dir = state.clone();
        let logs = cx
            .background_executor()
            .spawn(async move { diag::pending(&dir) })
            .await;
        if logs.is_empty() {
            return;
        }
        let detail = text::DETAIL.get().replace("{n}", &logs.len().to_string());
        let Ok(answer) = handle.update(cx, |_, window, cx| {
            window.prompt(
                PromptLevel::Warning,
                text::TITLE.get(),
                Some(&detail),
                &[text::BUNDLE.get(), text::DISMISS.get()],
                cx,
            )
        }) else {
            return;
        };
        let Ok(answer) = answer.await else { return };
        if answer == 0 {
            match cx
                .background_executor()
                .spawn(async move { diag::write_bundle(&state) })
                .await
            {
                Ok(path) => cx.update(|cx| on_bundle(&path, cx)),
                Err(e) => {
                    // 번들을 못 만들면 알리고 확인 처리하지 않는다: 다음 실행에서 다시 물어 로그를 잃지 않는다.
                    tracing::warn!(%e, "diagnostic bundle write failed");
                    let detail = text::BUNDLE_FAILED_DETAIL.format(&[("error", &e.to_string())]);
                    handle
                        .update(cx, |_, window, cx| {
                            drop(window.prompt(
                                PromptLevel::Critical,
                                text::BUNDLE_FAILED.get(),
                                Some(&detail),
                                &[text::DISMISS.get()],
                                cx,
                            ));
                        })
                        .ok();
                    return;
                }
            }
        }
        cx.background_executor()
            .spawn(async move { diag::acknowledge(&logs) })
            .await;
    })
    .detach();
}

#[cfg(test)]
mod tests;
