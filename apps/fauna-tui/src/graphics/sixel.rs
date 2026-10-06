//! The sixel encoder — RGB pixels into a DCS sixel sequence.
//!
//! Sixel is the oldest of the three arms and the only one that survives a
//! terminal multiplexer (`super::detect` module docs), which makes it the arm
//! most likely to be the one a real user sees.
//!
//! **The format, briefly.** A sixel sequence paints in horizontal *bands* six
//! pixels tall. Within a band each printable character carries one column of
//! those six pixels as a bitmask (`0x3F + bits`, bit 0 = the band's top row), so
//! a band is written one colour at a time: select a colour register, write the
//! whole row of columns with that colour's bits set, emit `$` to return to the
//! band's start, and repeat for the next colour. `-` ends the band and starts the
//! next.
//!
//! **Why a fixed 6×6×6 palette.** Sixel addresses colours through registers, so
//! an encoder must first reduce the image to a palette. The alternatives are a
//! real quantizer (median-cut and friends — hundreds of lines, and a dependency
//! or a lot of tests) or a fixed cube. At the size these thumbnails paint —
//! [`crate::thumbnail::THUMBNAIL_COLS`] cells wide, so a couple of hundred pixels
//! across at any sane cell size — the 216-entry cube is visually fine and is
//! *deterministic*, which means it is unit-testable to the byte. If thumbnails
//! ever grow to the point where cube banding is visible, a quantizer drops in
//! behind [`quantize`] without touching the band encoder.
//!
//! We emit only the registers the image actually uses, so a flat picture costs a
//! handful of palette definitions rather than 216.
//!
//! No dependency is added for any of this: the workspace has no sixel crate, and
//! this is ~100 lines we can test exactly.

use image::RgbImage;

/// Device Control String introducer + sixel mode.
///
/// `0;1;0` — aspect ratio 1:1, and **P2 = 1**: pixels whose bit is not set stay
/// transparent rather than being painted background. That is what lets a final
/// short band and any trimmed run leave the terminal's own cells showing instead
/// of stamping black over them.
const INTRODUCER: &[u8] = b"\x1bP0;1;0q";

/// String Terminator.
const TERMINATOR: &[u8] = b"\x1b\\";

/// A band is six pixel rows — the format's defining constant, and the `six` in
/// "sixel".
const BAND: u32 = 6;

/// Levels per channel in the fixed colour cube (6³ = 216 registers).
const LEVELS: u16 = 6;

/// Runs shorter than this cost more as `!<n><char>` than written out — `!4?` is
/// three bytes against four, so four is where repetition starts paying.
const MIN_RLE_RUN: usize = 4;

/// Map an 8-bit channel onto the cube's 0..=5 level.
fn level(value: u8) -> u16 {
    (value as u16 * (LEVELS - 1) + 127) / 255
}

/// The palette register for a pixel: the 6×6×6 cube index, `r*36 + g*6 + b`.
fn quantize(pixel: [u8; 3]) -> u16 {
    level(pixel[0]) * 36 + level(pixel[1]) * 6 + level(pixel[2])
}

/// A register's channel percentages, as sixel wants them (`0..=100`, not `0..=255`).
///
/// Level `l` is the 8-bit value `l * 51`, i.e. `l * 20` percent — so the cube
/// lands on exact percentages with no rounding.
fn register_color(index: u16) -> (u16, u16, u16) {
    ((index / 36) * 20, ((index / 6) % 6) * 20, (index % 6) * 20)
}

/// Encode `image` as a complete sixel sequence, introducer through terminator.
///
/// The caller positions the cursor first: a sixel paints at the cursor, and this
/// function has no opinion about where that is (`super::Painter`).
pub fn encode(image: &RgbImage) -> Vec<u8> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return Vec::new();
    }

    // Quantize once. Every band below reads this, and re-deriving the register
    // per band would decode the same pixel six times.
    let registers: Vec<u16> = image.pixels().map(|pixel| quantize(pixel.0)).collect();

    let mut out = Vec::with_capacity(registers.len() / 2);
    out.extend_from_slice(INTRODUCER);
    out.extend_from_slice(format!("\"1;1;{width};{height}").as_bytes());

    let mut used = [false; (LEVELS * LEVELS * LEVELS) as usize];
    for &register in &registers {
        used[register as usize] = true;
    }
    for (index, _) in used.iter().enumerate().filter(|(_, seen)| **seen) {
        let (r, g, b) = register_color(index as u16);
        out.extend_from_slice(format!("#{index};2;{r};{g};{b}").as_bytes());
    }

    for band_top in (0..height).step_by(BAND as usize) {
        let band_height = BAND.min(height - band_top);

        // Only the registers present in this band — a band of sky costs one
        // colour pass, not 216.
        let mut band_registers: Vec<u16> = (band_top..band_top + band_height)
            .flat_map(|y| {
                let row = (y * width) as usize;
                registers[row..row + width as usize].iter().copied()
            })
            .collect();
        band_registers.sort_unstable();
        band_registers.dedup();

        for (position, &register) in band_registers.iter().enumerate() {
            if position > 0 {
                // Graphics carriage return: back to the band's first column so
                // this colour overlays the ones already written.
                out.push(b'$');
            }
            out.extend_from_slice(format!("#{register}").as_bytes());

            let mut columns: Vec<u8> = (0..width)
                .map(|x| {
                    let mut bits = 0u8;
                    for dy in 0..band_height {
                        let index = ((band_top + dy) * width + x) as usize;
                        if registers[index] == register {
                            bits |= 1 << dy;
                        }
                    }
                    0x3f + bits
                })
                .collect();
            // A trailing run of "no pixels here" paints nothing (P2 = 1), so
            // writing it out is pure bytes on the wire.
            while columns.last() == Some(&0x3f) {
                columns.pop();
            }
            write_runs(&mut out, &columns);
        }
        // Graphics newline: next band.
        out.push(b'-');
    }

    out.extend_from_slice(TERMINATOR);
    out
}

/// Write `columns` with run-length compression where it pays.
fn write_runs(out: &mut Vec<u8>, columns: &[u8]) {
    let mut start = 0;
    while start < columns.len() {
        let glyph = columns[start];
        let run = columns[start..]
            .iter()
            .take_while(|&&next| next == glyph)
            .count();
        if run >= MIN_RLE_RUN {
            out.extend_from_slice(format!("!{run}").as_bytes());
            out.push(glyph);
        } else {
            out.extend(std::iter::repeat_n(glyph, run));
        }
        start += run;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, color: [u8; 3]) -> RgbImage {
        RgbImage::from_pixel(width, height, image::Rgb(color))
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// The palette definitions, without their leading `#`.
    ///
    /// `#N` introduces **two** different things — a definition (`#N;2;r;g;b`) and
    /// a band's colour *selection* (`#N` then sixel data) — so a test that greps
    /// for `#215` cannot tell them apart. Band data is sixel glyphs, `!`, digits,
    /// `$` and `-`, never `;`, so `;2;` separates them exactly.
    fn palette(encoded: &[u8]) -> Vec<String> {
        text(encoded)
            .split('#')
            .skip(1)
            .filter(|chunk| chunk.contains(";2;"))
            .map(str::to_string)
            .collect()
    }

    /// Everything from the first band's colour selection onward — i.e. the
    /// picture, with the header and palette stripped. The palette is emitted
    /// contiguously before the first band, so the last definition ends it.
    fn band_data(encoded: &[u8]) -> String {
        let rendered = text(encoded);
        let last_definition = rendered
            .rfind(";2;")
            .expect("at least one palette definition");
        let bands_at = rendered[last_definition..]
            .find('#')
            .map(|at| last_definition + at)
            .expect("band data follows the palette");
        rendered[bands_at..].to_string()
    }

    #[test]
    fn the_cube_maps_the_channel_extremes_exactly() {
        // Black and white must survive a round trip unchanged, or every image
        // gains a tint. `level` is the only place that can get this wrong.
        assert_eq!(level(0), 0);
        assert_eq!(level(255), 5);
        assert_eq!(register_color(quantize([0, 0, 0])), (0, 0, 0));
        assert_eq!(register_color(quantize([255, 255, 255])), (100, 100, 100));
    }

    #[test]
    fn the_cube_separates_the_channels() {
        // A red pixel must not land on a register that paints green.
        assert_eq!(register_color(quantize([255, 0, 0])), (100, 0, 0));
        assert_eq!(register_color(quantize([0, 255, 0])), (0, 100, 0));
        assert_eq!(register_color(quantize([0, 0, 255])), (0, 0, 100));
    }

    #[test]
    fn a_solid_image_is_a_well_formed_sequence() {
        let encoded = encode(&solid(4, 6, [255, 0, 0]));
        let rendered = text(&encoded);

        assert!(
            rendered.starts_with("\x1bP0;1;0q"),
            "introducer: {rendered}"
        );
        assert!(rendered.ends_with("\x1b\\"), "terminator: {rendered}");
        assert!(
            rendered.contains("\"1;1;4;6"),
            "raster attributes: {rendered}"
        );
        // Red is cube index 180 (5*36), painted at 100% red.
        assert!(rendered.contains("#180;2;100;0;0"), "palette: {rendered}");
        // A full 6-row band with every bit set is `~` (0x3f + 0b111111), and four
        // identical columns is exactly where RLE starts paying.
        assert!(rendered.contains("#180!4~"), "band: {rendered}");
    }

    #[test]
    fn only_the_registers_the_image_uses_are_defined() {
        // 216 palette definitions for a two-colour picture is bytes we never send.
        let mut image = solid(2, 6, [0, 0, 0]);
        image.put_pixel(1, 1, image::Rgb([255, 255, 255]));

        assert_eq!(
            palette(&encode(&image)),
            vec!["0;2;0;0;0", "215;2;100;100;100"],
            "exactly the two registers the picture uses, and nothing else"
        );
    }

    #[test]
    fn each_band_ends_with_a_graphics_newline() {
        // 13 rows is three bands: two full and a short tail.
        let encoded = encode(&solid(2, 13, [0, 0, 255]));
        assert_eq!(text(&encoded).matches('-').count(), 3);
    }

    #[test]
    fn a_short_final_band_sets_only_the_rows_that_exist() {
        // 2 rows of a 6-row band: bits 0 and 1 → `0x3f + 0b000011` = 'B'. Setting
        // the absent four ('~', all six) would paint colour into pixels the image
        // does not have — a white bar below every short picture.
        let data = band_data(&encode(&solid(1, 2, [255, 255, 255])));
        assert!(data.starts_with("#215B"), "short band: {data:?}");
    }

    #[test]
    fn a_second_colour_in_a_band_is_written_after_a_graphics_return() {
        // The `$` is what makes the colours overlay rather than march rightwards.
        let mut image = solid(2, 6, [0, 0, 0]);
        image.put_pixel(1, 0, image::Rgb([255, 255, 255]));
        let rendered = text(&encode(&image));
        assert!(rendered.contains('$'), "graphics return: {rendered}");
        let band = rendered.split('$').nth(1).expect("a second colour pass");
        assert!(
            band.starts_with("#215"),
            "second pass selects white: {band}"
        );
    }

    #[test]
    fn trailing_empty_columns_are_trimmed() {
        // Only the first column is white, so white's pass must stop after it
        // rather than writing seven "nothing here" glyphs.
        let mut image = solid(8, 6, [0, 0, 0]);
        for y in 0..6 {
            image.put_pixel(0, y, image::Rgb([255, 255, 255]));
        }
        let data = band_data(&encode(&image));
        let white = data.split("#215").nth(1).expect("a white pass");
        assert!(white.starts_with('~'), "white paints column 0: {white:?}");
        assert!(
            !white.starts_with("~?"),
            "and stops there rather than writing seven 'nothing here' glyphs: {white:?}"
        );
    }

    #[test]
    fn runs_shorter_than_the_rle_threshold_are_written_out() {
        // `!3~` would be no smaller than `~~~`, and is harder to read on a wire
        // dump. Three columns is below the threshold.
        let rendered = text(&encode(&solid(3, 6, [255, 255, 255])));
        assert!(
            rendered.contains("#215~~~"),
            "short run written out: {rendered}"
        );
        assert!(
            !rendered.contains('!'),
            "no RLE below the threshold: {rendered}"
        );
    }

    #[test]
    fn a_long_run_is_compressed() {
        let rendered = text(&encode(&solid(200, 6, [255, 255, 255])));
        assert!(
            rendered.contains("!200~"),
            "long run compressed: {rendered}"
        );
    }

    #[test]
    fn an_empty_image_encodes_to_nothing_rather_than_a_malformed_sequence() {
        // An introducer with no terminator would hang the terminal's parser.
        assert!(encode(&RgbImage::new(0, 0)).is_empty());
        assert!(encode(&RgbImage::new(4, 0)).is_empty());
    }

    #[test]
    fn every_band_of_a_gradient_is_encoded() {
        // A real-ish picture: no panic, every band present, sequence well-formed.
        let mut image = RgbImage::new(16, 32);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 16) as u8, (y * 8) as u8, 128]);
        }
        let rendered = text(&encode(&image));
        assert!(rendered.starts_with("\x1bP"), "introducer");
        assert!(rendered.ends_with("\x1b\\"), "terminator");
        assert_eq!(
            rendered.matches('-').count(),
            32 / 6 + 1,
            "six bands: {rendered}"
        );
    }
}
