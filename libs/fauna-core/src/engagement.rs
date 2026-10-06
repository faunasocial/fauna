//! Dedup identities for **explicit-act** engagement, plus the nest-trust anomaly flags.
//!
//! Scope note (frame D9, 2026-07-12): this module used to also carry the
//! *implicit* behavioral wire vocabulary (`EngagementType`'s impression / view /
//! scroll variants, `EngagementEvent`, `ShareTarget`, the 10-second-bucketed
//! `compute_event_id`, `EngagementAuditReport`) for the pre-frame
//! `fauna.engagement.{record,list}` path. That path was client-dead fleet-wide
//! since inception and is **retired** — implicit cues never cross a user
//! boundary as raw events; they rest as a sealed rollup on the user's own nest
//! and contribute only as opt-in, k-anonymized aggregates.
//! Mechanism: `docs/goal/behavior/engagement-cues.md`.
//!
//! What remains is the **explicit-act** machinery, which is untouched by that
//! retirement: the two dedup-id functions behind the public per-post counters.

use serde::{Deserialize, Serialize};

use crate::data::ContentHash;
use crate::identity::ActorId;

/// Compute a **stable** (time-independent) dedup id by hashing (actor, content, tag).
///
/// This is the key for *reversible toggle* state (a like) where the nest must
/// answer "does this actor currently hold this action?" across all time:
/// `fauna.posts.interact` `like` inserts the toggle event (first-insert ⇒ bump
/// the counter), `unlike` deletes that same event (removed ⇒ decrement).
/// Because the id omits the timestamp, re-liking is idempotent (same id ⇒
/// `INSERT OR IGNORE` no-ops) and unlike-then-relike round-trips to one row.
pub fn compute_toggle_event_id(
    actor: &ActorId,
    content_id: &ContentHash,
    event_type_tag: &str,
) -> ContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"fauna-toggle-event-v1");
    hasher.update(&actor.0);
    hasher.update(&content_id.digest());
    hasher.update(event_type_tag.as_bytes());
    ContentHash::from_digest_raw(*hasher.finalize().as_bytes())
}

/// Stable dedup id for a *reference-driven* engagement — a reply / repost /
/// quote post `referencing` contributing one count to its `target`.
///
/// Keyed by the **referencing post id** (globally unique + content-addressed) +
/// target + tag, NOT by the actor: distinct posts by the same actor each count
/// (three replies ⇒ `reply_count == 3`), yet a byte-identical re-create of one
/// post — which content-addressed post-create dedups — never double-counts (same
/// referencing id ⇒ same event id ⇒ `INSERT OR IGNORE` no-ops). The post-create
/// analogue of [`compute_toggle_event_id`] (which keys by actor for the
/// like/unlike toggle, where re-liking must collapse to one).
pub fn compute_reference_event_id(
    referencing_post: &ContentHash,
    target: &ContentHash,
    event_type_tag: &str,
) -> ContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"fauna-reference-event-v1");
    hasher.update(&referencing_post.digest());
    hasher.update(&target.digest());
    hasher.update(event_type_tag.as_bytes());
    ContentHash::from_digest_raw(*hasher.finalize().as_bytes())
}

/// Heuristic anomaly markers a nest raises about a *peer* nest's reported
/// engagement volume/timing (see `db/nest_trust.rs`). Unrelated to the retired
/// per-actor behavioral path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnomalyFlag {
    UniformTiming,
    SingleNestConcentration,
    FreshAccountCluster,
    FailedChallenges,
    VolumeAnomaly,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both ids are *dedup identities* persisted in `engagement_events`: a silent
    // change to either hash would orphan every existing row (re-liking would
    // double-count, a re-created post would re-increment). Pin the domain
    // separation and the keying, so a refactor cannot quietly redefine them.

    #[test]
    fn toggle_event_id_is_stable_and_actor_keyed() {
        let actor = ActorId([5u8; 32]);
        let other = ActorId([9u8; 32]);
        let content = ContentHash::from_digest_raw([6u8; 32]);

        // Time-independent + idempotent: the same (actor, content, tag) always
        // collapses to one identity, which is what makes re-liking a no-op.
        assert_eq!(
            compute_toggle_event_id(&actor, &content, "like"),
            compute_toggle_event_id(&actor, &content, "like"),
        );
        // Distinct actors and distinct tags are distinct events.
        assert_ne!(
            compute_toggle_event_id(&actor, &content, "like"),
            compute_toggle_event_id(&other, &content, "like"),
        );
        assert_ne!(
            compute_toggle_event_id(&actor, &content, "like"),
            compute_toggle_event_id(&actor, &content, "repost"),
        );
    }

    #[test]
    fn reference_event_id_is_keyed_by_referencing_post_not_actor() {
        let target = ContentHash::from_digest_raw([1u8; 32]);
        let reply_a = ContentHash::from_digest_raw([2u8; 32]);
        let reply_b = ContentHash::from_digest_raw([3u8; 32]);

        // Distinct posts each count (three replies ⇒ reply_count == 3) …
        assert_ne!(
            compute_reference_event_id(&reply_a, &target, "reply"),
            compute_reference_event_id(&reply_b, &target, "reply"),
        );
        // … while a byte-identical re-create collapses to one.
        assert_eq!(
            compute_reference_event_id(&reply_a, &target, "reply"),
            compute_reference_event_id(&reply_a, &target, "reply"),
        );
    }

    #[test]
    fn toggle_and_reference_ids_are_domain_separated() {
        // Same trailing inputs must not collide across the two schemes — the
        // domain-separation prefixes are load-bearing, not decoration.
        let h = ContentHash::from_digest_raw([7u8; 32]);
        let actor = ActorId([7u8; 32]);
        assert_ne!(
            compute_toggle_event_id(&actor, &h, "like"),
            compute_reference_event_id(&h, &h, "like"),
        );
    }
}
