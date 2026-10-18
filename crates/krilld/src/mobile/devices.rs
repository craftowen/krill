//! 0600 원자 저장 기기 레지스트리와 5분 일회용 페어링 티켓. 호출자는 한 잠금으로 직렬화한다.
use super::e2ee::{invalid, random};
use krill_proto::mobile::{PairedMobileDevice, PairingTicket};
use krill_proto::os::fs::{DirBuilderExt, OpenOptionsExt};
use ring::{digest, hmac};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub const TICKET_TTL_MS: u64 = 300_000;
const MAX_DEVICES: usize = 32;
const MAX_FILE: u64 = 1 << 20;

#[derive(Debug)]
pub struct Devices {
    path: Option<PathBuf>,
    entries: Vec<PairedMobileDevice>,
    ticket: Option<PairingTicket>,
    failures: u8,
    retry_at: u64,
}
impl Devices {
    pub fn open(dir: Option<&Path>) -> io::Result<Self> {
        let path = dir.map(|d| d.join("mobile-devices.json"));
        let devices = if let Some(path) = &path {
            match crate::os::open_no_follow(OpenOptions::new().read(true), path) {
                Ok(file) => {
                    let meta = file.metadata()?;
                    if !meta.is_file()
                        || meta.len() > MAX_FILE
                        || !crate::os::private_to_me(&meta, 0o077)
                    {
                        return Err(io::ErrorKind::PermissionDenied.into());
                    }
                    let mut bytes = Vec::new();
                    file.take(MAX_FILE.saturating_add(1))
                        .read_to_end(&mut bytes)?;
                    let devices: Vec<PairedMobileDevice> =
                        serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                    if devices.len() > MAX_DEVICES {
                        return Err(invalid());
                    }
                    for device in &devices {
                        decode_hex::<32>(&device.shared_secret_hash)?;
                    }
                    devices
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
                Err(e) => return Err(e),
            }
        } else {
            Vec::new()
        };
        Ok(Self {
            path,
            entries: devices,
            ticket: None,
            failures: 0,
            retry_at: 0,
        })
    }
    /// 상태 응답에 재접속 검증자를 포함하지 않는다.
    pub fn list(&self) -> Vec<PairedMobileDevice> {
        self.entries
            .iter()
            .map(|d| {
                let mut d = d.clone();
                d.shared_secret_hash.clear();
                d
            })
            .collect()
    }
    pub fn active_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|d| d.revoked_at_ms.is_none())
            .count()
    }
    pub fn ticket(&self, now: u64) -> Option<&PairingTicket> {
        self.ticket.as_ref().filter(|t| now < t.expires_at_ms)
    }
    pub fn clear_ticket(&mut self) {
        self.ticket = None;
    }
    pub fn mint(&mut self, now: u64, rotate: bool) -> io::Result<PairingTicket> {
        if !rotate && let Some(t) = self.ticket(now) {
            return Ok(t.clone());
        }
        let ticket = PairingTicket {
            token: random()?,
            secret: random()?,
            expires_at_ms: now.saturating_add(TICKET_TTL_MS),
        };
        self.ticket = Some(ticket.clone());
        Ok(ticket)
    }
    /// 실패 예산은 ticket 회전으로 초기화하지 않는다. 전체 게이트웨이에 적용해 주소 우회를 막는다.
    pub fn attempt(&mut self, now: u64) -> io::Result<()> {
        if now < self.retry_at {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if self.retry_at != 0 {
            self.failures = 0;
            self.retry_at = 0;
        }
        self.failures = self.failures.saturating_add(1);
        if self.failures >= 5 {
            self.retry_at = now.saturating_add(60_000);
        }
        Ok(())
    }
    pub fn pairing_secret(&self, token: &[u8; 32], now: u64) -> io::Result<[u8; 32]> {
        let ticket = self.ticket(now).ok_or_else(invalid)?;
        if !equal(&ticket.token, token) {
            return Err(invalid());
        }
        Ok(ticket.secret)
    }
    pub fn device_secret(&self, id: &str) -> io::Result<[u8; 32]> {
        let d = self
            .entries
            .iter()
            .find(|d| d.device_id == id && d.revoked_at_ms.is_none())
            .ok_or_else(invalid)?;
        decode_hex(&d.shared_secret_hash)
    }
    /// AEAD proof를 확인한 뒤에만 호출한다. 디스크 저장 성공 후 티켓을 소멸시킨다.
    pub fn pair(
        &mut self,
        token: &[u8; 32],
        label: &str,
        now: u64,
    ) -> io::Result<(PairedMobileDevice, [u8; 32])> {
        self.pairing_secret(token, now)?;
        if label.len() > 128 || self.active_count() >= MAX_DEVICES {
            return Err(invalid());
        }
        let credential = random()?;
        let hash = digest::digest(&digest::SHA256, &credential);
        let device = PairedMobileDevice {
            device_id: hex(&random()?),
            label: label.to_owned(),
            key_fingerprint: hex(hash.as_ref().get(..8).ok_or_else(invalid)?),
            shared_secret_hash: hex(hash.as_ref()),
            created_at_ms: now,
            last_seen_ms: now,
            revoked_at_ms: None,
        };
        let mut next = self.entries.clone();
        next.retain(|d| d.revoked_at_ms.is_none());
        next.push(device.clone());
        self.save(&next)?;
        self.entries = next;
        self.ticket = None;
        self.failures = 0;
        self.retry_at = 0;
        let mut public = device;
        public.shared_secret_hash.clear();
        Ok((public, credential))
    }
    pub fn seen(&mut self, id: &str, now: u64) -> io::Result<()> {
        let mut next = self.entries.clone();
        let d = next
            .iter_mut()
            .find(|d| d.device_id == id && d.revoked_at_ms.is_none())
            .ok_or_else(invalid)?;
        d.last_seen_ms = now;
        self.save(&next)?;
        self.entries = next;
        self.failures = 0;
        self.retry_at = 0;
        Ok(())
    }
    pub fn revoke(&mut self, id: Option<&str>, all: bool, now: u64) -> io::Result<usize> {
        if !all && id.is_none() {
            return Err(invalid());
        }
        let mut next = self.entries.clone();
        let mut count = 0;
        for d in &mut next {
            if d.revoked_at_ms.is_none() && (all || Some(d.device_id.as_str()) == id) {
                d.revoked_at_ms = Some(now);
                count += 1;
            }
        }
        self.save(&next)?;
        self.entries = next;
        // 전체 회수는 대기 QR도 폐기해야 즉시 노출이 끝난다.
        if all {
            self.clear_ticket();
        }
        Ok(count)
    }
    fn save(&self, devices: &[PairedMobileDevice]) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let dir = path.parent().ok_or_else(invalid)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let meta = fs::symlink_metadata(dir)?;
        if !meta.is_dir() || !crate::os::private_to_me(&meta, 0o022) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let tmp = dir.join(format!(".mobile-devices-{}.tmp", hex(&random()?)));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            #[cfg(unix)]
            file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
            file.write_all(&serde_json::to_vec(devices).map_err(io::Error::other)?)?;
            file.sync_all()?;
            fs::rename(&tmp, path)?;
            // Windows는 디렉터리를 File::open으로 열지 못한다(os error 5, OWE-835 실측).
            #[cfg(unix)]
            fs::File::open(dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            fs::remove_file(tmp).ok();
        }
        result
    }
}
fn equal(a: &[u8], b: &[u8]) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, a);
    let other = hmac::Key::new(hmac::HMAC_SHA256, b);
    hmac::verify(
        &key,
        b"mk-mobile-auth",
        hmac::sign(&other, b"mk-mobile-auth").as_ref(),
    )
    .is_ok()
}
pub fn hex(bytes: &[u8]) -> String {
    {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            write!(out, "{b:02x}").ok();
            out
        })
    }
}
pub fn decode_hex<const N: usize>(value: &str) -> io::Result<[u8; N]> {
    if value.len() != N.saturating_mul(2) {
        return Err(invalid());
    }
    let mut out = [0; N];
    for (slot, pair) in out.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
        let text = std::str::from_utf8(pair).map_err(|_| invalid())?;
        *slot = u8::from_str_radix(text, 16).map_err(|_| invalid())?;
    }
    Ok(out)
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
