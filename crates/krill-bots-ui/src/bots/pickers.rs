// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Shared Orca `BotComputerPicker`, `BotAgentLaunchPicker`, `BotRunsOnPicker`, and `BotNameSuggestion` controls.

use std::collections::HashMap;

use gpui::{AnyElement, App, Context, FontWeight, SharedString, Task, Window, div, prelude::*, px};
use gpui_component::button::Button;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::popover::Popover;
use gpui_component::{Icon, IconName, Sizable as _};
use krill_proto::agent::Permission;
use krill_proto::bots::{
    BotAgentModel, BotAgentModels, BotConfig, BotRpcRequest, BotRpcResponse, bot_slug_from_name,
};

use super::rpc::{
    BOT_RUNTIMES, BotRpcCall, call_checked, effort_label, effort_options_for_runtime,
    runtime_display_label, runtime_models,
};
use crate::hosts::HostId;
use crate::i18n::catalog::bots as text;
use crate::i18n::catalog::import_bot as itext;
use crate::ui;

/// A computer the bot can be created and run on (Orca's "Computer" choices).
#[derive(Clone)]
pub struct HostChoice {
    pub id: HostId,
    pub label: SharedString,
    pub rpc: BotRpcCall,
}

impl std::fmt::Debug for HostChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostChoice")
            .field("id", &self.id)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

/// Why a computer cannot take a new bot; Orca greys these out in its Computer list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocked {
    /// Its Krill predates bots (its version, or `""` when unstated).
    NoBots(String),
    Unreachable,
}

/// Probes whether `rpc`'s daemon supports bots.
#[must_use]
pub fn probe_host(rpc: &BotRpcCall) -> Option<Blocked> {
    let err = rpc(&BotRpcRequest::ListBots).err()?;
    Some(
        super::rpc::unsupported_version(&err).map_or(Blocked::Unreachable, |version| {
            Blocked::NoBots(version.to_owned())
        }),
    )
}

/// Runtimes a new bot can start with: on this computer, only detected/installed runtimes.
#[must_use]
pub fn available_runtimes(local: bool, cx: &App) -> Vec<&'static str> {
    BOT_RUNTIMES
        .iter()
        .map(|rt| rt.id)
        .filter(|id| !local || crate::accounts::installed::agent_maybe_installed(cx, id))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunsOnVariant {
    /// Collapsible bordered summary row (`ImportBotDialog`).
    Row,
    /// Two-column Computer + Agent field grid (`NewBotDialog`).
    Fields,
}

/// Wrapper implementing `gpui_component::Selectable` for custom `Popover::trigger` divs.
#[derive(gpui::IntoElement)]
pub(super) struct PopoverTrigger(pub gpui::Stateful<gpui::Div>, pub bool);

impl gpui_component::Selectable for PopoverTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.1 = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.1
    }
}

impl gpui::RenderOnce for PopoverTrigger {
    fn render(self, _: &mut gpui::Window, _: &mut gpui::App) -> impl gpui::IntoElement {
        self.0
    }
}

/// Shared state for Computer + Agent + Model + Effort + Permission selection.
pub struct BotAgentSetup {
    pub host_id: HostId,
    pub host_label: SharedString,
    pub host_is_local: bool,
    pub rpc: BotRpcCall,
    pub hosts: Vec<HostChoice>,
    pub blocked: HashMap<HostId, Blocked>,
    probe_tasks: Vec<Task<()>>,

    pub runtime: &'static str,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission_mode: Option<Permission>,
    pub discovered: Option<BotAgentModels>,
    models_task: Option<Task<()>>,

    pub runs_on_expanded: bool,
    pub agent_popover_open: bool,
}

impl BotAgentSetup {
    pub fn from_hosts<E: 'static>(
        hosts: Vec<HostChoice>,
        lens: fn(&mut E) -> Option<&mut Self>,
        cx: &mut Context<E>,
    ) -> Self {
        let local_label: SharedString = text::HOST_LOCAL
            .mac_or(crate::i18n::catalog::other_os::HOST_LOCAL)
            .get()
            .into();
        let fallback = HostChoice {
            id: HostId::Local,
            label: local_label,
            rpc: super::rpc::daemon_bot_rpc(None),
        };
        let hosts = if hosts.is_empty() {
            vec![fallback]
        } else {
            hosts
        };
        let active = hosts[0].clone();
        let local = active.id.is_local();
        let runtimes = available_runtimes(local, cx);
        let default_agent = crate::settings::current(cx).default_agent;
        let preferred = krill_proto::agent::canonical_id(&default_agent);
        let runtime = runtimes
            .iter()
            .copied()
            .find(|id| *id == preferred)
            .or_else(|| runtimes.first().copied())
            .unwrap_or("claude");
        let mut me = Self {
            host_id: active.id,
            host_label: active.label,
            host_is_local: local,
            rpc: active.rpc,
            hosts,
            blocked: HashMap::new(),
            probe_tasks: Vec::new(),
            runtime,
            model: None,
            effort: None,
            permission_mode: None,
            discovered: None,
            models_task: None,
            runs_on_expanded: false,
            agent_popover_open: false,
        };
        me.probe_hosts(lens, cx);
        me.load_models(lens, cx);
        me
    }

    pub fn probe_hosts<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> Option<&mut Self>,
        cx: &mut Context<E>,
    ) {
        self.probe_tasks.clear();
        for host in self.hosts.clone() {
            let task = cx.spawn(async move |this, cx| {
                let blocked = cx
                    .background_executor()
                    .spawn(async move { probe_host(&host.rpc) })
                    .await;
                this.update(cx, |this, cx| {
                    let Some(s) = lens(this) else {
                        return;
                    };
                    if let Some(reason) = blocked {
                        s.blocked.insert(host.id.clone(), reason);
                        if s.host_id == host.id
                            && let Some(first_ready) = s
                                .hosts
                                .iter()
                                .find(|h| !s.blocked.contains_key(&h.id))
                                .cloned()
                        {
                            s.select_host(&first_ready.id, lens, cx);
                        }
                    } else {
                        s.blocked.remove(&host.id);
                    }
                    cx.notify();
                })
                .ok();
            });
            self.probe_tasks.push(task);
        }
    }

    pub fn select_host<E: 'static>(
        &mut self,
        id: &HostId,
        lens: fn(&mut E) -> Option<&mut Self>,
        cx: &mut Context<E>,
    ) {
        if self.blocked.contains_key(id) {
            return;
        }
        let Some(choice) = self.hosts.iter().find(|h| &h.id == id).cloned() else {
            return;
        };
        self.rpc = choice.rpc;
        self.host_label = choice.label;
        self.host_is_local = choice.id.is_local();
        self.host_id = choice.id;
        let runtimes = available_runtimes(self.host_is_local, cx);
        if runtimes.contains(&self.runtime) {
            self.model = None;
            self.effort = None;
            self.load_models(lens, cx);
        } else {
            let default_agent = crate::settings::current(cx).default_agent;
            let preferred = krill_proto::agent::canonical_id(&default_agent);
            if let Some(next) = runtimes
                .iter()
                .copied()
                .find(|id| *id == preferred)
                .or_else(|| runtimes.first().copied())
            {
                self.set_runtime(next, lens, cx);
            } else {
                self.model = None;
                self.effort = None;
                self.load_models(lens, cx);
            }
        }
        cx.notify();
    }

    pub fn set_runtime<E: 'static>(
        &mut self,
        runtime: &'static str,
        lens: fn(&mut E) -> Option<&mut Self>,
        cx: &mut Context<E>,
    ) {
        if self.runtime != runtime {
            self.runtime = runtime;
            self.model = None;
            self.effort = None;
            self.load_models(lens, cx);
        }
        cx.notify();
    }

    pub fn set_model<E: 'static>(&mut self, model: Option<String>, cx: &mut Context<E>) {
        if krill_proto::agent::canonical_id(self.runtime) == "agy"
            && model
                .as_deref()
                .is_some_and(|m| krill_proto::agent::strip_agy_effort_suffix(m).1.is_some())
        {
            self.effort = None;
        }
        self.model = model;
        cx.notify();
    }

    pub fn set_effort<E: 'static>(&mut self, effort: Option<String>, cx: &mut Context<E>) {
        self.effort = effort.filter(|e| !e.trim().is_empty());
        cx.notify();
    }

    pub fn set_permission<E: 'static>(&mut self, mode: Option<Permission>, cx: &mut Context<E>) {
        self.permission_mode = mode;
        cx.notify();
    }

    #[must_use]
    pub fn has_overrides(&self) -> bool {
        self.model.is_some() || self.effort.is_some() || self.permission_mode.is_some()
    }

    pub fn reset_chat_defaults<E: 'static>(&mut self, cx: &mut Context<E>) {
        self.model = None;
        self.effort = None;
        self.permission_mode = None;
        cx.notify();
    }

    pub fn load_models<E: 'static>(
        &mut self,
        lens: fn(&mut E) -> Option<&mut Self>,
        cx: &mut Context<E>,
    ) {
        let rpc = self.rpc.clone();
        let agent_id = self.runtime.to_owned();
        let expected_runtime = self.runtime;
        self.discovered = None;
        self.models_task = Some(cx.spawn(async move |this, cx| {
            let reply = cx
                .background_executor()
                .spawn(
                    async move { call_checked(&rpc, &BotRpcRequest::ListAgentModels { agent_id }) },
                )
                .await;
            this.update(cx, |this, cx| {
                let Some(setup) = lens(this) else {
                    return;
                };
                if setup.runtime != expected_runtime {
                    return;
                }
                setup.discovered = Some(match reply {
                    Ok(BotRpcResponse::AgentModels(models)) => models,
                    _ => BotAgentModels::default(),
                });
                cx.notify();
            })
            .ok();
        }));
    }

    #[must_use]
    pub fn model_choices(&self) -> Vec<BotAgentModel> {
        match &self.discovered {
            Some(found)
                if !found.models.is_empty()
                    && crate::agent_models::prefers_discovered(self.runtime) =>
            {
                found.models.clone()
            }
            _ => runtime_models(self.runtime),
        }
    }

    #[must_use]
    pub fn model_notice(&self) -> Option<SharedString> {
        let Some(found) = &self.discovered else {
            return Some(text::MODELS_LOADING.get().into());
        };
        found
            .reason
            .clone()
            .filter(|_| found.models.is_empty())
            .map(Into::into)
    }

    #[must_use]
    pub fn model_label(&self, id: &str) -> String {
        self.model_choices()
            .into_iter()
            .find(|m| m.id == id)
            .map_or_else(|| id.to_owned(), |m| m.label)
    }

    #[must_use]
    pub fn supports_effort(&self) -> bool {
        super::rpc::effort_options_for_runtime(self.runtime).len() > 1
    }

    #[must_use]
    pub fn summary_detail(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(m) = &self.model {
            parts.push(self.model_label(m));
        }
        if let Some(eff) = &self.effort {
            parts.push(effort_label(eff).to_owned());
        }
        if let Some(perm) = self.permission_mode {
            parts.push(permission_label(Some(perm)).to_owned());
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    #[must_use]
    pub fn current_blocked(&self) -> Option<&Blocked> {
        self.blocked.get(&self.host_id)
    }

    #[must_use]
    pub fn is_blocked(&self) -> bool {
        self.current_blocked().is_some()
    }
}

#[must_use]
fn permission_label(mode: Option<Permission>) -> &'static str {
    match mode {
        None => text::NB_PERMISSION_SAME_AS_CHATS.get(),
        Some(Permission::Manual) => text::NB_PERMISSION_ASK.get(),
        Some(Permission::Yolo) => text::NB_PERMISSION_FULL.get(),
    }
}

#[must_use]
fn folder_preview(name: &str, taken_names: &[String], host_id: &HostId) -> String {
    let base = if name.trim().is_empty() {
        text::NB_DEFAULT_NAME.get()
    } else {
        name.trim()
    };
    let mut candidate = base.to_owned();
    let is_taken = |n: &str| taken_names.iter().any(|t| t.trim().eq_ignore_ascii_case(n));
    if is_taken(&candidate) {
        for i in 2..=taken_names.len() + 2 {
            let next = format!("{base} {i}");
            if !is_taken(&next) {
                candidate = next;
                break;
            }
        }
    }
    let slug = bot_slug_from_name(&candidate);
    if cfg!(windows) && host_id.is_local() {
        format!("~\\.krill\\bots\\{}", slug.as_str())
    } else {
        format!("~/.krill/bots/{}", slug.as_str())
    }
}

/// Computer row that cannot take a new bot (greyed out with reason + Update link when outdated).
#[must_use]
pub fn blocked_host_row(label: &SharedString, blocked: &Blocked, cx: &App) -> gpui::Div {
    let p = ui::palette(cx);
    let (hint, action) = match blocked {
        Blocked::NoBots(version) if version.is_empty() => (
            text::NB_HOST_NO_BOTS.format(&[("host", label)]),
            Some(text::NB_HOST_UPDATE_ACTION.get()),
        ),
        Blocked::NoBots(version) => (
            text::NB_HOST_NO_BOTS_VERSION.format(&[("host", label), ("version", version)]),
            Some(text::NB_HOST_UPDATE_ACTION.get()),
        ),
        Blocked::Unreachable => (text::NB_HOST_UNREACHABLE.format(&[("host", label)]), None),
    };
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_color(p.muted_foreground)
                        .child(label.clone()),
                )
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(hint),
                ),
        )
        .children(action.map(|action| {
            div()
                .flex_none()
                .text_size(ui::TEXT_XS)
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.foreground)
                .child(action)
        }))
}

#[must_use]
fn host_icon(is_local: bool, p: &ui::Palette) -> gpui_component::Icon {
    ui::icons::svg_icon(
        if is_local {
            ui::icons::MONITOR_SVG
        } else {
            ui::icons::CLOUD_SVG
        },
        ui::ICON_SM,
        p.muted_foreground,
    )
}

/// Inline error message for a blocked computer when the user tries to Create (`22927c89`).
#[must_use]
fn blocked_computer_error(blocked: Option<&Blocked>) -> Option<&'static str> {
    match blocked {
        Some(Blocked::Unreachable) => Some(text::NB_VAL_OFFLINE.get()),
        Some(Blocked::NoBots(_)) => Some(text::NB_VAL_UPDATE_KRILL.get()),
        None => None,
    }
}

/// Renders Orca `BotComputerPicker`.
pub fn render_computer_picker<E: 'static>(
    prefix: &'static str,
    setup: &BotAgentSetup,
    invalid: bool,
    lens: fn(&mut E) -> Option<&mut BotAgentSetup>,
    on_update_host: fn(&mut E, &mut Context<E>),
    p: &ui::Palette,
    cx: &mut Context<E>,
) -> AnyElement {
    let host_id_sel: SharedString = format!("{prefix}-host").into();
    let current = div()
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .gap_1p5()
        .child(host_icon(setup.host_is_local, p))
        .child(div().truncate().child(setup.host_label.clone()));
    if setup.hosts.len() < 2 {
        return div()
            .id(host_id_sel.clone())
            .debug_selector(move || host_id_sel.to_string())
            .h(px(28.))
            .px_2p5()
            .rounded(ui::RADIUS_SM)
            .border_1()
            .border_color(if invalid { p.destructive } else { p.border })
            .bg(p.background)
            .flex()
            .items_center()
            .text_size(ui::TEXT_SM)
            .text_color(p.foreground)
            .child(current)
            .into_any_element();
    }

    let this = cx.weak_entity();
    let mut rows: Vec<(HostChoice, Option<Blocked>)> = setup
        .hosts
        .iter()
        .map(|h| (h.clone(), setup.blocked.get(&h.id).cloned()))
        .collect();
    rows.sort_by_key(|(_, blocked)| blocked.is_some());
    let current_id = setup.host_id.clone();

    let button = Button::new(host_id_sel.clone())
        .outline()
        .small()
        .w_full()
        .dropdown_caret(true)
        .accessibility_label(setup.host_label.clone())
        .debug_selector(move || host_id_sel.to_string())
        .child(current)
        .dropdown_menu(move |mut menu, _, _| {
            for (host, blocked) in &rows {
                let this = this.clone();
                let id = host.id.clone();
                let item = match blocked {
                    None => PopupMenuItem::new(host.label.clone())
                        .checked(id == current_id)
                        .on_click(move |_, _, cx| {
                            this.update(cx, |v, cx| {
                                if let Some(s) = lens(v) {
                                    s.select_host(&id, lens, cx);
                                }
                            })
                            .ok();
                        }),
                    Some(blocked) => {
                        let update = matches!(blocked, Blocked::NoBots(_));
                        let label = host.label.clone();
                        let blocked = blocked.clone();
                        PopupMenuItem::element(move |_, cx| blocked_host_row(&label, &blocked, cx))
                            .disabled(!update)
                            .on_click(move |_, _, cx| {
                                this.update(cx, on_update_host).ok();
                            })
                    }
                };
                menu = menu.item(item);
            }
            menu
        });

    if invalid {
        div()
            .w_full()
            .rounded(ui::RADIUS_SM)
            .border_1()
            .border_color(p.destructive)
            .child(button)
            .into_any_element()
    } else {
        button.into_any_element()
    }
}

/// Renders Orca `BotAgentLaunchPicker` (trigger + Popover with Agent / Model / Effort / Permission + chat default hint & reset link).
pub fn render_agent_launch_picker<E: 'static>(
    prefix: &'static str,
    setup: &BotAgentSetup,
    show_chat_default_hint: bool,
    invalid: bool,
    lens: fn(&mut E) -> Option<&mut BotAgentSetup>,
    p: &ui::Palette,
    cx: &mut Context<E>,
) -> gpui::Div {
    let local = setup.host_is_local;
    let available = available_runtimes(local, cx);
    let no_agents = available.is_empty();
    let runtime = setup.runtime;
    let runtime_label = runtime_display_label(runtime);
    let detail = setup.summary_detail();

    let popover_id: SharedString = format!("{prefix}-agent-popover").into();
    let trigger_id: SharedString = format!("{prefix}-runtime").into();
    let entity = cx.entity();

    let trigger_body = div()
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .gap_1p5()
        .child(ui::icons::agent_brand_icon(
            runtime,
            ui::ICON_SM,
            p.foreground,
        ))
        .child(div().truncate().child(runtime_label.clone()))
        .children(detail.as_ref().map(|d| {
            div()
                .truncate()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(d.clone())
        }));

    let trigger = div()
        .id(trigger_id.clone())
        .debug_selector(move || trigger_id.to_string())
        .role(gpui::Role::Button)
        .tab_index(0)
        .focus_visible(|d| d.border_1().border_color(p.ring))
        .aria_label(runtime_label)
        .w_full()
        .h(px(28.))
        .px_2p5()
        .rounded(ui::RADIUS_SM)
        .border_1()
        .border_color(if invalid { p.destructive } else { p.border })
        .bg(p.background)
        .when(!no_agents, |d| {
            d.cursor_pointer().hover(|d| d.bg(p.accent.opacity(0.4)))
        })
        .when(no_agents, |d| d.opacity(0.5))
        .flex()
        .items_center()
        .justify_between()
        .gap_1p5()
        .text_size(ui::TEXT_SM)
        .text_color(p.foreground)
        .child(trigger_body)
        .child(
            Icon::new(IconName::ChevronDown)
                .size(px(ui::ICON_XS))
                .text_color(p.muted_foreground),
        );

    let picker = Popover::new(popover_id)
        .open(setup.agent_popover_open && !no_agents)
        .appearance(false)
        .on_open_change(cx.listener(move |this, open: &bool, _, cx| {
            if let Some(s) = lens(this) {
                s.agent_popover_open = *open;
                cx.notify();
            }
        }))
        .trigger(PopoverTrigger(trigger, false))
        .content(move |_, _, cx| {
            entity.update(cx, |this, cx| {
                let palette = ui::palette(cx);
                let Some(s) = lens(this) else {
                    return div().into_any_element();
                };
                render_agent_popover_body(prefix, s, lens, palette, cx)
            })
        });

    let has_overrides = setup.has_overrides();
    let reset_id: SharedString = format!("{prefix}-use-chat-defaults").into();
    let err_sel = format!("{prefix}-agent-error");

    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .when(no_agents, |d| {
            d.child(
                div()
                    .w_full()
                    .h(px(28.))
                    .px_2p5()
                    .rounded(ui::RADIUS_SM)
                    .border_1()
                    .border_color(if invalid { p.destructive } else { p.border })
                    .bg(p.background)
                    .flex()
                    .items_center()
                    .text_size(ui::TEXT_SM)
                    .text_color(p.muted_foreground)
                    .child(text::NB_VAL_NO_AGENTS_SHORT.get()),
            )
            .when(invalid, |d| {
                d.child(
                    div()
                        .id(SharedString::from(err_sel.clone()))
                        .debug_selector(move || err_sel.clone())
                        .role(gpui::Role::Alert)
                        .text_size(ui::TEXT_XS)
                        .text_color(p.destructive)
                        .child(text::NB_VAL_NO_AGENTS_AVAILABLE.get()),
                )
            })
            .when(!invalid && show_chat_default_hint, |d| {
                d.child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::NB_NO_AGENTS.get()),
                )
            })
        })
        .when(!no_agents, |d| {
            d.child(picker)
                .when(show_chat_default_hint && !has_overrides, |d| {
                    d.child(
                        div()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .child(text::NB_USING_CHAT_DEFAULT.get()),
                    )
                })
                .when(has_overrides, |d| {
                    d.child(
                        div()
                            .id(reset_id.clone())
                            .debug_selector(move || reset_id.to_string())
                            .role(gpui::Role::Button)
                            .tab_index(0)
                            .cursor_pointer()
                            .self_start()
                            .text_size(ui::TEXT_XS)
                            .text_color(p.muted_foreground)
                            .underline()
                            .hover(|d| d.text_color(p.foreground))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(s) = lens(this) {
                                    s.reset_chat_defaults(cx);
                                }
                            }))
                            .child(text::NB_USE_CHAT_DEFAULTS.get()),
                    )
                })
        })
}

fn render_agent_popover_body<E: 'static>(
    prefix: &'static str,
    setup: &BotAgentSetup,
    lens: fn(&mut E) -> Option<&mut BotAgentSetup>,
    p: &ui::Palette,
    cx: &mut Context<E>,
) -> AnyElement {
    let runtimes = available_runtimes(setup.host_is_local, cx);
    let current_runtime = setup.runtime;
    let current_model = setup.model.clone();
    let current_effort = setup.effort.clone();
    let current_perm = setup.permission_mode;
    let models = setup.model_choices();
    let notice = setup.model_notice();
    let supports_effort = setup.supports_effort();

    let mut rt_col = div()
        .w(px(140.))
        .flex_none()
        .pr_2()
        .border_r_1()
        .border_color(p.border)
        .flex()
        .flex_col()
        .gap_0p5();
    for rt_id in runtimes {
        let active = rt_id == current_runtime;
        let btn_id: SharedString = format!("{prefix}-rt-{rt_id}").into();
        let label = runtime_display_label(rt_id);
        rt_col = rt_col.child(
            div()
                .id(btn_id.clone())
                .debug_selector(move || btn_id.to_string())
                .role(gpui::Role::Button)
                .tab_index(0)
                .h(px(28.))
                .px_2()
                .rounded(ui::RADIUS_SM)
                .cursor_pointer()
                .when(active, |d| d.bg(p.accent).font_weight(FontWeight::MEDIUM))
                .when(!active, |d| d.hover(|d| d.bg(p.accent.opacity(0.5))))
                .flex()
                .items_center()
                .gap_1p5()
                .text_size(ui::TEXT_SM)
                .text_color(p.foreground)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = lens(this) {
                        s.set_runtime(rt_id, lens, cx);
                    }
                }))
                .child(ui::icons::agent_brand_icon(
                    rt_id,
                    ui::ICON_XS,
                    p.foreground,
                ))
                .child(div().flex_1().truncate().child(label)),
        );
    }

    let mut model_list = div()
        .id(SharedString::from(format!("{prefix}-model")))
        .debug_selector(move || format!("{prefix}-model"))
        .max_h(px(176.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .id(SharedString::from(format!("{prefix}-model-default")))
                .role(gpui::Role::Button)
                .h(px(26.))
                .px_2()
                .rounded(ui::RADIUS_XS)
                .cursor_pointer()
                .when(current_model.is_none(), |d| d.bg(p.accent))
                .hover(|d| d.bg(p.accent.opacity(0.5)))
                .flex()
                .items_center()
                .justify_between()
                .text_size(ui::TEXT_SM)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = lens(this) {
                        s.set_model(None, cx);
                    }
                }))
                .child(text::MODEL_DEFAULT.get())
                .when(current_model.is_none(), |d| {
                    d.child(Icon::new(IconName::Check).size(px(ui::ICON_XS)))
                }),
        );
    for m in models {
        let checked = current_model.as_deref() == Some(m.id.as_str());
        let mid = m.id.clone();
        let row_id: SharedString = format!("{prefix}-m-{}", m.id).into();
        model_list = model_list.child(
            div()
                .id(row_id)
                .role(gpui::Role::Button)
                .h(px(26.))
                .px_2()
                .rounded(ui::RADIUS_XS)
                .cursor_pointer()
                .when(checked, |d| d.bg(p.accent))
                .hover(|d| d.bg(p.accent.opacity(0.5)))
                .flex()
                .items_center()
                .justify_between()
                .gap_1p5()
                .text_size(ui::TEXT_SM)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = lens(this) {
                        s.set_model(Some(mid.clone()), cx);
                    }
                }))
                .child(div().flex_1().truncate().child(m.label))
                .when(checked, |d| {
                    d.child(Icon::new(IconName::Check).size(px(ui::ICON_XS)))
                }),
        );
    }
    if let Some(n) = notice {
        model_list = model_list.child(
            div()
                .px_2()
                .py_1()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(n),
        );
    }

    let effort_row = if supports_effort {
        let mut segs = Vec::new();
        for &eff in effort_options_for_runtime(current_runtime) {
            let active = if eff.is_empty() {
                current_effort.is_none()
            } else {
                current_effort.as_deref() == Some(eff)
            };
            let eff_owned = eff.to_owned();
            let id: SharedString = format!(
                "{prefix}-eff-{}",
                if eff.is_empty() { "default" } else { eff }
            )
            .into();
            segs.push(
                ui::segment(p, id, effort_label(eff), active).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let next = (!eff_owned.is_empty()).then_some(eff_owned.clone());
                        if let Some(s) = lens(this) {
                            s.set_effort(next, cx);
                        }
                    },
                )),
            );
        }
        Some(
            div()
                .pt_2()
                .border_t_1()
                .border_color(p.border)
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::NB_EFFORT.get()),
                )
                .child(ui::segmented_control(p, segs)),
        )
    } else {
        None
    };

    let perm_choices: [(Option<Permission>, &'static str); 3] = [
        (None, "same"),
        (Some(Permission::Manual), "ask"),
        (Some(Permission::Yolo), "yolo"),
    ];
    let perm_segs = perm_choices.into_iter().map(|(mode, key)| {
        let active = current_perm == mode;
        let id: SharedString = format!("{prefix}-perm-{key}").into();
        ui::segment(p, id, permission_label(mode), active).on_click(cx.listener(
            move |this, _, _, cx| {
                if let Some(s) = lens(this) {
                    s.set_permission(mode, cx);
                }
            },
        ))
    });

    let right_col = div()
        .flex_1()
        .min_w_0()
        .pl_2()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(text::FIELD_MODEL.get()),
        )
        .child(model_list)
        .children(effort_row)
        .child(
            div()
                .pt_2()
                .border_t_1()
                .border_color(p.border)
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::NB_PERMISSION.get()),
                )
                .child(ui::segmented_control(p, perm_segs)),
        );

    div()
        .id(SharedString::from(format!("{prefix}-agent-popover-body")))
        .debug_selector(move || format!("{prefix}-agent-popover-body"))
        .w(px(396.))
        .p_2p5()
        .bg(p.popover)
        .text_color(p.popover_foreground)
        .border_1()
        .border_color(p.border)
        .rounded(ui::RADIUS_MD)
        .shadow_lg()
        .occlude()
        .flex()
        .items_start()
        .child(rt_col)
        .child(right_col)
        .into_any_element()
}

/// Renders Orca `BotRunsOnPicker` (`Fields` = 2-column grid for `NewBotDialog`, `Row` = collapsible card for `ImportBotDialog`).
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors Orca BotRunsOnPicker component props across Field/Row variants"
)]
pub fn render_runs_on_picker<E: 'static>(
    prefix: &'static str,
    variant: RunsOnVariant,
    setup: &BotAgentSetup,
    bot_name: Option<&str>,
    taken_names: &[String],
    show_submit_feedback: bool,
    lens: fn(&mut E) -> Option<&mut BotAgentSetup>,
    on_update_host: fn(&mut E, &mut Context<E>),
    p: &ui::Palette,
    cx: &mut Context<E>,
) -> gpui::Div {
    let field_label = |label: &'static str, fields: bool| {
        div()
            .when(fields, |d| {
                d.text_size(ui::TEXT_XS).text_color(p.muted_foreground)
            })
            .when(!fields, |d| {
                d.text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.foreground)
            })
            .child(label)
    };

    let fields = variant == RunsOnVariant::Fields;
    let computer_error = show_submit_feedback
        .then(|| blocked_computer_error(setup.current_blocked()))
        .flatten();
    let agent_invalid =
        show_submit_feedback && available_runtimes(setup.host_is_local, cx).is_empty();
    let computer_hint = if fields || computer_error.is_some() {
        None
    } else {
        Some(text::NB_COMPUTER_HINT.get())
    };
    let computer_err_sel = format!("{prefix}-computer-error");
    let computer_col = div()
        .min_w_0()
        .when(fields, gpui::Styled::flex_1)
        .flex()
        .flex_col()
        .gap_1()
        .child(field_label(text::NB_COMPUTER.get(), fields))
        .child(render_computer_picker(
            prefix,
            setup,
            computer_error.is_some(),
            lens,
            on_update_host,
            p,
            cx,
        ))
        .children(computer_error.map(|err| {
            div()
                .id(SharedString::from(computer_err_sel.clone()))
                .debug_selector(move || computer_err_sel.clone())
                .role(gpui::Role::Alert)
                .text_size(ui::TEXT_XS)
                .text_color(p.destructive)
                .child(err)
        }))
        .children(computer_hint.map(|h| {
            div()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .child(h)
        }));

    let agent_col = div()
        .min_w_0()
        .when(fields, gpui::Styled::flex_1)
        .flex()
        .flex_col()
        .gap_1()
        .child(field_label(text::NB_AGENT.get(), fields))
        .child(render_agent_launch_picker(
            prefix,
            setup,
            !fields,
            agent_invalid,
            lens,
            p,
            cx,
        ));

    let folder_line = bot_name.map(|name| {
        let preview = folder_preview(name, taken_names, &setup.host_id);
        div()
            .text_size(ui::TEXT_2XS)
            .text_color(p.muted_foreground)
            .child(text::NB_FOLDER_PREVIEW.format(&[("path", &preview)]))
    });

    if fields {
        return div()
            .debug_selector(move || format!("{prefix}-runs-on-fields"))
            .flex()
            .items_start()
            .gap_2()
            .child(computer_col)
            .child(agent_col);
    }

    let strong = |s: SharedString| {
        div()
            .font_weight(FontWeight::MEDIUM)
            .text_color(p.foreground)
            .child(s)
    };
    let line = text::NB_RUNS_LINE.get();
    let (before_host, rest) = line.split_once("{host}").unwrap_or((line, ""));
    let (between, after_agent) = rest.split_once("{agent}").unwrap_or((rest, ""));
    let runtime = setup.runtime;
    let agent_label = runtime_display_label(runtime);
    let detail = setup.summary_detail();

    div()
        .rounded(ui::RADIUS_MD)
        .border_1()
        .border_color(p.border)
        .flex()
        .flex_col()
        .child(
            div()
                .id(SharedString::from(format!("{prefix}-runs-on")))
                .debug_selector(move || format!("{prefix}-runs-on"))
                .role(gpui::Role::Button)
                .tab_index(0)
                .focus_visible(|d| d.border_1().border_color(p.ring))
                .px_2p5()
                .py_2()
                .cursor_pointer()
                .hover(|d| d.bg(p.accent.opacity(0.4)))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(s) = lens(this) {
                        s.runs_on_expanded = !s.runs_on_expanded;
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .min_w_0()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_1()
                                .text_size(ui::TEXT_SM)
                                .text_color(p.muted_foreground)
                                .when(!before_host.trim().is_empty(), |d| {
                                    d.child(before_host.trim().to_owned())
                                })
                                .child(host_icon(setup.host_is_local, p))
                                .child(strong(setup.host_label.clone()))
                                .when(!between.trim().is_empty(), |d| {
                                    d.child(between.trim().to_owned())
                                })
                                .child(ui::icons::agent_brand_icon(
                                    runtime,
                                    ui::ICON_XS,
                                    p.foreground,
                                ))
                                .child(strong(agent_label.into()))
                                .when(!after_agent.trim().is_empty(), |d| {
                                    d.child(after_agent.trim().to_owned())
                                }),
                        )
                        .children(detail.map(|d| {
                            div()
                                .truncate()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(d)
                        })),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(text::NB_CHANGE.get())
                        .child(
                            Icon::new(if setup.runs_on_expanded {
                                IconName::ChevronUp
                            } else {
                                IconName::ChevronDown
                            })
                            .size(px(ui::ICON_MD)),
                        ),
                ),
        )
        .when(setup.runs_on_expanded, |d| {
            d.child(
                div()
                    .id(SharedString::from(format!("{prefix}-change")))
                    .debug_selector(move || format!("{prefix}-change"))
                    .px_2p5()
                    .pt_1()
                    .pb_3()
                    .border_t_1()
                    .border_color(p.border)
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(computer_col)
                    .child(agent_col)
                    .children(folder_line),
            )
        })
}

/// Renders Orca `BotNameSuggestion` (fixed 16 px height row under the heading name field).
pub fn render_bot_name_suggestion<E: 'static>(
    id: &'static str,
    suggestion: Option<String>,
    fallback: Option<AnyElement>,
    on_take: impl Fn(&mut E, String, &mut Window, &mut Context<E>) + 'static,
    p: &ui::Palette,
    cx: &mut Context<E>,
) -> gpui::Div {
    div()
        .mt_1p5()
        .h(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .children(suggestion.map(|name| {
            let label = itext::USE_SUGGESTION.format(&[("name", &name)]);
            let take_name = name;
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(gpui::Role::Button)
                .tab_index(0)
                .cursor_pointer()
                .flex()
                .items_center()
                .gap_1()
                .text_size(ui::TEXT_XS)
                .text_color(p.muted_foreground)
                .hover(|d| d.text_color(p.foreground))
                .on_click(cx.listener(move |this, _, window, cx| {
                    on_take(this, take_name.clone(), window, cx);
                }))
                .child(label)
                .child(
                    div()
                        .px_1()
                        .rounded(ui::RADIUS_XS)
                        .border_1()
                        .border_color(p.border)
                        .bg(p.muted)
                        .text_size(ui::TEXT_2XS)
                        .text_color(p.muted_foreground)
                        .child(itext::TAB_KEY.get()),
                )
        }))
        .when(fallback.is_some(), |d| d.children(fallback))
}

/// Matches a picked external bot path against `ListImportedSources` (`(canonical_path, BotId)`) rows
/// to show whether an earlier active Krill bot was already created from that source.
///
/// `canonical_path` should be precomputed off the UI thread (and only for local hosts) so rendering
/// never performs synchronous filesystem syscalls.
#[must_use]
pub fn find_imported_bot<'a>(
    bots: &'a [BotConfig],
    imported_sources: &[(String, krill_proto::bots::BotId)],
    candidate_path: &str,
    canonical_path: Option<&str>,
) -> Option<&'a BotConfig> {
    imported_sources.iter().find_map(|(src, id)| {
        let same = src == candidate_path || canonical_path.is_some_and(|c| c == src);
        same.then(|| bots.iter().find(|b| &b.id == id)).flatten()
    })
}

impl std::fmt::Debug for BotAgentSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotAgentSetup")
            .field("host_id", &self.host_id)
            .field("runtime", &self.runtime)
            .field("model", &self.model)
            .field("effort", &self.effort)
            .finish_non_exhaustive()
    }
}
