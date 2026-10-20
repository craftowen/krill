//! Disk state has one background reader and ordered, detached background writes.
//! UI readers only inspect the last accepted in-memory snapshot.

use super::*;
use krill_proto::orchestration::{OrchestrationStore, WorktreeStore};

#[derive(Default)]
pub(super) struct State {
    pub task: Option<Task<()>>,
    pub initialized: bool,
    pub dirty: bool,
    pub startup: startup::Edits,
    pub generation: u64,
    pub remote_generation: u64,
    pub remote_refresh: RemoteRefresh,
    #[cfg(all(test, unix))]
    pub next_initial_io: Option<tokio::sync::oneshot::Receiver<()>>,
    #[cfg(all(test, unix))]
    pub next_remote_io: Option<tokio::sync::oneshot::Receiver<()>>,
    #[cfg(all(test, unix))]
    pub remote_starts: usize,
    pub poll_generation: u64,
    writes_pending: usize,
    pub store: WorktreeStore,
    pub orchestration: OrchestrationStore,
    imported: Snapshot,
    imported_meta: Meta,
    pub folder_updated_at: BTreeMap<PathBuf, u64>,
}

#[derive(Default)]
pub(super) struct RemoteRefresh {
    pub pending: bool,
    pub again: bool,
}

#[derive(Default)]
struct PendingWrites(Option<tokio::sync::oneshot::Receiver<()>>);
impl gpui::Global for PendingWrites {}
#[derive(Default)]
struct PendingInitializations(Vec<tokio::sync::oneshot::Receiver<()>>);
impl gpui::Global for PendingInitializations {}

fn ensure_write_queue(cx: &mut App) {
    if cx.has_global::<PendingWrites>() {
        return;
    }
    cx.set_global(PendingWrites::default());
    cx.set_global(PendingInitializations::default());
    // The app owns the tail even after every sidebar window has closed.
    cx.on_app_quit(|cx| {
        let tail = cx.global_mut::<PendingWrites>().0.take();
        cx.background_executor().spawn(async move {
            if let Some(tail) = tail {
                tail.await.ok();
            }
        })
    })
    .detach();
}

/// Await all writes queued before this call without blocking the UI. Explicit quit
/// awaits this before `cx.quit()`, outside GPUI's short `on_app_quit` timeout.
pub fn flush_file_io(cx: &mut App) -> Task<()> {
    ensure_write_queue(cx);
    cx.spawn(async move |cx| {
        // An edit made during startup is saved by its completion callback. Await
        // that callback before capturing the write tail for explicit quit.
        loop {
            let initializers =
                cx.update(|cx| std::mem::take(&mut cx.global_mut::<PendingInitializations>().0));
            if initializers.is_empty() {
                break;
            }
            for initializer in initializers {
                initializer.await.ok();
            }
        }
        let completed = cx.update(|cx| {
            let previous = cx.global_mut::<PendingWrites>().0.take();
            let (finish, tail) = tokio::sync::oneshot::channel();
            let (completed, wait) = tokio::sync::oneshot::channel();
            cx.global_mut::<PendingWrites>().0 = Some(tail);
            cx.background_executor()
                .spawn(async move {
                    if let Some(previous) = previous {
                        previous.await.ok();
                    }
                    finish.send(()).ok();
                    completed.send(()).ok();
                })
                .detach();
            wait
        });
        completed.await.ok();
    })
}

struct Startup {
    state: PersistedSidebarState,
    error: Option<String>,
    writable: bool,
    agents: Vec<AgentProfile>,
}

impl Startup {
    fn load(path: Option<&Path>) -> Self {
        let mut error = None;
        let mut writable = true;
        let state = match path.map(load_state) {
            Some(Ok(state)) => state,
            Some(Err(err)) if err.kind() != io::ErrorKind::NotFound => {
                writable = path.is_some_and(|p| matches!(p.try_exists(), Ok(false)));
                error = Some(err.to_string());
                Default::default()
            }
            _ => Default::default(),
        };
        if let Some(dir) = path.and_then(Path::parent)
            && !state.2.parents.is_empty()
            && !dir.join(external::COMMENTS_FILE).exists()
        {
            let migration = WorktreeStore::mutate(dir, |store| {
                for (child, parent) in &state.2.parents {
                    let child = child.to_string_lossy().into_owned();
                    let parent = parent.to_string_lossy().into_owned();
                    store.set_parent(&child, Some(&parent))?;
                    store.parents.insert(child, parent);
                }
                Ok(())
            });
            if let Err(err) = migration {
                error = Some(err.to_string());
            }
        }
        Self {
            state,
            error,
            writable,
            agents: sort_agents(
                Agents::load(None)
                    .unwrap_or_default()
                    .profiles
                    .into_iter()
                    .map(|(id, p)| AgentProfile { id, name: p.name })
                    .collect(),
            ),
        }
    }

    fn apply(self, sidebar: &mut Sidebar) {
        let edits = std::mem::take(&mut sidebar.disk.startup);
        let (prefs, options, lin, ui, order) = edits.merge(self.state, startup::capture(sidebar));
        sidebar.project_order = order;
        sidebar.prefs = prefs;
        sidebar.options = options;
        sidebar.collapsed = ui.collapsed;
        sidebar.collapsed_groups = ui.collapsed_groups;
        sidebar.collapsed_hosts = ui.collapsed_hosts;
        sidebar.expanded_cards = ui.expanded_cards;
        sidebar.shown_external = ui.shown_external;
        sidebar.removed_projects = ui.removed_projects;
        sidebar.selected_worktree = ui.selected_worktree;
        sidebar.parents = lin.parents;
        sidebar.collapsed_lineage = lin.collapsed_lineage;
        sidebar.sleeping_worktrees = lin.sleeping_worktrees;
        if sidebar.agents.is_empty() {
            sidebar.agents = self.agents;
        }
        if !self.writable {
            sidebar.path = None;
        }
        if self.error.is_some() {
            sidebar.error = self.error;
        }
        sidebar.disk.initialized = true;
    }
}

pub(super) struct DiskSnapshot {
    basis: Snapshot,
    snapshot: Snapshot,
    imported: Snapshot,
    imported_meta: Meta,
    imported_paths: BTreeSet<String>,
    prefs: Preferences,
    comments: BTreeMap<PathBuf, String>,
    issues: BTreeMap<PathBuf, String>,
    statuses: BTreeMap<PathBuf, krill_proto::orchestration::WorkspaceStatus>,
    parents: BTreeMap<PathBuf, PathBuf>,
    updated_at: BTreeMap<PathBuf, u64>,
    store: WorktreeStore,
    orchestration: OrchestrationStore,
    auto_titles: BTreeMap<SessionId, String>,
    visuals: BTreeMap<PathBuf, external::ProjectVisualMeta>,
    folder_updated_at: BTreeMap<PathBuf, u64>,
}

impl DiskSnapshot {
    pub fn load(path: Option<&Path>, mut snapshot: Snapshot, mut meta: Meta) -> Self {
        let basis = snapshot.clone();
        let dir = path
            .and_then(Path::parent)
            .filter(|p| !p.as_os_str().is_empty());
        let mut store = WorktreeStore::default();
        let mut orchestration = OrchestrationStore::default();
        let mut imported = Snapshot::default();
        let mut imported_meta = Meta::new();
        let mut imported_paths = BTreeSet::new();
        let mut prefs = Preferences::new();
        let mut comments = external::load_comments_from_sidebar_path(path);
        let mut issues = BTreeMap::new();
        let mut statuses = BTreeMap::new();
        if let Some(dir) = dir {
            store = WorktreeStore::load(dir).unwrap_or_default();
            orchestration = OrchestrationStore::load(dir).unwrap_or_default();
            external::include_imported_projects(dir, &mut snapshot.trees, &mut meta);
            external::include_imported_projects(dir, &mut imported.trees, &mut imported_meta);
            imported_paths = external::imported_repo_paths(dir);
            external::apply_imported_project_prefs(
                dir,
                &mut prefs,
                &mut comments,
                &mut issues,
                &mut statuses,
            );
        }
        let mut parents = BTreeMap::new();
        for (child, parent) in &store.parents {
            let child = PathBuf::from(child);
            let parent = PathBuf::from(parent);
            if lineage::can_assign_parent(&parents, &child, &parent) {
                parents.insert(child, parent);
            }
        }
        for (key, value) in &store.statuses {
            if let Ok(status) = krill_proto::orchestration::WorkspaceStatus::parse(value) {
                statuses.insert(PathBuf::from(key), status);
            }
        }
        for (key, value) in &store.issues {
            issues.insert(PathBuf::from(key), value.clone());
        }
        let updated_at = store
            .updated_at
            .iter()
            .map(|(k, v)| (PathBuf::from(k), *v))
            .collect();
        let repos = snapshot.trees.keys().cloned().collect::<Vec<_>>();
        let folders = snapshot
            .sessions
            .iter()
            .filter(|s| s.worktree.is_none())
            .filter_map(|s| s.cwd.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let visuals = external::resolve_project_visuals(dir, &repos, &folders);
        let folder_updated_at = folders
            .iter()
            .filter_map(|p| {
                let ts = fs::metadata(p)
                    .ok()?
                    .modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                Some((p.clone(), ts))
            })
            .collect();
        Self {
            basis,
            snapshot,
            imported,
            imported_meta,
            imported_paths,
            prefs,
            comments,
            issues,
            statuses,
            parents,
            updated_at,
            store,
            orchestration,
            auto_titles: load_auto_titles_from_sidebar_path(path),
            visuals,
            folder_updated_at,
        }
    }
}

fn folder_paths(snapshot: &Snapshot) -> BTreeSet<&Path> {
    snapshot
        .sessions
        .iter()
        .filter(|s| s.worktree.is_none())
        .filter_map(|s| s.cwd.as_deref())
        .collect()
}

impl Sidebar {
    pub(super) fn ensure_external_polling(&mut self, cx: &mut Context<Self>) {
        ensure_write_queue(cx);
        if self.disk.task.is_some() {
            return;
        }
        let startup_finished = if self.disk.initialized {
            None
        } else {
            let (send, recv) = tokio::sync::oneshot::channel();
            cx.global_mut::<PendingInitializations>().0.push(recv);
            Some(send)
        };
        // Keep only the finite initialization alive until pre-load edits have
        // entered the detached write queue. Closing a window must not cancel
        // those edits; release the owner before the weak polling loop starts.
        let startup_owner = (!self.disk.initialized).then(|| cx.entity());
        // Legacy lineage migration writes too. Serialize this initial read/migration
        // with edits already submitted or made while startup is in flight.
        let initial = (!self.disk.initialized).then(|| {
            let path = self.path.clone();
            let previous = cx.global_mut::<PendingWrites>().0.take();
            let (finish, tail) = tokio::sync::oneshot::channel();
            cx.global_mut::<PendingWrites>().0 = Some(tail);
            #[cfg(all(test, unix))]
            let gate = self.disk.next_initial_io.take();
            cx.background_executor().spawn(async move {
                if let Some(previous) = previous {
                    previous.await.ok();
                }
                #[cfg(all(test, unix))]
                if let Some(gate) = gate {
                    gate.await.ok();
                }
                let loaded = Startup::load(path.as_deref());
                finish.send(()).ok();
                loaded
            })
        });
        self.disk.task = Some(cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            if let Some(initial) = initial {
                let startup = initial.await;
                if this
                    .update(cx, |s, cx| {
                        startup.apply(s);
                        if s.disk.dirty {
                            s.disk.dirty = false;
                            s.write_state(cx);
                        }
                        s.rebuild(cx);
                    })
                    .is_err()
                {
                    return;
                }
            }
            if let Some(startup_finished) = startup_finished {
                startup_finished.send(()).ok();
            }
            drop(startup_owner);
            loop {
                let Ok(Some((path, snapshot, meta, generation))) = this.read_with(cx, |s, _| {
                    (s.is_visible() && s.disk.writes_pending == 0).then(|| {
                        (
                            s.path.clone(),
                            s.snapshot.clone(),
                            s.meta.clone(),
                            s.disk.generation,
                        )
                    })
                }) else {
                    if this.read_with(cx, |_, _| ()).is_err() {
                        return;
                    }
                    bg.timer(Duration::from_millis(100)).await;
                    continue;
                };
                let data = bg
                    .spawn(async move { DiskSnapshot::load(path.as_deref(), snapshot, meta) })
                    .await;
                if this
                    .update(cx, |s, cx| {
                        s.accept_disk_snapshot(generation, data, cx);
                    })
                    .is_err()
                {
                    return;
                }
                bg.timer(Duration::from_secs(2)).await;
            }
        }));
    }

    fn accept_disk_snapshot(
        &mut self,
        generation: u64,
        data: DiskSnapshot,
        cx: &mut Context<Self>,
    ) {
        // A scan started before an edit/delete/import must never resurrect its state.
        if self.disk.generation == generation
            && self.disk.writes_pending == 0
            && self.snapshot.trees == data.basis.trees
            && folder_paths(&self.snapshot) == folder_paths(&data.basis)
            && self.disk_snapshot_changed(&data)
        {
            let changed = self.apply_disk_snapshot(data);
            if changed {
                self.write_state(cx);
            }
            self.rebuild(cx);
        }
    }

    fn disk_snapshot_changed(&self, data: &DiskSnapshot) -> bool {
        self.snapshot.trees != data.snapshot.trees
            || self.project_visuals != data.visuals
            || self.disk.store != data.store
            || self.disk.orchestration != data.orchestration
            || self.disk.folder_updated_at != data.folder_updated_at
            || (self.path.is_some()
                && (self.comments != data.comments || self.auto_titles != data.auto_titles))
            || data.prefs.keys().any(|key| !self.prefs.contains_key(key))
    }

    pub(super) fn apply_disk_snapshot(&mut self, data: DiskSnapshot) -> bool {
        self.snapshot.trees = data.snapshot.trees;
        for (key, value) in &data.imported_meta {
            self.meta
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        if self.path.is_some() {
            self.comments = data.comments;
            self.auto_titles = data.auto_titles;
        }
        self.project_visuals = data.visuals;
        self.disk.folder_updated_at = data.folder_updated_at;
        self.disk.store = data.store;
        self.disk.orchestration = data.orchestration;
        self.disk.imported = data.imported;
        self.disk.imported_meta = data.imported_meta;
        // Disk is the SSOT for lineage after preceding writes finish.
        let auto = if self.state_dir().is_some() {
            self.parents = data.parents;
            self.worktree_statuses = data.statuses;
            self.worktree_updated_at = data.updated_at;
            self.issues = data.issues;
            self.auto_collapse_idle_heavy_repos(&data.imported_paths)
        } else {
            false
        };
        for (key, value) in data.prefs {
            self.prefs.entry(key).or_insert(value);
        }
        self.prune_stale_worktree_state();
        auto
    }

    pub(super) fn include_imported_snapshot(&mut self) -> bool {
        for (repo, trees) in &self.disk.imported.trees {
            let list = self.snapshot.trees.entry(repo.clone()).or_default();
            for tree in trees {
                if !list.contains(tree) {
                    list.push(tree.clone());
                }
            }
        }
        for (key, value) in &self.disk.imported_meta {
            self.meta
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        false
    }

    pub(super) fn enqueue_disk_write(
        &mut self,
        operation: impl FnOnce() -> io::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.enqueue_disk_write_with_completion(
            operation,
            |s, result, _| {
                if let Err(error) = result {
                    s.error = Some(error.to_string());
                }
            },
            cx,
        );
    }

    pub(super) fn enqueue_disk_write_with_completion(
        &mut self,
        operation: impl FnOnce() -> io::Result<()> + Send + 'static,
        completion: impl FnOnce(&mut Self, io::Result<()>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        ensure_write_queue(cx);
        self.disk.generation = self.disk.generation.wrapping_add(1);
        let previous = cx.global_mut::<PendingWrites>().0.take();
        let (finish, tail) = tokio::sync::oneshot::channel();
        cx.global_mut::<PendingWrites>().0 = Some(tail);
        self.disk.writes_pending += 1;
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        // Detached writes survive dropping Sidebar/its UI completion task. Each operation
        // awaits its predecessor on a worker, never a UI lock or a thread join.
        cx.background_executor()
            .spawn(async move {
                if let Some(previous) = previous {
                    previous.await.ok();
                }
                let result = operation();
                finish.send(()).ok();
                result_tx.send(result).ok();
            })
            .detach();
        cx.spawn(async move |this, cx| {
            if let Ok(result) = result_rx.await {
                this.update(cx, |s, cx| {
                    s.disk.writes_pending = s.disk.writes_pending.saturating_sub(1);
                    completion(s, result, cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    #[cfg(all(test, unix))]
    pub(super) fn new_loaded(path: Option<PathBuf>) -> Self {
        let mut sidebar = Self::new(path);
        Startup::load(sidebar.path.as_deref()).apply(&mut sidebar);
        let data = DiskSnapshot::load(
            sidebar.path.as_deref(),
            sidebar.snapshot.clone(),
            sidebar.meta.clone(),
        );
        sidebar.apply_disk_snapshot(data);
        sidebar.rows = sidebar.local_rows();
        sidebar.row_hosts = vec![HostId::Local; sidebar.rows.len()];
        sidebar
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[gpui::test]
    fn blocked_write_queue_keeps_ui_running_and_saves_in_order(cx: &mut gpui::TestAppContext) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        cx.update(gpui_component::init);
        let sidebar = cx.new(|_| Sidebar::new(None));
        let (release, blocked) = tokio::sync::oneshot::channel();
        cx.update(|cx| {
            ensure_write_queue(cx);
            cx.global_mut::<PendingWrites>().0 = Some(blocked);
        });
        let step = Arc::new(AtomicUsize::new(0));
        for index in 0..3 {
            let step = step.clone();
            sidebar.update(cx, |s, cx| {
                s.enqueue_disk_write(
                    move || {
                        assert_eq!(step.fetch_add(1, Ordering::SeqCst), index);
                        Ok(())
                    },
                    cx,
                );
            });
        }
        cx.run_until_parked();
        assert_eq!(step.load(Ordering::SeqCst), 0);
        // An ordinary foreground update still completes while the writer awaits its lock/backend.
        sidebar.update(cx, |s, cx| {
            s.notice = Some("UI tick".into());
            cx.notify();
        });
        assert_eq!(
            cx.read(|cx| sidebar.read(cx).notice.clone()).as_deref(),
            Some("UI tick")
        );
        drop(sidebar); // closing a window must not discard already queued writes
        release.send(()).unwrap();
        cx.run_until_parked();
        assert_eq!(step.load(Ordering::SeqCst), 3);
    }

    #[gpui::test]
    fn initial_load_preserves_edits_and_stale_file_scan_cannot_restore_parent(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let sidebar = cx.new(|_| Sidebar::new(None));
        let mut old = DiskSnapshot::load(None, Snapshot::default(), Meta::new());
        old.store
            .parents
            .insert("/repo/child".into(), "/repo/old".into());
        sidebar.update(cx, |s, cx| {
            s.prefs.entry("/repo".into()).or_default().name = "Edited during load".into();
            s.disk.dirty = true;
            s.ensure_external_polling(cx);
            s.disk.generation = 1;
            s.parents.insert("/repo/child".into(), "/repo/new".into());
            s.accept_disk_snapshot(0, old, cx);
            assert_eq!(
                s.worktree_parent(Path::new("/repo/child")),
                Some("/repo/new".into())
            );
        });
        cx.run_until_parked();
        assert!(cx.read(|cx| sidebar.read(cx).disk.initialized));
        assert_eq!(
            cx.read(|cx| sidebar.read(cx).prefs[Path::new("/repo")].name.clone()),
            "Edited during load"
        );
    }

    #[gpui::test]
    fn delayed_startup_keeps_default_round_trips_and_explicit_clears(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(format!(
            "krill-sidebar-startup-intent-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sidebar.bin");
        let repo = PathBuf::from("/repo");
        let card = PathBuf::from("/repo/card");
        let child = PathBuf::from("/repo/child");
        let prefs = BTreeMap::from([(
            repo.clone(),
            Preference {
                pinned: true,
                name: "saved".into(),
            },
        )]);
        let mut options = WorkspaceOptions {
            group_by: options::GroupBy::None,
            sort_by: options::SortBy::Name,
            ..Default::default()
        };
        options.card_properties.issue = false;
        options.filters.repos.insert(repo.clone());
        let lin = lineage::LineageState {
            parents: BTreeMap::from([(child.clone(), repo.clone())]),
            sleeping_worktrees: BTreeSet::from([repo.clone()]),
            ..Default::default()
        };
        let ui = UiExpansionState {
            collapsed: BTreeSet::from([repo.clone()]),
            collapsed_hosts: BTreeSet::from([HostId::Local]),
            expanded_cards: BTreeSet::from([card.clone()]),
            selected_worktree: Some(repo.clone()),
            ..Default::default()
        };
        save_state(
            &path,
            &prefs,
            &options,
            &lin,
            &ui,
            &[repo.clone(), card.clone()],
        )
        .unwrap();
        let sidebar = cx.new(|_| Sidebar::new(Some(path.clone())));
        let (release, held) = tokio::sync::oneshot::channel();
        sidebar.update(cx, |s, cx| {
            s.disk.next_initial_io = Some(held);
            s.project_order = vec![card.clone(), repo.clone()];
            s.set_group_by(options::GroupBy::WorkspaceStatus, cx);
            s.set_group_by(options::GroupBy::Repo, cx);
            s.set_card_property(options::CardProperty::Issue, true, cx);
            s.clear_repo_filters(cx);
            s.fold_host(&HostId::Local, Some(true), cx);
            s.fold(&repo, Some(true), cx);
            s.toggle_card_sessions(&card, cx);
            s.toggle_card_sessions(&card, cx);
            s.select_worktree(None, cx);
            s.wake_worktree(&repo, cx);
            s.clear_worktree_parent(&child, cx);
            // A rename to the initially visible empty value must retain the saved pin.
            s.disk.startup.names.insert(repo.clone());
            s.prefs.entry(repo.clone()).or_default().name.clear();
            s.write_state(cx);
        });
        cx.run_until_parked();
        sidebar.update(cx, |s, cx| {
            assert!(!s.disk.initialized);
            s.notice = Some("UI tick".into());
            cx.notify();
        });
        // An unrelated external preference edit during the blocked load still applies.
        options.sort_by = options::SortBy::Recent;
        save_state(
            &path,
            &prefs,
            &options,
            &lin,
            &ui,
            &[repo.clone(), card.clone()],
        )
        .unwrap();
        release.send(()).unwrap();
        cx.run_until_parked();
        let (saved_prefs, saved_options, saved_lin, saved_ui, saved_order) =
            load_state(&path).unwrap();
        assert_eq!(saved_order, [card.clone(), repo.clone()]);
        assert_eq!(saved_options.group_by, options::GroupBy::Repo);
        assert_eq!(saved_options.sort_by, options::SortBy::Recent);
        assert!(saved_options.card_properties.issue);
        assert!(saved_options.filters.repos.is_empty());
        assert!(saved_prefs[&repo].pinned);
        assert!(saved_prefs[&repo].name.is_empty());
        assert!(!saved_ui.collapsed.contains(&repo));
        assert!(!saved_ui.collapsed_hosts.contains(&HostId::Local));
        assert!(!saved_ui.expanded_cards.contains(&card));
        assert!(saved_ui.selected_worktree.is_none());
        assert!(!saved_lin.sleeping_worktrees.contains(&repo));
        assert!(
            !WorktreeStore::load(&dir)
                .unwrap()
                .parents
                .contains_key(child.to_str().unwrap())
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[gpui::test]
    fn remote_refresh_coalesces_while_backend_is_held(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let sidebar = cx.new(|_| Sidebar::new(None));
        let manager = HostManager::new();
        let (release, held) = tokio::sync::oneshot::channel();
        sidebar.update(cx, |s, cx| {
            s.disk.next_remote_io = Some(held);
            for _ in 0..25 {
                s.sync_remote_hosts(&manager, cx);
            }
        });
        cx.run_until_parked();
        sidebar.update(cx, |s, cx| {
            assert_eq!(s.disk.remote_starts, 1);
            assert!(s.disk.remote_refresh.pending);
            s.notice = Some("UI tick".into());
            cx.notify();
        });
        release.send(()).unwrap();
        cx.run_until_parked();
        sidebar.update(cx, |s, _| {
            assert_eq!(s.disk.remote_starts, 2);
            assert!(!s.disk.remote_refresh.pending);
            assert!(!s.disk.remote_refresh.again);
        });
    }

    #[gpui::test]
    fn explicit_flush_includes_edits_waiting_for_initial_load(cx: &mut gpui::TestAppContext) {
        use std::cell::Cell;
        use std::rc::Rc;
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(format!(
            "krill-sidebar-initial-flush-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sidebar.bin");
        let sidebar = cx.new(|_| Sidebar::new(Some(path.clone())));
        sidebar.update(cx, |s, cx| s.toggle_pin("/repo".into(), cx));
        let barrier = cx.update(flush_file_io);
        let finished = Rc::new(Cell::new(false));
        let done = finished.clone();
        cx.spawn(async move |_| {
            barrier.await;
            done.set(true);
        })
        .detach();
        cx.run_until_parked();
        assert!(finished.get());
        assert!(load(&path).unwrap()[Path::new("/repo")].pinned);
        fs::remove_dir_all(dir).unwrap();
    }

    #[gpui::test]
    fn closing_sidebar_before_initial_load_still_persists_edits(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let dir = std::env::temp_dir().join(format!(
            "krill-sidebar-close-startup-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sidebar.bin");
        let sidebar = cx.new(|_| Sidebar::new(Some(path.clone())));
        sidebar.update(cx, |s, cx| s.toggle_pin("/repo".into(), cx));
        drop(sidebar);
        let barrier = cx.update(flush_file_io);
        let saved = path.clone();
        cx.spawn(async move |_| {
            barrier.await;
            assert!(load(&saved).unwrap()[Path::new("/repo")].pinned);
        })
        .detach();
        cx.run_until_parked();
        fs::remove_dir_all(dir).unwrap();
    }

    #[gpui::test]
    fn new_does_not_open_files_and_render_uses_captured_badges(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let path =
            std::env::temp_dir().join(format!("krill-sidebar-construction-{}", std::process::id()));
        // An unreadable state path is not touched by construction.
        fs::create_dir_all(&path).unwrap();
        let sidebar = cx.new(|_| Sidebar::new(Some(path.clone())));
        assert!(cx.read(|cx| sidebar.read(cx).error.is_none()));
        sidebar.update(cx, |s, _| {
            s.disk
                .store
                .issues
                .insert("/repo/tree".into(), "TEST-1".into());
            assert_eq!(
                s.issue_for(Path::new("/repo/tree")).as_deref(),
                Some("TEST-1")
            );
            assert_eq!(
                external::checkpoint_parts_from_store(&s.disk.store, Path::new("/repo/tree"))
                    .unwrap()
                    .issue
                    .as_deref(),
                Some("TEST-1")
            );
            assert_eq!(s.worktree_parent(Path::new("/repo/tree")), None);
        });
        fs::remove_dir_all(path).unwrap();
    }
}
