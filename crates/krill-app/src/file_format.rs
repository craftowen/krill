//! OWE-664(MK-120): 에디터가 여는 파일의 종류 분류와 읽기 전용 미리보기.
//!
//! 분류(`classify`)·CSV·ipynb·래스터 헤더 파서는 gpui 없이 테스트한다. gpui 뷰어는 [`view`]에만 있다.
//! 확장자 표·한도의 근거는 Orca(Copyright (c) 2026 Lovecast Inc., MIT License, .ref/orca/LICENSE)의
//! `src/shared/{image,media,font,binary}-file-extensions.ts`, `binary-buffer.ts`, `raster-image-preview-limits.ts`,
//! `components/editor/image-viewer-zoom.ts`, `csv/csv-file-limits.ts`다. 코드는 옮기지 않고 표와 규칙만 따랐다.

use std::path::Path;
use std::process::{Command, Stdio};

pub mod csv;
pub mod ipynb;
pub mod raster;
pub mod view;

#[cfg(test)]
#[cfg(unix)]
mod tests;

/// 앞부분에서 NUL을 찾는 바이트 수. git과 Orca(`BINARY_PROBE_BYTES`)가 쓰는 휴리스틱과 같다.
pub const BINARY_PROBE_BYTES: usize = 8 * 1024;

/// 텍스트·마크다운·ipynb 한도. Orca는 Monaco 기준 50MB(`MAX_TEXT_FILE_SIZE`)지만 앱 메모리 예산(≤ 120MB,
/// AGENTS §2)보다 크다.
/// ponytail: 8MiB는 추정 — 편집기 rope·하이라이트 사본까지 예산 안에 들도록 잡은 보수값. MK-066 메모리 게이트로 측정한 뒤 조정.
pub const MAX_TEXT_BYTES: u64 = 8 * 1024 * 1024;

/// 이미지 파일 한도. Orca는 50MB(`MAX_PREVIEWABLE_BINARY_SIZE`)지만 위와 같은 예산 이유로 낮춘다.
/// ponytail: 16MiB는 추정 — 픽셀 한도([`raster::MAX_PIXELS`])가 실제 디코드 메모리를 막고, 이 값은 파일 읽기만 막는다.
pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// 파일 종류. 뷰어 선택과 "텍스트로 편집 가능한가"를 정한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `language`는 gpui-component highlighter 이름. 켜지지 않은 문법은 highlighter가 plain으로 그린다.
    Text {
        language: &'static str,
    },
    Markdown,
    Csv {
        delimiter: csv::Delimiter,
    },
    Ipynb,
    Image(ImageFormat),
    Media,
    Font,
    Pdf,
    Binary,
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Raster,
    /// SVG는 이미지로 보이지만 소스 텍스트로도 편집한다(Orca `TEXT_IMAGE_EXTENSIONS`).
    Svg,
}

impl Kind {
    /// 텍스트 편집기로 열 수 있는 종류.
    pub fn is_text_editable(self) -> bool {
        matches!(
            self,
            Self::Text { .. }
                | Self::Markdown
                | Self::Csv { .. }
                | Self::Ipynb
                | Self::Image(ImageFormat::Svg)
        )
    }
}

const RASTER_IMAGE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];
const MEDIA: &[&str] = &[
    "mp3", "wav", "m4a", "aac", "ogg", "oga", "flac", "opus", "mp4", "m4v", "mov", "webm",
];
const FONT: &[&str] = &["woff", "woff2", "ttf", "otf"];
/// Orca `NON_IMAGE_BINARY_EXTENSIONS`에서 미디어·폰트·pdf(위에서 먼저 잡힘)를 뺀 나머지 + 미리보기 없는 오디오·비디오·폰트.
const BINARY: &[&str] = &[
    "7z", "bz2", "gz", "jar", "rar", "tar", "tgz", "war", "xz", "zip", "zst", "avi", "mkv", "doc",
    "docx", "ppt", "pptx", "xls", "xlsx", "eot", "ttc", "a", "bin", "class", "dll", "dylib", "exe",
    "idx", "lockb", "node", "o", "pack", "pyc", "pyd", "so", "sqlite", "sqlite3", "wasm",
];
const MARKDOWN: &[&str] = &["md", "mdx", "markdown"];

/// 확장자(소문자, 점 없음). 점으로 시작하는 파일 이름(`.gitignore`)은 확장자가 없다.
fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// 브라우저로 바로 열 수 있는 HTML 웹 문서(`.html`·`.htm`)인가.
#[must_use]
pub fn is_html_path(path: &Path) -> bool {
    matches!(extension(path).as_deref(), Some("html" | "htm"))
}

/// 앞 [`BINARY_PROBE_BYTES`] 안에 NUL이 있으면 바이너리.
pub fn looks_binary(head: &[u8]) -> bool {
    head.iter().take(BINARY_PROBE_BYTES).any(|&b| b == 0)
}

/// 확장자 + 앞부분 바이트(`head`, [`BINARY_PROBE_BYTES`] 이상이면 충분) + 파일 길이 → 종류.
///
/// 확장자가 정하는 종류(이미지·미디어·폰트·pdf·바이너리)는 내용을 보지 않는다. 나머지는 NUL 검사로 텍스트/바이너리를
/// 가르고 크기 한도를 적용한다. CSV는 앞부분만 미리보기하므로([`csv::PREVIEW_BYTES`]) 크기 한도가 없다.
pub fn classify(path: &Path, head: &[u8], len: u64) -> Kind {
    let ext = extension(path);
    let ext = ext.as_deref().unwrap_or("");
    if ext == "svg" || RASTER_IMAGE.contains(&ext) {
        if len > MAX_IMAGE_BYTES {
            return Kind::TooLarge;
        }
        return Kind::Image(if ext == "svg" {
            ImageFormat::Svg
        } else {
            ImageFormat::Raster
        });
    }
    if MEDIA.contains(&ext) {
        return Kind::Media;
    }
    if FONT.contains(&ext) {
        return Kind::Font;
    }
    if ext == "pdf" {
        return Kind::Pdf;
    }
    if BINARY.contains(&ext) || looks_binary(head) {
        return Kind::Binary;
    }
    if ext == "csv" || ext == "tsv" {
        let delimiter = if ext == "tsv" {
            csv::Delimiter::Tab
        } else {
            csv::sniff(&String::from_utf8_lossy(head))
        };
        return Kind::Csv { delimiter };
    }
    if len > MAX_TEXT_BYTES {
        return Kind::TooLarge;
    }
    if ext == "ipynb" {
        return Kind::Ipynb;
    }
    if MARKDOWN.contains(&ext) {
        return Kind::Markdown;
    }
    Kind::Text {
        language: language_for(path),
    }
}

/// 경로 → gpui-component highlighter 언어 이름(`highlighter::Language::name`). 모르면 `"text"`.
pub fn language_for(path: &Path) -> &'static str {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name {
        "Makefile" | "makefile" | "GNUmakefile" => return "make",
        "CMakeLists.txt" => return "cmake",
        _ => {}
    }
    match extension(path).as_deref().unwrap_or("") {
        "rs" => "rust",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "py" | "pyi" => "python",
        "sh" | "bash" | "zsh" => "bash",
        "yaml" | "yml" => "yaml",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        "css" | "scss" => "css",
        "html" | "htm" => "html",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "lua" => "lua",
        "php" => "php",
        "proto" => "proto",
        "rb" => "ruby",
        "scala" => "scala",
        "sql" => "sql",
        "swift" => "swift",
        "zig" => "zig",
        "ex" | "exs" => "elixir",
        "graphql" | "gql" => "graphql",
        "svelte" => "svelte",
        "astro" => "astro",
        "diff" | "patch" => "diff",
        "cmake" => "cmake",
        "md" | "mdx" | "markdown" => "markdown",
        _ => "text",
    }
}

/// 이미지 뷰 배율. 상수는 Orca `image-viewer-zoom.ts`와 같다.
pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 8.;
pub const ZOOM_STEP: f32 = 1.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zoom {
    /// 창에 맞춤(작은 이미지는 키우지 않는다).
    Fit,
    /// 이미지 1px = 1pt 기준 배율.
    Scale(f32),
}

impl Zoom {
    /// 맞춤 상태에서 +/-는 100%에서 시작한다.
    /// ponytail: 맞춤 배율(창 크기 의존)을 모른 채 1.0 기준 — 뷰포트 크기를 뷰가 알게 되면 그 배율에서 시작.
    fn scale(self) -> f32 {
        match self {
            Self::Fit => 1.,
            Self::Scale(s) => s,
        }
    }

    #[must_use]
    pub fn zoom_in(self) -> Self {
        Self::Scale((self.scale() * ZOOM_STEP).clamp(MIN_ZOOM, MAX_ZOOM))
    }

    #[must_use]
    pub fn zoom_out(self) -> Self {
        Self::Scale((self.scale() / ZOOM_STEP).clamp(MIN_ZOOM, MAX_ZOOM))
    }
}

/// "기본 앱으로 열기" — Launch Services(`open`)·Windows 탐색기가 확장자에 맞는 앱을 고른다.
/// 실행 파일은 그 앱이 **실행**하므로 Finder에서 보기로 바꾼다(저장소 속 바이너리 한 번 클릭 실행 방지).
pub fn open_default_command(path: &Path) -> Command {
    // 링크는 이름이 아니라 `open`이 따라가 실행할 대상으로 판정한다.
    let target = std::fs::canonicalize(path).ok();
    let target = target.as_deref().unwrap_or(path);
    let executable = runs_when_opened(path)
        || runs_when_opened(target)
        || std::fs::metadata(target).is_ok_and(|m| m.is_file() && is_executable(target, &m));
    if executable {
        return reveal_command(path);
    }
    #[cfg(windows)]
    {
        let mut cmd = explorer();
        cmd.arg(path);
        cmd
    }
    #[cfg(not(windows))]
    open_command(&[], path)
}

/// macOS는 실행 비트와 무관하게 `open`이 실행·설치하는 확장자(앱 번들·`.command`·설치 패키지 등).
fn runs_when_opened(path: &Path) -> bool {
    const MACOS_RUNNABLE: &[&str] = &[
        "app", "command", "tool", "terminal", "pkg", "mpkg", "workflow", "scpt", "scptd", "action",
    ];
    cfg!(target_os = "macos")
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| MACOS_RUNNABLE.iter().any(|r| e.eq_ignore_ascii_case(r)))
}

/// 기본 앱으로 열면 실행되는 파일인가. Windows는 실행 비트가 없어 셸이 실행하는 확장자로 본다.
fn is_executable(path: &Path, meta: &std::fs::Metadata) -> bool {
    const WINDOWS_RUNNABLE: &[&str] = &[
        "exe",
        "com",
        "bat",
        "cmd",
        "ps1",
        "vbs",
        "vbe",
        "js",
        "jse",
        "wsf",
        "wsh",
        "msi",
        "msc",
        "scr",
        "hta",
        "cpl",
        "lnk",
        "url",
        "reg",
        "jar",
        "appref-ms",
    ];
    if cfg!(windows) {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| WINDOWS_RUNNABLE.iter().any(|r| e.eq_ignore_ascii_case(r)))
    } else {
        crate::platform::has_exec_bits(meta)
    }
}

/// "Finder에서 보기" — `open -R`은 파일을 선택한 채 Finder 창을 연다. Windows는 `explorer.exe /select,"<경로>"`.
pub fn reveal_command(path: &Path) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // explorer는 `/select,`와 경로를 한 인자로 받아 따옴표를 직접 해석한다. Windows 경로에는 `"`가 올 수 없다.
        let mut cmd = explorer();
        cmd.raw_arg(format!("/select,\"{}\"", path.display()));
        cmd
    }
    #[cfg(not(windows))]
    open_command(&["-R"], path)
}

#[cfg(windows)]
fn explorer() -> Command {
    let mut cmd = crate::platform::background_command("explorer.exe");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

#[cfg(not(windows))]
fn open_command(flags: &[&str], path: &Path) -> Command {
    let mut cmd = crate::platform::background_command("/usr/bin/open");
    cmd.args(flags).arg("--").arg(path);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}
