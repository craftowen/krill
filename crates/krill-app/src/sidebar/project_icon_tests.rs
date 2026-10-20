use super::*;

const ICO: &[u8] = include_bytes!("../../../../packaging/macos/favicon.ico");

fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("krill-icons-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

#[test]
fn ico_project_icon_detection() {
    let repo = fixture("ico");
    assert!(has_valid_icon_bytes(ICO));
    assert!(!has_valid_icon_bytes(&[0, 0, 1, 0, 0, 0]));
    assert!(!has_valid_icon_bytes(&[0, 0, 2, 0, 1, 0]));
    assert!(!has_valid_icon_bytes(&[0, 0, 1, 0]));

    let root_icon = repo.join("favicon.ico");
    fs::write(&root_icon, ICO).unwrap();
    assert_eq!(detect_local_file_icon(&repo), Some(root_icon.clone()));
    assert_eq!(
        resolve_project_icon(&repo, true, None, None),
        Some(crate::ui::icons::ProjectIcon::Image(root_icon.clone()))
    );
    fs::remove_file(root_icon).unwrap();

    let packaged = repo.join("packaging/macos/favicon.ico");
    fs::create_dir_all(packaged.parent().unwrap()).unwrap();
    fs::write(&packaged, ICO).unwrap();
    assert_eq!(detect_local_file_icon(&repo), Some(packaged.clone()));
    fs::remove_file(packaged).unwrap();

    let linked = repo.join("brand/site.ico");
    fs::create_dir_all(linked.parent().unwrap()).unwrap();
    fs::write(&linked, ICO).unwrap();
    fs::write(
        repo.join("index.html"),
        r#"<link rel="icon" href="/brand/site.ico?v=2">"#,
    )
    .unwrap();
    assert_eq!(detect_local_file_icon(&repo), Some(linked));
    fs::write(repo.join("favicon.ico"), b"invalid image").unwrap();
    assert!(detect_local_file_icon(&repo).is_some());
    fs::remove_file(repo.join("index.html")).unwrap();
    assert_eq!(detect_local_file_icon(&repo), None);
    fs::remove_dir_all(repo).unwrap();
}

#[test]
fn cargo_homepage_project_icon_detection() {
    let repo = fixture("cargo");
    let favicon = "https://www.google.com/s2/favicons?domain=krill.page&sz=64";
    let manifest = repo.join("Cargo.toml");
    fs::write(
        &manifest,
        "[package]\nname='demo'\nhomepage='https://krill.page/docs'\n",
    )
    .unwrap();
    assert_eq!(
        detect_cargo_homepage_favicon_url(&repo).as_deref(),
        Some(favicon)
    );
    assert_eq!(
        probe_project_icon_source(&repo, true),
        CachedIconOutcome::RemoteUrl(favicon.into())
    );
    fs::write(
        &manifest,
        "[workspace.package]\nhomepage='https://krill.page'\n",
    )
    .unwrap();
    assert_eq!(
        detect_cargo_homepage_favicon_url(&repo).as_deref(),
        Some(favicon)
    );

    let member = repo.join("crates/demo");
    fs::create_dir_all(&member).unwrap();
    fs::write(
        member.join("Cargo.toml"),
        "[package]\nname='demo'\nhomepage.workspace=true\n",
    )
    .unwrap();
    assert_eq!(
        detect_cargo_homepage_favicon_url(&member).as_deref(),
        Some(favicon)
    );
    fs::write(
        member.join("Cargo.toml"),
        "[package]\nname='demo'\nworkspace='../..'\nhomepage.workspace=true\n",
    )
    .unwrap();
    assert_eq!(
        detect_cargo_homepage_favicon_url(&member).as_deref(),
        Some(favicon)
    );
    fs::write(
        &manifest,
        "[package]\nhomepage.workspace=true\n[workspace.package]\nhomepage='https://krill.page'\n",
    )
    .unwrap();
    assert_eq!(
        detect_cargo_homepage_favicon_url(&repo).as_deref(),
        Some(favicon)
    );
    fs::write(member.join("Cargo.toml"), "[package]\nname='demo'\n").unwrap();
    assert_eq!(detect_cargo_homepage_favicon_url(&member), None);
    fs::write(&manifest, "not valid toml = [").unwrap();
    assert_eq!(detect_cargo_homepage_favicon_url(&repo), None);
    fs::write(&manifest, "[package]\nhomepage='https://github.com/o/r'\n").unwrap();
    assert_eq!(detect_cargo_homepage_favicon_url(&repo), None);
    fs::write(
        &manifest,
        " ".repeat(usize::try_from(MAX_ICON_SOURCE_BYTES + 1).unwrap()),
    )
    .unwrap();
    assert_eq!(detect_cargo_homepage_favicon_url(&repo), None);
    fs::remove_dir_all(repo).unwrap();
}

#[test]
fn resolves_krill_packaged_favicon() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert_eq!(
        detect_local_file_icon(&repo),
        Some(repo.join("packaging/macos/favicon.ico"))
    );
    assert_eq!(
        detect_cargo_homepage_favicon_url(&repo).as_deref(),
        Some("https://www.google.com/s2/favicons?domain=krill.page&sz=64")
    );
}
