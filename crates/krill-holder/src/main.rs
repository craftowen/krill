#![deny(clippy::indexing_slicing, clippy::print_stdout, clippy::print_stderr)]

#[cfg(unix)]
use std::fs::File;
use std::io;
use std::process::ExitCode;

use krill_proto::daemon::DETACH_ARG;

/// `krill-holder [--detach] <krilld 경로> [krilld 인자…]`
fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1).peekable();
    if args.next_if(|a| a == DETACH_ARG).is_some() && detach().is_err() || args.peek().is_none() {
        return ExitCode::FAILURE;
    }
    match krill_holder::run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(kind = ?error.kind(), "holder stopped");
            ExitCode::FAILURE
        }
    }
}

/// `--detach`: 개발 빌드에서 앱·`krill`가 직접 띄울 때(MK-025). 새 세션을 만들어 띄운 쪽의 제어 터미널·프로세스
/// 그룹에서 떨어지고 stdio를 `/dev/null`로 돌린다. launchd가 띄우는 경로(MK-106 plist)에는 이 인자가 없다 —
/// launchd 잡은 자기 setsid 금지(CONVENTIONS A1). exec 핸드오프는 이 인자를 다시 넘기지 않는다(이미 세션
/// 리더라 setsid가 실패한다). 띄운 쪽에게서 새어 들어온 fd 3 이상은 먼저 닫는다(OWE-485).
#[cfg(unix)]
fn detach() -> io::Result<()> {
    krill_holder::close_inherited()?;
    rustix::process::setsid()?;
    let null = File::options().read(true).write(true).open("/dev/null")?;
    rustix::stdio::dup2_stdin(&null)?;
    rustix::stdio::dup2_stdout(&null)?;
    rustix::stdio::dup2_stderr(&null)?;
    Ok(())
}

/// Windows: 콘솔 분리는 띄운 쪽의 `DETACHED_PROCESS` 생성 플래그가 맡는다(OWE-829).
#[cfg(windows)]
fn detach() -> io::Result<()> {
    krill_holder::close_inherited()
}
