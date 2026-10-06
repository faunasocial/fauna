//! UniFFI façade for the shared content-label badge presentation
//! ([`fauna_core::content_category::content_label_style`]).
//!
//! Gives Apple / Windows / Android the identical category vocabulary + badge
//! `label`/`icon`/`colour` map so no client hard-codes it (priority #1/#2 — the
//! moderation analog of `search::search_content_type_badge` /
//! `spam::spam_threshold_band`). The Rust-native Linux app calls
//! `fauna_core::content_category::*` directly. The return
//! [`ContentLabelStyle`](fauna_core::content_category::ContentLabelStyle) embeds
//! a `fauna_core` `LocalizedText`, which `uniffi-bindgen-go` can't emit as a
//! cross-namespace import — so this module is gated `moderation-badge`
//! (default-on; off in the Go mail-bridge `--no-default-features` build, which
//! has no moderation surface), same as `search` / `value-format`. See
//! `docs/goal/behavior/moderation.md` § Where logic lives.

use fauna_core::content_category::{ContentLabelEntry, ContentLabelStyle};

/// UniFFI face of
/// [`fauna_core::content_category::content_label_style`] — map a wire `category`
/// string (one of the canonical 5, else `Other`) to its badge presentation. The
/// client resolves the returned `label.key` through its i18n pipeline and maps
/// `tint`/`accent` hex to its native colour type.
#[uniffi::export]
pub fn content_label_style(category: String) -> ContentLabelStyle {
    fauna_core::content_category::content_label_style(&category)
}

/// UniFFI face of
/// [`fauna_core::content_category::primary_content_label`] — pick the
/// highest-confidence label entry (`None` if `labels` is empty). One shared
/// decision so a feed post-card, a DM bubble, and the moderation queue all
/// agree on which of several `labels` wins the one visible `content-label-badge`
/// — no client re-derives the "which category wins" reduce (moderation.md
/// § Per-row badge data path).
#[uniffi::export]
pub fn primary_content_label(labels: Vec<ContentLabelEntry>) -> Option<ContentLabelEntry> {
    fauna_core::content_category::primary_content_label(&labels).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_delegates_for_known_and_unknown() {
        let spam = content_label_style("spam".into());
        assert_eq!(spam.label.key, "moderation.category.spam");
        assert_eq!(spam.tint, "#EF4444");
        // Unknown → grey, raw string capitalized as the (passthrough) key.
        let other = content_label_style("csam".into());
        assert_eq!(other.label.key, "Csam");
        assert_eq!(other.tint, "#6B7280");
    }

    #[test]
    fn primary_content_label_picks_the_highest_confidence_entry() {
        let labels = vec![
            ContentLabelEntry {
                category: "spam".into(),
                confidence_per_mille: 400,
            },
            ContentLabelEntry {
                category: "nsfw".into(),
                confidence_per_mille: 900,
            },
        ];
        assert_eq!(primary_content_label(labels).unwrap().category, "nsfw");
    }

    #[test]
    fn primary_content_label_of_empty_is_none() {
        assert!(primary_content_label(vec![]).is_none());
    }
}
