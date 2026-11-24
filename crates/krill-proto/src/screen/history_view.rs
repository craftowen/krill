//! Negotiated frozen history ranges; legacy HistoryRequest/HistoryRows stay byte-for-byte unchanged.
use super::{
    Cell, Link, MAX_COLS, MAX_HISTORY_ROWS, MAX_LINES, MAX_URI, MAX_ZEROWIDTH, Msg, Screen, Seq,
    Source, kind,
};
use crate::SessionId;
use crate::ipc::{self, MAX_FRAME};
use serde::{Deserialize, Serialize};

const HISTORY_SCREEN_TAG: &[u8; 4] = b"HVS1";

/// One view is owned by one attach connection. Zero explicitly opens a new view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryViewRequest {
    pub req: u32,
    pub view: u64,
    /// Frozen logical row index, newest first. A past-end request returns the oldest window.
    pub skip: u64,
    pub count: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HistoryViewStatus {
    Ready,
    Missing,
    Invalidated,
    Capacity,
}

/// Stable identity of the source actually decoded, never a fresh relative ordinal alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryIdentity {
    pub session: SessionId,
    pub source: Source,
    pub start_seq: Seq,
    pub end_seq: Seq,
    /// SHA256 of decoded source rows at the frozen cutoff (including its replay basis).
    pub digest: [u8; 32],
}

/// Atomic live-screen snapshot captured at the same cutoff as the opening frozen history view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryScreen {
    #[serde(deserialize_with = "bounded_screen_rows")]
    pub rows: Vec<Vec<Cell>>,
    #[serde(deserialize_with = "bounded_screen_links")]
    pub links: Vec<Vec<Link>>,
}

impl HistoryScreen {
    pub fn from_screen(screen: &Screen) -> Self {
        Self {
            rows: screen.rows.clone(),
            links: screen.links.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryViewRows {
    pub req: u32,
    pub view: u64,
    pub session: SessionId,
    /// Live clear epoch, recording generation and cleared raw offset respectively.
    pub epoch: [u64; 3],
    pub status: HistoryViewStatus,
    pub identity: Option<HistoryIdentity>,
    pub skip: u64,
    /// Known prefix length; exact when complete is true.
    pub total: u64,
    pub complete: bool,
    /// Newest first, beginning at skip; may be shorter than count to fit a frame/source.
    #[serde(deserialize_with = "bounded_rows")]
    pub rows: Vec<Vec<Cell>>,
    /// Optional postfix; skipped in the base representation for N-1 compatibility.
    #[serde(default, skip)]
    pub screen: Option<HistoryScreen>,
}

fn varint_len(mut n: usize) -> usize {
    let mut len = 1usize;
    while n >= 0x80 {
        n >>= 7;
        len = len.saturating_add(1);
    }
    len
}

fn screen_wire_size(screen: &HistoryScreen) -> Option<usize> {
    postcard::experimental::serialized_size(screen)
        .ok()?
        .checked_add(HISTORY_SCREEN_TAG.len())
}

impl HistoryViewRows {
    pub fn fit(&mut self) {
        const MAX_BODY: usize = MAX_FRAME - 1;
        let rows = std::mem::take(&mut self.rows);
        let base_empty = postcard::experimental::serialized_size(self).unwrap_or(MAX_BODY);
        let base_prefix = base_empty.saturating_sub(1);
        let first_total = match rows.first() {
            Some(row) => {
                let first_size = postcard::experimental::serialized_size(row).unwrap_or(usize::MAX);
                base_prefix
                    .saturating_add(varint_len(1))
                    .saturating_add(first_size)
            }
            None => base_empty,
        };
        let screen_cap = (MAX_BODY.saturating_sub(base_empty) / 4).saturating_mul(3);
        let screen_budget = MAX_BODY.saturating_sub(first_total).min(screen_cap);
        let mut screen_bytes = 0usize;
        if let Some(screen) = self.screen.as_mut() {
            let mut cost = screen_wire_size(screen).unwrap_or(usize::MAX);
            if cost > screen_budget && !screen.links.is_empty() {
                screen.links.clear();
                cost = screen_wire_size(screen).unwrap_or(usize::MAX);
            }
            if cost <= screen_budget {
                screen_bytes = cost;
            } else {
                self.screen = None;
            }
        }
        let mut used_rows = 0usize;
        let mut keep = 0usize;
        for row in &rows {
            let row_size = postcard::experimental::serialized_size(row).unwrap_or(usize::MAX);
            let next_rows = used_rows.saturating_add(row_size);
            let next_keep = keep.saturating_add(1);
            let total = base_prefix
                .saturating_add(varint_len(next_keep))
                .saturating_add(next_rows)
                .saturating_add(screen_bytes);
            if total > MAX_BODY {
                break;
            }
            used_rows = next_rows;
            keep = next_keep;
        }
        self.rows = rows;
        self.rows.truncate(keep);

        loop {
            let body_len = postcard::experimental::serialized_size(self)
                .ok()
                .and_then(|base| match &self.screen {
                    Some(screen) => screen_wire_size(screen).and_then(|s| base.checked_add(s)),
                    None => Some(base),
                });
            if body_len.is_some_and(|n| n <= MAX_BODY) {
                break;
            }
            if let Some(screen) = self.screen.as_mut() {
                if screen.links.is_empty() {
                    self.screen = None;
                } else {
                    screen.links.clear();
                }
            } else if self.rows.pop().is_none() {
                break;
            }
        }
    }
}
impl Msg for HistoryViewRequest {
    const KIND: u8 = kind::HISTORY_VIEW;
}
impl Msg for HistoryViewRows {
    const KIND: u8 = kind::HISTORY_VIEW_ROWS;

    fn encode(&self) -> Result<Vec<u8>, ipc::Error> {
        let mut body = postcard::to_stdvec(self).map_err(ipc::Error::Codec)?;
        if let Some(screen) = &self.screen {
            body.extend_from_slice(HISTORY_SCREEN_TAG);
            body.extend(postcard::to_stdvec(screen).map_err(ipc::Error::Codec)?);
        }
        ipc::encode_frame(Self::KIND, &body)
    }

    fn decode(kind: u8, body: &[u8]) -> Result<Self, ipc::Error> {
        if kind != Self::KIND {
            return Err(ipc::Error::UnexpectedKind {
                want: Self::KIND,
                got: kind,
            });
        }
        let (mut value, rest) =
            postcard::take_from_bytes::<Self>(body).map_err(ipc::Error::Codec)?;
        if value.rows.len() > usize::from(MAX_HISTORY_ROWS) {
            return Err(ipc::Error::TooLong(value.rows.len()));
        }
        if let Some(tail) = rest.strip_prefix(HISTORY_SCREEN_TAG) {
            let (screen, _future) =
                postcard::take_from_bytes::<HistoryScreen>(tail).map_err(ipc::Error::Codec)?;
            value.screen = Some(screen);
        }
        Ok(value)
    }
}

fn bounded_rows<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Vec<Cell>>, D::Error> {
    struct Rows;
    impl<'de> serde::de::Visitor<'de> for Rows {
        type Value = Vec<Vec<Cell>>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded frozen history rows")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error as _;
            let cap = usize::from(MAX_HISTORY_ROWS);
            if seq.size_hint().is_some_and(|n| n > cap) {
                return Err(A::Error::custom("too many frozen history rows"));
            }
            let mut rows = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(cap));
            while let Some(Row(row)) = seq.next_element()? {
                if rows.len() >= cap {
                    return Err(A::Error::custom("too many frozen history rows"));
                }
                rows.push(row);
            }
            Ok(rows)
        }
    }
    d.deserialize_seq(Rows)
}

fn bounded_screen_rows<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Vec<Cell>>, D::Error> {
    struct ScreenRows;
    impl<'de> serde::de::Visitor<'de> for ScreenRows {
        type Value = Vec<Vec<Cell>>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded frozen screen rows")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error as _;
            let cap = usize::from(MAX_LINES);
            if seq.size_hint().is_some_and(|n| n > cap) {
                return Err(A::Error::custom("too many frozen screen rows"));
            }
            let mut rows = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(cap));
            while let Some(ScreenRow(row)) = seq.next_element()? {
                if rows.len() >= cap {
                    return Err(A::Error::custom("too many frozen screen rows"));
                }
                rows.push(row);
            }
            Ok(rows)
        }
    }
    d.deserialize_seq(ScreenRows)
}

fn bounded_screen_links<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<Vec<Link>>, D::Error> {
    struct ScreenLinksList;
    impl<'de> serde::de::Visitor<'de> for ScreenLinksList {
        type Value = Vec<Vec<Link>>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded frozen screen link rows")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error as _;
            let cap = usize::from(MAX_LINES);
            if seq.size_hint().is_some_and(|n| n > cap) {
                return Err(A::Error::custom("too many frozen screen link rows"));
            }
            let mut rows = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(cap));
            while let Some(ScreenLinkRow(links)) = seq.next_element()? {
                if rows.len() >= cap {
                    return Err(A::Error::custom("too many frozen screen link rows"));
                }
                rows.push(links);
            }
            Ok(rows)
        }
    }
    d.deserialize_seq(ScreenLinksList)
}

struct Row(Vec<Cell>);
impl<'de> Deserialize<'de> for Row {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Cells;
        impl<'de> serde::de::Visitor<'de> for Cells {
            type Value = Row;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded history columns")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Row, A::Error> {
                use serde::de::Error as _;
                let cap = usize::from(MAX_COLS);
                if seq.size_hint().is_some_and(|n| n > cap) {
                    return Err(A::Error::custom("too many history columns"));
                }
                let mut cells = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(cap));
                while let Some(cell) = seq.next_element()? {
                    if cells.len() >= cap {
                        return Err(A::Error::custom("too many history columns"));
                    }
                    cells.push(cell);
                }
                Ok(Row(cells))
            }
        }
        d.deserialize_seq(Cells)
    }
}

struct ScreenRow(Vec<Cell>);
impl<'de> Deserialize<'de> for ScreenRow {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct ScreenCells;
        impl<'de> serde::de::Visitor<'de> for ScreenCells {
            type Value = ScreenRow;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded frozen screen columns")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<ScreenRow, A::Error> {
                use serde::de::Error as _;
                let cap = usize::from(MAX_COLS);
                if seq.size_hint().is_some_and(|n| n > cap) {
                    return Err(A::Error::custom("too many frozen screen columns"));
                }
                let mut cells = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(cap));
                while let Some(cell) = seq.next_element::<Cell>()? {
                    if cells.len() >= cap {
                        return Err(A::Error::custom("too many frozen screen columns"));
                    }
                    if cell.zerowidth.len() > MAX_ZEROWIDTH {
                        return Err(A::Error::custom("too many zerowidth characters"));
                    }
                    cells.push(cell);
                }
                Ok(ScreenRow(cells))
            }
        }
        d.deserialize_seq(ScreenCells)
    }
}

struct ScreenLinkRow(Vec<Link>);
impl<'de> Deserialize<'de> for ScreenLinkRow {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Links;
        impl<'de> serde::de::Visitor<'de> for Links {
            type Value = ScreenLinkRow;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded frozen screen links")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<ScreenLinkRow, A::Error> {
                use serde::de::Error as _;
                let cap = usize::from(MAX_COLS);
                if seq.size_hint().is_some_and(|n| n > cap) {
                    return Err(A::Error::custom("too many frozen screen links"));
                }
                let mut links = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(cap));
                while let Some(link) = seq.next_element::<Link>()? {
                    if links.len() >= cap {
                        return Err(A::Error::custom("too many frozen screen links"));
                    }
                    if link.uri.len() > MAX_URI
                        || link.cols == 0
                        || link
                            .column
                            .checked_add(link.cols)
                            .is_none_or(|end| end > MAX_COLS)
                    {
                        return Err(A::Error::custom("invalid frozen screen link"));
                    }
                    links.push(link);
                }
                Ok(ScreenLinkRow(links))
            }
        }
        d.deserialize_seq(Links)
    }
}
