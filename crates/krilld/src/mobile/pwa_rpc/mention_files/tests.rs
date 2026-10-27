use super::*;
struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "krill-mentions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn file(&self, name: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "private body").unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}
#[test]
fn safe_relative_names_only_no_bodies_secrets_builds_or_traversal() {
    let f = Fixture::new();
    for name in [
        "src/hello.rs",
        "docs/space name.md",
        ".env",
        ".private/config",
        "target/debug/app",
        "build/output",
        "secrets/token",
        "config/auth.json",
        "private.key",
        "config/credentials.json",
        "notes/line\nbreak",
    ] {
        f.file(name);
    }
    let value = load(&f.0, "").unwrap();
    assert_eq!(
        value["items"],
        json!([{"path":"docs/space name.md"},{"path":"src/hello.rs"}])
    );
    assert!(!value.to_string().contains("private body"));
    assert_eq!(
        load(&f.0, "HELLO").unwrap()["items"],
        json!([{"path":"src/hello.rs"}])
    );
    for query in ["../", "/tmp", "foo/../", "C:\\files", "a\nb"] {
        assert!(load(&f.0, query).is_err(), "{query}");
    }
    assert!(load(&f.0, &"a".repeat(257)).is_err());
}
#[cfg(unix)]
#[test]
fn rejects_final_and_intermediate_symlinks_even_inside_root() {
    let f = Fixture::new();
    f.file("real/file.rs");
    std::os::unix::fs::symlink(f.0.join("real"), f.0.join("linked")).unwrap();
    std::os::unix::fs::symlink(f.0.join("real/file.rs"), f.0.join("link.rs")).unwrap();
    assert!(relative_file(&f.0, Path::new("linked/file.rs")).is_none());
    assert!(relative_file(&f.0, Path::new("link.rs")).is_none());
    assert_eq!(
        load(&f.0, "").unwrap()["items"],
        json!([{"path":"real/file.rs"}])
    );
}
#[test]
fn file_count_is_bounded() {
    let f = Fixture::new();
    for n in 0..MAX_ITEMS + 10 {
        f.file(&format!("file-{n}.rs"));
    }
    let value = load(&f.0, "").unwrap();
    assert!(value["items"].as_array().unwrap().len() <= MAX_ITEMS);
    assert_eq!(value["truncated"], true);
    assert!(relative_file(&f.0, Path::new("../outside")).is_none());
}
#[test]
fn git_ignore_is_honored_and_cwd_is_search_boundary() {
    let f = Fixture::new();
    let output = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&f.0)
        .output()
        .unwrap();
    assert!(output.status.success());
    f.file("other.rs");
    f.file("sub/visible.rs");
    f.file("sub/ignored.rs");
    fs::write(f.0.join(".gitignore"), "ignored.rs\n").unwrap();
    let value = load(&f.0.join("sub"), "").unwrap();
    assert_eq!(value["items"], json!([{"path":"visible.rs"}]));
}
