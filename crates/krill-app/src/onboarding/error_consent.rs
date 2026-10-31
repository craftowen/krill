//! 첫 설치 후 오류 보고 opt-in 동의(MK-127·OWE-690 후속).
//!
//! - 오류 보고는 기본 OFF다. 이 배너에서 [보내기 켜기]를 눌러야만 동의 파일이 생긴다.
//! - [보내지 않기]·닫기(X)는 파일을 만들지 않고(있으면 지우고) `onboarding.toml`에만
//!   `error_reporting_asked = true`를 남긴다. 어느 쪽도 온보딩 완료·세션을 막지 않는다.
//! - 동의 파일은 `krill_proto::error_reporting` 하나만 쓴다. 설정 > Resources 토글과
//!   `krill errors enable/disable`이 같은 파일을 읽고 쓰므로 상태가 항상 일치한다.

use std::path::Path;

use gpui::App;

use super::state;
use crate::i18n::catalog::onboarding as text;

/// 조치 배너에서 쓰는 점검 ID. 셸 토스트·닫기(X) 경로가 함께 쓴다.
pub const CHECK_ID: &str = "error-reporting-consent";

/// 묻지 않은 구 `onboarding.toml`(필드 없음)은 `false`로 읽혀 한 번 묻는다.
#[must_use]
pub fn is_asked(state_dir: &Path) -> bool {
    state::load(state_dir).error_reporting_asked
}

/// 동의 파일이 있으면 `true`. 환경 거부와 무관하게 파일 존재로만 본다.
/// 파일이 있으면 이미 선택한 것으로 보고 다시 묻지 않는다.
#[must_use]
pub fn is_consented(state_dir: &Path) -> bool {
    krill_proto::error_reporting::status(state_dir).is_ok_and(|status| status.consented)
}

/// 지금 배너를 띄워야 하는가. 물었거나 이미 동의 파일이 있으면 `false`.
#[must_use]
pub fn needs_consent(state_dir: &Path) -> bool {
    !is_asked(state_dir) && !is_consented(state_dir)
}

/// 물었음을 기록한다. 완료·단계는 그대로 둔다.
pub fn mark_asked(state_dir: &Path) -> std::io::Result<()> {
    let mut stored = state::load(state_dir);
    if stored.error_reporting_asked {
        return Ok(());
    }
    stored.error_reporting_asked = true;
    state::save(state_dir, &stored)
}

/// 첫 실행 진입에서 부른다. 필요할 때만 배너를 게시하고 중복 게시하지 않는다.
pub fn offer_if_needed(state_dir: &Path, cx: &mut App) {
    let dir = state_dir.to_path_buf();
    let work = crate::settings::background_file_io(cx, move || needs_consent(&dir));
    cx.spawn(async move |cx| {
        if !work.await {
            return;
        }
        cx.update(|cx| {
            if cx
                .try_global::<super::actions::FixNeeds>()
                .is_some_and(|needs| needs.items.iter().any(|need| need.check_id == CHECK_ID))
            {
                return;
            }
            let message = format!(
                "{}\n{}",
                text::ERROR_CONSENT_TITLE.get(),
                text::ERROR_CONSENT_DESC.get()
            );
            super::actions::publish_consent_need(message, cx);
        });
    })
    .detach();
}

/// [보내기 켜기]: 동의 파일을 만들고 물었음을 남긴 뒤 배너를 내린다.
/// 실패해도 온보딩 완료는 유지하고 배너에 원인을 남긴다.
pub fn accept(state_dir: &Path, cx: &mut App) {
    let dir = state_dir.to_path_buf();
    let work = crate::settings::background_file_io(cx, move || {
        krill_proto::error_reporting::enable(&dir).and_then(|()| mark_asked(&dir))
    });
    cx.spawn(async move |cx| {
        let outcome = work.await;
        cx.update(|cx| match outcome {
            Ok(()) => {
                super::actions::dismiss_need(CHECK_ID, cx);
                crate::errors::refresh_run(cx);
            }
            Err(error) => {
                super::actions::report_failure(CHECK_ID, error.to_string(), cx);
            }
        });
    })
    .detach();
}

/// [보내지 않기]·닫기: 동의 파일을 지우고(없으면 그대로) 물었음을 남긴다.
/// 기본 OFF라 거절해도 전송 대기만 지워지고 온보딩·세션은 정상 진행된다.
pub fn decline(state_dir: &Path, cx: &mut App) {
    let dir = state_dir.to_path_buf();
    let work = crate::settings::background_file_io(cx, move || {
        // `disable`은 파일이 없어도 성공한다. 잠금 경합으로 정리가 미뤄지면
        // 그 오류를 남기되 물었음 기록은 유지한다(다음 실행에 다시 묻지 않는다).
        let disable = krill_proto::error_reporting::disable(&dir);
        let marked = mark_asked(&dir);
        marked.and(disable)
    });
    cx.spawn(async move |cx| {
        let outcome = work.await;
        cx.update(|cx| match outcome {
            Ok(()) => {
                super::actions::dismiss_need(CHECK_ID, cx);
                crate::errors::refresh_run(cx);
            }
            Err(error) => {
                super::actions::report_failure(CHECK_ID, error.to_string(), cx);
            }
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("krill-error-consent-{name}"));
        std::fs::remove_dir_all(&path).ok();
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn fresh_state_needs_consent_and_asked_does_not() {
        let root = dir(&format!("fresh-{}", std::process::id()));
        assert!(needs_consent(&root));
        mark_asked(&root).unwrap();
        assert!(is_asked(&root));
        assert!(!needs_consent(&root));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn consented_state_needs_nothing() {
        let root = dir(&format!("consented-{}", std::process::id()));
        krill_proto::error_reporting::enable(&root).unwrap();
        assert!(is_consented(&root));
        assert!(!needs_consent(&root));
        // 동의 파일이 있으면 물었음과 무관하게 묻지 않는다.
        assert!(!state::load(&root).error_reporting_asked);
        krill_proto::error_reporting::disable(&root).unwrap();
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn legacy_onboarding_file_without_field_counts_as_not_asked() {
        let root = dir(&format!("legacy-{}", std::process::id()));
        std::fs::write(
            state::state_path(&root),
            "completed = true\nstep = \"done\"\nversion = 1\n",
        )
        .unwrap();
        assert!(!is_asked(&root));
        assert!(needs_consent(&root));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn mark_asked_preserves_completion() {
        let root = dir(&format!("keep-{}", std::process::id()));
        let done = state::load(&root).completed();
        state::save(&root, &done).unwrap();
        mark_asked(&root).unwrap();
        let back = state::load(&root);
        assert!(back.completed);
        assert!(back.error_reporting_asked);
        assert!(!needs_consent(&root));
        std::fs::remove_dir_all(&root).ok();
    }

    #[gpui::test]
    fn accepting_enables_reports_and_suppresses_repeat_prompt(cx: &mut gpui::TestAppContext) {
        let root = dir(&format!("accept-{}", std::process::id()));
        let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
        cx.update(|_, cx| {
            offer_if_needed(&root, cx);
            offer_if_needed(&root, cx);
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert_eq!(
                cx.global::<super::super::actions::FixNeeds>().items.len(),
                1
            );
            assert!(!is_consented(&root));
            accept(&root, cx);
        });
        cx.run_until_parked();
        assert!(is_consented(&root));
        assert!(is_asked(&root));
        cx.update(|_, cx| offer_if_needed(&root, cx));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert!(
                cx.global::<super::super::actions::FixNeeds>()
                    .items
                    .is_empty()
            );
        });
        std::fs::remove_dir_all(&root).ok();
    }

    #[gpui::test]
    fn declining_keeps_reports_off_and_does_not_block_onboarding(cx: &mut gpui::TestAppContext) {
        let root = dir(&format!("decline-{}", std::process::id()));
        state::save(&root, &state::load(&root).completed()).unwrap();
        let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
        cx.update(|_, cx| offer_if_needed(&root, cx));
        cx.run_until_parked();
        cx.update(|_, cx| decline(&root, cx));
        cx.run_until_parked();
        assert!(!is_consented(&root));
        assert!(is_asked(&root));
        assert!(state::load(&root).completed);
        cx.update(|_, cx| offer_if_needed(&root, cx));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert!(
                cx.global::<super::super::actions::FixNeeds>()
                    .items
                    .is_empty()
            );
        });
        std::fs::remove_dir_all(&root).ok();
    }
}
