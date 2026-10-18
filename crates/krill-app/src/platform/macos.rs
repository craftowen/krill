//! macOS 구현: `AppKit`·`IOKit`·`Carbon` 직접 호출(CONVENTIONS U1: mk-app의 objc2·FFI 구역).

use std::ffi::{CStr, c_char, c_void};
use std::ptr::{self, NonNull};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{AnyWindowHandle, App, AppContext as _, Window};
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
const PREVENT_USER_IDLE_SYSTEM_SLEEP: &str = "PreventUserIdleSystemSleep";

static DOCK_BADGE: Mutex<Option<String>> = Mutex::new(None);

/// 이 프로세스가 `EnableSecureEventInput`을 켠 상태인지 기록한다(MK-076).
/// Carbon 내부 카운터가 중첩되므로 상태가 바뀔 때만 FFI를 부른다.
static SECURE_INPUT_ENGAGED: AtomicBool = AtomicBool::new(false);

/// 창을 다음 디스플레이 링크 틱을 기다리지 않고 지금 그린다. 더러운 뷰가 없으면 gpui가 그리지 않는다.
///
/// gpui 뷰의 `displayLayer:`를 부른다 = 디스플레이 링크 틱과 같은 그리기 경로(MK-013). 그 경로는 앱을 빌리므로 업데이트
/// 안에서 바로 부르면 빌림 충돌이 난다 → 포그라운드 태스크로 미룬다. 창이 닫혔거나 `AppKit` 창이 아니면(테스트 플랫폼)
/// 아무것도 안 한다.
pub fn draw_now(handle: AnyWindowHandle, cx: &App) {
    cx.spawn(async move |cx| {
        let Ok(Some(view)) = cx.update_window(handle, |_, window, _| ns_view(window)) else {
            return;
        };
        // 업데이트가 끝나 앱 빌림이 없다. 위와 이 호출 사이에 런루프가 돌지 않으므로 뷰는 살아 있다.
        display_layer(view);
    })
    .detach();
}

fn ns_view(window: &Window) -> Option<NonNull<c_void>> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::AppKit(h) => Some(h.ns_view),
        _ => None,
    }
}

fn display_layer(view: NonNull<c_void>) {
    let view = view.cast::<AnyObject>().as_ptr();
    // SAFETY: view는 살아 있는 gpui NSView(`draw_now`), `layer`는 NSView 메서드라 응답한다.
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    // SAFETY: `displayLayer:`는 gpui가 자기 뷰 클래스에 등록한 메서드다(vendor/gpui-pre-macos `window.rs`). gpui 업데이트
    // 밖에서 부른다(`draw_now`). 인자는 그 뷰의 레이어(CALayerDelegate 규약).
    let (): () = unsafe { msg_send![view, displayLayer: layer] };
}

/// 플랫폼 IME 쪽 조합을 버린다(`NSTextInputContext discardMarkedText`). 위젯이 조합을 끊고 확정했을 때 부른다 — 안 버리면
/// IME가 같은 글자를 나중에 다시 보낸다(MK-002). `AppKit` 창이 아니면(테스트 플랫폼) 아무것도 안 한다.
pub fn discard_marked_text(window: &Window) {
    let Some(view) = ns_view(window) else { return };
    let view = view.cast::<AnyObject>().as_ptr();
    // SAFETY: view는 살아 있는 gpui NSView(창이 그리는 중), `inputContext`는 NSResponder 메서드다.
    let ctx: *mut AnyObject = unsafe { msg_send![view, inputContext] };
    if ctx.is_null() {
        return;
    }
    // SAFETY: ctx는 위에서 받은 NSTextInputContext(널 아님), `discardMarkedText`는 그 클래스 메서드다.
    let (): () = unsafe { msg_send![ctx, discardMarkedText] };
}

/// 시스템 경고음(터미널 벨, OWE-490).
pub fn beep() {
    // SAFETY: AppKit `NSBeep`은 인자 없는 C 함수다. gpui가 AppKit을 링크하고, 메인 스레드(gpui 업데이트)에서 부른다.
    unsafe { NSBeep() }
}

/// Dock 아이콘 배지(`[[NSApplication sharedApplication] dockTile] setBadgeLabel:`, MK-044).
///
/// 빈 문자열이나 `None`이면 배지를 지운다. 같은 값이면 `AppKit` 호출을 생략한다.
pub fn set_dock_badge(label: Option<&str>) {
    let next = label.filter(|s| !s.is_empty()).map(str::to_owned);
    if let Ok(mut guard) = DOCK_BADGE.lock() {
        if *guard == next {
            return;
        }
        guard.clone_from(&next);
    }
    // SAFETY: `NSApp`은 AppKit이 내보내는 전역 포인터다. 앱이 초기화된 경우에만 `dockTile`을 갱신한다.
    let app = unsafe { NSApp };
    if !app.is_null() {
        set_app_dock_badge(app, next.as_deref());
    }
}

/// 마지막으로 설정한 Dock 배지 문자열(테스트·상태 확인용).
pub fn dock_badge() -> Option<String> {
    DOCK_BADGE.lock().ok().and_then(|g| g.clone())
}

/// `NSApplication`의 `NSDockTile`에 배지를 직접 설정하고 `badgeLabel`을 다시 읽어 돌려준다.
pub fn sync_dock_tile(label: Option<&str>) -> Option<String> {
    let cls = AnyClass::get(c"NSApplication")?;
    // SAFETY: `[NSApplication sharedApplication]`은 AppKit 싱글턴을 돌려준다.
    let app: *mut AnyObject = unsafe { msg_send![cls, sharedApplication] };
    if app.is_null() {
        return None;
    }
    set_app_dock_badge(app, label);
    read_app_dock_badge(app)
}

fn set_app_dock_badge(app: *mut AnyObject, label: Option<&str>) {
    // SAFETY: `app`은 널이 아닌 `NSApplication` 인스턴스고 `dockTile`은 표준 메서드다.
    let tile: *mut AnyObject = unsafe { msg_send![app, dockTile] };
    if tile.is_null() {
        return;
    }
    let cf = label.filter(|s| !s.is_empty()).and_then(cf_string);
    let ns_str = cf.map_or(ptr::null(), <*const c_void>::cast::<AnyObject>);
    // SAFETY: `CFStringRef`는 `NSString`과 toll-free bridge되며 `setBadgeLabel:`은 `nullable NSString *`을 복사한다.
    let (): () = unsafe { msg_send![tile, setBadgeLabel: ns_str] };
    if let Some(cf) = cf {
        cf_release(cf);
    }
}

fn read_app_dock_badge(app: *mut AnyObject) -> Option<String> {
    // SAFETY: `app`은 널이 아닌 `NSApplication` 인스턴스다.
    let tile: *mut AnyObject = unsafe { msg_send![app, dockTile] };
    if tile.is_null() {
        return None;
    }
    // SAFETY: `badgeLabel`은 `nullable NSString *`을 돌려준다.
    let badge: *const AnyObject = unsafe { msg_send![tile, badgeLabel] };
    if badge.is_null() {
        return None;
    }
    // SAFETY: `badge`는 널이 아닌 `NSString`이며 `UTF8String`은 NUL 종료 UTF-8 포인터를 돌려준다.
    let utf8: *const c_char = unsafe { msg_send![badge, UTF8String] };
    if utf8.is_null() {
        return None;
    }
    // SAFETY: `utf8`은 이 호출 동안 유효한 NUL 종료 C 문자열이다.
    let text = unsafe { CStr::from_ptr(utf8) }
        .to_string_lossy()
        .into_owned();
    (!text.is_empty()).then_some(text)
}

/// `IOPMAssertionCreateWithName`으로 잡은 유휴 잠자기 방지 핸들(MK-044). Drop 시 해제된다.
#[derive(Debug)]
pub struct SleepAssertion(u32);

impl SleepAssertion {
    /// `PreventUserIdleSystemSleep` assertion을 잡는다. `reason`은 macOS 상한(128자)에 맞춰 전달한다.
    pub fn acquire(reason: &str) -> Option<Self> {
        let kind = cf_string(PREVENT_USER_IDLE_SYSTEM_SLEEP)?;
        let bounded: String = reason.chars().take(128).collect();
        let Some(name) = cf_string(&bounded) else {
            cf_release(kind);
            return None;
        };
        let mut id: u32 = 0;
        // SAFETY: `kind`와 `name`은 유효한 `CFStringRef`이고 `id`는 쓰기 가능한 출력 포인터다.
        let rc = unsafe {
            IOPMAssertionCreateWithName(kind, K_IOPM_ASSERTION_LEVEL_ON, name, &raw mut id)
        };
        cf_release(name);
        cf_release(kind);
        (rc == 0).then_some(Self(id))
    }

    pub const fn id(&self) -> u32 {
        self.0
    }
}

impl Drop for SleepAssertion {
    fn drop(&mut self) {
        // SAFETY: `self.0`은 `IOPMAssertionCreateWithName`이 돌려준 유효한 assertion ID다.
        let _rc = unsafe { IOPMAssertionRelease(self.0) };
    }
}

pub(super) fn cf_string(s: &str) -> Option<*const c_void> {
    let len = isize::try_from(s.len()).ok()?;
    // SAFETY: `s`는 `len` 바이트의 유효한 UTF-8 슬라이스이며 `CFStringCreateWithBytes`가 복사해 새 객체를 만든다.
    let cf = unsafe {
        CFStringCreateWithBytes(ptr::null(), s.as_ptr(), len, K_CF_STRING_ENCODING_UTF8, 0)
    };
    (!cf.is_null()).then_some(cf)
}

pub(super) fn cf_release(cf: *const c_void) {
    if !cf.is_null() {
        // SAFETY: `cf`는 `CFStringCreateWithBytes`가 만든 널이 아닌 CoreFoundation 객체다.
        unsafe { CFRelease(cf) };
    }
}

/// macOS Secure Keyboard Entry(`EnableSecureEventInput` / `DisableSecureEventInput`, MK-076)를 켜거나 끈다.
/// 이미 같은 상태면 다시 부르지 않아 참조 카운터가 어긋나지 않게 한다. 성공하면 `true`.
pub fn set_secure_event_input(enabled: bool) -> bool {
    if SECURE_INPUT_ENGAGED.load(Ordering::Acquire) == enabled {
        return true;
    }
    let status = if enabled {
        // SAFETY: Carbon `EnableSecureEventInput`은 인자 없는 C 함수로 현재 프로세스의 보안 키 입력을 켠다.
        unsafe { EnableSecureEventInput() }
    } else {
        // SAFETY: Carbon `DisableSecureEventInput`은 인자 없는 C 함수로 앞선 `EnableSecureEventInput`을 해제한다.
        unsafe { DisableSecureEventInput() }
    };
    if status == 0 {
        SECURE_INPUT_ENGAGED.store(enabled, Ordering::Release);
        true
    } else {
        false
    }
}

/// 이 프로세스가 Secure Keyboard Entry를 켠 상태인지 돌려준다.
pub fn is_secure_event_input_engaged() -> bool {
    SECURE_INPUT_ENGAGED.load(Ordering::Acquire)
}

/// 시스템 전체에서 Secure Keyboard Entry가 켜져 있는지 조회한다(`IsSecureEventInputEnabled`).
pub fn is_system_secure_event_input_enabled() -> bool {
    // SAFETY: Carbon `IsSecureEventInputEnabled`는 인자 없는 읽기 전용 C 함수다.
    unsafe { IsSecureEventInputEnabled() != 0 }
}

// `AppKit` 공개 심볼.
unsafe extern "C" {
    static NSApp: *mut AnyObject;
    fn NSBeep();
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        alloc: *const c_void,
        bytes: *const u8,
        num_bytes: isize,
        encoding: u32,
        is_external_representation: u8,
    ) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: *const c_void,
        assertion_level: u32,
        assertion_name: *const c_void,
        assertion_id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(assertion_id: u32) -> i32;
}

// `Carbon` (HIToolbox) 공개 C 함수(MK-076 Secure Keyboard Entry).
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn EnableSecureEventInput() -> i32;
    fn DisableSecureEventInput() -> i32;
    fn IsSecureEventInputEnabled() -> u8;
}
