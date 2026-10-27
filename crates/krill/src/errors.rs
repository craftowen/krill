//! `krill errors status|enable|disable|flush [--json]`: 오류 보고 opt-in과 업로드.
//! 동의·대기열은 `krill_proto::error_reporting`이 다루고, 업로드만 OS curl([`transport`])로 HTTPS POST한다.
//! 텔레메트리 동의와 별개다. 앱이 주기적으로 `flush`를 실행하고, 앱 없이 쓰는 환경은 직접 실행한다.

mod transport;

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::ExitCode;

use krill_proto::daemon;
use krill_proto::error_reporting::{
    self as reporting, Env, FlushOutcome, FlushReport, Reply, Status, UploadLock,
};

const USAGE: &str = "usage: krill errors status|enable|disable|flush [--json]";

/// 동의 범위(도움말과 `enable` 결과에 그대로 보인다).
const SCOPE: &str = "\
error reporting is opt-in and separate from `krill telemetry`.
  sent:     error code, public source location (crates/krill*/src file:line), Krill version and build,
            OS, CPU architecture, UI stall duration bucket, native crash exception and signal type
  not sent: messages, prompts, terminal output, file paths, account or host names, raw logs or crash reports
  upload:   the Krill app runs `krill errors flush` periodically; without the app, run it yourself
  stop:     `krill errors disable` (also deletes the local queue)";

const FLAGS: &str = "\
  --json            print the status or flush result as JSON
  --endpoint <url>  flush only: https URL, or http on 127.0.0.1/localhost/[::1] for local testing";

pub fn run(args: &[OsString]) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    let help = matches!(
        args.first().map(String::as_str),
        Some("-h" | "--help" | "help")
    ) || args
        .iter()
        .skip(1)
        .any(|a| matches!(a.as_str(), "-h" | "--help"));
    if help {
        println!("{USAGE}\n{FLAGS}\n{SCOPE}");
        return ExitCode::SUCCESS;
    }
    let Some((command, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let mut json = false;
    let mut endpoint = None;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--endpoint" if command == "flush" => {
                let Some(url) = it.next() else {
                    eprintln!("--endpoint needs a value\n{USAGE}");
                    return ExitCode::from(2);
                };
                endpoint = Some(url.as_str());
            }
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    if !matches!(command.as_str(), "status" | "enable" | "disable" | "flush") {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let Some(state) = daemon::state_dir() else {
        eprintln!("errors: HOME is not set");
        return ExitCode::FAILURE;
    };
    match command.as_str() {
        "enable" => show(&state, reporting::enable(&state), true, json),
        "disable" => show(&state, reporting::disable(&state), false, json),
        "flush" => match transport::Endpoint::parse(endpoint.unwrap_or(reporting::ENDPOINT)) {
            Ok(endpoint) => flush(&state, &endpoint, json),
            Err(e) => {
                eprintln!("errors: {e}");
                ExitCode::from(2)
            }
        },
        _ => show(&state, Ok(()), false, json),
    }
}

/// 바꾼 뒤(또는 그대로) 현재 상태를 보인다. 동의 직후에는 범위를 함께 보인다.
fn show(state: &Path, changed: io::Result<()>, scope: bool, json: bool) -> ExitCode {
    match changed.and_then(|()| reporting::status(state)) {
        Ok(status) => {
            print_status(&status, json);
            if scope && !json {
                println!("{SCOPE}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("errors: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_status(status: &Status, json: bool) {
    if json {
        println!("{}", serde_json::to_string(status).unwrap_or_default());
        return;
    }
    let line = match (&status.veto, status.consented, status.enabled) {
        (Some(var), true, _) => {
            format!("off ({var} is set; consent saved and takes effect once it is unset)")
        }
        (Some(var), false, _) => format!("off ({var} is set; nothing is recorded or sent)"),
        (None, _, true) => {
            let retry = status
                .next_attempt_at_ms
                .map(|at| format!("; retrying in {}s", seconds_until(at)))
                .unwrap_or_default();
            format!(
                "on (opt-in; {} pending{retry}; stop with `krill errors disable`)",
                status.pending
            )
        }
        (None, _, false) => "off (default; start with `krill errors enable`)".to_owned(),
    };
    println!("error reporting  {line}");
}

fn seconds_until(at_ms: u64) -> u64 {
    at_ms.saturating_sub(reporting::now_ms()).div_ceil(1000)
}

fn flush(state: &Path, endpoint: &transport::Endpoint, json: bool) -> ExitCode {
    let curl = transport::system_curl();
    let result = run_flush(state, &Env::live(), reporting::native::collect, |body| {
        transport::post(&curl, endpoint, body, transport::DEADLINE)
    });
    let report = match result {
        Ok(report) => report,
        Err(e) => {
            eprintln!("errors: {e}");
            return ExitCode::FAILURE;
        }
    };
    if json {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        println!("error reporting  {}", describe(&report));
    }
    if report.outcome == FlushOutcome::Deferred {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// 동의 확인 → 업로드 single-flight 잠금 → native 수집(잠금 안에서 한 번) → 대기열 전송.
/// 동의가 없거나 다른 업로드가 돌면 수집·전송 없이 끝난다. native 수집 실패는 전송을 막지 않는다.
fn run_flush(
    state: &Path,
    env: &Env,
    collect: impl FnOnce(&Path) -> io::Result<usize>,
    transport: impl FnMut(&[u8]) -> Reply,
) -> io::Result<FlushReport> {
    let Some(consent) = reporting::consent_with(state, env)? else {
        return Ok(FlushReport {
            outcome: FlushOutcome::NotConsented,
            pending: reporting::status_with(state, env)?.pending,
            ..FlushReport::default()
        });
    };
    let Some(flight) = UploadLock::try_acquire(state)? else {
        return Ok(FlushReport {
            outcome: FlushOutcome::Busy,
            pending: reporting::status_with(state, env)?.pending,
            ..FlushReport::default()
        });
    };
    if let Err(error) = collect(state) {
        tracing::warn!(%error, "error reporting: native report collection skipped");
    }
    reporting::flush(state, &consent, &flight, transport)
}

fn describe(report: &FlushReport) -> String {
    let retry = report
        .next_attempt_at_ms
        .map(|at| format!("; retrying in {}s", seconds_until(at)))
        .unwrap_or_default();
    let pending = report.pending;
    match report.outcome {
        FlushOutcome::Empty => format!("nothing to send ({pending} pending)"),
        FlushOutcome::Sent | FlushOutcome::Deferred => {
            let failed = if report.outcome == FlushOutcome::Deferred {
                "; upload incomplete"
            } else {
                ""
            };
            format!(
                "sent {} new, {} already received, {} rejected, {} dropped locally{failed}; {pending} pending{retry}",
                report.accepted, report.duplicates, report.rejected, report.dropped
            )
        }
        FlushOutcome::Backoff => format!("waiting to retry; {pending} pending{retry}"),
        FlushOutcome::Busy => format!("another upload is running ({pending} pending)"),
        FlushOutcome::NotConsented => "off; nothing sent".to_owned(),
        FlushOutcome::Withdrawn => "consent changed during upload; nothing more sent".to_owned(),
    }
}

#[cfg(test)]
mod tests;
