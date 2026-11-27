//! Regression tests for v0.3.34 PR-A settings/voice/mobile/search/schedule fixes
//! (`UI-05`, `UI-07`, `UI-08`, `UI-17`, `UI-18`, `UI-26`).
//! Kept in a dedicated test module to avoid touching `settings/tests.rs` (PR #270).

use super::*;
use gpui::{TestAppContext, px};
use krill_proto::schedule::ScheduleRun;
use krill_proto::usage::{AccountsSnapshot, ResetConsumeOutcome};
use krill_proto::voice::{
    DEFAULT_DESKTOP_SHORTCUT, VoiceConfig, VoiceConnectionStatus, VoiceReply, VoiceRequest,
    VoiceStatusInfo,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn init_settings_test(cx: &mut TestAppContext, name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("krill-v0334-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.clone();
    cx.update(|cx| {
        gpui_component::init(cx);
        init_at(Some(d), cx);
    });
    cx.run_until_parked();
    dir
}

#[gpui::test]
fn voice_shortcut_capture_intercepts_bound_chord_cancels_on_tab_or_hide_and_rolls_back_on_err(
    cx: &mut TestAppContext,
) {
    let dir = init_settings_test(cx, "ui05-voice-capture");
    let fail_updates = Arc::new(AtomicBool::new(false));
    let fail_flag = Arc::clone(&fail_updates);
    let voice_call: crate::voice::VoiceCall = Arc::new(move |req| match req {
        VoiceRequest::Status => Ok(VoiceReply::Status(VoiceStatusInfo {
            available: true,
            status: VoiceConnectionStatus::Ready,
            app_version: Some("1.0.0".into()),
            engine_ready: true,
            engine_error: None,
            refine_available: true,
            refine_enabled: true,
            vocab_count: 0,
        })),
        VoiceRequest::ConfigSet(patch) => {
            if fail_flag.load(Ordering::Acquire) {
                Err(std::io::Error::other("simulated voice update failure"))
            } else {
                let mut cfg = VoiceConfig::default();
                if let Some(sc) = &patch.desktop_shortcut {
                    cfg.desktop_shortcut = sc.clone();
                }
                if let Some(refine) = patch.refine_enabled {
                    cfg.refine_enabled = refine;
                }
                Ok(VoiceReply::Config(cfg))
            }
        }
        _ => Ok(VoiceReply::Config(VoiceConfig::default())),
    });

    let (view, vcx) = cx.add_window_view(|_, cx| {
        let mut v = SettingsView::new(cx);
        v.set_voice_call_for_test(voice_call);
        v.set_tab(AdminTab::Voice, cx);
        v
    });
    vcx.run_until_parked();

    // 1. Tab switch cancels armed capture and drops interceptor.
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.start_voice_shortcut_capture(window, cx);
            assert!(v.voice.capturing_shortcut);
            assert!(v.voice.capture_intercept.is_some());
            v.set_tab(AdminTab::Appearance, cx);
            assert!(!v.voice.capturing_shortcut);
            assert!(v.voice.capture_intercept.is_none());
        });
    });

    // 2. set_visible(false) also cancels armed capture.
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.set_tab(AdminTab::Voice, cx);
            v.start_voice_shortcut_capture(window, cx);
            assert!(v.voice.capturing_shortcut);
            v.set_visible(false, cx);
            assert!(!v.voice.capturing_shortcut);
            assert!(v.voice.capture_intercept.is_none());
            v.set_visible(true, cx);
        });
    });

    // 3. Bound chord (cmd-f) is intercepted cleanly without focusing search.
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.start_voice_shortcut_capture(window, cx);
        });
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("cmd-f");
    vcx.run_until_parked();
    vcx.read_entity(&view, |v, _| {
        assert!(!v.voice.capturing_shortcut);
        assert!(v.voice.capture_intercept.is_none());
        assert_ne!(
            v.voice.config.desktop_shortcut, DEFAULT_DESKTOP_SHORTCUT,
            "cmd-f must be captured as the new shortcut instead of firing FocusSearch"
        );
    });

    // 4. RPC Err rolls back optimistic desktop_shortcut and refine_enabled mutations.
    fail_updates.store(true, Ordering::Release);
    let saved_shortcut = vcx.read_entity(&view, |v, _| v.voice.config.desktop_shortcut.clone());
    vcx.update(|window, cx| {
        view.update(cx, |v, cx| {
            v.start_voice_shortcut_capture(window, cx);
        });
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("cmd-shift-k");
    vcx.run_until_parked();
    vcx.read_entity(&view, |v, _| {
        assert_eq!(
            v.voice.config.desktop_shortcut, saved_shortcut,
            "failed shortcut update must roll back optimistic UI"
        );
        assert!(v.voice.error.is_some());
    });

    let saved_refine = vcx.read_entity(&view, |v, _| v.voice.config.refine_enabled);
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            let prev = v.voice.config.clone();
            v.voice.config.refine_enabled = !saved_refine;
            v.apply_voice_patch_with_rollback(
                krill_proto::voice::VoiceConfigPatch {
                    refine_enabled: Some(!saved_refine),
                    ..Default::default()
                },
                Some(prev),
                cx,
            );
        });
    });
    vcx.run_until_parked();
    vcx.read_entity(&view, |v, _| {
        assert_eq!(
            v.voice.config.refine_enabled, saved_refine,
            "failed AI refine toggle must roll back optimistic UI"
        );
    });

    std::fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn consume_reset_credit_errors_survive_followup_refresh_accounts_async(cx: &mut TestAppContext) {
    let dir = init_settings_test(cx, "ui07-reset-error");
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));

    for outcome in [
        Ok(ResetConsumeOutcome::NoCredit),
        Ok(ResetConsumeOutcome::NothingToReset),
        Err("simulated reset error".to_owned()),
    ] {
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                // Mirror the consume_reset_credit completion ordering: refresh_accounts_async is
                // started with account_error cleared BEFORE assigning the error from outcome.
                v.account_error = None;
                v.refresh_accounts_async(cx);
                v.account_error = match &outcome {
                    Ok(ResetConsumeOutcome::NoCredit) => Some(
                        crate::i18n::catalog::status::PROVIDER_RESET_USAGE_NO_CREDIT
                            .get()
                            .into(),
                    ),
                    Ok(ResetConsumeOutcome::NothingToReset) => Some(
                        crate::i18n::catalog::status::PROVIDER_RESET_USAGE_NOTHING
                            .get()
                            .into(),
                    ),
                    Ok(_) => None,
                    Err(err) => Some(err.clone().into()),
                };
                assert!(v.account_error.is_some());
                // Even if an AccountsSnapshot refresh completes right after, before.1 was None,
                // so the new error must survive.
                let _ = AccountsSnapshot::default();
            });
        });
        vcx.run_until_parked();
        vcx.read_entity(&view, |v, _| {
            assert!(
                v.account_error.is_some(),
                "account_error must survive follow-up refresh_accounts_async for {outcome:?}"
            );
        });
    }

    std::fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn schedule_log_viewer_classifies_nonzero_finished_run_as_failed(cx: &mut TestAppContext) {
    let dir = init_settings_test(cx, "ui08-schedule-log");
    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    let failed_run = ScheduleRun {
        id: "nightly".into(),
        minute: 29_882_880,
        phase: "finished".into(),
        status: Some(1),
        session: None,
        cwd: Some("/tmp/repo".into()),
        output: Some("exit 1".into()),
        error: None,
        trigger: None,
        run_number: None,
        duration_ms: None,
        timed_out: None,
    };
    vcx.update(|_, cx| {
        view.update(cx, |v, cx| {
            v.open_schedule_run_log(&failed_run, cx);
        });
    });
    vcx.run_until_parked();
    vcx.read_entity(&view, |v, _| {
        let log = v.schedule_log().expect("schedule log open");
        assert_eq!(log.status, Some(1));
        let (tone, _) = schedules::run_outcome_badge_spec_for(&log.phase, log.status);
        assert_eq!(
            tone,
            crate::ui::BadgeTone::Destructive,
            "finished run with exit status 1 must show Destructive badge in Schedule Log Viewer"
        );
    });
    std::fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn sync_recording_paused_keeps_toggle_enabled_on_transient_ipc_failure(cx: &mut TestAppContext) {
    let dir = init_settings_test(cx, "ui17-recording-pause");
    let view = cx.new(SettingsView::new);
    view.update(cx, |v, _| {
        assert_eq!(v.history_pause_supported, None);
        v.apply_recording_pause_sync_outcome(&Err(PauseError::Failed("daemon restarting".into())));
        assert_eq!(
            v.history_pause_supported, None,
            "transient IPC error must not permanently disable Recording Pause toggle"
        );
        v.apply_recording_pause_sync_outcome(&Ok(false));
        assert_eq!(v.history_pause_supported, Some(true));
        v.apply_recording_pause_sync_outcome(&Err(PauseError::Failed("transient".into())));
        assert_eq!(v.history_pause_supported, Some(true));
        v.apply_recording_pause_sync_outcome(&Err(PauseError::Unsupported));
        assert_eq!(v.history_pause_supported, Some(false));
    });
    std::fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn settings_search_index_resolves_every_entry_and_includes_quick_commands_and_feedback(
    cx: &mut TestAppContext,
) {
    let dir = init_settings_test(cx, "ui18-search-index");
    // Dead SKILLS_TITLE must not be indexed; skill keywords route to AUTO_SETUP_ALL_TITLE.
    assert!(
        !crate::i18n::SETTINGS_SEARCH_INDEX
            .iter()
            .any(|(_, title, _)| *title == text::SKILLS_TITLE),
        "dead SKILLS_TITLE must be removed from SETTINGS_SEARCH_INDEX"
    );
    assert!(search::search_hits("skill").contains(&(AdminTab::Agents, text::AUTO_SETUP_ALL_TITLE)));
    assert!(
        search::search_hits("quick command")
            .contains(&(AdminTab::QuickCommands, text::TAB_QUICK_COMMANDS))
    );
    assert!(search::search_hits("feedback").contains(&(AdminTab::Feedback, text::TAB_FEEDBACK)));
    assert!(search::search_hits("피드백").contains(&(AdminTab::Feedback, text::TAB_FEEDBACK)));

    let (view, vcx) = cx.add_window_view(|_, cx| SettingsView::new(cx));
    // Verify every index entry resolves to a rendered search_focus anchor on macOS (and on
    // non-macOS skip macOS-only UPDATES_AUTO_RESTART).
    for &(tab, title, _) in crate::i18n::SETTINGS_SEARCH_INDEX {
        if !cfg!(target_os = "macos") && title == text::UPDATES_AUTO_RESTART {
            continue;
        }
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.open_search_hit((tab, title), cx);
            });
            window.draw(cx).clear(cx);
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        vcx.read_entity(&view, |v, _| {
            assert!(
                v.search_focus.rendered.get(),
                "SETTINGS_SEARCH_INDEX entry ({tab:?}, {}) did not render a search_focus anchor",
                title.id
            );
        });
    }
    std::fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn standalone_mobile_page_shows_login_required_qr_prompt_when_signed_out(cx: &mut TestAppContext) {
    let dir = init_settings_test(cx, "ui26-mobile-qr");
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let mut v = SettingsView::new(cx);
        v.set_mobile_page(true, cx);
        v.mobile.set_signed_out_relay_for_test();
        v
    });
    vcx.simulate_resize(gpui::size(px(900.), px(700.)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("mobile-page-qr-empty").is_some());
    vcx.read_entity(&view, |v, _| {
        assert_eq!(
            v.empty_qr_label(false),
            crate::i18n::catalog::mobile::QR_NEEDS_LOGIN.get()
        );
        assert_eq!(
            v.empty_qr_label(true),
            crate::i18n::catalog::mobile::QR_NEEDS_LOGIN.get()
        );
    });
    std::fs::remove_dir_all(dir).ok();
}
