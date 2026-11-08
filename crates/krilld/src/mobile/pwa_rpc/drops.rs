//! Krill Drop mobile actions. Layout, ids and limits come from `krill_proto::drop`; every reply is
//! derived from what is on disk, so a phone may repeat any call after a dropped connection.
use super::{decode_base64, invalid};
use crate::os::open_no_follow;
use krill_proto::drop::{self, CHUNK_BYTES, MAX_DROP_BYTES};
use krill_proto::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) struct Chunk<'a> {
    pub upload: &'a str,
    pub name: &'a str,
    pub size: u64,
    pub offset: u64,
    pub data: &'a str,
}

/// Append `chunk` when it starts at the bytes already received; otherwise only report them.
pub(super) fn put(state: &Path, inbox: &Path, chunk: &Chunk<'_>) -> io::Result<Value> {
    if !drop::is_id(chunk.upload) {
        return Err(invalid());
    }
    if chunk.size > MAX_DROP_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "File is larger than 2 GiB",
        ));
    }
    let name = drop::sanitize_name(chunk.name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid file name"))?;
    let bytes = decode_base64(chunk.data)?;
    let len = u64::try_from(bytes.len()).map_err(|_| invalid())?;
    if bytes.len() > CHUNK_BYTES
        || chunk
            .offset
            .checked_add(len)
            .is_none_or(|end| end > chunk.size)
    {
        return Err(invalid());
    }
    let done = drop::done_path(state, chunk.upload);
    if let Ok(saved) = fs::read_to_string(&done) {
        return Ok(json!({"received":chunk.size,"saved":saved}));
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(drop::incoming_dir(state))?;
    let partial = drop::partial_path(state, chunk.upload);
    let mut file = open_no_follow(
        OpenOptions::new().create(true).append(true).mode(0o600),
        &partial,
    )?;
    let mut received = file.metadata()?.len();
    if received > chunk.size {
        return Err(invalid());
    }
    if chunk.offset == received && len > 0 {
        file.write_all(&bytes)?;
        received += len;
    }
    if received < chunk.size {
        return Ok(json!({"received":received}));
    }
    file.sync_all()?;
    let saved = publish(&partial, inbox, &name)?;
    let saved = saved.to_string_lossy();
    open_no_follow(
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600),
        &done,
    )?
    .write_all(saved.as_bytes())?;
    fs::remove_file(&partial)?;
    Ok(json!({"received":chunk.size,"saved":saved}))
}

/// Give the finished upload its first free inbox name. Hard links never replace an existing file.
fn publish(partial: &Path, inbox: &Path, name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(inbox)?;
    for n in 0..10_000 {
        let target = inbox.join(drop::numbered_name(name, n));
        let linked = fs::hard_link(partial, &target).or_else(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                Err(error)
            } else {
                copy_new(partial, &target)
            }
        });
        match linked {
            Ok(()) => return Ok(target),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

/// Fallback for volumes without hard links (another disk, exFAT).
fn copy_new(from: &Path, to: &Path) -> io::Result<()> {
    let mut out = OpenOptions::new().write(true).create_new(true).open(to)?;
    let copied = io::copy(&mut File::open(from)?, &mut out).and_then(|_| out.sync_all());
    if copied.is_err() {
        fs::remove_file(to).ok();
    }
    copied
}

pub(super) fn list(state: &Path, inbox: &Path) -> io::Result<Value> {
    drop::prune(state, SystemTime::now());
    let items: Vec<Value> = drop::list(state)?
        .into_iter()
        .map(|item| {
            let ms = item
                .added
                .duration_since(UNIX_EPOCH)
                .map_or(0, |at| u64::try_from(at.as_millis()).unwrap_or(u64::MAX));
            json!({"id":item.id,"name":item.name,"size":item.size,"modified_unix_ms":ms})
        })
        .collect();
    Ok(json!({"items":items,"inbox":inbox.to_string_lossy()}))
}

pub(super) fn get(state: &Path, id: &str, offset: u64) -> io::Result<Value> {
    let item = drop::item(state, id)?.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "This file is no longer on the PC")
    })?;
    if offset > item.size {
        return Err(invalid());
    }
    let mut file = open_no_follow(OpenOptions::new().read(true), &item.path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.take(CHUNK_BYTES as u64).read_to_end(&mut bytes)?;
    let end = offset.saturating_add(bytes.len() as u64);
    Ok(json!({
        "data":super::super::ws::base64(&bytes, false),
        "size":item.size,
        "eof":bytes.len() < CHUNK_BYTES || end >= item.size,
    }))
}

pub(super) fn remove(state: &Path, id: &str) -> io::Result<Value> {
    drop::remove(state, id)?;
    Ok(json!({"removed":true}))
}

#[cfg(test)]
mod tests {
    use super::super::Backend;
    use super::*;
    use crate::mobile::ws::base64;
    use krill_proto::ipc::{self, ServerHello};
    use std::sync::Weak;

    struct Fixture {
        root: PathBuf,
        state: PathBuf,
        inbox: PathBuf,
        backend: Backend,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).ok();
        }
    }
    fn fixture() -> Fixture {
        let root = std::env::temp_dir().join(format!("krill-drop-rpc-{}", drop::new_id().unwrap()));
        let state = root.join("state");
        let inbox = drop::inbox_dir(&root.join("home"));
        let mut backend = Backend::new(
            Weak::new(),
            Some(&state),
            ServerHello {
                proto_min: 1,
                proto_max: 1,
                features: ipc::FEATURES,
                build: "test".into(),
                instance_id: 1,
            },
        );
        backend.drop_inbox = Some(inbox.clone());
        Fixture {
            root,
            state,
            inbox,
            backend,
        }
    }
    impl Fixture {
        fn rpc(&self, request: &Value) -> Result<Value, String> {
            let reply: Value =
                serde_json::from_str(&self.backend.request(&request.to_string())).unwrap();
            if reply["ok"] == true {
                Ok(reply["data"].clone())
            } else {
                Err(reply["error"].as_str().unwrap_or_default().to_owned())
            }
        }
        fn put(&self, upload: &str, name: &str, all: &[u8], offset: usize, end: usize) -> Value {
            self.rpc(
                &json!({"action":"drop_put","upload":upload,"name":name,"size":all.len(),
                "offset":offset,"data":base64(&all[offset..end], false)}),
            )
            .unwrap()
        }
    }
    fn bytes(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| u8::try_from(i * 7 % 251).unwrap())
            .collect()
    }

    #[test]
    fn upload_resumes_from_the_received_offset_and_never_overwrites() {
        let f = fixture();
        let file = bytes(CHUNK_BYTES + 1000);
        let upload = drop::new_id().unwrap();
        let first = f.put(&upload, "../../report.pdf", &file, 0, CHUNK_BYTES);
        assert_eq!(first, json!({"received":CHUNK_BYTES}));
        // A resend of an applied chunk and a chunk past the end only report progress.
        assert_eq!(f.put(&upload, "report.pdf", &file, 0, 10), first);
        assert_eq!(
            f.put(
                &upload,
                "report.pdf",
                &file,
                CHUNK_BYTES + 10,
                CHUNK_BYTES + 20
            ),
            first
        );
        let done = f.put(&upload, "report.pdf", &file, CHUNK_BYTES, file.len());
        let saved = f.inbox.join("report.pdf");
        assert_eq!(done["received"], file.len());
        assert_eq!(done["saved"], saved.to_string_lossy().as_ref());
        assert_eq!(fs::read(&saved).unwrap(), file);
        // The final chunk retried after a lost reply answers the same result.
        assert_eq!(
            f.put(&upload, "report.pdf", &file, CHUNK_BYTES, file.len()),
            done
        );
        let again = f.put(&drop::new_id().unwrap(), "report.pdf", b"other", 0, 5);
        assert_eq!(fs::read(f.inbox.join("report (1).pdf")).unwrap(), b"other");
        assert_eq!(
            again["saved"],
            f.inbox.join("report (1).pdf").to_string_lossy().as_ref()
        );
        assert_eq!(fs::read(&saved).unwrap(), file);
        let empty = f.put(&drop::new_id().unwrap(), "empty.txt", b"", 0, 0);
        assert_eq!(fs::read(f.inbox.join("empty.txt")).unwrap(), b"");
        assert_eq!(empty["received"], 0);
        assert_eq!(
            fs::read_dir(drop::incoming_dir(&f.state))
                .unwrap()
                .filter(|e| {
                    e.as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".partial")
                })
                .count(),
            0
        );
    }

    #[test]
    fn upload_rejects_bad_ids_names_sizes_and_chunks() {
        let f = fixture();
        let id = drop::new_id().unwrap();
        for request in [
            json!({"upload":"../x","name":"a","size":1,"offset":0,"data":"AA=="}),
            json!({"upload":id.to_uppercase(),"name":"a","size":1,"offset":0,"data":"AA=="}),
            json!({"upload":id,"name":"..","size":1,"offset":0,"data":"AA=="}),
            json!({"upload":id,"name":"a","size":MAX_DROP_BYTES + 1,"offset":0,"data":""}),
            json!({"upload":id,"name":"a","size":1,"offset":0,"data":"AAA="}),
            json!({"upload":id,"name":"a","size":u64::MAX,"offset":u64::MAX,"data":"AA=="}),
            json!({"upload":id,"name":"a","size":CHUNK_BYTES + 1,"offset":0,
                "data":base64(&bytes(CHUNK_BYTES + 1), false)}),
        ] {
            let mut request = request;
            request["action"] = "drop_put".into();
            assert!(f.rpc(&request).is_err(), "{request}");
        }
        assert!(!f.inbox.exists());
    }

    #[test]
    fn outbox_lists_streams_and_removes_by_id_only() {
        let f = fixture();
        let file = bytes(CHUNK_BYTES * 2);
        let source = f.root.join("result.bin");
        fs::create_dir_all(&f.root).unwrap();
        fs::write(&source, &file).unwrap();
        let item = drop::add(&f.state, &source).unwrap();
        let listed = f.rpc(&json!({"action":"drop_list"})).unwrap();
        assert_eq!(listed["inbox"], f.inbox.to_string_lossy().as_ref());
        assert_eq!(listed["items"].as_array().unwrap().len(), 1);
        assert_eq!(listed["items"][0]["id"], item.id);
        assert_eq!(listed["items"][0]["name"], "result.bin");
        assert_eq!(listed["items"][0]["size"], file.len());
        let mut got = Vec::new();
        loop {
            let chunk = f
                .rpc(&json!({"action":"drop_get","id":item.id,"offset":got.len()}))
                .unwrap();
            got.extend(decode_base64(chunk["data"].as_str().unwrap()).unwrap());
            if chunk["eof"] == true {
                break;
            }
        }
        assert_eq!(got, file);
        for id in ["../../state", ".tmp-x", "", &item.id.to_uppercase()] {
            assert!(
                f.rpc(&json!({"action":"drop_get","id":id,"offset":0}))
                    .is_err()
            );
            assert!(f.rpc(&json!({"action":"drop_remove","id":id})).is_err());
        }
        assert!(
            f.rpc(&json!({"action":"drop_get","id":item.id,"offset":file.len() + 1}))
                .is_err()
        );
        assert!(
            f.rpc(&json!({"action":"drop_get","id":item.id,"path":"/etc/passwd","offset":0}))
                .is_err()
        );
        for _ in 0..2 {
            assert_eq!(
                f.rpc(&json!({"action":"drop_remove","id":item.id}))
                    .unwrap(),
                json!({"removed":true})
            );
        }
        assert_eq!(
            f.rpc(&json!({"action":"drop_list"})).unwrap()["items"],
            json!([])
        );
        assert!(source.exists());
    }

    #[test]
    #[cfg(unix)]
    fn listing_prunes_expired_items_and_stale_uploads() {
        let f = fixture();
        let source = f.root.join("old.txt");
        fs::create_dir_all(&f.root).unwrap();
        fs::write(&source, b"old").unwrap();
        let old = drop::add(&f.state, &source).unwrap();
        let fresh = drop::add(&f.state, &source).unwrap();
        let upload = drop::new_id().unwrap();
        f.put(&upload, "half.bin", &bytes(10), 0, 5);
        let partial = drop::partial_path(&f.state, &upload);
        let age = |path: &Path, ttl: std::time::Duration| {
            File::open(path)
                .unwrap()
                .set_modified(SystemTime::now() - ttl - std::time::Duration::from_secs(60))
                .unwrap();
        };
        age(old.path.parent().unwrap(), drop::OUTBOX_TTL);
        age(&partial, drop::PARTIAL_TTL);
        let listed = f.rpc(&json!({"action":"drop_list"})).unwrap();
        assert_eq!(listed["items"].as_array().unwrap().len(), 1);
        assert_eq!(listed["items"][0]["id"], fresh.id);
        assert!(!old.path.exists());
        assert!(!partial.exists());
    }

    #[test]
    fn snapshot_advertises_drop() {
        let mut f = fixture();
        let registry = std::sync::Arc::new(crate::registry::Registry::new(None, None));
        f.backend.registry = std::sync::Arc::downgrade(&registry);
        let snapshot = f.rpc(&json!({"action":"snapshot"})).unwrap();
        assert_eq!(snapshot["capabilities"]["drop"], true);
    }
}
