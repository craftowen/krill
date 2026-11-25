// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License):
// src/renderer/src/components/bots/settings-view/BotSettingsSubpage.tsx, BotComputerPage.tsx,
// BotWhoCanAskPage.tsx, BotSharingPage.tsx, settings/bots/BotGeneralSettings.tsx,
// BotLookAroundSection.tsx, BotTidyUpSection.tsx and bots/BotTidyUpReview.tsx.
//! One drill-in of the bot settings view each: the root's rows open these, Back returns.

use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, FontWeight, InteractiveElement as _,
    IntoElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Icon, IconName, Sizable as _};
use krill_proto::agent::Permission;
use krill_proto::bots::{
    BotAgentModels, BotConfig, BotMember, BotRpcRequest, BotRpcResponse, ExternalSource,
    LookAroundEvery, TidyEvery, UpdateBotParams,
};

use crate::i18n::catalog::bots as text;
use crate::i18n::catalog::chat as chat_text;
use crate::ui;

use super::View;
use super::rpc::{
    BOT_RUNTIMES, call_checked, effort_label, effort_options_for_runtime, runtime_display_label,
    runtime_models,
};
use super::settings_root::{
    Extras, Open, Row, SettingsPage, computer_name, patch, row_select, row_switch,
};

/// Orca's `LOOK_AROUND_EVERY_LABELS`.
pub(super) fn look_around_every_label(every: LookAroundEvery) -> &'static str {
    match every {
        LookAroundEvery::Every4h => text::SV_LOOK_EVERY_4H.get(),
        LookAroundEvery::Every2h => text::SV_LOOK_EVERY_2H.get(),
        LookAroundEvery::TwiceDaily => text::SV_LOOK_EVERY_TWICE.get(),
        LookAroundEvery::Mornings => text::SV_LOOK_EVERY_MORNINGS.get(),
    }
}

fn tidy_every_label(every: TidyEvery) -> &'static str {
    match every {
        TidyEvery::Night => text::SV_TIDY_EVERY_NIGHT.get(),
        TidyEvery::Week => text::SV_TIDY_EVERY_WEEK.get(),
    }
}

/// "Nov 6" (or "11월 6일") for a fact's `YYYY-MM-DD`; anything else as written.
pub(super) fn short_date(ymd: &str) -> String {
    let mut parts = ymd.splitn(3, '-').skip(1).map(str::parse::<usize>);
    let month = |m: usize| text::SV_MONTHS.get().split(',').nth(m.wrapping_sub(1));
    match (parts.next(), parts.next()) {
        (Some(Ok(m)), Some(Ok(d))) if month(m).is_some() => text::SV_SHORT_DATE
            .format(&[("mon", month(m).unwrap_or_default()), ("d", &d.to_string())]),
        _ => ymd.to_owned(),
    }
}

/// "Nov 22 14:44" on this computer's clock.
pub(super) fn when_label(at_ms: u64) -> String {
    let epoch = i64::try_from(at_ms / 1_000).unwrap_or(i64::MAX);
    let day = crate::local_time::format_timestamp(epoch)
        .map(|s| short_date(&s.chars().take(10).collect::<String>()))
        .unwrap_or_default();
    let clock = crate::local_time::format_clock(epoch).unwrap_or_default();
    format!("{day} {clock}")
}

/// What the last tidy-up changed, read from its History row (Orca's `BotTidyUpReview` run).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TidyRun {
    pub at_ms: u64,
    pub merged: usize,
    pub moved: usize,
    pub archive: Option<String>,
    pub undoable: bool,
    pub undone: bool,
}

impl TidyRun {
    pub(super) const fn changes(&self) -> usize {
        self.merged + self.moved
    }
}

/// Krill's tidy-up drops repeats and moves overflow to `memory/archive-YYYY-MM.md`, noting the
/// move in one added line; everything else it removed was a repeat.
pub(super) fn tidy_run(entry: &krill_proto::bots::BotMemoryHistoryEntry) -> TidyRun {
    let (moved, archive) = entry
        .added
        .iter()
        .find_map(|line| {
            let (_, rest) = line.split_once("Archived ")?;
            let (n, rest) = rest.split_once(' ')?;
            let n = n.parse::<usize>().ok()?;
            let archive = rest.split_once("memory/").map(|(_, f)| f.trim().to_owned());
            Some((n, archive))
        })
        .unwrap_or((0, None));
    TidyRun {
        at_ms: entry.timestamp_ms,
        merged: entry.removed.len().saturating_sub(moved),
        moved,
        archive,
        undoable: entry.undoable,
        undone: entry.undone,
    }
}

impl View {
    pub(super) fn render_settings_subpage(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        match self.settings.page {
            SettingsPage::Root => self.render_settings_root(bot, p, cx),
            SettingsPage::Agent => self.render_agent_page(bot, p, cx),
            SettingsPage::Permissions => Self::render_permissions_page(bot, p, cx),
            SettingsPage::Computer => self.render_computer_page(bot, p, cx),
            SettingsPage::LookAround => self.render_look_around_page(bot, p, window, cx),
            SettingsPage::TidyUp => self.render_tidy_up_page(bot, p, window, cx),
            SettingsPage::Schedules => self.render_schedules_page(bot, p, window, cx),
            SettingsPage::Soul => self.render_soul_page(bot, p, window, cx),
            SettingsPage::Memory => self.render_memory_page(bot, p, window, cx),
            SettingsPage::WhoCanAsk => self.render_who_can_ask_page(bot, p, window, cx),
            SettingsPage::Tools => self.render_tools_page(bot, p, window, cx),
            SettingsPage::Skills => Self::render_skills_page(bot, p, cx),
            SettingsPage::Sharing => self.render_sharing_page(bot, p, cx),
        }
    }

    /// Orca's `SettingsSubsectionHeader`: a 14 px title, a muted line and an action.
    pub(super) fn subsection_header(
        title: &str,
        description: Option<SharedString>,
        action: Option<AnyElement>,
        p: &ui::Palette,
    ) -> gpui::Div {
        div()
            .w_full()
            .flex()
            .items_start()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.foreground)
                            .child(title.to_owned()),
                    )
                    .children(description.map(|d| {
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(d)
                    })),
            )
            .children(action.map(|a| div().flex_none().child(a)))
    }

    /// Orca's `SettingsRow`: label and description, the control beside (or under, when narrow).
    pub(super) fn settings_form_row(
        label: &str,
        description: Vec<SharedString>,
        control: Option<AnyElement>,
        p: &ui::Palette,
    ) -> gpui::Div {
        let has_desc = !description.is_empty();
        div()
            .w_full()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_x_4()
            .gap_y_2()
            .py(if has_desc { px(12.) } else { px(8.) })
            .child(
                div()
                    .min_w(px(128.))
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.foreground)
                            .child(label.to_owned()),
                    )
                    .children(description.into_iter().map(|d| {
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(d)
                    })),
            )
            .children(control.map(|c| div().flex_none().max_w_full().child(c)))
    }

    fn render_agent_page(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_agent_models(&bot.agent_id, cx);
        let runtime = bot.agent_id.clone();
        let menu_runtime = runtime.clone();
        let this = cx.weak_entity();
        let bot_id_rt = bot.id.clone();
        let enabled_runtimes = super::pickers::available_runtimes(self.host.is_local(), cx);
        let runtime_button = Button::new("bot-settings-agent-runtime")
            .debug_selector(|| "bot-settings-agent-runtime".into())
            .outline()
            .small()
            .w_full()
            .dropdown_caret(true)
            .accessibility_label(runtime_display_label(&runtime))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(ui::icons::agent_brand_icon(
                        &runtime,
                        ui::ICON_SM,
                        p.foreground,
                    ))
                    .child(div().truncate().child(runtime_display_label(&runtime))),
            )
            .dropdown_menu(move |mut menu, _, _| {
                let current_canon = krill_proto::agent::canonical_id(&menu_runtime);
                for rt in BOT_RUNTIMES {
                    let is_current = current_canon == rt.id;
                    // Orca #47382: show only enabled/installed agents (always keep the bot's current agent).
                    if !is_current
                        && !enabled_runtimes.is_empty()
                        && !enabled_runtimes.contains(&rt.id)
                    {
                        continue;
                    }
                    let this = this.clone();
                    let bot_id = bot_id_rt.clone();
                    menu = menu.item(
                        PopupMenuItem::new(rt.label())
                            .icon(ui::icons::agent_menu_icon(rt.id))
                            .checked(is_current)
                            .on_click(move |_, _, cx| {
                                this.update(cx, |v, cx| {
                                    let Some(params) = v
                                        .bots
                                        .iter()
                                        .find(|b| {
                                            b.id == bot_id
                                                && krill_proto::agent::canonical_id(&b.agent_id)
                                                    != rt.id
                                        })
                                        .map(|current| UpdateBotParams {
                                            // A new agent starts on its own default model and effort.
                                            agent_id: Some(rt.id.to_owned()),
                                            model: krill_proto::bots::FieldPatch::Clear,
                                            effort: krill_proto::bots::FieldPatch::Clear,
                                            ..patch(current)
                                        })
                                    else {
                                        return;
                                    };
                                    v.patch_bot(params, cx);
                                })
                                .ok();
                            }),
                    );
                }
                menu
            });

        let runtime_canon = krill_proto::agent::canonical_id(&runtime);
        let choices = match &self.settings.agent_models {
            Some((agent, found))
                if krill_proto::agent::canonical_id(agent) == runtime_canon
                    && !found.models.is_empty()
                    && crate::agent_models::prefers_discovered(runtime_canon) =>
            {
                found.models.clone()
            }
            _ => runtime_models(runtime_canon),
        };
        let current = bot.model.clone().filter(|m| !m.trim().is_empty());
        let has_model_override = current.is_some();
        let model_label = current.as_deref().map_or_else(
            || text::MODEL_DEFAULT.get().to_owned(),
            |id| {
                choices
                    .iter()
                    .find(|m| m.id == id)
                    .map_or_else(|| id.to_owned(), |m| m.label.clone())
            },
        );
        let this = cx.weak_entity();
        let bot_id_model = bot.id.clone();
        let model_button = Button::new("bot-settings-agent-model")
            .debug_selector(|| "bot-settings-agent-model".into())
            .outline()
            .small()
            .w_full()
            .dropdown_caret(true)
            .label(model_label)
            .dropdown_menu(move |mut menu, _, _| {
                let pick = |choice: Option<String>| {
                    let this = this.clone();
                    let bot_id = bot_id_model.clone();
                    move |_: &ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                        let choice = choice.clone();
                        this.update(cx, |v, cx| {
                            let Some(params) =
                                v.bots.iter().find(|b| b.id == bot_id).map(|current| {
                                    // Antigravity launch model IDs encode effort (e.g. `gemini-4-neon-high`);
                                    // clear any separate effort override so they stay consistent.
                                    let clear_effort =
                                        krill_proto::agent::canonical_id(&current.agent_id)
                                            == "agy"
                                            && choice.as_deref().is_some_and(|m| {
                                                krill_proto::agent::strip_agy_effort_suffix(m)
                                                    .1
                                                    .is_some()
                                            });
                                    UpdateBotParams {
                                        model: choice.clone().map_or(
                                            krill_proto::bots::FieldPatch::Clear,
                                            krill_proto::bots::FieldPatch::Set,
                                        ),
                                        effort: if clear_effort {
                                            krill_proto::bots::FieldPatch::Clear
                                        } else {
                                            krill_proto::bots::FieldPatch::Unchanged
                                        },
                                        ..patch(current)
                                    }
                                })
                            else {
                                return;
                            };
                            v.patch_bot(params, cx);
                        })
                        .ok();
                    }
                };
                menu = menu.max_h(px(360.)).scrollable(true).item(
                    PopupMenuItem::new(text::MODEL_DEFAULT.get())
                        .checked(current.is_none())
                        .on_click(pick(None)),
                );
                for choice in &choices {
                    menu = menu.item(
                        PopupMenuItem::new(choice.label.clone())
                            .checked(current.as_deref() == Some(choice.id.as_str()))
                            .on_click(pick(Some(choice.id.clone()))),
                    );
                }
                menu
            });
        let effort_options = effort_options_for_runtime(runtime_canon);
        let effort = bot.effort.clone().unwrap_or_default();
        let bot_id_effort = bot.id.clone();
        let ent = cx.entity();
        let effort_select = row_select(
            "bot-settings-agent-effort",
            text::FIELD_EFFORT.get(),
            effort_options
                .iter()
                .position(|e| *e == effort)
                .unwrap_or(0),
            effort_options
                .iter()
                .enumerate()
                .map(|(i, e)| (i, effort_label(e).into()))
                .collect(),
            |b| b.small().w_full(),
            move |i, _, cx| {
                let Some(eff) = effort_options.get(i).copied() else {
                    return;
                };
                ent.update(cx, |this, cx| {
                    let Some(params) =
                        this.bots
                            .iter()
                            .find(|b| b.id == bot_id_effort)
                            .map(|current| UpdateBotParams {
                                effort: if eff.is_empty() {
                                    krill_proto::bots::FieldPatch::Clear
                                } else {
                                    krill_proto::bots::FieldPatch::Set(eff.to_owned())
                                },
                                ..patch(current)
                            })
                    else {
                        return;
                    };
                    this.patch_bot(params, cx);
                });
            },
        );

        let has_overrides = has_model_override || !effort.is_empty();
        let reset_bot = bot.clone();
        let body = div()
            .w_full()
            .rounded(ui::RADIUS_LG)
            .bg(p.foreground.opacity(0.035))
            .px_3()
            .py(px(10.))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(ui::TEXT_MD)
                            .font_weight(FontWeight::MEDIUM)
                            .line_height(px(18.))
                            .text_color(p.foreground)
                            .child(text::SV_AGENT_GROUP.get()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .line_height(px(16.))
                            .text_color(p.muted_foreground)
                            .child(text::SV_AGENT_DESC.get()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(div().flex_1().min_w(px(120.)).child(runtime_button))
                    .child(div().flex_1().min_w(px(120.)).child(model_button))
                    .child(div().flex_1().min_w(px(120.)).child(effort_select)),
            )
            .child(if has_overrides {
                div()
                    .id("bot-settings-agent-use-defaults")
                    .debug_selector(|| "bot-settings-agent-use-defaults".into())
                    .role(gpui::Role::Button)
                    .tab_index(0)
                    .self_start()
                    .cursor_pointer()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .underline()
                    .hover(|d| d.text_color(p.foreground))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let params = UpdateBotParams {
                            model: krill_proto::bots::FieldPatch::Clear,
                            effort: krill_proto::bots::FieldPatch::Clear,
                            ..patch(&reset_bot)
                        };
                        this.patch_bot(params, cx);
                    }))
                    .child(text::NB_USE_CHAT_DEFAULTS.get())
                    .into_any_element()
            } else {
                div()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::NB_USING_CHAT_DEFAULT.get())
                    .into_any_element()
            });
        Self::settings_screen("bot-settings-agent-page").child(body)
    }

    /// The agent's own model list, as the chat composer and New bot show it.
    pub(super) fn ensure_agent_models(&mut self, agent_id: &str, cx: &mut Context<Self>) {
        let canon = krill_proto::agent::canonical_id(agent_id);
        if self
            .settings
            .agent_models
            .as_ref()
            .is_some_and(|(a, _)| krill_proto::agent::canonical_id(a) == canon)
            || self.settings.models_loading_for.as_deref() == Some(canon)
        {
            return;
        }
        let rpc = self.rpc.clone();
        let req_host = self.host.clone();
        let agent = canon.to_owned();
        self.settings.models_loading_for = Some(agent.clone());
        self.settings.models_task = Some(cx.spawn(async move |this, cx| {
            let query = agent.clone();
            let reply = cx
                .background_executor()
                .spawn(async move {
                    call_checked(&rpc, &BotRpcRequest::ListAgentModels { agent_id: query })
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host != req_host
                    || this.settings.models_loading_for.as_deref() != Some(agent.as_str())
                {
                    return;
                }
                this.settings.models_loading_for = None;
                this.settings.models_task = None;
                let found = match reply {
                    Ok(BotRpcResponse::AgentModels(models)) => models,
                    _ => BotAgentModels::default(),
                };
                this.settings.agent_models = Some((agent, found));
                cx.notify();
            })
            .ok();
        }));
    }

    fn render_permissions_page(
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let rows = [
            (
                Permission::Manual,
                "bot-settings-permission-ask",
                ui::icons::HAND_SVG,
                chat_text::PERMISSION_ASK_DESC.get(),
            ),
            (
                Permission::Yolo,
                "bot-settings-permission-full",
                ui::icons::SHIELD_ALERT_SVG,
                chat_text::PERMISSION_FULL_ACCESS_DESC.get(),
            ),
        ]
        .into_iter()
        .map(|(mode, id, icon, desc)| {
            let on = bot.permission_mode == mode;
            let bot_id = bot.id.clone();
            Row::new(id, Some(icon), Self::permission_label(mode))
                .detail(desc)
                .wrap_detail()
                .end(div().w(px(16.)).when(on, |d| {
                    d.child(
                        Icon::new(IconName::Check)
                            .size(px(ui::ICON_LG))
                            .text_color(p.foreground),
                    )
                }))
                .opens(Open::run(move |this, _, cx| {
                    if let Some(params) = this
                        .bots
                        .iter()
                        .find(|b| b.id == bot_id && b.permission_mode != mode)
                        .map(|current| UpdateBotParams {
                            permission_mode: Some(mode),
                            ..patch(current)
                        })
                    {
                        this.patch_bot(params, cx);
                    }
                }))
        })
        .collect();
        Self::settings_screen("bot-settings-permissions-page").child(Self::render_inset_group(
            None,
            None,
            rows,
            Some(text::SV_PERMISSIONS_FOOT.get().into()),
            false,
            p,
            cx,
        ))
    }

    /// The bot's folder: its custom workspace, else its home under the bots folder.
    pub(super) fn bot_folder_path(&self, bot: &BotConfig) -> Option<std::path::PathBuf> {
        bot.workspace
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                self.host
                    .is_local()
                    .then(krill_proto::daemon::state_dir)
                    .flatten()
                    .map(|state| state.join("bots").join(&bot.folder))
            })
    }

    fn render_computer_page(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let local = self.host.is_local();
        let name = computer_name(self);
        let path = self.bot_folder_path(bot);
        let shown = path.as_ref().map_or_else(
            || format!("~/…/bots/{}", bot.folder),
            |p| match krill_proto::os::home_dir() {
                Some(home) => krill_proto::bots::tilde_display(p, &home),
                None => p.display().to_string(),
            },
        );
        let mut folder = Row::new(
            "bot-settings-computer-folder",
            Some(ui::icons::FOLDER_SVG),
            text::SV_FOLDER.get(),
        )
        .detail(shown);
        if let Some(path) = path {
            folder = folder.end(
                Button::new("bot-settings-copy-path")
                    .debug_selector(|| "bot-settings-copy-path".into())
                    .ghost()
                    .small()
                    .icon(IconName::Copy)
                    .tooltip(text::SV_COPY_PATH.get())
                    .accessibility_label(text::SV_COPY_PATH.get())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                            path.to_string_lossy().into_owned(),
                        ));
                        this.notice = Some(text::SV_PATH_COPIED.get().into());
                        cx.notify();
                    })),
            );
        }
        let rows = vec![
            Row::new(
                "bot-settings-computer-name",
                Some(if local {
                    ui::icons::LAPTOP_SVG
                } else {
                    ui::icons::SERVER_SVG
                }),
                name.clone(),
            )
            .detail(if local {
                text::SV_THIS_COMPUTER.get()
            } else {
                text::SV_PAIRED_SERVER.get()
            }),
            folder,
        ];
        let foot = if local {
            text::SV_COMPUTER_FOOT.get().to_owned()
        } else {
            text::SV_COMPUTER_HOST_FOOT.format(&[("host", &name)])
        };
        Self::settings_screen("bot-settings-computer-page").child(Self::render_inset_group(
            None,
            None,
            rows,
            Some(foot.into()),
            false,
            p,
            cx,
        ))
    }

    /// Look for: saved on Enter and when focus leaves, as Orca's field does.
    fn ensure_look_focus_input(
        &mut self,
        bot: &BotConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.settings.look_focus_input.clone() {
            crate::i18n::sync_input_placeholder(
                &input,
                text::SV_LOOK_FOCUS_PLACEHOLDER.get(),
                window,
                cx,
            );
            let next = bot.look_around.focus.as_str();
            let last_synced = self.settings.look_focus_synced.as_deref();
            if last_synced != Some(next) {
                let dirty =
                    last_synced.is_some_and(|prev| input.read(cx).value().trim() != prev.trim());
                if !dirty {
                    let next_owned = next.to_owned();
                    input.update(cx, |st, cx| {
                        st.set_value(next_owned.clone(), window, cx);
                    });
                    self.settings.look_focus_synced = Some(next_owned);
                }
            }
            return;
        }
        let focus = bot.look_around.focus.clone();
        self.settings.look_focus_synced = Some(focus.clone());
        let input = cx.new(|cx| {
            let mut st =
                InputState::new(window, cx).placeholder(text::SV_LOOK_FOCUS_PLACEHOLDER.get());
            if !focus.is_empty() {
                st.set_value(focus, window, cx);
            }
            st
        });
        self.settings.look_focus_sub =
            Some(
                cx.subscribe_in(&input, window, |this, ent, ev: &InputEvent, window, cx| {
                    match ev {
                        InputEvent::PressEnter {
                            secondary: false,
                            shift: false,
                        } => {
                            if crate::jump::entity_input_is_composing(ent, window, cx) {
                                return;
                            }
                        }
                        InputEvent::Blur => {}
                        _ => return,
                    }
                    let typed = ent.read(cx).value().trim().to_owned();
                    let Some(bot) = this.selected_bot().cloned() else {
                        return;
                    };
                    if typed != bot.look_around.focus {
                        this.settings.look_focus_synced = Some(typed.clone());
                        let mut look = bot.look_around.clone();
                        look.focus = typed;
                        this.patch_bot(
                            UpdateBotParams {
                                look_around: Some(look),
                                ..patch(&bot)
                            },
                            cx,
                        );
                    }
                }),
            );
        self.settings.look_focus_input = Some(input);
    }

    fn render_look_around_page(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_look_focus_input(bot, window, cx);
        let look = bot.look_around.clone();
        let ent = cx.entity();
        let switch_bot_id = bot.id.clone();
        let header = Self::subsection_header(
            text::SV_LOOK_AROUND.get(),
            Some(if look.enabled {
                look_around_every_label(self.settings_extras(&bot.id).look_around_every).into()
            } else {
                text::SV_OFF.get().into()
            }),
            Some(
                row_switch(
                    p,
                    "bot-look-around-switch",
                    text::SV_LOOK_AROUND.get(),
                    look.enabled,
                    false,
                    move |on, _, cx| {
                        ent.update(cx, |this, cx| {
                            let Some(params) = this
                                .bots
                                .iter()
                                .find(|b| b.id == switch_bot_id)
                                .map(|current| {
                                    let mut look = current.look_around.clone();
                                    look.enabled = on;
                                    UpdateBotParams {
                                        look_around: Some(look),
                                        ..patch(current)
                                    }
                                })
                            else {
                                return;
                            };
                            this.patch_bot(params, cx);
                        });
                    },
                )
                .into_any_element(),
            ),
            p,
        );
        let mut section = div().w_full().flex().flex_col().gap_4().child(header);
        if !look.enabled {
            section = section.child(
                div()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::SV_LOOK_ON_DESC.get()),
            );
            return Self::settings_screen("bot-settings-look-around-page").child(section);
        }
        let ent = cx.entity();
        let every_bot_id = bot.id.clone();
        let every = row_select(
            "bot-look-around-every",
            text::SV_HOW_OFTEN.get(),
            self.settings_extras(&bot.id).look_around_every,
            LookAroundEvery::ALL
                .into_iter()
                .map(|e| (e, look_around_every_label(e).into()))
                .collect(),
            |b| b.small().w(px(256.)),
            move |every, _, cx| {
                ent.update(cx, |this, cx| {
                    this.update_settings_extras(
                        every_bot_id.clone(),
                        Extras {
                            look_around_every: Some(every),
                            ..Extras::default()
                        },
                        cx,
                    );
                });
            },
        );
        let ent = cx.entity();
        let weekdays_bot_id = bot.id.clone();
        let weekdays = row_switch(
            p,
            "bot-look-around-weekdays",
            text::SV_WEEKDAYS.get(),
            look.weekdays_only,
            false,
            move |on, _, cx| {
                ent.update(cx, |this, cx| {
                    let Some(params) =
                        this.bots
                            .iter()
                            .find(|b| b.id == weekdays_bot_id)
                            .map(|current| {
                                let mut look = current.look_around.clone();
                                look.weekdays_only = on;
                                UpdateBotParams {
                                    look_around: Some(look),
                                    ..patch(current)
                                }
                            })
                    else {
                        return;
                    };
                    this.patch_bot(params, cx);
                });
            },
        );
        let running = self.soul_memory.is_looking_around(&bot.id);
        let latest = self
            .soul_memory
            .suggestions
            .iter()
            .map(|s| s.created_at_ms)
            .max();
        let last_line = latest.map_or_else(
            || text::SV_LOOK_NOT_YET.get().to_owned(),
            |at| {
                format!(
                    "{} · {}",
                    when_label(at),
                    text::SV_LOOK_SUGGESTIONS
                        .format(&[("n", &self.soul_memory.suggestions.len().to_string())])
                )
            },
        );
        let look_bot = bot.id.clone();
        let look_now = ui::secondary(
            "bot-look-around-now",
            if running {
                text::SV_LOOKING.get()
            } else {
                text::SV_LOOK_NOW.get()
            },
        )
        .debug_selector(|| "bot-look-around-now".into())
        .small()
        .disabled(running || bot.paused)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.run_look_around(look_bot.clone(), cx);
        }));
        let every_desc: SharedString = text::SV_LOOK_EVERY_DESC
            .format(&[("agent", &runtime_display_label(&bot.agent_id))])
            .into();
        section = section
            .child(Self::settings_form_row(
                text::SV_HOW_OFTEN.get(),
                vec![every_desc],
                Some(every.into_any_element()),
                p,
            ))
            .child(Self::settings_form_row(
                text::SV_WEEKDAYS.get(),
                Vec::new(),
                Some(weekdays.into_any_element()),
                p,
            ))
            .child(
                div()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.foreground)
                            .child(text::SV_LOOK_FOCUS.get()),
                    )
                    .children(
                        self.settings
                            .look_focus_input
                            .as_ref()
                            .map(|i| Input::new(i).small()),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(text::SV_LOOK_FOCUS_DESC.get()),
                    ),
            )
            .child(Self::settings_form_row(
                text::SV_LAST_LOOKED.get(),
                vec![last_line.into(), text::SV_WHILE_RUNNING.get().into()],
                Some(look_now.into_any_element()),
                p,
            ));
        Self::settings_screen("bot-settings-look-around-page").child(section)
    }

    /// The last tidy-up as its History row tells it, once History is read.
    pub(super) fn last_tidy_run(&self) -> Option<TidyRun> {
        let id = self
            .soul_memory
            .tidy_status
            .as_ref()?
            .last_tidy_id
            .as_deref()?;
        self.soul_memory
            .memory_history
            .iter()
            .find(|e| e.entry_id == id)
            .map(tidy_run)
    }

    fn render_tidy_up_page(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let tidy = bot.memory_tidy;
        let status = self.soul_memory.tidy_status.clone().unwrap_or_default();
        let last = self.last_tidy_run();
        let line = if tidy.enabled {
            let every = match self.settings_extras(&bot.id).tidy_every {
                TidyEvery::Night => text::SV_TIDY_NIGHT_LINE.get(),
                TidyEvery::Week => text::SV_TIDY_WEEK_LINE.get(),
            };
            match status.last_run_ms {
                Some(at) => format!(
                    "{every} · {}",
                    text::SV_LAST_TIDIED_WHEN.format(&[("when", &when_label(at))])
                ),
                None => every.to_owned(),
            }
        } else {
            text::SV_OFF.get().to_owned()
        };
        let ent = cx.entity();
        let switch_bot_id = bot.id.clone();
        let header = Self::subsection_header(
            text::SV_MEMORY_UPKEEP.get(),
            Some(line.into()),
            Some(
                row_switch(
                    p,
                    "bot-tidy-up-switch",
                    text::SV_MEMORY_UPKEEP.get(),
                    tidy.enabled,
                    false,
                    move |on, _, cx| {
                        ent.update(cx, |this, cx| {
                            let Some(params) = this
                                .bots
                                .iter()
                                .find(|b| b.id == switch_bot_id)
                                .map(|current| {
                                    let mut tidy = current.memory_tidy;
                                    tidy.enabled = on;
                                    UpdateBotParams {
                                        memory_tidy: Some(tidy),
                                        ..patch(current)
                                    }
                                })
                            else {
                                return;
                            };
                            this.patch_bot(params, cx);
                        });
                    },
                )
                .into_any_element(),
            ),
            p,
        );
        let mut section = div().w_full().flex().flex_col().gap_4().child(header);
        if status.auto_disabled || !tidy.enabled {
            section = section.child(
                div()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(if status.auto_disabled {
                        text::SV_TIDY_TURNED_OFF.get()
                    } else {
                        text::SV_TIDY_ON_DESC.get()
                    }),
            );
        }
        if tidy.enabled {
            let ent = cx.entity();
            let every_bot = bot.clone();
            let every = row_select(
                "bot-tidy-up-every",
                text::SV_HOW_OFTEN.get(),
                self.settings_extras(&bot.id).tidy_every,
                [TidyEvery::Night, TidyEvery::Week]
                    .into_iter()
                    .map(|e| (e, tidy_every_label(e).into()))
                    .collect(),
                |b| b.small().w(px(256.)),
                move |every, _, cx| {
                    ent.update(cx, |this, cx| {
                        this.update_settings_extras(
                            every_bot.id.clone(),
                            Extras {
                                tidy_every: Some(every),
                                ..Extras::default()
                            },
                            cx,
                        );
                    });
                },
            );
            let running = self.soul_memory.tidy_running;
            let last_line: AnyElement = match &last {
                Some(run) => div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .child(format!(
                        "{} · {} ·",
                        when_label(run.at_ms),
                        text::SV_TIDY_CHANGES.format(&[("n", &run.changes().to_string())])
                    ))
                    .child(
                        div()
                            .id("bot-tidy-up-review")
                            .debug_selector(|| "bot-tidy-up-review".into())
                            .underline()
                            .cursor_pointer()
                            .hover(|d| d.text_color(p.foreground))
                            .child(text::SV_TIDY_REVIEW.get())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.settings.tidy_review = true;
                                this.focus.focus(window, cx);
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
                None => text::SV_TIDY_NEVER.get().into_any_element(),
            };
            let tidy_bot = bot.id.clone();
            let tidy_now = ui::secondary(
                "bot-tidy-up-now",
                if running {
                    text::SV_TIDYING.get()
                } else {
                    text::SV_TIDY_NOW.get()
                },
            )
            .debug_selector(|| "bot-tidy-up-now".into())
            .small()
            .disabled(running || bot.paused)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.run_memory_tidy(tidy_bot.clone(), cx);
            }));
            let last_row = div()
                .w_full()
                .py_3()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_x_4()
                .gap_y_2()
                .child(
                    div()
                        .min_w(px(128.))
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(ui::TEXT_LG)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.foreground)
                                .child(text::SV_LAST_TIDIED.get()),
                        )
                        .child(
                            div()
                                .text_size(ui::TEXT_SM)
                                .text_color(p.muted_foreground)
                                .child(last_line)
                                .when_some(
                                    self.soul_memory
                                        .last_tidy_summary
                                        .clone()
                                        .filter(|_| !status.auto_disabled && last.is_none()),
                                    gpui::ParentElement::child,
                                )
                                .child(text::SV_WHILE_RUNNING.get()),
                        ),
                )
                .child(div().flex_none().child(tidy_now));
            section = section
                .child(Self::settings_form_row(
                    text::SV_HOW_OFTEN.get(),
                    vec![text::SV_TIDY_EVERY_DESC.get().into()],
                    Some(every.into_any_element()),
                    p,
                ))
                .child(last_row);
        }
        let screen = Self::settings_screen("bot-settings-tidy-up-page").child(section);
        match last.filter(|_| self.settings.tidy_review) {
            Some(run) => screen.child(self.render_tidy_review(bot, &run, p, window, cx)),
            None => screen,
        }
    }

    /// Orca's "Last night's tidy-up" dialog: what it changed, and Undo for the whole run.
    fn render_tidy_review(
        &self,
        bot: &BotConfig,
        run: &TidyRun,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hour = crate::local_time::format_clock(i64::try_from(run.at_ms / 1_000).unwrap_or(0))
            .and_then(|c| c.split(':').next().and_then(|h| h.parse::<u32>().ok()))
            .unwrap_or(12);
        let title = if hour < 9 {
            text::SV_TIDY_REVIEW_TITLE_NIGHT.get()
        } else {
            text::SV_TIDY_REVIEW_TITLE.get()
        };
        let mut tidied: Vec<String> = Vec::new();
        if run.merged > 0 {
            tidied.push(text::SV_TIDY_MERGED.format(&[("n", &run.merged.to_string())]));
        }
        if run.moved > 0 {
            tidied.push(text::SV_TIDY_MOVED.format(&[
                ("n", &run.moved.to_string()),
                ("file", run.archive.as_deref().unwrap_or("archive.md")),
            ]));
        }
        let undo_bot = bot.id.clone();
        let dialog = div()
            .id("bot-tidy-review")
            .debug_selector(|| "bot-tidy-review".into())
            .w(px(512.))
            .max_w_full()
            .rounded(ui::RADIUS_LG)
            .border_1()
            .border_color(p.border)
            .bg(p.popover)
            .shadow_lg()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_size(ui::TEXT_XL)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.foreground)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .text_color(p.muted_foreground)
                            .child(when_label(run.at_ms)),
                    ),
            )
            .when(!tidied.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(
                            div()
                                .text_size(ui::TEXT_SM)
                                .text_color(p.muted_foreground)
                                .child(text::SV_TIDY_TIDIED.get()),
                        )
                        .children(tidied.into_iter().map(|line| {
                            div()
                                .text_size(ui::TEXT_LG)
                                .text_color(p.foreground)
                                .child(line)
                        })),
                )
            })
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(if run.undone {
                        text::SV_TIDY_UNDONE.get()
                    } else {
                        text::SV_TIDY_UNDO_NOTE.get()
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        Button::new("bot-tidy-review-undo")
                            .debug_selector(|| "bot-tidy-review-undo".into())
                            .ghost()
                            .small()
                            .icon(IconName::Undo2)
                            .label(text::SV_TIDY_UNDO_ALL.get())
                            .disabled(run.undone || !run.undoable)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.undo_memory_tidy(undo_bot.clone(), cx);
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                ui::secondary(
                                    "bot-tidy-review-history",
                                    text::SV_TIDY_OPEN_HISTORY.get(),
                                )
                                .small()
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.open_settings_page(SettingsPage::Memory, cx);
                                        this.settings.memory_history_tab = true;
                                    },
                                )),
                            )
                            .child(
                                ui::primary("bot-tidy-review-done", text::SV_TIDY_DONE.get())
                                    .debug_selector(|| "bot-tidy-review-done".into())
                                    .small()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings.tidy_review = false;
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        ui::modal_backdrop(
            window.viewport_size(),
            ui::modal_focus_scope(
                div()
                    .id("bot-tidy-review-backdrop")
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_4()
                    .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "escape" && !ev.keystroke.modifiers.modified() {
                            this.settings.tidy_review = false;
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings.tidy_review = false;
                        cx.notify();
                    }))
                    .child(dialog),
                &self.focus,
            ),
        )
        .into_any_element()
    }

    fn render_who_can_ask_page(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_connector_inputs(bot, window, cx);
        let mut screen = Self::settings_screen("bot-settings-who-can-ask-page");
        if self.connectors.bot_access_requests.iter().any(|r| {
            r.bot_id == bot.id && r.status == krill_proto::bots::BotAccessRequestStatus::Pending
        }) {
            screen = screen.child(self.render_access_requests_card(&bot.id, p, cx));
        }
        // One list per place people reach the bot: each Slack workspace, Telegram, Discord….
        let mut places: Vec<(ExternalSource, Option<String>)> = Vec::new();
        for m in bot
            .members
            .iter()
            .filter(|m| m.source != ExternalSource::Google)
        {
            let key = (m.source, m.workspace_id.clone());
            if !places.contains(&key) {
                places.push(key);
            }
        }
        for source in bot.connectors.enabled_sources() {
            if source != ExternalSource::Google
                && source != ExternalSource::Composio
                && !places.iter().any(|(s, _)| *s == source)
            {
                places.push((source, None));
            }
        }
        if places.is_empty() {
            return screen.child(Self::render_inset_group(
                None,
                None,
                vec![
                    Row::new(
                        "bot-who-can-ask-none",
                        Some(ui::icons::PLUG_SVG),
                        text::SV_NO_CONNECTORS.get(),
                    )
                    .detail(text::SV_NO_CONNECTORS_DESC.format(&[("name", &bot.name)]))
                    .wrap_detail()
                    .opens(Open::Tab(super::DetailTab::Connectors)),
                ],
                None,
                false,
                p,
                cx,
            ));
        }
        let busy = self.patch_task.is_some();
        for (place_idx, (source, workspace)) in places.into_iter().enumerate() {
            let people: Vec<&BotMember> = bot
                .members
                .iter()
                .filter(|m| m.source == source && m.workspace_id == workspace)
                .collect();
            let title = workspace.as_deref().map_or_else(
                || source_label(source).to_owned(),
                |w| format!("{} · {w}", source_label(source)),
            );
            let list = Self::who_can_ask_card(bot, place_idx, &people, busy, p, cx);
            screen = screen.child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .px_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(ui::TEXT_SM)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.muted_foreground)
                            .child(ui::icons::svg_icon(
                                ui::icons::PLUG_SVG,
                                ui::ICON_SM,
                                p.muted_foreground,
                            ))
                            .child(div().truncate().child(title)),
                    )
                    .child(
                        div()
                            .rounded(ui::RADIUS_LG)
                            .bg(p.foreground.opacity(0.035))
                            .p_3()
                            .child(list),
                    ),
            );
        }
        screen
    }

    /// Orca's `BotWhoCanAskList` for one connector: who can ask, as one sentence and chips, then
    /// Before you share (Keep only me, Ask for approval).
    fn who_can_ask_card(
        bot: &BotConfig,
        place_idx: usize,
        people: &[&BotMember],
        busy: bool,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let name = |m: &BotMember| {
            if m.name.trim().is_empty() {
                m.id.clone()
            } else {
                m.name.clone()
            }
        };
        let names: Vec<String> = people.iter().map(|m| name(m)).collect();
        let summary = match names.as_slice() {
            [] => text::SV_WHO_SUMMARY_ONLY.format(&[("name", &bot.name)]),
            [one] => text::SV_WHO_SUMMARY_ONE.format(&[("person", one), ("name", &bot.name)]),
            [rest @ .., last] if names.len() <= 3 => text::SV_WHO_SUMMARY_SOME.format(&[
                ("people", &rest.join(", ")),
                ("last", last),
                ("name", &bot.name),
            ]),
            _ => text::SV_WHO_SUMMARY_MANY
                .format(&[("n", &names.len().to_string()), ("name", &bot.name)]),
        };
        let control = if people.is_empty() {
            text::SV_WHO_CONTROL_NONE.format(&[("name", &bot.name)])
        } else {
            text::SV_WHO_CONTROL_MANY.format(&[("name", &bot.name)])
        };
        let chip = |id: SharedString, face: AnyElement, label: String| {
            div()
                .id(id)
                .h(px(28.))
                .pl_1()
                .pr(px(10.))
                .flex()
                .items_center()
                .gap_1p5()
                .rounded_full()
                .border_1()
                .border_color(p.border)
                .text_size(ui::TEXT_MD)
                .text_color(p.foreground)
                .child(face)
                .child(label)
        };
        let face = |s: String| {
            div()
                .size(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(p.foreground.opacity(0.06))
                .text_size(ui::TEXT_2XS)
                .text_color(p.muted_foreground)
                .child(s)
                .into_any_element()
        };
        let mut chips = div().flex().flex_wrap().gap_2().child(
            chip(
                SharedString::from(format!("bot-who-you-{place_idx}")),
                div()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(IconName::User)
                            .size(px(ui::ICON_SM))
                            .text_color(p.muted_foreground),
                    )
                    .into_any_element(),
                text::SV_WHO_YOU.get().to_owned(),
            )
            .child(
                div()
                    .text_color(p.muted_foreground)
                    .child(format!("· {}", text::SV_WHO_OWNER.get())),
            ),
        );
        for (member_idx, m) in people.iter().enumerate() {
            let initials: String = name(m)
                .split_whitespace()
                .filter_map(|w| w.chars().next())
                .take(2)
                .collect::<String>()
                .to_uppercase();
            let bot_id = bot.id.clone();
            let member = (*m).clone();
            chips = chips.child(
                chip(
                    SharedString::from(format!("bot-who-{place_idx}-{member_idx}-{}", m.id)),
                    face(initials),
                    name(m),
                )
                .child(
                    Button::new(SharedString::from(format!(
                        "bot-who-can-ask-remove-{place_idx}-{member_idx}-{}",
                        m.id
                    )))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .tooltip(text::SV_REMOVE.get())
                    .accessibility_label(text::SV_REMOVE.get())
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_bot_members(bot_id.clone(), vec![member.clone()], cx);
                    })),
                ),
            );
        }
        let mut advice: Vec<AnyElement> = Vec::new();
        let fix = |icon: &'static [u8], title: String, detail: String, action: AnyElement| {
            div()
                .flex()
                .items_start()
                .gap(px(10.))
                .child(
                    Icon::default()
                        .data(icon)
                        .size(px(ui::ICON_LG))
                        .mt_0p5()
                        .text_color(p.muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(ui::TEXT_MD)
                                .text_color(p.foreground)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(ui::TEXT_SM)
                                .text_color(p.muted_foreground)
                                .child(detail),
                        ),
                )
                .child(div().flex_none().child(action))
                .into_any_element()
        };
        if !people.is_empty() {
            let bot_id = bot.id.clone();
            let all: Vec<BotMember> = people.iter().map(|m| (*m).clone()).collect();
            advice.push(fix(
                ui::icons::USERS_SVG,
                text::SV_WHO_FIX_ONLY_ME.format(&[("name", &bot.name)]),
                text::SV_WHO_FIX_ONLY_ME_DETAIL.get().to_owned(),
                ui::secondary(
                    SharedString::from(format!("bot-who-keep-only-me-{place_idx}")),
                    text::SV_WHO_FIX_ONLY_ME_ACTION.get(),
                )
                .small()
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.remove_bot_members(bot_id.clone(), all.clone(), cx);
                }))
                .into_any_element(),
            ));
        }
        if !people.is_empty() && bot.permission_mode == Permission::Yolo {
            let ask_bot_id = bot.id.clone();
            advice.push(fix(
                ui::icons::HAND_SVG,
                text::SV_WHO_FIX_ASK.get().to_owned(),
                text::SV_WHO_FIX_ASK_DETAIL.format(&[("name", &bot.name)]),
                ui::secondary(
                    SharedString::from(format!("bot-who-switch-ask-{place_idx}")),
                    text::SV_WHO_FIX_ASK_ACTION.get(),
                )
                .small()
                .disabled(busy)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(params) =
                        this.bots
                            .iter()
                            .find(|b| b.id == ask_bot_id)
                            .map(|current| UpdateBotParams {
                                permission_mode: Some(Permission::Manual),
                                ..patch(current)
                            })
                    {
                        this.patch_bot(params, cx);
                    }
                }))
                .into_any_element(),
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.muted_foreground)
                            .child(text::SV_WHO_HEADING.format(&[("name", &bot.name)])),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_LG)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.foreground)
                            .child(summary),
                    )
                    .child(
                        div()
                            .text_size(ui::TEXT_SM)
                            .text_color(p.muted_foreground)
                            .child(control),
                    ),
            )
            .child(chips)
            .when(!advice.is_empty(), |d| {
                d.child(
                    div()
                        .rounded(ui::RADIUS_MD)
                        .border_1()
                        .border_color(p.border)
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_size(ui::TEXT_SM)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.muted_foreground)
                                .child(text::SV_WHO_ADVICE.get()),
                        )
                        .children(advice),
                )
            })
    }

    fn render_skills_page(
        bot: &BotConfig,
        p: &ui::Palette,
        _cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let header = Self::subsection_header(
            text::SV_SKILLS.get(),
            Some(text::SV_SKILLS_DESC.get().into()),
            None,
            p,
        );
        let count_label = if bot.skills.is_empty() {
            text::SV_NONE.get().to_owned()
        } else {
            text::SV_COUNT.format(&[("n", &bot.skills.len().to_string())])
        };
        let mut card = div()
            .w_full()
            .overflow_hidden()
            .rounded(ui::RADIUS_XL)
            .border_1()
            .border_color(p.border)
            .bg(p.card)
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(32.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(ui::RADIUS_MD)
                            .bg(p.muted)
                            .child(
                                Icon::default()
                                    .data(ui::icons::BOT_SVG)
                                    .size(px(ui::ICON_LG))
                                    .text_color(p.muted_foreground),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui::TEXT_LG)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.foreground)
                                    .child(text::SV_SKILLS_OWN.format(&[("name", &bot.name)])),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .text_color(p.muted_foreground)
                                    .child(if bot.skills.is_empty() {
                                        text::SV_SKILLS_NONE_DESC.get().to_owned()
                                    } else {
                                        count_label.clone()
                                    }),
                            ),
                    )
                    .when(bot.skills.is_empty(), |d| {
                        d.child(
                            div()
                                .flex_none()
                                .text_size(ui::TEXT_SM)
                                .text_color(p.muted_foreground)
                                .child(count_label.clone()),
                        )
                    }),
            );
        if !bot.skills.is_empty() {
            let mut list = div()
                .w_full()
                .border_t_1()
                .border_color(p.border)
                .px_4()
                .py_2p5()
                .flex()
                .flex_col()
                .gap_2();
            for s in &bot.skills {
                let desc = krill_proto::bots::find_bot_skill(s).map(|sk| {
                    super::templates::localized_skill_description(sk.name, sk.description)
                });
                list = list.child(
                    div()
                        .id(SharedString::from(format!("bot-skill-{s}")))
                        .flex()
                        .items_start()
                        .gap_2p5()
                        .child(
                            Icon::default()
                                .data(ui::icons::BOOK_OPEN_SVG)
                                .size(px(ui::ICON_MD))
                                .mt(px(2.))
                                .text_color(p.muted_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_size(ui::TEXT_MD)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.foreground)
                                        .child(super::templates::localized_skill_title(s)),
                                )
                                .children(desc.map(|d| {
                                    div()
                                        .text_size(ui::TEXT_SM)
                                        .text_color(p.muted_foreground)
                                        .child(d)
                                })),
                        ),
                );
            }
            card = card.child(list);
        }
        Self::settings_screen("bot-settings-skills-page")
            .child(div().flex().flex_col().gap_3().child(header).child(card))
    }

    fn render_sharing_page(
        &self,
        bot: &BotConfig,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let export_bot = bot.id.clone();
        let share = vec![
            Row::new(
                "bot-sharing-export",
                Some(ui::icons::SHARE_2_SVG),
                text::SV_SHARE_FILE.get(),
            )
            .detail(text::SV_SHARE_FILE_DESC.get())
            .wrap_detail()
            .end(
                ui::secondary("bots-sharing-export", text::SV_SHARE_BOT.get())
                    .debug_selector(|| "bots-sharing-export".into())
                    .small()
                    .disabled(self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.export_bot_zip(export_bot.clone(), cx);
                    })),
            ),
        ];
        let receive = vec![
            Row::new(
                "bot-sharing-import-zip",
                Some(ui::icons::DOWNLOAD_SVG),
                text::SV_IMPORT_FILE.get(),
            )
            .opens(Open::run(|this, _, cx| this.import_bot_zip_from_dialog(cx))),
            Row::new(
                "bot-sharing-import-external",
                Some(ui::icons::FOLDER_PLUS_SVG),
                text::IMPORT_EXTERNAL_TITLE.get(),
            )
            .opens(Open::run(|this, _, cx| this.open_import(cx))),
        ];
        Self::settings_screen("bot-settings-sharing-page")
            .child(Self::render_inset_group(
                None, None, share, None, false, p, cx,
            ))
            .child(Self::render_inset_group(
                None, None, receive, None, false, p, cx,
            ))
    }

    /// Orca's `BotToolsSection`: the bot's own tools in one card, then Add a tool, which opens
    /// Krill's catalog and custom server form under it.
    fn render_tools_page(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let tools: Vec<_> = bot.mcp_tools.iter().filter(|t| t.enabled).collect();
        let line = if tools.is_empty() {
            text::SV_NONE.get().to_owned()
        } else {
            tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let row = |id: SharedString,
                   icon: AnyElement,
                   dashed_icon: bool,
                   title: SharedString,
                   detail: Option<String>| {
            div()
                .id(id)
                .w_full()
                .px_4()
                .py_3()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .size(px(32.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(ui::RADIUS_MD)
                        .map(|d| {
                            if dashed_icon {
                                d.border_1()
                                    .border_dashed()
                                    .border_color(p.muted_foreground.opacity(0.45))
                            } else {
                                d.bg(p.muted)
                            }
                        })
                        .child(icon),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_size(ui::TEXT_LG)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(p.foreground)
                                .child(title),
                        )
                        .children(detail.map(|d| {
                            div()
                                .truncate()
                                .text_size(ui::TEXT_SM)
                                .text_color(p.muted_foreground)
                                .child(d)
                        })),
                )
        };
        let wrench = || {
            Icon::default()
                .data(ui::icons::WRENCH_SVG)
                .size(px(ui::ICON_LG))
                .text_color(p.muted_foreground)
                .into_any_element()
        };
        let mut card = div()
            .w_full()
            .overflow_hidden()
            .rounded(ui::RADIUS_XL)
            .border_1()
            .border_color(p.border)
            .bg(p.card)
            .flex()
            .flex_col();
        for (i, t) in tools.iter().enumerate() {
            let detail = t
                .endpoint_url
                .clone()
                .or_else(|| (!t.command.is_empty()).then(|| t.command.join(" ")));
            card = card
                .child(row(
                    SharedString::from(format!("bot-tool-{i}")),
                    wrench(),
                    false,
                    t.name.clone().into(),
                    detail,
                ))
                .child(div().h(px(1.)).bg(p.border));
        }
        let adding = self.settings.tools_adding;
        card = card.child(
            row(
                "bot-tool-add".into(),
                Icon::new(if adding {
                    IconName::Minus
                } else {
                    IconName::Plus
                })
                .size(px(ui::ICON_LG))
                .text_color(p.muted_foreground)
                .into_any_element(),
                true,
                text::SV_TOOLS_ADD.get().into(),
                None,
            )
            .debug_selector(|| "bot-tool-add".into())
            .cursor_pointer()
            .hover(|d| d.bg(p.accent.opacity(0.4)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.settings.tools_adding = !this.settings.tools_adding;
                cx.notify();
            })),
        );
        let mut screen = Self::settings_screen("bot-settings-tools-page").child(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(Self::subsection_header(
                    text::SV_TOOLS.get(),
                    Some(line.into()),
                    None,
                    p,
                ))
                .child(card),
        );
        if adding {
            let cards = self.tools_cards(bot, p, window, cx);
            screen = screen
                .child(cards.curated)
                .child(cards.custom)
                .child(cards.relay);
        }
        screen
    }
}

/// A connector's name, as Who can ask heads its list.
fn source_label(source: ExternalSource) -> &'static str {
    match source {
        ExternalSource::Slack => "Slack",
        ExternalSource::Telegram => "Telegram",
        ExternalSource::Discord => "Discord",
        ExternalSource::Linear => "Linear",
        ExternalSource::GitHub => "GitHub",
        ExternalSource::Google => "Google",
        ExternalSource::Composio => "Composio",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use krill_proto::bots::{BotAccessRequest, BotAccessRequestStatus, BotId};

    use super::*;

    #[gpui::test]
    fn who_can_ask_page_initializes_pairing_input_directly_in_panel_mode(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let bot_id = BotId::new("bot-ask").unwrap();
        let bot = BotConfig::new(bot_id.clone(), "Ops", "claude", 1_700_000_000_000).unwrap();
        let reqs = vec![BotAccessRequest {
            token: "tok-ask".to_owned(),
            bot_id: bot_id.clone(),
            source: ExternalSource::Telegram,
            sender_id: "u-1".to_owned(),
            sender_name: "Alice".to_owned(),
            channel_id: "c-1".to_owned(),
            status: BotAccessRequestStatus::Pending,
            created_at: 1_700_000_000_000,
            decided_at: None,
        }];
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![bot.clone()])),
            BotRpcRequest::ListAccessRequests { .. } => {
                Ok(BotRpcResponse::AccessRequests(reqs.clone()))
            }
            _ => Ok(BotRpcResponse::Ok),
        });

        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                // Open Settings > WhoCanAsk directly without ever visiting DetailTab::Connectors.
                v.show_in_panel(&bot_id, super::super::DetailTab::Settings, cx);
                v.open_settings_page(SettingsPage::WhoCanAsk, cx);
            });
        });
        vcx.run_until_parked();

        assert!(vcx.debug_bounds("bot-settings-who-can-ask-page").is_some());
        vcx.update(|_, cx| {
            assert!(
                view.read(cx).connectors.pairing_code_input.is_some(),
                "WhoCanAsk must initialize pairing_code_input via ensure_connector_inputs"
            );
        });
    }

    #[gpui::test]
    fn look_focus_input_resyncs_when_clean_and_preserves_dirty_user_draft(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let bot_id = BotId::new("bot-look").unwrap();
        let mut bot = BotConfig::new(bot_id.clone(), "Scout", "claude", 1_700_000_000_000).unwrap();
        bot.look_around.enabled = true;
        bot.look_around.focus = "PRs".to_owned();
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(vec![bot.clone()])),
            _ => Ok(BotRpcResponse::Ok),
        });

        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&bot_id, super::super::DetailTab::Settings, cx);
                v.open_settings_page(SettingsPage::LookAround, cx);
            });
        });
        vcx.run_until_parked();

        vcx.update(|_, cx| {
            let v = view.read(cx);
            let input = v.settings.look_focus_input.as_ref().unwrap();
            assert_eq!(input.read(cx).value().as_str(), "PRs");
            assert_eq!(v.settings.look_focus_synced.as_deref(), Some("PRs"));
        });

        // External bot update re-syncs clean input.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.bots[0].look_around.focus = "Issues".to_owned();
                cx.notify();
            });
        });
        vcx.run_until_parked();

        vcx.update(|window, cx| {
            let input = view
                .read(cx)
                .settings
                .look_focus_input
                .as_ref()
                .unwrap()
                .clone();
            assert_eq!(input.read(cx).value().as_str(), "Issues");
            assert_eq!(
                view.read(cx).settings.look_focus_synced.as_deref(),
                Some("Issues")
            );
            // User begins editing a local draft without committing yet.
            input.update(cx, |s, cx| {
                s.set_value("My draft".to_owned(), window, cx);
            });
            view.update(cx, |v, cx| {
                v.bots[0].look_around.focus = "External update".to_owned();
                cx.notify();
            });
        });
        vcx.run_until_parked();

        vcx.update(|_, cx| {
            let input = view.read(cx).settings.look_focus_input.as_ref().unwrap();
            assert_eq!(
                input.read(cx).value().as_str(),
                "My draft",
                "external focus refresh must not overwrite a dirty user draft"
            );
        });
    }
}
