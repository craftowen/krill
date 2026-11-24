use super::*;

#[gpui::test]
fn menu_bar_icon_preserves_every_raster_row(cx: &mut gpui::TestAppContext) {
    cx.read(|cx| {
        objc2::rc::autoreleasepool(|_| {
            let edge = crate::ui::MENU_BAR_ICON_SIZE;
            let renderer = cx.svg_renderer();
            let svg = renderer.parse_svg(crate::ui::icons::KRILL_SVG).unwrap();
            let raster = renderer
                .render_parsed(
                    &svg,
                    SvgSize::ExactSize(size(DevicePixels(edge), DevicePixels(edge))),
                )
                .unwrap();
            let pixels = raster.as_bytes(0).unwrap();
            assert!(pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0));
            assert!(pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[3] == 0));

            // 실제 메뉴바에 전달하는 NSImage를 다시 읽어 TIFF 변환까지 거친 모양을 검사한다.
            let image = icon(cx).unwrap();
            // SAFETY: image는 icon이 반환한 살아 있는 NSImage다.
            let template: bool = unsafe { msg_send![&*image, isTemplate] };
            assert!(template);
            // SAFETY: image는 살아 있고 TIFFRepresentation은 현재 autorelease 스코프에서 유효하다.
            let tiff: *mut AnyObject = unsafe { msg_send![&*image, TIFFRepresentation] };
            assert!(!tiff.is_null());
            let class = AnyClass::get(c"NSBitmapImageRep").unwrap();
            // SAFETY: alloc이 initWithData까지 소유할 NSBitmapImageRep를 반환한다.
            let bitmap: *mut AnyObject = unsafe { msg_send![class, alloc] };
            // SAFETY: tiff는 위 NSImage가 생성한 유효한 NSData다.
            let bitmap: *mut AnyObject = unsafe { msg_send![bitmap, initWithData: tiff] };
            // SAFETY: init 계열의 +1 소유권을 RAII로 옮긴다.
            let bitmap = unsafe { Retained::from_raw(bitmap) }.unwrap();
            // SAFETY: bitmap은 살아 있는 NSBitmapImageRep다.
            let width: isize = unsafe { msg_send![&*bitmap, pixelsWide] };
            // SAFETY: bitmap은 살아 있는 NSBitmapImageRep다.
            let height: isize = unsafe { msg_send![&*bitmap, pixelsHigh] };
            // SAFETY: bitmap은 살아 있는 NSBitmapImageRep다.
            let samples: isize = unsafe { msg_send![&*bitmap, samplesPerPixel] };
            assert_eq!(width, isize::try_from(edge).unwrap());
            assert_eq!(height, width);
            assert_eq!(samples, 4);

            let row_bytes = usize::try_from(edge).unwrap() * 4;
            for (y, row) in pixels.chunks_exact(row_bytes).enumerate() {
                for (x, pixel) in row.as_chunks::<4>().0.iter().enumerate() {
                    let mut actual = [0_usize; 4];
                    // SAFETY: x/y는 검사한 크기 안에 있고 actual은 samplesPerPixel개의 NSUInteger를 담는다.
                    unsafe {
                        let (): () = msg_send![&*bitmap, getPixel: actual.as_mut_ptr(),
                        atX: isize::try_from(x).unwrap(), y: isize::try_from(y).unwrap()];
                    }
                    // 템플릿 이미지의 윤곽은 알파로 결정된다. 모든 픽셀을 비교해 행 밀림을 잡는다.
                    assert_eq!(actual[3], usize::from(pixel[3]), "alpha at ({x}, {y})");
                }
            }
        });
    });
}
