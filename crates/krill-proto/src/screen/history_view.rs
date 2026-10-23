//! Negotiated frozen history ranges; legacy HistoryRequest/HistoryRows stay byte-for-byte unchanged.
use super::{Cell, Msg, OPS_BUDGET, Seq, Source, kind};
use crate::{SessionId, ipc};
use serde::{Deserialize, Serialize};

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
}

impl HistoryViewRows {
    pub fn fit(&mut self) {
        let mut used = 0usize;
        let keep = self
            .rows
            .iter()
            .take_while(|row| {
                used = used.saturating_add(
                    postcard::experimental::serialized_size(row).unwrap_or(usize::MAX),
                );
                used <= OPS_BUDGET
            })
            .count();
        self.rows.truncate(keep);
    }
}
impl Msg for HistoryViewRequest {
    const KIND: u8 = kind::HISTORY_VIEW;
}
impl Msg for HistoryViewRows {
    const KIND: u8 = kind::HISTORY_VIEW_ROWS;
    fn decode(kind: u8, body: &[u8]) -> Result<Self, ipc::Error> {
        let value: Self = ipc::decode(Self::KIND, kind, body)?;
        if value.rows.len() > usize::from(super::MAX_HISTORY_ROWS) {
            return Err(ipc::Error::TooLong(value.rows.len()));
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
            let cap = usize::from(super::MAX_HISTORY_ROWS);
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
                let cap = usize::from(super::MAX_COLS);
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
