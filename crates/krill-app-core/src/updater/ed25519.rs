//! `ring` 기반 Ed25519 서명 검증과 SHA-256·hex 보조 함수(MK-063, T4-D14).
//!
//! 앱은 매니페스트(`latest.json`)의 Ed25519 서명 **검증만** 수행하며, 서명 생성 코드나 비밀키 타입을
//! 포함하지 않는다(서명 생성은 릴리스 파이프라인 몫).

use ring::signature::{ED25519, UnparsedPublicKey};
use sha2::{Digest as _, Sha256};

/// Ed25519 공개키(32바이트).
pub type PublicKey = [u8; 32];
/// Ed25519 서명(64바이트).
pub type Signature = [u8; 64];

/// `public` 공개키로 `message`의 Ed25519 `signature`를 검증한다(`ring::signature::UnparsedPublicKey`).
#[must_use]
pub fn verify(public: &PublicKey, message: &[u8], signature: &Signature) -> bool {
    UnparsedPublicKey::new(&ED25519, public)
        .verify(message, signature)
        .is_ok()
}

/// SHA-256 해시를 64자리 소문자 hex로 돌려준다.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    encode_hex(&digest)
}

/// 고정 길이 소문자/대문자 hex 문자열을 바이트 배열로 디코딩한다.
#[must_use]
pub fn decode_hex<const N: usize>(hex: &str) -> Option<[u8; N]> {
    if hex.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    let raw = hex.as_bytes();
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = nibble(raw[i * 2])?;
        let lo = nibble(raw[i * 2 + 1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

/// 바이트 슬라이스를 소문자 hex 문자열로 인코딩한다.
#[must_use]
pub fn encode_hex(bytes: &[u8]) -> String {
    const LUT: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(LUT[usize::from(b >> 4)]));
        out.push(char::from(LUT[usize::from(b & 0x0f)]));
    }
    out
}

fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
