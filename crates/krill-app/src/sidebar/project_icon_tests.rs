use super::*;

const ICO: &[u8] = include_bytes!("../../../../packaging/macos/favicon.ico");

#[test]
fn imported_github_avatar_does_not_hide_service_favicon() {
    use crate::ui::icons::ProjectIcon;

    let repo = fixture("imported-avatar");
    let cache = repo.join("cache");
    let avatar_url = "https://github.com/owner.png?size=64";
    let avatar = ImportedRepoIcon {
        kind: "image".into(),
        value: avatar_url.into(),
    };
    let cached_avatar = cached_icon_path_for_url(&cache, avatar_url);
    write_cached_icon_bytes(&cached_avatar, ICO).unwrap();
    let favicon = repo.join("public/favicon.ico");
    fs::create_dir_all(favicon.parent().unwrap()).unwrap();
    fs::write(&favicon, ICO).unwrap();
    assert_eq!(
        resolve_project_icon(&repo, true, Some(&cache), Some(&avatar)),
        Some(ProjectIcon::Image(favicon.clone()))
    );

    // 사용자가 지정한 이모지·파일은 자동 파비콘보다 우선한다.
    let emoji = ImportedRepoIcon {
        kind: "emoji".into(),
        value: "🦀".into(),
    };
    assert_eq!(
        resolve_project_icon(&repo, true, Some(&cache), Some(&emoji)),
        Some(ProjectIcon::Emoji("🦀".into()))
    );
    let custom = ImportedRepoIcon {
        kind: "image".into(),
        value: cached_avatar.to_string_lossy().into_owned(),
    };
    assert_eq!(
        resolve_project_icon(&repo, true, Some(&cache), Some(&custom)),
        Some(ProjectIcon::Image(cached_avatar.clone()))
    );

    fs::remove_file(favicon).unwrap();
    // 디렉터리 mtime 캐시와 독립된 경로에서 파비콘 없는 프로젝트의 폴백도 확인한다.
    let empty = repo.join("empty");
    fs::create_dir_all(&empty).unwrap();
    assert_eq!(
        resolve_project_icon(&empty, true, Some(&cache), Some(&avatar)),
        Some(ProjectIcon::Image(cached_avatar))
    );
    fs::remove_dir_all(repo).unwrap();
}

fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("krill-icons-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(dir).unwrap()
}

#[test]
fn jpeg_remote_icon_is_cached() {
    // GitHub 아바타처럼 `.png` URL이 JPEG를 돌려줘도 캐시에 남아야 같은 URL을 다시 받지 않는다.
    let jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xDB, 0, 0x43, 0, 8, 6, 6, 7, 6];
    assert!(has_valid_icon_bytes(jpeg));
    assert!(!has_valid_icon_bytes(&[0xFF, 0xD8]));
    let dir = fixture("jpeg");
    let url = "https://github.com/owner.png?size=64";
    let target = cached_icon_path_for_url(&dir, url);
    write_cached_icon_bytes(&target, jpeg).unwrap();
    assert_eq!(
        resolve_or_fetch_remote_icon(url, &dir),
        Some(crate::ui::icons::ProjectIcon::Image(target))
    );
    fs::remove_dir_all(&dir).ok();
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

#[test]
fn svg_and_monorepo_project_icon_detection() {
    let repo = fixture("svg-monorepo");
    let svg = br#"<?xml version="1.0" encoding="UTF-8"?>
<!-- brand mark -->
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><circle cx="16" cy="16" r="14"/></svg>"#;
    assert!(has_valid_icon_bytes(svg));
    assert!(!has_valid_icon_bytes(
        b"<!DOCTYPE html><html><body><svg></svg></body></html>"
    ));

    let monorepo_svg = repo.join("apps/web/public/favicon.svg");
    fs::create_dir_all(monorepo_svg.parent().unwrap()).unwrap();
    fs::write(&monorepo_svg, svg).unwrap();
    assert_eq!(detect_local_file_icon(&repo), Some(monorepo_svg.clone()));
    fs::remove_file(&monorepo_svg).unwrap();

    let nested_ico = repo.join("public/favicon/favicon.ico");
    fs::create_dir_all(nested_ico.parent().unwrap()).unwrap();
    fs::write(&nested_ico, ICO).unwrap();
    assert_eq!(detect_local_file_icon(&repo), Some(nested_ico.clone()));
    fs::remove_file(&nested_ico).unwrap();

    let linked_svg = repo.join("apps/web/public/brand/mark.svg");
    fs::create_dir_all(linked_svg.parent().unwrap()).unwrap();
    fs::write(&linked_svg, svg).unwrap();
    fs::write(
        repo.join("apps/web/index.html"),
        r#"<link rel="icon" type="image/svg+xml" href="/brand/mark.svg">"#,
    )
    .unwrap();
    assert_eq!(detect_local_file_icon(&repo), Some(linked_svg));
    fs::remove_dir_all(repo).unwrap();
}

#[test]
fn remote_repo_icon_probe_prioritizes_repo_favicon_over_github_avatar() {
    use crate::ui::icons::ProjectIcon;

    let root = fixture("remote-probe");
    let repo = root.join("remote-repo");
    let cache = root.join("cache");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::write(
        repo.join(".git/config"),
        "[remote \"origin\"]\n\turl = https://github.com/craftowen/demo.git\n",
    )
    .unwrap();
    let avatar_url = "https://github.com/craftowen.png?size=64";
    let cached_avatar = cached_icon_path_for_url(&cache, avatar_url);
    write_cached_icon_bytes(&cached_avatar, ICO).unwrap();

    let probe_fn =
        |(): &mut (), cwd: &Path, candidates: &[String]| -> Option<Vec<Option<PathBuf>>> {
            Some(
                candidates
                    .iter()
                    .map(|rel| {
                        let p = cwd.join(rel);
                        p.is_file().then_some(p)
                    })
                    .collect(),
            )
        };
    let read_fn = |(): &mut (), path: &Path, max_bytes: usize| -> Option<Vec<u8>> {
        let bytes = fs::read(path).ok()?;
        (bytes.len() <= max_bytes).then_some(bytes)
    };

    // 1. 파비콘 파일이 없을 때는 GitHub 아바타로 폴백한다.
    assert_eq!(
        probe_single_remote_repo_icon(&repo, &cache, &mut (), probe_fn, read_fn),
        Some(ProjectIcon::Image(cached_avatar.clone()))
    );

    // 2. 원격 저장소에 SVG 파비콘이 있으면 GitHub 아바타보다 최우선으로 선택·캐시한다.
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><rect width="24" height="24"/></svg>"#;
    let remote_svg = repo.join("apps/web/public/favicon.svg");
    fs::create_dir_all(remote_svg.parent().unwrap()).unwrap();
    fs::write(&remote_svg, svg).unwrap();

    let resolved =
        probe_single_remote_repo_icon(&repo, &cache, &mut (), probe_fn, read_fn).unwrap();
    let ProjectIcon::Image(cached_remote_svg) = resolved else {
        panic!("expected cached image icon");
    };
    assert_ne!(cached_remote_svg, cached_avatar);
    assert_eq!(
        cached_remote_svg.extension().and_then(|e| e.to_str()),
        Some("svg")
    );
    assert!(
        cached_remote_svg
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("remote-"))
    );
    fs::remove_dir_all(root).unwrap();
}
