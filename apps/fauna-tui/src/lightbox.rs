//! The `image-lightbox` overlay — ui.yaml's `components.image-lightbox`, the
//! one painter every page that opens a picture full-screen paints through.
//!
//! The component is `used_in` the feed (`post-image`, inside `post-card`) and
//! `conversations.conversation_detail` (a received `dm-attachment-image`). Both
//! pages hold the same state, `lightbox: Option<String>` (the open picture's
//! blob hash, set by the trigger's click and cleared by Escape), and paint it
//! here, so the two cannot drift: the enlargement width, the placeholder for a
//! picture whose bytes never arrived, and the flat (unscoped) paint are this
//! module's alone. Where each page *finds* the picture differs — the feed's
//! [`crate::image_cache`], the conversations attachment store — so the caller
//! hands in a lookup. The lightbox is app glue, not shared Rust
//! (`docs/goal/ui/conversations.md` § Where logic lives → *App glue*).

use std::borrow::Borrow;

use fauna_ui_ids as ids;

use crate::element::Element;
use crate::thumbnail::{LIGHTBOX_COLS, PLACEHOLDER, Thumbnail, rasterize_rgb};

/// The `image-lightbox` overlay for the picture `open` names, or nothing when
/// no picture is open.
///
/// `picture` resolves the open hash to the thumbnail the page already holds
/// for it. The picture is RE-RASTERIZED from its cached
/// [`Thumbnail::pixels`] at [`LIGHTBOX_COLS`] rather than repeating the
/// inline preview, so it is genuinely enlarged — the encoded bytes are long
/// gone by then, so this is the only way to a bigger picture without a second
/// fetch. A picture the lookup cannot resolve (bytes not arrived, evicted,
/// undecodable) still opens, as the placeholder: a click that silently did
/// nothing is the dropped-action shape testing.md point 11 forbids.
///
/// Painted flat, not scoped: a driver reads `is_visible("image-lightbox")`
/// unscoped, and there is exactly one at a time. A page paints it LAST — a
/// modal overlay paints over everything below it.
pub(crate) fn elements<T: Borrow<Thumbnail>>(
    open: Option<&str>,
    picture: impl FnOnce(&str) -> Option<T>,
) -> Vec<Element> {
    let Some(hash) = open else {
        return Vec::new();
    };
    let enlarged =
        picture(hash).and_then(|thumb| rasterize_rgb(&thumb.borrow().pixels, LIGHTBOX_COLS));
    vec![match enlarged {
        Some(thumb) => Element::thumbnail(ids::IMAGE_LIGHTBOX, thumb),
        None => Element::label(ids::IMAGE_LIGHTBOX, PLACEHOLDER),
    }]
}
