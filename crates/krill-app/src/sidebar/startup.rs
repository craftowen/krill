//! Merge delayed startup data by edited field/key, including explicit clears.

use super::*;

#[derive(Default)]
pub(super) struct Edits {
    last: PersistedSidebarState,
    fields: BTreeSet<&'static str>,
    pub names: BTreeSet<PathBuf>,
    pins: BTreeSet<PathBuf>,
    pub ui: UiExpansionState,
    pub parents: BTreeSet<PathBuf>,
    pub collapsed_lineage: BTreeSet<PathBuf>,
    pub sleeping: BTreeSet<PathBuf>,
}

pub(super) fn capture(s: &Sidebar) -> PersistedSidebarState {
    (
        s.prefs.clone(),
        s.options.clone(),
        lineage::LineageState {
            parents: s.parents.clone(),
            collapsed_lineage: s.collapsed_lineage.clone(),
            sleeping_worktrees: s.sleeping_worktrees.clone(),
        },
        UiExpansionState {
            collapsed: s.collapsed.clone(),
            collapsed_groups: s.collapsed_groups.clone(),
            collapsed_hosts: s.collapsed_hosts.clone(),
            expanded_cards: s.expanded_cards.clone(),
            selected_worktree: s.selected_worktree.clone(),
            shown_external: s.shown_external.clone(),
            removed_projects: s.removed_projects.clone(),
        },
        s.project_order.clone(),
        s.worktree_order.clone(),
    )
}

fn record_set<K: Ord + Clone>(before: &BTreeSet<K>, now: &BTreeSet<K>, touched: &mut BTreeSet<K>) {
    touched.extend(before.symmetric_difference(now).cloned());
}

fn merge_set<K: Ord + Clone>(saved: &mut BTreeSet<K>, now: &BTreeSet<K>, touched: &BTreeSet<K>) {
    for key in touched {
        if now.contains(key) {
            saved.insert(key.clone());
        } else {
            saved.remove(key);
        }
    }
}

impl Edits {
    pub fn touch(&mut self, field: &'static str) {
        self.fields.insert(field);
    }

    pub fn reset_filters(&mut self) {
        self.fields.extend([
            "filters.hide_sleeping",
            "filters.always_show_default_branch",
            "filters.hide_automation",
            "filters.hide_cli",
            "repos.clear",
            "hosts.clear",
        ]);
    }

    pub fn record(&mut self, now: PersistedSidebarState) {
        macro_rules! scalars {
            ($($($field:ident).+),+ $(,)?) => {$(
                if self.last.1.$($field).+ != now.1.$($field).+ {
                    self.fields.insert(stringify!($($field).+));
                }
            )+};
        }
        scalars!(
            group_by,
            sort_by,
            card_layout,
            card_properties.issue,
            card_properties.pr,
            card_properties.comment,
            card_properties.ports,
            card_properties.branch,
            card_properties.host,
            filters.hide_sleeping,
            filters.always_show_default_branch,
            filters.hide_automation,
            filters.hide_cli
        );
        let default = Preference::default();
        for key in self.last.0.keys().chain(now.0.keys()) {
            let before = self.last.0.get(key).unwrap_or(&default);
            let after = now.0.get(key).unwrap_or(&default);
            if before.name != after.name {
                self.names.insert(key.clone());
            }
            if before.pinned != after.pinned {
                self.pins.insert(key.clone());
            }
        }
        for key in self.last.2.parents.keys().chain(now.2.parents.keys()) {
            if self.last.2.parents.get(key) != now.2.parents.get(key) {
                self.parents.insert(key.clone());
            }
        }
        record_set(
            &self.last.2.collapsed_lineage,
            &now.2.collapsed_lineage,
            &mut self.collapsed_lineage,
        );
        record_set(
            &self.last.2.sleeping_worktrees,
            &now.2.sleeping_worktrees,
            &mut self.sleeping,
        );
        if self.last.1.filters.repos != now.1.filters.repos {
            self.fields.insert("repos.clear");
        }
        if self.last.1.filters.hosts != now.1.filters.hosts {
            self.fields.insert("hosts.clear");
        }
        macro_rules! sets {
            ($($field:ident),+ $(,)?) => {$(
                record_set(&self.last.3.$field, &now.3.$field, &mut self.ui.$field);
            )+};
        }
        sets!(
            collapsed,
            collapsed_groups,
            collapsed_hosts,
            expanded_cards,
            shown_external,
            removed_projects
        );
        if self.last.3.selected_worktree != now.3.selected_worktree {
            self.fields.insert("selected");
        }
        if self.last.4 != now.4 {
            self.fields.insert("project_order");
        }
        if self.last.5 != now.5 {
            self.fields.insert("worktree_order");
        }
        self.last = now;
    }

    pub fn merge(
        mut self,
        mut saved: PersistedSidebarState,
        now: PersistedSidebarState,
    ) -> PersistedSidebarState {
        self.record(now);
        let now = self.last;
        macro_rules! scalars {
            ($($($field:ident).+),+ $(,)?) => {$(
                if self.fields.contains(stringify!($($field).+)) {
                    saved.1.$($field).+ = now.1.$($field).+;
                }
            )+};
        }
        scalars!(
            group_by,
            sort_by,
            card_layout,
            card_properties.issue,
            card_properties.pr,
            card_properties.comment,
            card_properties.ports,
            card_properties.branch,
            card_properties.host,
            filters.hide_sleeping,
            filters.always_show_default_branch,
            filters.hide_automation,
            filters.hide_cli
        );
        for path in self.names {
            saved.0.entry(path.clone()).or_default().name =
                now.0.get(&path).map(|p| p.name.clone()).unwrap_or_default();
        }
        for path in self.pins {
            saved.0.entry(path.clone()).or_default().pinned =
                now.0.get(&path).is_some_and(|p| p.pinned);
        }
        for path in self.parents {
            if let Some(parent) = now.2.parents.get(&path) {
                saved.2.parents.insert(path, parent.clone());
            } else {
                saved.2.parents.remove(&path);
            }
        }
        merge_set(
            &mut saved.2.collapsed_lineage,
            &now.2.collapsed_lineage,
            &self.collapsed_lineage,
        );
        merge_set(
            &mut saved.2.sleeping_worktrees,
            &now.2.sleeping_worktrees,
            &self.sleeping,
        );
        if self.fields.contains("repos.clear") {
            saved.1.filters.repos.clone_from(&now.1.filters.repos);
        }

        if self.fields.contains("hosts.clear") {
            saved.1.filters.hosts.clone_from(&now.1.filters.hosts);
        }

        macro_rules! sets {
            ($($field:ident),+ $(,)?) => {$(
                merge_set(&mut saved.3.$field, &now.3.$field, &self.ui.$field);
            )+};
        }
        sets!(
            collapsed,
            collapsed_groups,
            collapsed_hosts,
            expanded_cards,
            shown_external,
            removed_projects
        );
        if self.fields.contains("selected") {
            saved.3.selected_worktree = now.3.selected_worktree;
        }
        if self.fields.contains("project_order") {
            saved.4 = now.4;
        }
        if self.fields.contains("worktree_order") {
            saved.5 = now.5;
        }
        saved
    }
}
