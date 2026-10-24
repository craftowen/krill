//! Shared restart state for the settings page, update card and automatic checks.

use gpui::{App, AppContext as _};

use super::{StagedUpdate, UpdateState};

struct PreparedRestart {
    child: std::process::Child,
    // Publishing checks use the same filesystem lock. Keep the selected bytes
    // stable until this helper exits or the old app process terminates.
    staging_lock: std::fs::File,
}

fn claim_payload(staged: &StagedUpdate) -> Result<std::fs::File, String> {
    let guard = super::staging_lock(&staged.app_bundle)
        .map_err(|error| super::Error::Io(error).user_message())?;
    let path = super::staging_path(&staged.app_bundle);
    if super::bundle_short_version(&path).as_deref() != Some(staged.to_version.as_str()) {
        return Err(crate::i18n::catalog::updater_relaunch::CARD_PAYLOAD_CHANGED
            .get()
            .to_owned());
    }
    Ok(guard)
}

/// Relauncher preparation is separate from downloading or staging an update.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RestartStatus {
    #[default]
    Idle,
    Starting,
    Failed(String),
}

impl RestartStatus {
    #[must_use]
    pub fn is_starting(&self) -> bool {
        matches!(self, Self::Starting)
    }

    #[must_use]
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(error) => Some(error),
            Self::Idle | Self::Starting => None,
        }
    }
}

/// Prepare the relauncher without blocking the UI, then request the durable quit action.
/// A failed launch keeps the staged payload available for another attempt.
pub fn restart_staged_update(staged: StagedUpdate, cx: &mut App) {
    let launch = staged.clone();
    restart_with(
        staged,
        cx,
        move || {
            if launch.relaunched {
                Ok(None)
            } else {
                let staging_lock = claim_payload(&launch)?;
                let child = launch
                    .spawn_relauncher_child()
                    .map_err(|error| error.user_message())?;
                Ok(Some(PreparedRestart {
                    child,
                    staging_lock,
                }))
            }
        },
        |cx| {
            if let Ok(quit) = cx.build_action("shell::Quit", None) {
                cx.dispatch_action(quit.as_ref());
            }
        },
    );
}

fn restart_with(
    staged: StagedUpdate,
    cx: &mut App,
    launch: impl FnOnce() -> Result<Option<PreparedRestart>, String> + Send + 'static,
    quit: impl FnOnce(&mut App) + 'static,
) {
    let mut state = cx.try_global::<UpdateState>().cloned().unwrap_or_default();
    if state.restart.is_starting()
        || matches!(state.status, super::CheckStatus::Checking)
        || state.staged.as_ref() != Some(&staged)
    {
        return;
    }
    state.restart = RestartStatus::Starting;
    state.card_collapsed = false;
    cx.set_global(state);
    cx.refresh_windows();
    let work = cx.background_spawn(async move { launch() });
    cx.spawn(async move |cx| {
        let result = work.await;
        cx.update(move |cx| {
            let mut state = cx.try_global::<UpdateState>().cloned().unwrap_or_default();
            if state.staged.as_ref() != Some(&staged) || !state.restart.is_starting() {
                if let Ok(Some(prepared)) = result {
                    // This child belongs to the obsolete preparation, not the
                    // currently staged update. Do not leave it waiting to swap
                    // an old payload when the app eventually exits.
                    cx.background_spawn(async move {
                        let PreparedRestart {
                            mut child,
                            staging_lock,
                        } = prepared;
                        child.kill().ok();
                        child.wait().ok();
                        drop(staging_lock);
                    })
                    .detach();
                }
                return;
            }
            match result {
                Ok(child) => {
                    // Keep the guard while durable quit flushes pending app state.
                    if let Some(prepared) = child {
                        let wait = cx.background_spawn(async move {
                            let PreparedRestart {
                                mut child,
                                staging_lock,
                            } = prepared;
                            let result = child
                                .wait()
                                .map(|_| ())
                                .map_err(|error| super::Error::Io(error).user_message());
                            drop(staging_lock);
                            result
                        });
                        observe_relauncher(staged, wait, cx);
                    }
                    quit(cx);
                }
                Err(error) => {
                    state.restart = RestartStatus::Failed(error);
                    state.card_collapsed = false;
                    cx.set_global(state);
                    cx.refresh_windows();
                }
            }
        });
    })
    .detach();
}

/// A launched child can fail before the durable quit finishes (including when
/// the user cancels the unsaved-file prompt). Reap it and make retry available.
fn observe_relauncher(staged: StagedUpdate, wait: gpui::Task<Result<(), String>>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let result = wait.await;
        cx.update(move |cx| {
            let mut state = cx.try_global::<UpdateState>().cloned().unwrap_or_default();
            if state.staged.as_ref() != Some(&staged) || !state.restart.is_starting() {
                return;
            }
            state.restart = RestartStatus::Failed(result.err().unwrap_or_else(|| {
                crate::i18n::catalog::updater_relaunch::CARD_RESTART_EXITED
                    .get()
                    .to_owned()
            }));
            state.card_collapsed = false;
            cx.set_global(state);
            cx.refresh_windows();
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use gpui::TestAppContext;

    use super::*;
    use crate::updater::CheckStatus;

    fn staged(version: &str) -> StagedUpdate {
        StagedUpdate {
            state_dir: "/test/update".into(),
            app_bundle: "/test/Krill.app".into(),
            from_version: "0.1.0".into(),
            to_version: version.into(),
            manifest_url: "https://example.test/latest.json".into(),
            relaunched: false,
        }
    }

    fn ready(staged: &StagedUpdate, cx: &mut App) {
        cx.set_global(UpdateState {
            status: CheckStatus::Ready(staged.clone()),
            staged: Some(staged.clone()),
            ..Default::default()
        });
    }

    #[gpui::test]
    fn restart_failure_keeps_payload_and_allows_retry(cx: &mut TestAppContext) {
        let staged = staged("0.2.0");
        let quits = Arc::new(AtomicUsize::new(0));
        cx.update(|cx| {
            ready(&staged, cx);
            let quits = Arc::clone(&quits);
            restart_with(
                staged.clone(),
                cx,
                || Err("launcher unavailable".into()),
                move |_| {
                    quits.fetch_add(1, Ordering::SeqCst);
                },
            );
            assert!(cx.global::<UpdateState>().restart.is_starting());
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let state = cx.global::<UpdateState>();
            assert_eq!(state.staged.as_ref(), Some(&staged));
            assert_eq!(state.restart.error(), Some("launcher unavailable"));
            assert!(!state.card_collapsed);
            assert_eq!(quits.load(Ordering::SeqCst), 0);
            let quits = Arc::clone(&quits);
            restart_with(
                staged.clone(),
                cx,
                || Ok(None),
                move |_| {
                    quits.fetch_add(1, Ordering::SeqCst);
                },
            );
        });
        cx.run_until_parked();
        assert_eq!(quits.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn restart_blocks_duplicate_clicks_until_durable_quit(cx: &mut TestAppContext) {
        let staged = staged("0.2.0");
        let starts = Arc::new(AtomicUsize::new(0));
        let quits = Arc::new(AtomicUsize::new(0));
        cx.update(|cx| {
            ready(&staged, cx);
            for _ in 0..2 {
                let starts = Arc::clone(&starts);
                let quits = Arc::clone(&quits);
                restart_with(
                    staged.clone(),
                    cx,
                    move || {
                        starts.fetch_add(1, Ordering::SeqCst);
                        Ok(None)
                    },
                    move |_| {
                        quits.fetch_add(1, Ordering::SeqCst);
                    },
                );
            }
        });
        cx.run_until_parked();
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(quits.load(Ordering::SeqCst), 1);
        cx.update(|cx| {
            assert!(cx.global::<UpdateState>().restart.is_starting());
            let starts = Arc::clone(&starts);
            restart_with(
                staged,
                cx,
                move || {
                    starts.fetch_add(1, Ordering::SeqCst);
                    Ok(None)
                },
                |_| {},
            );
        });
        cx.run_until_parked();
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn restart_ignores_replaced_payload_completion(cx: &mut TestAppContext) {
        let first = staged("0.2.0");
        let next = staged("0.3.0");
        let quits = Arc::new(AtomicUsize::new(0));
        cx.update(|cx| {
            ready(&first, cx);
            let quits = Arc::clone(&quits);
            restart_with(
                first.clone(),
                cx,
                || Ok(None),
                move |_| {
                    quits.fetch_add(1, Ordering::SeqCst);
                },
            );
            ready(&next, cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let state = cx.global::<UpdateState>();
            assert_eq!(state.staged.as_ref(), Some(&next));
            assert_eq!(state.restart, RestartStatus::Idle);
        });
        assert_eq!(quits.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn restart_rejects_a_stale_card_before_launch(cx: &mut TestAppContext) {
        let first = staged("0.2.0");
        let next = staged("0.3.0");
        let starts = Arc::new(AtomicUsize::new(0));
        cx.update(|cx| {
            ready(&next, cx);
            let starts = Arc::clone(&starts);
            restart_with(
                first,
                cx,
                move || {
                    starts.fetch_add(1, Ordering::SeqCst);
                    Ok(None)
                },
                |_| {},
            );
        });
        cx.run_until_parked();
        assert_eq!(starts.load(Ordering::SeqCst), 0);
    }

    #[gpui::test]
    fn restart_waits_for_the_manual_check_before_launch(cx: &mut TestAppContext) {
        let staged = staged("0.2.0");
        let starts = Arc::new(AtomicUsize::new(0));
        cx.update(|cx| {
            ready(&staged, cx);
            let mut state = cx.global::<UpdateState>().clone();
            state.status = CheckStatus::Checking;
            cx.set_global(state);
            let starts = starts.clone();
            restart_with(
                staged,
                cx,
                move || {
                    starts.fetch_add(1, Ordering::SeqCst);
                    Ok(None)
                },
                |_| panic!("checking must not quit"),
            );
        });
        cx.run_until_parked();
        assert_eq!(starts.load(Ordering::SeqCst), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn restart_claim_serializes_publish_and_rejects_replaced_payload() {
        use std::fs;
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                fs::remove_dir_all(&self.0).ok();
            }
        }
        let dir =
            Cleanup(std::env::temp_dir().join(format!("restart-claim-{}", std::process::id())));
        fs::remove_dir_all(&dir.0).ok();
        fs::create_dir_all(&dir.0).unwrap();
        let mut staged = staged("0.2.0");
        staged.app_bundle = dir.0.join("Krill.app");
        let payload = super::super::staging_path(&staged.app_bundle);
        fs::create_dir_all(payload.join("Contents")).unwrap();
        let write = |version: &str| {
            fs::write(payload.join("Contents/Info.plist"), format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>")).unwrap();
        };
        write("0.2.0");
        let guard = claim_payload(&staged).unwrap();
        let publisher = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(super::super::append_ext(&staged.app_bundle, "update.lock"))
            .unwrap();
        assert!(
            publisher.try_lock().is_err(),
            "publisher cannot replace the claimed bytes"
        );
        drop(guard);
        publisher.try_lock().unwrap();
        write("0.3.0");
        drop(publisher);
        assert!(
            claim_payload(&staged).is_err(),
            "old card must not launch the new bytes"
        );
        staged.to_version = "0.3.0".into();
        assert!(claim_payload(&staged).is_ok());
    }

    #[gpui::test]
    fn restart_helper_exit_recovers_cancelled_or_delayed_quit(cx: &mut TestAppContext) {
        let staged = staged("0.2.0");
        cx.update(|cx| {
            ready(&staged, cx);
            let mut state = cx.global::<UpdateState>().clone();
            state.restart = RestartStatus::Starting;
            state.card_collapsed = true;
            cx.set_global(state);
            let wait = cx.background_spawn(async { Ok(()) });
            observe_relauncher(staged.clone(), wait, cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            let state = cx.global::<UpdateState>();
            assert_eq!(state.staged.as_ref(), Some(&staged));
            assert!(state.restart.error().is_some());
            assert!(!state.card_collapsed);
            restart_with(staged, cx, || Err("retry reached launcher".into()), |_| {});
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                cx.global::<UpdateState>().restart.error(),
                Some("retry reached launcher")
            );
        });
    }

    #[gpui::test]
    fn restart_helper_exit_does_not_replace_a_new_update(cx: &mut TestAppContext) {
        let first = staged("0.2.0");
        let next = staged("0.3.0");
        cx.update(|cx| {
            ready(&first, cx);
            let mut state = cx.global::<UpdateState>().clone();
            state.restart = RestartStatus::Starting;
            cx.set_global(state);
            let wait = cx.background_spawn(async { Err("old child exited".into()) });
            observe_relauncher(first, wait, cx);
            ready(&next, cx);
        });
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(cx.global::<UpdateState>().staged.as_ref(), Some(&next));
            assert_eq!(cx.global::<UpdateState>().restart, RestartStatus::Idle);
        });
    }
}
