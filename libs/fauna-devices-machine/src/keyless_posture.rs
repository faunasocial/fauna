//! Whether a roster row carries the **keyless-posture marker**
//! (`device-keyless-posture-badge`; `docs/goal/ui/devices.md` § Custody facet,
//! piece 1).
//!
//! Posture is bundle key reach — DERIVED, never stored or asked: a device is
//! keyless when its enrolled principal holds no generation wrap at the
//! observer's resolved tip. The keyed set is
//! `AccountStoreHandle::keyed_principals`; the row's principal is
//! [`crate::DeviceSummary::principal`]. This is the one place the join and its
//! fail-safes live, so no app re-derives them in its own language.

use std::collections::BTreeSet;

/// `true` when the row's enrolled `principal` (lower-case hex) is absent from
/// `keyed` — the set of principals keyed at the resolved tip.
///
/// Three fail-safes, each answering `false` (no badge) rather than a guess,
/// because a "holds no keys" marker must never rest on an unknown:
///
/// * `keyed` is `None` — no tip resolves for this observer (or no store is
///   assembled yet);
/// * `principal` is `None` — a row not yet carrying a principal (the
///   `sync_devices` row before the enrollment ceremony's UPDATE sets
///   `auth_device_key`);
/// * `principal` does not decode as 32 bytes of hex.
pub fn keyless_posture(keyed: Option<&BTreeSet<[u8; 32]>>, principal: Option<&str>) -> bool {
    let (Some(keyed), Some(hex)) = (keyed, principal) else {
        return false;
    };
    match fauna_core::hex32::decode(hex) {
        Ok(principal) => !keyed.contains(&principal),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: [u8; 32]) -> String {
        fauna_core::hex32::encode(&b)
    }

    /// The marker's one positive case: a known principal the tip does not key.
    #[test]
    fn an_unkeyed_principal_is_keyless() {
        let keyed: BTreeSet<[u8; 32]> = [[1u8; 32]].into_iter().collect();
        assert!(keyless_posture(Some(&keyed), Some(&hex([2u8; 32]))));
    }

    #[test]
    fn a_keyed_principal_is_not_keyless() {
        let keyed: BTreeSet<[u8; 32]> = [[1u8; 32]].into_iter().collect();
        assert!(!keyless_posture(Some(&keyed), Some(&hex([1u8; 32]))));
    }

    /// No resolved tip: even an empty-looking world must not paint "holds no
    /// keys" on every row.
    #[test]
    fn no_resolved_tip_marks_nothing() {
        assert!(!keyless_posture(None, Some(&hex([2u8; 32]))));
    }

    #[test]
    fn a_row_without_a_principal_marks_nothing() {
        let keyed: BTreeSet<[u8; 32]> = BTreeSet::new();
        assert!(!keyless_posture(Some(&keyed), None));
    }

    #[test]
    fn an_undecodable_principal_marks_nothing() {
        let keyed: BTreeSet<[u8; 32]> = BTreeSet::new();
        assert!(!keyless_posture(Some(&keyed), Some("not-hex")));
    }
}
