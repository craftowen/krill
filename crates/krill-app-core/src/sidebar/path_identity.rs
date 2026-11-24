//! Reconcile equivalent Windows paths at the snapshot boundary. Keys retain the
//! first readable spelling; identity keys never become launch directories.

use super::*;
use krill_proto::path::{equivalent, identity, normalize};

type Aliases = BTreeMap<PathBuf, PathBuf>;

fn preferred(path: &Path, aliases: &Aliases) -> PathBuf {
    aliases
        .get(&identity(path))
        .cloned()
        .unwrap_or_else(|| normalize(path))
}

fn rekey<T>(map: &mut BTreeMap<PathBuf, T>, aliases: &Aliases, merge: impl Fn(&mut T, T)) {
    *map = std::mem::take(map)
        .into_iter()
        .fold(BTreeMap::new(), |mut out, (path, value)| {
            match out.entry(preferred(&path, aliases)) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(value);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    merge(entry.get_mut(), value);
                }
            }
            out
        });
}

// An empty default must not replace user metadata from another spelling. When
// both aliases have conflicting nonempty text, keep the first in stable path order.
fn merge_text(current: &mut String, other: String) {
    if current.is_empty() {
        *current = other;
    }
}

fn merge_optional_text(current: &mut Option<String>, other: Option<String>) {
    if current.as_ref().is_none_or(String::is_empty) {
        *current = other;
    }
}

fn merge_meta(current: &mut Tree, other: Tree) {
    merge_optional_text(&mut current.branch, other.branch);
    current.dirty = current.dirty.max(other.dirty);
}

fn reset_set(set: &mut BTreeSet<PathBuf>, aliases: &Aliases) {
    *set = std::mem::take(set)
        .into_iter()
        .map(|p| preferred(&p, aliases))
        .collect();
}

pub(super) fn reconcile(snapshot: &mut Snapshot) -> Aliases {
    let mut aliases = Aliases::new();
    let mut trees: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for (repo, worktrees) in std::mem::take(&mut snapshot.trees) {
        let repo = aliases
            .entry(identity(&repo))
            .or_insert_with(|| normalize(&repo))
            .clone();
        let list = trees.entry(repo).or_default();
        for path in worktrees {
            let path = aliases
                .entry(identity(&path))
                .or_insert_with(|| normalize(&path))
                .clone();
            if !list.contains(&path) {
                list.push(path);
            }
        }
    }
    snapshot.trees = trees;
    for path in snapshot
        .sessions
        .iter()
        .flat_map(|s| s.worktree.iter().chain(s.cwd.iter()))
    {
        aliases
            .entry(identity(path))
            .or_insert_with(|| normalize(path));
    }
    for session in &mut snapshot.sessions {
        session.cwd = session.cwd.as_deref().map(|p| preferred(p, &aliases));
        session.worktree = session.worktree.as_deref().map(|p| preferred(p, &aliases));
        if let Some(cwd) = session.cwd.as_deref().filter(|p| windows_style(p)) {
            session.worktree = snapshot
                .trees
                .keys()
                .chain(snapshot.trees.values().flatten())
                .filter(|root| path_within(cwd, root))
                .max_by_key(|root| identity(root).as_os_str().len())
                .cloned()
                .or_else(|| session.worktree.clone());
        }
    }
    aliases
}

impl Sidebar {
    pub(super) fn reconcile_path_identity(&mut self) {
        let windows_local = self
            .snapshot
            .trees
            .keys()
            .chain(self.snapshot.trees.values().flatten())
            .chain(
                self.snapshot
                    .sessions
                    .iter()
                    .flat_map(|s| s.worktree.iter().chain(s.cwd.iter())),
            )
            .chain(self.prefs.keys())
            .any(|p| windows_style(p));
        if windows_local {
            let aliases = reconcile(&mut self.snapshot);
            rekey(&mut self.meta, &aliases, merge_meta);
            rekey(&mut self.prefs, &aliases, |current, other| {
                current.pinned |= other.pinned;
                merge_text(&mut current.name, other.name);
            });
            rekey(&mut self.comments, &aliases, merge_text);
            rekey(&mut self.issues, &aliases, merge_text);
            rekey(&mut self.worktree_hosts, &aliases, merge_text);
            rekey(&mut self.ports, &aliases, |current, other| {
                current.extend(other);
                current.sort_unstable();
                current.dedup();
            });
            // Status has no empty/default marker or revision: retain the first
            // explicit value in stable path order when aliases conflict.
            rekey(&mut self.worktree_statuses, &aliases, |_, _| {});
            rekey(&mut self.worktree_updated_at, &aliases, |current, other| {
                *current = (*current).max(other);
            });
            rekey(
                &mut self.disk.worktree_created_at,
                &aliases,
                |current, other| {
                    if *current == 0 || other != 0 && other < *current {
                        *current = other;
                    }
                },
            );
            rekey(
                &mut self.disk.folder_updated_at,
                &aliases,
                |current, other| {
                    *current = (*current).max(other);
                },
            );
            rekey(&mut self.project_visuals, &aliases, |current, other| {
                merge_optional_text(&mut current.display_name, other.display_name);
                merge_optional_text(&mut current.badge_color, other.badge_color);
                if current.icon.is_none() {
                    current.icon = other.icon;
                }
            });
            self.parents = std::mem::take(&mut self.parents)
                .into_iter()
                .map(|(child, parent)| (preferred(&child, &aliases), preferred(&parent, &aliases)))
                .collect();
            for set in [
                &mut self.collapsed,
                &mut self.expanded_cards,
                &mut self.shown_external,
                &mut self.removed_projects,
                &mut self.collapsed_lineage,
                &mut self.sleeping_worktrees,
                &mut self.automation_worktrees,
                &mut self.cli_worktrees,
                &mut self.recently_created_worktrees,
                &mut self.manually_folded_cards,
                &mut self.options.filters.repos,
            ] {
                reset_set(set, &aliases);
            }
            self.selected_worktree = self
                .selected_worktree
                .as_deref()
                .map(|p| preferred(p, &aliases));
            for order in [&mut self.project_order, &mut self.worktree_order] {
                for path in order.iter_mut() {
                    *path = preferred(path, &aliases);
                }
                let mut seen = BTreeSet::new();
                order.retain(|p| seen.insert(p.clone()));
            }
        }
        for remote in &mut self.remote_hosts {
            if !remote
                .snapshot
                .trees
                .keys()
                .chain(remote.snapshot.trees.values().flatten())
                .chain(
                    remote
                        .snapshot
                        .sessions
                        .iter()
                        .filter_map(|s| s.cwd.as_ref()),
                )
                .any(|p| windows_style(p))
            {
                continue;
            }
            let aliases = reconcile(&mut remote.snapshot);
            rekey(&mut remote.meta, &aliases, merge_meta);
        }
    }
}

pub(super) fn same(path: &Path, other: &Path) -> bool {
    equivalent(path, other)
}
