//! Snapshot for the wizard's `claim_code` page.
//!
//! Mirrors the shape of `invite_request.rs`: a small enum for the
//! state-machine state, a `LocalizedText` for the i18n status row, and a
//! `submit_enabled` predicate. The page is reached only when handle-check
//! returns `HandleCheckOutcome::UnregisteredUnclaimedNest` — the user
//! atomically becomes the admin via `POST /api/v1/claim-admin`.
//!
//! Per `docs/goal/behavior/onboarding.md` §3a (Claim code).

use serde::{Deserialize, Serialize};

use crate::state::LocalizedText;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ClaimCodeState {
    /// Initial state. Submit-enabled iff input is non-empty (the
    /// `submit_enabled` field on the snapshot — driven from machine state).
    Idle,
    /// `wizard_submit_claim_code` is in flight. Submit disabled.
    Submitting,
    /// Server returned 2xx. The wizard transitions to `Done` and
    /// `wizard_outcome()` returns `LoggedIn { nest_url, handle }` —
    /// per-app glue persists `(nest_url, handle, secret)` to its
    /// long-term identity store.
    Claimed,
    /// 4xx response (bad code, already-claimed, signature mismatch).
    /// `submit_enabled` returns to true so the user can retry with a
    /// corrected code.
    Invalid { reason: String },
    /// 5xx / network error. `transient` mirrors the
    /// `InviteRequestState::Error` shape: true means "try again later",
    /// false means "this won't get better on its own."
    Error { transient: bool, cause: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ClaimCodeSnapshot {
    pub state: ClaimCodeState,
    pub message: LocalizedText,
    /// True iff the Claim button should be enabled. The machine sets this
    /// to false during `Submitting` and true on `Idle` / `Invalid` /
    /// `Error`. The client may additionally gate it on input emptiness
    /// (UI-side concern) — that's not modelled here.
    pub submit_enabled: bool,
}

impl ClaimCodeSnapshot {
    pub fn idle() -> Self {
        Self {
            state: ClaimCodeState::Idle,
            message: LocalizedText {
                key: "onboarding.claim_code.idle".into(),
                args: Default::default(),
            },
            // The wizard surface enables Claim once the user types a code;
            // `submit_enabled` here means "the machine isn't in an inflight
            // state". The UI further gates on input emptiness.
            submit_enabled: true,
        }
    }
}

impl Default for ClaimCodeSnapshot {
    fn default() -> Self {
        Self::idle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_snapshot_is_submittable() {
        let s = ClaimCodeSnapshot::idle();
        assert_eq!(s.state, ClaimCodeState::Idle);
        assert!(s.submit_enabled);
    }

    #[test]
    fn unit_state_serializes_as_string() {
        // Pin the wire shape: the Python e2e helper sends
        // `{"state": "Idle"}` / `"Submitting"` / `"Claimed"` as bare
        // strings (external-tagging default for unit variants). If serde's
        // tagging changes, the bridge breaks silently.
        let raw = serde_json::to_string(&ClaimCodeState::Idle).unwrap();
        assert_eq!(raw, "\"Idle\"");
        let raw = serde_json::to_string(&ClaimCodeState::Submitting).unwrap();
        assert_eq!(raw, "\"Submitting\"");
        let raw = serde_json::to_string(&ClaimCodeState::Claimed).unwrap();
        assert_eq!(raw, "\"Claimed\"");
    }

    #[test]
    fn tagged_state_round_trips() {
        let s = ClaimCodeState::Invalid {
            reason: "not match".into(),
        };
        let raw = serde_json::to_string(&s).unwrap();
        let s2: ClaimCodeState = serde_json::from_str(&raw).unwrap();
        assert_eq!(s, s2);

        let s = ClaimCodeState::Error {
            transient: true,
            cause: "boom".into(),
        };
        let raw = serde_json::to_string(&s).unwrap();
        let s2: ClaimCodeState = serde_json::from_str(&raw).unwrap();
        assert_eq!(s, s2);
    }

    #[test]
    fn snapshot_round_trips_via_json() {
        // Mirrors the e2e bridge's wire path: Python serialises the dict,
        // sends across, the machine deserialises into ClaimCodeSnapshot.
        let s = ClaimCodeSnapshot {
            state: ClaimCodeState::Invalid {
                reason: "code does not match any pending claim".into(),
            },
            message: LocalizedText {
                key: "onboarding.claim_code.invalid".into(),
                args: Default::default(),
            },
            submit_enabled: true,
        };
        let raw = serde_json::to_string(&s).unwrap();
        let s2: ClaimCodeSnapshot = serde_json::from_str(&raw).unwrap();
        assert_eq!(s, s2);
    }
}
