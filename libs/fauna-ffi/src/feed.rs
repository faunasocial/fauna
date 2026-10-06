//! UniFFI façade for shared post-source classification (`fauna_feed`).
//!
//! The wire `source` field is a comma-separated origin-protocol list
//! (`"fauna, bluesky"`, `docs/goal/ui/feed.md` § Posts). `fauna_feed::classify_sources`
//! turns it into an ordered, deduplicated `Vec<SourceKind>` with canonical labels.
//! This façade projects each `SourceKind` to a flat [`FfiSourceBadge`] `{ id, label }`
//! — the same shape web reads off `fauna_wasm::classifySources` — so Apple / Windows /
//! Android key their genuinely platform-specific icon map (SF Symbols / Material / GTK)
//! off the stable `id` and read the user-facing label from one definition, instead of
//! each re-encoding the `source -> badge` switch (priority #1/#2/#4).
//!
//! The return type is a **fauna-ffi-local** `uniffi::Record` (built-in `String` fields,
//! not a bare `fauna_feed` type) so uniffi-bindgen-go emits a self-contained Go binding —
//! no cross-namespace import (same reason `src/markdown.rs` mirrors its tokens). The Go
//! mail bridge never renders badges, but keeping the export gate-free is harmless.

use fauna_core::source_glyph::SourceGlyph;
use fauna_feed::{SourceKind, classify_sources as classify};

/// One classified post-source badge: a stable lowercase `id`
/// (`"fauna" | "bluesky" | "nostr" | "activitypub" | "email" | "other"`) for keying a
/// client's platform icon map, the canonical user-facing display `label`
/// (e.g. `ActivityPub` → "Fediverse"; an unknown token shows itself; an empty one
/// shows "Unknown"), and the shared `glyph` icon concept. Mirror of [`SourceKind`]'s
/// `id`/`label`/`glyph` accessors.
///
/// `glyph` is the feed-badge half of the shared `SourceGlyph` concept the
/// conversations rail also resolves to (`Rail::glyph` / `SourceKind::glyph`), so a
/// native app keys ONE `SourceGlyph → asset` map off it for both surfaces —
/// killing the within-client rail-vs-badge split (render-model.md § Deltas → D5).
#[derive(uniffi::Record)]
pub struct FfiSourceBadge {
    pub id: String,
    pub label: String,
    pub glyph: SourceGlyph,
}

impl From<SourceKind> for FfiSourceBadge {
    fn from(kind: SourceKind) -> Self {
        FfiSourceBadge {
            id: kind.id(),
            label: kind.label(),
            glyph: kind.glyph(),
        }
    }
}

/// Classify the comma-separated wire `source` field into ordered, deduplicated source
/// badges (`docs/goal/ui/feed.md` § Where logic lives). Order follows first appearance;
/// duplicates collapse; empty/whitespace tokens are skipped, so an empty or
/// unknown-only-empty field yields an empty list (the client renders no badge). A
/// multi-source post (`"fauna, bluesky"`) yields one badge per origin.
#[uniffi::export]
pub fn classify_sources(source_field: String) -> Vec<FfiSourceBadge> {
    // No bridges roster yet — see `fauna_wasm::feed::classify_sources`.
    classify(&source_field, &[])
        .into_iter()
        .map(Into::into)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The badge projection must carry the shared `glyph` concept so a native
    /// app keys the SAME `SourceGlyph → asset` map off the feed badge that it
    /// uses for the conversations rail (D5). Pins the projection to
    /// `SourceKind::glyph()`; the canonical concept-per-source decision itself is
    /// pinned in `fauna-feed` / `fauna-core`.
    #[test]
    fn badge_carries_the_source_glyph() {
        let badges = classify_sources(
            "fauna, bluesky, activitypub, nostr, email, facebook, instagram, rss".into(),
        );
        let glyphs: Vec<_> = badges.iter().map(|b| b.glyph).collect();
        assert_eq!(
            glyphs,
            vec![
                SourceGlyph::Fox,
                SourceGlyph::Butterfly,
                SourceGlyph::Globe,
                SourceGlyph::Bolt,
                SourceGlyph::Envelope,
                SourceGlyph::Archive,
                SourceGlyph::Archive,
                SourceGlyph::Unknown, // "rss" is outside the known set
            ],
        );
        // …and the projection still carries id + label unchanged.
        assert_eq!(badges[0].id, "fauna");
        assert_eq!(badges[2].label, "Fediverse");
        assert_eq!(badges[5].label, "Facebook");
    }
}
