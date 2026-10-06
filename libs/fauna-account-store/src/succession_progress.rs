//! **What the post-succession corpus re-seal has done so far** — the durable
//! half of leg 5's progress surface (`succession-aftermath.md` § Re-key scope:
//! the re-seal is *"started at first successor sign-in, **surfaced with
//! progress, resumed until complete**"*).
//!
//! This module holds the record the pass **writes**; the user-facing projection
//! that turns it into a line of copy lives in
//! `fauna_client_sync::agent::CorpusResealProgress`, because on desktop the
//! reader is a different **process** — the pass runs inside the per-user
//! `fauna-sync-agent`, and the app learns of it over `ListEngines`
//! (`sync-agent.md` § Control plane split). Splitting the two here is what keeps
//! the copy from being written twice: the engine records facts, one shared
//! projection decides what the user is told about them, and no app writes a
//! `match` over either.
//!
//! **Why it is persisted at all, rather than returned to the caller.** The four
//! sibling legs of the aftermath run *in* the signing-in app, so their outcome
//! reaches the UI by simply being returned. This one does not: it runs in the
//! agent's catch-up pass, on whatever schedule that pass runs, and the Settings
//! page is typically opened long after — often in a later app launch entirely.
//! A value the UI can read **between passes** therefore has to rest somewhere,
//! and the engine's own `SyncDb` is the only store on the producing side of the
//! process boundary.
//!
//! ⚠ **This is not the completion observable.** `SyncDb::list_pending_current_root_reseal`
//! is, and `sync-agent.md` § Credential model bound (3) is enforced on *that*
//! drained list — never on a count from here. The record below reports what one
//! pass did; it is a progress *report*, and a report is not a proof.

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The `meta` key the record rests under. Versioned in the name so a future
/// shape change is a new key rather than a parse failure of an old one — a
/// record that cannot be read degrades to "no pass has run", which is exactly
/// the honest reading (the pass itself is idempotent and re-drives).
const META_KEY: &str = "corpus_reseal_pass_v1";

/// One pass of `fauna_sync_engine::engine::SyncEngine::reseal_predecessor_sealed`,
/// as the progress surface needs it.
///
/// **Three arms, and the middle one carries two numbers rather than a boolean**,
/// because "done" and "still owed" are the same pass with different remainders:
/// a device that moved 900 files and could not move 3 has genuinely made
/// progress *and* is genuinely unfinished, and a surface that reported only one
/// of those would be lying either way (the same reasoning that gave leg 3 its
/// partly-owed line).
///
/// [`Self::Running`] is written **at pass entry** and overwritten at exit. It
/// therefore also covers the interrupted case — a pass killed mid-way leaves it
/// behind — and that is correct rather than merely tolerable: the entries it
/// had not reached are still unmarked, so the corpus really is still owed and
/// the next catch-up really does re-drive it. "In progress" is the true
/// statement about a corpus in exactly that state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CorpusResealPass {
    /// A pass is under way (or was interrupted before it settled).
    Running,
    /// A pass finished. `owed` counts entries it examined and could not move —
    /// a contained per-entry failure, or a re-sealed copy whose change record
    /// did not land (which deliberately leaves the entry unmarked).
    Settled { resealed: u64, owed: u64 },
    /// The pass could not run at all. Carries the display string the caller
    /// already has, since these errors are not enumerable enough to key i18n
    /// off — the same convention the four sibling legs' `Failed` arms follow.
    Failed { reason: String },
}

impl CorpusResealPass {
    /// Whether anything is still owed **by this device**, on a retry.
    ///
    /// `Running` and `Failed` both count as owed: neither settled anything, and
    /// § Re-key scope's *"resumed until complete"* is exactly "keep driving
    /// while this is true". Note this is the *device's* view — a pass that
    /// settled with nothing owed here says nothing about another device's sets.
    ///
    /// ⚠ **Deliberately NOT a `fauna_core::progress::Passage<O>`**, though it
    /// spells the same `Running | Failed => owed` rule the seven aftermath
    /// projections now inherit from that one type. This is the persisted
    /// *producing-side* record (a versioned at-rest shape under `META_KEY`,
    /// read across a process boundary); the `Passage` is its *projection*,
    /// `fauna_client_sync::agent::CorpusResealProgress`, which answers "owed by
    /// someone" where this answers "owed by this device". Two questions, two
    /// types — converting this one would churn an at-rest shape for no gain.
    pub fn still_owed(&self) -> bool {
        match self {
            Self::Running | Self::Failed { .. } => true,
            Self::Settled { owed, .. } => *owed > 0,
        }
    }
}

impl crate::db::SyncDb {
    /// Read the last recorded pass, or `None` when this set has never run one.
    ///
    /// `None` is the reading for every identity that never succeeded — the pass
    /// returns before touching the DB when it holds no predecessor keys, so the
    /// overwhelmingly common fleet never writes a record and never renders a
    /// line. It is *also* the reading on a successor's device that holds no
    /// predecessor material; distinguishing those two is the caller's job and
    /// needs a fact the engine does not have (whether the account succeeded at
    /// all), which is why `fauna_client_sync::agent::corpus_reseal_progress`
    /// takes it as an argument.
    ///
    /// An unparseable record reads as `None` rather than erroring: the record is
    /// a progress report about an idempotent pass, so losing one costs a line of
    /// copy until the next pass, never correctness.
    pub fn corpus_reseal_pass(&self) -> Result<Option<CorpusResealPass>> {
        Ok(self
            .meta_get(META_KEY)?
            .and_then(|stored| serde_json::from_str(&stored).ok()))
    }

    /// Record what the pass is doing / has done. Called at pass entry
    /// ([`CorpusResealPass::Running`]) and at exit.
    pub fn record_corpus_reseal_pass(&self, pass: &CorpusResealPass) -> Result<()> {
        self.meta_put(META_KEY, &serde_json::to_string(pass)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SyncDb;

    fn db() -> SyncDb {
        SyncDb::open_in_memory().expect("in-memory db")
    }

    #[test]
    fn a_set_that_never_ran_a_pass_reads_as_no_record() {
        assert_eq!(db().corpus_reseal_pass().unwrap(), None);
    }

    #[test]
    fn a_recorded_pass_round_trips() {
        let db = db();
        for pass in [
            CorpusResealPass::Running,
            CorpusResealPass::Settled {
                resealed: 9,
                owed: 2,
            },
            CorpusResealPass::Failed {
                reason: "nest unreachable".into(),
            },
        ] {
            db.record_corpus_reseal_pass(&pass).unwrap();
            assert_eq!(db.corpus_reseal_pass().unwrap(), Some(pass));
        }
    }

    /// The degradation the type doc promises: a record written by some future
    /// shape (or corrupted) must read as "no pass known", never as an error that
    /// takes the whole status read down with it.
    #[test]
    fn an_unparseable_record_degrades_to_no_record_rather_than_erroring() {
        let db = db();
        db.meta_put(META_KEY, "{\"Settled\":{\"resealed\":")
            .unwrap();
        assert_eq!(db.corpus_reseal_pass().unwrap(), None);
    }

    /// `still_owed` is the resume condition, so the two non-settled arms must
    /// both count as owed — a pass that never settled settled nothing.
    #[test]
    fn only_a_settled_pass_with_nothing_left_is_not_still_owed() {
        assert!(CorpusResealPass::Running.still_owed());
        assert!(CorpusResealPass::Failed { reason: "x".into() }.still_owed());
        assert!(
            CorpusResealPass::Settled {
                resealed: 1,
                owed: 1
            }
            .still_owed()
        );
        assert!(
            !CorpusResealPass::Settled {
                resealed: 4,
                owed: 0
            }
            .still_owed()
        );
    }
}
