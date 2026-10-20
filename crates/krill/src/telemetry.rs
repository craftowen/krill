//! `krill telemetry status|enable|disable`(MK-089): 텔레메트리 opt-in 동의 파일 관리.
//! 상태 디렉터리의 동의 파일만 읽고 쓴다. 네트워크를 쓰지 않는다.

use std::ffi::OsString;
use std::process::ExitCode;

use krill_proto::daemon;
use krill_proto::telemetry::{self, Consent, Reason};

const USAGE: &str = "usage: krill telemetry status|enable|disable";

pub fn run(args: &[OsString]) -> ExitCode {
    // 서브커맨드 자리만 도움말로 본다(status|enable|disable 고정이라 값 오인 없음).
    let help_here = matches!(
        args.first().and_then(|a| a.to_str()),
        Some("-h" | "--help" | "help")
    ) || matches!(args.get(1).and_then(|a| a.to_str()), Some("-h" | "--help"));
    if help_here {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let args: Vec<String> = args
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["status"] => status(),
        ["enable"] => flip(true),
        ["disable"] => flip(false),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn status() -> ExitCode {
    let Some(state) = daemon::state_dir() else {
        println!("telemetry  off (no state directory; nothing is sent)");
        return ExitCode::SUCCESS;
    };
    match telemetry::consent(Some(&state), &telemetry::Env::live()) {
        Consent::Enabled => {
            println!("telemetry  on (opt-in; stop with `krill telemetry disable`)");
        }
        Consent::Disabled(Reason::NoConsentFile) => {
            println!("telemetry  off (default; start with `krill telemetry enable`)");
        }
        Consent::Disabled(Reason::EnvVeto(var)) => {
            println!("telemetry  off ({var} is set; nothing is sent)");
        }
    }
    ExitCode::SUCCESS
}

fn flip(want_on: bool) -> ExitCode {
    let Some(state) = daemon::state_dir() else {
        eprintln!("telemetry: HOME is not set");
        return ExitCode::FAILURE;
    };
    let result = if want_on {
        telemetry::enable(&state)
    } else {
        telemetry::disable(&state)
    };
    if let Err(e) = result {
        eprintln!("telemetry: {e}");
        return ExitCode::FAILURE;
    }
    if want_on
        && let Some(var) = telemetry::consent(Some(&state), &telemetry::Env::live()).veto_var()
    {
        println!("telemetry  off ({var} is set; consent saved and takes effect once it is unset)");
    } else {
        return status();
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn help_returns_zero() {
        assert_eq!(run(&[OsString::from("--help")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("-h")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("help")]), ExitCode::SUCCESS);
    }
}
