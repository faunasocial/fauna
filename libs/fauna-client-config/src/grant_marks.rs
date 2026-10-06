//! The owner's verdict on a carried-across capability grant — the Nests page's
//! **Keep** on a grant row marked un-adjudicated (`succession-aftermath.md`
//! § Re-key scope → *Adjudicating what the aftermath carries across*, the
//! capability-grants row), written through the succession-ledger seam
//! (`fauna.state.succession-ledger`'s `grant-mark/…` rows).
//!
//! The *Remove* half needs nothing here: revoking the grant retires its
//! Nests-page row. The raise is the post-store-ready pass's
//! (`SuccessionLedgerStore::raise_grant_marks`).

use crate::store_seam::{StoreError, SuccessionLedgerStore};
use fauna_core::data::UnattestedVerdict;

/// Record **Keep** on every open mark of `grant_id`. Returns whether anything
/// was open, so a caller can tell a real adjudication from a no-op — which is
/// not an error: a concurrent device may already have answered, and the
/// honest outcome is the same.
///
/// No CAS: each mark row's arm is the verdict-precedence minimum, so a
/// decided verdict beats a still-`Open` one whichever lands first. A press on
/// an already-answered grant writes nothing.
pub async fn keep_grant_mark(
    ledger: &dyn SuccessionLedgerStore,
    grant_id: &[u8],
) -> Result<bool, StoreError> {
    let mut current = ledger.load().await?;
    if !current.decide_grant_marks_for(grant_id, UnattestedVerdict::Kept) {
        return Ok(false);
    }
    ledger.merge(current.marks_replica()).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::FakeSuccessionLedgerStore;
    use crate::test_nest::block_on;
    use fauna_core::data::GrantUnattestedMark;
    use fauna_core::identity::ActorId;
    use fauna_core::succession_ledger::SuccessionLedger;

    fn mark(grant: u8, verdict: UnattestedVerdict) -> GrantUnattestedMark {
        GrantUnattestedMark {
            grant_id: vec![grant; 16],
            predecessor: ActorId([7u8; 32]),
            verdict,
        }
    }

    #[test]
    fn keep_records_the_verdict_and_a_second_press_writes_nothing() {
        let store = FakeSuccessionLedgerStore::with(SuccessionLedger {
            unattested_grant_marks: vec![mark(1, UnattestedVerdict::Open)],
            ..SuccessionLedger::empty(ActorId([1u8; 32]))
        });
        assert!(block_on(keep_grant_mark(&store, &[1u8; 16])).unwrap());
        assert_eq!(
            store.current().unattested_grant_marks,
            vec![mark(1, UnattestedVerdict::Kept)],
            "the verdict is recorded, the mark never deleted"
        );
        let merges = store.merges();
        assert!(!block_on(keep_grant_mark(&store, &[1u8; 16])).unwrap());
        assert_eq!(store.merges(), merges, "an answered mark puts nothing");
    }

    #[test]
    fn a_refused_keep_surfaces_and_leaves_the_mark_open() {
        let store = FakeSuccessionLedgerStore::with(SuccessionLedger {
            unattested_grant_marks: vec![mark(1, UnattestedVerdict::Open)],
            ..SuccessionLedger::empty(ActorId([1u8; 32]))
        });
        store.refuse_next_merges(1);
        assert!(block_on(keep_grant_mark(&store, &[1u8; 16])).is_err());
        assert!(store.current().unattested_grant_marks[0].verdict.is_open());
    }
}
