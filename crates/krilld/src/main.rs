#![deny(clippy::indexing_slicing, clippy::print_stdout, clippy::print_stderr)]
//! `krilld <소켓 경로>`: 소켓을 게시하고 연결마다 스레드 하나로 [`Registry::serve`]를 돌린다.
//! 직접 실행 중 같은 경로로 이미 떠 있으면 0으로 끝낸다(MK-022). holder 아래에서 떴는데 건강한 데몬이
//! 소켓을 쥐고 있으면 중복 기동이라 전용 종료 코드([`daemon::KRILLD_DUPLICATE`])로 끝낸다(OWE-589).
//! holder 아래에서는 새로 게시한 listen 소켓을 holder에 맡기고,
//! 다시 뜨면 돌려받아 이어 쓴다(OWE-491).
//!
//! stdin이 소켓이면 holder가 띄운 것이다: holder 링크로 보관 세션을 돌려받고(`Ready`까지), 세션을 만들 수 있다.
//! 아니면(테스트·수동 실행) 세션 없이 Hello만 한다. 상태 디렉터리(세그먼트·테마·감사 로그)는 holder 아래에서만 쓴다.
//!
//! holder 재결합([`krilld::rejoin`]): holder 링크가 끊기면 고아로 계속 서빙하다가 다음 krilld에 세션을 넘기고 끝난다.
//! holder 아래에서 떴는데 소켓 락을 못 잡으면 고아 krilld에게서 세션을 넘겨받는다.
//!
//! Windows는 holder가 상속한 익명 파이프를 `Holder::connect_inherited`로 회수한다.

use std::io;
#[cfg(unix)]
use std::os::fd::AsFd;
#[cfg(windows)]
use std::os::windows::io::AsSocket;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::thread;

use krill_proto::daemon;
use krill_proto::ipc::{FEATURES, PROTO_MAX, PROTO_MIN, ServerHello};
use krilld::holder::Holder;
use krilld::ipc::Listener;
use krilld::registry::Registry;
use krilld::rejoin::{self, Handover};
#[cfg(unix)]
use rustix::fs::{FileType, fstat};

// 해제한 핫 스크롤백·sync 버퍼를 footprint로 돌려주는 할당기(OWE-477, 비공개 OWE-477 스파이크 기록).
#[cfg(not(windows))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> ExitCode {
    let Some(argument) = std::env::args_os().nth(1) else {
        return ExitCode::from(2);
    };
    // 앱 시작 점검은 소켓을 열지 않고 버전만 읽는다.
    if argument == "--version" {
        use std::io::Write as _;
        return match writeln!(io::stdout().lock(), "{}", krill_proto::BUILD) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    // 세션 Job/ConPTY 안에서 에이전트만 실행한다. 데몬 소켓·상태·스레드는 만들지 않는다.
    #[cfg(windows)]
    if argument == "--agent-exec" {
        return krilld::agent::run_windows_agent(std::env::args_os().skip(2));
    }
    #[cfg(windows)]
    if argument == "--shell-exec" {
        return krilld::run_windows_shell(std::env::args_os().skip(2));
    }
    #[cfg(windows)]
    if argument == "--console-members" {
        return std::env::args()
            .nth(2)
            .and_then(|pid| pid.parse().ok())
            .map_or(ExitCode::from(2), krilld::run_console_probe);
    }
    let path = PathBuf::from(argument);
    krill_log::init(krill_log::Component::Daemon).ok();
    tracing::info!("daemon starting");
    // 세션 panic은 `catch_unwind`가 잡지만(P4) 원인은 크래시 로그로 남긴다(MK-067).
    krill_proto::diag::install_panic_hook("krilld");
    let _error_run = daemon::state_dir().and_then(|state| {
        krill_proto::error_reporting::runs::RunGuard::start(
            &state,
            krill_proto::error_reporting::Process::Daemon,
        )
        .ok()
        .flatten()
    });
    // 실패해도 데몬은 뜬다: 해제 페이지가 늦게 돌아올 뿐 세션에는 영향이 없다.
    krilld::alloc::return_freed_pages().ok();
    krilld::alloc::mobile_thread().ok();
    // MK-066: 메모리 게이트가 의도적 회귀를 잡는지 확인하는 주입(`scripts/memory-gate.sh --inject`). 평소에는 빈 Vec.
    let _ballast = std::hint::black_box(ballast());
    #[cfg(unix)]
    let stdin = io::stdin();
    #[cfg(unix)]
    let linked = fstat(stdin.as_fd())
        .is_ok_and(|st| FileType::from_raw_mode(st.st_mode) == FileType::Socket);
    #[cfg(windows)]
    let linked = std::env::var_os("KRILL_HOLDER_LINK").is_some();
    let (holder, held) = if linked {
        #[cfg(unix)]
        let connection = stdin.as_fd().try_clone_to_owned().and_then(Holder::connect);
        #[cfg(windows)]
        let connection = Holder::connect_inherited();
        let Ok((holder, link, kept, held)) = connection else {
            return ExitCode::FAILURE; // holder가 링크를 끊고 다시 띄운다
        };
        (Some((holder, link, kept)), held)
    } else {
        (None, None)
    };
    // holder가 맡아 둔 소켓이 있으면 이어 쓴다(OWE-491). `Err`: 게시는 고아 krilld에게서 세션을 넘겨받은 뒤에 한다.
    let reused = held.is_some();
    let mut lis = match held.map_or_else(|| Listener::bind(&path), |fd| Listener::adopt(&path, fd))
    {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            match linked.then(|| Handover::take(&path)) {
                Some(Ok(h)) => Err(h),
                // 중복 기동: 건강한 데몬이 소켓을 쥐고 있고 인계 상대도 없다. holder가 재기동하지
                // 않게 전용 코드로 끝낸다. 직접 실행은 MK-022대로 조용히 0으로 끝낸다.
                Some(Err(_)) => return ExitCode::from(daemon::KRILLD_DUPLICATE),
                None => return ExitCode::SUCCESS,
            }
        }
        Err(error) => {
            tracing::error!(kind = ?error.kind(), "listener failed");
            krill_proto::error_reporting::capture(
                krill_proto::error_reporting::Process::Daemon,
                krill_proto::error_reporting::Code::DaemonListenerFailed,
                file!(),
                line!(),
            );
            return ExitCode::FAILURE;
        }
    };
    // 새로 게시하면(처음 기동, 인계 뒤) holder에 맡긴다.
    let fresh = !reused || lis.is_err();
    let mut adopt = Vec::new();
    let (registry, link) = if let Some((holder, link, mut kept)) = holder {
        if let Err(h) = &mut lis {
            let Ok(a) = h.merge(&mut kept) else {
                return ExitCode::FAILURE; // 고아 krilld가 세션을 계속 쥔다
            };
            adopt = a;
        }
        let dir = daemon::state_dir();
        (
            Registry::new(Some((holder, kept)), dir.as_deref()),
            Some(link),
        )
    } else {
        (Registry::new(None, None), None)
    };
    let registry = Arc::new(registry);
    // Recover interrupted speculative checkouts even if no creation dialog is opened again.
    krilld::git::worktree::recover_preparation();
    // 세션을 다음 krilld에 넘기면 holder 스레드가 알린다. 그 전에는 끝나지 않는다(`done`을 여기서도 쥔다).
    let (done, handed) = mpsc::channel();
    registry.exit_with(done.clone()); // 업그레이드(MK-062)
    if let Some(link) = link {
        let (r, path, done) = (Arc::clone(&registry), path.clone(), done.clone());
        let reader = thread::Builder::new()
            .name("holder".to_owned())
            .stack_size(256 * 1024)
            .spawn(move || {
                krilld::alloc::mobile_thread().ok();
                r.run_holder(link);
                if rejoin::orphan(&r, &path).is_ok() {
                    done.send(()).ok();
                }
            });
        if reader.is_err() {
            return ExitCode::FAILURE; // 답을 나눠 줄 리더가 없으면 holder 요청이 끝나지 않는다
        }
    }
    let r = Arc::clone(&registry);
    if thread::Builder::new()
        .name("agent-status".to_owned())
        .stack_size(256 * 1024)
        .spawn(move || {
            krilld::alloc::mobile_thread().ok();
            r.monitor_status();
        })
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    if linked {
        // 소켓 전달이 늦어 spool로 간 이벤트를 krilld가 다음에 재기동할 때까지 두지 않는다.
        // 한 번의 panic으로 수거가 영구 정지하지 않게 한 바퀴씩 격리한다(K-06).
        let r = Arc::clone(&registry);
        thread::Builder::new()
            .name("spool".to_owned())
            .stack_size(256 * 1024)
            .spawn(move || {
                krilld::alloc::mobile_thread().ok();
                loop {
                    thread::sleep(krilld::status::SPOOL_POLL);
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        r.drain_spool();
                    }))
                    .is_err()
                    {
                        tracing::warn!("spool drain panicked; continuing");
                    }
                }
            })
            .ok();
    }
    let lis = match lis {
        Ok(l) => l,
        Err(h) => {
            if rejoin::adopt(&registry, adopt).is_err() {
                return ExitCode::FAILURE; // old daemon retains ownership until every Adopt is acknowledged
            }
            match h.finish(&path) {
                Ok(l) => l,
                // 세션은 holder가 맡았다. holder가 다음 krilld에 다시 넘긴다.
                Err(_) => return ExitCode::FAILURE,
            }
        }
    };
    // 클라이언트를 받기 전에 맡겨야 세션을 가진 holder는 늘 소켓도 쥔다. 링크가 끊겼으면 고아 운영(rejoin)이다.
    // ponytail: 첫 게시와 `HoldListener` 사이에 죽으면 그 holder는 소켓 없이 남아 연결이 거부되고 holder가 하나 더
    // 뜰 수 있다. 세션을 만들기 전이라 갈라질 세션은 없다. 막아야 하면 holder 자체 flock을 더한다.
    if fresh && let Some(h) = registry.holder() {
        #[cfg(unix)]
        let fd = lis.as_fd();
        #[cfg(windows)]
        let fd = lis.as_socket();
        h.hold_listener(fd).ok();
    }
    krilld::monitor::start(Arc::clone(&registry), done.clone()).ok();
    if linked {
        let r = Arc::clone(&registry);
        thread::Builder::new()
            .name("scheduler".to_owned())
            .stack_size(256 * 1024)
            .spawn(move || {
                krilld::alloc::mobile_thread().ok();
                loop {
                    // 한 번의 panic으로 스케줄러가 영구 정지하지 않게 격리한다(K-06).
                    // `poll_schedules`의 오류는 이미 안에서 처리하고 `io::Result`로만 나온다.
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        r.poll_schedules(std::time::SystemTime::now())
                    })) {
                        Err(_) => tracing::warn!("schedule poll panicked; continuing"),
                        Ok(Err(error)) => {
                            tracing::warn!(kind = ?error.kind(), "schedule poll failed");
                            krill_proto::error_reporting::capture(
                                krill_proto::error_reporting::Process::Daemon,
                                krill_proto::error_reporting::Code::SchedulerFailed,
                                file!(),
                                line!(),
                            );
                        }
                        Ok(Ok(())) => {}
                    }
                    thread::sleep(std::time::Duration::from_secs(1));
                }
            })
            .ok();
    }
    if linked {
        // OWE-702: 앱이 꺼져 있어도 자동전환을 평가한다. 한 바퀴씩 격리한다(K-06).
        let r = Arc::clone(&registry);
        thread::Builder::new()
            .name("usage-tick".to_owned())
            .stack_size(256 * 1024)
            .spawn(move || {
                krilld::alloc::mobile_thread().ok();
                let mut seen = None;
                let mut quota_tick = std::time::Instant::now();
                loop {
                    r.wait_account_work(
                        krilld::registry::usage_tick::TICK.saturating_sub(quota_tick.elapsed()),
                    );
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if quota_tick.elapsed() >= krilld::registry::usage_tick::TICK {
                            r.tick_accounts(&mut seen);
                            quota_tick = std::time::Instant::now();
                        }
                        r.process_account_work();
                    }))
                    .is_err()
                    {
                        seen = None;
                        tracing::warn!("usage tick panicked; continuing");
                    }
                }
            })
            .ok();
    }
    tracing::info!(linked, reused, "daemon accepting clients");
    let hello = ServerHello {
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        features: FEATURES,
        build: krill_proto::BUILD.to_owned(),
        instance_id: u64::from(std::process::id()),
    };
    if !owns_mobile(&path) {
        tracing::info!("mobile gateway skipped: socket is not the state dir default");
    } else if let Err(error) = registry.start_mobile(hello.clone()) {
        tracing::warn!(kind=?error.kind(), "mobile gateway unavailable");
    }
    let accept = thread::Builder::new()
        .name("accept".to_owned())
        .stack_size(256 * 1024)
        .spawn(move || {
            krilld::alloc::mobile_thread().ok();
            // 여기서 닫는 연결은 Hello 없이 끝나 클라이언트에는 EOF로만 보인다. 상태가 바뀔 때만 남긴다.
            let mut rejected = None;
            let mut spawn_error = None;
            let mut full = 0u64;
            loop {
                let s = match lis.accept() {
                    Ok(s) => s,
                    Err(error) => {
                        if rejected.replace(error.kind()) != Some(error.kind()) {
                            tracing::warn!(kind = ?error.kind(), "connection closed before hello");
                        }
                        continue;
                    }
                };
                rejected = None;
                // Bound unclassified connection threads without counting existing
                // attach/watch/wait clients or the dedicated holder connection.
                let Some(permit) = krilld::registry::admission::handshake() else {
                    if full == 0 {
                        tracing::warn!("handshake admission full; closing new connections");
                    }
                    full = full.saturating_add(1);
                    continue;
                };
                if full > 0 {
                    tracing::info!(closed = full, "handshake admission recovered");
                    full = 0;
                }
                let (hello, r) = (hello.clone(), Arc::clone(&registry));
                match thread::Builder::new()
                    .name("conn".to_owned())
                    .stack_size(256 * 1024)
                    .spawn(move || {
                        krilld::alloc::mobile_thread().ok();
                        r.serve_admitted(s, &hello, permit).ok()
                    })
                {
                    Ok(_) => spawn_error = None,
                    Err(error) => {
                        if spawn_error.replace(error.kind()) != Some(error.kind()) {
                            tracing::warn!(kind = ?error.kind(), "could not start connection handler");
                        }
                    }
                }
            }
        });
    if accept.is_err() {
        return ExitCode::FAILURE;
    }
    handed.recv().ok();
    drop(done);
    tracing::info!("daemon handover complete");
    ExitCode::SUCCESS
}

/// 상태 폴더의 기본 소켓(`<상태 폴더>/run/krilld.sock`, [`daemon::Paths::dev`])으로 뜬 데몬만 모바일 게이트웨이를 켠다.
/// 다른 소켓으로 뜬 데몬(테스트·수동 실행)이 같은 상태 폴더의 `mobile-gateway.json`을 쓰면
/// 실사용 데몬의 터널을 죽이고 같은 릴레이 ID로 붙는다.
fn owns_mobile(socket: &Path) -> bool {
    daemon::state_dir()
        .is_some_and(|dir| daemon::socket_path(&dir).is_ok_and(|path| path == socket))
}

/// 환경 변수 `KRILL_TEST_BALLAST_MB`(메비바이트 단위)만큼을 0이 아닌 값으로 채워 쥔다(dirty 페이지라 footprint에 잡힌다). 없거나 잘못된 값이면 빈 Vec.
fn ballast() -> Vec<u8> {
    std::env::var("KRILL_TEST_BALLAST_MB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .and_then(|mb| mb.checked_mul(1 << 20))
        .map(|n| vec![1; n])
        .unwrap_or_default()
}
