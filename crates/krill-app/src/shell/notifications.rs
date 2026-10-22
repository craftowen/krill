//! Shell notices share the component toast stack. Only cards intercept the pointer;
//! the absolute overlay never changes terminal geometry or takes keyboard focus.

use super::*;
use gpui::{Anchor, Entity};
use gpui_component::notification::{
    Notification, NotificationDelivery, NotificationList, NotificationType,
};
use krill_app::onboarding::actions::{FixNeed, FixNeeds};
use krill_app::ui;

#[cfg(all(test, unix))]
mod tests;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum Kind {
    Link,
    Reconnected,
    Away,
    Notice,
    Stalled,
    Fix(String),
    Paste,
}

impl Kind {
    fn key(&self) -> SharedString {
        match self {
            Self::Link => "link-banner".into(),
            Self::Reconnected => "reconnected-banner".into(),
            Self::Away => "away-summary".into(),
            Self::Notice => "notice".into(),
            Self::Stalled => "stalled".into(),
            Self::Fix(id) => format!("fix-{id}").into(),
            Self::Paste => "multiline-paste-warning".into(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Value {
    Text(String),
    Fix(FixNeed),
    Paste(krill_app::security::PendingPaste),
}

#[derive(Default)]
pub(super) struct Notifications {
    list: Option<Entity<NotificationList>>,
    /// Remember dismissed/expired values until their source changes. Rendering
    /// an unchanged daemon snapshot must not restart the toast timeout.
    seen: HashMap<Kind, Value>,
    notes: HashMap<Kind, Entity<Notification>>,
}

impl Shell {
    pub(super) fn sync_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut current = Vec::new();
        for (kind, message) in [
            (Kind::Link, self.link.banner()),
            (Kind::Reconnected, self.reconnected.clone()),
            (Kind::Away, self.away.clone()),
            (Kind::Notice, self.notice.clone()),
            (Kind::Stalled, self.stalled_banner()),
        ] {
            if let Some(message) = message {
                current.push((kind, Value::Text(message)));
            }
        }
        if let Some(needs) = cx.try_global::<FixNeeds>() {
            current.extend(
                needs
                    .items
                    .iter()
                    .cloned()
                    .map(|need| (Kind::Fix(need.check_id.clone()), Value::Fix(need))),
            );
        }
        if let Some(pending) = self.security.pending_paste() {
            current.push((Kind::Paste, Value::Paste(pending.clone())));
        }
        if current.is_empty() && self.notifications.list.is_none() {
            return;
        }
        let list = self
            .notifications
            .list
            .get_or_insert_with(|| cx.new(|cx| NotificationList::new(window, cx)))
            .clone();
        let removed: Vec<_> = self
            .notifications
            .seen
            .keys()
            .filter(|kind| !current.iter().any(|(key, _)| key == *kind))
            .cloned()
            .collect();
        for kind in removed {
            self.notifications.seen.remove(&kind);
            if let Some(note) = self.notifications.notes.remove(&kind) {
                note.update(cx, |note, cx| note.dismiss(window, cx));
            }
        }
        for (kind, value) in current {
            if self.notifications.seen.get(&kind) == Some(&value) {
                continue;
            }
            self.notifications.seen.insert(kind.clone(), value.clone());
            let shell = cx.entity().downgrade();
            let closed_kind = kind.clone();
            let closed_value = value.clone();
            let close_shell = shell.clone();
            let note = match &value {
                Value::Text(message) => {
                    let message = krill_app::i18n::cached_notice(message);
                    let note = match kind {
                        Kind::Notice | Kind::Stalled => {
                            Notification::new().with_type(NotificationType::Warning)
                        }
                        Kind::Link if matches!(self.link, Link::Failed(_)) => {
                            Notification::new().with_type(NotificationType::Error)
                        }
                        _ => Notification::new().icon(
                            gpui_component::Icon::new(IconName::Info)
                                .text_color(ui::palette(cx).preserve),
                        ),
                    };
                    // State-dependent warnings and errors remain readable until
                    // resolved or dismissed. Summaries use the stack's 5s timer;
                    // the reconnected source clears after RECONNECTED_FOR.
                    let note = note.autohide(matches!(kind, Kind::Away | Kind::Reconnected));
                    let marker = kind.key();
                    let note = note.content(move |_, _, _| {
                        div()
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .debug_selector({
                                let marker = marker.clone();
                                move || marker.to_string()
                            })
                            .child(message.clone())
                            .into_any_element()
                    });
                    if matches!(kind, Kind::Away | Kind::Notice) {
                        let click_shell = close_shell.clone();
                        let click_kind = kind.clone();
                        let click_value = value.clone();
                        note.on_click(move |_, _, cx| {
                            click_shell
                                .update(cx, |this, cx| {
                                    match (&click_kind, &click_value) {
                                        (Kind::Away, Value::Text(message))
                                            if this.away.as_ref() == Some(message) =>
                                        {
                                            this.away = None;
                                        }
                                        (Kind::Notice, Value::Text(message))
                                            if this.notice.as_ref() == Some(message) =>
                                        {
                                            this.notice = None;
                                        }
                                        _ => {}
                                    }
                                    cx.notify();
                                })
                                .ok();
                        })
                    } else {
                        note
                    }
                }
                Value::Fix(need) => {
                    let need = need.clone();
                    Notification::new()
                        .autohide(false)
                        .content(move |_, _, cx| {
                            krill_app::onboarding::banner::render_need(need.clone(), cx)
                        })
                }
                Value::Paste(pending) => {
                    let pending = pending.clone();
                    Notification::new()
                        .autohide(false)
                        .content(move |_, _, cx| {
                            Self::render_security_notice(&pending, shell.clone(), cx)
                        })
                }
            }
            .id1::<Kind>(kind.key())
            .placement(Anchor::BottomRight)
            .delivery(NotificationDelivery::InApp)
            .font_family(ui::FONT_UI)
            .text_size(ui::TEXT_SM)
            .rounded(ui::RADIUS_LG)
            .shadow_lg()
            .on_close(move |_, cx| {
                close_shell
                    .update(cx, |this, cx| {
                        match (&closed_kind, &closed_value) {
                            (Kind::Notice, Value::Text(message))
                                if this.notice.as_ref() == Some(message) =>
                            {
                                this.notice = None;
                            }
                            (Kind::Away, Value::Text(message))
                                if this.away.as_ref() == Some(message) =>
                            {
                                this.away = None;
                            }
                            (Kind::Paste, Value::Paste(pending))
                                if this.security.pending_paste() == Some(pending) =>
                            {
                                this.cancel_pending_paste(cx);
                            }
                            (Kind::Fix(id), Value::Fix(need))
                                if cx
                                    .try_global::<FixNeeds>()
                                    .is_some_and(|needs| needs.items.contains(need)) =>
                            {
                                krill_app::onboarding::actions::dismiss_need(id, cx);
                            }
                            _ => {}
                        }
                        cx.notify();
                    })
                    .ok();
            });
            // Keep the newly mounted entity through the public list API so a
            // resolved source can dismiss that card without clearing other kinds.
            let mounted = list.update(cx, |list, cx| {
                let before: Vec<_> = list.notifications().iter().map(Entity::entity_id).collect();
                list.push(note, window, cx);
                list.notifications()
                    .into_iter()
                    .find(|note| !before.contains(&note.entity_id()))
            });
            if let Some(note) = mounted {
                self.notifications.notes.insert(kind, note);
            }
        }
    }

    pub(super) fn has_notifications(&self, cx: &App) -> bool {
        self.notifications
            .list
            .as_ref()
            .is_some_and(|list| !list.read(cx).notifications().is_empty())
    }

    pub(super) fn notification_overlay(&self) -> Option<AnyElement> {
        let list = self.notifications.list.as_ref()?;
        Some(
            div()
                .absolute()
                .inset_0()
                .bottom(ui::STATUS_BAR_HEIGHT)
                .debug_selector(|| "shell-toasts".into())
                .child(list.clone())
                .into_any_element(),
        )
    }
}
