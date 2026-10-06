//! Canonical source/protocol **glyph** — the single Rust-owned mapping from an
//! origin protocol to the *concept* its icon depicts (a fox for the native
//! Fauna protocol, a butterfly for Bluesky, …) **and to the emoji every app
//! paints for it**, instead of each app privately re-deciding either.
//!
//! The concept was shared first (D5) and the map was left per-app on the
//! premise that each would render it in its own native asset family. Every one
//! of the seven then chose emoji — and the same six — so what stayed behind was
//! seven copies of one table, which had already begun to disagree
//! ([`SourceGlyph::emoji`] records the drift). Both halves live here now.
//!
//! Both the conversations rail (`fauna_conversations::Rail::glyph`) and the
//! feed badge (`fauna_feed::SourceKind::glyph`) resolve to this enum, so within
//! an app the rail and the badge cannot split either. Forcing the concept into
//! Rust is the priority-#1 fix for the
//! drift documented in `docs/goal/architecture/render-model.md` § Deltas → D5:
//! Apple mapped FaunaMls→lock / Bluesky→cloud while the others used a
//! fox / butterfly, and web even split fox (rail) vs leaf (feed badge) for the
//! *same* source.
//!
//! **Variant names are the concept, not the source** (`Fox`, not `Fauna`): the
//! canonical brand decision lives here, so a re-brand is a deliberate change to
//! this one mapping that ripples to every app's exhaustive match — exactly
//! the priority-#1 safety the drift cost us.
//!
//! Canonical concept per source, ratified by the user 2026-06-22:
//! Fauna → Fox, Bluesky → Butterfly, Fediverse (ActivityPub / Mastodon /
//! Pleroma / …) → Globe, Nostr → Bolt, Email → Envelope, an archive import
//! (Facebook / Instagram) → Archive (a box; ratified with the archive-import
//! design 2026-09-06), anything else → Unknown. A third-party bridge
//! (`ui/conversations.md` § Where logic lives → *The `Bridged` adapter*,
//! ruled 2026-10-02) declares one id of this set in its manifest; `Bridge` — a
//! bridge, 🌉 — is the generic concept for one whose identity is not to hand.

use serde::{Deserialize, Serialize};

/// The concept a source/protocol icon depicts. A stable semantic token; the
/// only icon code that stays per-app is the *call site* that asks
/// [`SourceGlyph::emoji`] for the glyph and hands it to the native widget.
///
/// Serializes to its lowercase id (`"fox"`, `"butterfly"`, …) so web reads the
/// same key off the serialized conversations snapshot and the feed badge;
/// native apps switch on the `uniffi::Enum` directly.
///
/// `Archive` sits after `Unknown`, out of the source-family grouping above,
/// by design: UniFFI numbers variants in declaration order and the Go
/// binding is hand-mirrored, so a new concept is appended at the end rather
/// than inserted where it reads best (see
/// `archive_is_last_so_existing_ffi_discriminants_are_stable` below).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SourceGlyph {
    /// The native Fauna protocol — a fox (the project's animal brand).
    Fox,
    /// Email / SMTP — an envelope.
    Envelope,
    /// Bluesky / the AT Protocol — a butterfly (Bluesky's brand mark).
    Butterfly,
    /// Nostr — a lightning bolt.
    Bolt,
    /// The Fediverse (ActivityPub: Mastodon / Pleroma / Misskey / …) — a globe.
    /// Deliberately the *generic* fediverse concept, not the Mastodon elephant,
    /// since the source is broader than Mastodon.
    Globe,
    /// A source outside the known set — a generic broadcast / antenna concept.
    Unknown,
    /// Content re-authored from an export archive (a Facebook or Instagram
    /// import, `behavior/archive-import.md`) — a box. The feed badge of an
    /// imported post and the `Archive` conversations rail both resolve here;
    /// the badge label names the platform.
    Archive,
    /// A bridge — the generic concept for a conversation or post carried by a
    /// bridge principal (`ui/conversations.md` § Where logic lives → *The
    /// `Bridged` adapter*): what `Rail::Bridged` paints when the thread carries
    /// no bridge identity, and a glyph a bridge manifest may declare
    /// (`architecture/third-party.md` § The manifest → *The `bridge` block*).
    /// Appended last, like `Archive`, so existing FFI discriminants stand.
    Bridge,
}

impl SourceGlyph {
    /// Every concept, in declaration order — so a test or a picker can walk the
    /// set without re-listing it (and a new variant must be added here too).
    pub const ALL: &'static [SourceGlyph] = &[
        SourceGlyph::Fox,
        SourceGlyph::Envelope,
        SourceGlyph::Butterfly,
        SourceGlyph::Bolt,
        SourceGlyph::Globe,
        SourceGlyph::Unknown,
        SourceGlyph::Archive,
        SourceGlyph::Bridge,
    ];

    /// Stable lowercase id — one of `"fox" | "envelope" | "butterfly" | "bolt"
    /// | "globe" | "unknown" | "archive" | "bridge"`. Matches the serde form, so it is
    /// the key web uses for its emoji map and the suffix a native app uses
    /// for an asset name / i18n lookup. Mirrors [`fauna_feed::SourceKind::id`]
    /// in spirit.
    pub fn id(&self) -> &'static str {
        match self {
            SourceGlyph::Fox => "fox",
            SourceGlyph::Envelope => "envelope",
            SourceGlyph::Butterfly => "butterfly",
            SourceGlyph::Bolt => "bolt",
            SourceGlyph::Globe => "globe",
            SourceGlyph::Unknown => "unknown",
            SourceGlyph::Archive => "archive",
            SourceGlyph::Bridge => "bridge",
        }
    }

    /// Parse a stable lowercase [`id`](SourceGlyph::id) back into its concept.
    ///
    /// Anything unrecognised — including a newer nest's glyph id this build has
    /// no variant for — reads as [`SourceGlyph::Unknown`], which is exactly the
    /// concept "a source outside the known set" already means. That keeps an
    /// older app rendering a sane badge against a newer nest rather than
    /// failing the row (`version-compatibility.md`: a client may be older than
    /// the nest it talks to).
    pub fn from_id(id: &str) -> Self {
        match id {
            "fox" => SourceGlyph::Fox,
            "envelope" => SourceGlyph::Envelope,
            "butterfly" => SourceGlyph::Butterfly,
            "bolt" => SourceGlyph::Bolt,
            "globe" => SourceGlyph::Globe,
            "archive" => SourceGlyph::Archive,
            "bridge" => SourceGlyph::Bridge,
            _ => SourceGlyph::Unknown,
        }
    }

    /// The emoji **every** app paints for this concept — the one
    /// `SourceGlyph → emoji` map, owned here instead of re-decided per app.
    ///
    /// All seven apps landed on emoji (`render-model.md` § Deltas → D5: neither
    /// SF Symbols nor Segoe Fluent has a fox or a butterfly, and mixing one
    /// emoji beside five native symbols in a single rail column reads
    /// inconsistently), so the "per-app native asset map" that D5 left behind
    /// was seven copies of one table — and they had already begun to disagree
    /// (see the envelope below). A future move to bundled brand art stays a
    /// deliberate change to this one function.
    ///
    /// **The envelope carries VS16** (`U+2709 U+FE0F`). `U+2709` is
    /// `Emoji_Presentation=No`, so the bare codepoint defaults to *text*
    /// presentation — monochrome, and single-width in a terminal cell grid
    /// beside the other five glyphs' double width. The selector is what makes
    /// it render as the color, full-size emoji the other five are by default.
    /// [`crate::notification_glyph::NotificationGlyph::emoji`] applies the same
    /// rule to `↩️` / `❤️`; apple and windows had both already fixed their own
    /// copies this way while linux / tui / web / android still shipped the bare
    /// codepoint.
    pub fn emoji(&self) -> &'static str {
        match self {
            SourceGlyph::Fox => "🦊",
            SourceGlyph::Envelope => "✉️",
            SourceGlyph::Butterfly => "🦋",
            SourceGlyph::Bolt => "⚡",
            SourceGlyph::Globe => "🌐",
            SourceGlyph::Unknown => "📡",
            SourceGlyph::Archive => "📦",
            SourceGlyph::Bridge => "🌉",
        }
    }
}

/// The emoji for a glyph **id** — the entry point for a surface that reads the
/// lowercase serde string off a snapshot rather than the enum (web off
/// `thread.glyph` / `badge.glyph`). Mirrors
/// [`crate::notification_glyph::notification_type_emoji`].
pub fn source_glyph_emoji(id: &str) -> &'static str {
    SourceGlyph::from_id(id).emoji()
}

/// Who a bridge is, as a thread row or a feed badge paints it — the identity a
/// bridge principal declares in its manifest's `bridge` block
/// (`architecture/third-party.md` § The manifest → *The `bridge` block*),
/// reduced to the three fields an app renders. Carried on
/// `fauna_conversations::ThreadSummary::bridge` / `ThreadDetail::bridge` for a
/// `Rail::Bridged` thread (`ui/conversations.md` § Where logic lives → *The
/// `Bridged` adapter*, ruling 2 (a)) and handed to
/// `fauna_feed::classify_sources` as the roster a bridged post's source token
/// resolves against (`ui/feed.md` § Implementation status today) — one type for
/// both, so the conversation row and the feed badge cannot name a bridge
/// differently.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BridgeIdentitySnapshot {
    /// The manifest's `bridge.id` — a one-label lowercase name, unique per
    /// account roster; the source token a bridged post carries.
    pub id: String,
    /// The bridge's declared display label (`Matrix`, …).
    pub label: String,
    /// The declared glyph — one member of the fixed set; never an image.
    pub glyph: SourceGlyph,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_lowercase() {
        assert_eq!(SourceGlyph::Fox.id(), "fox");
        assert_eq!(SourceGlyph::Envelope.id(), "envelope");
        assert_eq!(SourceGlyph::Butterfly.id(), "butterfly");
        assert_eq!(SourceGlyph::Bolt.id(), "bolt");
        assert_eq!(SourceGlyph::Globe.id(), "globe");
        assert_eq!(SourceGlyph::Unknown.id(), "unknown");
        assert_eq!(SourceGlyph::Archive.id(), "archive");
        assert_eq!(SourceGlyph::Bridge.id(), "bridge");
    }

    #[test]
    fn serde_form_matches_id() {
        // Web reads this string off the serialized snapshot / badge; it must
        // equal `id()` so one lowercase emoji map serves both surfaces.
        for g in [
            SourceGlyph::Fox,
            SourceGlyph::Envelope,
            SourceGlyph::Butterfly,
            SourceGlyph::Bolt,
            SourceGlyph::Globe,
            SourceGlyph::Unknown,
            SourceGlyph::Archive,
            SourceGlyph::Bridge,
        ] {
            let json = serde_json::to_string(&g).unwrap();
            assert_eq!(json, format!("\"{}\"", g.id()));
        }
    }

    #[test]
    fn every_concept_has_its_canonical_emoji() {
        // The one map. Exhaustive on purpose: a new SourceGlyph variant breaks
        // `emoji()`'s match at compile time, so it must consciously pick one.
        assert_eq!(SourceGlyph::Fox.emoji(), "🦊");
        assert_eq!(SourceGlyph::Envelope.emoji(), "✉️");
        assert_eq!(SourceGlyph::Butterfly.emoji(), "🦋");
        assert_eq!(SourceGlyph::Bolt.emoji(), "⚡");
        assert_eq!(SourceGlyph::Globe.emoji(), "🌐");
        assert_eq!(SourceGlyph::Unknown.emoji(), "📡");
        assert_eq!(SourceGlyph::Archive.emoji(), "📦");
        assert_eq!(SourceGlyph::Bridge.emoji(), "🌉");
    }

    #[test]
    fn new_concepts_are_appended_so_existing_ffi_discriminants_are_stable() {
        // Each concept after `Unknown` is appended on purpose (`Archive`
        // 2026-09-06, `Bridge` 2026-10-02): UniFFI numbers the variants in
        // declaration order and the Go binding mirrors them, so a new concept
        // goes at the END. `ALL` mirrors the declaration.
        assert_eq!(
            &SourceGlyph::ALL[5..],
            &[
                SourceGlyph::Unknown,
                SourceGlyph::Archive,
                SourceGlyph::Bridge
            ]
        );
        assert_eq!(SourceGlyph::ALL.last(), Some(&SourceGlyph::Bridge));
        assert_eq!(SourceGlyph::ALL.len(), 8);
        assert_eq!(SourceGlyph::from_id("archive"), SourceGlyph::Archive);
        assert_eq!(SourceGlyph::from_id("bridge"), SourceGlyph::Bridge);
    }

    #[test]
    fn the_envelope_carries_the_emoji_presentation_selector() {
        // The drift this map was lifted to end: linux / tui / web / android all
        // shipped the BARE U+2709, which is `Emoji_Presentation=No` and so
        // renders monochrome text-presentation (single-width in a terminal
        // grid) beside five double-width color glyphs, while apple and windows
        // had each already fixed their own copy. Pin the selector so the bare
        // codepoint cannot come back.
        assert_eq!(
            SourceGlyph::Envelope.emoji().chars().collect::<Vec<_>>(),
            vec!['\u{2709}', '\u{FE0F}'],
        );
    }

    #[test]
    fn every_glyph_is_a_single_scalar_or_a_scalar_plus_vs16() {
        // Structural guard on the whole map: a glyph is one codepoint, or one
        // codepoint plus VS16 — never an accidental multi-glyph string, and
        // never a stray selector in the wrong position.
        for g in SourceGlyph::ALL {
            let chars: Vec<char> = g.emoji().chars().collect();
            match chars.as_slice() {
                [_] => {}
                [_, '\u{FE0F}'] => {}
                other => panic!("{} has a malformed glyph: {other:?}", g.id()),
            }
        }
    }

    #[test]
    fn ids_round_trip_and_unknown_ids_fall_back() {
        for g in SourceGlyph::ALL {
            assert_eq!(
                SourceGlyph::from_id(g.id()),
                *g,
                "round trip for {}",
                g.id()
            );
            assert_eq!(source_glyph_emoji(g.id()), g.emoji());
        }
        // A newer nest's glyph id an older app has no variant for still renders
        // the "outside the known set" badge instead of failing the row.
        assert_eq!(SourceGlyph::from_id("elephant"), SourceGlyph::Unknown);
        assert_eq!(source_glyph_emoji(""), SourceGlyph::Unknown.emoji());
    }
}
