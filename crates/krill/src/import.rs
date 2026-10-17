//! `krill import orca`: Orca 데이터 스캐너 & 원클릭 전체 이관 CLI (MK-172 / OWE-726).

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::orca_import::{ImportDomain, ImportOptions, format_report_human, run_import};

const USAGE: &str = "usage: krill import orca [--dry-run] [--json] [--only accounts,ssh,projects,settings] [--from <orca-dir>]";

pub fn run(raw: &[OsString]) -> ExitCode {
    match execute(raw) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
            eprintln!("import: {e}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("import: {e}");
            ExitCode::FAILURE
        }
    }
}

fn execute(raw: &[OsString]) -> io::Result<()> {
    let args: Vec<String> = raw
        .iter()
        .cloned()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "arguments must be UTF-8"))?;

    let Some((first, rest)) = args.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE));
    };
    if first != "orca" {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, USAGE));
    }

    let mut dry_run = false;
    let mut json = false;
    let mut domains: Vec<ImportDomain> = Vec::new();
    let mut from_dir: Option<PathBuf> = None;

    let mut idx = 0usize;
    while let Some(arg) = rest.get(idx) {
        match arg.as_str() {
            "--dry-run" | "-n" => {
                dry_run = true;
            }
            "--json" => {
                json = true;
            }
            "--only" => {
                idx = idx.saturating_add(1);
                let val = rest.get(idx).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--only requires a value")
                })?;
                domains = ImportDomain::parse_list(val)?;
            }
            s if s.starts_with("--only=") => {
                let val = s.trim_start_matches("--only=");
                domains = ImportDomain::parse_list(val)?;
            }
            "--from" => {
                idx = idx.saturating_add(1);
                let val = rest.get(idx).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--from requires a path")
                })?;
                from_dir = Some(PathBuf::from(val));
            }
            s if s.starts_with("--from=") => {
                let val = s.trim_start_matches("--from=");
                if val.trim().is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--from requires a path",
                    ));
                }
                from_dir = Some(PathBuf::from(val));
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown flag '{other}' ({USAGE})"),
                ));
            }
        }
        idx = idx.saturating_add(1);
    }

    let opts = ImportOptions::from_env(from_dir, domains, dry_run)?;
    let report = run_import(&opts)?;
    if json {
        let rendered = serde_json::to_string_pretty(&report).map_err(io::Error::other)?;
        println!("{rendered}");
    } else {
        println!("{}", format_report_human(&report));
    }
    Ok(())
}
