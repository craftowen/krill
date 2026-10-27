use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "krill-catalog-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(p.join("home")).unwrap();
        fs::create_dir_all(p.join("repo/sub")).unwrap();
        fs::create_dir(p.join("repo/.git")).unwrap();
        Self(p)
    }
    fn file(&self, name: &str, body: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    fn load(&self, agent: &str) -> serde_json::Value {
        load(&self.0.join("home"), &self.0.join("repo/sub"), agent, None)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

#[test]
fn discovers_project_and_global_skills_commands_with_cli_invocation_and_precedence() {
    let f = Fixture::new();
    f.file(
        "repo/.claude/skills/check/SKILL.md",
        "---\nname: check\ndescription: Project check\n---\nprivate body",
    );
    f.file(
        "home/.claude/skills/check/SKILL.md",
        "---\nname: check\ndescription: Global check\n---",
    );
    f.file(
        "home/.claude/skills/korean/SKILL.md",
        "---\nname: 한글\ndescription: >\n  여러 줄\n  설명\n---",
    );
    f.file(
        "repo/.agents/skills/shared-check/SKILL.md",
        "---\nname: shared-check\ndescription: Shared agent skill\n---\nprivate body",
    );
    f.file(
        "repo/.claude/commands/review/security.md",
        "---\ndescription: Security review\n---",
    );
    f.file(
        "home/.codex/prompts/review.md",
        "---\ndescription: Review prompt\n---",
    );
    let claude = f.load("claude");
    let items = claude["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["text"] == "/review:security"));
    assert!(items.iter().any(|i| i["text"] == "/check"
        && i["description"] == "Project check"
        && i["scope"] == "Project"));
    assert!(items.iter().any(|i| i["description"] == "여러 줄 설명"));
    assert!(!items.iter().any(|i| i["text"] == "/shared-check"));
    assert!(!claude.to_string().contains("private body"));
    let codex = f.load("codex");
    assert!(
        codex["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["text"] == "$shared-check")
    );
    assert!(
        codex["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["text"] == "/prompts:review")
    );
    let opencode = f.load("opencode");
    assert!(
        opencode["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["text"] == "/shared-check")
    );
    f.file(
        "home/.gemini/antigravity-cli/skills/deep-research/SKILL.md",
        "---\nname: deep-research\ndescription: Deep research pipeline\n---",
    );
    f.file(
        "agy-profile/.gemini/config/skills/profile-skill/SKILL.md",
        "---\nname: profile-skill\ndescription: Profile skill\n---",
    );
    let agy = load(
        &f.0.join("home"),
        &f.0.join("repo/sub"),
        "agy",
        Some(&f.0.join("agy-profile")),
    );
    let agy_items = agy["items"].as_array().unwrap();
    assert!(agy_items.iter().any(|i| i["text"] == "/deep-research"));
    assert!(agy_items.iter().any(|i| i["text"] == "/profile-skill"));
    let inside_home = f.0.join("home/notes/sub");
    fs::create_dir_all(&inside_home).unwrap();
    let from_home_subdir = load(&f.0.join("home"), &inside_home, "claude", None);
    assert!(
        from_home_subdir["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["text"] == "/check" && i["scope"] == "Global")
    );
    assert_eq!(f.load("shell")["supported"], false);
}

#[test]
fn account_config_is_respected_and_hidden_or_invalid_skills_are_excluded() {
    let f = Fixture::new();
    f.file("account/skills/account/SKILL.md", "---\nname: account\n---");
    f.file("home/.codex/skills/wrong/SKILL.md", "---\nname: wrong\n---");
    f.file(
        "repo/.agents/skills/hidden/SKILL.md",
        "---\nname: hidden\nuser-invocable: false\n---",
    );
    f.file(
        "repo/.agents/skills/invalid/SKILL.md",
        "---\nname: bad\u{1b}[31m\n---",
    );
    let value = load(
        &f.0.join("home"),
        &f.0.join("repo"),
        "codex",
        Some(&f.0.join("account")),
    );
    let items = value["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["text"], "$account");
}

#[cfg(unix)]
#[test]
fn symlink_skills_work_and_cycles_and_files_are_bounded() {
    let f = Fixture::new();
    f.file("real/SKILL.md", "---\nname: linked\n---");
    let root = f.0.join("home/.agents/skills");
    fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(f.0.join("real"), root.join("linked")).unwrap();
    std::os::unix::fs::symlink(&root, root.join("cycle")).unwrap();
    let items = f.load("codex")["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["text"], "$linked");
    for n in 0..MAX_ENTRIES + 8 {
        f.file(&format!("repo/.claude/commands/c{n}.md"), "body");
    }
    let value = f.load("claude");
    assert!(value["items"].as_array().unwrap().len() <= MAX_ENTRIES);
    assert_eq!(value["truncated"], true);
}

#[test]
fn frontmatter_parser_is_bounded_and_tolerates_unstructured_text() {
    for text in [
        "",
        "---",
        "name: nope",
        "---\nno colon\n---",
        "---\nx: y\n---\nname: nope",
    ] {
        assert_eq!(field(text, "name"), None);
    }
    assert_eq!(
        field("---\nname: 'hello'\n---", "name").as_deref(),
        Some("hello")
    );
}

proptest::proptest! {
    #[test]
    fn arbitrary_frontmatter_never_panics(content in ".{0,8192}") {
        let _ = field(&content, "name");
        let _ = field(&content, "description");
    }
}
