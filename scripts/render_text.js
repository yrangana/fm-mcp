// Renders lines of text as black on white into a PNG, for the smoke test's ocr checks.
// Usage: osascript -l JavaScript render_text.js <out.png> [line ...]
ObjC.import('AppKit');
function run(argv) {
  const out = argv[0], lines = argv.slice(1);
  const w = 800, h = 60 + lines.length * 48;
  const rep = $.NSBitmapImageRep.alloc.initWithBitmapDataPlanesPixelsWidePixelsHighBitsPerSampleSamplesPerPixelHasAlphaIsPlanarColorSpaceNameBytesPerRowBitsPerPixel(null, w, h, 8, 4, true, false, $.NSDeviceRGBColorSpace, 0, 0);
  $.NSGraphicsContext.saveGraphicsState;
  $.NSGraphicsContext.setCurrentContext($.NSGraphicsContext.graphicsContextWithBitmapImageRep(rep));
  $.NSColor.whiteColor.setFill; $.NSRectFill($.NSMakeRect(0, 0, w, h));
  const attrs = $({
    [$.NSFontAttributeName.js]: $.NSFont.systemFontOfSize(32), [$.NSForegroundColorAttributeName.js]: $.NSColor.blackColor});
  lines.forEach((t, i) => $(t).drawAtPointWithAttributes($.NSMakePoint(30, h - 70 - i * 48), attrs));
  $.NSGraphicsContext.restoreGraphicsState;
  rep.representationUsingTypeProperties($.NSBitmapImageFileTypePNG, $()).writeToFileAtomically(out, true);
}
