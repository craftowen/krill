//! 재개 전 세그먼트를 새 세션의 지연 히스토리로 잇는다. 원본 seq와 세그먼트 형식은 바꾸지 않는다.
use super::{History, Store, mkdir};
use krill_proto::SessionId;
use krill_proto::os::fs::OpenOptionsExt as _;
use krill_proto::screen::{Cell, MAX_HISTORY_ROWS};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};

const PRIOR: &str = "prior";
const MAX_GENERATIONS: usize = 64;

#[derive(Debug)]
pub(crate) struct Archive {
    // 최신 세션부터. 이전 세션마다 마지막 화면도 별도 페이지로 제공한다.
    generations: Vec<History>,
}

impl Store {
    pub(crate) fn inherit_history(&self, session: SessionId, prior: SessionId) -> io::Result<()> {
        let _pause = self.record_guard();
        let _gate = super::lock(&self.gate);
        // 일시중지해도 재개 계보 메타데이터는 남긴다. 새 터미널 원본은 Recorder가 계속 막는다.
        if !self.recording || session == prior {
            return Ok(());
        }
        let dir = self.session_dir(session);
        mkdir(&dir)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join(PRIOR))?;
        file.write_all(&prior.0.to_le_bytes())?;
        file.sync_all()
    }

    /// `Resume` 중 holder 기동이 실패했을 때 미리 만들어 둔 `prior` 링크와 빈 디렉터리를 걷는다.
    pub(crate) fn discard_inherited_history(&self, session: SessionId) {
        let _gate = super::lock(&self.gate);
        if !self.recording {
            return;
        }
        let dir = self.session_dir(session);
        fs::remove_file(dir.join(PRIOR)).ok();
        fs::remove_dir(&dir).ok();
    }

    pub(super) fn history_chain(&self, session: SessionId) -> Vec<SessionId> {
        let mut seen = Vec::new();
        let mut current = session;
        for _ in 0..MAX_GENERATIONS {
            if seen.contains(&current) {
                break;
            }
            seen.push(current);
            let dir = self.session_dir(current);
            // ⌘K는 재개 전 이력도 가린다(seq 0인 경우까지 포함).
            if !self.recording || dir.join(super::CLEARED_FILE).exists() {
                break;
            }
            let Some(bytes) = fs::read(dir.join(PRIOR))
                .ok()
                .and_then(|v| v.try_into().ok())
            else {
                break;
            };
            current = SessionId(u64::from_le_bytes(bytes));
        }
        seen
    }

    pub(crate) fn archive(&self, session: SessionId) -> io::Result<Archive> {
        let generations = self
            .history_chain(session)
            .into_iter()
            .map(|id| self.history(id))
            .collect::<io::Result<_>>()?;
        Ok(Archive { generations })
    }
}

impl Archive {
    pub(crate) fn len(&self) -> usize {
        self.generations
            .iter()
            .enumerate()
            .fold(0usize, |n, (i, h)| {
                n.saturating_add(h.len())
                    .saturating_add(usize::from(i > 0 && !h.is_empty()))
            })
    }

    pub(crate) fn rows(
        &self,
        mut page: usize,
        skip: usize,
        count: usize,
    ) -> io::Result<(usize, Vec<Vec<Cell>>)> {
        for (i, history) in self.generations.iter().enumerate().rev() {
            if page < history.len() {
                return history.rows(page, skip, count);
            }
            page = page.saturating_sub(history.len());
            if i > 0 && !history.is_empty() {
                if page == 0 {
                    let screen = history.restore()?.screen();
                    let end = screen
                        .rows
                        .iter()
                        .rposition(|r| r.iter().any(|c| c.ch != ' ' && c.ch != '\0'))
                        .map_or(0, |i| i + 1);
                    let last = end.saturating_sub(skip);
                    let first = last.saturating_sub(count.min(usize::from(MAX_HISTORY_ROWS)));
                    return Ok((
                        end,
                        screen.rows.get(first..last).unwrap_or_default().to_vec(),
                    ));
                }
                page = page.saturating_sub(1);
            }
        }
        Err(io::ErrorKind::NotFound.into())
    }
}
