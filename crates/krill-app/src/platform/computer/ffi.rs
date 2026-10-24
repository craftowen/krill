//! computer 워커의 macOS FFI: `CoreFoundation`·`HIServices`(AX)·`CoreGraphics` 이벤트·`libproc`·`AppKit`.
//!
//! 이 워커의 `unsafe`는 전부 이 파일에 있다(CONVENTIONS U1·U2). 공개 함수는 모두 safe이고 CF 소유권은
//! [`Cf`]가 쥔다(드롭 시 `CFRelease`). 여기에는 콜백이 없어 panic이 C 경계를 넘을 경로가 없다(U10).
//! AX 호출은 대상 앱과의 동기 Mach IPC라 막힐 수 있다. 상한은 [`set_messaging_timeout`]으로 둔다.

use std::ffi::{c_int, c_void};
use std::fmt;
use std::ptr::{self, NonNull};

use objc2::msg_send;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::NSString;

type CfTypeRef = *const c_void;
type CfIndex = isize;
type CfTypeId = usize;
type Boolean = u8;
pub type AxError = i32;

const UTF8: u32 = 0x0800_0100;
const NUMBER_F64: CfIndex = 13;
const AX_VALUE_CGPOINT: u32 = 1;
const AX_VALUE_CGSIZE: u32 = 2;
const AX_VALUE_AXERROR: u32 = 5;
const PROC_PIDTBSDINFO: c_int = 3;
const HID_EVENT_TAP: u32 = 0;
const MOUSE_LEFT_DOWN: u32 = 1;
const MOUSE_LEFT_UP: u32 = 2;
const MOUSE_BUTTON_LEFT: u32 = 0;
const FIELD_CLICK_STATE: u32 = 1;
const FIELD_SOURCE_USER_DATA: u32 = 42;
/// 이 워커가 합성한 이벤트 표식(`kCGEventSourceUserData`, ASCII `KRIL`).
pub const SYNTHETIC_MARK: i64 = 0x4B52_494C;

pub mod ax {
    pub const SUCCESS: i32 = 0;
    pub const FAILURE: i32 = -25200;
    pub const ILLEGAL_ARGUMENT: i32 = -25201;
    pub const INVALID_ELEMENT: i32 = -25202;
    pub const CANNOT_COMPLETE: i32 = -25204;
    pub const ATTRIBUTE_UNSUPPORTED: i32 = -25205;
    pub const ACTION_UNSUPPORTED: i32 = -25206;
    pub const NOT_IMPLEMENTED: i32 = -25208;
    pub const API_DISABLED: i32 = -25211;
    pub const NO_VALUE: i32 = -25212;
}

pub const fn ax_error_name(code: AxError) -> &'static str {
    match code {
        ax::SUCCESS => "success",
        ax::FAILURE => "ax_failure",
        ax::ILLEGAL_ARGUMENT => "ax_illegal_argument",
        ax::INVALID_ELEMENT => "ax_invalid_element",
        ax::CANNOT_COMPLETE => "ax_cannot_complete",
        ax::ATTRIBUTE_UNSUPPORTED => "ax_attribute_unsupported",
        ax::ACTION_UNSUPPORTED => "ax_action_unsupported",
        ax::NOT_IMPLEMENTED => "ax_not_implemented",
        ax::API_DISABLED => "ax_api_disabled",
        ax::NO_VALUE => "ax_no_value",
        _ => "ax_error",
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CgPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CgSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
struct CfRange {
    location: CfIndex,
    length: CfIndex,
}

#[repr(C)]
#[derive(Default)]
struct ProcessSerialNumber {
    high: u32,
    low: u32,
}
const _: () = assert!(size_of::<ProcessSerialNumber>() == 8);

/// Fresh `WindowServer` foreground witness without an `AppKit` event loop.
/// `NSWorkspace` can retain the old application in this headless worker, and the
/// system-wide AX focused-application attribute can time out independently of the target.
pub fn foreground_pid() -> Result<i32, i32> {
    let mut serial = ProcessSerialNumber::default();
    // SAFETY: `serial` has the public Process Manager ABI and is a writable output value.
    let status = unsafe { GetFrontProcess(&raw mut serial) };
    if status != 0 {
        return Err(status);
    }
    let mut pid = 0;
    // SAFETY: `serial` was initialized by GetFrontProcess; `pid` is a writable pid_t.
    let status = unsafe { GetProcessPID(&raw const serial, &raw mut pid) };
    if status != 0 {
        return Err(status);
    }
    if pid <= 0 {
        return Err(-50); // paramErr: an unusable witness must never authorize input.
    }
    Ok(pid)
}

/// `<sys/proc_info.h>`의 `struct proc_bsdinfo`(`PROC_PIDTBSDINFO_SIZE` = 136).
#[repr(C)]
struct ProcBsdInfo {
    flags: u32,
    status: u32,
    xstatus: u32,
    pid: u32,
    ppid: u32,
    uid: u32,
    gid: u32,
    ruid: u32,
    rgid: u32,
    svuid: u32,
    svgid: u32,
    rfu: u32,
    comm: [u8; 16],
    name: [u8; 32],
    nfiles: u32,
    pgid: u32,
    pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    nice: i32,
    start_tvsec: u64,
    start_tvusec: u64,
}
const _: () = assert!(size_of::<ProcBsdInfo>() == 136);

/// 소유한(+1) CF 참조.
pub struct Cf(NonNull<c_void>);

impl Cf {
    /// Create/Copy 규칙의 +1 참조를 넘겨받는다.
    ///
    /// # Safety
    /// `raw`는 null이거나 호출자가 소유권을 넘기는 살아 있는 CF 객체여야 한다.
    unsafe fn adopt(raw: CfTypeRef) -> Option<Self> {
        NonNull::new(raw.cast_mut()).map(Self)
    }

    /// 빌린(+0) 참조를 retain해 소유한다.
    ///
    /// # Safety
    /// `raw`는 null이거나 이 호출 동안 살아 있는 CF 객체여야 한다.
    unsafe fn retain(raw: CfTypeRef) -> Option<Self> {
        let raw = NonNull::new(raw.cast_mut())?;
        // SAFETY: 호출자가 `raw`가 살아 있는 CF 객체임을 보장한다. 늘어난 참조는 `Self`가 드롭 때 놓는다.
        unsafe { CFRetain(raw.as_ptr()) };
        Some(Self(raw))
    }

    const fn as_ptr(&self) -> CfTypeRef {
        self.0.as_ptr()
    }

    fn type_id(&self) -> CfTypeId {
        // SAFETY: `self`는 살아 있는 CF 객체다.
        unsafe { CFGetTypeID(self.as_ptr()) }
    }

    /// `CFHash`. 같은 객체(`CFEqual`)면 같은 값이다. 방문 집합의 버킷용이다.
    pub fn hash_code(&self) -> usize {
        // SAFETY: `self`는 살아 있는 CF 객체다.
        unsafe { CFHash(self.as_ptr()) }
    }
}

impl Clone for Cf {
    fn clone(&self) -> Self {
        // SAFETY: `self`가 살아 있는 참조를 쥐고 있고, 늘어난 참조는 새 `Cf`가 드롭 때 놓는다.
        unsafe { CFRetain(self.as_ptr()) };
        Self(self.0)
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        // SAFETY: `Cf`는 정확히 한 참조를 소유한다.
        unsafe { CFRelease(self.as_ptr()) };
    }
}

impl PartialEq for Cf {
    fn eq(&self, other: &Self) -> bool {
        // SAFETY: 둘 다 살아 있는 CF 객체다.
        unsafe { CFEqual(self.as_ptr(), other.as_ptr()) != 0 }
    }
}

impl fmt::Debug for Cf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Cf({:p})", self.0)
    }
}

pub fn cf_string(text: &str) -> Option<Cf> {
    let len = CfIndex::try_from(text.len()).ok()?;
    // SAFETY: `text` 바이트는 호출 동안 유효하고 길이가 맞다. 결과는 Create 규칙의 +1 참조다.
    let raw = unsafe { CFStringCreateWithBytes(ptr::null(), text.as_ptr(), len, UTF8, 0) };
    // SAFETY: Create 규칙의 +1 참조다.
    unsafe { Cf::adopt(raw) }
}

/// `CFString`이면 앞 `max_utf16` 단위까지 UTF-8로 읽는다. 둘째 값은 잘림 여부다.
pub fn string_value(value: &Cf, max_utf16: usize) -> Option<(String, bool)> {
    // SAFETY: 인자 없는 타입 ID 조회다.
    let string_type = unsafe { CFStringGetTypeID() };
    if value.type_id() != string_type {
        return None;
    }
    // SAFETY: `value`는 CFString이다.
    let total = unsafe { CFStringGetLength(value.as_ptr()) };
    let take = total.min(CfIndex::try_from(max_utf16).unwrap_or(CfIndex::MAX));
    // UTF-16 한 단위는 UTF-8 3바이트 이하다(서로게이트 쌍 2단위 → 4바이트).
    let cap = usize::try_from(take).ok()?.checked_mul(3)?;
    let mut buf = vec![0u8; cap];
    let mut used: CfIndex = 0;
    // SAFETY: 범위는 문자열 안이고 `buf`는 `cap`바이트 쓰기 가능하며 `used`는 유효한 출력 위치다.
    let converted = unsafe {
        CFStringGetBytes(
            value.as_ptr(),
            CfRange {
                location: 0,
                length: take,
            },
            UTF8,
            0,
            0,
            buf.as_mut_ptr(),
            CfIndex::try_from(cap).ok()?,
            &raw mut used,
        )
    };
    buf.truncate(usize::try_from(used).ok()?);
    let text = String::from_utf8(buf).ok()?;
    Some((text, converted < total))
}

pub fn bool_value(value: &Cf) -> Option<bool> {
    // SAFETY: 인자 없는 타입 ID 조회다.
    let bool_type = unsafe { CFBooleanGetTypeID() };
    if value.type_id() != bool_type {
        return None;
    }
    // SAFETY: `value`는 CFBoolean이다.
    Some(unsafe { CFBooleanGetValue(value.as_ptr()) } != 0)
}

pub fn number_value(value: &Cf) -> Option<f64> {
    // SAFETY: 인자 없는 타입 ID 조회다.
    let number_type = unsafe { CFNumberGetTypeID() };
    if value.type_id() != number_type {
        return None;
    }
    let mut out = 0f64;
    // SAFETY: `value`는 CFNumber이고 `out`은 `kCFNumberDoubleType`에 맞는 f64 출력 위치다.
    let ok = unsafe { CFNumberGetValue(value.as_ptr(), NUMBER_F64, (&raw mut out).cast()) };
    (ok != 0).then_some(out)
}

fn ax_value_type(value: &Cf) -> Option<u32> {
    // SAFETY: 인자 없는 타입 ID 조회다.
    let ax_value = unsafe { AXValueGetTypeID() };
    if value.type_id() != ax_value {
        return None;
    }
    // SAFETY: `value`는 AXValue다.
    Some(unsafe { AXValueGetType(value.as_ptr()) })
}

pub fn point_value(value: &Cf) -> Option<CgPoint> {
    if ax_value_type(value)? != AX_VALUE_CGPOINT {
        return None;
    }
    let mut out = CgPoint::default();
    // SAFETY: CGPoint형 AXValue이고 `out`은 CGPoint 출력 위치다.
    let ok = unsafe { AXValueGetValue(value.as_ptr(), AX_VALUE_CGPOINT, (&raw mut out).cast()) };
    (ok != 0).then_some(out)
}

pub fn size_value(value: &Cf) -> Option<CgSize> {
    if ax_value_type(value)? != AX_VALUE_CGSIZE {
        return None;
    }
    let mut out = CgSize::default();
    // SAFETY: CGSize형 AXValue이고 `out`은 CGSize 출력 위치다.
    let ok = unsafe { AXValueGetValue(value.as_ptr(), AX_VALUE_CGSIZE, (&raw mut out).cast()) };
    (ok != 0).then_some(out)
}

/// `CFArray` 원소를 retain해 돌려준다. 배열이 아니면 `None`.
pub fn array_items(value: &Cf) -> Option<Vec<Cf>> {
    // SAFETY: 인자 없는 타입 ID 조회다.
    let array_type = unsafe { CFArrayGetTypeID() };
    if value.type_id() != array_type {
        return None;
    }
    // SAFETY: `value`는 CFArray다.
    let count = unsafe { CFArrayGetCount(value.as_ptr()) };
    let mut items = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
    for index in 0..count {
        // SAFETY: `index`는 0..count 안이다. 결과는 배열이 쥔 +0 참조다.
        let raw = unsafe { CFArrayGetValueAtIndex(value.as_ptr(), index) };
        // SAFETY: 배열이 살아 있는 동안 원소도 살아 있다.
        if let Some(item) = unsafe { Cf::retain(raw) } {
            items.push(item);
        }
    }
    Some(items)
}

/// `AXUIElementCopyMultipleAttributeValues`용 속성 이름 배열. 콜백 없는 `CFArray`라 원소 수명을 이 값이 쥔다.
#[derive(Debug)]
pub struct AttrBatch {
    array: Cf,
    names: Vec<Cf>,
}

impl AttrBatch {
    pub fn new(names: &[&Cf]) -> Option<Self> {
        let owned: Vec<Cf> = names.iter().copied().cloned().collect();
        let raws: Vec<CfTypeRef> = owned.iter().map(Cf::as_ptr).collect();
        let count = CfIndex::try_from(raws.len()).ok()?;
        // SAFETY: `raws`는 `count`개의 살아 있는 CFString이다. 콜백이 null이라 배열은 원소를 retain하지 않으므로
        // `names`가 배열보다 오래 산다(같은 구조체, `array`가 먼저 드롭된다).
        let raw = unsafe { CFArrayCreate(ptr::null(), raws.as_ptr(), count, ptr::null()) };
        // SAFETY: Create 규칙의 +1 참조다.
        let array = unsafe { Cf::adopt(raw) }?;
        Some(Self {
            array,
            names: owned,
        })
    }
}

pub fn application(pid: i32) -> Option<Cf> {
    // SAFETY: 임의 pid로 호출 가능하며 결과는 Create 규칙의 +1 참조다.
    let raw = unsafe { AXUIElementCreateApplication(pid) };
    // SAFETY: Create 규칙의 +1 참조다.
    unsafe { Cf::adopt(raw) }
}

pub fn system_wide() -> Option<Cf> {
    // SAFETY: 인자 없는 생성 함수, 결과는 +1 참조다.
    let raw = unsafe { AXUIElementCreateSystemWide() };
    // SAFETY: Create 규칙의 +1 참조다.
    unsafe { Cf::adopt(raw) }
}

/// 시스템 전체 요소에 주면 이 프로세스의 모든 AX 호출 기본 상한이 된다.
pub fn set_messaging_timeout(element: &Cf, seconds: f32) -> AxError {
    // SAFETY: `element`는 살아 있는 AXUIElement다.
    unsafe { AXUIElementSetMessagingTimeout(element.as_ptr(), seconds) }
}

pub fn copy_attribute(element: &Cf, attr: &Cf) -> Result<Cf, AxError> {
    let mut out: CfTypeRef = ptr::null();
    // SAFETY: `element`·`attr`는 살아 있고 `out`은 유효한 출력 위치다.
    let error =
        unsafe { AXUIElementCopyAttributeValue(element.as_ptr(), attr.as_ptr(), &raw mut out) };
    // SAFETY: Copy 규칙의 +1 참조(실패면 null)다.
    let value = unsafe { Cf::adopt(out) };
    if error != ax::SUCCESS {
        return Err(error);
    }
    value.ok_or(ax::NO_VALUE)
}

pub fn attribute_count(element: &Cf, attr: &Cf) -> Result<usize, AxError> {
    let mut out: CfIndex = 0;
    // SAFETY: `element`·`attr`는 살아 있고 `out`은 유효한 출력 위치다.
    let error =
        unsafe { AXUIElementGetAttributeValueCount(element.as_ptr(), attr.as_ptr(), &raw mut out) };
    if error != ax::SUCCESS {
        return Err(error);
    }
    Ok(usize::try_from(out).unwrap_or(0))
}

/// 배열 속성의 앞 `max`개만 복사한다(거대한 자식 목록 상한).
pub fn copy_attribute_values(element: &Cf, attr: &Cf, max: usize) -> Result<Vec<Cf>, AxError> {
    let mut out: CfTypeRef = ptr::null();
    let max = CfIndex::try_from(max).unwrap_or(CfIndex::MAX);
    // SAFETY: `element`·`attr`는 살아 있고 `out`은 유효한 출력 위치다.
    let error = unsafe {
        AXUIElementCopyAttributeValues(element.as_ptr(), attr.as_ptr(), 0, max, &raw mut out)
    };
    // SAFETY: Copy 규칙의 +1 참조(실패면 null)다.
    let value = unsafe { Cf::adopt(out) };
    if error != ax::SUCCESS {
        return Err(error);
    }
    Ok(value
        .and_then(|array| array_items(&array))
        .unwrap_or_default())
}

/// 여러 속성을 한 번의 IPC로 읽는다. 미지원·값 없음만 `None`이며 다른 개별 오류도 보존한다.
pub fn copy_multiple(element: &Cf, batch: &AttrBatch) -> Result<Vec<Option<Cf>>, AxError> {
    let mut out: CfTypeRef = ptr::null();
    // SAFETY: `element`·`batch.array`는 살아 있고 `out`은 유효한 출력 위치다.
    let error = unsafe {
        AXUIElementCopyMultipleAttributeValues(
            element.as_ptr(),
            batch.array.as_ptr(),
            0,
            &raw mut out,
        )
    };
    // SAFETY: Copy 규칙의 +1 참조(실패면 null)다.
    let value = unsafe { Cf::adopt(out) };
    if error != ax::SUCCESS {
        return Err(error);
    }
    let items = value
        .and_then(|array| array_items(&array))
        .ok_or(ax::FAILURE)?;
    if items.len() != batch.names.len() {
        return Err(ax::FAILURE);
    }
    items.into_iter().map(optional_attribute).collect()
}

/// 배치의 개별 `AXError를` 값 없음으로 오인하면 불완전한 지문으로 변경을 허용할 수 있다.
fn optional_attribute(value: Cf) -> Result<Option<Cf>, AxError> {
    if ax_value_type(&value) != Some(AX_VALUE_AXERROR) {
        return Ok(Some(value));
    }
    let mut error: AxError = ax::FAILURE;
    // SAFETY: `value`는 AXError형 AXValue이고 `error`는 AXError 출력 위치다.
    let ok = unsafe { AXValueGetValue(value.as_ptr(), AX_VALUE_AXERROR, (&raw mut error).cast()) };
    if ok == 0 {
        return Err(ax::FAILURE);
    }
    match error {
        ax::NO_VALUE | ax::ATTRIBUTE_UNSUPPORTED | ax::NOT_IMPLEMENTED => Ok(None),
        _ => Err(error),
    }
}

pub fn action_names(element: &Cf, max: usize) -> Result<Vec<String>, AxError> {
    let mut out: CfTypeRef = ptr::null();
    // SAFETY: `element`는 살아 있고 `out`은 유효한 출력 위치다.
    let error = unsafe { AXUIElementCopyActionNames(element.as_ptr(), &raw mut out) };
    // SAFETY: Copy 규칙의 +1 참조(실패면 null)다.
    let value = unsafe { Cf::adopt(out) };
    if error != ax::SUCCESS {
        return Err(error);
    }
    Ok(value
        .and_then(|array| array_items(&array))
        .unwrap_or_default()
        .iter()
        .take(max)
        .filter_map(|name| string_value(name, 128).map(|(name, _)| name))
        .collect())
}

pub fn perform_action(element: &Cf, action: &Cf) -> AxError {
    // SAFETY: `element`·`action`은 살아 있는 AXUIElement·CFString이다.
    unsafe { AXUIElementPerformAction(element.as_ptr(), action.as_ptr()) }
}

pub fn set_attribute(element: &Cf, attr: &Cf, value: &Cf) -> AxError {
    // SAFETY: 세 인자 모두 살아 있는 CF 객체다.
    unsafe { AXUIElementSetAttributeValue(element.as_ptr(), attr.as_ptr(), value.as_ptr()) }
}

pub fn is_settable(element: &Cf, attr: &Cf) -> Result<bool, AxError> {
    let mut out: Boolean = 0;
    // SAFETY: `element`·`attr`는 살아 있고 `out`은 유효한 출력 위치다.
    let error =
        unsafe { AXUIElementIsAttributeSettable(element.as_ptr(), attr.as_ptr(), &raw mut out) };
    if error != ax::SUCCESS {
        return Err(error);
    }
    Ok(out != 0)
}

pub fn element_pid(element: &Cf) -> Option<i32> {
    let mut out: c_int = 0;
    // SAFETY: `element`는 살아 있고 `out`은 유효한 출력 위치다.
    let error = unsafe { AXUIElementGetPid(element.as_ptr(), &raw mut out) };
    (error == ax::SUCCESS).then_some(out)
}

/// 화면 좌표의 최상단 AX 요소(hit-test).
pub fn element_at(system: &Cf, at: CgPoint) -> Result<Cf, AxError> {
    let mut out: CfTypeRef = ptr::null();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "AXUIElementCopyElementAtPosition takes float screen points"
    )]
    let (x, y) = (at.x as f32, at.y as f32);
    // SAFETY: `system`은 살아 있는 AXUIElement이고 `out`은 유효한 출력 위치다.
    let error = unsafe { AXUIElementCopyElementAtPosition(system.as_ptr(), x, y, &raw mut out) };
    // SAFETY: Copy 규칙의 +1 참조(실패면 null)다.
    let value = unsafe { Cf::adopt(out) };
    if error != ax::SUCCESS {
        return Err(error);
    }
    value.ok_or(ax::NO_VALUE)
}

/// 이벤트 합성(`CGEventPost`) 권한. 프롬프트 없음.
pub fn post_event_preflight() -> bool {
    // SAFETY: 인자 없는 읽기 전용 C 함수다.
    unsafe { CGPreflightPostEventAccess() }
}

/// 합성 입력 이벤트. 만들기만 하고 [`post`]해야 발행된다.
#[derive(Debug)]
pub struct Event(Cf);

fn mark(event: &Cf) {
    // SAFETY: `event`는 살아 있는 CGEvent다.
    unsafe { CGEventSetIntegerValueField(event.as_ptr(), FIELD_SOURCE_USER_DATA, SYNTHETIC_MARK) };
}

pub fn mouse_event(down: bool, at: CgPoint) -> Option<Event> {
    let kind = if down { MOUSE_LEFT_DOWN } else { MOUSE_LEFT_UP };
    // SAFETY: null 소스는 허용되며 결과는 Create 규칙의 +1 참조다.
    let raw = unsafe { CGEventCreateMouseEvent(ptr::null(), kind, at, MOUSE_BUTTON_LEFT) };
    // SAFETY: Create 규칙의 +1 참조다.
    let event = unsafe { Cf::adopt(raw) }?;
    // SAFETY: `event`는 살아 있는 마우스 CGEvent다.
    unsafe { CGEventSetIntegerValueField(event.as_ptr(), FIELD_CLICK_STATE, 1) };
    mark(&event);
    Some(Event(event))
}

/// 유니코드 문자열을 싣는 키 이벤트(가상 키 0). IME를 거치는지는 미확인이다.
pub fn unicode_key_event(down: bool, units: &[u16]) -> Option<Event> {
    // SAFETY: null 소스는 허용되며 결과는 Create 규칙의 +1 참조다.
    let raw = unsafe { CGEventCreateKeyboardEvent(ptr::null(), 0, down) };
    // SAFETY: Create 규칙의 +1 참조다.
    let event = unsafe { Cf::adopt(raw) }?;
    // 키 뗌은 정리 이벤트다. 중간에 포커스가 바뀌어도 새 수신기에 문자열을 싣지 않는다.
    if down {
        // SAFETY: `units`는 호출 동안 유효한 UTF-16 버퍼이고 길이가 맞다(CG가 복사한다).
        unsafe { CGEventKeyboardSetUnicodeString(event.as_ptr(), units.len(), units.as_ptr()) };
    }
    mark(&event);
    Some(Event(event))
}

/// HID 이벤트 탭에 발행한다. CG는 실패를 알리지 않는다.
pub fn post(event: &Event) {
    // SAFETY: `event`는 살아 있는 CGEvent다.
    unsafe { CGEventPost(HID_EVENT_TAP, event.0.as_ptr()) };
}

/// 프로세스 시작 시각(초, 마이크로초). PID 재사용을 가르는 증거다.
pub fn process_start(pid: i32) -> Option<(u64, u64)> {
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<ProcBsdInfo>::zeroed();
    let size = c_int::try_from(size_of::<ProcBsdInfo>()).ok()?;
    // SAFETY: `info`는 `size`바이트 쓰기 가능한 버퍼다.
    let written = unsafe { proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
    if written != size {
        return None;
    }
    // SAFETY: 0으로 초기화했고 커널이 구조체 전체를 채웠다(`written == size`). 모든 필드는 정수다.
    let info = unsafe { info.assume_init() };
    Some((info.start_tvsec, info.start_tvusec))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningApp {
    pub pid: i32,
    pub name: Option<String>,
    pub bundle_id: Option<String>,
    pub active: bool,
    pub hidden: bool,
}

/// 메인 스레드 런루프를 한 번 비운다. `NSWorkspace` 목록 갱신이 런루프로 오기 때문이다(효과는 미측정).
pub fn pump_run_loop() {
    // SAFETY: CoreFoundation이 내보내는 불변 CFString 상수를 읽는다.
    let mode = unsafe { CF_RUN_LOOP_DEFAULT_MODE };
    // SAFETY: 0초·첫 소스 처리 후 반환이라 막히지 않는다.
    unsafe { CFRunLoopRunInMode(mode, 0.0, 1) };
}

fn ns_string(value: Option<&Retained<NSString>>) -> Option<String> {
    value.map(ToString::to_string)
}

/// 일반(Dock 표시) 앱 목록. `AppKit` 클래스가 없으면 빈 목록이다.
pub fn running_apps(max: usize) -> Vec<RunningApp> {
    autoreleasepool(|_| {
        let Some(class) = AnyClass::get(c"NSWorkspace") else {
            return Vec::new();
        };
        // SAFETY: `sharedWorkspace`는 AppKit 싱글턴을 돌려준다.
        let workspace: Option<Retained<AnyObject>> = unsafe { msg_send![class, sharedWorkspace] };
        let Some(workspace) = workspace else {
            return Vec::new();
        };
        // SAFETY: `runningApplications`는 `NSArray<NSRunningApplication *>`를 돌려준다.
        let apps: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*workspace, runningApplications] };
        let Some(apps) = apps else {
            return Vec::new();
        };
        // SAFETY: `apps`는 NSArray다.
        let count: usize = unsafe { msg_send![&*apps, count] };
        let mut out = Vec::new();
        for index in 0..count {
            if out.len() >= max {
                break;
            }
            // SAFETY: `index < count`이고 원소는 배열이 살아 있는 동안 유효하다.
            let app: *mut AnyObject = unsafe { msg_send![&*apps, objectAtIndex: index] };
            if app.is_null() {
                continue;
            }
            // SAFETY: `app`은 NSRunningApplication이다.
            let policy: isize = unsafe { msg_send![app, activationPolicy] };
            if policy != 0 {
                continue;
            }
            // SAFETY: `app`은 NSRunningApplication이다.
            let pid: i32 = unsafe { msg_send![app, processIdentifier] };
            // SAFETY: `app`은 NSRunningApplication이다.
            let name: Option<Retained<NSString>> = unsafe { msg_send![app, localizedName] };
            // SAFETY: `app`은 NSRunningApplication이다.
            let bundle: Option<Retained<NSString>> = unsafe { msg_send![app, bundleIdentifier] };
            // SAFETY: `app`은 NSRunningApplication이다.
            let active: Bool = unsafe { msg_send![app, isActive] };
            // SAFETY: `app`은 NSRunningApplication이다.
            let hidden: Bool = unsafe { msg_send![app, isHidden] };
            out.push(RunningApp {
                pid,
                name: ns_string(name.as_ref()),
                bundle_id: ns_string(bundle.as_ref()),
                active: active.as_bool(),
                hidden: hidden.as_bool(),
            });
        }
        out
    })
}

pub fn bundle_id_of(pid: i32) -> Option<String> {
    autoreleasepool(|_| {
        let class = AnyClass::get(c"NSRunningApplication")?;
        // SAFETY: 클래스 메서드이며 없는 pid면 nil이다.
        let app: Option<Retained<AnyObject>> =
            unsafe { msg_send![class, runningApplicationWithProcessIdentifier: pid] };
        let app = app?;
        // SAFETY: `app`은 NSRunningApplication이다.
        let bundle: Option<Retained<NSString>> = unsafe { msg_send![&*app, bundleIdentifier] };
        ns_string(bundle.as_ref())
    })
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetFrontProcess(serial: *mut ProcessSerialNumber) -> i32;
    fn GetProcessPID(serial: *const ProcessSerialNumber, pid: *mut i32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRetain(cf: CfTypeRef) -> CfTypeRef;
    fn CFRelease(cf: CfTypeRef);
    fn CFGetTypeID(cf: CfTypeRef) -> CfTypeId;
    fn CFEqual(a: CfTypeRef, b: CfTypeRef) -> Boolean;
    fn CFHash(cf: CfTypeRef) -> usize;
    fn CFStringGetTypeID() -> CfTypeId;
    fn CFStringCreateWithBytes(
        alloc: CfTypeRef,
        bytes: *const u8,
        len: CfIndex,
        encoding: u32,
        external: Boolean,
    ) -> CfTypeRef;
    fn CFStringGetLength(string: CfTypeRef) -> CfIndex;
    fn CFStringGetBytes(
        string: CfTypeRef,
        range: CfRange,
        encoding: u32,
        loss_byte: u8,
        external: Boolean,
        buffer: *mut u8,
        max_len: CfIndex,
        used: *mut CfIndex,
    ) -> CfIndex;
    fn CFArrayGetTypeID() -> CfTypeId;
    fn CFArrayGetCount(array: CfTypeRef) -> CfIndex;
    fn CFArrayGetValueAtIndex(array: CfTypeRef, index: CfIndex) -> CfTypeRef;
    fn CFArrayCreate(
        alloc: CfTypeRef,
        values: *const CfTypeRef,
        count: CfIndex,
        callbacks: *const c_void,
    ) -> CfTypeRef;
    fn CFBooleanGetTypeID() -> CfTypeId;
    fn CFBooleanGetValue(boolean: CfTypeRef) -> Boolean;
    fn CFNumberGetTypeID() -> CfTypeId;
    fn CFNumberGetValue(number: CfTypeRef, kind: CfIndex, out: *mut c_void) -> Boolean;
    fn CFRunLoopRunInMode(mode: CfTypeRef, seconds: f64, return_after_source: Boolean) -> i32;
    #[link_name = "kCFRunLoopDefaultMode"]
    static CF_RUN_LOOP_DEFAULT_MODE: CfTypeRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: c_int) -> CfTypeRef;
    fn AXUIElementCreateSystemWide() -> CfTypeRef;
    fn AXUIElementSetMessagingTimeout(element: CfTypeRef, seconds: f32) -> AxError;
    fn AXUIElementCopyAttributeValue(
        element: CfTypeRef,
        attr: CfTypeRef,
        out: *mut CfTypeRef,
    ) -> AxError;
    fn AXUIElementGetAttributeValueCount(
        element: CfTypeRef,
        attr: CfTypeRef,
        out: *mut CfIndex,
    ) -> AxError;
    fn AXUIElementCopyAttributeValues(
        element: CfTypeRef,
        attr: CfTypeRef,
        index: CfIndex,
        max: CfIndex,
        out: *mut CfTypeRef,
    ) -> AxError;
    fn AXUIElementCopyMultipleAttributeValues(
        element: CfTypeRef,
        attrs: CfTypeRef,
        options: u32,
        out: *mut CfTypeRef,
    ) -> AxError;
    fn AXUIElementCopyActionNames(element: CfTypeRef, out: *mut CfTypeRef) -> AxError;
    fn AXUIElementPerformAction(element: CfTypeRef, action: CfTypeRef) -> AxError;
    fn AXUIElementSetAttributeValue(
        element: CfTypeRef,
        attr: CfTypeRef,
        value: CfTypeRef,
    ) -> AxError;
    fn AXUIElementIsAttributeSettable(
        element: CfTypeRef,
        attr: CfTypeRef,
        out: *mut Boolean,
    ) -> AxError;
    fn AXUIElementGetPid(element: CfTypeRef, out: *mut c_int) -> AxError;
    fn AXUIElementCopyElementAtPosition(
        element: CfTypeRef,
        x: f32,
        y: f32,
        out: *mut CfTypeRef,
    ) -> AxError;
    fn AXValueGetTypeID() -> CfTypeId;
    fn AXValueGetType(value: CfTypeRef) -> u32;
    fn AXValueGetValue(value: CfTypeRef, kind: u32, out: *mut c_void) -> Boolean;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightPostEventAccess() -> bool;
    fn CGEventCreateMouseEvent(source: CfTypeRef, kind: u32, at: CgPoint, button: u32)
    -> CfTypeRef;
    fn CGEventCreateKeyboardEvent(source: CfTypeRef, key: u16, down: bool) -> CfTypeRef;
    fn CGEventKeyboardSetUnicodeString(event: CfTypeRef, len: usize, units: *const u16);
    fn CGEventSetIntegerValueField(event: CfTypeRef, field: u32, value: i64);
    fn CGEventPost(tap: u32, event: CfTypeRef);
}

unsafe extern "C" {
    fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buffer: *mut c_void, size: c_int)
    -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_strings_round_trip_and_truncate_on_utf16_units() {
        let text = "가😀b";
        let cf = cf_string(text).unwrap();
        assert_eq!(string_value(&cf, 100), Some((text.to_owned(), false)));
        // "가" (1 unit) + half of the surrogate pair must not produce broken UTF-8.
        assert_eq!(string_value(&cf, 2), Some(("가".to_owned(), true)));
        assert_eq!(string_value(&cf, 0), Some((String::new(), true)));
        assert_eq!(cf.clone(), cf);
        assert_ne!(cf_string("x").unwrap(), cf);
        assert!(bool_value(&cf).is_none());
        assert!(number_value(&cf).is_none());
        assert!(point_value(&cf).is_none());
    }

    #[test]
    fn process_start_identifies_live_processes_only() {
        let me = i32::try_from(std::process::id()).unwrap();
        let start = process_start(me).unwrap();
        assert!(start.0 > 0);
        assert_eq!(process_start(me), Some(start));
        assert_eq!(process_start(0), None);
        assert_eq!(process_start(-1), None);
    }

    #[test]
    fn attr_batch_keeps_names_alive() {
        let names: Vec<Cf> = ["AXRole", "AXTitle"]
            .iter()
            .map(|n| cf_string(n).unwrap())
            .collect();
        let refs: Vec<&Cf> = names.iter().collect();
        let batch = AttrBatch::new(&refs).unwrap();
        drop(names);
        let items = array_items(&batch.array).unwrap();
        assert_eq!(string_value(&items[1], 16).unwrap().0, "AXTitle");
    }

    #[test]
    fn events_build_without_posting() {
        assert!(mouse_event(true, CgPoint { x: 1.0, y: 1.0 }).is_some());
        let units: Vec<u16> = "hi".encode_utf16().collect();
        assert!(unicode_key_event(true, &units).is_some());
    }
}
