ObjC.import("AppKit");
ObjC.import("CoreGraphics");
function captureGeometryFacts(pid, id) {
  pid = Number(pid); id = String(id);
  const raw = $.CGWindowListCopyWindowInfo(1, 0);
  if (!raw) throw Error("window census unavailable");
  const rows = ObjC.deepUnwrap(ObjC.castRefToObject(raw));
  const matches = rows.filter(w => Number(w.kCGWindowOwnerPID) === pid &&
    String(w.kCGWindowNumber) === id && Number(w.kCGWindowLayer) === 0);
  if (matches.length !== 1) throw Error("window geometry unavailable or ambiguous");
  const b = matches[0].kCGWindowBounds;
  const screens = $.NSScreen.screens, count = Number(screens.count);
  if (count < 1 || count > 16) throw Error("display census outside bounded contract");
  const displays = [];
  for (let i = 0; i < count; i++) {
    const screen = screens.objectAtIndex(i);
    const display = Number(ObjC.unwrap(screen.deviceDescription.objectForKey("NSScreenNumber")));
    const frame = $.CGDisplayBounds(display);
    const mode = $.CGDisplayCopyDisplayMode(display);
    if (!mode || (typeof mode.isNil === "function" && mode.isNil())) throw Error("display mode unavailable");
    let pixelWidth, pixelHeight;
    try {
      pixelWidth = Number($.CGDisplayModeGetPixelWidth(mode));
      pixelHeight = Number($.CGDisplayModeGetPixelHeight(mode));
      if (!Number.isInteger(pixelWidth) || pixelWidth <= 0 || pixelWidth > 4294967295 ||
          !Number.isInteger(pixelHeight) || pixelHeight <= 0 || pixelHeight > 4294967295)
        throw Error("display mode pixels unavailable or outside bounded contract");
    } finally {
      $.CGDisplayModeRelease(mode);
    }
    displays.push({id:display,
      logicalBounds:{x:Number(frame.origin.x),y:Number(frame.origin.y),
        width:Number(frame.size.width),height:Number(frame.size.height)},
      pixelWidth:pixelWidth,
      pixelHeight:pixelHeight,
      backingScaleFactor:Number(screen.backingScaleFactor),
      rotationDegrees:Number($.CGDisplayRotation(display))});
  }
  return {bounds:{x:Number(b.X),y:Number(b.Y),
    width:Number(b.Width),height:Number(b.Height)},displays:displays};
}
