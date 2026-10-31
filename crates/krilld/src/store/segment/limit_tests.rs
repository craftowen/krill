use super::*;
use crate::store::{Limits, Store};
use std::io::Write as _;
use std::sync::Arc;

const ID: SessionId = SessionId(42);
const SIZE: WinSize = WinSize { cols: 80, rows: 24 };

#[expect(clippy::unwrap_used, reason = "test fixture encoding")]
fn fixture(extra: usize, sealed: bool, key: Option<&Key>) -> Vec<u8> {
    // The first complete record ends exactly at the decoder's raw byte limit.
    let mut raw = Vec::new();
    Record::Data(vec![b'x'; usize::try_from(MAX_RAW).unwrap() - 5]).encode_into(&mut raw);
    let mut suffix = Vec::new();
    Record::Data(vec![b'y']).encode_into(&mut suffix);
    raw.extend_from_slice(suffix.get(..extra).unwrap());
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
    encoder.write_all(&raw).unwrap();
    let mut compressed = if sealed {
        encoder.finish().unwrap()
    } else {
        encoder.flush().unwrap();
        encoder.get_ref().clone()
    };
    let mut bytes = Header {
        base: Base::Start,
        encrypted: key.is_some(),
        session: ID,
        start_seq: 0,
        size: SIZE,
    }
    .encode()
    .to_vec();
    if let Some(key) = key {
        let (mut crypto, salt) = Stream::create(key, &bytes).unwrap();
        bytes.extend_from_slice(&salt);
        crypto.seal(&mut compressed).unwrap();
    }
    bytes.extend(chunk(&compressed));
    bytes
}

#[test]
fn raw_limit_accepts_exactly_bounded_sealed_and_unsealed_segments() {
    let key = Key::from_slice(&[7; 32]).unwrap();
    for key in [None, Some(&key)] {
        for sealed in [false, true] {
            let bytes = fixture(0, sealed, key);
            let segment = decode(&bytes, key).unwrap().unwrap();
            assert_eq!(segment.end_seq(), MAX_RAW - 5);
            assert_eq!(segment.sealed, sealed);
            assert_eq!(segment.valid_len, bytes.len() as u64);
        }
    }
}

#[test]
fn raw_limit_rejects_excess_even_after_a_complete_record() {
    let key = Key::from_slice(&[7; 32]).unwrap();
    for key in [None, Some(&key)] {
        for sealed in [false, true] {
            for extra in [1, 6] {
                let error = decode(&fixture(extra, sealed, key), key)
                    .map(|_| ())
                    .expect_err("oversized stream must be rejected");
                assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            }
        }
    }
}

#[test]
fn raw_limit_recovery_preserves_original_file_and_retries_after_repair() {
    for encrypted in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "krill-segment-raw-limit-{}-{encrypted}",
            std::process::id()
        ));
        let key = encrypted.then(|| Arc::new(Key::from_slice(&[7; 32]).unwrap()));
        let store = Store::with_key(
            root.join("sessions"),
            Limits::default(),
            true,
            encrypted,
            key.clone(),
        );
        let dir = store.session_dir(ID);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seg-00000000000000000000.krs");
        let mut original = fixture(6, true, key.as_deref());
        original.extend_from_slice(b"torn tail");
        fs::write(&path, &original).unwrap();
        for _ in 0..2 {
            let error = store.recorder(ID, SIZE).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read(&path).unwrap(), original);
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        }
        // Replacing the bad input with a readable file must permit a fresh open.
        fs::write(&path, fixture(0, true, key.as_deref())).unwrap();
        let mut recorder = store.recorder(ID, SIZE).unwrap();
        assert_eq!(recorder.seq(), MAX_RAW - 5);
        recorder.append(b"after repair");
        recorder.seal();
        let paths = crate::store::segments(&dir).unwrap();
        assert_eq!(paths.len(), 2);
        let next = read(&paths[1], key.as_deref()).unwrap().unwrap();
        assert_eq!(next.header.start_seq, MAX_RAW - 5);
        assert_eq!(next.records, vec![Record::Data(b"after repair".to_vec())]);
        drop(recorder);
        fs::remove_dir_all(root).unwrap();
    }
}
