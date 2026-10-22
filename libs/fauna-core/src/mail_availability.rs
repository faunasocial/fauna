//! Whether a box can run mail at all — the deployment's content-scanner
//! declaration and the two predicates every reader decides from
//! (`docs/goal/behavior/mail-bridge-lifecycle.md` § Default-off on first
//! claim → *A box with no content scanners cannot enable mail*).
//!
//! The MTA's scan gate is default-on and fail-closed, and the nest image ships
//! no scanner of its own, so a box whose compose wires no clamd/rspamd cannot
//! accept inbound mail. The entrypoint declares that on the topology hatch
//! (`content_scanners = "wired" | "absent"`); the nest reads it once at boot
//! and reports it on `fauna.setup.status`. A nest with no hatch at all (outside
//! the image) reads [`ContentScanners::Unknown`] and keeps today's behaviour.
//!
//! One home: the nest's supervisor gate and `set_mail_enabled` refusal, and the
//! client machines' `mail_unavailable_here` snapshot fields, all call these —
//! none re-derives them.

use serde::{Deserialize, Serialize};

use crate::nat_mode::NodeMode;

/// The deployment's content-scanner declaration. Serde uses the wire strings
/// and is open like [`ContentScanners::from_wire`]: an unrecognised value
/// decodes as `Unknown`, and `Default` is `Unknown` (a client record that has
/// not heard from the box yet).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ContentScanners {
    /// Both scanner addresses are wired — the MTA's scan gate can run.
    Wired,
    /// The artifact declared no scanners — inbound mail cannot be accepted.
    Absent,
    /// Nothing declared anything (no hatch, an unreadable one, or a wire
    /// value this build does not know) — today's behaviour stands.
    #[default]
    #[serde(other)]
    Unknown,
}

impl ContentScanners {
    /// The wire/hatch string (`"wired"` / `"absent"` / `"unknown"`).
    pub fn as_wire_str(self) -> &'static str {
        match self {
            ContentScanners::Wired => "wired",
            ContentScanners::Absent => "absent",
            ContentScanners::Unknown => "unknown",
        }
    }

    /// Parse the wire/hatch string. Open: anything unrecognised (including
    /// the empty string a reply without the field defaults to) is `Unknown`.
    pub fn from_wire(s: &str) -> ContentScanners {
        match s {
            "wired" => ContentScanners::Wired,
            "absent" => ContentScanners::Absent,
            _ => ContentScanners::Unknown,
        }
    }
}

/// Can the MTA run on this box? `node_mode != Private && scanners != Absent`.
/// The nest's `mta_should_run` is `mail_enabled && mta_can_run(..)`.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn mta_can_run(node_mode: NodeMode, scanners: ContentScanners) -> bool {
    node_mode != NodeMode::Private && scanners != ContentScanners::Absent
}

/// Would enabling mail need an MTA this box cannot run?
/// `node_mode != Private && scanners == Absent` — the condition under which
/// `fauna.bridges.set_mail_enabled(true)` is refused and the Mail pages read
/// unavailable. Never true on the private axis (the home box's enable brings
/// up the MDA alone).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn mail_unavailable_here(node_mode: NodeMode, scanners: ContentScanners) -> bool {
    node_mode != NodeMode::Private && scanners == ContentScanners::Absent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_str_round_trips_and_is_open() {
        for s in [
            ContentScanners::Wired,
            ContentScanners::Absent,
            ContentScanners::Unknown,
        ] {
            assert_eq!(ContentScanners::from_wire(s.as_wire_str()), s);
        }
        assert_eq!(ContentScanners::from_wire(""), ContentScanners::Unknown);
        assert_eq!(
            ContentScanners::from_wire("Wired"),
            ContentScanners::Unknown
        );
        assert_eq!(
            ContentScanners::from_wire("partial"),
            ContentScanners::Unknown
        );
    }

    #[test]
    fn the_whole_axis_by_scanners_table() {
        use ContentScanners::*;
        use NodeMode::*;
        // (node_mode, scanners, mta_can_run, mail_unavailable_here)
        let table = [
            (Public, Wired, true, false),
            (Public, Absent, false, true),
            (Public, Unknown, true, false),
            (Private, Wired, false, false),
            (Private, Absent, false, false),
            (Private, Unknown, false, false),
        ];
        for (mode, scanners, can_run, unavailable) in table {
            assert_eq!(
                mta_can_run(mode, scanners),
                can_run,
                "{mode:?} {scanners:?}"
            );
            assert_eq!(
                mail_unavailable_here(mode, scanners),
                unavailable,
                "{mode:?} {scanners:?}"
            );
        }
    }
}
