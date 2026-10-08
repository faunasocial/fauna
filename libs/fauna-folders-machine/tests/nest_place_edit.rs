//! The nest place's snapshot-policy editor, as the four on-screen buffers ⇄ the
//! wire policy (`docs/goal/behavior/backup-restore.md` § 8b).
//!
//! Behavior owner is § 8b; this is the shared *editor* half, lifted out of tui
//! (the lead app) at the six apps' trickle-down so each one paints controls
//! rather than re-deriving these rules. Two of them are traps that cost real
//! sessions, and both are unrepresentable once the app only formats and parses
//! through here:
//!
//! 1. **Blank is a VALUE, not a missing one.** Every knob is three-state, and
//!    the third state — unset, "nothing authoritative said" — is the resting
//!    value of every folder and where a knob RETURNS. An editor that cannot
//!    express it strands each folder it touches on a value its owner never
//!    chose, so a zero bound must render BLANK and a blank box must reach the
//!    nest as unset.
//! 2. **Retention inverts the omission rule.** `nest_place`'s knobs clear by
//!    omission, but `FolderUpdateRequest::retention_policy`'s `None` means
//!    *leave unchanged* — so clearing retention means sending the canonical
//!    binds-nothing policy, never `None`.

use fauna_folders_machine::{
    NEST_SNAPSHOTS_DEFAULT, NEST_SNAPSHOTS_OFF, NEST_SNAPSHOTS_ON, NestPlaceEdit,
    VersionRetentionEdit, nest_place_edit_from_row, nest_place_write, nest_snapshots_label,
    nest_snapshots_options, version_retention_edit_from_bounds, version_retention_write,
};

/// The select's option set is a catalog like every sibling picker
/// (`conflict_policy_options` / `member_access_options`),
/// so no app hand-rolls the value list or the value→label map.
#[test]
fn the_snapshots_select_offers_three_options_default_first() {
    let opts = nest_snapshots_options();
    let values: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
    assert_eq!(
        values,
        vec![
            NEST_SNAPSHOTS_DEFAULT,
            NEST_SNAPSHOTS_ON,
            NEST_SNAPSHOTS_OFF
        ],
        "the third state leads the list — it is where the knob rests and returns"
    );
    // Each option carries an i18n key the app resolves, never display text.
    for o in &opts {
        assert_eq!(
            o.label,
            nest_snapshots_label(&o.value),
            "the catalog's label and the standalone resolver must be one source"
        );
    }
}

#[test]
fn an_unknown_select_value_reads_as_the_default_state() {
    // Fail-safe in the only direction that is safe: an app that somehow shows a
    // value the catalog does not know must not be read as an explicit choice.
    assert_eq!(
        nest_snapshots_label("nonsense"),
        nest_snapshots_label(NEST_SNAPSHOTS_DEFAULT)
    );
    assert_eq!(
        nest_place_write(&edit("nonsense", "", "", "")).snapshots,
        None
    );
}

fn edit(
    snapshots: &str,
    quiet: &str,
    retention_snapshots: &str,
    retention_days: &str,
) -> NestPlaceEdit {
    NestPlaceEdit {
        snapshots: snapshots.to_string(),
        quiet_secs: quiet.to_string(),
        retention_snapshots: retention_snapshots.to_string(),
        retention_days: retention_days.to_string(),
    }
}

// ── Prefill: the row → the four buffers ─────────────────────────────────────

#[test]
fn a_folder_that_never_chose_prefills_entirely_blank() {
    let e = nest_place_edit_from_row(None, None, None);
    assert_eq!(e.snapshots, NEST_SNAPSHOTS_DEFAULT);
    assert_eq!(e.quiet_secs, "");
    assert_eq!(e.retention_snapshots, "");
    assert_eq!(e.retention_days, "");
}

#[test]
fn an_explicit_no_prefills_off_not_blank() {
    // The whole reason the select has three options: "don't keep snapshots" is
    // an owner's decision and must read back distinct from never having chosen.
    let e = nest_place_edit_from_row(Some(false), None, None);
    assert_eq!(e.snapshots, NEST_SNAPSHOTS_OFF);
    let e = nest_place_edit_from_row(Some(true), Some(120), None);
    assert_eq!(e.snapshots, NEST_SNAPSHOTS_ON);
    assert_eq!(e.quiet_secs, "120");
}

#[test]
fn a_zero_retention_bound_prefills_blank_because_zero_is_how_the_nest_spells_unset() {
    // `backup/retention.rs::parse_folder_retention` — "a zero in either field
    // means that bound is unset". Rendering the 0 back would round-trip an unset
    // bound into a rendered bound, and the two spellings would drift.
    let e = nest_place_edit_from_row(
        None,
        None,
        Some(r#"{"max_snapshots":0,"max_age_days":10}"#.to_string()),
    );
    assert_eq!(e.retention_snapshots, "");
    assert_eq!(e.retention_days, "10");
}

#[test]
fn an_unparseable_retention_policy_prefills_blank_rather_than_guessing() {
    // Same fail-safe direction the nest takes (`FolderRetention::Unparseable`
    // prunes nothing): show nothing rather than invent a bound.
    let e = nest_place_edit_from_row(None, None, Some("{not json".to_string()));
    assert_eq!(e.retention_snapshots, "");
    assert_eq!(e.retention_days, "");
}

// ── Save: the four buffers → the wire policy ────────────────────────────────

#[test]
fn the_save_sends_every_knob_the_user_set() {
    let w = nest_place_write(&edit(NEST_SNAPSHOTS_ON, "120", "5", "10"));
    assert_eq!(w.snapshots, Some(true));
    assert_eq!(w.quiet_secs, Some(120));
    let r = w.retention.expect("a set retention rides as a policy");
    assert!(r.replace(' ', "").contains(r#""max_snapshots":5"#), "{r}");
    assert!(r.replace(' ', "").contains(r#""max_age_days":10"#), "{r}");
}

#[test]
fn an_off_snapshots_knob_is_a_real_false_never_an_omission() {
    assert_eq!(
        nest_place_write(&edit(NEST_SNAPSHOTS_OFF, "", "", "")).snapshots,
        Some(false)
    );
}

#[test]
fn emptying_the_boxes_clears_the_policy_in_both_directions() {
    // The leg that proves the third state is reachable both ways. Without it a
    // user could set a knob and never take it back.
    let w = nest_place_write(&edit(NEST_SNAPSHOTS_DEFAULT, "", "", ""));
    assert_eq!(w.snapshots, None, "back to 'nothing authoritative said'");
    assert_eq!(w.quiet_secs, None);
    // ⚠ NOT `None` — retention's wire `None` means *leave unchanged*, so a
    // cleared retention must ride as the canonical binds-nothing policy.
    let r = w
        .retention
        .expect("clearing retention sends a binds-nothing policy, never None");
    assert!(r.replace(' ', "").contains(r#""max_snapshots":0"#), "{r}");
    assert!(r.replace(' ', "").contains(r#""max_age_days":0"#), "{r}");
}

#[test]
fn whitespace_and_junk_in_a_number_box_read_as_unset_not_as_zero_bound() {
    let w = nest_place_write(&edit(NEST_SNAPSHOTS_DEFAULT, "  ", "abc", " 7 "));
    assert_eq!(w.quiet_secs, None);
    let r = w.retention.expect("still a whole policy");
    assert!(r.replace(' ', "").contains(r#""max_snapshots":0"#), "{r}");
    assert!(r.replace(' ', "").contains(r#""max_age_days":7"#), "{r}");
}

#[test]
fn a_negative_quiet_period_is_refused_client_side_rather_than_sent() {
    // The nest refuses a negative `quiet_secs` outright
    // (`fauna.folders.bad_request`) and deliberately does not clamp it, because
    // clamping would turn "wait for quiet" into "cut on every tick" silently
    // (§ 8b). The editor must not walk the user into that refusal: a negative
    // box reads as unset, exactly like junk.
    assert_eq!(
        nest_place_write(&edit(NEST_SNAPSHOTS_DEFAULT, "-5", "", "")).quiet_secs,
        None
    );
}

#[test]
fn a_quiet_period_over_the_ceiling_is_refused_client_side_rather_than_sent() {
    // The nest refuses a `quiet_secs` above `NestPlacePolicy::MAX_QUIET_SECS`
    // the same way it refuses a negative one (§ 8b), so the editor reads it as
    // unset too. The ceiling itself is a real choice and rides as typed.
    let max = fauna_protocol::folders::NestPlacePolicy::MAX_QUIET_SECS;
    assert_eq!(
        nest_place_write(&edit(
            NEST_SNAPSHOTS_DEFAULT,
            &(max + 1).to_string(),
            "",
            ""
        ))
        .quiet_secs,
        None
    );
    assert_eq!(
        nest_place_write(&edit(NEST_SNAPSHOTS_DEFAULT, &max.to_string(), "", "")).quiet_secs,
        Some(max)
    );
}

// ── The round trip, which is what an app actually does ──────────────────────

#[test]
fn a_row_prefilled_and_saved_untouched_writes_back_what_it_read() {
    // An app renders the row, the user touches nothing, and saves. Nothing may
    // move — otherwise merely opening a folder rewrites its owner's policy.
    for row in [
        (None, None, None),
        (
            Some(true),
            Some(300),
            Some(r#"{"max_snapshots":5,"max_age_days":0}"#.to_string()),
        ),
        (
            Some(false),
            None,
            Some(r#"{"max_snapshots":0,"max_age_days":0}"#.to_string()),
        ),
    ] {
        let e = nest_place_edit_from_row(row.0, row.1, row.2.clone());
        let w = nest_place_write(&e);
        assert_eq!(w.snapshots, row.0, "snapshots moved for {row:?}");
        assert_eq!(w.quiet_secs, row.1, "quiet moved for {row:?}");
        // Retention compares by MEANING — absent, empty and the canonical
        // all-zero policy are one value to the nest, and the write always
        // spells it the canonical way.
        let before = row.2.as_deref().and_then(parse_bounds).unwrap_or((0, 0));
        let after = w
            .retention
            .as_deref()
            .and_then(parse_bounds)
            .unwrap_or((0, 0));
        assert_eq!(before, after, "retention moved for {row:?}");
    }
}

fn parse_bounds(raw: &str) -> Option<(u32, u32)> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    Some((
        v.get("max_snapshots")?.as_u64()? as u32,
        v.get("max_age_days")?.as_u64()? as u32,
    ))
}

// ── The fourth per-place knob: version retention (file-versions.md § Retention) ──

/// Blank⇄zero both ways: an unset bound prefills blank (never a rendered `0`
/// the owner appears to have chosen), and a blank — or junk, or negative — box
/// writes back `0`, that-bound-unset, never "keep zero versions".
#[test]
fn version_retention_blank_and_zero_are_one_spelling() {
    let e = version_retention_edit_from_bounds(0, 0);
    assert_eq!((e.count.as_str(), e.days.as_str()), ("", ""));
    let e = version_retention_edit_from_bounds(5, 0);
    assert_eq!((e.count.as_str(), e.days.as_str()), ("5", ""));

    for (count, days) in [("", ""), ("  ", "junk"), ("-3", "0")] {
        let w = version_retention_write(&VersionRetentionEdit {
            count: count.into(),
            days: days.into(),
        });
        assert_eq!(
            (w.max_versions_per_path, w.max_age_days),
            (0, 0),
            "({count:?}, {days:?}) must write the binds-nothing policy"
        );
    }
}

/// Prefill → save untouched writes back what it read — opening a folder's
/// editor must never rewrite its owner's version policy.
#[test]
fn version_retention_prefilled_and_saved_untouched_writes_back_what_it_read() {
    for bounds in [(0u32, 0u32), (5, 0), (0, 30), (7, 90)] {
        let e = version_retention_edit_from_bounds(bounds.0, bounds.1);
        let w = version_retention_write(&e);
        assert_eq!((w.max_versions_per_path, w.max_age_days), bounds);
    }
}
