//! Inline images via terminal graphics protocols — the post-paint seam.
//!
//! This closes the images half of [`apps/tui.md` § Rendering]: *"inline via
//! terminal graphics protocols (kitty, iTerm2, sixel), with a half-block cell
//! fallback on terminals without one."* [`detect`] picks the arm; [`Painter`]
//! puts pixels on the screen.
//!
//! **Why this is not in the `Line` model.** Everything else the tui paints is
//! text: an [`crate::element::Element`] becomes ratatui `Line`s and a
//! `Paragraph` scrolls them. A protocol image cannot be — it is an escape
//! sequence addressed to a **cell rect**, and a `Paragraph` has nowhere to carry
//! one. So this is the one place the client's "everything is a text line" model
//! legitimately breaks, and it breaks in exactly **one** seam: after
//! `terminal.draw` returns, we position the cursor and write escapes straight to
//! stdout, which ratatui does not own and does not know about. (Writing raw
//! escapes past ratatui is not new here — `wizard::copy_to_clipboard` has emitted
//! OSC 52 the same way since M2.)
//!
//! **The half-block art stays painted underneath**, always, even when a protocol
//! is live. Three reasons, and each is load-bearing:
//!
//! 1. It is the fallback that shows if the terminal lied about its capabilities,
//!    or if an emit fails. A picture degrades to a worse picture, never to a hole.
//! 2. For sixel and iTerm2 it is the **eraser**: those protocols composite into
//!    the text grid, so ratatui repainting the cells is what removes the old
//!    image when it scrolls (see [`Painter::paint`]).
//! 3. It is what keeps the client honest in e2e. `media-thumbnail`'s registry
//!    entry carries [`crate::thumbnail::HalfBlockArt::to_plaintext`], and
//!    `tests/e2e-unified/actions/media.py::painted_thumbnail_count` counts `▀` in
//!    it — the only positive producer→fetch→decrypt→paint assertion in the fleet
//!    (`tui.md` § Implementation status). An arm that *replaced* the art instead
//!    of overlaying it would empty that text and the assertion would pass
//!    vacuously against zero items, reporting green while painting nothing. The
//!    test `a_live_protocol_never_empties_the_registry_text` pins it.

pub mod detect;
mod iterm2;
mod kitty;
mod sixel;

pub use detect::{Protocol, detect};

use crate::thumbnail::Pixels;
use std::io::Write;

/// Cell metrics assumed when the terminal will not report its pixel size.
///
/// Only sixel needs this — kitty and iTerm2 are told a cell box (`c=`/`r=`,
/// `width=`/`height=`) and scale into it themselves, but a sixel is sized in
/// *pixels* and paints wherever those pixels reach.
///
/// **Erring small is deliberate.** Guess too large and the image spills past the
/// cells the layout reserved for it, pushing text around and corrupting the
/// frame. Guess too small and it underfills its box — a smaller picture, and
/// nothing else. So this is at the low end of real cell sizes rather than the
/// average one. It matters for tmux in particular, which has not always
/// forwarded the outer terminal's pixel dimensions.
const FALLBACK_CELL: CellSize = CellSize {
    width: 6,
    height: 12,
};

// Documented invariant: guessing large spills the picture past its cells and
// corrupts the frame; guessing small only shrinks it. Real cells are rarely
// narrower/shorter than this, and never shorter than wide.
const _: () = assert!(FALLBACK_CELL.width <= 8);
const _: () = assert!(FALLBACK_CELL.height <= 16);
const _: () = assert!(FALLBACK_CELL.height >= FALLBACK_CELL.width);

/// Base for kitty image ids.
///
/// Ids are `KITTY_ID_BASE + placement index`, which works because [`Painter::paint`]
/// deletes *every* live image and retransmits *every* placement whenever anything
/// changes — so the index is stable for exactly as long as it needs to be, and
/// there is no id bookkeeping to get wrong. Identifying by *picture* instead
/// would be subtly wrong: two items showing the same thumbnail share one picture,
/// and one id cannot address two placements of it.
///
/// The base is high and distinctive because kitty's id space is shared by every
/// program drawing into the window, and `a=d,d=i` deletes by id — a low counter
/// risks deleting somebody else's picture.
const KITTY_ID_BASE: u32 = 0xFA00_0000;

/// A terminal cell's size in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize {
    pub width: u16,
    pub height: u16,
}

/// One image, where it landed on screen this frame.
///
/// Built by [`crate::ui::render`] from the same line offsets the `Paragraph`
/// scrolled by — never recomputed independently, which is the only way the rect
/// cannot drift from where the art actually painted.
#[derive(Debug, Clone)]
pub struct Placement {
    /// The pixels to emit, at protocol resolution (`crate::thumbnail`).
    pub pixels: Pixels,
    /// Top-left cell, 0-based, in terminal coordinates.
    pub col: u16,
    pub row: u16,
    /// The cell box the art occupies — and so the box the image must fill.
    pub cols: u16,
    pub rows: u16,
    /// `Some((rows_before, total_rows))` when a scrolled block clips the art to
    /// fewer than its full `total_rows` cell-rows — `rows` above already holds
    /// the shrunk, visible count, and this records where within the *original*
    /// row count that visible window starts, so [`Painter::encode`] can crop the
    /// matching pixel-row band out of the full-resolution source instead of
    /// scaling the whole picture into a too-small box. `None` when the art is
    /// fully on screen — the ordinary case.
    pub crop: Option<(u16, u16)>,
}

impl Placement {
    /// Whether this is the same picture in the same place — the diff [`Painter`]
    /// runs.
    ///
    /// Identity is the `Arc`, not the pixels: the thumbnail cache is keyed by
    /// **content hash** and hands out clones of one `Arc` per picture, so pointer
    /// equality *is* content equality here, for free. A re-fetch mints a new
    /// `Arc` and correctly reads as a different picture. (Deliberately not
    /// `PartialEq`: that would invite a deep compare of a few hundred KB of
    /// pixels per frame.) `crop` IS compared structurally — it is a couple of
    /// cheap `u16`s, not pixels, and it is what actually decides which band of
    /// the source is on screen; the rect fields alone don't determine that.
    fn same_spot(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.pixels, &other.pixels)
            && self.col == other.col
            && self.row == other.row
            && self.cols == other.cols
            && self.rows == other.rows
            && self.crop == other.crop
    }
}

/// The pixel-row window a [`Placement::crop`] selects out of the full-height
/// source image — `(y, height)`, both in source pixels, ready for
/// `image::imageops::crop_imm`. `visible_rows` is the placement's (already
/// shrunk) `rows`; `None` selects the whole image, unchanged.
///
/// A pure function so the crop math is checkable in cell/row terms without
/// decoding an encoded protocol escape sequence.
fn crop_window(crop: Option<(u16, u16)>, visible_rows: u16, src_height: u32) -> (u32, u32) {
    match crop {
        Some((rows_before, total_rows)) => {
            let total_rows = (total_rows as u64).max(1);
            let y = (src_height as u64 * rows_before as u64 / total_rows) as u32;
            let y_end = (src_height as u64 * (rows_before as u64 + visible_rows as u64)
                / total_rows) as u32;
            let y = y.min(src_height);
            let height = y_end.saturating_sub(y).max(1).min(src_height - y);
            (y, height)
        }
        None => (0, src_height),
    }
}

/// This terminal, as the emit needs to know it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Metrics {
    cell: CellSize,
    /// The terminal's size in cells.
    size: (u16, u16),
}

/// Emits protocol graphics after each frame, and cleans up after itself.
pub struct Painter {
    protocol: Protocol,
    /// What is on screen right now, so a frame that changed nothing costs nothing.
    last: Vec<Placement>,
    /// The terminal size the live images were emitted at. Part of "unchanged"
    /// — see [`Painter::paint`].
    last_size: Option<(u16, u16)>,
}

impl Painter {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            last: Vec::new(),
            last_size: None,
        }
    }

    /// Emit `placements` for this frame.
    ///
    /// **The diff is the design, not an optimization.** A tui redraws on every
    /// keystroke, and re-sending a few thousand bytes of sixel per thumbnail per
    /// keypress would make the client visibly slow. But it is also what makes the
    /// images *correct*: ratatui's own paint is diff-based, so when nothing about
    /// a thumbnail's cells changed it does not repaint them — and the image
    /// sitting on those cells stays. Emitting only on change matches that exactly.
    ///
    /// **Invalidation splits by how the terminal stores the image**, which is the
    /// whole reason the arms differ here:
    ///
    /// - **sixel and iTerm2 composite into the text grid.** When a placement
    ///   moves, ratatui repaints the cells it used to occupy (their content
    ///   changed, so the diff catches them) and *that repaint is the erase*.
    ///   Nothing to delete.
    /// - **kitty keeps images on its own layer**, above the text. Repainting the
    ///   cells underneath does nothing to it, so a scrolled image would stay
    ///   pinned where it was drawn, floating over whatever is there now. It must
    ///   be deleted by id, explicitly, and that is what [`kitty::delete`] is for.
    ///
    /// **A resize re-emits even when nothing moved.** ratatui redraws the whole
    /// screen when the terminal's size changes, and for the compositing arms that
    /// redraw *erases* every image on it — while a resize that is purely
    /// horizontal leaves each thumbnail's rect exactly where it was. Diffing the
    /// placements alone would therefore conclude "nothing changed" about images
    /// that had just been wiped off the screen, and they would stay gone until
    /// something else happened to move them. So the terminal's own size is part
    /// of what unchanged has to mean.
    ///
    /// Writes to `out` — a real stdout in the client, a `Vec<u8>` in tests.
    pub fn paint(
        &mut self,
        placements: Vec<Placement>,
        out: &mut impl Write,
    ) -> std::io::Result<()> {
        self.paint_with(placements, terminal_metrics(), out)
    }

    /// Erase every protocol graphic this client painted. **Call before leaving
    /// the alternate screen**, never after.
    ///
    /// Protocol graphics are not cells: the painter writes them straight to
    /// stdout after ratatui has flushed the frame, so they live outside the
    /// alternate-screen buffer that `ratatui::restore()` swaps away. A clean
    /// exit could therefore leave the last thumbnail painted over the returning
    /// shell prompt — a live user read exactly that as "cannot exit", pressed
    /// Ctrl+C (which had already worked), and relaunched blind over an
    /// invisible prompt.
    ///
    /// Two arms, because the protocols differ in what they can retract: kitty
    /// owns an image layer and deletes from it explicitly; sixel and iTerm2
    /// have no retraction verb, so the erase-display is the only scrub — and it
    /// is safe precisely because we are still inside the alternate screen,
    /// whose buffer is discarded moments later anyway. The user's real
    /// scrollback is never what gets cleared. `HalfBlock` painted cells, which
    /// the screen swap already takes.
    pub fn scrub(protocol: Protocol, out: &mut impl Write) -> std::io::Result<()> {
        match protocol {
            Protocol::HalfBlock => return Ok(()),
            Protocol::Kitty => out.write_all(&kitty::delete_all())?,
            Protocol::Sixel | Protocol::Iterm2 => out.write_all(b"\x1b[2J")?,
        }
        out.flush()
    }

    /// [`Painter::paint`] with the terminal handed in rather than measured — the
    /// measurement is an ioctl, and a test has no terminal to make one against.
    fn paint_with(
        &mut self,
        placements: Vec<Placement>,
        metrics: Metrics,
        out: &mut impl Write,
    ) -> std::io::Result<()> {
        // Half-blocks are already on screen: ratatui painted them as text. There
        // is no second pass for the fallback arm.
        if self.protocol == Protocol::HalfBlock {
            return Ok(());
        }
        if self.unchanged(&placements) && self.last_size == Some(metrics.size) {
            return Ok(());
        }

        // Save/restore the cursor around the whole pass: a sixel drags the cursor
        // along as it paints, and every arm needs it moved to the rect first. The
        // client's real cursor must not be collateral damage.
        out.write_all(b"\x1b7")?;

        if self.protocol == Protocol::Kitty {
            for index in 0..self.last.len() {
                out.write_all(&kitty::delete(KITTY_ID_BASE + index as u32))?;
            }
        }

        for (index, placement) in placements.iter().enumerate() {
            let bytes = self.encode(placement, metrics.cell, KITTY_ID_BASE + index as u32);
            if bytes.is_empty() {
                // An unencodable picture degrades to the half-block art already
                // painted underneath — never a blanked row (`ui/media.md`).
                continue;
            }
            // 1-based, row then column.
            write!(out, "\x1b[{};{}H", placement.row + 1, placement.col + 1)?;
            out.write_all(&bytes)?;
        }

        out.write_all(b"\x1b8")?;
        out.flush()?;
        self.last = placements;
        self.last_size = Some(metrics.size);
        Ok(())
    }

    fn unchanged(&self, placements: &[Placement]) -> bool {
        self.last.len() == placements.len()
            && std::iter::zip(&self.last, placements).all(|(old, new)| old.same_spot(new))
    }

    fn encode(&self, placement: &Placement, cell: CellSize, id: u32) -> Vec<u8> {
        let cells = (placement.cols, placement.rows);
        let (src_width, src_height) = placement.pixels.dimensions();
        let (crop_y, crop_height) = crop_window(placement.crop, placement.rows, src_height);
        // `SubImage` doesn't implement `GenericImageView` itself in this image
        // crate version, so materialize the crop before resizing it — a small
        // extra allocation, only ever paid while a span is actually clipped.
        let cropped =
            image::imageops::crop_imm(placement.pixels.as_ref(), 0, crop_y, src_width, crop_height)
                .to_image();
        // The cell box the art occupies already holds the picture's aspect (the
        // rasterizer derived the art's row count from it), so scaling to exactly
        // that box is what fills the rect without letterboxing it twice.
        let scaled = image::imageops::resize(
            &cropped,
            placement.cols as u32 * cell.width as u32,
            placement.rows as u32 * cell.height as u32,
            image::imageops::FilterType::Triangle,
        );
        match self.protocol {
            Protocol::Kitty => kitty::encode(&scaled, cells, id),
            Protocol::Iterm2 => iterm2::encode(&scaled, cells),
            Protocol::Sixel => sixel::encode(&scaled),
            // Returned above; the arm exists so a new protocol is a compile error
            // here rather than a silent fallthrough.
            Protocol::HalfBlock => Vec::new(),
        }
    }
}

/// This terminal, in one ioctl: its size in cells, and each cell's size in
/// pixels — or [`FALLBACK_CELL`] where it will not say.
///
/// The cell size is derived rather than asked for directly: terminals report the
/// window's total pixel size and its size in cells, and the ratio is what we
/// need. A zero in either means "pixel dimensions not supported" — a common
/// answer, and not an error. A failed ioctl reports a `(0, 0)` cell size, which
/// is stable rather than merely absent: an unknown that changed every frame
/// would re-emit every frame.
fn terminal_metrics() -> Metrics {
    let Ok(size) = crossterm::terminal::window_size() else {
        return Metrics {
            cell: FALLBACK_CELL,
            size: (0, 0),
        };
    };
    let cell = if size.width == 0 || size.height == 0 || size.columns == 0 || size.rows == 0 {
        FALLBACK_CELL
    } else {
        CellSize {
            width: size.width / size.columns,
            height: size.height / size.rows,
        }
    };
    Metrics {
        cell,
        size: (size.columns, size.rows),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_ui_ids as ids;

    /// A picture. Distinct calls mean distinct pictures — which is what the
    /// thumbnail cache produces, one `Arc` per content hash.
    fn picture() -> Pixels {
        std::sync::Arc::new(image::RgbImage::from_pixel(8, 8, image::Rgb([200, 30, 30])))
    }

    /// A placement of `picture` at `row`. Share one `Pixels` to mean "the same
    /// picture", mint a fresh one to mean "a different picture".
    fn placement_of(picture: &Pixels, row: u16) -> Placement {
        Placement {
            pixels: std::sync::Arc::clone(picture),
            col: 5,
            row,
            cols: 4,
            rows: 2,
            crop: None,
        }
    }

    /// An ordinary terminal: 8×16px cells, 120×40.
    const TERMINAL: Metrics = Metrics {
        cell: CellSize {
            width: 8,
            height: 16,
        },
        size: (120, 40),
    };

    fn scrub_bytes(protocol: Protocol) -> Vec<u8> {
        let mut out = Vec::new();
        Painter::scrub(protocol, &mut out).expect("a Vec never fails to write");
        out
    }

    /// The exit scrub emits a real retraction on every arm that can paint
    /// graphics, and nothing at all on the arm that cannot.
    ///
    /// This is the headless half of the "cannot exit" field finding: whether
    /// the bytes are *sent* is a mechanism, and mechanisms are testable — only
    /// "does the terminal then look clean" needs an eye. Without this, the
    /// whole fix would rest on a human's word.
    #[test]
    fn the_exit_scrub_retracts_on_every_graphics_arm() {
        assert_eq!(
            scrub_bytes(Protocol::HalfBlock),
            b"",
            "half-blocks are cells — the alternate-screen swap already takes \
             them, and writing escapes for them would be noise on exit"
        );

        let kitty = scrub_bytes(Protocol::Kitty);
        assert!(
            kitty.windows(7).any(|w| w == b"a=d,d=a"),
            "kitty owns an image layer, so the scrub must DELETE from it \
             (a=d,d=a — all placements), not clear the screen around it; got {:?}",
            String::from_utf8_lossy(&kitty)
        );

        // No retraction verb exists in either raster protocol, so the
        // erase-display is the only scrub available — safe only because the
        // alternate screen is still up when it runs.
        for protocol in [Protocol::Sixel, Protocol::Iterm2] {
            assert_eq!(
                scrub_bytes(protocol),
                b"\x1b[2J",
                "{protocol:?} has no delete verb — erase-display is the scrub"
            );
        }
    }

    fn paint(painter: &mut Painter, placements: Vec<Placement>) -> String {
        paint_on(painter, placements, TERMINAL)
    }

    fn paint_on(painter: &mut Painter, placements: Vec<Placement>, metrics: Metrics) -> String {
        let mut out = Vec::new();
        painter
            .paint_with(placements, metrics, &mut out)
            .expect("a Vec never fails to write");
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn the_half_block_arm_emits_nothing() {
        // ratatui already painted the art as text; a second pass would be a
        // second picture.
        let mut painter = Painter::new(Protocol::HalfBlock);
        assert_eq!(paint(&mut painter, vec![placement_of(&picture(), 3)]), "");
    }

    #[test]
    fn an_unchanged_frame_emits_nothing() {
        // The keystroke case: a tui redraws constantly and the image is already
        // where it belongs. Re-sending it per keypress is what makes a client
        // feel slow — and the terminal never asked for it.
        let mut painter = Painter::new(Protocol::Sixel);
        let art = picture();
        assert!(
            !paint(&mut painter, vec![placement_of(&art, 3)]).is_empty(),
            "the first paint emits"
        );
        assert_eq!(
            paint(&mut painter, vec![placement_of(&art, 3)]),
            "",
            "the second is a no-op"
        );
        assert_eq!(
            paint(&mut painter, vec![placement_of(&art, 3)]),
            "",
            "and stays one"
        );
    }

    #[test]
    fn a_moved_placement_is_repainted() {
        // The scroll case.
        let mut painter = Painter::new(Protocol::Sixel);
        let art = picture();
        paint(&mut painter, vec![placement_of(&art, 3)]);
        let scrolled = paint(&mut painter, vec![placement_of(&art, 2)]);
        assert!(
            scrolled.contains("\x1b[3;6H"),
            "positioned at the new row: {scrolled:?}"
        );
        assert!(scrolled.contains("\x1bP"), "and the picture follows");
    }

    #[test]
    fn a_changed_picture_in_the_same_spot_is_repainted() {
        // Same rect, different picture — a thumbnail that finished loading over a
        // placeholder, or an item re-fetched. A diff keyed on the rect alone
        // would show the old picture forever.
        let mut painter = Painter::new(Protocol::Sixel);
        paint(&mut painter, vec![placement_of(&picture(), 3)]);
        assert!(!paint(&mut painter, vec![placement_of(&picture(), 3)]).is_empty());
    }

    #[test]
    fn appearing_and_disappearing_images_repaint() {
        let mut painter = Painter::new(Protocol::Sixel);
        let (a, b) = (picture(), picture());
        paint(&mut painter, vec![placement_of(&a, 3)]);
        assert!(!paint(&mut painter, vec![placement_of(&a, 3), placement_of(&b, 6)]).is_empty());
        assert!(!paint(&mut painter, vec![placement_of(&a, 3)]).is_empty());
        assert!(
            !paint(&mut painter, vec![]).is_empty(),
            "clearing the page is a change"
        );
    }

    #[test]
    fn the_cursor_is_saved_and_restored_around_the_pass() {
        // A sixel drags the cursor with it. Leaving it there would strand the
        // client's own cursor wherever the last thumbnail ended.
        let mut painter = Painter::new(Protocol::Sixel);
        let emitted = paint(&mut painter, vec![placement_of(&picture(), 3)]);
        assert!(emitted.starts_with("\x1b7"), "saved: {emitted:?}");
        assert!(emitted.ends_with("\x1b8"), "restored");
    }

    #[test]
    fn each_placement_is_positioned_before_its_picture() {
        // 0-based cells, 1-based escape: row 3 col 5 → `ESC[4;6H`. An off-by-one
        // here paints every thumbnail one cell out, on every arm at once.
        let mut painter = Painter::new(Protocol::Sixel);
        let emitted = paint(&mut painter, vec![placement_of(&picture(), 3)]);
        assert!(
            emitted.contains("\x1b[4;6H"),
            "cursor to the rect: {emitted:?}"
        );
    }

    #[test]
    fn kitty_deletes_the_old_placements_before_repainting() {
        // The load-bearing arm difference: kitty images float above the text, so
        // repainting the cells underneath does NOT erase them. Without this the
        // old picture stays pinned where it was drawn, over whatever is there now.
        let mut painter = Painter::new(Protocol::Kitty);
        let art = picture();
        let first = paint(&mut painter, vec![placement_of(&art, 3)]);
        assert!(
            !first.contains("a=d"),
            "nothing to delete on the first paint"
        );

        let scrolled = paint(&mut painter, vec![placement_of(&art, 2)]);
        assert!(scrolled.contains("a=d,d=i"), "deletes by id: {scrolled:?}");
        let delete_at = scrolled.find("a=d").expect("a delete");
        let transmit_at = scrolled.find("a=T").expect("a transmit");
        assert!(delete_at < transmit_at, "the delete precedes the repaint");
    }

    #[test]
    fn a_kitty_delete_names_the_id_the_transmit_used() {
        // If these ever diverge the delete silently deletes nothing, and stale
        // pictures pile up on kitty's image layer.
        let mut painter = Painter::new(Protocol::Kitty);
        let art = picture();
        let first = paint(&mut painter, vec![placement_of(&art, 3)]);
        assert!(
            first.contains(&format!("i={KITTY_ID_BASE}")),
            "transmitted: {first:?}"
        );
        let scrolled = paint(&mut painter, vec![placement_of(&art, 2)]);
        assert!(
            scrolled.contains(&format!("a=d,d=i,i={KITTY_ID_BASE}")),
            "the same id is deleted: {scrolled:?}"
        );
    }

    #[test]
    fn every_live_kitty_image_is_deleted_when_the_frame_changes() {
        // Three images on screen, then one: the two that left must be deleted, or
        // they float over the rows that replaced them.
        let mut painter = Painter::new(Protocol::Kitty);
        let (a, b, c) = (picture(), picture(), picture());
        paint(
            &mut painter,
            vec![
                placement_of(&a, 0),
                placement_of(&b, 3),
                placement_of(&c, 6),
            ],
        );
        let shrunk = paint(&mut painter, vec![placement_of(&a, 0)]);
        for index in 0..3u32 {
            assert!(
                shrunk.contains(&format!("a=d,d=i,i={}", KITTY_ID_BASE + index)),
                "image {index} deleted: {shrunk:?}"
            );
        }
    }

    #[test]
    fn two_placements_of_the_same_picture_get_two_kitty_ids() {
        // Two items can carry the same thumbnail — they share one cache entry and
        // therefore one `Pixels`. Addressing both with one kitty id would make
        // the second transmit replace the first image rather than add one.
        let mut painter = Painter::new(Protocol::Kitty);
        let shared = picture();
        let emitted = paint(
            &mut painter,
            vec![placement_of(&shared, 0), placement_of(&shared, 4)],
        );
        assert!(
            emitted.contains(&format!("i={KITTY_ID_BASE}")),
            "first id: {emitted:?}"
        );
        assert!(
            emitted.contains(&format!("i={}", KITTY_ID_BASE + 1)),
            "second id: {emitted:?}"
        );
        assert_eq!(emitted.matches("a=T").count(), 2, "two transmits");
    }

    #[test]
    fn sixel_and_iterm2_never_delete_because_the_text_repaint_erases_them() {
        // If either ever needs a delete, the arm split in `paint`'s docs is wrong.
        for protocol in [Protocol::Sixel, Protocol::Iterm2] {
            let mut painter = Painter::new(protocol);
            let art = picture();
            paint(&mut painter, vec![placement_of(&art, 3)]);
            let scrolled = paint(&mut painter, vec![placement_of(&art, 2)]);
            assert!(
                !scrolled.contains("a=d"),
                "{protocol:?} emitted a kitty delete"
            );
        }
    }

    #[test]
    fn each_arm_emits_its_own_introducer() {
        let cases = [
            (Protocol::Sixel, "\x1bP"),
            (Protocol::Kitty, "\x1b_G"),
            (Protocol::Iterm2, "\x1b]1337;"),
        ];
        for (protocol, introducer) in cases {
            let mut painter = Painter::new(protocol);
            let emitted = paint(&mut painter, vec![placement_of(&picture(), 3)]);
            assert!(
                emitted.contains(introducer),
                "{protocol:?} emits {introducer:?}"
            );
        }
    }

    #[test]
    fn the_image_is_scaled_into_its_cell_box() {
        // The rect the art occupies IS the picture's box; a sixel sized to
        // anything else spills into a neighbour's cells or underfills its own.
        // 4 cells × 8px = 32 wide, 2 cells × 16px = 32 tall.
        let mut painter = Painter::new(Protocol::Sixel);
        let emitted = paint(&mut painter, vec![placement_of(&picture(), 3)]);
        assert!(
            emitted.contains("\"1;1;32;32"),
            "raster attributes should be the cell box in pixels: {emitted:?}"
        );
    }

    #[test]
    fn a_cropped_placement_still_scales_into_its_shrunk_cell_box() {
        // Only the bottom half of a 2-row art is visible: cols unchanged
        // (4×8=32), rows shrunk to 1 (1×16=16) by the caller before this ever
        // sees the placement — the same box math as the uncropped case, just
        // starting from the already-clipped `rows`. Regression: a crop that
        // resized into the ORIGINAL (pre-clip) box would spill into whatever is
        // one line below the block, corrupting the frame.
        let mut painter = Painter::new(Protocol::Sixel);
        let placement = Placement {
            crop: Some((1, 2)),
            rows: 1,
            ..placement_of(&picture(), 3)
        };
        let emitted = paint(&mut painter, vec![placement]);
        assert!(
            emitted.contains("\"1;1;32;16"),
            "raster attributes should be the SHRUNK cell box: {emitted:?}"
        );
    }

    #[test]
    fn crop_window_selects_the_right_pixel_band() {
        // A 100px-tall source, 4 cell-rows total.
        assert_eq!(
            crop_window(None, 2, 100),
            (0, 100),
            "no crop is the whole image"
        );
        assert_eq!(
            crop_window(Some((0, 4)), 2, 100),
            (0, 50),
            "top half visible (rows 0-1 of 0-3)"
        );
        assert_eq!(
            crop_window(Some((2, 4)), 2, 100),
            (50, 50),
            "bottom half visible (rows 2-3 of 0-3)"
        );
        assert_eq!(
            crop_window(Some((1, 4)), 2, 100),
            (25, 50),
            "the middle two rows of 4 visible"
        );
    }

    #[test]
    fn same_spot_requires_equal_crop() {
        // A scroll that changes which band of a thumbnail is visible must
        // re-emit — `crop` decides what's actually on screen even when every
        // other field coincidentally matches.
        let art = picture();
        let whole = Placement {
            pixels: std::sync::Arc::clone(&art),
            col: 5,
            row: 3,
            cols: 4,
            rows: 2,
            crop: None,
        };
        let cropped = Placement {
            crop: Some((0, 3)),
            ..whole.clone()
        };
        assert!(
            !whole.same_spot(&cropped),
            "a crop change must not read as the same spot"
        );
        assert!(
            cropped.same_spot(&cropped.clone()),
            "identical crop IS the same spot"
        );
    }

    #[test]
    fn a_resize_re_emits_even_when_no_placement_moved() {
        // The bug this exists to catch: ratatui redraws everything when the
        // terminal resizes, and for the compositing arms that redraw ERASES every
        // image — while a purely horizontal resize leaves each thumbnail's rect
        // untouched. Diffing placements alone would say "nothing changed" about
        // pictures that had just been wiped off the screen, and they would stay
        // gone until something else moved them.
        let mut painter = Painter::new(Protocol::Sixel);
        let art = picture();
        paint_on(&mut painter, vec![placement_of(&art, 3)], TERMINAL);
        assert_eq!(
            paint_on(&mut painter, vec![placement_of(&art, 3)], TERMINAL),
            "",
            "same terminal, same placement: still a no-op"
        );

        let wider = Metrics {
            size: (140, 40),
            ..TERMINAL
        };
        assert!(
            !paint_on(&mut painter, vec![placement_of(&art, 3)], wider).is_empty(),
            "a resize must re-emit even though the placement is identical"
        );
        assert_eq!(
            paint_on(&mut painter, vec![placement_of(&art, 3)], wider),
            "",
            "and then settle again at the new size"
        );
    }

    #[test]
    fn an_unmeasurable_terminal_does_not_re_emit_every_frame() {
        // A failed ioctl reports a stable (0, 0) rather than an absent size; an
        // unknown that changed every frame would re-send every sixel on every
        // keystroke.
        let unknown = Metrics {
            cell: FALLBACK_CELL,
            size: (0, 0),
        };
        let mut painter = Painter::new(Protocol::Sixel);
        let art = picture();
        assert!(!paint_on(&mut painter, vec![placement_of(&art, 3)], unknown).is_empty());
        assert_eq!(
            paint_on(&mut painter, vec![placement_of(&art, 3)], unknown),
            ""
        );
    }

    /// The vacuous-e2e trap this whole design is shaped around
    /// ([`super`] module docs, reason 3).
    ///
    /// `painted_thumbnail_count`'s tui branch counts `▀` in `media-thumbnail`'s
    /// registry text. That text comes from the element, not the frame — so an arm
    /// that painted *only* pixels would leave it empty, the count would drop to
    /// zero, and `test_media.py` would keep passing while asserting nothing. The
    /// element must carry both representations no matter which arm is live.
    #[test]
    fn a_live_protocol_never_empties_the_registry_text() {
        let thumbnail = crate::thumbnail::Thumbnail {
            art: crate::thumbnail::HalfBlockArt {
                rows: vec![vec![
                    crate::thumbnail::HalfBlockCell {
                        top: [1, 2, 3],
                        bottom: [4, 5, 6],
                    };
                    4
                ]],
            },
            pixels: picture(),
        };
        let element = crate::element::Element::thumbnail(ids::MEDIA_THUMBNAIL, thumbnail);

        assert!(
            element.text.contains(crate::thumbnail::HALF_BLOCK),
            "the registry text is the art's glyphs — the e2e paint assertion reads this"
        );
        assert!(
            element.art.is_some(),
            "the fallback arm stays painted underneath"
        );
        assert!(
            element.pixels.is_some(),
            "and the protocol arm has its pixels"
        );
    }
}
