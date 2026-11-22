// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License):
// src/renderer/src/components/bots/settings-view/BotSettingsRoot.tsx, BotSettingsRows.tsx,
// bot-settings-place.ts, BotItemsSettingsGroup.tsx and bot-settings-row-states.ts.
//! Bot settings as Orca draws them in the right panel: the bot's card, then inset groups of
//! rows (Agent, Schedule, Items, Soul and memory, Access, Sharing, Notifications), then Delete.
//! A row's text opens its own subpage; a switch or picker at its end acts at once.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
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

/// A switch for a row's end, as Orca's compact `BotSettingsSwitch`.
pub(super) fn row_switch(
    p: &ui::Palette,
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    on: bool,
    toggle: impl Fn(bool, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    ui::switch(p, id, on, false, toggle).aria_label(label.into())
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
        text::HOST_LOCAL.get().to_owned()
    } else {
        view.available_hosts
            .iter()
            .find(|(id, _, _)| id == &view.host)
            .map_or_else(|| "?".to_owned(), |(_, label, _)| label.clone())
    }
}

impl View {
    /// Applies one change from a settings row and shows the bot as krilld returns it.
    pub(super) fn patch_bot(&mut self, params: UpdateBotParams, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        let prev_agent = self
            .bots
            .iter()
            .find(|b| b.id == params.bot_id)
            .map(|b| krill_proto::agent::canonical_id(&b.agent_id).to_owned());
        self.error = None;
        self.mutation_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(
                    async move { call_checked(&rpc, &BotRpcRequest::UpdateBot(Box::new(params))) },
                )
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        let next_agent =
                            krill_proto::agent::canonical_id(&updated.agent_id).to_owned();
                        let provider_changed = prev_agent
                            .as_deref()
                            .is_some_and(|prev| prev != next_agent.as_str());
                        this.settings.sync_from_bot(&updated);
                        this.soul_memory.sync_config_from_bot(&updated);
                        if provider_changed {
                            this.settings.agent_models = None;
                            this.settings.models_loading_for = None;
                            this.settings.models_task = None;
                            this.bot_sessions.retain(|s| s.bot_id != updated.id);
                        }
                        this.upsert_bot_config(*updated);
                        if provider_changed {
                            this.ensure_agent_models(&next_agent, cx);
                            this.emit_sessions_changed(cx);
                        }
                        this.emit_badge(cx);
                    }
                    Ok(_) => {}
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
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
                this.settings.extras_task = None;
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
                    Ok(_) => {}
                    Err(err) => this.error = Some(SharedString::from(err)),
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
        let rpc = self.rpc.clone();
        self.error = None;
        self.mutation_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let mut last = Ok(BotRpcResponse::Ok);
                    for member in members {
                        last = call_checked(
                            &rpc,
                            &BotRpcRequest::RemoveBotMember {
                                bot_id: bot_id.clone(),
                                member,
                            },
                        );
                        if last.is_err() {
                            break;
                        }
                    }
                    last
                })
                .await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => this.upsert_bot_config(*updated),
                    Ok(_) => {}
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
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
                    .font_weight(FontWeight::MEDIUM)
                    .line_height(px(18.))
                    .text_color(p.foreground)
                    .child(title.clone()),
            )
            .children(detail.map(|d| {
                div()
                    .text_size(ui::TEXT_XS)
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
                .text_size(ui::TEXT_XS)
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
                                .text_size(ui::TEXT_XS)
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
                    .text_size(ui::TEXT_XS)
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
        bot: &BotConfig,
        make: fn(&BotConfig, bool) -> UpdateBotParams,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let ent = cx.entity();
        let bot = bot.clone();
        row_switch(p, id, label, on, move |checked, _, cx| {
            let params = make(&bot, checked);
            ent.update(cx, |this, cx| this.patch_bot(params, cx));
        })
    }

    pub(super) fn render_settings_root(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_settings_extras(&bot.id, cx);
        let upload = self.settings.avatar_cache.get(&bot.id).cloned();
        let card = div()
            .w_full()
            .rounded(ui::RADIUS_LG)
            .bg(p.foreground.opacity(0.035))
            .p_3()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(avatar::render(
                &avatar::Avatar::of(bot),
                upload,
                &bot.name,
                px(32.),
                ui::RADIUS_MD,
                p,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(px(20.))
                    .text_color(p.foreground)
                    .child(bot.name.clone()),
            )
            .child(
                Button::new("bot-settings-share")
                    .debug_selector(|| "bot-settings-share".into())
                    .ghost()
                    .xsmall()
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
                bot,
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
                bot,
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
                bot,
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
            Row::new(
                "bot-settings-schedules",
                Some(ui::icons::CALENDAR_CLOCK_SVG),
                text::SV_SCHEDULES.get(),
            )
            .value(Some(count(bot.schedules.len())))
            .opens(Open::Page(SettingsPage::Schedules)),
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
                |b| {
                    b.small()
                        .debug_selector(|| "bot-settings-items-stale-select".into())
                },
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
            .opens(Open::Page(SettingsPage::Sharing)),
        ];

        let notify = |id: &'static str,
                      label: &'static str,
                      on: bool,
                      make: fn(&BotConfig, bool) -> UpdateBotParams,
                      cx: &mut Context<Self>| {
            Row::new(id, None, label).end(Self::switch_patch(p, id, label, on, bot, make, cx))
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
