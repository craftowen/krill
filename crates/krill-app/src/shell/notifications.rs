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
    Notice,
    SidebarNotice,
    SidebarPreferenceError,
    SidebarPollError,
    SidebarPrGuidance,
    Stalled,
    AccountInput,
    Fix(String),
    Paste,
}

impl Kind {
    fn key(&self) -> SharedString {
        match self {
            Self::Link => "link-banner".into(),
            Self::Notice => "notice".into(),
            Self::SidebarNotice => "sidebar-notice".into(),
            Self::SidebarPreferenceError => "sidebar-preference-error".into(),
            Self::SidebarPollError => "sidebar-poll-error".into(),
            Self::SidebarPrGuidance => "sidebar-pr-guidance".into(),
            Self::Stalled => "stalled".into(),
            Self::AccountInput => "account-input-recovery".into(),
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
    AccountInput {
        session: krill_proto::SessionId,
        cancelling: bool,
    },
}

#[derive(Default)]
pub(super) struct Notifications {
    list: Option<Entity<NotificationList>>,
    /// Remember dismissed/expired values until their source changes. Rendering
    /// an unchanged daemon snapshot must not restart the toast timeout.
    seen: HashMap<Kind, Value>,
    notes: HashMap<Kind, Entity<Notification>>,
    /// Per-session failed handoffs whose recovery card was explicitly closed.
    dismissed_account_input: HashMap<krill_proto::SessionId, (String, Option<String>)>,
}

impl Shell {
    pub(super) fn sync_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.notifications
            .dismissed_account_input
            .retain(|session, (account_id, error)| {
                self.accounts_snapshot.details.handoffs.iter().any(|h| {
                    h.from == *session
                        && h.to.is_none()
                        && h.error.is_some()
                        && &h.account_id == account_id
                        && &h.error == error
                })
            });
        let mut current = Vec::new();
        let (sb_notice, sb_pref_err, sb_poll_err, sb_pr_guidance) = {
            let sb = self.sidebar.read(cx);
            (
                sb.notice().map(str::to_owned),
                sb.preference_error().map(str::to_owned),
                sb.poll_error_notice().map(str::to_owned),
                sb.pr_guidance().map(str::to_owned),
            )
        };
        for (kind, message) in [
            (
                Kind::Link,
                matches!(self.link, Link::Failed(_))
                    .then(|| self.link.banner())
                    .flatten(),
            ),
            (Kind::Notice, self.notice.clone()),
            (Kind::SidebarNotice, sb_notice),
            (Kind::SidebarPreferenceError, sb_pref_err),
            (Kind::SidebarPollError, sb_poll_err),
            (Kind::SidebarPrGuidance, sb_pr_guidance),
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
        if let Some(session) = self.failed_account_input() {
            let dismissed = self
                .notifications
                .dismissed_account_input
                .get(&session)
                .is_some_and(|(account_id, error)| {
                    self.accounts_snapshot.details.handoffs.iter().any(|h| {
                        h.from == session
                            && h.to.is_none()
                            && &h.account_id == account_id
                            && &h.error == error
                    })
                });
            if !dismissed {
                current.push((
                    Kind::AccountInput,
                    Value::AccountInput {
                        session,
                        cancelling: self.account_cancel.is_some(),
                    },
                ));
            }
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
                        Kind::Notice
                        | Kind::SidebarNotice
                        | Kind::SidebarPollError
                        | Kind::Stalled => {
                            Notification::new().with_type(NotificationType::Warning)
                        }
                        Kind::SidebarPreferenceError => {
                            Notification::new().with_type(NotificationType::Error)
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
                    // resolved or dismissed. One-shot notices use the stack's 5s timer.
                    let note = note.autohide(matches!(
                        kind,
                        Kind::Notice
                            | Kind::SidebarNotice
                            | Kind::SidebarPrGuidance
                    ));
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
                    if matches!(
                        kind,
                        Kind::Notice
                            | Kind::SidebarNotice
                            | Kind::SidebarPreferenceError
                            | Kind::SidebarPollError
                            | Kind::SidebarPrGuidance
                    ) {
                        let click_shell = close_shell.clone();
                        let click_value = value.clone();
                        let click_kind = kind.clone();
                        note.on_click(move |_, _, cx| {
                            click_shell
                                .update(cx, |this, cx| {
                                    match (&click_kind, &click_value) {
                                        (Kind::Notice, Value::Text(message))
                                            if this.notice.as_ref() == Some(message) =>
                                        {
                                            this.notice = None;
                                        }
                                        (Kind::SidebarNotice, Value::Text(message)) => {
                                            this.sidebar.update(cx, |sb, cx| {
                                                if sb.notice() == Some(message.as_str()) {
                                                    sb.clear_notice(cx);
                                                }
                                            });
                                        }
                                        (Kind::SidebarPreferenceError, Value::Text(message)) => {
                                            this.sidebar.update(cx, |sb, cx| {
                                                if sb.preference_error() == Some(message.as_str()) {
                                                    sb.clear_preference_error(cx);
                                                }
                                            });
                                        }
                                        (Kind::SidebarPollError, Value::Text(message)) => {
                                            this.sidebar.update(cx, |sb, cx| {
                                                if sb.poll_error() == Some(message.as_str()) {
                                                    sb.clear_poll_error(cx);
                                                }
                                            });
                                        }
                                        (Kind::SidebarPrGuidance, Value::Text(message)) => {
                                            this.sidebar.update(cx, |sb, cx| {
                                                if sb.pr_guidance() == Some(message.as_str()) {
                                                    sb.dismiss_pr_guidance(cx);
                                                }
                                            });
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
                        .autohide(need.check_id.starts_with("hook."))
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
                Value::AccountInput {
                    session,
                    cancelling,
                } => {
                    let session = *session;
                    let cancelling = *cancelling;
                    Notification::new()
                        .with_type(NotificationType::Warning)
                        .autohide(false)
                        .content(move |_, _, _| {
                            let shell = shell.clone();
                            div()
                                .debug_selector(|| "account-input-recovery".into())
                                .flex()
                                .flex_col()
                                .gap_2()
                                .w_full()
                                .min_w_0()
                                .whitespace_normal()
                                .child(krill_app::i18n::catalog::settings::ACCOUNT_INPUT_HELD.get())
                                .child(
                                    ui::secondary(
                                        "account-continue-original",
                                        krill_app::i18n::catalog::settings::ACCOUNT_CONTINUE_ORIGINAL.get(),
                                    )
                                    .debug_selector(|| "account-continue-original".into())
                                    .disabled(cancelling)
                                    .on_click(move |_, _, cx| {
                                        shell
                                            .update(cx, |this, cx| {
                                                this.continue_original_account(session, cx);
                                            })
                                            .ok();
                                    }),
                                )
                                .into_any_element()
                        })
                }
            }
            .id1::<Kind>(kind.key())
            .placement(Anchor::TopRight)
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
                            (Kind::SidebarNotice, Value::Text(message)) => {
                                this.sidebar.update(cx, |sb, cx| {
                                    if sb.notice() == Some(message.as_str()) {
                                        sb.clear_notice(cx);
                                    }
                                });
                            }
                            (Kind::SidebarPreferenceError, Value::Text(message)) => {
                                this.sidebar.update(cx, |sb, cx| {
                                    if sb.preference_error() == Some(message.as_str()) {
                                        sb.clear_preference_error(cx);
                                    }
                                });
                            }
                            (Kind::SidebarPollError, Value::Text(message)) => {
                                this.sidebar.update(cx, |sb, cx| {
                                    if sb.poll_error() == Some(message.as_str()) {
                                        sb.clear_poll_error(cx);
                                    }
                                });
                            }
                            (Kind::SidebarPrGuidance, Value::Text(message)) => {
                                this.sidebar.update(cx, |sb, cx| {
                                    if sb.pr_guidance() == Some(message.as_str()) {
                                        sb.dismiss_pr_guidance(cx);
                                    }
                                });
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
                            (Kind::AccountInput, Value::AccountInput { session, .. })
                                if this.notifications.seen.get(&Kind::AccountInput)
                                    == Some(&closed_value)
                                    && this.failed_account_input() == Some(*session) =>
                            {
                                if let Some(handoff) = this
                                    .accounts_snapshot
                                    .details
                                    .handoffs
                                    .iter()
                                    .find(|h| h.from == *session && h.to.is_none() && h.error.is_some())
                                {
                                    this.notifications.dismissed_account_input.insert(
                                        *session,
                                        (handoff.account_id.clone(), handoff.error.clone()),
                                    );
                                }
                                this.notifications.seen.remove(&Kind::AccountInput);
                                this.notifications.notes.remove(&Kind::AccountInput);
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
