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

    /// Shows `tab` for `bot_id`, the bot whose folder the focused chat runs in. Returns
    /// whether `bot_id` is now the selected bot (unsaved Soul edits can hold the switch back).
    pub fn show_in_panel(
        &mut self,
        bot_id: &BotId,
        tab: DetailTab,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.selected_bot_id.as_ref() != Some(bot_id) {
            self.connectors.page = None;
            self.connectors.adding = None;
            // Another bot opens Settings on its list, with its own field values.
            self.settings.reset_pages();
            self.select_bot(bot_id.clone(), cx);
            // A bot newer than this view's list: fetch the list again.
            if self.selected_bot_id.as_ref() == Some(bot_id)
                && !self.bots.iter().any(|b| &b.id == bot_id)
            {
                self.reload(cx);
            }
        }
        if self.detail_tab != tab {
            // Settings opens on Orca's list, not on the page it was last left at; a row that
            // asked for a page (Home's Upcoming) set it just before.
            if self.detail_tab == DetailTab::Settings {
                self.settings.reset_pages();
            }
            self.set_detail_tab(tab, cx);
            if tab == DetailTab::Items {
                // A fresh opening: rows and suggestions changed since get their dots again.
                self.items.seen_at_open.remove(bot_id);
                self.items.up_next_seen_at_open.remove(bot_id);
            }
        }
        if tab == DetailTab::Items {
            self.ensure_upcoming_poll(cx);
        }
        self.selected_bot_id.as_ref() == Some(bot_id)
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
            DetailTab::Items => self.render_home_tab(&bot, p, window, cx),
            DetailTab::SoulMemory => self
                .render_soul_memory_tab(&bot, p, window, cx)
                .into_any_element(),
            DetailTab::Connectors => self
                .render_connectors_tab(&bot, p, window, cx)
                .into_any_element(),
            DetailTab::ToolsSharing => self
                .render_tools_sharing_tab(&bot, p, window, cx)
                .into_any_element(),
            DetailTab::Settings => self
                .render_settings_tab(&bot, p, window, cx)
                .into_any_element(),
        };
        root.child(body)
    }
}
