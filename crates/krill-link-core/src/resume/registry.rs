//! Authenticated server registry. Unknown Resume never opens a new UDS.
use super::{Attachment, HANDSHAKE_TIMEOUT, Logical, StreamId, StreamStatus, wire};
use crate::Error;
use crate::local::Stream;
use iroh::EndpointId;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const MAX_ENTRIES: usize = 256;
const MAX_REQUESTS: usize = 128;
const PEER_REQUESTS: usize = 64;
const TOMBSTONE_TIME: Duration = Duration::from_secs(10);
const PENDING_OPEN_TIMEOUT: Duration = HANDSHAKE_TIMEOUT.saturating_mul(2);

#[derive(Debug)]
struct Entry {
    owner: EndpointId,
    created_at: Instant,
    logical: Option<Arc<Logical>>,
    task: Option<JoinHandle<()>>,
    closed_at: Option<Instant>,
}

#[derive(Debug)]
struct PendingOpen<'a> {
    registry: &'a Registry,
    id: StreamId,
    armed: bool,
}

impl Drop for PendingOpen<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.registry.abort_pending(self.id);
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    entries: BTreeMap<StreamId, Entry>,
    attempts: VecDeque<(Instant, EndpointId)>,
}
#[derive(Debug)]
pub(crate) struct Registry {
    inner: Mutex<Inner>,
    grace: Duration,
    buffer_limit: usize,
}

impl Registry {
    pub fn new(grace: Duration, buffer_limit: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            grace,
            buffer_limit,
        }
    }
    pub fn statuses(&self) -> Vec<StreamStatus> {
        self.inner
            .lock()
            .map(|inner| {
                inner
                    .entries
                    .values()
                    .filter_map(|entry| entry.logical.as_ref().map(|l| l.snapshot()))
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn revoke(&self, node: EndpointId) {
        if let Ok(mut inner) = self.inner.lock() {
            for entry in inner.entries.values_mut().filter(|e| e.owner == node) {
                if let Some(l) = &entry.logical {
                    l.close("device revoked");
                }
                entry.closed_at = Some(Instant::now());
            }
        }
    }
    pub fn sweep(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            let now = Instant::now();
            for entry in inner.entries.values_mut() {
                let stale_pending = entry.logical.is_none()
                    && now.saturating_duration_since(entry.created_at) >= PENDING_OPEN_TIMEOUT;
                if (entry.logical.as_ref().is_some_and(|l| l.is_closed()) || stale_pending)
                    && entry.closed_at.is_none()
                {
                    entry.closed_at = Some(now);
                }
            }
            inner.entries.retain(|_, entry| {
                !(entry
                    .closed_at
                    .is_some_and(|at| now.saturating_duration_since(at) >= TOMBSTONE_TIME)
                    && entry.task.as_ref().is_none_or(JoinHandle::is_finished))
            });
        }
    }
    pub async fn shutdown(&self) {
        let entries = self
            .inner
            .lock()
            .map(|mut inner| std::mem::take(&mut inner.entries))
            .unwrap_or_default();
        for (_, entry) in entries {
            if let Some(l) = entry.logical {
                l.close("server shutdown");
            }
            if let Some(task) = entry.task {
                task.await.ok();
            }
        }
    }

    fn locate(
        &self,
        node: EndpointId,
        hello: wire::Hello,
        allowed: &RwLock<BTreeSet<EndpointId>>,
    ) -> Result<Option<Arc<Logical>>, Error> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| Error::Protocol("registry unavailable"))?;
        if !allowed.read().is_ok_and(|set| set.contains(&node)) {
            return Err(Error::Protocol("unauthorized device"));
        }
        let now = Instant::now();
        while inner
            .attempts
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) >= Duration::from_secs(1))
        {
            inner.attempts.pop_front();
        }
        if inner.attempts.len() >= MAX_REQUESTS
            || inner
                .attempts
                .iter()
                .filter(|(_, peer)| *peer == node)
                .count()
                >= PEER_REQUESTS
        {
            return Err(Error::Protocol("resume request rate exceeded"));
        }
        inner.attempts.push_back((now, node));
        if let Some(entry) = inner.entries.get(&hello.id) {
            if entry.owner != node || entry.closed_at.is_some() {
                return Err(Error::Protocol("stream owner mismatch or expired"));
            }
            let logical = entry
                .logical
                .as_ref()
                .filter(|l| !l.is_closed())
                .ok_or(Error::Protocol("stream unavailable"))?;
            if !hello.resume {
                return Err(Error::Protocol("duplicate stream open"));
            }
            return Ok(Some(Arc::clone(logical)));
        }
        if hello.resume
            || hello.generation != 1
            || hello.received != 0
            || hello.received_fin
            || hello.grace_ms == 0
        {
            return Err(Error::Protocol("unknown resume stream"));
        }
        if inner.entries.len() >= MAX_ENTRIES {
            return Err(Error::Protocol("stream registry full"));
        }
        inner.entries.insert(
            hello.id,
            Entry {
                owner: node,
                created_at: now,
                logical: None,
                task: None,
                closed_at: None,
            },
        );
        Ok(None)
    }

    pub async fn accept(
        &self,
        conn: Connection,
        mut send: SendStream,
        mut recv: RecvStream,
        socket: &Path,
        allowed: &RwLock<BTreeSet<EndpointId>>,
    ) {
        let Ok(Ok(hello)) =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, wire::read_hello(&mut recv)).await
        else {
            return;
        };
        let node = conn.remote_id();
        let found = self.locate(node, hello, allowed);
        let logical = match found {
            Ok(Some(l)) => l,
            Ok(None) => {
                let mut pending = PendingOpen {
                    registry: self,
                    id: hello.id,
                    armed: true,
                };
                let unix = tokio::time::timeout(HANDSHAKE_TIMEOUT, Stream::connect(socket)).await;
                let opened = match unix {
                    Ok(Ok(unix)) => Logical::spawn(
                        unix,
                        hello.id,
                        self.grace.min(Duration::from_millis(hello.grace_ms)),
                        self.buffer_limit,
                    ),
                    _ => Err(Error::Protocol("upstream unavailable")),
                };
                let Ok((logical, task)) = opened else {
                    self.fail_open(hello.id);
                    pending.armed = false;
                    send.write_all(&[1]).await.ok();
                    send.finish().ok();
                    return;
                };
                let installed = if let Ok(mut inner) = self.inner.lock() {
                    if let Some(entry) = inner.entries.get_mut(&hello.id) {
                        if entry.closed_at.is_none()
                            && allowed.read().is_ok_and(|set| set.contains(&node))
                        {
                            entry.logical = Some(Arc::clone(&logical));
                            entry.task = Some(task);
                            true
                        } else {
                            entry.closed_at.get_or_insert_with(Instant::now);
                            logical.close("device revoked during open");
                            false
                        }
                    } else {
                        logical.close("device revoked during open");
                        false
                    }
                } else {
                    logical.close("registry unavailable");
                    false
                };
                pending.armed = false;
                if !installed {
                    return;
                }
                logical
            }
            Err(_) => {
                send.write_all(&[2]).await.ok();
                send.finish().ok();
                return;
            }
        };
        let (ready, rx) = oneshot::channel();
        let attach = Attachment {
            conn,
            send,
            recv,
            hello,
            reply: true,
            ready,
        };
        if logical.attach(attach).await.is_ok() {
            tokio::time::timeout(HANDSHAKE_TIMEOUT.saturating_mul(2), rx)
                .await
                .ok();
        }
    }
    fn abort_pending(&self, id: StreamId) {
        if let Ok(mut inner) = self.inner.lock()
            && inner
                .entries
                .get(&id)
                .is_some_and(|e| e.logical.is_none() && e.closed_at.is_none())
        {
            inner.entries.remove(&id);
        }
    }
    fn fail_open(&self, id: StreamId) {
        if let Ok(mut inner) = self.inner.lock()
            && let Some(entry) = inner.entries.get_mut(&id)
        {
            entry.closed_at = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_rate_limits_unknown_ids_and_never_creates_upstreams() {
        let registry = Registry::new(Duration::from_secs(120), 1024);
        let node = iroh::SecretKey::from_bytes(&[9; 32]).public();
        let allowed = RwLock::new(BTreeSet::from([node]));
        let hello = wire::Hello {
            id: StreamId([1; 16]),
            generation: 2,
            received: 0,
            received_fin: false,
            resume: true,
            grace_ms: 120_000,
            sent: 0,
        };
        for _ in 0..PEER_REQUESTS {
            assert!(matches!(
                registry.locate(node, hello, &allowed),
                Err(Error::Protocol("unknown resume stream"))
            ));
        }
        assert!(matches!(
            registry.locate(node, hello, &allowed),
            Err(Error::Protocol("resume request rate exceeded"))
        ));
        assert!(registry.inner.lock().unwrap().entries.is_empty());
    }
    #[test]
    fn revoked_pending_open_cannot_be_installed_or_recreated() {
        let registry = Registry::new(Duration::from_secs(120), 1024);
        let node = iroh::SecretKey::from_bytes(&[8; 32]).public();
        let allowed = RwLock::new(BTreeSet::from([node]));
        let hello = wire::Hello {
            id: StreamId([2; 16]),
            generation: 1,
            received: 0,
            received_fin: false,
            resume: false,
            grace_ms: 120_000,
            sent: 0,
        };
        assert!(registry.locate(node, hello, &allowed).unwrap().is_none());
        registry.revoke(node);
        assert!(registry.locate(node, hello, &allowed).is_err());
        assert!(
            registry
                .locate(
                    node,
                    wire::Hello {
                        resume: true,
                        generation: 2,
                        ..hello
                    },
                    &allowed
                )
                .is_err()
        );
        assert!(
            registry.inner.lock().unwrap().entries[&hello.id]
                .closed_at
                .is_some()
        );
    }

    #[tokio::test]
    async fn aborted_pending_opens_cleanup_without_exhausting_capacity_and_keep_revoked_tombstones()
    {
        let registry = Arc::new(Registry::new(Duration::from_secs(120), 1024));
        let node = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let allowed = Arc::new(RwLock::new(BTreeSet::from([node])));

        for i in 0_u16..300 {
            registry.inner.lock().unwrap().attempts.clear();
            let mut raw = [0_u8; 16];
            raw[..2].copy_from_slice(&i.to_le_bytes());
            let id = StreamId(raw);
            let hello = wire::Hello {
                id,
                generation: 1,
                received: 0,
                received_fin: false,
                resume: false,
                grace_ms: 120_000,
                sent: 0,
            };
            let reg = Arc::clone(&registry);
            let allow = Arc::clone(&allowed);
            let (entered_tx, entered_rx) = oneshot::channel();
            let handle = tokio::spawn(async move {
                assert!(reg.locate(node, hello, &allow).unwrap().is_none());
                let _pending = PendingOpen {
                    registry: &reg,
                    id,
                    armed: true,
                };
                entered_tx.send(()).ok();
                std::future::pending::<()>().await;
            });
            entered_rx.await.unwrap();
            handle.abort();
            handle.await.ok();
        }
        assert!(registry.inner.lock().unwrap().entries.is_empty());

        // Subsequent open succeeds (not "stream registry full") and revocation during pending open retains tombstone.
        registry.inner.lock().unwrap().attempts.clear();
        let revoked_id = StreamId([0xee; 16]);
        let hello = wire::Hello {
            id: revoked_id,
            generation: 1,
            received: 0,
            received_fin: false,
            resume: false,
            grace_ms: 120_000,
            sent: 0,
        };
        {
            assert!(registry.locate(node, hello, &allowed).unwrap().is_none());
            let _pending = PendingOpen {
                registry: &registry,
                id: revoked_id,
                armed: true,
            };
            registry.revoke(node);
        }
        assert!(
            registry.inner.lock().unwrap().entries[&revoked_id]
                .closed_at
                .is_some()
        );
        assert!(registry.locate(node, hello, &allowed).is_err());
    }

    #[test]
    fn sweep_reclaims_stale_orphaned_pending_reservations() {
        let registry = Registry::new(Duration::from_secs(120), 1024);
        let node = iroh::SecretKey::from_bytes(&[6; 32]).public();
        let allowed = RwLock::new(BTreeSet::from([node]));
        let id = StreamId([0xab; 16]);
        let hello = wire::Hello {
            id,
            generation: 1,
            received: 0,
            received_fin: false,
            resume: false,
            grace_ms: 120_000,
            sent: 0,
        };
        assert!(registry.locate(node, hello, &allowed).unwrap().is_none());
        registry.sweep();
        assert!(
            registry.inner.lock().unwrap().entries[&id]
                .closed_at
                .is_none()
        );

        registry
            .inner
            .lock()
            .unwrap()
            .entries
            .get_mut(&id)
            .unwrap()
            .created_at = Instant::now()
            .checked_sub(PENDING_OPEN_TIMEOUT + Duration::from_secs(1))
            .unwrap();
        registry.sweep();
        assert!(
            registry.inner.lock().unwrap().entries[&id]
                .closed_at
                .is_some()
        );

        registry
            .inner
            .lock()
            .unwrap()
            .entries
            .get_mut(&id)
            .unwrap()
            .closed_at = Instant::now().checked_sub(TOMBSTONE_TIME + Duration::from_secs(1));
        registry.sweep();
        assert!(registry.inner.lock().unwrap().entries.is_empty());
    }
}
