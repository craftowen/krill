//! Default-enabled app usage analytics controls and bounded background uploader.
mod transport;
use krill_proto::{
    daemon,
    telemetry::{self, Env, Outcome, Status},
};
use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage: krill telemetry status|enable|disable|flush [--json]";
pub fn run(args: &[OsString]) -> ExitCode {
    let args: Vec<_> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h" | "help"))
    {
        println!(
            "{USAGE}\nEnabled on install. Disable deletes the pending queue. DO_NOT_TRACK, KRILL_TELEMETRY_DISABLED and CI override enable.\nOnly install ID, event ID, app version, OS, architecture, UTC timestamp and first_open/app_opened/active_day are sent.\nError reporting remains separately opt-in (`krill errors`)."
        );
        return ExitCode::SUCCESS;
    }
    let Some(command) = args.first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if !matches!(command.as_str(), "status" | "enable" | "disable" | "flush")
        || args.iter().skip(1).any(|arg| arg != "--json")
    {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let json = args.iter().any(|arg| arg == "--json");
    let Some(state) = daemon::state_dir() else {
        eprintln!("telemetry: state directory unavailable");
        return ExitCode::FAILURE;
    };
    match execute(&state, command, json) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("telemetry: {error}");
            ExitCode::FAILURE
        }
    }
}
fn execute(state: &Path, command: &str, json: bool) -> io::Result<ExitCode> {
    match command {
        "enable" => telemetry::enable(state)?,
        "disable" => telemetry::disable(state)?,
        "flush" => {
            let endpoint = std::env::var(telemetry::ENDPOINT_VAR)
                .unwrap_or_else(|_| telemetry::ENDPOINT.to_owned());
            let endpoint = transport::Endpoint::parse(&endpoint).map_err(io::Error::other)?;
            let curl = transport::system_curl();
            let report =
                telemetry::flush_with(state, &Env::live(), telemetry::now_unix(), |body| {
                    match transport::post(&curl, &endpoint, body, transport::DEADLINE) {
                        transport::Reply::Http { status, body } => Ok((status, body)),
                        transport::Reply::Transport => Err(io::Error::other("upload failed")),
                    }
                })?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&report).map_err(io::Error::other)?
                );
            } else {
                println!(
                    "telemetry  {:?}; accepted {}, pending {}",
                    report.outcome, report.accepted, report.pending
                );
            }
            return Ok(if report.outcome == Outcome::Deferred {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            });
        }
        _ => {}
    }
    let status = telemetry::status_with(state, &Env::live())?;
    print_status(&status, json)?;
    Ok(ExitCode::SUCCESS)
}
fn print_status(status: &Status, json: bool) -> io::Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(status).map_err(io::Error::other)?
        );
    } else {
        println!(
            "telemetry  {}; pending {}; last delivery {:?}; last success {}; next retry {}{}",
            if status.enabled {
                "on (default; stop with `krill telemetry disable`)"
            } else {
                "off"
            },
            status.pending,
            status.last_outcome,
            status
                .last_success_unix
                .map_or_else(|| "never".to_owned(), |value| value.to_string()),
            status
                .next_attempt_unix
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            status
                .veto
                .as_ref()
                .map_or_else(String::new, |var| format!("; veto {var}"))
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests;
