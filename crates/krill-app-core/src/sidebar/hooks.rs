//! 저장소 훅 감지와 저장은 UI 스레드에서 파일·프로세스 잠금을 기다리지 않는다.

use std::path::PathBuf;
use std::time::Duration;

use gpui::Context;
use krill_proto::orchestration::{RepoHookSummary, WorktreeStore, inspect_repo_hooks};

use super::Sidebar;

#[derive(Default)]
pub(super) struct SetupPrompt {
    pub(super) summary: Option<RepoHookSummary>,
    task: Option<gpui::Task<()>>,
    saving: bool,
    generation: u64,
}

impl Sidebar {
    pub(super) fn setup_prompt_dir(&self, cx: &gpui::App) -> Option<PathBuf> {
        self.state_dir()
            .map(std::path::Path::to_path_buf)
            .or_else(|| crate::settings::state_dir(cx))
    }

    pub(super) fn ensure_setup_prompt_polling(&mut self, cx: &mut Context<Self>) {
        if self.setup_prompt.task.is_some()
            || self.setup_prompt.saving
            || self.snapshot.trees.is_empty()
        {
            return;
        }
        let Some(dir) = self.setup_prompt_dir(cx) else {
            return;
        };
        self.setup_prompt.task = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            loop {
                let Ok(repos) = this.read_with(cx, |s, _| {
                    if s.is_visible() {
                        s.snapshot.trees.keys().cloned().collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    }
                }) else {
                    return;
                };
                let generation = this
                    .read_with(cx, |s, _| (s.setup_prompt.generation, s.disk.generation))
                    .ok();
                let state_dir = dir.clone();
                let (repos, summary) = bg
                    .spawn(async move {
                        let summary = repos
                            .iter()
                            .map(|repo| inspect_repo_hooks(repo, Some(&state_dir)))
                            .find(RepoHookSummary::should_show_setup_prompt);
                        (repos, summary)
                    })
                    .await;
                if this
                    .update(cx, |s, cx| {
                        // 스캔 중 저장소 목록이 바뀌었으면 이전 카드가 다시 나타나지 않는다.
                        if generation == Some((s.setup_prompt.generation, s.disk.generation))
                            && !s.setup_prompt.saving
                            && s.is_visible()
                            && s.snapshot.trees.keys().eq(repos.iter())
                            && s.setup_prompt.summary != summary
                        {
                            s.setup_prompt.summary = summary;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    return;
                }
                bg.timer(Duration::from_secs(2)).await;
            }
        }));
    }

    pub(super) fn save_setup_prompt(
        &mut self,
        dir: PathBuf,
        key: String,
        script: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.setup_prompt.saving {
            return;
        }
        // 진행 중인 감지 결과가 저장 완료 뒤 이전 카드를 되살리지 않게 취소한다.
        self.setup_prompt.generation = self.setup_prompt.generation.wrapping_add(1);
        self.setup_prompt.task = None;
        self.setup_prompt.saving = true;
        self.enqueue_disk_write_with_completion(
            move || {
                WorktreeStore::mutate(&dir, |store| {
                    if let Some(script) = script {
                        store.set_setup_script(&key, &script)?;
                    } else {
                        store.set_setup_prompt_dismissed(&key, true);
                    }
                    Ok(())
                })
            },
            |s, result, _| {
                s.setup_prompt.saving = false;
                match result {
                    Ok(()) => s.setup_prompt.summary = None,
                    Err(error) => s.notice = Some(error.to_string()),
                }
            },
            cx,
        );
    }
}
