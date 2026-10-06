//! The nest place's snapshot-policy **editor** — the four on-screen buffers ⇄
//! the wire policy.
//!
//! Behavior authority is `docs/goal/behavior/backup-restore.md` § 8b (what the
//! knobs mean, why each is three-state, and why `retention_policy` inverts the
//! omission rule); this module owns only the app-facing edit shape, so the seven
//! apps painting `folder-nest-snapshots-select` / `-quiet-input` /
//! `-retention-snapshots` / `-retention-days` / `-save-button` format and parse
//! through one implementation instead of seven.
//!
//! Lifted out of tui (the lead app) at the six apps' slice-e trickle-down. Two
//! rules are traps rather than details, and both become unrepresentable here:
//!
//! - **Blank is a VALUE.** Unset — "nothing authoritative said" — is the resting
//!   state of every folder and where a knob RETURNS when its owner picks the
//!   default. So a zero bound renders BLANK (zero is the nest's own spelling of
//!   unset, `backup/retention.rs::parse_folder_retention`) and a blank box
//!   reaches the nest as unset, never as `0`.
//! - **Retention inverts the omission rule.** `nest_place`'s two knobs clear by
//!   omission, but `FolderUpdateRequest::retention_policy`'s `None` means *leave
//!   unchanged* (it cannot be a nested `Option` — dag-cbor will not round-trip
//!   one). Clearing retention therefore sends the canonical binds-nothing
//!   policy, which the nest already reads as `FolderRetention::NotSet`.

use fauna_core::localized::LocalizedText;
use serde::{Deserialize, Serialize};

use crate::state::RetentionPolicy;

/// The `folder-nest-snapshots-select` wire values. The select round-trips a raw
/// value and paints a localized label beside it (the house value/display split
/// every sibling picker uses), so these are stable identifiers — never
/// user-visible text.
///
/// `default` is the honest third state, not a synonym for `off`: it is what the
/// knob returns to, and the nest reads it as *no owner preference*.
pub const NEST_SNAPSHOTS_DEFAULT: &str = "default";
/// The owner's explicit "keep snapshots".
pub const NEST_SNAPSHOTS_ON: &str = "on";
/// The owner's explicit "don't keep snapshots" — a real `false` at rest, which
/// must stay distinguishable from [`NEST_SNAPSHOTS_DEFAULT`].
pub const NEST_SNAPSHOTS_OFF: &str = "off";

/// A canonical `folder-nest-snapshots-select` option — the same value + i18n
/// label shape as [`ConflictPolicyOption`](crate::ConflictPolicyOption), so an app builds the
/// select from this catalog rather than hand-rolling the value list and a
/// value→label map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct NestSnapshotsOption {
    /// The wire value the select writes and the cross-app `select(id, value)`
    /// e2e contract drives — one of the three constants above.
    pub value: String,
    pub label: LocalizedText,
}

/// The canonical option order: the default state leads, because it is where
/// every folder rests and where a knob returns.
const NEST_SNAPSHOTS_ORDER: [&str; 3] = [
    NEST_SNAPSHOTS_DEFAULT,
    NEST_SNAPSHOTS_ON,
    NEST_SNAPSHOTS_OFF,
];

/// The three-state select's option catalog.
pub fn nest_snapshots_options() -> Vec<NestSnapshotsOption> {
    NEST_SNAPSHOTS_ORDER
        .into_iter()
        .map(|value| NestSnapshotsOption {
            value: value.to_string(),
            label: nest_snapshots_label(value),
        })
        .collect()
}

/// Resolve a select value to its label. An unrecognized value degrades to the
/// default state's label — fail-safe in the only safe direction, since reading
/// an unknown value as an explicit on/off would attribute a choice the owner
/// never made.
pub fn nest_snapshots_label(value: &str) -> LocalizedText {
    LocalizedText::key(match value {
        NEST_SNAPSHOTS_ON => "devices.nest_snapshots_on_label",
        NEST_SNAPSHOTS_OFF => "devices.nest_snapshots_off_label",
        _ => "devices.nest_snapshots_default_label",
    })
}

/// The nest-place editor's four on-screen buffers, exactly as an app holds them:
/// one select value plus three free-text boxes. Strings, not typed options,
/// because that is what a text box *is* — the parse is this module's job, and
/// centralizing it is the point.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct NestPlaceEdit {
    /// `folder-nest-snapshots-select` — one of the three constants above.
    pub snapshots: String,
    /// `folder-nest-quiet-input`. Blank = unset (the nest-wide cadence).
    pub quiet_secs: String,
    /// `folder-nest-retention-snapshots`. Blank = that bound unset; blank in
    /// BOTH retention boxes keeps everything, never "keep zero".
    pub retention_snapshots: String,
    /// `folder-nest-retention-days`, the sibling of the field above.
    pub retention_days: String,
}

/// The policy as `DevicesMachine::set_folder_nest_place` takes it — the **whole**
/// policy, because the nest applies it whole. (That method lives in
/// `fauna-devices-machine`, which depends on this crate, so the link is by name.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct NestPlaceWrite {
    /// `None` writes the knob back to unset — the omission-clears rule.
    pub snapshots: Option<bool>,
    /// `None` writes the knob back to unset.
    pub quiet_secs: Option<i64>,
    /// ⚠ The one knob that does NOT clear by omission: `None` here means *leave
    /// unchanged*, so [`nest_place_write`] never produces it — a cleared
    /// retention rides as the canonical binds-nothing policy instead.
    pub retention: Option<String>,
}

/// Seed the editor from a folder row (`FolderSummary::nest_snapshots` /
/// `nest_snapshot_quiet_secs` / `retention_policy`).
///
/// An unset knob prefills BLANK — never `"0"` — because blank is how the user
/// says "use the default", and a rendered `0` would read as a real bound the
/// owner chose.
pub fn nest_place_edit_from_row(
    nest_snapshots: Option<bool>,
    nest_snapshot_quiet_secs: Option<i64>,
    retention_policy: Option<String>,
) -> NestPlaceEdit {
    let retention = retention_policy.as_deref().and_then(parse_retention);
    NestPlaceEdit {
        snapshots: match nest_snapshots {
            Some(true) => NEST_SNAPSHOTS_ON,
            Some(false) => NEST_SNAPSHOTS_OFF,
            None => NEST_SNAPSHOTS_DEFAULT,
        }
        .to_string(),
        quiet_secs: nest_snapshot_quiet_secs
            .map(|s| s.to_string())
            .unwrap_or_default(),
        // A zero bound is the nest's own spelling of "this bound is unset", so
        // it renders blank too — otherwise a save would round-trip an unset
        // bound into a rendered one and the two spellings would drift.
        retention_snapshots: retention
            .filter(|r| r.max_snapshots > 0)
            .map(|r| r.max_snapshots.to_string())
            .unwrap_or_default(),
        retention_days: retention
            .filter(|r| r.max_age_days > 0)
            .map(|r| r.max_age_days.to_string())
            .unwrap_or_default(),
    }
}

/// Read the four buffers into the whole policy to send.
///
/// Every knob rides on every save, because the nest applies the policy whole: a
/// knob the user emptied must reach it as unset, which is what these `None`s
/// mean. Retention is the exception the struct documents.
pub fn nest_place_write(edit: &NestPlaceEdit) -> NestPlaceWrite {
    NestPlaceWrite {
        snapshots: match edit.snapshots.as_str() {
            NEST_SNAPSHOTS_ON => Some(true),
            NEST_SNAPSHOTS_OFF => Some(false),
            _ => None,
        },
        // A negative quiet period is refused by the nest outright
        // (`fauna.folders.bad_request`) and deliberately not clamped, so the
        // editor must not walk the user into that refusal: it reads as unset,
        // exactly like junk.
        quiet_secs: edit
            .quiet_secs
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|s| *s >= 0),
        retention: retention_from_inputs(&edit.retention_snapshots, &edit.retention_days),
    }
}

/// Parse the nest's opaque `retention_policy` JSON into the canonical two-field
/// shape. An unparseable value yields `None` — blank boxes rather than a guess,
/// the same fail-safe direction the nest takes (`FolderRetention::Unparseable`
/// prunes nothing).
pub fn parse_retention(raw: &str) -> Option<RetentionPolicy> {
    serde_json::from_str::<RetentionPolicy>(raw).ok()
}

// ── The FOURTH per-place knob: version retention (file-versions.md § Retention) ──
//
// `backup-restore.md` § 8b: the version knobs join the same per-place editor
// (`folder-version-retention-count` / `folder-version-retention-days`, IDs
// user-approved 2026-08-17) and ride the same ONE `fauna.folders.update` save —
// but as the SIBLING wire field `version_retention`, never folded into
// `retention_policy` (§ 8 forbids the silent re-map). Same blank⇄zero rules as
// the snapshot-retention boxes above, deliberately as a sibling struct pair
// rather than new fields on [`NestPlaceEdit`]/[`NestPlaceWrite`]: the save call
// takes the version half as an `Option`, so an app that has not yet painted the
// knobs passes `None` — the wire's *leave unchanged* — and can never clear a
// policy its user cannot see.

/// The two version-retention text boxes, exactly as an app holds them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct VersionRetentionEdit {
    /// `folder-version-retention-count` — max listable versions per path.
    /// Blank = that bound unset.
    pub count: String,
    /// `folder-version-retention-days` — max version age in days. Blank = that
    /// bound unset; blank in BOTH boxes keeps everything, never "keep zero".
    pub days: String,
}

/// The version-retention policy as the save call takes it — **whole**, because
/// the nest applies it whole (`fauna_protocol::folders::VersionRetention`
/// semantics: a `0` bound is unset; both zero = the binds-nothing policy the
/// nest rests as `NULL`).
///
/// ⚠ The `Option` sits at the *call*, not in here: `Some(both-zero)` **clears**
/// (the user emptied the boxes); `None` at the save call means *leave
/// unchanged* — the arm a lagging app rides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct VersionRetentionWrite {
    pub max_versions_per_path: u32,
    pub max_age_days: u32,
}

/// Seed the two version-retention boxes from a folder row's bounds
/// (`FolderSummary::version_retention`, flattened to its two numbers — an
/// absent policy is `(0, 0)`). A zero bound prefills BLANK, never `"0"`, for
/// the same reason as [`nest_place_edit_from_row`]'s retention boxes: zero is
/// the nest's own spelling of unset, and a rendered `0` would read as a bound
/// the owner chose.
pub fn version_retention_edit_from_bounds(
    max_versions_per_path: u32,
    max_age_days: u32,
) -> VersionRetentionEdit {
    VersionRetentionEdit {
        count: if max_versions_per_path > 0 {
            max_versions_per_path.to_string()
        } else {
            String::new()
        },
        days: if max_age_days > 0 {
            max_age_days.to_string()
        } else {
            String::new()
        },
    }
}

/// Read the two boxes into the whole policy to send. Blank (or junk, or a
/// negative) reads as `0` — that bound unset — so an emptied editor sends the
/// binds-nothing policy and the nest rests the column back to `NULL` (keep
/// everything), never "keep zero versions".
pub fn version_retention_write(edit: &VersionRetentionEdit) -> VersionRetentionWrite {
    VersionRetentionWrite {
        max_versions_per_path: edit.count.trim().parse().unwrap_or(0),
        max_age_days: edit.days.trim().parse().unwrap_or(0),
    }
}

/// Serialize the two retention boxes back to the nest's opaque JSON.
///
/// A blank box rides as `0`, the nest's own spelling of "that bound is unset" —
/// never as "keep zero snapshots", which would delete the user's history.
/// **Both blank ⇒ the canonical binds-nothing policy, NOT `None`**: this is the
/// one place the editor cannot use the omission-clears rule, because
/// `retention_policy`'s `None` means *leave unchanged*, so returning it would
/// let a user set retention and never take it back.
pub fn retention_from_inputs(snapshots: &str, days: &str) -> Option<String> {
    serde_json::to_string(&RetentionPolicy {
        max_snapshots: snapshots.trim().parse().unwrap_or(0),
        max_age_days: days.trim().parse().unwrap_or(0),
    })
    .ok()
}
