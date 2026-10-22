//! 읽기 전용 미리보기 뷰어(gpui). 입력은 경로와 이미 읽은 데이터 — 파일 읽기·탭 연결은 OWE-665가 한다.
//! 화면 구성은 Orca `ImageViewer.tsx`·`EditorMarkdownFileSurface.tsx`·`csv/CsvGrid.tsx`·`IpynbViewer.tsx`를 참고했다
//! (Copyright (c) 2026 Lovecast Inc., MIT License, .ref/orca/LICENSE).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use gpui::{
    AnyElement, App, Context, ElementId, Image, ImageFormat, ObjectFit, SharedString,
    UniformListScrollHandle, Window, div, img, prelude::*, px, uniform_list,
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

/// 이미지 출처: 디스크 경로(래스터·저장된 파일) 또는 미리보기용 메모리 버퍼(SVG 편집 중 내용).
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

    /// SVG 미리보기용: 디스크가 아니라 지금 에디터 버퍼로 그린다(OWE-874 — 미저장 수정 반영).
    pub fn new_svg_bytes(svg: Vec<u8>) -> Self {
        Self {
            source: ImageSource::SvgBytes(Arc::new(Image::from_bytes(ImageFormat::Svg, svg))),
            dims: None,
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
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(pal.border)
            .child(
                tool("image-fit", text::FIT.get())
                    .selected(self.zoom == Zoom::Fit)
                    .on_click(cx.listener(|v, _, _, cx| v.set_zoom(Zoom::Fit, cx))),
            )
            .child(
                tool("image-actual", text::ACTUAL_SIZE.get())
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
            _ => div()
                .id("image-scroll")
                .size_full()
                .p(crate::ui::SPACE_4)
                .child(image.size_full().object_fit(ObjectFit::ScaleDown)),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(pal.editor_surface)
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// 경로 이미지의 디코드 캐시와 GPU 텍스처를 놓는다. gpui는 경로별로 영구 캐시하므로 디스크 내용이 바뀌었거나
/// (재로드·저장) 탭이 닫히면 불러야 다음 그리기에서 새로 읽고, 닫힌 이미지가 메모리에 남지 않는다.
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

/// 마크다운 미리보기(gpui-component `TextView`). 편집↔미리보기 토글은 에디터 쪽(OWE-663/665)에서.
pub fn markdown(id: impl Into<ElementId>, source: impl Into<SharedString>, cx: &App) -> AnyElement {
    let pal = crate::ui::palette(cx);
    let id = id.into();
    div()
        .id(id.clone())
        .size_full()
        .overflow_y_scroll()
        .p(crate::ui::SPACE_4)
        .bg(pal.editor_surface)
        .text_color(pal.foreground)
        .child(TextView::markdown(id, source))
        .into_any_element()
}

/// CSV/TSV 표. 머리 행은 고정, 나머지는 `uniform_list`로 보이는 행만 그린다. 가로는 열 너비 합만큼 스크롤.
#[derive(Debug)]
pub struct CsvView {
    table: Table,
    widths: Vec<f32>,
    scroll: UniformListScrollHandle,
}

impl CsvView {
    pub fn new(table: Table) -> Self {
        let widths = column_widths(&table);
        Self {
            table,
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
                .map(|cell| cell.chars().count())
                .max()
                .unwrap_or(0)
                .clamp(CSV_MIN_CHARS, CSV_MAX_CHARS);
            #[expect(clippy::cast_precision_loss, reason = "CSV_MAX_CHARS 이하라 정확하다")]
            let chars = chars as f32;
            chars * CSV_CHAR_WIDTH + 16.
        })
        .collect()
}

fn csv_row(cells: &[String], widths: &[f32], border: gpui::Hsla) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .h(px(CSV_ROW_HEIGHT))
        .border_b_1()
        .border_color(border)
        .children(widths.iter().enumerate().map(|(i, &w)| {
            div()
                .flex_none()
                .w(px(w))
                .px_2()
                .border_r_1()
                .border_color(border)
                .truncate()
                .child(cells.get(i).cloned().unwrap_or_default())
        }))
}

impl Render for CsvView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = crate::ui::palette(cx);
        let border = pal.border;
        let total: f32 = self.widths.iter().sum();
        let body_rows = self.table.rows.len().saturating_sub(1);
        let header = self.table.rows.first().map_or_else(div, |cells| {
            csv_row(cells, &self.widths, border)
                .bg(pal.muted)
                .font_weight(gpui::FontWeight::SEMIBOLD)
        });
        let list = uniform_list(
            "csv-rows",
            body_rows,
            cx.processor(move |v, range: std::ops::Range<usize>, _, _| {
                range
                    .filter_map(|i| {
                        let cells = v.table.rows.get(i + 1)?;
                        Some(csv_row(cells, &v.widths, border))
                    })
                    .collect()
            }),
        )
        .track_scroll(&self.scroll)
        .flex_1();
        let notice = self.table.truncated.then(|| {
            div()
                .flex_none()
                .px_2()
                .py_1()
                .text_size(crate::ui::TEXT_XS)
                .text_color(pal.muted_foreground)
                .child(text::CSV_TRUNCATED.format(&[("rows", &self.table.rows.len().to_string())]))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(pal.editor_surface)
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
    rows: Vec<(usize, Option<usize>)>,
}
impl NotebookPreview {
    pub fn new(notebook: Notebook) -> Self {
        let rows = notebook
            .cells
            .iter()
            .enumerate()
            .flat_map(|(cell, value)| {
                std::iter::once((cell, None))
                    .chain((0..value.outputs.len()).map(move |output| (cell, Some(output))))
            })
            .collect();
        Self { notebook, rows }
    }
}
struct NotebookList {
    source: Arc<NotebookPreview>,
    state: gpui::ListState,
}

fn notebook_row(preview: &NotebookPreview, index: usize, cx: &App) -> AnyElement {
    let pal = crate::ui::palette(cx);
    let Some(&(cell_index, output_index)) = preview.rows.get(index) else {
        return div().into_any_element();
    };
    let Some(cell) = preview.notebook.cells.get(cell_index) else {
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
        match cell.outputs.get(index) {
            Some(Output::Stream { text, .. } | Output::Text(text)) => {
                mono(text.clone()).into_any_element()
            }
            Some(Output::Error { name, message }) => mono(format!("{name}: {message}"))
                .text_color(pal.destructive)
                .into_any_element(),
            Some(Output::Omitted(mime)) => div()
                .text_size(crate::ui::TEXT_XS)
                .text_color(pal.muted_foreground)
                .child(text::IPYNB_OUTPUT_OMITTED.format(&[("mime", mime)]))
                .into_any_element(),
            None => div().into_any_element(),
        }
    } else {
        let source = match cell.kind {
            CellKind::Markdown => div()
                .child(TextView::markdown(cell_index, cell.source.clone()))
                .into_any_element(),
            CellKind::Code | CellKind::Raw => mono(cell.source.clone())
                .p_2()
                .rounded(crate::ui::RADIUS_SM)
                .bg(pal.muted)
                .into_any_element(),
        };
        let prompt = (cell.kind == CellKind::Code).then(|| {
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
        .bg(pal.editor_surface)
        .when(count == 0, |view| {
            view.text_color(pal.muted_foreground)
                .child(text::IPYNB_EMPTY.get())
        })
        .when(count != 0, |view| {
            view.child(
                gpui::list(list_state, move |index, _, cx| {
                    notebook_row(&preview, index, cx)
                })
                .size_full(),
            )
        })
        .into_any_element()
}

/// 미리보기할 수 없는 파일: 종류·크기와 "기본 앱으로 열기"·"Finder에서 보기".
pub fn placeholder(path: &Path, kind: Kind, len: u64, cx: &App) -> AnyElement {
    let pal = crate::ui::palette(cx);
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
    let open_action = {
        let path = path.to_path_buf();
        crate::ui::primary("file-open-default", text::OPEN_DEFAULT.get())
            .debug_selector(|| "file-open-default".into())
            .on_click(move |_, _, cx| spawn_open(path.clone(), super::open_default_command, cx))
    };
    let reveal_action = {
        let path = path.to_path_buf();
        crate::ui::secondary(
            "file-reveal",
            text::REVEAL
                .mac_or(crate::i18n::catalog::other_os::REVEAL)
                .get(),
        )
        .debug_selector(|| "file-reveal".into())
        .on_click(move |_, _, cx| spawn_open(path.clone(), super::reveal_command, cx))
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .bg(pal.editor_surface)
        .text_color(pal.foreground)
        .child(div().text_size(crate::ui::TEXT_LG).child(name))
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
        .child(div().flex().gap_2().child(open_action).child(reveal_action))
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

/// `open`은 Launch Services 응답을 기다리므로 UI 스레드 밖에서 실행한다. 로그에 경로는 남기지 않는다(A8).
pub fn spawn_open(path: PathBuf, command: fn(&Path) -> Command, cx: &App) {
    cx.background_spawn(async move {
        // Building an Open command can stat the target to avoid executing binaries.
        match command(&path).status() {
            // explorer.exe는 열기에 성공해도 1로 끝난다.
            Ok(status) if status.success() || cfg!(windows) => {}
            Ok(status) => tracing::warn!(?status, "open failed"),
            Err(error) => tracing::warn!(kind = ?error.kind(), "open failed to start"),
        }
    })
    .detach();
}
