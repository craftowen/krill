//! Versioned link-only framing. This never parses or changes krilld frames.
use super::StreamId;
use iroh::endpoint::{RecvStream, SendStream};

use crate::Error;

pub const MAGIC: [u8; 4] = *b"MKL2";
pub const CHUNK: usize = 8 * 1024;
pub const HEADER_LEN: usize = 14;
pub const HELLO_LEN: usize = 54;

/// Open retries are idempotent until the creating connection generation ends.
/// Once that generation is lost, only Resume is allowed (even when Ready was lost).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub id: StreamId,
    pub generation: u64,
    pub received: u64,
    pub received_fin: bool,
    pub resume: bool,
    pub grace_ms: u64,
    pub sent: u64,
}

impl Hello {
    pub fn encode(self) -> [u8; HELLO_LEN] {
        let mut out = [0; HELLO_LEN];
        out[..4].copy_from_slice(&MAGIC);
        out[4] = u8::from(self.resume) | (u8::from(self.received_fin) << 1);
        out[5..21].copy_from_slice(&self.id.0);
        out[21..29].copy_from_slice(&self.generation.to_le_bytes());
        out[29..37].copy_from_slice(&self.received.to_le_bytes());
        out[37..45].copy_from_slice(&self.grace_ms.to_le_bytes());
        out[45] = 0;
        out[46..54].copy_from_slice(&self.sent.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8; HELLO_LEN]) -> Result<Self, Error> {
        if bytes[..4] != MAGIC || bytes[4] & !3 != 0 || bytes[45] != 0 {
            return Err(Error::Protocol("unsupported resume preamble"));
        }
        Ok(Self {
            id: StreamId(array(&bytes[5..21])?),
            generation: u64::from_le_bytes(array(&bytes[21..29])?),
            received: u64::from_le_bytes(array(&bytes[29..37])?),
            received_fin: bytes[4] & 2 != 0,
            resume: bytes[4] & 1 != 0,
            grace_ms: u64::from_le_bytes(array(&bytes[37..45])?),
            sent: u64::from_le_bytes(array(&bytes[46..54])?),
        })
    }
}

fn array<const N: usize>(bytes: &[u8]) -> Result<[u8; N], Error> {
    bytes
        .try_into()
        .map_err(|_| Error::Protocol("invalid field length"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Data = 1,
    Ack = 2,
    Fin = 3,
    Reset = 4,
}

impl TryFrom<u8> for Kind {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            1 => Ok(Self::Data),
            2 => Ok(Self::Ack),
            3 => Ok(Self::Fin),
            4 => Ok(Self::Reset),
            _ => Err(Error::Protocol("unknown resume frame")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: Kind,
    pub offset: u64,
    pub len: u32,
    pub fin: bool,
}

impl Header {
    pub fn encode(self) -> [u8; HEADER_LEN] {
        let mut out = [0; HEADER_LEN];
        out[0] = self.kind as u8;
        out[1] = u8::from(self.fin);
        out[2..10].copy_from_slice(&self.offset.to_le_bytes());
        out[10..14].copy_from_slice(&self.len.to_le_bytes());
        out
    }
    pub fn decode(bytes: &[u8; HEADER_LEN]) -> Result<Self, Error> {
        let kind = Kind::try_from(bytes[0])?;
        let len = u32::from_le_bytes(array(&bytes[10..14])?);
        if bytes[1] > 1
            || len > u32::try_from(CHUNK).map_err(|_| Error::Protocol("chunk size"))?
            || (kind == Kind::Data && (len == 0 || bytes[1] != 0))
            || (kind != Kind::Data && len != 0)
            || (kind != Kind::Ack && bytes[1] != 0)
        {
            return Err(Error::Protocol("invalid resume frame length or flags"));
        }
        let offset = u64::from_le_bytes(array(&bytes[2..10])?);
        offset
            .checked_add(u64::from(len))
            .ok_or(Error::Protocol("offset overflow"))?;
        Ok(Self {
            kind,
            offset,
            len,
            fin: bytes[1] != 0,
        })
    }
}

pub async fn read_hello(recv: &mut RecvStream) -> Result<Hello, Error> {
    let mut bytes = [0; HELLO_LEN];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|_| Error::Protocol("truncated resume preamble"))?;
    Hello::decode(&bytes)
}

pub async fn write_hello(send: &mut SendStream, hello: Hello) -> Result<(), Error> {
    send.write_all(&hello.encode())
        .await
        .map_err(|e| Error::Transport(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn fixed_v2_hello_and_header() {
        let h = Hello {
            id: StreamId([0x55; 16]),
            generation: 1,
            received: 2,
            received_fin: true,
            resume: true,
            grace_ms: 120_000,
            sent: 3,
        };
        let bytes = h.encode();
        assert_eq!(&bytes[..5], b"MKL2\x03");
        assert_eq!(&bytes[21..29], &[1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&bytes[37..45], &[0xc0, 0xd4, 1, 0, 0, 0, 0, 0]);
        assert_eq!(Hello::decode(&bytes).unwrap(), h);
        let hdr = Header {
            kind: Kind::Data,
            offset: 9,
            len: 3,
            fin: false,
        };
        assert_eq!(hdr.encode(), [1, 0, 9, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0]);
        assert_eq!(Header::decode(&hdr.encode()).unwrap(), hdr);
    }
    #[test]
    fn rejects_legacy_preamble_and_oversized_frames() {
        assert!(Hello::decode(&[0; HELLO_LEN]).is_err());
        let header = Header {
            kind: Kind::Data,
            offset: u64::MAX,
            len: 1,
            fin: false,
        };
        assert!(Header::decode(&header.encode()).is_err());
        let header = Header {
            kind: Kind::Data,
            offset: 0,
            len: 8193,
            fin: false,
        };
        assert!(Header::decode(&header.encode()).is_err());
    }
    proptest! {
        #[test]
        fn arbitrary_hello_roundtrip(id in any::<[u8;16]>(),generation in any::<u64>(),received in any::<u64>(),sent in any::<u64>(),received_fin in any::<bool>(),resume in any::<bool>(),grace_ms in any::<u64>()) {
            let hello=Hello { id:StreamId(id),generation,received,received_fin,resume,grace_ms,sent };
            prop_assert_eq!(Hello::decode(&hello.encode()).unwrap(),hello);
        }
        #[test]
        fn headers_never_panic(bytes in any::<[u8; HEADER_LEN]>()) {
            if let Ok(h) = Header::decode(&bytes) { prop_assert_eq!(Header::decode(&h.encode()).unwrap(),h); }
        }
        #[test]
        fn hello_never_panics(bytes in any::<[u8; HELLO_LEN]>()) {
            if let Ok(h) = Hello::decode(&bytes) { prop_assert_eq!(h.encode(), bytes); }
        }
    }
}
