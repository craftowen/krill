//! E2EE v2: HKDF-SHA256 키 분리, ChaCha20-Poly1305, 방향별 단조 nonce.
//! 각 연결은 양측 CSPRNG nonce를 salt로 사용해야 한다. 세션 키를 재사용하지 않는다.
use ring::{
    aead, hkdf,
    rand::{SecureRandom, SystemRandom},
};
use std::fmt;
use std::io;

pub const MAX_PLAINTEXT: usize = krill_proto::ipc::MAX_FRAME + 4;

pub fn random() -> io::Result<[u8; 32]> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| invalid())?;
    Ok(bytes)
}

pub(crate) fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid mobile authentication or frame",
    )
}

#[derive(Debug, Clone, Copy)]
pub enum Endpoint {
    Client,
    Daemon,
}

pub struct Channel {
    send: Cipher,
    receive: Cipher,
}
impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel").finish_non_exhaustive()
    }
}
struct Cipher {
    key: aead::LessSafeKey,
    prefix: [u8; 4],
    next: Option<u64>,
}

impl Channel {
    /// `salt` = server challenge(32B) || client nonce(32B), 둘 다 연결마다 새로 만든다.
    pub fn new(secret: &[u8; 32], salt: &[u8; 64], endpoint: Endpoint) -> io::Result<Self> {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(secret);
        let client = Cipher::new(&prk, b"mk-mobile-v2/client-to-daemon", *b"MKC2")?;
        let daemon = Cipher::new(&prk, b"mk-mobile-v2/daemon-to-client", *b"MKD2")?;
        let (send, receive) = match endpoint {
            Endpoint::Client => (client, daemon),
            Endpoint::Daemon => (daemon, client),
        };
        Ok(Self { send, receive })
    }
    pub fn seal(&mut self, plaintext: &[u8]) -> io::Result<Vec<u8>> {
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(invalid());
        }
        let seq = self.send.next.ok_or_else(invalid)?;
        self.send.next = seq.checked_add(1);
        let mut data = plaintext.to_vec();
        self.send
            .key
            .seal_in_place_append_tag(
                nonce(self.send.prefix, seq),
                aead::Aad::from(seq.to_le_bytes()),
                &mut data,
            )
            .map_err(|_| invalid())?;
        let mut out = Vec::with_capacity(data.len().saturating_add(8));
        out.extend_from_slice(&seq.to_le_bytes());
        out.extend(data);
        Ok(out)
    }
    pub fn open(&mut self, frame: &[u8]) -> io::Result<Vec<u8>> {
        if !(24..=MAX_PLAINTEXT.saturating_add(24)).contains(&frame.len()) {
            return Err(invalid());
        }
        let seq = u64::from_le_bytes(
            frame
                .get(..8)
                .and_then(|s| s.try_into().ok())
                .ok_or_else(invalid)?,
        );
        let next = self.receive.next.ok_or_else(invalid)?;
        if seq < next {
            return Err(invalid());
        }
        let mut data = frame.get(8..).ok_or_else(invalid)?.to_vec();
        let plaintext = self
            .receive
            .key
            .open_in_place(
                nonce(self.receive.prefix, seq),
                aead::Aad::from(seq.to_le_bytes()),
                &mut data,
            )
            .map_err(|_| invalid())?;
        let len = plaintext.len();
        data.truncate(len);
        self.receive.next = seq.checked_add(1);
        Ok(data)
    }
}
impl Cipher {
    fn new(prk: &hkdf::Prk, info: &[u8], prefix: [u8; 4]) -> io::Result<Self> {
        let context = [info];
        let okm = prk
            .expand(&context, &aead::CHACHA20_POLY1305)
            .map_err(|_| invalid())?;
        let key = aead::UnboundKey::from(okm);
        Ok(Self {
            key: aead::LessSafeKey::new(key),
            prefix,
            next: Some(0),
        })
    }
}
fn nonce(prefix: [u8; 4], seq: u64) -> aead::Nonce {
    let mut bytes = [0; 12];
    for (slot, value) in bytes
        .iter_mut()
        .zip(prefix.into_iter().chain(seq.to_le_bytes()))
    {
        *slot = value;
    }
    aead::Nonce::assume_unique_for_key(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> (Channel, Channel) {
        (
            Channel::new(&[7; 32], &[9; 64], Endpoint::Client).unwrap(),
            Channel::new(&[7; 32], &[9; 64], Endpoint::Daemon).unwrap(),
        )
    }
    #[test]
    fn bidirectional_replay_and_tamper_rejection() {
        let (mut a, mut b) = pair();
        let f = a.seal(b"prompt").unwrap();
        // Node/OpenSSL HKDF + ChaCha20-Poly1305 독립 구현과 고정 바이트 비교.
        assert_eq!(
            crate::mobile::devices::hex(&f),
            "0000000000000000ca8c87d414301b7fc3a6a3f09a5fb977fb9422ea97cb"
        );
        assert_eq!(b.open(&f).unwrap(), b"prompt");
        assert!(b.open(&f).is_err());
        let mut bad = a.seal(b"approval").unwrap();
        *bad.last_mut().unwrap() ^= 1;
        assert!(b.open(&bad).is_err());
        let valid = a.seal(b"next").unwrap();
        assert_eq!(b.open(&valid).unwrap(), b"next");
        let back = b.seal(b"screen").unwrap();
        assert_eq!(a.open(&back).unwrap(), b"screen");
        assert!(a.open(&valid).is_err());
    }
    #[test]
    fn new_challenge_and_direction_separate_keys() {
        let (mut a, mut b) = pair();
        let f = a.seal(b"same").unwrap();
        assert_ne!(f, b.seal(b"same").unwrap());
        let mut other = Channel::new(&[7; 32], &[8; 64], Endpoint::Daemon).unwrap();
        assert!(other.open(&f).is_err());
    }
    #[test]
    fn sequence_exhaustion_never_wraps() {
        let (mut a, mut b) = pair();
        a.send.next = Some(u64::MAX);
        b.receive.next = Some(u64::MAX);
        let f = a.seal(b"last").unwrap();
        assert_eq!(b.open(&f).unwrap(), b"last");
        assert!(a.seal(b"wrap").is_err());
        assert!(b.open(&f).is_err());
    }
    proptest::proptest! {
        #[test]
        fn arbitrary_frames_never_panic(frame in proptest::collection::vec(proptest::num::u8::ANY, 0..1024)) {
            let (_,mut b)=pair();
            b.open(&frame).ok();
        }
    }
}
