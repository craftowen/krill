//! Computer-use 격리 워커(M13 실험, 설계 C′). 같은 앱 실행 파일을 `--computer-worker`로 띄우면 GPUI 초기화 전에
//! stdin/stdout 프레임 서버(`krill-computer-worker/1`)로 동작한다.
//!
//! - CLI의 기본 엔진이다. 호출마다 한 번 띄우고, 워커에 없는 동작은 CLI가 동작 단위로 JXA에 넘긴다.
//!   같은 실행 파일이라는 이유로 Krill.app의 TCC 허용을 물려받는다고 가정하지 않는다(`permissions` op가
//!   이 프로세스 자신의 상태를 보고한다).
//! - GUI·AX 작업은 이 프로세스에만 있다. holder·daemon에는 아무것도 추가하지 않는다.
//! - `unsafe`는 `ffi.rs`에만 있다. 나머지(프로토콜·장부·잠금·정책·AX 실행기)는 safe 코드다.
//!
//! 인자: `--computer-worker` (필수), `--computer-protect-pid=<pid>` (반복 가능, 대상에서 제외할 제어 프로세스).
//!
//! 종료 코드: 0 정상(EOF·`shutdown`), 1 초기화 실패, 2 프로토콜 오류(상한 초과·JSON 아님), 3 출력 전송로 실패
//! (호출자가 stdout을 읽지 않아 상한을 넘었거나 쓰기가 정체·실패). 3이면 마지막 줄이 잘렸을 수 있다.

use std::process::ExitCode;

#[cfg(target_os = "macos")]
mod ax;
#[cfg(target_os = "macos")]
mod ffi;
#[cfg(target_os = "macos")]
mod ledger;
#[cfg(target_os = "macos")]
mod lock;
#[cfg(target_os = "macos")]
mod protocol;
#[cfg(target_os = "macos")]
mod worker;

pub const WORKER_FLAG: &str = "--computer-worker";
pub const PROTECT_PID_PREFIX: &str = "--computer-protect-pid=";

/// 인자에 [`WORKER_FLAG`]가 있으면 워커를 돌리고 종료 코드를 돌려준다. 없으면 부작용 없이 `None`.
/// 루트 `main`이 GPUI·로거·업데이터보다 먼저 부른다.
pub fn maybe_run_worker() -> Option<ExitCode> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if !args.iter().any(|arg| arg == WORKER_FLAG) {
        return None;
    }
    Some(ExitCode::from(run(&args)))
}

#[cfg(target_os = "macos")]
fn random_hex() -> Option<String> {
    use ring::rand::{SecureRandom as _, SystemRandom};
    use std::fmt::Write as _;
    let mut bytes = [0u8; 16];
    SystemRandom::new().fill(&mut bytes).ok()?;
    let mut hex = String::with_capacity(32);
    for byte in bytes {
        write!(hex, "{byte:02x}").ok()?;
    }
    Some(hex)
}

#[cfg(target_os = "macos")]
fn run(args: &[std::ffi::OsString]) -> u8 {
    let mut protected = Vec::new();
    for arg in args {
        let Some(raw) = arg
            .to_str()
            .and_then(|arg| arg.strip_prefix(PROTECT_PID_PREFIX))
        else {
            continue;
        };
        match raw.parse::<i32>() {
            Ok(pid) if pid > 0 => protected.push(pid),
            _ => {
                eprintln!("krill-app: invalid {PROTECT_PID_PREFIX}{raw}");
                return 2;
            }
        }
    }
    let (Some(epoch), Some(context)) = (random_hex(), random_hex()) else {
        eprintln!("krill-app: computer worker could not create an identity");
        return 1;
    };
    // stdout은 버퍼 없는 fd 사본으로 쓴다. 프레임 하나를 다 쓰면 파이프에 들어간 것이고, 종료 때 표준 라이브러리가
    // 버퍼에 남은 조각을 뒤늦게 내보내는 일이 없다.
    let Ok(stdout) = std::os::fd::AsFd::as_fd(&std::io::stdout()).try_clone_to_owned() else {
        eprintln!("krill-app: computer worker could not open stdout");
        return 1;
    };
    let identity = worker::Identity { epoch, context };
    // TCC와 Process Manager는 프로세스의 첫 호출만 느리다(약 10 ms씩). 검사를 대신하지 않고 첫 요청을 덜 기다리게 한다.
    std::thread::spawn(|| {
        crate::platform::permissions::probe_accessibility();
        ffi::foreground_pid().ok();
    });
    let backend = ax::MacBackend::new(identity.clone(), protected);
    worker::serve(
        std::io::BufReader::new(std::io::stdin()),
        std::fs::File::from(stdout),
        identity,
        worker::Config {
            lock_path: lock::default_lock_path(),
            write_stall: worker::WRITE_STALL,
        },
        backend,
    )
}

/// macOS 밖: 지원하지 않음을 알리고 아무 입력도 받지 않고 끝낸다.
#[cfg(not(target_os = "macos"))]
fn run(_args: &[std::ffi::OsString]) -> u8 {
    use std::io::Write as _;
    let hello = serde_json::json!({
        "type": "hello",
        "contract": "krill-computer-worker/1",
        "supported": false,
        "capabilities": [],
        "unsupported": ["all"],
    });
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{hello}").is_err() || out.flush().is_err() {
        return 1;
    }
    0
}
