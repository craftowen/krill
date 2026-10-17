//! 개발 데몬과 클라이언트가 같은 소스 빌드 식별자를 쓴다(OWE-588).
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::Path;

fn is_test_entry(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| {
            name == "tests"
                || name == "tests.rs"
                || Path::new(name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|stem| stem.ends_with("_tests"))
        })
}

fn hash_tree(path: &Path, hash: &mut DefaultHasher) -> io::Result<()> {
    if is_test_entry(path) {
        return Ok(());
    }
    println!("cargo:rerun-if-changed={}", path.display());
    if path.is_dir() {
        let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let child = entry.path();
            if is_test_entry(&child) {
                continue;
            }
            entry.file_name().hash(hash);
            hash_tree(&child, hash)?;
        }
    } else {
        fs::read(path)?.hash(hash);
    }
    Ok(())
}

fn main() -> io::Result<()> {
    // 시각·체크아웃 경로를 넣지 않아 같은 소스의 재빌드·다른 워크트리는 같은 값이다.
    let root = Path::new("../..");
    let mut hash = DefaultHasher::new();
    for file in ["Cargo.toml", "Cargo.lock", "crates/krill-proto/build.rs"] {
        hash_tree(&root.join(file), &mut hash)?;
    }
    for name in [
        "krill-proto",
        "krill-holder",
        "krilld",
        "krill-term",
        "krill-log",
    ] {
        name.hash(&mut hash);
        let dir = root.join("crates").join(name);
        hash_tree(&dir.join("Cargo.toml"), &mut hash)?;
        hash_tree(&dir.join("src"), &mut hash)?;
    }
    for vendor in ["vendor/alacritty_terminal/src", "vendor/vte/src"] {
        vendor.hash(&mut hash);
        hash_tree(&root.join(vendor), &mut hash)?;
    }
    println!(
        "cargo:rustc-env=KRILL_DEV_BUILD={}+dev.{:016x}",
        env!("CARGO_PKG_VERSION"),
        hash.finish()
    );
    Ok(())
}
