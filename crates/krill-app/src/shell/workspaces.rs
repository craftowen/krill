//! 탭 표시는 활성 워크스페이스(호스트 + 워크트리/폴더)에 한정한다.
//! 전체 레이아웃은 그대로 보관하므로 다른 워크스페이스의 세션·분할·탭 속성도 유지된다.

use super::*;
use krill_app::hosts::HostId;
use std::path::Path;

#[cfg(all(test, unix))]
mod tests;

const MAX_STARTUP_WORKSPACES: usize = 64;

#[derive(Clone, Debug, Eq)]
pub(super) struct Workspace {
    pub(super) host: HostId,
    pub(super) folder: PathBuf,
}

impl PartialEq for Workspace {
    fn eq(&self, other: &Self) -> bool {
        self.host == other.host && krill_proto::path::equivalent(&self.folder, &other.folder)
    }
}

impl Shell {
    /// Reconcile daemon IDs, never launches. Appending inactive tabs preserves
    /// existing indices, split trees, focused panes and custom tab properties.
    pub(super) fn reconcile_workspace_sessions(&mut self, cx: &mut Context<Self>) {
        if self.layout_loading || self.role != WindowRole::Main {
            return;
        }
        let connected: Vec<_> = self.sidebar.read(cx).connected_sessions().collect();
        for (host, session) in connected {
            self.observed_sessions.insert((host, session));
        }
        let stale_panes: Vec<PaneId> = (0..self.layout.tabs().len())
            .flat_map(|i| self.layout.tab_panes(i))
            .filter(|&pane| {
                let Some(session) = self.layout.session(pane) else {
                    return false;
                };
                let host = self.layout.host(pane);
                let sidebar = self.sidebar.read(cx);
                sidebar.host_connected(host)
                    && (sidebar
                        .session_on(host, session)
                        .is_some_and(sidebar::Session::is_exited)
                        // A local daemon restarting for an update may briefly
                        // publish an incomplete list. Absence is not proof of
                        // PTY exit; keep saved tabs until an explicit exit.
                        || host.is_remote() && sidebar.host_has_trees(host)
                            && self.observed_sessions.contains(&(host.clone(), session))
                            && sidebar.session_on(host, session).is_none())
                    && !self.restoring_sessions.contains(&session)
                    && !self.restarting_panes.contains(&pane)
                    && !self
                        .accounts_for_host(host)
                        .details
                        .handoffs
                        .iter()
                        .any(|h| h.from == session || h.to == Some(session))
            })
            .collect();
        if !stale_panes.is_empty() {
            for &pane in &stale_panes {
                if let Some(session) = self.layout.session(pane) {
                    let host = self.layout.host(pane).clone();
                    self.workspace_closing.insert((host.clone(), session));
                    self.observed_sessions.remove(&(host, session));
                }
            }
            self.close_with_policy(close::Policy::Preserve, cx, |layout| {
                let focused = layout.focused().filter(|p| !stale_panes.contains(p));
                for index in (0..layout.tabs().len()).rev() {
                    for pane in layout.tab_panes(index) {
                        if stale_panes.contains(&pane) {
                            layout.activate(index);
                            layout.focus(pane);
                            layout.close_pane();
                        }
                    }
                }
                if let Some(focused) = focused {
                    for index in 0..layout.tabs().len() {
                        if layout.tab_panes(index).contains(&focused) {
                            layout.activate(index);
                            layout.focus(focused);
                            break;
                        }
                    }
                }
            });
        }
        self.startup_reconcile_panes
            .retain(|pane| self.layout.pane_exists(*pane));
        let primary = self
            .sidebar
            .read(cx)
            .selected_workspace()
            .map(|(host, folder)| Workspace { host, folder })
            .or_else(|| {
                // A restored layout can be ready before sidebar selection is
                // restored (or selection can be empty). Reconcile the workspace
                // already on screen without requiring a sidebar click first.
                self.tab_workspace(self.layout.active(), cx)
            });
        let mut targets = Vec::new();
        if let Some(ws) = &primary {
            targets.push(ws.clone());
        }
        for &pane in &self.startup_reconcile_panes {
            if let Some(ws) = self.pane_workspace(pane, cx)
                && !targets.contains(&ws)
            {
                targets.push(ws);
            }
        }
        if targets.is_empty() {
            return;
        }
        let active = self.layout.active();
        let focused = self.layout.focused();
        let mut imported = false;
        let mut imported_primary = false;
        for workspace in &targets {
            let added = self.import_missing_workspace_sessions(workspace, cx);
            if added && primary.as_ref() == Some(workspace) {
                imported_primary = true;
            }
            imported |= added;
        }
        if !self.startup_reconcile_panes.is_empty() {
            let pending = std::mem::take(&mut self.startup_reconcile_panes);
            for pane in pending {
                if !self.startup_pane_settled(pane, cx) {
                    self.startup_reconcile_panes.push(pane);
                }
            }
        }
        if !imported {
            return;
        }
        if let Some(focused) = focused {
            self.layout.activate(active);
            self.layout.focus(focused);
        } else if imported_primary {
            self.sync(cx);
        } else {
            self.layout.deactivate();
        }
        self.save();
        self.refresh_detached(cx);
        cx.notify();
    }

    pub(super) fn enqueue_startup_workspaces(
        &mut self,
        saved_tab_count: usize,
        saved_active: usize,
    ) {
        self.startup_reconcile_panes.clear();
        let order = (saved_active < saved_tab_count)
            .then_some(saved_active)
            .into_iter()
            .chain((0..saved_tab_count).filter(|&i| i != saved_active));
        for index in order {
            for pane in self.layout.tab_panes(index) {
                if self.layout.file(pane).is_some() || self.startup_reconcile_panes.contains(&pane)
                {
                    continue;
                }
                if self.startup_reconcile_panes.len() >= MAX_STARTUP_WORKSPACES {
                    return;
                }
                self.startup_reconcile_panes.push(pane);
            }
        }
    }

    fn workspace_awaiting_create(&self, workspace: &Workspace, cx: &App) -> bool {
        // Create can appear in the daemon list before its reply reaches the
        // launching pane. Wait for that pane's ID before importing missing IDs.
        // A pane whose Create reply was lost never receives it, so import instead.
        self.live.keys().any(|pane| {
            self.layout.session(*pane).is_none()
                && !self.outcome_unknown.contains_key(pane)
                && self.layout.file(*pane).is_none()
                && self.pane_workspace(*pane, cx).as_ref() == Some(workspace)
        })
    }

    fn import_missing_workspace_sessions(&mut self, workspace: &Workspace, cx: &App) -> bool {
        if self.workspace_awaiting_create(workspace, cx) {
            return false;
        }
        let existing = self.layout.sessions_for_host(&workspace.host);
        let missing: Vec<_> = self
            .sidebar
            .read(cx)
            .live_sessions_in_workspace(&workspace.host, &workspace.folder)
            .into_iter()
            .filter(|session| {
                !existing.contains(session)
                    && !self
                        .workspace_closing
                        .contains(&(workspace.host.clone(), *session))
            })
            .collect();
        if missing.is_empty() {
            return false;
        }
        let mut represented = existing;
        let mut imported = false;
        for session in missing {
            if represented.contains(&session) {
                continue;
            }
            self.layout
                .new_tab_after(self.layout.tabs().len().saturating_sub(1));
            if let Some(pane) = self.layout.focused() {
                self.layout.set_host(pane, workspace.host.clone());
                self.layout.set_session(pane, session);
                self.layout.set_launch(
                    pane,
                    Create {
                        account: None,
                        program: Program::Login,
                        cwd: workspace.folder.as_os_str().as_bytes().to_vec(),
                        cols: pane::SIZE.0,
                        rows: pane::SIZE.1,
                    },
                );
            }
            represented.push(session);
            imported = true;
        }
        imported
    }

    fn startup_pane_settled(&self, pane: PaneId, cx: &App) -> bool {
        let Some(workspace) = self.pane_workspace(pane, cx) else {
            return true;
        };
        if self.workspace_awaiting_create(&workspace, cx) {
            return false;
        }
        let sidebar = self.sidebar.read(cx);
        if !sidebar.host_connected(&workspace.host) || !sidebar.host_has_trees(&workspace.host) {
            return false;
        }
        if workspace.host.is_remote()
            || !sidebar
                .live_sessions_in_workspace(&workspace.host, &workspace.folder)
                .is_empty()
        {
            return true;
        }
        self.layout
            .session(pane)
            .is_none_or(|sid| sidebar.session_on(&workspace.host, sid).is_some())
    }

    pub(super) fn open_saved_workspace(
        &mut self,
        host: HostId,
        folder: &Path,
        cx: &mut Context<Self>,
    ) {
        let cleaned = host
            .is_local()
            .then(|| folder.to_str())
            .flatten()
            .and_then(|s| {
                krill_proto::orchestration::strip_workspace_instance(s).filter(|_| !folder.exists())
            })
            .map(PathBuf::from);
        let folder = cleaned.as_deref().unwrap_or(folder);
        if self.layout_loading {
            self.restore_workspace = Some((host, folder.to_path_buf()));
            return;
        }
        self.dashboard = None;
        self.sync_sidebar_visibility(cx);
        self.close_settings(None, cx);
        let workspace = Workspace {
            host: host.clone(),
            folder: folder.to_path_buf(),
        };
        if let Some(index) =
            (0..self.layout.tabs().len()).find(|i| self.tab_contains_workspace(*i, &workspace, cx))
        {
            self.change(true, cx, |l| {
                l.activate(index);
            });
        } else {
            self.new_folder_tab_on(host, folder, Program::Login, cx);
        }
    }

    /// 숨은 로컬 칸도 재부팅 복원 대상이다. 화면 스트림은 열지 않아 MK-035 예산을 유지한다.
    pub(super) fn restore_hidden_sessions(&mut self, visible: &[PaneId], cx: &mut Context<Self>) {
        // 재부팅 직후엔 앱이 krilld보다 먼저 떠서 이른 재개가 연결 실패로 끝나고 다시 시도되지 않았다
        // (2026-10-28: 앱 기동 뒤 데몬 수신까지 약 8초). 감시 연결이 붙으면(`set_link`) 다시 불린다.
        if self.role != WindowRole::Main || self.link != Link::Up {
            return;
        }
        let Some(target) = self.local_target() else {
            return;
        };
        let visible_sessions: Vec<_> = visible
            .iter()
            .filter_map(|p| self.layout.session(*p))
            .collect();
        let candidates: Vec<_> = (0..self.layout.tabs().len())
            .flat_map(|i| self.layout.tab_panes(i))
            .filter(|p| self.layout.host(*p).is_local() && self.layout.file(*p).is_none())
            .filter_map(|p| self.layout.session(p).map(|id| (p, id)))
            .filter(|(_, id)| {
                !visible_sessions.contains(id)
                    && !self.observed_sessions.contains(&(HostId::Local, *id))
                    && !self.workspace_closing.contains(&(HostId::Local, *id))
                    && !self.restore_attempted.contains(id)
                    && self
                        .sidebar
                        .read(cx)
                        .session_on(&HostId::Local, *id)
                        .is_none()
            })
            .collect();
        for (pane_id, old) in candidates {
            if !self.restoring_sessions.insert(old) {
                continue;
            }
            self.restore_attempted.insert(old);
            let mut launch = self.launch_for(pane_id);
            if let Some(cwd) = self.layout.respawn_cwd(pane_id) {
                super::apply_respawn_cwd(&mut launch.cwd, cwd, false);
            }
            let target = target.clone();
            let creation = Arc::clone(self.creations.entry(pane_id).or_default());
            cx.spawn(async move |this, cx| {
                let got = cx
                    .background_executor()
                    .spawn(async move {
                        if !pane::supports_resume_history(&target)? {
                            return Ok(None);
                        }
                        let (session, created) =
                            pane::ensure_live_session_in(&target, Some(old), &launch)?;
                        if created && !close::Creation::publish(&creation, session) {
                            close::send(
                                &krill_app::resources::daemon_call_for(Some(target)),
                                session,
                            )?;
                            return Err(std::io::ErrorKind::Interrupted.into());
                        }
                        Ok::<_, std::io::Error>(Some(session))
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.restoring_sessions.remove(&old);
                    // 재개 요청이 닿았는지 모르면, 보이게 된 칸도 다시 재개하지 않고 아는 세션만 찾는다.
                    let unknown = got.as_ref().is_err_and(pane::is_unknown_outcome);
                    if let Err(e) = &got
                        && unknown
                    {
                        this.outcome_unknown
                            .insert(pane_id, super::start_error_text(e));
                    }
                    // 정상 종료(closed)된 세션은 데몬이 재개를 거부하므로 그 워크트리는 계속 숨는다.
                    if (unknown || matches!(got, Ok(Some(_))))
                        && let Some(workspace) = this.pane_workspace(pane_id, cx)
                    {
                        this.sidebar.update(cx, |s, cx| {
                            s.reveal_restored_worktree(workspace.folder, cx);
                        });
                    }
                    let session = match got {
                        Ok(Some(new)) => {
                            this.observed_sessions.insert((HostId::Local, new));
                            if new != old {
                                for rebound in this.layout.rebind_session(&HostId::Local, old, new)
                                {
                                    this.drop_stale_pastes_for_pane(rebound, Some(new), cx);
                                }
                                this.save();
                            }
                            new
                        }
                        // 실패·구 데몬이면 옛 ID를 유지한다. 선택돼 있었다면 콜드 폴백을 보여준다.
                        _ => old,
                    };
                    if this
                        .layout
                        .visible()
                        .iter()
                        .any(|p| this.layout.session(*p) == Some(session))
                    {
                        this.sync(cx);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    pub(super) fn tab_workspace(&self, index: usize, cx: &App) -> Option<Workspace> {
        let selected = self
            .sidebar
            .read(cx)
            .selected_workspace()
            .map(|(host, folder)| Workspace { host, folder });
        if let Some(selected) =
            selected.filter(|workspace| self.tab_contains_workspace(index, workspace, cx))
        {
            return Some(selected);
        }
        // Without an explicit matching selection, split focus alone must not
        // change the workspace shown in the tab bar.
        self.pane_workspace(*self.layout.tab_panes(index).first()?, cx)
    }

    fn tab_contains_workspace(&self, index: usize, workspace: &Workspace, cx: &App) -> bool {
        self.layout
            .tab_panes(index)
            .into_iter()
            .any(|pane| self.pane_workspace(pane, cx).as_ref() == Some(workspace))
    }

    pub(super) fn active_workspace(&self, cx: &App) -> Workspace {
        if let Some(ws) = self.tab_workspace(self.layout.active(), cx) {
            return ws;
        }
        if let Some((host, folder)) = self.sidebar.read(cx).selected_workspace() {
            return Workspace { host, folder };
        }
        Workspace {
            host: self.launch_host.clone(),
            folder: self.folder(),
        }
    }

    fn pane_workspace(&self, pane: PaneId, cx: &App) -> Option<Workspace> {
        if !self.layout.pane_exists(pane) {
            return None;
        }
        let host = self.layout.host(pane).clone();
        // 실행 폴더는 영속 상태다. 라이브 cwd(cd)는 워크스페이스 소속을 바꾸지 않는다.
        let folder = self
            .layout
            .launch(pane)
            .filter(|launch| !launch.cwd.is_empty())
            .map(|launch| PathBuf::from(std::ffi::OsString::from_vec(launch.cwd.clone())))
            .or_else(|| {
                self.layout
                    .session(pane)
                    .and_then(|sid| self.sidebar.read(cx).folder_on(&host, sid))
                    .map(ToOwned::to_owned)
            })
            .or_else(|| {
                self.layout
                    .file(pane)
                    .and_then(|file| self.sidebar.read(cx).workspace_for_path_on(&host, file))
            })
            .unwrap_or_else(|| {
                PathBuf::from(std::ffi::OsString::from_vec(self.launch_for(pane).cwd))
            });
        let folder = self
            .sidebar
            .read(cx)
            .workspace_for_path_on(&host, &folder)
            .unwrap_or(folder);
        Some(Workspace { host, folder })
    }

    pub(super) fn open_workspace_on(
        &mut self,
        host: HostId,
        folder: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let folder = self
            .sidebar
            .read(cx)
            .workspace_for_path_on(&host, &folder)
            .unwrap_or(folder);
        self.launch_host = host.clone();
        let target = self.layout.mru_order().into_iter().find(|&index| {
            self.tab_workspace(index, cx).is_some_and(|w| {
                w.host == host && krill_proto::path::equivalent(&w.folder, &folder)
            })
        });
        if let Some(index) = target {
            self.dashboard = None;
            self.close_settings(None, cx);
            self.launch_host = host;
            self.change(true, cx, |l| l.activate(index));
            return;
        }
        let session = self.sidebar.read(cx).session_for_workspace(&host, &folder);
        if let Some(session) = session {
            self.open_session_on(host, session, cx);
        } else {
            self.new_folder_tab_on(host, &folder, Program::Login, cx);
        }
    }

    pub(super) fn workspace_tabs(&self, cx: &App) -> Vec<usize> {
        self.tabs_in_workspace(self.layout.active(), cx)
    }

    pub(super) fn tabs_in_workspace(&self, index: usize, cx: &App) -> Vec<usize> {
        let Some(workspace) = self.tab_workspace(index, cx) else {
            return Vec::new();
        };
        (0..self.layout.tabs().len())
            .filter(|&i| self.tab_contains_workspace(i, &workspace, cx))
            .collect()
    }

    pub(super) fn cycle_workspace_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.cycle_workspace_tab_matching(delta, None, cx);
    }

    pub(super) fn cycle_workspace_tab_matching(
        &mut self,
        delta: isize,
        editor: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        let tabs: Vec<_> = self
            .workspace_tabs(cx)
            .into_iter()
            .filter(|&i| editor.is_none_or(|editor| self.layout.is_editor_tab(i) == editor))
            .collect();
        if tabs.is_empty() {
            return;
        }
        if let Some(pos) = tabs.iter().position(|&i| i == self.layout.active()) {
            let next = (pos.cast_signed() + delta)
                .rem_euclid(tabs.len().cast_signed())
                .cast_unsigned();
            self.change(true, cx, |l| l.activate(tabs[next]));
        } else {
            let active = self.layout.active();
            let next = if delta > 0 {
                tabs.iter().find(|&&i| i > active).or_else(|| tabs.first())
            } else {
                tabs.iter()
                    .rev()
                    .find(|&&i| i < active)
                    .or_else(|| tabs.last())
            };
            if let Some(&next) = next {
                self.change(true, cx, |l| l.activate(next));
            }
        }
    }

    /// 전체 인덱스를 유지해 닫기·드래그 등 기존 액션과 같은 탭을 가리킨다.
    pub(super) fn workspace_tab_labels(&self, cx: &App) -> Vec<(usize, String)> {
        let indices = self.workspace_tabs(cx);
        let mut labels: Vec<_> = indices
            .iter()
            .map(|&i| self.tab_display_label(i, cx))
            .collect();
        tabs::number_duplicates(&mut labels);
        indices.into_iter().zip(labels).collect()
    }

    pub(super) fn close_workspace_tabs(
        &mut self,
        closing: Vec<usize>,
        prefer: usize,
        cx: &mut Context<Self>,
    ) {
        let tabs = self.workspace_tabs(cx);
        let remaining = std::iter::once(prefer)
            .filter(|i| !closing.contains(i))
            .chain(
                self.layout
                    .mru_order()
                    .into_iter()
                    .filter(|i| tabs.contains(i)),
            )
            .chain(tabs.iter().copied().filter(|&i| i > prefer))
            .chain(tabs.iter().rev().copied().filter(|&i| i < prefer))
            .find(|i| !closing.contains(i))
            .and_then(|i| self.layout.tabs().get(i))
            .map(|t| t.focused);
        for &i in &closing {
            self.record_closed_tab(i, cx);
        }
        self.close_layout(cx, |l| {
            for i in closing.into_iter().rev() {
                l.close_tab(i);
            }
            if let Some(pane) = remaining
                && let Some(index) = l.tabs().iter().position(|tab| tab.root.contains(pane))
            {
                l.activate(index);
                l.focus(pane);
            }
        });
    }
}
