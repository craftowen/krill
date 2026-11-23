// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's "Import from another app" (`ImportBotDialog`, `ImportBotHero`, `ImportBotSourceList`,
//! `ImportBotExportFields`): the old bot, an arrow and the new one; its name and picture picker;
//! the bots `OpenClaw`, Hermes, `OpenBot` or Orca keep on this computer, one to pick, then "Another
//! app…" (send the old bot a message asking for a copy of itself, then add the folder it made);
//! collapsible "Runs on <host> with <agent> · Change"; and a footer saying keys and logins stay
//! behind (or "Update <bot>" when re-importing an unchanged selection).

use std::path::PathBuf;

use gpui::{
    AnyElement, Context, ExternalPaths, FontWeight, Pixels, SharedString, Subscription, Task,
    Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{Disableable as _, Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::bots::{
    BOT_IMPORT_EXPORT_PROMPT, BotConfig, BotId, BotRpcRequest, BotRpcResponse, CreateBotParams,
    CreateBotV2Params, ExternalBotCandidate, ExternalBotSourceApp, bot_import_app_label,
    bot_name_from_export_folder,
};

use super::pickers::{
    BotAgentSetup, HostChoice, RunsOnVariant, available_runtimes, find_imported_bot,
    render_bot_name_suggestion, render_runs_on_picker,
};
use super::picture_picker::PictureState;
use super::rpc::{BotRpcCall, call_checked};
use super::{OpenBotHome, View, avatar};
use crate::hosts::HostId;
use crate::i18n::catalog::import_bot as text;
use crate::ui;

/// The dialog's width (Orca `sm:max-w-md`).
pub(super) const DIALOG_WIDTH: Pixels = px(448.);

/// Orca's `BOT_IMPORT_EXPORT_PROMPT`: what the old bot is asked to do.
pub(super) const EXPORT_PROMPT: &str = BOT_IMPORT_EXPORT_PROMPT;

/// Matches Orca `BotPickList.tsx` (`FILTER_FROM = 10`).
const FILTER_FROM: usize = 10;

/// What the dialog shows: the bots found, or the two steps for another app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ImportView {
    #[default]
    Pick,
    Export,
}

/// The import dialog's own state.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent UI state flags matching Orca ImportBotDialog"
)]
pub(super) struct ImportDialog {
    pub view: ImportView,
    /// The picked bot's source key (`"{app:?}:{path}"`).
    pub picked: Option<String>,
    /// The name as typed; `None` follows the pick (or stays blank for previously imported sources).
    pub name_input: Option<gpui::Entity<InputState>>,
    pub typed: Option<String>,
    pub reply_input: Option<gpui::Entity<TextareaState>>,
    pub filter_input: Option<gpui::Entity<InputState>>,
    pub folder: Option<PathBuf>,
    pub files: Vec<PathBuf>,
    pub copied: bool,
    pub message_open: bool,
    pub creating: bool,
    pub scanning: bool,
    pub scanned: bool,
    pub scan_host: Option<HostId>,
    pub candidates: Vec<ExternalBotCandidate>,
    pub canonical_paths: std::collections::HashMap<String, String>,
    pub host_bots: Vec<BotConfig>,
    pub imported_sources: Vec<(String, BotId)>,
    pub picture: PictureState,
    pub runs_on: Option<BotAgentSetup>,
    scan_task: Option<Task<()>>,
    subs: Vec<Subscription>,
}

impl ImportDialog {
    #[must_use]
    pub fn new(cx: &mut gpui::App) -> Self {
        Self {
            view: ImportView::Pick,
            picked: None,
            name_input: None,
            typed: None,
            reply_input: None,
            filter_input: None,
            folder: None,
            files: Vec::new(),
            copied: false,
            message_open: false,
            creating: false,
            scanning: false,
            scanned: false,
            scan_host: None,
            candidates: Vec::new(),
            canonical_paths: std::collections::HashMap::new(),
            host_bots: Vec::new(),
            imported_sources: Vec::new(),
            picture: PictureState::new("import-bot", None, cx),
            runs_on: None,
            scan_task: None,
            subs: Vec::new(),
        }
    }

    pub fn input_is_composing(&self, window: &mut Window, app: &mut gpui::App) -> bool {
        super::opt_input_composing(self.name_input.as_ref(), window, app)
            || super::opt_textarea_composing(self.reply_input.as_ref(), window, app)
            || super::opt_input_composing(self.filter_input.as_ref(), window, app)
            || self.picture.input_is_composing(window, app)
    }
}

fn pick_key(c: &ExternalBotCandidate) -> String {
    format!("{:?}:{}:{}", c.app, c.agent, c.path)
}

/// "Hermes default profile", "`OpenClaw` agent", or "imported before as `<bot>`".
fn row_detail(c: &ExternalBotCandidate, existing: Option<&BotConfig>) -> String {
    if let Some(bot) = existing {
        return text::IMPORTED_AS.format(&[("bot", &bot.name)]);
    }
    let kind = match (c.app, c.agent.as_str()) {
        (ExternalBotSourceApp::Hermes, "default") => text::DEFAULT_PROFILE.get(),
        (ExternalBotSourceApp::Hermes, _) => text::PROFILE.get(),
        _ => text::AGENT.get(),
    };
    text::ROW_DETAIL.format(&[("app", bot_import_app_label(c.app)), ("kind", kind)])
}

/// Orca's `botNameFromExportFolder`: "sage-export" is "Sage".
pub(super) fn name_from_export_folder(path: &std::path::Path) -> Option<String> {
    bot_name_from_export_folder(&path.to_string_lossy())
}

/// The first turn of a bot made from another app's export (Orca `botImportKickoff`).
pub(super) fn export_kickoff(
    name: &str,
    folder: &std::path::Path,
    files: &[PathBuf],
    reply: &str,
) -> String {
    let mut where_ = format!("Its files are in {}", folder.display());
    if !files.is_empty() {
        let list: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
        where_.push_str(", plus ");
        where_.push_str(&list.join(", "));
    }
    where_.push('.');
    let mut out = format!(
        "Your owner made you to take over \"{name}\" from another app. {where_}\n\nBring it over instead of the usual setup: read those files, write what defines it into your SOUL.md and what it knew into MEMORY.md, and tell your owner what came over and what didn't. Never copy passwords, keys, tokens or cookies."
    );
    if !reply.trim().is_empty() {
        out.push_str("\n\nWhat it wrote back:\n\n");
        out.push_str(reply.trim());
    }
    out
}

fn initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default()
}

impl View {
    /// The bots dialog is the small import dialog: its frame takes this width.
    #[must_use]
    pub fn dialog_width(&self) -> Option<Pixels> {
        (self.screen == super::Screen::Import).then_some(DIALOG_WIDTH)
    }

    fn import_host_choices(&self, _cx: &gpui::App) -> Vec<HostChoice> {
        let mut choices: Vec<HostChoice> = self
            .available_hosts
            .iter()
            .map(|(id, label, target)| HostChoice {
                id: id.clone(),
                label: label.clone().into(),
                rpc: if id == &self.host {
                    self.rpc.clone()
                } else {
                    super::daemon_bot_rpc(target.clone())
                },
            })
            .collect();
        if let Some(pos) = choices.iter().position(|c| c.id == self.host) {
            let cur = choices.remove(pos);
            choices.insert(0, cur);
        } else {
            choices.insert(
                0,
                HostChoice {
                    id: self.host.clone(),
                    label: crate::i18n::catalog::bots::HOST_LOCAL
                        .mac_or(crate::i18n::catalog::other_os::HOST_LOCAL)
                        .get()
                        .into(),
                    rpc: self.rpc.clone(),
                },
            );
        }
        choices
    }

    /// Opens Import: a fresh dialog, and a look for bots on the active host.
    pub(super) fn reset_import_dialog(&mut self, cx: &mut Context<Self>) {
        self.import_dialog.picture.cancel_running_generation();
        let mut next = ImportDialog::new(cx);
        let choices = self.import_host_choices(cx);
        let setup = BotAgentSetup::from_hosts(choices, Self::import_runs_on_mut, cx);
        next.runs_on = Some(setup);
        next.host_bots.clone_from(&self.bots);
        self.import_dialog = next;
        self.import_state.candidates.clear();
        self.import_state.source_filter = None;
        self.scan_import_host(cx);
    }

    fn import_runs_on_mut(&mut self) -> Option<&mut BotAgentSetup> {
        self.import_dialog.runs_on.as_mut()
    }

    fn import_picture_mut(&mut self) -> &mut PictureState {
        &mut self.import_dialog.picture
    }

    fn on_import_update_host(_: &mut Self, cx: &mut Context<Self>) {
        cx.emit(super::OpenRemoteSettings);
    }

    fn scan_import_host(&mut self, cx: &mut Context<Self>) {
        let (rpc, host_id) = match &self.import_dialog.runs_on {
            Some(setup) => (setup.rpc.clone(), setup.host_id.clone()),
            None => (self.rpc.clone(), self.host.clone()),
        };
        let is_local = host_id.is_local();
        self.import_dialog.scan_host = Some(host_id.clone());
        self.import_dialog.scanning = true;
        self.import_dialog.scanned = false;
        self.import_dialog.candidates.clear();
        self.import_dialog.canonical_paths.clear();
        self.import_dialog.imported_sources.clear();
        self.error = None;
        let active_host = self.host.clone();
        let active_bots = self.bots.clone();

        self.import_dialog.scan_task = Some(cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let cands = match call_checked(
                        &rpc,
                        &BotRpcRequest::ImportExternalBots {
                            source: None,
                            path: None,
                            dry_run: true,
                        },
                    )? {
                        BotRpcResponse::ImportedExternalBots { candidates, .. } => candidates,
                        other => {
                            return Err(format!(
                                "unexpected ImportExternalBots response: {other:?}"
                            ));
                        }
                    };
                    let mut canonical_paths = std::collections::HashMap::new();
                    if is_local {
                        for cand in &cands {
                            if let Ok(canon) = std::fs::canonicalize(&cand.path) {
                                canonical_paths.insert(
                                    cand.path.clone(),
                                    canon.to_string_lossy().into_owned(),
                                );
                            }
                        }
                    }
                    let imported_sources =
                        match call_checked(&rpc, &BotRpcRequest::ListImportedSources) {
                            Ok(BotRpcResponse::ImportedSources(sources)) => sources,
                            _ => Vec::new(),
                        };
                    let bots = if host_id == active_host && !active_bots.is_empty() {
                        active_bots
                    } else {
                        match call_checked(&rpc, &BotRpcRequest::ListBots) {
                            Ok(BotRpcResponse::Bots(list)) => list,
                            _ => Vec::new(),
                        }
                    };
                    Ok::<_, String>((cands, canonical_paths, imported_sources, bots))
                })
                .await;
            this.update(cx, |this, cx| {
                this.import_dialog.scanning = false;
                this.import_dialog.scanned = true;
                match outcome {
                    Ok((cands, canonical_paths, sources, bots)) => {
                        this.import_dialog.candidates.clone_from(&cands);
                        this.import_dialog.canonical_paths = canonical_paths;
                        this.import_dialog.imported_sources = sources;
                        this.import_dialog.host_bots = bots;
                        this.import_state.candidates = cands;
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn ensure_runs_on_initialized(&mut self, cx: &mut Context<Self>) {
        if self.import_dialog.runs_on.is_none() {
            let choices = self.import_host_choices(cx);
            let setup = BotAgentSetup::from_hosts(choices, Self::import_runs_on_mut, cx);
            self.import_dialog.runs_on = Some(setup);
            self.import_dialog.host_bots = self.bots.clone();
            if self.import_dialog.scan_host.is_none() {
                self.scan_import_host(cx);
            }
        } else if let Some(setup) = &self.import_dialog.runs_on
            && self.import_dialog.scan_host.as_ref() != Some(&setup.host_id)
        {
            self.scan_import_host(cx);
        }
    }

    fn import_pick(&self) -> Option<&ExternalBotCandidate> {
        let cands = &self.import_dialog.candidates;
        self.import_dialog
            .picked
            .as_ref()
            .and_then(|key| cands.iter().find(|c| &pick_key(c) == key))
            .or_else(|| cands.first())
    }

    fn import_exporting(&self) -> bool {
        self.import_dialog.view == ImportView::Export
            || (self.import_dialog.scanned
                && !self.import_dialog.scanning
                && self.import_dialog.candidates.is_empty())
    }

    fn imported_before<'a>(&'a self, pick: Option<&ExternalBotCandidate>) -> Option<&'a BotConfig> {
        let cand = pick?;
        find_imported_bot(
            &self.import_dialog.host_bots,
            &self.import_dialog.imported_sources,
            &cand.path,
            self.import_dialog
                .canonical_paths
                .get(&cand.path)
                .map(String::as_str),
        )
    }

    fn import_name(&self) -> String {
        if let Some(t) = &self.import_dialog.typed {
            return t.clone();
        }
        if self.import_exporting() {
            return String::new();
        }
        let pick = self.import_pick();
        if self.imported_before(pick).is_some() {
            String::new()
        } else {
            pick.map(|c| c.name.clone()).unwrap_or_default()
        }
    }

    fn ensure_import_dialog_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.import_dialog.name_input.is_none() {
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder(text::NAME_PLACEHOLDER.get()));
            let sub =
                cx.subscribe_in(
                    &input,
                    window,
                    |this, inp, ev: &InputEvent, window, cx| match ev {
                        InputEvent::Change => {
                            this.import_dialog.typed = Some(inp.read(cx).value().to_string());
                            cx.notify();
                        }
                        InputEvent::PressEnter { .. }
                            if !crate::jump::entity_input_is_composing(inp, window, cx) =>
                        {
                            this.submit_import_or_update(cx);
                        }
                        _ => {}
                    },
                );
            self.import_dialog.subs.push(sub);
            input.update(cx, |s, cx| s.focus(window, cx));
            self.import_dialog.name_input = Some(input);
        }
        if self.import_dialog.reply_input.is_none() {
            let reply = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(3, 6)
                    .placeholder(text::REPLY_PLACEHOLDER.get())
            });
            self.import_dialog.reply_input = Some(reply);
        }
        if self.import_dialog.filter_input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(text::FILTER.get()));
            let sub = cx.observe(&input, |_, _, cx| cx.notify());
            self.import_dialog.subs.push(sub);
            self.import_dialog.filter_input = Some(input);
        }
    }

    fn take_name_suggestion(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.import_dialog.typed = Some(name.clone());
        if let Some(inp) = &self.import_dialog.name_input {
            inp.update(cx, |s, cx| {
                s.set_value(name, window, cx);
                s.focus(window, cx);
            });
        }
        cx.notify();
    }

    /// Syncs the heading name input when picking a row, and swaps placeholder when re-importing.
    fn sync_import_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = (!self.import_exporting())
            .then(|| self.import_pick().cloned())
            .flatten();
        let again =
            self.imported_before(pick.as_ref()).is_some() && self.import_name().trim().is_empty();
        let wanted_placeholder = if again {
            text::NAME_ANOTHER.get()
        } else {
            text::NAME_PLACEHOLDER.get()
        };
        if let Some(input) = &self.import_dialog.name_input {
            input.update(cx, |s, cx| {
                s.set_placeholder(wanted_placeholder, window, cx);
            });
        }
        if self.import_dialog.typed.is_some() {
            return;
        }
        let target = self.import_name();
        if let Some(input) = &self.import_dialog.name_input
            && input.read(cx).value().as_ref() != target
        {
            input.update(cx, |s, cx| s.set_value(target, window, cx));
            // Setting value emits Change; stay un-typed so it continues tracking the selection.
            self.import_dialog.typed = None;
        }
    }

    pub(super) fn submit_import_or_update(&mut self, cx: &mut Context<Self>) {
        if !self.import_exporting() {
            let trimmed = self.import_name().trim().to_owned();
            if trimmed.is_empty()
                && let Some(existing) = self.imported_before(self.import_pick()).cloned()
                && let Some(pick) = self.import_pick().cloned()
            {
                let host = self
                    .import_dialog
                    .runs_on
                    .as_ref()
                    .map_or_else(|| self.host.clone(), |s| s.host_id.clone());
                let draft = text::UPDATE_DRAFT.format(&[
                    ("name", &pick.name),
                    ("app", bot_import_app_label(pick.app)),
                ]);
                self.screen = super::Screen::Detail;
                self.selected_bot_id = Some(existing.id.clone());
                cx.emit(OpenBotHome {
                    bot_id: existing.id,
                    kickoff: Some(draft),
                    host,
                });
                cx.notify();
                return;
            }
        }
        self.create_imported_bot(cx);
    }

    /// Create: imports the picked candidate (allowing copies with chosen settings) or creates a
    /// new bot initialized with the export folder + reply kickoff.
    pub(super) fn create_imported_bot(&mut self, cx: &mut Context<Self>) {
        if self.import_dialog.creating {
            return;
        }
        let name = self.import_name().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let Some(setup) = &self.import_dialog.runs_on else {
            return;
        };
        if setup.is_blocked() || available_runtimes(setup.host_is_local, cx).is_empty() {
            return;
        }
        let rpc: BotRpcCall = setup.rpc.clone();
        let host = setup.host_id.clone();
        let agent = setup.runtime.to_owned();
        let model = setup.model.clone();
        let effort = setup.effort.clone();
        let permission_mode = setup.permission_mode;
        let (avatar_field, avatar_color, upload_png) =
            self.import_dialog.picture.avatar_and_upload();
        let exporting = self.import_exporting();

        let job = if exporting {
            if !host.is_local() {
                return;
            }
            let Some(folder) = self
                .import_dialog
                .folder
                .clone()
                .or_else(|| self.import_dialog.files.first().cloned())
            else {
                return;
            };
            let files: Vec<PathBuf> = self
                .import_dialog
                .files
                .iter()
                .filter(|f| **f != folder)
                .cloned()
                .collect();
            let reply = self
                .import_dialog
                .reply_input
                .as_ref()
                .map(|i| i.read(cx).value().to_string())
                .unwrap_or_default();
            Err(export_kickoff(&name, &folder, &files, &reply))
        } else {
            let Some(pick) = self.import_pick().cloned() else {
                return;
            };
            Ok(pick)
        };

        let params = CreateBotV2Params {
            base: CreateBotParams {
                name,
                agent_id: agent,
                description: String::new(),
                emoji: None,
                avatar: avatar_field,
                model,
                effort,
                permission_mode,
                workspace: None,
                template_id: None,
                initial_soul: None,
            },
            slug: None,
            avatar_color,
            skills: Vec::new(),
            events: Vec::new(),
        };

        self.import_dialog.creating = true;
        self.error = None;
        self.notice = None;
        cx.notify();

        self.import_create_task = Some(cx.spawn(async move |this, cx| {
            let made = cx
                .background_executor()
                .spawn(async move {
                    let (mut bot, kickoff) = match job {
                        Ok(pick) => {
                            let res = call_checked(
                                &rpc,
                                &BotRpcRequest::ImportExternalBotCopy {
                                    source: Some(pick.app),
                                    path: pick.path,
                                    params: Box::new(params),
                                },
                            )?;
                            match res {
                                BotRpcResponse::Bot(b) => (*b, None),
                                other => {
                                    return Err(format!(
                                        "unexpected ImportExternalBotCopy response: {other:?}"
                                    ));
                                }
                            }
                        }
                        Err(kickoff) => {
                            let res =
                                call_checked(&rpc, &BotRpcRequest::CreateBotV2(Box::new(params)))?;
                            match res {
                                BotRpcResponse::Bot(b) => (*b, Some(kickoff)),
                                other => {
                                    return Err(format!(
                                        "unexpected CreateBotV2 response: {other:?}"
                                    ));
                                }
                            }
                        }
                    };
                    if let Some(png_bytes) = upload_png
                        && let Ok(BotRpcResponse::BotAvatar(_)) = call_checked(
                            &rpc,
                            &BotRpcRequest::UploadBotAvatar {
                                bot_id: bot.id.clone(),
                                png_bytes,
                            },
                        )
                    {
                        bot.avatar = Some(avatar::UPLOAD_AVATAR.to_owned());
                    }
                    Ok::<_, String>((bot, kickoff))
                })
                .await;

            this.update(cx, |this, cx| {
                this.import_dialog.creating = false;
                match made {
                    Ok((bot, kickoff)) => {
                        let id = bot.id.clone();
                        if host == this.host {
                            this.upsert_bot_config(bot);
                            this.emit_badge(cx);
                            this.selected_bot_id = Some(id.clone());
                        }
                        this.screen = super::Screen::Detail;
                        cx.emit(OpenBotHome {
                            bot_id: id,
                            kickoff,
                            host,
                        });
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn pick_export_folder(cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some(text::CHOOSE_FOLDER.get().into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await {
                this.update(cx, |this, cx| {
                    for p in paths {
                        if p.is_dir() && this.import_dialog.folder.is_none() {
                            this.import_dialog.folder = Some(p);
                        } else if !this.import_dialog.files.contains(&p) {
                            this.import_dialog.files.push(p);
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Orca's `ImportBotDialog`.
    pub(super) fn render_import_dialog(
        &mut self,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_runs_on_initialized(cx);
        self.ensure_import_dialog_inputs(window, cx);
        self.sync_import_name(window, cx);

        let exporting = self.import_exporting();
        let name = self.import_name();
        let trimmed = name.trim().to_owned();
        let pick_opt = (!exporting).then(|| self.import_pick().cloned()).flatten();
        let existing_match = pick_opt
            .as_ref()
            .and_then(|p| {
                find_imported_bot(
                    &self.import_dialog.host_bots,
                    &self.import_dialog.imported_sources,
                    &p.path,
                    self.import_dialog
                        .canonical_paths
                        .get(&p.path)
                        .map(String::as_str),
                )
            })
            .cloned();
        let again = if trimmed.is_empty() {
            existing_match
        } else {
            None
        };

        let export_path = self
            .import_dialog
            .folder
            .clone()
            .or_else(|| self.import_dialog.files.first().cloned());
        let suggestion = (exporting && self.import_dialog.typed.is_none())
            .then(|| export_path.as_deref().and_then(name_from_export_folder))
            .flatten();

        let (setup_local, setup_blocked, has_agents) = match &self.import_dialog.runs_on {
            Some(setup) => (
                setup.host_is_local,
                setup.is_blocked(),
                !available_runtimes(setup.host_is_local, cx).is_empty(),
            ),
            None => (self.host.is_local(), false, true),
        };
        let missing: Option<String> = if exporting && !setup_local {
            Some(text::EXPORT_NOT_ON_SERVER.get().to_owned())
        } else if exporting && export_path.is_none() {
            Some(text::ADD_FOLDER.get().to_owned())
        } else if !exporting && pick_opt.is_none() {
            None
        } else if trimmed.is_empty() && again.is_none() {
            Some(text::NAME_IT.get().to_owned())
        } else {
            None
        };

        let ready = missing.is_none()
            && !trimmed.is_empty()
            && if exporting {
                export_path.is_some()
            } else {
                pick_opt.is_some()
            };
        let can_create = ready && !self.import_dialog.creating && !setup_blocked && has_agents;

        let (old_initial, old_label) = if exporting {
            match &export_path {
                Some(path) => (
                    name_from_export_folder(path).map(|n| initial(&n)),
                    path.file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                ),
                None => (None, text::OLD_BOT.get().to_owned()),
            }
        } else {
            match &pick_opt {
                Some(c) => (
                    Some(initial(&c.name)),
                    bot_import_app_label(c.app).to_owned(),
                ),
                None => (None, text::OLD_BOT.get().to_owned()),
            }
        };

        let face_label = |label: String| {
            div()
                .h(px(16.))
                .max_w_full()
                .truncate()
                .text_size(ui::TEXT_XS)
                .line_height(px(16.))
                .text_color(p.muted_foreground)
                .child(label)
        };
        let old_face = match old_initial {
            Some(i) => div()
                .size(px(72.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(16.))
                .bg(p.muted)
                .text_size(ui::TEXT_2XL)
                .font_weight(FontWeight::SEMIBOLD)
                .child(i),
            None => div()
                .size(px(72.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(16.))
                .border_1()
                .border_dashed()
                .border_color(p.input)
                .text_size(ui::TEXT_2XL)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.muted_foreground)
                .child("?"),
        };

        let right_face = if let Some(ref bot) = again {
            let cached_img = self.settings.avatar_cache.get(&bot.id).cloned();
            div()
                .w(px(112.))
                .flex()
                .flex_col()
                .items_center()
                .gap_1p5()
                .child(avatar::render(
                    &avatar::Avatar::of(bot),
                    cached_img,
                    &bot.name,
                    px(96.),
                    px(16.),
                    p,
                ))
                .child(face_label(bot.name.clone()))
        } else {
            div()
                .w(px(112.))
                .flex()
                .flex_col()
                .items_center()
                .gap_1p5()
                .child(self.import_dialog.picture.render_import_face_trigger(
                    Self::import_picture_mut,
                    p,
                    cx,
                ))
                .child(face_label(text::NEW_BOT_LABEL.get().to_owned()))
        };

        let hero = div()
            .debug_selector(|| "bot-import-hero".into())
            .h(px(128.))
            .flex()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .w(px(112.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1p5()
                    .child(old_face)
                    .child(face_label(old_label)),
            )
            .child(
                div().mb_5().child(
                    Icon::new(IconName::ArrowRight)
                        .size(px(20.))
                        .text_color(p.muted_foreground),
                ),
            )
            .child(right_face);

        let again_hint = again.as_ref().map(|bot| {
            div()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(text::IMPORTED_BEFORE.format(&[("bot", &bot.name)]))
                .into_any_element()
        });
        let suggestion_row = render_bot_name_suggestion(
            "bot-import-suggestion",
            suggestion,
            again_hint,
            Self::take_name_suggestion,
            p,
            cx,
        );

        let name_input = self.import_dialog.name_input.clone();
        let name_field = div()
            .flex()
            .flex_col()
            .children(name_input.map(|i| {
                div()
                    .text_size(ui::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(Input::new(&i).large())
            }))
            .child(suggestion_row);

        let body: AnyElement = if exporting {
            self.render_import_export_steps(p, cx).into_any_element()
        } else {
            self.render_import_sources(p, cx).into_any_element()
        };

        let runs_on_row = if again.is_none()
            && let Some(setup) = self.import_dialog.runs_on.as_ref()
        {
            let taken: Vec<String> = self
                .import_dialog
                .host_bots
                .iter()
                .map(|b| b.name.clone())
                .collect();
            Some(render_runs_on_picker(
                "import-bot",
                RunsOnVariant::Row,
                setup,
                Some(&name),
                &taken,
                false,
                Self::import_runs_on_mut,
                Self::on_import_update_host,
                p,
                cx,
            ))
        } else {
            None
        };

        let back_to_pick = self.import_dialog.view == ImportView::Export
            && !self.import_dialog.candidates.is_empty();
        let footer = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(match missing {
                        Some(m) => div().child(m).into_any_element(),
                        None if again.is_some() => div().into_any_element(),
                        None => div()
                            .id("bot-import-keys-stay")
                            .child(text::KEYS_STAY.get())
                            .tooltip(|w, cx| {
                                ui::tooltip_view("bot-import-keys", text::NEVER_COPIED.get(), w, cx)
                            })
                            .into_any_element(),
                    }),
            )
            .child(
                ui::ghost(
                    "bot-import-cancel",
                    if back_to_pick {
                        text::BACK.get()
                    } else {
                        text::CANCEL.get()
                    },
                )
                .small()
                .disabled(self.import_dialog.creating)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if back_to_pick {
                        this.import_dialog.view = ImportView::Pick;
                        this.import_dialog.typed = None;
                        cx.notify();
                    } else {
                        this.import_dialog.picture.cancel_running_generation();
                        cx.emit(super::Close);
                    }
                })),
            )
            .child(if let Some(bot) = again {
                let bot_name = bot.name;
                ui::primary(
                    "bot-import-update",
                    text::UPDATE_BOT.format(&[("bot", &bot_name)]),
                )
                .debug_selector(|| "bot-import-update".into())
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.submit_import_or_update(cx)))
            } else {
                ui::primary(
                    "bot-import-create",
                    if self.import_dialog.creating {
                        text::CREATING.get().to_owned()
                    } else if trimmed.is_empty() {
                        text::CREATE.get().to_owned()
                    } else {
                        text::CREATE_NAMED.format(&[("name", &trimmed)])
                    },
                )
                .debug_selector(|| "bot-import-create".into())
                .small()
                .disabled(!can_create)
                .on_click(cx.listener(|this, _, _, cx| this.submit_import_or_update(cx)))
            });

        div()
            .id("bots-import-view")
            .debug_selector(|| "bots-import-view".into())
            .relative()
            .w_full()
            .max_h(px(760.))
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Paste, _, cx| {
                    if this
                        .import_dialog
                        .picture
                        .paste_image(Self::import_picture_mut, cx)
                    {
                        cx.stop_propagation();
                    }
                }),
            )
            .on_key_down(
                cx.listener(move |this, ev: &gpui::KeyDownEvent, window, cx| {
                    if ev.keystroke.key == "tab"
                        && !ev.keystroke.modifiers.shift
                        && this.import_dialog.typed.is_none()
                        && this.import_exporting()
                        && let Some(s) = this
                            .import_dialog
                            .folder
                            .clone()
                            .or_else(|| this.import_dialog.files.first().cloned())
                            .as_deref()
                            .and_then(name_from_export_folder)
                    {
                        this.take_name_suggestion(s, window, cx);
                        cx.stop_propagation();
                    }
                }),
            )
            .child(
                div().absolute().top_4().right_4().child(
                    Button::new("bot-import-close")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .accessibility_label(text::CLOSE.get())
                        .debug_selector(|| "bot-import-close".into())
                        .disabled(self.import_dialog.creating)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.import_dialog.creating {
                                this.import_dialog.picture.cancel_running_generation();
                                cx.emit(super::Close);
                            }
                        })),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .text_center()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text::TITLE.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(if exporting {
                                text::EXPORT_DESCRIPTION.get()
                            } else {
                                text::DESCRIPTION.get()
                            }),
                    ),
            )
            .child(
                div()
                    .id("bot-import-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(hero)
                    .child(name_field)
                    .children(self.error.clone().map(|e| ui::error_banner(p, e)))
                    .child(body)
                    .children(runs_on_row),
            )
            .child(footer)
    }

    /// Orca's `ImportBotSourceList`: "Bring over", the bots found, "Another app…" last.
    fn render_import_sources(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let cands = &self.import_dialog.candidates;
        let query = self
            .import_dialog
            .filter_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_lowercase())
            .unwrap_or_default();
        let picked = self.import_pick().map(pick_key);
        let row = |id: SharedString, debug_sel: String, on: bool| {
            div()
                .id(id)
                .debug_selector(move || debug_sel.clone())
                .role(gpui::Role::Button)
                .tab_index(0)
                .flex()
                .items_center()
                .gap_2p5()
                .rounded(ui::RADIUS_LG)
                .px_2()
                .py_1p5()
                .cursor_pointer()
                .when(on, |d| d.bg(p.accent))
                .when(!on, |d| d.hover(|d| d.bg(p.accent.opacity(0.5))))
        };
        let tile = |t: String| {
            div()
                .size(px(28.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(ui::RADIUS_LG)
                .bg(p.muted)
                .text_size(ui::TEXT_SM)
                .font_weight(FontWeight::SEMIBOLD)
                .child(t)
        };
        let words = |name: String, detail: String| {
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_size(ui::TEXT_SM)
                        .font_weight(FontWeight::MEDIUM)
                        .child(name),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(detail),
                )
        };
        let mut rows = div()
            .id("bot-import-rows")
            .max_h(px(300.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_0p5()
            .rounded(ui::RADIUS_LG)
            .border_1()
            .border_color(p.border)
            .p_1();
        for (idx, c) in cands
            .iter()
            .filter(|c| query.is_empty() || c.name.to_lowercase().contains(&query))
            .enumerate()
        {
            let key = pick_key(c);
            let on = picked.as_deref() == Some(key.as_str());
            let existing = find_imported_bot(
                &self.import_dialog.host_bots,
                &self.import_dialog.imported_sources,
                &c.path,
                self.import_dialog
                    .canonical_paths
                    .get(&c.path)
                    .map(String::as_str),
            );
            rows = rows.child(
                row(
                    format!("bot-import-pick-{idx}-{key}").into(),
                    format!("bot-import-pick-{}", c.name),
                    on,
                )
                .aria_selected(on)
                .child(tile(initial(&c.name)))
                .child(words(c.name.clone(), row_detail(c, existing)))
                .when(on, |d| {
                    d.child(
                        Icon::new(IconName::Check)
                            .size(px(16.))
                            .text_color(p.foreground),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.import_dialog.picked = Some(key.clone());
                    this.import_dialog.typed = None;
                    cx.notify();
                })),
            );
        }
        rows = rows.child(
            row(
                "bot-import-other-app".into(),
                "bot-import-other-app".to_owned(),
                false,
            )
            .child(
                div()
                    .size(px(28.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(ui::RADIUS_LG)
                    .border_1()
                    .border_dashed()
                    .border_color(p.input)
                    .child(
                        Icon::new(IconName::Plus)
                            .size(px(14.))
                            .text_color(p.muted_foreground),
                    ),
            )
            .child(words(
                text::OTHER_APP.get().to_owned(),
                text::OTHER_APP_NOTE.get().to_owned(),
            ))
            .child(
                Icon::new(IconName::ChevronRight)
                    .size(px(16.))
                    .text_color(p.muted_foreground),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.import_dialog.view = ImportView::Export;
                this.import_dialog.typed = None;
                cx.notify();
            })),
        );
        let count = if self.import_dialog.scanning {
            text::LOOKING.get().to_owned()
        } else {
            text::FOUND.format(&[("n", &cands.len().to_string())])
        };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .mb_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .font_weight(FontWeight::MEDIUM)
                            .child(text::BRING_OVER.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(count),
                    ),
            )
            .when(cands.len() > FILTER_FROM, |d| {
                d.children(self.import_dialog.filter_input.clone().map(|i| {
                    div()
                        .debug_selector(|| "bot-import-filter".into())
                        .mb_2()
                        .child(
                            Input::new(&i)
                                .small()
                                .prefix(Icon::new(IconName::Search).size(px(14.))),
                        )
                }))
            })
            .child(rows)
    }

    /// Orca's `ImportBotExportFields`: send the old bot the message, then add the folder it made
    /// (and its reply).
    fn render_import_export_steps(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let copied = self.import_dialog.copied;
        let chosen = self.import_dialog.folder.is_some() || !self.import_dialog.files.is_empty();
        let circle = |n: usize, done: bool| {
            div()
                .mt(px(6.))
                .size(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .text_size(ui::TEXT_2XS)
                .font_weight(FontWeight::SEMIBOLD)
                .when(done, |d| {
                    d.border_color(p.primary)
                        .bg(p.primary)
                        .text_color(p.primary_foreground)
                        .child(Icon::new(IconName::Check).size(px(12.)))
                })
                .when(!done, |d| {
                    d.border_color(p.border)
                        .text_color(p.muted_foreground)
                        .child(n.to_string())
                })
        };
        let head = |title: &'static str| {
            div()
                .min_h(px(32.))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_size(ui::TEXT_SM)
                        .font_weight(FontWeight::MEDIUM)
                        .child(title),
                )
        };
        let message_open = self.import_dialog.message_open;
        let step1 = div().flex().gap_2p5().child(circle(1, copied)).child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    head(text::SEND_STEP.get()).child(
                        Button::new("bot-import-copy")
                            .debug_selector(|| "bot-import-copy".into())
                            .xsmall()
                            .when(copied, gpui_component::button::ButtonVariants::ghost)
                            .when(!copied, gpui_component::button::Button::outline)
                            .label(if copied {
                                text::COPIED.get()
                            } else {
                                text::COPY_MESSAGE.get()
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    EXPORT_PROMPT.to_owned(),
                                ));
                                this.import_dialog.copied = true;
                                cx.notify();
                            })),
                    ),
                )
                .child(
                    div()
                        .id("bot-import-see-message")
                        .flex()
                        .items_center()
                        .gap_1()
                        .cursor_pointer()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .hover(|d| d.text_color(p.foreground))
                        .child(text::SEE_MESSAGE.get())
                        .child(
                            Icon::new(if message_open {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            })
                            .size(px(12.)),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.import_dialog.message_open = !this.import_dialog.message_open;
                            cx.notify();
                        })),
                )
                .when(message_open, |d| {
                    d.child(
                        div()
                            .rounded(ui::RADIUS_LG)
                            .border_1()
                            .border_color(p.border)
                            .px(px(10.))
                            .py_2()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(EXPORT_PROMPT),
                    )
                }),
        );
        let chip = |id: String, path: &std::path::Path, folder: bool, idx: Option<usize>| {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            div()
                .flex()
                .items_center()
                .gap_2()
                .rounded(ui::RADIUS_LG)
                .border_1()
                .border_color(p.border)
                .py_1()
                .pr_1()
                .pl(px(10.))
                .text_size(ui::TEXT_XS)
                .child(
                    Icon::new(if folder {
                        IconName::Folder
                    } else {
                        IconName::File
                    })
                    .size(px(14.))
                    .text_color(p.muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(path.display().to_string()),
                )
                .child(
                    Button::new(SharedString::from(id))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(IconName::X).size(px(12.)))
                        .accessibility_label(SharedString::from(
                            text::REMOVE_PATH.format(&[("name", &name)]),
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            match idx {
                                None => this.import_dialog.folder = None,
                                Some(i) if i < this.import_dialog.files.len() => {
                                    this.import_dialog.files.remove(i);
                                }
                                Some(_) => {}
                            }
                            cx.notify();
                        })),
                )
        };
        let mut drop_list = div()
            .id("bot-import-dropzone")
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                for p in paths.paths() {
                    if p.is_dir() && this.import_dialog.folder.is_none() {
                        this.import_dialog.folder = Some(p.clone());
                    } else if !this.import_dialog.files.contains(p) {
                        this.import_dialog.files.push(p.clone());
                    }
                }
                cx.notify();
            }))
            .flex()
            .flex_col()
            .gap_1p5();
        if let Some(f) = &self.import_dialog.folder {
            drop_list = drop_list.child(chip("bot-import-remove-folder".into(), f, true, None));
        }
        for (i, f) in self.import_dialog.files.iter().enumerate() {
            drop_list = drop_list.child(chip(
                format!("bot-import-remove-file-{i}"),
                f,
                false,
                Some(i),
            ));
        }
        if !chosen {
            drop_list = drop_list.child(
                div()
                    .rounded(ui::RADIUS_LG)
                    .border_1()
                    .border_dashed()
                    .border_color(p.input)
                    .px(px(10.))
                    .py(px(10.))
                    .flex()
                    .justify_center()
                    .gap_1()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::DROP_HINT.get())
                    .child(
                        div()
                            .id("bot-import-choose-folder")
                            .debug_selector(|| "bot-import-choose-folder".into())
                            .cursor_pointer()
                            .text_color(p.foreground)
                            .hover(gpui::Styled::underline)
                            .child(text::CHOOSE_FOLDER.get())
                            .on_click(cx.listener(|_, _, _, cx| Self::pick_export_folder(cx))),
                    ),
            );
        }
        let step2 = div().flex().gap_2p5().child(circle(2, chosen)).child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(head(text::ADD_STEP.get()))
                .child(drop_list)
                .when(chosen, |d| {
                    d.children(
                        self.import_dialog
                            .reply_input
                            .clone()
                            .map(|i| Textarea::new(&i).small()),
                    )
                }),
        );
        let none_found = self.import_dialog.view == ImportView::Pick;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .when(none_found, |d| {
                d.child(
                    div()
                        .text_center()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::NONE_FOUND.get()),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(step1)
                    .child(step2),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_folder_names_the_bot() {
        assert_eq!(
            name_from_export_folder(std::path::Path::new("/Users/me/Downloads/sage-export")),
            Some("Sage".to_owned())
        );
        assert_eq!(
            name_from_export_folder(std::path::Path::new("/x/research_desk")),
            Some("Research desk".to_owned())
        );
        let k = export_kickoff("Sage", std::path::Path::new("/d/sage-export"), &[], "hi");
        assert!(k.contains("/d/sage-export") && k.ends_with("hi"));
    }
}
