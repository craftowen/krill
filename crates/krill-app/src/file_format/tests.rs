use std::path::Path;

use super::csv::{self, Delimiter, Table};
use super::ipynb::{self, CellKind, Output};
use super::raster::{self, Dimensions, RasterError};
use super::*;

fn kind(name: &str) -> Kind {
    classify(Path::new(name), b"plain text\n", 11)
}

/// Orca 확장자 표(.ref/orca/src/shared/*-file-extensions.ts) 전체가 같은 종류로 분류된다.
#[test]
fn orca_extension_table_is_classified() {
    for ext in ["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"] {
        assert_eq!(
            kind(&format!("a.{ext}")),
            Kind::Image(ImageFormat::Raster),
            "{ext}"
        );
    }
    assert_eq!(kind("a.svg"), Kind::Image(ImageFormat::Svg));
    assert!(kind("a.svg").is_text_editable());
    assert!(!kind("a.png").is_text_editable());
    for ext in [
        "mp3", "wav", "m4a", "aac", "ogg", "oga", "flac", "opus", "mp4", "m4v", "mov", "webm",
    ] {
        assert_eq!(kind(&format!("a.{ext}")), Kind::Media, "{ext}");
    }
    for ext in ["woff", "woff2", "ttf", "otf"] {
        assert_eq!(kind(&format!("a.{ext}")), Kind::Font, "{ext}");
    }
    assert_eq!(kind("a.pdf"), Kind::Pdf);
    for ext in [
        "7z", "bz2", "gz", "jar", "rar", "tar", "tgz", "war", "xz", "zip", "zst", "avi", "mkv",
        "doc", "docx", "ppt", "pptx", "xls", "xlsx", "eot", "ttc", "a", "bin", "class", "dll",
        "dylib", "exe", "idx", "lockb", "node", "o", "pack", "pyc", "pyd", "so", "sqlite",
        "sqlite3", "wasm",
    ] {
        assert_eq!(kind(&format!("a.{ext}")), Kind::Binary, "{ext}");
    }
    // 대소문자 무시.
    assert_eq!(kind("A.PNG"), Kind::Image(ImageFormat::Raster));
    assert_eq!(kind("notes.md"), Kind::Markdown);
    assert_eq!(kind("x.MDX"), Kind::Markdown);
    assert_eq!(kind("n.ipynb"), Kind::Ipynb);
    assert_eq!(
        kind("t.tsv"),
        Kind::Csv {
            delimiter: Delimiter::Tab
        }
    );
    assert_eq!(kind("main.rs"), Kind::Text { language: "rust" });
    assert_eq!(kind(".gitignore"), Kind::Text { language: "text" });
    assert_eq!(kind("Makefile"), Kind::Text { language: "make" });
}

#[test]
fn nul_in_first_8kib_means_binary() {
    let mut head = vec![b'a'; BINARY_PROBE_BYTES];
    assert_eq!(
        classify(Path::new("README"), &head, 8192),
        Kind::Text { language: "text" }
    );
    head[BINARY_PROBE_BYTES - 1] = 0;
    assert_eq!(classify(Path::new("README"), &head, 8192), Kind::Binary);
    // 검사 범위 밖의 NUL은 보지 않는다.
    let mut long = vec![b'a'; BINARY_PROBE_BYTES + 1];
    long[BINARY_PROBE_BYTES] = 0;
    assert!(!looks_binary(&long));
    // 텍스트 확장자라도 NUL이 있으면 바이너리.
    assert_eq!(classify(Path::new("x.rs"), b"fn\0", 3), Kind::Binary);
}

#[test]
fn size_limits_apply_per_kind() {
    let text = |name: &str, len| classify(Path::new(name), b"x", len);
    assert_eq!(
        text("a.rs", MAX_TEXT_BYTES),
        Kind::Text { language: "rust" }
    );
    assert_eq!(text("a.rs", MAX_TEXT_BYTES + 1), Kind::TooLarge);
    assert_eq!(text("a.md", MAX_TEXT_BYTES + 1), Kind::TooLarge);
    assert_eq!(text("a.ipynb", MAX_TEXT_BYTES + 1), Kind::TooLarge);
    assert_eq!(
        text("a.png", MAX_IMAGE_BYTES),
        Kind::Image(ImageFormat::Raster)
    );
    assert_eq!(text("a.png", MAX_IMAGE_BYTES + 1), Kind::TooLarge);
    // CSV는 앞부분만 미리보기하므로 크기와 무관.
    assert_eq!(
        text("a.csv", u64::MAX),
        Kind::Csv {
            delimiter: Delimiter::Comma
        }
    );
    // 자리표시자 종류는 크기와 무관.
    assert_eq!(text("a.mp4", u64::MAX), Kind::Media);
}

/// highlighter가 아는 이름이고, 켜진 문법(rust·json)은 실제로 하이라이트된다.
#[test]
fn languages_map_to_highlighter_names() {
    use gpui_component::highlighter::Language;
    assert_eq!(
        Language::from_str(language_for(Path::new("a.rs"))),
        Language::Rust
    );
    assert_eq!(
        Language::from_str(language_for(Path::new("a.json"))),
        Language::Json
    );
    assert_eq!(
        Language::from_str(language_for(Path::new("a.unknown"))),
        Language::Plain
    );
    for (file, name) in [
        ("a.toml", "toml"),
        ("a.ts", "typescript"),
        ("a.tsx", "tsx"),
        ("a.jsx", "javascript"),
        ("a.py", "python"),
        ("a.yml", "yaml"),
        ("a.sh", "bash"),
    ] {
        assert_eq!(language_for(Path::new(file)), name, "{file}");
    }
}

#[test]
fn csv_quotes_escapes_and_line_endings() {
    let src = "name,note\r\n\"Kim, J\",\"say \"\"hi\"\"\"\r\n\"multi\nline\",x\rlast,\"\"";
    let t = csv::preview(src.as_bytes(), src.len() as u64, Delimiter::Comma);
    assert_eq!(
        t,
        Table {
            rows: vec![
                vec!["name".into(), "note".into()],
                vec!["Kim, J".into(), "say \"hi\"".into()],
                vec!["multi\nline".into(), "x".into()],
                vec!["last".into(), String::new()],
            ],
            columns: 2,
            truncated: false,
        }
    );
    // 필드 중간의 따옴표는 글자, BOM은 버린다, 빈 끝 필드도 센다.
    let t = csv::preview("\u{feff}a\"b,c,\n".as_bytes(), 9, Delimiter::Comma);
    assert_eq!(
        t.rows,
        vec![vec!["a\"b".to_owned(), "c".into(), String::new()]]
    );
}

#[test]
fn tsv_and_delimiter_sniffing() {
    assert_eq!(csv::sniff("\n\na\tb\tc,d\n"), Delimiter::Tab);
    assert_eq!(csv::sniff("\"x\t\ty\",z\n"), Delimiter::Comma);
    assert_eq!(csv::sniff("a\tb\n"), Delimiter::Tab);
    assert_eq!(
        classify(Path::new("a.csv"), b"a\tb\tc\n1\t2\t3\n", 14),
        Kind::Csv {
            delimiter: Delimiter::Tab
        }
    );
    let t = csv::preview(b"a\tb\n1\t\"2\t3\"\n", 13, Delimiter::Tab);
    assert_eq!(t.rows[1], vec!["1".to_owned(), "2\t3".into()]);
}

#[test]
fn csv_head_preview_drops_partial_record_and_caps_rows() {
    // 파일이 더 길면 마지막(잘렸을 수 있는) 레코드를 버린다.
    let t = csv::preview(b"a,b\n1,2\n3,", 1_000, Delimiter::Comma);
    assert_eq!(t.rows.len(), 2);
    assert!(t.truncated);
    let many = "x\n".repeat(csv::MAX_ROWS + 5);
    let t = csv::preview(many.as_bytes(), many.len() as u64, Delimiter::Comma);
    assert_eq!(t.rows.len(), csv::MAX_ROWS);
    assert!(t.truncated);
    let wide = ",".repeat(csv::MAX_COLUMNS + 10);
    let t = csv::preview(wide.as_bytes(), wide.len() as u64, Delimiter::Comma);
    assert_eq!(t.columns, csv::MAX_COLUMNS);
    assert!(t.truncated);
}

#[test]
fn ipynb_cells_and_text_outputs() {
    let nb = r##"{
      "nbformat": 4, "nbformat_minor": 5, "metadata": {},
      "cells": [
        {"cell_type": "markdown", "metadata": {}, "source": ["# Title\n", "body"]},
        {"cell_type": "code", "execution_count": 3, "metadata": {}, "source": "print(1)",
         "outputs": [
           {"output_type": "stream", "name": "stdout", "text": ["1\r\n"]},
           {"output_type": "execute_result", "execution_count": 3,
            "data": {"text/plain": ["2"], "text/html": "<b>2</b>"}, "metadata": {}},
           {"output_type": "display_data", "data": {"image/png": "iVBOR"}, "metadata": {}},
           {"output_type": "error", "ename": "ValueError", "evalue": "bad", "traceback": ["\u001b[0m"]}
         ]},
        {"cell_type": "raw", "metadata": {}, "source": []},
        {"no_type": true}
      ]
    }"##;
    let parsed = ipynb::parse(nb).unwrap();
    assert_eq!(parsed.cells.len(), 3);
    let [md, code, raw] = &parsed.cells[..] else {
        panic!("three cells");
    };
    assert_eq!(
        (md.kind, md.source.as_str()),
        (CellKind::Markdown, "# Title\nbody")
    );
    assert_eq!(code.kind, CellKind::Code);
    assert_eq!(code.execution_count, Some(3));
    assert_eq!(
        code.outputs,
        vec![
            Output::Stream {
                name: "stdout".into(),
                text: "1\n".into()
            },
            Output::Text("2".into()),
            Output::Omitted("image/png".into()),
            Output::Error {
                name: "ValueError".into(),
                message: "bad".into()
            },
        ]
    );
    assert_eq!((raw.kind, raw.source.as_str()), (CellKind::Raw, ""));
}

#[test]
fn ipynb_rejects_other_formats() {
    assert!(matches!(ipynb::parse("{"), Err(ipynb::ParseError::Json(_))));
    assert!(matches!(
        ipynb::parse(r#"{"nbformat": 3, "worksheets": []}"#),
        Err(ipynb::ParseError::Unsupported)
    ));
    assert!(matches!(
        ipynb::parse(r#"{"nbformat": 4}"#),
        Err(ipynb::ParseError::Unsupported)
    ));
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "dimensions()와 비교하기 위한 헬퍼"
)]
fn dims(width: u32, height: u32) -> Option<Dimensions> {
    Some(Dimensions { width, height })
}

#[test]
fn raster_headers_report_dimensions() {
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend_from_slice(&640u32.to_be_bytes());
    png.extend_from_slice(&480u32.to_be_bytes());
    assert_eq!(raster::dimensions(&png), dims(640, 480));

    assert_eq!(
        raster::dimensions(b"GIF89a\x20\x03\x58\x02"),
        dims(800, 600)
    );

    let mut bmp = vec![0u8; 26];
    bmp[..2].copy_from_slice(b"BM");
    bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
    bmp[18..22].copy_from_slice(&100i32.to_le_bytes());
    bmp[22..26].copy_from_slice(&(-50i32).to_le_bytes());
    assert_eq!(raster::dimensions(&bmp), dims(100, 50));

    assert_eq!(raster::dimensions(b"\0\0\x01\0\x01\0\0\x20"), dims(256, 32));

    let webp = |chunk: &[u8], body: &[u8]| {
        let mut v = b"RIFF\0\0\0\0WEBP".to_vec();
        v.extend_from_slice(chunk);
        v.extend_from_slice(body);
        v
    };
    // VP8: 크기 4 + 프레임 태그 3 + 시작 코드 3, 그다음 가로·세로.
    let vp8 = webp(b"VP8 ", b"\0\0\0\0\0\0\0\x9d\x01\x2a\x40\x01\xf0\x00");
    assert_eq!(raster::dimensions(&vp8), dims(320, 240));
    // VP8L: (w-1)=99, (h-1)=49 → 14비트씩.
    let bits: u32 = 0x63 | (0x31 << 14);
    let mut body = b"\0\0\0\0\x2f".to_vec();
    body.extend_from_slice(&bits.to_le_bytes());
    assert_eq!(raster::dimensions(&webp(b"VP8L", &body)), dims(100, 50));
    let vp8x = webp(b"VP8X", b"\0\0\0\0\0\0\0\0\x1f\x00\x00\x0f\x00\x00");
    assert_eq!(raster::dimensions(&vp8x), dims(32, 16));

    // JPEG: APP0 세그먼트를 건너뛰고 SOF0에서 읽는다.
    let jpeg = b"\xff\xd8\xff\xe0\x00\x04ab\xff\xff\xc0\x00\x11\x08\x01\xe0\x02\x80";
    assert_eq!(raster::dimensions(jpeg), dims(640, 480));
    // DHT(C4)는 SOF가 아니다.
    assert_eq!(
        raster::dimensions(b"\xff\xd8\xff\xc4\x00\x02\xff\xc0\x00\x11\x08\x00\x10\x00\x20"),
        dims(32, 16)
    );

    assert_eq!(raster::dimensions(b"not an image"), None);
    assert_eq!(raster::dimensions(b"\x89PNG\r\n\x1a\n\0\0"), None);
}

#[test]
fn raster_limits() {
    let gif = |w: u16, h: u16| {
        let mut v = b"GIF89a".to_vec();
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v
    };
    assert_eq!(
        raster::check(&gif(3840, 2160)),
        Ok(Dimensions {
            width: 3840,
            height: 2160
        })
    );
    let big = Dimensions {
        width: 4096,
        height: 4096,
    };
    assert_eq!(
        raster::check(&gif(4096, 4096)),
        Err(RasterError::TooLarge(big))
    );
    assert_eq!(raster::check(&gif(0, 10)), Err(RasterError::Invalid));
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend_from_slice(&(raster::MAX_DIMENSION + 1).to_be_bytes());
    png.extend_from_slice(&1u32.to_be_bytes());
    assert!(matches!(raster::check(&png), Err(RasterError::TooLarge(_))));
}

#[test]
fn zoom_steps_clamp_like_orca() {
    assert_eq!(Zoom::Fit.zoom_in(), Zoom::Scale(1.25));
    assert_eq!(Zoom::Fit.zoom_out(), Zoom::Scale(0.8));
    let mut z = Zoom::Fit;
    for _ in 0..30 {
        z = z.zoom_in();
    }
    assert_eq!(z, Zoom::Scale(MAX_ZOOM));
    for _ in 0..60 {
        z = z.zoom_out();
    }
    assert_eq!(z, Zoom::Scale(MIN_ZOOM));
}

#[test]
fn open_commands_pass_path_as_one_literal_argument() {
    let path = Path::new("/tmp/한글 'x'; $(touch nope) -n.pdf");
    let args = |cmd: std::process::Command| {
        assert_eq!(cmd.get_program(), "/usr/bin/open");
        cmd.get_args().map(ToOwned::to_owned).collect::<Vec<_>>()
    };
    assert_eq!(
        args(open_default_command(path)),
        ["--".as_ref(), path.as_os_str()]
    );
    assert_eq!(
        args(reveal_command(path)),
        ["-R".as_ref(), "--".as_ref(), path.as_os_str()]
    );
}

/// 감사 2026-10-17: 실행 파일에 "기본 앱으로 열기"를 누르면 `open`이 터미널에서 실행했다. Finder에서 보기로 바꾼다.
#[test]
fn open_default_does_not_execute_executables() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("mk-open-exec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("tool");
    std::fs::write(&exe, b"\xcf\xfa\xed\xfe\0").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let doc = dir.join("a.pdf");
    std::fs::write(&doc, b"%PDF").unwrap();
    let flags = |cmd: std::process::Command| cmd.get_args().any(|a| a == "-R");
    assert!(
        flags(open_default_command(&exe)),
        "executable is revealed, not run"
    );
    assert!(!flags(open_default_command(&doc)));
    std::fs::remove_dir_all(&dir).ok();
}

proptest::proptest! {
    #[test]
    fn parsers_never_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..2048), cut in 0u64..4096) {
        drop(csv::preview(&bytes, cut, Delimiter::Comma));
        drop(csv::preview(&bytes, cut, Delimiter::Tab));
        drop(ipynb::parse(&String::from_utf8_lossy(&bytes)));
        raster::check(&bytes).ok();
        let _ = classify(Path::new("x"), &bytes, cut);
    }

    #[test]
    fn jpeg_segment_walk_never_panics(tail in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..512)) {
        let mut bytes = vec![0xff, 0xd8];
        bytes.extend(tail);
        let _ = raster::dimensions(&bytes);
    }
}

/// 모든 뷰어가 그려진다(OWE-665가 탭에 연결하기 전 스모크 검사).
#[gpui::test]
fn viewers_render(cx: &mut gpui::TestAppContext) {
    use gpui::{
        AppContext as _, Entity, IntoElement, ParentElement as _, Render, Styled as _, div,
    };

    struct Host {
        image: Entity<view::ImageView>,
        csv: Entity<view::CsvView>,
        notebook: ipynb::Notebook,
    }
    impl Render for Host {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(self.image.clone())
                .child(self.csv.clone())
                .child(view::markdown("md", "# 제목\n\n- 항목", cx))
                .child(view::notebook("nb", &self.notebook, cx))
                .child(view::placeholder(
                    Path::new("/tmp/a.pdf"),
                    Kind::Pdf,
                    2048,
                    cx,
                ))
        }
    }

    let rows = "a,b\n".repeat(5_000);
    let table = csv::preview(rows.as_bytes(), rows.len() as u64, Delimiter::Comma);
    let notebook = ipynb::parse(
        r#"{"nbformat":4,"cells":[{"cell_type":"markdown","source":"*x*"},{"cell_type":"code","source":"1","outputs":[{"output_type":"display_data","data":{"image/png":""}}]}]}"#,
    )
    .unwrap();
    cx.update(gpui_component::init);
    let (host, cx) = cx.add_window_view(|_, cx| Host {
        image: cx.new(|_| {
            view::ImageView::new(
                "/tmp/missing.png".into(),
                Some(Dimensions {
                    width: 10,
                    height: 10,
                }),
            )
        }),
        csv: cx.new(|_| view::CsvView::new(table)),
        notebook,
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| host.read(cx).image.read(cx).zoom()),
        Zoom::Fit
    );
}

/// MK-121 QA: 작은 파일이 자리표시자에 "0.0 MB"로 보이던 결함.
#[test]
fn placeholder_size_label_uses_bytes_and_kb_below_one_mib() {
    use super::view::size_label;
    assert_eq!(size_label(7), "7 B");
    assert_eq!(size_label(4096), "4 KB");
    assert_eq!(size_label(1025), "2 KB");
    assert_eq!(size_label(5 * 1024 * 1024), "5.0 MB");
}
