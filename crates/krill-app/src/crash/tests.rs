use std::fs;
use std::path::Path;

use gpui::{TestAppContext, VisualTestContext};

use super::*;

fn state(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mk-crash-{}-{name}", std::process::id()));
    fs::remove_dir_all(&d).ok();
    fs::create_dir_all(diag::crash_dir(&d)).unwrap();
    d
}

fn open<'a>(cx: &'a mut TestAppContext, state: &Path) -> &'a mut VisualTestContext {
    let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
    let state = state.to_path_buf();
    cx.update(|window, cx| offer_report(state, |_, _| {}, window, cx));
    cx.run_until_parked();
    cx
}

#[gpui::test]
fn crash_log_prompts_and_bundle_is_redacted(cx: &mut TestAppContext) {
    let state = state("bundle");
    fs::write(
        diag::crash_dir(&state).join("crash-1-1-krill-app.txt"),
        "message: key sk-ant-api03-secret\n-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();
    let cx = open(cx, &state);
    let (title, _) = cx.pending_prompt().expect("diagnostic dialog");
    assert_eq!(title, text::TITLE.get());
    cx.simulate_prompt_answer(text::BUNDLE.get());
    cx.run_until_parked();

    assert!(diag::pending(&state).is_empty());
    let bundles: Vec<_> = fs::read_dir(state.join("diagnostics")).unwrap().collect();
    assert_eq!(bundles.len(), 1);
    let bundle = fs::read_to_string(bundles[0].as_ref().unwrap().path()).unwrap();
    assert!(!bundle.contains("sk-ant-api03"), "{bundle}");
    assert!(!bundle.contains("AAAA"), "{bundle}");
    fs::remove_dir_all(&state).ok();
}

#[gpui::test]
fn dismiss_does_not_ask_again(cx: &mut TestAppContext) {
    let state = state("dismiss");
    fs::write(diag::crash_dir(&state).join("crash-1-1-krill-app.txt"), "x").unwrap();
    let cx = open(cx, &state);
    cx.simulate_prompt_answer(text::DISMISS.get());
    cx.run_until_parked();
    assert!(diag::pending(&state).is_empty());
    assert!(!state.join("diagnostics").exists());
    let s = state.clone();
    cx.update(|window, cx| offer_report(s, |_, _| {}, window, cx));
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt());
    fs::remove_dir_all(&state).ok();
}

#[gpui::test]
fn no_crash_no_prompt(cx: &mut TestAppContext) {
    let state = state("none");
    let cx = open(cx, &state);
    assert!(!cx.has_pending_prompt());
    fs::remove_dir_all(&state).ok();
}

/// 번들 쓰기에 실패하면 오류 대화상자를 띄우고 확인 처리하지 않아 다음 실행에 다시 묻는다.
#[gpui::test]
fn bundle_failure_shows_error_and_keeps_logs_pending(cx: &mut TestAppContext) {
    let state = state("bundle-fail");
    fs::write(diag::crash_dir(&state).join("crash-1-1-krill-app.txt"), "x").unwrap();
    // 번들 디렉터리 자리에 파일을 두어 디렉터리 생성이 실패하게 한다.
    fs::write(state.join("diagnostics"), "not a dir").unwrap();
    let cx = open(cx, &state);
    cx.simulate_prompt_answer(text::BUNDLE.get());
    cx.run_until_parked();
    let (title, _) = cx.pending_prompt().expect("bundle failure dialog");
    assert_eq!(title, text::BUNDLE_FAILED.get());
    assert_eq!(diag::pending(&state).len(), 1);
    fs::remove_dir_all(&state).ok();
}
