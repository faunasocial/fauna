//! Subscription-tier **period-key custody** — the author's client-side state
//! transitions over a `SubscriptionsConfig` replica
//! (`fauna_core::data::SubscriptionsConfig`), the fold of the account's
//! `fauna.state.subscriptions` rows that [`crate::period_keys::PeriodKeyStore`]
//! reads and joins back.
//!
//! The nest holds no tier period key (`tiers.create` stores only the birth
//! `KeyBlob`), so the author's client
//! generates the 32-byte broadcast period key at tier-create and retains it
//! here — one fleet-only, generation-tip-sealed plane row per period key,
//! synced across the author's device fleet and never readable by a nest. The
//! mint orchestration reads the `current` period to wrap the live roster and
//! rotates it on a membership removal; every rotated-out `prior` period is
//! retained (uncapped) so a later archival mint can backfill a new subscriber's
//! back-catalogue.
//!
//! These functions are **pure** transitions over an in-memory replica (the
//! caller supplies the fresh random key, the current time and the minting
//! identity), so they are deterministic and unit-testable with no RNG. The
//! caller persists a transition by handing the replica to
//! [`PeriodKeyStore::merge_custody`](crate::period_keys::PeriodKeyStore::merge_custody),
//! which joins each record into its row and never deletes one — so there is
//! no "forget" transition here: a period, once recorded, stays. The thin RNG
//! generator and the mint+upload orchestration that drives these live
//! alongside in this crate (the standing shared-Rust home, mirroring how
//! `fauna-client-mail-settings` owns the MSEK lifecycle while `fauna-core::data`
//! owns the `MailConfig` shape).
//!
//! Authority for the at-rest custody shape:
//! `docs/goal/architecture/key-material-hierarchy.md` § Audience: an opaque set
//! of subscriber pubkeys → *Encrypted-mode at-rest custody*. Behavior:
//! `docs/goal/behavior/monetization.md` § Pillar 1.

use fauna_core::data::{PendingRemoval, SubscriptionsConfig, TierPeriod, TierPeriodKeys};
use fauna_core::identity::ActorId;
use fauna_core::subscription::crypto::period_key_commitment;

/// A custody mutation that could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyError {
    /// `rotate` was asked to rotate a tier the author holds no period key for
    /// (it was never created in encrypted mode).
    TierNotFound,
}

impl std::fmt::Display for CustodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CustodyError::TierNotFound => {
                write!(
                    f,
                    "no period key held for tier (not created in encrypted mode)"
                )
            }
        }
    }
}

impl std::error::Error for CustodyError {}

/// Index of the custody entry for `tier_name`, if any.
fn tier_index(custody: &SubscriptionsConfig, tier_name: &str) -> Option<usize> {
    custody.tiers.iter().position(|t| t.tier_name == tier_name)
}

/// Record the version-1 period key for a newly-created tier and return the
/// recorded period.
///
/// **Idempotent** (mirrors the nest's idempotent `tiers.create` upsert): if the
/// author already holds a period key for `tier_name` this is a no-op and the
/// existing `current` is returned unchanged — re-recording would orphan the
/// live `KeyBlob` already minted against the current key. Call this from the
/// tier-create orchestration with a freshly generated key.
///
/// `minter` is the identity minting the key — the session's own
/// (the seat's current actor id), which after a succession is the successor —
/// stamped as the period's `minted_by`, the era stamp the post-succession
/// rotation reads.
pub fn record_new_tier(
    custody: &mut SubscriptionsConfig,
    minter: ActorId,
    tier_name: &str,
    key: [u8; 32],
    now_micros: u64,
) -> TierPeriod {
    if let Some(i) = tier_index(custody, tier_name) {
        return custody.tiers[i].current.clone();
    }
    let period = TierPeriod {
        version: 1,
        key: key.into(),
        rotated_at: now_micros,
        minted_by: Some(minter),
    };
    custody.tiers.push(TierPeriodKeys {
        tier_name: tier_name.to_string(),
        current: period.clone(),
        prior: Vec::new(),
    });
    period
}

/// The current (active) period the mint wraps to the live roster, if the author
/// holds one for `tier_name`.
pub fn current_period(custody: &SubscriptionsConfig, tier_name: &str) -> Option<TierPeriod> {
    tier_index(custody, tier_name).map(|i| custody.tiers[i].current.clone())
}

/// What a stored `KeyBlob`'s key witness (`KeyBlob::key_commitment`) says
/// about the key it wraps, read against this custody's history for the tier
/// — the answer to *"is the blob the nest serves keyed with what new posts
/// seal under?"*, which neither the blob's `author` nor its `rotated_at` can
/// give (an ordinary approve re-stamps both without minting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveBlobKey {
    /// The blob wraps `current` — the tier is settled.
    Current,
    /// The blob wraps a key this custody has rotated OUT (a `prior` period):
    /// the live blob is stale-keyed and a republish of `current` is owed. The
    /// shapes that leave this behind: an approve that read custody before a
    /// rotation but stamped a later `rotated_at`, so the nest never refused
    /// it; and the losing side of two devices'
    /// concurrent same-version rotations.
    RotatedOut,
    /// The blob wraps a key this custody has never held: a peer device rotated
    /// past this custody and its key has not merged in yet. Not this device's
    /// to fix — republishing `current` over it would be the very regression
    /// `RotatedOut` names, from the other side. The next walk brings the
    /// key's row, and the next pass classifies it.
    Foreign,
}

/// Classify the key a stored blob wraps, by its witness, against the tier's
/// custody history. `None` when the author holds no period for `tier_name`.
pub fn live_blob_key(
    custody: &SubscriptionsConfig,
    tier_name: &str,
    witness: &[u8; 32],
) -> Option<LiveBlobKey> {
    let i = tier_index(custody, tier_name)?;
    let entry = &custody.tiers[i];
    let commits_to = |p: &TierPeriod| period_key_commitment(&p.key) == *witness;
    Some(if commits_to(&entry.current) {
        LiveBlobKey::Current
    } else if entry.prior.iter().any(commits_to) {
        LiveBlobKey::RotatedOut
    } else {
        LiveBlobKey::Foreign
    })
}

/// Rotate the tier's period key on a membership removal: the outgoing `current`
/// is pushed to the front of `prior` (most-recent first) and a fresh `current`
/// is recorded with `version + 1` and a strictly-greater `rotated_at`.
///
/// `rotated_at` is `max(now_micros, prev.rotated_at + 1)` so the nest's
/// `rotated_at`-monotonicity check (`fauna.subscriptions.stale_rotation`) is
/// satisfied even across same-microsecond rotations or a non-monotonic clock.
/// Returns the new `current`; errors with [`CustodyError::TierNotFound`] if the
/// author holds no period key for the tier. `minter` as for
/// [`record_new_tier`]: a rotation mints, so the stamp moves to whoever
/// rotates — which after a succession is the successor, and is what ends the
/// post-succession rotation's owed-ness for this tier.
pub fn rotate(
    custody: &mut SubscriptionsConfig,
    minter: ActorId,
    tier_name: &str,
    new_key: [u8; 32],
    now_micros: u64,
) -> Result<TierPeriod, CustodyError> {
    let i = tier_index(custody, tier_name).ok_or(CustodyError::TierNotFound)?;
    let entry = &mut custody.tiers[i];
    let prev = entry.current.clone();
    let next = TierPeriod {
        version: prev.version + 1,
        key: new_key.into(),
        rotated_at: now_micros.max(prev.rotated_at + 1),
        minted_by: Some(minter),
    };
    entry.prior.insert(0, prev);
    entry.current = next.clone();
    Ok(next)
}

/// Re-stamp the tier's `current` period with the `rotated_at` the nest
/// actually accepted, leaving its version and key untouched. Returns whether a
/// stamp was written.
///
/// A mint loop may have to advance `rotated_at` past the value that was
/// persisted before the upload (a `stale_rotation` or `roster_mismatch`
/// retry), and the local `current` must end up carrying what the winning blob
/// carried — the invariant `orchestration::drive_removal` keeps by committing
/// the winning period. Persisted, the re-stamped period is a new row beside
/// the earlier stamp (the plane rewrites nothing in place), and the fold
/// reads the two as one period at the later stamp
/// (`SubscriptionsConfig::fold_row`, *one mint reads as one period*) — so a
/// re-stamp never costs an archival backfill a duplicate version.
///
/// **No-op when `current` is no longer `version`** (a peer device's rotation
/// won the merge in between): that period has moved to `prior`, where the
/// stamp buys nothing.
pub fn restamp_current(
    custody: &mut SubscriptionsConfig,
    tier_name: &str,
    version: u64,
    rotated_at: u64,
) -> bool {
    match tier_index(custody, tier_name) {
        Some(i) if custody.tiers[i].current.version == version => {
            custody.tiers[i].current.rotated_at = rotated_at;
            true
        }
        _ => false,
    }
}

// ── crash-recovery sentinel (subscriber-removal rotation) ───────────────────
//
// A removal rotates to a *fresh* period key that is irrecoverable once the nest
// stores the re-wrapped `KeyBlob`, so the orchestration (`crate::orchestration`)
// stages the new period as a `removal/` row BEFORE the network upload and only
// commits it into `current` once the nest confirms, settling the staging
// (`PeriodKeyStore::settle_removal`) in the same step. These pure helpers are
// the staged-removal half of that lifecycle. Identity of a staged removal is
// `(tier_name, subscriber_id, new_period.key)` — the random fresh key uniquely
// distinguishes one device's staging from another's, so a concurrent device's
// differently-keyed staging the fold unions in is never touched.

/// Whether two stagings refer to the same fresh-key removal.
fn same_staging(
    r: &PendingRemoval,
    tier_name: &str,
    subscriber_id: &ActorId,
    key: &[u8; 32],
) -> bool {
    r.tier_name == tier_name && &r.subscriber_id == subscriber_id && &r.new_period.key == key
}

/// Stage a subscriber-removal rotation sentinel, unless a staging of the same
/// fresh key is already held (then a no-op: a staged removal is one plane
/// row, keyed by its content, and is never re-stamped — the mint loop keeps
/// its advancing `rotated_at` in memory and commits the winner). A genuinely
/// new staging is appended; a concurrent device's differently-keyed staging is
/// never dropped (no-data-loss). Persist the replica after calling, BEFORE the
/// nest upload.
pub fn stage_pending_removal(custody: &mut SubscriptionsConfig, removal: PendingRemoval) {
    let key: [u8; 32] = removal.new_period.key.clone().into();
    if !custody
        .pending_removals
        .iter()
        .any(|r| same_staging(r, &removal.tier_name, &removal.subscriber_id, &key))
    {
        custody.pending_removals.push(removal);
    }
}

/// The first staged removal awaiting commit for `(tier_name, subscriber_id)`, if
/// any — the resume entry point reads this to re-drive an interrupted upload.
pub fn find_pending_removal<'a>(
    custody: &'a SubscriptionsConfig,
    tier_name: &str,
    subscriber_id: &ActorId,
) -> Option<&'a PendingRemoval> {
    fauna_core::keyed_staging::find(&custody.pending_removals, |r| {
        r.tier_name == tier_name && &r.subscriber_id == subscriber_id
    })
}

/// **Idempotently** commit a staged period into the tier's history: `period`
/// becomes `current` (the rotated-out prior current moves into `prior`), and
/// re-committing an already-applied period is a no-op. Built on
/// [`TierPeriodKeys::merge`], so it never drops a key and converges regardless
/// of how many times a crash-resumed commit replays it. Errors with
/// [`CustodyError::TierNotFound`] if the author holds no entry for the tier
/// (a removal always targets a tier the author created a key for).
pub fn commit_period(
    custody: &mut SubscriptionsConfig,
    tier_name: &str,
    period: &TierPeriod,
) -> Result<(), CustodyError> {
    let i = tier_index(custody, tier_name).ok_or(CustodyError::TierNotFound)?;
    let incoming = TierPeriodKeys {
        tier_name: tier_name.to_string(),
        current: period.clone(),
        prior: Vec::new(),
    };
    custody.tiers[i] = custody.tiers[i].merge(&incoming);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::{PendingRemoval, TierPeriod};
    use fauna_core::identity::ActorId;

    fn cfg() -> SubscriptionsConfig {
        SubscriptionsConfig::default()
    }

    const ME: ActorId = ActorId([7u8; 32]);

    #[test]
    fn record_new_tier_creates_version_1() {
        let mut c = cfg();
        let p = record_new_tier(&mut c, ME, "gold", [0x11; 32], 1000);
        assert_eq!(p.version, 1);
        assert_eq!(p.key, [0x11; 32]);
        assert_eq!(p.rotated_at, 1000);
        assert_eq!(c.tiers.len(), 1);
        assert_eq!(c.tiers[0].tier_name, "gold");
        assert!(c.tiers[0].prior.is_empty());
    }

    #[test]
    fn record_new_tier_is_idempotent_and_does_not_clobber() {
        let mut c = cfg();
        record_new_tier(&mut c, ME, "gold", [0x11; 32], 1000);
        // A second create with a different key must NOT overwrite the live key.
        let p = record_new_tier(&mut c, ME, "gold", [0x22; 32], 2000);
        assert_eq!(p.key, [0x11; 32], "existing period key preserved");
        assert_eq!(p.version, 1);
        assert_eq!(c.tiers.len(), 1);
    }

    #[test]
    fn current_period_retrieves_active_key() {
        let mut c = cfg();
        assert!(current_period(&c, "gold").is_none());
        record_new_tier(&mut c, ME, "gold", [0x11; 32], 1000);
        let p = current_period(&c, "gold").expect("held");
        assert_eq!(p.key, [0x11; 32]);
        assert!(current_period(&c, "silver").is_none());
    }

    #[test]
    /// The three answers the key witness can give, read against one custody
    /// history: the current key, a rotated-out key, a key never held — plus
    /// the tier custody never recorded.
    fn live_blob_key_classifies_a_witness_against_the_history() {
        let mut c = cfg();
        record_new_tier(&mut c, ME, "gold", [0x11; 32], 1000);
        rotate(&mut c, ME, "gold", [0x22; 32], 2000).expect("rotates");
        let commit = |k: &[u8; 32]| period_key_commitment(k);
        assert_eq!(
            live_blob_key(&c, "gold", &commit(&[0x22; 32])),
            Some(LiveBlobKey::Current)
        );
        assert_eq!(
            live_blob_key(&c, "gold", &commit(&[0x11; 32])),
            Some(LiveBlobKey::RotatedOut)
        );
        assert_eq!(
            live_blob_key(&c, "gold", &commit(&[0x77; 32])),
            Some(LiveBlobKey::Foreign)
        );
        assert_eq!(live_blob_key(&c, "silver", &commit(&[0x22; 32])), None);
        // The raw key is not its own witness: a blob must carry the
        // commitment, never the key.
        assert_eq!(
            live_blob_key(&c, "gold", &[0x22; 32]),
            Some(LiveBlobKey::Foreign)
        );
    }

    #[test]
    fn rotate_advances_version_and_retains_prior() {
        let mut c = cfg();
        record_new_tier(&mut c, ME, "gold", [0x11; 32], 1000);
        let p2 = rotate(&mut c, ME, "gold", [0x22; 32], 2000).expect("rotates");
        assert_eq!(p2.version, 2);
        assert_eq!(p2.key, [0x22; 32]);
        assert_eq!(p2.rotated_at, 2000);
        // current flipped; the v1 key is retained for archival backfill.
        assert_eq!(current_period(&c, "gold").unwrap().key, [0x22; 32]);
        let prior = &c.tiers[0].prior;
        assert_eq!(prior.len(), 1);
        assert_eq!(prior[0].version, 1);
        assert_eq!(prior[0].key, [0x11; 32]);

        // A second rotation pushes v2 to the front of prior (most-recent first).
        let p3 = rotate(&mut c, ME, "gold", [0x33; 32], 3000).expect("rotates again");
        assert_eq!(p3.version, 3);
        let prior = &c.tiers[0].prior;
        assert_eq!(prior.len(), 2);
        assert_eq!(prior[0].version, 2, "most-recent prior first");
        assert_eq!(prior[1].version, 1);
    }

    #[test]
    fn rotate_forces_strictly_monotonic_rotated_at() {
        let mut c = cfg();
        record_new_tier(&mut c, ME, "gold", [0x11; 32], 5000);
        // Clock went backwards / same micro: rotated_at must still strictly
        // advance so the nest's stale_rotation check passes.
        let p = rotate(&mut c, ME, "gold", [0x22; 32], 4000).expect("rotates");
        assert_eq!(p.rotated_at, 5001, "max(now, prev+1)");
    }

    #[test]
    fn rotate_unknown_tier_errors() {
        let mut c = cfg();
        assert_eq!(
            rotate(&mut c, ME, "ghost", [0x22; 32], 2000),
            Err(CustodyError::TierNotFound)
        );
    }

    fn sub(b: u8) -> ActorId {
        ActorId([b; 32])
    }

    #[test]
    fn commit_period_makes_it_current_and_is_idempotent() {
        let mut c = cfg();
        record_new_tier(&mut c, ME, "gold", [0x11; 32], 1000);
        let v2 = TierPeriod {
            version: 2,
            key: [0x22; 32].into(),
            rotated_at: 2000,
            minted_by: None,
        };
        commit_period(&mut c, "gold", &v2).expect("commits");
        assert_eq!(current_period(&c, "gold").unwrap().key, [0x22; 32]);
        assert_eq!(c.tiers[0].prior.len(), 1, "v1 retained");
        assert_eq!(c.tiers[0].prior[0].key, [0x11; 32]);

        // A crash-resumed commit replays the SAME period: must NOT double-rotate.
        commit_period(&mut c, "gold", &v2).expect("re-commits");
        assert_eq!(current_period(&c, "gold").unwrap().version, 2);
        assert_eq!(
            c.tiers[0].prior.len(),
            1,
            "idempotent — no extra prior entry"
        );
    }

    #[test]
    fn commit_period_unknown_tier_errors() {
        let mut c = cfg();
        let p = TierPeriod {
            version: 2,
            key: [0x22; 32].into(),
            rotated_at: 2000,
            minted_by: None,
        };
        assert_eq!(
            commit_period(&mut c, "ghost", &p),
            Err(CustodyError::TierNotFound)
        );
    }

    fn staged(tier: &str, s: u8, version: u64, key: u8, rotated_at: u64) -> PendingRemoval {
        PendingRemoval {
            tier_name: tier.into(),
            subscriber_id: sub(s),
            new_period: TierPeriod {
                version,
                key: [key; 32].into(),
                rotated_at,
                minted_by: None,
            },
        }
    }

    #[test]
    fn stage_find_pending_removal_lifecycle() {
        let mut c = cfg();
        assert!(find_pending_removal(&c, "gold", &sub(1)).is_none());

        stage_pending_removal(&mut c, staged("gold", 1, 2, 0xAA, 2000));
        let found = find_pending_removal(&c, "gold", &sub(1)).expect("staged");
        assert_eq!(found.new_period.key, [0xAA; 32]);
        assert_eq!(found.new_period.rotated_at, 2000);

        // A staging of the same fresh key is never re-stamped: one staged
        // removal is one row, and the mint loop commits the winning stamp.
        stage_pending_removal(&mut c, staged("gold", 1, 2, 0xAA, 9999));
        assert_eq!(c.pending_removals.len(), 1, "no second record");
        assert_eq!(
            find_pending_removal(&c, "gold", &sub(1))
                .unwrap()
                .new_period
                .rotated_at,
            2000
        );
    }

    #[test]
    fn stage_does_not_clobber_concurrent_differently_keyed_staging() {
        // The fold may union two devices' stagings for the SAME (tier,
        // subscriber) with DIFFERENT fresh keys — both irrecoverable. Staging
        // one must leave the other intact.
        let mut c = cfg();
        stage_pending_removal(&mut c, staged("gold", 1, 2, 0xAA, 2000));
        stage_pending_removal(&mut c, staged("gold", 1, 2, 0xBB, 2050));
        assert_eq!(c.pending_removals.len(), 2);
        assert_eq!(c.pending_removals[0].new_period.key, [0xAA; 32]);
        assert_eq!(
            c.pending_removals[1].new_period.key, [0xBB; 32],
            "the other device's key survives"
        );
    }

    /// The minter is the identity handed in, never read from the replica —
    /// a period minted after a succession names the successor.
    #[test]
    fn a_mint_stamps_the_identity_handed_in() {
        let successor = ActorId([9u8; 32]);
        let mut c = cfg();
        let v1 = record_new_tier(&mut c, ME, "gold", [0x11; 32], 1000);
        assert!(v1.was_minted_by(&ME));
        let v2 = rotate(&mut c, successor, "gold", [0x22; 32], 2000).expect("rotates");
        assert!(v2.was_minted_by(&successor));
        assert!(
            c.tiers[0].prior[0].was_minted_by(&ME),
            "history keeps its stamp"
        );
    }
}
