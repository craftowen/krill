//! OWE-733(MK-179): 오른쪽 사이드바 `Search` 탭(`⌘⇧F`) — 워크스페이스 코드 전문 검색 엔진,
//! 옵션 토글(대소문자·단어 단위·정규식·포함/제외 glob), 파일별 그룹화 가상 리스트(`uniform_list`),
//! 결과 행 클릭 시 중앙 코드 에디터(`MK-119`)의 해당 줄·열로 즉시 이동하는 통합 모듈.
//!
//! - UI 배치 및 검색 결과 트리 동작 참고:
//!   `.ref/orca/src/renderer/src/components/right-sidebar/SearchResultsPane.tsx`,
//!   `.ref/orca/src/renderer/src/components/right-sidebar/SearchQueryRow.tsx`,
//!   `.ref/orca/src/renderer/src/components/right-sidebar/SearchFilters.tsx`,
//!   `.ref/orca/src/renderer/src/components/right-sidebar/SearchResultItems.tsx`,
//!   `.ref/orca/src/main/ipc/text-search.ts`
//!   (Copyright (c) 2026 Lovecast Inc., MIT License, `.ref/orca/LICENSE`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    ClipboardItem, Context, ElementId, Entity, FontWeight, MouseButton, SharedString, Subscription,
    Task, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_component::{Icon, IconName, Selectable as _, Sizable as _};

use super::{OpenFile, OpenFileAt, Panel, Reap, text};
use crate::ui;

const MAX_TOTAL_READ_BYTES: u64 = 64 * 1024 * 1024;

struct SearchCancel(Arc<AtomicBool>);
impl Drop for SearchCancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct SearchBudget {
    deadline: Instant,
    cancel: Arc<AtomicBool>,
    nodes: usize,
    bytes: u64,
    truncated: bool,
}
impl SearchBudget {
    fn new(cancel: Arc<AtomicBool>) -> Self {
        Self {
            deadline: Instant::now() + RG_TIMEOUT,
            cancel,
            nodes: 0,
            bytes: 0,
            truncated: false,
        }
    }
    fn stopped(&mut self) -> bool {
        let stopped = self.cancel.load(Ordering::Relaxed) || Instant::now() >= self.deadline;
        self.truncated |= stopped;
        stopped
    }
    fn node(&mut self) -> bool {
        self.nodes = self.nodes.saturating_add(1);
        self.truncated |= self.nodes > 50_000;
        self.stopped() || self.nodes > 50_000
    }
}

/// 검색 결과 최대 매치 수(상한 도달 시 `truncated = true`).
pub const DEFAULT_MAX_RESULTS: usize = 2_000;

/// 검색 대상 단일 파일 최대 크기(에디터 한도와 동일한 4 MiB).
const MAX_SEARCH_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// 폴백 워커가 스캔할 최대 파일 수.
const MAX_FALLBACK_FILES: usize = 10_000;
const MAX_FALLBACK_READ_BYTES: u64 = MAX_TOTAL_READ_BYTES;

/// `rg --json` 표준 출력 최대 바이트 상한(16 MiB).
const MAX_RG_STDOUT_BYTES: usize = 16 * 1024 * 1024;

/// `rg` 프로세스 실행 제한 시간.
const RG_TIMEOUT: Duration = Duration::from_secs(5);

/// 워크스페이스 검색 입력 디바운스 지연 시간(키 입력마다 `rg`/스캔 중복 스폰 방지).
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);

/// `git check-ignore` 프로세스 실행 제한 시간.
const GIT_IGNORE_TIMEOUT: Duration = Duration::from_secs(2);

/// `git check-ignore` 표준 출력 최대 바이트 상한(2 MiB).
const MAX_GIT_IGNORE_STDOUT_BYTES: usize = 2 * 1024 * 1024;

/// 결과 줄 미리보기 최대 문자 수.
const MAX_PREVIEW_CHARS: usize = 300;

/// 긴 줄에서 매치 시작 위치 앞쪽에 유지할 문맥 문자 수(패널 확대 시 노출 가능한 앞 문맥 상한).
const PREVIEW_CONTEXT_BEFORE: usize = 120;

/// 스크롤 뷰포트 폭 측정 전 기본 검색 패널 행 폭(우측 사이드바 기본 폭 `280pt`).
const DEFAULT_SEARCH_ROW_WIDTH_PX: f32 = 280.0;

/// 매치 행의 좌우 여백·줄 번호 열·간격·하이라이트 패딩 합계(여유 마진 포함, px).
const MATCH_ROW_CHROME_PX: f32 = 76.0;

/// 매우 좁은 패널에서도 보장할 최소 미리보기 열 수.
const MIN_MATCH_PREVIEW_COLS: usize = 12;

/// 앞 문맥 생략 시 뒤 문맥(`after`) 노출을 위해 예약할 최대 열 수.
const AFTER_CONTEXT_RESERVE_COLS: usize = 6;

/// 단일 시작 위치에서 허용하는 정규식 백트래킹 최대 스텝 수.
const MAX_REGEX_STEPS_PER_MATCH: usize = 4_096;

/// 한 줄 전체에서 허용하는 정규식 백트래킹 최대 스텝 수.
const MAX_REGEX_STEPS_PER_LINE: usize = 100_000;

/// 가상 리스트 행 높이(px).
const ROW_HEIGHT: f32 = 24.0;

/// 워크스페이스 전문 검색 옵션.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOptions {
    pub query: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
    pub include_pattern: String,
    pub exclude_pattern: String,
    pub max_results: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            case_sensitive: false,
            whole_word: false,
            use_regex: false,
            include_pattern: String::new(),
            exclude_pattern: String::new(),
            max_results: DEFAULT_MAX_RESULTS,
        }
    }
}

/// 단일 검색 일치 항목.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// 1-based 줄 번호.
    pub line: u32,
    /// 1-based 문자(char) 열 번호.
    pub column: u32,
    /// 일치 구간의 문자(char) 길이.
    pub match_length: u32,
    /// 줄 텍스트 미리보기(제어 문자 공백 치환, 최대 300자).
    pub line_content: String,
}

/// 파일별 검색 결과 그룹.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchFileResult {
    pub path: PathBuf,
    pub relative_path: String,
    pub matches: Vec<SearchMatch>,
}

/// 전체 워크스페이스 검색 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchResult {
    pub files: Vec<SearchFileResult>,
    pub total_matches: usize,
    pub truncated: bool,
}

/// `uniform_list` 가상화를 위한 평탄화된 검색 결과 행.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchRow {
    FileHeader {
        file_idx: usize,
        path: PathBuf,
        relative_path: String,
        file_name: String,
        dir_path: String,
        match_count: usize,
        collapsed: bool,
    },
    Match {
        file_idx: usize,
        match_idx: usize,
        path: PathBuf,
        relative_path: String,
        hit: SearchMatch,
    },
}

/// 우측 패널 내 워크스페이스 검색 상태.
pub struct WorkspaceSearch {
    pub(crate) repo: PathBuf,
    pub(crate) options: SearchOptions,
    pub(crate) result: SearchResult,
    pub(crate) collapsed_files: BTreeSet<String>,
    pub(crate) rows: Vec<SearchRow>,
    pub(crate) selected_row: Option<usize>,
    pub(crate) error: Option<String>,
    pub(crate) searching: bool,
    pub(crate) generation: u64,
    pub(crate) search_task: Option<Task<()>>,
    cancel: Option<SearchCancel>,
    pub(crate) scroll: UniformListScrollHandle,
    pub(crate) query_input: Option<Entity<InputState>>,
    pub(crate) query_sub: Option<Subscription>,
    pub(crate) include_input: Option<Entity<InputState>>,
    pub(crate) include_sub: Option<Subscription>,
    pub(crate) exclude_input: Option<Entity<InputState>>,
    pub(crate) exclude_sub: Option<Subscription>,
    #[cfg(test)]
    pub(crate) rendered_rows: usize,
}

impl std::fmt::Debug for WorkspaceSearch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceSearch")
            .field("repo", &self.repo)
            .field("options", &self.options)
            .field("result", &self.result)
            .field("selected_row", &self.selected_row)
            .field("searching", &self.searching)
            .finish_non_exhaustive()
    }
}

impl WorkspaceSearch {
    #[must_use]
    pub fn new(repo: PathBuf) -> Self {
        Self {
            repo,
            options: SearchOptions::default(),
            result: SearchResult::default(),
            collapsed_files: BTreeSet::new(),
            rows: Vec::new(),
            selected_row: None,
            error: None,
            searching: false,
            generation: 0,
            search_task: None,
            cancel: None,
            scroll: UniformListScrollHandle::new(),
            query_input: None,
            query_sub: None,
            include_input: None,
            include_sub: None,
            exclude_input: None,
            exclude_sub: None,
            #[cfg(test)]
            rendered_rows: 0,
        }
    }

    pub fn set_repo(&mut self, repo: PathBuf, active: bool) {
        if self.repo == repo {
            return;
        }
        self.repo = repo;
        self.collapsed_files.clear();
        self.selected_row = None;
        self.error = None;
        self.generation = self.generation.wrapping_add(1);
        self.cancel = None;
        self.search_task = None;
        self.searching = active && !self.options.query.trim().is_empty();
        self.result = SearchResult::default();
        self.rows.clear();
    }

    pub fn run_sync(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.cancel = None;
        self.search_task = None;
        self.searching = false;
        if self.options.query.is_empty() {
            self.result = SearchResult::default();
            self.collapsed_files.clear();
            self.rows.clear();
            self.selected_row = None;
            self.error = None;
            return;
        }
        match search_worktree(&self.repo, &self.options) {
            Ok(res) => {
                self.error = None;
                self.result = res;
                self.collapsed_files
                    .retain(|rel| self.result.files.iter().any(|f| &f.relative_path == rel));
                self.selected_row = None;
                self.rebuild_rows();
            }
            Err(err) => {
                self.error = Some(err);
                self.result = SearchResult::default();
                self.rows.clear();
                self.selected_row = None;
            }
        }
    }

    pub fn toggle_file_collapse(&mut self, relative_path: &str) {
        if !self.collapsed_files.remove(relative_path) {
            self.collapsed_files.insert(relative_path.to_owned());
        }
        self.rebuild_rows();
    }

    pub fn collapse_all(&mut self) {
        if self.collapsed_files.len() == self.result.files.len() {
            self.collapsed_files.clear();
        } else {
            for file in &self.result.files {
                self.collapsed_files.insert(file.relative_path.clone());
            }
        }
        self.rebuild_rows();
    }

    pub(crate) fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (file_idx, file) in self.result.files.iter().enumerate() {
            let collapsed = self.collapsed_files.contains(&file.relative_path);
            let (file_name, dir_path) = split_file_and_dir(&file.relative_path);
            self.rows.push(SearchRow::FileHeader {
                file_idx,
                path: file.path.clone(),
                relative_path: file.relative_path.clone(),
                file_name,
                dir_path,
                match_count: file.matches.len(),
                collapsed,
            });
            if !collapsed {
                for (match_idx, hit) in file.matches.iter().enumerate() {
                    self.rows.push(SearchRow::Match {
                        file_idx,
                        match_idx,
                        path: file.path.clone(),
                        relative_path: file.relative_path.clone(),
                        hit: hit.clone(),
                    });
                }
            }
        }
    }
}

fn split_file_and_dir(relative_path: &str) -> (String, String) {
    match relative_path.rsplit_once('/') {
        Some((dir, name)) => (name.to_owned(), dir.to_owned()),
        None => (relative_path.to_owned(), String::new()),
    }
}

/// 쉼표(`,`)로 구분된 glob 필터 문자열을 정규화된 패턴 목록으로 파싱한다.
#[must_use]
pub fn parse_glob_patterns(raw: &str) -> Vec<String> {
    raw.split(',')
        .filter_map(|part| {
            let normalized = part.trim().replace('\\', "/");
            let trimmed = normalized
                .trim_start_matches("./")
                .trim_start_matches('/')
                .trim();
            if trimmed.is_empty() {
                return None;
            }
            if let Some(prefix) = trimmed.strip_suffix('/') {
                if prefix.is_empty() {
                    None
                } else {
                    Some(format!("{prefix}/**"))
                }
            } else {
                Some(trimmed.to_owned())
            }
        })
        .collect()
}

/// 워크트리 루트에서 옵션을 적용해 전문 검색을 수행한다.
/// `rg`(`ripgrep`)를 우선 사용하며, 바이너리를 찾을 수 없으면 순수 Rust 폴백 검색기를 실행한다.
///
/// # Errors
/// 정규식 구문 오류 등으로 검색 자체가 실패한 경우 오류 메시지를 반환한다.
pub fn search_worktree(root: &Path, options: &SearchOptions) -> Result<SearchResult, String> {
    search_worktree_control(root, options, Arc::new(AtomicBool::new(false)))
}

fn search_worktree_control(
    root: &Path,
    options: &SearchOptions,
    cancel: Arc<AtomicBool>,
) -> Result<SearchResult, String> {
    let mut budget = SearchBudget::new(cancel);
    if options.query.is_empty() {
        return Ok(SearchResult::default());
    }
    if !root.exists() {
        return Ok(SearchResult::default());
    }
    let matcher = LineMatcher::compile(options)?;
    if let Some(rg_bin) = find_rg_binary() {
        match search_with_rg_control(
            &rg_bin,
            root,
            options,
            RG_TIMEOUT,
            MAX_RG_STDOUT_BYTES,
            &budget.cancel,
        ) {
            Ok(result) => return Ok(result),
            Err(RgError::Unavailable) => {}
            Err(RgError::Execution(msg)) => return Err(msg),
        }
    }
    Ok(search_worktree_fallback_with_matcher(
        root,
        options,
        &matcher,
        &mut budget,
    ))
}

/// 순수 Rust 파일 워커 기반 전문 검색(테스트 및 `rg` 미설치 환경용).
///
/// # Errors
/// 정규식 구문 오류 시 오류 메시지를 반환한다.
pub fn search_worktree_fallback(
    root: &Path,
    options: &SearchOptions,
) -> Result<SearchResult, String> {
    if options.query.is_empty() || !root.exists() {
        return Ok(SearchResult::default());
    }
    let matcher = LineMatcher::compile(options)?;
    let mut budget = SearchBudget::new(Arc::new(AtomicBool::new(false)));
    Ok(search_worktree_fallback_with_matcher(
        root,
        options,
        &matcher,
        &mut budget,
    ))
}

enum LineMatcher {
    Literal {
        query: Vec<char>,
        case_sensitive: bool,
        whole_word: bool,
    },
    Regex {
        pieces: Vec<RegexPiece>,
        case_sensitive: bool,
        whole_word: bool,
    },
}

#[derive(Clone)]
struct RegexPiece {
    atom: RegexAtom,
    quant: Quantifier,
}

#[derive(Clone)]
enum RegexAtom {
    Literal(char),
    Dot,
    Shorthand(char),
    WordBoundary,
    Start,
    End,
    Class {
        negated: bool,
        items: Vec<ClassItem>,
    },
}

#[derive(Clone)]
enum ClassItem {
    Single(char),
    Range(char, char),
    Shorthand(char),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quantifier {
    Once,
    ZeroOrMore,
    OneOrMore,
    ZeroOrOne,
}

impl LineMatcher {
    fn compile(options: &SearchOptions) -> Result<Self, String> {
        if !options.use_regex {
            return Ok(Self::Literal {
                query: options.query.chars().collect(),
                case_sensitive: options.case_sensitive,
                whole_word: options.whole_word,
            });
        }
        let pieces = parse_regex_pieces(&options.query)?;
        Ok(Self::Regex {
            pieces,
            case_sensitive: options.case_sensitive,
            whole_word: options.whole_word,
        })
    }

    fn find_spans(
        &self,
        raw_line: &str,
        budget: &mut SearchBudget,
        limit: usize,
    ) -> Vec<(usize, usize)> {
        let line_chars: Vec<(usize, char)> = raw_line.char_indices().collect();
        let n = line_chars.len();
        let mut out = Vec::new();
        let mut i = 0usize;
        let mut line_steps = 0usize;

        while i < n && out.len() < limit {
            if i.is_multiple_of(64) && budget.stopped() {
                break;
            }
            if line_steps >= MAX_REGEX_STEPS_PER_LINE {
                budget.truncated = true;
                break;
            }
            let matched_end = match self {
                Self::Literal {
                    query,
                    case_sensitive,
                    whole_word,
                } => match_literal_at(&line_chars, i, query, *case_sensitive, *whole_word),
                Self::Regex {
                    pieces,
                    case_sensitive,
                    whole_word,
                } => {
                    let mut steps = 0usize;
                    let matched = match_regex_at(
                        &line_chars,
                        i,
                        pieces,
                        *case_sensitive,
                        *whole_word,
                        &mut steps,
                    );
                    line_steps = line_steps.saturating_add(steps);
                    matched
                }
            };
            if let Some(end_idx) = matched_end
                && end_idx > i
            {
                let byte_start = line_chars.get(i).map_or(0, |&(b, _)| b);
                let byte_end = line_chars.get(end_idx).map_or(raw_line.len(), |&(b, _)| b);
                out.push((byte_start, byte_end));
                i = end_idx;
            } else {
                i = i.saturating_add(1);
            }
        }
        out
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn chars_eq(a: char, b: char, case_sensitive: bool) -> bool {
    if case_sensitive {
        a == b
    } else {
        a.to_lowercase().eq(b.to_lowercase())
    }
}

fn is_word_boundary_at(line_chars: &[(usize, char)], idx: usize) -> bool {
    let prev = idx
        .checked_sub(1)
        .and_then(|p| line_chars.get(p))
        .is_some_and(|&(_, c)| is_word_char(c));
    let next = line_chars.get(idx).is_some_and(|&(_, c)| is_word_char(c));
    prev != next
}

fn match_literal_at(
    line_chars: &[(usize, char)],
    start: usize,
    query: &[char],
    case_sensitive: bool,
    whole_word: bool,
) -> Option<usize> {
    if query.is_empty() || start.saturating_add(query.len()) > line_chars.len() {
        return None;
    }
    for (offset, &qc) in query.iter().enumerate() {
        let &(_, lc) = line_chars.get(start.saturating_add(offset))?;
        if !chars_eq(lc, qc, case_sensitive) {
            return None;
        }
    }
    let end = start.saturating_add(query.len());
    if whole_word
        && (!is_word_boundary_at(line_chars, start) || !is_word_boundary_at(line_chars, end))
    {
        return None;
    }
    Some(end)
}

fn parse_regex_pieces(pattern: &str) -> Result<Vec<RegexPiece>, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0usize;
    let mut pieces: Vec<RegexPiece> = Vec::new();
    let mut group_starts: Vec<usize> = Vec::new();

    while let Some(&ch) = chars.get(i) {
        match ch {
            '(' => {
                group_starts.push(pieces.len());
                i = i.saturating_add(1);
                if chars.get(i) == Some(&'?') && chars.get(i.saturating_add(1)) == Some(&':') {
                    i = i.saturating_add(2);
                }
            }
            ')' => {
                let Some(group_start) = group_starts.pop() else {
                    return Err("unmatched closing parenthesis in regular expression".to_owned());
                };
                i = i.saturating_add(1);
                let (quant, next_i) = consume_quantifier(&chars, i);
                i = next_i;
                if quant != Quantifier::Once {
                    let group_slice = pieces.get(group_start..).unwrap_or(&[]).to_vec();
                    if group_slice.len() == 1
                        && let Some(last) = pieces.last_mut()
                        && last.quant == Quantifier::Once
                    {
                        last.quant = quant;
                    } else if !group_slice.is_empty() {
                        for mut gp in group_slice {
                            if gp.quant == Quantifier::OneOrMore || gp.quant == Quantifier::Once {
                                gp.quant = Quantifier::ZeroOrMore;
                            }
                            pieces.push(gp);
                        }
                    }
                }
            }
            '^' => {
                pieces.push(RegexPiece {
                    atom: RegexAtom::Start,
                    quant: Quantifier::Once,
                });
                i = i.saturating_add(1);
            }
            '$' => {
                pieces.push(RegexPiece {
                    atom: RegexAtom::End,
                    quant: Quantifier::Once,
                });
                i = i.saturating_add(1);
            }
            '.' => {
                i = i.saturating_add(1);
                let (quant, next_i) = consume_quantifier(&chars, i);
                i = next_i;
                pieces.push(RegexPiece {
                    atom: RegexAtom::Dot,
                    quant,
                });
            }
            '\\' => {
                let Some(&esc) = chars.get(i.saturating_add(1)) else {
                    return Err("trailing backslash in regular expression".to_owned());
                };
                i = i.saturating_add(2);
                if esc == 'b' {
                    pieces.push(RegexPiece {
                        atom: RegexAtom::WordBoundary,
                        quant: Quantifier::Once,
                    });
                } else {
                    let atom = match esc {
                        'w' | 'W' | 'd' | 'D' | 's' | 'S' => RegexAtom::Shorthand(esc),
                        'n' => RegexAtom::Literal('\n'),
                        'r' => RegexAtom::Literal('\r'),
                        't' => RegexAtom::Literal('\t'),
                        other => RegexAtom::Literal(other),
                    };
                    let (quant, next_i) = consume_quantifier(&chars, i);
                    i = next_i;
                    pieces.push(RegexPiece { atom, quant });
                }
            }
            '[' => {
                i = i.saturating_add(1);
                let negated = if chars.get(i) == Some(&'^') {
                    i = i.saturating_add(1);
                    true
                } else {
                    false
                };
                let mut items = Vec::new();
                let mut closed = false;
                while let Some(&c) = chars.get(i) {
                    if c == ']' && !items.is_empty() {
                        closed = true;
                        i = i.saturating_add(1);
                        break;
                    }
                    if c == '\\' {
                        let Some(&esc) = chars.get(i.saturating_add(1)) else {
                            return Err("unclosed character class in regular expression".to_owned());
                        };
                        i = i.saturating_add(2);
                        items.push(match esc {
                            'w' | 'W' | 'd' | 'D' | 's' | 'S' => ClassItem::Shorthand(esc),
                            other => ClassItem::Single(other),
                        });
                    } else if chars.get(i.saturating_add(1)) == Some(&'-')
                        && let Some(&end_c) = chars.get(i.saturating_add(2))
                        && end_c != ']'
                    {
                        items.push(ClassItem::Range(c, end_c));
                        i = i.saturating_add(3);
                    } else {
                        items.push(ClassItem::Single(c));
                        i = i.saturating_add(1);
                    }
                }
                if !closed {
                    return Err("unclosed character class in regular expression".to_owned());
                }
                let (quant, next_i) = consume_quantifier(&chars, i);
                i = next_i;
                pieces.push(RegexPiece {
                    atom: RegexAtom::Class { negated, items },
                    quant,
                });
            }
            '*' | '+' | '?' => {
                return Err("quantifier without preceding token in regular expression".to_owned());
            }
            lit => {
                i = i.saturating_add(1);
                let (quant, next_i) = consume_quantifier(&chars, i);
                i = next_i;
                pieces.push(RegexPiece {
                    atom: RegexAtom::Literal(lit),
                    quant,
                });
            }
        }
    }
    if !group_starts.is_empty() {
        return Err("unclosed group in regular expression".to_owned());
    }
    Ok(pieces)
}

fn consume_quantifier(chars: &[char], idx: usize) -> (Quantifier, usize) {
    match chars.get(idx) {
        Some('*') => (Quantifier::ZeroOrMore, idx.saturating_add(1)),
        Some('+') => (Quantifier::OneOrMore, idx.saturating_add(1)),
        Some('?') => (Quantifier::ZeroOrOne, idx.saturating_add(1)),
        _ => (Quantifier::Once, idx),
    }
}

fn match_regex_at(
    line_chars: &[(usize, char)],
    start: usize,
    pieces: &[RegexPiece],
    case_sensitive: bool,
    whole_word: bool,
    steps: &mut usize,
) -> Option<usize> {
    let end = match_regex_pieces(line_chars, start, pieces, case_sensitive, steps)?;
    if whole_word
        && (!is_word_boundary_at(line_chars, start) || !is_word_boundary_at(line_chars, end))
    {
        return None;
    }
    Some(end)
}

fn match_regex_pieces(
    line_chars: &[(usize, char)],
    pos: usize,
    pieces: &[RegexPiece],
    case_sensitive: bool,
    steps: &mut usize,
) -> Option<usize> {
    if *steps >= MAX_REGEX_STEPS_PER_MATCH {
        return None;
    }
    *steps = steps.saturating_add(1);

    let Some(piece) = pieces.first() else {
        return Some(pos);
    };
    let rest = pieces.get(1..).unwrap_or(&[]);

    match &piece.atom {
        RegexAtom::Start => {
            if pos == 0 {
                match_regex_pieces(line_chars, pos, rest, case_sensitive, steps)
            } else {
                None
            }
        }
        RegexAtom::End => {
            if pos == line_chars.len() {
                match_regex_pieces(line_chars, pos, rest, case_sensitive, steps)
            } else {
                None
            }
        }
        RegexAtom::WordBoundary => {
            if is_word_boundary_at(line_chars, pos) {
                match_regex_pieces(line_chars, pos, rest, case_sensitive, steps)
            } else {
                None
            }
        }
        atom => match piece.quant {
            Quantifier::Once => {
                if atom_matches_char(atom, line_chars, pos, case_sensitive) {
                    match_regex_pieces(
                        line_chars,
                        pos.saturating_add(1),
                        rest,
                        case_sensitive,
                        steps,
                    )
                } else {
                    None
                }
            }
            Quantifier::ZeroOrOne => {
                if atom_matches_char(atom, line_chars, pos, case_sensitive)
                    && let Some(end) = match_regex_pieces(
                        line_chars,
                        pos.saturating_add(1),
                        rest,
                        case_sensitive,
                        steps,
                    )
                {
                    return Some(end);
                }
                match_regex_pieces(line_chars, pos, rest, case_sensitive, steps)
            }
            Quantifier::ZeroOrMore | Quantifier::OneOrMore => {
                let min_count = usize::from(piece.quant == Quantifier::OneOrMore);
                let mut max_count = 0usize;
                while atom_matches_char(
                    atom,
                    line_chars,
                    pos.saturating_add(max_count),
                    case_sensitive,
                ) {
                    max_count = max_count.saturating_add(1);
                }
                if max_count < min_count {
                    return None;
                }
                for count in (min_count..=max_count).rev() {
                    if *steps >= MAX_REGEX_STEPS_PER_MATCH {
                        return None;
                    }
                    if let Some(end) = match_regex_pieces(
                        line_chars,
                        pos.saturating_add(count),
                        rest,
                        case_sensitive,
                        steps,
                    ) {
                        return Some(end);
                    }
                }
                None
            }
        },
    }
}

fn atom_matches_char(
    atom: &RegexAtom,
    line_chars: &[(usize, char)],
    pos: usize,
    case_sensitive: bool,
) -> bool {
    let Some(&(_, ch)) = line_chars.get(pos) else {
        return false;
    };
    match atom {
        RegexAtom::Literal(expected) => chars_eq(ch, *expected, case_sensitive),
        RegexAtom::Dot => ch != '\n' && ch != '\r',
        RegexAtom::Shorthand(kind) => shorthand_matches(*kind, ch),
        RegexAtom::Class { negated, items } => {
            let matched = items.iter().any(|item| match *item {
                ClassItem::Single(c) => chars_eq(ch, c, case_sensitive),
                ClassItem::Range(lo, hi) => {
                    if case_sensitive {
                        ch >= lo && ch <= hi
                    } else {
                        let cl = ch.to_ascii_lowercase();
                        cl >= lo.to_ascii_lowercase() && cl <= hi.to_ascii_lowercase()
                    }
                }
                ClassItem::Shorthand(k) => shorthand_matches(k, ch),
            });
            if *negated { !matched } else { matched }
        }
        RegexAtom::Start | RegexAtom::End | RegexAtom::WordBoundary => false,
    }
}

fn shorthand_matches(kind: char, ch: char) -> bool {
    match kind {
        'w' => is_word_char(ch),
        'W' => !is_word_char(ch),
        'd' => ch.is_ascii_digit(),
        'D' => !ch.is_ascii_digit(),
        's' => ch.is_whitespace(),
        'S' => !ch.is_whitespace(),
        _ => ch == kind,
    }
}

#[derive(Debug)]
pub(crate) enum RgError {
    Unavailable,
    Execution(String),
}

fn find_rg_binary() -> Option<PathBuf> {
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("rg");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for fixed in [
        "/opt/homebrew/bin/rg",
        "/usr/local/bin/rg",
        "/usr/bin/rg",
        "/opt/local/bin/rg",
    ] {
        let candidate = PathBuf::from(fixed);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "Owning the task cancels its pipe IO on every return path via Drop"
)]
fn finish_search_pipe<T: Send + 'static>(
    task: crate::process_io::PipeTask<T>,
    deadline: Instant,
    cancel: &AtomicBool,
) -> std::io::Result<T> {
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        if let Some(result) = task.try_finish() {
            return result;
        }
        if Instant::now() >= deadline {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(all(test, unix))]
pub(crate) fn search_with_rg_limits(
    rg_bin: &Path,
    root: &Path,
    options: &SearchOptions,
    timeout: Duration,
    max_stdout_bytes: usize,
) -> Result<SearchResult, RgError> {
    search_with_rg_control(
        rg_bin,
        root,
        options,
        timeout,
        max_stdout_bytes,
        &AtomicBool::new(false),
    )
}

fn search_with_rg_control(
    rg_bin: &Path,
    root: &Path,
    options: &SearchOptions,
    timeout: Duration,
    max_stdout_bytes: usize,
    cancel: &AtomicBool,
) -> Result<SearchResult, RgError> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(SearchResult {
            truncated: true,
            ..SearchResult::default()
        });
    }
    let max_results = if options.max_results == 0 {
        DEFAULT_MAX_RESULTS
    } else {
        options.max_results
    };
    let max_count = max_results.saturating_add(1).to_string();
    let include_globs = parse_glob_patterns(&options.include_pattern);
    let exclude_globs = parse_glob_patterns(&options.exclude_pattern);

    let mut cmd = crate::platform::background_command(rg_bin);
    cmd.current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .args([
            "--no-config",
            "--json",
            "--line-number",
            "--column",
            "--hidden",
            "--max-filesize",
            "4M",
            "--max-count",
            &max_count,
            "--color",
            "never",
            "--glob",
            "!.git",
            "--glob",
            "!.git/**",
        ]);

    if options.case_sensitive {
        cmd.arg("--case-sensitive");
    } else {
        cmd.arg("--ignore-case");
    }
    if options.whole_word {
        cmd.arg("--word-regexp");
    }
    if !options.use_regex {
        cmd.arg("--fixed-strings");
    }
    for glob in &include_globs {
        cmd.arg("--glob").arg(glob);
        if !glob.contains('/') && !glob.contains('*') && !glob.contains('?') {
            cmd.arg("--glob").arg(format!("**/{glob}/**"));
        }
    }
    for glob in &exclude_globs {
        cmd.arg("--glob").arg(format!("!{glob}"));
        if !glob.contains('/') && !glob.contains('*') && !glob.contains('?') {
            cmd.arg("--glob").arg(format!("!**/{glob}/**"));
        }
    }
    cmd.arg("--").arg(&options.query).arg(".");

    let spawned = cmd.spawn().map_err(|_| RgError::Unavailable)?;
    let mut child = Reap(spawned);
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| RgError::Execution("missing rg stdout".to_owned()))?;

    let cap = u64::try_from(max_stdout_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let deadline = Instant::now() + timeout;
    let reader = crate::process_io::read(stdout, cap, deadline)
        .map_err(|error| RgError::Execution(error.to_string()))?;
    let mut exit_code = None;
    let mut timed_out = false;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(SearchResult {
                truncated: true,
                ..SearchResult::default()
            });
        }
        match child.0.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                timed_out = true;
                child.0.kill().ok();
                child.0.wait().ok();
                break;
            }
            Err(e) => return Err(RgError::Execution(e.to_string())),
        }
    }
    child.0.kill().ok();
    child.0.wait().ok();

    let captured = finish_search_pipe(reader, deadline + Duration::from_millis(50), cancel)
        .map_err(|error| RgError::Execution(error.to_string()))?;
    timed_out |= captured.timed_out;
    let mut output = captured.bytes;

    if exit_code == Some(2) && output.is_empty() && !timed_out {
        return Err(RgError::Execution("invalid search pattern".to_owned()));
    }

    let stdout_capped = output.len() >= max_stdout_bytes;
    if output.len() > max_stdout_bytes {
        output.truncate(max_stdout_bytes);
    }

    let mut result = parse_rg_json_lines(root, &output, max_results);
    if timed_out || stdout_capped {
        result.truncated = true;
    }
    Ok(result)
}

#[derive(serde::Deserialize)]
struct RgEnvelope<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(borrow)]
    data: Option<RgMatchData<'a>>,
}

#[derive(serde::Deserialize)]
struct RgMatchData<'a> {
    #[serde(borrow)]
    path: Option<RgText<'a>>,
    #[serde(borrow)]
    lines: Option<RgText<'a>>,
    line_number: Option<u32>,
    #[serde(default)]
    submatches: Vec<RgSubmatch>,
}

#[derive(serde::Deserialize)]
struct RgText<'a> {
    #[serde(borrow)]
    text: Option<std::borrow::Cow<'a, str>>,
}

#[derive(serde::Deserialize)]
struct RgSubmatch {
    start: usize,
    end: usize,
}

fn parse_rg_json_lines(root: &Path, stdout: &[u8], max_results: usize) -> SearchResult {
    let mut grouped: BTreeMap<String, Vec<SearchMatch>> = BTreeMap::new();
    let mut total_matches = 0usize;
    let mut truncated = false;

    for raw_line in stdout.split(|&b| b == b'\n') {
        if raw_line.is_empty() {
            continue;
        }
        let Ok(envelope) = serde_json::from_slice::<RgEnvelope<'_>>(raw_line) else {
            truncated = true;
            continue;
        };
        if envelope.kind != "match" {
            continue;
        }
        let Some(data) = envelope.data else {
            continue;
        };
        let Some(rel_raw) = data.path.and_then(|p| p.text) else {
            continue;
        };
        let Some(line_text) = data.lines.and_then(|l| l.text) else {
            continue;
        };
        let line_number = data.line_number.unwrap_or(1);
        let rel = normalize_rel_path(&rel_raw);
        if rel.is_empty() {
            continue;
        }
        for sub in data.submatches {
            if total_matches >= max_results {
                truncated = true;
                break;
            }
            let hit = match_from_byte_span(&line_text, line_number, sub.start, sub.end);
            grouped.entry(rel.clone()).or_default().push(hit);
            total_matches = total_matches.saturating_add(1);
        }
        if truncated {
            break;
        }
    }

    let files = grouped
        .into_iter()
        .map(|(relative_path, mut matches)| {
            matches.sort_by_key(|m| (m.line, m.column));
            SearchFileResult {
                path: root.join(&relative_path),
                relative_path,
                matches,
            }
        })
        .collect();

    SearchResult {
        files,
        total_matches,
        truncated,
    }
}

fn normalize_rel_path(raw: &str) -> String {
    let replaced = raw.replace('\\', "/");
    replaced
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned()
}

fn preview_window_start(col0: usize, len_chars: usize) -> usize {
    if col0.saturating_add(len_chars) <= MAX_PREVIEW_CHARS {
        0
    } else {
        col0.saturating_sub(PREVIEW_CONTEXT_BEFORE)
    }
}

fn match_from_byte_span(
    raw_line: &str,
    line_number: u32,
    byte_start: usize,
    byte_end: usize,
) -> SearchMatch {
    let clean_line = raw_line.trim_end_matches(['\r', '\n']);
    let clamped_start = byte_start.min(clean_line.len());
    let clamped_end = byte_end.max(clamped_start).min(clean_line.len());

    let mut col0 = 0usize;
    let mut len_chars = 0usize;
    for (byte_idx, _) in clean_line.char_indices() {
        if byte_idx < clamped_start {
            col0 = col0.saturating_add(1);
        } else if byte_idx < clamped_end {
            len_chars = len_chars.saturating_add(1);
        } else {
            break;
        }
    }
    let window_start = preview_window_start(col0, len_chars);
    let preview: String = clean_line
        .chars()
        .skip(window_start)
        .take(MAX_PREVIEW_CHARS)
        .map(|c| if c == '\t' || c.is_control() { ' ' } else { c })
        .collect();

    SearchMatch {
        line: line_number.max(1),
        column: u32::try_from(col0.saturating_add(1)).unwrap_or(u32::MAX),
        match_length: u32::try_from(len_chars).unwrap_or(u32::MAX),
        line_content: preview,
    }
}

fn search_worktree_fallback_with_matcher(
    root: &Path,
    options: &SearchOptions,
    matcher: &LineMatcher,
    budget: &mut SearchBudget,
) -> SearchResult {
    let max_results = if options.max_results == 0 {
        DEFAULT_MAX_RESULTS
    } else {
        options.max_results
    };
    let include_globs = parse_glob_patterns(&options.include_pattern);
    let exclude_globs = parse_glob_patterns(&options.exclude_pattern);

    let candidates = collect_candidate_files(root, MAX_FALLBACK_FILES, budget);
    let rels: Vec<String> = candidates.iter().map(|(_, rel)| rel.clone()).collect();
    let ignored_set = query_git_ignored_set_control(
        root,
        &rels,
        GIT_IGNORE_TIMEOUT.min(budget.deadline.saturating_duration_since(Instant::now())),
        MAX_GIT_IGNORE_STDOUT_BYTES,
        &budget.cancel,
    );

    let mut files = Vec::new();
    let mut total_matches = 0usize;
    let mut truncated = false;

    for (path, relative_path) in candidates {
        if budget.stopped() || budget.bytes >= MAX_FALLBACK_READ_BYTES {
            truncated = true;
            break;
        }
        if is_rel_ignored(&ignored_set, &relative_path) {
            continue;
        }
        if !include_globs.is_empty()
            && !include_globs
                .iter()
                .any(|g| glob_matches_path(g, &relative_path))
        {
            continue;
        }
        if exclude_globs
            .iter()
            .any(|g| glob_matches_path(g, &relative_path))
        {
            continue;
        }
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_SEARCH_FILE_BYTES {
            continue;
        }
        if budget.bytes.saturating_add(meta.len()) > MAX_FALLBACK_READ_BYTES {
            truncated = true;
            break;
        }
        let Ok(file) = fs::File::open(&path) else {
            continue;
        };
        let mut bytes = Vec::new();
        if file
            .take(
                (MAX_SEARCH_FILE_BYTES + 1)
                    .min(MAX_FALLBACK_READ_BYTES.saturating_sub(budget.bytes)),
            )
            .read_to_end(&mut bytes)
            .is_err()
        {
            continue;
        }
        budget.bytes = budget
            .bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if bytes.len() > usize::try_from(MAX_SEARCH_FILE_BYTES).unwrap_or(usize::MAX) {
            truncated = true;
            continue;
        }
        if bytes.iter().take(8_192).any(|&b| b == 0) {
            continue;
        }
        let Ok(content) = std::str::from_utf8(&bytes) else {
            continue;
        };

        let mut file_matches = Vec::new();
        for (line_idx, raw_line) in content.lines().enumerate() {
            if budget.stopped() {
                truncated = true;
                break;
            }
            let line_number = u32::try_from(line_idx.saturating_add(1)).unwrap_or(u32::MAX);
            for (byte_start, byte_end) in matcher.find_spans(
                raw_line,
                budget,
                max_results.saturating_sub(total_matches).saturating_add(1),
            ) {
                if total_matches >= max_results {
                    truncated = true;
                    break;
                }
                file_matches.push(match_from_byte_span(
                    raw_line,
                    line_number,
                    byte_start,
                    byte_end,
                ));
                total_matches = total_matches.saturating_add(1);
            }
            if truncated {
                break;
            }
        }
        if !file_matches.is_empty() {
            files.push(SearchFileResult {
                path,
                relative_path,
                matches: file_matches,
            });
        }
        if truncated {
            break;
        }
    }

    tracing::debug!(
        nodes = budget.nodes,
        read_bytes = budget.bytes,
        total_matches,
        truncated = truncated || budget.truncated,
        "fallback search budget"
    );
    SearchResult {
        files,
        total_matches,
        truncated: truncated || budget.truncated || budget.bytes >= MAX_FALLBACK_READ_BYTES,
    }
}

fn collect_candidate_files(
    root: &Path,
    limit: usize,
    budget: &mut SearchBudget,
) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back(root.to_path_buf());

    while let Some(dir) = queue.pop_front() {
        if budget.stopped() || out.len() >= limit {
            budget.truncated = true;
            break;
        }
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<(String, PathBuf, bool)> = Vec::new();
        for entry in rd.flatten() {
            if budget.node() {
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" || name == "node_modules" || name == "target" {
                continue;
            }
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            entries.push((name, path, ft.is_dir()));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, path, is_dir) in entries {
            if budget.stopped() || budget.truncated || out.len() >= limit {
                budget.truncated = true;
                break;
            }
            if is_dir {
                queue.push_back(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                if !rel_str.is_empty() {
                    out.push((path, rel_str));
                }
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

pub(crate) fn query_git_ignored_set_with_limits(
    repo: &Path,
    relative_paths: &[String],
    timeout: Duration,
    max_stdout_bytes: usize,
) -> BTreeSet<String> {
    query_git_ignored_set_control(
        repo,
        relative_paths,
        timeout,
        max_stdout_bytes,
        &AtomicBool::new(false),
    )
}

fn query_git_ignored_set_control(
    repo: &Path,
    relative_paths: &[String],
    timeout: Duration,
    max_stdout_bytes: usize,
    cancel: &AtomicBool,
) -> BTreeSet<String> {
    if cancel.load(Ordering::Relaxed) || timeout.is_zero() {
        return BTreeSet::new();
    }
    if relative_paths.is_empty() || !repo.join(".git").exists() {
        return BTreeSet::new();
    }
    let Ok(spawned) = crate::platform::background_command("git")
        .current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(["check-ignore", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return BTreeSet::new();
    };
    let mut child = Reap(spawned);
    let Some(stdout) = child.0.stdout.take() else {
        return BTreeSet::new();
    };
    let deadline = Instant::now() + timeout;
    let writer = child.0.stdin.take().and_then(|stdin| {
        let mut payload = Vec::new();
        for rel in relative_paths {
            payload.extend_from_slice(rel.as_bytes());
            payload.push(0);
        }
        crate::process_io::write(stdin, payload, deadline).ok()
    });
    let cap = u64::try_from(max_stdout_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let Ok(reader) = crate::process_io::read(stdout, cap, deadline) else {
        return BTreeSet::new();
    };
    loop {
        if cancel.load(Ordering::Relaxed) {
            return BTreeSet::new();
        }
        match child.0.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                child.0.kill().ok();
                child.0.wait().ok();
                break;
            }
        }
    }
    child.0.kill().ok();
    child.0.wait().ok();
    if let Some(w) = writer {
        finish_search_pipe(w, deadline + Duration::from_millis(50), cancel).ok();
    }
    let Ok(captured) = finish_search_pipe(reader, deadline + Duration::from_millis(50), cancel)
    else {
        return BTreeSet::new();
    };
    let mut stdout_bytes = captured.bytes;
    if stdout_bytes.len() > max_stdout_bytes {
        stdout_bytes.truncate(max_stdout_bytes);
        let valid_len = stdout_bytes
            .iter()
            .rposition(|&b| b == 0)
            .map_or(0, |idx| idx + 1);
        stdout_bytes.truncate(valid_len);
    }
    stdout_bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).trim_end_matches('/').to_owned())
        .collect()
}

fn is_rel_ignored(ignored: &BTreeSet<String>, relative_path: &str) -> bool {
    if ignored.contains(relative_path) {
        return true;
    }
    let mut prefix = String::new();
    for seg in relative_path.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(seg);
        if ignored.contains(&prefix) {
            return true;
        }
    }
    false
}

/// 단일 glob 패턴이 상대 경로(`src/lib.rs`)와 일치하는지 검사한다.
#[must_use]
pub fn glob_matches_path(pattern: &str, relative_path: &str) -> bool {
    let pat_chars: Vec<char> = pattern.chars().collect();
    let rel_chars: Vec<char> = relative_path.chars().collect();
    if glob_chars_match(&pat_chars, &rel_chars) {
        return true;
    }
    if !pattern.contains('/') {
        let basename = relative_path.rsplit('/').next().unwrap_or(relative_path);
        let base_chars: Vec<char> = basename.chars().collect();
        if glob_chars_match(&pat_chars, &base_chars) {
            return true;
        }
        for seg in relative_path.split('/') {
            let seg_chars: Vec<char> = seg.chars().collect();
            if glob_chars_match(&pat_chars, &seg_chars) {
                return true;
            }
        }
    } else if !pattern.contains('*') && !pattern.contains('?') {
        let dir_prefix = format!("{pattern}/");
        if relative_path.starts_with(&dir_prefix) {
            return true;
        }
    }
    false
}

fn glob_chars_match(pat: &[char], text: &[char]) -> bool {
    match pat.first() {
        None => text.is_empty(),
        Some('*') => {
            if pat.get(1) == Some(&'*') {
                if pat.get(2) == Some(&'/') {
                    let rest_pat = pat.get(3..).unwrap_or(&[]);
                    if glob_chars_match(rest_pat, text) {
                        return true;
                    }
                    for (i, ch) in text.iter().enumerate() {
                        if *ch == '/' {
                            let after_slash = text.get(i + 1..).unwrap_or(&[]);
                            if glob_chars_match(rest_pat, after_slash) {
                                return true;
                            }
                        }
                    }
                    false
                } else {
                    let rest_pat = pat.get(2..).unwrap_or(&[]);
                    if rest_pat.is_empty() {
                        return true;
                    }
                    for i in 0..=text.len() {
                        if glob_chars_match(rest_pat, text.get(i..).unwrap_or(&[])) {
                            return true;
                        }
                    }
                    false
                }
            } else {
                let rest_pat = pat.get(1..).unwrap_or(&[]);
                if glob_chars_match(rest_pat, text) {
                    return true;
                }
                for (i, ch) in text.iter().enumerate() {
                    if *ch == '/' {
                        break;
                    }
                    if glob_chars_match(rest_pat, text.get(i + 1..).unwrap_or(&[])) {
                        return true;
                    }
                }
                false
            }
        }
        Some('?') => {
            let Some(first_text) = text.first() else {
                return false;
            };
            if *first_text == '/' {
                return false;
            }
            glob_chars_match(pat.get(1..).unwrap_or(&[]), text.get(1..).unwrap_or(&[]))
        }
        Some(c) => {
            let Some(first_text) = text.first() else {
                return false;
            };
            if c != first_text {
                return false;
            }
            glob_chars_match(pat.get(1..).unwrap_or(&[]), text.get(1..).unwrap_or(&[]))
        }
    }
}

/// 줄 미리보기 텍스트를 `(앞부분, 일치 부분, 뒷부분)`으로 문자(char) 인덱스 기준으로 분리한다.
#[must_use]
pub fn split_highlighted_preview(
    line_content: &str,
    column: u32,
    match_length: u32,
) -> (String, String, String) {
    let col0 = usize::try_from(column.saturating_sub(1)).unwrap_or(0);
    let len_chars = usize::try_from(match_length).unwrap_or(0);
    let line_char_count = line_content.chars().count();
    let window_start = if line_char_count <= MAX_PREVIEW_CHARS {
        preview_window_start(col0, len_chars)
    } else {
        0
    };
    let start_char = col0.saturating_sub(window_start);
    let end_char = start_char.saturating_add(len_chars);

    let mut before = String::new();
    let mut matched = String::new();
    let mut after = String::new();

    for (idx, ch) in line_content.chars().enumerate() {
        if idx < start_char {
            before.push(ch);
        } else if idx < end_char {
            matched.push(ch);
        } else {
            after.push(ch);
        }
    }
    let trimmed_before = before.trim_start().to_owned();
    (trimmed_before, matched, after)
}

fn char_display_cols(ch: char) -> usize {
    if ch.is_ascii() { 1 } else { 2 }
}

fn str_display_cols(s: &str) -> usize {
    s.chars().map(char_display_cols).sum()
}

fn elide_start_to_cols(s: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    if max_cols == 1 {
        return "…".to_owned();
    }
    let tail_budget = max_cols.saturating_sub(1);
    let mut used = 0usize;
    let mut tail_chars: Vec<char> = Vec::new();
    for ch in s.chars().rev() {
        let w = char_display_cols(ch);
        if used.saturating_add(w) > tail_budget {
            break;
        }
        used = used.saturating_add(w);
        tail_chars.push(ch);
    }
    tail_chars.reverse();
    let mut out = String::with_capacity(tail_chars.len().saturating_add('…'.len_utf8()));
    out.push('…');
    for ch in tail_chars {
        out.push(ch);
    }
    out
}

fn truncate_end_to_cols(s: &str, max_cols: usize) -> String {
    if max_cols == 0 {
        return String::new();
    }
    if str_display_cols(s) <= max_cols {
        return s.to_owned();
    }
    if max_cols == 1 {
        return "…".to_owned();
    }
    let head_budget = max_cols.saturating_sub(1);
    let mut used = 0usize;
    let mut out = String::new();
    for ch in s.chars() {
        let w = char_display_cols(ch);
        if used.saturating_add(w) > head_budget {
            break;
        }
        used = used.saturating_add(w);
        out.push(ch);
    }
    out.push('…');
    out
}

/// 현재 행 폭(`row_width_px`)과 고정폭 글꼴 문자 폭(`cell_width_px`)으로 매치 미리보기에
/// 사용할 수 있는 최대 고정폭 열 수를 계산한다.
#[must_use]
pub fn max_cols_for_row_width(row_width_px: f32, cell_width_px: f32) -> usize {
    let effective_cell = if cell_width_px.is_finite() && cell_width_px > 1.0 {
        cell_width_px
    } else {
        7.0
    };
    let text_px = (row_width_px - MATCH_ROW_CHROME_PX).max(0.0);
    let raw_cols = (text_px / effective_cell).floor();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "0 이상 유한 부동소수점을 usize로 변환"
    )]
    let cols = if raw_cols.is_finite() && raw_cols >= 0.0 {
        raw_cols as usize
    } else {
        MIN_MATCH_PREVIEW_COLS
    };
    cols.max(MIN_MATCH_PREVIEW_COLS)
}

/// 현재 행에서 사용 가능한 고정폭 열(`max_cols`) 예산에 맞춰 `(앞 문맥, 일치 구간, 뒤 문맥)`을
/// 조정한다. 앞 문맥이 예산을 넘거나 원본 줄에서 앞부분이 이미 잘린 경우(`window_start > 0`)
/// 앞부분을 `…`로 생략해 일치 강조(`matched`)가 항상 패널 폭 안에 노출되도록 보장한다.
#[must_use]
pub fn fit_highlighted_preview_for_cols(
    line_content: &str,
    column: u32,
    match_length: u32,
    max_cols: usize,
) -> (String, String, String) {
    let budget = max_cols.max(MIN_MATCH_PREVIEW_COLS);
    let (before, matched, after) = split_highlighted_preview(line_content, column, match_length);
    let col0 = usize::try_from(column.saturating_sub(1)).unwrap_or(0);
    let len_chars = usize::try_from(match_length).unwrap_or(0);
    let pre_windowed = line_content.chars().count() <= MAX_PREVIEW_CHARS
        && preview_window_start(col0, len_chars) > 0;

    let before_cols = str_display_cols(&before);
    let matched_cols = str_display_cols(&matched);
    let after_cols = str_display_cols(&after);

    if !pre_windowed
        && before_cols
            .saturating_add(matched_cols)
            .saturating_add(after_cols.min(AFTER_CONTEXT_RESERVE_COLS))
            <= budget
    {
        return (before, matched, after);
    }

    if matched_cols >= budget {
        if !before.is_empty() && budget > 6 {
            let fitted_matched = truncate_end_to_cols(&matched, budget.saturating_sub(1));
            return ("…".to_owned(), fitted_matched, String::new());
        }
        let fitted_matched = truncate_end_to_cols(&matched, budget);
        return (String::new(), fitted_matched, String::new());
    }

    if before.is_empty() {
        return (before, matched, after);
    }

    let remaining = budget.saturating_sub(matched_cols);
    let right_reserve = after_cols
        .clamp(2, AFTER_CONTEXT_RESERVE_COLS)
        .min(remaining / 2);
    let before_budget = remaining.saturating_sub(right_reserve);

    let fitted_before = if before_cols > before_budget {
        elide_start_to_cols(&before, before_budget)
    } else if pre_windowed {
        if before_cols.saturating_add(1) <= before_budget {
            format!("…{before}")
        } else {
            elide_start_to_cols(&before, before_budget)
        }
    } else {
        before
    };

    (fitted_before, matched, after)
}

impl Panel {
    /// `Search` 탭으로 전환하고 검색어 입력창에 포커스를 준다.
    pub fn show_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_tab(super::PanelTab::Search, cx);
        let input = self.ensure_search_query_input(window, cx);
        let _ = self.ensure_search_include_input(window, cx);
        let _ = self.ensure_search_exclude_input(window, cx);
        input.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    /// 현재 워크스페이스 검색 옵션.
    #[must_use]
    pub fn search_options(&self) -> &SearchOptions {
        &self.search.options
    }

    /// 현재 워크스페이스 검색 결과.
    #[must_use]
    pub fn search_result(&self) -> &SearchResult {
        &self.search.result
    }

    /// 현재 워크스페이스 검색 평탄화 행 목록(`uniform_list` 모델).
    #[must_use]
    pub fn search_rows(&self) -> &[SearchRow] {
        &self.search.rows
    }

    /// 검색어를 설정하고 비동기 검색을 갱신한다.
    pub fn set_search_query(&mut self, query: impl Into<String>, cx: &mut Context<Self>) {
        self.search.options.query = query.into();
        self.spawn_async_search(cx);
    }

    /// 포함 glob 필터(`files to include`)를 설정하고 비동기 검색을 갱신한다.
    pub fn set_search_include(&mut self, include: impl Into<String>, cx: &mut Context<Self>) {
        self.search.options.include_pattern = include.into();
        self.spawn_async_search(cx);
    }

    /// 제외 glob 필터(`files to exclude`)를 설정하고 비동기 검색을 갱신한다.
    pub fn set_search_exclude(&mut self, exclude: impl Into<String>, cx: &mut Context<Self>) {
        self.search.options.exclude_pattern = exclude.into();
        self.spawn_async_search(cx);
    }

    /// 대소문자 구분(`Aa`) 옵션을 토글하고 비동기 검색을 갱신한다.
    pub fn toggle_search_case_sensitive(&mut self, cx: &mut Context<Self>) {
        self.search.options.case_sensitive = !self.search.options.case_sensitive;
        self.spawn_async_search(cx);
    }

    /// 단어 단위 일치(`ab`) 옵션을 토글하고 비동기 검색을 갱신한다.
    pub fn toggle_search_whole_word(&mut self, cx: &mut Context<Self>) {
        self.search.options.whole_word = !self.search.options.whole_word;
        self.spawn_async_search(cx);
    }

    /// 정규식(`.*`) 옵션을 토글하고 비동기 검색을 갱신한다.
    pub fn toggle_search_regex(&mut self, cx: &mut Context<Self>) {
        self.search.options.use_regex = !self.search.options.use_regex;
        self.spawn_async_search(cx);
    }

    /// 특정 파일 그룹의 접기/펼치기를 토글한다.
    pub fn toggle_search_file_collapse(&mut self, relative_path: &str, cx: &mut Context<Self>) {
        self.search.toggle_file_collapse(relative_path);
        cx.notify();
    }

    /// 모든 검색 결과 파일 그룹을 접거나 펼친다.
    pub fn collapse_all_search_files(&mut self, cx: &mut Context<Self>) {
        self.search.collapse_all();
        cx.notify();
    }

    /// 검색 결과 행 인덱스를 클릭 처리한다(파일 헤더면 접기/펼치기, 매치 행이면 `OpenFileAt` 발행).
    pub fn activate_search_row(&mut self, row_idx: usize, cx: &mut Context<Self>) {
        let Some(row) = self.search.rows.get(row_idx).cloned() else {
            return;
        };
        match row {
            SearchRow::FileHeader { relative_path, .. } => {
                self.toggle_search_file_collapse(&relative_path, cx);
            }
            SearchRow::Match { path, hit, .. } => {
                self.search.selected_row = Some(row_idx);
                cx.emit(OpenFileAt {
                    path,
                    line: hit.line,
                    column: hit.column,
                });
                cx.notify();
            }
        }
    }

    pub(crate) fn spawn_async_search(&mut self, cx: &mut Context<Self>) {
        self.schedule_async_search(false, cx);
    }

    pub(crate) fn schedule_async_search(&mut self, debounce: bool, cx: &mut Context<Self>) {
        self.search.cancel = None;
        self.search.generation = self.search.generation.wrapping_add(1);
        let generation = self.search.generation;
        if self.search.options.query.is_empty() {
            self.search.searching = false;
            self.search.search_task = None;
            self.search.result = SearchResult::default();
            self.search.collapsed_files.clear();
            self.search.rows.clear();
            self.search.selected_row = None;
            self.search.error = None;
            cx.notify();
            return;
        }
        self.search.searching = true;
        self.search.error = None;
        let cancel = Arc::new(AtomicBool::new(false));
        self.search.cancel = Some(SearchCancel(Arc::clone(&cancel)));
        let repo = self.search.repo.clone();
        let options = self.search.options.clone();
        self.search.search_task = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(SEARCH_DEBOUNCE).await;
                // 대기 중 새 입력이 들어왔으면 `rg`를 스폰하지 않고 조용히 끝낸다.
                let current = this.update(cx, |this, _| this.search.generation).ok();
                if current != Some(generation) {
                    return;
                }
            }
            let outcome = cx
                .background_executor()
                .spawn(async move { search_worktree_control(&repo, &options, cancel) })
                .await;
            this.update(cx, |this, cx| {
                if this.search.generation != generation {
                    return;
                }
                this.search.searching = false;
                match outcome {
                    Ok(res) => {
                        this.search.error = None;
                        this.search.result = res;
                        this.search.collapsed_files.retain(|rel| {
                            this.search
                                .result
                                .files
                                .iter()
                                .any(|f| &f.relative_path == rel)
                        });
                        this.search.selected_row = None;
                        this.search.rebuild_rows();
                    }
                    Err(err) => {
                        this.search.error = Some(err);
                        this.search.result = SearchResult::default();
                        this.search.rows.clear();
                        this.search.selected_row = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn ensure_search_query_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = &self.search.query_input {
            return input.clone();
        }
        let initial = self.search.options.query.clone();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(text::SEARCH_PLACEHOLDER.get())
                .default_value(initial)
                .clean_on_escape()
        });
        let sub = cx.subscribe(&input, |this, state, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                let val = state.read(cx).value().to_string();
                if this.search.options.query != val {
                    this.search.options.query = val;
                    this.schedule_async_search(true, cx);
                }
            }
            InputEvent::PressEnter { .. } => {
                let val = state.read(cx).value().to_string();
                this.search.options.query = val;
                this.schedule_async_search(false, cx);
            }
            _ => {}
        });
        self.search.query_input = Some(input.clone());
        self.search.query_sub = Some(sub);
        input
    }

    fn ensure_search_include_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = &self.search.include_input {
            return input.clone();
        }
        let initial = self.search.options.include_pattern.clone();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(text::SEARCH_INCLUDE_PLACEHOLDER.get())
                .default_value(initial)
                .clean_on_escape()
        });
        let sub = cx.subscribe(&input, |this, state, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                let val = state.read(cx).value().to_string();
                if this.search.options.include_pattern != val {
                    this.search.options.include_pattern = val;
                    this.schedule_async_search(true, cx);
                }
            }
            InputEvent::PressEnter { .. } => {
                let val = state.read(cx).value().to_string();
                this.search.options.include_pattern = val;
                this.schedule_async_search(false, cx);
            }
            _ => {}
        });
        self.search.include_input = Some(input.clone());
        self.search.include_sub = Some(sub);
        input
    }

    fn ensure_search_exclude_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = &self.search.exclude_input {
            return input.clone();
        }
        let initial = self.search.options.exclude_pattern.clone();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(text::SEARCH_EXCLUDE_PLACEHOLDER.get())
                .default_value(initial)
                .clean_on_escape()
        });
        let sub = cx.subscribe(&input, |this, state, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                let val = state.read(cx).value().to_string();
                if this.search.options.exclude_pattern != val {
                    this.search.options.exclude_pattern = val;
                    this.schedule_async_search(true, cx);
                }
            }
            InputEvent::PressEnter { .. } => {
                let val = state.read(cx).value().to_string();
                this.search.options.exclude_pattern = val;
                this.schedule_async_search(false, cx);
            }
            _ => {}
        });
        self.search.exclude_input = Some(input.clone());
        self.search.exclude_sub = Some(sub);
        input
    }

    pub(crate) fn render_search_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query_input = self.ensure_search_query_input(window, cx);
        let include_input = self.ensure_search_include_input(window, cx);
        let exclude_input = self.ensure_search_exclude_input(window, cx);

        let p = ui::palette(cx);
        let case_sensitive = self.search.options.case_sensitive;
        let whole_word = self.search.options.whole_word;
        let use_regex = self.search.options.use_regex;
        let has_query = !self.search.options.query.is_empty();
        let file_count = self.search.result.files.len();
        let match_count = self.search.result.total_matches;
        let truncated = self.search.result.truncated;
        let row_count = self.search.rows.len();

        let query_row = div()
            .id("search-toolbar")
            .debug_selector(|| "search-toolbar".into())
            .flex_none()
            .px_2()
            .pt_1p5()
            .pb_1()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .child(
                div().flex_1().min_w(px(112.)).child(
                    Input::new(&query_input)
                        .xsmall()
                        .prefix(Icon::new(IconName::Search).xsmall())
                        .cleanable(true),
                ),
            )
            .child(
                Button::new("search-toggle-case")
                    .debug_selector(|| "search-toggle-case".into())
                    .label("Aa")
                    .ghost()
                    .xsmall()
                    .selected(case_sensitive)
                    .tooltip(text::SEARCH_CASE_SENSITIVE.get())
                    .accessibility_label(text::SEARCH_CASE_SENSITIVE.get())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_search_case_sensitive(cx);
                    })),
            )
            .child(
                Button::new("search-toggle-word")
                    .debug_selector(|| "search-toggle-word".into())
                    .label("ab")
                    .ghost()
                    .xsmall()
                    .selected(whole_word)
                    .tooltip(text::SEARCH_WHOLE_WORD.get())
                    .accessibility_label(text::SEARCH_WHOLE_WORD.get())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_search_whole_word(cx);
                    })),
            )
            .child(
                Button::new("search-toggle-regex")
                    .debug_selector(|| "search-toggle-regex".into())
                    .label(".*")
                    .ghost()
                    .xsmall()
                    .selected(use_regex)
                    .tooltip(text::SEARCH_REGEX.get())
                    .accessibility_label(text::SEARCH_REGEX.get())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_search_regex(cx);
                    })),
            )
            .child(
                Button::new("search-collapse-all")
                    .debug_selector(|| "search-collapse-all".into())
                    .icon(IconName::ChevronRight)
                    .ghost()
                    .xsmall()
                    .tooltip(text::EXPLORER_COLLAPSE_ALL.get())
                    .accessibility_label(text::EXPLORER_COLLAPSE_ALL.get())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.collapse_all_search_files(cx);
                    })),
            );

        let filters_box = div()
            .id("search-filters")
            .debug_selector(|| "search-filters".into())
            .flex_none()
            .px_2()
            .pb_1p5()
            .flex()
            .flex_col()
            .gap_1()
            .border_b_1()
            .border_color(p.sidebar_border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .w(px(68.))
                            .flex_none()
                            .truncate()
                            .text_size(ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(text::SEARCH_INCLUDE_LABEL.get()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&include_input).xsmall().cleanable(true)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(
                        div()
                            .w(px(68.))
                            .flex_none()
                            .truncate()
                            .text_size(ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .child(text::SEARCH_EXCLUDE_LABEL.get()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&exclude_input).xsmall().cleanable(true)),
                    ),
            );

        let summary_bar = (has_query && (file_count > 0 || truncated)).then(|| {
            let msg = if truncated {
                text::SEARCH_TRUNCATED.format(&[
                    ("files", &file_count.to_string()),
                    ("matches", &match_count.to_string()),
                ])
            } else {
                text::SEARCH_SUMMARY.format(&[
                    ("files", &file_count.to_string()),
                    ("matches", &match_count.to_string()),
                ])
            };
            div()
                .id("search-summary")
                .debug_selector(|| "search-summary".into())
                .flex_none()
                .px_2p5()
                .py_1()
                .border_b_1()
                .border_color(p.sidebar_border)
                .text_size(ui::TEXT_2XS)
                .text_color(if truncated {
                    p.warning
                } else {
                    p.muted_foreground
                })
                .child(msg)
        });

        let error_bar = self.search.error.as_ref().map(|msg| {
            div()
                .flex_none()
                .p_2()
                .child(ui::error_banner(p, msg.clone()))
        });

        let body = if !has_query {
            Self::render_empty_state(
                "search-empty-idle",
                IconName::Search,
                text::SEARCH_IDLE.get(),
                "",
                cx,
            )
            .into_any_element()
        } else if self.search.searching && row_count == 0 {
            Self::render_empty_state(
                "search-loading",
                IconName::Search,
                text::SEARCH_LOADING.get(),
                "",
                cx,
            )
            .into_any_element()
        } else if row_count == 0 {
            Self::render_empty_state(
                "search-empty-no-results",
                IconName::Search,
                text::SEARCH_NO_RESULTS.get(),
                "",
                cx,
            )
            .into_any_element()
        } else {
            let scroll = self.search.scroll.clone();
            uniform_list(
                "search-results",
                row_count,
                cx.processor(Self::render_search_rows),
            )
            .track_scroll(&scroll)
            .size_full()
            .into_any_element()
        };

        div()
            .id("search-pane")
            .debug_selector(|| "search-pane".into())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(query_row)
            .child(filters_box)
            .children(summary_bar)
            .children(error_bar)
            .child(
                div()
                    .id("search-results-container")
                    .flex_1()
                    .min_h_0()
                    .child(body),
            )
    }

    fn search_row_max_cols(&self, window: &Window) -> usize {
        let list_width_px = self
            .search
            .scroll
            .0
            .borrow()
            .last_item_size
            .map(|sz| f32::from(sz.item.width))
            .filter(|&w| w.is_finite() && w > 0.0)
            .unwrap_or(DEFAULT_SEARCH_ROW_WIDTH_PX);
        let font_size = ui::TEXT_XS.to_pixels(window.rem_size());
        let font_id = window
            .text_system()
            .resolve_font(&gpui::font(ui::FONT_MONO));
        let cell_width_px = window
            .text_system()
            .advance(font_id, font_size, 'm')
            .map_or(f32::from(font_size) * 0.62, |s| f32::from(s.width))
            .max(4.0);
        max_cols_for_row_width(list_width_px, cell_width_px)
    }

    fn render_search_rows(
        &mut self,
        range: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        #[cfg(test)]
        {
            self.search.rendered_rows = range.len();
        }
        let max_cols = self.search_row_max_cols(window);
        let p = ui::palette(cx);
        let selected_bg = p.sidebar_accent;
        let hover_bg = p.sidebar_accent;
        let highlight_bg = p.warning.opacity(0.3);
        let fg = p.sidebar_foreground;
        let muted = p.muted_foreground;

        range
            .filter_map(|ix| {
                let row = self.search.rows.get(ix)?.clone();
                let is_selected = self.search.selected_row == Some(ix);
                let weak = cx.weak_entity();
                let row_selector = format!("search-row-{ix}");

                match row {
                    SearchRow::FileHeader {
                        path,
                        relative_path,
                        file_name,
                        dir_path,
                        match_count,
                        collapsed,
                        ..
                    } => {
                        let file_selector = format!("search-file-{relative_path}");
                        let rel_for_click = relative_path.clone();
                        let menu_path = path.clone();
                        let menu_rel = relative_path.clone();

                        Some(
                            div()
                                .id(ElementId::Name(SharedString::from(row_selector.clone())))
                                .debug_selector(move || row_selector.clone())
                                .h(px(ROW_HEIGHT))
                                .w_full()
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .cursor_pointer()
                                .text_color(if is_selected {
                                    p.sidebar_accent_foreground
                                } else {
                                    fg
                                })
                                .when(is_selected, |d| d.bg(selected_bg))
                                .when(!is_selected, |d| {
                                    d.hover(move |s| {
                                        s.bg(hover_bg).text_color(p.sidebar_accent_foreground)
                                    })
                                })
                                .child(
                                    Icon::new(if collapsed {
                                        IconName::ChevronRight
                                    } else {
                                        IconName::ChevronDown
                                    })
                                    .xsmall()
                                    .text_color(muted),
                                )
                                .child(Icon::new(IconName::File).xsmall().text_color(muted))
                                .child(
                                    div()
                                        .id(ElementId::Name(SharedString::from(
                                            file_selector.clone(),
                                        )))
                                        .debug_selector(move || file_selector.clone())
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .overflow_hidden()
                                        .child(
                                            div()
                                                .flex_none()
                                                .min_w_0()
                                                .truncate()
                                                .text_size(ui::TEXT_SM)
                                                .font_weight(FontWeight::MEDIUM)
                                                .font_family(ui::FONT_MONO)
                                                .text_color(if is_selected {
                                                    p.sidebar_accent_foreground
                                                } else {
                                                    fg
                                                })
                                                .child(file_name),
                                        )
                                        .when(!dir_path.is_empty(), |d| {
                                            d.child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .font_family(ui::FONT_MONO)
                                                    .text_size(ui::TEXT_SM)
                                                    .text_color(muted)
                                                    .child(dir_path),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .px_1()
                                        .rounded(ui::RADIUS_XS)
                                        .bg(p.muted)
                                        .font_family(ui::FONT_MONO)
                                        .text_size(ui::TEXT_2XS)
                                        .text_color(muted)
                                        .child(match_count.to_string()),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.toggle_search_file_collapse(&rel_for_click, cx);
                                    }),
                                )
                                .context_menu(move |menu, _, _| {
                                    file_header_context_menu(menu, &weak, &menu_path, &menu_rel)
                                })
                                .into_any_element(),
                        )
                    }
                    SearchRow::Match {
                        path,
                        relative_path,
                        hit,
                        ..
                    } => {
                        let match_selector =
                            format!("search-match-{relative_path}-{}-{}", hit.line, hit.column);
                        let highlight_selector = format!(
                            "search-highlight-{relative_path}-{}-{}",
                            hit.line, hit.column
                        );
                        let (before, matched, after) = fit_highlighted_preview_for_cols(
                            &hit.line_content,
                            hit.column,
                            hit.match_length,
                            max_cols,
                        );
                        let open_path = path.clone();
                        let line = hit.line;
                        let column = hit.column;
                        let menu_path = path.clone();
                        let menu_rel = relative_path.clone();

                        Some(
                            div()
                                .id(ElementId::Name(SharedString::from(row_selector.clone())))
                                .debug_selector(move || row_selector.clone())
                                .h(px(ROW_HEIGHT))
                                .w_full()
                                .pl(px(22.))
                                .pr_2()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .cursor_pointer()
                                .overflow_hidden()
                                .text_color(if is_selected {
                                    p.sidebar_accent_foreground
                                } else {
                                    fg
                                })
                                .when(is_selected, |d| d.bg(selected_bg))
                                .when(!is_selected, |d| {
                                    d.hover(move |s| {
                                        s.bg(hover_bg).text_color(p.sidebar_accent_foreground)
                                    })
                                })
                                .child(
                                    div()
                                        .w(px(28.))
                                        .flex_none()
                                        .text_right()
                                        .font_family(ui::FONT_MONO)
                                        .text_size(ui::TEXT_2XS)
                                        .text_color(muted)
                                        .child(line.to_string()),
                                )
                                .child(
                                    div()
                                        .id(ElementId::Name(SharedString::from(
                                            match_selector.clone(),
                                        )))
                                        .debug_selector(move || match_selector.clone())
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .items_center()
                                        .overflow_hidden()
                                        .font_family(ui::FONT_MONO)
                                        .text_size(ui::TEXT_SM)
                                        .text_color(if is_selected {
                                            p.sidebar_accent_foreground
                                        } else {
                                            fg
                                        })
                                        .when(!before.is_empty(), |d| {
                                            d.child(div().flex_none().child(before))
                                        })
                                        .when(!matched.is_empty(), |d| {
                                            d.child(
                                                div()
                                                    .id(ElementId::Name(SharedString::from(
                                                        highlight_selector.clone(),
                                                    )))
                                                    .debug_selector(move || {
                                                        highlight_selector.clone()
                                                    })
                                                    .flex_none()
                                                    .px_0p5()
                                                    .rounded(ui::RADIUS_XS)
                                                    .bg(highlight_bg)
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(matched),
                                            )
                                        })
                                        .when(!after.is_empty(), |d| {
                                            d.child(
                                                div().flex_1().min_w_0().truncate().child(after),
                                            )
                                        }),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.search.selected_row = Some(ix);
                                        cx.emit(OpenFileAt {
                                            path: open_path.clone(),
                                            line,
                                            column,
                                        });
                                        cx.notify();
                                    }),
                                )
                                .context_menu(move |menu, _, _| {
                                    match_row_context_menu(
                                        menu, &weak, &menu_path, &menu_rel, line, column,
                                    )
                                })
                                .into_any_element(),
                        )
                    }
                }
            })
            .collect()
    }
}

fn file_header_context_menu(
    menu: PopupMenu,
    weak: &gpui::WeakEntity<Panel>,
    path: &Path,
    relative_path: &str,
) -> PopupMenu {
    let (w_open, open_path) = (weak.clone(), path.to_path_buf());
    let abs_str = path.to_string_lossy().into_owned();
    let rel_str = relative_path.to_owned();
    let reveal_path = path.to_path_buf();

    menu.item(
        PopupMenuItem::new(text::OPEN_IN_EDITOR.get())
            .icon(IconName::FileText)
            .on_click(move |_, _, cx| {
                w_open
                    .update(cx, |_, cx| cx.emit(OpenFile(open_path.clone())))
                    .ok();
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(text::EXPLORER_COPY_PATH.get())
            .icon(IconName::Copy)
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(abs_str.clone()));
            }),
    )
    .item(
        PopupMenuItem::new(text::EXPLORER_COPY_REL_PATH.get())
            .icon(IconName::Copy)
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(rel_str.clone()));
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(
            text::EXPLORER_REVEAL_FINDER
                .mac_or(crate::i18n::catalog::other_os::REVEAL)
                .get(),
        )
        .icon(IconName::FolderOpen)
        .on_click(move |_, _, cx| {
            cx.reveal_path(&reveal_path);
        }),
    )
}

fn match_row_context_menu(
    menu: PopupMenu,
    weak: &gpui::WeakEntity<Panel>,
    path: &Path,
    relative_path: &str,
    line: u32,
    column: u32,
) -> PopupMenu {
    let (w_open, open_path) = (weak.clone(), path.to_path_buf());
    let line_path = format!("{relative_path}:{line}");
    let abs_str = path.to_string_lossy().into_owned();
    let rel_str = relative_path.to_owned();

    menu.item(
        PopupMenuItem::new(text::OPEN_IN_EDITOR.get())
            .icon(IconName::FileText)
            .on_click(move |_, _, cx| {
                w_open
                    .update(cx, |_, cx| {
                        cx.emit(OpenFileAt {
                            path: open_path.clone(),
                            line,
                            column,
                        });
                    })
                    .ok();
            }),
    )
    .separator()
    .item(
        PopupMenuItem::new(text::SEARCH_COPY_LINE_PATH.get())
            .icon(IconName::Copy)
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(line_path.clone()));
            }),
    )
    .item(
        PopupMenuItem::new(text::EXPLORER_COPY_PATH.get())
            .icon(IconName::Copy)
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(abs_str.clone()));
            }),
    )
    .item(
        PopupMenuItem::new(text::EXPLORER_COPY_REL_PATH.get())
            .icon(IconName::Copy)
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(rel_str.clone()));
            }),
    )
}

#[cfg(test)]
mod performance_tests {
    use super::*;

    #[test]
    fn cancelled_rg_does_not_spawn_even_a_missing_binary() {
        let result = search_with_rg_control(
            Path::new("/missing-rg"),
            Path::new("/repo"),
            &SearchOptions {
                query: "needle".into(),
                ..SearchOptions::default()
            },
            Duration::from_secs(5),
            1_024,
            &AtomicBool::new(true),
        )
        .unwrap();
        assert!(result.truncated);
        assert!(result.files.is_empty());
    }

    #[test]
    fn fallback_node_and_deadline_budgets_stop_before_more_io() {
        let mut budget = SearchBudget::new(Arc::new(AtomicBool::new(false)));
        budget.nodes = 50_000;
        assert!(budget.node());
        assert!(budget.truncated);
        let mut budget = SearchBudget::new(Arc::new(AtomicBool::new(false)));
        budget.deadline = Instant::now();
        assert!(
            collect_candidate_files(Path::new("/missing-root"), 10_000, &mut budget).is_empty()
        );
        assert!(budget.truncated);
    }

    #[test]
    fn cancellation_stops_matching_inside_a_long_line() {
        let matcher = LineMatcher::compile(&SearchOptions {
            query: "needle".into(),
            ..SearchOptions::default()
        })
        .unwrap();
        let mut budget = SearchBudget::new(Arc::new(AtomicBool::new(true)));
        assert!(
            matcher
                .find_spans(&"a".repeat(100_000), &mut budget, 100)
                .is_empty()
        );
        assert!(budget.truncated);
    }
}

#[cfg(all(test, unix))]
mod cancelled_pipe_tests {
    use super::*;
    #[test]
    fn cancellation_after_leader_exit_does_not_wait_for_an_inherited_pipe_deadline() {
        let (read, _held_writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let task = crate::process_io::read(read, 1024, deadline).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancelled);
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            flag.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let result = finish_search_pipe(task, deadline, &cancelled);
        trigger.join().unwrap();
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
