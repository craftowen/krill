//! 입력 에코 즉시 그리기(MK-013 결정 2, OWE-480).
//!
//! gpui macOS는 디스플레이 링크 틱에서만 그려서 key→present에 다음 틱까지의 대기(0~1 프레임)가 붙는다. 60Hz에서는
//! p95 17.0~18.8ms로 기준(16ms)을 넘었고, 에코 화면이 오면 바로 그리면 3.48ms였다(MK-013). 그래서 키를 세션에 보낸 뒤
//! [`WINDOW`] 안에 그 터미널의 화면이 바뀌면 틱을 기다리지 않고 창을 바로 그린다([`platform::draw_now`]).
//!
//! 키 하나에 한 번만 그린다. 키 뒤 대량 출력(Enter → `cat`)은 틱 합치기에 맡긴다(`displayLayer:`는 부를 때마다
//! 디스플레이 링크를 멈췄다 다시 켠다).
//!
//! 잠정 결정 의존(MK-013 결정 2, 오웬 승인 대기)은 이 파일과 `platform.rs`에 가둔다. 거부되면 둘과 호출부만 지운다.

use std::time::{Duration, Instant};

use gpui::{AnyWindowHandle, App};

use crate::platform;

/// 키를 보낸 뒤 이 시간 안에 온 화면을 에코로 본다(MK-013 제안값, key→Term p95보다 넉넉하게).
pub const WINDOW: Duration = Duration::from_millis(50);

/// 터미널 하나의 "마지막 키 뒤 아직 즉시 그리지 않음" 상태. 키를 PTY로 보내는 쪽과 화면 델타를 적용하는 쪽이 같이 쓴다.
// ponytail: 디스플레이 주기와 관계없이 켠다. 120Hz는 즉시 그리기 없이 합격이고 링크 재시작 비용은 측정 안 함. 120Hz에서
// 비용이 보이면 주기 ≤ 60Hz 창에서만 켠다.
#[derive(Debug, Default)]
pub struct EchoDraw {
    key_at: Option<Instant>,
}

impl EchoDraw {
    /// 키를 세션에 보냈다.
    pub fn key_sent(&mut self, now: Instant) {
        self.key_at = Some(now);
    }

    /// 이 터미널의 화면을 바꿨다(`Terminal::set_screen` 뒤). 키 뒤 [`WINDOW`] 안의 첫 화면이면 창을 지금 그린다.
    pub fn screen_changed(&mut self, window: AnyWindowHandle, cx: &App) {
        if self.take(Instant::now()) {
            platform::draw_now(window, cx);
        }
    }

    /// 즉시 그릴지 판정한다. 키 하나에 한 번만 참이다.
    pub fn take(&mut self, now: Instant) -> bool {
        self.key_at
            .take()
            .is_some_and(|at| now.saturating_duration_since(at) <= WINDOW)
    }
}

#[cfg(test)]
mod tests;
