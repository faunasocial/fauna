//! The half-block rasterizer — encoded image bytes into terminal cell art.
//!
//! This is the **images half** of [`apps/tui.md` § Rendering], and the whole
//! of the tui's per-app thumbnail glue. Everything upstream — fetching the
//! blob by hash, verifying its content address, decrypting it under the owner
//! `BackupKey` — is shared Rust in `MediaMachine::fetch_thumbnail`, because
//! `ui/media.md` § Thumbnails puts it there: *"per-app glue is reduced to
//! painting the returned bytes"* (priority #2). So this module decodes and
//! rasterizes. It fetches nothing and decrypts nothing.
//!
//! **Why half-blocks.** A terminal cell is roughly twice as tall as it is wide,
//! so painting one pixel per cell squashes an image to half its aspect. The
//! `▀` (upper half block) trick recovers it: one cell carries **two** vertically
//! stacked pixels — the glyph's foreground paints the top pixel, its background
//! the bottom. A cell is then square-ish, and a `cols`-wide grid holds
//! `2 × rows` pixels of height.
//!
//! **Why this module knows nothing about ratatui.** The art is a plain pixel
//! grid; [`crate::ui::element_lines`] turns it into styled spans. That is the
//! same split [`crate::element::Element::doc`] already uses for rich bodies —
//! the element model feeds the automation registry and the focus ring too, and
//! neither of those reads paint types. See [`crate::element`].
//!
//! The kitty/iTerm2/sixel protocol arms (§ Rendering's other half) do **not**
//! belong here: they emit escape sequences at a cell rect in a post-paint pass,
//! which is a different seam entirely. Half-blocks are the fallback arm, and the
//! only arm that is `Line`-shaped.

/// One terminal cell of half-block art: two vertically stacked pixels painted
/// as a single `▀` glyph (fg = [`top`](Self::top), bg = [`bottom`](Self::bottom)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HalfBlockCell {
    /// The upper pixel — painted as the glyph's foreground.
    pub top: [u8; 3],
    /// The lower pixel — painted as the glyph's background.
    pub bottom: [u8; 3],
}

/// A decoded thumbnail as terminal cell art: one entry per painted row, each a
/// row of [`HalfBlockCell`]s. Ratatui-free by design (module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HalfBlockArt {
    pub rows: Vec<Vec<HalfBlockCell>>,
}

/// Pixels at protocol resolution, shared: one decode feeds every frame that
/// paints the picture, and [`crate::graphics::Placement`] clones this per frame.
pub type Pixels = std::sync::Arc<image::RgbImage>;

/// A decoded thumbnail in **both** the representations the two rendering arms
/// need — the half-block cells [`crate::ui::element_lines`] paints as text, and
/// the pixels [`crate::graphics::Painter`] emits through a terminal graphics
/// protocol.
///
/// Both, always, and never one or the other. The half-block art is the fallback
/// arm, the eraser for the compositing protocols, *and* the e2e observable, so a
/// live protocol never replaces it — see [`crate::graphics`] module docs, which
/// own that claim.
#[derive(Debug, Clone)]
pub struct Thumbnail {
    pub art: HalfBlockArt,
    pub pixels: Pixels,
}

/// The glyph a half-block cell paints. Its foreground is the cell's top pixel,
/// its background the bottom — so one character carries two pixels.
pub const HALF_BLOCK: &str = "▀";

/// The stand-in painted when an item has no thumbnail, its fetch failed, or it
/// has not loaded yet — the tui's answer to linux's `image-x-generic-symbolic`
/// placeholder icon (`apps/fauna-linux/src/views/media/item.rs`).
///
/// A glyph rather than a phrase, deliberately: it needs no translation, so the
/// Media page keeps the zero-net-new-i18n-keys property it landed with.
pub const PLACEHOLDER: &str = "□";

/// Painted width in cells. Small enough that a row of items still reads as a
/// list on an 80-column terminal, large enough that the picture is recognizable
/// — the terminal analogue of linux's 32px list icon.
pub const THUMBNAIL_COLS: u32 = 16;

/// The cell width a feed `post-image` rasterizes to — a recognizable inline
/// preview, wider than the [`THUMBNAIL_COLS`] media-list icon because a feed
/// image is body content, not a list glyph (linux paints it at a fixed 240px
/// box; this is the terminal analogue). Aspect ratio is preserved, so height
/// follows from the source; the painter crops to the on-screen rect, so a wide
/// value never overflows the card — it only bounds the resolution.
pub const POST_IMAGE_COLS: u32 = 32;

/// The cell width the `image-lightbox` rasterizes to — the "full-screen viewer"
/// of ui.yaml's `image-lightbox`, expressed the only way a terminal can: a cell
/// grid several times the inline [`POST_IMAGE_COLS`] card preview, sized to sit
/// inside a conventional 80-column terminal with its border.
///
/// The lightbox re-rasterizes from [`Thumbnail::pixels`] via [`rasterize_rgb`],
/// so on a kitty/iTerm2/sixel terminal this genuinely gains detail (the painter
/// scales the same pixels into a bigger rect); on the half-block fallback arm it
/// is a bigger, coarser grid of the same picture — which is still the difference
/// between "a thumbnail in a card" and "the picture, on its own screen".
pub const LIGHTBOX_COLS: u32 = 72;

/// Upper bound on a terminal cell's width in pixels, used to size
/// [`Thumbnail::pixels`].
///
/// The protocol arms paint into a `THUMBNAIL_COLS`-wide cell box, so the pixels
/// they need are `THUMBNAIL_COLS × cell_width` across — but cell width is a
/// property of the terminal, and this decode happens on a background fetch that
/// knows nothing about the screen. So we size for the widest cell anyone
/// plausibly runs and let [`crate::graphics::Painter`] scale down to the real box
/// at emit time. Erring high here costs a little memory per cached thumbnail;
/// erring low would cap the picture's resolution below what the terminal can show.
const MAX_CELL_PX: u32 = 16;

impl HalfBlockArt {
    /// The art's plaintext — the characters this actually paints, which is what
    /// [`crate::element::Element::text`] must carry: a terminal's "plaintext" is
    /// its glyphs, and every `get_text` reads that field.
    ///
    /// This is also the e2e observable. A painted thumbnail is *text* on this
    /// client, so `media-thumbnail`'s registry entry proves the paint — which is
    /// why tui can assert the producer→fetch→decrypt→paint path end-to-end
    /// headlessly, where the GUI apps paint into a native image view and fall
    /// back to per-app unit tests (`tests/e2e-unified/actions/media.py`
    /// `painted_thumbnail_count`).
    pub fn to_plaintext(&self) -> String {
        self.rows
            .iter()
            .map(|row| HALF_BLOCK.repeat(row.len()))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Decode `bytes` and rasterize them to `cols`-wide half-block art, preserving
/// aspect ratio.
///
/// `bytes` are the **encoded** image (JPEG/PNG/…) that
/// `MediaMachine::fetch_thumbnail` hands back already fetched, verified and
/// decrypted. Returns `None` when they do not decode, or when either dimension
/// rounds away to nothing — never a panic and never an error to surface: one
/// unreadable thumbnail must fall back to the placeholder and must not blank the
/// row or raise the page banner (`ui/media.md` § Thumbnails; the machine's own
/// `fetch_thumbnail` contract says the same of its errors). That degrade is the
/// behavior linux pins in `paint_thumbnail_swaps_valid_bytes_and_rejects_garbage`.
///
/// **This is the only place the pixels exist.** `bytes` are borrowed and the
/// caller drops them the moment the fetch op completes, so whatever this returns
/// is what every later frame has to paint from. That is why the returned
/// [`Thumbnail`] carries protocol-resolution pixels alongside the cell art rather
/// than the art alone: a graphics protocol needs real pixels, and re-fetching per
/// frame is not a thing a cache is for.
pub fn rasterize(bytes: &[u8], cols: u32) -> Option<Thumbnail> {
    let decoded = image::load_from_memory(bytes).ok()?;
    rasterize_rgb(&decoded.to_rgb8(), cols)
}

/// Rasterize an **already-decoded** picture to `cols`-wide half-block art —
/// [`rasterize`]'s body, minus the decode.
///
/// The split exists for the one caller that has pixels but no bytes: the
/// `image-lightbox` re-rasterizes a post image the [`crate::image_cache`]
/// already holds, at [`LIGHTBOX_COLS`] instead of [`POST_IMAGE_COLS`]. The
/// encoded bytes are long gone by then (this module's docs: they are borrowed
/// and dropped when the fetch op completes), so re-deriving the bigger art from
/// [`Thumbnail::pixels`] is the only way to get one without a second GET.
///
/// Nothing is upscaled: `pixels` is still capped at the source width, so a
/// lightbox over a small picture paints a bigger *cell grid* of the same
/// picture, and a graphics-protocol terminal (which scales `pixels` into
/// whatever rect it is given) genuinely gains detail from the larger rect.
pub fn rasterize_rgb(rgb: &image::RgbImage, cols: u32) -> Option<Thumbnail> {
    let (w, h) = rgb.dimensions();
    if w == 0 || h == 0 || cols == 0 {
        return None;
    }

    // Two pixels per cell vertically, and a cell is ~2× as tall as it is wide —
    // so the pixel grid is `cols` wide by `2 × rows` tall, and scaling height by
    // `cols/w` (not `2·cols/w`) is what makes the painted block hold the
    // original aspect.
    let pixel_h = ((h as f32 / w as f32) * cols as f32).round().max(1.0) as u32;
    // An odd pixel height would leave a final cell with no bottom pixel; round
    // up to keep every cell a full pair.
    let pixel_h = pixel_h + (pixel_h % 2);
    let scaled = image::imageops::resize(
        rgb,
        cols,
        pixel_h,
        // Triangle, not Nearest: a thumbnail shrunk to ~16 cells drops most of
        // its pixels, and a nearest-neighbour pick of one survivor per cell
        // turns fine detail into noise. Averaging is what keeps the shrunken
        // picture readable.
        image::imageops::FilterType::Triangle,
    );

    let rows = (0..pixel_h / 2)
        .map(|row| {
            (0..cols)
                .map(|col| HalfBlockCell {
                    top: scaled.get_pixel(col, row * 2).0,
                    bottom: scaled.get_pixel(col, row * 2 + 1).0,
                })
                .collect()
        })
        .collect();

    // The protocol arms' source. Never *upscaled* past the original: a thumbnail
    // blown up to a nominal 256px is the same picture with more bytes, and the
    // protocol arm would only have to send them.
    let source_w = (cols * MAX_CELL_PX).min(w);
    let source_h = ((h as f32 / w as f32) * source_w as f32).round().max(1.0) as u32;
    let pixels = std::sync::Arc::new(image::imageops::resize(
        rgb,
        source_w,
        source_h,
        image::imageops::FilterType::Triangle,
    ));

    Some(Thumbnail {
        art: HalfBlockArt { rows },
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid minimal 1×1 grayscale PNG — the same fixture linux's thumbnail
    /// test uses (`apps/fauna-linux/src/views/media/item.rs`), standing in for
    /// the decoded bytes `MediaMachine::fetch_thumbnail` hands back.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x00, 0x00, 0x00, 0x00, 0x3a,
        0x7e, 0x9b, 0x55, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8,
        0x0f, 0x00, 0x01, 0x01, 0x01, 0x00, 0xb1, 0x38, 0xf6, 0x14, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// Encode a `w`×`h` RGB image as PNG — a stand-in for a fetched thumbnail
    /// blob of a known shape, so the aspect + colour assertions have something
    /// to read.
    fn png_of(w: u32, h: u32, pixel: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb(pixel(x, y)));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode the fixture PNG");
        out.into_inner()
    }

    /// Undecodable bytes degrade to `None` — the caller keeps the placeholder.
    ///
    /// The per-item degrade of `media.md` § Thumbnails: one unreadable thumbnail
    /// must never blank the row or raise the page banner. Linux pins the same
    /// contract in `paint_thumbnail_swaps_valid_bytes_and_rejects_garbage`.
    #[test]
    fn garbage_bytes_rasterize_to_none() {
        assert!(rasterize(b"not an image", THUMBNAIL_COLS).is_none());
        assert!(rasterize(&[], THUMBNAIL_COLS).is_none());
        // Truncated PNG: a real header, no pixels — the shape a partial fetch
        // would take.
        assert!(rasterize(&TINY_PNG[..20], THUMBNAIL_COLS).is_none());
    }

    /// Decodable bytes rasterize to art at the requested width.
    #[test]
    fn valid_png_rasterizes_to_art() {
        let art = rasterize(TINY_PNG, THUMBNAIL_COLS)
            .expect("a valid PNG must rasterize")
            .art;
        assert!(!art.rows.is_empty(), "art must have at least one row");
        for row in &art.rows {
            assert_eq!(
                row.len(),
                THUMBNAIL_COLS as usize,
                "every row is exactly the requested width"
            );
        }
    }

    /// A square image paints a square-ish block: two pixels per cell vertically
    /// means a `cols`-wide square image is `cols/2` **rows** tall. Painting one
    /// pixel per cell instead would double the height and squash the picture.
    #[test]
    fn square_image_paints_half_as_many_rows_as_columns() {
        let art = rasterize(&png_of(64, 64, |_, _| [10, 20, 30]), 16)
            .expect("decodable")
            .art;
        assert_eq!(art.rows.len(), 8, "a square image is cols/2 rows tall");
        assert_eq!(art.rows[0].len(), 16);
    }

    /// A wide image stays wide: aspect is preserved, not stretched to a square.
    #[test]
    fn wide_image_paints_fewer_rows_than_a_square_one() {
        let art = rasterize(&png_of(64, 16, |_, _| [10, 20, 30]), 16)
            .expect("decodable")
            .art;
        // 4:1 source at 16 cols → 4 pixel rows → 2 cell rows.
        assert_eq!(art.rows.len(), 2, "a 4:1 image paints 4:1");
    }

    /// The two pixels of a cell land on the right halves: the **top** pixel is
    /// the cell's `top` (the glyph's fg) and the pixel below it is `bottom` (the
    /// bg). Inverting them would paint the image upside down in vertical pairs
    /// — a scrambling no aspect or size assertion above would catch.
    #[test]
    fn cell_pairs_the_pixel_above_with_the_pixel_below() {
        // A red band over a blue one, split at an **odd** pixel row so the
        // boundary falls *inside* a cell rather than between two: at 16 cells
        // the image is 16 pixels tall, so an even split would put the colour
        // change exactly on a cell edge and no cell would ever hold both.
        let bytes = png_of(16, 16, |_, y| if y < 7 { [255, 0, 0] } else { [0, 0, 255] });
        let art = rasterize(&bytes, 16).expect("decodable").art;
        assert_eq!(art.rows.len(), 8);

        // The first cell row sits entirely in the red band, the last entirely in
        // the blue one.
        assert_eq!(art.rows[0][0].top, [255, 0, 0], "top row is the red band");
        assert_eq!(art.rows[0][0].bottom, [255, 0, 0]);
        let last = art.rows.last().expect("rows").first().expect("cells");
        assert_eq!(last.top, [0, 0, 255], "bottom row is the blue band");
        assert_eq!(last.bottom, [0, 0, 255]);

        // Cell row 3 holds pixels 6 and 7 — the last red row and the first blue
        // one. Its top must be the redder of the two and its bottom the bluer;
        // inverted pairing would swap them. This is what proves the order rather
        // than a coincidence of a uniform image.
        let middle = art.rows[3][0];
        assert!(
            middle.top[0] > middle.bottom[0] && middle.bottom[2] > middle.top[2],
            "the straddling cell pairs red-above with blue-below, got {middle:?}"
        );
    }

    /// The plaintext is one `▀` per cell, one line per row — what the terminal
    /// literally paints, and the string every `get_text` (and the e2e paint
    /// assertion) reads off the element.
    #[test]
    fn plaintext_is_one_glyph_per_cell_and_one_line_per_row() {
        let art = rasterize(&png_of(64, 64, |_, _| [1, 2, 3]), 16)
            .expect("decodable")
            .art;
        let text = art.to_plaintext();
        let lines: Vec<_> = text.split('\n').collect();
        assert_eq!(lines.len(), 8, "one line per cell row");
        assert!(
            lines.iter().all(|l| l.chars().count() == 16),
            "one glyph per cell"
        );
        assert!(text.contains(HALF_BLOCK));
        assert_ne!(text, PLACEHOLDER, "real art is not the placeholder");
    }

    /// One decode yields **both** arms' pixels. The protocol arms have no other
    /// source: the encoded bytes are borrowed and the caller drops them.
    #[test]
    fn rasterize_yields_protocol_pixels_beside_the_cell_art() {
        let thumbnail = rasterize(&png_of(64, 64, |_, _| [10, 20, 30]), 16).expect("decodable");
        assert_eq!(thumbnail.art.rows.len(), 8, "the cell art is unchanged");
        assert_eq!(
            thumbnail.pixels.dimensions(),
            (64, 64),
            "the pixels are the picture, not the 16-cell reduction of it"
        );
        assert_eq!(thumbnail.pixels.get_pixel(32, 32).0, [10, 20, 30]);
    }

    /// The protocol source is capped, not unbounded: a 4000px original is a lot
    /// of resident bytes per cached item, and no terminal can show them.
    #[test]
    fn a_large_source_is_capped_at_the_widest_cell_box_worth_painting() {
        let thumbnail = rasterize(&png_of(2000, 1000, |_, _| [4, 5, 6]), 16).expect("decodable");
        assert_eq!(
            thumbnail.pixels.dimensions(),
            (16 * MAX_CELL_PX, 16 * MAX_CELL_PX / 2),
            "capped to the cell box, aspect preserved"
        );
    }

    /// …and never upscaled: blowing a 32px thumbnail up to 256px is the same
    /// picture with eight times the bytes to send.
    #[test]
    fn a_small_source_is_not_upscaled() {
        let thumbnail = rasterize(&png_of(32, 32, |_, _| [7, 8, 9]), 16).expect("decodable");
        assert_eq!(thumbnail.pixels.dimensions(), (32, 32));
    }
}
