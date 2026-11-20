// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Tab 2 — Soul & Memory: SOUL.md and MEMORY.md editors with silent-deletion guard,
//! parsed memory fact list with explicit Forget, Memory Tidy-up, and Look Around suggestions.

use gpui::{Context, Entity, FontWeight, SharedString, Window, div, prelude::*};
use gpui_component::input::{Textarea, TextareaState};
use gpui_component::{Disableable as _, Sizable as _};
use krill_proto::bots::{
    BOT_MEMORY_INDEX_LINE_CAP, BotConfig, BotId, BotRpcRequest, BotRpcResponse, BotSuggestion,
    MemoryDeletionViolation, count_memory_lines, parse_memory_facts, validate_memory_edit,
};

use crate::i18n::catalog::bots as text;
use crate::ui;

use super::View;

#[derive(Default)]
pub(super) struct SoulMemoryState {
    pub(super) soul_loaded: String,
    pub(super) memory_loaded: String,
    pub(super) soul_input: Option<Entity<TextareaState>>,
    pub(super) memory_input: Option<Entity<TextareaState>>,
    pub(super) needs_sync_inputs: bool,
    pub(super) deletion_violation: Option<MemoryDeletionViolation>,
    pub(super) armed_forget: Option<String>,
    pub(super) tidy_running: bool,
    pub(super) last_tidy_summary: Option<SharedString>,
    pub(super) look_around_running: bool,
    pub(super) suggestions: Vec<BotSuggestion>,
}

impl SoulMemoryState {
    pub(super) fn set_loaded(&mut self, soul: String, memory: String) {
        self.soul_loaded = soul;
        self.memory_loaded = memory;
        self.needs_sync_inputs = true;
        self.deletion_violation = None;
        self.armed_forget = None;
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
    }
}

impl View {
    fn ensure_soul_memory_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.has_global::<gpui_component::Theme>() {
            return;
        }
        if self.soul_memory.soul_input.is_none() {
            let initial = self.soul_memory.soul_loaded.clone();
            self.soul_memory.soul_input = Some(cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(5, 14)
                    .placeholder(text::SOUL_PLACEHOLDER.get())
                    .default_value(initial)
            }));
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
        if self.soul_memory.needs_sync_inputs {
            let soul = self.soul_memory.soul_loaded.clone();
            let memory = self.soul_memory.memory_loaded.clone();
            if let Some(i) = &self.soul_memory.soul_input {
                i.update(cx, |s, cx| s.set_value(soul, window, cx));
            }
            if let Some(i) = &self.soul_memory.memory_input {
                i.update(cx, |s, cx| s.set_value(memory, window, cx));
            }
            self.soul_memory.needs_sync_inputs = false;
        }
    }

    pub fn save_soul(&mut self, bot_id: BotId, content: String, cx: &mut Context<Self>) {
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let saved_copy = content.clone();
            let res = cx
                .background_executor()
                .spawn(async move { rpc(&BotRpcRequest::SetSoul { bot_id, content }) })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::Ok | BotRpcResponse::Text(_)) => {
                        this.soul_memory.soul_loaded = saved_copy;
                        this.notice = Some(SharedString::from(text::SOUL_SAVED.get()));
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn save_memory(&mut self, bot_id: BotId, content: String, cx: &mut Context<Self>) {
        // Enforce silent-deletion guard before sending: surface MemoryDeletionViolation clearly.
        if let Err(violation) = validate_memory_edit(&self.soul_memory.memory_loaded, &content) {
            self.soul_memory.deletion_violation = Some(violation);
            self.notice = None;
            cx.notify();
            return;
        }
        self.soul_memory.deletion_violation = None;
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let sent = content.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    rpc(&BotRpcRequest::UpdateMemory {
                        bot_id,
                        content,
                        allow_deletions: false,
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(BotRpcResponse::MemoryUpdated {
                        content: next_content,
                        restored_facts,
                        ..
                    }) => {
                        this.soul_memory.memory_loaded = next_content;
                        this.soul_memory.needs_sync_inputs = true;
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
                    Ok(BotRpcResponse::Ok) => {
                        this.soul_memory.memory_loaded = sent;
                        this.notice = Some(SharedString::from(text::MEMORY_SAVED.get()));
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn forget_memory_fact(&mut self, bot_id: BotId, fact: String, cx: &mut Context<Self>) {
        self.soul_memory.armed_forget = None;
        self.soul_memory.deletion_violation = None;
        let rpc = self.rpc.clone();
        self.busy = true;
        self.error = None;
        self.notice = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let resp = rpc(&BotRpcRequest::ForgetMemoryFact {
                        bot_id: bot_id_bg.clone(),
                        fact,
                    })?;
                    match resp {
                        BotRpcResponse::MemoryUpdated { content, .. } => Ok(content),
                        BotRpcResponse::Ok => {
                            match rpc(&BotRpcRequest::GetMemory { bot_id: bot_id_bg }) {
                                Ok(BotRpcResponse::Text(t)) => Ok(t),
                                Ok(BotRpcResponse::Error(msg)) => Err(msg),
                                Err(e) => Err(e),
                                Ok(_) => Ok(String::new()),
                            }
                        }
                        BotRpcResponse::Error(msg) => Err(msg),
                        other => Err(format!("unexpected ForgetMemoryFact response: {other:?}")),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(next_memory) => {
                        this.soul_memory.memory_loaded = next_memory;
                        this.soul_memory.needs_sync_inputs = true;
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

    pub fn run_memory_tidy(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.soul_memory.tidy_running {
            return;
        }
        let rpc = self.rpc.clone();
        self.soul_memory.tidy_running = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let bot_id_bg = bot_id.clone();
            let res = cx
                .background_executor()
                .spawn(async move {
                    let tidy = rpc(&BotRpcRequest::RunMemoryTidy {
                        bot_id: bot_id_bg.clone(),
                    })?;
                    let mem = match rpc(&BotRpcRequest::GetMemory { bot_id: bot_id_bg }) {
                        Ok(BotRpcResponse::Text(t)) => Some(t),
                        _ => None,
                    };
                    Ok::<_, String>((tidy, mem))
                })
                .await;
            this.update(cx, |this, cx| {
                this.soul_memory.tidy_running = false;
                match res {
                    Ok((
                        BotRpcResponse::MemoryTidyResult {
                            summary, changes, ..
                        },
                        mem,
                    )) => {
                        if let Some(m) = mem {
                            this.soul_memory.memory_loaded = m;
                            this.soul_memory.needs_sync_inputs = true;
                        }
                        let msg = text::TIDY_RESULT
                            .format(&[("changes", &changes.to_string()), ("summary", &summary)]);
                        this.soul_memory.last_tidy_summary = Some(SharedString::from(msg.clone()));
                        this.notice = Some(SharedString::from(msg));
                    }
                    Ok((BotRpcResponse::Error(msg), _)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn run_look_around(&mut self, bot_id: BotId, cx: &mut Context<Self>) {
        if self.soul_memory.look_around_running {
            return;
        }
        let rpc = self.rpc.clone();
        self.soul_memory.look_around_running = true;
        self.error = None;
        self.action_task = Some(cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { rpc(&BotRpcRequest::RunLookAround { bot_id }) })
                .await;
            this.update(cx, |this, cx| {
                this.soul_memory.look_around_running = false;
                match res {
                    Ok(BotRpcResponse::LookAroundResult { suggestions, .. }) => {
                        this.soul_memory.suggestions = suggestions;
                    }
                    Ok(BotRpcResponse::Error(msg)) => {
                        this.error = Some(SharedString::from(msg));
                    }
                    Err(err) => {
                        this.error = Some(SharedString::from(err));
                    }
                    Ok(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "SOUL.md + MEMORY.md editors, fact list, tidy-up, and look-around"
    )]
    pub(super) fn render_soul_memory_tab(
        &mut self,
        bot: &BotConfig,
        p: &ui::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.ensure_soul_memory_inputs(window, cx);
        let bot_id_soul = bot.id.clone();
        let bot_id_mem = bot.id.clone();
        let bot_id_tidy = bot.id.clone();
        let bot_id_look = bot.id.clone();

        let facts = parse_memory_facts(&self.soul_memory.memory_loaded);
        let line_count = count_memory_lines(&self.soul_memory.memory_loaded);
        let line_count_badge = text::MEMORY_LINE_COUNT.format(&[
            ("lines", &line_count.to_string()),
            ("cap", &BOT_MEMORY_INDEX_LINE_CAP.to_string()),
        ]);

        // SOUL.md editor card.
        let soul_card = ui::card(p)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(ui::label(
                        p,
                        text::SOUL_SECTION.get(),
                        Some(text::SOUL_DESC.get()),
                    ))
                    .child(
                        ui::primary("bots-save-soul", text::SAVE_SOUL_BTN.get())
                            .debug_selector(|| "bots-save-soul".into())
                            .disabled(self.busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let content = this.soul_memory.soul_input.as_ref().map_or_else(
                                    || this.soul_memory.soul_loaded.clone(),
                                    |i| i.read(cx).value().to_string(),
                                );
                                this.save_soul(bot_id_soul.clone(), content, cx);
                            })),
                    ),
            )
            .children(
                self.soul_memory
                    .soul_input
                    .as_ref()
                    .map(|i| Textarea::new(i).small()),
            );

        // Memory deletion violation banner if triggered.
        let violation_banner = self.soul_memory.deletion_violation.as_ref().map(|v| {
            let joined = v.deleted_facts.join("; ");
            let msg = text::MEMORY_DELETION_VIOLATION.format(&[
                ("n", &v.deleted_facts.len().to_string()),
                ("facts", &joined),
            ]);
            ui::error_banner(p, msg).debug_selector(|| "bots-memory-deletion-violation".into())
        });

        // MEMORY.md editor card.
        let memory_card = ui::card(p)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(ui::label(
                        p,
                        text::MEMORY_SECTION.get(),
                        Some(text::MEMORY_DESC.get()),
                    ))
                    .child(
                        ui::controls()
                            .child(ui::badge(
                                p,
                                if line_count > BOT_MEMORY_INDEX_LINE_CAP {
                                    ui::BadgeTone::Warning
                                } else {
                                    ui::BadgeTone::Muted
                                },
                                line_count_badge,
                            ))
                            .child(
                                ui::primary("bots-save-memory", text::SAVE_MEMORY_BTN.get())
                                    .debug_selector(|| "bots-save-memory".into())
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let content =
                                            this.soul_memory.memory_input.as_ref().map_or_else(
                                                || this.soul_memory.memory_loaded.clone(),
                                                |i| i.read(cx).value().to_string(),
                                            );
                                        this.save_memory(bot_id_mem.clone(), content, cx);
                                    })),
                            ),
                    ),
            )
            .children(violation_banner)
            .children(
                self.soul_memory
                    .memory_input
                    .as_ref()
                    .map(|i| Textarea::new(i).small()),
            );

        // Parsed memory facts with explicit Forget buttons.
        let facts_heading = text::MEMORY_FACTS_HEADING.format(&[("n", &facts.len().to_string())]);
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
                let armed = self.soul_memory.armed_forget.as_deref() == Some(fact_text.as_str());
                let bot_forget = bot.id.clone();
                let btn_id = format!("bots-forget-fact-{idx}");
                let btn_sel = btn_id.clone();
                facts_card = facts_card.child(
                    div()
                        .w_full()
                        .min_w_0()
                        .px_2p5()
                        .py_1p5()
                        .rounded(ui::RADIUS_SM)
                        .bg(p.muted.opacity(0.35))
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
                            ui::danger_confirm(
                                SharedString::from(btn_id),
                                text::FORGET_BTN.get(),
                                text::FORGET_CONFIRM.get(),
                                armed,
                            )
                            .debug_selector(move || btn_sel.clone())
                            .disabled(self.busy)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if this.soul_memory.armed_forget.as_deref()
                                        == Some(fact_text.as_str())
                                    {
                                        this.forget_memory_fact(
                                            bot_forget.clone(),
                                            fact_text.clone(),
                                            cx,
                                        );
                                    } else {
                                        this.soul_memory.armed_forget = Some(fact_text.clone());
                                        cx.notify();
                                    }
                                },
                            )),
                        ),
                );
            }
        }

        // Memory Tidy-up row.
        let tidy_card = ui::row(p)
            .child(ui::label(
                p,
                text::TIDY_SECTION.get(),
                Some(text::TIDY_DESC.get()),
            ))
            .child(
                ui::controls().child(
                    ui::secondary(
                        "bots-run-memory-tidy",
                        if self.soul_memory.tidy_running {
                            text::TIDY_RUNNING.get()
                        } else {
                            text::RUN_TIDY_BTN.get()
                        },
                    )
                    .debug_selector(|| "bots-run-memory-tidy".into())
                    .disabled(self.soul_memory.tidy_running || self.busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run_memory_tidy(bot_id_tidy.clone(), cx);
                    })),
                ),
            );

        // Look Around & Suggestions card.
        let suggestions_heading = text::SUGGESTIONS_HEADING
            .format(&[("n", &self.soul_memory.suggestions.len().to_string())]);
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
                    .child(ui::label(
                        p,
                        text::LOOK_AROUND_SECTION.get(),
                        Some(text::LOOK_AROUND_DESC.get()),
                    ))
                    .child(
                        ui::secondary(
                            "bots-run-look-around",
                            if self.soul_memory.look_around_running {
                                text::LOOK_AROUND_RUNNING.get()
                            } else {
                                text::RUN_LOOK_AROUND_BTN.get()
                            },
                        )
                        .debug_selector(|| "bots-run-look-around".into())
                        .disabled(self.soul_memory.look_around_running || self.busy)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.run_look_around(bot_id_look.clone(), cx);
                        })),
                    ),
            )
            .child(
                div()
                    .text_size(ui::TEXT_XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.muted_foreground)
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
            for (sidx, s) in self.soul_memory.suggestions.iter().cloned().enumerate() {
                let why_line = text::SUGGESTION_WHY.format(&[("text", &s.why)]);
                let ask_line = text::SUGGESTION_ASK.format(&[("text", &s.ask)]);
                let bot_track = bot.id.clone();
                let track_title = s.title.clone();
                let track_desc = format!("{}\n{}", s.why, s.ask);
                let link_url = s
                    .link
                    .clone()
                    .filter(|u| u.starts_with("https://") || u.starts_with("http://"));
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
                                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                                                )
                                            })
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
            .child(tidy_card)
            .child(look_card)
    }
}
