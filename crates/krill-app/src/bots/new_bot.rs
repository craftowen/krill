// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's "New bot" dialog: top color band, first-bot import nudge, 64px picture popover trigger +
//! large bare heading Name field, 5-line Identity box, 2-column Computer + Agent picker,
//! template part chips or editable proposal fields (`Watches`, `Does`, `Reports`, `Takes over`),
//! and Create (⌘↵).

use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, ExternalPaths, FocusHandle, FontWeight,
    KeyDownEvent, SharedString, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_component::ActiveTheme as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::popover::Popover;
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
#[cfg(test)]
use krill_proto::agent::Permission;
#[cfg(test)]
use krill_proto::bots::BotAgentModel;
#[cfg(test)]
use krill_proto::bots::ExternalBotCandidate;
use krill_proto::bots::{
    BOT_PROPOSAL_PURPOSE_MAX, BotConfig, BotImportNudgeInfo, BotProposal, BotRpcRequest,
    BotRpcResponse, BotTemplate, CreateBotParams, CreateBotV2Params, ExternalBotSourceApp,
    bot_import_nudge, find_bot_skill, proposal_identity, proposal_soul, template_proposal,
};

use super::avatar::{self, PictureError};
use super::pickers::{
    BotAgentSetup, PopoverTrigger, RunsOnVariant, available_runtimes, render_runs_on_picker,
};
use super::picture_picker::PictureState;
use super::rpc::call_checked;
use crate::hosts::HostId;
use crate::i18n::catalog::bots as text;
use crate::i18n::catalog::import_bot as itext;
use crate::ui;

pub use super::pickers::HostChoice;

const NAME_MAX_BYTES: usize = krill_proto::bots::BOT_NAME_MAX_BYTES;
const PURPOSE_MAX: usize = BOT_PROPOSAL_PURPOSE_MAX;
const PURPOSE_COUNTER_FROM: usize = 160;
const DIALOG_WIDTH: f32 = 512.;
const PROPOSAL_DIALOG_WIDTH: f32 = 576.;

#[derive(Debug, Clone)]
pub enum Event {
    Created {
        bot: Box<BotConfig>,
        warning: Option<String>,
        host: HostId,
    },
    Close,
    UpdateHost,
    BrowseTemplates,
    OpenImport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemplateChip {
    Apps,
    Skills,
    Schedules,
    Events,
}

type ChipContentFn =
    Box<dyn Fn(&NewBot, &ui::Palette, &mut Context<NewBot>) -> AnyElement + 'static>;

/// First free `"New bot"`, `"New bot 2"`, … (Orca fills an empty name the same way).
#[must_use]
pub fn unique_name(base: &str, taken: &[String]) -> String {
    let is_taken = |n: &str| taken.iter().any(|t| t.trim().eq_ignore_ascii_case(n));
    if !is_taken(base) {
        return base.to_owned();
    }
    (2..=taken.len() + 2)
        .map(|i| format!("{base} {i}"))
        .find(|n| !is_taken(n))
        .unwrap_or_else(|| base.to_owned())
}

pub struct NewBot {
    focus: FocusHandle,
    setup: BotAgentSetup,
    taken_names: Vec<String>,
    pub picture: PictureState,
    name: Entity<InputState>,
    purpose: Entity<TextareaState>,
    watches: Option<Entity<TextareaState>>,
    does: Option<Entity<TextareaState>>,
    reports: Option<Entity<TextareaState>>,
    template: Option<&'static BotTemplate>,
    proposal: Option<BotProposal>,
    prefilled_identity: Option<String>,
    origin: Option<String>,
    skills: Vec<&'static str>,
    open_chip: Option<TemplateChip>,
    nudge: Option<BotImportNudgeInfo>,
    nudge_dismissed: bool,
    nudge_task: Option<Task<()>>,
    submitting: bool,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl NewBot {
    pub fn new(
        hosts: Vec<HostChoice>,
        taken_names: Vec<String>,
        template: Option<&'static BotTemplate>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let proposal = template.map(template_proposal);
        let origin = template.map(|t| text::NB_FROM_TEMPLATE.format(&[("template", t.name)]));
        Self::new_with_proposal(hosts, taken_names, template, proposal, origin, window, cx)
    }

    pub fn new_with_proposal(
        hosts: Vec<HostChoice>,
        taken_names: Vec<String>,
        template: Option<&'static BotTemplate>,
        proposal: Option<BotProposal>,
        origin: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let setup = BotAgentSetup::from_hosts(hosts, Self::setup_mut, cx);
        let tpl_mascot = template
            .map(|t| t.mascot)
            .or_else(|| proposal.as_ref().and_then(|p| p.mascot.as_deref()))
            .and_then(avatar::mascot_id);
        let picture = PictureState::new("new-bot", tpl_mascot, cx);

        let prefilled_identity = proposal.as_ref().and_then(|p| {
            let _ = template?;
            let id_text = proposal_identity(
                p.watches.as_deref(),
                p.does.as_deref(),
                p.reports.as_deref(),
            );
            (!id_text.is_empty()).then_some(id_text)
        });
        let initial_name = proposal
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let initial_purpose = prefilled_identity
            .clone()
            .or_else(|| proposal.as_ref().map(|p| p.purpose.clone()))
            .unwrap_or_default();

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(text::NB_NAME_LABEL.get())
                .default_value(initial_name)
        });
        let purpose = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(5, 7)
                .placeholder(text::NB_IDENTITY_PLACEHOLDER.get())
                .default_value(initial_purpose)
        });

        let (watches, does, reports) = if template.is_none()
            && let Some(ref prop) = proposal
        {
            let mut mk = |val: Option<&str>, cx: &mut Context<Self>| {
                let s = val.unwrap_or("").to_owned();
                cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(1, 3)
                        .default_value(s)
                })
            };
            (
                Some(mk(prop.watches.as_deref(), cx)),
                Some(mk(prop.does.as_deref(), cx)),
                Some(mk(prop.reports.as_deref(), cx)),
            )
        } else {
            (None, None, None)
        };

        let mut subs = vec![
            cx.subscribe_in(&name, window, |this, inp, ev: &InputEvent, window, cx| {
                this.on_name_input(inp, ev, window, cx);
            }),
            cx.subscribe_in(
                &purpose,
                window,
                |this, inp, ev: &InputEvent, window, cx| {
                    this.on_purpose_input(inp, ev, window, cx);
                },
            ),
        ];
        for extra in [&watches, &does, &reports].into_iter().flatten() {
            let sub = cx.observe(extra, |_, _, cx| cx.notify());
            subs.push(sub);
        }

        let skills = template.map(|t| t.skills.to_vec()).unwrap_or_default();

        let first_bot = taken_names.is_empty();
        let nudge_dismissed = crate::settings::current(cx).bot_import_nudge_dismissed;

        let mut me = Self {
            focus: cx.focus_handle(),
            setup,
            taken_names,
            picture,
            name,
            purpose,
            watches,
            does,
            reports,
            template,
            proposal,
            prefilled_identity,
            origin,
            skills,
            open_chip: None,
            nudge: None,
            nudge_dismissed,
            nudge_task: None,
            submitting: false,
            error: None,
            task: None,
            _subs: subs,
        };

        if first_bot
            && !nudge_dismissed
            && let Some(local_rpc) = me
                .setup
                .hosts
                .iter()
                .find(|h| h.id.is_local())
                .map(|h| h.rpc.clone())
        {
            me.nudge_task = Some(cx.spawn(async move |this, cx| {
                let cands = cx
                    .background_executor()
                    .spawn(async move {
                        match call_checked(
                            &local_rpc,
                            &BotRpcRequest::ImportExternalBots {
                                source: None,
                                path: None,
                                dry_run: true,
                            },
                        ) {
                            Ok(BotRpcResponse::ImportedExternalBots { candidates, .. }) => {
                                candidates
                            }
                            _ => Vec::new(),
                        }
                    })
                    .await;
                this.update(cx, |this, cx| {
                    this.nudge = bot_import_nudge(&cands);
                    cx.notify();
                })
                .ok();
            }));
        }

        me
    }

    fn setup_mut(&mut self) -> &mut BotAgentSetup {
        &mut self.setup
    }

    pub(super) fn picture_mut(&mut self) -> &mut PictureState {
        &mut self.picture
    }

    fn on_update_host(_: &mut Self, cx: &mut Context<Self>) {
        cx.emit(Event::UpdateHost);
    }

    /// Computer the new bot is currently being made on.
    #[must_use]
    pub fn host(&self) -> &HostId {
        &self.setup.host_id
    }

    /// Picks `id` as target computer.
    pub fn select_host(&mut self, id: &HostId, cx: &mut Context<Self>) {
        self.error = None;
        self.setup.select_host(id, Self::setup_mut, cx);
    }

    #[must_use]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn focus_name(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |s, cx| s.focus(window, cx));
    }

    pub fn set_purpose(&mut self, purpose: String, window: &mut Window, cx: &mut Context<Self>) {
        self.purpose
            .update(cx, |s, cx| s.set_value(purpose, window, cx));
    }

    #[cfg(test)]
    pub fn set_nudge_candidates_for_test(
        &mut self,
        candidates: &[ExternalBotCandidate],
        cx: &mut Context<Self>,
    ) {
        self.nudge = bot_import_nudge(candidates);
        cx.notify();
    }

    #[cfg(test)]
    pub fn set_name_for_test(&mut self, val: &str, window: &mut Window, cx: &mut Context<Self>) {
        let owned = val.to_owned();
        self.name.update(cx, |s, cx| s.set_value(owned, window, cx));
    }

    #[cfg(test)]
    pub fn set_gen_description_for_test(
        &mut self,
        desc: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picture
            .set_gen_description_for_test(desc, Self::picture_mut, window, cx);
    }

    fn on_name_input(
        &mut self,
        input: &Entity<InputState>,
        ev: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            InputEvent::PressEnter { .. }
                if !crate::jump::entity_input_is_composing(input, window, cx) =>
            {
                if self.can_submit(cx) {
                    self.submit(cx);
                }
            }
            InputEvent::Change if !crate::jump::entity_input_is_composing(input, window, cx) => {
                let value = input.read(cx).value().to_string();
                let capped = clip_bytes(&value, NAME_MAX_BYTES);
                if capped.len() < value.len() {
                    let capped = capped.to_owned();
                    input.update(cx, |s, cx| s.set_value(capped, window, cx));
                }
                cx.notify();
            }
            InputEvent::Change => cx.notify(),
            _ => {}
        }
    }

    fn on_purpose_input(
        &mut self,
        input: &Entity<TextareaState>,
        ev: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let InputEvent::Change = ev {
            if !super::opt_textarea_composing(Some(input), window, cx)
                && self.prefilled_identity.is_none()
            {
                let value = input.read(cx).value().to_string();
                let capped = clip_chars(&value, PURPOSE_MAX);
                if capped.len() < value.len() {
                    let capped = capped.to_owned();
                    input.update(cx, |s, cx| s.set_value(capped, window, cx));
                }
            }
            cx.notify();
        }
    }

    fn input_is_composing(&self, window: &mut Window, cx: &mut App) -> bool {
        crate::jump::entity_input_is_composing(&self.name, window, cx)
            || super::opt_textarea_composing(Some(&self.purpose), window, cx)
            || super::opt_textarea_composing(self.watches.as_ref(), window, cx)
            || super::opt_textarea_composing(self.does.as_ref(), window, cx)
            || super::opt_textarea_composing(self.reports.as_ref(), window, cx)
    }

    fn identity_too_long(&self, cx: &App) -> bool {
        let Some(ref pre) = self.prefilled_identity else {
            return false;
        };
        let cur = self.purpose.read(cx).value().trim().to_owned();
        cur != *pre && cur.chars().count() > PURPOSE_MAX
    }

    fn can_submit(&self, cx: &App) -> bool {
        !self.submitting
            && !self.setup.is_blocked()
            && !available_runtimes(self.setup.host_is_local, cx).is_empty()
            && !self.identity_too_long(cx)
            && !self.name.read(cx).value().trim().is_empty()
    }

    pub fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        self.picture.step(delta, cx);
    }

    pub fn set_background(&mut self, rgb: u32, cx: &mut Context<Self>) {
        self.picture.set_background(rgb, cx);
    }

    pub fn load_picture(
        &mut self,
        read: impl FnOnce() -> Result<Vec<u8>, PictureError> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.picture.load_picture(read, Self::picture_mut, cx);
    }

    #[cfg(test)]
    pub(super) fn setup(&self) -> &BotAgentSetup {
        &self.setup
    }

    #[cfg(test)]
    pub(super) fn set_runtime(&mut self, runtime: &'static str, cx: &mut Context<Self>) {
        self.setup.set_runtime(runtime, Self::setup_mut, cx);
    }

    #[cfg(test)]
    pub(super) fn set_model(&mut self, model: Option<String>, cx: &mut Context<Self>) {
        self.setup.set_model(model, cx);
    }

    #[cfg(test)]
    pub(super) fn set_effort(&mut self, effort: Option<String>, cx: &mut Context<Self>) {
        self.setup.set_effort(effort, cx);
    }

    #[cfg(test)]
    pub(super) fn set_permission(&mut self, perm: Option<Permission>, cx: &mut Context<Self>) {
        self.setup.set_permission(perm, cx);
    }

    #[cfg(test)]
    pub(super) fn model_choices(&self) -> Vec<BotAgentModel> {
        self.setup.model_choices()
    }

    #[cfg(test)]
    pub(super) fn model_notice(&self) -> Option<SharedString> {
        self.setup.model_notice()
    }

    pub(super) fn toggle_skill(&mut self, skill: &'static str, cx: &mut Context<Self>) {
        if let Some(i) = self.skills.iter().position(|s| *s == skill) {
            self.skills.remove(i);
        } else {
            self.skills.push(skill);
        }
        cx.notify();
    }

    fn form_is_empty(&self, cx: &App) -> bool {
        self.template.is_none()
            && self.proposal.is_none()
            && self.name.read(cx).value().trim().is_empty()
            && self.purpose.read(cx).value().trim().is_empty()
    }

    fn effective_proposal(&self, name: &str, cx: &App) -> Option<BotProposal> {
        let prop = self.proposal.as_ref()?;
        let purpose = self.purpose.read(cx).value().trim().to_owned();
        if let Some(ref pre) = self.prefilled_identity {
            if purpose == *pre {
                return Some(BotProposal {
                    name: name.to_owned(),
                    ..prop.clone()
                });
            }
            return Some(BotProposal {
                name: name.to_owned(),
                purpose: if purpose.is_empty() {
                    prop.purpose.clone()
                } else {
                    purpose
                },
                watches: None,
                does: None,
                reports: None,
                automations: prop.automations.clone(),
                template_id: prop.template_id.clone(),
                mascot: prop.mascot.clone(),
                worktrees: prop.worktrees.clone(),
            });
        }
        let read_opt = |opt: &Option<Entity<TextareaState>>, fallback: &Option<String>| {
            opt.as_ref().map_or_else(
                || fallback.clone(),
                |e| {
                    let t = e.read(cx).value().trim().to_owned();
                    (!t.is_empty()).then_some(t)
                },
            )
        };
        Some(BotProposal {
            name: name.to_owned(),
            purpose: if purpose.is_empty() {
                prop.purpose.clone()
            } else {
                purpose
            },
            watches: read_opt(&self.watches, &prop.watches),
            does: read_opt(&self.does, &prop.does),
            reports: read_opt(&self.reports, &prop.reports),
            automations: prop.automations.clone(),
            template_id: prop.template_id.clone(),
            mascot: prop.mascot.clone(),
            worktrees: prop.worktrees.clone(),
        })
    }

    /// Builds the create request from the current dialog state.
    fn params(&self, cx: &App) -> CreateBotV2Params {
        let typed = clip_bytes(self.name.read(cx).value().trim(), NAME_MAX_BYTES).to_owned();
        let name = if typed.is_empty() {
            unique_name(text::NB_DEFAULT_NAME.get(), &self.taken_names)
        } else {
            typed
        };
        let (avatar_field, avatar_color, _) = self.picture.avatar_and_upload();
        let template = self.template;
        let made = self.effective_proposal(&name, cx);
        let (description, initial_soul) = if let Some(ref prop) = made {
            let soul = if let Some(tpl) = template
                && self
                    .prefilled_identity
                    .as_ref()
                    .is_some_and(|pre| self.purpose.read(cx).value().trim() == pre.as_str())
            {
                tpl.default_soul.to_owned()
            } else {
                proposal_soul(&name, prop)
            };
            (prop.purpose.clone(), Some(soul))
        } else {
            let raw_purpose = self.purpose.read(cx).value().trim().to_owned();
            (clip_chars(&raw_purpose, PURPOSE_MAX).to_owned(), None)
        };

        CreateBotV2Params {
            base: CreateBotParams {
                name,
                agent_id: self.setup.runtime.to_owned(),
                description,
                emoji: None,
                avatar: avatar_field,
                model: self.setup.model.clone(),
                effort: self.setup.effort.clone(),
                permission_mode: self.setup.permission_mode,
                workspace: None,
                template_id: template.map(|t| t.id.to_owned()),
                initial_soul,
            },
            slug: None,
            avatar_color,
            skills: self.skills.iter().map(|s| (*s).to_owned()).collect(),
            events: Vec::new(),
        }
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if self.submitting || self.setup.is_blocked() || self.identity_too_long(cx) {
            return;
        }
        let params = self.params(cx);
        let (_, _, picture) = self.picture.avatar_and_upload();
        let clear_skills =
            self.template.is_some_and(|t| !t.skills.is_empty()) && self.skills.is_empty();
        let rpc = self.setup.rpc.clone();
        let host = self.setup.host_id.clone();
        self.submitting = true;
        self.error = None;
        cx.notify();

        self.task = Some(cx.spawn(async move |this, cx| {
            let created = cx
                .background_executor()
                .spawn(async move {
                    let req = BotRpcRequest::CreateBotV2(Box::new(params));
                    let mut bot = match call_checked(&rpc, &req)? {
                        BotRpcResponse::Bot(bot) => bot,
                        other => return Err(format!("unexpected CreateBot response: {other:?}")),
                    };
                    let mut warning = None;
                    if clear_skills {
                        let req = BotRpcRequest::UpdateBotExtras(Box::new(
                            krill_proto::bots::UpdateBotExtrasParams {
                                bot_id: bot.id.clone(),
                                avatar_color: krill_proto::bots::FieldPatch::Unchanged,
                                skills: Some(Vec::new()),
                                events: None,
                            },
                        ));
                        match call_checked(&rpc, &req) {
                            Ok(BotRpcResponse::Bot(updated)) => bot = updated,
                            Ok(_) => bot.skills.clear(),
                            Err(err) => warning = Some(err),
                        }
                    }
                    if let Some(png_bytes) = picture {
                        let upload = BotRpcRequest::UploadBotAvatar {
                            bot_id: bot.id.clone(),
                            png_bytes,
                        };
                        match call_checked(&rpc, &upload) {
                            Ok(BotRpcResponse::BotAvatar(_)) => {
                                bot.avatar = Some(avatar::UPLOAD_AVATAR.to_owned());
                            }
                            Ok(other) => {
                                warning =
                                    Some(format!("unexpected UploadBotAvatar response: {other:?}"));
                            }
                            Err(err) => warning = Some(err),
                        }
                    }
                    Ok::<_, String>((bot, warning))
                })
                .await;
            this.update(cx, |this, cx| {
                this.submitting = false;
                match created {
                    Ok((bot, warning)) => cx.emit(Event::Created { bot, warning, host }),
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if !self.submitting {
            self.picture.cancel_running_generation();
            cx.emit(Event::Close);
        }
    }

    fn on_key(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.input_is_composing(window, cx) {
            return;
        }
        let keys = &e.keystroke;
        match keys.key.as_str() {
            "escape" if !keys.modifiers.modified() => {
                cx.stop_propagation();
                self.close(cx);
            }
            "enter"
                if keys.modifiers.secondary() && !keys.modifiers.shift && !keys.modifiers.alt =>
            {
                cx.stop_propagation();
                if self.can_submit(cx) {
                    self.submit(cx);
                }
            }
            _ => {}
        }
    }

    fn render_import_nudge(&self, p: &ui::Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.setup.host_is_local || self.nudge_dismissed {
            return None;
        }
        let info = self.nudge?;
        let app_icon_id = match info.app {
            ExternalBotSourceApp::OpenClaw => "openclaw",
            _ => "hermes",
        };
        let line = if info.count == 1 {
            itext::NUDGE_ONE.format(&[("app", info.label)])
        } else {
            itext::NUDGE_MANY.format(&[("count", &info.count.to_string()), ("app", info.label)])
        };
        let action_label = if info.count == 1 {
            itext::NUDGE_ACTION_ONE.get()
        } else {
            itext::NUDGE_ACTION_MANY.get()
        };

        Some(
            div()
                .id("new-bot-import-nudge")
                .debug_selector(|| "new-bot-import-nudge".into())
                .px_2p5()
                .py_2()
                .rounded(ui::RADIUS_MD)
                .border_1()
                .border_color(p.border)
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(ui::icons::agent_brand_icon(
                            app_icon_id,
                            ui::ICON_LG,
                            p.foreground,
                        ))
                        .child(
                            div()
                                .text_size(ui::TEXT_SM)
                                .text_color(p.foreground)
                                .child(line),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            ui::secondary("new-bot-import-nudge-action", action_label)
                                .xsmall()
                                .debug_selector(|| "new-bot-import-nudge-action".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.picture.cancel_running_generation();
                                    cx.emit(Event::OpenImport);
                                })),
                        )
                        .child(ui::button_with_tooltip(
                            Button::new("new-bot-import-nudge-dismiss")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::Close).size(px(ui::ICON_XS)))
                                .accessibility_label(itext::NUDGE_DISMISS.get())
                                .debug_selector(|| "new-bot-import-nudge-dismiss".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.nudge_dismissed = true;
                                    crate::settings::edit(cx, |s| {
                                        s.bot_import_nudge_dismissed = true;
                                    });
                                    cx.notify();
                                })),
                            "new-bot-import-nudge-dismiss",
                            itext::NUDGE_DISMISS.get(),
                        )),
                )
                .into_any_element(),
        )
    }

    fn render_takes_over(&self, p: &ui::Palette) -> Option<AnyElement> {
        let prop = self.proposal.as_ref()?;
        if prop.worktrees.is_empty() {
            return None;
        }
        let pills = prop.worktrees.iter().map(|entry| {
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_0p5()
                .rounded_full()
                .border_1()
                .border_color(p.border)
                .text_size(ui::TEXT_XS)
                .text_color(p.foreground)
                .child(ui::icons::svg_icon(
                    ui::icons::GIT_BRANCH_SVG,
                    ui::ICON_XS,
                    p.muted_foreground,
                ))
                .child(div().truncate().child(entry.name.clone()))
        });
        Some(
            div()
                .id("new-bot-takes-over")
                .debug_selector(|| "new-bot-takes-over".into())
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::NB_TAKES_OVER.get()),
                )
                .child(div().flex().flex_wrap().gap_1p5().children(pills))
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::NB_AGENTS_KEEP_RUNNING.get()),
                )
                .into_any_element(),
        )
    }

    fn render_template_parts(
        &self,
        tpl: &'static BotTemplate,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let apps_n = tpl.connectors.len();
        let skills_n = tpl.skills.len();
        let kept_n = self.skills.len();
        let sched_n = tpl.schedules.len();
        let events_n = tpl.events.len();

        let mut row = div()
            .id("new-bot-template-parts")
            .debug_selector(|| "new-bot-template-parts".into())
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5();

        let chip_btn = |id: &'static str,
                        chip: TemplateChip,
                        icon: AnyElement,
                        label: SharedString,
                        warning: bool,
                        muted_label: bool,
                        content: ChipContentFn| {
            let open = self.open_chip == Some(chip);
            let entity = cx.entity();
            Popover::new(SharedString::from(format!("{id}-popover")))
                .open(open)
                .appearance(false)
                .on_open_change(cx.listener(move |this, is_open: &bool, _, cx| {
                    this.open_chip = (*is_open).then_some(chip);
                    cx.notify();
                }))
                .trigger(PopoverTrigger(
                    div()
                        .id(id)
                        .debug_selector(move || id.to_owned())
                        .role(gpui::Role::Button)
                        .tab_index(0)
                        .h(px(26.))
                        .px_2p5()
                        .rounded_full()
                        .border_1()
                        .border_color(if warning {
                            p.warning.opacity(0.45)
                        } else {
                            p.border
                        })
                        .cursor_pointer()
                        .hover(|d| d.bg(p.accent.opacity(0.5)))
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(ui::TEXT_XS)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if warning {
                            p.warning
                        } else if muted_label {
                            p.muted_foreground
                        } else {
                            p.foreground
                        })
                        .child(icon)
                        .child(label),
                    open,
                ))
                .content(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        let palette = ui::palette(cx);
                        div()
                            .w(px(288.))
                            .p_2()
                            .bg(palette.popover)
                            .text_color(palette.popover_foreground)
                            .border_1()
                            .border_color(palette.border)
                            .rounded(ui::RADIUS_MD)
                            .shadow_lg()
                            .occlude()
                            .child(content(this, palette, cx))
                            .into_any_element()
                    })
                })
        };

        if apps_n > 0 || tpl.needs_setup.is_some() {
            let label: SharedString = if apps_n == 1 {
                text::NB_TPL_PART_APPS_ONE.get().into()
            } else {
                text::NB_TPL_PART_APPS_MANY
                    .format(&[("n", &apps_n.to_string())])
                    .into()
            };
            let warn = tpl.needs_setup.is_some();
            row = row.child(chip_btn(
                "new-bot-chip-apps",
                TemplateChip::Apps,
                ui::icons::svg_icon(
                    ui::icons::PLUG_SVG,
                    ui::ICON_MD,
                    if warn { p.warning } else { p.foreground },
                )
                .into_any_element(),
                label,
                warn,
                false,
                Box::new(move |this, p, _| {
                    let mut list = div().flex().flex_col().gap_1();
                    for c in tpl.connectors {
                        let state = div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::NB_TPL_PICKS_IN_CHAT.get())
                            .into_any_element();
                        list = list.child(template_part_row(
                            ui::icons::svg_icon(
                                ui::icons::PLUG_SVG,
                                ui::ICON_MD,
                                p.muted_foreground,
                            )
                            .into_any_element(),
                            super::View::source_label(*c).to_owned(),
                            None,
                            Some(state),
                            p,
                        ));
                    }
                    if let Some(note) = tpl.needs_setup {
                        list = list.child(template_part_row(
                            Icon::new(IconName::CircleAlert)
                                .size(px(ui::ICON_MD))
                                .text_color(p.warning)
                                .into_any_element(),
                            text::NB_TPL_MANUAL_SETUP.get().to_owned(),
                            Some(note.to_owned()),
                            None,
                            p,
                        ));
                    }
                    let _ = this;
                    list.into_any_element()
                }),
            ));
        }

        if skills_n > 0 {
            let label: SharedString = if kept_n == skills_n {
                if skills_n == 1 {
                    text::NB_TPL_PART_SKILLS_ONE.get().into()
                } else {
                    text::NB_TPL_PART_SKILLS_MANY
                        .format(&[("n", &skills_n.to_string())])
                        .into()
                }
            } else {
                text::NB_TPL_PART_SKILLS_PARTIAL
                    .format(&[
                        ("on", &kept_n.to_string()),
                        ("total", &skills_n.to_string()),
                    ])
                    .into()
            };
            row = row.child(chip_btn(
                "new-bot-chip-skills",
                TemplateChip::Skills,
                ui::icons::svg_icon(ui::icons::BOOK_OPEN_SVG, ui::ICON_MD, p.foreground)
                    .into_any_element(),
                label,
                false,
                kept_n < skills_n,
                Box::new(move |this, p, cx| {
                    let mut list = div().flex().flex_col().gap_1();
                    for skill_id in tpl.skills {
                        let checked = this.skills.contains(skill_id);
                        let desc = find_bot_skill(skill_id).map(|s| s.description.to_owned());
                        let sid = *skill_id;
                        let view = cx.weak_entity();
                        let toggle = ui::switch(
                            p,
                            SharedString::from(format!("new-bot-skill-switch-{sid}")),
                            checked,
                            false,
                            move |_, _, cx| {
                                view.update(cx, |this, cx| this.toggle_skill(sid, cx)).ok();
                            },
                        )
                        .into_any_element();
                        list = list.child(template_part_row(
                            ui::icons::svg_icon(
                                ui::icons::BOOK_OPEN_SVG,
                                ui::ICON_MD,
                                p.muted_foreground,
                            )
                            .into_any_element(),
                            (*skill_id).to_owned(),
                            desc,
                            Some(toggle),
                            p,
                        ));
                    }
                    list.into_any_element()
                }),
            ));
        }

        if sched_n > 0 {
            let label: SharedString = if sched_n == 1 {
                text::NB_TPL_PART_ROUTINES_ONE.get().into()
            } else {
                text::NB_TPL_PART_ROUTINES_MANY
                    .format(&[("n", &sched_n.to_string())])
                    .into()
            };
            row = row.child(chip_btn(
                "new-bot-chip-schedules",
                TemplateChip::Schedules,
                ui::icons::svg_icon(ui::icons::CLOCK_SVG, ui::ICON_MD, p.foreground)
                    .into_any_element(),
                label,
                false,
                false,
                Box::new(move |_, p, _| {
                    let mut list = div().flex().flex_col().gap_1();
                    for s in tpl.schedules {
                        let st = div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::NB_TPL_STARTS_OFF.get())
                            .into_any_element();
                        list = list.child(template_part_row(
                            ui::icons::svg_icon(
                                ui::icons::CLOCK_SVG,
                                ui::ICON_MD,
                                p.muted_foreground,
                            )
                            .into_any_element(),
                            s.what.to_owned(),
                            Some(s.when.to_owned()),
                            Some(st),
                            p,
                        ));
                    }
                    list.into_any_element()
                }),
            ));
        }

        if events_n > 0 {
            let label: SharedString = if events_n == 1 {
                text::NB_TPL_PART_EVENTS_ONE.get().into()
            } else {
                text::NB_TPL_PART_EVENTS_MANY
                    .format(&[("n", &events_n.to_string())])
                    .into()
            };
            row = row.child(chip_btn(
                "new-bot-chip-events",
                TemplateChip::Events,
                ui::icons::svg_icon(ui::icons::ZAP_SVG, ui::ICON_MD, p.foreground)
                    .into_any_element(),
                label,
                false,
                false,
                Box::new(move |_, p, _| {
                    let mut list = div().flex().flex_col().gap_1();
                    for ev in tpl.events {
                        let st = div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::NB_TPL_PICKS_IN_CHAT.get())
                            .into_any_element();
                        list = list.child(template_part_row(
                            ui::icons::svg_icon(
                                ui::icons::ZAP_SVG,
                                ui::ICON_MD,
                                p.muted_foreground,
                            )
                            .into_any_element(),
                            ev.name.to_owned(),
                            None,
                            Some(st),
                            p,
                        ));
                    }
                    list.into_any_element()
                }),
            ));
        }

        row
    }

    fn render_proposal_fields(&self, p: &ui::Palette) -> Option<AnyElement> {
        if self.template.is_some() {
            return None;
        }
        let prop = self.proposal.as_ref()?;
        let row =
            |id: &'static str, label: &'static str, entity: Option<&Entity<TextareaState>>| {
                entity.map(|inp| {
                    div()
                        .id(id)
                        .debug_selector(move || id.to_owned())
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(label),
                        )
                        .child(Textarea::new(inp).appearance(false))
                })
            };
        let has_any = self.watches.is_some()
            || self.does.is_some()
            || self.reports.is_some()
            || !prop.worktrees.is_empty();
        if !has_any {
            return None;
        }

        Some(
            div()
                .id("new-bot-proposal-fields")
                .debug_selector(|| "new-bot-proposal-fields".into())
                .pt_4()
                .border_t_1()
                .border_color(p.border)
                .flex()
                .flex_col()
                .gap_4()
                .children(row(
                    "new-bot-watches",
                    text::NB_WATCHES.get(),
                    self.watches.as_ref(),
                ))
                .children(row("new-bot-does", text::NB_DOES.get(), self.does.as_ref()))
                .children(row(
                    "new-bot-reports",
                    text::NB_REPORTS.get(),
                    self.reports.as_ref(),
                ))
                .children(self.render_takes_over(p))
                .into_any_element(),
        )
    }

    fn render_footer(&self, p: &ui::Palette, cx: &mut Context<Self>) -> gpui::Div {
        let show_templates = self.form_is_empty(cx);
        let can_submit = self.can_submit(cx);
        let chord = if cfg!(target_os = "macos") {
            "⌘ ↵"
        } else {
            "Ctrl ↵"
        };
        div()
            .pt_1()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .when(show_templates, |d| {
                d.child(
                    div()
                        .id("new-bot-start-from-template")
                        .debug_selector(|| "new-bot-start-from-template".into())
                        .role(gpui::Role::Button)
                        .tab_index(0)
                        .focus_visible(|d| d.border_1().border_color(p.ring))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .text_size(ui::TEXT_SM)
                        .text_color(p.muted_foreground)
                        .hover(|d| d.text_color(p.foreground))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.picture.cancel_running_generation();
                            cx.emit(Event::BrowseTemplates);
                        }))
                        .child(ui::icons::svg_icon(
                            ui::icons::LAYOUT_TEMPLATE_SVG,
                            ui::ICON_MD,
                            p.muted_foreground,
                        ))
                        .child(text::NB_TEMPLATE_SHORT.get()),
                )
            })
            .child(
                div().ml_auto().child(
                    ui::primary("new-bot-create", text::NB_CREATE_SHORT.get())
                        .debug_selector(|| "new-bot-create".into())
                        .disabled(!can_submit)
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx)))
                        .child(
                            div()
                                .ml_1()
                                .px_1p5()
                                .py_0p5()
                                .rounded(ui::RADIUS_XS)
                                .border_1()
                                .border_color(p.primary_foreground.opacity(0.2))
                                .text_size(ui::TEXT_2XS)
                                .font_weight(FontWeight::MEDIUM)
                                .child(chord),
                        ),
                ),
            )
    }
}

/// Longest prefix of `s` within `max` bytes ending on a char boundary.
fn clip_bytes(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or(s)
}

/// First `max` characters of `s`.
fn clip_chars(s: &str, max: usize) -> &str {
    s.char_indices()
        .nth(max)
        .and_then(|(i, _)| s.get(..i))
        .unwrap_or(s)
}

impl Render for NewBot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = ui::palette(cx);
        let dark = cx.theme().mode.is_dark();
        let viewport = window.viewport_size();
        let width = if self.proposal.is_some() && self.template.is_none() {
            PROPOSAL_DIALOG_WIDTH
        } else {
            DIALOG_WIDTH
        };

        let purpose_text = self.purpose.read(cx).value().to_string();
        let purpose_len = purpose_text.chars().count();
        let show_counter = purpose_len >= PURPOSE_COUNTER_FROM
            && self
                .prefilled_identity
                .as_deref()
                .is_none_or(|pre| purpose_text.trim() != pre);
        let too_long = self.identity_too_long(cx);

        let name_row = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                self.picture
                    .render_new_bot_trigger(Self::picture_mut, p, cx),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(ui::TEXT_2XL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(Input::new(&self.name).appearance(false)),
                    )
                    .children(self.picture.intake_error().map(|err| {
                        div()
                            .mt_1()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.destructive)
                            .child(
                                Icon::new(IconName::CircleAlert)
                                    .size(px(ui::ICON_MD))
                                    .text_color(p.destructive),
                            )
                            .child(err.message())
                    })),
            );

        let identity_box = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(p.border)
                    .bg(p.background)
                    .px_3()
                    .py_2()
                    .text_size(ui::TEXT_SM)
                    .child(Textarea::new(&self.purpose).appearance(false)),
            )
            .when(show_counter, |d| {
                d.child(
                    div()
                        .text_right()
                        .text_size(ui::TEXT_XS)
                        .text_color(if too_long {
                            p.destructive
                        } else {
                            p.muted_foreground
                        })
                        .child(format!("{purpose_len}/{PURPOSE_MAX}")),
                )
            });

        let taken = self.taken_names.clone();
        let runs_on = render_runs_on_picker(
            "new-bot",
            RunsOnVariant::Fields,
            &self.setup,
            None,
            &taken,
            Self::setup_mut,
            Self::on_update_host,
            p,
            cx,
        );

        let body = div()
            .id("new-bot-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .children(self.render_import_nudge(p, cx))
            .child(
                div()
                    .pt_1()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(name_row)
                    .child(identity_box),
            )
            .child(runs_on)
            .when_some(self.template, |d, tpl| {
                d.children(self.render_takes_over(p))
                    .child(self.render_template_parts(tpl, p, cx))
            })
            .children(self.render_proposal_fields(p))
            .children(self.error.clone().map(|e| ui::error_banner(p, e)));

        let band = self.picture.band_color();
        let card = div()
            .id("new-bot-dialog")
            .debug_selector(|| "new-bot-dialog".into())
            .role(gpui::Role::Dialog)
            .aria_label(text::NB_TITLE.get())
            .occlude()
            .relative()
            .overflow_hidden()
            .w(px(width))
            .max_w((viewport.width - ui::SPACE_4 * 2.).max(px(0.)))
            .max_h((viewport.height - ui::SPACE_4 * 2.).max(px(0.)))
            .p_5()
            .rounded(ui::RADIUS_LG)
            .bg(p.popover)
            .text_color(p.popover_foreground)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_MD)
            .border_1()
            .border_color(p.border)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_4()
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Paste, _, cx| {
                    if this.picture.paste_image(Self::picture_mut, cx) {
                        cx.stop_propagation();
                    }
                }),
            )
            .capture_action(
                cx.listener(|this, _: &gpui_component::input::Escape, window, cx| {
                    if this.input_is_composing(window, cx) {
                        window.prevent_default();
                    }
                }),
            )
            .on_key_down(
                cx.listener(|this, e: &KeyDownEvent, window, cx| this.on_key(e, window, cx)),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.picture.drop_paths(paths, Self::picture_mut, cx);
            }))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(band.map(|rgb| {
                div()
                    .id("new-bot-band")
                    .debug_selector(|| "new-bot-band".into())
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(84.))
                    .bg(ui::new_bot_band_fill(rgb, dark))
            }))
            .child(
                div().absolute().top_4().right_4().child(
                    Button::new("new-bot-close")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .accessibility_label(text::CLOSE.get())
                        .debug_selector(|| "new-bot-close".into())
                        .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_XL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(text::NB_TITLE.get()),
                    )
                    .children(self.origin.clone().map(|orig| {
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(orig)
                    })),
            )
            .child(body)
            .child(self.render_footer(p, cx));

        ui::modal_backdrop(
            viewport,
            div()
                .id("new-bot-backdrop")
                .debug_selector(|| "new-bot-backdrop".into())
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.close(cx)),
                )
                .child(ui::modal_focus_scope(card, &self.focus)),
        )
    }
}

impl EventEmitter<Event> for NewBot {}

impl std::fmt::Debug for NewBot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewBot")
            .field("submitting", &self.submitting)
            .field("has_template", &self.template.is_some())
            .field("has_proposal", &self.proposal.is_some())
            .finish_non_exhaustive()
    }
}

fn template_part_row(
    icon: AnyElement,
    title: String,
    subtitle: Option<String>,
    right: Option<AnyElement>,
    p: &ui::Palette,
) -> gpui::Div {
    div()
        .px_2()
        .py_1p5()
        .rounded(ui::RADIUS_XS)
        .flex()
        .items_start()
        .gap_2p5()
        .child(div().mt(px(2.)).flex_none().child(icon))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(ui::TEXT_SM)
                        .text_color(p.foreground)
                        .child(title),
                )
                .children(subtitle.map(|s| {
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(s)
                })),
        )
        .children(right)
}

impl gpui::Focusable for NewBot {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_keep_to_krillds_byte_cap_on_character_boundaries() {
        let korean = "가".repeat(30);
        let clipped = clip_bytes(&korean, NAME_MAX_BYTES);
        assert_eq!(clipped.chars().count(), 21);
        assert!(clipped.len() <= NAME_MAX_BYTES);
        assert_eq!(clip_chars("abcdef", 3), "abc");
        assert_eq!(clip_chars("ab", 3), "ab");
    }

    #[test]
    fn empty_names_fill_in_like_orca() {
        assert_eq!(unique_name("New bot", &[]), "New bot");
        let taken = vec!["new bot".to_owned(), "New bot 2".to_owned()];
        assert_eq!(unique_name("New bot", &taken), "New bot 3");
    }
}
