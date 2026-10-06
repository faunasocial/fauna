//! `device-p2p-participation-toggle`'s per-row paint and the rule that
//! decides which roster row is THIS device's (`docs/goal/behavior/p2p.md`
//! § Per-device participation → *Which row is this device's*).
//!
//! One rule, two readers: [`crate::DevicesMachine::set_p2p_participation`]
//! decides the gesture's arm with it, and [`crate::DevicesMachine::snapshot`]
//! paints every row's [`P2pParticipationPaint`] with it — so the checkbox an
//! app draws and the arm its click takes can never disagree, and no app
//! re-derives own-ness from its own device id.

use serde::{Deserialize, Serialize};

use fauna_core::localized::LocalizedText;

/// `device-p2p-participation-toggle`'s paint for one roster row, computed by
/// the machine and published on [`crate::DeviceSummary::p2p_participation_paint`].
/// An app draws a checkbox that is `checked`, labelled `label`, enabled
/// exactly when `actionable`, and whose click sends
/// `set_p2p_participation(index, !checked)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct P2pParticipationPaint {
    /// The row is THIS device's own (the gesture's own arm): the switch is
    /// the device's own, both directions. `false` on every row of an app
    /// with no participation door (web — no runtime, no own row).
    pub own: bool,
    /// On the own row: the device-local switch the door read, else the
    /// row's report, else the default (on). On a sibling's row: its last
    /// report, unreported reading as the default (on).
    pub checked: bool,
    /// `devices.p2p_participation_own` on the own row; on a sibling's,
    /// `…_off_requested` while an off request is pending, `…_unreported`
    /// when it never reported, else `devices.p2p_participation`.
    pub label: LocalizedText,
    /// The own row: always. A sibling's: only while it may still be on and
    /// no off request is pending — enabling is local consent on that
    /// device, so the only thing ever sendable to a sibling is `off`.
    pub actionable: bool,
}

/// What the participation door answered to "which row did this device
/// enroll on" — the first step of the own-row rule, read once per refresh
/// (and afresh at every gesture).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum OwnRowAnswer {
    /// No door is wired (web): there is no own row at all.
    #[default]
    NoDoor,
    /// The door named the enrolled row (hex device id).
    Enrolled(String),
    /// A door is wired but named no row — not enrolled yet, or no runtime.
    /// The fleet id, then the app's this-device hint, decide.
    Unnamed,
}

impl OwnRowAnswer {
    pub(crate) fn from_door(read: Option<Result<Option<String>, String>>) -> Self {
        match read {
            None => Self::NoDoor,
            Some(Ok(Some(row))) => Self::Enrolled(row),
            Some(_) => Self::Unnamed,
        }
    }
}

/// Whether the roster row `device_id` (with its granted `principal`) is THIS
/// device's own: the door's enrolled row, else the row whose principal is
/// this device's own fleet id, else the app's `set_this_device_row` hint.
/// Never own without a door — the own arm writes through it.
pub(crate) fn is_own_row(
    answer: &OwnRowAnswer,
    own_fleet_id: Option<&str>,
    this_device_row: Option<&str>,
    device_id: &str,
    principal: Option<&str>,
) -> bool {
    match answer {
        OwnRowAnswer::NoDoor => false,
        OwnRowAnswer::Enrolled(row) => row == device_id,
        OwnRowAnswer::Unnamed => match own_fleet_id {
            Some(fleet_id) => principal == Some(fleet_id),
            None => this_device_row == Some(device_id),
        },
    }
}

/// The paint for one row, given whether it is this device's own
/// ([`is_own_row`]), the door's read of the device-local switch, and the
/// row's own report and pending off request.
pub fn p2p_participation_paint(
    own: bool,
    own_local: Option<bool>,
    reported: Option<bool>,
    off_requested: bool,
) -> P2pParticipationPaint {
    let checked = if own {
        own_local.or(reported).unwrap_or(true)
    } else {
        reported.unwrap_or(true)
    };
    let label = if own {
        "devices.p2p_participation_own"
    } else if off_requested {
        "devices.p2p_participation_off_requested"
    } else if reported.is_none() {
        "devices.p2p_participation_unreported"
    } else {
        "devices.p2p_participation"
    };
    P2pParticipationPaint {
        own,
        checked,
        label: LocalizedText::key(label),
        actionable: own || (checked && !off_requested),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This device's own row paints the DEVICE-LOCAL switch — the authority —
    /// over a stale report, and is actionable in both directions.
    #[test]
    fn the_own_row_paints_the_local_participation_and_is_always_actionable() {
        let paint = p2p_participation_paint(true, Some(false), Some(true), false);
        assert!(
            !paint.checked,
            "the device-local off wins over the stale report"
        );
        assert!(paint.actionable, "the own switch turns back on too");
        assert!(paint.own);
        assert_eq!(paint.label.key, "devices.p2p_participation_own");

        let no_runtime = p2p_participation_paint(true, None, Some(false), false);
        assert!(!no_runtime.checked, "no local read: the report stands in");
        let nothing = p2p_participation_paint(true, None, None, false);
        assert!(nothing.checked, "nothing known: the default, on");
    }

    /// A sibling's row paints its last REPORT and can only be turned off:
    /// unreported reads as the default (on) and says so; a reported-off
    /// sibling and one with a pending off request are inert.
    #[test]
    fn a_sibling_row_paints_its_report_and_only_turns_off() {
        let never = p2p_participation_paint(false, Some(false), None, false);
        assert!(
            never.checked,
            "unreported paints the default; the local switch is not its"
        );
        assert_eq!(never.label.key, "devices.p2p_participation_unreported");
        assert!(never.actionable);
        assert!(!never.own);

        let on = p2p_participation_paint(false, None, Some(true), false);
        assert!(on.checked);
        assert_eq!(on.label.key, "devices.p2p_participation");
        assert!(on.actionable);

        let off = p2p_participation_paint(false, None, Some(false), false);
        assert!(!off.checked);
        assert!(!off.actionable, "enabling is local consent on that device");

        let asked = p2p_participation_paint(false, None, Some(true), true);
        assert_eq!(asked.label.key, "devices.p2p_participation_off_requested");
        assert!(!asked.actionable, "already asked: nothing more to send");
    }

    /// The three-step own-row rule, and its fourth answer: no door, no own
    /// row — whatever the fleet id or the app's hint say.
    #[test]
    fn the_own_row_rule_prefers_the_door_then_the_fleet_id_then_the_hint() {
        let enrolled = OwnRowAnswer::Enrolled("bb".into());
        assert!(is_own_row(&enrolled, Some("pa"), Some("aa"), "bb", None));
        assert!(!is_own_row(
            &enrolled,
            Some("pa"),
            Some("aa"),
            "aa",
            Some("pa")
        ));

        let unnamed = OwnRowAnswer::Unnamed;
        assert!(is_own_row(
            &unnamed,
            Some("pa"),
            Some("bb"),
            "aa",
            Some("pa")
        ));
        assert!(
            !is_own_row(&unnamed, Some("pa"), Some("bb"), "bb", None),
            "a known fleet id outranks the hint, even when no row matches it"
        );
        assert!(is_own_row(&unnamed, None, Some("bb"), "bb", None));
        assert!(!is_own_row(&unnamed, None, None, "bb", None));

        assert!(
            !is_own_row(
                &OwnRowAnswer::NoDoor,
                Some("pa"),
                Some("aa"),
                "aa",
                Some("pa")
            ),
            "no door (web): no own row"
        );
    }

    #[test]
    fn the_door_read_maps_onto_the_rule() {
        assert_eq!(OwnRowAnswer::from_door(None), OwnRowAnswer::NoDoor);
        assert_eq!(
            OwnRowAnswer::from_door(Some(Ok(Some("aa".into())))),
            OwnRowAnswer::Enrolled("aa".into())
        );
        assert_eq!(
            OwnRowAnswer::from_door(Some(Ok(None))),
            OwnRowAnswer::Unnamed
        );
        assert_eq!(
            OwnRowAnswer::from_door(Some(Err("no runtime".into()))),
            OwnRowAnswer::Unnamed
        );
    }
}
