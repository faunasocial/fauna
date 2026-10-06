//! Linux's `SourceGlyph → emoji` call site. The *concept* is decided once in
//! [`fauna_core::source_glyph::SourceGlyph`] (D5;
//! `docs/goal/architecture/render-model.md` § Deltas), and since every app
//! renders that concept as the same emoji, the **map** is shared too —
//! [`SourceGlyph::emoji`]. Linux paints the result in a `gtk::Label`, the
//! family the conversations rail already used; GTK's symbolic-icon theme has no
//! fox / butterfly glyph, so emoji is the only family that can render the
//! canonical concept here.
//!
//! One map, shared by the conversation list, the thread header, AND the feed
//! badge (`feed::post_list::build_protocol_badges`), so the rail and the badge
//! can never drift from the concept — or each other — again.

use fauna_core::source_glyph::SourceGlyph;

/// The emoji Linux paints for a [`SourceGlyph`] concept.
///
/// A thin alias over the shared map: this crate's call sites take the free
/// function rather than the method, and keeping the name lets the module doc
/// above stay the one place that explains linux's icon family.
pub fn source_glyph_emoji(glyph: SourceGlyph) -> &'static str {
    glyph.emoji()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_concept_renders_the_shared_emoji() {
        // Linux no longer owns these bytes — it asserts it is showing the
        // canonical ones. `fauna_core::source_glyph` pins their values (and the
        // envelope's emoji-presentation selector) for every app at once.
        for glyph in SourceGlyph::ALL {
            assert_eq!(source_glyph_emoji(*glyph), glyph.emoji(), "{}", glyph.id());
        }
    }
}
