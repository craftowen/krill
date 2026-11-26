// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License):
// src/renderer/src/components/bots/settings-view/BotSettingsRoot.tsx, BotSettingsRows.tsx,
// bot-settings-place.ts, BotItemsSettingsGroup.tsx and bot-settings-row-states.ts.
//! Bot settings as Orca draws them in the right panel: the bot's card, then inset groups of
//! rows (Agent, Schedule, Items, Soul and memory, Access, Sharing, Notifications), then Delete.
//! A row's text opens its own subpage; a switch or picker at its end acts at once.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, Context, FontWeight, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::agent::Permission;
use krill_proto::bots::{
    BotAccessRequestStatus, BotConfig, BotId, BotItemsStale, BotMember, BotRpcRequest,
    BotRpcResponse, BotSettingsExtras, ExternalSource, FieldPatch, LookAroundEvery, TidyEvery,
    UpdateBotParams,
};

use crate::i18n::catalog::bots as text;
use crate::i18n::catalog::chat as chat_text;
use crate::ui;

use super::rpc::{call_checked, runtime_display_label};
use super::{DetailTab, View, avatar};

/// Request the host `Shell` to show another bot page in the panel (Connectors is its own tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenSettingsPage(pub DetailTab);

/// Request the host `Shell` to confirm deleting a bot (Orca's Delete bot… at the list's end).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestDeleteBot(pub krill_proto::bots::BotId);

/// Orca's `BotSettingsPage`: one drill-in of the settings view; the root has none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum SettingsPage {
    #[default]
    Root,
    Agent,
    Permissions,
    Computer,
    LookAround,
    TidyUp,
    /// Krill's own bot schedules (Orca keeps them on the Automations page).
    Schedules,
    Soul,
    Memory,
    WhoCanAsk,
    Tools,
    Skills,
    Sharing,
}

impl SettingsPage {
    /// The crumb's last part, as Orca's `botSettingsPageTitle`.
    pub(super) fn title(self) -> Option<&'static str> {
        Some(match self {
            Self::Root => return None,
            Self::Agent => text::SV_AGENT_MODEL.get(),
            Self::Permissions => text::SV_PERMISSIONS.get(),
            Self::Computer => text::SV_COMPUTER.get(),
            Self::LookAround => text::SV_LOOK_AROUND.get(),
            Self::TidyUp => text::SV_MEMORY_UPKEEP.get(),
            Self::Schedules => text::SV_SCHEDULES.get(),
            Self::Soul => text::SV_SOUL.get(),
            Self::Memory => text::SV_MEMORY.get(),
            Self::WhoCanAsk => text::SV_WHO_CAN_ASK.get(),
            Self::Tools => text::SV_TOOLS.get(),
            Self::Skills => text::SV_SKILLS.get(),
            Self::Sharing => text::SV_SHARING.get(),
        })
    }
}

type RowAction = Rc<dyn Fn(&mut View, &mut Window, &mut Context<View>)>;

/// What a row's text does when clicked.
#[derive(Clone)]
pub(super) enum Open {
    Page(SettingsPage),
    Tab(DetailTab),
    Run(RowAction),
}

impl Open {
    pub(super) fn run(f: impl Fn(&mut View, &mut Window, &mut Context<View>) + 'static) -> Self {
        Self::Run(Rc::new(f))
    }
}

/// A change to the bot's presets; `None` leaves a field as it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Extras {
    pub items_stale: Option<BotItemsStale>,
    pub look_around_every: Option<LookAroundEvery>,
    pub tidy_every: Option<TidyEvery>,
}

/// Orca's `SettingsInsetRow`.
pub(super) struct Row {
    id: SharedString,
    icon: Option<&'static [u8]>,
    icon_name: Option<IconName>,
    title: SharedString,
    detail: Option<SharedString>,
    wrap_detail: bool,
    value: Option<SharedString>,
    end: Option<AnyElement>,
    open: Option<Open>,
}

impl Row {
    pub(super) fn new(
        id: impl Into<SharedString>,
        icon: Option<&'static [u8]>,
        title: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            icon_name: None,
            title: title.into(),
            detail: None,
            wrap_detail: false,
            value: None,
            end: None,
            open: None,
        }
    }

    pub(super) fn icon_name(mut self, icon: IconName) -> Self {
        self.icon_name = Some(icon);
        self
    }

    pub(super) fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        let detail = detail.into();
        if !detail.trim().is_empty() {
            self.detail = Some(detail);
        }
        self
    }

    pub(super) const fn wrap_detail(mut self) -> Self {
        self.wrap_detail = true;
        self
    }

    pub(super) fn value(mut self, value: Option<impl Into<SharedString>>) -> Self {
        self.value = value.map(Into::into);
        self
    }

    pub(super) fn end(mut self, end: impl gpui::IntoElement) -> Self {
        self.end = Some(end.into_any_element());
        self
    }

    pub(super) fn opens(mut self, open: Open) -> Self {
        self.open = Some(open);
        self
    }

    const fn has_icon(&self) -> bool {
        self.icon.is_some() || self.icon_name.is_some()
    }
}

/// A standard switch for subpage rows (32×18px track, 14px thumb).
pub(super) fn row_switch(
    p: &ui::Palette,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    on: bool,
    disabled: bool,
    toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    ui::switch(p, id, on, disabled, toggle).aria_label(label.into())
}

/// A compact switch for Settings root rows (24×14px track, 10px thumb — Orca `BotSettingsSwitch`).
pub(super) fn row_switch_compact(
    p: &ui::Palette,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    on: bool,
    toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    ui::switch_compact(p, id, on, false, toggle).aria_label(label.into())
}

/// Orca's `xs` select trigger: the value, a caret, and a checked menu of `choices`.
pub(super) fn row_select<T: Copy + PartialEq + 'static>(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    current: T,
    choices: Vec<(T, SharedString)>,
    style: impl FnOnce(Button) -> Button,
    pick: impl Fn(T, &mut Window, &mut App) + 'static,
) -> impl gpui::IntoElement {
    let shown = choices
        .iter()
        .find(|(c, _)| *c == current)
        .map(|(_, l)| l.clone())
        .unwrap_or_default();
    let pick = Rc::new(pick);
    style(
        Button::new(id)
            .outline()
            .xsmall()
            .dropdown_caret(true)
            .accessibility_label(label.into())
            .label(shown),
    )
    .dropdown_menu(move |mut menu, _, _| {
        for (choice, name) in &choices {
            let pick = pick.clone();
            let choice = *choice;
            menu = menu.item(
                PopupMenuItem::new(name.clone())
                    .checked(choice == current)
                    .on_click(move |_, window, cx| pick(choice, window, cx)),
            );
        }
        menu
    })
}

pub(super) fn patch(bot: &BotConfig) -> UpdateBotParams {
    UpdateBotParams {
        bot_id: bot.id.clone(),
        name: None,
        description: None,
        emoji: FieldPatch::Unchanged,
        avatar: FieldPatch::Unchanged,
        agent_id: None,
        model: FieldPatch::Unchanged,
        effort: FieldPatch::Unchanged,
        permission_mode: None,
        workspace: FieldPatch::Unchanged,
        paused: None,
        first_run_completed: None,
        notifications: None,
        schedules: None,
        look_around: None,
        memory_tidy: None,
        mcp_tools: None,
    }
}

/// A count, or "None" (Orca's `countValue`).
pub(super) fn count(n: usize) -> String {
    if n == 0 {
        text::SV_NONE.get().to_owned()
    } else {
        n.to_string()
    }
}

/// "4h" while Look around is on (Orca's `lookAroundValue`).
pub(super) fn look_around_short(every: LookAroundEvery) -> &'static str {
    match every {
        LookAroundEvery::Every4h => text::SV_LOOK_SHORT_4H.get(),
        LookAroundEvery::Every2h => text::SV_LOOK_SHORT_2H.get(),
        LookAroundEvery::TwiceDaily => text::SV_LOOK_SHORT_TWICE.get(),
        LookAroundEvery::Mornings => text::SV_LOOK_SHORT_MORNINGS.get(),
    }
}

/// "Nightly" or "Weekly" while Memory upkeep is on (Orca's `tidyUpValue`).
pub(super) fn tidy_short(every: TidyEvery) -> &'static str {
    match every {
        TidyEvery::Night => text::SV_TIDY_SHORT_NIGHT.get(),
        TidyEvery::Week => text::SV_TIDY_SHORT_WEEK.get(),
    }
}

/// "3 days" … "Never" (Orca's Mark items stale after choices).
pub(super) fn stale_label(stale: BotItemsStale) -> String {
    stale.days().map_or_else(
        || text::SV_STALE_NEVER.get().to_owned(),
        |d| text::SV_STALE_DAYS.format(&[("days", &d.to_string())]),
    )
}

/// The bot's computer as Settings names it: this Mac, or the paired host's label.
pub(super) fn computer_name(view: &View) -> String {
    if view.host.is_local() {
        text::HOST_LOCAL
            .mac_or(crate::i18n::catalog::other_os::HOST_LOCAL)
            .get()
            .to_owned()
    } else {
        view.available_hosts
            .iter()
            .find(|(id, _, _)| id == &view.host)
            .map_or_else(|| "?".to_owned(), |(_, label, _)| label.clone())
    }
}

/// A settings mutation queued behind an in-flight `patch_task` so rapid toggles serialize
/// instead of overwriting and dropping the previous RPC.
pub(super) enum PendingPatch {
    UpdateBot {
        req_host: crate::hosts::HostId,
        params: Box<UpdateBotParams>,
        prev_bot: Option<Box<BotConfig>>,
    },
    RemoveMembers {
        req_host: crate::hosts::HostId,
        bot_id: BotId,
        members: Vec<BotMember>,
        prev_bot: Option<Box<BotConfig>>,
    },
}

fn apply_field_patch(slot: &mut Option<String>, patch: &FieldPatch<String>) {
    match patch {
        FieldPatch::Unchanged => {}
        FieldPatch::Clear => *slot = None,
        FieldPatch::Set(v) => *slot = Some(v.clone()),
    }
}

fn apply_update_bot_params(bot: &mut BotConfig, params: &UpdateBotParams) {
    if let Some(name) = &params.name {
        bot.name.clone_from(name);
    }
    if let Some(description) = &params.description {
        bot.description.clone_from(description);
    }
    apply_field_patch(&mut bot.emoji, &params.emoji);
    apply_field_patch(&mut bot.avatar, &params.avatar);
    if let Some(agent_id) = &params.agent_id {
        bot.agent_id.clone_from(agent_id);
    }
    apply_field_patch(&mut bot.model, &params.model);
    apply_field_patch(&mut bot.effort, &params.effort);
    if let Some(permission_mode) = params.permission_mode {
        bot.permission_mode = permission_mode;
    }
    apply_field_patch(&mut bot.workspace, &params.workspace);
    if let Some(paused) = params.paused {
        bot.paused = paused;
    }
    if let Some(first_run_completed) = params.first_run_completed {
        bot.first_run_completed = first_run_completed;
    }
    if let Some(notifications) = params.notifications {
        bot.notifications = notifications;
    }
    if let Some(schedules) = &params.schedules {
        bot.schedules.clone_from(schedules);
    }
    if let Some(look_around) = &params.look_around {
        bot.look_around.clone_from(look_around);
    }
    if let Some(memory_tidy) = params.memory_tidy {
        bot.memory_tidy = memory_tidy;
    }
    if let Some(mcp_tools) = &params.mcp_tools {
        bot.mcp_tools.clone_from(mcp_tools);
    }
}

impl PendingPatch {
    const fn req_host(&self) -> &crate::hosts::HostId {
        match self {
            Self::UpdateBot { req_host, .. } | Self::RemoveMembers { req_host, .. } => req_host,
        }
    }

    fn bot_id(&self) -> &BotId {
        match self {
            Self::UpdateBot { params, .. } => &params.bot_id,
            Self::RemoveMembers { bot_id, .. } => bot_id,
        }
    }

    fn set_prev_bot(&mut self, bot: BotConfig) {
        match self {
            Self::UpdateBot { prev_bot, .. } | Self::RemoveMembers { prev_bot, .. } => {
                *prev_bot = Some(Box::new(bot));
            }
        }
    }

    fn apply_to(&self, bot: &mut BotConfig) {
        match self {
            Self::UpdateBot { params, .. } => apply_update_bot_params(bot, params),
            Self::RemoveMembers { members, .. } => {
                bot.members.retain(|m| !members.contains(m));
            }
        }
    }
}

impl View {
    /// Applies one change from a settings row and shows the bot as krilld returns it.
    /// Rapid changes serialize through `settings.patch_queue` with optimistic updates and rollback.
    pub(super) fn patch_bot(&mut self, params: UpdateBotParams, cx: &mut Context<Self>) {
        let req_host = self.host.clone();
        self.settings
            .patch_queue
            .retain(|p| p.req_host() == &req_host);
        let prev_bot = self.bots.iter().find(|b| b.id == params.bot_id).cloned();
        if let Some(bot) = self.bots.iter_mut().find(|b| b.id == params.bot_id) {
            apply_update_bot_params(bot, &params);
            if self.selected_bot_id.as_ref() == Some(&params.bot_id) {
                let snapshot = bot.clone();
                self.settings.sync_from_bot(&snapshot);
                self.soul_memory.sync_config_from_bot(&snapshot);
            }
        }
        self.error = None;
        self.notice = None;
        if self.patch_task.is_some() {
            self.settings
                .patch_queue
                .push_back(PendingPatch::UpdateBot {
                    req_host,
                    params: Box::new(params),
                    prev_bot: prev_bot.map(Box::new),
                });
            cx.notify();
            return;
        }
        self.start_patch_update(req_host, params, prev_bot, cx);
    }

    fn start_patch_update(
        &mut self,
        req_host: crate::hosts::HostId,
        params: UpdateBotParams,
        prev_bot: Option<BotConfig>,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        let req_bot = params.bot_id.clone();
        let prev_agent = prev_bot
            .as_ref()
            .map(|b| krill_proto::agent::canonical_id(&b.agent_id).to_owned());
        self.patch_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(
                    async move { call_checked(&rpc, &BotRpcRequest::UpdateBot(Box::new(params))) },
                )
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host {
                    return;
                }
                this.patch_task = None;
                let same_bot = this.selected_bot_id.as_ref() == Some(&req_bot);
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        let confirmed = *updated;
                        let next_agent =
                            krill_proto::agent::canonical_id(&confirmed.agent_id).to_owned();
                        let provider_changed = prev_agent
                            .as_deref()
                            .is_some_and(|prev| prev != next_agent.as_str());
                        if let Some(next_for_bot) = this
                            .settings
                            .patch_queue
                            .iter_mut()
                            .find(|p| p.req_host() == &req_host && p.bot_id() == &confirmed.id)
                        {
                            next_for_bot.set_prev_bot(confirmed.clone());
                        }
                        let mut merged = confirmed;
                        for queued in &this.settings.patch_queue {
                            if queued.req_host() == &req_host && queued.bot_id() == &merged.id {
                                queued.apply_to(&mut merged);
                            }
                        }
                        if same_bot {
                            this.settings.sync_from_bot(&merged);
                            this.soul_memory.sync_config_from_bot(&merged);
                        }
                        if provider_changed {
                            if same_bot {
                                this.settings.agent_models = None;
                                this.settings.models_loading_for = None;
                                this.settings.models_task = None;
                            }
                            this.bot_sessions.retain(|s| s.bot_id != merged.id);
                        }
                        this.upsert_bot_config(merged);
                        if provider_changed {
                            if same_bot {
                                this.ensure_agent_models(&next_agent, cx);
                            }
                            this.emit_sessions_changed(cx);
                        }
                        this.emit_badge(cx);
                    }
                    Ok(_) => {}
                    Err(err) => {
                        this.settings
                            .patch_queue
                            .retain(|p| !(p.req_host() == &req_host && p.bot_id() == &req_bot));
                        if let Some(rollback) = prev_bot {
                            if same_bot {
                                this.settings.sync_from_bot(&rollback);
                                this.soul_memory.sync_config_from_bot(&rollback);
                            }
                            this.upsert_bot_config(rollback);
                            this.emit_badge(cx);
                        }
                        if same_bot {
                            this.error = Some(SharedString::from(err));
                        }
                    }
                }
                this.drain_patch_queue(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn drain_patch_queue(&mut self, cx: &mut Context<Self>) {
        if self.patch_task.is_some() {
            return;
        }
        while let Some(next) = self.settings.patch_queue.pop_front() {
            if next.req_host() != &self.host {
                continue;
            }
            match next {
                PendingPatch::UpdateBot {
                    req_host,
                    params,
                    prev_bot,
                } => {
                    self.start_patch_update(req_host, *params, prev_bot.map(|b| *b), cx);
                    return;
                }
                PendingPatch::RemoveMembers {
                    req_host,
                    bot_id,
                    members,
                    prev_bot,
                } => {
                    self.start_remove_members(req_host, bot_id, members, prev_bot.map(|b| *b), cx);
                    return;
                }
            }
        }
    }

    /// The bot's Items, Look around and Memory upkeep presets (defaults until krilld answers).
    pub(super) fn settings_extras(&self, bot_id: &BotId) -> BotSettingsExtras {
        self.settings
            .extras
            .get(bot_id)
            .copied()
            .unwrap_or_default()
    }

    /// Reads the presets once per bot; an older krilld without the RPC leaves the defaults.
    pub(super) fn ensure_settings_extras(&mut self, bot_id: &BotId, cx: &mut Context<Self>) {
        if self.settings.extras.contains_key(bot_id) || self.settings.extras_task.is_some() {
            return;
        }
        self.update_settings_extras(bot_id.clone(), Extras::default(), cx);
    }

    /// Changes the presets that are `Some` (none: just reads them) and keeps krilld's answer.
    pub(super) fn update_settings_extras(
        &mut self,
        bot_id: BotId,
        change: Extras,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        let req_host = self.host.clone();
        let read_only = change == Extras::default();
        let req = if read_only {
            BotRpcRequest::GetBotSettingsExtras {
                bot_id: bot_id.clone(),
            }
        } else {
            BotRpcRequest::UpdateBotSettingsExtras {
                bot_id: bot_id.clone(),
                items_stale: change.items_stale,
                look_around_every: change.look_around_every,
                tidy_every: change.tidy_every,
            }
        };
        self.settings.extras_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { call_checked(&rpc, &req) })
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host {
                    return;
                }
                this.settings.extras_task = None;
                let same_bot = this.selected_bot_id.as_ref() == Some(&bot_id);
                match res {
                    Ok(BotRpcResponse::BotSettingsExtras(extras)) => {
                        this.settings.extras.insert(bot_id, extras);
                    }
                    // An older krilld: keep the defaults so the page and Home still work.
                    Ok(_) | Err(_) if read_only => {
                        this.settings
                            .extras
                            .insert(bot_id, BotSettingsExtras::default());
                    }
                    Err(err) if same_bot => this.error = Some(SharedString::from(err)),
                    Ok(_) | Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Settings › Who can ask: takes these people off the bot, one after another.
    pub(super) fn remove_bot_members(
        &mut self,
        bot_id: BotId,
        members: Vec<BotMember>,
        cx: &mut Context<Self>,
    ) {
        if members.is_empty() {
            return;
        }
        let req_host = self.host.clone();
        self.settings
            .patch_queue
            .retain(|p| p.req_host() == &req_host);
        let prev_bot = self.bots.iter().find(|b| b.id == bot_id).cloned();
        if let Some(bot) = self.bots.iter_mut().find(|b| b.id == bot_id) {
            bot.members.retain(|m| !members.contains(m));
        }
        self.error = None;
        self.notice = None;
        if self.patch_task.is_some() {
            self.settings
                .patch_queue
                .push_back(PendingPatch::RemoveMembers {
                    req_host,
                    bot_id,
                    members,
                    prev_bot: prev_bot.map(Box::new),
                });
            cx.notify();
            return;
        }
        self.start_remove_members(req_host, bot_id, members, prev_bot, cx);
    }

    fn start_remove_members(
        &mut self,
        req_host: crate::hosts::HostId,
        bot_id: BotId,
        members: Vec<BotMember>,
        prev_bot: Option<BotConfig>,
        cx: &mut Context<Self>,
    ) {
        let rpc = self.rpc.clone();
        let req_bot = bot_id.clone();
        self.patch_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let mut last_ok_bot: Option<BotConfig> = None;
                    for member in members {
                        match call_checked(
                            &rpc,
                            &BotRpcRequest::RemoveBotMember {
                                bot_id: bot_id.clone(),
                                member,
                            },
                        ) {
                            Ok(BotRpcResponse::Bot(updated)) => {
                                last_ok_bot = Some(*updated);
                            }
                            Ok(_) => {}
                            Err(err) => return (last_ok_bot, Err(err)),
                        }
                    }
                    (last_ok_bot, Ok(()))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host {
                    return;
                }
                this.patch_task = None;
                let same_bot = this.selected_bot_id.as_ref() == Some(&req_bot);
                match res {
                    (Some(confirmed), Ok(())) => {
                        if let Some(next_for_bot) = this
                            .settings
                            .patch_queue
                            .iter_mut()
                            .find(|p| p.req_host() == &req_host && p.bot_id() == &confirmed.id)
                        {
                            next_for_bot.set_prev_bot(confirmed.clone());
                        }
                        let mut merged = confirmed;
                        for queued in &this.settings.patch_queue {
                            if queued.req_host() == &req_host && queued.bot_id() == &merged.id {
                                queued.apply_to(&mut merged);
                            }
                        }
                        this.upsert_bot_config(merged);
                        this.emit_badge(cx);
                    }
                    (None, Ok(())) => {}
                    (last_ok_bot, Err(err)) => {
                        this.settings
                            .patch_queue
                            .retain(|p| !(p.req_host() == &req_host && p.bot_id() == &req_bot));
                        if let Some(rollback) = last_ok_bot.or(prev_bot) {
                            this.upsert_bot_config(rollback);
                            this.emit_badge(cx);
                        }
                        if same_bot {
                            this.error = Some(SharedString::from(err));
                        }
                    }
                }
                this.drain_patch_queue(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The subpage the Settings tab shows (the host's Back returns to the list first).
    #[must_use]
    pub fn settings_subpage(&self) -> Option<&'static str> {
        self.settings.page.title()
    }

    /// Back to the settings list from a subpage.
    pub fn close_settings_subpage(&mut self, cx: &mut Context<Self>) {
        self.open_settings_page(SettingsPage::Root, cx);
    }

    pub(super) fn open_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        if self.settings.page == SettingsPage::Soul && page != SettingsPage::Soul {
            self.flush_unsaved_soul_edits(cx);
        }
        self.settings.page = page;
        self.settings.memory_history_tab = false;
        self.settings.tidy_review = false;
        self.soul_memory.armed_forget = None;
        self.error = None;
        cx.notify();
    }

    fn render_settings_row(
        row: Row,
        compact: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Row {
            id,
            icon,
            icon_name,
            title,
            detail,
            wrap_detail,
            value,
            end,
            open,
        } = row;
        let has_detail = detail.is_some();
        let has_value = value.is_some();
        // Orca's root rows are denser, with 14 px icons; subpages use 16 px.
        let icon_px = if compact { ui::ICON_MD } else { ui::ICON_LG };
        let icon_el = icon
            .map(|data| Icon::default().data(data))
            .or_else(|| icon_name.map(Icon::new))
            .map(|icon| {
                div()
                    .w(px(16.))
                    .flex_none()
                    .flex()
                    .justify_center()
                    .when(has_detail, |d| d.self_start().pt(px(1.)))
                    .child(icon.size(px(icon_px)).text_color(p.muted_foreground))
            });
        let text_col = div()
            .min_w_0()
            .flex()
            .flex_col()
            .map(|d| {
                if has_value {
                    d.flex_none().max_w(relative(0.7))
                } else {
                    d.flex_1()
                }
            })
            .child(
                div()
                    .truncate()
                    .text_size(ui::TEXT_MD)
                    .font_weight(if !compact || has_detail {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    })
                    .line_height(px(18.))
                    .text_color(p.foreground)
                    .child(title.clone()),
            )
            .children(detail.map(|d| {
                div()
                    .text_size(ui::TEXT_SM)
                    .line_height(px(16.))
                    .text_color(p.muted_foreground)
                    .map(|el| if wrap_detail { el } else { el.truncate() })
                    .child(d)
            }));
        let value_el = value.map(|v| {
            div()
                .ml_auto()
                .min_w_0()
                .h(px(16.))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_size(ui::TEXT_SM)
                .line_height(px(16.))
                .text_color(p.muted_foreground)
                .child(v)
        });
        let (min_h, py) = if compact {
            (px(36.), px(4.))
        } else {
            (px(44.), px(8.))
        };
        let inner = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(10.))
            .children(icon_el)
            .child(text_col)
            .children(value_el);
        let base = div()
            .id(id.clone())
            .debug_selector({
                let id = id.clone();
                move || id.to_string()
            })
            .w_full()
            .min_w_0()
            .min_h(min_h)
            .py(py)
            .pl_3()
            .pr(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .aria_label(title);
        let click = |open: Open| {
            cx.listener(
                move |this: &mut Self, _: &gpui::ClickEvent, window, cx| match &open {
                    Open::Page(page) => this.open_settings_page(*page, cx),
                    Open::Tab(tab) => cx.emit(OpenSettingsPage(*tab)),
                    Open::Run(run) => run(this, window, cx),
                },
            )
        };
        match (open, end) {
            // The text opens the subpage; the control at the end acts on its own.
            (Some(open), Some(end)) => base
                .child(
                    div()
                        .id(SharedString::from(format!("{id}-open")))
                        .debug_selector({
                            let id = id.clone();
                            move || format!("{id}-open")
                        })
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .cursor_pointer()
                        .child(inner)
                        .on_click(click(open)),
                )
                .hover(|d| d.bg(p.accent.opacity(0.6)))
                .child(div().flex_none().flex().items_center().gap_1p5().child(end))
                .into_any_element(),
            (Some(open), None) => base
                .cursor_pointer()
                .hover(|d| d.bg(p.accent.opacity(0.6)))
                .child(inner)
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size(px(ui::ICON_MD))
                        .text_color(p.muted_foreground),
                )
                .on_click(click(open))
                .into_any_element(),
            (None, end) => base
                .child(inner)
                .children(
                    end.map(|end| div().flex_none().flex().items_center().gap_1p5().child(end)),
                )
                .into_any_element(),
        }
    }

    /// Orca's `SettingsInsetGroup`: a small heading (and its action), then a muted card of
    /// hairline-split rows, then an optional foot line.
    pub(super) fn render_inset_group(
        title: Option<AnyElement>,
        action: Option<AnyElement>,
        rows: Vec<Row>,
        foot: Option<SharedString>,
        compact: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut card = div()
            .w_full()
            .rounded(ui::RADIUS_LG)
            .overflow_hidden()
            .bg(p.foreground.opacity(0.035))
            .flex()
            .flex_col();
        let mut previous_icon = false;
        for (i, row) in rows.into_iter().enumerate() {
            let icon = row.has_icon();
            // Hairlines start at the text, under an icon only when both rows have one.
            let inset = if icon && previous_icon { 38. } else { 12. };
            if i > 0 {
                card = card.child(
                    div()
                        .w_full()
                        .pl(px(inset))
                        .child(div().h(px(1.)).w_full().bg(p.border)),
                );
            }
            previous_icon = icon;
            card = card.child(Self::render_settings_row(row, compact, p, cx));
        }
        Self::inset_frame(title, action, card, foot, p)
    }

    /// An inset group's heading and foot around any `card`.
    pub(super) fn inset_frame(
        title: Option<AnyElement>,
        action: Option<AnyElement>,
        card: impl gpui::IntoElement,
        foot: Option<SharedString>,
        p: &ui::Palette,
    ) -> gpui::Div {
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_1p5()
            .when(title.is_some() || action.is_some(), |d| {
                d.child(
                    div()
                        .min_h(px(20.))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .text_size(ui::TEXT_SM)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.muted_foreground)
                                .children(title),
                        )
                        .children(action),
                )
            })
            .child(card)
            .children(foot.map(|f| {
                div()
                    .px_3()
                    .text_size(ui::TEXT_SM)
                    .line_height(px(17.))
                    .text_color(p.muted_foreground)
                    .child(f)
            }))
    }

    pub(super) fn group_title(title: &str) -> AnyElement {
        title.to_owned().into_any_element()
    }

    fn root_group(
        title: Option<&str>,
        rows: Vec<Row>,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        Self::render_inset_group(title.map(Self::group_title), None, rows, None, true, p, cx)
    }

    /// Orca's permission line: the bot's starting permission.
    pub(super) fn permission_label(mode: Permission) -> &'static str {
        match mode {
            Permission::Yolo => chat_text::PERMISSION_FULL_ACCESS.get(),
            Permission::Manual => chat_text::PERMISSION_ASK.get(),
        }
    }

    fn switch_patch(
        p: &ui::Palette,
        id: &'static str,
        label: &'static str,
        on: bool,
        bot_id: BotId,
        make: fn(&BotConfig, bool) -> UpdateBotParams,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let ent = cx.entity();
        row_switch_compact(p, id, label, on, move |checked, _, cx| {
            ent.update(cx, |this, cx| {
                let Some(current) = this.bots.iter().find(|b| b.id == bot_id) else {
                    return;
                };
                let params = make(current, checked);
                this.patch_bot(params, cx);
            });
        })
    }

    fn start_inline_name_edit(
        &mut self,
        bot: &BotConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial = bot.name.clone();
        let input = cx.new(|cx| {
            let mut st =
                InputState::new(window, cx).placeholder(text::FIELD_NAME_PLACEHOLDER.get());
            st.set_value(initial, window, cx);
            st
        });
        self.settings.inline_name_sub =
            Some(
                cx.subscribe_in(&input, window, |this, ent, ev: &InputEvent, window, cx| {
                    if !matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        return;
                    }
                    if matches!(ev, InputEvent::PressEnter { .. })
                        && crate::jump::entity_input_is_composing(ent, window, cx)
                    {
                        return;
                    }
                    this.commit_inline_name_edit(cx);
                }),
            );
        input.update(cx, |s, cx| s.focus(window, cx));
        self.settings.inline_name_input = Some(input);
        self.settings.inline_name_editing = true;
        cx.notify();
    }

    fn commit_inline_name_edit(&mut self, cx: &mut Context<Self>) {
        if !self.settings.inline_name_editing {
            return;
        }
        let typed = self
            .settings
            .inline_name_input
            .as_ref()
            .map(|i| i.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        self.settings.inline_name_editing = false;
        self.settings.inline_name_input = None;
        self.settings.inline_name_sub = None;
        if !typed.is_empty()
            && let Some(bot) = self.selected_bot().cloned()
            && typed != bot.name
        {
            self.patch_bot(
                UpdateBotParams {
                    name: Some(typed),
                    ..patch(&bot)
                },
                cx,
            );
        }
        cx.notify();
    }

    fn cancel_inline_name_edit(&mut self, cx: &mut Context<Self>) {
        self.settings.inline_name_editing = false;
        self.settings.inline_name_input = None;
        self.settings.inline_name_sub = None;
        cx.notify();
    }

    pub(super) fn render_settings_root(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_settings_extras(&bot.id, cx);
        let upload = self.settings.avatar_cache.get(&bot.id).cloned();
        let pic_bot_id = bot.id.clone();
        let rename_bot = bot.clone();
        let name_slot: AnyElement = if self.settings.inline_name_editing
            && let Some(input) = &self.settings.inline_name_input
        {
            div()
                .id("bot-settings-name-edit")
                .debug_selector(|| "bot-settings-name-edit".into())
                .flex_1()
                .min_w_0()
                .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" && !ev.keystroke.modifiers.modified() {
                        cx.stop_propagation();
                        this.cancel_inline_name_edit(cx);
                    }
                }))
                .child(Input::new(input).small())
                .into_any_element()
        } else {
            div()
                .id("bot-settings-name-btn")
                .debug_selector(|| "bot-settings-name-btn".into())
                .role(gpui::Role::Button)
                .tab_index(0)
                .aria_label(text::SV_RENAME_HINT.get())
                .flex_1()
                .min_w_0()
                .truncate()
                .rounded(ui::RADIUS_XS)
                .px_1()
                .ml(px(-4.))
                .cursor_pointer()
                .hover(|d| d.bg(p.accent.opacity(0.6)))
                .text_size(ui::TEXT_LG)
                .font_weight(FontWeight::SEMIBOLD)
                .line_height(px(20.))
                .text_color(p.foreground)
                .child(bot.name.clone())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.start_inline_name_edit(&rename_bot, window, cx);
                }))
                .into_any_element()
        };
        let card = div()
            .w_full()
            .rounded(ui::RADIUS_LG)
            .bg(p.foreground.opacity(0.035))
            .p_3()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .id("bot-settings-picture-btn")
                    .debug_selector(|| "bot-settings-picture-btn".into())
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .aria_label(text::SV_CHANGE_PICTURE.get())
                    .flex_none()
                    .rounded(ui::RADIUS_MD)
                    .cursor_pointer()
                    .hover(|d| d.opacity(0.85))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick_and_upload_avatar(pic_bot_id.clone(), window, cx);
                    }))
                    .child(avatar::render(
                        &avatar::Avatar::of(bot),
                        upload,
                        &bot.name,
                        px(32.),
                        ui::RADIUS_MD,
                        p,
                    )),
            )
            .child(name_slot)
            .child(
                Button::new("bot-settings-share")
                    .debug_selector(|| "bot-settings-share".into())
                    .ghost()
                    .xsmall()
                    .size(px(28.))
                    .icon(
                        Icon::default()
                            .data(ui::icons::SHARE_2_SVG)
                            .size(px(ui::ICON_MD)),
                    )
                    .tooltip(text::SV_SHARE.get())
                    .accessibility_label(text::SV_SHARE.get())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_settings_page(SettingsPage::Sharing, cx);
                    })),
            );

        let agent = vec![
            Row::new(
                "bot-settings-agent",
                Some(ui::icons::BOT_SVG),
                text::SV_AGENT_GROUP.get(),
            )
            .value(Some(runtime_display_label(&bot.agent_id)))
            .opens(Open::Page(SettingsPage::Agent)),
            Row::new(
                "bot-settings-permissions",
                Some(ui::icons::SHIELD_SVG),
                text::SV_PERMISSIONS.get(),
            )
            .value(Some(Self::permission_label(bot.permission_mode)))
            .opens(Open::Page(SettingsPage::Permissions)),
            Row::new(
                "bot-settings-computer",
                Some(if self.host.is_local() {
                    ui::icons::LAPTOP_SVG
                } else {
                    ui::icons::SERVER_SVG
                }),
                text::SV_COMPUTER.get(),
            )
            .value(Some(computer_name(self)))
            .opens(Open::Page(SettingsPage::Computer)),
            Row::new(
                "bot-settings-active",
                Some(ui::icons::ACTIVITY_SVG),
                text::SV_ACTIVE.get(),
            )
            .end(Self::switch_patch(
                p,
                "bot-settings-active-switch",
                text::SV_ACTIVE.get(),
                !bot.paused,
                bot.id.clone(),
                |bot, on| UpdateBotParams {
                    paused: Some(!on),
                    ..patch(bot)
                },
                cx,
            )),
        ];

        let look = &bot.look_around;
        let tidy = bot.memory_tidy;
        let extras = self.settings_extras(&bot.id);
        let schedule = vec![
            Row::new(
                "bot-settings-look-around",
                Some(ui::icons::EYE_SVG),
                text::SV_LOOK_AROUND.get(),
            )
            .value(
                look.enabled
                    .then(|| look_around_short(extras.look_around_every)),
            )
            .opens(Open::Page(SettingsPage::LookAround))
            .end(Self::switch_patch(
                p,
                "bot-settings-look-around-switch",
                text::SV_LOOK_AROUND.get(),
                look.enabled,
                bot.id.clone(),
                |bot, on| {
                    let mut look = bot.look_around.clone();
                    look.enabled = on;
                    UpdateBotParams {
                        look_around: Some(look),
                        ..patch(bot)
                    }
                },
                cx,
            )),
            Row::new(
                "bot-settings-memory-upkeep",
                Some(ui::icons::MOON_SVG),
                text::SV_MEMORY_UPKEEP.get(),
            )
            .value(tidy.enabled.then(|| tidy_short(extras.tidy_every)))
            .opens(Open::Page(SettingsPage::TidyUp))
            .end(Self::switch_patch(
                p,
                "bot-settings-memory-upkeep-switch",
                text::SV_MEMORY_UPKEEP.get(),
                tidy.enabled,
                bot.id.clone(),
                |bot, on| {
                    let mut tidy = bot.memory_tidy;
                    tidy.enabled = on;
                    UpdateBotParams {
                        memory_tidy: Some(tidy),
                        ..patch(bot)
                    }
                },
                cx,
            )),
        ];

        let stale_bot = bot.clone();
        let ent = cx.entity();
        let items = vec![
            Row::new(
                "bot-settings-items-stale",
                Some(ui::icons::HOURGLASS_SVG),
                text::SV_STALE_SHORT.get(),
            )
            .end(row_select(
                "bot-settings-items-stale-select",
                text::SV_STALE_TITLE.get(),
                extras.items_stale,
                BotItemsStale::ALL
                    .into_iter()
                    .map(|s| (s, stale_label(s).into()))
                    .collect(),
                |b| b.debug_selector(|| "bot-settings-items-stale-select".into()),
                move |choice, _, cx| {
                    ent.update(cx, |this, cx| {
                        this.update_settings_extras(
                            stale_bot.id.clone(),
                            Extras {
                                items_stale: Some(choice),
                                ..Extras::default()
                            },
                            cx,
                        );
                    });
                },
            )),
        ];

        // MEMORY.md and its topic files.
        let memory_files = self.soul_memory.memory_topics.len() + 1;
        let soul_memory = vec![
            Row::new(
                "bot-settings-soul",
                Some(ui::icons::SMILE_SVG),
                text::SV_SOUL.get(),
            )
            .opens(Open::Page(SettingsPage::Soul)),
            Row::new(
                "bot-settings-memory",
                Some(ui::icons::BRAIN_SVG),
                text::SV_MEMORY.get(),
            )
            .value(Some(
                text::SV_MEMORY_FILES.format(&[("n", &memory_files.to_string())]),
            ))
            .opens(Open::Page(SettingsPage::Memory)),
        ];

        let connectors = self
            .all_connector_statuses
            .iter()
            .filter(|s| s.bot_id == bot.id && s.enabled)
            .count();
        let access = vec![
            Row::new(
                "bot-settings-connectors",
                Some(ui::icons::PLUG_SVG),
                text::SV_CONNECTORS.get(),
            )
            .value(Some(count(connectors)))
            .opens(Open::Tab(DetailTab::Connectors)),
            Row::new(
                "bot-settings-who-can-ask",
                Some(ui::icons::USERS_SVG),
                text::SV_WHO_CAN_ASK.get(),
            )
            .value(Some(self.who_can_ask_value(bot)))
            .opens(Open::Page(SettingsPage::WhoCanAsk)),
            Row::new(
                "bot-settings-tools",
                Some(ui::icons::WRENCH_SVG),
                text::SV_TOOLS.get(),
            )
            .value(Some(count(bot.mcp_tools.len())))
            .opens(Open::Page(SettingsPage::Tools)),
            Row::new(
                "bot-settings-skills",
                Some(ui::icons::BOOK_OPEN_SVG),
                text::SV_SKILLS.get(),
            )
            .value(Some(count(bot.skills.len())))
            .opens(Open::Page(SettingsPage::Skills)),
        ];

        let sharing = vec![
            Row::new(
                "bot-settings-sharing",
                Some(ui::icons::SHARE_2_SVG),
                text::SV_SHARING.get(),
            )
            .value(Some(text::SV_SHARING_OFF.get()))
            .opens(Open::Page(SettingsPage::Sharing)),
        ];

        let notify = |id: &'static str,
                      label: &'static str,
                      on: bool,
                      make: fn(&BotConfig, bool) -> UpdateBotParams,
                      cx: &mut Context<Self>| {
            Row::new(id, None, label).end(Self::switch_patch(
                p,
                id,
                label,
                on,
                bot.id.clone(),
                make,
                cx,
            ))
        };
        let notifications = vec![
            notify(
                "bot-settings-notify-permission",
                text::SV_NOTIFY_PERMISSION.get(),
                bot.notifications.needs_permission,
                |bot, on| {
                    let mut n = bot.notifications;
                    n.needs_permission = on;
                    UpdateBotParams {
                        notifications: Some(n),
                        ..patch(bot)
                    }
                },
                cx,
            ),
            notify(
                "bot-settings-notify-complete",
                text::SV_NOTIFY_COMPLETE.get(),
                bot.notifications.task_complete,
                |bot, on| {
                    let mut n = bot.notifications;
                    n.task_complete = on;
                    UpdateBotParams {
                        notifications: Some(n),
                        ..patch(bot)
                    }
                },
                cx,
            ),
            notify(
                "bot-settings-notify-failed",
                text::SV_NOTIFY_FAILED.get(),
                bot.notifications.could_not_finish,
                |bot, on| {
                    let mut n = bot.notifications;
                    n.could_not_finish = on;
                    UpdateBotParams {
                        notifications: Some(n),
                        ..patch(bot)
                    }
                },
                cx,
            ),
        ];

        let delete_id = bot.id.clone();
        Self::settings_screen("bot-settings-root")
            .gap_3()
            .child(card)
            .child(Self::root_group(
                Some(text::SV_AGENT_GROUP.get()),
                agent,
                p,
                cx,
            ))
            .child(Self::root_group(
                Some(text::SV_SCHEDULE.get()),
                schedule,
                p,
                cx,
            ))
            .child(Self::root_group(Some(text::SV_ITEMS.get()), items, p, cx))
            .child(Self::root_group(
                Some(text::SV_SOUL_MEMORY.get()),
                soul_memory,
                p,
                cx,
            ))
            .child(Self::root_group(Some(text::SV_ACCESS.get()), access, p, cx))
            .child(Self::root_group(None, sharing, p, cx))
            .child(Self::root_group(
                Some(text::SV_NOTIFICATIONS.get()),
                notifications,
                p,
                cx,
            ))
            .child(
                div().child(
                    ui::ghost("bot-settings-delete", text::BP_DELETE.get())
                        .debug_selector(|| "bot-settings-delete".into())
                        .icon(
                            Icon::default()
                                .data(ui::icons::TRASH_SVG)
                                .size(px(ui::ICON_SM)),
                        )
                        .text_color(p.destructive)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(RequestDeleteBot(delete_id.clone()));
                        })),
                ),
            )
    }

    /// Orca's settings screen: a scrolling column, `px-3 pt-3.5 pb-6`, groups 18 px apart.
    pub(super) fn settings_screen(id: &'static str) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .overflow_x_hidden()
            .px_3()
            .pt(px(14.))
            .pb_6()
            .flex()
            .flex_col()
            .gap(px(18.))
    }

    /// "Only you", "You and 4 others", "… · 1 request" (Orca's `whoCanAskValue`).
    pub(super) fn who_can_ask_value(&self, bot: &BotConfig) -> String {
        let people = bot
            .members
            .iter()
            .filter(|m| m.source != ExternalSource::Google)
            .count();
        let requests = self
            .connectors
            .bot_access_requests
            .iter()
            .filter(|r| r.bot_id == bot.id && r.status == BotAccessRequestStatus::Pending)
            .count();
        let who = if people == 0 {
            text::SV_ONLY_YOU.get().to_owned()
        } else {
            text::SV_YOU_AND_OTHERS.format(&[("n", &people.to_string())])
        };
        if requests > 0 {
            format!(
                "{who} · {}",
                text::SV_REQUESTS.format(&[("n", &requests.to_string())])
            )
        } else {
            who
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use krill_proto::bots::BotNotifications;

    use super::*;

    #[gpui::test]
    fn rapid_patch_bot_serializes_without_dropping_and_rolls_back_on_error(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let bot_id = BotId::new("bot-1").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "Alpha", "claude", 1_700_000_000_000).unwrap();
        bot.notifications = BotNotifications {
            needs_permission: false,
            task_complete: false,
            could_not_finish: false,
        };
        let state = Arc::new(Mutex::new(bot));
        let calls = Arc::new(Mutex::new(Vec::<UpdateBotParams>::new()));
        let fail_next = Arc::new(Mutex::new(false));
        let st = state.clone();
        let cl = calls.clone();
        let fl = fail_next.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![st.lock().unwrap().clone()])),
            BotRpcRequest::UpdateBot(params) => {
                cl.lock().unwrap().push((**params).clone());
                if *fl.lock().unwrap() {
                    return Err("simulated patch failure".to_owned());
                }
                let mut cur = st.lock().unwrap();
                apply_update_bot_params(&mut cur, params);
                Ok(BotRpcResponse::Bot(Box::new(cur.clone())))
            }
            _ => Ok(BotRpcResponse::Ok),
        });

        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&bot_id, DetailTab::Settings, cx);
            });
        });
        vcx.run_until_parked();

        // Two rapid toggles before the first RPC settles serialize through patch_queue.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                let first = v.selected_bot().unwrap().clone();
                let mut n1 = first.notifications;
                n1.needs_permission = true;
                v.patch_bot(
                    UpdateBotParams {
                        notifications: Some(n1),
                        ..patch(&first)
                    },
                    cx,
                );
                assert!(v.selected_bot().unwrap().notifications.needs_permission);
                assert!(v.patch_task.is_some());

                // Second toggle reads live optimistically-updated bot and queues behind patch 1.
                let second = v.selected_bot().unwrap().clone();
                let mut n2 = second.notifications;
                n2.task_complete = true;
                v.patch_bot(
                    UpdateBotParams {
                        notifications: Some(n2),
                        ..patch(&second)
                    },
                    cx,
                );
                assert!(v.selected_bot().unwrap().notifications.task_complete);
                assert_eq!(v.settings.patch_queue.len(), 1);
            });
        });
        vcx.run_until_parked();

        assert_eq!(calls.lock().unwrap().len(), 2);
        vcx.update(|_, cx| {
            let v = view.read(cx);
            let updated = v.selected_bot().unwrap();
            assert!(updated.notifications.needs_permission);
            assert!(updated.notifications.task_complete);
            assert!(v.settings.patch_queue.is_empty());
            assert!(v.patch_task.is_none());
            assert!(v.error.is_none());
        });

        // A failing patch rolls back optimistic state and drops queued follow-ups for that bot.
        *fail_next.lock().unwrap() = true;
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                let cur = v.selected_bot().unwrap().clone();
                v.patch_bot(
                    UpdateBotParams {
                        paused: Some(true),
                        ..patch(&cur)
                    },
                    cx,
                );
                let cur2 = v.selected_bot().unwrap().clone();
                let mut n3 = cur2.notifications;
                n3.could_not_finish = true;
                v.patch_bot(
                    UpdateBotParams {
                        notifications: Some(n3),
                        ..patch(&cur2)
                    },
                    cx,
                );
                assert!(v.selected_bot().unwrap().paused);
                assert!(v.selected_bot().unwrap().notifications.could_not_finish);
            });
        });
        vcx.run_until_parked();

        vcx.update(|_, cx| {
            let v = view.read(cx);
            let rolled = v.selected_bot().unwrap();
            assert!(!rolled.paused, "failed patch must roll back paused");
            assert!(
                !rolled.notifications.could_not_finish,
                "queued follow-up for failed bot must be dropped"
            );
            assert!(v.error.is_some());
            assert!(v.settings.patch_queue.is_empty());
        });

        // Stale host completion is ignored after switching hosts (UB-20).
        *fail_next.lock().unwrap() = false;
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                let cur = v.selected_bot().unwrap().clone();
                v.patch_bot(
                    UpdateBotParams {
                        paused: Some(true),
                        ..patch(&cur)
                    },
                    cx,
                );
                v.host = crate::hosts::HostId::Remote("iroh:other".into());
                v.bots[0].paused = false;
            });
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            assert!(!view.read(cx).bots[0].paused);
        });
    }

    #[gpui::test]
    fn switching_bots_keeps_patch_queue_draining_and_host_switch_resets_extras_models_and_scan(
        cx: &mut gpui::TestAppContext,
    ) {
        use krill_proto::bots::BotAgentModels;
        use std::sync::{Arc, Mutex};

        let b1 = BotId::new("bot-switch-a").unwrap();
        let b2 = BotId::new("bot-switch-b").unwrap();
        let cfg1 = BotConfig::new(b1.clone(), "Alpha", "claude", 1_700_000_000_000).unwrap();
        let cfg2 = BotConfig::new(b2.clone(), "Beta", "claude", 1_700_000_000_000).unwrap();
        let calls: Arc<Mutex<Vec<BotId>>> = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(Mutex::new(cfg1.clone()));
        let calls_rpc = calls.clone();
        let state_rpc = state.clone();
        let cfg2_rpc = cfg2.clone();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![
                state_rpc.lock().unwrap().clone(),
                cfg2_rpc.clone(),
            ])),
            BotRpcRequest::UpdateBot(params) => {
                calls_rpc.lock().unwrap().push(params.bot_id.clone());
                let mut cur = state_rpc.lock().unwrap();
                apply_update_bot_params(&mut cur, params);
                Ok(BotRpcResponse::Bot(Box::new(cur.clone())))
            }
            _ => Ok(BotRpcResponse::Ok),
        });
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx));
        vcx.run_until_parked();

        // R2A-20: two rapid toggles on Bot A followed immediately by switching to Bot B must
        // keep patch_task alive and drain patch_queue so both Bot A edits persist.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                let first = v.bots.iter().find(|b| b.id == b1).unwrap().clone();
                let mut n1 = first.notifications;
                n1.needs_permission = true;
                v.patch_bot(
                    UpdateBotParams {
                        notifications: Some(n1),
                        ..patch(&first)
                    },
                    cx,
                );
                let second = v.bots.iter().find(|b| b.id == b1).unwrap().clone();
                let mut n2 = second.notifications;
                n2.task_complete = true;
                v.patch_bot(
                    UpdateBotParams {
                        notifications: Some(n2),
                        ..patch(&second)
                    },
                    cx,
                );
                assert_eq!(v.settings.patch_queue.len(), 1);
                v.confirm_select_bot(b2.clone(), cx);
                assert!(
                    v.patch_task.is_some(),
                    "confirm_select_bot must keep in-flight patch_task alive"
                );
            });
        });
        vcx.run_until_parked();
        assert_eq!(calls.lock().unwrap().as_slice(), &[b1.clone(), b1.clone()]);
        vcx.update(|_, cx| {
            let v = view.read(cx);
            let updated_a = v.bots.iter().find(|b| b.id == b1).unwrap();
            assert!(updated_a.notifications.needs_permission);
            assert!(updated_a.notifications.task_complete);
            assert!(v.settings.patch_queue.is_empty());
        });

        // R2A-19: switching host resets extras_task, models_task, models_loading_for,
        // agent_models, import_state.scanning, and patch_queue.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.settings.extras_task = Some(cx.spawn(async move |_, _| {
                    std::future::pending::<()>().await;
                }));
                v.settings.models_task = Some(cx.spawn(async move |_, _| {
                    std::future::pending::<()>().await;
                }));
                v.settings.models_loading_for = Some("claude".to_owned());
                v.settings.agent_models = Some(("claude".to_owned(), BotAgentModels::default()));
                v.import_state.scanning = true;
                v.rpc_is_mock = false;
                v.set_host_target(
                    crate::hosts::HostId::Remote("host-b".into()),
                    None,
                    Vec::new(),
                    cx,
                );
                assert!(v.settings.extras_task.is_none());
                assert!(v.settings.models_task.is_none());
                assert!(v.settings.models_loading_for.is_none());
                assert!(v.settings.agent_models.is_none());
                assert!(!v.import_state.scanning);
                assert!(v.settings.patch_queue.is_empty());
            });
        });
    }
}
