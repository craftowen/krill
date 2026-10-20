//! CONVENTIONS §5 `krill-holder` 줄 수 상한(결정 12-2, 2026-10-19 개정)을 플랫폼별로 센다.
//! 상한 상향은 하네스 변경이다(메인테이너 승인). 이 테스트의 숫자만 올려 통과시키지 않는다.

use std::path::{Path, PathBuf};

const MACOS_MAX: usize = 1_300;
const LINUX_SYS_MAX: usize = 300;
const WINDOWS_MAX: usize = 2_000;

/// lib.rs에서 `#[cfg(unix)]`로만 선언되는 모듈 파일 — Windows 빌드 몫에서 뺀다.
const UNIX_ONLY: [&str; 4] = ["holder.rs", "link.rs", "sys.rs", "teardown.rs"];
const LINUX_ONLY: &str = "sys/linux.rs";

fn src_files() -> std::io::Result<Vec<(String, String)>> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path: PathBuf = entry?.path();
            if path.is_dir() {
                walk(&path, root, out)?;
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(root)
                    .map_err(std::io::Error::other)?
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&path)?));
            }
        }
        Ok(())
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&root, &root, &mut out)?;
    Ok(out)
}

fn is_windows_only(rel: &str) -> bool {
    rel.split('/')
        .any(|part| part == "windows" || part == "windows.rs")
}

/// 물리 행(`wc -l`). OWE-832 실측과 같은 셈이다.
fn physical(src: &str) -> usize {
    src.lines().count()
}

/// 최상위 `dropped` 속성이 붙은 항목(바로 앞 `///` 포함)을 뺀 행. OWE-832의 "Windows 선택 행" 셈이다.
/// rustfmt 출력이라 최상위 항목은 0열 `}`나, 0열에서 시작해 `;`로 끝나는 줄에서 끝난다.
fn selected(src: &str, dropped: &[&str]) -> usize {
    let (mut kept, mut docs, mut skipping) = (0, 0, false);
    for line in src.lines() {
        if skipping {
            skipping =
                !(line.starts_with('}') || (!line.starts_with([' ', '#']) && line.ends_with(';')));
        } else if dropped.contains(&line) {
            kept -= docs;
            docs = 0;
            skipping = true;
        } else {
            docs = if line.starts_with("///") { docs + 1 } else { 0 };
            kept += 1;
        }
    }
    kept
}

#[test]
fn holder_stays_within_per_platform_line_budget() {
    let files = src_files().unwrap();
    let macos: usize = files
        .iter()
        .filter(|(rel, _)| !is_windows_only(rel) && rel != LINUX_ONLY)
        .map(|(_, src)| physical(src))
        .sum();
    let linux: usize = files
        .iter()
        .filter(|(rel, _)| rel == LINUX_ONLY)
        .map(|(_, src)| physical(src))
        .sum();
    let windows: usize = files
        .iter()
        .filter(|(rel, _)| !UNIX_ONLY.contains(&rel.as_str()) && rel != LINUX_ONLY)
        .map(|(_, src)| selected(src, &["#[cfg(unix)]", "#[cfg(test)]"]))
        .sum();
    assert!(macos <= MACOS_MAX, "macOS 빌드 {macos}줄 > {MACOS_MAX}");
    assert!(
        linux <= LINUX_SYS_MAX,
        "{LINUX_ONLY} {linux}줄 > {LINUX_SYS_MAX}"
    );
    assert!(
        windows <= WINDOWS_MAX,
        "Windows 빌드 {windows}줄 > {WINDOWS_MAX}"
    );
}

#[test]
fn selected_drops_gated_items_with_their_docs() {
    let src = "use a;\n#[cfg(unix)]\nuse b;\n/// doc\n#[cfg(unix)]\nfn f() {\n    x();\n}\n\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
    assert_eq!(selected(src, &["#[cfg(unix)]", "#[cfg(test)]"]), 2);
}
