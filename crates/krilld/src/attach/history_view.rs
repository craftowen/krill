//! One bounded frozen view per attach worker. Relative cold ordinals never escape this token.
use super::Tap;
use crate::store::stable::Manifest;
use krill_proto::screen::{
    HistoryIdentity, HistoryViewRequest, HistoryViewRows, HistoryViewStatus, Seq, Source,
};
use sha2::{Digest, Sha256};
use std::io;

#[derive(Default)]
pub(super) struct Service {
    serial: u64,
    view: Option<View>,
}
enum SourceKind {
    Hot,
    Cold { gen_idx: usize, page: usize },
    PriorScreen { rows: Vec<Box<[u8]>> },
}
struct SourceInfo {
    total: usize,
    covered: usize,
    identity: HistoryIdentity,
    kind: SourceKind,
}
struct View {
    token: u64,
    epoch: [u64; 3],
    hot: Vec<Box<[u8]>>,
    manifests: Vec<Manifest>,
    info: Vec<Option<SourceInfo>>,
    indexed: bool,
}

impl Service {
    pub(super) fn load(
        &mut self,
        tap: &Tap,
        req: &HistoryViewRequest,
        cancelled: &dyn Fn() -> bool,
    ) -> HistoryViewRows {
        let mut reply = HistoryViewRows {
            req: req.req,
            view: req.view,
            session: tap.id(),
            epoch: [0; 3],
            status: HistoryViewStatus::Invalidated,
            identity: None,
            skip: req.skip,
            total: 0,
            complete: false,
            rows: Vec::new(),
        };
        if req.view == 0 {
            let Ok((epoch, cutoff, hot)) = tap.history_snapshot(cancelled) else {
                reply.status = HistoryViewStatus::Missing;
                return reply;
            };
            reply.epoch = epoch;
            let result = (|| -> io::Result<View> {
                // Retain exact compact rows, not capture_row's spare capacity for
                // every terminal column. Decode only a requested hot window.
                let hot = hot
                    .into_iter()
                    .map(|row| {
                        postcard::to_stdvec(&row)
                            .map(Vec::into_boxed_slice)
                            .map_err(io::Error::other)
                    })
                    .collect::<io::Result<Vec<_>>>()?;
                let weight = hot
                    .iter()
                    .map(|row| row.len())
                    .sum::<usize>()
                    .saturating_add(
                        hot.capacity()
                            .saturating_mul(std::mem::size_of::<Box<[u8]>>()),
                    );
                if weight > 4 * 1024 * 1024 {
                    return Err(io::ErrorKind::OutOfMemory.into());
                }
                let mut digest = Sha256::new();
                for row in &hot {
                    digest.update(u64::try_from(row.len()).unwrap_or(u64::MAX).to_le_bytes());
                    digest.update(row);
                }
                let manifests = tap
                    .store()
                    .map(|store| store.stable_manifests(tap.id(), cutoff, cancelled))
                    .transpose()?
                    .unwrap_or_default();
                self.serial = self
                    .serial
                    .checked_add(1)
                    .ok_or(io::ErrorKind::OutOfMemory)?;
                let pages = manifests.iter().map(Manifest::len).sum::<usize>();
                let hot_id = HistoryIdentity {
                    session: tap.id(),
                    source: Source::Hot,
                    start_seq: Seq(cutoff),
                    end_seq: Seq(cutoff),
                    digest: digest.finalize().into(),
                };
                let mut info = Vec::with_capacity(pages + 1);
                info.push(Some(SourceInfo {
                    total: hot.len(),
                    covered: 0,
                    identity: hot_id,
                    kind: SourceKind::Hot,
                }));
                info.extend((0..pages).map(|_| None));
                Ok(View {
                    token: self.serial,
                    epoch,
                    hot,
                    manifests,
                    info,
                    indexed: false,
                })
            })();
            if let Err(error) = result.as_ref() {
                self.view = None;
                reply.status = if error.kind() == io::ErrorKind::OutOfMemory {
                    HistoryViewStatus::Capacity
                } else {
                    HistoryViewStatus::Missing
                };
                return reply;
            }
            self.view = result.ok();
        }
        let Some(view) = self.view.as_mut() else {
            return reply;
        };
        reply.view = view.token;
        reply.epoch = view.epoch;
        if req.view != 0 && req.view != view.token {
            return reply;
        }
        let count = usize::from(req.count.clamp(1, krill_proto::screen::MAX_HISTORY_ROWS));
        if let Ok(()) = view.range(req.skip, count, &mut reply, cancelled) {
            reply.status = HistoryViewStatus::Ready;
        } else {
            reply.status = HistoryViewStatus::Missing;
            reply.rows.clear();
            reply.identity = None;
        }
        reply.fit();
        reply
    }
}
impl View {
    fn cold(
        &self,
        index: usize,
        skip: usize,
        count: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<crate::store::stable::Range> {
        let info = self
            .info
            .get(index)
            .and_then(Option::as_ref)
            .ok_or(io::ErrorKind::NotFound)?;
        let SourceKind::Cold { gen_idx, page } = info.kind else {
            return Err(io::ErrorKind::NotFound.into());
        };
        let manifest = self.manifests.get(gen_idx).ok_or(io::ErrorKind::NotFound)?;
        let hot = if gen_idx == 0 {
            let covered = self
                .info
                .iter()
                .skip(1)
                .take(index.saturating_sub(1))
                .filter_map(Option::as_ref)
                .map(|info| info.covered)
                .sum::<usize>();
            self.hot
                .get(covered..covered.saturating_add(info.covered))
                .filter(|rows| !rows.is_empty())
        } else {
            None
        };
        manifest.range(page, skip, count, hot, cancelled)
    }
    fn range(
        &mut self,
        requested: u64,
        count: usize,
        reply: &mut HistoryViewRows,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<()> {
        if !self.indexed && requested >= u64::try_from(self.hot.len()).unwrap_or(u64::MAX) {
            self.info.truncate(1);
            for (gen_idx, manifest) in self.manifests.iter().enumerate() {
                let hot_ref: &[Box<[u8]>] = if gen_idx == 0 { &self.hot } else { &[] };
                let (index, prior_screen) =
                    manifest.index_with_screen(hot_ref, gen_idx > 0, cancelled)?;
                if let Some(screen) = prior_screen
                    && !screen.rows.is_empty()
                {
                    self.info.push(Some(SourceInfo {
                        identity: screen.entry.identity,
                        total: screen.entry.total,
                        covered: screen.entry.covered,
                        kind: SourceKind::PriorScreen { rows: screen.rows },
                    }));
                }
                for (page, entry) in index.into_iter().enumerate().rev() {
                    self.info.push(Some(SourceInfo {
                        identity: entry.identity,
                        total: entry.total,
                        covered: entry.covered,
                        kind: SourceKind::Cold { gen_idx, page },
                    }));
                }
            }
            self.indexed = true;
        }
        let mut base = 0u64;
        let mut target = requested;
        for index in 0..self.info.len() {
            if cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if self.info.get(index).is_some_and(Option::is_none) {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let info = self
                .info
                .get(index)
                .and_then(Option::as_ref)
                .ok_or(io::ErrorKind::InvalidData)?;
            let total = u64::try_from(info.total).map_err(io::Error::other)?;
            let end = base.saturating_add(total);
            let oldest = self.info.get(index + 1..).is_none_or(|rest| {
                rest.iter()
                    .all(|slot| slot.as_ref().is_some_and(|next| next.total == 0))
            });
            if oldest && target >= end {
                target = end
                    .saturating_sub(u64::try_from(count).unwrap_or(u64::MAX))
                    .max(base);
            }
            if target < end || oldest {
                let skip =
                    usize::try_from(target.saturating_sub(base)).map_err(io::Error::other)?;
                reply.skip = target;
                reply.total = end;
                reply.complete = oldest;
                reply.identity = Some(info.identity.clone());
                match &info.kind {
                    SourceKind::Hot => {
                        reply.rows = self
                            .hot
                            .iter()
                            .skip(skip)
                            .take(count)
                            .map(|row| postcard::from_bytes(row).map_err(io::Error::other))
                            .collect::<io::Result<Vec<_>>>()?;
                    }
                    SourceKind::PriorScreen { rows } => {
                        reply.rows = rows
                            .iter()
                            .skip(skip)
                            .take(count)
                            .map(|row| postcard::from_bytes(row).map_err(io::Error::other))
                            .collect::<io::Result<Vec<_>>>()?;
                    }
                    SourceKind::Cold { .. } => {
                        let expected = info.identity.clone();
                        let loaded = self.cold(index, skip, count, cancelled)?;
                        if loaded.identity != expected
                            || loaded.total != info.total
                            || loaded.covered != info.covered
                        {
                            return Err(io::ErrorKind::InvalidData.into());
                        }
                        reply.rows = loaded.rows;
                    }
                }
                return Ok(());
            }
            base = end;
        }
        Err(io::ErrorKind::InvalidData.into())
    }
}
