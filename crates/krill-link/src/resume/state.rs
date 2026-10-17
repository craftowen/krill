//! Bounded replay ledger; ACK means bytes written to the destination UDS.
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::wire::CHUNK;
use crate::Error;

/// Shared across every server and tunnel in this process, including in-flight frames.
pub const PROCESS_BUFFER_LIMIT: usize = 4 * 1024 * 1024;
static BUFFER_BYTES: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
pub struct Bytes {
    storage: Box<[u8]>,
    reserved: usize,
    len: usize,
}
impl Bytes {
    pub fn zeroed(len: usize) -> Result<Self, Error> {
        if len > CHUNK {
            return Err(Error::Protocol("chunk too large"));
        }
        let reservation = len
            .checked_add(256)
            .ok_or(Error::Protocol("reservation overflow"))?;
        BUFFER_BYTES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(reservation)
                    .filter(|n| *n <= PROCESS_BUFFER_LIMIT)
            })
            .map_err(|_| Error::Protocol("process replay buffer exhausted"))?;
        Ok(Self {
            storage: vec![0; len].into_boxed_slice(),
            reserved: reservation,
            len,
        })
    }
    pub fn as_mut(&mut self) -> &mut [u8] {
        &mut self.storage[..self.len]
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.storage[..self.len]
    }
}
impl Drop for Bytes {
    fn drop(&mut self) {
        BUFFER_BYTES.fetch_sub(self.reserved, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub struct Chunk {
    pub offset: u64,
    pub bytes: Arc<Bytes>,
}
impl Chunk {
    pub fn end(&self) -> Result<u64, Error> {
        add(self.offset, self.bytes.as_slice().len())
    }
}

pub fn add(offset: u64, len: usize) -> Result<u64, Error> {
    offset
        .checked_add(u64::try_from(len).map_err(|_| Error::Protocol("length overflow"))?)
        .ok_or(Error::Protocol("offset overflow"))
}

#[derive(Debug, Default)]
pub struct Ledger {
    pub tx: VecDeque<Chunk>,
    pub tx_next: u64,
    pub tx_acked: u64,
    pub tx_exposed: u64,
    pub tx_fin: bool,
    pub tx_fin_acked: bool,
    pub rx_committed: u64,
    pub rx_fin: bool,
    pub generation: u64,
    pub retransmitted: u64,
}

impl Ledger {
    pub fn append(&mut self, bytes: Bytes, limit: usize) -> Result<(), Error> {
        let next = add(self.tx_next, bytes.as_slice().len())?;
        if next.saturating_sub(self.tx_acked)
            > u64::try_from(limit).map_err(|_| Error::Protocol("invalid replay limit"))?
        {
            return Err(Error::Protocol("stream replay buffer exhausted"));
        }
        self.tx.push_back(Chunk {
            offset: self.tx_next,
            bytes: Arc::new(bytes),
        });
        self.tx_next = next;
        Ok(())
    }

    pub fn acknowledge(&mut self, offset: u64, fin: bool, resume: bool) -> Result<(), Error> {
        if offset > self.tx_exposed
            || (resume && offset < self.tx_acked)
            || (fin && (!self.tx_fin || offset != self.tx_next))
        {
            return Err(Error::Protocol("invalid acknowledgement offset"));
        }
        self.tx_acked = self.tx_acked.max(offset);
        self.tx_fin_acked |= fin;
        while self
            .tx
            .front()
            .is_some_and(|c| c.end().is_ok_and(|end| end <= self.tx_acked))
        {
            self.tx.pop_front();
        }
        let threshold = self.tx.len().saturating_mul(2).max(64);
        if self.tx.capacity() > threshold {
            self.tx.shrink_to(self.tx.len().max(16));
        }
        Ok(())
    }

    pub fn outbound(
        &mut self,
        cursor: &mut u64,
    ) -> Result<Option<(u64, Arc<Bytes>, usize)>, Error> {
        // ACKs may advance after the resume preamble captured the send cursor.
        *cursor = (*cursor).max(self.tx_acked);
        for chunk in &self.tx {
            let end = chunk.end()?;
            if *cursor < end {
                if *cursor < chunk.offset {
                    return Err(Error::Protocol("replay cursor gap"));
                }
                let skip = usize::try_from(*cursor - chunk.offset)
                    .map_err(|_| Error::Protocol("replay cursor overflow"))?;
                self.tx_exposed = self.tx_exposed.max(end);
                return Ok(Some((end, Arc::clone(&chunk.bytes), skip)));
            }
        }
        Ok(None)
    }

    pub fn received_prefix(&self, offset: u64, len: usize) -> Result<usize, Error> {
        let end = add(offset, len)?;
        if offset > self.rx_committed || (self.rx_fin && end > self.rx_committed) {
            return Err(Error::Protocol("incoming byte offset gap"));
        }
        usize::try_from(
            self.rx_committed
                .saturating_sub(offset)
                .min(u64::try_from(len).map_err(|_| Error::Protocol("length overflow"))?),
        )
        .map_err(|_| Error::Protocol("incoming byte offset overflow"))
    }
    pub fn finished(&self) -> bool {
        self.tx_fin_acked && self.rx_fin
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    fn data(bytes: &[u8]) -> Bytes {
        let mut out = Bytes::zeroed(bytes.len()).unwrap();
        out.as_mut().copy_from_slice(bytes);
        out
    }
    #[test]
    fn invalid_ack_and_stream_limit() {
        let mut l = Ledger::default();
        l.append(data(b"abc"), 3).unwrap();
        assert!(l.append(data(b"d"), 3).is_err());
        assert!(l.acknowledge(1, false, false).is_err());
        let mut cursor = 0;
        l.outbound(&mut cursor).unwrap();
        l.acknowledge(2, false, false).unwrap();
        assert!(l.acknowledge(1, false, true).is_err());
        assert!(l.acknowledge(4, false, true).is_err());
        assert!(l.received_prefix(1, 1).is_err());
    }
    #[test]
    fn late_ack_skips_retired_replay_prefix_before_the_next_frame() {
        let mut ledger = Ledger::default();
        ledger.append(data(b"old!"), 32).unwrap();
        let mut previous_cursor = 0;
        ledger.outbound(&mut previous_cursor).unwrap();
        ledger.append(data(b"new!"), 32).unwrap();
        // The preamble said zero; the old UDS writer then commits four bytes.
        let mut resume_cursor = 0;
        ledger.acknowledge(4, false, false).unwrap();
        let (end, bytes, skip) = ledger.outbound(&mut resume_cursor).unwrap().unwrap();
        assert_eq!(resume_cursor, 4);
        assert_eq!(end, 8);
        assert_eq!(&bytes.as_slice()[skip..], b"new!");
    }
    proptest! {
        #[test]
        fn random_outages_partial_writes_and_lost_acks(input in proptest::collection::vec(any::<u8>(),0..2048), writes in proptest::collection::vec(1usize..70,1..100), losses in proptest::collection::vec(any::<bool>(),1..100)) {
            let mut sender=Ledger::default(); let mut receiver=Ledger::default(); let mut delivered=Vec::new();
            for bytes in input.chunks(83) { sender.append(data(bytes),4096).unwrap(); }
            let mut cursor=0; let mut step=0;
            while delivered.len()<input.len() {
                let Some((end,bytes,skip))=sender.outbound(&mut cursor).unwrap() else { break };
                let slice=&bytes.as_slice()[skip..];
                let dedup=receiver.received_prefix(cursor,slice.len()).unwrap();
                let n=(slice.len()-dedup).min(writes[step%writes.len()]);
                delivered.extend_from_slice(&slice[dedup..dedup+n]);
                receiver.rx_committed+=u64::try_from(n).unwrap();
                if !losses[step%losses.len()] { sender.acknowledge(receiver.rx_committed,false,false).unwrap(); }
                // Break at arbitrary destination write boundaries, then handshake a cumulative ACK.
                sender.acknowledge(receiver.rx_committed,false,true).unwrap();
                cursor=receiver.rx_committed;
                prop_assert!(cursor<=end);
                step+=1;
            }
            prop_assert_eq!(delivered,input);
            prop_assert_eq!(sender.tx_acked,receiver.rx_committed);
        }
    }
}
