//! 읽기 전용 미리보기 뷰어(gpui). 입력은 경로와 이미 읽은 데이터 — 파일 읽기·탭 연결은 OWE-665가 한다.
//! 화면 구성은 Orca `ImageViewer.tsx`·`EditorMarkdownFileSurface.tsx`·`csv/CsvGrid.tsx`·`IpynbViewer.tsx`를 참고했다
//! (Copyright (c) 2026 Lovecast Inc., MIT License, .ref/orca/LICENSE).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, ElementId, Image, ObjectFit, SharedString, UniformListScrollHandle,
    Window, div, img, prelude::*, px, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::text::TextView;
use gpui_component::{Disableable as _, IconName, Selectable as _, Sizable as _};

use super::csv::Table;
use super::ipynb::{CellKind, Notebook, Output};
use super::raster::{Dimensions, RasterError};
use super::{Kind, Zoom};
use crate::i18n::catalog::file_format as text;

const CSV_ROW_HEIGHT: f32 = 24.;
/// 열 너비 추정용 글자 폭. 실화면(`TEXT_SM` 고정폭)에서 글자당 약 7.2pt라 7로는 짧은 값도 잘렸다(MK-121 QA).
/// ponytail: 글꼴을 재지 않은 고정 추정값 — 잘림이 거슬리면 `TextSystem`으로 머리 행을 재서 정한다.
const CSV_CHAR_WIDTH: f32 = 8.;
const CSV_MIN_CHARS: usize = 4;
const CSV_MAX_CHARS: usize = 40;
/// 열 너비를 정할 때 보는 앞 행 수.
const CSV_WIDTH_SAMPLE_ROWS: usize = 100;

#[derive(Debug)]
pub struct ImageView {
    source: ImageSource,
    /// SVG처럼 헤더로 크기를 모르면 `None` — 맞춤만 쓴다.
    dims: Option<Dimensions>,
    zoom: Zoom,
}

/// 이미지 출처: 디스크 경로(래스터·저장된 파일) 또는 미리보기용 메모리 버퍼(SVG 원본 내용).
#[derive(Debug, Clone)]
enum ImageSource {
    Path(PathBuf),
    SvgBytes(Arc<Image>),
}

impl ImageView {
    pub fn new(path: PathBuf, dims: Option<Dimensions>) -> Self {
        Self {
            source: ImageSource::Path(path),
            dims,
            zoom: Zoom::Fit,
        }
    }

    /// Install bytes whose image identity was prepared on the background executor.
    pub fn new_svg_image(image: Arc<Image>) -> Self {
        Self {
            source: ImageSource::SvgBytes(image),
            dims: None,
            zoom: Zoom::Fit,
        }
    }

    /// 메모리 SVG 바이트(경로 기반이면 `None`). 미리보기가 버퍼 기준인지 확인할 때 쓴다.
    pub fn svg_bytes(&self) -> Option<Vec<u8>> {
        match &self.source {
            ImageSource::SvgBytes(image) => Some(image.bytes.clone()),
            ImageSource::Path(_) => None,
        }
    }

    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    pub fn zoom_in(&mut self, cx: &mut Context<Self>) {
        if self.dims.is_some() {
            self.set_zoom(self.zoom.zoom_in(), cx);
        }
    }

    pub fn zoom_out(&mut self, cx: &mut Context<Self>) {
        if self.dims.is_some() {
            self.set_zoom(self.zoom.zoom_out(), cx);
        }
    }

    pub fn zoom_reset(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(Zoom::Fit, cx);
    }

    fn set_zoom(&mut self, zoom: Zoom, cx: &mut Context<Self>) {
        self.zoom = zoom;
        cx.notify();
    }
}

impl Render for ImageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = crate::ui::palette(cx);
        let fixed = self.dims.is_none();
        let tool = |id: &'static str, label: &'static str| {
            Button::new(id)
                .ghost()
                .xsmall()
                .label(label)
                .accessibility_label(label)
        };
        let toolbar = div()
            .debug_selector(|| "image-toolbar".into())
            .flex()
            .flex_wrap()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(pal.border)
            .child(
                tool("image-fit", text::FIT.get())
                    .debug_selector(|| "image-fit-button".into())
                    .selected(self.zoom == Zoom::Fit)
                    .on_click(cx.listener(|v, _, _, cx| v.set_zoom(Zoom::Fit, cx))),
            )
            .child(
                tool("image-actual", text::ACTUAL_SIZE.get())
                    .debug_selector(|| "image-actual-button".into())
                    .selected(self.zoom == Zoom::Scale(1.))
                    .disabled(fixed)
                    .on_click(cx.listener(|v, _, _, cx| v.set_zoom(Zoom::Scale(1.), cx))),
            )
            .child(
                Button::new("image-zoom-out")
                    .debug_selector(|| "image-zoom-out".into())
                    .ghost()
                    .xsmall()
                    .icon(IconName::Minus)
                    .tooltip(text::ZOOM_OUT.get())
                    .accessibility_label(text::ZOOM_OUT.get())
                    .disabled(fixed)
                    .on_click(cx.listener(|v, _, _, cx| v.set_zoom(v.zoom.zoom_out(), cx))),
            )
            .child(
                Button::new("image-zoom-in")
                    .debug_selector(|| "image-zoom-in".into())
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip(text::ZOOM_IN.get())
                    .accessibility_label(text::ZOOM_IN.get())
                    .disabled(fixed)
                    .on_click(cx.listener(|v, _, _, cx| v.set_zoom(v.zoom.zoom_in(), cx))),
            )
            .when_some(scale_label(self.zoom), |d, label| {
                d.child(
                    div()
                        .font_family(crate::ui::FONT_MONO)
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(pal.muted_foreground)
                        .child(label),
                )
            });
        let image = match &self.source {
            ImageSource::Path(path) => img(path.clone()),
            ImageSource::SvgBytes(image) => img(image.clone()),
        };
        let body = match (self.zoom, self.dims) {
            (Zoom::Scale(scale), Some(dims)) => {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "한 변은 raster::MAX_DIMENSION(32,768) 이하라 f32로 정확하다"
                )]
                let (w, h) = (dims.width as f32 * scale, dims.height as f32 * scale);
                div()
                    .id("image-scroll")
                    .size_full()
                    .overflow_scroll()
                    .p(crate::ui::SPACE_4)
                    .child(image.flex_none().w(px(w)).h(px(h)))
            }
            // 맞춤: 작은 이미지는 키우지 않는다(Orca `getZoomedImageLayoutSize`의 min(1, …)).
            // 칸을 절대 위치로 채워 높이를 확정한다 — 퍼센트 높이만 주면 이미지가 너비÷비율로 키를 잡아
            // 칸보다 커지고, 가운데 정렬된 그림이 보이는 영역 밖에 그려져 확대·축소 전까지 비어 보였다.
            _ => div()
                .id("image-scroll")
                .debug_selector(|| "image-scroll".into())
                .size_full()
                .relative()
                .child(
                    div()
                        .absolute()
                        .top(crate::ui::SPACE_4)
                        .bottom(crate::ui::SPACE_4)
                        .left(crate::ui::SPACE_4)
                        .right(crate::ui::SPACE_4)
                        .child(
                            image
                                .absolute()
                                .size_full()
                                .object_fit(ObjectFit::ScaleDown)
                                .debug_selector(|| "image-fit".into()),
                        ),
                ),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(pal.editor_surface)
            .font_family(crate::ui::FONT_UI)
            .text_size(crate::ui::TEXT_SM)
            .text_color(pal.foreground)
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// 경로 이미지의 디코드 캐시와 GPU 텍스처를 놓는다. gpui는 경로별로 영구 캐시하므로 디스크 내용이 바뀌었거나
/// (재로드) 탭이 닫히면 불러야 다음 그리기에서 새로 읽고, 닫힌 이미지가 메모리에 남지 않는다.
pub fn release_image(path: &Path, window: &mut Window, cx: &mut App) {
    let resource = gpui::Resource::Path(path.into());
    if let Some(Ok(image)) = window.get_asset::<gpui::ImgResourceLoader>(&resource, cx) {
        cx.drop_image(image, Some(window));
    }
    cx.remove_asset::<gpui::ImgResourceLoader>(&resource);
}

fn scale_label(zoom: Zoom) -> Option<String> {
    match zoom {
        Zoom::Fit => None,
        Zoom::Scale(s) => Some(format!("{:.0}%", s * 100.)),
    }
}

/// 이미지 헤더 검사 실패 문구.
pub fn image_error(error: RasterError) -> String {
    match error {
        RasterError::Invalid => text::IMAGE_INVALID.get().to_owned(),
        RasterError::TooLarge(d) => text::IMAGE_TOO_LARGE.format(&[
            ("width", &d.width.to_string()),
            ("height", &d.height.to_string()),
        ]),
    }
}

/// 마크다운 미리보기(gpui-component `TextView`). 원본↔미리보기 토글은 파일 탭 쪽(OWE-663/665)에서.
pub fn markdown(id: impl Into<ElementId>, source: impl Into<SharedString>, cx: &App) -> AnyElement {
    let pal = crate::ui::palette(cx);
    let id = id.into();
    div()
        .id(id.clone())
        .size_full()
        .overflow_y_scroll()
        .p(crate::ui::SPACE_4)
        .font_family(crate::ui::FONT_UI)
        .text_size(crate::ui::TEXT_MD)
        .bg(pal.editor_surface)
        .text_color(pal.foreground)
        .child(TextView::markdown(id, source))
        .into_any_element()
}

/// CSV/TSV 표. 머리 행은 고정, 나머지는 `uniform_list`로 보이는 행만 그린다. 가로는 열 너비 합만큼 스크롤.
#[derive(Debug)]
pub struct CsvView {
    table: Arc<Table>,
    widths: Vec<f32>,
    scroll: UniformListScrollHandle,
}

impl CsvView {
    pub fn new(table: Table) -> Self {
        let widths = column_widths(&table);
        Self {
            table: Arc::new(table),
            widths,
            scroll: UniformListScrollHandle::new(),
        }
    }
}

/// 앞 [`CSV_WIDTH_SAMPLE_ROWS`]행의 가장 긴 셀 글자 수로 열 너비를 정한다.
fn column_widths(table: &Table) -> Vec<f32> {
    (0..table.columns)
        .map(|col| {
            let chars = table
                .rows
                .iter()
                .take(CSV_WIDTH_SAMPLE_ROWS)
                .filter_map(|row| row.get(col))
                .map(|cell| cell.chars().take(CSV_MAX_CHARS).count())
                .max()
                .unwrap_or(0)
                .clamp(CSV_MIN_CHARS, CSV_MAX_CHARS);
            #[expect(clippy::cast_precision_loss, reason = "CSV_MAX_CHARS 이하라 정확하다")]
            let chars = chars as f32;
            chars * CSV_CHAR_WIDTH + 16.
        })
        .collect()
}

fn csv_row(table: &Arc<Table>, row: usize, widths: &[f32], border: gpui::Hsla) -> gpui::Div {
    let cells = table.rows.get(row).map_or(&[][..], Vec::as_slice);
    div()
        .flex()
        .items_center()
        .h(px(CSV_ROW_HEIGHT))
        .border_b_1()
        .border_color(border)
        .children(widths.iter().enumerate().map(|(i, &w)| {
            let source = Arc::clone(table);
            div()
                .id((
                    "csv-cell",
                    row.saturating_mul(table.columns).saturating_add(i),
                ))
                .cursor_pointer()
                .tooltip(|window, cx| {
                    gpui_component::tooltip::Tooltip::new(crate::i18n::catalog::shell::COPY.get())
                        .build(window, cx)
                })
                .on_click(move |_, _, cx| {
                    if let Some(value) = source.rows.get(row).and_then(|r| r.get(i)) {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(value.clone()));
                    }
                })
                .flex_none()
                .w(px(w))
                .px_2()
                .border_r_1()
                .border_color(border)
                .truncate()
                .child(
                    cells
                        .get(i)
                        .map(|value| value.chars().take(512).collect::<String>())
                        .unwrap_or_default(),
                )
        }))
}

impl Render for CsvView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = crate::ui::palette(cx);
        let border = pal.border;
        let total: f32 = self.widths.iter().sum();
        let body_rows = self.table.rows.len().saturating_sub(1);
        let header = self.table.rows.first().map_or_else(div, |_| {
            csv_row(&self.table, 0, &self.widths, border)
                .bg(pal.muted)
                .font_weight(gpui::FontWeight::SEMIBOLD)
        });
        let list = uniform_list(
            "csv-rows",
            body_rows,
            cx.processor(move |v, range: std::ops::Range<usize>, _, _| {
                range
                    .filter_map(|i| {
                        v.table.rows.get(i + 1)?;
                        Some(csv_row(&v.table, i + 1, &v.widths, border))
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1()
        .min_h_0();
        let notice = self.table.truncated.then(|| {
            div()
                .flex_none()
                .px_2()
                .py_1()
                .font_family(crate::ui::FONT_UI)
                .text_size(crate::ui::TEXT_XS)
                .text_color(pal.muted_foreground)
                .child(text::CSV_TRUNCATED.format(&[("rows", &self.table.rows.len().to_string())]))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(pal.editor_surface)
            .text_color(pal.foreground)
            .text_size(crate::ui::TEXT_SM)
            .font_family(crate::ui::FONT_MONO)
            .children(notice)
            .child(
                div()
                    .id("csv-hscroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_x_scroll()
                    .child(
                        div()
                            .w(px(total))
                            .h_full()
                            .flex()
                            .flex_col()
                            .child(header.flex_none())
                            .child(list),
                    ),
            )
    }
}

/// Prepared off-thread; both tiny cells and many outputs have one virtual row each.
#[derive(Debug)]
pub struct NotebookPreview {
    notebook: Notebook,
    rows: Vec<(usize, Option<usize>, std::ops::Range<usize>)>,
    errors: std::collections::BTreeMap<(usize, usize), String>,
}
impl NotebookPreview {
    pub fn new(notebook: Notebook) -> Self {
        let mut rows = Vec::new();
        let mut errors = std::collections::BTreeMap::new();
        for (cell, value) in notebook.cells.iter().enumerate() {
            let chunks = if value.kind == CellKind::Markdown {
                std::iter::once(0..value.source.len()).collect()
            } else {
                preview_chunks(&value.source)
            };
            for range in chunks {
                rows.push((cell, None, range));
            }
            for (output, value) in value.outputs.iter().enumerate() {
                match value {
                    Output::Stream { text, .. } | Output::Text(text) => {
                        for range in preview_chunks(text) {
                            rows.push((cell, Some(output), range));
                        }
                    }
                    Output::Error { name, message } => {
                        let text = format!("{name}: {message}");
                        for range in preview_chunks(&text) {
                            rows.push((cell, Some(output), range));
                        }
                        errors.insert((cell, output), text);
                    }
                    Output::Omitted(_) => rows.push((cell, Some(output), 0..0)),
                }
            }
        }
        Self {
            notebook,
            rows,
            errors,
        }
    }
}
// A single unbroken line must not make one virtual row arbitrarily expensive.
fn preview_chunks(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut chars = 0usize;
    let mut lines = 0usize;
    for (index, ch) in text.char_indices() {
        chars += 1;
        lines += usize::from(ch == '\n');
        if chars >= 2_048 || lines >= 32 {
            let end = index + ch.len_utf8();
            rows.push(start..end);
            start = end;
            chars = 0;
            lines = 0;
        }
    }
    if start < text.len() || rows.is_empty() {
        rows.push(start..text.len());
    }
    rows
}

struct NotebookList {
    source: Arc<NotebookPreview>,
    state: gpui::ListState,
}

fn notebook_row(
    preview: &NotebookPreview,
    index: usize,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let pal = crate::ui::palette(cx);
    let Some((cell_index, output_index, range)) = preview.rows.get(index) else {
        return div().into_any_element();
    };
    let Some(cell) = preview.notebook.cells.get(*cell_index) else {
        return div().into_any_element();
    };
    let mono = |value: String| {
        div()
            .font_family(crate::ui::FONT_MONO)
            .text_size(crate::ui::TEXT_SM)
            .whitespace_normal()
            .child(value)
    };
    let body = if let Some(index) = output_index {
        match cell.outputs.get(*index) {
            Some(Output::Stream { text, .. } | Output::Text(text)) => {
                mono(text.get(range.clone()).unwrap_or_default().to_owned()).into_any_element()
            }
            Some(Output::Error { .. }) => mono(
                preview
                    .errors
                    .get(&(*cell_index, *index))
                    .and_then(|value| value.get(range.clone()))
                    .unwrap_or_default()
                    .to_owned(),
            )
            .text_color(pal.destructive)
            .into_any_element(),
            Some(Output::Omitted(mime)) => div()
                .text_size(crate::ui::TEXT_XS)
                .text_color(pal.muted_foreground)
                .child(
                    text::IPYNB_OUTPUT_OMITTED
                        .format(&[("mime", &mime.chars().take(256).collect::<String>())]),
                )
                .into_any_element(),
            None => div().into_any_element(),
        }
    } else {
        let source = match cell.kind {
            CellKind::Markdown => {
                let long = cell.source.chars().take(2_049).count() > 2_048;
                let expanded =
                    window.use_keyed_state(("notebook-markdown-expand", index), cx, |_, _| false);
                let show_all = *expanded.read(cx);
                let source = if !long || show_all {
                    cell.source.clone()
                } else {
                    cell.source.chars().take(2_048).collect()
                };
                div()
                    .child(TextView::markdown(index, source))
                    .when(long && !show_all, |view| {
                        view.child(
                            Button::new(("notebook-markdown-more", index))
                                .label(crate::i18n::catalog::jump::SEE_MORE.get())
                                .on_click(move |_, _, cx| {
                                    expanded.update(cx, |value, cx| {
                                        *value = true;
                                        cx.notify();
                                    });
                                }),
                        )
                    })
                    .into_any_element()
            }
            CellKind::Code | CellKind::Raw => mono(
                cell.source
                    .get(range.clone())
                    .unwrap_or_default()
                    .to_owned(),
            )
            .p_2()
            .rounded(crate::ui::RADIUS_SM)
            .bg(pal.muted)
            .into_any_element(),
        };
        let prompt = (cell.kind == CellKind::Code && range.start == 0).then(|| {
            div()
                .font_family(crate::ui::FONT_MONO)
                .text_size(crate::ui::TEXT_XS)
                .text_color(pal.muted_foreground)
                .child(
                    cell.execution_count
                        .map_or_else(|| "[ ]".to_owned(), |count| format!("[{count}]")),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(prompt)
            .child(source)
            .into_any_element()
    };
    div()
        .id(("notebook-row", index))
        .debug_selector(move || format!("notebook-row-{index}"))
        .w_full()
        .px(crate::ui::SPACE_4)
        .pb_3()
        .child(body)
        .into_any_element()
}

/// Variable-height virtualization creates only rows intersecting the viewport.
pub fn notebook(
    id: impl Into<ElementId>,
    preview: Arc<NotebookPreview>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let pal = crate::ui::palette(cx);
    let id = id.into();
    let count = preview.rows.len();
    let state = window.use_keyed_state(id.clone(), cx, |_, _| NotebookList {
        source: preview.clone(),
        state: gpui::ListState::new(count, gpui::ListAlignment::Top, px(200.)),
    });
    let list_state = state.update(cx, |state, _| {
        if !Arc::ptr_eq(&state.source, &preview) {
            state.state.reset(count);
            state.source = preview.clone();
        }
        state.state.clone()
    });
    div()
        .id(id)
        .size_full()
        .font_family(crate::ui::FONT_UI)
        .text_size(crate::ui::TEXT_MD)
        .text_color(pal.foreground)
        .bg(pal.editor_surface)
        .when(count == 0, |view| {
            view.text_color(pal.muted_foreground)
                .child(text::IPYNB_EMPTY.get())
        })
        .when(count != 0, |view| {
            view.child(
                gpui::list(list_state, move |index, window, cx| {
                    notebook_row(&preview, index, window, cx)
                })
                .size_full(),
            )
        })
        .into_any_element()
}

/// 자리표시자·미리보기의 UI 버튼 콜백 타입.
pub type StageCallback = Box<dyn Fn(&mut Window, &mut App) + 'static>;

/// 원격 파일을 이 기기로 내려받는 중일 때 자리표시자·미리보기에 그릴 진행률과 취소 동작.
pub struct PlaceholderStage {
    pub percent: u8,
    pub on_cancel: StageCallback,
}

impl std::fmt::Debug for PlaceholderStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaceholderStage")
            .field("percent", &self.percent)
            .finish_non_exhaustive()
    }
}

/// 호스트 위치(로컬 vs 원격)에 맞는 기본 앱 열기 버튼 라벨.
#[must_use]
pub fn open_action_label(is_remote: bool) -> &'static str {
    if is_remote {
        text::OPEN_ON_THIS_PC
            .mac_or(crate::i18n::catalog::other_os::OPEN_ON_THIS_PC)
            .get()
    } else {
        text::OPEN_DEFAULT.get()
    }
}

/// 호스트 위치(로컬 vs 원격)에 맞는 파일 관리자 표시 버튼·메뉴 라벨.
#[must_use]
pub fn reveal_action_label(is_remote: bool) -> &'static str {
    if is_remote {
        text::DOWNLOAD_AND_REVEAL
            .mac_or(crate::i18n::catalog::other_os::DOWNLOAD_AND_REVEAL)
            .get()
    } else {
        text::REVEAL
            .mac_or(crate::i18n::catalog::other_os::REVEAL)
            .get()
    }
}

/// 원격 파일 스테이징 진행률·취소 컨텐츠. `100%` 폭을 미리 예약해 자릿수가 바뀌어도 취소 버튼이 밀리지 않는다.
fn stage_status_controls(stage: PlaceholderStage, pal: &crate::ui::Palette) -> gpui::Div {
    let on_cancel = stage.on_cancel;
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .grid()
                .font_family(crate::ui::FONT_MONO)
                .text_size(crate::ui::TEXT_XS)
                .text_color(pal.muted_foreground)
                .child(
                    div()
                        .row_start(1)
                        .col_start(1)
                        .invisible()
                        .child(text::DOWNLOADING_PERCENT.format(&[("percent", "100")])),
                )
                .child(
                    div()
                        .debug_selector(|| "file-stage-progress".into())
                        .row_start(1)
                        .col_start(1)
                        .child(
                            text::DOWNLOADING_PERCENT
                                .format(&[("percent", &stage.percent.to_string())]),
                        ),
                ),
        )
        .child(
            Button::new("file-stage-cancel")
                .debug_selector(|| "file-stage-cancel".into())
                .ghost()
                .xsmall()
                .label(text::CANCEL_DOWNLOAD.get())
                .on_click(move |_, window, cx| on_cancel(window, cx)),
        )
}

/// 자리표시자·HTML 미리보기 밖(원본 보기·일반 파일 탭 우클릭 등)에서 스테이징 중일 때 띄우는 상단 진행 줄.
pub fn stage_status_bar(stage: PlaceholderStage, pal: &crate::ui::Palette) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_end()
        .px_2()
        .py_1()
        .border_b_1()
        .border_color(pal.border)
        .bg(pal.editor_surface)
        .child(stage_status_controls(stage, pal))
        .into_any_element()
}

/// 원격 파일 스테이징 진행률·취소 줄. 원격 칸에서는 대기 중에도 같은 높이(`h_6`)를 예약해 레이아웃 흔들림을 막는다.
fn stage_status_row(
    is_remote: bool,
    stage: Option<PlaceholderStage>,
    pal: &crate::ui::Palette,
) -> Option<AnyElement> {
    if !is_remote {
        return None;
    }
    let mut row = div().h_6().flex().items_center().justify_center();
    if let Some(stage) = stage {
        row = row.child(stage_status_controls(stage, pal));
    }
    Some(row.into_any_element())
}

/// 진행률·취소·호출자 콜백을 연결한 HTML 메타데이터 미리보기.
#[expect(
    clippy::too_many_arguments,
    reason = "HTML preview wires path, remote state, metadata, stage progress, and three action callbacks"
)]
pub fn html_preview_interactive(
    path: &Path,
    is_remote: bool,
    title: Option<&str>,
    len: u64,
    stage: Option<PlaceholderStage>,
    on_open_browser: impl Fn(&mut Window, &mut App) + 'static,
    on_source: impl Fn(&mut Window, &mut App) + 'static,
    on_reveal: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let pal = crate::ui::palette(cx);
    let busy = stage.is_some();
    let open_label = crate::i18n::catalog::editor::OPEN_BROWSER.get();
    let source_label = crate::i18n::catalog::editor::SOURCE.get();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let open_browser = crate::ui::primary("file-html-open-browser", open_label)
        .debug_selector(|| "file-html-open-browser".into())
        .disabled(busy)
        .on_click(move |_, window, cx| on_open_browser(window, cx));
    let show_source = crate::ui::secondary("file-html-edit", source_label)
        .debug_selector(|| "file-html-edit".into())
        .disabled(busy)
        .on_click(move |_, window, cx| on_source(window, cx));
    let reveal_action = crate::ui::secondary("file-reveal", reveal_action_label(is_remote))
        .debug_selector(|| "file-reveal".into())
        .disabled(busy)
        .on_click(move |_, window, cx| on_reveal(window, cx));
    div()
        .debug_selector(|| "file-html-preview".into())
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .font_family(crate::ui::FONT_UI)
        .text_size(crate::ui::TEXT_SM)
        .bg(pal.editor_surface)
        .text_color(pal.foreground)
        .when_some(title, |d, t| {
            d.child(
                div()
                    .text_size(crate::ui::TEXT_MD)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(t.to_owned()),
            )
        })
        .child(
            div()
                .font_family(crate::ui::FONT_MONO)
                .text_size(crate::ui::TEXT_SM)
                .child(name),
        )
        .child(
            div()
                .text_size(crate::ui::TEXT_SM)
                .text_color(pal.muted_foreground)
                .child(format!("{} · {}", text::KIND_HTML.get(), size_label(len))),
        )
        .child(
            div()
                .text_size(crate::ui::TEXT_SM)
                .text_color(pal.muted_foreground)
                .child(text::HTML_PREVIEW_DESC.get()),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_center()
                .gap_2()
                .child(open_browser)
                .child(show_source)
                .child(reveal_action),
        )
        .children(stage_status_row(is_remote, stage, pal))
        .into_any_element()
}

/// 미리보기할 수 없는 로컬 파일: 종류·크기와 "기본 앱으로 열기"·"Finder에서 보기".
pub fn placeholder(path: &Path, kind: Kind, len: u64, cx: &App) -> AnyElement {
    let open_path = path.to_path_buf();
    let reveal_path = path.to_path_buf();
    placeholder_interactive(
        path,
        false,
        kind,
        len,
        None,
        move |_, cx| {
            spawn_open_on_reported(
                open_path.clone(),
                None,
                super::open_default_command,
                |_, _| {},
                cx,
            );
        },
        move |_, cx| {
            spawn_open_on_reported(
                reveal_path.clone(),
                None,
                super::reveal_command,
                |_, _| {},
                cx,
            );
        },
        cx,
    )
}

/// 진행률·취소·호출자 콜백을 연결한 미리보기 불가 자리표시자.
#[expect(
    clippy::too_many_arguments,
    reason = "placeholder wires path, remote state, kind, size, stage progress, and two action callbacks"
)]
pub fn placeholder_interactive(
    path: &Path,
    is_remote: bool,
    kind: Kind,
    len: u64,
    stage: Option<PlaceholderStage>,
    on_open: impl Fn(&mut Window, &mut App) + 'static,
    on_reveal: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let pal = crate::ui::palette(cx);
    let busy = stage.is_some();
    let label = match kind {
        Kind::Media => text::KIND_MEDIA.get(),
        Kind::Font => text::KIND_FONT.get(),
        Kind::Pdf => text::KIND_PDF.get(),
        Kind::TooLarge => text::KIND_TOO_LARGE.get(),
        _ => text::KIND_BINARY.get(),
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let open_action = crate::ui::primary("file-open-default", open_action_label(is_remote))
        .debug_selector(|| "file-open-default".into())
        .disabled(busy)
        .on_click(move |_, window, cx| on_open(window, cx));
    let reveal_action = crate::ui::secondary("file-reveal", reveal_action_label(is_remote))
        .debug_selector(|| "file-reveal".into())
        .disabled(busy)
        .on_click(move |_, window, cx| on_reveal(window, cx));
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .font_family(crate::ui::FONT_UI)
        .text_size(crate::ui::TEXT_SM)
        .bg(pal.editor_surface)
        .text_color(pal.foreground)
        .child(
            div()
                .max_w_full()
                .px_4()
                .truncate()
                .font_family(crate::ui::FONT_MONO)
                .text_size(crate::ui::TEXT_SM)
                .child(name),
        )
        .child(
            div()
                .text_size(crate::ui::TEXT_SM)
                .text_color(pal.muted_foreground)
                .child(format!("{label} · {}", size_label(len))),
        )
        .child(
            div()
                .text_size(crate::ui::TEXT_SM)
                .text_color(pal.muted_foreground)
                .child(text::NO_PREVIEW.get()),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_center()
                .gap_2()
                .child(open_action)
                .child(reveal_action),
        )
        .children(stage_status_row(is_remote, stage, pal))
        .into_any_element()
}

/// 자리표시자의 파일 크기. 1MiB 미만을 "0.0 MB"로 보이지 않게 바이트·KB로 쓴다.
pub(crate) fn size_label(len: u64) -> String {
    const KIB: u64 = 1 << 10;
    match len {
        0..KIB => format!("{len} B"),
        KIB..0x10_0000 => format!("{} KB", len.div_ceil(KIB)),
        _ => crate::resources::mb(len),
    }
}

/// 로컬 경로에 대해 외부 앱·파일 관리자 실행 명령을 만들고 실행한다(실행 파일 방지 `stat` 포함).
pub fn run_open_command(target_path: &Path, command: fn(&Path) -> Command) {
    // Building an Open command can stat the target to avoid executing binaries.
    match command(target_path).status() {
        // explorer.exe는 열기에 성공해도 1로 끝난다.
        Ok(status) if status.success() || cfg!(windows) => {}
        Ok(status) => tracing::warn!(?status, "open failed"),
        Err(error) => tracing::warn!(kind = ?error.kind(), "open failed to start"),
    }
}

/// 원격 파일이면 로컬 캐시로 내려받은 뒤 `command`를 실행하고, 스테이징 실패 시 UI 스레드에서 `on_error`를 호출한다.
pub fn spawn_open_on_reported(
    path: PathBuf,
    remote: Option<(krill_hosts::HostId, crate::hosts::HostTarget)>,
    command: fn(&Path) -> Command,
    on_error: impl FnOnce(String, &mut App) + 'static,
    cx: &App,
) {
    let work = cx.background_executor().spawn(async move {
        let target_path = match remote {
            Some((host, target)) => match crate::remote_fs::stage_to_local_cache(
                &target,
                &host,
                &path,
                crate::remote_fs::MAX_STAGE_BYTES,
            ) {
                Ok((local, _)) => local,
                Err(error) => return Err(error),
            },
            None => path,
        };
        run_open_command(&target_path, command);
        Ok(())
    });
    cx.spawn(async move |cx| {
        if let Err(error) = work.await
            && let Some(msg) = crate::remote_fs::stage_error_message(&error)
        {
            cx.update(|cx| on_error(msg, cx));
        }
    })
    .detach();
}

#[cfg(test)]
mod performance_tests {
    use super::*;

    #[test]
    fn notebook_chunks_keep_every_utf8_byte_and_bound_single_long_rows() {
        for text in [
            "한글🙂".repeat(10_000),
            "line\n".repeat(5_000),
            String::new(),
        ] {
            let ranges = preview_chunks(&text);
            let rebuilt: String = ranges
                .iter()
                .map(|range| text.get(range.clone()).unwrap())
                .collect();
            assert_eq!(rebuilt, text);
            for range in ranges {
                assert!(text.get(range).unwrap().chars().count() <= 2_048);
            }
        }
    }

    #[test]
    fn notebook_long_output_and_error_are_split_but_markdown_keeps_one_document() {
        let preview = NotebookPreview::new(Notebook {
            cells: vec![super::super::ipynb::Cell {
                kind: CellKind::Markdown,
                source: "# Heading\n".repeat(1_000),
                execution_count: None,
                outputs: vec![
                    Output::Text("🙂".repeat(10_000)),
                    Output::Error {
                        name: "Error".into(),
                        message: "m".repeat(10_000),
                    },
                ],
            }],
        });
        assert_eq!(
            preview
                .rows
                .iter()
                .filter(|(_, output, _)| output.is_none())
                .count(),
            1
        );
        assert!(
            preview
                .rows
                .iter()
                .filter(|(_, output, _)| *output == Some(0))
                .count()
                > 1
        );
        assert!(
            preview
                .rows
                .iter()
                .filter(|(_, output, _)| *output == Some(1))
                .count()
                > 1
        );
    }
}
