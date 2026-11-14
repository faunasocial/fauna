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

/// Record **Keep** on every open mark of `grant_id` AND of `replacement_id`
/// — the id the succession re-mint gives `grant_id`'s replacement
/// (`GrantEventSigner::remint_replacement_id`). Returns whether anything was
/// open, so a caller can tell a real adjudication from a no-op — which is
/// not an error: a concurrent device may already have answered, and the
/// honest outcome is the same.
///
/// Both ids, because the page may have painted the row before the re-mint
/// retired it: the retired id's mark rows stay at rest, still `Open`, while
/// the question now lives on the replacement's carried copies — a Keep that
/// decided the old id alone would answer `true` and leave the refreshed page
/// asking again. Before the retirement the replacement holds no mark, so
/// deciding it is a no-op and the retirement's own carry takes the answer.
/// And the retirement can land between this function's read and its write:
/// what the write answers is decided once more, so a carry that slipped in
/// behind the read is settled too.
///
/// No CAS: each mark row's arm is the verdict-precedence minimum, so a
/// decided verdict beats a still-`Open` one whichever lands first. A press on
/// an already-answered grant writes nothing.
pub async fn keep_grant_mark(
    ledger: &dyn SuccessionLedgerStore,
    grant_id: &[u8],
    replacement_id: &[u8],
) -> Result<bool, StoreError> {
    let mut current = ledger.load().await?;
    let mut decided = false;
    // The read, then what the first write answers: two rounds at most.
    for _ in 0..2 {
        let old = current.decide_grant_marks_for(grant_id, UnattestedVerdict::Kept);
        let new = current.decide_grant_marks_for(replacement_id, UnattestedVerdict::Kept);
        if !(old || new) {
            break;
        }
        decided = true;
        current = ledger.merge(current.marks_replica()).await?;
    }
    Ok(decided)
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
        assert!(block_on(keep_grant_mark(&store, &[1u8; 16], &[9u8; 16])).unwrap());
        assert_eq!(
            store.current().unattested_grant_marks,
            vec![mark(1, UnattestedVerdict::Kept)],
            "the verdict is recorded, the mark never deleted"
        );
        let merges = store.merges();
        assert!(!block_on(keep_grant_mark(&store, &[1u8; 16], &[9u8; 16])).unwrap());
        assert_eq!(store.merges(), merges, "an answered mark puts nothing");
    }

    /// The press names a row the re-mint retired: both the retired id's
    /// rows and the replacement's carried copies are open, and one Keep
    /// settles both in one write.
    #[test]
    fn keep_on_a_retired_id_settles_the_replacement_too() {
        let store = FakeSuccessionLedgerStore::with(SuccessionLedger {
            unattested_grant_marks: vec![
                mark(1, UnattestedVerdict::Open),
                mark(9, UnattestedVerdict::Open),
                mark(5, UnattestedVerdict::Open),
            ],
            ..SuccessionLedger::empty(ActorId([1u8; 32]))
        });
        assert!(block_on(keep_grant_mark(&store, &[1u8; 16], &[9u8; 16])).unwrap());
        let marks = store.current().unattested_grant_marks;
        assert!(!GrantUnattestedMark::any_open(&marks, &[1u8; 16]));
        assert!(!GrantUnattestedMark::any_open(&marks, &[9u8; 16]));
        assert!(
            GrantUnattestedMark::any_open(&marks, &[5u8; 16]),
            "a grant the press did not name keeps its question"
        );
        assert_eq!(store.merges(), 1, "one write settles both");
    }

    #[test]
    fn a_refused_keep_surfaces_and_leaves_the_mark_open() {
        let store = FakeSuccessionLedgerStore::with(SuccessionLedger {
            unattested_grant_marks: vec![mark(1, UnattestedVerdict::Open)],
            ..SuccessionLedger::empty(ActorId([1u8; 32]))
        });
        store.refuse_next_merges(1);
        assert!(block_on(keep_grant_mark(&store, &[1u8; 16], &[9u8; 16])).is_err());
        assert!(store.current().unattested_grant_marks[0].verdict.is_open());
    }
}
