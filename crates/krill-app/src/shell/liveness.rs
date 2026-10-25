//! A healthy daemon watch does not imply that every attached stream is responsive.

use super::*;
use krill_app::hosts::HostId;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeCause {
    InputAckTimeout,
    TransportLost,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ProbeHealth {
    #[default]
    Healthy,
    Suspect,
    Stalled,
}

/// All clocks saturate: wall time can go backwards or include time asleep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProbeDelay {
    awake: Duration,
    asleep: Duration,
    beyond_rtt: Option<Duration>,
}

fn probe_delay(previous: Now, now: Now, rtt: Option<Duration>) -> ProbeDelay {
    let awake = now.awake.saturating_duration_since(previous.awake);
    let wall = now.wall.duration_since(previous.wall).unwrap_or_default();
    ProbeDelay {
        awake,
        asleep: wall.saturating_sub(awake),
        beyond_rtt: rtt.map(|rtt| awake.saturating_sub(rtt)),
    }
}

#[derive(Default)]
pub(super) struct LivenessMonitor {
    probes: BTreeMap<PaneId, ProbeHealth>,
    notified: BTreeSet<HostId>,
    previous: Option<Now>,
}

impl LivenessMonitor {
    fn observe(&mut self, pane: PaneId, cause: Option<ProbeCause>) -> ProbeHealth {
        let state = self.probes.entry(pane).or_default();
        *state = match (cause, *state) {
            (None, _) => ProbeHealth::Healthy,
            // A missing writer is definite transport loss, not a missed probe.
            (Some(ProbeCause::InputAckTimeout), ProbeHealth::Healthy) => ProbeHealth::Suspect,
            (Some(ProbeCause::TransportLost | ProbeCause::InputAckTimeout), _) => {
                ProbeHealth::Stalled
            }
        };
        *state
    }

    fn should_notify(&mut self, host: &HostId) -> bool {
        self.notified.insert(host.clone())
    }
}

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

    pub(super) fn recover_stalled_panes(&mut self, now: Now, cx: &mut Context<Self>) {
        let mut monitor = std::mem::take(&mut self.liveness_monitor);
        let delay = monitor
            .previous
            .map(|previous| probe_delay(previous, now, None));
        monitor.previous = Some(now);
        monitor
            .probes
            .retain(|pane, _| self.live.contains_key(pane));
        let mut unhealthy_hosts = BTreeSet::new();
        let mut notices: BTreeMap<HostId, Vec<Notice>> = BTreeMap::new();
        let mut recovered = false;
        for (&pane, live) in &mut self.live {
            if live.pane.account_switching() || live.pane.is_closed() || live.pane.is_cold() {
                monitor.probes.remove(&pane);
                continue;
            }
            let host = self.layout.host(pane).clone();
            if live.sock.is_none() {
                // Reattachment is in progress; retain the per-host notice latch.
                unhealthy_hosts.insert(host);
                monitor.probes.remove(&pane);
                continue;
            }
            let cause = if !live.pane.is_linked() {
                Some(ProbeCause::TransportLost)
            } else if live.pane.stalled(now) {
                Some(ProbeCause::InputAckTimeout)
            } else {
                None
            };
            let state = monitor.observe(pane, cause);
            if state != ProbeHealth::Healthy {
                unhealthy_hosts.insert(host.clone());
            }
            if state != ProbeHealth::Stalled {
                continue;
            }
            // Only close this UI stream. Reattach the same session and leave the PTY alone.
            live.cut();
            notices
                .entry(host.clone())
                .or_default()
                .extend(live.pane.unlink());
            tracing::warn!(%host, ?cause, ?delay, "terminal stream reconnecting");
            recovered = true;
        }
        monitor
            .notified
            .retain(|host| unhealthy_hosts.contains(host));
        for (host, notices) in notices {
            if monitor.should_notify(&host) {
                let notice = if notices.is_empty() {
                    match krill_app::i18n::language() {
                        krill_app::i18n::Language::Ko => {
                            format!("{host}: 세션에 다시 연결하는 중…")
                        }
                        krill_app::i18n::Language::En => format!("{host}: Reconnecting sessions…"),
                    }
                } else {
                    notices
                        .iter()
                        .map(notice_text)
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                self.notice = Some(notice);
                cx.notify();
            }
        }
        self.liveness_monitor = monitor;
        if recovered {
            // Unacknowledged input is uncertain and must never be replayed.
            cx.notify();
        }
    }

    /// One missed probe stays quiet; an ACK clears stale health before the next tick.
    pub(super) fn stalled_banner(&self) -> Option<String> {
        let now = Now::get();
        let stalled = self.layout.visible().into_iter().any(|pane| {
            self.liveness_monitor.probes.get(&pane) == Some(&ProbeHealth::Stalled)
                && self
                    .live
                    .get(&pane)
                    .is_some_and(|live| live.pane.stalled(now))
        });
        krill_app::link::stalled_banner(stalled)
    }
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn liveness_one_miss_is_suspect_and_success_resets_it() {
        let pane = Layout::default().active_tab().focused;
        let mut monitor = LivenessMonitor::default();
        assert_eq!(
            monitor.observe(pane, Some(ProbeCause::InputAckTimeout)),
            ProbeHealth::Suspect
        );
        assert_eq!(monitor.observe(pane, None), ProbeHealth::Healthy);
        assert_eq!(
            monitor.observe(pane, Some(ProbeCause::InputAckTimeout)),
            ProbeHealth::Suspect
        );
        assert_eq!(
            monitor.observe(pane, Some(ProbeCause::InputAckTimeout)),
            ProbeHealth::Stalled
        );
        assert_eq!(
            monitor.observe(pane, Some(ProbeCause::TransportLost)),
            ProbeHealth::Stalled
        );
    }

    #[test]
    fn liveness_notice_is_once_per_host_until_recovery() {
        let mut monitor = LivenessMonitor::default();
        let a = HostId::Remote("a".into());
        let b = HostId::Remote("b".into());
        assert!(monitor.should_notify(&a));
        assert!(!monitor.should_notify(&a));
        assert!(monitor.should_notify(&b));
        monitor.notified.remove(&a);
        assert!(monitor.should_notify(&a));
    }

    #[test]
    fn liveness_delay_separates_sleep_rtt_and_clock_rollback() {
        let before = Now::get();
        let after = Now {
            awake: before.awake + Duration::from_millis(700),
            wall: before.wall + Duration::from_secs(31),
        };
        let delay = probe_delay(before, after, Some(Duration::from_millis(200)));
        assert_eq!(delay.beyond_rtt, Some(Duration::from_millis(500)));
        assert_eq!(delay.asleep, Duration::from_millis(30300));
        assert_eq!(
            probe_delay(after, before, Some(Duration::from_secs(2))).beyond_rtt,
            Some(Duration::ZERO)
        );
        assert_eq!(probe_delay(after, before, None).asleep, Duration::ZERO);
    }
}
