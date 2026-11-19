use super::*;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mk-editor-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn text(opened: Opened) -> String {
    match opened {
        Opened::Text(_, t) => t,
        other => panic!("not text: {other:?}"),
    }
}

/// mtime 해상도와 무관하게 지문이 바뀌도록 길이도 바꾼다.
fn rewrite(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
}

#[test]
fn open_classifies_text_binary_large_dir_and_bad_utf8() {
    let dir = TempDir::new("open");
    let p = dir.0.join("a.rs");
    fs::write(&p, "fn main() {}\n").unwrap();
    assert_eq!(text(open(&p).unwrap().0), "fn main() {}\n");

    let bin = dir.0.join("b.dat");
    fs::write(&bin, b"ab\0cd").unwrap();
    assert_eq!(open(&bin).unwrap().0, Opened::Placeholder(Kind::Binary, 5));

    let bad = dir.0.join("c.txt");
    fs::write(&bad, [0xff, 0xfe, b'a']).unwrap();
    assert_eq!(
        open(&bad).unwrap().0,
        Opened::Unopenable(Unopenable::NotUtf8)
    );

    let big = dir.0.join("d.log");
    fs::write(&big, vec![b'a'; usize::try_from(MAX_BYTES).unwrap() + 1]).unwrap();
    assert_eq!(
        open(&big).unwrap().0,
        Opened::Placeholder(Kind::TooLarge, MAX_BYTES + 1)
    );

    assert_eq!(
        open(&dir.0).unwrap().0,
        Opened::Unopenable(Unopenable::Directory)
    );
    assert!(open(&dir.0.join("missing")).is_err());
}

/// MK-121 분기 매핑: 분류 결과마다 파일 탭 화면(코드·미리보기 토글·이미지·표·자리표시자)이 정해진다.
#[test]
fn open_maps_each_format_to_its_viewer() {
    let dir = TempDir::new("viewer");
    let write = |name: &str, body: &[u8]| {
        let p = dir.0.join(name);
        fs::write(&p, body).unwrap();
        p
    };
    let opened = |p: &Path| open(p).unwrap().0;
    let kind_of = |p: &Path| match opened(p) {
        Opened::Text(kind, _) => kind,
        other => panic!("not text: {other:?}"),
    };

    let rs = write("main.rs", b"fn main() {}\n");
    assert_eq!(kind_of(&rs), Kind::Text { language: "rust" });
    assert_eq!(preview(kind_of(&rs)), None);
    assert_eq!(language(kind_of(&rs), &rs), "rust");

    let md = write("README.md", b"# hi\n");
    assert_eq!(preview(kind_of(&md)), Some(Preview::Markdown));
    assert_eq!(language(kind_of(&md), &md), "markdown");

    let csv = write("a.csv", b"a,b\n1,2\n");
    assert_eq!(
        preview(kind_of(&csv)),
        Some(Preview::Table(Delimiter::Comma))
    );
    let tsv = write("a.tsv", b"a\tb\n1\t2\n");
    assert_eq!(preview(kind_of(&tsv)), Some(Preview::Table(Delimiter::Tab)));

    let nb = write("n.ipynb", br#"{"cells":[],"nbformat":4}"#);
    assert_eq!(preview(kind_of(&nb)), Some(Preview::Notebook));
    assert_eq!(language(kind_of(&nb), &nb), "json");

    let svg = write("i.svg", b"<svg xmlns='http://www.w3.org/2000/svg'/>");
    assert_eq!(preview(kind_of(&svg)), Some(Preview::Svg));

    let html = write("page.html", b"<!DOCTYPE html><title>Demo</title>");
    assert_eq!(kind_of(&html), Kind::Text { language: "html" });
    assert_eq!(preview(kind_of(&html)), Some(Preview::Html));
    assert_eq!(language(kind_of(&html), &html), "html");

    // 1x1 PNG 헤더: 크기만 읽고 텍스트로 열지 않는다.
    let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    let png = write("p.png", &png);
    assert_eq!(
        opened(&png),
        Opened::Image(Ok(Dimensions {
            width: 1,
            height: 1
        }))
    );

    for (name, kind) in [
        ("doc.pdf", Kind::Pdf),
        ("clip.mp4", Kind::Media),
        ("f.woff2", Kind::Font),
        ("x.zip", Kind::Binary),
    ] {
        let p = write(name, b"abc");
        assert_eq!(opened(&p), Opened::Placeholder(kind, 3), "{name}");
    }

    // 에디터 한도를 넘는 CSV는 앞부분 표만, 마크다운은 자리표시자.
    let mut rows = b"a,b\n".to_vec();
    while byte_len(&rows) <= MAX_BYTES {
        rows.extend_from_slice(b"1234567890,abcdefghij\n");
    }
    let big_csv = write("big.csv", &rows);
    match opened(&big_csv) {
        Opened::Table(table) => {
            assert!(table.truncated);
            assert_eq!(table.rows[0], ["a", "b"]);
        }
        other => panic!("not a table: {other:?}"),
    }
    let big_md = write("big.md", &rows);
    assert_eq!(
        opened(&big_md),
        Opened::Placeholder(Kind::TooLarge, byte_len(&rows))
    );
}

#[test]
fn doc_reloads_on_external_change() {
    let dir = TempDir::new("reload");
    let p = dir.0.join("a.txt");
    fs::write(&p, "one").unwrap();
    let (_, st) = open(&p).unwrap();
    let mut doc = Doc::new(p.clone(), st);
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Nothing);
    rewrite(&p, "outside change");
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Reload);
    let (opened, st) = open(&p).unwrap();
    assert_eq!(text(opened), "outside change");
    doc = Doc::new(p.clone(), st);
    assert_eq!(doc.disk(), Disk::Same);
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Nothing);
}

#[test]
fn deleted_file_is_marked_and_reloads_when_recreated() {
    let dir = TempDir::new("delete");
    let p = dir.0.join("a.txt");
    fs::write(&p, "one").unwrap();
    let (_, st) = open(&p).unwrap();
    let mut doc = Doc::new(p.clone(), st);
    fs::remove_file(&p).unwrap();
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Marked);
    assert_eq!(doc.disk(), Disk::Deleted);
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Nothing);

    rewrite(&p, "recreated");
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Reload);
}

/// 감사 2026-10-17: FIFO를 열면 `open(2)`이 쓰는 쪽을 기다리며 UI 스레드가 멈췄다. 일반 파일이 아니면 열지 않는다.
#[test]
fn open_does_not_block_on_fifo() {
    let dir = TempDir::new("fifo");
    let p = dir.0.join("pipe");
    assert!(Command::new("mkfifo").arg(&p).status().unwrap().success());
    assert_eq!(open(&p).unwrap().0, Opened::Unopenable(Unopenable::NotFile));
}

#[test]
fn language_and_name() {
    let p = Path::new("/a/b/Main.RS");
    assert_eq!(language(file_format::classify(p, b"", 0), p), "rust");
    let p = Path::new("/a/Makefile");
    assert_eq!(language(file_format::classify(p, b"", 0), p), "make");
    assert_eq!(file_name(Path::new("/a/b/c.json")), "c.json");
}

#[test]
fn quick_open_lists_git_files_respecting_gitignore() {
    let dir = TempDir::new("quick");
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .current_dir(&dir.0)
                .args(args)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["init", "-q"]);
    fs::write(dir.0.join(".gitignore"), "ignored.txt\n").unwrap();
    fs::write(dir.0.join("ignored.txt"), "x").unwrap();
    fs::create_dir(dir.0.join("src")).unwrap();
    fs::write(dir.0.join("src/lib.rs"), "x").unwrap();
    let files = quick_open::list(&dir.0);
    assert!(files.contains(&PathBuf::from("src/lib.rs")));
    assert!(files.contains(&PathBuf::from(".gitignore")));
    assert!(!files.contains(&PathBuf::from("ignored.txt")));

    let items = quick_open::items(&files);
    let hit = crate::jump::search(&items, "librs")[0];
    assert_eq!(
        quick_open::chosen(&files, items[hit].id),
        Some(&PathBuf::from("src/lib.rs"))
    );
}

#[test]
fn quick_open_walks_plain_folder_skipping_hidden() {
    let dir = TempDir::new("walk");
    fs::create_dir_all(dir.0.join("a/node_modules")).unwrap();
    fs::create_dir_all(dir.0.join(".hidden")).unwrap();
    fs::write(dir.0.join("a/x.txt"), "x").unwrap();
    fs::write(dir.0.join("a/node_modules/y.js"), "x").unwrap();
    fs::write(dir.0.join(".hidden/z"), "x").unwrap();
    let files = quick_open::list(&dir.0);
    assert_eq!(files, vec![PathBuf::from("a/x.txt")]);
}

#[test]
fn html_syntax_highlights_tags_attributes_style_and_script_and_extracts_title() {
    let src = concat!(
        "<!DOCTYPE html>\n",
        "<!-- header comment -->\n",
        "<html lang=\"ko\">\n",
        "<head>\n",
        "  <title>  2차 교육:\n  Aside & 달세ERP  </title>\n",
        "  <style>body { color: #ff00aa; font-size: 16px; }</style>\n",
        "</head>\n",
        "<body>\n",
        "  <script>const count = 42; console.log(\"ready\");</script>\n",
        "</body>\n",
        "</html>\n",
    );
    assert_eq!(
        syntax::html_title(src).as_deref(),
        Some("2차 교육: Aside & 달세ERP")
    );
    assert_eq!(syntax::html_title("<html><title>   </title></html>"), None);

    let tokens = syntax::tokenize_for("html", src);
    let has = |needle: &str, expected_kind: &str| {
        tokens
            .iter()
            .any(|(range, kind)| *kind == expected_kind && src.get(range.clone()) == Some(needle))
    };
    assert!(has("<!DOCTYPE html>", "keyword"), "{tokens:?}");
    assert!(has("<!-- header comment -->", "comment"), "{tokens:?}");
    assert!(has("html", "tag"), "{tokens:?}");
    assert!(has("lang", "attribute"), "{tokens:?}");
    assert!(has("\"ko\"", "string"), "{tokens:?}");
    assert!(
        has("color", "property"),
        "embedded <style> CSS property: {tokens:?}"
    );
    assert!(
        has("#ff00aa", "constant"),
        "embedded <style> CSS hex color: {tokens:?}"
    );
    assert!(
        has("const", "keyword"),
        "embedded <script> JS keyword: {tokens:?}"
    );
    assert!(
        has("42", "number"),
        "embedded <script> JS number: {tokens:?}"
    );
    assert!(
        has("\"ready\"", "string"),
        "embedded <script> JS string: {tokens:?}"
    );
}
