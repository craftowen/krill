//! Recover every in-flight account replacement and finish its continuation.

use super::*;

impl Registry {
    pub(super) fn recover_account_work(&self, home: &Path) {
        if self
            .account_recovered
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        // A crash between Spawned and the durable handoff must not leave a second agent
        // consuming the selected account while a retry starts another replacement.
        let prepared = self.recover_prepared_accounts();
        let saved: Vec<_> = lock(&self.account_handoffs).values().cloned().collect();
        if saved.is_empty() {
            return;
        }
        let state_dir = self.state_dir();
        let snapshot = accounts::refresh_accounts_with(home, state_dir.as_deref(), false);
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        for mut handoff in saved {
            let (old_live, new_live) = {
                let state = lock(&self.state);
                let live = |id: &SessionId| {
                    state.sessions.contains_key(id)
                        && !state.exited.contains_key(id)
                        && !state.closing.contains(id)
                };
                (live(&handoff.from), handoff.to.is_some_and(|id| live(&id)))
            };
            if !old_live {
                if handoff.to.is_none() && handoff.error.is_none() {
                    handoff.error =
                        Some("Original session has ended; automatic resume cancelled".into());
                    lock(&self.account_handoffs).insert(handoff.from, handoff);
                }
                continue;
            }
            if handoff.error.is_some() {
                continue;
            }
            if let Some(new) = handoff.to {
                if !new_live {
                    handoff.to = None;
                    handoff.error = Some(
                        "Replacement session is unavailable; original session preserved".into(),
                    );
                } else if let Err(error) = self.close(&peer, handoff.from) {
                    self.close(&peer, new).ok();
                    handoff.to = None;
                    handoff.error = Some(format!(
                        "Could not retire original session ({error}); original session preserved"
                    ));
                }
                lock(&self.account_handoffs).insert(handoff.from, handoff);
            } else if let Some(account) = snapshot
                .accounts
                .iter()
                .find(|a| a.id == handoff.account_id)
            {
                lock(&self.account_work.pending)
                    .entry(account.provider)
                    .or_insert_with(|| SwitchTarget {
                        account_id: account.id.clone(),
                    });
            } else {
                handoff.error =
                    Some("Selected account is unavailable; original session preserved".into());
                lock(&self.account_handoffs).insert(handoff.from, handoff);
            }
        }
        self.save_account_handoffs().ok();
        for (path, prepared) in prepared {
            if self.continue_account_work(&prepared).is_ok() {
                std::fs::remove_file(path).ok();
            }
        }
    }

    fn recover_prepared_accounts(&self) -> Vec<(std::path::PathBuf, Prepared)> {
        let Some(dir) = self.state_dir() else {
            return Vec::new();
        };
        // Read the old single-record journal too, for upgrades from sequential switching.
        let mut paths = vec![dir.join(PREPARED_FILE)];
        if let Ok(entries) = std::fs::read_dir(dir.join(PREPARED_DIR)) {
            paths.extend(
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "json")),
            );
        }
        paths
            .into_iter()
            .filter_map(|path| {
                let ready = self.recover_prepared_account(&path);
                if ready.is_none() {
                    std::fs::remove_file(&path).ok();
                }
                ready.map(|prepared| (path, prepared))
            })
            .collect()
    }

    fn recover_prepared_account(&self, path: &Path) -> Option<Prepared> {
        let prepared = std::fs::read(path)
            .ok()
            .filter(|raw| raw.len() <= 4096)
            .and_then(|raw| serde_json::from_slice::<Prepared>(&raw).ok());
        if let Some(prepared) = prepared {
            let committed = lock(&self.account_handoffs)
                .get(&prepared.from)
                .is_some_and(|h| h.to == Some(prepared.to));
            let source = self.agents.stored(prepared.from).ok().flatten();
            let owned = self
                .agents
                .stored(prepared.to)
                .ok()
                .flatten()
                .is_some_and(|a| {
                    a.account.is_some_and(|a| a.id == prepared.account_id)
                        && source.as_ref().is_none_or(|s| {
                            s.profile == a.profile
                                && (s.session_id.is_none() || s.session_id == a.session_id)
                        })
                });
            if !committed && owned {
                let old_live = {
                    let state = lock(&self.state);
                    state.sessions.contains_key(&prepared.from)
                        && !state.exited.contains_key(&prepared.from)
                        && !state.closing.contains(&prepared.from)
                };
                if old_live {
                    self.close(
                        &Peer {
                            pid: None,
                            scope: Scope::Full,
                        },
                        prepared.to,
                    )
                    .ok();
                } else {
                    // The original ended independently while the daemon was down. Keep the
                    // replacement and make it reachable instead of closing the remaining PTY.
                    lock(&self.account_handoffs).insert(
                        prepared.from,
                        SessionHandoff {
                            from: prepared.from,
                            to: Some(prepared.to),
                            account_id: prepared.account_id.clone(),
                            error: None,
                        },
                    );
                    self.save_account_handoffs().ok();
                    return Some(prepared);
                }
            }
            if committed && owned {
                return Some(prepared);
            }
        }
        None
    }

    pub(super) fn continue_account_work(&self, prepared: &Prepared) -> std::io::Result<()> {
        if !prepared.continue_work {
            return Ok(());
        }
        let peer = Peer {
            pid: None,
            scope: Scope::Full,
        };
        let tap = self
            .tap(&peer, prepared.to)
            .ok_or(std::io::ErrorKind::NotFound)?;
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let (committed, old_live, new_live) = {
                let state = lock(&self.state);
                let live = |id| state.sessions.contains_key(&id) && !state.exited.contains_key(&id);
                (
                    lock(&self.account_handoffs)
                        .get(&prepared.from)
                        .is_some_and(|h| h.to == Some(prepared.to)),
                    live(prepared.from) && !state.closing.contains(&prepared.from),
                    live(prepared.to),
                )
            };
            if !committed || old_live || !new_live || tap.pulse().closed {
                return Err(std::io::ErrorKind::NotConnected.into());
            }
            if tap.pulse().end > 0 && tap.quiet_ms() >= 150 {
                return Self::deliver_input_to_tap(&tap, CONTINUE_PROMPT.as_bytes(), true);
            }
            if Instant::now() >= deadline {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
