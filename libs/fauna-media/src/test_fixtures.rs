//! Shared test-fixture image construction, behind the `test-fixtures`
//! feature. Six copies of the same `image::RgbImage` → PNG-encode boilerplate
//! had accumulated across this crate's own tests and three downstream
//! crates' tests (`fauna-ffi`, `fauna-sync-engine` ×2, `fauna-media-machine`)
//! — each its own compilation unit, so the compiler exerted no
//! anti-duplication pressure. This is the one canonical body.

/// A `w`×`h` RGB image, gradient-filled across all three channels so the
/// pixel data is never trivial/uniform (some codecs/validators special-case
/// an all-one-color image).
pub fn build_rgb_image(w: u32, h: u32) -> image::DynamicImage {
    let mut img = image::RgbImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        *p = image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
    }
    image::DynamicImage::ImageRgb8(img)
}

/// A minimal valid `w`×`h` PNG — the fixture every "does `process_media`
/// render a thumbnail" test needs.
pub fn build_png(w: u32, h: u32) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    build_rgb_image(w, h)
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("PNG encode of a test fixture cannot fail");
    buf.into_inner()
}
