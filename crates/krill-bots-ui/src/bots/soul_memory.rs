// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Tab 2 — Soul & Memory: SOUL.md & MEMORY.md editors with dirty/error preservation guard,
//! parsed memory facts (Edit/Forget), Memory History + Undo, Memory Topics (`memory/<topic>.md`),
//! Memory Tidy-up config/undo, and Look Around settings + persisted suggestions.

use std::collections::HashSet;

use gpui::{App, Context, Entity, FontWeight, SharedString, Subscription, Window, div, prelude::*};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::{Disableable as _, Sizable as _};
use krill_proto::bots::{
    BOT_MEMORY_INDEX_LINE_CAP, BotConfig, BotId, BotLookAroundConfig, BotMemoryHistoryEntry,
    BotMemoryTidyConfig, BotMemoryTidyStatus, BotMemoryTopicInfo, BotRpcRequest, BotRpcResponse,
    BotSuggestion, FieldPatch, MemoryDeletionViolation, UpdateBotParams, count_memory_lines,
    parse_memory_facts, validate_memory_edit,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

use super::rpc::call_checked;
use super::{
    OpenSession, View, clear_input_on_window, opt_input_composing, opt_textarea_composing,
};

const INTERVAL_PRESETS: [(&str, u64); 4] =
    [("15m", 900), ("1h", 3600), ("6h", 21600), ("24h", 86400)];

const SYNC_SOUL: u8 = 1 << 0;
const SYNC_MEMORY: u8 = 1 << 1;
const SYNC_TOPIC: u8 = 1 << 2;
const SYNC_BOT_CONFIG: u8 = 1 << 3;

#[derive(Default)]
pub(super) struct SoulMemoryState {
    pub(super) loaded_bot_id: Option<BotId>,
    pub(super) soul_loaded: String,
    pub(super) memory_loaded: String,
    pub(super) soul_input: Option<Entity<TextareaState>>,
    pub(super) soul_input_sub: Option<Subscription>,
    pub(super) saving_soul_content: Option<String>,
    pub(super) memory_input: Option<Entity<TextareaState>>,
    pub(super) sync_mask: u8,
    pub(super) deletion_violation: Option<MemoryDeletionViolation>,
    pub(super) armed_forget: Option<String>,
    pub(super) editing_fact: Option<(usize, String)>,
    pub(super) fact_edit_input: Option<Entity<InputState>>,
    pub(super) fact_edit_sub: Option<Subscription>,
    pub(super) memory_history: Vec<BotMemoryHistoryEntry>,
    pub(super) memory_topics: Vec<BotMemoryTopicInfo>,
    pub(super) selected_topic: Option<String>,
    pub(super) topic_loaded: String,
    pub(super) topic_name_input: Option<Entity<InputState>>,
    pub(super) topic_content_input: Option<Entity<TextareaState>>,
    pub(super) tidy_running: bool,
    pub(super) last_tidy_summary: Option<SharedString>,
    pub(super) tidy_status: Option<BotMemoryTidyStatus>,
    pub(super) tidy_config: BotMemoryTidyConfig,
    pub(super) look_around_running: HashSet<BotId>,
    pub(super) look_around_config: BotLookAroundConfig,
    pub(super) look_around_focus_input: Option<Entity<InputState>>,
    pub(super) suggestions: Vec<BotSuggestion>,
    /// Per-bot cached suggestions for instant SWR paint on bot switch.
    pub(super) suggestions_cache: std::collections::BTreeMap<BotId, Vec<BotSuggestion>>,
    pub(super) busy: bool,
}

impl SoulMemoryState {
    pub(super) fn is_looking_around(&self, bot_id: &BotId) -> bool {
        self.look_around_running.contains(bot_id)
    }

    pub(super) fn request_sync(&mut self, flag: u8) {
        self.sync_mask |= flag;
    }

    fn take_sync(&mut self, flag: u8) -> bool {
        let active = (self.sync_mask & flag) != 0;
        self.sync_mask &= !flag;
        active
    }

    pub(super) fn set_loaded(&mut self, soul: String, memory: String) {
        self.soul_loaded = soul;
        self.memory_loaded = memory;
        self.request_sync(SYNC_SOUL | SYNC_MEMORY);
        self.deletion_violation = None;
        self.armed_forget = None;
        self.editing_fact = None;
    }

    pub(super) fn apply_loaded_sections(
        &mut self,
        bot_id: &BotId,
        soul: Option<String>,
        memory: Option<String>,
    ) {
        self.loaded_bot_id = Some(bot_id.clone());
        self.deletion_violation = None;
        self.armed_forget = None;
        self.editing_fact = None;
        if let Some(s) = soul {
            self.soul_loaded = s;
            self.request_sync(SYNC_SOUL);
        }
        if let Some(m) = memory {
            self.memory_loaded = m;
            self.request_sync(SYNC_MEMORY);
        }
    }

    pub(super) fn sync_config_from_bot(&mut self, bot: &BotConfig) {
        self.look_around_config = bot.look_around.clone();
        self.tidy_config = bot.memory_tidy;
        self.request_sync(SYNC_BOT_CONFIG);
    }

    pub(super) fn input_is_composing(&self, window: &mut Window, app: &mut App) -> bool {
        opt_textarea_composing(self.soul_input.as_ref(), window, app)
            || opt_textarea_composing(self.memory_input.as_ref(), window, app)
            || opt_input_composing(self.fact_edit_input.as_ref(), window, app)
            || opt_input_composing(self.topic_name_input.as_ref(), window, app)
            || opt_textarea_composing(self.topic_content_input.as_ref(), window, app)
            || opt_input_composing(self.look_around_focus_input.as_ref(), window, app)
    }

    pub(super) fn has_unsaved_edits(&self, app: &App) -> bool {
        let soul_dirty = self
            .soul_input
            .as_ref()
            .is_some_and(|i| i.read(app).value().as_str() != self.soul_loaded.as_str());
        let memory_dirty = self
            .memory_input
            .as_ref()
            .is_some_and(|i| i.read(app).value().as_str() != self.memory_loaded.as_str());
        soul_dirty || memory_dirty
    }

    pub(super) fn discard_unsaved_edits(&mut self) {
        self.request_sync(SYNC_SOUL | SYNC_MEMORY);
    }

    pub(super) fn refresh_placeholders(&self, window: &mut Window, cx: &mut Context<View>) {
        if let Some(i) = &self.soul_input {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::SOUL_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.memory_input {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::MEMORY_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.topic_name_input {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::MEMORY_TOPIC_NAME_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.topic_content_input {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::MEMORY_TOPIC_CONTENT_PLACEHOLDER.get(), window, cx);
            });
        }
        if let Some(i) = &self.look_around_focus_input {
            i.update(cx, |s, cx| {
                s.set_placeholder(text::LOOK_AROUND_FOCUS_PLACEHOLDER.get(), window, cx);
            });
        }
    }
}

impl View {
    pub(super) fn ensure_soul_memory_inputs(
        &mut self,
        bot: &BotConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !cx.has_global::<gpui_component::Theme>() {
            return;
        }
        if self.soul_memory.soul_input.is_none() {
            let initial = self.soul_memory.soul_loaded.clone();
            let input = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(5, 14)
                    .placeholder(text::SOUL_PLACEHOLDER.get())
                    .default_value(initial)
            });
            self.soul_memory.soul_input_sub = Some(cx.subscribe_in(
                &input,
                window,
                |this, _, ev: &InputEvent, _, cx| match ev {
                    InputEvent::Change => {
                        cx.notify();
                    }
                    InputEvent::Blur
                        if this.mode == super::Mode::Panel
                            && this.detail_tab == super::DetailTab::Settings
                            && this.settings.page == super::settings_root::SettingsPage::Soul =>
                    {
                        this.flush_unsaved_soul_edits(cx);
                    }
                    _ => {}
                },
            ));
            self.soul_memory.soul_input = Some(input);
        }
        if self.soul_memory.memory_input.is_none() {
            let initial = self.soul_memory.memory_loaded.clone();
            self.soul_memory.memory_input = Some(cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(5, 14)
                    .placeholder(text::MEMORY_PLACEHOLDER.get())
                    .default_value(initial)
            }));
        }
        if self.soul_memory.topic_name_input.is_none() {
            self.soul_memory.topic_name_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(text::MEMORY_TOPIC_NAME_PLACEHOLDER.get())
            }));
        }
        if self.soul_memory.topic_content_input.is_none() {
            self.soul_memory.topic_content_input = Some(cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(4, 10)
                    .placeholder(text::MEMORY_TOPIC_CONTENT_PLACEHOLDER.get())
            }));
        }
        if self.soul_memory.look_around_focus_input.is_none() {
            let focus_val = bot.look_around.focus.clone();
            self.soul_memory.look_around_focus_input = Some(cx.new(|cx| {
                let mut st = InputState::new(window, cx)
                    .placeholder(text::LOOK_AROUND_FOCUS_PLACEHOLDER.get());
                if !focus_val.is_empty() {
                    st.set_value(focus_val, window, cx);
                }
                st
            }));
        }
        if self.soul_memory.take_sync(SYNC_SOUL) {
            let soul = self.soul_memory.soul_loaded.clone();
            if let Some(i) = &self.soul_memory.soul_input {
                i.update(cx, |s, cx| s.set_value(soul, window, cx));
            }
        }
        if self.soul_memory.take_sync(SYNC_MEMORY) {
            let memory = self.soul_memory.memory_loaded.clone();
            if let Some(i) = &self.soul_memory.memory_input {
                i.update(cx, |s, cx| s.set_value(memory, window, cx));
            }
        }
        if self.soul_memory.take_sync(SYNC_TOPIC) {
            let name = self.soul_memory.selected_topic.clone().unwrap_or_default();
            let content = self.soul_memory.topic_loaded.clone();
            if let Some(i) = &self.soul_memory.topic_name_input {
                i.update(cx, |s, cx| s.set_value(name, window, cx));
            }
            if let Some(i) = &self.soul_memory.topic_content_input {
                i.update(cx, |s, cx| s.set_value(content, window, cx));
            }
        }
        if self.soul_memory.take_sync(SYNC_BOT_CONFIG) {
            let focus_val = self.soul_memory.look_around_config.focus.clone();
            if let Some(i) = &self.soul_memory.look_around_focus_input {
                i.update(cx, |s, cx| s.set_value(focus_val, window, cx));
            }
        }
    }

    pub fn save_soul(&mut self, bot_id: BotId, content: String, cx: &mut Context<Self>) {
        let _ = self.try_save_soul(bot_id, content, cx);
    }

    pub(super) fn try_save_soul(
        &mut self,
        bot_id: BotId,
        content: String,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.soul_memory.busy {
            return false;
        }
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let origin_detail_gen = self.detail_gen;
        let expected_bot = bot_id.clone();
        self.soul_memory.busy = true;
        self.soul_memory.saving_soul_content = Some(content.clone());
        self.error = None;
        self.notice = None;
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            let saved_copy = content.clone();
            let res = cx
                .background_executor()
                .spawn(
                    async move { call_checked(&rpc, &BotRpcRequest::SetSoul { bot_id, content }) },
                )
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.detail_gen != origin_detail_gen
                    || this.selected_bot_id.as_ref() != Some(&expected_bot)
                {
                    return;
                }
                this.soul_memory.busy = false;
                this.soul_memory.saving_soul_content = None;
                match res {
                    Ok(_) => {
                        this.soul_memory.soul_loaded = saved_copy;
                        this.panel_switch_dismissed_for = None;
                        this.notice = Some(SharedString::from(text::SOUL_SAVED.get()));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
        true
    }

    pub(super) fn flush_unsaved_soul_edits(&mut self, cx: &mut Context<Self>) {
        let Some(bot_id) = self.selected_bot_id.clone() else {
            return;
        };
        let Some(input) = &self.soul_memory.soul_input else {
            return;
        };
        let draft = input.read(cx).value().to_string();
        if draft != self.soul_memory.soul_loaded {
            let _ = self.try_save_soul(bot_id, draft, cx);
        }
    }

    pub fn save_memory(&mut self, bot_id: BotId, content: String, cx: &mut Context<Self>) {
        if self.soul_memory.busy {
            return;
        }
        if let Err(violation) = validate_memory_edit(&self.soul_memory.memory_loaded, &content) {
            self.soul_memory.deletion_violation = Some(violation);
            self.notice = None;
            cx.notify();
            return;
        }
        self.soul_memory.deletion_violation = None;
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let origin_detail_gen = self.detail_gen;
        let expected_bot = bot_id.clone();
        self.soul_memory.busy = true;
        self.error = None;
        self.notice = None;
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_hist = bot_id.clone();
            let sent = content.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let resp = call_checked(
                        &rpc,
                        &BotRpcRequest::UpdateMemory {
                            bot_id,
                            content,
                            allow_deletions: false,
                        },
                    )?;
                    let hist = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemoryHistory {
                            bot_id: bot_id_hist,
                            limit: Some(25),
                        },
                    ) {
                        Ok(BotRpcResponse::MemoryHistory(entries)) => Some(entries),
                        _ => None,
                    };
                    Ok::<_, String>((resp, hist))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.detail_gen != origin_detail_gen
                    || this.selected_bot_id.as_ref() != Some(&expected_bot)
                {
                    return;
                }
                this.soul_memory.busy = false;
                match res {
                    Ok((
                        BotRpcResponse::MemoryUpdated {
                            content: next_content,
                            restored_facts,
                            ..
                        },
                        hist,
                    )) => {
                        this.soul_memory.memory_loaded = next_content;
                        this.soul_memory.request_sync(SYNC_MEMORY);
                        if !this.soul_memory.has_unsaved_edits(cx) {
                            this.panel_switch_dismissed_for = None;
                        }
                        if let Some(entries) = hist {
                            this.soul_memory.memory_history = entries;
                        }
                        if restored_facts.is_empty() {
                            this.notice = Some(SharedString::from(text::MEMORY_SAVED.get()));
                        } else {
                            this.soul_memory.deletion_violation = Some(MemoryDeletionViolation {
                                deleted_facts: restored_facts.clone(),
                            });
                            let msg = text::MEMORY_RESTORED_NOTICE
                                .format(&[("n", &restored_facts.len().to_string())]);
                            this.notice = Some(SharedString::from(msg));
                        }
                    }
                    Ok((_, hist)) => {
                        this.soul_memory.memory_loaded = sent;
                        if !this.soul_memory.has_unsaved_edits(cx) {
                            this.panel_switch_dismissed_for = None;
                        }
                        if let Some(entries) = hist {
                            this.soul_memory.memory_history = entries;
                        }
                        this.notice = Some(SharedString::from(text::MEMORY_SAVED.get()));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn forget_memory_fact(&mut self, bot_id: BotId, fact: String, cx: &mut Context<Self>) {
        if self.soul_memory.busy {
            return;
        }
        self.soul_memory.armed_forget = None;
        self.soul_memory.deletion_violation = None;
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let origin_detail_gen = self.detail_gen;
        let expected_bot = bot_id.clone();
        self.soul_memory.busy = true;
        self.error = None;
        self.notice = None;
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let resp = call_checked(
                        &rpc,
                        &BotRpcRequest::ForgetMemoryFact {
                            bot_id: bot_id_bg.clone(),
                            fact,
                        },
                    )?;
                    let memory_text = match resp {
                        BotRpcResponse::MemoryUpdated { content, .. } => content,
                        _ => match call_checked(
                            &rpc,
                            &BotRpcRequest::GetMemory {
                                bot_id: bot_id_bg.clone(),
                            },
                        ) {
                            Ok(BotRpcResponse::Text(t)) => t,
                            Err(err) => return Err(err),
                            Ok(_) => String::new(),
                        },
                    };
                    let hist = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemoryHistory {
                            bot_id: bot_id_bg,
                            limit: Some(25),
                        },
                    ) {
                        Ok(BotRpcResponse::MemoryHistory(entries)) => Some(entries),
                        _ => None,
                    };
                    Ok::<_, String>((memory_text, hist))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.detail_gen != origin_detail_gen
                    || this.selected_bot_id.as_ref() != Some(&expected_bot)
                {
                    return;
                }
                this.soul_memory.busy = false;
                match res {
                    Ok((next_memory, hist)) => {
                        this.soul_memory.memory_loaded = next_memory;
                        this.soul_memory.request_sync(SYNC_MEMORY);
                        if let Some(entries) = hist {
                            this.soul_memory.memory_history = entries;
                        }
                        this.notice = Some(SharedString::from(text::FORGET_DONE.get()));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn edit_memory_fact(
        &mut self,
        bot_id: BotId,
        old_fact: String,
        new_fact: &str,
        cx: &mut Context<Self>,
    ) {
        if self.soul_memory.busy {
            return;
        }
        let trimmed = new_fact.trim().to_owned();
        if trimmed.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            cx.notify();
            return;
        }
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let origin_detail_gen = self.detail_gen;
        let expected_bot = bot_id.clone();
        self.soul_memory.busy = true;
        self.error = None;
        self.notice = None;
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let resp = call_checked(
                        &rpc,
                        &BotRpcRequest::EditMemoryFact {
                            bot_id: bot_id_bg.clone(),
                            old_fact,
                            new_fact: trimmed,
                        },
                    )?;
                    let mem = match resp {
                        BotRpcResponse::MemoryUpdated { content, .. } => Some(content),
                        _ => match call_checked(
                            &rpc,
                            &BotRpcRequest::GetMemory {
                                bot_id: bot_id_bg.clone(),
                            },
                        ) {
                            Ok(BotRpcResponse::Text(t)) => Some(t),
                            _ => None,
                        },
                    };
                    let hist = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemoryHistory {
                            bot_id: bot_id_bg,
                            limit: Some(25),
                        },
                    ) {
                        Ok(BotRpcResponse::MemoryHistory(entries)) => Some(entries),
                        _ => None,
                    };
                    Ok::<_, String>((mem, hist))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.detail_gen != origin_detail_gen
                    || this.selected_bot_id.as_ref() != Some(&expected_bot)
                {
                    return;
                }
                this.soul_memory.busy = false;
                match res {
                    Ok((mem, hist)) => {
                        this.soul_memory.editing_fact = None;
                        let handle = this.window_handle;
                        clear_input_on_window(&mut this.soul_memory.fact_edit_input, handle, cx);
                        if let Some(m) = mem {
                            this.soul_memory.memory_loaded = m;
                            this.soul_memory.request_sync(SYNC_MEMORY);
                        }
                        if let Some(entries) = hist {
                            this.soul_memory.memory_history = entries;
                        }
                        this.notice = Some(SharedString::from(text::MEMORY_SAVED.get()));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn undo_memory_change(&mut self, bot_id: BotId, entry_id: String, cx: &mut Context<Self>) {
        if self.soul_memory.busy {
            return;
        }
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let origin_detail_gen = self.detail_gen;
        let expected_bot = bot_id.clone();
        self.soul_memory.busy = true;
        self.error = None;
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let resp = call_checked(
                        &rpc,
                        &BotRpcRequest::UndoMemoryChange {
                            bot_id: bot_id_bg.clone(),
                            entry_id,
                        },
                    )?;
                    let mem = match resp {
                        BotRpcResponse::MemoryUpdated { content, .. } => Some(content),
                        _ => match call_checked(
                            &rpc,
                            &BotRpcRequest::GetMemory {
                                bot_id: bot_id_bg.clone(),
                            },
                        ) {
                            Ok(BotRpcResponse::Text(t)) => Some(t),
                            _ => None,
                        },
                    };
                    let hist = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemoryHistory {
                            bot_id: bot_id_bg,
                            limit: Some(25),
                        },
                    ) {
                        Ok(BotRpcResponse::MemoryHistory(entries)) => Some(entries),
                        _ => None,
                    };
                    Ok::<_, String>((mem, hist))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.detail_gen != origin_detail_gen
                    || this.selected_bot_id.as_ref() != Some(&expected_bot)
                {
                    return;
                }
                this.soul_memory.busy = false;
                match res {
                    Ok((mem, hist)) => {
                        if let Some(m) = mem {
                            this.soul_memory.memory_loaded = m;
                            this.soul_memory.request_sync(SYNC_MEMORY);
                        }
                        if let Some(entries) = hist {
                            this.soul_memory.memory_history = entries;
                        }
                        this.notice = Some(SharedString::from(text::MEMORY_SAVED.get()));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn run_memory_tidy(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.soul_memory.tidy_running {
            return;
        }
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let origin_detail_gen = self.detail_gen;
        let expected_bot = bot_id.clone();
        self.soul_memory.tidy_running = true;
        self.error = None;
        self.tidy_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let tidy = call_checked(
                        &rpc,
                        &BotRpcRequest::RunMemoryTidy {
                            bot_id: bot_id_bg.clone(),
                        },
                    )?;
                    let mem = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemory {
                            bot_id: bot_id_bg.clone(),
                        },
                    ) {
                        Ok(BotRpcResponse::Text(t)) => Some(t),
                        _ => None,
                    };
                    let status = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemoryTidyStatus {
                            bot_id: bot_id_bg.clone(),
                        },
                    ) {
                        Ok(BotRpcResponse::MemoryTidyStatus(st)) => Some(st),
                        _ => None,
                    };
                    let hist = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemoryHistory {
                            bot_id: bot_id_bg.clone(),
                            limit: Some(25),
                        },
                    ) {
                        Ok(BotRpcResponse::MemoryHistory(entries)) => Some(entries),
                        _ => None,
                    };
                    let topics = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListMemoryTopics { bot_id: bot_id_bg },
                    ) {
                        Ok(BotRpcResponse::MemoryTopics(list)) => Some(list),
                        _ => None,
                    };
                    Ok::<_, String>((tidy, mem, status, hist, topics))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.detail_gen != origin_detail_gen
                    || this.selected_bot_id.as_ref() != Some(&expected_bot)
                {
                    return;
                }
                this.soul_memory.tidy_running = false;
                match res {
                    Ok((
                        BotRpcResponse::MemoryTidyResult {
                            summary, changes, ..
                        },
                        mem,
                        status,
                        hist,
                        topics,
                    )) => {
                        if let Some(m) = mem {
                            this.soul_memory.memory_loaded = m;
                            this.soul_memory.request_sync(SYNC_MEMORY);
                        }
                        if let Some(st) = status {
                            this.soul_memory.tidy_status = Some(st);
                        }
                        if let Some(entries) = hist {
                            this.soul_memory.memory_history = entries;
                        }
                        if let Some(list) = topics {
                            this.soul_memory.memory_topics = list;
                        }
                        let msg = text::TIDY_RESULT
                            .format(&[("changes", &changes.to_string()), ("summary", &summary)]);
                        this.soul_memory.last_tidy_summary = Some(SharedString::from(msg.clone()));
                        this.notice = Some(SharedString::from(msg));
                    }
                    Ok((_, mem, status, hist, topics)) => {
                        if let Some(m) = mem {
                            this.soul_memory.memory_loaded = m;
                            this.soul_memory.request_sync(SYNC_MEMORY);
                        }
                        if let Some(st) = status {
                            this.soul_memory.tidy_status = Some(st);
                        }
                        if let Some(entries) = hist {
                            this.soul_memory.memory_history = entries;
                        }
                        if let Some(list) = topics {
                            this.soul_memory.memory_topics = list;
                        }
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn undo_memory_tidy(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.soul_memory.tidy_running {
            return;
        }
        let tidy_id = self
            .soul_memory
            .tidy_status
            .as_ref()
            .and_then(|st| st.last_tidy_id.clone());
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let origin_detail_gen = self.detail_gen;
        let expected_bot = bot_id.clone();
        self.soul_memory.tidy_running = true;
        self.error = None;
        self.tidy_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let resp = call_checked(
                        &rpc,
                        &BotRpcRequest::UndoMemoryTidy {
                            bot_id: bot_id_bg.clone(),
                            tidy_id,
                        },
                    )?;
                    let mem = match resp {
                        BotRpcResponse::MemoryUpdated { content, .. } => Some(content),
                        _ => match call_checked(
                            &rpc,
                            &BotRpcRequest::GetMemory {
                                bot_id: bot_id_bg.clone(),
                            },
                        ) {
                            Ok(BotRpcResponse::Text(t)) => Some(t),
                            _ => None,
                        },
                    };
                    let status = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemoryTidyStatus {
                            bot_id: bot_id_bg.clone(),
                        },
                    ) {
                        Ok(BotRpcResponse::MemoryTidyStatus(st)) => Some(st),
                        _ => None,
                    };
                    let hist = match call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemoryHistory {
                            bot_id: bot_id_bg.clone(),
                            limit: Some(25),
                        },
                    ) {
                        Ok(BotRpcResponse::MemoryHistory(entries)) => Some(entries),
                        _ => None,
                    };
                    let topics = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListMemoryTopics { bot_id: bot_id_bg },
                    ) {
                        Ok(BotRpcResponse::MemoryTopics(list)) => Some(list),
                        _ => None,
                    };
                    Ok::<_, String>((mem, status, hist, topics))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.detail_gen != origin_detail_gen
                    || this.selected_bot_id.as_ref() != Some(&expected_bot)
                {
                    return;
                }
                this.soul_memory.tidy_running = false;
                match res {
                    Ok((mem, status, hist, topics)) => {
                        if let Some(m) = mem {
                            this.soul_memory.memory_loaded = m;
                            this.soul_memory.request_sync(SYNC_MEMORY);
                        }
                        if let Some(st) = status {
                            this.soul_memory.tidy_status = Some(st);
                        }
                        if let Some(entries) = hist {
                            this.soul_memory.memory_history = entries;
                        }
                        if let Some(list) = topics {
                            this.soul_memory.memory_topics = list;
                        }
                        this.notice = Some(SharedString::from(text::MEMORY_SAVED.get()));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn save_memory_tidy_config(
        &mut self,
        bot_id: BotId,
        cfg: BotMemoryTidyConfig,
        cx: &mut Context<Self>,
    ) {
        if self.soul_memory.busy {
            return;
        }
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let origin_detail_gen = self.detail_gen;
        let expected_bot = bot_id.clone();
        self.soul_memory.busy = true;
        self.error = None;
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                            bot_id,
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
                            memory_tidy: Some(cfg),
                            mcp_tools: None,
                        })),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.detail_gen != origin_detail_gen
                    || this.selected_bot_id.as_ref() != Some(&expected_bot)
                {
                    return;
                }
                this.soul_memory.busy = false;
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        this.soul_memory.tidy_config = updated.memory_tidy;
                        this.upsert_bot_config(*updated);
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                    }
                    Ok(_) => {
                        this.soul_memory.tidy_config = cfg;
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn select_memory_topic(&mut self, bot_id: BotId, topic: &str, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let origin_detail_gen = self.detail_gen;
        let expected_bot = bot_id.clone();
        self.error = None;
        self.notice = None;
        let topic_owned = topic.to_owned();
        self.topic_read_task = Some(cx.spawn(async move |this, cx| {
            let topic_bg = topic_owned.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::GetMemoryTopic {
                            bot_id,
                            topic: topic_bg,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.detail_gen != origin_detail_gen
                    || this.selected_bot_id.as_ref() != Some(&expected_bot)
                {
                    return;
                }
                match res {
                    Ok(BotRpcResponse::Text(content)) => {
                        this.soul_memory.selected_topic = Some(topic_owned);
                        this.soul_memory.topic_loaded = content;
                        this.soul_memory.request_sync(SYNC_TOPIC);
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected GetMemoryTopic response: {other:?}"
                        )));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub fn save_memory_topic(
        &mut self,
        bot_id: BotId,
        topic: &str,
        content: String,
        cx: &mut Context<Self>,
    ) {
        if self.soul_memory.busy {
            return;
        }
        let topic = topic.trim().to_owned();
        if topic.is_empty() {
            self.error = Some(SharedString::from(text::ERR_EMPTY_FIELD.get()));
            cx.notify();
            return;
        }
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let origin_detail_gen = self.detail_gen;
        let expected_bot = bot_id.clone();
        self.soul_memory.busy = true;
        self.error = None;
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let topic_bg = topic.clone();
            let content_bg = content.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::SetMemoryTopic {
                            bot_id: bot_id_bg.clone(),
                            topic: topic_bg,
                            content: content_bg,
                        },
                    )?;
                    let topics = match call_checked(
                        &rpc,
                        &BotRpcRequest::ListMemoryTopics { bot_id: bot_id_bg },
                    ) {
                        Ok(BotRpcResponse::MemoryTopics(list)) => Some(list),
                        _ => None,
                    };
                    Ok::<_, String>(topics)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.detail_gen != origin_detail_gen
                    || this.selected_bot_id.as_ref() != Some(&expected_bot)
                {
                    return;
                }
                this.soul_memory.busy = false;
                match res {
                    Ok(topics) => {
                        this.soul_memory.selected_topic = Some(topic);
                        this.soul_memory.topic_loaded = content;
                        if let Some(list) = topics {
                            this.soul_memory.memory_topics = list;
                        }
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn run_look_around(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.soul_memory.is_looking_around(&bot_id) {
            return;
        }
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let req_host = self.host.clone();
        let bot_id_key = bot_id.clone();
        self.soul_memory.look_around_running.insert(bot_id.clone());
        self.error = None;
        let task = cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(&rpc, &BotRpcRequest::RunLookAround { bot_id: bot_id_bg })
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen || this.host != req_host {
                    return;
                }
                this.soul_memory.look_around_running.remove(&bot_id);
                this.look_tasks.remove(&bot_id);
                if this.selected_bot_id.as_ref() != Some(&bot_id) {
                    cx.notify();
                    return;
                }
                match res {
                    Ok(
                        BotRpcResponse::LookAroundResult { suggestions, .. }
                        | BotRpcResponse::Suggestions(suggestions),
                    ) => {
                        this.soul_memory.suggestions = suggestions;
                    }
                    Ok(other) => {
                        this.error = Some(SharedString::from(format!(
                            "unexpected RunLookAround response: {other:?}"
                        )));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.look_tasks.insert(bot_id_key, task);
        cx.notify();
    }

    pub fn save_look_around_config(
        &mut self,
        bot_id: BotId,
        mut cfg: BotLookAroundConfig,
        cx: &mut Context<Self>,
    ) {
        if self.soul_memory.busy {
            return;
        }
        if let Some(input) = &self.soul_memory.look_around_focus_input {
            input.read(cx).value().trim().clone_into(&mut cfg.focus);
        }
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let req_host = self.host.clone();
        let bot_id_check = bot_id.clone();
        self.soul_memory.busy = true;
        self.error = None;
        let cfg_bg = cfg.clone();
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::UpdateBot(Box::new(UpdateBotParams {
                            bot_id,
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
                            look_around: Some(cfg_bg),
                            memory_tidy: None,
                            mcp_tools: None,
                        })),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen || this.host != req_host {
                    return;
                }
                let same_bot = this.selected_bot_id.as_ref() == Some(&bot_id_check);
                if same_bot {
                    this.soul_memory.busy = false;
                }
                match res {
                    Ok(BotRpcResponse::Bot(updated)) => {
                        if same_bot {
                            this.soul_memory.look_around_config = updated.look_around.clone();
                            this.notice = Some(SharedString::from(text::SAVED.get()));
                        }
                        this.upsert_bot_config(*updated);
                    }
                    Ok(_) if same_bot => {
                        this.soul_memory.look_around_config = cfg;
                        this.notice = Some(SharedString::from(text::SAVED.get()));
                    }
                    Err(err) if same_bot => this.error = Some(SharedString::from(err)),
                    Ok(_) | Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn start_suggestion(
        &mut self,
        bot_id: BotId,
        suggestion_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.soul_memory.busy {
            return;
        }
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let req_host = self.host.clone();
        self.soul_memory.busy = true;
        self.error = None;
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let sid_bg = suggestion_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::StartSuggestion {
                            bot_id: bot_id_bg,
                            suggestion_id: sid_bg,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.host != req_host
                    || this.selected_bot_id.as_ref() != Some(&bot_id)
                {
                    return;
                }
                this.soul_memory.busy = false;
                match res {
                    Ok(BotRpcResponse::SessionStarted {
                        bot_id, session_id, ..
                    }) => {
                        this.soul_memory
                            .suggestions
                            .retain(|s| s.id != suggestion_id);
                        cx.emit(OpenSession { bot_id, session_id });
                    }
                    Ok(BotRpcResponse::Item(item)) => {
                        this.soul_memory
                            .suggestions
                            .retain(|s| s.id != suggestion_id);
                        this.items.push_front_item(*item);
                        this.sync_open_items_for_bot(&bot_id);
                        this.emit_badge(cx);
                    }
                    Ok(_) => {
                        this.soul_memory
                            .suggestions
                            .retain(|s| s.id != suggestion_id);
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn dismiss_suggestion(
        &mut self,
        bot_id: BotId,
        suggestion_id: String,
        cx: &mut Context<Self>,
    ) {
        if self.soul_memory.busy {
            return;
        }
        let rpc = self.rpc.clone();
        let origin_host_gen = self.host_gen;
        let req_host = self.host.clone();
        let bot_id_check = bot_id.clone();
        self.soul_memory.busy = true;
        self.error = None;
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            let sid_bg = suggestion_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    call_checked(
                        &rpc,
                        &BotRpcRequest::DismissSuggestion {
                            bot_id,
                            suggestion_id: sid_bg,
                        },
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.host_gen != origin_host_gen
                    || this.host != req_host
                    || this.selected_bot_id.as_ref() != Some(&bot_id_check)
                {
                    return;
                }
                this.soul_memory.busy = false;
                match res {
                    Ok(_) => {
                        this.soul_memory
                            .suggestions
                            .retain(|s| s.id != suggestion_id);
                    }
                    Err(err) => this.error = Some(SharedString::from(err)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(super) fn render_soul_memory_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_soul_memory_inputs(bot, window, cx);
        let soul_save_id = bot.id.clone();
        let mem_save_id = bot.id.clone();
        let tidy_bot_id = bot.id.clone();
        let undo_tidy_bot_id = bot.id.clone();
        let tidy_cfg_bot_id = bot.id.clone();
        let look_bot_id = bot.id.clone();
        let look_cfg_bot_id = bot.id.clone();
        let topic_save_bot_id = bot.id.clone();

        let current_memory = self.soul_memory.memory_input.as_ref().map_or_else(
            || self.soul_memory.memory_loaded.clone(),
            |i| i.read(cx).value().to_string(),
        );
        let line_count = count_memory_lines(&current_memory);
        let over_cap = line_count > BOT_MEMORY_INDEX_LINE_CAP;
        let line_badge = text::MEMORY_LINE_COUNT.format(&[
            ("lines", &line_count.to_string()),
            ("cap", &BOT_MEMORY_INDEX_LINE_CAP.to_string()),
        ]);
        let facts = parse_memory_facts(&self.soul_memory.memory_loaded);
        let facts_heading = text::MEMORY_FACTS_HEADING.format(&[("n", &facts.len().to_string())]);
        let suggestions_heading = text::SUGGESTIONS_HEADING
            .format(&[("n", &self.soul_memory.suggestions.len().to_string())]);
        let is_dirty = self.soul_memory.has_unsaved_edits(cx);

        let soul_card = ui::card(p)
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_SM)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .child(text::SOUL_SECTION.get()),
                                    )
                                    .when(is_dirty, |d| {
                                        d.child(ui::badge(
                                            p,
                                            ui::BadgeTone::Warning,
                                            text::DIRTY_UNSAVED_BADGE.get(),
                                        ))
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::SOUL_DESC.get()),
                            ),
                    )
                    .child(
                        ui::secondary("bots-save-soul", text::SAVE_SOUL_BTN.get())
                            .debug_selector(|| "bots-save-soul".into())
                            .disabled(self.soul_memory.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let content = this
                                    .soul_memory
                                    .soul_input
                                    .as_ref()
                                    .map(|i| i.read(cx).value().to_string())
                                    .unwrap_or_default();
                                this.save_soul(soul_save_id.clone(), content, cx);
                            })),
                    ),
            )
            .children(
                self.soul_memory
                    .soul_input
                    .as_ref()
                    .map(|i| Textarea::new(i).small()),
            );

        let violation_banner = self.soul_memory.deletion_violation.as_ref().map(|v| {
            let preview = v.deleted_facts.join(", ");
            let msg = text::MEMORY_DELETION_VIOLATION.format(&[
                ("n", &v.deleted_facts.len().to_string()),
                ("facts", &preview),
            ]);
            ui::error_banner(p, msg).debug_selector(|| "bots-memory-deletion-violation".into())
        });

        let memory_card = ui::card(p)
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(ui::TEXT_SM)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .child(text::MEMORY_SECTION.get()),
                                    )
                                    .child(ui::badge(
                                        p,
                                        if over_cap {
                                            ui::BadgeTone::Warning
                                        } else {
                                            ui::BadgeTone::Neutral
                                        },
                                        line_badge,
                                    )),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::MEMORY_DESC.get()),
                            ),
                    )
                    .child(
                        ui::secondary("bots-save-memory", text::SAVE_MEMORY_BTN.get())
                            .debug_selector(|| "bots-save-memory".into())
                            .disabled(self.soul_memory.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let content = this
                                    .soul_memory
                                    .memory_input
                                    .as_ref()
                                    .map(|i| i.read(cx).value().to_string())
                                    .unwrap_or_default();
                                this.save_memory(mem_save_id.clone(), content, cx);
                            })),
                    ),
            )
            .children(violation_banner)
            .children(
                self.soul_memory
                    .memory_input
                    .as_ref()
                    .map(|i| Textarea::new(i).small()),
            );

        let mut facts_card = ui::card(p).flex().flex_col().gap_2().child(
            div()
                .text_size(ui::TEXT_SM)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.foreground)
                .child(facts_heading),
        );
        if facts.is_empty() {
            facts_card = facts_card.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::MEMORY_FACTS_EMPTY.get()),
            );
        } else {
            for (idx, fact) in facts.into_iter().enumerate() {
                let fact_text = fact.text.clone();
                let fact_for_edit_start = fact.text.clone();
                let fact_for_edit_save = fact.text.clone();
                let armed = self.soul_memory.armed_forget.as_deref() == Some(fact_text.as_str());
                let is_editing = self
                    .soul_memory
                    .editing_fact
                    .as_ref()
                    .is_some_and(|(row_idx, orig)| *row_idx == idx && orig == &fact_text);
                let bot_forget = bot.id.clone();
                let bot_edit = bot.id.clone();
                let btn_id = format!("bots-forget-fact-{idx}");
                let btn_sel = btn_id.clone();

                let mut row = div()
                    .w_full()
                    .min_w_0()
                    .px_2p5()
                    .py_1p5()
                    .rounded(ui::RADIUS_SM)
                    .bg(p.muted.opacity(0.35))
                    .border_1()
                    .border_color(p.border)
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .gap_1p5()
                                    .when_some(fact.date, |d, dt| {
                                        d.child(ui::badge(p, ui::BadgeTone::Muted, dt))
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(ui::TEXT_XS)
                                            .text_color(p.foreground)
                                            .child(fact.text),
                                    )
                                    .when_some(fact.source, |d, src| {
                                        d.child(ui::badge(p, ui::BadgeTone::Neutral, src))
                                    }),
                            )
                            .child(
                                ui::controls()
                                    .child(
                                        ui::ghost(
                                            SharedString::from(format!("bots-edit-fact-{idx}")),
                                            if is_editing {
                                                text::CANCEL.get()
                                            } else {
                                                text::FACT_EDIT_BTN.get()
                                            },
                                        )
                                        .disabled(self.soul_memory.busy)
                                        .on_click(cx.listener(
                                            move |this, _, window, cx| {
                                                let already_editing = this
                                                    .soul_memory
                                                    .editing_fact
                                                    .as_ref()
                                                    .is_some_and(|(row_idx, orig)| {
                                                        *row_idx == idx
                                                            && orig == &fact_for_edit_start
                                                    });
                                                if already_editing {
                                                    this.soul_memory.editing_fact = None;
                                                } else {
                                                    let cur = fact_for_edit_start.clone();
                                                    let bot_submit = this.selected_bot_id.clone();
                                                    let old_submit = cur.clone();
                                                    let inp = cx.new(|cx| {
                                                        let mut s = InputState::new(window, cx);
                                                        s.set_value(cur, window, cx);
                                                        s
                                                    });
                                                    this.soul_memory.fact_edit_sub = Some(
                                                        cx.subscribe_in(
                                                            &inp,
                                                            window,
                                                            move |this,
                                                                  ent,
                                                                  ev: &InputEvent,
                                                                  window,
                                                                  cx| {
                                                                if matches!(
                                                                    ev,
                                                                    InputEvent::PressEnter {
                                                                        secondary: false,
                                                                        shift: false,
                                                                    }
                                                                ) && !crate::jump::entity_input_is_composing(
                                                                    ent, window, cx,
                                                                ) && let Some(bid) = &bot_submit {
                                                                    let val = ent
                                                                        .read(cx)
                                                                        .value()
                                                                        .to_string();
                                                                    this.edit_memory_fact(
                                                                        bid.clone(),
                                                                        old_submit.clone(),
                                                                        &val,
                                                                        cx,
                                                                    );
                                                                }
                                                            },
                                                        ),
                                                    );
                                                    this.soul_memory.fact_edit_input = Some(inp);
                                                    this.soul_memory.editing_fact =
                                                        Some((idx, fact_for_edit_start.clone()));
                                                }
                                                cx.notify();
                                            },
                                        )),
                                    )
                                    .child(
                                        ui::danger_confirm(
                                            SharedString::from(btn_id),
                                            text::FORGET_BTN.get(),
                                            text::FORGET_CONFIRM.get(),
                                            armed,
                                        )
                                        .debug_selector(move || btn_sel.clone())
                                        .disabled(self.soul_memory.busy)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if this.soul_memory.armed_forget.as_deref()
                                                == Some(fact_text.as_str())
                                            {
                                                this.forget_memory_fact(
                                                    bot_forget.clone(),
                                                    fact_text.clone(),
                                                    cx,
                                                );
                                            } else {
                                                this.soul_memory.armed_forget =
                                                    Some(fact_text.clone());
                                                cx.notify();
                                            }
                                        })),
                                    ),
                            ),
                    );
                if is_editing && let Some(inp) = &self.soul_memory.fact_edit_input {
                    let inp_read = inp.clone();
                    row = row.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().child(Input::new(inp).small()))
                            .child(
                                ui::primary(
                                    SharedString::from(format!("bots-save-fact-{idx}")),
                                    text::FACT_SAVE_BTN.get(),
                                )
                                .disabled(self.soul_memory.busy)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let val = inp_read.read(cx).value().to_string();
                                        this.edit_memory_fact(
                                            bot_edit.clone(),
                                            fact_for_edit_save.clone(),
                                            &val,
                                            cx,
                                        );
                                    },
                                )),
                            ),
                    );
                }
                facts_card = facts_card.child(row);
            }
        }

        // Memory History & 1-click Undo
        let hist_heading = text::MEMORY_HISTORY_HEADING
            .format(&[("n", &self.soul_memory.memory_history.len().to_string())]);
        let mut history_card = ui::card(p)
            .debug_selector(|| "bots-memory-history-card".into())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(hist_heading),
            );
        if self.soul_memory.memory_history.is_empty() {
            history_card = history_card.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::MEMORY_HISTORY_EMPTY.get()),
            );
        } else {
            for (hidx, entry) in self.soul_memory.memory_history.iter().take(10).enumerate() {
                let entry_id = entry.entry_id.clone();
                let bot_undo = bot.id.clone();
                let summary = format!(
                    "{} ({}) +{} / -{}",
                    entry.action,
                    entry.actor,
                    entry.added.len(),
                    entry.removed.len()
                );
                history_card = history_card.child(
                    div()
                        .w_full()
                        .px_2p5()
                        .py_1p5()
                        .rounded(ui::RADIUS_SM)
                        .border_1()
                        .border_color(p.border)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(ui::TEXT_XS)
                                .text_color(p.foreground)
                                .child(summary),
                        )
                        .child(if entry.undone {
                            ui::badge(p, ui::BadgeTone::Muted, text::MEMORY_UNDONE_BADGE.get())
                                .into_any_element()
                        } else if entry.undoable {
                            ui::secondary(
                                SharedString::from(format!("bots-memory-undo-{hidx}")),
                                text::MEMORY_UNDO_BTN.get(),
                            )
                            .debug_selector(move || format!("bots-memory-undo-{hidx}"))
                            .disabled(self.soul_memory.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.undo_memory_change(bot_undo.clone(), entry_id.clone(), cx);
                            }))
                            .into_any_element()
                        } else {
                            div().into_any_element()
                        }),
                );
            }
        }

        // Memory Tidy-up + Schedule Config + Undo Last Tidy
        let tidy_status = self.soul_memory.tidy_status.as_ref();
        let can_undo_tidy = tidy_status.is_some_and(|s| s.undoable);
        let tidy_cfg = self.soul_memory.tidy_config;
        let tidy_cfg_bot_toggle = tidy_cfg_bot_id.clone();
        let tidy_card = ui::card(p)
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.foreground)
                                    .child(text::TIDY_SECTION.get()),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::TIDY_DESC.get()),
                            ),
                    )
                    .child(
                        ui::controls()
                            .when(can_undo_tidy, |d| {
                                d.child(
                                    ui::secondary(
                                        "bots-undo-memory-tidy",
                                        text::MEMORY_TIDY_UNDO_BTN.get(),
                                    )
                                    .disabled(self.soul_memory.tidy_running)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.undo_memory_tidy(undo_tidy_bot_id.clone(), cx);
                                        },
                                    )),
                                )
                            })
                            .child(
                                ui::button_with_reserved_label(
                                    "bots-run-memory-tidy",
                                    text::RUN_TIDY_BTN.get(),
                                    text::TIDY_RUNNING.get(),
                                    self.soul_memory.tidy_running,
                                )
                                .debug_selector(|| "bots-run-memory-tidy".into())
                                .disabled(self.soul_memory.tidy_running)
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.run_memory_tidy(tidy_bot_id.clone(), cx);
                                    },
                                )),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child({
                        let ent = cx.entity();
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(ui::label(p, text::MEMORY_TIDY_AUTO_LABEL.get(), None))
                            .child(
                                ui::switch(
                                    p,
                                    "bots-tidy-auto-switch",
                                    tidy_cfg.enabled,
                                    self.soul_memory.busy,
                                    move |checked, _, cx| {
                                        let bid = tidy_cfg_bot_toggle.clone();
                                        ent.update(cx, |this, cx| {
                                            let mut next = this.soul_memory.tidy_config;
                                            next.enabled = checked;
                                            this.save_memory_tidy_config(bid, next, cx);
                                        });
                                    },
                                )
                                .aria_label(text::MEMORY_TIDY_AUTO_LABEL.get()),
                            )
                    })
                    .child(ui::segmented_control(
                        p,
                        INTERVAL_PRESETS.into_iter().map(|(label, secs)| {
                            let bid = tidy_cfg_bot_id.clone();
                            ui::segment(
                                p,
                                format!("bots-tidy-interval-{secs}"),
                                label,
                                tidy_cfg.interval_secs == secs,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let mut next = this.soul_memory.tidy_config;
                                    next.interval_secs = secs;
                                    this.save_memory_tidy_config(bid.clone(), next, cx);
                                },
                            ))
                        }),
                    )),
            )
            .when_some(
                tidy_status.filter(|st| st.consecutive_failures > 0),
                |d, st| {
                    d.child(ui::badge(
                        p,
                        ui::BadgeTone::Warning,
                        text::MEMORY_TIDY_STATUS_FAILS
                            .format(&[("n", &st.consecutive_failures.to_string())]),
                    ))
                },
            )
            .when_some(self.soul_memory.last_tidy_summary.as_ref(), |d, summary| {
                d.child(
                    div()
                        .text_size(ui::TEXT_XS)
                        .text_color(p.muted_foreground)
                        .child(summary.clone()),
                )
            });

        // Memory Topics (`memory/<topic>.md`)
        let topics_heading = text::MEMORY_TOPICS_HEADING
            .format(&[("n", &self.soul_memory.memory_topics.len().to_string())]);
        let mut topics_pills = div().flex().flex_wrap().items_center().gap_1p5();
        if self.soul_memory.memory_topics.is_empty() {
            topics_pills = topics_pills.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::MEMORY_TOPICS_EMPTY.get()),
            );
        } else {
            for info in &self.soul_memory.memory_topics {
                let topic = info.topic.clone();
                let bid = bot.id.clone();
                let selected = self.soul_memory.selected_topic.as_deref() == Some(topic.as_str());
                topics_pills = topics_pills.child(
                    ui::segment(
                        p,
                        SharedString::from(format!("bots-topic-{topic}")),
                        format!("memory/{topic}.md"),
                        selected,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_memory_topic(bid.clone(), &topic, cx);
                    })),
                );
            }
        }
        let topics_card = ui::card(p)
            .debug_selector(|| "bots-memory-topics-card".into())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .text_size(ui::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(topics_heading),
            )
            .child(topics_pills)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(
                        self.soul_memory
                            .topic_name_input
                            .as_ref()
                            .map(|i| div().flex_1().min_w_0().child(Input::new(i).small())),
                    )
                    .child(
                        ui::secondary("bots-memory-topic-save", text::MEMORY_TOPIC_SAVE_BTN.get())
                            .disabled(self.soul_memory.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let name = this
                                    .soul_memory
                                    .topic_name_input
                                    .as_ref()
                                    .map(|i| i.read(cx).value().to_string())
                                    .unwrap_or_default();
                                let body = this
                                    .soul_memory
                                    .topic_content_input
                                    .as_ref()
                                    .map(|i| i.read(cx).value().to_string())
                                    .unwrap_or_default();
                                this.save_memory_topic(topic_save_bot_id.clone(), &name, body, cx);
                            })),
                    ),
            )
            .children(
                self.soul_memory
                    .topic_content_input
                    .as_ref()
                    .map(|i| Textarea::new(i).small()),
            );

        // Look Around config + proactive suggestions
        let look_cfg = self.soul_memory.look_around_config.clone();
        let look_cfg_save = look_cfg_bot_id.clone();
        let mut look_card = ui::card(p)
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_size(ui::TEXT_SM)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(p.foreground)
                                    .child(text::LOOK_AROUND_SECTION.get()),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::LOOK_AROUND_DESC.get()),
                            ),
                    )
                    .child({
                        let look_running = self.soul_memory.is_looking_around(&bot.id);
                        ui::button_with_reserved_label(
                            "bots-run-look-around",
                            text::RUN_LOOK_AROUND_BTN.get(),
                            text::LOOK_AROUND_RUNNING.get(),
                            look_running,
                        )
                        .debug_selector(|| "bots-run-look-around".into())
                        .disabled(look_running)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.run_look_around(look_bot_id.clone(), cx);
                        }))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child({
                        let ent = cx.entity();
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(ui::label(p, text::LOOK_AROUND_ENABLED_LABEL.get(), None))
                            .child(
                                ui::switch(
                                    p,
                                    "bots-look-around-enabled",
                                    look_cfg.enabled,
                                    self.soul_memory.busy,
                                    move |checked, _, cx| {
                                        ent.update(cx, |this, cx| {
                                            this.soul_memory.look_around_config.enabled = checked;
                                            cx.notify();
                                        });
                                    },
                                )
                                .aria_label(text::LOOK_AROUND_ENABLED_LABEL.get()),
                            )
                    })
                    .child({
                        let ent = cx.entity();
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(ui::label(p, text::LOOK_AROUND_WEEKDAYS_LABEL.get(), None))
                            .child(
                                ui::switch(
                                    p,
                                    "bots-look-around-weekdays",
                                    look_cfg.weekdays_only,
                                    self.soul_memory.busy,
                                    move |checked, _, cx| {
                                        ent.update(cx, |this, cx| {
                                            this.soul_memory.look_around_config.weekdays_only =
                                                checked;
                                            cx.notify();
                                        });
                                    },
                                )
                                .aria_label(text::LOOK_AROUND_WEEKDAYS_LABEL.get()),
                            )
                    })
                    .child(ui::segmented_control(
                        p,
                        INTERVAL_PRESETS.into_iter().map(|(label, secs)| {
                            ui::segment(
                                p,
                                format!("bots-look-interval-{secs}"),
                                label,
                                look_cfg.interval_secs == secs,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.soul_memory.look_around_config.interval_secs = secs;
                                    cx.notify();
                                },
                            ))
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(
                        self.soul_memory
                            .look_around_focus_input
                            .as_ref()
                            .map(|i| div().flex_1().min_w_0().child(Input::new(i).small())),
                    )
                    .child(
                        ui::secondary("bots-look-around-save", text::LOOK_AROUND_SAVE_BTN.get())
                            .disabled(self.soul_memory.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let cfg = this.soul_memory.look_around_config.clone();
                                this.save_look_around_config(look_cfg_save.clone(), cfg, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .text_size(ui::TEXT_XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.foreground)
                    .child(suggestions_heading),
            );

        if self.soul_memory.suggestions.is_empty() {
            look_card = look_card.child(
                div()
                    .text_size(ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(text::SUGGESTIONS_EMPTY.get()),
            );
        } else {
            for (sidx, s) in self.soul_memory.suggestions.clone().into_iter().enumerate() {
                let why_line = text::SUGGESTION_WHY.format(&[("text", &s.why)]);
                let ask_line = text::SUGGESTION_ASK.format(&[("text", &s.ask)]);
                let bot_track = bot.id.clone();
                let bot_start = bot.id.clone();
                let bot_dismiss = bot.id.clone();
                let sid_start = s.id.clone();
                let sid_dismiss = s.id.clone();
                let track_title = s.title.clone();
                let track_desc = format!("{}\n{}", s.why, s.ask);
                let link_url = s.link.as_deref().and_then(super::sanitize_http_url);
                look_card =
                    look_card.child(
                        div()
                            .id(SharedString::from(format!("bots-suggestion-{sidx}")))
                            .debug_selector(move || format!("bots-suggestion-{sidx}"))
                            .w_full()
                            .p_2p5()
                            .rounded(ui::RADIUS_SM)
                            .bg(p.muted.opacity(0.35))
                            .border_1()
                            .border_color(p.border)
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(ui::TEXT_SM)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.foreground)
                                            .child(s.title),
                                    )
                                    .child(
                                        ui::controls()
                                            .when_some(link_url, |d, url| {
                                                d.child(
                                                    ui::ghost(
                                                        SharedString::from(format!(
                                                            "bots-suggestion-link-{sidx}"
                                                        )),
                                                        text::OPEN_URL_BTN.get(),
                                                    )
                                                    .on_click(move |_, _, cx| {
                                                        if let Some(clean) =
                                                            super::sanitize_http_url(&url)
                                                        {
                                                            cx.open_url(&clean);
                                                        }
                                                    }),
                                                )
                                            })
                                            .child(
                                                ui::primary(
                                                    SharedString::from(format!(
                                                        "bots-suggestion-start-{sidx}"
                                                    )),
                                                    text::SUGGESTION_START_BTN.get(),
                                                )
                                                .debug_selector(move || {
                                                    format!("bots-suggestion-start-{sidx}")
                                                })
                                                .disabled(self.soul_memory.busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.start_suggestion(
                                                        bot_start.clone(),
                                                        sid_start.clone(),
                                                        cx,
                                                    );
                                                })),
                                            )
                                            .child(
                                                ui::secondary(
                                                    SharedString::from(format!(
                                                        "bots-suggestion-track-{sidx}"
                                                    )),
                                                    text::SUGGESTION_TRACK_BTN.get(),
                                                )
                                                .disabled(self.busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.create_bot_item(
                                                        bot_track.clone(),
                                                        &track_title,
                                                        Some(track_desc.clone()),
                                                        None,
                                                        cx,
                                                    );
                                                })),
                                            )
                                            .child(
                                                ui::ghost(
                                                    SharedString::from(format!(
                                                        "bots-suggestion-dismiss-{sidx}"
                                                    )),
                                                    text::SUGGESTION_DISMISS_BTN.get(),
                                                )
                                                .disabled(self.soul_memory.busy)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.dismiss_suggestion(
                                                        bot_dismiss.clone(),
                                                        sid_dismiss.clone(),
                                                        cx,
                                                    );
                                                })),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(why_line),
                            )
                            .child(
                                div()
                                    .text_size(ui::TEXT_XS)
                                    .text_color(p.foreground)
                                    .child(ask_line),
                            ),
                    );
            }
        }

        div()
            .id("bots-tab-soul-memory-body")
            .debug_selector(|| "bots-tab-soul-memory-body".into())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_5()
            .py_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(soul_card)
            .child(memory_card)
            .child(facts_card)
            .child(history_card)
            .child(topics_card)
            .child(tidy_card)
            .child(look_card)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::Modifiers;
    use krill_proto::bots::{BotConfig, BotMemoryTopicInfo, BotRpcRequest, BotRpcResponse};

    use super::*;
    use crate::bots::{DetailTab, settings_root::SettingsPage};

    fn make_bot(id: &str, name: &str) -> BotConfig {
        BotConfig::new(BotId::new(id).unwrap(), name, "claude", 1_700_000_000_000).unwrap()
    }

    #[gpui::test]
    fn soul_save_clears_declined_switch_and_guards_stale_bot_and_tidy_refreshes_topics(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let bot_a = make_bot("bot-a", "Alpha");
        let bot_b = make_bot("bot-b", "Beta");
        let id_a = bot_a.id.clone();
        let id_b = bot_b.id.clone();
        let saved_soul = Arc::new(Mutex::new(String::new()));
        let topics = Arc::new(Mutex::new(Vec::<BotMemoryTopicInfo>::new()));
        let saved_cap = saved_soul.clone();
        let topics_cap = topics.clone();
        let bots_list = vec![bot_a, bot_b];
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(bots_list.clone())),
            BotRpcRequest::GetSoul { .. } => Ok(BotRpcResponse::Text("Initial soul".to_owned())),
            BotRpcRequest::GetMemory { .. } => {
                Ok(BotRpcResponse::Text("# Memory\n\n- Fact 1\n".to_owned()))
            }
            BotRpcRequest::SetSoul { content, .. } => {
                saved_cap.lock().unwrap().clone_from(content);
                Ok(BotRpcResponse::Ok)
            }
            BotRpcRequest::RunMemoryTidy { bot_id } => {
                topics_cap.lock().unwrap().push(BotMemoryTopicInfo {
                    topic: "archive-2026-11".to_owned(),
                    bytes: 42,
                    updated_at_ms: 1_700_000_000_100,
                });
                Ok(BotRpcResponse::MemoryTidyResult {
                    bot_id: bot_id.clone(),
                    summary: "Archived 1".to_owned(),
                    changes: 1,
                })
            }
            BotRpcRequest::UndoMemoryTidy { .. } => {
                topics_cap.lock().unwrap().clear();
                Ok(BotRpcResponse::MemoryUpdated {
                    content: "# Memory\n\n- Fact 1\n".to_owned(),
                    restored_facts: Vec::new(),
                    removed_count: 0,
                })
            }
            BotRpcRequest::ListMemoryTopics { .. } => Ok(BotRpcResponse::MemoryTopics(
                topics_cap.lock().unwrap().clone(),
            )),
            BotRpcRequest::RunLookAround { bot_id } => Ok(BotRpcResponse::LookAroundResult {
                bot_id: bot_id.clone(),
                suggestions: Vec::new(),
            }),
            _ => Ok(BotRpcResponse::Ok),
        });

        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        vcx.simulate_resize(gpui::size(gpui::px(400.), gpui::px(900.)));
        vcx.run_until_parked();

        // Open Bot A Settings > Soul and simulate editing + leaving Soul page (UB-17).
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_in_panel(&id_a, DetailTab::Settings, cx);
                v.open_settings_page(SettingsPage::Soul, cx);
                v.panel_switch_dismissed_for = Some(id_b.clone());
            });
        });
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.settings.soul_editing = Some(true);
                let inp = v.soul_memory.soul_input.clone().unwrap();
                inp.update(cx, |s, cx| s.set_value("Edited soul", window, cx));
                v.close_settings_subpage(cx);
            });
        });
        vcx.run_until_parked();
        assert_eq!(*saved_soul.lock().unwrap(), "Edited soul");
        vcx.update(|_, cx| {
            let v = view.read(cx);
            assert_eq!(v.soul_memory.soul_loaded, "Edited soul");
            assert!(
                v.panel_switch_dismissed_for.is_none(),
                "successful Soul save must clear panel_switch_dismissed_for"
            );
        });

        // UB-20: Stale save_soul completion after switching selected_bot_id must not clobber Bot B.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                assert!(v.try_save_soul(id_a.clone(), "Stale A soul".to_owned(), cx));
                v.selected_bot_id = Some(id_b.clone());
                v.soul_memory.soul_loaded = "Bot B soul".to_owned();
                v.soul_memory.busy = false;
            });
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            assert_eq!(view.read(cx).soul_memory.soul_loaded, "Bot B soul");
        });

        // UB-49: run_memory_tidy and undo_memory_tidy refresh memory_topics.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| v.run_memory_tidy(id_b.clone(), cx));
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            assert_eq!(view.read(cx).soul_memory.memory_topics.len(), 1);
            assert_eq!(
                view.read(cx).soul_memory.memory_topics[0].topic,
                "archive-2026-11"
            );
        });
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| v.undo_memory_tidy(id_b.clone(), cx));
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            assert!(view.read(cx).soul_memory.memory_topics.is_empty());
        });

        // UB-29: look_around_running is tracked per bot_id.
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.run_look_around(id_a.clone(), cx);
                assert!(v.soul_memory.is_looking_around(&id_a));
                assert!(!v.soul_memory.is_looking_around(&id_b));
            });
        });
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            let v = view.read(cx);
            assert!(!v.soul_memory.is_looking_around(&id_a));
            assert!(v.look_tasks.is_empty());
        });

        // UB-17: Clicking Read while busy must NOT flip soul_editing to Some(false).
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.selected_bot_id = Some(id_a.clone());
                v.open_settings_page(SettingsPage::Soul, cx);
                v.settings.soul_editing = Some(true);
                v.soul_memory.soul_loaded = "Saved".to_owned();
                let inp = v.soul_memory.soul_input.clone().unwrap();
                inp.update(cx, |s, cx| s.set_value("Unsaved while busy", window, cx));
                v.soul_memory.busy = true;
                v.soul_memory.saving_soul_content = None;
                cx.notify();
            });
        });
        vcx.run_until_parked();
        let read_btn = vcx
            .debug_bounds("bot-soul-read")
            .expect("Read tab rendered");
        vcx.simulate_click(read_btn.center(), Modifiers::default());
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            assert_eq!(
                view.read(cx).settings.soul_editing,
                Some(true),
                "clicking Read while busy must not flip soul_editing to false"
            );
        });
    }
}
