//! 마스터 키(macOS 키체인, Linux는 상태 디렉터리의 0600 파일, Windows는 같은 자리의 DPAPI 봉인 파일)와 세그먼트별 인증 암호화.
//! 잠정 MK-024 포맷에 대한 암호 의존은 이 모듈에 둔다.

use std::fmt;
use std::io;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::Mutex;

use ring::{
    aead, hkdf,
    rand::{SecureRandom as _, SystemRandom},
};
#[cfg(target_os = "macos")]
use security_framework::os::macos::keychain::SecKeychain;
#[cfg(target_os = "macos")]
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

#[cfg(target_os = "macos")]
const SERVICE: &str = "com.krill.segment-key.v2";
#[cfg(target_os = "macos")]
const NOT_FOUND: i32 = -25300;
#[cfg(target_os = "macos")]
const DUPLICATE: i32 = -25299;
pub(super) const SALT_LEN: usize = 32;
// Keychain의 UI 허용 상태는 프로세스 단위다. 이 모듈의 동시 조회를 직렬화한다.
#[cfg(target_os = "macos")]
static KEYCHAIN_GATE: Mutex<()> = Mutex::new(());
/// Linux·Windows 키 파일 이름(상태 디렉터리 바로 아래).
#[cfg(any(target_os = "linux", windows))]
const KEY_FILE: &str = "segment.key";

pub(super) struct Key(Zeroizing<[u8; 32]>);

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key([redacted])")
    }
}

impl Key {
    #[cfg(target_os = "macos")]
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

    /// 헤드리스 Linux VM에는 키체인이 없다. 키를 상태 디렉터리의 0600 파일에 둔다(디스크 권한만큼만 보호된다).
    /// Windows는 같은 파일에 DPAPI(현재 사용자 범위)로 봉인한 blob을 둔다. 다른 사용자·다른 기기에서는 풀리지 않는다.
    /// 임시 파일을 다 쓴 뒤 `link`로 자리에 놓아, 경쟁하는 생성자가 반쯤 쓴 키를 읽거나 기존 키를 덮어쓰지 않는다.
    #[cfg(any(target_os = "linux", windows))]
    pub(super) fn load(root: &Path, existing: bool) -> io::Result<Arc<Self>> {
        use krill_proto::os::fs::OpenOptionsExt as _;
        use std::io::Write as _;
        let path = root.join(KEY_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => return Self::unseal(bytes).map(Arc::new),
            Err(e) if e.kind() == io::ErrorKind::NotFound && !existing => {}
            Err(e) => return Err(unavailable(&e)),
        }
        let mut bytes = Zeroizing::new([0; 32]);
        SystemRandom::new()
            .fill(bytes.as_mut())
            .map_err(crypto_error)?;
        let tmp = root.join(format!(".{KEY_FILE}.{}", std::process::id()));
        std::fs::remove_file(&tmp).ok();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(&seal(bytes.as_ref())?)?;
        file.sync_all()?;
        drop(file);
        let placed = std::fs::hard_link(&tmp, &path);
        std::fs::remove_file(&tmp).ok();
        match placed {
            Ok(()) => Ok(Arc::new(Self(bytes))),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                Self::unseal(std::fs::read(&path)?).map(Arc::new)
            }
            Err(e) => Err(e),
        }
    }

    /// 키 파일 내용을 키로 되돌린다. 풀지 못하면(다른 사용자의 DPAPI blob·변조) 키를 새로 만들지 않고 거부한다.
    #[cfg(any(target_os = "linux", windows))]
    fn unseal(bytes: Vec<u8>) -> io::Result<Self> {
        let bytes = Zeroizing::new(bytes);
        #[cfg(windows)]
        let bytes = crate::sys::unprotect(&bytes).map_err(|e| unavailable(&e))?;
        Self::from_slice(&bytes)
    }

    /// 명시한 키체인만 검색한다. 테스트는 임시 키체인으로 같은 경로를 검증한다.
    #[cfg(target_os = "macos")]
    pub(super) fn load_from(
        root: &Path,
        existing: bool,
        keychain: &SecKeychain,
    ) -> io::Result<Arc<Self>> {
        let account = format!(
            "{:x}",
            Sha256::digest(root.canonicalize()?.as_os_str().as_bytes())
        );
        let found = keychain.find_generic_password(SERVICE, &account);
        match found {
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
        // Part of the on-disk v2 format, independent of the product display name.
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(self.0.as_ref());
        let okm = prk
            .expand(&[b"krill segment v2"], &aead::AES_256_GCM)
            .map_err(crypto_error)?;
        Ok(aead::LessSafeKey::new(aead::UnboundKey::from(okm)))
    }
}

#[cfg(target_os = "macos")]
fn keychain_error(e: security_framework::base::Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("segment key unavailable (keychain status {})", e.code()),
    )
}

#[cfg(any(target_os = "linux", windows))]
fn unavailable(e: &io::Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("segment key unavailable ({e})"),
    )
}

/// 키 파일에 쓸 바이트. Linux는 키 그대로, Windows는 DPAPI blob.
#[cfg(target_os = "linux")]
#[expect(clippy::unnecessary_wraps, reason = "Windows 판과 같은 시그니처")]
fn seal(key: &[u8]) -> io::Result<Vec<u8>> {
    Ok(key.to_vec())
}

#[cfg(windows)]
fn seal(key: &[u8]) -> io::Result<Vec<u8>> {
    crate::sys::protect(key)
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

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn key_file_is_dpapi_sealed_reused_and_required_when_data_exists() {
        let root = std::env::temp_dir().join(format!("krilld-dpapi-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        assert!(
            Key::load(&root, true).is_err(),
            "데이터가 있는데 키가 없으면 새로 만들지 않는다"
        );
        let first = Key::load(&root, false).unwrap();
        let sealed = std::fs::read(root.join(KEY_FILE)).unwrap();
        assert!(sealed.len() > 32, "평문 32바이트가 아니라 DPAPI blob이다");
        assert!(!sealed.windows(32).any(|w| w == first.0.as_ref()));
        let second = Key::load(&root, true).unwrap();
        assert_eq!(first.0.as_ref(), second.0.as_ref());
        let mut bad = sealed;
        if let Some(b) = bad.get_mut(40) {
            *b ^= 1;
        }
        std::fs::write(root.join(KEY_FILE), bad).unwrap();
        let err = Key::load(&root, true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        std::fs::remove_dir_all(root).unwrap();
    }
}
