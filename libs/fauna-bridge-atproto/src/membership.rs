//! D2/D6 — the per-lexicon membership policy for external repo writes
//! (`docs/goal/behavior/atproto-pds-full.md` § Problem 1 → D2, § Problem 4
//! → D6).
//!
//! One external write (`createRecord` / `putRecord` / `deleteRecord`, and
//! each row of an `applyWrites` batch) asks exactly one question before
//! anything else happens: **does this record's meaning already exist as a
//! Fauna concept?** [`classify_external_write`] is that question, and its
//! answer is one of three:
//!
//! - [`WriteDisposition::RoundTrip`] — reverse-translate into Fauna, let the
//!   normal one-way projection put it back in the repo (D1).
//! - [`WriteDisposition::Journal`] — no Fauna concept; store the dag-cbor
//!   verbatim in the per-user native-records journal, which the bridge
//!   unions into the MST.
//! - [`WriteDisposition::Refuse`] — an explicit, **sub-typed** XRPC error.
//!
//! # Contract
//!
//! Pure function of its inputs: no I/O, no clock, wasm-clean, in the
//! always-on core beside [`crate::authz`] and [`crate::fetch_guard`] for the
//! same reason those are — the policy is one decision point, and a pure one
//! is table-driven-testable.
//!
//! # Journal is the safe default, and that is deliberate
//!
//! D2's policy is *round-trip iff a Fauna concept already exists for the
//! record's meaning — never invent a hollow Fauna concept just to round-trip*
//! (`atproto-pds-full.md` § D2). ATProto repos are open-world: real PDSs
//! accept arbitrary structurally-valid records, so an unknown NSID journals
//! rather than erroring. Refusal is reserved for the cases where accepting
//! would contradict a Fauna invariant or silently break repo re-derivability.
//!
//! # Membership as verified against the code (2026-07-24, F2.1)
//!
//! D2 shipped a *starting* table and explicitly deferred verification to F2:
//! "F2 verifies each round-trip row against the consume-side interaction
//! vocabulary *as built* before implementing it — a row whose Fauna concept
//! turns out not to exist falls back to the journal, which is always safe."
//! That verification ran, and it moved one row:
//!
//! | Lexicon | Starting table | **As built** | Why |
//! |---|---|---|---|
//! | `app.bsky.feed.post` | round-trip | **round-trip** | `Post` is a first-class Fauna concept; the projection emits exactly this collection (`outbound.rs`) |
//! | `app.bsky.actor.profile` | round-trip | **round-trip** | `Profile` likewise, at the spec rkey `self` |
//! | `app.bsky.feed.like` / `repost`, `app.bsky.graph.follow` | round-trip *"on subjects the consume side already represents"* | **journal** | **The Fauna concept does not exist.** The consume side models these read-only: `viewer_like`/`viewer_repost`/`viewer_following` are AppView view-state AT-URIs (`types.rs`), the Bluesky kind registry holds exactly one kind — `bluesky.feed.thread`, a read (`fauna-protocol/src/kind.rs::register_bluesky_kinds`) — and the Bluesky bridge provider *hard-refuses* follows (`bins/fauna-nest/src/bluesky/bridge_provider.rs`: "Bluesky does not support bridge follows"). Round-tripping would mean inventing the hollow concept D2 forbids |
//! | everything else | journal | **journal** | unchanged |
//!
//! Nothing is lost by journaling: the record still lands in the repo, still
//! federates over the firehose, and still reads back through `getRecord`.
//! What it does not do is manufacture a Fauna-side like/follow that no Fauna
//! app could display or revoke. If Fauna later grows that vocabulary, the
//! row moves back with a test, not a redesign.

use serde::{Deserialize, Serialize};

// ── Collections the projection owns (`outbound.rs`) ──────────────────────────

/// The feed-post collection — the only post collection the projection emits.
pub const COLLECTION_POST: &str = "app.bsky.feed.post";
/// The profile collection. A mutable singleton at the spec rkey
/// [`crate::outbound::PROFILE_SELF_RKEY`], which owns that constant.
pub const COLLECTION_PROFILE: &str = "app.bsky.actor.profile";

// ── Input ────────────────────────────────────────────────────────────────────

/// Which repo verb produced this write. `putRecord` is `Update` even when the
/// rkey does not exist yet: the disposition must not depend on repo state, or
/// the same call would be refused or accepted depending on timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteAction {
    /// `com.atproto.repo.createRecord`, or an `applyWrites` `#create`.
    Create,
    /// `com.atproto.repo.putRecord`, or an `applyWrites` `#update`.
    Update,
    /// `com.atproto.repo.deleteRecord`, or an `applyWrites` `#delete`.
    Delete,
}

// ── Output ───────────────────────────────────────────────────────────────────

/// Which Fauna concept a round-tripped write reverse-translates into. Kept a
/// closed enum rather than a string so a new projection-owned collection
/// cannot be added without the consuming `match` being updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundTrip {
    /// A Fauna `Post` — created, or tombstoned on delete.
    Post,
    /// The account's Fauna `Profile` — the C2 sanctioned update path.
    Profile,
}

/// D6's three refusal flavors. The distinction is load-bearing and travels in
/// the XRPC error *name*: a later session must be able to tell "this will
/// never work" from "this is not built yet".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalSubType {
    /// Permanent — the operation contradicts a Fauna invariant.
    Policy,
    /// The capability exists, but only in Fauna apps. The message says so.
    FaunaSurface,
    /// Planned but unbuilt. The message says "not yet" — **never** harden
    /// this into policy.
    Deferred,
}

/// A sub-typed refusal, carrying the message the XRPC layer surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    pub sub_type: RefusalSubType,
    pub message: String,
}

/// What the nest does with one external write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "disposition")]
pub enum WriteDisposition {
    /// Reverse-translate into the named Fauna concept (D1).
    RoundTrip(RoundTrip),
    /// Store the record's dag-cbor verbatim in the native-records journal.
    Journal,
    /// Refuse with an explicit, sub-typed XRPC error.
    Refuse(Refusal),
}

impl WriteDisposition {
    fn refuse(sub_type: RefusalSubType, message: impl Into<String>) -> Self {
        Self::Refuse(Refusal {
            sub_type,
            message: message.into(),
        })
    }
}

/// Classify one external repo write per D2 membership + D6 refusal sub-typing.
///
/// `collection` is the record's NSID; `action` the repo verb. The result is a
/// **static** membership fact — it depends on nothing but these two inputs.
/// Runtime gates layer on top and belong to the caller, not here: notably, a
/// [`RoundTrip::Post`] still refuses *fauna-surface* at ingest time when the
/// account has no D10 authoring delegation yet (`atproto-pds-full.md` § D10
/// → *Mint ceremony*), because that is account state, not lexicon membership.
pub fn classify_external_write(collection: &str, action: WriteAction) -> WriteDisposition {
    match collection {
        // Posts are immutable, matching the network: create and delete are
        // the repo-write pair D10's `Capability::Post` authorizes (it covers
        // both the `Post` and the post-`Tombstone` envelope).
        COLLECTION_POST => match action {
            WriteAction::Create | WriteAction::Delete => {
                WriteDisposition::RoundTrip(RoundTrip::Post)
            }
            WriteAction::Update => WriteDisposition::refuse(
                RefusalSubType::Policy,
                "posts are immutable; delete the post and create a new one",
            ),
        },
        // The profile is a mutable singleton — the one collection where an
        // update is the *sanctioned* path (C2). Deleting it is refused
        // rather than journaled: the projection owns this collection, so a
        // journal tombstone for it would collide with the record the
        // projection keeps emitting and the repo would stop being
        // re-derivable from Fauna state (D1's "the repo is derived state").
        COLLECTION_PROFILE => match action {
            WriteAction::Create | WriteAction::Update => {
                WriteDisposition::RoundTrip(RoundTrip::Profile)
            }
            WriteAction::Delete => WriteDisposition::refuse(
                RefusalSubType::FaunaSurface,
                "the profile record is derived from your Fauna profile; \
                 edit or clear it in a Fauna app",
            ),
        },
        // Open-world default (D2). Every other collection — known-lexicon
        // interactions with no Fauna concept (like/repost/follow, lists,
        // threadgates, starter packs, labeler decls) and third-party NSIDs
        // alike — journals verbatim. Structural validation already ran
        // bridge-side; there is no schema gate here on purpose.
        _ => WriteDisposition::Journal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(collection: &str, action: WriteAction) -> RoundTrip {
        match classify_external_write(collection, action) {
            WriteDisposition::RoundTrip(rt) => rt,
            other => panic!("expected round-trip for {collection}/{action:?}, got {other:?}"),
        }
    }

    fn refusal(collection: &str, action: WriteAction) -> Refusal {
        match classify_external_write(collection, action) {
            WriteDisposition::Refuse(r) => r,
            other => panic!("expected refusal for {collection}/{action:?}, got {other:?}"),
        }
    }

    #[test]
    fn a_post_create_and_delete_round_trip_but_an_update_is_policy_refused() {
        assert_eq!(
            round_trip(COLLECTION_POST, WriteAction::Create),
            RoundTrip::Post
        );
        // Delete round-trips too — it becomes a Fauna tombstone, which the
        // same D10 `Capability::Post` authorizes signing.
        assert_eq!(
            round_trip(COLLECTION_POST, WriteAction::Delete),
            RoundTrip::Post
        );

        let r = refusal(COLLECTION_POST, WriteAction::Update);
        assert_eq!(r.sub_type, RefusalSubType::Policy);
        assert!(
            r.message.contains("immutable"),
            "the refusal must say why, got: {}",
            r.message
        );
    }

    #[test]
    fn the_profile_singleton_accepts_an_update_and_refuses_a_delete_to_fauna() {
        assert_eq!(
            round_trip(COLLECTION_PROFILE, WriteAction::Create),
            RoundTrip::Profile
        );
        assert_eq!(
            round_trip(COLLECTION_PROFILE, WriteAction::Update),
            RoundTrip::Profile
        );

        // Fauna-surface, NOT policy: the capability exists, it just lives in
        // a Fauna app. Sub-typing this wrong would tell a future session
        // the operation is permanently impossible.
        let r = refusal(COLLECTION_PROFILE, WriteAction::Delete);
        assert_eq!(r.sub_type, RefusalSubType::FaunaSurface);
        assert!(
            r.message.contains("Fauna app"),
            "a fauna-surface refusal must say where to go, got: {}",
            r.message
        );
    }

    /// The D2 row this slice moved. Verified against the consume side as
    /// built: no Fauna like/repost/follow vocabulary exists, so round-tripping
    /// would invent the hollow concept D2 forbids.
    #[test]
    fn interactions_with_no_fauna_concept_journal_rather_than_round_trip() {
        for collection in [
            "app.bsky.feed.like",
            "app.bsky.feed.repost",
            "app.bsky.graph.follow",
            "app.bsky.graph.block",
        ] {
            for action in [
                WriteAction::Create,
                WriteAction::Update,
                WriteAction::Delete,
            ] {
                assert_eq!(
                    classify_external_write(collection, action),
                    WriteDisposition::Journal,
                    "{collection}/{action:?} must journal — Fauna has no such concept"
                );
            }
        }
    }

    #[test]
    fn known_unmappable_lexicons_and_unknown_nsids_both_journal() {
        for collection in [
            "app.bsky.graph.list",
            "app.bsky.graph.listitem",
            "app.bsky.feed.threadgate",
            "app.bsky.feed.postgate",
            "app.bsky.graph.starterpack",
            "app.bsky.labeler.service",
            // Open-world: a third-party lexicon is accepted, not errored.
            "com.example.someones.customRecord",
            "",
        ] {
            assert_eq!(
                classify_external_write(collection, WriteAction::Create),
                WriteDisposition::Journal,
                "{collection} must journal"
            );
        }
    }

    /// A near-miss NSID must not be mistaken for the collection it resembles;
    /// matching is exact, never prefix- or substring-based.
    #[test]
    fn collection_matching_is_exact() {
        for near_miss in [
            "app.bsky.feed.post.extra",
            "app.bsky.feed.posts",
            "xapp.bsky.feed.post",
            "APP.BSKY.FEED.POST",
            " app.bsky.feed.post",
        ] {
            assert_eq!(
                classify_external_write(near_miss, WriteAction::Create),
                WriteDisposition::Journal,
                "{near_miss} must not be treated as the post collection"
            );
        }
    }

    /// The disposition is a pure function of (collection, action) — pinning
    /// this keeps repo state from leaking into the decision, which is what
    /// makes `putRecord` classify identically whether or not the rkey exists.
    #[test]
    fn classification_is_stable_across_repeated_calls() {
        for _ in 0..3 {
            assert_eq!(
                classify_external_write(COLLECTION_POST, WriteAction::Create),
                WriteDisposition::RoundTrip(RoundTrip::Post)
            );
        }
    }
}
