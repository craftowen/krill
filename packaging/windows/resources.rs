//! Shared Windows PE identity for every shipped Krill executable. No resources on other targets.
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../../packaging/windows/resources.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    let dir = Path::new("../../packaging/windows");
    for file in ["krill.rc", "krill.ico"] {
        println!("cargo:rerun-if-changed={}", dir.join(file).display());
    }
    let name = env!("CARGO_PKG_NAME");
    let description = match name {
        "krill-app" => "Krill Desktop",
        "krill" => "Krill Command Line",
        "krilld" => "Krill Session Daemon",
        "krill-holder" => "Krill Terminal Holder",
        "krill-link" => "Krill Remote Link",
        _ => "Krill",
    };
    let mut macros = vec![
        format!(
            "KRILL_FILE_VERSION={},{},{},0",
            env!("CARGO_PKG_VERSION_MAJOR"),
            env!("CARGO_PKG_VERSION_MINOR"),
            env!("CARGO_PKG_VERSION_PATCH")
        ),
        format!("KRILL_VERSION=\"{}\\0\"", env!("CARGO_PKG_VERSION")),
        format!("KRILL_INTERNAL_NAME=\"{name}\\0\""),
        format!("KRILL_ORIGINAL_FILENAME=\"{name}.exe\\0\""),
        format!("KRILL_DESCRIPTION=\"{description}\\0\""),
    ];
    if name == "krill-app" {
        macros.push("KRILL_APP".into());
    }
    let mut header = String::new();
    for definition in macros {
        header.push_str("#define ");
        header.push_str(&definition.replacen('=', " ", 1));
        header.push('\n');
    }
    std::fs::write(
        Path::new(&std::env::var("OUT_DIR")?).join("krill-version.h"),
        header,
    )?;
    embed_resource::compile(
        dir.join("krill.rc"),
        embed_resource::ParamsIncludeDirs([dir]),
    )
    .manifest_required()?;
    Ok(())
}
