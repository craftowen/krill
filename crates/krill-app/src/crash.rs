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
    let logs = diag::pending(&state);
    if logs.is_empty() {
        return;
    }
    let detail = text::DETAIL.get().replace("{n}", &logs.len().to_string());
    let answer = window.prompt(
        PromptLevel::Warning,
        text::TITLE.get(),
        Some(&detail),
        &[text::BUNDLE.get(), text::DISMISS.get()],
        cx,
    );
    cx.spawn(async move |cx| {
        if answer.await == Ok(0) {
            match diag::write_bundle(&state) {
                Ok(path) => cx.update(|cx| on_bundle(&path, cx)),
                Err(e) => eprintln!("diagnostic bundle write failed: {e}"),
            }
        }
        diag::acknowledge(&logs);
    })
    .detach();
}

#[cfg(test)]
mod tests;
