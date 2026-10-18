use super::{MAX_STREAMS, StreamId, StreamPhase, StreamStatus, state, wire};
use crate::local::Stream;
use crate::{Error, store::now_unix_ms};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use state::{Bytes, Ledger, add};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, ReadHalf, WriteHalf};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
use wire::{CHUNK, HEADER_LEN, Header, Hello, Kind};

static STREAMS: AtomicUsize = AtomicUsize::new(0);
struct StreamSlot;
impl StreamSlot {
    fn reserve() -> Result<Self, Error> {
        STREAMS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(1).filter(|n| *n <= MAX_STREAMS)
            })
            .map_err(|_| Error::Protocol("process stream limit reached"))?;
        Ok(Self)
    }
}
impl Drop for StreamSlot {
    fn drop(&mut self) {
        STREAMS.fetch_sub(1, Ordering::AcqRel);
    }
}
const ACK_DELAY: Duration = Duration::from_millis(20);
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub(crate) struct Attachment {
    pub conn: Connection,
    pub send: SendStream,
    pub recv: RecvStream,
    pub hello: Hello,
    pub reply: bool,
    pub ready: oneshot::Sender<Result<(), &'static str>>,
}

#[derive(Debug)]
pub(crate) struct Logical {
    pub id: StreamId,
    ledger: Mutex<Ledger>,
    status: Mutex<StreamStatus>,
    pub changed: Notify,
    stop: watch::Sender<bool>,
    attach_tx: mpsc::Sender<Attachment>,
}

impl Logical {
    pub fn spawn(
        unix: Stream,
        id: StreamId,
        grace: Duration,
        buffer_limit: usize,
    ) -> Result<(Arc<Self>, JoinHandle<()>), Error> {
        let slot = StreamSlot::reserve()?;
        let (stop, _) = watch::channel(false);
        let (attach_tx, attach_rx) = mpsc::channel(1);
        let logical = Arc::new(Self {
            id,
            ledger: Mutex::new(Ledger::default()),
            status: Mutex::new(StreamStatus {
                id,
                state: StreamPhase::Suspended,
                last_disconnect_at_ms: None,
                disconnect_count: 0,
                retransmitted_bytes: 0,
                buffered_bytes: 0,
                last_resume_ms: None,
                close_reason: None,
            }),
            changed: Notify::new(),
            stop,
            attach_tx,
        });
        let task = Arc::clone(&logical);
        let join = tokio::spawn(async move {
            let _slot = slot;
            task.run(unix, attach_rx, grace, buffer_limit).await;
        });
        Ok((logical, join))
    }

    fn with<R>(&self, f: impl FnOnce(&mut Ledger) -> Result<R, Error>) -> Result<R, Error> {
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| Error::Protocol("replay ledger unavailable"))?;
        f(&mut ledger)
    }
    pub fn snapshot(&self) -> StreamStatus {
        let mut status = match self.status.lock() {
            Ok(s) => s.clone(),
            Err(e) => e.into_inner().clone(),
        };
        if let Ok(ledger) = self.ledger.lock() {
            status.retransmitted_bytes = ledger.retransmitted;
            status.buffered_bytes = ledger.tx_next.saturating_sub(ledger.tx_acked);
        }
        status
    }
    pub fn is_closed(&self) -> bool {
        *self.stop.borrow()
    }
    pub async fn closed(&self) {
        let mut stop = self.stop.subscribe();
        stop.wait_for(|closed| *closed).await.ok();
    }
    pub fn close(&self, reason: &str) {
        if let Ok(mut s) = self.status.lock() {
            if s.state == StreamPhase::Closed {
                return;
            }
            s.state = StreamPhase::Closed;
            s.close_reason = Some(reason.to_owned());
        }
        self.stop.send_replace(true);
        self.changed.notify_waiters();
    }
    pub fn hello(&self, generation: u64, resume: bool, grace: Duration) -> Result<Hello, Error> {
        self.with(|l| {
            Ok(Hello {
                id: self.id,
                generation,
                received: l.rx_committed,
                sent: l.tx_next,
                received_fin: l.rx_fin,
                resume,
                grace_ms: u64::try_from(grace.as_millis())
                    .map_err(|_| Error::Protocol("invalid grace period"))?,
            })
        })
    }
    pub async fn attach(&self, attachment: Attachment) -> Result<(), Error> {
        self.attach_tx
            .send(attachment)
            .await
            .map_err(|_| Error::Protocol("stream closed"))
    }

    async fn run(
        self: Arc<Self>,
        unix: Stream,
        mut attachments: mpsc::Receiver<Attachment>,
        mut grace: Duration,
        buffer_limit: usize,
    ) {
        let (read, write) = tokio::io::split(unix);
        let (incoming_tx, incoming_rx) = mpsc::channel(1);
        let mut local_tasks = JoinSet::new();
        let me = Arc::clone(&self);
        local_tasks.spawn(async move { me.read_local(read, buffer_limit).await });
        let me = Arc::clone(&self);
        local_tasks.spawn(async move { me.write_local(write, incoming_rx).await });
        let mut stop = self.stop.subscribe();
        let mut network: Option<JoinHandle<()>> = None;
        let mut disconnected = Some((Instant::now(), now_unix_ms()));
        let mut outage_counted = false;
        let mut resume_target: Option<(u64, u64, Instant)> = None;
        let mut tick = tokio::time::interval(ACK_DELAY);
        loop {
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                _ = stop.changed() => break,
                result = local_tasks.join_next(), if !local_tasks.is_empty() => {
                    match result {
                        Some(Ok(Ok(())))=>{},
                        Some(Ok(Err(error)))=>{ self.close(&error.to_string()); break; },
                        _=>{ self.close("local socket task failed"); break; }
                    }
                }
                attachment = attachments.recv() => {
                    let Some(mut a) = attachment else { break };
                    let replacing=self.with(|l| Ok(l.generation>0)).unwrap_or(false);
                    let valid = self.with(|l| {
                        if a.hello.id != self.id || a.hello.generation <= l.generation || a.hello.sent < l.rx_committed || a.hello.grace_ms==0 {
                            return Err(Error::Protocol("stale stream generation"));
                        }
                        l.acknowledge(a.hello.received, a.hello.received_fin, true)?;
                        l.generation = a.hello.generation;
                        Ok(())
                    });
                    if valid.is_err() || self.is_closed() {
                        a.ready.send(Err("invalid resume state")).ok();
                        continue;
                    }
                    grace=grace.min(Duration::from_millis(a.hello.grace_ms));
                    if replacing && !outage_counted {
                        outage_counted=true;
                        disconnected=Some((Instant::now(),now_unix_ms()));
                        if let Ok(mut s)=self.status.lock() { s.last_disconnect_at_ms=Some(now_unix_ms()); s.disconnect_count=s.disconnect_count.saturating_add(1); }
                    }
                    if let Some(task) = network.take() { task.abort(); task.await.ok(); }
                    if let Ok(mut s) = self.status.lock() { s.state = StreamPhase::Resuming; }
                    if a.reply {
                        let reply = self.hello(a.hello.generation, true, grace.min(Duration::from_millis(a.hello.grace_ms)));
                        let sent = match reply {
                            Ok(h) => {
                                let remaining=disconnected.map_or(grace,|(at,_)|grace.saturating_sub(at.elapsed()));
                                tokio::select! {
                                result=tokio::time::timeout(remaining, async {
                                a.send.write_all(&[0]).await.map_err(|_| Error::Protocol("ready write failed"))?;
                                wire::write_hello(&mut a.send,h).await
                            })=>result.is_ok_and(|r|r.is_ok()),
                                _=a.conn.closed()=>false,
                                _=stop.changed()=>false,
                                }
                            },
                            Err(_) => false,
                        };
                        if !sent { a.ready.send(Err("ready failed")).ok(); continue; }
                    }
                    let target = self.with(|l| Ok((l.tx_next, a.hello.sent, Instant::now())));
                    resume_target = target.ok();
                    let me=Arc::clone(&self); let incoming=incoming_tx.clone();
                    let cursor = a.hello.received;
                    let generation=a.hello.generation;
                    network=Some(tokio::spawn(async move { me.transport(a.conn,a.send,a.recv,cursor,generation,incoming).await; }));
                    // Keep the original outage deadline until backlog has actually been ACKed.
                    a.ready.send(Ok(())).ok();
                }
                _ = tick.tick() => {
                    if let Some(task) = &network
                        && task.is_finished() {
                        if let Some(task)=network.take() { task.await.ok(); }
                        resume_target=None;
                        if !outage_counted {
                            outage_counted=true;
                            disconnected=Some((Instant::now(),now_unix_ms()));
                            if let Ok(mut s)=self.status.lock() {
                                s.last_disconnect_at_ms=Some(now_unix_ms());
                                s.disconnect_count=s.disconnect_count.saturating_add(1);
                            }
                        }
                        if let Ok(mut s)=self.status.lock() { s.state=StreamPhase::Suspended; }
                        self.changed.notify_waiters();
                    }
                    if let Some((tx_target,rx_target,started))=resume_target
                        && self.with(|l| Ok(l.tx_acked>=tx_target && l.rx_committed>=rx_target)).unwrap_or(false) {
                        let outage = disconnected.take();
                        outage_counted=false;
                        if let Ok(mut s)=self.status.lock() {
                            s.state=StreamPhase::Normal;
                            if s.disconnect_count>0 {
                                s.last_resume_ms=Some(u64::try_from(outage.map_or_else(|| started.elapsed(), |(at,_)| at.elapsed()).as_millis()).unwrap_or(u64::MAX));
                            }
                        }
                        resume_target=None;
                    }
                    if disconnected.is_some_and(|(at,wall)| at.elapsed()>=grace || now_unix_ms().saturating_sub(wall)>=u64::try_from(grace.as_millis()).unwrap_or(u64::MAX)) {
                        self.close("resume grace expired"); break;
                    }
                    if self.with(|l| Ok(l.finished())).unwrap_or(false) {
                        // Let the last FIN ACK reach QUIC before retaining a closed registry entry.
                        tokio::time::sleep(ACK_DELAY.saturating_mul(2)).await;
                        self.close("complete"); break;
                    }
                }
            }
        }
        self.close(
            &self
                .snapshot()
                .close_reason
                .unwrap_or_else(|| "stream stopped".to_owned()),
        );
        if let Some(task) = network {
            task.abort();
            task.await.ok();
        }
        local_tasks.abort_all();
        while local_tasks.join_next().await.is_some() {}
        self.with(|l| {
            l.tx = std::collections::VecDeque::new();
            Ok(())
        })
        .ok();
    }

    async fn read_local(&self, mut unix: ReadHalf<Stream>, limit: usize) -> Result<(), Error> {
        let mut stop = self.stop.subscribe();
        let mut scratch = [0; CHUNK];
        loop {
            // `read` is cancel safe: losing the race to `stop` consumes no bytes.
            let n = tokio::select! { _=stop.changed()=>return Ok(()), n=unix.read(&mut scratch)=>n.map_err(Error::Io)? };
            if n == 0 {
                self.with(|l| {
                    l.tx_fin = true;
                    Ok(())
                })?;
                self.changed.notify_waiters();
                return Ok(());
            }
            let mut bytes = Bytes::zeroed(n)?;
            bytes.as_mut().copy_from_slice(&scratch[..n]);
            self.with(|l| l.append(bytes, limit))?;
            self.changed.notify_waiters();
        }
    }

    async fn write_local(
        &self,
        mut unix: WriteHalf<Stream>,
        mut incoming: mpsc::Receiver<Incoming>,
    ) -> Result<(), Error> {
        let mut stop = self.stop.subscribe();
        loop {
            let frame =
                tokio::select! { _=stop.changed()=>return Ok(()), frame=incoming.recv()=>frame };
            let Some(frame) = frame else { return Ok(()) };
            match frame {
                Incoming::Data { offset, bytes } => {
                    let mut skip =
                        self.with(|l| l.received_prefix(offset, bytes.as_slice().len()))?;
                    while skip < bytes.as_slice().len() {
                        // Cancellation safe single writes; commit immediately, without an await.
                        let n = tokio::select! { _=stop.changed()=>return Ok(()), n=unix.write(&bytes.as_slice()[skip..])=>n.map_err(Error::Io)? };
                        if n == 0 {
                            return Err(Error::Protocol("zero local write"));
                        }
                        self.with(|l| {
                            l.rx_committed = add(l.rx_committed, n)?;
                            Ok(())
                        })?;
                        skip = skip
                            .checked_add(n)
                            .ok_or(Error::Protocol("write overflow"))?;
                        self.changed.notify_waiters();
                    }
                }
                Incoming::Fin(offset) => {
                    self.with(|l| {
                        if offset == l.rx_committed {
                            Ok(())
                        } else {
                            Err(Error::Protocol("invalid final offset"))
                        }
                    })?;
                    unix.shutdown().await.map_err(Error::Io)?;
                    self.with(|l| {
                        l.rx_fin = true;
                        Ok(())
                    })?;
                    self.changed.notify_waiters();
                }
            }
        }
    }

    async fn transport(
        self: Arc<Self>,
        conn: Connection,
        mut send: SendStream,
        mut recv: RecvStream,
        cursor: u64,
        generation: u64,
        incoming: mpsc::Sender<Incoming>,
    ) {
        let reader = self.receive_frames(&mut recv, incoming);
        let writer = self.send_frames(&mut send, cursor);
        tokio::pin!(reader, writer);
        tokio::select! {
            _=conn.closed()=>{},
            result=&mut reader=> { if result.as_ref().is_err_and(|e| matches!(e, Error::Protocol(_))) && self.with(|l|Ok(l.generation==generation)).unwrap_or(false) { self.close("invalid peer frame"); } },
            result=&mut writer=> { if result.as_ref().is_err_and(|e| matches!(e, Error::Protocol(_))) && self.with(|l|Ok(l.generation==generation)).unwrap_or(false) { self.close("invalid replay state"); } },
        }
        self.changed.notify_waiters();
    }

    async fn receive_frames(
        &self,
        recv: &mut RecvStream,
        incoming: mpsc::Sender<Incoming>,
    ) -> Result<(), Error> {
        loop {
            let mut bytes = [0; HEADER_LEN];
            recv.read_exact(&mut bytes).await.map_err(read_error)?;
            let header = Header::decode(&bytes)?;
            match header.kind {
                Kind::Data => {
                    let len =
                        usize::try_from(header.len).map_err(|_| Error::Protocol("frame length"))?;
                    let mut bytes = Bytes::zeroed(len)?;
                    recv.read_exact(bytes.as_mut()).await.map_err(read_error)?;
                    incoming
                        .send(Incoming::Data {
                            offset: header.offset,
                            bytes,
                        })
                        .await
                        .map_err(|_| Error::Protocol("local writer closed"))?;
                }
                Kind::Ack => {
                    self.with(|l| l.acknowledge(header.offset, header.fin, false))?;
                    self.changed.notify_waiters();
                }
                Kind::Fin => incoming
                    .send(Incoming::Fin(header.offset))
                    .await
                    .map_err(|_| Error::Protocol("local writer closed"))?,
                Kind::Reset => return Err(Error::Protocol("peer reset stream")),
            }
        }
    }

    async fn send_frames(&self, send: &mut SendStream, mut cursor: u64) -> Result<(), Error> {
        let replay_until = self.with(|l| Ok(l.tx_exposed))?;
        let mut fin_sent = false;
        let mut previous_ack = None;
        let mut last_ack_at = Instant::now();
        let mut tick = tokio::time::interval(ACK_DELAY);
        loop {
            let notified = self.changed.notified();
            let (ack, fin, tx_fin, tx_next) =
                self.with(|l| Ok((l.rx_committed, l.rx_fin, l.tx_fin, l.tx_next)))?;
            if previous_ack != Some((ack, fin))
                && (fin
                    || previous_ack.is_none()
                    || last_ack_at.elapsed() >= ACK_DELAY
                    || previous_ack
                        .is_some_and(|(previous, _)| ack.saturating_sub(previous) >= 32 * 1024))
            {
                send.write_all(
                    &Header {
                        kind: Kind::Ack,
                        offset: ack,
                        len: 0,
                        fin,
                    }
                    .encode(),
                )
                .await
                .map_err(|e| Error::Transport(e.to_string()))?;
                previous_ack = Some((ack, fin));
                last_ack_at = Instant::now();
            }
            if let Some((end, bytes, skip)) = self.with(|l| l.outbound(&mut cursor))? {
                let payload = &bytes.as_slice()[skip..];
                send.write_all(
                    &Header {
                        kind: Kind::Data,
                        offset: cursor,
                        len: u32::try_from(payload.len())
                            .map_err(|_| Error::Protocol("payload length"))?,
                        fin: false,
                    }
                    .encode(),
                )
                .await
                .map_err(|e| Error::Transport(e.to_string()))?;
                let mut written = 0;
                while written < payload.len() {
                    let n = send
                        .write(&payload[written..])
                        .await
                        .map_err(|e| Error::Transport(e.to_string()))?;
                    if n == 0 {
                        return Err(Error::Transport("zero transport write".to_owned()));
                    }
                    let from = add(cursor, written)?;
                    let replay = add(from, n)?
                        .min(replay_until)
                        .saturating_sub(from.min(replay_until));
                    self.with(|l| {
                        l.retransmitted = l.retransmitted.saturating_add(replay);
                        Ok(())
                    })?;
                    written += n;
                }
                cursor = end;
                continue;
            }
            if tx_fin && cursor == tx_next && !fin_sent {
                send.write_all(
                    &Header {
                        kind: Kind::Fin,
                        offset: cursor,
                        len: 0,
                        fin: false,
                    }
                    .encode(),
                )
                .await
                .map_err(|e| Error::Transport(e.to_string()))?;
                fin_sent = true;
            }
            tokio::select! { ()=notified=>{}, _=tick.tick()=>{} }
        }
    }
}

#[derive(Debug)]
enum Incoming {
    Data { offset: u64, bytes: Bytes },
    Fin(u64),
}

fn read_error(error: iroh::endpoint::ReadExactError) -> Error {
    match error {
        iroh::endpoint::ReadExactError::ReadError(iroh::endpoint::ReadError::ConnectionLost(e)) => {
            Error::Transport(e.to_string())
        }
        _ => Error::Protocol("peer closed logical stream"),
    }
}
