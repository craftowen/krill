use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

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
fn atomic_save_replaces_content_keeps_mode_and_leaves_no_temp() {
    let dir = TempDir::new("save");
    let p = dir.0.join("script.sh");
    fs::write(&p, "old\n").unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o750)).unwrap();
    let ino_before = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&p).unwrap());

    let stamp_after = save_atomic(&p, "new 내용\n", stamp(&p).unwrap()).unwrap();

    assert_eq!(fs::read_to_string(&p).unwrap(), "new 내용\n");
    let meta = fs::metadata(&p).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o750);
    // rename으로 교체했으니 inode가 바뀐다(제자리 덮어쓰기가 아님).
    assert_ne!(std::os::unix::fs::MetadataExt::ino(&meta), ino_before);
    assert_eq!(stamp(&p).unwrap(), Some(stamp_after));
    let names: Vec<_> = fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("script.sh")]);
}

#[test]
fn atomic_save_follows_symlink_and_creates_deleted_file() {
    let dir = TempDir::new("link");
    let real = dir.0.join("real.txt");
    fs::write(&real, "a").unwrap();
    let link = dir.0.join("link.txt");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    save_atomic(&link, "b", stamp(&link).unwrap()).unwrap();
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&real).unwrap(), "b");

    let gone = dir.0.join("gone.txt");
    save_atomic(&gone, "back", None).unwrap();
    assert_eq!(fs::read_to_string(&gone).unwrap(), "back");
}

#[test]
fn atomic_save_failure_keeps_old_file() {
    let dir = TempDir::new("fail");
    let p = dir.0.join("ro").join("x.txt");
    fs::create_dir(p.parent().unwrap()).unwrap();
    fs::write(&p, "keep").unwrap();
    fs::set_permissions(p.parent().unwrap(), fs::Permissions::from_mode(0o500)).unwrap();
    let result = save_atomic(&p, "lost?", stamp(&p).unwrap());
    fs::set_permissions(p.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(&p).unwrap(), "keep");
}

/// 열기 → 편집(dirty) → 되돌리기(깨끗) → 저장 → 자기 저장은 외부 변경이 아님.
#[test]
fn doc_edit_dirty_save_cycle() {
    let dir = TempDir::new("doc");
    let p = dir.0.join("a.txt");
    fs::write(&p, "one").unwrap();
    let (opened, st) = open(&p).unwrap();
    let mut doc = Doc::new(p.clone(), text(opened), st);
    assert!(!doc.is_dirty());

    assert!(doc.edited("one!" == doc.saved()));
    assert!(doc.is_dirty());
    assert!(
        !doc.edited("one!!" == doc.saved()),
        "still dirty: no change"
    );
    assert!(
        doc.edited("one" == doc.saved()),
        "undo back to saved is clean"
    );
    assert!(!doc.is_dirty());

    doc.edited(false);
    doc.save("two".into()).unwrap();
    assert!(!doc.is_dirty());
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Nothing);
}

#[test]
fn clean_doc_reloads_on_external_change() {
    let dir = TempDir::new("reload");
    let p = dir.0.join("a.txt");
    fs::write(&p, "one").unwrap();
    let (opened, st) = open(&p).unwrap();
    let mut doc = Doc::new(p.clone(), text(opened), st);
    rewrite(&p, "outside change");
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Reload);
    let (opened, st) = open(&p).unwrap();
    doc.reloaded(text(opened), st);
    assert_eq!(doc.saved(), "outside change");
    assert_eq!(doc.disk(), Disk::Same);
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Nothing);
}

#[test]
fn dirty_doc_is_marked_not_reloaded_then_keep_or_reload() {
    let dir = TempDir::new("conflict");
    let p = dir.0.join("a.txt");
    fs::write(&p, "one").unwrap();
    let (opened, st) = open(&p).unwrap();
    let mut doc = Doc::new(p.clone(), text(opened), st);
    doc.edited(false);
    rewrite(&p, "outside change");
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Marked);
    assert_eq!(doc.disk(), Disk::Changed);
    assert!(doc.is_dirty(), "draft kept");
    // 같은 변경으로는 다시 표시하지 않는다.
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Nothing);

    doc.keep_mine(stamp(&p).unwrap());
    assert_eq!(doc.disk(), Disk::Same);
    assert!(doc.is_dirty());

    rewrite(&p, "second outside change!");
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Marked);
    let (opened, st) = open(&p).unwrap();
    doc.reloaded(text(opened), st);
    assert!(!doc.is_dirty());
    assert_eq!(doc.disk(), Disk::Same);
}

#[test]
fn deleted_file_is_marked_and_save_recreates() {
    let dir = TempDir::new("delete");
    let p = dir.0.join("a.txt");
    fs::write(&p, "one").unwrap();
    let (opened, st) = open(&p).unwrap();
    let mut doc = Doc::new(p.clone(), text(opened), st);
    fs::remove_file(&p).unwrap();
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Marked);
    assert_eq!(doc.disk(), Disk::Deleted);
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Nothing);

    doc.save("one".into()).unwrap();
    assert_eq!(doc.disk(), Disk::Same);

    // 깨끗한 탭은 지워졌다 다시 생기면 다시 읽는다.
    fs::remove_file(&p).unwrap();
    doc.poll(None);
    rewrite(&p, "recreated");
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Reload);
}

/// 감사 2026-10-17: 읽은 뒤 바깥(에이전트 등)에서 바뀐 파일을 ⌘S가 조용히 덮어쓰지 않는다(lost update).
#[test]
fn save_refuses_to_overwrite_external_change() {
    let dir = TempDir::new("lost");
    let p = dir.0.join("a.txt");
    fs::write(&p, "one").unwrap();
    let (opened, st) = open(&p).unwrap();
    let mut doc = Doc::new(p.clone(), text(opened), st);
    doc.edited(false);
    // 감시 주기(1초)가 돌기 전에 바깥에서 바뀌었다.
    rewrite(&p, "agent edit");
    assert!(matches!(doc.save("mine".into()), Err(SaveError::Conflict)));
    assert_eq!(fs::read_to_string(&p).unwrap(), "agent edit");
    assert!(doc.is_dirty(), "draft kept");

    // 감시가 표시한 뒤에도 답하기 전에는 저장하지 않고, "내 편집 유지"를 고르면 저장한다.
    assert_eq!(doc.poll(stamp(&p).unwrap()), Poll::Marked);
    assert!(matches!(doc.save("mine".into()), Err(SaveError::Conflict)));
    assert_eq!(fs::read_to_string(&p).unwrap(), "agent edit");
    doc.keep_mine(stamp(&p).unwrap());
    doc.save("mine".into()).unwrap();
    assert_eq!(fs::read_to_string(&p).unwrap(), "mine");
    assert!(!doc.is_dirty());

    // 지운 파일을 누가 다시 만들었으면 덮어쓰지 않는다.
    fs::remove_file(&p).unwrap();
    doc.poll(None);
    rewrite(&p, "recreated by someone");
    assert!(matches!(doc.save("mine".into()), Err(SaveError::Conflict)));
    assert_eq!(fs::read_to_string(&p).unwrap(), "recreated by someone");
}

/// 감사 2026-10-17: 쓰기 권한이 없는 파일을 rename으로 갈아끼우지 않는다(폴더 쓰기 권한만으로 덮어쓰기 방지).
#[test]
fn save_refuses_read_only_file() {
    let dir = TempDir::new("ro-file");
    let p = dir.0.join("locked.txt");
    fs::write(&p, "keep").unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o444)).unwrap();
    let result = save_atomic(&p, "overwrite?", stamp(&p).unwrap());
    assert!(
        matches!(&result, Err(SaveError::Io(e)) if e.kind() == io::ErrorKind::PermissionDenied),
        "{result:?}"
    );
    assert_eq!(fs::read_to_string(&p).unwrap(), "keep");
    let names: Vec<_> = fs::read_dir(&dir.0).unwrap().collect();
    assert_eq!(names.len(), 1, "no temp file left");
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
