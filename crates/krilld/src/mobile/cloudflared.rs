//! 검증한 공식 cloudflared를 비동기로 확보한다. 네트워크·압축 해제는 게이트웨이 락 밖에서 수행한다.
use crate::session::lock;
use ring::digest;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
enum Download {
    #[default]
    Idle,
    Running,
    Done(io::Result<PathBuf>),
}
#[derive(Debug, Default)]
pub(super) struct Resolver {
    state: Arc<Mutex<Download>>,
}
type Installer = fn(&Path) -> io::Result<PathBuf>;

impl Resolver {
    pub fn resolve(&self, dir: Option<&Path>) -> io::Result<PathBuf> {
        let override_bin = std::env::var_os("KRILL_CLOUDFLARED_BIN");
        let path = std::env::var_os("PATH");
        if let Some(binary) = find_binary(override_bin.as_deref(), path.as_deref(), dir) {
            return Ok(binary);
        }
        let dir = dir
            .ok_or_else(|| io::Error::other("cloudflared 자동 설치용 상태 디렉터리가 없습니다"))?;
        self.acquire(dir, install)
    }
    fn acquire(&self, dir: &Path, installer: Installer) -> io::Result<PathBuf> {
        let mut state = lock(&self.state);
        match &*state {
            Download::Running => return Err(downloading()),
            Download::Done(_) => {
                if let Download::Done(result) = std::mem::take(&mut *state) {
                    return result;
                }
            }
            Download::Idle => {}
        }
        *state = Download::Running;
        let shared = Arc::clone(&self.state);
        let dir = dir.to_owned();
        if let Err(error) = std::thread::Builder::new()
            .name("mobile-cloudflared".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let result = installer(&dir).map_err(|e| {
                    io::Error::new(e.kind(), format!("cloudflared 내려받기 실패: {e}"))
                });
                *lock(&shared) = Download::Done(result);
            })
        {
            *state = Download::Idle;
            return Err(error);
        }
        Err(downloading())
    }
    pub fn downloading(&self) -> bool {
        matches!(*lock(&self.state), Download::Running)
    }
}
fn downloading() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "cloudflared 내려받는 중 — 잠시 후 다시 시도",
    )
}
fn executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
fn find_binary(
    override_bin: Option<&OsStr>,
    path: Option<&OsStr>,
    dir: Option<&Path>,
) -> Option<PathBuf> {
    find_with(override_bin, path, dir, executable)
}
fn find_with(
    override_bin: Option<&OsStr>,
    path: Option<&OsStr>,
    dir: Option<&Path>,
    mut exists: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(binary) = override_bin {
        return Some(PathBuf::from(binary));
    }
    path.into_iter()
        .flat_map(std::env::split_paths)
        .map(|p| p.join("cloudflared"))
        .chain([
            PathBuf::from("/opt/homebrew/bin/cloudflared"),
            PathBuf::from("/usr/local/bin/cloudflared"),
        ])
        .chain(dir.map(|p| p.join("bin/cloudflared")))
        .find(|p| exists(p))
}
fn release(os: &str, arch: &str) -> io::Result<(&'static str, &'static str)> {
    match (os, arch) {
        ("macos", "aarch64") => Ok((
            "cloudflared-darwin-arm64.tgz",
            "647615df99d49cd179ea5e13ef760b9bbfb33630f6ae62f87e33dfadda60adcc",
        )),
        ("macos", "x86_64") => Ok((
            "cloudflared-darwin-amd64.tgz",
            "3b0dbaf378337e9d7fb6cd235a76d1d6a7dee434c2d8edf9281164fb6a3837ab",
        )),
        ("linux", "x86_64") => Ok((
            "cloudflared-linux-amd64",
            "3bfc539f45d2620c71debb7a86be0efa737ed5ccbad9eb85c787116900e5c586",
        )),
        ("linux", "aarch64") => Ok((
            "cloudflared-linux-arm64",
            "2c65dfabf616c40efc7545d107d7a57ad5e21377e2964b7e88af19a238074a46",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "cloudflared 자동 설치를 지원하지 않는 플랫폼",
        )),
    }
}
fn verify(path: &Path, expected: &str) -> io::Result<()> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut context = digest::Context::new(&digest::SHA256);
    let mut buffer = [0; 8192];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        context.update(
            buffer
                .get(..n)
                .ok_or_else(|| io::Error::other("해시 버퍼 범위 오류"))?,
        );
    }
    if super::devices::hex(context.finish().as_ref()) != expected {
        fs::remove_file(path)?;
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cloudflared SHA-256 불일치",
        ));
    }
    Ok(())
}
fn install(dir: &Path) -> io::Result<PathBuf> {
    let (asset, expected) = release(std::env::consts::OS, std::env::consts::ARCH)?;
    install_with(dir, asset, expected, |archive| {
        let status = Command::new("curl")
            .args(["-fsSL", "--max-time", "180", "-o"])
            .arg(archive)
            .arg(format!(
                "https://github.com/cloudflare/cloudflared/releases/download/2026.10.1/{asset}"
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "공식 릴리스 요청 종료 코드 {status}"
            )))
        }
    })
}
fn install_with(
    dir: &Path,
    asset: &str,
    expected: &str,
    fetch: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let bin = dir.join("bin");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&bin)?;
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700))?;
    let tmp = bin.join(format!(
        ".cloudflared-{}",
        super::devices::hex(&super::e2ee::random()?)
    ));
    fs::DirBuilder::new().mode(0o700).create(&tmp)?;
    let result = (|| {
        let archive = tmp.join(asset);
        fetch(&archive)?;
        verify(&archive, expected)?;
        let binary = if Path::new(asset).extension() == Some(OsStr::new("tgz")) {
            let status = Command::new("/usr/bin/tar")
                .arg("-xzf")
                .arg(&archive)
                .arg("-C")
                .arg(&tmp)
                .arg("cloudflared")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if !status.success() {
                return Err(io::Error::other("cloudflared 압축 해제 실패"));
            }
            tmp.join("cloudflared")
        } else {
            archive
        };
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
        let target = bin.join("cloudflared");
        fs::rename(binary, &target)?;
        Ok(target)
    })();
    fs::remove_dir_all(&tmp).ok();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mk-cf-{name}-{}",
            super::super::devices::hex(&super::super::e2ee::random().unwrap())
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn binary(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[test]
    fn resolution_order_and_executable_check() {
        let dir = temp("paths");
        let first = dir.join("first");
        let second = dir.join("second");
        binary(&first.join("cloudflared"));
        binary(&second.join("cloudflared"));
        binary(&dir.join("bin/cloudflared"));
        let path = std::env::join_paths([&first, &second]).unwrap();
        assert_eq!(
            find_binary(Some(OsStr::new("/explicit")), Some(&path), Some(&dir)),
            Some("/explicit".into())
        );
        assert_eq!(
            find_binary(None, Some(&path), Some(&dir)),
            Some(first.join("cloudflared"))
        );
        fs::set_permissions(first.join("cloudflared"), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            find_binary(None, Some(&path), Some(&dir)),
            Some(second.join("cloudflared"))
        );
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn standard_locations_precede_cached_binary() {
        let locations = [
            PathBuf::from("/opt/homebrew/bin/cloudflared"),
            PathBuf::from("/usr/local/bin/cloudflared"),
            PathBuf::from("/state/bin/cloudflared"),
        ];
        for first in 0..locations.len() {
            let found = find_with(None, None, Some(Path::new("/state")), |p| {
                locations.iter().skip(first).any(|v| v == p)
            });
            assert_eq!(found, locations.get(first).cloned());
        }
        assert!(find_with(None, None, None, |_| false).is_none());
    }
    #[test]
    fn digest_mismatch_is_deleted_and_release_table_is_complete() {
        let dir = temp("sha");
        let file = dir.join("download");
        fs::write(&file, b"tampered").unwrap();
        assert!(verify(&file, &"0".repeat(64)).is_err());
        assert!(!file.exists());
        fs::write(&file, b"abc").unwrap();
        verify(
            &file,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )
        .unwrap();
        for os in ["macos", "linux"] {
            for arch in ["aarch64", "x86_64"] {
                assert_eq!(release(os, arch).unwrap().1.len(), 64);
            }
        }
        assert!(release("windows", "x86_64").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn fake_download_installs_archive_and_raw_binary_atomically() {
        let dir = temp("install");
        let source = dir.join("source");
        binary(&source.join("cloudflared"));
        let archive = dir.join("fixture.tgz");
        assert!(
            Command::new("/usr/bin/tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(&source)
                .arg("cloudflared")
                .status()
                .unwrap()
                .success()
        );
        for asset in ["cloudflared-darwin-arm64.tgz", "cloudflared-linux-arm64"] {
            let input = if Path::new(asset).extension() == Some(OsStr::new("tgz")) {
                archive.clone()
            } else {
                source.join("cloudflared")
            };
            let bytes = fs::read(&input).unwrap();
            let hash = super::super::devices::hex(digest::digest(&digest::SHA256, &bytes).as_ref());
            let target =
                install_with(&dir, asset, &hash, |out| fs::copy(&input, out).map(|_| ())).unwrap();
            assert_eq!(
                fs::read(&target).unwrap(),
                fs::read(source.join("cloudflared")).unwrap()
            );
            assert_eq!(
                fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert_eq!(
                fs::metadata(dir.join("bin")).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert!(
                install_with(&dir, asset, &"0".repeat(64), |out| fs::write(out, b"wrong")).is_err()
            );
            assert!(target.exists());
            assert_eq!(fs::read_dir(dir.join("bin")).unwrap().count(), 1);
        }
        fs::remove_dir_all(dir).unwrap();
    }
    static STARTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn fake(dir: &Path) -> io::Result<PathBuf> {
        STARTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(100));
        fs::metadata(dir)?;
        Ok(dir.join("bin/cloudflared"))
    }
    #[test]
    fn downloading_is_nonblocking_and_single_flight() {
        let resolver = Resolver::default();
        let dir = temp("async");
        assert_eq!(
            resolver.acquire(&dir, fake).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(resolver.downloading());
        for _ in 0..5 {
            assert_eq!(
                resolver.acquire(&dir, fake).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while resolver.downloading() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            resolver.acquire(&dir, fake).unwrap(),
            dir.join("bin/cloudflared")
        );
        assert_eq!(STARTS.load(std::sync::atomic::Ordering::SeqCst), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}
