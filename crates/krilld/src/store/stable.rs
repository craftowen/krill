//! Bounded metadata for one frozen attach view; decoded source rows are never retained here.
use super::{History, Store, fs, io, no_theme, start_of};
use krill_proto::SessionId;
use krill_proto::screen::{Cell, HistoryIdentity, Seq, Source};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

pub(crate) const MAX_SOURCES: usize = 4096;
const MAX_PATH_BYTES: usize = 512 * 1024;

#[derive(Clone)]
struct FileStamp {
    len: u64,
    created: Option<SystemTime>,
    digest: [u8; 32],
}

pub(crate) struct Manifest {
    history: History,
    files: Vec<FileStamp>,
    cutoff: u64,
    session: SessionId,
}

pub(crate) struct Range {
    pub identity: HistoryIdentity,
    pub total: usize,
    pub covered: usize,
    pub rows: Vec<Vec<Cell>>,
}

impl Store {
    pub(crate) fn stable_manifest(
        &self,
        id: SessionId,
        cutoff: u64,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Manifest> {
        let mut paths = Vec::new();
        let mut bytes = 0usize;
        let cleared_seq = self.cleared_seq(id);
        // No frozen history can precede the clear boundary. An empty new
        // view must not try to replay an entirely cleared segment basis.
        if self.recording && cleared_seq < cutoff {
            match fs::read_dir(self.session_dir(id)) {
                Ok(entries) => {
                    for entry in entries {
                        if cancelled() {
                            return Err(io::ErrorKind::Interrupted.into());
                        }
                        let path = entry?.path();
                        if path.extension().is_none_or(|ext| ext != "krs")
                            || start_of(&path).is_none()
                        {
                            continue;
                        }
                        bytes = bytes.saturating_add(path.as_os_str().as_encoded_bytes().len());
                        if paths.len() >= MAX_SOURCES || bytes > MAX_PATH_BYTES {
                            return Err(io::ErrorKind::OutOfMemory.into());
                        }
                        if start_of(&path).is_some_and(|start| start < cutoff) {
                            paths.push(path);
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        paths.sort();
        let mut first_page = 0usize;
        if cleared_seq > 0 {
            while let Some(path) = paths.get(first_page) {
                if cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if paths
                    .get(first_page + 1)
                    .and_then(|path| start_of(path))
                    .is_some_and(|next| next <= cleared_seq)
                {
                    first_page += 1;
                    continue;
                }
                if let Some(segment) = super::segment::read(path, self.key.as_deref())?
                    && segment.end_seq() <= cleared_seq
                {
                    first_page += 1;
                    continue;
                }
                break;
            }
        }
        let history = History {
            segs: paths,
            first_page,
            cleared_seq,
            key: self.key.clone(),
        };
        let files = history
            .segs
            .iter()
            .map(|p| {
                if cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let m = fs::metadata(p)?;
                Ok(FileStamp {
                    len: m.len(),
                    created: m.created().ok(),
                    digest: prefix_digest(p, m.len(), cancelled)?,
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Manifest {
            history,
            files,
            cutoff,
            session: id,
        })
    }

    /// This barrier is independent of recorder creation/sealing. Waiting readers can cancel.
    pub(crate) fn with_history_epoch_cancelled<T>(
        &self,
        id: SessionId,
        cancelled: &dyn Fn() -> bool,
        f: impl FnOnce(u64, u64) -> T,
    ) -> io::Result<T> {
        let _publication = loop {
            if cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            match self.history_publication.try_read() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        };
        Ok(f(
            self.generation.load(Ordering::Acquire),
            self.cleared_seq(id),
        ))
    }
    #[cfg(all(test, unix))]
    pub(crate) fn with_history_epoch<T>(&self, id: SessionId, f: impl FnOnce(u64, u64) -> T) -> T {
        let _publication = self
            .history_publication
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(
            self.generation.load(Ordering::Acquire),
            self.cleared_seq(id),
        )
    }
}

pub(crate) struct IndexEntry {
    pub identity: HistoryIdentity,
    pub total: usize,
    pub covered: usize,
}
fn prefix_digest(path: &Path, len: u64, cancelled: &dyn Fn() -> bool) -> io::Result<[u8; 32]> {
    let mut file = fs::File::open(path)?;
    let mut remaining = len;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    while remaining > 0 {
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let take = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let n = file.read(buffer.get_mut(..take).ok_or(io::ErrorKind::InvalidData)?)?;
        if n == 0 {
            return Err(io::ErrorKind::NotFound.into());
        }
        hash.update(buffer.get(..n).ok_or(io::ErrorKind::InvalidData)?);
        remaining = remaining.saturating_sub(u64::try_from(n).unwrap_or(u64::MAX));
    }
    Ok(hash.finalize().into())
}
impl Manifest {
    pub(crate) fn index(
        &self,
        hot: &[Box<[u8]>],
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Vec<IndexEntry>> {
        let _permit = super::replay_gate::acquire(cancelled).ok_or(io::ErrorKind::Interrupted)?;
        self.validate(cancelled)?;
        let mut entries = (0..self.len())
            .map(|page| IndexEntry {
                identity: HistoryIdentity {
                    session: self.session,
                    source: Source::Cold {
                        page: u32::try_from(page).unwrap_or(u32::MAX),
                    },
                    start_seq: Seq(self.history.cleared_seq),
                    end_seq: Seq(self.history.cleared_seq),
                    digest: [0; 32],
                },
                total: 0,
                covered: 0,
            })
            .collect::<Vec<_>>();
        let mut hash = Sha256::new();
        hash.update(self.history.cleared_seq.to_le_bytes());
        let mut total = 0usize;
        let mut tail = VecDeque::new();
        let mut failed = false;
        self.history
            .visit_until(self.cutoff, cancelled, |page, event| match event {
                super::HistoryEvent::Rows(batch) => {
                    for row in batch {
                        match postcard::to_stdvec(&row) {
                            Ok(bytes) => {
                                hash.update(
                                    u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes(),
                                );
                                hash.update(&bytes);
                                if !hot.is_empty() {
                                    tail.push_back((page, bytes.into_boxed_slice()));
                                    if tail.len() > hot.len() {
                                        tail.pop_front();
                                    }
                                }
                            }
                            Err(_) => failed = true,
                        }
                        total = total.saturating_add(1);
                    }
                }
                super::HistoryEvent::End(result) => {
                    if let Ok(source) = result {
                        if !source.gaps.is_empty() {
                            failed = true;
                        }
                        if let Some(entry) = entries.get_mut(page) {
                            entry.total = total;
                            entry.identity.start_seq = Seq(source.start_seq);
                            entry.identity.end_seq = Seq(source.end_seq);
                            entry.identity.digest = std::mem::take(&mut hash).finalize().into();
                        } else {
                            failed = true;
                        }
                    } else {
                        failed = true;
                    }
                    total = 0;
                    hash = Sha256::new();
                    hash.update(self.history.cleared_seq.to_le_bytes());
                }
            });
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if failed {
            return Err(io::ErrorKind::NotFound.into());
        }
        for ((page, row), expected) in tail.iter().rev().zip(hot) {
            if row != expected {
                return Err(io::ErrorKind::InvalidData.into());
            }
            if let Some(entry) = entries.get_mut(*page) {
                entry.covered = entry.covered.saturating_add(1);
            }
        }
        for entry in &mut entries {
            entry.total = entry.total.saturating_sub(entry.covered);
        }
        self.validate(cancelled)?;
        Ok(entries)
    }

    pub(crate) fn len(&self) -> usize {
        self.history.len()
    }

    fn validate(&self, cancelled: &dyn Fn() -> bool) -> io::Result<()> {
        // Losing a replay basis must never silently turn a cached source into a keyframe fallback.
        for (path, stamp) in self.history.segs.iter().zip(&self.files) {
            if cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let current = fs::metadata(path)?;
            if current.len() < stamp.len
                || current.created().ok() != stamp.created
                || prefix_digest(path, stamp.len, cancelled)? != stamp.digest
            {
                return Err(io::ErrorKind::NotFound.into());
            }
        }
        Ok(())
    }

    pub(crate) fn range(
        &self,
        page: usize,
        skip: usize,
        count: usize,
        hot: Option<&[Box<[u8]>]>,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<Range> {
        let _permit = super::replay_gate::acquire(cancelled).ok_or(io::ErrorKind::Interrupted)?;
        self.validate(cancelled)?;
        let raw = self.history.raw_index(page)?;
        let seg = self.history.read_raw(raw)?;
        if seg.header.session != self.session {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let look = no_theme();
        let mut engine = self.history.warm_checked(raw, &seg, &look, cancelled)?;
        engine.take_history();
        let mut total = 0usize;
        let mut hash = Sha256::new();
        let mut encoding_failed = false;
        hash.update(self.history.cleared_seq.to_le_bytes());
        // Verify an exact frozen hot suffix across every newest cold source, never a value-pattern guess.
        let hot = hot.unwrap_or_default();
        let mut tail = VecDeque::new();
        let (gaps, _) = super::replay_harvest_cancelled(
            &mut engine,
            &seg,
            &look,
            Some(&mut |batch: Vec<Vec<Cell>>| {
                for row in batch {
                    match postcard::to_stdvec(&row) {
                        Ok(bytes) => {
                            hash.update(
                                u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes(),
                            );
                            hash.update(&bytes);
                            if !hot.is_empty() {
                                tail.push_back(bytes.into_boxed_slice());
                                if tail.len() > hot.len() {
                                    tail.pop_front();
                                }
                            }
                        }
                        Err(_) => encoding_failed = true,
                    }
                    total = total.saturating_add(1);
                }
            }),
            Some(self.cutoff),
            self.history.cleared_seq,
            cancelled,
        );
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if encoding_failed {
            return Err(io::ErrorKind::InvalidData.into());
        }
        drop(engine);
        let digest: [u8; 32] = hash.finalize().into();
        if !gaps.is_empty() {
            return Err(io::ErrorKind::NotFound.into());
        }
        let covered = hot.len().min(total);
        if !tail.iter().rev().eq(hot.iter().take(covered)) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let total = total.saturating_sub(covered);
        let end = total.saturating_sub(skip);
        let start =
            end.saturating_sub(count.min(usize::from(krill_proto::screen::MAX_HISTORY_ROWS)));
        let mut rows = Vec::new();
        if start < end {
            let mut engine = self.history.warm_checked(raw, &seg, &look, cancelled)?;
            engine.take_history();
            let mut at = 0usize;
            super::replay_harvest_cancelled(
                &mut engine,
                &seg,
                &look,
                Some(&mut |batch: Vec<Vec<Cell>>| {
                    for row in batch {
                        if (start..end).contains(&at) {
                            rows.push(row);
                        }
                        at = at.saturating_add(1);
                    }
                }),
                Some(self.cutoff),
                self.history.cleared_seq,
                cancelled,
            );
        }
        self.validate(cancelled)?;
        rows.reverse();
        Ok(Range {
            identity: HistoryIdentity {
                session: self.session,
                source: Source::Cold {
                    page: u32::try_from(page).map_err(io::Error::other)?,
                },
                start_seq: Seq(seg.header.start_seq.max(self.history.cleared_seq)),
                end_seq: Seq(seg.end_seq().min(self.cutoff)),
                digest,
            },
            total,
            covered,
            rows,
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::Limits;
    use super::*;
    use krill_proto::holder::WinSize;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    #[expect(clippy::unwrap_used, reason = "isolated history test fixture setup")]
    fn fixture(tag: &str) -> (PathBuf, Arc<Store>, super::super::Recorder) {
        let path = std::env::temp_dir().join(format!("krill-stable-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&path).ok();
        let store = Store::open(
            &path,
            Limits {
                seal_bytes: 4096,
                ..Limits::default()
            },
        )
        .unwrap();
        let rec = store
            .recorder(SessionId(701), WinSize { cols: 80, rows: 24 })
            .unwrap();
        (path, store, rec)
    }
    #[expect(
        clippy::unwrap_used,
        reason = "writing to an in-memory test buffer cannot fail"
    )]
    fn output(from: usize, to: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        for n in from..to {
            writeln!(bytes, "stable-{n:06}\r").unwrap();
        }
        bytes
    }
    #[test]
    fn frozen_source_reloads_exact_rows_after_append_and_rejects_gc_ordinal_remap() {
        let (path, store, mut rec) = fixture("range");
        for n in 0..8 {
            rec.append(&output(n * 120, (n + 1) * 120));
            rec.seal();
        }
        let frozen = store
            .stable_manifest(SessionId(701), rec.seq(), &|| false)
            .unwrap();
        let first = frozen.range(0, 7, 11, None, &|| false).unwrap();
        let (total, mut expected) = store
            .history(SessionId(701))
            .unwrap()
            .rows(0, 7, 11)
            .unwrap();
        expected.reverse();
        assert_eq!(first.total, total);
        assert_eq!(first.rows, expected);
        rec.append(&output(960, 1100));
        rec.seal();
        let again = frozen.range(0, 7, 11, None, &|| false).unwrap();
        assert_eq!(first.identity, again.identity);
        assert_eq!(first.rows, again.rows);
        let removed = frozen.history.segs.first().unwrap().clone();
        fs::remove_file(removed).unwrap();
        assert!(
            frozen.range(0, 7, 11, None, &|| false).is_err(),
            "a fresh ordinal must never replace a lost frozen source/basis"
        );
        drop(rec);
        fs::remove_dir_all(path).ok();
    }
    #[test]
    fn clear_generation_and_truncated_source_invalidate_identity() {
        let (path, store, mut rec) = fixture("clear");
        rec.append(&output(0, 120));
        rec.seal();
        let frozen = store
            .stable_manifest(SessionId(701), rec.seq(), &|| false)
            .unwrap();
        let before =
            store.with_history_epoch(SessionId(701), |generation, clear| (generation, clear));
        store.clear_scrollback(SessionId(701), rec.seq());
        let after =
            store.with_history_epoch(SessionId(701), |generation, clear| (generation, clear));
        assert_ne!(before, after);
        let source = frozen.history.segs.first().unwrap();
        fs::write(source, []).unwrap();
        assert!(frozen.range(0, 0, 10, None, &|| false).is_err());
        store.clear().unwrap();
        let privacy =
            store.with_history_epoch(SessionId(701), |generation, clear| (generation, clear));
        assert_ne!(privacy.0, after.0);
        drop(rec);
        fs::remove_dir_all(path).ok();
    }
    #[test]
    fn metadata_overflow_is_explicit_without_truncated_manifest() {
        let (path, store, rec) = fixture("capacity");
        let dir = store.session_dir(SessionId(701));
        fs::create_dir_all(&dir).unwrap();
        for n in 0..=MAX_SOURCES {
            fs::write(dir.join(format!("seg-{n:020}.krs")), []).unwrap();
        }
        let result = store.stable_manifest(SessionId(701), u64::MAX, &|| false);
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::OutOfMemory);
        drop(rec);
        fs::remove_dir_all(path).ok();
    }

    #[test]
    fn clear_at_sealed_boundary_excludes_old_sources_after_new_output() {
        let (path, store, mut recorder) = fixture("clear-sealed-boundary");
        recorder.append(&output(0, 250));
        recorder.seal();
        store.clear_scrollback(SessionId(701), recorder.seq());
        recorder.append(&output(250, 500));
        recorder.seal();
        let frozen = store
            .stable_manifest(SessionId(701), recorder.seq(), &|| false)
            .unwrap();
        assert_eq!(frozen.len(), store.history(SessionId(701)).unwrap().len());
        let indexed = frozen.index(&[], &|| false).unwrap();
        assert!(indexed.iter().any(|entry| entry.total > 0));
        for (page, entry) in indexed.into_iter().enumerate() {
            let loaded = frozen.range(page, 0, 24, None, &|| false).unwrap();
            assert_eq!(entry.identity, loaded.identity);
            assert_eq!(entry.total, loaded.total);
        }
        fs::remove_dir_all(path).ok();
    }

    #[test]
    fn linear_index_exactly_deduplicates_repeated_hot_across_segments() {
        let (path, store, mut rec) = fixture("linear-index");
        for _ in 0..12 {
            rec.append(&b"A\r\n".repeat(200));
            rec.seal();
        }
        let frozen = store
            .stable_manifest(SessionId(701), rec.seq(), &|| false)
            .unwrap();
        let (plain, reads) =
            super::super::tests::counted_raw_reads(|| frozen.index(&[], &|| false).unwrap());
        assert!(
            reads <= frozen.len() + 1,
            "one index pass must read sources linearly: {reads} / {}",
            frozen.len()
        );
        let mut hot = VecDeque::new();
        {
            let _permit = super::super::replay_gate::acquire(&|| false).unwrap();
            frozen
                .history
                .visit_until(rec.seq(), &|| false, |_, event| {
                    if let super::super::HistoryEvent::Rows(batch) = event {
                        for row in batch {
                            hot.push_back(row);
                            if hot.len() > 1000 {
                                hot.pop_front();
                            }
                        }
                    }
                });
        }
        let hot = hot
            .into_iter()
            .rev()
            .map(|row| postcard::to_stdvec(&row).unwrap().into_boxed_slice())
            .collect::<Vec<_>>();
        assert_eq!(hot.len(), 1000);
        let indexed = frozen.index(&hot, &|| false).unwrap();
        assert_eq!(
            indexed.iter().map(|entry| entry.covered).sum::<usize>(),
            1000
        );
        assert_eq!(
            plain.iter().map(|entry| entry.total).sum::<usize>()
                - indexed.iter().map(|entry| entry.total).sum::<usize>(),
            1000
        );
        let last = indexed.len() - 1;
        assert_eq!(
            indexed[last].covered,
            plain[last].total.min(1000),
            "entire newest repeated-row segment is hot, not a pattern suffix guess"
        );
        let oldest = frozen.range(0, 0, 24, None, &|| false).unwrap();
        assert_eq!(oldest.identity, indexed[0].identity);
        let (_, deep_reads) = super::super::tests::counted_raw_reads(|| {
            frozen.range(last / 2, 0, 24, None, &|| false).unwrap()
        });
        assert!(
            deep_reads <= 4 * frozen.len() + 8,
            "one deep source reload must be linear, not a source-by-source prefix index: {deep_reads}"
        );
        drop(rec);
        fs::remove_dir_all(path).ok();
    }
    #[test]
    fn initial_prefix_hash_rejects_same_size_rewrite_before_first_decode_and_cancels() {
        use std::io::{Seek, SeekFrom};
        let (path, store, mut rec) = fixture("rewrite");
        rec.append(&output(0, 120));
        rec.seal();
        let frozen = store
            .stable_manifest(SessionId(701), rec.seq(), &|| false)
            .unwrap();
        assert_eq!(
            frozen.index(&[], &|| true).err().unwrap().kind(),
            io::ErrorKind::Interrupted
        );
        let source = frozen.history.segs.first().unwrap();
        let mut bytes = fs::read(source).unwrap();
        let byte = bytes.get_mut(40).unwrap();
        *byte ^= 1;
        let mut file = fs::OpenOptions::new().write(true).open(source).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&bytes).unwrap();
        drop(file);
        assert!(
            frozen.index(&[], &|| false).is_err(),
            "never bind first lazy source identity to a rewritten frozen file"
        );
        drop(rec);
        fs::remove_dir_all(path).ok();
    }
    #[test]
    fn slow_history_publication_and_waiting_privacy_writer_do_not_hold_recorder_gate() {
        use std::sync::mpsc;
        let (path, store, rec) = fixture("publication");
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let reader = Arc::clone(&store);
        let publishing = std::thread::spawn(move || {
            reader.with_history_epoch(SessionId(701), |_, _| {
                entered.send(()).unwrap();
                released.recv().unwrap();
            });
        });
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        let (begin, began) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let privacy = Arc::clone(&store);
        let clearing = std::thread::spawn(move || {
            begin.send(()).unwrap();
            privacy.clear().unwrap();
            done.send(()).unwrap();
        });
        began.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            store.gate.try_lock().is_ok(),
            "network publication never owns recorder gate"
        );
        let (recorded, progress) = mpsc::channel();
        let independent = Arc::clone(&store);
        let recorder = std::thread::spawn(move || {
            let mut other = independent
                .recorder(SessionId(702), WinSize { cols: 80, rows: 24 })
                .unwrap();
            other.append(b"independent\r\n");
            other.seal();
            recorded.send(()).unwrap();
        });
        progress.recv_timeout(Duration::from_secs(1)).unwrap();
        recorder.join().unwrap();
        assert!(
            finished.try_recv().is_err(),
            "privacy ACK waits until older publication completes"
        );
        release.send(()).unwrap();
        publishing.join().unwrap();
        finished.recv_timeout(Duration::from_secs(1)).unwrap();
        clearing.join().unwrap();
        drop(rec);
        fs::remove_dir_all(path).ok();
    }
}
