//! PR polling is separate from daemon snapshots so a slow gh cannot delay agent status.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::Context;

use super::Sidebar;
use crate::pull_request::{Service, Status};

impl Sidebar {
    fn pr_branches(&self) -> BTreeMap<PathBuf, String> {
        self.meta
            .iter()
            .filter_map(|(path, tree)| {
                tree.branch
                    .as_ref()
                    .filter(|b| !b.is_empty())
                    .map(|b| (path.clone(), b.clone()))
            })
            .collect()
    }

    pub(super) fn connect_pr(&mut self, service: Arc<Service>, cx: &mut Context<Self>) {
        self.pr_service = Some(service.clone());
        if self.pr_task.is_some() {
            return;
        }
        self.pr_task = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            loop {
                let Ok(branches) = this.read_with(cx, |s, _| s.pr_branches()) else {
                    return;
                };
                let runner = service.clone();
                let (branches, statuses) = bg
                    .spawn(async move {
                        let statuses = runner.refresh(&branches, Instant::now());
                        (branches, statuses)
                    })
                    .await;
                if this
                    .update(cx, |s, cx| {
                        // Discard results if a branch changed while gh was running.
                        let current = s.pr_branches();
                        let statuses: BTreeMap<PathBuf, Status> = statuses
                            .into_iter()
                            .filter(|(path, _)| {
                                current.contains_key(path)
                                    && current.get(path) == branches.get(path)
                            })
                            .collect();
                        // 자동 조회 결과는 배지/메뉴의 상태로만 보관한다. 시작 시 네트워크·
                        // gh 실패가 모든 워크스페이스에 안내를 반복해서 띄우지 않게 한다.
                        s.pr_guidance_dismissed = true;
                        if s.prs != statuses {
                            s.prs = statuses;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    return;
                }
                bg.timer(Duration::from_secs(1)).await;
            }
        }));
    }

    pub(super) fn create_pr(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(branch) = self.meta.get(&path).and_then(|m| m.branch.clone()) else {
            return;
        };
        if self
            .prs
            .get(&path)
            .is_some_and(|s| matches!(s, Status::Missing | Status::Failed(_)))
        {
            self.notice = self
                .prs
                .get(&path)
                .and_then(Status::guidance)
                .map(str::to_owned);
            cx.notify();
            return;
        }
        let Some(service) = self.pr_service.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { service.create(&path, &branch, Instant::now()) })
                .await;
            if let Err(message) = result {
                this.update(cx, |s, cx| {
                    s.notice = Some(message.to_owned());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;
    use std::io;
    use std::path::Path;

    struct Unavailable(io::ErrorKind);

    impl crate::pull_request::Runner for Unavailable {
        fn run(&self, _: &Path, _: &str, _: crate::pull_request::Action) -> io::Result<Status> {
            Err(self.0.into())
        }
    }

    #[gpui::test]
    fn automatic_pr_failures_stay_quiet_until_user_action(cx: &mut gpui::TestAppContext) {
        for kind in [
            io::ErrorKind::Other,
            io::ErrorKind::TimedOut,
            io::ErrorKind::NotFound,
        ] {
            let sidebar = cx.new(|_| Sidebar::new_loaded(None));
            let path = PathBuf::from("/no-real-io");
            sidebar.update(cx, |sidebar, cx| {
                sidebar.meta.insert(
                    path.clone(),
                    super::super::Tree {
                        branch: Some("topic".into()),
                        dirty: None,
                    },
                );
                sidebar.connect_pr(Arc::new(Service::new(Arc::new(Unavailable(kind)))), cx);
            });
            cx.run_until_parked();
            sidebar.update(cx, |sidebar, cx| {
                assert!(sidebar.prs[&path].guidance().is_some());
                assert!(sidebar.pr_guidance_dismissed);
                assert!(sidebar.notice.is_none());
                sidebar.create_pr(path.clone(), cx);
                assert_eq!(
                    sidebar.notice.as_deref(),
                    sidebar.prs[&path].guidance(),
                    "manual actions must retain actionable guidance"
                );
            });
        }
    }
}
