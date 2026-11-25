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
        let mut seen_sources = std::collections::BTreeSet::new();
        for c in self
            .connectors
            .bot_statuses
            .iter()
            .chain(self.all_connector_statuses.iter())
            .filter(|c| c.bot_id == bot.id)
        {
            if !seen_sources.insert(c.source) {
                continue;
            }
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
        let mut seen_tokens = std::collections::BTreeSet::new();
        for r in self
            .connectors
            .bot_access_requests
            .iter()
            .chain(self.all_access_requests.iter())
            .filter(|r| r.bot_id == bot.id && r.status == BotAccessRequestStatus::Pending)
        {
            if !seen_tokens.insert(r.token.as_str()) {
                continue;
            }
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
        let hover_key = format!("need:{}:{}", bot.id.as_str(), need.key);
        let hovered = self.items.hover_uid.as_deref() == Some(hover_key.as_str());
        let need_slug = format!("{}-{}", bot.id.as_str(), need.key.replace(':', "-"));
        let sel = format!("bot-home-need-{need_slug}");
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
        let in_group = self.group_panel.active;
        let go = move |_this: &mut Self, cx: &mut Context<Self>| match &open {
            NeedOpen::Session(session_id) => cx.emit(OpenSession {
                bot_id: bot_id.clone(),
                session_id: *session_id,
            }),
            NeedOpen::Connectors if in_group => {
                cx.emit(super::OpenBotConnectors(bot_id.clone()));
            }
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
            let action_id = format!("bot-home-need-action-{need_slug}");
            let action_sel = action_id.clone();
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
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        ui::ghost(SharedString::from(action_id), need.action.clone())
                            .debug_selector(move || action_sel.clone())
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                go2(this, cx);
                            })),
                    ),
            );
        }
        row
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::{Modifiers, px};
    use krill_proto::bots::{
        BotConfig, BotConnectorStatus, BotErrorCategory, BotId, BotRpcRequest, BotRpcResponse,
        ConnectorHealthState, ExternalSource,
    };

    use super::*;

    #[gpui::test]
    fn need_row_action_button_stops_propagation_and_scopes_ids_by_bot(
        cx: &mut gpui::TestAppContext,
    ) {
        let now = super::super::now_unix_ms();
        let b1 = BotId::new("bot-alpha").unwrap();
        let b2 = BotId::new("bot-beta").unwrap();
        let cfg1 = BotConfig::new(b1.clone(), "Alpha", "agy", now).unwrap();
        let cfg2 = BotConfig::new(b2.clone(), "Beta", "agy", now).unwrap();
        let statuses = vec![
            BotConnectorStatus {
                bot_id: b1.clone(),
                source: ExternalSource::Slack,
                enabled: true,
                connected: false,
                state: ConnectorHealthState::Error,
                error_category: Some(BotErrorCategory::SignedOut),
                last_error: Some("signed out".to_owned()),
                last_sync_ms: None,
                account_label: None,
            },
            BotConnectorStatus {
                bot_id: b2.clone(),
                source: ExternalSource::Slack,
                enabled: true,
                connected: false,
                state: ConnectorHealthState::Error,
                error_category: Some(BotErrorCategory::SignedOut),
                last_error: Some("signed out".to_owned()),
                last_sync_ms: None,
                account_label: None,
            },
        ];
        let bots_rpc = vec![cfg1, cfg2];
        let rpc: super::super::BotRpcCall = Arc::new(move |req| match req {
            BotRpcRequest::ListBots => Ok(BotRpcResponse::Bots(bots_rpc.clone())),
            BotRpcRequest::ListItems { .. } => Ok(BotRpcResponse::Items(vec![])),
            BotRpcRequest::ListBotSessions { .. } => Ok(BotRpcResponse::BotSessions(vec![])),
            BotRpcRequest::ConnectorStatus { .. } => {
                Ok(BotRpcResponse::ConnectorStatuses(statuses.clone()))
            }
            BotRpcRequest::ListAccessRequests { .. } => Ok(BotRpcResponse::AccessRequests(vec![])),
            BotRpcRequest::GetBotUpcoming { .. } => Ok(BotRpcResponse::Upcoming(
                krill_proto::bots::BotUpcoming::default(),
            )),
            BotRpcRequest::GetSoul { .. } | BotRpcRequest::GetMemory { .. } => {
                Ok(BotRpcResponse::Text(String::new()))
            }
            BotRpcRequest::ListMemoryTopics { .. } => Ok(BotRpcResponse::MemoryTopics(vec![])),
            BotRpcRequest::ListSuggestions { .. } => Ok(BotRpcResponse::Suggestions(vec![])),
            _ => Ok(BotRpcResponse::Ok),
        });

        cx.update(|cx| {
            gpui_component::init(cx);
            crate::ui::apply(gpui_component::ThemeMode::Dark, cx);
        });
        let (view, vcx) = cx.add_window_view(|_, cx| View::with_rpc(rpc, None, cx).into_panel());
        let opens = Arc::new(Mutex::new(Vec::<BotId>::new()));
        let opens_sub = opens.clone();
        vcx.update(|_, cx| {
            cx.subscribe(&view, move |_, ev: &super::super::OpenBotConnectors, _| {
                opens_sub.lock().unwrap().push(ev.0.clone());
            })
            .detach();
        });
        vcx.simulate_resize(gpui::size(px(360.), px(600.)));
        vcx.run_until_parked();
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.show_group_in_panel("grp-needs", &[b1.clone(), b2.clone()], &[], cx);
            });
        });
        vcx.run_until_parked();

        // Both bots have Slack needs with distinct bot-prefixed selectors.
        assert!(
            vcx.debug_bounds("bot-home-need-bot-alpha-connector-slack")
                .is_some()
        );
        assert!(
            vcx.debug_bounds("bot-home-need-bot-beta-connector-slack")
                .is_some()
        );

        // Hover only bot-alpha's need row; only bot-alpha's action button appears,
        // and clicking it emits OpenBotConnectors exactly once (no double bubble).
        vcx.update(|_, cx| {
            view.update(cx, |v, cx| {
                v.items.hover_uid = Some("need:bot-alpha:connector:slack".to_owned());
                cx.notify();
            });
        });
        vcx.run_until_parked();
        let action_btn = vcx
            .debug_bounds("bot-home-need-action-bot-alpha-connector-slack")
            .expect("bot-alpha action button");
        assert!(
            vcx.debug_bounds("bot-home-need-action-bot-beta-connector-slack")
                .is_none()
        );
        vcx.simulate_click(action_btn.center(), Modifiers::default());
        vcx.run_until_parked();
        assert_eq!(*opens.lock().unwrap(), vec![b1]);
    }
}
