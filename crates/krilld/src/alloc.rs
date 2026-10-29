//! 전역 할당기 설정(OWE-477). krilld는 jemalloc을 쓰고(`main.rs`의 `#[global_allocator]`), 해제한 페이지를
//! decay 없이 바로 OS에 돌려준다. macOS 기본 malloc은 강등으로 해제한 200열 행(4,800B)을 footprint로 돌려주지 않았고,
//! jemalloc 기본 decay(10초)는 할당이 멈춘 유휴 데몬에서 진행되지 않았다(실측, 비공개 OWE-477 스파이크 기록).
//! Windows는 jemalloc-sys가 빌드되지 않아 시스템 할당기(프로세스 힙)를 쓰고, 아래 함수는 할 일이 없다(OWE-819).
//! 4,800B 행 2만 개(사적 커밋 97.9MiB)를 모두 해제하면 기다림 없이 1.2~1.9MiB로 내려갔다(합성 측정, Server 2022,
//! docs/research/windows-spike-OWE-833.md). 실제 krilld 세션 강등의 반납은 미측정.

#[cfg(windows)]
use std::io::Error;
#[cfg(not(windows))]
use tikv_jemalloc_ctl::{Access, AsName, Error};

/// 이후 해제되는 페이지를 즉시 OS에 돌려주도록 jemalloc decay를 0으로 둔다. `main` 첫 줄, 스레드를 띄우기 전에 부른다.
///
/// 새 아레나는 `arenas.*` 기본값을, 이미 있는 아레나 0(메인 스레드)은 `arena.0.*`를 따른다.
///
/// # Errors
///
/// jemalloc이 mallctl 쓰기를 거부하면 그 오류.
#[cfg(not(windows))]
pub fn return_freed_pages() -> Result<(), Error> {
    for key in [
        b"arenas.dirty_decay_ms\0".as_slice(),
        b"arenas.muzzy_decay_ms\0",
        b"arena.0.dirty_decay_ms\0",
        b"arena.0.muzzy_decay_ms\0",
    ] {
        key.name().write(0_isize)?;
    }
    Ok(())
}

#[cfg(windows)]
#[expect(clippy::unnecessary_wraps, reason = "jemalloc 판과 같은 시그니처")]
pub fn return_freed_pages() -> Result<(), Error> {
    Ok(())
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn decay_is_zero_after_setup() {
        return_freed_pages().unwrap();
        let dirty: isize = b"arenas.dirty_decay_ms\0".name().read().unwrap();
        let muzzy: isize = b"arena.0.muzzy_decay_ms\0".name().read().unwrap();
        assert_eq!((dirty, muzzy), (0, 0));
    }
}

/// Mobile uses short-lived RPC workers and a small number of long-lived stream workers.
/// Share arena 0 and bypass per-thread caches so reconnects/history do not retain arenas
/// proportional to the number of worker threads. This changes this calling thread only.
#[cfg(not(windows))]
pub fn mobile_thread() -> Result<(), Error> {
    b"thread.arena\0".name().write(0_u32)?;
    b"thread.tcache.enabled\0".name().write(false)
}

#[cfg(windows)]
#[expect(clippy::unnecessary_wraps, reason = "jemalloc 판과 같은 시그니처")]
pub fn mobile_thread() -> Result<(), Error> {
    Ok(())
}

/// 일시적 대형 작업(콜드 세그먼트 zstd 재생·세션 강등) 뒤 jemalloc arena 0과 시스템 malloc 캐시를 비운다.
#[cfg(not(windows))]
pub(crate) fn purge() {
    for key in [
        b"arena.0.dirty_decay_ms\0".as_slice(),
        b"arena.0.muzzy_decay_ms\0",
    ] {
        key.name().write(0_isize).ok();
    }
    crate::sys::release_malloc_cache();
}

#[cfg(windows)]
pub(crate) fn purge() {
    crate::sys::release_malloc_cache();
}

#[cfg(all(test, not(windows)))]
mod mobile_tests {
    use super::*;
    #[test]
    fn mobile_arena_policy_is_thread_local() {
        let before: bool = b"thread.tcache.enabled\0".name().read().unwrap();
        std::thread::spawn(|| {
            mobile_thread().unwrap();
            let arena: u32 = b"thread.arena\0".name().read().unwrap();
            assert_eq!(arena, 0);
            let cache: bool = b"thread.tcache.enabled\0".name().read().unwrap();
            assert!(!cache);
        })
        .join()
        .unwrap();
        let after: bool = b"thread.tcache.enabled\0".name().read().unwrap();
        assert_eq!(after, before);
    }
}
