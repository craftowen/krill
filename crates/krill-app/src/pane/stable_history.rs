//! Frozen logical coordinates with bounded decoded rows; the shown viewport is always pinned.
use krill_proto::SessionId;
use krill_proto::screen::{Cell, HistoryViewRequest, HistoryViewRows, HistoryViewStatus};
use std::collections::BTreeMap;
const MAX_ROWS: usize = 4000;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(super) struct Cache {
    session: SessionId,
    view: u64,
    epoch: Option<[u64; 3]>,
    rows: BTreeMap<usize, Vec<Cell>>,
    known: usize,
    complete: Option<usize>,
    shown: usize,
    wanted: usize,
    lines: usize,
    pending: Option<u32>,
    stopped: bool,
    notice: Option<String>,
}
impl Cache {
    pub(super) fn new(session: SessionId) -> Self {
        Self {
            session,
            view: 0,
            epoch: None,
            rows: BTreeMap::new(),
            known: 0,
            complete: None,
            shown: 0,
            wanted: 0,
            lines: 0,
            pending: None,
            stopped: false,
            notice: None,
        }
    }
    pub(super) fn visible(&self) -> usize {
        self.shown
    }
    pub(super) fn total(&self) -> usize {
        self.complete.unwrap_or(self.known)
    }
    pub(super) fn is_stopped(&self) -> bool {
        self.stopped
    }
    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn complete_total(&self) -> Option<usize> {
        self.complete
    }
    pub(super) fn can_compose(&self, lines: usize) -> bool {
        (self.shown.saturating_sub(lines)..self.shown).all(|i| self.rows.contains_key(&i))
    }
    pub(super) fn can_compose_at(&self, wanted: usize) -> bool {
        let wanted = self.complete.map_or(wanted, |total| wanted.min(total));
        self.lines > 0
            && (wanted.saturating_sub(self.lines)..wanted).all(|i| self.rows.contains_key(&i))
    }
    pub(super) fn row(&self, index: usize) -> Option<&Vec<Cell>> {
        self.rows.get(&index)
    }
    pub(super) fn expire(&mut self) {
        self.stop(HistoryViewStatus::Invalidated);
        self.pending = None;
    }
    fn stop(&mut self, status: HistoryViewStatus) {
        self.stopped = true;
        self.notice=Some(match status {
            HistoryViewStatus::Capacity=>"History cache metadata limit reached. Current view is retained; return to the bottom to open a new view.",
            HistoryViewStatus::Missing=>"Original history is no longer available. Current view is retained; return to the bottom to refresh.",
            _=>"History view changed or expired. Current view is retained; return to the bottom to refresh.",
        }.to_owned());
    }
    pub(super) fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }
    pub(super) fn show(&mut self, wanted: usize, lines: usize) {
        self.wanted = self.complete.map_or(wanted, |total| wanted.min(total));
        self.lines = lines;
        if (self.wanted.saturating_sub(lines)..self.wanted).all(|i| self.rows.contains_key(&i)) {
            self.shown = self.wanted;
        }
    }
    pub(super) fn next(
        &mut self,
        wanted: usize,
        lines: usize,
        req: u32,
    ) -> Option<HistoryViewRequest> {
        self.show(wanted, lines);
        if self.stopped || self.pending.is_some() || wanted == 0 {
            return None;
        }
        let start = self.wanted.saturating_sub(lines);
        let missing = (start..self.wanted).find(|i| !self.rows.contains_key(i))?;
        // Keep adjacent scrolling in memory. A source/frame can end before
        // the target, so resume after the closest cached prefix on each reply.
        let prefetch_start = if self.wanted < self.shown {
            missing.saturating_sub(128)
        } else {
            // A distant older target must make pinned progress even when wide
            // prefetched rows exceed the byte budget before a source boundary.
            missing
        };
        let start = (prefetch_start..missing)
            .rev()
            .find(|i| self.rows.contains_key(i))
            .map_or(prefetch_start, |i| i.saturating_add(1));
        self.pending = Some(req);
        Some(HistoryViewRequest {
            req,
            view: self.view,
            skip: u64::try_from(start).unwrap_or(u64::MAX),
            count: u16::try_from(
                self.wanted
                    .saturating_add(128)
                    .saturating_sub(start)
                    .min(usize::from(krill_proto::screen::MAX_HISTORY_ROWS)),
            )
            .unwrap_or(krill_proto::screen::MAX_HISTORY_ROWS)
            .max(1),
        })
    }
    pub(super) fn accept(&mut self, reply: HistoryViewRows) -> bool {
        if self.pending != Some(reply.req) {
            return false;
        }
        self.pending = None;
        if reply.session != self.session
            || self.view != 0 && self.view != reply.view
            || self.epoch.is_some_and(|epoch| epoch != reply.epoch)
        {
            self.stop(HistoryViewStatus::Invalidated);
            return false;
        }
        if reply.status != HistoryViewStatus::Ready {
            self.stop(reply.status);
            return false;
        }
        if reply.view == 0 {
            self.stop(HistoryViewStatus::Invalidated);
            return false;
        }
        let (Ok(skip), Ok(total)) = (usize::try_from(reply.skip), usize::try_from(reply.total))
        else {
            self.stop(HistoryViewStatus::Capacity);
            return false;
        };
        if reply
            .identity
            .as_ref()
            .is_none_or(|id| id.session != self.session)
            || skip.saturating_add(reply.rows.len()) > total
            || reply.rows.is_empty() && skip < total
        {
            self.stop(HistoryViewStatus::Missing);
            return false;
        }
        let wanted = if reply.complete {
            self.wanted.min(total)
        } else {
            self.wanted
        };
        let shown = self.shown;
        let lines = self.lines;
        let is_pinned = |i: usize| {
            (shown.saturating_sub(lines)..shown).contains(&i)
                || (wanted.saturating_sub(lines)..wanted).contains(&i)
        };
        let incoming = skip..skip.saturating_add(reply.rows.len());
        let mut pinned_bytes = self
            .rows
            .iter()
            .filter(|(i, _)| is_pinned(**i) && !incoming.contains(*i))
            .map(|(_, row)| Self::weight(row))
            .sum::<usize>();
        for (n, row) in reply.rows.iter().enumerate() {
            let i = skip.saturating_add(n);
            if self.rows.get(&i).is_some_and(|old| old != row) {
                self.stop(HistoryViewStatus::Invalidated);
                return false;
            }
            if is_pinned(i) {
                pinned_bytes = pinned_bytes.saturating_add(Self::weight(row));
            }
        }
        if pinned_bytes > MAX_BYTES {
            self.stop(HistoryViewStatus::Capacity);
            return false;
        }
        self.view = reply.view;
        self.epoch = Some(reply.epoch);
        self.known = self.known.max(total);
        if reply.complete {
            self.complete = Some(total);
        }
        for (n, row) in reply.rows.into_iter().enumerate() {
            self.rows.insert(skip.saturating_add(n), row);
        }
        let before = self.shown;
        self.show(self.wanted, self.lines);
        self.prune();
        self.can_compose(self.lines) && (self.shown != before || self.shown == self.wanted)
    }
    fn weight(row: &Vec<Cell>) -> usize {
        row.capacity()
            .saturating_mul(std::mem::size_of::<Cell>())
            .saturating_add(
                row.iter()
                    .map(|c| {
                        c.zerowidth
                            .capacity()
                            .saturating_mul(std::mem::size_of::<char>())
                    })
                    .sum::<usize>(),
            )
    }
    fn prune(&mut self) {
        let mut bytes = self.rows.values().map(Self::weight).sum::<usize>();
        let mut candidates = self
            .rows
            .keys()
            .copied()
            .filter(|i| {
                !((self.shown.saturating_sub(self.lines)..self.shown).contains(i)
                    || (self.wanted.saturating_sub(self.lines)..self.wanted).contains(i))
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|i| std::cmp::Reverse(i.abs_diff(self.shown)));
        for i in candidates {
            if self.rows.len() <= MAX_ROWS && bytes <= MAX_BYTES {
                break;
            }
            if let Some(row) = self.rows.remove(&i) {
                bytes = bytes.saturating_sub(Self::weight(&row));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::screen::{HistoryIdentity, Seq, Source};
    fn reply(req: HistoryViewRequest) -> HistoryViewRows {
        HistoryViewRows {
            req: req.req,
            view: 7,
            session: SessionId(1),
            epoch: [0; 3],
            status: HistoryViewStatus::Ready,
            identity: Some(HistoryIdentity {
                session: SessionId(1),
                source: Source::Hot,
                start_seq: Seq(1),
                end_seq: Seq(2),
                digest: [0; 32],
            }),
            skip: req.skip,
            total: 100_000,
            complete: false,
            rows: (0..req.count)
                .map(|n| {
                    format!("row-{}", req.skip + u64::from(n))
                        .chars()
                        .map(|ch| Cell {
                            ch,
                            ..Cell::default()
                        })
                        .collect()
                })
                .collect(),
        }
    }
    #[test]
    fn deep_cache_evicts_only_offscreen_and_reloads_exact_coordinates() {
        let mut cache = Cache::new(SessionId(1));
        for n in 1u32..=600 {
            let offset = usize::try_from(n).unwrap() * 24;
            if let Some(request) = cache.next(offset, 24, n) {
                assert!(cache.accept(reply(request)));
            }
            assert_eq!(cache.visible(), offset);
            assert!(cache.rows.len() <= MAX_ROWS);
            assert!(cache.row(offset - 1).is_some());
        }
        let old = cache.visible();
        let anchor = cache.row(old - 1).unwrap().clone();
        assert!(cache.row(0).is_none());
        let request = cache.next(24, 24, 601).unwrap();
        assert_eq!(request.view, 7);
        assert_eq!(request.skip, 0);
        assert_eq!(
            cache.visible(),
            old,
            "a cache miss cannot move the shown viewport/selection"
        );
        assert_eq!(cache.row(old - 1), Some(&anchor));
        assert!(cache.accept(reply(request)));
        assert_eq!(cache.visible(), 24);
        assert_eq!(
            cache
                .row(23)
                .unwrap()
                .iter()
                .map(|c| c.ch)
                .collect::<String>(),
            "row-23"
        );
    }
    #[test]
    fn missing_clear_scope_capacity_and_reconnect_keep_the_shown_anchor() {
        for status in [
            HistoryViewStatus::Missing,
            HistoryViewStatus::Invalidated,
            HistoryViewStatus::Capacity,
        ] {
            let mut cache = Cache::new(SessionId(1));
            let initial = cache.next(24, 24, 1).unwrap();
            cache.accept(reply(initial));
            let shown = cache.row(23).unwrap().clone();
            let next = cache.next(512, 24, 2).unwrap();
            let mut missing = reply(next);
            missing.status = status;
            missing.rows.clear();
            assert!(!cache.accept(missing));
            assert_eq!(cache.visible(), 24);
            assert_eq!(cache.row(23), Some(&shown));
            assert!(cache.take_notice().is_some());
            assert!(cache.next(512, 24, 3).is_none());
        }
        let mut cache = Cache::new(SessionId(1));
        let first = cache.next(24, 24, 1).unwrap();
        cache.accept(reply(first));
        let next = cache.next(512, 24, 2).unwrap();
        let mut changed = reply(next);
        changed.epoch[0] = 1;
        assert!(!cache.accept(changed));
        assert_eq!(cache.visible(), 24);
        cache.expire();
        assert_eq!(cache.visible(), 24);
        assert!(cache.take_notice().is_some());
        let mut empty = Cache::new(SessionId(1));
        let next = empty.next(24, 24, 1).unwrap();
        let mut other = reply(next);
        other.session = SessionId(2);
        assert!(!empty.accept(other));
        assert_eq!(empty.visible(), 0);
    }

    #[test]
    fn adjacent_scrolls_reuse_prefetch_and_short_source_replies_advance() {
        let mut cache = Cache::new(SessionId(1));
        let request = cache.next(512, 24, 1).unwrap();
        assert!(cache.accept(reply(request)));
        let reverse = cache.next(511, 24, 2).unwrap();
        assert!(cache.accept(reply(reverse)));
        for offset in (400..511).rev() {
            assert!(cache.next(offset, 24, 2).is_none());
            assert_eq!(cache.visible(), offset);
        }
        let mut split = Cache::new(SessionId(1));
        let initial = split.next(512, 24, 1).unwrap();
        assert!(split.accept(reply(initial)));
        let first = split.next(511, 24, 2).unwrap();
        let mut short = reply(first);
        short.rows.truncate(64);
        assert!(!split.accept(short));
        let second = split.next(511, 24, 3).unwrap();
        assert_eq!(second.skip, first.skip + 64);
        assert!(split.accept(reply(second)));
        assert_eq!(split.visible(), 511);
    }

    #[test]
    fn distant_wide_miss_advances_pinned_rows_without_refetching_evicted_prefetch() {
        let mut cache = Cache::new(SessionId(1));
        let initial = cache.next(24, 24, 1).unwrap();
        assert!(cache.accept(reply(initial)));
        for (req, expected) in [(2, 9976), (3, 9992)] {
            let request = cache.next(10_000, 24, req).unwrap();
            assert_eq!(request.skip, expected);
            let mut chunk = reply(request);
            chunk.rows = vec![
                vec![Cell::default(); usize::from(krill_proto::screen::MAX_COLS)];
                if req == 2 { 16 } else { 8 }
            ];
            cache.accept(chunk);
            assert!(cache.rows.values().map(Cache::weight).sum::<usize>() <= MAX_BYTES);
        }
        assert_eq!(cache.visible(), 10_000);
        assert!(!cache.stopped);
    }

    #[test]
    fn over_budget_wide_target_is_rejected_before_replacing_shown_rows() {
        let mut cache = Cache::new(SessionId(1));
        let first = cache.next(24, 24, 1).unwrap();
        cache.accept(reply(first));
        let shown = cache.row(23).unwrap().clone();
        for n in 2..100 {
            let Some(request) = cache.next(2048, 1024, n) else {
                break;
            };
            let mut chunk = reply(request);
            chunk.rows =
                vec![vec![Cell::default(); usize::from(krill_proto::screen::MAX_COLS)]; 16];
            cache.accept(chunk);
            assert_eq!(cache.visible(), 24);
            assert_eq!(cache.row(23), Some(&shown));
            assert!(cache.rows.values().map(Cache::weight).sum::<usize>() <= MAX_BYTES);
        }
        assert!(cache.stopped);
        assert!(cache.take_notice().unwrap().contains("limit"));
        assert_eq!(cache.visible(), 24);
    }
}
