//! Pane strip for a running session left on an account its host has since replaced
//! (Orca's Codex restart notice, for every provider and host).

use super::{Context, PaneId, Shell, sidebar};
use gpui::prelude::FluentBuilder as _;
use gpui::{AnyElement, App, InteractiveElement as _, IntoElement as _, MouseButton};
use gpui::{ParentElement as _, Styled as _, div};
use gpui_component::Sizable as _;
use krill_app::hosts::HostId;
use krill_app::i18n::catalog::shell as text;

struct Stale {
    switch: sidebar::SwitchSessionAccount,
    current: String,
    next: String,
}

fn non_empty(value: &str) -> Option<&str> {
    Some(value.trim()).filter(|v| !v.is_empty())
}

impl Shell {
    fn stale_account(&self, id: PaneId, cx: &App) -> Option<Stale> {
        let live = self.live.get(&id)?;
        if live.pane.is_closed() || live.pane.account_switching() {
            return None;
        }
        let host = self.layout.host(id);
        let session_id = self.layout.session(id).or_else(|| live.pane.session())?;
        let sidebar = self.sidebar.read(cx);
        let session = sidebar
            .session_on(host, session_id)
            .filter(|s| !s.agent_ended())?;
        let current = session
            .account
            .as_ref()
            .filter(|a| !a.account_id.is_empty())?;
        // Choices already gate connection, capability bits, pending handoffs,
        // provider, launchability, and NeedsLogin/RateLimited readiness.
        let target = sidebar
            .session_account_choices(host, session)
            .into_iter()
            .find(|a| a.active && a.id != current.account_id)?;
        if self
            .dismissed_stale_account_banners
            .get(&(host.clone(), session_id))
            == Some(&target.id)
        {
            return None;
        }
        let next = target
            .email
            .as_deref()
            .and_then(non_empty)
            .or_else(|| non_empty(&target.label))
            .unwrap_or(&target.id)
            .to_owned();
        Some(Stale {
            current: non_empty(&current.label)
                .unwrap_or(&current.account_id)
                .to_owned(),
            next,
            switch: sidebar::SwitchSessionAccount {
                host: host.clone(),
                session: session_id,
                provider: target.provider,
                account_id: target.id,
            },
        })
    }

    pub(super) fn render_stale_account_banner(
        &self,
        id: PaneId,
        rounded_top: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let Stale {
            switch,
            current,
            next,
        } = self.stale_account(id, cx)?;
        let p = krill_app::ui::palette(cx);
        let pane = id.get();
        let message = text::STALE_ACCOUNT_BANNER.format(&[("prev", &current), ("next", &next)]);
        let key = (switch.host.clone(), switch.session);
        let dismissed = switch.account_id.clone();
        Some(
            div()
                .id(("stale-account-banner", pane))
                .debug_selector(move || format!("stale-account-banner-{pane}"))
                .w_full()
                .flex_none()
                .min_w_0()
                .px_3()
                .py_1p5()
                .when(rounded_top, |d| {
                    d.rounded_t(krill_app::ui::PANE_CARD_INNER_RADIUS)
                })
                .bg(p.warning.opacity(0.10))
                .border_b_1()
                .border_color(p.border)
                .flex()
                .items_center()
                .gap_2()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(krill_app::ui::TEXT_XS)
                        .text_color(p.foreground)
                        .child(message),
                )
                .child(
                    krill_app::ui::primary(
                        ("stale-account-switch", pane),
                        text::STALE_ACCOUNT_SWITCH_BTN.get(),
                    )
                    .xsmall()
                    .debug_selector(move || format!("stale-account-switch-{pane}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let switch = switch.clone();
                        this.sidebar
                            .update(cx, |s, cx| s.switch_session_account(switch, cx));
                    })),
                )
                .child(
                    krill_app::ui::ghost(
                        ("stale-account-dismiss", pane),
                        text::STALE_ACCOUNT_KEEP_BTN.get(),
                    )
                    .xsmall()
                    .debug_selector(move || format!("stale-account-dismiss-{pane}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.dismissed_stale_account_banners
                            .insert(key.clone(), dismissed.clone());
                        cx.notify();
                    })),
                )
                .into_any_element(),
        )
    }

    /// Dismissals live only as long as their session does on that host.
    pub(super) fn prune_stale_account_dismissals(&mut self, host: &HostId, cx: &App) {
        let sidebar = self.sidebar.read(cx);
        self.dismissed_stale_account_banners
            .retain(|(h, session), _| {
                h != host
                    || sidebar
                        .session_on(h, *session)
                        .is_some_and(|s| !s.agent_ended())
            });
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
