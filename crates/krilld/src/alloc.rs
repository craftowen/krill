//! 전역 할당기 설정(OWE-477). krilld는 jemalloc을 쓰고(`main.rs`의 `#[global_allocator]`), 해제한 페이지를
//! decay 없이 바로 OS에 돌려준다. macOS 기본 malloc은 강등으로 해제한 200열 행(4,800B)을 footprint로 돌려주지 않았고,
//! jemalloc 기본 decay(10초)는 할당이 멈춘 유휴 데몬에서 진행되지 않았다(실측, docs/spikes/OWE-477.md).

use tikv_jemalloc_ctl::{Access, AsName, Error};

/// 이후 해제되는 페이지를 즉시 OS에 돌려주도록 jemalloc decay를 0으로 둔다. `main` 첫 줄, 스레드를 띄우기 전에 부른다.
///
/// 새 아레나는 `arenas.*` 기본값을, 이미 있는 아레나 0(메인 스레드)은 `arena.0.*`를 따른다.
///
/// # Errors
///
/// jemalloc이 mallctl 쓰기를 거부하면 그 오류.
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

#[cfg(test)]
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
pub(crate) fn mobile_thread() -> Result<(), Error> {
    b"thread.arena\0".name().write(0_u32)?;
    b"thread.tcache.enabled\0".name().write(false)
}

#[cfg(test)]
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
