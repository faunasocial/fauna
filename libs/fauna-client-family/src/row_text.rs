//! Resolved Family-page text, shared between tui and linux (the two native
//! shells that render a `FamilyContentNotice` readout directly in Rust). Each
//! function here used to be one function hand-duplicated in both apps: build
//! the per-notice line via `fauna_core::format::content_notice_line`, then
//! resolve + join. The resolve target already took an app-supplied `lookup`
//! closure, and both apps passed the exact same one (`fauna_i18n::strings::lookup`
//! is a single shared function, not per-app-generated), so nothing about the
//! join was actually app-specific. Mirrors `fauna_client_backup::row_text`.

use fauna_protocol::family::{FamilyAgeBandInfo, FamilyContentNotice};

/// The guardian's per-ward `family-ward-content-notices` readout — one
/// "{category}: {count}" line per Guardian Notify count (category + count
/// only, never content).
pub fn ward_content_notices_text(notices: &[FamilyContentNotice]) -> String {
    notices
        .iter()
        .map(|n| {
            let line = fauna_core::format::content_notice_line(&n.category, n.count);
            format!(
                "{}: {}",
                line.label.resolve(fauna_i18n::strings::lookup),
                line.value.resolve(fauna_i18n::strings::lookup)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The guardian's per-ward `family-ward-age-band` readout ("Age band: 13–15 ·
/// set by guardian"), or `None` when the ward's band is one this client
/// cannot name — the row is then **absent, never placeholdered**
/// (`family-safety.md` § App surface → *Age-band surfaces*). Resolved nested:
/// the band and provenance labels are keys.
pub fn ward_age_band_text(info: &FamilyAgeBandInfo) -> Option<String> {
    fauna_protocol::age::age_band_line(&info.band, &info.provenance, false)
        .map(|t| t.resolve_nested(fauna_i18n::strings::lookup))
}

/// The ward's own `family-age-band-summary` ("Your age band: 13–15 · set by
/// guardian, set at admission"), on the same terms as [`ward_age_band_text`].
pub fn own_age_band_text(info: &FamilyAgeBandInfo) -> Option<String> {
    fauna_protocol::age::age_band_line(&info.band, &info.provenance, true)
        .map(|t| t.resolve_nested(fauna_i18n::strings::lookup))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_band_readouts_resolve_nested_and_vanish_for_an_unnamed_band() {
        let info = FamilyAgeBandInfo {
            band: "13-15".into(),
            provenance: "guardian-asserted".into(),
            ..Default::default()
        };
        let ward = ward_age_band_text(&info).expect("named band");
        assert!(ward.contains("13–15"), "{ward}");
        assert!(ward.contains("set by guardian"), "{ward}");
        assert!(
            !ward.contains("family.age_band"),
            "keys must resolve: {ward}"
        );
        let own = own_age_band_text(&info).expect("named band");
        assert!(own.starts_with("Your age band"), "{own}");
        assert!(own.contains("13–15"), "{own}");
        let unnamed = FamilyAgeBandInfo {
            band: "teen".into(),
            provenance: "guardian-asserted".into(),
            ..Default::default()
        };
        assert_eq!(ward_age_band_text(&unnamed), None);
        assert_eq!(own_age_band_text(&unnamed), None);
    }

    #[test]
    fn joins_one_line_per_notice_in_order() {
        let notices = vec![
            FamilyContentNotice {
                category: "spam".into(),
                count: 3,
                ..Default::default()
            },
            FamilyContentNotice {
                category: "violence".into(),
                count: 1,
                ..Default::default()
            },
        ];
        let text = ward_content_notices_text(&notices);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains('3'));
        assert!(lines[1].contains('1'));
    }

    #[test]
    fn empty_notices_is_an_empty_string() {
        assert_eq!(ward_content_notices_text(&[]), "");
    }
}
