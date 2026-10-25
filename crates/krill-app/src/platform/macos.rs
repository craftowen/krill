//! macOS 구현: `AppKit`·`IOKit`·`Carbon` 직접 호출(CONVENTIONS U1: mk-app의 objc2·FFI 구역).

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{AnyWindowHandle, App, AppContext as _, Window};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol, ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
const PREVENT_USER_IDLE_SYSTEM_SLEEP: &str = "PreventUserIdleSystemSleep";

thread_local! {
    static DOCK_BADGE: RefCell<Option<String>> = const { RefCell::new(None) };
}

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

/// Krill 알림음(`Contents/Resources/krill-chime.caf`, `packaging/macos/make_chime.py`). 번들 밖 실행처럼 리소스가 없거나
/// 재생을 시작하지 못하면 `NSBeep`으로 대신한다.
pub fn play_notification_chime() {
    if !play_bundled_sound("krill-chime") {
        beep();
    }
}

/// 이름으로 찾은 `NSSound`(번들 Resources → 시스템 사운드 순)를 재생한다. 재생을 시작했으면 `true`.
fn play_bundled_sound(name: &str) -> bool {
    let Some(cls) = AnyClass::get(c"NSSound") else {
        return false;
    };
    let Some(cf) = cf_string(name) else {
        return false;
    };
    // SAFETY: `soundNamed:`는 NSSound 클래스 메서드다. 인자는 NSString과 toll-free bridge되는 CFString이고, 반환은
    // autorelease된 캐시 인스턴스나 nil이다. 메인 스레드(gpui 업데이트)에서 부른다.
    let sound: *mut AnyObject = unsafe { msg_send![cls, soundNamed: cf.cast::<AnyObject>()] };
    cf_release(cf);
    if sound.is_null() {
        return false;
    }
    // 같은 캐시 인스턴스가 아직 울리는 중이면 `play`가 NO를 돌려주므로 먼저 멈추고 처음부터 다시 낸다.
    // SAFETY: sound는 널이 아닌 NSSound, `stop`·`play`는 BOOL을 돌려주는 인스턴스 메서드다.
    let _: bool = unsafe { msg_send![sound, stop] };
    // SAFETY: 위와 같다.
    unsafe { msg_send![sound, play] }
}

/// Dock 아이콘 배지(`[[NSApplication sharedApplication] dockTile] setBadgeLabel:`, MK-044).
///
/// 빈 문자열이나 `None`이면 배지를 지운다. 같은 값이면 `AppKit` 호출을 생략한다.
pub fn set_dock_badge(label: Option<&str>) {
    let next = label.filter(|s| !s.is_empty()).map(str::to_owned);
    let changed = DOCK_BADGE.with_borrow_mut(|badge| {
        if *badge == next {
            return false;
        }
        badge.clone_from(&next);
        true
    });
    if !changed {
        return;
    }
    // SAFETY: `NSApp`은 AppKit이 내보내는 전역 포인터다. 앱이 초기화된 경우에만 `dockTile`을 갱신한다.
    let app = unsafe { NSApp };
    if !app.is_null() {
        set_app_dock_badge(app, next.as_deref());
    }
}

/// 마지막으로 설정한 Dock 배지 문자열(테스트·상태 확인용).
pub fn dock_badge() -> Option<String> {
    DOCK_BADGE.with_borrow(Clone::clone)
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

/// 창이 보이고 앱이 활성인 동안만 잡는 `NSProcessInfo` 활동: App Nap과 타이머 합치기를 막는다. 유휴 시스템 잠자기는
/// 막지 않는다(그건 [`SleepAssertion`] 몫). Drop 시 끝난다.
#[derive(Debug)]
pub struct LatencyActivity(Retained<ProtocolObject<dyn NSObjectProtocol>>);

impl LatencyActivity {
    pub fn begin() -> Self {
        let options = NSActivityOptions::UserInitiatedAllowingIdleSystemSleep
            | NSActivityOptions::LatencyCritical;
        Self(
            NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                options,
                &NSString::from_str("Krill window is visible and active"),
            ),
        )
    }
}

impl Drop for LatencyActivity {
    fn drop(&mut self) {
        // SAFETY: `self.0`은 `beginActivityWithOptions:reason:`이 돌려준 활동 토큰이고 Drop에서 한 번만 끝낸다.
        unsafe { NSProcessInfo::processInfo().endActivity(&self.0) };
    }
}

/// `[NSApp isActive]`. 메인 스레드에서만 부른다. 앱이 아직 없으면(테스트 플랫폼) `false`.
pub fn app_is_active() -> bool {
    // SAFETY: `NSApp`은 AppKit이 내보내는 전역 포인터다. 널이면 아래에서 거른다.
    let app = unsafe { NSApp };
    // SAFETY: `app`은 널이 아닌 `NSApplication`, `isActive`는 BOOL을 돌려주는 표준 메서드다.
    !app.is_null() && unsafe { msg_send![app, isActive] }
}

/// 주 디스플레이가 잠들었는지(`CGDisplayIsAsleep`). 어느 스레드에서든 부를 수 있다.
pub fn display_asleep() -> bool {
    // SAFETY: 인자 없는 CoreGraphics 조회 함수다.
    let display = unsafe { CGMainDisplayID() };
    // SAFETY: `display`는 `CGMainDisplayID`가 돌려준 ID다. 잘못된 ID여도 0을 돌려줄 뿐이다.
    unsafe { CGDisplayIsAsleep(display) != 0 }
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

/// macOS `public.tiff` 클립보드 비트맵 바이트를 PNG 바이트로 변환한다(#28869).
/// `NSBitmapImageRep`의 `representationUsingType:properties:`(`NSBitmapImageFileTypePNG = 4`)를 쓴다.
#[must_use]
pub fn tiff_to_png(tiff: &[u8]) -> Option<Vec<u8>> {
    if tiff.is_empty() {
        return None;
    }
    let ns_data_cls = AnyClass::get(c"NSData")?;
    let rep_cls = AnyClass::get(c"NSBitmapImageRep")?;
    let dict_cls = AnyClass::get(c"NSDictionary")?;
    // SAFETY: `tiff.as_ptr()`는 `tiff.len()` 바이트 동안 유효하며 `dataWithBytes:length:`는 바이트를 복사한다.
    let ns_data: *mut AnyObject = unsafe {
        msg_send![ns_data_cls, dataWithBytes: tiff.as_ptr().cast::<c_void>(), length: tiff.len()]
    };
    if ns_data.is_null() {
        return None;
    }
    // SAFETY: `ns_data`는 유효한 `NSData` 객체다. 유효한 이미지가 아니면 nil을 돌려준다.
    let rep: *mut AnyObject = unsafe { msg_send![rep_cls, imageRepWithData: ns_data] };
    if rep.is_null() {
        return None;
    }
    // SAFETY: `[NSDictionary dictionary]`는 빈 불변 딕셔너리를 돌려준다.
    let props: *mut AnyObject = unsafe { msg_send![dict_cls, dictionary] };
    // NSBitmapImageFileTypePNG = 4
    let png_type: usize = 4;
    // SAFETY: `rep`은 유효한 `NSBitmapImageRep`이고 `representationUsingType:properties:`는 `nullable NSData *`를 돌려준다.
    let png_data: *mut AnyObject =
        unsafe { msg_send![rep, representationUsingType: png_type, properties: props] };
    if png_data.is_null() {
        return None;
    }
    // SAFETY: `png_data`는 널이 아닌 `NSData`다.
    let len: usize = unsafe { msg_send![png_data, length] };
    // SAFETY: `png_data`는 널이 아닌 `NSData`이며 `bytes`는 내부 버퍼 포인터를 돌려준다.
    let ptr: *const u8 = unsafe { msg_send![png_data, bytes] };
    if len == 0 || ptr.is_null() {
        return None;
    }
    // SAFETY: `ptr`은 `png_data` 내부의 `len` 바이트 버퍼를 가리키며 아래 `to_vec()` 동안 살아 있다.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
}

/// macOS 시스템 클립보드(`NSPasteboard::generalPasteboard`)에서 이미지 바이트를 직접 읽는다.
///
/// `gpui`의 기본 클립보드 리더는 `NSFilenamesPboardType`이나 `public.utf8-plain-text`가 함께 들어 있으면
/// 이미지 항목(`public.png`·`public.tiff`)을 건너뛴다. `prefer_over_text`가 `false`면 클립보드 `types`
/// 선언 순서에서 이미지 UTI가 파일·일반 텍스트보다 먼저 선언된 경우(Safari·Preview·CleanShot 등의 이미지 복사)에만
/// 이미지를 추출하며, `true`면(죽은 임시 파일 경로·빈 문자열 등) 텍스트 순서와 무관하게 이미지를 추출한다.
#[must_use]
pub fn read_pasteboard_image(prefer_over_text: bool) -> Option<(gpui::ImageFormat, Vec<u8>)> {
    let pb_cls = AnyClass::get(c"NSPasteboard")?;
    // SAFETY: `[NSPasteboard generalPasteboard]`는 AppKit 시스템 클립보드 싱글턴을 돌려준다.
    let pb: *mut AnyObject = unsafe { msg_send![pb_cls, generalPasteboard] };
    if pb.is_null() {
        return None;
    }
    // SAFETY: `pb`는 널이 아닌 `NSPasteboard`이며 `types`는 선언된 UTI 배열(`nullable NSArray *`)을 돌려준다.
    let types: *mut AnyObject = unsafe { msg_send![pb, types] };
    if types.is_null() {
        return None;
    }

    let index_of = |uti: &str| -> Option<usize> {
        let s = NSString::from_str(uti);
        // SAFETY: `types`는 유효한 `NSArray`이고 `s`는 유효한 `NSString`이다.
        let contains: bool = unsafe { msg_send![types, containsObject: &*s] };
        if !contains {
            return None;
        }
        // SAFETY: `containsObject:`가 참이므로 해당 인덱스를 돌려준다.
        let idx: usize = unsafe { msg_send![types, indexOfObject: &*s] };
        Some(idx)
    };

    let candidates = [
        ("public.png", gpui::ImageFormat::Png),
        ("public.jpeg", gpui::ImageFormat::Jpeg),
        ("org.webmproject.webp", gpui::ImageFormat::Webp),
        ("com.compuserve.gif", gpui::ImageFormat::Gif),
        ("com.microsoft.bmp", gpui::ImageFormat::Bmp),
        ("public.tiff", gpui::ImageFormat::Tiff),
    ];

    let min_img_idx = candidates
        .iter()
        .filter_map(|(uti, _)| index_of(uti))
        .min()?;

    if !prefer_over_text {
        let file_idx = index_of("NSFilenamesPboardType").or_else(|| index_of("public.file-url"));
        if file_idx.is_some_and(|idx| idx < min_img_idx) {
            return None;
        }
        if index_of("public.utf8-plain-text").is_some_and(|idx| idx < min_img_idx) {
            return None;
        }
    }

    for (uti, fmt) in candidates {
        if index_of(uti).is_none() {
            continue;
        }
        let s = NSString::from_str(uti);
        // SAFETY: `pb`는 유효한 `NSPasteboard`이고 `dataForType:`는 `nullable NSData *`를 돌려준다.
        let data: *mut AnyObject = unsafe { msg_send![pb, dataForType: &*s] };
        if data.is_null() {
            continue;
        }
        // SAFETY: `data`는 널이 아닌 `NSData`다.
        let len: usize = unsafe { msg_send![data, length] };
        // SAFETY: `data`는 널이 아닌 `NSData`이며 `bytes`는 내부 버퍼 포인터를 돌려준다.
        let ptr: *const u8 = unsafe { msg_send![data, bytes] };
        if len == 0 || ptr.is_null() {
            continue;
        }
        // SAFETY: `ptr`은 `data` 내부의 `len` 바이트 버퍼를 가리키며 아래 `to_vec()` 동안 살아 있다.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
        if fmt == gpui::ImageFormat::Tiff {
            if let Some(png) = tiff_to_png(&bytes) {
                return Some((gpui::ImageFormat::Png, png));
            }
            return Some((gpui::ImageFormat::Tiff, bytes));
        }
        return Some((fmt, bytes));
    }
    None
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

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayIsAsleep(display: u32) -> u32;
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
