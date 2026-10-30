use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::Path;

use krill_proto::ipc::MachineIdentity;
use krill_proto::os::fs::OpenOptionsExt as _;
use ring::rand::{SecureRandom as _, SystemRandom};

use super::with_suffix;

pub(super) fn load(socket: &Path) -> io::Result<MachineIdentity> {
    let path = with_suffix(socket, ".machine-id");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(with_suffix(socket, ".machine-id.lock"))?;
    lock.lock()?;
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => {
            let mut bytes = Vec::with_capacity(17);
            File::open(&path)?.take(17).read_to_end(&mut bytes)?;
            let bytes = bytes.try_into().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid machine identity length",
                )
            })?;
            return Ok(MachineIdentity(bytes));
        }
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "machine identity is not a regular file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut bytes = [0; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("machine identity randomness unavailable"))?;
    let temp = with_suffix(&path, &format!(".{}.tmp", u128::from_le_bytes(bytes)));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(MachineIdentity(bytes))
    })();
    if result.is_err() {
        fs::remove_file(&temp).ok();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    struct Directory(std::path::PathBuf);

    impl Directory {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    fn directory(tag: &str) -> io::Result<Directory> {
        let dir = std::env::temp_dir().join(format!("krill-{tag}-{}", std::process::id()));
        fs::create_dir(&dir)?;
        Ok(Directory(dir))
    }

    #[test]
    fn restart_retains_identity_and_distinct_socket_has_its_own() {
        let dir = directory("machine-restart").unwrap();
        let socket = dir.path().join("daemon.sock");
        let first = load(&socket).unwrap();
        assert_eq!(load(&socket).unwrap(), first);
        assert_ne!(load(&dir.path().join("other.sock")).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(with_suffix(&socket, ".machine-id"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn corrupt_existing_identity_is_preserved() {
        let dir = directory("machine-corrupt").unwrap();
        let socket = dir.path().join("daemon.sock");
        let path = with_suffix(&socket, ".machine-id");
        fs::write(&path, b"damaged").unwrap();
        assert_eq!(
            load(&socket).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(fs::read(&path).unwrap(), b"damaged");
    }

    #[test]
    fn concurrent_creation_publishes_one_complete_identity() {
        let dir = directory("machine-concurrent").unwrap();
        let socket = dir.path().join("daemon.sock");
        let barrier = Arc::new(Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let socket = socket.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    load(&socket).unwrap()
                })
            })
            .collect();
        let ids: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        let first = *ids.first().unwrap();
        assert!(ids.iter().all(|id| *id == first));
        assert_eq!(load(&socket).unwrap(), first);
    }

    #[test]
    fn unavailable_directory_does_not_create_an_identity_elsewhere() {
        let dir = directory("machine-unavailable").unwrap();
        let socket = dir.path().join("missing/daemon.sock");
        assert_eq!(load(&socket).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!dir.path().join("daemon.sock.machine-id").exists());
    }
}
