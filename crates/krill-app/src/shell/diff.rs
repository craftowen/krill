//! MK-085: route review comments through the focused agent's existing input journal.

use gpui::{Context, Entity, Subscription};
use krill_app::diff::{Panel, SendComments, input_bytes};
use krill_term::ime::Output;

use super::Shell;

impl Shell {
    pub(super) fn subscribe_diff(panel: &Entity<Panel>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(panel, |this, panel, event: &SendComments, cx| {
            let source = event.1;
            let Some(live) = this
                .layout
                .focused()
                .and_then(|focused| this.live.get(&focused))
                .filter(|l| {
                    l.launch.is_some()
                        && l.pane.is_linked()
                        && !l.pane.is_cold()
                        && !l.pane.is_closed()
                })
            else {
                panel.update(cx, |p, cx| p.no_agent(source, cx));
                return;
            };
            let out = Output {
                pty: input_bytes(&event.0, live.pane.modes()),
                discard_ime: false,
            };
            let accepted = this.send(&out, None, cx);
            panel.update(cx, |p, cx| p.comments_sent(source, accepted, cx));
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn review_send_without_connected_agent_is_rejected(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (shell, cx) = cx.add_window_view(|_, cx| Shell::new(None, cx));
        cx.run_until_parked();
        assert!(cx.debug_bounds("comments-no-agent").is_none());
        cx.update(|_, cx| {
            let panel = shell.read(cx).diff.clone();
            panel.update(cx, |_, cx| cx.emit(SendComments::review("review".into())));
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("comments-no-agent").is_some());
    }
}
