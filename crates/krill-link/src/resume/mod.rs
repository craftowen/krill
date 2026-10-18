//! Resumable link stream metadata and local-socket transport.
use serde::{Deserialize, Serialize};

mod logical;
pub(crate) mod registry;
mod state;
pub(crate) mod wire;
pub(crate) use logical::{Attachment, HANDSHAKE_TIMEOUT, Logical};

pub const MAX_STREAMS: usize = 32;
pub const DEFAULT_STREAM_BUFFER_LIMIT: usize = 1024 * 1024;
/// Shared across every server and tunnel in this process, including in-flight frames.
pub const PROCESS_BUFFER_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StreamId(pub [u8; 16]);

impl StreamId {
    pub fn random() -> Result<Self, crate::Error> {
        use ring::rand::{SecureRandom as _, SystemRandom};
        let mut bytes = [0; 16];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| crate::Error::Crypto("stream id generation failed"))?;
        Ok(Self(bytes))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamPhase {
    Normal,
    Suspended,
    Resuming,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamStatus {
    pub id: StreamId,
    pub state: StreamPhase,
    pub last_disconnect_at_ms: Option<u64>,
    pub disconnect_count: u64,
    pub retransmitted_bytes: u64,
    pub buffered_bytes: u64,
    pub last_resume_ms: Option<u64>,
    pub close_reason: Option<String>,
}
