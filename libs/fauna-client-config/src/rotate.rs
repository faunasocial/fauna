//! The deployment-seed rotation ceremony's shared **outcome** — the
//! [`SeedRotation`] verdict type, its screen sentence, and the retry budget.
//!
//! Authority: `docs/goal/architecture/nest/box-recovery.md` § Deployment-seed
//! rotation (§ The ceremony + § Custody after rotation).
//!
//! The drive itself — mint, custody on the account plane *before* dispatch
//! (the CR-1 ordering, `nest/common.md`), dispatch, mark — is
//! [`crate::rotate_deployment_seed_on_plane`] (`crate::custody_leg`); this module
//! holds only what that drive and every app's rendering of it share.

/// How many times the drive re-attempts a step that failed *transiently*.
///
/// Deliberately more generous than the custody leg's capture budget
/// (`MAX_CAPTURE_ATTEMPTS`), for a reason specific to
/// this ceremony: a committed rotation **tears down the box's serving generation**
/// (`box-recovery.md` § Adoption by the running process), so every live connection
/// — including the one this drive is running on — is dropped by WS 1001 right after
/// the rotate reply flushes. The marking step therefore reconnects *by design*, and
/// that reconnect additionally re-pins through the rotation chain before it can
/// authenticate. Each attempt already blocks for reconnect up to the per-request
/// deadline, so this needs no in-loop sleep (wasm-clean), the same as the capture.
pub(crate) const MAX_ROTATION_ATTEMPTS: usize = 5;

/// The outcome of [`crate::rotate_deployment_seed_on_plane`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedRotation {
    /// The box is serving the successor identity this drive minted and custodied.
    Rotated {
        /// The successor `nest_actor_id` — the box's identity from now on.
        nest_actor_id: [u8; 32],
        /// The rotation-log position the ceremony wrote; `0` on an idempotent ack.
        seq: u64,
        /// The nest acked idempotently (it was already serving this seed — a retry
        /// after a lost reply). A success, not a failure: the box is on the
        /// identity the caller asked for.
        already_rotated: bool,
        /// Whether the predecessor's custody entry was marked `superseded_by`
        /// before this returned. `false` means the rotation itself committed but
        /// the bookkeeping write did not land — cosmetic and self-healing (any
        /// client that verifies the chain writes the same marker), but the caller
        /// should say so rather than swallow it: until it lands, the admin's
        /// recovery list still offers a box identity the fleet now refuses.
        predecessor_marked: bool,
    },
    /// The box acked with an identity that is **not** the seed this drive sent —
    /// a protocol violation, so nothing is marked and no ancestor is evicted. The
    /// successor seed stays custodied (an orphan entry, harmless); the caller
    /// should surface the mismatch rather than retry into it.
    RefusedIdentityMismatch {
        /// The `nest_actor_id` the box claimed after the ceremony.
        presented: [u8; 32],
    },
}

/// The sentence a screen shows for a finished ceremony — decided once here
/// rather than seven times (priority #2), because the mapping is not obvious in
/// exactly the place it matters most.
///
/// **`predecessor_marked: false` is a SUCCESS with a caveat, never a failure.**
/// By the time it can be false the box *has* rotated, so an app that reported it
/// as an error would tell the admin the opposite of what happened to their nest.
/// What it actually costs them is one stale row: the predecessor still shows as
/// recoverable in their own recovery list until some client verifies the chain
/// and writes the marker (any of them will, on the next connect). The wording
/// says exactly that and nothing worse.
///
/// [`SeedRotation::RefusedIdentityMismatch`] is the genuinely alarming one — the
/// box acked an identity nobody sent — so it gets its own sentence rather than
/// collapsing into a generic failure.
pub fn seed_rotation_verdict(outcome: &SeedRotation) -> fauna_core::localized::LocalizedText {
    use fauna_core::localized::LocalizedText;
    match outcome {
        SeedRotation::Rotated {
            predecessor_marked: true,
            ..
        } => LocalizedText::key("admin.nest_page.rotate_seed_done"),
        SeedRotation::Rotated {
            predecessor_marked: false,
            ..
        } => LocalizedText::key("admin.nest_page.rotate_seed_done_unmarked"),
        SeedRotation::RefusedIdentityMismatch { .. } => {
            LocalizedText::key("admin.nest_page.rotate_seed_mismatch")
        }
    }
}

#[cfg(test)]
mod verdict_tests {
    use super::*;

    fn rotated(predecessor_marked: bool) -> SeedRotation {
        SeedRotation::Rotated {
            nest_actor_id: [9u8; 32],
            seq: 1,
            already_rotated: false,
            predecessor_marked,
        }
    }

    /// The three verdicts are three DISTINCT sentences. The pair that matters is
    /// the first two: an unmarked predecessor must not be reported with the same
    /// words as a clean rotation, because the difference is a stale entry in the
    /// admin's own recovery list — the one thing they would act on.
    ///
    /// Mutation-verified: collapsing the unmarked arm into the clean one reds
    /// exactly this test. It exists because the first version of this mapping
    /// lived inline in tui's op and *survived* that mutation — the app-level
    /// tests only ever set the rendered string directly, so nothing anywhere
    /// pinned that `predecessor_marked` changed what the admin is told.
    #[test]
    fn every_finished_ceremony_gets_its_own_sentence() {
        let clean = seed_rotation_verdict(&rotated(true));
        let unmarked = seed_rotation_verdict(&rotated(false));
        let mismatch =
            seed_rotation_verdict(&SeedRotation::RefusedIdentityMismatch { presented: [1; 32] });

        assert_ne!(
            clean, unmarked,
            "an unmarked predecessor is a success the admin must still be told about"
        );
        assert_ne!(clean, mismatch);
        assert_ne!(unmarked, mismatch);
    }

    /// An idempotent ack (`already_rotated`) is a success, and it is deliberately
    /// NOT a fourth sentence: the box is on the identity the caller asked for, so
    /// the only thing left to report is whether the bookkeeping landed.
    #[test]
    fn an_idempotent_ack_reads_as_the_ordinary_success() {
        let ack = SeedRotation::Rotated {
            nest_actor_id: [9u8; 32],
            seq: 0,
            already_rotated: true,
            predecessor_marked: true,
        };
        assert_eq!(
            seed_rotation_verdict(&ack),
            seed_rotation_verdict(&rotated(true))
        );
    }
}
