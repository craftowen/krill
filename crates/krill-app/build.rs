//! Windows 타깃에서 krill-app.exe에 Krill 아이콘(packaging/windows/krill.ico)을 임베드한다. 다른 타깃은 아무것도 안 한다.
use std::path::Path;

fn main() -> Result<(), embed_resource::CompilationResult> {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    let dir = Path::new("../../packaging/windows");
    for file in ["krill.rc", "krill.ico"] {
        println!("cargo:rerun-if-changed={}", dir.join(file).display());
    }
    embed_resource::compile(
        dir.join("krill.rc"),
        embed_resource::ParamsIncludeDirs([dir]),
    )
    .manifest_required()
}
