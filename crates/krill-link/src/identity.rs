//! 기기 비밀키(`link/secret.key`) 생성·보관 및 마스킹(`Debug` 리댁션).
//!
//! - 상태 디렉터리 `<state_dir>/link/secret.key`에 0600 권한·원자적 쓰기로 보관한다.
//! - 로그·오류·`Debug` 출력에 비밀키 원문이나 바이트를 절대 노출하지 않는다.

use std::fmt;
use std::fs;
#[cfg(unix)]
use std::fs::Permissions;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use iroh::{EndpointId, SecretKey};
use ring::rand::{SecureRandom as _, SystemRandom};
use zeroize::Zeroizing;

use crate::Error;
use crate::store::{acquire_store_lock, decode_hex, encode_hex, write_atomic_0600};

/// 기기 비밀키 상대 경로 (`<state_dir>/link/secret.key`).
pub const SECRET_KEY_REL_PATH: &str = "link/secret.key";

/// 기기 신원(Ed25519 비밀키 + 공개 `EndpointId`).
///
/// `fmt::Debug`는 공개 `node_id`만 표시하고 비밀키는 `"<redacted>"`로 마스킹한다(CONVENTIONS T2).
#[derive(Clone)]
pub struct DeviceIdentity {
    secret: SecretKey,
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceIdentity")
            .field("node_id", &self.secret.public())
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

impl DeviceIdentity {
    /// `<state_dir>/link/secret.key` 경로를 돌려준다.
    pub fn key_path(state_dir: &Path) -> PathBuf {
        state_dir.join(SECRET_KEY_REL_PATH)
    }

    /// `<state_dir>/link/secret.key`에서 기기 신원을 읽거나, 없으면 새로 생성해 0600 원자적 쓰기로 저장한다.
    pub fn load_or_create(state_dir: &Path) -> Result<Self, Error> {
        let _lock = acquire_store_lock(state_dir)?;
        let path = Self::key_path(state_dir);
        if path.exists() {
            return Self::load_existing(&path);
        }
        let mut raw = Zeroizing::new([0u8; 32]);
        let rng = SystemRandom::new();
        rng.fill(raw.as_mut())
            .map_err(|_| Error::Crypto("failed to generate device secret key"))?;
        let hex_line = Zeroizing::new(format!("{}\n", encode_hex(&*raw)));
        write_atomic_0600(&path, hex_line.as_bytes()).map_err(Error::Io)?;
        let secret = SecretKey::from_bytes(&raw);
        Ok(Self { secret })
    }

    fn load_existing(path: &Path) -> Result<Self, Error> {
        #[cfg(unix)]
        if let Ok(meta) = fs::metadata(path) {
            let mode = meta.permissions().mode() & 0o777;
            if mode != 0o600 {
                fs::set_permissions(path, Permissions::from_mode(0o600)).map_err(Error::Io)?;
            }
        }
        let bytes = Zeroizing::new(fs::read(path).map_err(Error::Io)?);
        let key_bytes = parse_key_bytes(&bytes)?;
        let secret = SecretKey::from_bytes(&key_bytes);
        Ok(Self { secret })
    }

    /// 공개 기기 ID (`iroh::EndpointId`, Ed25519 공개키).
    pub fn node_id(&self) -> EndpointId {
        self.secret.public()
    }

    #[cfg(unix)]
    pub(crate) fn secret_key(&self) -> &SecretKey {
        &self.secret
    }
}

fn parse_key_bytes(raw: &[u8]) -> Result<Zeroizing<[u8; 32]>, Error> {
    if raw.len() == 32 {
        let mut out = Zeroizing::new([0u8; 32]);
        out.copy_from_slice(raw);
        return Ok(out);
    }
    let s = std::str::from_utf8(raw)
        .map_err(|_| Error::InvalidKey("secret.key is not valid UTF-8 or 32-byte key"))?;
    let trimmed = s.trim();
    if trimmed.len() != 64 {
        return Err(Error::InvalidKey(
            "secret.key must be 32 raw bytes or 64 hex characters",
        ));
    }
    // 오류 메시지에 키 조각이 절대 포함되지 않도록 일반 오류로 변환한다.
    let decoded = Zeroizing::new(
        decode_hex(trimmed)
            .map_err(|_| Error::InvalidKey("secret.key contains invalid hex byte"))?,
    );
    let mut out = Zeroizing::new([0u8; 32]);
    if decoded.len() != 32 {
        return Err(Error::InvalidKey("secret.key decoded length is not 32"));
    }
    out.copy_from_slice(&decoded);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_creates_0600_key_and_persists_across_reloads() {
        let dir = std::env::temp_dir().join(format!("mklink-id-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();

        let id1 = DeviceIdentity::load_or_create(&dir).unwrap();
        let path = DeviceIdentity::key_path(&dir);
        assert!(path.is_file());
        #[cfg(unix)]
        {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let id2 = DeviceIdentity::load_or_create(&dir).unwrap();
        assert_eq!(id1.node_id(), id2.node_id());

        // Debug 문자열에 비밀키 원문이 절대 들어가지 않는지 검증한다.
        let raw_file = fs::read_to_string(&path).unwrap();
        let raw_hex = raw_file.trim();
        let dbg_str = format!("{id1:?}");
        assert!(dbg_str.contains("<redacted>"));
        assert!(!dbg_str.contains(raw_hex));

        // 깨진 비밀키 파일에서 오류가 나도 파일 내용이 오류 문자열에 남지 않는다.
        let secret_fragment = "deadbeefcafebabe_secret_material_do_not_leak";
        fs::write(&path, secret_fragment).unwrap();
        let err = DeviceIdentity::load_or_create(&dir).unwrap_err();
        let err_msg = err.to_string();
        assert!(!err_msg.contains(secret_fragment));

        fs::remove_dir_all(&dir).ok();
    }
}
