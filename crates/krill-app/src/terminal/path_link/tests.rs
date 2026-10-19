use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mk-path-link-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("한글.txt"), "x").unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

/// 한 행. 2칸 글자(한글·전각)는 WIDE + SPACER 두 칸이다.
fn row(text: &str) -> Vec<Cell> {
    let mut out = Vec::new();
    for ch in text.chars() {
        let wide = is_cjk_or_hangul(ch) || is_fullwidth_punct(ch);
        out.push(Cell {
            ch,
            flags: if wide { Flags::WIDE } else { Flags::default() },
            ..Cell::default()
        });
        if wide {
            out.push(Cell {
                flags: Flags::SPACER,
                ..Cell::default()
            });
        }
    }
    out
}

fn at(text: &str, column: u16, cwd: &Path) -> Option<PathLink> {
    Cache::default().link_at_with_home(&row(text), column, Some(cwd), Some(cwd))
}

#[test]
fn parses_compiler_grep_and_git_formats() {
    let dir = TempDir::new("formats");
    let file = dir.0.join("src/main.rs");
    // rustc: "  --> src/main.rs:12:5"
    let l = at("  --> src/main.rs:12:5", 8, &dir.0).unwrap();
    assert_eq!(
        l,
        PathLink {
            column: 6,
            cols: 16,
            path: file.clone(),
            line: Some(12),
            col: Some(5),
        }
    );
    // grep -n: "src/main.rs:3:fn main" — 링크는 `:3`까지, 뒤 내용 위는 링크가 아니다.
    let l = at("src/main.rs:3:fn main", 0, &dir.0).unwrap();
    assert_eq!((l.cols, l.line, l.col), (13, Some(3), None));
    assert_eq!(at("src/main.rs:3:fn main", 15, &dir.0), None);
    // git diff: "+++ b/src/main.rs"
    let l = at("+++ b/src/main.rs", 6, &dir.0).unwrap();
    assert_eq!((l.column, l.cols, &l.path), (4, 13, &file));
    // 따옴표·괄호·문장 끝 점은 경로에 넣지 않는다.
    let l = at("error in 'src/main.rs'.", 12, &dir.0).unwrap();
    assert_eq!((l.column, l.cols), (10, 11));
    let l = at("see src/main.rs.", 5, &dir.0).unwrap();
    assert_eq!((l.column, l.cols, l.line), (4, 11, None));
    // 전각 괄호·한글 조사가 바로 이어져도 파일 경로만 정확히 잡는다(#30093).
    let l = at("（src/main.rs:12）", 4, &dir.0).unwrap();
    assert_eq!(
        (l.column, l.cols, &l.path, l.line),
        (2, 14, &file, Some(12))
    );
    let l = at("src/main.rs에서 오류", 2, &dir.0).unwrap();
    assert_eq!((l.column, l.cols, &l.path, l.line), (0, 11, &file, None));
    assert_eq!(at("src/main.rs에서 오류", 12, &dir.0), None);
}

#[test]
fn resolves_relative_to_cwd_absolute_and_home() {
    let dir = TempDir::new("resolve");
    let file = dir.0.join("src/main.rs");
    let abs = format!("{}:7", file.display());
    let l = at(&abs, 1, Path::new("/nonexistent")).unwrap();
    assert_eq!((l.path, l.line), (file.clone(), Some(7)));
    // 상대 경로는 cwd 기준: 다른 cwd에서는 같은 글자가 링크가 아니다.
    assert_eq!(at("src/main.rs", 0, &dir.0).unwrap().path, file);
    assert_eq!(
        at("./src/main.rs", 0, &dir.0).unwrap().path,
        dir.0.join("./src/main.rs")
    );
    assert_eq!(at("src/main.rs", 0, Path::new("/")), None);
    // `~/`는 홈 기준(테스트는 홈을 임시 폴더로 준다).
    assert_eq!(
        at("~/src/main.rs", 3, &dir.0).unwrap().path,
        dir.0.join("src/main.rs")
    );
    // 없는 cwd(아직 모름)면 상대 경로는 링크가 아니다.
    let mut cache = Cache::default();
    assert_eq!(
        cache.link_at_with_home(&row("src/main.rs"), 0, None, None),
        None
    );
}

#[test]
fn ignores_missing_files_directories_and_urls() {
    let dir = TempDir::new("ignore");
    assert_eq!(at("src/nope.rs:1:1", 2, &dir.0), None);
    assert_eq!(at("src", 1, &dir.0), None, "폴더는 파일 링크가 아니다");
    assert_eq!(at("https://example.com/src/main.rs", 10, &dir.0), None);
    assert_eq!(at("file:///etc/hosts", 10, &dir.0), None);
    assert_eq!(
        at("a  src/main.rs", 1, &dir.0),
        None,
        "공백 위는 링크가 아니다"
    );
    assert_eq!(at("src/main.rs", 40, &dir.0), None, "행 밖");
    // 양방향 제어 문자는 경로를 끊는다(위장 방지).
    assert_eq!(at("src/ma\u{202e}in.rs", 1, &dir.0), None);
}

#[test]
fn wide_characters_map_to_cells() {
    let dir = TempDir::new("wide");
    // "보기 한글.txt": '보'(0-1) '기'(2-3) ' '(4) '한'(5-6) '글'(7-8) ".txt"(9-12)
    let text = "보기 한글.txt";
    for column in [5, 6, 8, 12] {
        let l = at(text, column, &dir.0).unwrap();
        assert_eq!((l.column, l.cols), (5, 8), "칸 {column}");
    }
    assert_eq!(at(text, 4, &dir.0), None);
}

#[test]
fn cache_skips_disk_for_same_candidate() {
    let dir = TempDir::new("cache");
    let mut cache = Cache::default();
    let r = row("src/main.rs:1");
    assert!(cache.link_at_with_home(&r, 0, Some(&dir.0), None).is_some());
    // 같은 글자열이면 디스크를 다시 보지 않는다(지운 뒤에도 같은 결과 = 캐시를 탔다).
    fs::remove_file(dir.0.join("src/main.rs")).unwrap();
    assert!(cache.link_at_with_home(&r, 3, Some(&dir.0), None).is_some());
    // 다른 cwd면 다시 본다.
    assert!(
        cache
            .link_at_with_home(&r, 3, Some(Path::new("/")), None)
            .is_none()
    );
    assert!(cache.link_at_with_home(&r, 3, Some(&dir.0), None).is_none());
}

#[test]
fn url_at_parses_plaintext_http_https_and_trims_punctuation() {
    let r = row("visit (https://example.com:8080/a/b?x=1&y=2#frag).");
    assert_eq!(url_at(&r, 6), None, "여는 괄호 위는 링크가 아니다");
    let expected = UrlLink {
        column: 7,
        cols: 41,
        uri: "https://example.com:8080/a/b?x=1&y=2#frag".into(),
    };
    assert_eq!(url_at(&r, 7), Some(expected.clone()));
    assert_eq!(url_at(&r, 25), Some(expected.clone()));
    assert_eq!(url_at(&r, 47), Some(expected));
    assert_eq!(url_at(&r, 48), None, "닫는 괄호·마침표 위는 링크가 아니다");

    // 짝이 맞는 괄호(위키백과 등)는 유지하고 바깥 닫는 괄호만 잘라낸다.
    let wiki = row("(https://en.wikipedia.org/wiki/Rust_(programming_language))");
    assert_eq!(
        url_at(&wiki, 10),
        Some(UrlLink {
            column: 1,
            cols: 57,
            uri: "https://en.wikipedia.org/wiki/Rust_(programming_language)".into(),
        })
    );

    // http:// 및 한글 2칸 경로/주변 문자.
    let kr = row("링크: http://localhost:3000/문서!");
    assert_eq!(
        url_at(&kr, 10),
        Some(UrlLink {
            column: 6,
            cols: 26,
            uri: "http://localhost:3000/문서".into(),
        })
    );

    // CJK·한글 조사 및 전각 괄호에서 URL을 끊는다(#30213, #30037).
    let local_kr = row("서버 http://localhost:3000에서 열림");
    assert_eq!(
        url_at(&local_kr, 8),
        Some(UrlLink {
            column: 5,
            cols: 21,
            uri: "http://localhost:3000".into(),
        })
    );
    assert_eq!(url_at(&local_kr, 26), None);
    let path_kr = row("PR https://github.com/org/repo/pull/42를 확인");
    assert_eq!(
        url_at(&path_kr, 5),
        Some(UrlLink {
            column: 3,
            cols: 35,
            uri: "https://github.com/org/repo/pull/42".into(),
        })
    );
    let fw = row("（https://example.com/docs）");
    assert_eq!(
        url_at(&fw, 4),
        Some(UrlLink {
            column: 2,
            cols: 24,
            uri: "https://example.com/docs".into(),
        })
    );

    // 안전하지 않은 스킴·중간 접두사·양방향 제어 문자 위장은 거부한다.
    assert_eq!(url_at(&row("javascript:alert(1)"), 2), None);
    assert_eq!(url_at(&row("file:///etc/hosts"), 2), None);
    assert_eq!(url_at(&row("foohttps://example.com"), 10), None);
    assert_eq!(url_at(&row("https://..."), 8), None);
    assert_eq!(
        url_at(&row("https://example.com/\u{202e}gpj.exe"), 10),
        None
    );
}
