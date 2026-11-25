//! Krill 앱 실행 파일 진입점.
//!
//! 버전·빌드 식별자 등록, `--list` 가드, 패닉 훅, `--crash-test`(MK-067)만 이 크레이트에 두고
//! 실제 GPUI 셸·창 수명주기는 고정 버전(`0.0.0`) 라이브러리 크레이트 `krill-app-shell`에 위임한다.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    krill_proto::init_build_info(krill_version::VERSION, krill_version::BUILD);
    // 테스트 실행 파일 탐색이 일반 앱 산출물을 골라도 운영 데몬에 붙지 않는다.
    if std::env::args_os().skip(1).any(|arg| arg == "--list") {
        eprintln!("krill-app: --list requires a test executable");
        return std::process::ExitCode::FAILURE;
    }
    krill_proto::diag::install_panic_hook("krill-app");
    if std::env::args().any(|a| a == "--crash-test") {
        crash_test();
    }
    krill_app_shell::run()
}

#[expect(
    clippy::panic,
    reason = "MK-067: 크래시 로그·진단 대화상자 e2e용 강제 panic"
)]
fn crash_test() {
    panic!("MK-067 crash test (--crash-test)");
}
