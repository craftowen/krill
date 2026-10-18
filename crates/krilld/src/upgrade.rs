//! krilld 업그레이드(MK-062, ARCHITECTURE §3 "데몬 업그레이드"): 세대 공존 없이 holder가 새 바이너리로 krilld를
//! 다시 띄운다. 그래서 업데이트 뒤 데몬 세대는 늘 하나다(고아 데몬 0).
//!
//! 순서([`krill_proto::control::Request::Upgrade`]):
//! 1. 요청 빌드가 이 krilld보다 새것이 아니면 아무것도 하지 않는다(다운그레이드 금지).
//! 2. `UpgradeHolder` — holder가 새 바이너리로 exec 핸드오프(PID·세션·listen 소켓 유지). 실패하면 그대로 멈춘다.
//! 3. `UpgradeKrilld` — 다음 krilld 경로를 바꾸고, 다음 종료를 크래시로 세지 않게 한다(백오프 없이 즉시 재기동).
//! 4. PTY 리더를 멈추고 세그먼트를 봉인한다. 읽지 않은 출력은 커널 PTY 버퍼에 남아 새 krilld가 읽는다
//!    (무손실, ARCHITECTURE §4.1 'krilld 업그레이드'). 그 뒤 답하고 끝난다([`crate::registry::Registry::serve`]).
//!
//! 2·3 사이에 실패하면 새 holder + 구 krilld로 계속 돈다(holder 프로토콜은 추가만 하므로 호환된다).
//!
//! 잠정 결정 의존: OWE-389(MK-024 세그먼트 저장소). 무손실 재생은 4의 세그먼트 봉인과 새 krilld의 이어 기록에
//! 기댄다. 거부되면 이 모듈과 `registry.rs`의 `Request::Upgrade` 분기, `tests/upgrade.rs`의 처음부터 재생 검사를 고친다.

use krill_proto::os::ffi::OsStrExt as _;
use std::ffi::OsStr;
use std::path::Path;

use krill_proto::control::{Failure, Upgrade, Upgraded, is_newer};
use krill_proto::holder::{FromHolder, ToHolder};

use crate::holder::{Answer, Holder};
use crate::registry::Registry;

/// 이 krilld의 빌드. `ServerHello.build`와 같다.
const BUILD: &str = krill_proto::BUILD;

/// 업그레이드를 준비한다. `Started`면 호출자가 답을 보낸 뒤 프로세스를 끝낸다.
pub(crate) fn prepare(reg: &Registry, holder: &Holder, u: Upgrade) -> Result<Upgraded, Failure> {
    match is_newer(&u.build, BUILD) {
        None => return Err(Failure::BadRequest),
        Some(false) => return Ok(Upgraded::NotNewer),
        Some(true) => {}
    }
    for exe in [&u.holder, &u.krilld] {
        krill_proto::daemon::preflight(Path::new(OsStr::from_bytes(exe)))
            .map_err(|e| Failure::Os(e.raw_os_error().unwrap_or(libc::EIO)))?;
    }
    // OWE-684: exec 핸드오프 뒤 holder가 Done 없이 죽으면(좀비·SIGKILL) 링크가 끊겨
    // 리더가 `pending`을 비우고 기다리던 요청이 `NotConnected`로 끝난다. 성공으로
    // 오인하지 않고 실패로 보고한다. 구 krilld는 master fd를 쥔 채 계속 서빙하고,
    // 감시 연결에는 `Orphan`이 간다(앱 기존 재연결·알림 경로).
    if let Err(e) = done(&holder.upgrade_holder(u.holder)) {
        if !holder.is_connected() {
            tracing::warn!("upgrade: holder lost during exec handoff; keeping old krilld");
        }
        return Err(e);
    }
    done(&holder.request(|req| ToHolder::UpgradeKrilld {
        req,
        path: u.krilld,
    }))?;
    // 멈추지 못한 리더가 있어도 끝낸다: 그 세션은 krilld 크래시 범위(Gap 가능)로 이어진다.
    if let Err(e) = crate::rejoin::halt_all(reg) {
        tracing::warn!(kind = ?e.kind(), "upgrade: readers did not halt");
    }
    // Done 뒤·정지 중에 holder가 죽었으면 `Started`를 보내고 끝나면 세션이 유실된다
    // (새 holder를 띄울 주체가 없다). 실패로 보고하고 구 krilld가 계속 쥔다.
    // 판정과 종료 사이의 수 μs 경합은 핸드오프 재설계(needs-owen a) 없이 막지 못한다.
    if !holder.is_connected() {
        tracing::warn!("upgrade: holder lost before handover completed; keeping old krilld");
        return Err(Failure::NoHolder);
    }
    tracing::info!(build = %u.build, "upgrade: handing over to holder");
    Ok(Upgraded::Started)
}

fn done(answer: &std::io::Result<Answer>) -> Result<(), Failure> {
    match answer {
        Ok((FromHolder::Done { .. }, _)) => Ok(()),
        Ok((FromHolder::Failed { errno, .. }, _)) => Err(Failure::Os(*errno)),
        Ok(_) | Err(_) => Err(Failure::NoHolder),
    }
}
