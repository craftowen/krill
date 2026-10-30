//! `NSStatusItem` 소유권과 메뉴 콜백. `AppKit` 호출은 모두 gpui 메인 스레드에서 한다.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use gpui::{App, DevicePixels, SvgSize, size};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
use objc2::{msg_send, sel};

static COMMAND: AtomicU8 = AtomicU8::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    Quit,
}

/// status bar가 item을 보유하지만 target은 보유하지 않는다. 둘 다 앱 수명 동안 보관한다.
#[derive(Debug)]
pub struct StatusItem {
    bar: Retained<AnyObject>,
    item: Retained<AnyObject>,
    _target: Retained<AnyObject>,
    labels: [Retained<AnyObject>; 2],
}

impl StatusItem {
    pub fn new(open_label: &str, quit_label: &str, cx: &App) -> Option<Self> {
        if cx.is_test() {
            return None;
        }
        let image = icon(cx)?;
        let bar_class = AnyClass::get(c"NSStatusBar")?;
        // SAFETY: AppKit `systemStatusBar`는 메인 스레드에서 공유 인스턴스를 반환한다.
        let bar: *mut AnyObject = unsafe { msg_send![bar_class, systemStatusBar] };
        // SAFETY: 위 autoreleased 객체를 RAII 핸들이 보유한다.
        let bar = unsafe { Retained::retain(bar) }?;
        // SAFETY: 등록한 NSObject 하위 클래스는 인자 없는 new를 상속한다.
        let target: Retained<AnyObject> = unsafe { msg_send![target_class()?, new] };
        let menu_class = AnyClass::get(c"NSMenu")?;
        // SAFETY: NSMenu new는 소유된 메뉴를 반환한다.
        let menu: Retained<AnyObject> = unsafe { msg_send![menu_class, new] };
        let open = add_item(&menu, open_label, sel!(openKrill:), &target)?;
        let quit = add_item(&menu, quit_label, sel!(quitKrill:), &target)?;
        // SAFETY: -1.0은 NSVariableStatusItemLength이고 bar는 살아 있는 NSStatusBar다.
        let item: *mut AnyObject = unsafe { msg_send![&*bar, statusItemWithLength: -1.0_f64] };
        // SAFETY: 위 status item을 removeStatusItem까지 보유한다.
        let item = unsafe { Retained::retain(item) }?;
        // SAFETY: status item은 menu를 보유하고 target은 Self가 보유한다.
        let (): () = unsafe { msg_send![&*item, setMenu: &*menu] };
        // SAFETY: status item의 button은 빌린 NSStatusBarButton이다.
        let button: *mut AnyObject = unsafe { msg_send![&*item, button] };
        if !button.is_null() {
            // SAFETY: button은 살아 있고 image는 NSImage다. button이 보유한다.
            let (): () = unsafe { msg_send![button, setImage: &*image] };
            // SAFETY: NSImageLeft = 2로 이미지를 제목 왼쪽에 표시한다.
            let (): () = unsafe { msg_send![button, setImagePosition: 2_usize] };
        }
        let result = Self {
            bar,
            item,
            _target: target,
            labels: [open, quit],
        };
        result.set_title(crate::i18n::catalog::security::APP_MENU.get());
        Some(result)
    }

    pub fn set_menu_labels(&self, open: &str, quit: &str) {
        for (item, label) in self.labels.iter().zip([open, quit]) {
            let Some(label) = super::macos::cf_string(label) else {
                continue;
            };
            // SAFETY: item is a retained NSMenuItem on the UI thread; setTitle copies NSString.
            let (): () = unsafe { msg_send![&**item, setTitle: label.cast::<AnyObject>()] };
            super::macos::cf_release(label);
        }
    }

    pub fn set_title(&self, title: &str) {
        let Some(title) = super::macos::cf_string(title) else {
            return;
        };
        // SAFETY: item은 이 핸들이 보유한 NSStatusItem이다.
        let button: *mut AnyObject = unsafe { msg_send![&*self.item, button] };
        if !button.is_null() {
            // SAFETY: CFString은 NSString으로 bridge되며 setTitle이 복사한다.
            let (): () = unsafe { msg_send![button, setTitle: title.cast::<AnyObject>()] };
            // SAFETY: setToolTip은 같은 유효한 NSString을 복사한다.
            let (): () = unsafe { msg_send![button, setToolTip: title.cast::<AnyObject>()] };
        }
        super::macos::cf_release(title);
    }

    pub fn take_command() -> Option<Command> {
        match COMMAND.swap(0, Ordering::AcqRel) {
            1 => Some(Command::Open),
            2 => Some(Command::Quit),
            _ => None,
        }
    }
}

impl Drop for StatusItem {
    fn drop(&mut self) {
        // SAFETY: bar가 만든 살아 있는 item을 메인 스레드에서 한 번만 제거한다.
        let (): () = unsafe { msg_send![&*self.bar, removeStatusItem: &*self.item] };
    }
}

extern "C" fn open(_: &AnyObject, _: Sel, _: *mut AnyObject) {
    COMMAND.store(1, Ordering::Release);
}

extern "C" fn quit(_: &AnyObject, _: Sel, _: *mut AnyObject) {
    COMMAND.store(2, Ordering::Release);
}

fn target_class() -> Option<&'static AnyClass> {
    static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    *CLASS.get_or_init(|| {
        let mut class = ClassBuilder::new(c"MKMenuBarTarget", AnyClass::get(c"NSObject")?)?;
        // SAFETY: selector와 callback의 NSObject target-action v@:@ 타입이 일치한다.
        // callback은 atomic store만 하므로 panic이 C ABI를 넘지 않는다.
        unsafe { class.add_method(sel!(openKrill:), open as extern "C" fn(_, _, _)) };
        // SAFETY: 같은 target-action 타입이며 callback은 atomic store만 한다.
        unsafe { class.add_method(sel!(quitKrill:), quit as extern "C" fn(_, _, _)) };
        Some(class.register())
    })
}

fn add_item(
    menu: &AnyObject,
    label: &str,
    action: Sel,
    target: &AnyObject,
) -> Option<Retained<AnyObject>> {
    let label = super::macos::cf_string(label)?;
    let empty = super::macos::cf_string("")?;
    // SAFETY: NSMenu가 NSString 제목과 key equivalent를 복사하며 action은 등록한 selector다.
    let item: *mut AnyObject = unsafe {
        msg_send![menu, addItemWithTitle: label.cast::<AnyObject>(), action: action,
            keyEquivalent: empty.cast::<AnyObject>()]
    };
    super::macos::cf_release(label);
    super::macos::cf_release(empty);
    if item.is_null() {
        return None;
    }
    // SAFETY: menu가 보유한 NSMenuItem에 Self가 보유한 살아 있는 target을 설정한다.
    let (): () = unsafe { msg_send![item, setTarget: target] };
    // SAFETY: menu owns this non-null NSMenuItem; retain keeps it alive with StatusItem.
    unsafe { Retained::retain(item) }
}

/// 앱의 Krill 모노크롬 SVG를 gpui로 래스터화하여 template `NSImage`를 만든다.
fn icon(cx: &App) -> Option<Retained<AnyObject>> {
    let renderer = cx.svg_renderer();
    let svg = if let Ok(parsed) = renderer.parse_svg(crate::ui::icons::KRILL_SVG) {
        parsed
    } else {
        let path = crate::ui::MENU_BAR_ICON.path();
        let bytes = cx.asset_source().load(&path).ok()??;
        renderer.parse_svg(&bytes).ok()?
    };
    let edge = crate::ui::MENU_BAR_ICON_SIZE;
    let bitmap_edge = isize::try_from(edge).ok()?;
    let row_bytes = bitmap_edge.checked_mul(4)?;
    let byte_len = usize::try_from(row_bytes.checked_mul(bitmap_edge)?).ok()?;
    let image = renderer
        .render_parsed(
            &svg,
            SvgSize::ExactSize(size(DevicePixels(edge), DevicePixels(edge))),
        )
        .ok()?;
    let pixels = image.as_bytes(0)?;
    if pixels.len() != byte_len {
        return None;
    }
    let bitmap_class = AnyClass::get(c"NSBitmapImageRep")?;
    let color = super::macos::cf_string("NSDeviceRGBColorSpace")?;
    // SAFETY: alloc이 init까지 소유할 NSBitmapImageRep를 반환한다.
    let bitmap: *mut AnyObject = unsafe { msg_send![bitmap_class, alloc] };
    // bytesPerRow=0은 AppKit이 행을 패딩하므로 조밀한 gpui 버퍼와 같은 행 간격을 명시한다.
    // SAFETY: planes=nil이면 AppKit이 지정한 행 간격과 높이의 4채널 비트맵 영역을 할당한다.
    let bitmap: *mut AnyObject = unsafe {
        msg_send![bitmap, initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>(),
            pixelsWide: bitmap_edge, pixelsHigh: bitmap_edge,
            bitsPerSample: 8_isize, samplesPerPixel: 4_isize, hasAlpha: true,
            isPlanar: false, colorSpaceName: color.cast::<AnyObject>(),
            bytesPerRow: row_bytes, bitsPerPixel: 32_isize]
    };
    super::macos::cf_release(color);
    // SAFETY: init 계열이 반환한 +1 소유권을 RAII로 옮긴다.
    let bitmap = unsafe { Retained::from_raw(bitmap) }?;
    // SAFETY: bitmap은 위에서 생성한 살아 있는 NSBitmapImageRep다.
    let stride: isize = unsafe { msg_send![&*bitmap, bytesPerRow] };
    // SAFETY: 비 planar bitmap의 bytesPerPlane은 할당된 단일 평면의 바이트 수다.
    let capacity: isize = unsafe { msg_send![&*bitmap, bytesPerPlane] };
    if stride != row_bytes || usize::try_from(capacity).ok()? < byte_len {
        return None;
    }
    // SAFETY: 비 planar bitmapData가 위에서 할당한 연속 4채널 영역을 반환한다.
    let data: *mut u8 = unsafe { msg_send![&*bitmap, bitmapData] };
    if data.is_null() {
        return None;
    }
    // gpui의 검정 모노 BGRA는 RGB가 0이어서 AppKit RGBA에서도 같은 알파 마스크다.
    // SAFETY: 입력 길이·행 간격·할당 크기를 확인했다. 별도 할당된 두 영역은 겹치지 않는다.
    unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), data, pixels.len()) };
    // SAFETY: 유효한 bitmap의 TIFFRepresentation은 이 스코프에서 유효한 NSData다.
    let tiff: *mut AnyObject = unsafe { msg_send![&*bitmap, TIFFRepresentation] };
    let class = AnyClass::get(c"NSImage")?;
    // SAFETY: alloc한 NSImage를 즉시 init한다.
    let image: *mut AnyObject = unsafe { msg_send![class, alloc] };
    // SAFETY: TIFF 데이터는 bitmap이 만든 유효한 NSData이며 initWithData가 내용을 읽는다.
    let image: *mut AnyObject = unsafe { msg_send![image, initWithData: tiff] };
    // SAFETY: init 계열이 반환한 +1 소유권을 RAII로 옮긴다.
    let image = unsafe { Retained::from_raw(image) }?;
    // SAFETY: template 이미지 색은 macOS가 밝은/어두운 메뉴에 맞춘다.
    let (): () = unsafe { msg_send![&*image, setTemplate: true] };
    Some(image)
}

#[cfg(test)]
mod tests;
