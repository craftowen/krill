//! `krill drop`: queue files for a paired phone (Krill Drop outbox). Writes the state directory
//! directly, so it works while krilld is down; the phone pulls the outbox when it next connects.
use krill_proto::{daemon, drop};
use serde_json::json;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const HELP: &str = "usage: krill drop <file>... [--json]\n       krill drop ls [--json]\n       krill drop rm <id>\nFiles wait up to 7 days for the phone (Krill Drop on the PC screen). Phone uploads land in ~/Downloads/Krill Drop.";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Send { files: Vec<PathBuf>, json: bool },
    List { json: bool },
    Remove { id: String },
    Help,
}

fn parse(args: &[OsString]) -> Result<Command, &'static str> {
    let json = args.iter().any(|a| a == "--json");
    let rest: Vec<&OsString> = args.iter().filter(|a| *a != "--json").collect();
    match rest.first().and_then(|a| a.to_str()) {
        Some("-h" | "--help" | "help") => Ok(Command::Help),
        Some("ls") if rest.len() == 1 => Ok(Command::List { json }),
        Some("rm") if rest.len() == 2 && !json => Ok(Command::Remove {
            id: rest.get(1).and_then(|a| a.to_str()).ok_or(HELP)?.to_owned(),
        }),
        None | Some("ls" | "rm") => Err(HELP),
        _ => {
            let files: Vec<PathBuf> = rest
                .iter()
                .skip_while(|a| **a == "--")
                .map(PathBuf::from)
                .collect();
            if files.is_empty() {
                return Err(HELP);
            }
            Ok(Command::Send { files, json })
        }
    }
}

pub fn run(args: &[OsString]) -> ExitCode {
    let command = match parse(args) {
        Ok(Command::Help) => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(command) => command,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    let Some(state) = daemon::state_dir() else {
        eprintln!("krill drop: cannot find the Krill state directory (HOME is unset)");
        return ExitCode::FAILURE;
    };
    match command {
        Command::Send { files, json } => {
            let mut sent = Vec::new();
            let mut failed = false;
            for file in &files {
                match drop::add(&state, file) {
                    Ok(item) => {
                        if !json {
                            println!("{}\t{}", item.id, item.name);
                        }
                        sent.push(json!({"id":item.id,"name":item.name,"size":item.size}));
                    }
                    Err(error) => {
                        eprintln!("krill drop: {}: {error}", file.display());
                        failed = true;
                    }
                }
            }
            if json {
                println!("{}", json!({ "items": sent }));
            }
            if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Command::List { json } => {
            drop::prune(&state, SystemTime::now());
            let items = match drop::list(&state) {
                Ok(items) => items,
                Err(error) => {
                    eprintln!("krill drop: {error}");
                    return ExitCode::FAILURE;
                }
            };
            let inbox = krill_proto::os::home_dir().map(|home| drop::inbox_dir(&home));
            if json {
                let items: Vec<_> = items
                    .iter()
                    .map(|item| {
                        let ms = item
                            .added
                            .duration_since(UNIX_EPOCH)
                            .map_or(0, |at| u64::try_from(at.as_millis()).unwrap_or(u64::MAX));
                        json!({"id":item.id,"name":item.name,"size":item.size,"modified_unix_ms":ms})
                    })
                    .collect();
                println!("{}", json!({ "items": items, "inbox": inbox }));
            } else {
                for item in &items {
                    println!("{}\t{}\t{}", item.id, item.size, item.name);
                }
                if let Some(inbox) = inbox {
                    println!("inbox: {}", inbox.display());
                }
            }
            ExitCode::SUCCESS
        }
        Command::Remove { id } => match drop::remove(&state, &id) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("krill drop: {id}: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Help => ExitCode::SUCCESS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_send_list_remove_and_help() {
        assert_eq!(
            parse(&args(&["a.txt", "--json", "b c.pdf"])),
            Ok(Command::Send {
                files: vec!["a.txt".into(), "b c.pdf".into()],
                json: true
            })
        );
        assert_eq!(
            parse(&args(&["--", "ls"])),
            Ok(Command::Send {
                files: vec!["ls".into()],
                json: false
            })
        );
        assert_eq!(
            parse(&args(&["ls", "--json"])),
            Ok(Command::List { json: true })
        );
        assert_eq!(
            parse(&args(&["rm", "abc"])),
            Ok(Command::Remove { id: "abc".into() })
        );
        assert_eq!(parse(&args(&["--help"])), Ok(Command::Help));
        for bad in [
            &[][..],
            &["ls", "x"],
            &["rm"],
            &["rm", "a", "b"],
            &["--json"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
