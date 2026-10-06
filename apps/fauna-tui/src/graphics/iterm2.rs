//! The iTerm2 inline-images encoder (OSC 1337 `File=`).
//!
//! The simplest arm: iTerm2 takes an ordinary **image file**, base64'd, and
//! scales it into a cell box. So unlike kitty (raw RGB) and sixel (a palette and
//! a band encoder), the work here is re-encoding the pixels to PNG and wrapping
//! them.
//!
//! Like sixel and unlike kitty, the image is composited into the text grid, so
//! repainting the cells underneath erases it and there is nothing to delete
//! (`super::Painter`).
//!
//! Unreachable inside a multiplexer with passthrough off (`super::detect` module
//! docs).

use image::{ImageEncoder, RgbImage};

/// Operating System Command + iTerm2's private code.
const INTRODUCER: &[u8] = b"\x1b]1337;";

/// BEL — OSC's terminator.
const TERMINATOR: &[u8] = b"\x07";

/// Encode `image` as an inline-image sequence occupying `cells` (cols, rows).
///
/// The caller positions the cursor first (`super::Painter`). Returns an empty
/// sequence if the pixels will not PNG-encode — a picture we cannot send is the
/// half-block fallback's problem, never an error to surface (`ui/media.md`
/// § Thumbnails).
pub fn encode(image: &RgbImage, cells: (u16, u16)) -> Vec<u8> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let Some(png) = png_bytes(image) else {
        return Vec::new();
    };
    let payload = base64_of(&png);
    let (cols, rows) = cells;

    // `inline=1` paints it rather than offering it as a download.
    // `width`/`height` are bare numbers, which iTerm2 reads as **cells**.
    // `preserveAspectRatio=0`: the box we computed already holds the aspect
    // (`super::Painter`), and letting iTerm2 letterbox it a second time would
    // leave the picture floating inside its own cells.
    let args = format!(
        "File=inline=1;width={cols};height={rows};preserveAspectRatio=0;size={}",
        png.len()
    );

    let mut out = Vec::with_capacity(payload.len() + 64);
    out.extend_from_slice(INTRODUCER);
    out.extend_from_slice(args.as_bytes());
    out.push(b':');
    out.extend_from_slice(payload.as_bytes());
    out.extend_from_slice(TERMINATOR);
    out
}

/// Re-encode the pixels as PNG — the container iTerm2 wants.
///
/// PNG rather than JPEG: lossless, and the workspace `image` pin already carries
/// the encoder, so this adds nothing to the dependency graph.
fn png_bytes(image: &RgbImage) -> Option<Vec<u8>> {
    let mut buffer = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buffer)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(buffer)
}

fn base64_of(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn an_image_is_a_well_formed_inline_file_sequence() {
        let rendered = text(&encode(
            &RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3])),
            (4, 2),
        ));

        assert!(rendered.starts_with("\x1b]1337;"), "introducer: {rendered}");
        assert!(rendered.ends_with('\x07'), "BEL terminator");
        assert!(rendered.contains("File=inline=1"), "inline: {rendered}");
        assert!(
            rendered.contains("width=4;height=2"),
            "cell box: {rendered}"
        );
        assert!(
            rendered.contains("preserveAspectRatio=0"),
            "no second letterbox"
        );
    }

    #[test]
    fn the_payload_is_a_real_png_of_the_pixels() {
        // The one assertion that proves we sent a *picture* and not a wrapper
        // around garbage: decode it back and compare.
        use base64::Engine as _;
        let source = RgbImage::from_pixel(3, 2, image::Rgb([9, 8, 7]));
        let rendered = text(&encode(&source, (3, 1)));

        let payload = rendered
            .split_once(':')
            .expect("a payload follows the args")
            .1
            .trim_end_matches('\x07');
        let png = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("payload is base64");
        assert_eq!(&png[1..4], b"PNG", "PNG magic: {:?}", &png[..8]);

        let decoded = image::load_from_memory(&png)
            .expect("payload decodes")
            .to_rgb8();
        assert_eq!(decoded.dimensions(), (3, 2));
        assert_eq!(decoded.get_pixel(1, 1).0, [9, 8, 7]);
    }

    #[test]
    fn the_declared_size_matches_the_payload() {
        // iTerm2 reads `size` to know when the file is complete; a wrong value
        // leaves it waiting or truncating.
        use base64::Engine as _;
        let rendered = text(&encode(&RgbImage::new(8, 8), (2, 1)));
        let declared: usize = rendered
            .split("size=")
            .nth(1)
            .and_then(|rest| rest.split(':').next())
            .and_then(|value| value.parse().ok())
            .expect("a declared size");
        let payload = rendered
            .split_once(':')
            .expect("a payload")
            .1
            .trim_end_matches('\x07');
        let png = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("base64");
        assert_eq!(declared, png.len());
    }

    #[test]
    fn an_empty_image_encodes_to_nothing_rather_than_a_malformed_sequence() {
        assert!(encode(&RgbImage::new(0, 0), (1, 1)).is_empty());
    }
}
