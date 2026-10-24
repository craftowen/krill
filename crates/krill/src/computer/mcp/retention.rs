//! Bounded rolling retention. Completed entries may expire; live calls never disappear.
//! The sequence highwater is independent of retained payloads, so eviction never permits replay.

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::Value;

pub(super) const MAX_REQUESTS: usize = 128;
pub(super) const MAX_SEQUENCES: usize = 64;
pub(super) const MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct Recorded {
    pub input: Value,
    pub response: Option<Value>,
    pub queued: bool,
    pub input_bytes: usize,
    pub response_bytes: usize,
    pub order: u64,
}

#[derive(Debug)]
pub(super) struct Sequence {
    pub args: Value,
    pub until: Instant,
    pub waiting: Vec<String>,
    pub result: Option<Value>,
    pub args_bytes: usize,
    pub result_bytes: usize,
    pub order: u64,
}

#[derive(Debug, Default)]
pub(super) struct Retention {
    pub requests: BTreeMap<String, Recorded>,
    pub sequences: BTreeMap<u64, Sequence>,
    pub highwater: u64,
    pub input_bytes: usize,
    pub cache_bytes: usize,
    order: u64,
}

impl Retention {
    pub fn next_order(&mut self) -> u64 {
        self.order = self.order.saturating_add(1);
        self.order
    }

    /// Reserve before a native write. If all candidates are live, return busy without dispatch.
    pub fn room(&mut self, input: usize, cache: usize, requests: usize, sequences: usize) -> bool {
        if input > MAX_INPUT_BYTES || cache > MAX_CACHE_BYTES {
            return false;
        }
        loop {
            let request_full = self.requests.len().saturating_add(requests) > MAX_REQUESTS;
            let sequence_full = self.sequences.len().saturating_add(sequences) > MAX_SEQUENCES;
            let bytes_full = self.input_bytes.saturating_add(input) > MAX_INPUT_BYTES
                || self.cache_bytes.saturating_add(cache) > MAX_CACHE_BYTES;
            if !request_full && !sequence_full && !bytes_full {
                return true;
            }
            let rpc = self
                .requests
                .iter()
                .filter(|(_, r)| r.queued && (!sequence_full || request_full))
                .min_by_key(|(_, r)| r.order)
                .map(|(id, r)| (id.clone(), r.order));
            let seq = self
                .sequences
                .iter()
                .filter(|(_, s)| s.result.is_some() && (!request_full || sequence_full))
                .min_by_key(|(_, s)| s.order)
                .map(|(seq, s)| (*seq, s.order));
            match (rpc, seq) {
                (Some((_, order)), Some((seq, other))) if order > other => self.evict_seq(seq),
                (Some((id, _)), _) => self.evict_rpc(&id),
                (_, Some((seq, _))) => self.evict_seq(seq),
                (None, None) => return false,
            }
        }
    }

    fn evict_rpc(&mut self, id: &str) {
        if let Some(record) = self.requests.remove(id) {
            self.input_bytes = self.input_bytes.saturating_sub(record.input_bytes);
            self.cache_bytes = self.cache_bytes.saturating_sub(record.response_bytes);
        }
    }

    fn evict_seq(&mut self, seq: u64) {
        if let Some(record) = self.sequences.remove(&seq) {
            self.input_bytes = self.input_bytes.saturating_sub(record.args_bytes);
            self.cache_bytes = self.cache_bytes.saturating_sub(record.result_bytes);
        }
    }
}
