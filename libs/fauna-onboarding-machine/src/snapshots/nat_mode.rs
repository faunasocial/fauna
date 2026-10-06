//! Snapshot for the wizard's `nat_mode_choice` page.
//!
//! Reached after the storage-mode step commits (interim wiring — Phase-4 S8.7
//! re-anchors the transition on claim completion). The single, terminal
//! admin-path setup step: the admin confirms the nest's NAT axis
//! (`public` / `private`); `submit_nat_mode_choice` commits via the mutable
//! `fauna.setup.nat_mode` kind and exits to LoggedIn on success;
//! `defer_nat_mode_choice` exits keeping the seeded value (already a working
//! default — settable later from the admin panel).
//!
//! Per docs/goal/behavior/onboarding.md § 3b-bis.

use serde::{Deserialize, Serialize};

use crate::state::{LocalizedText, NodeMode};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum NatModeState {
    /// Initial / picking. Submit enabled. The pre-selection is the nest's
    /// resolved seed with the one-directional private-ward refinement
    /// (`reset_nat_mode_snapshot`), so the common case is confirm-only.
    Choosing,
    /// `submit_nat_mode_choice` is in flight. Submit disabled.
    Submitting,
    /// Server upserted the row. The wizard transitions to `Done` with
    /// `wizard_outcome() == LoggedIn`.
    Done,
    /// Submit failed. `transient: true` means transport/internal — retry may
    /// help. `transient: false` means a 4xx-class reject (`not_claimed`,
    /// `signature_failed`, `invalid_request`). Submit stays enabled either
    /// way — the set is mutable, resubmit is always allowed.
    Error { transient: bool, cause: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct NatModeSnapshot {
    pub state: NatModeState,
    /// The mode the confirm button will commit. Defaults to the nest's
    /// resolved seed (read from `fauna.setup.status`'s `node_mode`), refined
    /// private-ward for a private-network handle target — see
    /// `OnboardingMachine::reset_nat_mode_snapshot`.
    pub selected_mode: NodeMode,
    pub message: LocalizedText,
    /// True iff `nat-mode-confirm-button` should be enabled: `Choosing` and
    /// `Error` (resubmit allowed); disabled in `Submitting` and `Done`.
    pub submit_enabled: bool,
}

impl NatModeSnapshot {
    pub fn idle() -> Self {
        Self {
            state: NatModeState::Choosing,
            selected_mode: NodeMode::Public,
            message: LocalizedText {
                key: "onboarding.nat_mode.choosing".into(),
                args: Default::default(),
            },
            submit_enabled: true,
        }
    }
}

impl Default for NatModeSnapshot {
    fn default() -> Self {
        Self::idle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_snapshot_is_choosing_public_submittable() {
        let s = NatModeSnapshot::idle();
        assert_eq!(s.state, NatModeState::Choosing);
        assert_eq!(s.selected_mode, NodeMode::Public);
        assert_eq!(s.message.key, "onboarding.nat_mode.choosing");
        assert!(s.submit_enabled);
    }

    #[test]
    fn snapshot_round_trips_via_json() {
        let s = NatModeSnapshot {
            state: NatModeState::Error {
                transient: true,
                cause: "boom".into(),
            },
            selected_mode: NodeMode::Private,
            message: LocalizedText {
                key: "k".into(),
                args: Default::default(),
            },
            submit_enabled: true,
        };
        let raw = serde_json::to_string(&s).unwrap();
        let s2: NatModeSnapshot = serde_json::from_str(&raw).unwrap();
        assert_eq!(s, s2);
    }
}
