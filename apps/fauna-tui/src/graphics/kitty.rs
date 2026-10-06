//! The kitty graphics-protocol encoder.
//!
//! The richest of the three arms: kitty takes 24-bit RGB directly — no palette,
//! no quantization — and keeps images on **their own layer**, above the text
//! grid. That last property is the whole reason [`delete`] exists: a kitty image
//! is not made of cells, so repainting the cells underneath does not erase it.
//! Every other arm is cleaned up by the terminal itself when the text below is
//! redrawn; this one has to be told (`super::Painter`).
//!
//! Payloads are base64 and chunked, because the protocol caps an escape code's
//! payload — a thumbnail exceeds it comfortably.
//!
//! Unreachable inside a multiplexer with passthrough off, which is the recorded
//! user's terminal (`super::detect` module docs) — but a native kitty window is
//! the common case for everyone else, and it is a dozen lines on top of the seam
//! sixel already pays for.

use image::RgbImage;

/// Application Program Command introducer + the graphics `G`.
const INTRODUCER: &[u8] = b"\x1b_G";

/// String Terminator.
const TERMINATOR: &[u8] = b"\x1b\\";

/// Max base64 payload per escape code, per the protocol.
const CHUNK: usize = 4096;

/// Encode `image` as a transmit-and-display sequence at `cells` (cols, rows),
/// tagged `id` so [`delete`] can find it later.
///
/// The caller positions the cursor first — the image lands at the cursor
/// (`super::Painter`).
pub fn encode(image: &RgbImage, cells: (u16, u16), id: u32) -> Vec<u8> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let payload = base64_of(image.as_raw());
    let (cols, rows) = cells;

    // `a=T` transmit-and-display, `f=24` raw 24-bit RGB, `s`/`v` the pixel
    // dimensions that describes, `c`/`r` the cell box to scale into, `i` the
    // handle for deletion.
    //
    // `q=2` suppresses kitty's replies. It is not an optimization: a reply lands
    // on **stdin**, where the event stream is reading keystrokes, and an
    // unsolicited `ESC_Gi=1;OK ESC\` arriving mid-session is at best discarded
    // and at worst decoded as input. We have no reader for it, so we must not
    // ask for it.
    let mut header = format!("a=T,f=24,s={width},v={height},c={cols},r={rows},i={id},q=2");

    let mut out = Vec::with_capacity(payload.len() + 64);
    let mut chunks = payload.as_bytes().chunks(CHUNK).peekable();
    while let Some(chunk) = chunks.next() {
        let more = if chunks.peek().is_some() { 1 } else { 0 };
        out.extend_from_slice(INTRODUCER);
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(format!(",m={more};").as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(TERMINATOR);
        // Continuation chunks carry only the `m` key — the first code already
        // declared what the image is.
        header.clear();
    }
    out
}

/// Delete the image tagged `id`, freeing the terminal's copy of it.
///
/// `d=i` deletes by id *and* removes the placement. Without this a scrolled
/// image stays pinned where it was drawn, floating over whatever text now
/// occupies those cells.
/// Delete **every** image this client placed, without needing their ids
/// (`a=d,d=a` — "delete all placements"). The exit scrub's arm: teardown runs
/// from `main`, which never held the painter's id list, and an exiting client
/// has no reason to be surgical.
pub fn delete_all() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(INTRODUCER);
    out.extend_from_slice(b"a=d,d=a,q=2");
    out.extend_from_slice(TERMINATOR);
    out
}

pub fn delete(id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(INTRODUCER);
    out.extend_from_slice(format!("a=d,d=i,i={id},q=2").as_bytes());
    out.extend_from_slice(TERMINATOR);
    out
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
    fn a_small_image_is_one_self_describing_chunk() {
        let encoded = encode(
            &RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3])),
            (4, 2),
            7,
        );
        let rendered = text(&encoded);

        assert!(rendered.starts_with("\x1b_G"), "introducer: {rendered}");
        assert!(rendered.ends_with("\x1b\\"), "terminator: {rendered}");
        assert!(rendered.contains("a=T"), "transmit and display: {rendered}");
        assert!(rendered.contains("f=24"), "24-bit RGB: {rendered}");
        assert!(rendered.contains("s=2,v=2"), "pixel dimensions: {rendered}");
        assert!(rendered.contains("c=4,r=2"), "cell box: {rendered}");
        assert!(rendered.contains("i=7"), "image id: {rendered}");
        assert!(rendered.contains("m=0"), "no further chunks: {rendered}");
    }

    #[test]
    fn replies_are_suppressed() {
        // Load-bearing, and invisible if it regresses: a kitty reply lands on
        // stdin, where the event stream — not us — is reading.
        let encoded = encode(
            &RgbImage::from_pixel(1, 1, image::Rgb([0, 0, 0])),
            (1, 1),
            1,
        );
        assert!(
            text(&encoded).contains("q=2"),
            "transmit suppresses replies"
        );
        assert!(
            text(&delete(1)).contains("q=2"),
            "delete suppresses replies"
        );
    }

    #[test]
    fn the_payload_is_the_raw_rgb_triples() {
        use base64::Engine as _;
        let encoded = text(&encode(
            &RgbImage::from_pixel(1, 1, image::Rgb([9, 8, 7])),
            (1, 1),
            1,
        ));
        let payload = encoded
            .split_once(';')
            .expect("a payload follows the control data")
            .1
            .trim_end_matches("\x1b\\");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("payload is base64");
        assert_eq!(decoded, vec![9, 8, 7]);
    }

    #[test]
    fn a_large_image_is_chunked_and_only_the_last_chunk_says_stop() {
        // 64×64 RGB is 12,288 raw bytes → 16,384 base64 → four chunks. A chunk
        // boundary that mis-sets `m` leaves kitty waiting for a chunk that never
        // comes, and nothing paints.
        let encoded = text(&encode(&RgbImage::new(64, 64), (16, 8), 3));
        assert_eq!(encoded.matches("\x1b_G").count(), 4, "four chunks");
        assert_eq!(encoded.matches("m=1").count(), 3, "three continue");
        assert_eq!(encoded.matches("m=0").count(), 1, "one stops");
        assert!(encoded.ends_with("\x1b\\"), "terminator");
    }

    #[test]
    fn continuation_chunks_carry_only_the_more_key() {
        // Repeating the full control data per chunk is bytes on the wire, and
        // the protocol reads the first code as the declaration.
        let encoded = text(&encode(&RgbImage::new(64, 64), (16, 8), 3));
        assert_eq!(
            encoded.matches("a=T").count(),
            1,
            "declared once: {}",
            &encoded[..80]
        );
        assert_eq!(encoded.matches("i=3").count(), 1, "id declared once");
    }

    #[test]
    fn delete_targets_the_id_and_its_placement() {
        let rendered = text(&delete(42));
        assert!(rendered.starts_with("\x1b_G"), "introducer: {rendered}");
        assert!(rendered.contains("a=d"), "delete: {rendered}");
        assert!(rendered.contains("d=i"), "by id: {rendered}");
        assert!(rendered.contains("i=42"), "which id: {rendered}");
        assert!(rendered.ends_with("\x1b\\"), "terminator: {rendered}");
    }

    #[test]
    fn an_empty_image_encodes_to_nothing_rather_than_a_malformed_sequence() {
        assert!(encode(&RgbImage::new(0, 0), (1, 1), 1).is_empty());
    }
}
