//! A healthy daemon watch does not imply that every attached stream is responsive.

use super::*;

impl Shell {
    pub(super) fn start_pane_liveness(&mut self, cx: &mut Context<Self>) {
        if self.pane_liveness.is_none() {
            self.pane_liveness = Some(Self::poll_pane_liveness(cx));
        }
    }

    fn poll_pane_liveness(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if this
                    .update(cx, |this, cx| this.recover_stalled_panes(Now::get(), cx))
                    .is_err()
                {
                    return;
                }
            }
        })
    }

    fn recover_stalled_panes(&mut self, now: Now, cx: &mut Context<Self>) {
        let mut notices = Vec::new();
        let mut recovered = false;
        for live in self.live.values_mut() {
            if live.sock.is_none()
                || live.pane.account_switching()
                || live.pane.is_closed()
                || live.pane.is_cold()
                || live.pane.is_linked() && !live.pane.stalled(now)
            {
                continue;
            }
            // Only close this UI connection. The existing reader observes EOF and
            // reattaches the same session; the holder keeps owning its PTY.
            live.cut();
            notices.extend(live.pane.unlink());
            recovered = true;
        }
        if recovered {
            // Unacknowledged input is uncertain and must never be replayed.
            for notice in &notices {
                self.show(Some(notice), cx);
            }
            cx.background_spawn(async {
                tracing::warn!(
                    "terminal stream lost input transport or acknowledgement, reconnecting"
                );
            })
            .detach();
            cx.notify();
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
