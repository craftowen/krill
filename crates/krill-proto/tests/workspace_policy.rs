//! 워크스페이스 정책 테스트. AGENTS 규칙 5·CONVENTIONS U1: unsafe 린트를 끄는 속성은 FFI 모듈에만 둔다.
//! 허용 목록을 넓히는 것은 하네스 변경이다(메인테이너 승인).

use std::path::Path;

/// 워크스페이스 루트 기준. holder·krilld FFI 모듈(Unix·Windows), krill-app platform 모듈(objc2·Win32).
const UNSAFE_ALLOWED: [&str; 5] = [
    "crates/krill-holder/src/sys.rs",
    "crates/krill-holder/src/sys/windows.rs",
    "crates/krilld/src/sys.rs",
    "crates/krill-app/src/platform.rs",
    "crates/krill-app/src/platform/windows.rs",
];

/// 이 파일도 검사 대상이라 린트 이름을 글자 그대로 쓰지 않는다.
const LINT: &str = concat!("unsafe", "_code");

/// 공백을 지운 소스에서 린트 이름이 `forbid(`·`deny(` 바로 뒤가 아닌 곳(expect·allow·warn·목록 안)에 나오는지.
fn relaxes_unsafe_lint(src: &str) -> bool {
    let compact: String = src.chars().filter(|c| !c.is_whitespace()).collect();
    compact.match_indices(LINT).any(|(idx, _)| {
        let before = compact.get(..idx).unwrap_or_default();
        !(before.ends_with("forbid(") || before.ends_with("deny("))
    })
}

fn rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            rs_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

#[test]
fn unsafe_lint_is_relaxed_only_in_ffi_modules() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    rs_files(&root.join("crates"), &mut files).unwrap();
    let mut seen = Vec::new();
    let mut violations = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !relaxes_unsafe_lint(&std::fs::read_to_string(&file).unwrap()) {
            continue;
        }
        if UNSAFE_ALLOWED.contains(&rel.as_str()) {
            seen.push(rel);
        } else {
            violations.push(rel);
        }
    }
    assert!(
        violations.is_empty(),
        "FFI 모듈 밖에서 unsafe 린트를 끔: {violations:?}"
    );
    // 목록이 낡으면(파일 이동·삭제) 같이 고친다 — 죽은 항목이 새 파일 경로를 미리 열어 두지 않게.
    seen.sort();
    let mut allowed = UNSAFE_ALLOWED.to_vec();
    allowed.sort_unstable();
    assert_eq!(
        seen, allowed,
        "허용 목록에 더는 unsafe 예외가 없는 파일이 있다"
    );
}

#[test]
fn detects_relaxing_attributes() {
    let lint = LINT;
    assert!(!relaxes_unsafe_lint(&format!(
        "#![forbid({lint})]\n#![deny( {lint} )]"
    )));
    for bad in [
        format!("#![expect({lint}, reason = \"x\")]"),
        format!("#[allow(\n    {lint}\n)]"),
        format!("#![cfg_attr(unix, expect(\n {lint},\n reason = \"x\"))]"),
        format!("#[expect(clippy::foo, {lint})]"),
        format!("#![warn({lint})]"),
    ] {
        assert!(relaxes_unsafe_lint(&bad), "{bad}");
    }
}
