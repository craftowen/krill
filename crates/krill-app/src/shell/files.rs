//! MK-119: 가운데 영역 파일(에디터) 탭의 셸 통합 — 파일 칸 그리기·⌘S·⌘P·디스크 감시·미저장 닫기/종료 확인.
//! 상태기계와 디스크 IO는 [`krill_app::editor`](Orca 참조 출처도 그쪽 문서). 파일 칸은 세션에 붙지 않는다.
//! MK-121: 분류([`editor::open`])에 따라 코드 에디터·편집/미리보기 토글(마크다운·CSV·ipynb·SVG)·이미지·표·자리표시자를 고른다.

use std::path::Path;

use gpui::{Focusable as _, PromptLevel};
use gpui_component::Icon;
use gpui_component::input::{Editor, EditorState, InputEvent};
use krill_app::editor::{self, Disk, Doc, Opened, Poll, Preview, SaveError, Unopenable};
use krill_app::file_format::view::{self as fview, CsvView, ImageView};
use krill_app::file_format::{Kind, csv, ipynb};
use krill_app::i18n::catalog::artifacts as atext;
use krill_app::i18n::catalog::editor as etext;
use krill_app::i18n::catalog::file_format as ftext;
use krill_app::terminal::Highlight;
use krill_app::terminal::path_link::{self, PathLink};

use super::*;

/// 열린 파일의 디스크 지문을 보는 간격.
// ponytail: stat 폴링(파일 수 × 1Hz). 탭이 수백 개가 되면 FSEvents 감시로.
const WATCH_EVERY: Duration = Duration::from_secs(1);
const FILE_REQUESTS_FILE: &str = "file-requests.json";
const FILE_REQUESTS_LOCK: &str = "file-requests.lock";
const MAX_REQUEST_FILE_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CliFileActionKind {
    Open,
    Diff,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub(super) struct CliFileRequest {
    #[serde(default)]
    pub id: u64,
    pub kind: CliFileActionKind,
    pub worktree: PathBuf,
    pub path: PathBuf,
    pub relative_path: String,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
    #[serde(default)]
    pub staged: bool,
    #[serde(default)]
    pub focus: bool,
}

#[derive(Debug, Default, serde::Deserialize)]
struct CliFileRequestQueue {
    #[serde(default)]
    requests: Vec<CliFileRequest>,
}

fn take_pending_file_requests(state_dir: &Path) -> Vec<CliFileRequest> {
    use std::io::Read as _;
    let file_path = state_dir.join(FILE_REQUESTS_FILE);
    if !matches!(file_path.try_exists(), Ok(true)) {
        return Vec::new();
    }
    let lock_path = state_dir.join(FILE_REQUESTS_LOCK);
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
    else {
        return Vec::new();
    };
    // CLI가 쓰는 중이면 큐를 그대로 두고 다음 감시 주기에 다시 시도한다.
    // 잠금을 기다리면 파일 요청 하나가 다른 창의 요청 소비까지 막을 수 있다.
    if lock.try_lock().is_err() {
        return Vec::new();
    }
    let Ok(file) = std::fs::File::open(&file_path) else {
        lock.unlock().ok();
        return Vec::new();
    };
    let mut buf = Vec::new();
    let read_ok = file
        .take(MAX_REQUEST_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .is_ok();
    std::fs::remove_file(&file_path).ok();
    lock.unlock().ok();
    if !read_ok || u64::try_from(buf.len()).unwrap_or(u64::MAX) > MAX_REQUEST_FILE_BYTES {
        return Vec::new();
    }
    serde_json::from_slice::<CliFileRequestQueue>(&buf)
        .map(|q| q.requests)
        .unwrap_or_default()
}

/// The app owns queued disk writes even when the originating tab/window goes away.
#[derive(Default)]
struct PendingEditorWrites(Option<tokio::sync::oneshot::Receiver<()>>);
impl gpui::Global for PendingEditorWrites {}

fn editor_write_queue(cx: &mut App) -> Option<tokio::sync::oneshot::Receiver<()>> {
    if !cx.has_global::<PendingEditorWrites>() {
        cx.set_global(PendingEditorWrites::default());
        cx.on_app_quit(|cx| {
            let tail = cx.global_mut::<PendingEditorWrites>().0.take();
            cx.background_executor().spawn(async move {
                if let Some(tail) = tail {
                    tail.await.ok();
                }
            })
        })
        .detach();
    }
    cx.global_mut::<PendingEditorWrites>().0.take()
}

fn flush_editor_writes(cx: &mut App) -> Task<()> {
    let previous = editor_write_queue(cx);
    let (finish, tail) = tokio::sync::oneshot::channel();
    let (done, wait) = tokio::sync::oneshot::channel();
    cx.global_mut::<PendingEditorWrites>().0 = Some(tail);
    cx.background_executor()
        .spawn(async move {
            if let Some(previous) = previous {
                previous.await.ok();
            }
            finish.send(()).ok();
            done.send(()).ok();
        })
        .detach();
    cx.background_executor().spawn(async move {
        wait.await.ok();
    })
}

actions!(shell, [SaveFile, QuickOpen]);

pub(super) fn bind_keys(cx: &mut App) {
    let ctx = Some(KEY_CONTEXT);
    cx.bind_keys([
        krill_app::keys::bind("cmd-s", SaveFile, ctx),
        krill_app::keys::bind("cmd-p", QuickOpen, ctx),
    ]);
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent watcher, focus, palette and window-close lifecycle flags"
)]
pub(super) struct Files {
    /// 불러온 파일 칸. 보인 적 있는 칸만 들어 있다(숨은 복원 탭은 처음 보일 때 읽는다).
    tabs: HashMap<PaneId, FileTab>,
    watch: Option<Task<()>>,
    watching: bool,
    quick_open: Option<(gpui::Entity<jump::Palette>, Subscription)>,
    /// 탭·칸 포커스가 바뀌었다. 다음 그리기에서 키보드 포커스를 에디터나 터미널로 옮긴다.
    pub(super) refocus: bool,
    close_guard: bool,
    closing: bool,
    next_request: u64,
    loading: HashMap<PaneId, (u64, PathBuf)>,
    quick_generation: u64,
    quick_loading: bool,
    recent: Vec<PathBuf>,
    #[cfg(test)]
    next_io: Option<tokio::sync::oneshot::Receiver<()>>,
    /// 다음 그리기 전에 디코드 캐시를 버릴 이미지 경로(재로드·저장·탭 닫힘). 놓으려면 창이 있어야 해서 모아 둔다.
    released: Vec<PathBuf>,
    /// 터미널 링크(OWE-663 4번, OWE-867): 디스크 확인 캐시, ⌘ 아래 링크(칸, 밑줄 범위), 열고 나서 옮길 (파일, 줄, 열).
    link_lookup: Option<LinkLookup>,
    link_generation: u64,
    link_open_generation: u64,
    pub(super) hover: Option<(PaneId, Highlight)>,
    goto: Option<(PathBuf, u32, u32)>,
    /// 아티팩트 공유(`krill artifacts share`)가 진행 중인 칸.
    sharing: std::collections::HashSet<PaneId>,
}

#[derive(PartialEq)]
struct LinkKey {
    pane: PaneId,
    row: Vec<krill_proto::screen::Cell>,
    line: u16,
    column: u16,
    cwd: Option<PathBuf>,
}
struct LinkLookup {
    key: LinkKey,
    ready: bool,
    found: Option<PathLink>,
}

impl Default for Files {
    fn default() -> Self {
        Self {
            tabs: HashMap::new(),
            watch: None,
            watching: false,
            quick_open: None,
            refocus: true,
            close_guard: false,
            closing: false,
            next_request: 0,
            loading: HashMap::new(),
            quick_generation: 0,
            quick_loading: false,
            recent: Vec::new(),
            #[cfg(test)]
            next_io: None,
            released: Vec::new(),
            link_lookup: None,
            link_generation: 0,
            link_open_generation: 0,
            hover: None,
            goto: None,
            sharing: std::collections::HashSet::new(),
        }
    }
}

impl Files {
    pub(super) fn close_quick_open(&mut self) {
        self.quick_open = None;
        self.quick_generation = self.quick_generation.wrapping_add(1);
        self.quick_loading = false;
    }

    /// 닫히거나 바뀌는 탭의 이미지 캐시를 놓도록 예약한다.
    fn forget(&mut self, tab: &FileTab) {
        if tab.shows_image()
            && let Some(doc) = &tab.doc
        {
            self.released.push(doc.path().to_owned());
        }
    }
}

struct FileTab {
    path: PathBuf,
    revision: u64,
    saving: bool,
    preview_generation: u64,
    doc: Option<Doc>,
    body: Body,
    error: Option<String>,
}

enum Body {
    Text {
        state: gpui::Entity<EditorState>,
        _edits: Subscription,
        /// 편집/미리보기 토글이 있는 종류.
        preview: Option<Preview>,
        /// 미리보기를 보는 중. 들어갈 때 에디터 내용으로 만든 사본이다.
        shown: Option<Shown>,
    },
    Image(gpui::Entity<ImageView>),
    /// 에디터 한도를 넘는 CSV의 앞부분 표(읽기 전용).
    Table(gpui::Entity<CsvView>),
    Placeholder(Kind, u64),
    Unopenable(Unopenable),
    Failed(String),
}

/// 지금 보이는 미리보기.
enum Shown {
    /// 원문과 그릴 때 쓰는 요소 id(새로 만들 때마다 바꿔 `TextView`가 이전 파싱 결과를 쓰지 않게 한다).
    Markdown(SharedString, ElementId),
    Table(gpui::Entity<CsvView>),
    Notebook(Result<Arc<fview::NotebookPreview>, ipynb::ParseError>),
    Image(gpui::Entity<ImageView>),
}

/// Parse bounded preview data on the executor; entity creation remains on the UI.
enum PreparedPreview {
    Markdown(String),
    Table(csv::Table),
    Notebook(Result<Arc<fview::NotebookPreview>, ipynb::ParseError>),
    Svg(Arc<gpui::Image>),
}

fn prepare_preview(preview: Preview, text: &str) -> PreparedPreview {
    match preview {
        Preview::Markdown => PreparedPreview::Markdown(text.to_owned()),
        Preview::Table(delimiter) => {
            let head = text.as_bytes();
            let head = head.get(..csv::PREVIEW_BYTES).unwrap_or(head);
            PreparedPreview::Table(csv::preview(
                head,
                u64::try_from(text.len()).unwrap_or(u64::MAX),
                delimiter,
            ))
        }
        Preview::Notebook => PreparedPreview::Notebook(
            ipynb::parse(text).map(|notebook| Arc::new(fview::NotebookPreview::new(notebook))),
        ),
        Preview::Svg => PreparedPreview::Svg(Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Svg,
            text.as_bytes().to_vec(),
        ))),
    }
}

fn build_preview(prepared: PreparedPreview, cx: &mut App) -> Shown {
    static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    match prepared {
        PreparedPreview::Markdown(text) => {
            let n = GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Shown::Markdown(text.into(), ElementId::Name(format!("file-md-{n}").into()))
        }
        PreparedPreview::Table(table) => Shown::Table(cx.new(|_| CsvView::new(table))),
        PreparedPreview::Notebook(result) => Shown::Notebook(result),
        PreparedPreview::Svg(image) => Shown::Image(cx.new(|_| ImageView::new_svg_image(image))),
    }
}

fn read_file(
    path: &Path,
    preview: bool,
) -> (
    std::io::Result<(Opened, editor::Stamp)>,
    Option<PreparedPreview>,
) {
    let opened = editor::open(path);
    let prepared = match &opened {
        Ok((Opened::Text(kind, text), _)) if preview => {
            editor::preview(*kind).map(|p| prepare_preview(p, text))
        }
        _ => None,
    };
    (opened, prepared)
}

/// (미리보기 버튼, 편집 버튼) 이름.
fn toggle_labels(preview: Preview) -> (&'static str, &'static str) {
    match preview {
        Preview::Markdown => (etext::PREVIEW.get(), etext::EDIT.get()),
        Preview::Table(_) => (etext::TABLE.get(), etext::EDIT_TEXT.get()),
        Preview::Notebook => (etext::CELLS.get(), etext::EDIT_TEXT.get()),
        Preview::Svg => (etext::IMAGE.get(), etext::EDIT_TEXT.get()),
    }
}

impl FileTab {
    fn is_dirty(&self) -> bool {
        self.doc.as_ref().is_some_and(Doc::is_dirty)
    }

    #[cfg(test)]
    #[cfg(unix)]
    fn editor(&self) -> Option<&gpui::Entity<EditorState>> {
        match &self.body {
            Body::Text { state, .. } => Some(state),
            _ => None,
        }
    }

    /// 키보드 포커스를 받을 에디터. 미리보기 중이면 숨은 에디터에 입력이 가지 않게 `None`.
    fn focus_editor(&self) -> Option<&gpui::Entity<EditorState>> {
        match &self.body {
            Body::Text {
                state, shown: None, ..
            } => Some(state),
            _ => None,
        }
    }

    fn shows_image(&self) -> bool {
        matches!(
            self.body,
            Body::Image(_)
                | Body::Text {
                    shown: Some(Shown::Image(_)),
                    ..
                }
        )
    }
}

fn unopenable_text(reason: Unopenable) -> String {
    match reason {
        Unopenable::NotUtf8 => etext::NOT_UTF8.get().to_owned(),
        Unopenable::Directory => etext::DIRECTORY.get().to_owned(),
        Unopenable::NotFile => etext::NOT_FILE.get().to_owned(),
    }
}

/// 텍스트 탭을 다시 읽었더니 텍스트가 아니게 됐다(편집 내용은 그대로 둔다).
fn not_text(opened: &Opened) -> String {
    const MIB: u64 = 1024 * 1024;
    match opened {
        Opened::Unopenable(reason) => unopenable_text(*reason),
        Opened::Placeholder(Kind::TooLarge, n) => etext::TOO_LARGE.format(&[
            ("size", &n.div_ceil(MIB).to_string()),
            ("limit", &(editor::MAX_BYTES / MIB).to_string()),
        ]),
        Opened::Table(_) => etext::TOO_LARGE.format(&[
            ("size", &format!(">{}", editor::MAX_BYTES / MIB)),
            ("limit", &(editor::MAX_BYTES / MIB).to_string()),
        ]),
        _ => etext::BINARY.get().to_owned(),
    }
}

impl Shell {
    pub(super) fn file_actions(root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        root.on_action(cx.listener(|this, _: &SaveFile, _, cx| {
            if let Some(pane) = this.layout.focused() {
                this.save_file(pane, cx).detach();
            }
        }))
        .on_action(cx.listener(|this, _: &QuickOpen, window, cx| {
            this.toggle_quick_open(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &krill_app::settings::ZoomIn, window, cx| {
                this.on_zoom_step(1, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &krill_app::settings::ZoomOut, window, cx| {
                this.on_zoom_step(-1, window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &krill_app::settings::ZoomReset, window, cx| {
                this.on_zoom_reset(window, cx);
            }),
        )
        .on_action(cx.listener(|_, _: &Quit, _, cx| quit_app(cx)))
    }

    pub(super) fn is_editor_focused(&self, window: &Window, cx: &App) -> bool {
        if self.files.tabs.values().any(|tab| {
            tab.focus_editor()
                .is_some_and(|e| e.read(cx).focus_handle(cx).is_focused(window))
        }) {
            return true;
        }
        self.layout
            .focused()
            .is_some_and(|pane| self.layout.file(pane).is_some())
    }

    pub(super) fn on_zoom_step(&mut self, dir: i8, window: &mut Window, cx: &mut Context<Self>) {
        if self.page_open() {
            return;
        }
        if self.is_editor_focused(window, cx) {
            if let Some(pane) = self.layout.focused()
                && let Some(tab) = self.files.tabs.get(&pane)
            {
                match &tab.body {
                    Body::Image(img)
                    | Body::Text {
                        shown: Some(Shown::Image(img)),
                        ..
                    } => {
                        img.update(cx, |v, cx| {
                            if dir >= 0 {
                                v.zoom_in(cx);
                            } else {
                                v.zoom_out(cx);
                            }
                        });
                        return;
                    }
                    _ => {}
                }
            }
            krill_app::settings::step_editor_font_size(dir, cx);
        } else if self.layout.focused().is_some() {
            krill_app::settings::step_terminal_font_size(dir, cx);
        }
    }

    pub(super) fn on_zoom_reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page_open() {
            return;
        }
        if self.is_editor_focused(window, cx) {
            if let Some(pane) = self.layout.focused()
                && let Some(tab) = self.files.tabs.get(&pane)
            {
                match &tab.body {
                    Body::Image(img)
                    | Body::Text {
                        shown: Some(Shown::Image(img)),
                        ..
                    } => {
                        img.update(cx, ImageView::zoom_reset);
                        return;
                    }
                    _ => {}
                }
            }
            krill_app::settings::reset_editor_font_size(cx);
        } else if self.layout.focused().is_some() {
            krill_app::settings::reset_terminal_font_size(cx);
        }
    }

    /// 파일을 가운데 영역의 탭으로 연다(이미 열려 있으면 그 탭으로).
    pub(super) fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.files.recent.retain(|old| old != &path);
        self.files.recent.insert(0, path.clone());
        self.files.recent.truncate(100);
        self.close_settings(None, cx);
        self.diff
            .update(cx, krill_app::diff::Panel::clear_selection);
        self.change(true, cx, move |l| {
            l.open_file(path);
        });
        self.files.refocus = true;
    }

    /// 파일을 미리보기 탭으로 연다(OWE-849): 같은 칸을 재사용하며 다음 단일 클릭이 교체한다.
    pub(super) fn open_file_preview(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.close_settings(None, cx);
        self.diff
            .update(cx, krill_app::diff::Panel::clear_selection);
        let tabs = self.workspace_tabs(cx);
        self.change(true, cx, move |l| {
            l.open_file_preview_in(path, &tabs);
        });
        self.files.refocus = true;
    }

    /// 파일을 가운데 영역 에디터 탭으로 열고 지정한 1-based `(line, column)`으로 커서를 이동한다(OWE-733).
    pub(super) fn open_file_at(
        &mut self,
        path: PathBuf,
        line: u32,
        column: u32,
        cx: &mut Context<Self>,
    ) {
        self.files.goto = Some((path.clone(), line.max(1), column.max(1)));
        self.open_file(path, cx);
    }

    /// 세션의 현재 폴더(krilld가 알린 값), 모르면 칸을 연 폴더. 터미널 상대 경로 링크의 기준이다.
    pub(super) fn pane_cwd(&self, pane: PaneId, cx: &App) -> Option<PathBuf> {
        use krill_proto::os::ffi::OsStringExt as _;
        let live = self
            .layout
            .session(pane)
            .and_then(|id| self.sidebar.read(cx).session(id))
            .and_then(|s| s.cwd.clone());
        live.or_else(|| {
            let cwd = self.launch_for(pane).cwd;
            (!cwd.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(cwd)))
        })
    }

    /// 포인터 아래 출력 속 파일 경로 링크(OWE-663 4번). OSC 8 링크 칸은 그쪽이 먼저다.
    pub(super) fn path_link_at(
        &mut self,
        pane: PaneId,
        position: gpui::Point<Pixels>,
        action: Option<terminal_menu::FileAction>,
        cx: &mut Context<Self>,
    ) -> Option<(u16, PathLink)> {
        let cwd = self.pane_cwd(pane, cx);
        let l = self.live.get(&pane)?;
        let screen = l.pane.shown();
        let (column, line) = cell_at(&l.painted, &screen, position)?;
        if screen.link_at(line, column).is_some() {
            return None;
        }
        let row = screen.rows.get(usize::from(line))?.clone();
        let key = LinkKey {
            pane,
            row,
            line,
            column,
            cwd,
        };
        if let Some(action) = action {
            if let Some(lookup) = &self.files.link_lookup
                && lookup.key == key
                && lookup.ready
            {
                return lookup.found.clone().map(|link| (line, link));
            }
            self.files.link_open_generation = self.files.link_open_generation.wrapping_add(1);
            let generation = self.files.link_open_generation;
            let session = l.pane.session();
            let row = key.row;
            let cwd = key.cwd;
            cx.spawn(async move |this, cx| {
                let found = cx
                    .background_executor()
                    .spawn(async move {
                        path_link::Cache::default().link_at(&row, column, cwd.as_deref())
                    })
                    .await;
                this.update(cx, |this, cx| {
                    if this.files.link_open_generation == generation
                        && this.layout.focused() == Some(pane)
                        && this
                            .live
                            .get(&pane)
                            .is_some_and(|live| live.pane.session() == session)
                        && let Some(link) = found
                    {
                        this.activate_terminal_file(link, action, cx);
                    }
                })
                .ok();
            })
            .detach();
            return None;
        }
        if let Some(lookup) = &mut self.files.link_lookup
            && lookup.key == key
        {
            return lookup.found.clone().map(|link| (line, link));
        }
        self.files.link_generation = self.files.link_generation.wrapping_add(1);
        let generation = self.files.link_generation;
        let row = key.row.clone();
        let cwd = key.cwd.clone();
        self.files.link_lookup = Some(LinkLookup {
            key,
            ready: false,
            found: None,
        });
        cx.spawn(async move |this, cx| {
            let found =
                cx.background_executor()
                    .spawn(async move {
                        path_link::Cache::default().link_at(&row, column, cwd.as_deref())
                    })
                    .await;
            let pointer = this
                .read_with(cx, |this, _| this.window)
                .ok()
                .flatten()
                .and_then(|handle| {
                    handle
                        .update(cx, |_, window, _| {
                            (window.mouse_position(), window.modifiers().platform)
                        })
                        .ok()
                });
            this.update(cx, |this, cx| {
                if this.files.link_generation != generation {
                    return;
                }
                let Some(lookup) = &mut this.files.link_lookup else {
                    return;
                };
                let current_row = this
                    .live
                    .get(&pane)
                    .and_then(|l| l.pane.shown().rows.get(usize::from(line)).cloned());
                if current_row.as_ref() != Some(&lookup.key.row) {
                    return;
                }
                lookup.ready = true;
                lookup.found.clone_from(&found);
                if let Some(link) = found {
                    let under_pointer = pointer.is_some_and(|(position, command)| {
                        command
                            && this.live.get(&pane).is_some_and(|live| {
                                cell_at(&live.painted, &live.pane.shown(), position)
                                    == Some((column, line))
                            })
                    });
                    if under_pointer {
                        this.files.hover = Some((
                            pane,
                            Highlight {
                                line,
                                column: link.column,
                                cols: link.cols,
                            },
                        ));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
        None
    }

    pub(super) fn open_terminal_file(
        &mut self,
        path: PathBuf,
        action: terminal_menu::FileAction,
        cx: &mut Context<Self>,
    ) {
        let focused = self.layout.focused();
        let session = focused.and_then(|pane| self.live.get(&pane)?.pane.session());
        self.files.link_open_generation = self.files.link_open_generation.wrapping_add(1);
        let generation = self.files.link_open_generation;
        cx.spawn(async move |this, cx| {
            let check = path.clone();
            let is_file = cx
                .background_executor()
                .spawn(async move { check.is_file() })
                .await;
            this.update(cx, |this, cx| {
                if this.files.link_open_generation != generation
                    || this.layout.focused() != focused
                    || focused.and_then(|pane| this.live.get(&pane)?.pane.session()) != session
                {
                    return;
                }
                if is_file {
                    this.activate_terminal_file(
                        PathLink {
                            column: 0,
                            cols: 0,
                            path,
                            line: None,
                            col: None,
                        },
                        action,
                        cx,
                    );
                } else {
                    cx.reveal_path(&path);
                }
            })
            .ok();
        })
        .detach();
    }

    /// ⌘ 없이 클릭해도 여는 평문 `http(s)://` URL. 프로그램이 마우스 보고를 켰으면 클릭은
    /// 프로그램 것이고, OSC 8 링크는 보이는 글과 대상이 다를 수 있어 ⌘+클릭으로만 연다.
    pub(super) fn plain_url_at(
        &self,
        pane: PaneId,
        position: gpui::Point<Pixels>,
    ) -> Option<(Highlight, String)> {
        let l = self.live.get(&pane).filter(|l| !l.pane.modes().mouse())?;
        let screen = l.pane.shown();
        krill_app::terminal::url_link_target(&screen, &l.painted, position)
    }

    /// ⌘를 누른 채 움직이면 링크 밑줄(OSC 8·평문 URL·파일 경로)을 갱신한다(OWE-867). 바뀔 때만 다시 그린다.
    /// ⌘ 없이도 맨 클릭으로 열리는 평문 URL에는 밑줄을 긋는다.
    pub(super) fn update_path_hover(
        &mut self,
        pane: PaneId,
        position: gpui::Point<Pixels>,
        cmd: bool,
        cx: &mut Context<Self>,
    ) {
        if !cmd {
            self.files.link_generation = self.files.link_generation.wrapping_add(1);
            self.files.link_lookup = None;
        }
        let next = if cmd {
            let osc_or_url = self.live.get(&pane).and_then(|l| {
                let screen = l.pane.shown();
                krill_app::terminal::osc_link_highlight(&screen, &l.painted, position).or_else(
                    || {
                        krill_app::terminal::url_link_target(&screen, &l.painted, position)
                            .map(|(h, _)| h)
                    },
                )
            });
            if osc_or_url.is_some() {
                self.files.link_generation = self.files.link_generation.wrapping_add(1);
                self.files.link_lookup = None;
            }
            osc_or_url
                .or_else(|| {
                    self.path_link_at(pane, position, None, cx)
                        .map(|(line, link)| Highlight {
                            line,
                            column: link.column,
                            cols: link.cols,
                        })
                })
                .map(|h| (pane, h))
        } else {
            self.plain_url_at(pane, position).map(|(h, _)| (pane, h))
        };
        if self.files.hover != next {
            self.files.hover = next;
            cx.notify();
        }
    }

    /// 움직이지 않고 ⌘만 누르거나 떼도 밑줄이 따라오게 한다. Ctrl을 떼면 MRU 탭 전환을 확정한다.
    pub(super) fn on_link_modifiers(
        &mut self,
        e: &gpui::ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !e.modifiers.control && self.layout.mru_switching() {
            self.commit_mru_switcher(cx);
        }
        let position = window.mouse_position();
        let pane = self.layout.visible().into_iter().find(|p| {
            self.live
                .get(p)
                .is_some_and(|l| l.painted.get().0.contains(&position))
        });
        match pane {
            Some(pane) => self.update_path_hover(pane, position, e.modifiers.platform, cx),
            None if self.files.hover.is_some() => {
                self.files.hover = None;
                cx.notify();
            }
            None => {}
        }
    }

    /// 칸에 그릴 링크 밑줄.
    pub(super) fn path_hover(&self, pane: PaneId) -> Option<Highlight> {
        self.files.hover.filter(|h| h.0 == pane).map(|h| h.1)
    }

    /// 경로 링크를 가운데 에디터로 연다. 줄이 있으면 탭이 한 번 그려진 뒤(스크롤에 레이아웃이 필요) 그 줄로 옮긴다.
    pub(super) fn open_path_link(&mut self, link: PathLink, cx: &mut Context<Self>) {
        self.files.goto = link
            .line
            .map(|line| (link.path.clone(), line, link.col.unwrap_or(1)));
        self.files.hover = None;
        self.open_file(link.path, cx);
    }

    /// 파일 칸 하나를 그린다: 경로 줄, 디스크 변경 띠, 에디터(또는 열 수 없음 안내).
    pub(super) fn render_file_pane(
        &self,
        id: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = krill_app::ui::palette(cx);
        let is_split = self.layout.visible().len() > 1;
        let path = self.layout.file(id).map(Path::to_owned).unwrap_or_default();
        let tab = self.files.tabs.get(&id);
        let dirty = tab.is_some_and(FileTab::is_dirty);
        let header = div()
            .h(px(26.))
            .px_2()
            .flex_none()
            .flex()
            .items_center()
            .gap_1p5()
            .bg(p.sidebar)
            .border_b_1()
            .border_color(p.border)
            .child(
                Icon::new(IconName::FileText)
                    .size(px(12.))
                    .text_color(p.muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(krill_app::ui::FONT_MONO)
                    .text_size(krill_app::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    // OWE-849: 절대 경로 대신 워크트리 상대 경로(또는 `~` 축약)를 보인다.
                    .child(self.diff.read(cx).display_path(&path)),
            )
            .when(dirty, |d| {
                d.child(
                    div()
                        .text_size(krill_app::ui::TEXT_2XS)
                        .text_color(p.muted_foreground)
                        .child(etext::UNSAVED.get()),
                )
            })
            .children(self.preview_toggle(id, cx))
            .children(self.share_button(id, &path, cx))
            .when(is_split, |d| {
                d.child(
                    Button::new(("file-pane-close", id.get()))
                        .icon(IconName::Close)
                        .ghost()
                        .xsmall()
                        .tooltip(text::CLOSE_PANE.get())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.change(true, cx, |l| {
                                l.focus(id);
                                l.close_pane();
                            });
                        })),
                )
            });
        let banner = |msg: &str| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap_2()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(p.warning.opacity(0.3))
                .bg(p.warning.opacity(0.12))
                .text_color(p.warning)
                .text_size(krill_app::ui::TEXT_XS)
                .child(div().flex_1().min_w_0().truncate().child(msg.to_owned()))
        };
        let disk = tab
            .and_then(|t| t.doc.as_ref())
            .map_or(Disk::Same, Doc::disk);
        let disk_banner = match disk {
            Disk::Same => None,
            Disk::Deleted => {
                Some(banner(etext::DELETED.get()).debug_selector(|| "file-deleted".into()))
            }
            Disk::Changed => Some(
                banner(etext::CHANGED_ON_DISK.get())
                    .debug_selector(|| "file-changed-on-disk".into())
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .gap_1()
                            .child(
                                Button::new(("file-reload", id.get()))
                                    .debug_selector(|| "file-reload".into())
                                    .label(etext::RELOAD.get())
                                    .outline()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.reload_file(id, window, cx);
                                    })),
                            )
                            .child(
                                Button::new(("file-keep", id.get()))
                                    .debug_selector(|| "file-keep".into())
                                    .label(etext::KEEP.get())
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.keep_file(id, cx);
                                    })),
                            ),
                    ),
            ),
        };
        let error = tab.and_then(|t| t.error.clone()).map(|e| {
            div()
                .flex_none()
                .px_2()
                .py_1()
                .child(krill_app::ui::error_banner(p, e))
                .debug_selector(|| "file-error".into())
        });
        let notice = |msg: String| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(krill_app::ui::TEXT_SM)
                .text_color(p.muted_foreground)
                .child(msg)
                .into_any_element()
        };
        let body = match tab.map(|t| &t.body) {
            Some(Body::Text {
                shown: Some(shown), ..
            }) => match shown {
                Shown::Markdown(src, md_id) => fview::markdown(md_id.clone(), src.clone(), cx),
                Shown::Table(view) => view.clone().into_any_element(),
                Shown::Notebook(Ok(nb)) => {
                    fview::notebook(("file-ipynb", id.get()), nb.clone(), window, cx)
                }
                Shown::Notebook(Err(_)) => notice(ftext::IPYNB_INVALID.get().to_owned()),
                Shown::Image(view) => view.clone().into_any_element(),
            },
            Some(Body::Text { state, .. }) => div()
                .size_full()
                .bg(p.editor_surface)
                .child(Editor::new(state).bordered(false).h(gpui::relative(1.)))
                .into_any_element(),
            Some(Body::Image(view)) => view.clone().into_any_element(),
            Some(Body::Table(view)) => view.clone().into_any_element(),
            Some(Body::Placeholder(kind, len)) => fview::placeholder(&path, *kind, *len, cx),
            Some(Body::Unopenable(reason)) => notice(unopenable_text(*reason)),
            Some(Body::Failed(err)) => notice(etext::OPEN_FAILED.format(&[("error", err)])),
            None => notice(String::new()),
        };
        div()
            .id(ElementId::Name(format!("file-pane-{}", id.get()).into()))
            .debug_selector(|| "file-pane".into())
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .bg(p.background)
            .text_color(p.foreground)
            .child(header)
            .children(disk_banner)
            .children(error)
            .child(div().flex_1().min_h_0().child(body))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.change(true, cx, |l| l.focus(id))),
            )
            .into_any_element()
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(super) fn is_file_preview(&self, id: PaneId) -> Option<bool> {
        let Body::Text { preview, shown, .. } = &self.files.tabs.get(&id)?.body else {
            return None;
        };
        Some(preview.is_some() && shown.is_some())
    }

    /// 열린 HTML·Markdown 파일을 아티팩트(공개 링크)로 공유하는 머리 버튼.
    fn share_button(&self, id: PaneId, path: &Path, cx: &mut Context<Self>) -> Option<Button> {
        if !krill_proto::artifacts::is_shareable_path(path) {
            return None;
        }
        let busy = self.files.sharing.contains(&id);
        Some(
            krill_app::ui::button_with_reserved_label(
                ("file-share-artifact", id.get()),
                atext::SHARE_BUTTON.get(),
                atext::SHARING.get(),
                busy,
            )
            .debug_selector(|| "file-share-artifact".into())
            .icon(IconName::Globe)
            .ghost()
            .xsmall()
            .disabled(busy)
            .tooltip(atext::SHARE_TOOLTIP.get())
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.share_file_artifact(id, window, cx);
            })),
        )
    }

    /// 칸의 파일을 공유한다(이미 공유했으면 같은 링크의 내용을 갱신한다). 링크는 클립보드에 복사한다.
    /// 공개 공유가 꺼져 있으면 설정 > 아티팩트로 보낸다.
    pub(super) fn share_file_artifact(
        &mut self,
        id: PaneId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.layout.file(id).map(Path::to_owned) else {
            return;
        };
        if self.files.sharing.contains(&id) {
            return;
        }
        if self.files.tabs.get(&id).is_some_and(FileTab::is_dirty) {
            self.notice = Some(atext::SAVE_FIRST.get().to_owned());
            cx.notify();
            return;
        }
        // 설정을 아직 안 열었으면 게이트·CLI가 krilld와 같은 상태 디렉터리를 보게 먼저 맞춘다.
        if !cx.has_global::<krill_app::settings::Config>() {
            let dir = self.state_dir(cx);
            krill_app::settings::init_at(dir, cx);
        }
        let Some(run) = krill_app::artifacts::runner(cx) else {
            self.notice = Some(atext::ERROR.format(&[(
                "error",
                krill_app::i18n::catalog::orca_import::ERR_CLI_MISSING.get(),
            )]));
            cx.notify();
            return;
        };
        self.files.sharing.insert(id);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { krill_app::artifacts::share_path(&run, &path) })
                .await;
            this.update(cx, |this, cx| {
                this.files.sharing.remove(&id);
                this.notice = Some(match res {
                    Ok(out) => {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                            out.share_url.clone(),
                        ));
                        let msg = if out.change == "updated" {
                            atext::UPDATED
                        } else {
                            atext::SHARED
                        };
                        msg.format(&[("url", &out.share_url)])
                    }
                    Err(e) => e.message(),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 파일 칸 머리의 편집/미리보기 토글(마크다운·CSV·ipynb·SVG만).
    fn preview_toggle(&self, id: PaneId, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let Some(Body::Text {
            preview: Some(preview),
            shown,
            ..
        }) = self.files.tabs.get(&id).map(|t| &t.body)
        else {
            return None;
        };
        let (preview_label, edit_label) = toggle_labels(*preview);
        let previewing = shown.is_some();
        let p = krill_app::ui::palette(cx);
        let segment_button = |name: &'static str, label: &'static str, on: bool| {
            krill_app::ui::segment(p, (name, id.get()), label, previewing == on)
                .debug_selector(move || name.to_owned())
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.set_preview(id, on, window, cx);
                }))
        };
        Some(
            krill_app::ui::segmented_control(
                p,
                [
                    segment_button("file-preview", preview_label, true),
                    segment_button("file-edit", edit_label, false),
                ],
            )
            .debug_selector(|| "file-preview-toggle".into()),
        )
    }

    /// 편집/미리보기를 바꾼다. 미리보기는 들어갈 때마다 지금 에디터 내용으로 다시 만든다.
    pub(super) fn set_preview(
        &mut self,
        pane: PaneId,
        on: bool,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.files.tabs.get_mut(&pane) else {
            return;
        };
        let Body::Text {
            state,
            preview: Some(preview),
            shown,
            ..
        } = &mut tab.body
        else {
            return;
        };
        tab.preview_generation = tab.preview_generation.wrapping_add(1);
        let generation = tab.preview_generation;
        if on {
            let text = state.read(cx).text().clone();
            let state_id = state.entity_id();
            let preview = *preview;
            let revision = tab.revision;
            cx.spawn(async move |this, cx| {
                let prepared = cx
                    .background_executor()
                    .spawn(async move { prepare_preview(preview, &text.to_string()) })
                    .await;
                this.update(cx, |this, cx| {
                    let Some(tab) = this.files.tabs.get_mut(&pane) else {
                        return;
                    };
                    if tab.preview_generation != generation || tab.revision != revision {
                        return;
                    }
                    let Body::Text { state, shown, .. } = &mut tab.body else {
                        return;
                    };
                    if state.entity_id() != state_id {
                        return;
                    }
                    *shown = Some(build_preview(prepared, cx));
                    this.files.refocus = true;
                    this.sync_file_states(cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        } else {
            *shown = None;
        }
        self.files.refocus = true;
        self.sync_file_states(cx);
        cx.notify();
    }

    /// 열린 텍스트 탭들의 커서 위치·미리보기 여부를 `layout.file_states`에 동기화하고 바뀌었으면 저장한다(OWE-842).
    pub(super) fn sync_file_states(&mut self, cx: &App) {
        let mut changed = false;
        let entries: Vec<_> = self
            .files
            .tabs
            .iter()
            .filter_map(|(&pane, tab)| {
                let Body::Text {
                    state,
                    preview,
                    shown,
                    ..
                } = &tab.body
                else {
                    return None;
                };
                let pos = state.read(cx).cursor_position();
                let is_preview = preview.is_some() && shown.is_some();
                Some((
                    pane,
                    layout::SavedFileState {
                        line: pos.line,
                        column: pos.character,
                        preview: is_preview,
                    },
                ))
            })
            .collect();
        for (pane, st) in entries {
            if self.layout.file_state(pane) != Some(st) {
                self.layout.set_file_state(pane, st);
                changed = true;
            }
        }
        if changed {
            self.save();
        }
    }

    /// OWE-874: 파일 탭이 포커스된 상태에서 ⌘F — 미리보기면 편집 모드로 돌린 뒤 에디터 내장 검색을
    /// 연다. 에디터 본문이 없으면(이미지·표·자리표시자) false를 돌려 호출자가 터미널 검색으로 이어가게 한다.
    pub(super) fn open_editor_search(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self
            .files
            .tabs
            .get(&pane)
            .is_some_and(|t| matches!(t.body, Body::Text { .. }))
        {
            return false;
        }
        self.set_preview(pane, false, window, cx);
        let Some(state) = self
            .files
            .tabs
            .get(&pane)
            .and_then(FileTab::focus_editor)
            .cloned()
        else {
            return false;
        };
        state.update(cx, |s, cx| {
            s.open_search(false, cx);
            s.focus(window, cx);
        });
        cx.notify();
        true
    }

    /// 탭 바의 파일 탭 앞 아이콘(파일 + 미저장 점). 세션 칸이면 `None`.
    pub(super) fn file_tab_prefix(&self, pane: PaneId, cx: &App) -> Option<gpui::Div> {
        self.layout.file(pane)?;
        let p = krill_app::ui::palette(cx);
        let dirty = self.files.tabs.get(&pane).is_some_and(FileTab::is_dirty);
        Some(
            div()
                .flex()
                .items_center()
                .gap(krill_app::ui::SPACE_1)
                .child(Icon::new(IconName::FileText).small())
                .when(dirty, |d| {
                    d.child(
                        div()
                            .debug_selector(|| "file-dirty".into())
                            .size(krill_app::ui::STATE_DOT)
                            .rounded_full()
                            .bg(p.foreground),
                    )
                }),
        )
    }

    pub(super) fn file_requests_dir(&self, cx: &App) -> Option<PathBuf> {
        let explicit = self
            .path
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(PathBuf::from)
            .or_else(|| {
                let parent = self.paths.as_ref()?.socket.parent()?;
                if parent.file_name().is_some_and(|n| n == "run") {
                    parent.parent().map(PathBuf::from)
                } else {
                    Some(parent.to_path_buf())
                }
            });
        #[cfg(test)]
        {
            let _ = cx;
            explicit
        }
        #[cfg(not(test))]
        {
            explicit.or_else(|| krill_app::settings::state_dir(cx))
        }
    }

    /// `krill file open|diff` CLI가 상태 디렉터리(`file-requests.json`)에 넣은 요청을 소비한다.
    fn apply_file_requests(
        &mut self,
        requests: Vec<CliFileRequest>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if requests.is_empty() {
            return;
        }
        for req in requests {
            if req.focus {
                window.activate_window();
            }
            match req.kind {
                CliFileActionKind::Open => {
                    self.files.goto = req
                        .line
                        .map(|line| (req.path.clone(), line, req.column.unwrap_or(1)));
                    self.open_file(req.path, cx);
                }
                CliFileActionKind::Diff => {
                    self.close_settings(Some(window), cx);
                    if self.panel_collapsed {
                        self.panel_collapsed = false;
                        self.layout.panel_collapsed = false;
                        self.save();
                    }
                    let worktree = req.worktree;
                    let rel = req.relative_path;
                    let staged = req.staged;
                    self.diff.update(cx, |d, cx| {
                        d.set_visible(true, cx);
                        d.open_diff_file(worktree, &rel, staged, cx);
                    });
                }
            }
        }
        cx.notify();
    }

    /// 그리기 전에: 보이는 파일 칸을 읽고, 감시를 켜고, 바뀐 포커스를 옮기고, 창 닫기 확인을 건다.
    pub(super) fn sync_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for path in std::mem::take(&mut self.files.released) {
            fview::release_image(&path, window, cx);
        }
        self.apply_goto(window, cx);
        self.files
            .loading
            .retain(|pane, (_, path)| self.layout.file(*pane) == Some(path.as_path()));
        for pane in self.layout.visible() {
            if let Some(path) = self.layout.file(pane).map(Path::to_owned)
                && !self.files.tabs.contains_key(&pane)
                && !self.files.loading.contains_key(&pane)
            {
                self.start_file_load(pane, path, window, cx);
            }
        }
        self.sync_file_states(cx);
        if !self.files.watching
            && (self.layout.files().next().is_some() || self.file_requests_dir(cx).is_some())
        {
            self.files.watching = true;
            self.files.watch = Some(Self::watch_files(window, cx));
        }
        // 팔레트·모달·검색 바가 떠 있으면 그쪽 포커스를 뺏지 않는다.
        if self.jump.is_none()
            && self.dashboard.is_none()
            && self.search.is_none()
            && self.tab_rename.is_none()
            && self.files.quick_open.is_none()
            && std::mem::take(&mut self.files.refocus)
        {
            let editor = self
                .layout
                .focused()
                .and_then(|p| self.files.tabs.get(&p))
                .and_then(FileTab::focus_editor);
            match editor {
                Some(state) => state.update(cx, |s, cx| s.focus(window, cx)),
                None => window.focus(&self.focus, cx),
            }
        }
        if !self.files.close_guard {
            self.files.close_guard = true;
            let weak = cx.weak_entity();
            window.on_window_should_close(cx, move |window, cx| {
                weak.update(cx, |this, cx| this.request_close(window, cx))
                    .ok();
                false
            });
        }
    }

    /// Close only after dirty-file decisions and a durable layout barrier.
    pub(crate) fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.files.closing {
            return;
        }
        self.files.closing = true;
        let dirty = self.dirty_files();
        let close = if dirty.is_empty() {
            Task::ready(true)
        } else {
            let answer = prompt_unsaved(&self.unsaved_detail(&dirty), window, cx);
            Self::resolve_unsaved(answer, &dirty, cx)
        };
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let close = close.await;
            if close {
                let Ok((barrier, layout, editor_saved)) = this.update(cx, |this, cx| {
                    (
                        this.saver.barrier(cx),
                        this.layout.clone(),
                        flush_editor_writes(cx),
                    )
                }) else {
                    return;
                };
                editor_saved.await;
                if barrier.await {
                    let unchanged = this
                        .read_with(cx, |this, _| {
                            this.dirty_files().is_empty() && this.layout == layout
                        })
                        .unwrap_or(false);
                    if unchanged {
                        handle
                            .update(cx, |_, window, _| window.remove_window())
                            .ok();
                        return;
                    }
                }
            }
            this.update(cx, |this, cx| {
                this.files.closing = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 경로 링크의 줄로 커서를 옮긴다. 아직 안 읽은 탭이면 이번에 읽고 다음 프레임에 옮긴다.
    fn apply_goto(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, line, col)) = self.files.goto.take() else {
            return;
        };
        let Some(pane) = self
            .layout
            .focused()
            .filter(|p| self.layout.file(*p) == Some(path.as_path()))
        else {
            return;
        };
        let Some(tab) = self.files.tabs.get_mut(&pane) else {
            self.files.goto = Some((path, line, col));
            return;
        };
        if let Body::Text { shown, .. } = &mut tab.body
            && shown.is_some()
        {
            *shown = None;
        }
        if let Some(state) = tab.focus_editor().cloned() {
            let at =
                gpui_component::input::Position::new(line.saturating_sub(1), col.saturating_sub(1));
            state.update(cx, |s, cx| s.set_cursor_position(at, window, cx));
        }
        self.sync_file_states(cx);
    }

    fn start_file_load(
        &mut self,
        pane: PaneId,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.files.next_request = self.files.next_request.wrapping_add(1);
        let generation = self.files.next_request;
        self.files.loading.insert(pane, (generation, path.clone()));
        let saved = self.layout.file_state(pane);
        #[cfg(test)]
        let gate = self.files.next_io.take();
        cx.spawn_in(window, async move |this, cx| {
            let read_path = path.clone();
            let (opened, prepared) = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    if let Some(gate) = gate {
                        gate.await.ok();
                    }
                    read_file(&read_path, saved.is_none_or(|s| s.preview))
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.layout.file(pane) != Some(path.as_path())
                    || this.files.loading.get(&pane) != Some(&(generation, path.clone()))
                {
                    return;
                }
                this.files.loading.remove(&pane);
                let mut tab =
                    Self::load_file_tab(pane, path.clone(), saved, window, cx, opened, prepared);
                if this.files.goto.as_ref().is_some_and(|(p, _, _)| *p == path)
                    && let Body::Text { shown, .. } = &mut tab.body
                {
                    *shown = None;
                }
                this.files.tabs.insert(pane, tab);
                this.files.refocus = true;
                this.apply_goto(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_file_tab(
        pane: PaneId,
        path: PathBuf,
        saved: Option<layout::SavedFileState>,
        window: &mut Window,
        cx: &mut Context<Self>,
        opened: std::io::Result<(Opened, editor::Stamp)>,
        prepared: Option<PreparedPreview>,
    ) -> FileTab {
        let (body, stamp) = match opened {
            Ok((Opened::Text(kind, text), stamp)) => {
                let language = editor::language(kind, &path);
                let preview = editor::preview(kind);
                let shown = prepared.map(|prepared| build_preview(prepared, cx));
                let state = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language)
                        .line_number(true)
                        // OWE-874: 긴 줄은 자동 줄 바꿈(기본값 true를 고정 — 잘려서 못 보는 일이 없게).
                        .soft_wrap(true)
                        .default_value(text)
                });
                // 에디터가 받은 그대로를 기준으로 삼는다(정규화가 있어도 열자마자 dirty가 되지 않게).
                let baseline = state.read(cx).text().to_string();
                if let Some(s) = saved
                    && (s.line != 0 || s.column != 0)
                {
                    let at = gpui_component::input::Position::new(s.line, s.column);
                    state.update(cx, |ed, cx| ed.set_cursor_position(at, window, cx));
                }
                let edits = cx.subscribe(&state, move |this, _, e: &InputEvent, cx| {
                    if matches!(e, InputEvent::Change) {
                        this.file_edited(pane, cx);
                    }
                    this.sync_file_states(cx);
                });
                return FileTab {
                    path: path.clone(),
                    revision: 0,
                    saving: false,
                    preview_generation: 0,
                    doc: Some(Doc::new(path, baseline, stamp)),
                    body: Body::Text {
                        state,
                        _edits: edits,
                        preview,
                        shown,
                    },
                    error: None,
                };
            }
            Ok((Opened::Image(Ok(dims)), stamp)) => (
                Body::Image(cx.new(|_| ImageView::new(path.clone(), Some(dims)))),
                stamp,
            ),
            Ok((Opened::Image(Err(e)), stamp)) => (Body::Failed(fview::image_error(e)), stamp),
            Ok((Opened::Table(table), stamp)) => {
                (Body::Table(cx.new(|_| CsvView::new(table))), stamp)
            }
            Ok((Opened::Placeholder(kind, len), stamp)) => (Body::Placeholder(kind, len), stamp),
            Ok((Opened::Unopenable(reason), stamp)) => (Body::Unopenable(reason), stamp),
            Err(e) => {
                krill_app::onboarding::actions::note_access_denied(&path, &e, cx);
                return FileTab {
                    path: path.clone(),
                    revision: 0,
                    saving: false,
                    preview_generation: 0,
                    doc: None,
                    body: Body::Failed(e.to_string()),
                    error: None,
                };
            }
        };
        // 텍스트가 아닌 탭도 디스크 지문을 들고 있어 바뀌면 다시 읽는다(편집이 없어 늘 깨끗하다).
        FileTab {
            path: path.clone(),
            revision: 0,
            saving: false,
            preview_generation: 0,
            doc: Some(Doc::new(path, String::new(), stamp)),
            body,
            error: None,
        }
    }

    fn file_edited(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let Some(tab) = self.files.tabs.get_mut(&pane) else {
            return;
        };
        let (Some(doc), Body::Text { state, .. }) = (tab.doc.as_mut(), &tab.body) else {
            return;
        };
        tab.revision = tab.revision.wrapping_add(1);
        let revision = tab.revision;
        let state_id = state.entity_id();
        let text = state.read(cx).text().clone();
        let baseline = doc.saved_snapshot();
        // Optimistically mark dirty, then compare the persistent rope off-thread.
        // This keeps typing independent of the size of the saved buffer.
        doc.edited(false);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let compared_baseline = baseline.clone();
            let unchanged = cx
                .background_executor()
                .spawn(async move { text == *compared_baseline })
                .await;
            this.update(cx, |this, cx| {
                let Some(tab) = this.files.tabs.get_mut(&pane) else {
                    return;
                };
                if tab.revision != revision {
                    return;
                }
                let (Some(doc), Body::Text { state, .. }) = (tab.doc.as_mut(), &tab.body) else {
                    return;
                };
                if state.entity_id() == state_id
                    && Arc::ptr_eq(&doc.saved_snapshot(), &baseline)
                    && doc.edited(unchanged)
                {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Save a snapshot off-thread. Edits made while fsync runs stay dirty.
    pub(super) fn save_file(&mut self, pane: PaneId, cx: &mut Context<Self>) -> Task<bool> {
        let Some(tab) = self.files.tabs.get_mut(&pane) else {
            return Task::ready(false);
        };
        let (Some(doc), Body::Text { state, .. }) = (tab.doc.as_ref(), &tab.body) else {
            return Task::ready(false);
        };
        if tab.saving {
            return Task::ready(false);
        }
        tab.saving = true;
        let mut snapshot = doc.clone();
        let path = doc.path().to_owned();
        let text = state.read(cx).text().clone();
        let revision = tab.revision;
        let state_id = state.entity_id();
        #[cfg(test)]
        let gate = self.files.next_io.take();
        let previous = editor_write_queue(cx);
        let (finish, tail) = tokio::sync::oneshot::channel();
        cx.global_mut::<PendingEditorWrites>().0 = Some(tail);
        let (completed, result) = tokio::sync::oneshot::channel();
        cx.background_executor()
            .spawn(async move {
                if let Some(previous) = previous {
                    previous.await.ok();
                }
                #[cfg(test)]
                if let Some(gate) = gate {
                    gate.await.ok();
                }
                let result = snapshot.save(text.to_string());
                completed.send((snapshot, result)).ok();
                finish.send(()).ok();
            })
            .detach();
        cx.spawn(async move |this, cx| {
            let Ok((snapshot, result)) = result.await else {
                return false;
            };
            this.update(cx, |this, cx| {
                let Some(tab) = this.files.tabs.get_mut(&pane) else {
                    return false;
                };
                let Body::Text { state, .. } = &tab.body else {
                    return false;
                };
                if state.entity_id() != state_id || this.layout.file(pane) != Some(path.as_path()) {
                    return false;
                }
                tab.saving = false;
                let saved = match result {
                    Ok(()) => {
                        tab.doc = Some(snapshot);
                        let unchanged = tab.revision == revision;
                        if let Some(doc) = &mut tab.doc {
                            doc.edited(unchanged);
                        }
                        tab.error = None;
                        unchanged
                    }
                    Err(SaveError::Conflict) => {
                        tab.error = Some(etext::SAVE_CONFLICT.get().to_owned());
                        false
                    }
                    Err(SaveError::Io(e)) => {
                        krill_app::onboarding::actions::note_access_denied(&path, &e, cx);
                        tab.error = Some(etext::SAVE_FAILED.format(&[("error", &e.to_string())]));
                        false
                    }
                };
                if !saved && tab.error.is_none() {
                    this.file_edited(pane, cx);
                }
                cx.notify();
                saved
            })
            .unwrap_or(false)
        })
    }

    fn keep_file(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let Some(tab) = self.files.tabs.get(&pane) else {
            return;
        };
        let Some(doc) = &tab.doc else { return };
        let path = doc.path().to_owned();
        let revision = tab.revision;
        let before = doc.clone();
        cx.spawn(async move |this, cx| {
            let read_path = path.clone();
            let now = cx
                .background_executor()
                .spawn(async move { editor::stamp(&read_path) })
                .await;
            this.update(cx, |this, cx| {
                let Some(tab) = this.files.tabs.get_mut(&pane) else {
                    return;
                };
                if tab.revision != revision || tab.saving {
                    return;
                }
                let Some(doc) = &mut tab.doc else { return };
                if *doc != before || this.layout.file(pane) != Some(path.as_path()) {
                    return;
                }
                if let Ok(now) = now {
                    doc.keep_mine(now);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Read and parse off-thread; refuse stale results after any edit, save or close.
    fn reload_file(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.files.tabs.get(&pane) else {
            return;
        };
        if tab.saving || self.files.loading.contains_key(&pane) {
            return;
        }
        let Some(doc) = &tab.doc else { return };
        let path = doc.path().to_owned();
        let before = doc.clone();
        let revision = tab.revision;
        self.files.next_request = self.files.next_request.wrapping_add(1);
        let generation = self.files.next_request;
        self.files.loading.insert(pane, (generation, path.clone()));
        let saved = self.layout.file_state(pane);
        let previewing = matches!(&tab.body, Body::Text { shown: Some(_), .. });
        #[cfg(test)]
        let gate = self.files.next_io.take();
        cx.spawn_in(window, async move |this, cx| {
            let read_path = path.clone();
            let (opened, prepared) = cx
                .background_executor()
                .spawn(async move {
                    #[cfg(test)]
                    if let Some(gate) = gate {
                        gate.await.ok();
                    }
                    read_file(&read_path, previewing)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.files.loading.get(&pane) != Some(&(generation, path.clone())) {
                    return;
                }
                this.files.loading.remove(&pane);
                let Some(tab) = this.files.tabs.get_mut(&pane) else {
                    return;
                };
                if tab.revision != revision
                    || tab.saving
                    || tab.doc.as_ref() != Some(&before)
                    || this.layout.file(pane) != Some(path.as_path())
                {
                    return;
                }
                if !matches!(tab.body, Body::Text { .. }) {
                    if tab.shows_image() {
                        this.files.released.push(path.clone());
                    }
                    let fresh =
                        Self::load_file_tab(pane, path, saved, window, cx, opened, prepared);
                    this.files.tabs.insert(pane, fresh);
                    cx.notify();
                    return;
                }
                let (Some(doc), Body::Text { state, shown, .. }) =
                    (tab.doc.as_mut(), &mut tab.body)
                else {
                    return;
                };
                match opened {
                    Ok((Opened::Text(_, text), stamp)) => {
                        state.update(cx, |s, cx| {
                            let cursor = s.cursor_position();
                            s.set_value(text, window, cx);
                            s.set_cursor_position(cursor, window, cx);
                        });
                        let text = state.read(cx).text().to_string();
                        *shown = prepared.map(|p| build_preview(p, cx));
                        doc.reloaded(text, stamp);
                        tab.revision = tab.revision.wrapping_add(1);
                        tab.error = None;
                    }
                    Ok((other, stamp)) => {
                        doc.keep_mine(Some(stamp));
                        tab.error = Some(not_text(&other));
                    }
                    Err(e) => {
                        krill_app::onboarding::actions::note_access_denied(&path, &e, cx);
                        tab.error = Some(etext::OPEN_FAILED.format(&[("error", &e.to_string())]));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn watch_files(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_EVERY).await;
                let Ok(Some((paths, req_dir))) = this.update(cx, |this, cx| {
                    let req_dir = this.file_requests_dir(cx);
                    if this.layout.files().next().is_none() && req_dir.is_none() {
                        this.files.watching = false;
                        return None;
                    }
                    let paths = this
                        .files
                        .tabs
                        .iter()
                        .filter_map(|(p, t)| Some((*p, t.revision, t.doc.clone()?)))
                        .collect::<Vec<_>>();
                    Some((paths, req_dir))
                }) else {
                    return;
                };
                let (stamps, requests) = cx
                    .background_executor()
                    .spawn(async move {
                        // 큐의 잠금·읽기·삭제를 모두 IO 작업에서 마친다.
                        // 화면 그리기에서는 CLI나 디스크를 기다리지 않는다.
                        let requests = req_dir
                            .as_deref()
                            .map(take_pending_file_requests)
                            .unwrap_or_default();
                        let stamps = paths
                            .into_iter()
                            .map(|(p, revision, doc)| {
                                let now = editor::stamp(doc.path());
                                (p, revision, doc, now)
                            })
                            .collect::<Vec<_>>();
                        (stamps, requests)
                    })
                    .await;
                if stamps.is_empty() && requests.is_empty() {
                    continue;
                }
                let polled = this.update_in(cx, |this, window, cx| {
                    this.apply_file_requests(requests, window, cx);
                    for (pane, revision, before, now) in stamps {
                        // stat 자체가 실패하면(권한 등) 판단하지 않고 다음 주기에 다시 본다.
                        let Ok(now) = now else { continue };
                        let Some(tab) = this.files.tabs.get_mut(&pane) else {
                            continue;
                        };
                        if tab.saving
                            || tab.revision != revision
                            || tab.doc.as_ref() != Some(&before)
                        {
                            continue;
                        }
                        let Some(doc) = tab.doc.as_mut() else {
                            continue;
                        };
                        match doc.poll(now) {
                            Poll::Nothing => {}
                            Poll::Marked => cx.notify(),
                            Poll::Reload => this.reload_file(pane, window, cx),
                        }
                    }
                });
                if polled.is_err() {
                    return;
                }
            }
        })
    }

    /// ⌘P는 즉시 열고 백그라운드 스캔이 끝나면 입력을 유지한 채 목록을 갱신한다.
    pub(super) fn toggle_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.files.quick_generation = self.files.quick_generation.wrapping_add(1);
        let generation = self.files.quick_generation;
        if self.files.quick_open.take().is_some() {
            self.files.quick_loading = false;
            self.files.refocus = true;
            cx.notify();
            return;
        }
        self.close_settings(Some(window), cx);
        self.jump = None;
        if self.dashboard.take().is_some() {
            self.sync_sidebar_visibility(cx);
            self.sync_focused_session(cx);
        }
        if let Some(pane) = self.layout.focused() {
            self.commit_ime(pane, Some(window), cx);
        }
        let root = self.folder();
        let mut recent: Vec<_> = self
            .files
            .recent
            .iter()
            .filter_map(|path| path.strip_prefix(&root).ok().map(Path::to_path_buf))
            .collect();
        for (_, path) in self.layout.files() {
            if let Ok(relative) = path.strip_prefix(&root) {
                let relative = relative.to_path_buf();
                if !recent.contains(&relative) {
                    recent.push(relative);
                }
            }
        }
        let shared = Rc::new(std::cell::RefCell::new(recent.clone()));
        let palette =
            cx.new(|cx| jump::Palette::new(editor::quick_open::items(&recent), window, cx));
        palette.update(cx, |p, cx| {
            p.set_mode(jump::Mode::Files, cx);
            p.focus(window, cx);
        });
        let selected_files = shared.clone();
        let selected_root = root.clone();
        let events = cx.subscribe_in(
            &palette,
            window,
            move |this, _, event: &jump::Event, _, cx| {
                this.files.quick_open = None;
                this.files.quick_loading = false;
                this.files.refocus = true;
                if let jump::Event::Confirm(id) = *event {
                    let file = editor::quick_open::chosen(&selected_files.borrow(), id).cloned();
                    if let Some(file) = file {
                        this.open_file(selected_root.join(file), cx);
                    }
                }
                cx.notify();
            },
        );
        self.files.quick_open = Some((palette.clone(), events));
        self.files.quick_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let files = loop {
                if !this
                    .read_with(cx, |this, _| {
                        this.files.quick_generation == generation
                            && this.files.quick_loading
                            && this.files.quick_open.is_some()
                            && this.folder() == root
                    })
                    .unwrap_or(false)
                {
                    return;
                }
                let scan = root.clone();
                if let Some(files) = cx
                    .background_executor()
                    .spawn(async move { editor::quick_open::try_list(&scan) })
                    .await
                {
                    break files;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
            };
            this.update(cx, |this, cx| {
                if this.files.quick_generation != generation
                    || !this.files.quick_loading
                    || this.files.quick_open.is_none()
                    || this.folder() != root
                {
                    return;
                }
                let mut ordered: Vec<_> = recent
                    .into_iter()
                    .filter(|path| files.contains(path))
                    .collect();
                let mut seen: std::collections::HashSet<_> = ordered.iter().cloned().collect();
                for file in files {
                    if seen.insert(file.clone()) {
                        ordered.push(file);
                    }
                }
                let items = editor::quick_open::items(&ordered);
                *shared.borrow_mut() = ordered;
                palette.update(cx, |p, cx| p.set_items(items, cx));
                this.files.quick_loading = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn quick_open_palette(&self) -> Option<gpui::Entity<jump::Palette>> {
        self.files.quick_open.as_ref().map(|(p, _)| p.clone())
    }

    /// 닫히는 칸에 미저장 파일이 있으면 레이아웃 변경을 되돌리고 묻는다. 물었으면 true.
    pub(super) fn hold_dirty_close(
        &mut self,
        before: &Layout,
        policy: close::Policy,
        cx: &mut Context<Self>,
    ) -> bool {
        let lost: Vec<PaneId> = self
            .files
            .tabs
            .iter()
            .filter(|(p, t)| {
                (t.is_dirty() || t.saving) && self.layout.file(**p) != Some(t.path.as_path())
            })
            .map(|(p, _)| *p)
            .collect();
        if lost.is_empty() {
            let gone: Vec<PaneId> = self
                .files
                .tabs
                .keys()
                .filter(|p| {
                    self.files
                        .tabs
                        .get(p)
                        .is_some_and(|t| self.layout.file(**p) != Some(t.path.as_path()))
                })
                .copied()
                .collect();
            for p in gone {
                if let Some(tab) = self.files.tabs.remove(&p) {
                    self.files.forget(&tab);
                }
            }
            return false;
        }
        let after = std::mem::replace(&mut self.layout, before.clone());
        let detail = self.unsaved_detail(&lost);
        let Some(handle) = self.window else {
            return true;
        };
        // 액션 전달 중에는 창이 빌려져 있어 대화상자는 다음 틱에 띄운다.
        // ponytail: 확정 시 묻기 전 스냅샷(`after`)을 그대로 쓴다. 대화상자가 떠 있는 동안 생긴 레이아웃 변경(세션 붙기 등)은
        // 덮인다 — 세션은 분리된 세션으로 남는다. 잦아지면 닫기 연산을 다시 적용하는 방식으로.
        cx.spawn(async move |this, cx| {
            let Ok(answer) =
                cx.update_window(handle, |_, window, cx| prompt_unsaved(&detail, window, cx))
            else {
                return;
            };
            let Ok(close) = this.update(cx, |_, cx| Self::resolve_unsaved(answer, &lost, cx))
            else {
                return;
            };
            if close.await {
                this.update(cx, |this, cx| {
                    this.change_with_policy(true, policy, cx, move |l| *l = after);
                })
                .ok();
            }
        })
        .detach();
        true
    }

    fn dirty_files(&self) -> Vec<PaneId> {
        self.files
            .tabs
            .iter()
            .filter(|(_, t)| t.is_dirty() || t.saving)
            .map(|(p, _)| *p)
            .collect()
    }

    fn unsaved_detail(&self, panes: &[PaneId]) -> String {
        let names: Vec<String> = panes
            .iter()
            .filter_map(|p| self.layout.file(*p).map(editor::file_name))
            .collect();
        etext::UNSAVED_DETAIL.format(&[("files", &names.join("\n"))])
    }

    /// 대화상자 답을 기다려 처리한다: 저장(모두 성공해야 진행)·저장 안 함(편집 버림)·취소. 진행하면 true.
    fn resolve_unsaved(
        answer: impl std::future::Future<Output = Option<usize>> + 'static,
        panes: &[PaneId],
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let panes = panes.to_vec();
        cx.spawn(async move |this, cx| {
            let choice = answer.await;
            if choice == Some(0) {
                for pane in &panes {
                    let Ok(save) = this.update(cx, |this, cx| this.save_file(*pane, cx)) else {
                        return false;
                    };
                    if !save.await {
                        return false;
                    }
                }
                return this
                    .read_with(cx, |this, _| {
                        panes.iter().all(|p| {
                            this.files
                                .tabs
                                .get(p)
                                .is_none_or(|t| !t.is_dirty() && !t.saving)
                        })
                    })
                    .unwrap_or(false);
            }
            this.update(cx, |this, _cx| match choice {
                Some(1) => {
                    for p in &panes {
                        if let Some(tab) = this.files.tabs.remove(p) {
                            this.files.forget(&tab);
                        }
                    }
                    true
                }
                _ => false,
            })
            .unwrap_or(false)
        })
    }
}

/// 앱 종료의 유일한 경로(⌘Q·메뉴·메뉴바 아이콘). 셸 창마다 미저장 파일을 차례로 묻고, 취소가 없으면 끝낸다.
/// 액션 처리 중에는 그 창이 빌려져 있어 읽히지 않으므로(미저장을 못 보고 끝냄) 다음 틱에 본다.
pub(super) fn quit_app(cx: &mut App) {
    cx.defer(quit_now);
}

fn quit_now(cx: &mut App) {
    let dirty = cx
        .windows()
        .into_iter()
        .filter_map(|w| w.downcast::<Shell>())
        .find(|w| w.read(cx).is_ok_and(|s| !s.dirty_files().is_empty()));
    let Some(handle) = dirty else {
        // GPUI quit observers only have a 200ms budget. Finish filesystem work
        // while the event loop is still running, before requesting app shutdown.
        let pending = cx
            .windows()
            .into_iter()
            .filter_map(|w| w.downcast::<Shell>())
            .filter_map(|window| {
                window
                    .read(cx)
                    .ok()
                    .map(|shell| (window, shell.layout.clone(), shell.saver.barrier(cx)))
            })
            .collect::<Vec<_>>();
        let editor_saved = flush_editor_writes(cx);
        let settings_saved = krill_app::settings::flush_file_io(cx);
        let sidebar_saved = krill_app::sidebar::flush_file_io(cx);
        let star_saved = krill_app::star_nag::flush(cx);
        cx.spawn(async move |cx| {
            editor_saved.await;
            settings_saved.await;
            sidebar_saved.await;
            star_saved.await;
            let mut snapshots = Vec::new();
            for (window, layout, barrier) in pending {
                if !barrier.await {
                    return;
                }
                snapshots.push((window, layout));
            }
            cx.update(|cx| {
                if snapshots.iter().all(|(window, layout)| {
                    window.read(cx).is_ok_and(|shell| {
                        shell.layout == *layout && shell.dirty_files().is_empty()
                    })
                }) {
                    cx.quit();
                } else {
                    quit_app(cx);
                }
            });
        })
        .detach();
        return;
    };
    handle
        .update(cx, |shell, window, cx| {
            window.activate_window();
            let dirty = shell.dirty_files();
            let answer = prompt_unsaved(&shell.unsaved_detail(&dirty), window, cx);
            let close = Shell::resolve_unsaved(answer, &dirty, cx);
            cx.spawn(async move |_, cx| {
                // 이 창을 저장했거나 버렸으면 다음 창을 본다.
                if close.await {
                    cx.update(quit_app);
                }
            })
            .detach();
        })
        .ok();
}

/// 미저장 확인 대화상자(저장 · 저장 안 함 · 취소). 답은 버튼 순서.
fn prompt_unsaved(
    detail: &str,
    window: &mut Window,
    cx: &mut App,
) -> impl std::future::Future<Output = Option<usize>> + use<> {
    let answer = window.prompt(
        PromptLevel::Warning,
        etext::UNSAVED_TITLE.get(),
        Some(detail),
        &[etext::SAVE.get(), etext::DISCARD.get(), etext::CANCEL.get()],
        cx,
    );
    async move { answer.await.ok() }
}

pub(crate) mod bench;
#[cfg(test)]
#[cfg(unix)]
mod tests;
