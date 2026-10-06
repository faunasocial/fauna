//! **Whether this engine still needs the retired owner keys** — the observable
//! `sync-agent.md` § Credential model bound (3) is enforced on, and the one
//! piece of post-succession state that is a *proof* rather than a report.
//!
//! Bound (3) says the retired `BackupKey`s are *"pushed only while a re-seal is
//! owed and the capability is re-provisioned without them once the corpus is
//! re-sealed"*. Its enforcement design (§ Credential model → *Bound (3)'s
//! enforcement design*, ratified 2026-08-05) fixes the per-engine observable in
//! ruling 3: an engine has **drained** when
//!
//! 1. the placeholder fold has completed at least one full **non-deferred**
//!    round against the nest since this DB was created, and
//! 2. [`SyncDb::list_pending_current_root_reseal`] is empty **at answer time**.
//!
//! Both halves are load-bearing and neither is sufficient:
//!
//! - **Without (1)** a fresh empty DB reads as drained — it has an empty list
//!   because it has no rows yet, not because anything was re-sealed. That is
//!   precisely the fresh-device successor the whole aftermath exists for, and
//!   today's catch-up even runs the re-seal pass *before* the first fold
//!   (`fauna_sync_engine::engine_lifecycle::catch_up_pass`), so first-pass-empty is the
//!   common case rather than a corner. This is the family of bug: an
//!   observable blind to its own dominant input.
//! - **Without (2)** a fold proves only that the roster arrived, not that
//!   anything moved off the retired root.
//!
//! A **deferred** batch must not stamp (1). A deferred change is one the engine
//! could not resolve — and an unopened sealed path may be exactly a
//! predecessor-sealed label, i.e. the very evidence that the retired keys are
//! still needed. Stamping there would let the thing the keys are *for* count as
//! proof they are not.
//!
//! ⚠ **This is not [`crate::succession_progress`].** That module holds the
//! progress *report* a pass writes for the user-facing line; this one holds the
//! *evidence*. A report says what one pass did; only the live list says what is
//! still owed. Enforcing the bound on a count from there — including
//! `Settled { owed: 0 }` — is the mistake `sync-agent.md` A8 names by name.
//!
//! ## The root-generation guard (ruling 5)
//!
//! `sync_entries.current_root_sealed` is a bare boolean and nothing ever cleared
//! it, so a **second** succession (B→C) left every B-era entry stamped "sealed
//! under the current root" while its bytes actually rest under the retired B
//! root. The re-seal pass would skip exactly that corpus and the list would
//! drain **vacuously** — handing bound (3) a proof that is false. That is a
//! defect in the re-seal mechanism independent of the license, and this module
//! fixes it by persisting the identity that stamped the sentinels beside them:
//!
//! - [`SyncDb::adopt_sentinel_root_actor`] is the **write** guard — an engine
//!   coming up under a different actor id clears every sentinel and the fold
//!   marker before any pass runs, so the pass re-examines the whole corpus.
//! - [`SyncDb::reseal_drain`] is the **read** guard — it answers *not drained*
//!   whenever the stored identity is absent or does not match the caller's,
//!   without writing anything. The status handler serves the observable from a
//!   **read-only** DB handle and so cannot run the write guard; making the read
//!   defend itself is what keeps a device that has not yet caught up from
//!   reporting a stale drain.

use anyhow::Result;

/// The `meta` key the fold evidence rests under. Versioned in the name so a
/// future shape change is a new key rather than a misparse of an old one — and
/// an unreadable marker degrades to "not folded", which is the fail-closed
/// reading (the keys stay).
const META_FOLD_KEY: &str = "succession_fold_evidence_v1";

/// The `meta` key holding the hex actor id whose root the `current_root_sealed`
/// sentinels (and the fold marker) were written under.
const META_SENTINEL_ACTOR_KEY: &str = "sentinel_root_actor_v1";

/// One engine's answer to *"do you still need the retired owner keys?"*.
///
/// Deliberately **two booleans rather than one**, so a cold reader (and a
/// support log) can see *why* a device is not licensed — waiting on its first
/// fold is a different situation from still owing re-seals, and they resolve on
/// different timescales. The conjunction is taken in exactly one place,
/// `fauna_client_sync::agent::predecessor_keys_may_be_dropped`, so no app
/// recombines the halves and none of the seven can get it wrong independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResealDrain {
    /// A full non-deferred fold has landed against the nest since this DB was
    /// created, under the *current* root generation.
    pub folded: bool,
    /// [`SyncDb::list_pending_current_root_reseal`] was empty at answer time,
    /// under the *current* root generation.
    pub nothing_owed: bool,
    /// Every row holding bytes at rest was actually *visible* to that list —
    /// [`SyncDb::count_unclassified_at_rest`] was zero.
    ///
    /// The beside-conjunct that separates the list's two empty readings,
    /// *nothing is owed* from *nothing was looked at*. Without it the
    /// license's own test suite passes on an empty answer, which is the failure
    /// mode; and one arm of the invisibility — an at-rest row with no recorded
    /// manifest — never flips the predicate back, so ruling 2's self-healing
    /// argument does not cover it and the strand would be permanent.
    pub all_at_rest_classified: bool,
}

impl ResealDrain {
    /// The fail-closed answer: this engine cannot show it has drained.
    ///
    /// Returned whenever the question cannot be answered honestly — a DB whose
    /// sentinels belong to another identity, a fresh DB that has never adopted
    /// one, or a read that failed. Callers must treat it exactly like an absent
    /// answer: the keys stay.
    pub const NOT_DRAINED: Self = Self {
        folded: false,
        nothing_owed: false,
        all_at_rest_classified: false,
    };

    /// Whether this engine has verifiably drained — every conjunct, never a
    /// subset. Each one rules out a different way of being empty for the wrong
    /// reason.
    pub fn drained(&self) -> bool {
        self.folded && self.nothing_owed && self.all_at_rest_classified
    }
}

impl crate::db::SyncDb {
    /// Stamp "a full non-deferred fold just completed against the nest".
    ///
    /// Called from `fauna_sync_engine::engine::SyncEngine::pull_remote_changes`'s two
    /// non-deferred exits — the empty-feed return and the post-apply arm of a
    /// batch that deferred nothing. Idempotent; the marker is presence-only,
    /// since "at least one" is the whole question.
    pub fn mark_succession_fold_evidence(&self) -> Result<()> {
        self.meta_put(META_FOLD_KEY, "1")
    }

    /// Whether this DB carries fold evidence. An absent or unreadable marker is
    /// `false` — the fail-closed reading.
    pub fn has_succession_fold_evidence(&self) -> Result<bool> {
        Ok(self.meta_get(META_FOLD_KEY)?.is_some())
    }

    /// The hex actor id the sentinels in this DB were stamped under, or `None`
    /// on a DB that has never adopted one.
    pub fn sentinel_root_actor(&self) -> Result<Option<String>> {
        self.meta_get(META_SENTINEL_ACTOR_KEY)
    }

    /// **The write guard (ruling 5).** Bind this DB's `current_root_sealed`
    /// sentinels to `actor_id_hex`, clearing every sentinel and the fold marker
    /// first when they belong to a different identity.
    ///
    /// Returns `true` when it cleared — i.e. this engine just came up under a
    /// new root generation and the whole corpus is owed a fresh examination.
    /// A first adoption on a DB that never had one (`None` stored) writes the
    /// id **without** clearing: there is nothing yet to distrust, every entry's
    /// sentinel was written by this same identity, and clearing would force a
    /// pointless full re-examination on every existing device at upgrade.
    ///
    /// Call before any pass reads the sentinels — `fauna_sync_engine`'s
    /// engine-lifecycle catch-up does, ahead of both re-seal passes.
    pub fn adopt_sentinel_root_actor(&self, actor_id_hex: &str) -> Result<bool> {
        match self.sentinel_root_actor()? {
            Some(stored) if stored == actor_id_hex => Ok(false),
            Some(_) => {
                self.clear_current_root_sealed()?;
                self.meta_del(META_FOLD_KEY)?;
                self.meta_put(META_SENTINEL_ACTOR_KEY, actor_id_hex)?;
                Ok(true)
            }
            None => {
                self.meta_put(META_SENTINEL_ACTOR_KEY, actor_id_hex)?;
                Ok(false)
            }
        }
    }

    /// **The read guard + the observable.** This engine's [`ResealDrain`] as of
    /// now, from the point of view of `actor_id_hex`.
    ///
    /// Answers [`ResealDrain::NOT_DRAINED`] — without writing anything — when
    /// the stored sentinel identity is absent or is not `actor_id_hex`. That is
    /// the read half of ruling 5: a successor's device that has not yet run the
    /// write guard holds sentinels that mean nothing to it, and must not be able
    /// to report them as a drain.
    pub fn reseal_drain(&self, actor_id_hex: &str) -> Result<ResealDrain> {
        if self.sentinel_root_actor()?.as_deref() != Some(actor_id_hex) {
            return Ok(ResealDrain::NOT_DRAINED);
        }
        Ok(ResealDrain {
            folded: self.has_succession_fold_evidence()?,
            nothing_owed: self.list_pending_current_root_reseal()?.is_empty(),
            all_at_rest_classified: self.count_unclassified_at_rest()? == 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{SyncDb, SyncState};
    use fauna_core::data::ContentHash;

    fn db() -> SyncDb {
        SyncDb::open_in_memory().expect("in-memory db")
    }

    fn actor(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    /// An entry at rest with a recorded manifest — the shape
    /// `list_pending_current_root_reseal` counts as owed.
    fn seed_owed_entry(db: &SyncDb, path: &str) {
        seed_entry(
            db,
            path,
            SyncState::Synced,
            Some(ContentHash::from_digest_raw([7u8; 32])),
        );
    }

    fn seed_entry(db: &SyncDb, path: &str, state: SyncState, manifest: Option<ContentHash>) {
        db.upsert_entry(path, None, None, manifest, state, 0, 0, 1, 1, None)
            .expect("upsert");
    }

    #[test]
    fn a_fresh_db_is_not_drained_even_though_its_list_is_empty() {
        // The family: an empty list on a DB that never folded is the
        // fresh-device successor, not a finished one.
        let db = db();
        db.adopt_sentinel_root_actor(&actor(1)).unwrap();
        assert!(db.list_pending_current_root_reseal().unwrap().is_empty());
        let drain = db.reseal_drain(&actor(1)).unwrap();
        assert!(!drain.folded, "no fold has landed yet");
        assert!(drain.nothing_owed, "the list really is empty");
        assert!(!drain.drained(), "an unfolded engine has not drained");
    }

    #[test]
    fn fold_evidence_plus_an_empty_list_is_a_drain() {
        let db = db();
        db.adopt_sentinel_root_actor(&actor(1)).unwrap();
        db.mark_succession_fold_evidence().unwrap();
        assert!(db.reseal_drain(&actor(1)).unwrap().drained());
    }

    /// **The vacuity pin.** An at-rest row the pending predicate cannot
    /// see leaves that list empty for the WRONG reason — nothing was looked at,
    /// not nothing is owed. A `Synced` row with no recorded manifest is the arm
    /// that never heals: it gains no manifest by itself, so it never flips the
    /// predicate back, and a drop licensed here strands its bytes permanently.
    #[test]
    fn an_at_rest_row_the_predicate_cannot_see_is_not_a_drain() {
        let db = db();
        db.adopt_sentinel_root_actor(&actor(1)).unwrap();
        db.mark_succession_fold_evidence().unwrap();
        seed_entry(&db, "unnamed.txt", SyncState::Synced, None);

        let drain = db.reseal_drain(&actor(1)).unwrap();
        assert!(
            drain.nothing_owed,
            "precondition: the pending list really is empty — that is the trap"
        );
        assert!(!drain.all_at_rest_classified);
        assert!(
            !drain.drained(),
            "empty because nothing was looked at must never license the drop"
        );
    }

    /// The in-flight states are the other invisible arm. This one does heal —
    /// the row settles into `Synced`/`Placeholder` — but the license must not be
    /// granted while it is mid-move, since it was never examined.
    #[test]
    fn an_in_flight_row_is_not_a_drain() {
        let db = db();
        db.adopt_sentinel_root_actor(&actor(1)).unwrap();
        db.mark_succession_fold_evidence().unwrap();
        seed_entry(
            &db,
            "moving.txt",
            SyncState::Uploading,
            Some(ContentHash::from_digest_raw([9u8; 32])),
        );
        assert!(!db.reseal_drain(&actor(1)).unwrap().drained());
    }

    /// The one honest exclusion: a deleted row has nothing at rest, so it must
    /// not hold the license open forever.
    #[test]
    fn a_deleted_row_holds_nothing_at_rest_and_does_not_block_the_drain() {
        let db = db();
        db.adopt_sentinel_root_actor(&actor(1)).unwrap();
        db.mark_succession_fold_evidence().unwrap();
        seed_entry(&db, "gone.txt", SyncState::Deleted, None);
        assert!(db.reseal_drain(&actor(1)).unwrap().drained());
    }

    #[test]
    fn a_folded_engine_with_an_owed_entry_has_not_drained() {
        let db = db();
        db.adopt_sentinel_root_actor(&actor(1)).unwrap();
        db.mark_succession_fold_evidence().unwrap();
        seed_owed_entry(&db, "owed.txt");
        let drain = db.reseal_drain(&actor(1)).unwrap();
        assert!(drain.folded);
        assert!(!drain.nothing_owed);
        assert!(!drain.drained());
    }

    #[test]
    fn a_drained_db_read_under_a_different_actor_reports_not_drained() {
        // The read guard: the sentinels belong to actor 1, so actor 2 — a
        // successor — must not be able to read them as its own proof.
        let db = db();
        db.adopt_sentinel_root_actor(&actor(1)).unwrap();
        db.mark_succession_fold_evidence().unwrap();
        assert!(db.reseal_drain(&actor(1)).unwrap().drained());
        assert_eq!(
            db.reseal_drain(&actor(2)).unwrap(),
            ResealDrain::NOT_DRAINED
        );
    }

    #[test]
    fn a_db_that_never_adopted_an_actor_reports_not_drained() {
        let db = db();
        db.mark_succession_fold_evidence().unwrap();
        assert_eq!(
            db.reseal_drain(&actor(1)).unwrap(),
            ResealDrain::NOT_DRAINED
        );
    }

    #[test]
    fn a_second_succession_clears_the_sentinels_and_the_fold_marker() {
        // Ruling 5's whole point: B-era entries stamped `current_root_sealed`
        // rest under the retired B root once C succeeds. Without the clear the
        // re-seal pass skips them and the list drains vacuously.
        let db = db();
        db.adopt_sentinel_root_actor(&actor(1)).unwrap();
        db.mark_succession_fold_evidence().unwrap();
        seed_owed_entry(&db, "b-era.txt");
        db.mark_current_root_sealed("b-era.txt").unwrap();
        assert!(db.reseal_drain(&actor(1)).unwrap().drained());

        assert!(
            db.adopt_sentinel_root_actor(&actor(2)).unwrap(),
            "a new actor id must report that it cleared"
        );
        let drain = db.reseal_drain(&actor(2)).unwrap();
        assert!(
            !drain.folded,
            "the fold marker is cleared with the sentinels"
        );
        assert!(
            !drain.nothing_owed,
            "the B-era entry is owed again under C's root"
        );
        assert_eq!(
            db.list_pending_current_root_reseal().unwrap().len(),
            1,
            "the re-seal pass must re-examine the whole corpus"
        );
    }

    #[test]
    fn re_adopting_the_same_actor_clears_nothing() {
        let db = db();
        db.adopt_sentinel_root_actor(&actor(1)).unwrap();
        db.mark_succession_fold_evidence().unwrap();
        seed_owed_entry(&db, "kept.txt");
        db.mark_current_root_sealed("kept.txt").unwrap();

        assert!(!db.adopt_sentinel_root_actor(&actor(1)).unwrap());
        assert!(
            db.reseal_drain(&actor(1)).unwrap().drained(),
            "a same-identity restart must not force a full re-examination"
        );
    }

    #[test]
    fn a_first_adoption_on_an_existing_db_keeps_its_sentinels() {
        // The upgrade path: every existing device's sentinels were written by
        // the identity it is still running as, so adopting must not clear them.
        let db = db();
        seed_owed_entry(&db, "existing.txt");
        db.mark_current_root_sealed("existing.txt").unwrap();
        db.mark_succession_fold_evidence().unwrap();

        assert!(!db.adopt_sentinel_root_actor(&actor(1)).unwrap());
        assert!(db.reseal_drain(&actor(1)).unwrap().drained());
    }
}
