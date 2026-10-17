//! Daemon-owned, bounded webhook delivery; app restarts never duplicate notifications.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};

use krill_proto::{SessionId, control::AgentState};
use serde::Deserialize;
pub mod transport;

#[derive(Debug, Default, Deserialize)]
struct Config {
    #[serde(default)]
    webhooks: Vec<transport::Endpoint>,
}

#[derive(Debug)]
struct Event {
    id: SessionId,
    state: AgentState,
    deep_link: Option<String>,
}

pub(crate) struct Dispatcher {
    sender: SyncSender<Event>,
    seen: BTreeMap<SessionId, AgentState>,
}
impl Dispatcher {
    pub(crate) fn new(dir: Option<&Path>) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Event>(32);
        let path = dir.map(|p| p.join("settings.toml"));
        std::thread::Builder::new()
            .name("webhook-delivery".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                for event in receiver {
                    let config = load(path.as_ref());
                    for endpoint in config.webhooks.iter().take(16) {
                        if transport::deliver_transition(
                            endpoint,
                            event.id,
                            event.state,
                            event.deep_link.as_deref(),
                        )
                        .is_err()
                        {
                            tracing::warn!("webhook delivery failed");
                        }
                    }
                }
            })
            .ok();
        Self {
            sender,
            seen: BTreeMap::new(),
        }
    }
    pub(crate) fn observe(&mut self, id: SessionId, state: AgentState, deep_link: Option<String>) {
        let previous = self.seen.insert(id, state);
        if previous.is_some_and(|old| old != state)
            && matches!(
                state,
                AgentState::Waiting | AgentState::Blocked | AgentState::Done
            )
            && self
                .sender
                .try_send(Event {
                    id,
                    state,
                    deep_link,
                })
                .is_err()
        {
            tracing::warn!("webhook delivery queue unavailable");
        }
    }
    pub(crate) fn retain(&mut self, ids: &[SessionId]) {
        self.seen.retain(|id, _| ids.contains(id));
    }
}
fn load(path: Option<&PathBuf>) -> Config {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .filter(|s| s.len() <= 1024 * 1024)
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transitions_are_deduplicated_and_restarts_start_from_a_baseline() {
        let (sender, receiver) = mpsc::sync_channel(32);
        let mut dispatcher = Dispatcher {
            sender,
            seen: BTreeMap::new(),
        };
        let id = SessionId(42);
        dispatcher.observe(id, AgentState::Done, None);
        assert!(receiver.try_recv().is_err());
        for state in [
            AgentState::Working,
            AgentState::Waiting,
            AgentState::Waiting,
            AgentState::Blocked,
            AgentState::Done,
            AgentState::Done,
        ] {
            dispatcher.observe(id, state, Some("http://127.0.0.1:9000/?session=42".into()));
        }
        for expected in [AgentState::Waiting, AgentState::Blocked, AgentState::Done] {
            let event = receiver.try_recv().unwrap();
            assert_eq!(event.id, id);
            assert_eq!(event.state, expected);
            assert_eq!(
                event.deep_link.as_deref(),
                Some("http://127.0.0.1:9000/?session=42")
            );
        }
        assert!(receiver.try_recv().is_err());
        dispatcher.retain(&[]);
        dispatcher.observe(id, AgentState::Done, None);
        assert!(receiver.try_recv().is_err());
    }
}
