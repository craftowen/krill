// Adapted from Orca SplitPaneLanding (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Empty split/new-tab pane launcher matching Orca's `SplitPaneLanding`:
//! centered card with search input ("Search agents and files"), default agent row with
//! agent-picker dropdown (`⌘⌥T`), Terminal (`⌘T`), filtered Agents/New/Files search groups,
//! keyboard navigation (`↑`/`↓`/`Home`/`End`/`Ctrl+N/P/J/K`/`Enter`/`Esc`), and `Close pane`.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, App, Context, ElementId, Entity, Focusable as _, FontWeight, KeyDownEvent,
    MouseButton, SharedString, Subscription, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Enter, Escape, Input, InputEvent, InputState, MoveDown, MoveUp};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Icon, IconName, Sizable as _};
use krill_app::i18n::{Language, Message, catalog::shell as text};
use krill_app::layout::{Axis, PaneId};
use krill_app::pane;
use krill_app::ui;
use krill_proto::bots::BotId;
use krill_proto::control::{Create, Program};
use krill_proto::os::ffi::OsStrExt as _;

use super::Shell;

/// Maximum file rows shown in the launcher search results (Orca `MAX_FILE_RESULTS`).
const MAX_FILE_RESULTS: usize = 8;
/// Card maximum width (Orca `max-w-[380px]`).
const LAUNCHER_CARD_MAX_WIDTH: f32 = 380.;
/// Search row height (Orca `h-10` = 40px).
const LAUNCHER_INPUT_HEIGHT: f32 = 40.;
/// List row height (Orca `h-9` = 36px).
const LAUNCHER_ROW_HEIGHT: f32 = 36.;
/// Max scrollable list height when search returns many results.
const LAUNCHER_LIST_MAX_HEIGHT: f32 = 320.;

/// Runtime state for an empty split or new-tab launcher pane.
#[derive(Default)]
pub(super) struct SplitLauncher {
    pub(super) bot_id: Option<BotId>,
    pub(super) root_folder: Option<PathBuf>,
    pub(super) input: Option<(Entity<InputState>, Subscription)>,
    pub(super) query: String,
    pub(super) selected: usize,
    pub(super) files: Vec<PathBuf>,
    pub(super) files_loaded: bool,
    pub(super) files_loading: bool,
    pub(super) launching: bool,
}

impl SplitLauncher {
    pub(super) fn new(bot_id: Option<BotId>, root_folder: Option<PathBuf>) -> Self {
        Self {
            bot_id,
            root_folder,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LauncherRow {
    DefaultAgent {
        agent_id: String,
        label: String,
    },
    Agent {
        agent_id: String,
        label: String,
    },
    Terminal,
    File {
        rel_path: PathBuf,
        filename: String,
        directory: String,
    },
}

pub(super) fn landing_query_matches(candidate: &str, query: &str) -> bool {
    let needle = query.trim().to_lowercase();
    needle.is_empty() || candidate.to_lowercase().contains(&needle)
}

fn agent_search_alias_message(agent_id: &str) -> Message {
    match agent_id.to_lowercase().as_str() {
        "claude" | "claude-code" => text::LAUNCHER_ALIAS_CLAUDE,
        "codex" => text::LAUNCHER_ALIAS_CODEX,
        "agy" | "antigravity" => text::LAUNCHER_ALIAS_AGY,
        "opencode" => text::LAUNCHER_ALIAS_OPENCODE,
        "cursor" | "cursor-agent" => text::LAUNCHER_ALIAS_CURSOR,
        "pi" => text::LAUNCHER_ALIAS_PI,
        _ => text::LAUNCHER_ALIAS_AGENT,
    }
}

fn alias_message_matches(message: Message, query: &str) -> bool {
    [Language::Ko, Language::En].into_iter().any(|lang| {
        message
            .in_language(lang)
            .split_whitespace()
            .any(|alias| landing_query_matches(alias, query))
    })
}

fn split_trailing_segment(path: &Path) -> (String, String) {
    let filename = path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    (directory, filename)
}

fn rank_launcher_files<'a>(files: &'a [PathBuf], query: &str) -> Vec<&'a PathBuf> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut filename_hits = Vec::new();
    let mut path_hits = Vec::new();
    for path in files {
        let rel = path.to_string_lossy();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        if name.to_lowercase().contains(&needle) {
            filename_hits.push(path);
        } else if rel.to_lowercase().contains(&needle) {
            path_hits.push(path);
        }
    }
    filename_hits
        .into_iter()
        .chain(path_hits)
        .take(MAX_FILE_RESULTS)
        .collect()
}

fn fallback_agent_display_name(agent_id: &str) -> String {
    match agent_id.to_lowercase().as_str() {
        "claude" | "claude-code" => "Claude".to_owned(),
        "codex" => "Codex".to_owned(),
        "agy" | "antigravity" => "Antigravity".to_owned(),
        "opencode" => "OpenCode".to_owned(),
        "cursor" | "cursor-agent" => "Cursor".to_owned(),
        "pi" => "Pi".to_owned(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

impl Shell {
    pub(super) fn is_split_launcher(&self, pane: PaneId) -> bool {
        self.split_launchers.contains_key(&pane)
    }

    pub(super) fn focused_split_launcher_pane(&self) -> Option<PaneId> {
        let pane = self.layout.focused()?;
        self.is_split_launcher(pane).then_some(pane)
    }

    pub(super) fn split_launcher_has_focus(&self, pane: PaneId, window: &Window, cx: &App) -> bool {
        self.split_launchers
            .get(&pane)
            .and_then(|l| l.input.as_ref())
            .is_some_and(|(input, _)| input.read(cx).focus_handle(cx).is_focused(window))
    }

    pub(super) fn split_launcher_awaiting_launch(
        &self,
        workspace: &super::workspaces::Workspace,
        cx: &App,
    ) -> bool {
        self.split_launchers.iter().any(|(&pane, launcher)| {
            launcher.launching && self.pane_workspace(pane, cx).as_ref() == Some(workspace)
        })
    }

    pub(super) fn register_split_launcher(
        &mut self,
        pane: PaneId,
        bot_id: Option<BotId>,
        root_folder: Option<PathBuf>,
    ) {
        self.split_launchers
            .insert(pane, SplitLauncher::new(bot_id, root_folder));
    }

    pub(super) fn clear_split_launcher_launching(&mut self, pane: PaneId) {
        if let Some(launcher) = self.split_launchers.get_mut(&pane) {
            launcher.launching = false;
        }
    }

    /// Splits `pane` along `axis`. In a bot workspace, opens Orca's empty split launcher in the
    /// new pane so the user can pick a new bot chat, terminal, agent, or file.
    pub(super) fn split_pane(&mut self, pane: PaneId, axis: Axis, cx: &mut Context<Self>) {
        if !self.layout.pane_exists(pane) {
            return;
        }
        self.layout.set_zoomed_pane(None);
        if let Some(bot) = self.bot_for_pane(pane).cloned() {
            let host = self.layout.host(pane).clone();
            let bot_folder = self.bot_folder(&bot);
            let cwd_bytes = bot_folder
                .as_ref()
                .map(|p| p.as_os_str().as_bytes().to_vec())
                .unwrap_or_default();
            self.change_with_launcher(Some(bot.id), bot_folder, cx, |l| {
                l.focus(pane);
                l.split(axis);
                let new_pane = l.focused()?;
                if host.is_remote() {
                    l.set_host(new_pane, host);
                }
                l.set_launch(
                    new_pane,
                    Create {
                        account: None,
                        program: Program::Login,
                        cwd: cwd_bytes,
                        cols: pane::SIZE.0,
                        rows: pane::SIZE.1,
                    },
                );
                Some(new_pane)
            });
            return;
        }
        self.change(true, cx, |l| {
            l.focus(pane);
            l.split(axis);
        });
    }

    /// Opens a new tab in the active bot workspace showing the empty launcher card.
    pub(super) fn new_bot_launcher_tab(&mut self, cx: &mut Context<Self>) {
        let Some(bot) = self.focused_bot().cloned() else {
            self.new_tab_on_launch_host(cx);
            return;
        };
        let host = bot.host.clone();
        let bot_folder = self.bot_folder(&bot);
        let cwd_bytes = bot_folder
            .as_ref()
            .map(|p| p.as_os_str().as_bytes().to_vec())
            .unwrap_or_default();
        self.change_with_launcher(Some(bot.id), bot_folder, cx, |l| {
            l.new_tab();
            let new_pane = l.focused()?;
            if host.is_remote() {
                l.set_host(new_pane, host);
            }
            l.set_launch(
                new_pane,
                Create {
                    account: None,
                    program: Program::Login,
                    cwd: cwd_bytes,
                    cols: pane::SIZE.0,
                    rows: pane::SIZE.1,
                },
            );
            Some(new_pane)
        });
    }

    /// Tab bar `+` button handler: opens a new launcher tab in bot workspaces, or a terminal tab elsewhere.
    pub(super) fn on_new_tab_button(&mut self, cx: &mut Context<Self>) {
        if self.focused_bot().is_some() {
            self.new_bot_launcher_tab(cx);
        } else {
            self.new_tab_on_launch_host(cx);
        }
    }

    /// `Cmd+T` (`NewTab`) handler:
    /// - If already inside a launcher pane, selects `Terminal` (`⌘T`) right in that pane.
    /// - If in a bot workspace, opens a new tab with the launcher card.
    /// - Otherwise, opens a regular terminal tab.
    pub(super) fn on_new_tab_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_split_launcher_pane() {
            self.open_terminal_in_launcher_pane(pane, Some(window), cx);
        } else if self.focused_bot().is_some() {
            self.new_bot_launcher_tab(cx);
        } else {
            self.new_tab_on_launch_host(cx);
        }
    }

    /// `Cmd+Alt+T` (`NewAgentTab`) handler:
    /// - If inside a launcher pane, selects the default agent (new bot chat) right in that pane.
    /// - If in a bot workspace, starts a new bot chat directly.
    /// - Otherwise, launches the default agent in a new tab.
    pub(super) fn on_new_agent_tab_shortcut(&mut self, cx: &mut Context<Self>) {
        if let Some(pane) = self.focused_split_launcher_pane() {
            self.launch_default_agent_in_launcher_pane(pane, cx);
            return;
        }
        if let Some(bot) = self.focused_bot().cloned() {
            self.start_bot_session_on(&bot.host, bot.id, None, cx);
            return;
        }
        if self.link == krill_app::link::Link::Up && self.agents.is_some() {
            self.new_default_agent(cx);
        } else {
            self.notice = Some(
                krill_app::i18n::catalog::checks::D_AGENTS_NONE
                    .get()
                    .to_owned(),
            );
            cx.notify();
        }
    }

    pub(super) fn focus_split_launcher(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.ensure_split_launcher_input(pane, window, cx) {
            input.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    fn ensure_split_launcher_input(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<InputState>> {
        if let Some(existing) = self
            .split_launchers
            .get(&pane)
            .and_then(|l| l.input.as_ref())
            .map(|(input, _)| input.clone())
        {
            krill_app::i18n::sync_input_placeholder(
                &existing,
                text::LAUNCHER_SEARCH_PLACEHOLDER.get(),
                window,
                cx,
            );
            return Some(existing);
        }
        if !self.split_launchers.contains_key(&pane) {
            return None;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(text::LAUNCHER_SEARCH_PLACEHOLDER.get())
        });
        let sub = cx.subscribe(&input, move |this, input, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                let next = input.read(cx).value().to_string();
                let should_load_files = if let Some(launcher) = this.split_launchers.get_mut(&pane)
                {
                    if launcher.query != next {
                        launcher.query = next;
                        launcher.selected = 0;
                    }
                    !launcher.query.trim().is_empty()
                        && !launcher.files_loaded
                        && !launcher.files_loading
                } else {
                    false
                };
                if should_load_files {
                    this.load_split_launcher_files(pane, cx);
                }
                cx.notify();
            }
        });
        if let Some(launcher) = self.split_launchers.get_mut(&pane) {
            launcher.input = Some((input.clone(), sub));
        }
        Some(input)
    }

    fn load_split_launcher_files(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if self.layout.host(pane).is_remote() {
            if let Some(launcher) = self.split_launchers.get_mut(&pane) {
                launcher.files_loading = false;
                launcher.files_loaded = true;
            }
            return;
        }
        let root = self
            .split_launchers
            .get(&pane)
            .and_then(|l| l.root_folder.clone())
            .or_else(|| self.bot_for_pane(pane).and_then(|b| self.bot_folder(b)));
        let Some(root) = root.filter(|p| !p.as_os_str().is_empty()) else {
            return;
        };
        if let Some(launcher) = self.split_launchers.get_mut(&pane) {
            if launcher.files_loading || launcher.files_loaded {
                return;
            }
            launcher.files_loading = true;
            launcher.root_folder = Some(root.clone());
        }
        cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { krill_app::editor::quick_open::list(&root) })
                .await;
            this.update(cx, |this, cx| {
                if let Some(launcher) = this.split_launchers.get_mut(&pane) {
                    launcher.files = files;
                    launcher.files_loading = false;
                    launcher.files_loaded = true;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn launcher_default_agent_id(&self, pane: PaneId, cx: &App) -> Option<String> {
        if let Some(bot) = self.bot_for_pane(pane)
            && !bot.agent_id.trim().is_empty()
        {
            return Some(bot.agent_id.clone());
        }
        let configured = krill_app::settings::current(cx).default_agent;
        if !configured.trim().is_empty() {
            return Some(configured);
        }
        self.agents
            .as_ref()
            .and_then(|a| a.profiles.first())
            .map(|p| p.id.clone())
    }

    fn launcher_agent_display_name(&self, agent_id: &str) -> String {
        if matches!(agent_id.to_lowercase().as_str(), "claude" | "claude-code") {
            return "Claude".to_owned();
        }
        self.agent_profile_name(agent_id)
            .map_or_else(|| fallback_agent_display_name(agent_id), str::to_owned)
    }

    fn launcher_default_agent_label(&self, pane: PaneId, agent_id: Option<&str>) -> String {
        if let Some(bot) = self.bot_for_pane(pane) {
            return krill_app::i18n::catalog::bots::SV_NEW_CHAT_WITH_BOT
                .format(&[("bot", &bot.name)]);
        }
        match agent_id.filter(|id| !id.trim().is_empty()) {
            Some(id) => {
                let name = self.launcher_agent_display_name(id);
                text::LAUNCHER_NEW_NAMED_AGENT.format(&[("agent", &name)])
            }
            None => text::LAUNCHER_NEW_AGENT.get().to_owned(),
        }
    }

    fn launcher_agent_profiles(&self, pane: PaneId, cx: &App) -> Vec<(String, String)> {
        let local = !self.layout.host(pane).is_remote();
        let bot_agent = self
            .bot_for_pane(pane)
            .map(|b| krill_proto::agent::canonical_id(&b.agent_id).to_owned());
        if let Some(settings) = &self.agents
            && !settings.profiles.is_empty()
        {
            // Orca `#47382`: list only enabled/installed agents (always keep the bot's own agent).
            let filtered: Vec<(String, String)> = settings
                .profiles
                .iter()
                .filter(|p| {
                    !local
                        || bot_agent.as_deref().is_some_and(|ba| {
                            krill_proto::agent::canonical_id(&p.id).eq_ignore_ascii_case(ba)
                        })
                        || krill_app::accounts::installed::agent_maybe_installed(cx, &p.id)
                })
                .map(|p| (p.id.clone(), p.name.clone()))
                .collect();
            if !filtered.is_empty() {
                return filtered;
            }
            return settings
                .profiles
                .iter()
                .map(|p| (p.id.clone(), p.name.clone()))
                .collect();
        }
        if let Some(id) = self.launcher_default_agent_id(pane, cx) {
            let name = self.launcher_agent_display_name(&id);
            return vec![(id, name)];
        }
        Vec::new()
    }

    pub(super) fn launcher_rows(&self, pane: PaneId, cx: &App) -> Vec<LauncherRow> {
        let Some(launcher) = self.split_launchers.get(&pane) else {
            return Vec::new();
        };
        let query = launcher.query.trim();
        if query.is_empty() {
            let default_agent_id = self.launcher_default_agent_id(pane, cx);
            let label = self.launcher_default_agent_label(pane, default_agent_id.as_deref());
            return vec![
                LauncherRow::DefaultAgent {
                    agent_id: default_agent_id.unwrap_or_default(),
                    label,
                },
                LauncherRow::Terminal,
            ];
        }
        let mut rows = Vec::new();
        for (agent_id, name) in self.launcher_agent_profiles(pane, cx) {
            let label = text::LAUNCHER_NEW_NAMED_AGENT.format(&[("agent", &name)]);
            let matches = landing_query_matches(&name, query)
                || landing_query_matches(&agent_id, query)
                || landing_query_matches(&label, query)
                || alias_message_matches(agent_search_alias_message(&agent_id), query);
            if matches {
                rows.push(LauncherRow::Agent {
                    agent_id,
                    label: name,
                });
            }
        }
        if landing_query_matches(text::LAUNCHER_TERMINAL.get(), query)
            || alias_message_matches(text::LAUNCHER_ALIAS_TERMINAL, query)
        {
            rows.push(LauncherRow::Terminal);
        }
        for rel_path in rank_launcher_files(&launcher.files, query) {
            let (directory, filename) = split_trailing_segment(rel_path);
            rows.push(LauncherRow::File {
                rel_path: rel_path.clone(),
                filename,
                directory,
            });
        }
        rows
    }

    fn step_launcher_selection(&mut self, pane: PaneId, delta: isize, cx: &mut Context<Self>) {
        let count = self.launcher_rows(pane, cx).len();
        if count == 0 {
            return;
        }
        if let Some(launcher) = self.split_launchers.get_mut(&pane) {
            let current = launcher.selected.min(count.saturating_sub(1)).cast_signed();
            launcher.selected = (current + delta)
                .rem_euclid(count.cast_signed())
                .cast_unsigned();
            cx.notify();
        }
    }

    fn set_launcher_selection(&mut self, pane: PaneId, index: usize, cx: &mut Context<Self>) {
        let count = self.launcher_rows(pane, cx).len();
        if count == 0 {
            return;
        }
        if let Some(launcher) = self.split_launchers.get_mut(&pane) {
            launcher.selected = index.min(count.saturating_sub(1));
            cx.notify();
        }
    }

    fn clear_launcher_query(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let input = self
            .split_launchers
            .get(&pane)
            .filter(|l| !l.query.is_empty())
            .and_then(|l| l.input.as_ref())
            .map(|(input, _)| input.clone());
        let Some(input) = input else {
            return false;
        };
        if let Some(launcher) = self.split_launchers.get_mut(&pane) {
            launcher.query.clear();
            launcher.selected = 0;
        }
        input.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
        true
    }

    fn launcher_input_is_composing(&self, pane: PaneId, window: &mut Window, cx: &mut App) -> bool {
        let Some(input) = self
            .split_launchers
            .get(&pane)
            .and_then(|l| l.input.as_ref())
            .map(|(input, _)| input.clone())
        else {
            return false;
        };
        input.update(cx, |s, cx| {
            gpui::EntityInputHandler::marked_text_range(s, window, cx).is_some()
        })
    }

    fn activate_launcher_row(
        &mut self,
        pane: PaneId,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .split_launchers
            .get(&pane)
            .is_some_and(|launcher| launcher.launching)
        {
            return;
        }
        let rows = self.launcher_rows(pane, cx);
        let Some(row) = rows.get(index).cloned() else {
            return;
        };
        match row {
            LauncherRow::DefaultAgent { .. } => {
                self.launch_default_agent_in_launcher_pane(pane, cx);
            }
            LauncherRow::Agent { agent_id, .. } => {
                self.launch_agent_in_launcher_pane(pane, agent_id, cx);
            }
            LauncherRow::Terminal => {
                self.open_terminal_in_launcher_pane(pane, Some(window), cx);
            }
            LauncherRow::File { rel_path, .. } => {
                self.open_file_in_launcher_pane(pane, rel_path, cx);
            }
        }
    }

    pub(super) fn launch_default_agent_in_launcher_pane(
        &mut self,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) {
        if self
            .split_launchers
            .get(&pane)
            .is_some_and(|launcher| launcher.launching)
        {
            return;
        }
        if let Some(bot) = self.bot_for_pane(pane).cloned() {
            if self
                .bot_ui
                .opening
                .contains(&(bot.host.clone(), bot.id.clone()))
            {
                return;
            }
            if let Some(launcher) = self.split_launchers.get_mut(&pane) {
                launcher.launching = true;
            }
            self.start_bot_session_in_on(&bot.host, bot.id, None, Some(pane), cx);
            return;
        }
        if let Some(agent_id) = self.launcher_default_agent_id(pane, cx) {
            self.launch_agent_in_launcher_pane(pane, agent_id, cx);
        }
    }

    pub(super) fn launch_agent_in_launcher_pane(
        &mut self,
        pane: PaneId,
        agent_id: String,
        cx: &mut Context<Self>,
    ) {
        if self
            .split_launchers
            .get(&pane)
            .is_some_and(|launcher| launcher.launching)
        {
            return;
        }
        if let Some(bot) = self.bot_for_pane(pane).cloned()
            && (agent_id.is_empty()
                || krill_proto::agent::canonical_id(&bot.agent_id)
                    .eq_ignore_ascii_case(krill_proto::agent::canonical_id(&agent_id)))
        {
            if self
                .bot_ui
                .opening
                .contains(&(bot.host.clone(), bot.id.clone()))
            {
                return;
            }
            if let Some(launcher) = self.split_launchers.get_mut(&pane) {
                launcher.launching = true;
            }
            self.start_bot_session_in_on(&bot.host, bot.id, None, Some(pane), cx);
            return;
        }
        let cwd = self.launcher_cwd(pane);
        self.split_launchers.remove(&pane);
        krill_app::star_nag::record_agent_start(cx);
        self.change(true, cx, |l| {
            if let Some(tab) = l.tab_of_pane(pane) {
                l.activate(tab);
            }
            l.focus(pane);
            l.set_launch(
                pane,
                Create {
                    account: None,
                    program: Program::Agent { id: agent_id },
                    cwd: cwd.as_os_str().as_bytes().to_vec(),
                    cols: pane::SIZE.0,
                    rows: pane::SIZE.1,
                },
            );
        });
    }

    pub(super) fn open_terminal_in_launcher_pane(
        &mut self,
        pane: PaneId,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        if self
            .split_launchers
            .get(&pane)
            .is_some_and(|launcher| launcher.launching)
        {
            return;
        }
        let cwd = self.launcher_cwd(pane);
        self.split_launchers.remove(&pane);
        self.change(true, cx, |l| {
            if let Some(tab) = l.tab_of_pane(pane) {
                l.activate(tab);
            }
            l.focus(pane);
            l.set_chat_view(pane, false);
            l.set_launch(
                pane,
                Create {
                    account: None,
                    program: Program::Login,
                    cwd: cwd.as_os_str().as_bytes().to_vec(),
                    cols: pane::SIZE.0,
                    rows: pane::SIZE.1,
                },
            );
        });
        if let Some(window) = window {
            window.focus(&self.focus, cx);
        }
    }

    pub(super) fn open_file_in_launcher_pane(
        &mut self,
        pane: PaneId,
        rel_path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if self
            .split_launchers
            .get(&pane)
            .is_some_and(|launcher| launcher.launching)
        {
            return;
        }
        let root = self.launcher_cwd(pane);
        let full_path = if rel_path.is_absolute() {
            rel_path
        } else {
            root.join(rel_path)
        };
        let host = self.layout.host(pane).clone();
        self.split_launchers.remove(&pane);
        self.change(true, cx, |l| {
            l.open_file_in_pane(pane, host, full_path);
        });
        self.files.refocus = true;
    }

    fn launcher_cwd(&self, pane: PaneId) -> PathBuf {
        self.split_launchers
            .get(&pane)
            .and_then(|l| l.root_folder.clone())
            .or_else(|| self.bot_for_pane(pane).and_then(|b| self.bot_folder(b)))
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| {
                let cwd = self.launch_for(pane).cwd;
                if cwd.is_empty() {
                    super::folder::default_folder()
                } else {
                    use krill_proto::os::ffi::OsStringExt as _;
                    PathBuf::from(std::ffi::OsString::from_vec(cwd))
                }
            })
    }

    pub(super) fn render_split_launcher_pane(
        &mut self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(input) = self.ensure_split_launcher_input(id, window, cx) else {
            return div().into_any_element();
        };
        let p = ui::palette(cx);
        let in_card = self.workspace_stage() == super::pane_frame::WorkspaceStage::CardStage;
        let focus_state = self.pane_focus_state(id, window, cx);
        let is_split = self.layout.visible().len() > 1;
        let rows = self.launcher_rows(id, cx);
        let (query, selected) = self
            .split_launchers
            .get(&id)
            .map(|l| (l.query.clone(), l.selected))
            .unwrap_or_default();
        let searching = !query.trim().is_empty();
        let selected = if rows.is_empty() {
            0
        } else {
            selected.min(rows.len().saturating_sub(1))
        };
        let profiles = self.launcher_agent_profiles(id, cx);
        let new_agent_shortcut = krill_app::keys::shortcut_label(window, &super::NewAgentTab)
            .unwrap_or_else(|| text::SHORTCUT_NEW_AGENT_TAB.get().to_owned());
        let terminal_shortcut = krill_app::keys::shortcut_label(window, &super::NewTab)
            .unwrap_or_else(|| text::SHORTCUT_NEW_TAB.get().to_owned());

        let search_row = div()
            .id(("split-launcher-search", id.get()))
            .debug_selector(move || format!("split-launcher-search-{}", id.get()))
            .h(px(LAUNCHER_INPUT_HEIGHT))
            .w_full()
            .px(px(18.))
            .flex()
            .items_center()
            .gap(px(10.))
            .border_b_1()
            .border_color(p.border)
            .child(
                Icon::new(IconName::Search)
                    .with_size(ui::ICON_LG)
                    .text_color(p.muted_foreground.opacity(0.8)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&input).appearance(false).small()),
            );

        let mut list = div()
            .id(("split-launcher-list", id.get()))
            .debug_selector(move || format!("split-launcher-list-{}", id.get()))
            .w_full()
            .max_h(px(LAUNCHER_LIST_MAX_HEIGHT))
            .overflow_y_scroll()
            .p_2()
            .flex()
            .flex_col();

        if rows.is_empty() {
            list = list.child(
                div()
                    .id(("split-launcher-empty", id.get()))
                    .debug_selector(move || format!("split-launcher-empty-{}", id.get()))
                    .py_6()
                    .w_full()
                    .flex()
                    .justify_center()
                    .text_size(ui::TEXT_LG)
                    .text_color(p.muted_foreground)
                    .child(text::LAUNCHER_NO_MATCHES.get()),
            );
        } else {
            let mut shown_agents_heading = false;
            let mut shown_new_heading = false;
            let mut shown_files_heading = false;
            for (idx, row) in rows.iter().enumerate() {
                let is_selected = idx == selected;
                if searching {
                    match row {
                        LauncherRow::Agent { .. } if !shown_agents_heading => {
                            shown_agents_heading = true;
                            list = list.child(group_heading(p, text::LAUNCHER_GROUP_AGENTS.get()));
                        }
                        LauncherRow::Terminal if !shown_new_heading => {
                            shown_new_heading = true;
                            list = list.child(group_heading(p, text::LAUNCHER_GROUP_NEW.get()));
                        }
                        LauncherRow::File { .. } if !shown_files_heading => {
                            shown_files_heading = true;
                            list = list.child(group_heading(p, text::LAUNCHER_GROUP_FILES.get()));
                        }
                        _ => {}
                    }
                }
                let row_el = self.render_launcher_row(
                    id,
                    idx,
                    row,
                    is_selected,
                    &profiles,
                    &new_agent_shortcut,
                    &terminal_shortcut,
                    p,
                    cx,
                );
                list = list.child(row_el);
            }
        }

        let card = div()
            .id(("split-launcher-card", id.get()))
            .debug_selector(move || format!("split-launcher-card-{}", id.get()))
            .w_full()
            .rounded(ui::RADIUS_LG)
            .border_1()
            .border_color(p.border)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(search_row)
            .child(list);

        let close_btn = is_split.then(|| {
            div().mt_3().flex().justify_center().child(
                Button::new(("split-launcher-close", id.get()))
                    .debug_selector(move || format!("split-launcher-close-{}", id.get()))
                    .label(text::LAUNCHER_CLOSE_PANE.get())
                    .ghost()
                    .xsmall()
                    .text_color(p.muted_foreground)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.do_close_pane(id, cx);
                    })),
            )
        });

        let pane_strip = is_split.then(|| {
            let weak = cx.weak_entity();
            div()
                .id(("split-launcher-strip", id.get()))
                .debug_selector(move || format!("split-launcher-strip-{}", id.get()))
                .h(ui::TITLEBAR_HEIGHT)
                .w_full()
                .flex_none()
                .px_2()
                .bg(p.sidebar)
                .border_b_1()
                .border_color(p.border)
                .flex()
                .items_center()
                .justify_between()
                .child(ui::button_with_tooltip(
                    Button::new(("split-launcher-strip-plus", id.get()))
                        .debug_selector(move || format!("split-launcher-strip-plus-{}", id.get()))
                        .icon(IconName::Plus)
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.new_bot_launcher_tab(cx);
                        })),
                    format!("split-launcher-strip-plus-{}", id.get()),
                    text::NEW_TAB.get(),
                ))
                .child(
                    ui::button_with_tooltip(
                        Button::new(("split-launcher-strip-more", id.get()))
                            .debug_selector(move || {
                                format!("split-launcher-strip-more-{}", id.get())
                            })
                            .icon(IconName::Ellipsis)
                            .ghost()
                            .xsmall(),
                        format!("split-launcher-strip-more-{}", id.get()),
                        text::LAUNCHER_PANE_ACTIONS.get(),
                    )
                    .dropdown_menu(move |menu, _, _| {
                        let weak = weak.clone();
                        menu.item(
                            PopupMenuItem::new(text::LAUNCHER_CLOSE_SPLIT_PANE.get())
                                .icon(IconName::Close)
                                .on_click(move |_, _, cx| {
                                    weak.update(cx, |this, cx| {
                                        this.do_close_pane(id, cx);
                                    })
                                    .ok();
                                }),
                        )
                    }),
                )
        });

        let pane_body = div()
            .id(("split-launcher-pane", id.get()))
            .debug_selector(move || format!("split-launcher-pane-{}", id.get()))
            .size_full()
            .flex()
            .flex_col()
            .bg(p.background)
            .text_color(p.foreground)
            .font_family(ui::FONT_UI)
            .capture_action(cx.listener(move |this, _: &MoveUp, window, cx| {
                if this.launcher_input_is_composing(id, window, cx) {
                    return;
                }
                cx.stop_propagation();
                this.step_launcher_selection(id, -1, cx);
            }))
            .capture_action(cx.listener(move |this, _: &MoveDown, window, cx| {
                if this.launcher_input_is_composing(id, window, cx) {
                    return;
                }
                cx.stop_propagation();
                this.step_launcher_selection(id, 1, cx);
            }))
            .capture_action(cx.listener(move |this, _: &Enter, window, cx| {
                if this.launcher_input_is_composing(id, window, cx) {
                    return;
                }
                cx.stop_propagation();
                let selected = this.split_launchers.get(&id).map_or(0, |l| l.selected);
                this.activate_launcher_row(id, selected, window, cx);
            }))
            .capture_action(cx.listener(move |this, _: &Escape, window, cx| {
                if this.launcher_input_is_composing(id, window, cx) {
                    return;
                }
                if this.clear_launcher_query(id, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                let m = e.keystroke.modifiers;
                if m.control && !m.platform && !m.alt && !m.shift {
                    match e.keystroke.key.as_str() {
                        "n" | "j" => {
                            cx.stop_propagation();
                            window.prevent_default();
                            this.step_launcher_selection(id, 1, cx);
                            return;
                        }
                        "p" | "k" => {
                            cx.stop_propagation();
                            window.prevent_default();
                            this.step_launcher_selection(id, -1, cx);
                            return;
                        }
                        _ => {}
                    }
                }
                if !m.modified() {
                    match e.keystroke.key.as_str() {
                        "home" => {
                            cx.stop_propagation();
                            this.set_launcher_selection(id, 0, cx);
                        }
                        "end" => {
                            cx.stop_propagation();
                            let last = this.launcher_rows(id, cx).len().saturating_sub(1);
                            this.set_launcher_selection(id, last, cx);
                        }
                        _ => {}
                    }
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change(true, cx, |l| l.focus(id));
                    this.focus_split_launcher(id, window, cx);
                }),
            )
            .children(pane_strip)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .px_4()
                    .py(px(40.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(LAUNCHER_CARD_MAX_WIDTH))
                            .flex()
                            .flex_col()
                            .child(card)
                            .children(close_btn),
                    ),
            )
            .into_any_element();

        if in_card {
            super::pane_frame::render_pane_card(id, focus_state, p, p.background, pane_body)
        } else {
            pane_body
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Renders one launcher row with shared pane, selection, profiles, shortcuts and palette context"
    )]
    fn render_launcher_row(
        &self,
        pane: PaneId,
        index: usize,
        row: &LauncherRow,
        selected: bool,
        profiles: &[(String, String)],
        new_agent_shortcut: &str,
        terminal_shortcut: &str,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row_selector = match row {
            LauncherRow::DefaultAgent { .. } => {
                format!("split-launcher-row-default-agent-{}", pane.get())
            }
            LauncherRow::Agent { agent_id, .. } => {
                format!("split-launcher-row-agent-{agent_id}-{}", pane.get())
            }
            LauncherRow::Terminal => format!("split-launcher-row-terminal-{}", pane.get()),
            LauncherRow::File { filename, .. } => {
                format!("split-launcher-row-file-{filename}-{}", pane.get())
            }
        };
        let base = div()
            .id(ElementId::Name(
                format!("split-launcher-row-{}-{index}", pane.get()).into(),
            ))
            .debug_selector(move || row_selector.clone())
            .role(gpui::Role::Button)
            .w_full()
            .h(px(LAUNCHER_ROW_HEIGHT))
            .px(px(10.))
            .rounded(ui::RADIUS_SM)
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(10.))
            .text_size(ui::TEXT_LG)
            .when(selected, |d| d.bg(p.accent).text_color(p.accent_foreground))
            .when(!selected, |d| {
                d.text_color(p.popover_foreground)
                    .hover(|s| s.bg(p.accent).text_color(p.accent_foreground))
            })
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this
                    .split_launchers
                    .get(&pane)
                    .is_some_and(|l| l.selected != index)
                {
                    this.set_launcher_selection(pane, index, cx);
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.activate_launcher_row(pane, index, window, cx);
            }));

        match row {
            LauncherRow::DefaultAgent { agent_id, label } => {
                let icon = if let Some(bot) = self.bot_for_pane(pane) {
                    self.bot_avatar(bot, ui::ICON_LG, px(4.), p)
                } else if agent_id.is_empty() {
                    ui::icons::svg_box(ui::icons::BOT_SVG, ui::ICON_LG, p.muted_foreground)
                        .into_any_element()
                } else {
                    ui::icons::agent_brand_icon(agent_id, ui::ICON_LG, p.foreground)
                        .into_any_element()
                };
                let weak = cx.weak_entity();
                let menu_profiles = profiles.to_vec();
                let chevron = div()
                    .id(("split-launcher-agent-picker-wrap", pane.get()))
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        ui::button_with_tooltip(
                            Button::new(("split-launcher-agent-picker", pane.get()))
                                .debug_selector(move || {
                                    format!("split-launcher-agent-picker-{}", pane.get())
                                })
                                .icon(IconName::ChevronDown)
                                .ghost()
                                .xsmall(),
                            format!("split-launcher-agent-picker-{}", pane.get()),
                            text::LAUNCHER_CHOOSE_AGENT.get(),
                        )
                        .dropdown_menu(move |mut menu, _, _| {
                            for (profile_id, profile_name) in &menu_profiles {
                                let pid = profile_id.clone();
                                let weak = weak.clone();
                                let icon = ui::icons::agent_menu_icon(&pid);
                                menu = menu.item(
                                    PopupMenuItem::new(profile_name.clone())
                                        .icon(icon)
                                        .on_click(move |_, _, cx| {
                                            weak.update(cx, |this, cx| {
                                                this.launch_agent_in_launcher_pane(
                                                    pane,
                                                    pid.clone(),
                                                    cx,
                                                );
                                            })
                                            .ok();
                                        }),
                                );
                            }
                            menu
                        }),
                    );
                base.child(icon)
                    .child(div().min_w_0().truncate().child(label.clone()))
                    .when(!profiles.is_empty(), |d| d.child(chevron))
                    .child(div().flex_1())
                    .child(shortcut_hint(p, new_agent_shortcut))
                    .into_any_element()
            }
            LauncherRow::Agent { agent_id, label } => base
                .child(ui::icons::agent_brand_icon(
                    agent_id,
                    ui::ICON_LG,
                    p.foreground,
                ))
                .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                .into_any_element(),
            LauncherRow::Terminal => base
                .child(ui::icons::svg_box(
                    ui::icons::SQUARE_TERMINAL_SVG,
                    ui::ICON_LG,
                    p.muted_foreground,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(text::LAUNCHER_TERMINAL.get()),
                )
                .child(shortcut_hint(p, terminal_shortcut))
                .into_any_element(),
            LauncherRow::File {
                filename,
                directory,
                ..
            } => base
                .child(
                    Icon::new(IconName::File)
                        .with_size(ui::ICON_LG)
                        .text_color(p.muted_foreground),
                )
                .child(div().min_w_0().truncate().child(filename.clone()))
                .when(!directory.is_empty(), |d| {
                    d.child(
                        div()
                            .ml_auto()
                            .min_w_0()
                            .truncate()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(directory.clone()),
                    )
                })
                .into_any_element(),
        }
    }
}

fn group_heading(p: &ui::Palette, label: &'static str) -> impl IntoElement {
    div()
        .px(px(10.))
        .py_1()
        .text_size(ui::TEXT_SM)
        .font_weight(FontWeight::MEDIUM)
        .text_color(p.muted_foreground)
        .child(label)
}

fn shortcut_hint(p: &ui::Palette, label: &str) -> impl IntoElement {
    div()
        .flex_none()
        .text_size(ui::TEXT_SM)
        .text_color(p.muted_foreground)
        .child(SharedString::from(label.to_owned()))
}

#[cfg(all(test, unix))]
#[expect(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use krill_app::bots::SidebarBot;
    use krill_app::hosts::HostId;
    use krill_proto::SessionId;
    use krill_proto::bots::{BotRpcRequest, BotRpcResponse, BotSessionSummary};
    use krill_proto::control::{AgentProfile, AgentSettings};

    fn test_bot(id: &str, slug: &str, home_sid: SessionId) -> SidebarBot {
        let bot_id = BotId::new(id).unwrap();
        SidebarBot {
            host: HostId::Local,
            id: bot_id.clone(),
            name: "KrillBot".to_owned(),
            emoji: None,
            avatar: krill_app::bots::avatar::Avatar::Initial("K".to_owned()),
            avatar_color: None,
            description: "Test bot".to_owned(),
            slug: slug.to_owned(),
            paused: false,
            agent_id: "claude".to_owned(),
            permission: krill_proto::agent::Permission::Manual,
            workspace: None,
            attention: 0,
            items_need_you: 0,
            access_requesters: Vec::new(),
            sessions: vec![BotSessionSummary {
                session_id: home_sid,
                bot_id,
                link_id: None,
                pid: 0,
                live: true,
                working: false,
                label: String::new(),
                created_at_ms: 0,
            }],
            updated_at_ms: 0,
            schedules: Vec::new(),
        }
    }

    fn setup_bot_shell<'a>(
        cx: &'a mut TestAppContext,
        tag: &str,
    ) -> (
        Entity<Shell>,
        &'a mut VisualTestContext,
        Arc<Mutex<Vec<BotRpcRequest>>>,
        PathBuf,
    ) {
        let dir =
            std::env::temp_dir().join(format!("krill-bot-launcher-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("layout.bin");
        let calls: Arc<Mutex<Vec<BotRpcRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let rpc_calls = Arc::clone(&calls);
        let next_sid = Arc::new(std::sync::atomic::AtomicU64::new(500));
        let rpc: krill_app::bots::BotRpcCall = Arc::new(move |req: &BotRpcRequest| {
            rpc_calls.lock().unwrap().push(req.clone());
            match req {
                BotRpcRequest::StartBotSession { bot_id, .. } => {
                    let sid = SessionId(next_sid.fetch_add(1, std::sync::atomic::Ordering::SeqCst));
                    Ok(BotRpcResponse::SessionStarted {
                        bot_id: bot_id.clone(),
                        session_id: sid,
                        pid: 0,
                    })
                }
                _ => Ok(BotRpcResponse::Bots(Vec::new())),
            }
        });
        let (shell, vcx) = super::super::tests::open(cx, state);
        let bot_home = shell.update(vcx, |s, cx| {
            s.sidebar_collapsed = true;
            s.panel_collapsed = true;
            s.bot_rpc_override = Some(rpc);
            s.agents = Some(AgentSettings {
                profiles: vec![
                    AgentProfile {
                        id: "claude".to_owned(),
                        name: "Claude".to_owned(),
                    },
                    AgentProfile {
                        id: "codex".to_owned(),
                        name: "Codex".to_owned(),
                    },
                ],
                permission: krill_proto::agent::Permission::Manual,
            });
            let bot = test_bot("bot-1", "krill-test-bot", SessionId(42));
            let home = s.bot_folder(&bot).unwrap_or_else(|| dir.join("bot-home"));
            std::fs::create_dir_all(&home).ok();
            std::fs::write(home.join("SOUL.md"), "# Soul\n").ok();
            std::fs::create_dir_all(home.join("notes")).ok();
            std::fs::write(home.join("notes/plan.md"), "# Plan\n").ok();
            s.sidebar_bots = vec![bot];
            let pane = s.layout.focused().unwrap();
            s.layout.set_session(pane, SessionId(42));
            s.layout.set_launch(
                pane,
                Create {
                    account: None,
                    program: Program::Login,
                    cwd: home.as_os_str().as_bytes().to_vec(),
                    cols: pane::SIZE.0,
                    rows: pane::SIZE.1,
                },
            );
            s.layout.set_chat_view(pane, true);
            cx.notify();
            home
        });
        vcx.run_until_parked();
        (shell, vcx, calls, bot_home)
    }

    #[gpui::test]
    fn bot_split_opens_launcher_and_close_pane_collapses(cx: &mut TestAppContext) {
        let (shell, vcx, _calls, _home) = setup_bot_shell(cx, "split-close");
        let original_pane = shell.read_with(vcx, |s, _| s.layout.focused().unwrap());

        shell.update(vcx, |s, cx| {
            s.split_active(Axis::Horizontal, cx);
        });
        vcx.run_until_parked();

        let (visible, focused, is_launcher, rows) = shell.read_with(vcx, |s, cx| {
            let focused = s.layout.focused().unwrap();
            (
                s.layout.visible(),
                focused,
                s.is_split_launcher(focused),
                s.launcher_rows(focused, cx),
            )
        });
        assert_eq!(visible.len(), 2);
        assert_ne!(focused, original_pane);
        assert!(is_launcher, "split pane in bot workspace opens launcher");
        assert!(
            matches!(
                rows.first(),
                Some(LauncherRow::DefaultAgent { agent_id, label })
                    if agent_id == "claude" && label.contains("KrillBot")
            ),
            "bot split launcher prioritizes New chat with {{bot}}: {rows:?}"
        );
        assert!(matches!(rows.get(1), Some(LauncherRow::Terminal)));

        let card_sel: &'static str =
            Box::leak(format!("split-launcher-card-{}", focused.get()).into_boxed_str());
        let close_sel: &'static str =
            Box::leak(format!("split-launcher-close-{}", focused.get()).into_boxed_str());
        assert!(
            vcx.debug_bounds(card_sel).is_some(),
            "launcher card rendered"
        );
        let close_bounds = vcx
            .debug_bounds(close_sel)
            .expect("Close pane button visible in split");

        vcx.simulate_click(close_bounds.center(), Modifiers::default());
        vcx.run_until_parked();

        let (visible_after, focused_after, launchers_left) = shell.read_with(vcx, |s, _| {
            (
                s.layout.visible(),
                s.layout.focused(),
                s.split_launchers.len(),
            )
        });
        assert_eq!(visible_after, vec![original_pane]);
        assert_eq!(focused_after, Some(original_pane));
        assert_eq!(launchers_left, 0);
    }

    #[gpui::test]
    fn bot_cmd_t_opens_launcher_tab_and_second_cmd_t_opens_terminal(cx: &mut TestAppContext) {
        let (shell, vcx, _calls, home) = setup_bot_shell(cx, "cmd-t-tab");
        let initial_tabs = shell.read_with(vcx, |s, _| s.layout.tabs().len());

        vcx.dispatch_action(super::super::NewTab);
        vcx.run_until_parked();

        let (tabs_after, visible_workspace_tabs, focused, is_launcher) =
            shell.read_with(vcx, |s, cx| {
                let focused = s.layout.focused().unwrap();
                (
                    s.layout.tabs().len(),
                    s.workspace_tabs(cx).len(),
                    focused,
                    s.is_split_launcher(focused),
                )
            });
        assert_eq!(tabs_after, initial_tabs + 1);
        assert_eq!(
            visible_workspace_tabs, 2,
            "bot home tab and Cmd+T launcher tab must both stay visible in the bot tab bar"
        );
        assert!(is_launcher, "Cmd+T in bot workspace opens launcher tab");

        let card_sel: &'static str =
            Box::leak(format!("split-launcher-card-{}", focused.get()).into_boxed_str());
        let close_sel: &'static str =
            Box::leak(format!("split-launcher-close-{}", focused.get()).into_boxed_str());
        assert!(vcx.debug_bounds(card_sel).is_some());
        assert!(
            vcx.debug_bounds(close_sel).is_none(),
            "single-pane tab hides Close pane button like Orca"
        );

        // Pressing Cmd+T while already inside the launcher opens Terminal in place (matching ⌘T row).
        vcx.dispatch_action(super::super::NewTab);
        vcx.run_until_parked();

        let (still_launcher, launch, chat_view) = shell.read_with(vcx, |s, _| {
            (
                s.is_split_launcher(focused),
                s.layout.launch(focused).cloned(),
                s.layout.chat_view(focused),
            )
        });
        assert!(!still_launcher);
        let launch = launch.expect("terminal launch");
        assert_eq!(launch.program, Program::Login);
        let cwd = {
            use krill_proto::os::ffi::OsStringExt as _;
            PathBuf::from(std::ffi::OsString::from_vec(launch.cwd))
        };
        assert_eq!(cwd, home);
        assert_eq!(chat_view, Some(false));
    }

    #[gpui::test]
    fn launcher_default_agent_starts_bot_chat_in_place_without_prompt_modal(
        cx: &mut TestAppContext,
    ) {
        let (shell, vcx, calls, _home) = setup_bot_shell(cx, "new-bot-chat");
        let original_pane = shell.read_with(vcx, |s, _| s.layout.focused().unwrap());

        shell.update(vcx, |s, cx| {
            s.split_active(Axis::Horizontal, cx);
        });
        vcx.run_until_parked();
        let launcher_pane = shell.read_with(vcx, |s, _| s.layout.focused().unwrap());

        let row_sel: &'static str = Box::leak(
            format!("split-launcher-row-default-agent-{}", launcher_pane.get()).into_boxed_str(),
        );
        let picker_sel: &'static str = Box::leak(
            format!("split-launcher-agent-picker-{}", launcher_pane.get()).into_boxed_str(),
        );
        let row_bounds = vcx
            .debug_bounds(row_sel)
            .expect("default agent row rendered");
        let picker_bounds = vcx
            .debug_bounds(picker_sel)
            .expect("agent picker chevron rendered");
        // Click the left label side of the row rather than row_bounds.center(), which
        // can land on the inline agent-picker chevron depending on system font metrics.
        let click_point = gpui::point(row_bounds.origin.x + px(32.), row_bounds.center().y);
        assert!(
            !picker_bounds.contains(&click_point),
            "click point {click_point:?} must not hit agent picker {picker_bounds:?}"
        );
        vcx.simulate_click(click_point, Modifiers::default());
        vcx.run_until_parked();

        let recorded = calls.lock().unwrap();
        assert!(
            recorded.iter().any(|r| matches!(
                r,
                BotRpcRequest::StartBotSession {
                    bot_id,
                    prompt: None,
                    ..
                } if bot_id.as_str() == "bot-1"
            )),
            "must start bot session immediately without prompt: {recorded:?}"
        );
        drop(recorded);

        let (visible, still_launcher, session, chat_view) = shell.read_with(vcx, |s, _| {
            (
                s.layout.visible(),
                s.is_split_launcher(launcher_pane),
                s.layout.session(launcher_pane),
                s.layout.chat_view(launcher_pane),
            )
        });
        assert_eq!(visible, vec![original_pane, launcher_pane]);
        assert!(
            !still_launcher,
            "launcher transitions into live bot chat pane"
        );
        assert_eq!(session, Some(SessionId(500)));
        assert_eq!(chat_view, Some(true));
    }

    #[gpui::test]
    fn launcher_terminal_opens_bot_home_terminal_in_place(cx: &mut TestAppContext) {
        let (shell, vcx, _calls, home) = setup_bot_shell(cx, "open-terminal");

        shell.update(vcx, |s, cx| {
            s.split_active(Axis::Horizontal, cx);
        });
        vcx.run_until_parked();
        let launcher_pane = shell.read_with(vcx, |s, _| s.layout.focused().unwrap());

        let term_sel: &'static str = Box::leak(
            format!("split-launcher-row-terminal-{}", launcher_pane.get()).into_boxed_str(),
        );
        let term_bounds = vcx.debug_bounds(term_sel).expect("Terminal row rendered");
        vcx.simulate_click(term_bounds.center(), Modifiers::default());
        vcx.run_until_parked();

        let (still_launcher, launch, chat_view, focused_bot_id) = shell.read_with(vcx, |s, _| {
            (
                s.is_split_launcher(launcher_pane),
                s.layout.launch(launcher_pane).cloned(),
                s.layout.chat_view(launcher_pane),
                s.focused_bot().map(|b| b.id.clone()),
            )
        });
        assert!(!still_launcher);
        let launch = launch.expect("terminal launch configured");
        assert_eq!(launch.program, Program::Login);
        let cwd = {
            use krill_proto::os::ffi::OsStringExt as _;
            PathBuf::from(std::ffi::OsString::from_vec(launch.cwd))
        };
        assert_eq!(cwd, home);
        assert_eq!(chat_view, Some(false));
        assert_eq!(focused_bot_id, Some(BotId::new("bot-1").unwrap()));
    }

    #[gpui::test]
    fn launcher_keyboard_navigation_and_search_filter(cx: &mut TestAppContext) {
        let (shell, vcx, _calls, _home) = setup_bot_shell(cx, "search-filter");

        shell.update(vcx, |s, cx| {
            s.split_active(Axis::Horizontal, cx);
        });
        vcx.run_until_parked();
        let pane = shell.read_with(vcx, |s, _| s.layout.focused().unwrap());

        // Keyboard MoveDown wraps between DefaultAgent (0) and Terminal (1).
        vcx.dispatch_action(MoveDown);
        vcx.run_until_parked();
        assert_eq!(
            shell.read_with(vcx, |s, _| s.split_launchers.get(&pane).map(|l| l.selected)),
            Some(1)
        );
        vcx.dispatch_action(MoveDown);
        vcx.run_until_parked();
        assert_eq!(
            shell.read_with(vcx, |s, _| s.split_launchers.get(&pane).map(|l| l.selected)),
            Some(0)
        );

        // Search for "codex" -> only Codex agent
        shell.update(vcx, |s, cx| {
            if let Some(l) = s.split_launchers.get_mut(&pane) {
                l.query = "codex".to_owned();
                l.files = vec![PathBuf::from("SOUL.md"), PathBuf::from("notes/plan.md")];
                l.files_loaded = true;
            }
            cx.notify();
        });
        let rows = shell.read_with(vcx, |s, cx| s.launcher_rows(pane, cx));
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0], LauncherRow::Agent { agent_id, .. } if agent_id == "codex"));

        // Escape clears non-empty query
        vcx.dispatch_action(Escape);
        vcx.run_until_parked();
        assert_eq!(
            shell.read_with(vcx, |s, _| s
                .split_launchers
                .get(&pane)
                .map(|l| l.query.clone())),
            Some(String::new())
        );

        // Search for "term" -> Terminal
        shell.update(vcx, |s, cx| {
            if let Some(l) = s.split_launchers.get_mut(&pane) {
                l.query = "term".to_owned();
            }
            cx.notify();
        });
        let rows = shell.read_with(vcx, |s, cx| s.launcher_rows(pane, cx));
        assert_eq!(rows, vec![LauncherRow::Terminal]);

        // Search for "SOUL" -> SOUL.md file
        shell.update(vcx, |s, cx| {
            if let Some(l) = s.split_launchers.get_mut(&pane) {
                l.query = "soul".to_owned();
            }
            cx.notify();
        });
        let rows = shell.read_with(vcx, |s, cx| s.launcher_rows(pane, cx));
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            &rows[0],
            LauncherRow::File { filename, .. } if filename == "SOUL.md"
        ));

        // Opening the matched file replaces the launcher pane in place
        shell.update(vcx, |s, cx| {
            s.open_file_in_launcher_pane(pane, PathBuf::from("SOUL.md"), cx);
        });
        vcx.run_until_parked();
        let (still_launcher, file_name, host) = shell.read_with(vcx, |s, _| {
            (
                s.is_split_launcher(pane),
                s.layout
                    .file(pane)
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned()),
                s.layout.host(pane).clone(),
            )
        });
        assert!(!still_launcher);
        assert_eq!(file_name.as_deref(), Some("SOUL.md"));
        assert_eq!(host, HostId::Local);
    }

    #[gpui::test]
    fn launcher_enter_and_shortcuts_launch_and_close_correctly(cx: &mut TestAppContext) {
        let (shell, vcx, calls, home) = setup_bot_shell(cx, "keys-shortcuts");
        let original_pane = shell.read_with(vcx, |s, _| s.layout.focused().unwrap());

        // Split -> MoveDown to Terminal -> Enter opens terminal in bot home
        shell.update(vcx, |s, cx| s.split_active(Axis::Horizontal, cx));
        vcx.run_until_parked();
        let term_pane = shell.read_with(vcx, |s, _| s.layout.focused().unwrap());
        vcx.dispatch_action(MoveDown);
        vcx.run_until_parked();
        vcx.dispatch_action(Enter {
            secondary: false,
            shift: false,
        });
        vcx.run_until_parked();
        assert!(!shell.read_with(vcx, |s, _| s.is_split_launcher(term_pane)));
        assert_eq!(
            shell.read_with(vcx, |s, _| s.layout.chat_view(term_pane)),
            Some(false)
        );
        let term_cwd = shell.read_with(vcx, |s, _| {
            use krill_proto::os::ffi::OsStringExt as _;
            PathBuf::from(std::ffi::OsString::from_vec(
                s.layout.launch(term_pane).unwrap().cwd.clone(),
            ))
        });
        assert_eq!(term_cwd, home);

        // Split again -> Cmd+W closes empty split pane back to sibling
        shell.update(vcx, |s, cx| s.split_active(Axis::Vertical, cx));
        vcx.run_until_parked();
        let empty_pane = shell.read_with(vcx, |s, _| s.layout.focused().unwrap());
        assert!(shell.read_with(vcx, |s, _| s.is_split_launcher(empty_pane)));
        vcx.dispatch_action(super::super::ClosePane);
        vcx.run_until_parked();
        assert!(!shell.read_with(vcx, |s, _| s.layout.pane_exists(empty_pane)));

        // Split from original -> Cmd+Alt+T launches new bot chat in place
        shell.update(vcx, |s, cx| {
            s.split_pane(original_pane, Axis::Horizontal, cx);
        });
        vcx.run_until_parked();
        let chat_pane = shell.read_with(vcx, |s, _| s.layout.focused().unwrap());
        assert!(shell.read_with(vcx, |s, _| s.is_split_launcher(chat_pane)));
        vcx.dispatch_action(super::super::NewAgentTab);
        vcx.run_until_parked();
        assert!(!shell.read_with(vcx, |s, _| s.is_split_launcher(chat_pane)));
        assert_eq!(
            shell.read_with(vcx, |s, _| s.layout.chat_view(chat_pane)),
            Some(true)
        );
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|r| matches!(r, BotRpcRequest::StartBotSession { prompt: None, .. })),
            "Cmd+Alt+T in launcher starts bot chat without prompt"
        );

        // Selecting a different agent profile (codex) from picker launches it in bot home
        shell.update(vcx, Shell::new_bot_launcher_tab);
        vcx.run_until_parked();
        let other_agent_pane = shell.read_with(vcx, |s, _| s.layout.focused().unwrap());
        shell.update(vcx, |s, cx| {
            s.launch_agent_in_launcher_pane(other_agent_pane, "codex".to_owned(), cx);
        });
        vcx.run_until_parked();
        let other_launch = shell.read_with(vcx, |s, _| s.layout.launch(other_agent_pane).cloned());
        assert_eq!(
            other_launch.as_ref().map(|l| &l.program),
            Some(&Program::Agent {
                id: "codex".to_owned()
            })
        );
    }

    #[gpui::test]
    fn launcher_guards_duplicate_launch_remote_files_and_canonical_agent_id(
        cx: &mut TestAppContext,
    ) {
        let (shell, vcx, calls, _home) = setup_bot_shell(cx, "guards-remote-canonical");
        shell.update(vcx, |s, cx| {
            // Bot configured with alias `claude-code` should match `claude` via canonical_id.
            s.sidebar_bots[0].agent_id = "claude-code".to_owned();
            s.split_active(Axis::Horizontal, cx);
        });
        vcx.run_until_parked();
        let pane = shell.read_with(vcx, |s, _| s.layout.focused().unwrap());

        // Remote host launcher must NOT run local quick_open::list.
        shell.update(vcx, |s, cx| {
            s.layout.set_host(pane, HostId::Remote("remote-box".into()));
            s.load_split_launcher_files(pane, cx);
        });
        vcx.run_until_parked();
        shell.read_with(vcx, |s, _| {
            let l = s.split_launchers.get(&pane).unwrap();
            assert!(l.files_loaded);
            assert!(l.files.is_empty());
        });

        // Switch back to Local, simulate in-flight `launching = true` -> duplicate triggers ignored.
        calls.lock().unwrap().clear();
        shell.update(vcx, |s, cx| {
            s.layout.set_host(pane, HostId::Local);
            if let Some(l) = s.split_launchers.get_mut(&pane) {
                l.launching = true;
            }
            s.launch_default_agent_in_launcher_pane(pane, cx);
            s.launch_agent_in_launcher_pane(pane, "claude".to_owned(), cx);
        });
        vcx.run_until_parked();
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|r| matches!(r, BotRpcRequest::StartBotSession { .. })),
            "launching guard must prevent duplicate StartBotSession RPCs"
        );

        // Clear launching flag and pick `"claude"` against bot with `"claude-code"` -> starts bot session.
        shell.update(vcx, |s, cx| {
            s.clear_split_launcher_launching(pane);
            s.launch_agent_in_launcher_pane(pane, "claude".to_owned(), cx);
        });
        vcx.run_until_parked();
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|r| matches!(r, BotRpcRequest::StartBotSession { .. })),
            "canonical_id must match claude-code bot when claude is selected"
        );
    }
}
