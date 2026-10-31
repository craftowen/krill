use super::*;
use crate::store::{Limits, Store, tests::SEGMENT_V1};
use std::io::{Cursor, Write as _};

#[test]
fn oversized_chunk_length_is_rejected_before_reading_its_body() {
    let key = Key::from_slice(&[7; 32]).unwrap();
    for encrypted in [false, true] {
        let mut header = Header::decode(SEGMENT_V1).unwrap();
        header.encrypted = encrypted;
        let mut prefix = header.encode().to_vec();
        if encrypted {
            prefix.extend([0; SALT_LEN]);
        }
        prefix.extend(u32::MAX.to_le_bytes());
        prefix.extend(0_u32.to_le_bytes());
        let mut reader = Cursor::new(prefix);
        let declared_file_len = reader.get_ref().len() as u64 + u64::from(u32::MAX);
        let error = read_from(&mut reader, declared_file_len, encrypted.then_some(&key))
            .map(|_| ())
            .expect_err("an oversized length must fail before allocating or reading the body");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(reader.position(), reader.get_ref().len() as u64);
    }
}

#[test]
fn compressed_limit_counts_all_chunks_and_does_not_consume_excess_body() {
    let mut bytes = SEGMENT_V1.get(..HEADER_LEN).unwrap().to_vec();
    // A CRC-valid body at the exact limit must reach the decoder, not fail the size check.
    bytes.extend(chunk(&vec![0; MAX_STREAM]));
    let segment = decode(&bytes, None).unwrap().unwrap();
    assert_eq!(segment.valid_len, bytes.len() as u64);
    assert!(!segment.sealed);
    assert!(segment.records.is_empty());
    bytes.extend(chunk(&[1]));
    let expected_read = bytes.len() as u64 - 1;
    let mut reader = Cursor::new(bytes);
    let file_len = reader.get_ref().len() as u64;
    let error = read_from(&mut reader, file_len, None)
        .map(|_| ())
        .expect_err("a subsequent chunk must not bypass the aggregate stream limit");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(reader.position(), expected_read);
}

#[test]
fn oversized_sparse_file_is_preserved_and_recovery_can_retry() {
    let root =
        std::env::temp_dir().join(format!("krill-segment-input-limit-{}", std::process::id()));
    let store = Store::with_key(root.join("sessions"), Limits::default(), true, false, None);
    let dir = store.session_dir(SessionId(42));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("seg-00000000000000000000.krs");
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(SEGMENT_V1).unwrap();
    file.set_len(MAX_FILE + 1).unwrap();
    drop(file);
    for _ in 0..2 {
        let error = store
            .recorder(SessionId(42), WinSize { cols: 80, rows: 24 })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::metadata(&path).unwrap().len(), MAX_FILE + 1);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let mut prefix = [0; HEADER_LEN];
        fs::File::open(&path)
            .unwrap()
            .read_exact(&mut prefix)
            .unwrap();
        assert_eq!(prefix.as_slice(), SEGMENT_V1.get(..HEADER_LEN).unwrap());
    }
    fs::write(&path, SEGMENT_V1).unwrap();
    let recorder = store
        .recorder(SessionId(42), WinSize { cols: 80, rows: 24 })
        .unwrap();
    assert_eq!(recorder.seq(), 12);
    drop(recorder);
    fs::remove_dir_all(root).unwrap();
}

struct Failing;

impl Read for Failing {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::ErrorKind::PermissionDenied.into())
    }
}

#[test]
fn io_failure_is_not_recovered_as_a_truncated_tail() {
    // The reader fails in the header, a chunk header, or a chunk body.
    for cut in [
        0,
        10,
        HEADER_LEN,
        HEADER_LEN + CHUNK_HEAD,
        SEGMENT_V1.len() - 1,
    ] {
        let reader = SEGMENT_V1.get(..cut).unwrap().chain(Failing);
        let error = read_from(reader, SEGMENT_V1.len() as u64, None)
            .map(|_| ())
            .expect_err("read failures must preserve the original rather than trim its tail");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}

struct Fragmented<'a> {
    bytes: &'a [u8],
    interrupted: bool,
}

impl Read for Fragmented<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.interrupted = !self.interrupted;
        if self.interrupted {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let len = bytes.len().min(1);
        self.bytes
            .read(bytes.get_mut(..len).ok_or(io::ErrorKind::InvalidInput)?)
    }
}

#[test]
fn fragmented_and_interrupted_reads_preserve_the_fixed_fixture() {
    let reader = Fragmented {
        bytes: SEGMENT_V1,
        interrupted: false,
    };
    let segment = read_from(reader, SEGMENT_V1.len() as u64, None)
        .unwrap()
        .unwrap();
    assert_eq!(segment.valid_len, SEGMENT_V1.len() as u64);
    assert_eq!(segment.end_seq(), 12);
    assert!(segment.sealed);
}

#[test]
fn a_growing_input_is_only_read_to_the_snapshot_boundary() {
    let mut bytes = SEGMENT_V1.to_vec();
    bytes.extend(chunk(&vec![0; 1024]));
    let mut reader = Cursor::new(bytes);
    let segment = read_from(&mut reader, SEGMENT_V1.len() as u64, None)
        .unwrap()
        .unwrap();
    assert_eq!(reader.position(), SEGMENT_V1.len() as u64);
    assert_eq!(segment.valid_len, SEGMENT_V1.len() as u64);
    assert_eq!(segment.end_seq(), 12);
    assert!(segment.sealed);
}
