//! 읽기 전용 미리보기 뷰어(gpui). 입력은 경로와 이미 읽은 데이터 — 파일 읽기·탭 연결은 OWE-665가 한다.
//! 화면 구성은 Orca `ImageViewer.tsx`·`EditorMarkdownFileSurface.tsx`·`csv/CsvGrid.tsx`·`IpynbViewer.tsx`를 참고했다
//! (Copyright (c) 2026 Lovecast Inc., MIT License, .ref/orca/LICENSE).

use std::path::{Path, PathBuf};
use std::process::Command;

use gpui::{
    AnyElement, App, Context, ElementId, ObjectFit, SharedString, UniformListScrollHandle, Window,
    div, img, prelude::*, px, uniform_list,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::text::TextView;
use gpui_component::{Disableable as _, Selectable as _, Sizable as _};

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
    path: PathBuf,
    /// SVG처럼 헤더로 크기를 모르면 `None` — 맞춤만 쓴다.
    dims: Option<Dimensions>,
    zoom: Zoom,
}

impl ImageView {
    pub fn new(path: PathBuf, dims: Option<Dimensions>) -> Self {
        Self {
            path,
            dims,
            zoom: Zoom::Fit,
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
                tool("image-zoom-out", "−")
                    .tooltip(text::ZOOM_OUT.get())
                    .disabled(fixed)
                    .on_click(cx.listener(|v, _, _, cx| v.set_zoom(v.zoom.zoom_out(), cx))),
            )
            .child(
                tool("image-zoom-in", "+")
                    .tooltip(text::ZOOM_IN.get())
                    .disabled(fixed)
                    .on_click(cx.listener(|v, _, _, cx| v.set_zoom(v.zoom.zoom_in(), cx))),
            )
            .when_some(scale_label(self.zoom), |d, label| {
                d.child(
                    div()
                        .text_size(crate::ui::TEXT_XS)
                        .text_color(pal.muted_foreground)
                        .child(label),
                )
            });
        let image = img(self.path.clone());
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
pub fn markdown(id: impl Into<ElementId>, source: impl Into<SharedString>) -> AnyElement {
    let id = id.into();
    div()
        .id(id.clone())
        .size_full()
        .overflow_y_scroll()
        .p(crate::ui::SPACE_4)
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

/// ipynb 셀 목록(읽기 전용). 마크다운 셀은 미리보기, 코드·raw 셀은 고정폭, 출력은 텍스트만.
/// ponytail: 셀을 모두 그린다 — 크기는 [`super::MAX_TEXT_BYTES`]로 묶여 있다. 셀 수천 개가 느리면 gpui `list` 가상화로.
pub fn notebook(id: impl Into<ElementId>, nb: &Notebook, cx: &App) -> AnyElement {
    let pal = crate::ui::palette(cx);
    let id = id.into();
    let mono = |s: String| {
        div()
            .font_family(crate::ui::FONT_MONO)
            .text_size(crate::ui::TEXT_SM)
            .whitespace_normal()
            .child(s)
    };
    let cells = nb.cells.iter().enumerate().map(|(i, cell)| {
        let source = match cell.kind {
            CellKind::Markdown => div()
                .child(TextView::markdown(i, cell.source.clone()))
                .into_any_element(),
            CellKind::Code | CellKind::Raw => mono(cell.source.clone())
                .p_2()
                .rounded(crate::ui::RADIUS_SM)
                .bg(pal.muted)
                .into_any_element(),
        };
        let prompt = (cell.kind == CellKind::Code).then(|| {
            div()
                .text_size(crate::ui::TEXT_XS)
                .text_color(pal.muted_foreground)
                .child(match cell.execution_count {
                    Some(n) => format!("[{n}]"),
                    None => "[ ]".to_owned(),
                })
        });
        let outputs = cell.outputs.iter().map(|out| match out {
            Output::Stream { text, .. } | Output::Text(text) => mono(text.clone()),
            Output::Error { name, message } => {
                mono(format!("{name}: {message}")).text_color(pal.destructive)
            }
            Output::Omitted(mime) => div()
                .text_size(crate::ui::TEXT_XS)
                .text_color(pal.muted_foreground)
                .child(text::IPYNB_OUTPUT_OMITTED.format(&[("mime", mime)])),
        });
        div()
            .flex()
            .flex_col()
            .gap_1()
            .pb_3()
            .children(prompt)
            .child(source)
            .children(outputs)
    });
    div()
        .id(id.clone())
        .size_full()
        .overflow_y_scroll()
        .p(crate::ui::SPACE_4)
        .bg(pal.editor_surface)
        .when(nb.cells.is_empty(), |d| {
            d.text_color(pal.muted_foreground)
                .child(text::IPYNB_EMPTY.get())
        })
        .children(cells)
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
    let action = |id: &'static str, label: &'static str, make: fn(&Path) -> Command| {
        let path = path.to_path_buf();
        Button::new(id)
            .small()
            .label(label)
            .on_click(move |_, _, cx| spawn_open(make(&path), cx))
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
        .child(
            div()
                .flex()
                .gap_2()
                .child(action(
                    "file-open-default",
                    text::OPEN_DEFAULT.get(),
                    super::open_default_command,
                ))
                .child(action(
                    "file-reveal",
                    text::REVEAL.get(),
                    super::reveal_command,
                )),
        )
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
fn spawn_open(mut cmd: Command, cx: &App) {
    cx.background_spawn(async move {
        match cmd.status() {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::warn!(?status, "open failed"),
            Err(error) => tracing::warn!(kind = ?error.kind(), "open failed to start"),
        }
    })
    .detach();
}
