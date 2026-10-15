//! Resolved Family-page text, shared between tui and linux (the two native
//! shells that render a `FamilyContentNotice` readout directly in Rust). Each
//! function here used to be one function hand-duplicated in both apps: build
//! the per-notice line via `fauna_core::format::content_notice_line`, then
//! resolve + join. The resolve target already took an app-supplied `lookup`
//! closure, and both apps passed the exact same one (`fauna_i18n::strings::lookup`
//! is a single shared function, not per-app-generated), so nothing about the
//! join was actually app-specific. Mirrors `fauna_client_backup::row_text`.

use fauna_protocol::family::{FamilyAgeBandInfo, FamilyApprovalEntry, FamilyContentNotice};

/// The guardian's `family-approval-item` row text: who the item is from
/// (`FamilyApprovalEntry::display_text`, or the localized no-sender label rather
/// than a blank row beside live buttons) and, for a held mail, when it arrived
/// ("stranger@example.com · 14:03") — envelope metadata only, never content
/// (`family-safety.md` § Guardian policy — the enforcement split).
///
/// The time is the shared conversation bucket
/// (`FamilyApprovalEntry::timestamp_display`); only the local offset and the
/// older-than-a-week date (`absolute_date`, epoch ms in) are the shell's, as
/// for a message timestamp.
pub fn approval_item_text(
    entry: &FamilyApprovalEntry,
    now_ms: i64,
    utc_offset_seconds: i32,
    absolute_date: impl Fn(i64) -> String,
) -> String {
    let who = entry.display_text();
    let who = who
        .as_deref()
        .unwrap_or(fauna_i18n::strings::family::APPROVAL_NO_SENDER);
    let when = entry
        .timestamp_display(now_ms, utc_offset_seconds)
        .and_then(|d| {
            d.clock
                .or_else(|| d.localized.map(|l| l.resolve(fauna_i18n::strings::lookup)))
                .or_else(|| d.absolute_epoch_ms.map(&absolute_date))
        });
    match when {
        Some(when) => format!("{who} · {when}"),
        None => who.to_owned(),
    }
}

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
    fn a_held_mail_row_shows_its_sender_and_time_other_rows_are_unchanged() {
        let held_at = 1_700_000_000; // 22:13:20 UTC
        let now_ms = held_at * 1_000 + 60_000;
        let never = |_: i64| -> String { panic!("today needs no date") };
        let hold = FamilyApprovalEntry {
            kind: "mail_hold".into(),
            peer_address: "stranger@example.com".into(),
            created_at: held_at,
            ..Default::default()
        };
        assert_eq!(
            approval_item_text(&hold, now_ms, 0, never),
            "stranger@example.com · 22:13"
        );
        let null_path = FamilyApprovalEntry {
            peer_address: String::new(),
            ..hold.clone()
        };
        assert_eq!(
            approval_item_text(&null_path, now_ms, 0, never),
            format!(
                "{} · 22:13",
                fauna_i18n::strings::family::APPROVAL_NO_SENDER
            )
        );
        let old = approval_item_text(&hold, now_ms + 30 * 86_400_000, 0, |ms| {
            format!("date:{ms}")
        });
        assert_eq!(
            old,
            format!("stranger@example.com · date:{}", held_at * 1_000)
        );
        let knock = FamilyApprovalEntry {
            kind: "contact".into(),
            summary: "hi from bob".into(),
            created_at: held_at,
            ..Default::default()
        };
        assert_eq!(approval_item_text(&knock, now_ms, 0, never), "hi from bob");
    }

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
