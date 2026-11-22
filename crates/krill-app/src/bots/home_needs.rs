// Adapted from Orca (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Orca's own asks at the top of Home's Needs you group (`BotNeedsYouSection`, `BotNeedRow`):
//! a chat or worker waiting on the owner (an approval or a question), a connector that needs a
//! sign-in, and someone asking for access. Each row opens where it was asked.

use std::collections::BTreeMap;

use gpui::{Context, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, Sizable as _};
use gpui_kit_assets::IconName;
use krill_proto::SessionId;
use krill_proto::bots::{
    BotAccessRequestStatus, BotConfig, BotErrorCategory, ConnectorHealthState,
};
use krill_proto::control::AgentState;

use super::{DetailTab, OpenSession, View};
use crate::i18n::catalog::{bot_home as hm, bots as text};
use crate::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NeedKind {
    Approval,
    Question,
    SignIn,
    Access,
}

/// One of Orca's asks, as its row says it.
#[derive(Debug, Clone)]
pub(super) struct Need {
    pub key: String,
    pub kind: NeedKind,
    pub title: String,
    pub detail: String,
    pub action: String,
    pub open: NeedOpen,
}

#[derive(Debug, Clone)]
pub(super) enum NeedOpen {
    Session(SessionId),
    Connectors,
}

impl View {
    /// What the shell knows of each bot session's agent: Home lists the waiting ones.
    pub fn set_session_states(
        &mut self,
        states: BTreeMap<SessionId, AgentState>,
        cx: &mut Context<Self>,
    ) {
        if self.items.session_states != states {
            self.items.session_states = states;
            cx.notify();
        }
    }

    /// The bot's asks, approvals first (Orca `botNow`'s order).
    pub(super) fn home_needs(&self, bot: &BotConfig) -> Vec<Need> {
        let mut needs = Vec::new();
        let sessions: Vec<_> = self
            .bot_sessions
            .iter()
            .filter(|s| s.bot_id == bot.id && s.live)
            .collect();
        let home = sessions
            .iter()
            .find(|s| s.link_id.is_none())
            .map(|s| s.session_id);
        for s in &sessions {
            let state = self.items.session_states.get(&s.session_id).copied();
            let kind = match state {
                Some(AgentState::Blocked) => NeedKind::Approval,
                Some(AgentState::Waiting) => NeedKind::Question,
                _ => continue,
            };
            let title = if Some(s.session_id) == home {
                hm::NEED_CHAT.format(&[("name", &bot.name)])
            } else {
                let label = s.label.trim();
                if !label.is_empty() && label != bot.name.trim() {
                    label.to_owned()
                } else {
                    s.link_id
                        .clone()
                        .unwrap_or_else(|| text::BP_NEW_CHAT.get().to_owned())
                }
            };
            needs.push(Need {
                key: format!("session:{}", s.session_id.0),
                kind,
                title,
                detail: if kind == NeedKind::Approval {
                    hm::NEED_PERMISSION.get().to_owned()
                } else {
                    hm::NEED_WAITING.get().to_owned()
                },
                action: hm::NEED_OPEN.get().to_owned(),
                open: NeedOpen::Session(s.session_id),
            });
        }
        for c in self
            .connectors
            .bot_statuses
            .iter()
            .filter(|c| c.bot_id == bot.id)
        {
            let signed_out = c.error_category == Some(BotErrorCategory::SignedOut);
            if !c.enabled || !(c.state == ConnectorHealthState::Error || signed_out) {
                continue;
            }
            let name = Self::source_label(c.source).to_owned();
            needs.push(Need {
                key: format!("connector:{}", c.source.as_str()),
                kind: NeedKind::SignIn,
                title: name.clone(),
                detail: hm::NEED_CONNECTOR.format(&[("name", &name)]),
                action: hm::NEED_OPEN_NAMED.format(&[("name", &name)]),
                open: NeedOpen::Connectors,
            });
        }
        for r in self
            .connectors
            .bot_access_requests
            .iter()
            .filter(|r| r.bot_id == bot.id && r.status == BotAccessRequestStatus::Pending)
        {
            let person = if r.sender_name.trim().is_empty() {
                r.sender_id.clone()
            } else {
                r.sender_name.clone()
            };
            needs.push(Need {
                key: format!("access:{}", r.token),
                kind: NeedKind::Access,
                title: hm::NEED_ACCESS.format(&[("person", &person)]),
                detail: format!("{} · {}", Self::source_label(r.source), r.channel_id),
                action: hm::NEED_REVIEW.get().to_owned(),
                open: NeedOpen::Connectors,
            });
        }
        needs.sort_by_key(|n| n.kind != NeedKind::Approval);
        needs
    }

    /// Orca's `NeedRow`: its kind's glyph and title on one line, what it asks in the hover card,
    /// and its action named over the row's end on hover.
    pub(super) fn render_need_row(
        &self,
        bot: &BotConfig,
        need: &Need,
        p: &ui::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (icon, color) = match need.kind {
            NeedKind::Approval => (IconName::Shield, p.agent_question),
            NeedKind::Question => (IconName::CircleQuestionMark, p.agent_question),
            NeedKind::SignIn => (IconName::CircleAlert, p.destructive),
            NeedKind::Access => (IconName::UserPlus, p.agent_question),
        };
        let hover_key = format!("need:{}", need.key);
        let hovered = self.items.hover_uid.as_deref() == Some(hover_key.as_str());
        let sel = format!("bot-home-need-{}", need.key.replace(':', "-"));
        let debug = sel.clone();
        let (title, detail) = (need.title.clone(), need.detail.clone());
        let card = move |window: &mut Window, cx: &mut gpui::App| {
            let (title, detail) = (title.clone(), detail.clone());
            Tooltip::element(move |_, cx| {
                let p = ui::palette(cx);
                div()
                    .max_w(px(280.))
                    .text_size(ui::TEXT_XS)
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.popover_foreground)
                            .child(title.clone()),
                    )
                    .child(
                        div()
                            .mt_1()
                            .text_color(p.muted_foreground)
                            .child(detail.clone()),
                    )
            })
            .build(window, cx)
        };
        let open = need.open.clone();
        let bot_id = bot.id.clone();
        let go = move |_this: &mut Self, cx: &mut Context<Self>| match &open {
            NeedOpen::Session(session_id) => cx.emit(OpenSession {
                bot_id: bot_id.clone(),
                session_id: *session_id,
            }),
            NeedOpen::Connectors => cx.emit(super::settings_root::OpenSettingsPage(
                DetailTab::Connectors,
            )),
        };
        let go2 = go.clone();
        let mut row = div()
            .id(SharedString::from(sel))
            .debug_selector(move || debug.clone())
            .role(gpui::Role::ListItem)
            .aria_label(SharedString::from(
                hm::NEED_ACTION_FOR.format(&[("action", &need.action), ("title", &need.title)]),
            ))
            .tab_index(0)
            .relative()
            .min_w_0()
            .flex()
            .items_start()
            .gap_2()
            .rounded(ui::RADIUS_XS)
            .px_2()
            .py_1()
            .cursor_pointer()
            .hover(|d| d.bg(p.sidebar_accent))
            .tooltip(card)
            .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                let now_on = this.items.hover_uid.as_deref() == Some(hover_key.as_str());
                if *on != now_on {
                    this.items.hover_uid = on.then(|| hover_key.clone());
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| go(this, cx)))
            .child(
                div()
                    .h(px(20.))
                    .w(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(icon).size(px(12.)).text_color(color)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(ui::TEXT_MD)
                    .line_height(px(20.))
                    .text_color(p.foreground)
                    .child(need.title.clone()),
            );
        if hovered {
            row = row.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .flex()
                    .items_center()
                    .pl_1()
                    .pr(px(4.))
                    .rounded(ui::RADIUS_XS)
                    .bg(p.sidebar_accent)
                    .child(
                        ui::ghost("bot-home-need-action", need.action.clone())
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| go2(this, cx))),
                    ),
            );
        }
        row
    }
}
