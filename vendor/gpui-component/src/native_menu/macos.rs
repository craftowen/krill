//! macOS native menu implementation (AppKit `NSMenu` via objc2).
//!
//! Krill modification: original image icons are not monochrome AppKit templates.
//! Based on gpui-component (https://github.com/longbridge/gpui-kit), Apache-2.0.

use std::{cell::Cell, sync::Arc};

use gpui::{
    Action, App, AssetSource, DevicePixels, ImageFormat, Pixels, Point, SvgRenderer, Window,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageRep, NSDeviceRGBColorSpace, NSImage, NSMenu, NSMenuItem, NSView,
};
use objc2_foundation::{NSData, NSPoint, NSSize, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::{NativeMenuItem, resolve_icon_image};

/// Side length (in points) menu item images are scaled to. AppKit does not resize the image to fit
/// the row, so a large file would otherwise overflow it.
const MENU_IMAGE_SIZE: f64 = 16.0;

fn contained_menu_image_size(original: NSSize) -> NSSize {
    if !original.width.is_finite()
        || !original.height.is_finite()
        || original.width <= 0.
        || original.height <= 0.
    {
        return NSSize::new(MENU_IMAGE_SIZE, MENU_IMAGE_SIZE);
    }
    let scale = MENU_IMAGE_SIZE / original.width.max(original.height);
    NSSize::new(original.width * scale, original.height * scale)
}

/// Ivars for [`MenuTarget`]: the tag of the selected item, or `-1` if none.
struct MenuTargetIvars {
    selected: Cell<isize>,
}

define_class!(
    // A throwaway Objective-C object that receives the menu item action and
    // records which item (by tag) was clicked.
    #[unsafe(super(NSObject))]
    #[name = "GPUIComponentNativeMenuTarget"]
    #[ivars = MenuTargetIvars]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(menuItemClicked:))]
        fn menu_item_clicked(&self, sender: &NSMenuItem) {
            self.ivars().selected.set(sender.tag());
        }
    }
);

impl MenuTarget {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(MenuTargetIvars {
            selected: Cell::new(-1),
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// Show a native popup menu and dispatch the selected item's action.
///
/// The AppKit tracking loop is run from a foreground task so that GPUI is not
/// borrowed while the menu is open.
pub(super) fn show(
    items: Vec<NativeMenuItem>,
    asset_source: Arc<dyn AssetSource>,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(view_ptr) = ns_view_ptr(window) else {
        return;
    };
    // Inherent `Window::window_handle` (GPUI's `AnyWindowHandle`), not the
    // `raw_window_handle::HasWindowHandle` trait method in scope below.
    let handle = Window::window_handle(window);
    let svg_renderer = cx.svg_renderer();

    cx.spawn(async move |cx| {
        let action = run_menu(
            view_ptr,
            &items,
            asset_source.as_ref(),
            &svg_renderer,
            position,
        );
        let _ = cx.update(move |app| {
            let _ = handle.update(app, move |_, window, app| {
                if let Some(action) = action {
                    window.dispatch_action(action, app);
                }
                // Wake GPUI after the AppKit tracking loop returns so the window
                // resumes painting and re-registers its mouse handlers. Without
                // this, a dismissed menu (especially when nothing is selected)
                // leaves the window idle and unresponsive to a second
                // right-click.
                window.refresh();
            });
        });
    })
    .detach();
}

/// Build the menu (recursively, including submenus), show it, and return the
/// selected item's action.
fn run_menu(
    view_ptr: usize,
    items: &[NativeMenuItem],
    asset_source: &dyn AssetSource,
    svg_renderer: &SvgRenderer,
    position: Point<Pixels>,
) -> Option<Box<dyn Action>> {
    let mtm = MainThreadMarker::new()?;
    // SAFETY: `view_ptr` came from the window's AppKit handle, and the window
    // outlives this synchronous call.
    let view: &NSView = unsafe { &*(view_ptr as *const NSView) };

    let target = MenuTarget::new();
    let mut actions: Vec<&Box<dyn Action>> = Vec::new();
    let ns_menu = build_menu(
        items,
        asset_source,
        svg_renderer,
        &target,
        mtm,
        &mut actions,
    );

    // `position` is window-relative, logical pixels, origin top-left (GPUI).
    // AppKit view coordinates have their origin at the bottom-left, so flip y.
    let height = view.bounds().size.height;
    let location = NSPoint::new(
        f32::from(position.x) as f64,
        height - f32::from(position.y) as f64,
    );
    ns_menu.popUpMenuPositioningItem_atLocation_inView(None, location, Some(view));

    let tag = target.ivars().selected.get();
    if tag >= 0 {
        actions.get(tag as usize).map(|action| action.boxed_clone())
    } else {
        None
    }
}

/// Recursively build an `NSMenu`. Each actionable leaf item is given a tag equal
/// to its index in `actions`, so the selected tag maps back to its action.
fn build_menu<'a>(
    items: &'a [NativeMenuItem],
    asset_source: &dyn AssetSource,
    svg_renderer: &SvgRenderer,
    target: &MenuTarget,
    mtm: MainThreadMarker,
    actions: &mut Vec<&'a Box<dyn Action>>,
) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    // Items are configured explicitly, so disable AppKit's automatic enabling.
    menu.setAutoenablesItems(false);

    for item in items {
        match item {
            NativeMenuItem::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            NativeMenuItem::Item {
                label,
                disabled,
                checked,
                icon,
                action,
            } => {
                let ns_item = NSMenuItem::new(mtm);
                unsafe {
                    ns_item.setTitle(&NSString::from_str(label));
                    ns_item.setEnabled(!*disabled);
                    if let Some(icon) = icon {
                        if let Some(ns_image) = ns_image_for_icon(icon, asset_source, svg_renderer)
                        {
                            let original_image =
                                matches!(icon.source_ref(), crate::icon::IconSource::Image { .. });
                            let size = if original_image {
                                contained_menu_image_size(ns_image.size())
                            } else {
                                NSSize::new(MENU_IMAGE_SIZE, MENU_IMAGE_SIZE)
                            };
                            ns_image.setSize(size);
                            ns_image.setTemplate(!original_image);
                            ns_item.setImage(Some(&ns_image));
                        }
                    }

                    if *checked {
                        // `NSControlStateValueOn`
                        ns_item.setState(1);
                    }
                    if let Some(action) = action {
                        if !*disabled {
                            ns_item.setTag(actions.len() as isize);
                            actions.push(action);
                            ns_item.setTarget(Some(target as &AnyObject));
                            ns_item.setAction(Some(sel!(menuItemClicked:)));
                        }
                    }
                }
                menu.addItem(&ns_item);
            }
            NativeMenuItem::Submenu {
                label,
                disabled,
                items,
            } => {
                let ns_item = NSMenuItem::new(mtm);
                let submenu = build_menu(items, asset_source, svg_renderer, target, mtm, actions);
                ns_item.setTitle(&NSString::from_str(label));
                ns_item.setEnabled(!*disabled);
                ns_item.setSubmenu(Some(&submenu));
                menu.addItem(&ns_item);
            }
        }
    }

    menu
}

fn ns_image_for_icon(
    icon: &crate::Icon,
    asset_source: &dyn AssetSource,
    svg_renderer: &SvgRenderer,
) -> Option<Retained<NSImage>> {
    let image = resolve_icon_image(icon, asset_source)?;
    if image.bytes.is_empty() {
        return None;
    }

    if image.format == ImageFormat::Svg
        && matches!(icon.source_ref(), crate::icon::IconSource::Image { .. })
    {
        // AppKit's SVG decoder ignores filters used by official brand art.
        // Rasterize with the same GPUI renderer as the app, then hand native
        // AppKit a bitmap instead of asking it to reinterpret the SVG.
        let rendered =
            crate::icon::raster_icon_image(&image, DevicePixels(32), svg_renderer).ok()?;
        return ns_image_from_raster(&rendered);
    }

    let data = unsafe {
        NSData::dataWithBytes_length(image.bytes.as_ptr().cast(), image.bytes.len() as _)
    };
    NSImage::initWithData(NSImage::alloc(), &data)
}

fn ns_image_from_raster(rendered: &gpui::RenderImage) -> Option<Retained<NSImage>> {
    let size = rendered.size(0);
    let width = isize::try_from(i32::from(size.width)).ok()?;
    let height = isize::try_from(i32::from(size.height)).ok()?;
    let row_bytes = width.checked_mul(4)?;
    let byte_count = usize::try_from(row_bytes.checked_mul(height)?).ok()?;
    let source = rendered.as_bytes(0)?;
    if source.len() != byte_count {
        return None;
    }
    // SAFETY: AppKit provides this immutable color-space name for the process lifetime.
    let color_space = unsafe { NSDeviceRGBColorSpace };
    // SAFETY: null planes request AppKit-owned storage for a non-planar,
    // four-channel bitmap; the dimensions and row stride are checked above.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(), std::ptr::null_mut(), width, height, 8, 4,
            true, false, color_space, NSBitmapFormat::AlphaNonpremultiplied, row_bytes, 32,
        )
    }?;
    let destination = bitmap.bitmapData();
    if destination.is_null() {
        return None;
    }
    // SAFETY: bitmap owns at least byte_count writable bytes, allocated using
    // the exact dimensions and stride above, and stays alive throughout copy.
    let destination = unsafe { std::slice::from_raw_parts_mut(destination, byte_count) };
    destination.copy_from_slice(source);
    for pixel in destination.chunks_exact_mut(4) {
        // GPUI returns straight-alpha BGRA; AppKit's nonpremultiplied bitmap is RGBA.
        pixel.swap(0, 2);
    }
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(width as f64, height as f64));
    image.addRepresentation(&bitmap);
    Some(image)
}

/// Extract the AppKit `NSView` pointer from the window's raw handle.
fn ns_view_ptr(window: &Window) -> Option<usize> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    Some(handle.ns_view.as_ptr() as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_menu_images_keep_aspect_inside_the_icon_box() {
        for original in [NSSize::new(466.73, 532.09), NSSize::new(1000., 500.)] {
            let fitted = contained_menu_image_size(original);
            assert!(
                (fitted.width / fitted.height - original.width / original.height).abs() < 1e-12
            );
            assert!(fitted.width <= MENU_IMAGE_SIZE && fitted.height <= MENU_IMAGE_SIZE);
            assert!((fitted.width.max(fitted.height) - MENU_IMAGE_SIZE).abs() < 1e-12);
        }
        let empty = contained_menu_image_size(NSSize::new(0., 0.));
        assert_eq!(empty.width, MENU_IMAGE_SIZE);
        assert_eq!(empty.height, MENU_IMAGE_SIZE);
    }

    #[test]
    fn native_image_representation_keeps_raster_colors_and_alpha() {
        let bytes = br##"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="500"><path d="M0 0H500V500H0Z" fill="#ff0000"/><path d="M500 0H1000V500H500Z" fill="#00ff00" opacity=".5"/></svg>"##;
        let image = gpui::Image::from_bytes(ImageFormat::Svg, bytes.to_vec());
        let renderer = SvgRenderer::new(Arc::new(()));
        let raster = crate::icon::raster_icon_image(&image, DevicePixels(32), &renderer).unwrap();
        let native = ns_image_from_raster(&raster).unwrap();
        assert_eq!(native.size(), NSSize::new(32., 16.));
        let representations = native.representations();
        let representation = representations.objectAtIndex(0);
        let bitmap = (representation.as_ref() as &AnyObject)
            .downcast_ref::<NSBitmapImageRep>()
            .unwrap();
        assert_eq!(bitmap.bytesPerRow(), 32 * 4);
        let source = raster.as_bytes(0).unwrap();
        // SAFETY: the retained bitmap has the asserted stride and 16 rows;
        // its backing storage stays alive through this read-only comparison.
        let copied = unsafe { std::slice::from_raw_parts(bitmap.bitmapData(), source.len()) };
        for (bgra, rgba) in source.chunks_exact(4).zip(copied.chunks_exact(4)) {
            assert_eq!(rgba, [bgra[2], bgra[1], bgra[0], bgra[3]]);
        }
        assert!(copied.chunks_exact(4).any(|p| p == [255, 0, 0, 255]));
        // GPUI's f32 alpha demultiplication truncates some 255 channels to 254.
        // The exact source-to-native byte comparison above is the preservation check.
        assert!(
            copied
                .chunks_exact(4)
                .any(|p| { p[0] == 0 && p[1] >= 254 && p[2] == 0 && p[3] > 0 && p[3] < 255 })
        );
    }
}
