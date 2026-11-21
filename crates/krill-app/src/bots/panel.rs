// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! A bot page's right-sidebar body: Orca's Home items, Connectors and Bot settings, drawn from
//! the same tabs as the Bots modal but without its rail, header and modal frame. The host
//! draws the tab bar, the bot header and the Chats and Files tabs around it.

use gpui::{Context, Window, div, prelude::*};
use krill_proto::bots::BotId;

use super::{DetailTab, Mode, View};
use crate::ui;

impl View {
    /// Turns this view into a bot page's right-sidebar body.
    #[must_use]
    pub fn into_panel(mut self) -> Self {
        self.mode = Mode::Panel;
        self
    }

    /// Shows `tab` for `bot_id`, the bot whose folder the focused chat runs in.
    pub fn show_in_panel(&mut self, bot_id: &BotId, tab: DetailTab, cx: &mut Context<Self>) {
        if self.selected_bot_id.as_ref() != Some(bot_id) {
            self.select_bot(bot_id.clone(), cx);
        }
        if self.detail_tab != tab {
            self.set_detail_tab(tab, cx);
        }
    }

    pub(super) fn render_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = ui::palette(cx);
        let root = div()
            .id("bot-panel-body")
            .debug_selector(|| "bot-panel-body".into())
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_SM)
            .text_color(p.foreground)
            .children(self.render_pending_switch(p, cx))
            .children(
                self.error
                    .clone()
                    .map(|err| self.render_error_with_retry(err, p, cx)),
            )
            .children(self.render_notice(p));
        // The bot list is still loading (or the bot just went away): nothing to show yet.
        let Some(bot) = self.selected_bot().cloned() else {
            return root;
        };
        let body = match self.detail_tab {
            DetailTab::Items => self.render_items_tab(&bot, p, window, cx),
            DetailTab::SoulMemory => self.render_soul_memory_tab(&bot, p, window, cx),
            DetailTab::Connectors => self.render_connectors_tab(&bot, p, window, cx),
            DetailTab::ToolsSharing => self.render_tools_sharing_tab(&bot, p, window, cx),
            DetailTab::Settings => self.render_settings_tab(&bot, p, window, cx),
        };
        root.child(body)
    }
}
