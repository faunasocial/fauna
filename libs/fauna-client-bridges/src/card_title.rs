//! A bridge card's title — the name every app paints at the head of a Bridges
//! page network row, with a consented bridge app's declared glyph in front of
//! it (`docs/features/bridges.md` § Coverage contract, outcome 11: "a bridge app
//! you connected appears on the Bridges page as a network row with its own
//! glyph").
//!
//! The nest sets [`BridgeStatus::glyph`] only on a consented third-party bridge's
//! row (`architecture/third-party.md` § The manifest → *The `bridge` block*); a
//! first-party provider's row carries none, and its card stays the bare name.
//! The glyph is one id of the fixed [`SourceGlyph`] set — never an image — and
//! resolves through the one `SourceGlyph → emoji` map, so the Bridges row and
//! the feed badge / conversations rail of the same bridge cannot paint it
//! differently. An id this build does not know reads as
//! [`SourceGlyph::Unknown`] (a newer nest; `version-compatibility.md`).

use fauna_core::source_glyph::SourceGlyph;
use fauna_protocol::bridges_ui::BridgeStatus;

/// The declared glyph of a bridge row, or `None` for a first-party provider's
/// row (which declares none).
pub fn bridge_card_glyph(bridge: &BridgeStatus) -> Option<SourceGlyph> {
    bridge.glyph.as_deref().map(SourceGlyph::from_id)
}

/// The card's title text: `"<emoji> <name>"` when the row declares a glyph,
/// the bare name otherwise.
pub fn bridge_card_title(bridge: &BridgeStatus) -> String {
    bridge_card_title_of(&bridge.name, bridge.glyph.as_deref())
}

/// [`bridge_card_title`] over the two fields it reads — the row's `name` and
/// its declared `glyph` id — for the UniFFI and wasm faces, whose shells keep
/// their own decoded bridge rows and should not rebuild (or copy across the
/// boundary) a whole [`BridgeStatus`] to compute a title.
pub fn bridge_card_title_of(name: &str, glyph: Option<&str>) -> String {
    match glyph.map(SourceGlyph::from_id) {
        Some(glyph) => format!("{} {}", glyph.emoji(), name),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(glyph: Option<&str>) -> BridgeStatus {
        BridgeStatus {
            id: "matrix".to_string(),
            name: "Matrix".to_string(),
            available: true,
            linked: true,
            identity: None,
            mode: None,
            settings: Vec::new(),
            supports_follows: false,
            supports_follow_requests: false,
            link_modes: None,
            glyph: glyph.map(str::to_string),
            error: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn a_declared_glyph_heads_the_title() {
        assert_eq!(
            bridge_card_glyph(&row(Some("globe"))),
            Some(SourceGlyph::Globe)
        );
        assert_eq!(bridge_card_title(&row(Some("globe"))), "🌐 Matrix");
        assert_eq!(bridge_card_title(&row(Some("bridge"))), "🌉 Matrix");
    }

    #[test]
    fn a_first_party_row_is_the_bare_name() {
        assert_eq!(bridge_card_glyph(&row(None)), None);
        assert_eq!(bridge_card_title(&row(None)), "Matrix");
    }

    #[test]
    fn an_unknown_glyph_id_reads_as_unknown() {
        assert_eq!(
            bridge_card_glyph(&row(Some("dragon"))),
            Some(SourceGlyph::Unknown)
        );
        assert_eq!(bridge_card_title(&row(Some("dragon"))), "📡 Matrix");
    }

    #[test]
    fn the_field_form_matches_the_row_form() {
        assert_eq!(bridge_card_title_of("Matrix", Some("globe")), "🌐 Matrix");
        assert_eq!(bridge_card_title_of("Matrix", None), "Matrix");
    }
}
