//! Canonical content-moderation **category vocabulary** and its **badge
//! presentation** — the single shared-Rust source of the category set + the
//! `category → (label, icon, colour)` map every app's `ContentLabelBadge`
//! renders from.
//!
//! `docs/goal/behavior/moderation.md` § Categories & enforcement ratifies the
//! 5-category vocabulary (`spam` / `trusted` / `nsfw` / `phishing` /
//! `commercial`); § Where logic lives + rule 3 mandate that "no client
//! hard-codes the set or its styling" — the badge label/colour/icon is a shared
//! enum keyed on the wire `category` string. This module is that definition,
//! mirroring the `fauna_protocol::spam::spam_threshold_band` /
//! `fauna_client_search::render::content_type_badge` precedents: the pure logic
//! lives here, with a UniFFI face (`fauna-ffi` `contentLabelStyle`) and a wasm
//! face (`fauna-wasm` `contentLabelStyle`) so the native apps and web render
//! from one definition instead of each re-deriving the map (priority #2). It
//! removes web's `category-*` CSS map in `ContentLabelBadge.svelte` (drift #157)
//! and android's `when (category)` map in `ContentLabelBadge.kt`.

use crate::localized::LocalizedText;

/// The canonical content-moderation category vocabulary (moderation.md
/// § Categories & enforcement). A classifier MUST emit only the 5 named
/// variants; [`ContentCategory::from_wire`] maps any off-list string to
/// [`ContentCategory::Other`] (rendered verbatim, first letter capitalized).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentCategory {
    Spam,
    Trusted,
    Nsfw,
    Phishing,
    Commercial,
    /// Any string outside the canonical 5 — forward-compat / a producer that
    /// emits an off-list label. Renders grey with the raw string capitalized.
    Other,
}

impl ContentCategory {
    /// Map a wire `category` string to its canonical variant. Unknown → `Other`.
    pub fn from_wire(category: &str) -> Self {
        match category {
            "spam" => Self::Spam,
            "trusted" => Self::Trusted,
            "nsfw" => Self::Nsfw,
            "phishing" => Self::Phishing,
            "commercial" => Self::Commercial,
            _ => Self::Other,
        }
    }

    /// The `i18n/strings/en.yaml` key for the badge label, for the 5 known
    /// categories. `Other` has no fixed key — the caller renders the raw
    /// (capitalized) string as the key, which `resolve` echoes verbatim.
    fn label_key(self) -> Option<&'static str> {
        Some(match self {
            Self::Spam => "moderation.category.spam",
            Self::Trusted => "moderation.category.trusted",
            Self::Nsfw => "moderation.category.nsfw",
            Self::Phishing => "moderation.category.phishing",
            Self::Commercial => "moderation.category.commercial",
            Self::Other => return None,
        })
    }

    /// The emoji icon — identical web+android today, hoisted here so it stays so.
    fn icon(self) -> &'static str {
        match self {
            Self::Spam => "\u{26A0}\u{FE0F}",   // ⚠️
            Self::Trusted => "\u{2705}",        // ✅
            Self::Nsfw => "\u{1F648}",          // 🙈
            Self::Phishing => "\u{1F517}",      // 🔗
            Self::Commercial => "\u{1F4E2}",    // 📢
            Self::Other => "\u{1F3F7}\u{FE0F}", // 🏷️
        }
    }

    /// The background-tint base colour (hex). Each app applies its own
    /// low-alpha tint convention (web `rgba(.., 0.15)`, android `copy(alpha=.15)`).
    fn tint(self) -> &'static str {
        match self {
            Self::Spam | Self::Phishing => "#EF4444",
            Self::Trusted => "#22C55E",
            Self::Nsfw => "#F97316",
            Self::Commercial => "#EAB308",
            Self::Other => "#6B7280",
        }
    }

    /// The higher-contrast text/icon accent colour (hex) — the Tailwind-600 tone
    /// web already uses (the richer of the two patterns; android single-tone
    /// drift resolves toward this, priority #4).
    fn accent(self) -> &'static str {
        match self {
            Self::Spam | Self::Phishing => "#DC2626",
            Self::Trusted => "#16A34A",
            Self::Nsfw => "#EA580C",
            Self::Commercial => "#CA8A04",
            Self::Other => "#6B7280",
        }
    }
}

/// Shared presentation of a content-label badge: the localized `label`, an emoji
/// `icon`, and the two-tone colour (`tint` background base + higher-contrast
/// `accent` for text/icon), all hex. Each app maps the hex strings to its
/// native colour type and resolves `label.key` through its own i18n pipeline; no
/// client hard-codes the category→style map (moderation.md § Where logic lives —
/// the drift #157 lift).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ContentLabelStyle {
    pub label: LocalizedText,
    pub icon: String,
    pub tint: String,
    pub accent: String,
}

/// The per-row content-label data unit (`moderation.md` § Per-row badge data
/// path, ratified 2026-07-16) — one classifier verdict on one content item.
/// `confidence_per_mille` is the 0–1000 scaling of the classifier's 0.0–1.0
/// confidence (the dag-cbor wire forbids floats — the established convention,
/// e.g. [`crate::format::confidence_percent`]). Rides directly on the wire
/// embedded in `fauna_protocol::feed::FeedPostItem.labels` /
/// `fauna_client_conversations::MessageSnapshot.labels` (no protocol-local
/// mirror needed — this type is already float-free). `category` is one of the
/// canonical 5 (or an off-list producer string, which
/// [`content_label_style`] degrades to the grey `Other` badge).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ContentLabelEntry {
    pub category: String,
    pub confidence_per_mille: u16,
}

/// The single label a `content-label-badge` renders when an item carries
/// more than one classifier verdict — the highest-confidence entry (a tie
/// resolves to the last one found, `Iterator::max_by_key`'s convention).
/// One shared decision so a feed post-card and a DM bubble (and every future
/// caller) agree on which of several `labels` wins the one visible badge,
/// mirroring [`content_label_style`]'s "no client re-derives the map" rule.
pub fn primary_content_label(labels: &[ContentLabelEntry]) -> Option<&ContentLabelEntry> {
    labels.iter().max_by_key(|l| l.confidence_per_mille)
}

/// Merge a nest's verdicts for one item into the ones the device derived
/// itself — the moderation queue's **superset rule**, per category
/// (`moderation.md` § Implementation status today → *Web queue data-source*:
/// the union, the server row winning).
///
/// Every server entry is kept; a local entry survives only for a category the
/// server said nothing about. A server verdict wins its category even when the
/// local one is stronger: two scorers disagreeing is not settled by taking the
/// louder one, and the server's is the one every member of the room sees. The
/// local half never leaves the device — this only decides what this device
/// renders. The server's entries come first, in their own order, then the
/// surviving local ones in theirs.
pub fn merge_server_labels(
    server: &[ContentLabelEntry],
    local: &[ContentLabelEntry],
) -> Vec<ContentLabelEntry> {
    let mut merged = server.to_vec();
    merged.extend(
        local
            .iter()
            .filter(|l| !server.iter().any(|s| s.category == l.category))
            .cloned(),
    );
    merged
}

/// Capitalize the first character of `s` (ASCII-and-Unicode safe), leaving the
/// rest unchanged — the `Other`-category label fallback (mirrors the prior
/// per-app `replaceFirstChar { uppercase }` / `charAt(0).toUpperCase()`).
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Map a wire `category` string to its full badge presentation. The single
/// shared definition both UniFFI (`fauna-ffi`) and wasm (`fauna-wasm`) faces wrap.
pub fn content_label_style(category: &str) -> ContentLabelStyle {
    let cat = ContentCategory::from_wire(category);
    let label = match cat.label_key() {
        Some(key) => LocalizedText::key(key),
        // Unknown → render the raw string capitalized; no i18n entry matches it,
        // so `resolve` echoes the key verbatim (the prior per-app behaviour).
        None => LocalizedText::key(capitalize_first(category)),
    };
    ContentLabelStyle {
        label,
        icon: cat.icon().to_string(),
        tint: cat.tint().to_string(),
        accent: cat.accent().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_wire_maps_the_canonical_five_and_others() {
        assert_eq!(ContentCategory::from_wire("spam"), ContentCategory::Spam);
        assert_eq!(
            ContentCategory::from_wire("trusted"),
            ContentCategory::Trusted
        );
        assert_eq!(ContentCategory::from_wire("nsfw"), ContentCategory::Nsfw);
        assert_eq!(
            ContentCategory::from_wire("phishing"),
            ContentCategory::Phishing
        );
        assert_eq!(
            ContentCategory::from_wire("commercial"),
            ContentCategory::Commercial
        );
        // Anything off-list → Other.
        assert_eq!(ContentCategory::from_wire("csam"), ContentCategory::Other);
        assert_eq!(ContentCategory::from_wire(""), ContentCategory::Other);
    }

    #[test]
    fn style_of_each_known_category_matches_the_canonical_map() {
        // Labels are i18n keys (clients localize); icon + two-tone colour are the
        // canonical values lifted from web's (richer) CSS map.
        let spam = content_label_style("spam");
        assert_eq!(spam.label.key, "moderation.category.spam");
        assert_eq!(spam.icon, "\u{26A0}\u{FE0F}");
        assert_eq!(spam.tint, "#EF4444");
        assert_eq!(spam.accent, "#DC2626");

        let trusted = content_label_style("trusted");
        assert_eq!(trusted.label.key, "moderation.category.trusted");
        assert_eq!(trusted.icon, "\u{2705}");
        assert_eq!(trusted.tint, "#22C55E");
        assert_eq!(trusted.accent, "#16A34A");

        let nsfw = content_label_style("nsfw");
        assert_eq!(nsfw.label.key, "moderation.category.nsfw");
        assert_eq!(nsfw.icon, "\u{1F648}");
        assert_eq!(nsfw.tint, "#F97316");
        assert_eq!(nsfw.accent, "#EA580C");

        let phishing = content_label_style("phishing");
        assert_eq!(phishing.label.key, "moderation.category.phishing");
        assert_eq!(phishing.icon, "\u{1F517}");
        // phishing shares spam's red.
        assert_eq!(phishing.tint, "#EF4444");
        assert_eq!(phishing.accent, "#DC2626");

        let commercial = content_label_style("commercial");
        assert_eq!(commercial.label.key, "moderation.category.commercial");
        assert_eq!(commercial.icon, "\u{1F4E2}");
        assert_eq!(commercial.tint, "#EAB308");
        assert_eq!(commercial.accent, "#CA8A04");
    }

    #[test]
    fn unknown_category_renders_capitalized_verbatim_in_grey() {
        let s = content_label_style("csam");
        // No i18n key — the raw string, capitalized, is the key; resolve echoes it.
        assert_eq!(s.label.key, "Csam");
        assert_eq!(s.label.resolve(|_| None::<&str>), "Csam");
        assert_eq!(s.icon, "\u{1F3F7}\u{FE0F}");
        assert_eq!(s.tint, "#6B7280");
        assert_eq!(s.accent, "#6B7280");
    }

    #[test]
    fn empty_category_does_not_panic() {
        let s = content_label_style("");
        assert_eq!(s.label.key, "");
        assert_eq!(s.tint, "#6B7280");
    }

    #[test]
    fn capitalize_first_handles_edges() {
        assert_eq!(capitalize_first(""), "");
        assert_eq!(capitalize_first("a"), "A");
        assert_eq!(capitalize_first("csam"), "Csam");
        assert_eq!(capitalize_first("Already"), "Already");
    }

    #[test]
    fn primary_content_label_picks_the_highest_confidence_entry() {
        let labels = [
            ContentLabelEntry {
                category: "spam".into(),
                confidence_per_mille: 400,
            },
            ContentLabelEntry {
                category: "nsfw".into(),
                confidence_per_mille: 900,
            },
            ContentLabelEntry {
                category: "phishing".into(),
                confidence_per_mille: 100,
            },
        ];
        assert_eq!(primary_content_label(&labels).unwrap().category, "nsfw");
    }

    #[test]
    fn primary_content_label_of_empty_is_none() {
        assert!(primary_content_label(&[]).is_none());
    }

    fn entry(category: &str, confidence_per_mille: u16) -> ContentLabelEntry {
        ContentLabelEntry {
            category: category.into(),
            confidence_per_mille,
        }
    }

    #[test]
    fn a_server_verdict_wins_its_category_and_a_local_one_keeps_the_rest() {
        // `moderation.md` § Implementation status today → *Web queue
        // data-source*: the superset, the server row winning — here per
        // category, the per-row unit. A local detection the server did not
        // make stays (it never left the device); one the server also made
        // yields to the server's, whichever is stronger.
        let local = vec![entry("spam", 950), entry("phishing", 400)];
        let server = vec![entry("spam", 600), entry("nsfw", 700)];
        let merged = merge_server_labels(&server, &local);
        assert_eq!(
            merged,
            vec![
                entry("spam", 600),
                entry("nsfw", 700),
                entry("phishing", 400)
            ],
        );
    }

    #[test]
    fn merging_nothing_changes_nothing() {
        let local = vec![entry("spam", 950)];
        assert_eq!(merge_server_labels(&[], &local), local);
        assert_eq!(merge_server_labels(&local, &[]), local);
    }
}
