//! MK-119: 가운데 영역 파일(에디터) 탭의 셸 통합 — 파일 칸 그리기·⌘S·⌘P·디스크 감시·미저장 닫기/종료 확인.
//! 상태기계와 디스크 IO는 [`krill_app::editor`](Orca 참조 출처도 그쪽 문서). 파일 칸은 세션에 붙지 않는다.
//! MK-121: 분류([`editor::open`])에 따라 코드 에디터·편집/미리보기 토글(마크다운·CSV·ipynb·SVG)·이미지·표·자리표시자를 고른다.

use std::path::Path;

use gpui::PromptLevel;
use gpui_component::input::{Editor, EditorState, InputEvent};
use gpui_component::{Icon, Selectable as _};
use krill_app::editor::{self, Disk, Doc, Opened, Poll, Preview, SaveError, Unopenable};
use krill_app::file_format::view::{self as fview, CsvView, ImageView};
use krill_app::file_format::{Kind, csv, ipynb};
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
    // std `File::lock`은 Unix에서 `flock(LOCK_EX)`라 CLI 쪽 잠금과 같고 Windows에서도 돈다.
    if lock.lock().is_err() {
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

actions!(shell, [SaveFile, QuickOpen]);

pub(super) fn bind_keys(cx: &mut App) {
    let ctx = Some(KEY_CONTEXT);
    cx.bind_keys([
        krill_app::keys::bind("cmd-s", SaveFile, ctx),
        krill_app::keys::bind("cmd-p", QuickOpen, ctx),
    ]);
}

pub(super) struct Files {
    /// 불러온 파일 칸. 보인 적 있는 칸만 들어 있다(숨은 복원 탭은 처음 보일 때 읽는다).
    tabs: HashMap<PaneId, FileTab>,
    watch: Option<Task<()>>,
    watching: bool,
    quick_open: Option<(gpui::Entity<jump::Palette>, Subscription)>,
    /// 탭·칸 포커스가 바뀌었다. 다음 그리기에서 키보드 포커스를 에디터나 터미널로 옮긴다.
    pub(super) refocus: bool,
    close_guard: bool,
    /// 다음 그리기 전에 디코드 캐시를 버릴 이미지 경로(재로드·저장·탭 닫힘). 놓으려면 창이 있어야 해서 모아 둔다.
    released: Vec<PathBuf>,
    /// 터미널 경로 링크(OWE-663 4번): 디스크 확인 캐시, ⌘ 아래 링크(칸, 행, 링크), 열고 나서 옮길 (파일, 줄, 열).
    links: path_link::Cache,
    pub(super) hover: Option<(PaneId, u16, PathLink)>,
    goto: Option<(PathBuf, u32, u32)>,
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
            released: Vec::new(),
            links: path_link::Cache::default(),
            hover: None,
            goto: None,
        }
    }
}

impl Files {
    pub(super) fn close_quick_open(&mut self) {
        self.quick_open = None;
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
    Notebook(Result<ipynb::Notebook, ipynb::ParseError>),
    Image(gpui::Entity<ImageView>),
}

/// 에디터 내용으로 미리보기를 만든다. SVG는 디스크 파일을 그린다(저장한 내용).
// ponytail: SVG 미리보기는 미저장 편집을 반영하지 않는다 — 필요하면 `gpui::Image::from_bytes`로 그린다.
fn build_preview(preview: Preview, text: &str, path: &Path, cx: &mut App) -> Shown {
    static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    match preview {
        Preview::Markdown => {
            let n = GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Shown::Markdown(
                SharedString::from(text.to_owned()),
                ElementId::Name(format!("file-md-{n}").into()),
            )
        }
        Preview::Table(delimiter) => {
            let head = text.as_bytes();
            let head = head.get(..csv::PREVIEW_BYTES).unwrap_or(head);
            let len = u64::try_from(text.len()).unwrap_or(u64::MAX);
            let table = csv::preview(head, len, delimiter);
            Shown::Table(cx.new(|_| CsvView::new(table)))
        }
        Preview::Notebook => Shown::Notebook(ipynb::parse(text)),
        Preview::Svg => Shown::Image(cx.new(|_| ImageView::new(path.to_owned(), None))),
    }
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
                this.save_file(pane, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &QuickOpen, window, cx| {
            this.toggle_quick_open(window, cx);
        }))
        .on_action(cx.listener(|_, _: &Quit, _, cx| quit_app(cx)))
    }

    /// 파일을 가운데 영역의 탭으로 연다(이미 열려 있으면 그 탭으로).
    pub(super) fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.close_settings(None, cx);
        self.change(true, cx, move |l| {
            l.open_file(path);
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
    fn pane_cwd(&self, pane: PaneId, cx: &App) -> Option<PathBuf> {
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
        cx: &App,
    ) -> Option<(u16, PathLink)> {
        let cwd = self.pane_cwd(pane, cx);
        let l = self.live.get(&pane)?;
        let screen = l.pane.shown();
        let (column, line) = cell_at(&l.painted, &screen, position)?;
        if screen.link_at(line, column).is_some() {
            return None;
        }
        let row = screen.rows.get(usize::from(line))?;
        let link = self.files.links.link_at(row, column, cwd.as_deref())?;
        Some((line, link))
    }

    /// ⌘를 누른 채 움직이면 경로 밑줄을 갱신한다. 바뀔 때만 다시 그린다.
    pub(super) fn update_path_hover(
        &mut self,
        pane: PaneId,
        position: gpui::Point<Pixels>,
        cmd: bool,
        cx: &mut Context<Self>,
    ) {
        let next = if cmd {
            self.path_link_at(pane, position, cx)
                .map(|(line, link)| (pane, line, link))
        } else {
            None
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

    /// 칸에 그릴 경로 밑줄.
    pub(super) fn path_hover(&self, pane: PaneId) -> Option<Highlight> {
        let (_, line, link) = self.files.hover.as_ref().filter(|h| h.0 == pane)?;
        Some(Highlight {
            line: *line,
            column: link.column,
            cols: link.cols,
        })
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
    pub(super) fn render_file_pane(&self, id: PaneId, cx: &mut Context<Self>) -> AnyElement {
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
                    .text_size(krill_app::ui::TEXT_XS)
                    .text_color(p.muted_foreground)
                    .child(path.display().to_string()),
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
                .bg(p.warning)
                .text_color(p.warning_foreground)
                .text_size(krill_app::ui::TEXT_XS)
                .child(div().min_w_0().child(msg.to_owned()))
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
                                    .label(etext::RELOAD.get())
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.reload_file(id, window, cx);
                                    })),
                            )
                            .child(
                                Button::new(("file-keep", id.get()))
                                    .label(etext::KEEP.get())
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(doc) = this
                                            .files
                                            .tabs
                                            .get_mut(&id)
                                            .and_then(|t| t.doc.as_mut())
                                        {
                                            let now = editor::stamp(doc.path()).ok().flatten();
                                            doc.keep_mine(now);
                                        }
                                        cx.notify();
                                    })),
                            ),
                    ),
            ),
        };
        let error = tab.and_then(|t| t.error.clone()).map(|e| {
            div()
                .debug_selector(|| "file-error".into())
                .px_2()
                .py_1()
                .text_size(krill_app::ui::TEXT_XS)
                .text_color(p.destructive)
                .child(e)
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
                Shown::Markdown(src, md_id) => fview::markdown(md_id.clone(), src.clone()),
                Shown::Table(view) => view.clone().into_any_element(),
                Shown::Notebook(Ok(nb)) => fview::notebook(("file-ipynb", id.get()), nb, cx),
                Shown::Notebook(Err(_)) => notice(ftext::IPYNB_INVALID.get().to_owned()),
                Shown::Image(view) => view.clone().into_any_element(),
            },
            Some(Body::Text { state, .. }) => Editor::new(state)
                .bordered(false)
                .h(gpui::relative(1.))
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
        let button = |name: &'static str, label: &'static str, on: bool| {
            Button::new((name, id.get()))
                .label(label)
                .ghost()
                .xsmall()
                .selected(previewing == on)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.set_preview(id, on, window, cx);
                }))
        };
        Some(
            div()
                .flex()
                .flex_none()
                .gap_0p5()
                .debug_selector(|| "file-preview-toggle".into())
                .child(button("file-preview", preview_label, true))
                .child(button("file-edit", edit_label, false)),
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
        if matches!(shown, Some(Shown::Image(_))) || (on && *preview == Preview::Svg) {
            // 그사이 저장했을 수 있으니 SVG 캐시를 버리고 다시 그린다. 나갈 때도 디코드 결과를 놓는다.
            if let Some(doc) = &tab.doc {
                self.files.released.push(doc.path().to_owned());
            }
        }
        *shown = if on {
            let text = state.read(cx).text().to_string();
            let path = tab
                .doc
                .as_ref()
                .map(|d| d.path().to_owned())
                .unwrap_or_default();
            Some(build_preview(*preview, &text, &path, cx))
        } else {
            None
        };
        self.files.refocus = true;
        cx.notify();
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
                            .size(px(6.))
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
    pub(super) fn poll_file_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state_dir) = self.file_requests_dir(cx) else {
            return;
        };
        let requests = take_pending_file_requests(&state_dir);
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
        self.poll_file_requests(window, cx);
        self.apply_goto(window, cx);
        for pane in self.layout.visible() {
            if let Some(path) = self.layout.file(pane)
                && !self.files.tabs.contains_key(&pane)
            {
                let mut tab = Self::load_file_tab(pane, path.to_owned(), window, cx);
                if self
                    .files
                    .goto
                    .as_ref()
                    .is_some_and(|(goto_path, _, _)| goto_path == path)
                    && let Body::Text { shown, .. } = &mut tab.body
                {
                    *shown = None;
                }
                self.files.tabs.insert(pane, tab);
            }
        }
        if !self.files.watching
            && (self.layout.files().next().is_some() || self.file_requests_dir(cx).is_some())
        {
            self.files.watching = true;
            self.files.watch = Some(Self::watch_files(window, cx));
        }
        // 팔레트·검색 바가 떠 있으면 그쪽 포커스를 뺏지 않는다.
        if self.jump.is_none()
            && self.search.is_none()
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
                let Some(shell) = weak.upgrade() else {
                    return true;
                };
                let dirty = shell.read(cx).dirty_files();
                if dirty.is_empty() {
                    return true;
                }
                let detail = shell.read(cx).unsaved_detail(&dirty);
                let answer = prompt_unsaved(&detail, window, cx);
                let (weak, handle) = (weak.clone(), window.window_handle());
                cx.spawn(async move |cx| {
                    let close = weak
                        .update(cx, |_, cx| Self::resolve_unsaved(answer, &dirty, cx))
                        .ok();
                    let Some(close) = close else { return };
                    if close.await {
                        handle
                            .update(cx, |_, window, _| window.remove_window())
                            .ok();
                    }
                })
                .detach();
                false
            });
        }
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
            window.request_animation_frame();
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
    }

    fn load_file_tab(
        pane: PaneId,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FileTab {
        let (body, stamp) = match editor::open(&path) {
            Ok((Opened::Text(kind, text), stamp)) => {
                let language = editor::language(kind, &path);
                let preview = editor::preview(kind);
                let shown = preview.map(|preview| build_preview(preview, &text, &path, cx));
                let state = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language)
                        .line_number(true)
                        .default_value(text)
                });
                // 에디터가 받은 그대로를 기준으로 삼는다(정규화가 있어도 열자마자 dirty가 되지 않게).
                let baseline = state.read(cx).text().to_string();
                let edits = cx.subscribe(&state, move |this, _, e: &InputEvent, cx| {
                    if matches!(e, InputEvent::Change) {
                        this.file_edited(pane, cx);
                    }
                });
                return FileTab {
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
                    doc: None,
                    body: Body::Failed(e.to_string()),
                    error: None,
                };
            }
        };
        // 텍스트가 아닌 탭도 디스크 지문을 들고 있어 바뀌면 다시 읽는다(편집이 없어 늘 깨끗하다).
        FileTab {
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
        let unchanged = *state.read(cx).text() == *doc.saved();
        if doc.edited(unchanged) {
            cx.notify();
        }
    }

    /// ⌘S: 원자적으로 저장한다. 성공하면 true.
    // ponytail: fsync까지 UI 스레드에서 한다. 네트워크 볼륨에서 멈춤이 보이면 백그라운드 작업으로.
    pub(super) fn save_file(&mut self, pane: PaneId, cx: &mut Context<Self>) -> bool {
        let Some(tab) = self.files.tabs.get_mut(&pane) else {
            return false;
        };
        let (Some(doc), Body::Text { state, .. }) = (tab.doc.as_mut(), &tab.body) else {
            return false;
        };
        let text = state.read(cx).text().to_string();
        let saved = match doc.save(text) {
            Ok(()) => {
                tab.error = None;
                true
            }
            Err(SaveError::Conflict) => {
                tab.error = Some(etext::SAVE_CONFLICT.get().to_owned());
                false
            }
            Err(SaveError::Io(e)) => {
                krill_app::onboarding::actions::note_access_denied(doc.path(), &e, cx);
                tab.error = Some(etext::SAVE_FAILED.format(&[("error", &e.to_string())]));
                false
            }
        };
        cx.notify();
        saved
    }

    /// 디스크 내용으로 다시 읽는다(깨끗한 탭의 자동 재로드, 또는 dirty 탭에서 사용자가 고름). 커서 줄은 유지한다.
    fn reload_file(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.files.tabs.get_mut(&pane) else {
            return;
        };
        if !matches!(tab.body, Body::Text { .. }) {
            // 이미지·표·자리표시자: 분류부터 다시 한다(종류가 바뀌었을 수 있다). 이미지 캐시도 버린다.
            let Some(path) = tab.doc.as_ref().map(|d| d.path().to_owned()) else {
                return;
            };
            if tab.shows_image() {
                self.files.released.push(path.clone());
            }
            let fresh = Self::load_file_tab(pane, path, window, cx);
            self.files.tabs.insert(pane, fresh);
            cx.notify();
            return;
        }
        let (
            Some(doc),
            Body::Text {
                state,
                preview,
                shown,
                ..
            },
        ) = (tab.doc.as_mut(), &mut tab.body)
        else {
            return;
        };
        match editor::open(doc.path()) {
            Ok((Opened::Text(_, text), stamp)) => {
                state.update(cx, |s, cx| {
                    let cursor = s.cursor_position();
                    s.set_value(text, window, cx);
                    s.set_cursor_position(cursor, window, cx);
                });
                let text = state.read(cx).text().to_string();
                if let (Some(preview), Some(old)) = (*preview, shown.as_ref()) {
                    if matches!(old, Shown::Image(_)) {
                        self.files.released.push(doc.path().to_owned());
                    }
                    *shown = Some(build_preview(preview, &text, doc.path(), cx));
                }
                doc.reloaded(text, stamp);
                tab.error = None;
            }
            Ok((other, stamp)) => {
                doc.keep_mine(Some(stamp));
                tab.error = Some(not_text(&other));
            }
            Err(e) => {
                krill_app::onboarding::actions::note_access_denied(doc.path(), &e, cx);
                doc.keep_mine(editor::stamp(doc.path()).ok().flatten());
                tab.error = Some(etext::OPEN_FAILED.format(&[("error", &e.to_string())]));
            }
        }
        cx.notify();
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
                        .filter_map(|(p, t)| Some((*p, t.doc.as_ref()?.path().to_owned())))
                        .collect::<Vec<_>>();
                    Some((paths, req_dir))
                }) else {
                    return;
                };
                let (stamps, has_requests) = cx
                    .background_executor()
                    .spawn(async move {
                        let has_requests = req_dir.as_deref().is_some_and(|dir| {
                            matches!(dir.join(FILE_REQUESTS_FILE).try_exists(), Ok(true))
                        });
                        let stamps = paths
                            .into_iter()
                            .map(|(p, path)| (p, editor::stamp(&path)))
                            .collect::<Vec<_>>();
                        (stamps, has_requests)
                    })
                    .await;
                if stamps.is_empty() && !has_requests {
                    continue;
                }
                let polled = this.update_in(cx, |this, window, cx| {
                    if has_requests {
                        this.poll_file_requests(window, cx);
                    }
                    for (pane, now) in stamps {
                        // stat 자체가 실패하면(권한 등) 판단하지 않고 다음 주기에 다시 본다.
                        let Ok(now) = now else { continue };
                        let Some(doc) = this.files.tabs.get_mut(&pane).and_then(|t| t.doc.as_mut())
                        else {
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

    /// ⌘P: 작업 폴더의 파일을 퍼지 검색해 연다. 목록은 백그라운드에서 만든 뒤 팔레트를 띄운다.
    fn toggle_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.files.quick_open.take().is_some() {
            self.files.refocus = true;
            cx.notify();
            return;
        }
        let root = self.folder();
        cx.spawn_in(window, async move |this, cx| {
            let scan = root.clone();
            let files = cx
                .background_executor()
                .spawn(async move { editor::quick_open::list(&scan) })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.settings.is_some() {
                    return;
                }
                if let Some(pane) = this.layout.focused() {
                    this.commit_ime(pane, Some(window), cx);
                }
                let items = editor::quick_open::items(&files);
                let palette = cx.new(|cx| jump::Palette::new(items, window, cx));
                palette.update(cx, |p, cx| {
                    p.input().update(cx, |input, cx| {
                        input.set_placeholder(etext::QUICK_OPEN.get(), window, cx);
                    });
                    p.focus(window, cx);
                });
                let events =
                    cx.subscribe_in(&palette, window, move |this, _, e: &jump::Event, _, cx| {
                        this.files.quick_open = None;
                        this.files.refocus = true;
                        if let jump::Event::Confirm(id) = *e
                            && let Some(rel) = editor::quick_open::chosen(&files, id)
                        {
                            this.open_file(root.join(rel), cx);
                        }
                        cx.notify();
                    });
                this.files.quick_open = Some((palette, events));
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
    pub(super) fn hold_dirty_close(&mut self, before: &Layout, cx: &mut Context<Self>) -> bool {
        let lost: Vec<PaneId> = self
            .files
            .tabs
            .iter()
            .filter(|(p, t)| t.is_dirty() && self.layout.file(**p).is_none())
            .map(|(p, _)| *p)
            .collect();
        if lost.is_empty() {
            let gone: Vec<PaneId> = self
                .files
                .tabs
                .keys()
                .filter(|p| self.layout.file(**p).is_none())
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
                this.update(cx, |this, cx| this.change(true, cx, move |l| *l = after))
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
            .filter(|(_, t)| t.is_dirty())
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
            this.update(cx, |this, cx| match choice {
                Some(0) => panes.iter().all(|p| this.save_file(*p, cx)),
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
        cx.quit();
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
