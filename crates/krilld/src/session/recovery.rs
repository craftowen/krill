//! 엔진 변경은 잠금 안에서 격리하고, unwind 뒤의 엔진은 재사용하지 않는다.

use std::panic::{self, AssertUnwindSafe};
use std::sync::MutexGuard;

use super::{Engine, Live, Shared};

impl Live {
    fn reset_engine(&mut self) {
        self.size = crate::term::clamp_size(self.size);
        self.engine = Engine::with_history(
            self.size,
            if self.parked {
                0
            } else {
                crate::term::HOT_LINES
            },
        );
        // 복구가 완료 hook 뒤의 입력 보호를 해제하거나 동면을 허용해서는 안 된다.
        self.input_since_done = true;
        self.last_input_ns = crate::status::now_ns();
        self.revision = self.revision.wrapping_add(1);
    }

    pub(super) fn update_engine<T>(&mut self, update: impl FnOnce(&mut Engine) -> T) -> Option<T> {
        if let Ok(value) = panic::catch_unwind(AssertUnwindSafe(|| update(&mut self.engine))) {
            Some(value)
        } else {
            self.reset_engine();
            tracing::warn!("terminal engine panicked; replaced engine");
            None
        }
    }
}

impl Shared {
    /// poison 뒤의 엔진은 버린다. seq·구독·입력 장부와 PTY는 유지한다.
    pub(super) fn live(&self) -> MutexGuard<'_, Live> {
        loop {
            match self.live.lock() {
                Ok(live) => return live,
                Err(poison) => {
                    let mut live = poison.into_inner();
                    live.reset_engine();
                    live.resized = Some(live.size);
                    let revision = live.revision;
                    self.live.clear_poison();
                    drop(live);
                    // live → out 중첩 잠금을 만들지 않는다. 빈 발행은 원본 seq를 바꾸지 않는다.
                    self.publish(&[], revision, false);
                    tracing::warn!("recovered poisoned terminal engine lock");
                }
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
