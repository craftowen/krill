//! 키체인 마스터 키와 세그먼트별 인증 암호화. 잠정 MK-024 포맷에 대한 암호 의존은 이 모듈에 둔다.

use std::fmt;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ring::{
    aead, hkdf,
    rand::{SecureRandom as _, SystemRandom},
};
use security_framework::os::macos::keychain::SecKeychain;
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

const SERVICE: &str = "com.minikrill.segment-key.v2";
const NOT_FOUND: i32 = -25300;
const DUPLICATE: i32 = -25299;
pub(super) const SALT_LEN: usize = 32;
// Keychain의 UI 허용 상태는 프로세스 단위다. 이 모듈의 동시 조회를 직렬화한다.
static KEYCHAIN_GATE: Mutex<()> = Mutex::new(());

pub(super) struct Key(Zeroizing<[u8; 32]>);

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key([redacted])")
    }
}

impl Key {
    pub(super) fn load(root: &Path, existing: bool) -> io::Result<Arc<Self>> {
        let _gate = crate::session::lock(&KEYCHAIN_GATE);
        let _ui = if SecKeychain::user_interaction_allowed().map_err(keychain_error)? {
            Some(SecKeychain::disable_user_interaction().map_err(keychain_error)?)
        } else {
            None
        };
        let keychain = SecKeychain::default().map_err(keychain_error)?;
        Self::load_from(root, existing, &keychain)
    }

    /// 명시한 키체인만 검색한다. 테스트는 임시 키체인으로 같은 경로를 검증한다.
    pub(super) fn load_from(
        root: &Path,
        existing: bool,
        keychain: &SecKeychain,
    ) -> io::Result<Arc<Self>> {
        let account = format!(
            "{:x}",
            Sha256::digest(root.canonicalize()?.as_os_str().as_bytes())
        );
        match keychain.find_generic_password(SERVICE, &account) {
            Ok((bytes, _)) => Self::from_slice(bytes.as_ref()).map(Arc::new),
            Err(e) if e.code() == NOT_FOUND && !existing => {
                let mut bytes = Zeroizing::new([0; 32]);
                SystemRandom::new()
                    .fill(bytes.as_mut())
                    .map_err(crypto_error)?;
                // add만 쓴다. 경쟁하는 생성자가 기존 키를 덮어쓰면 이전 세그먼트를 잃는다.
                match keychain.add_generic_password(SERVICE, &account, bytes.as_ref()) {
                    Ok(()) => Ok(Arc::new(Self(bytes))),
                    Err(e) if e.code() == DUPLICATE => {
                        let (bytes, _) = keychain
                            .find_generic_password(SERVICE, &account)
                            .map_err(keychain_error)?;
                        Self::from_slice(bytes.as_ref()).map(Arc::new)
                    }
                    Err(e) => Err(keychain_error(e)),
                }
            }
            Err(e) => Err(keychain_error(e)),
        }
    }

    pub(super) fn from_slice(bytes: &[u8]) -> io::Result<Self> {
        let bytes = bytes.try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid segment key length")
        })?;
        Ok(Self(Zeroizing::new(bytes)))
    }

    fn derive(&self, salt: &[u8; SALT_LEN]) -> io::Result<aead::LessSafeKey> {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(self.0.as_ref());
        let okm = prk
            .expand(&[b"minikrill segment v2"], &aead::AES_256_GCM)
            .map_err(crypto_error)?;
        Ok(aead::LessSafeKey::new(aead::UnboundKey::from(okm)))
    }
}

fn keychain_error(e: security_framework::base::Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("segment key unavailable (keychain status {})", e.code()),
    )
}

fn crypto_error(_: ring::error::Unspecified) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "segment authentication failed")
}

/// 세그먼트마다 무작위 256비트 salt로 별도 키를 유도한다. nonce는 청크 순번(0부터)이며 재개하지 않는다.
/// 헤더 전체와 salt가 AAD여서 세션·seq·크기 변조, 다른 파일의 청크 교체, 순서 변경을 거부한다.
pub(super) struct Stream {
    key: aead::LessSafeKey,
    aad: Vec<u8>,
    index: u64,
}

impl Stream {
    pub(super) fn create(key: &Key, header: &[u8]) -> io::Result<(Self, [u8; SALT_LEN])> {
        let mut salt = [0; SALT_LEN];
        SystemRandom::new().fill(&mut salt).map_err(crypto_error)?;
        Ok((Self::open(key, header, &salt)?, salt))
    }

    pub(super) fn open(key: &Key, header: &[u8], salt: &[u8; SALT_LEN]) -> io::Result<Self> {
        let mut aad = header.to_vec();
        aad.extend_from_slice(salt);
        Ok(Self {
            key: key.derive(salt)?,
            aad,
            index: 0,
        })
    }

    fn nonce(&mut self) -> io::Result<aead::Nonce> {
        let index = self.index;
        self.index = self
            .index
            .checked_add(1)
            .ok_or_else(|| io::Error::other("segment nonce exhausted"))?;
        let mut nonce = [0; aead::NONCE_LEN];
        if let Some(tail) = nonce.get_mut(4..) {
            tail.copy_from_slice(&index.to_be_bytes());
        }
        Ok(aead::Nonce::assume_unique_for_key(nonce))
    }

    pub(super) fn seal(&mut self, body: &mut Vec<u8>) -> io::Result<()> {
        let nonce = self.nonce()?;
        self.key
            .seal_in_place_append_tag(nonce, aead::Aad::from(&self.aad), body)
            .map_err(crypto_error)
    }

    pub(super) fn decrypt<'a>(&mut self, body: &'a mut [u8]) -> io::Result<&'a [u8]> {
        let nonce = self.nonce()?;
        self.key
            .open_in_place(nonce, aead::Aad::from(&self.aad), body)
            .map(|b| &*b)
            .map_err(crypto_error)
    }
}
