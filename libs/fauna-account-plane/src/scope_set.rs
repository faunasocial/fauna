//! A replica's **content-scope set** — which content scopes this account
//! participates in, derived rather than configured.
//!
//! Owner: `docs/goal/architecture/account-sync-plane.md` § Feeds and cursors →
//! *Scope partition*. A scope is the unit of subscription and of frontier
//! tracking, and "a replica's scope set is itself account data (derived from
//! memberships, class 2) and changes as the account joins and leaves things".
//! Content scopes are "one per `(kind, scope_id)` the account participates in
//! … own-actor scopes (mail, calendar, card, own posts) plus member scopes
//! (each joined `__conv` channel)".
//!
//! The derivation splits on where the scope id comes from, and that split is
//! the whole design:
//!
//! - **The own-actor half needs nothing but the account.** Every own-actor
//!   kind's scope id *is* this actor, so the set is a pure function of the
//!   actor id — no membership source, no app knowledge, nothing to observe.
//!   `fauna_sync_engine::account_runtime::AccountStoreRuntime::start`
//!   seeds it from the actor id it already holds, so all 7 apps inherit the
//!   same four scopes with no per-app code and no app can forget one.
//! - **The member half is a membership read**, so it stays the caller's to
//!   supply: the joined `__conv` channels come from the app's live MLS
//!   session, and the set changes as the account joins and leaves. Callers
//!   pass channels to [`derive_content_scopes`] and register the difference
//!   (`fauna_sync_engine::account_runtime::AccountStoreHandle::register_content_scope`).
//!
//! **Registration is additive here, deliberately.** Leaving a channel is *T2
//! transition 3* (scope departure — the subscription ends and that scope's
//! items leave the replica, charter § The replica boundary), which is a
//! deletion path with its own rules, not the inverse of this function. This
//! module answers "what does the account participate in"; the dropping lives
//! in [`crate::departure`], which diffs this derivation against a durable
//! subscription marker and refuses to act on anything less than an affirmative
//! membership answer. The split is the point: a derivation that is merely
//! *unsure* costs a skipped walk here, and must never reach the deletion path.

use fauna_protocol::scope::{ContentScope, ScopeError};

/// The content kinds whose scope id is the account's **own actor**.
///
/// This is the derivation's own-actor *subset* of the segment-store kind table
/// (`message-segment-store.md` § Layout is the authoritative kind list) —
/// which is exactly the fact this module needs, and the reason it can be a
/// list at all: `conv` is absent because its scope id is the MLS channel, not
/// any actor, so it is a membership read rather than a derivation from the
/// account. A new own-actor kind joins the plane by landing here.
pub const OWN_ACTOR_KINDS: [&str; 4] = ["mail", "calendar", "card", "post"];

/// The scopes this account participates in purely by existing: one per
/// [`OWN_ACTOR_KINDS`] entry, scoped to `actor`.
///
/// Infallible in practice — the kind tags are compile-time constants of valid
/// shape — but the error is kept rather than unwrapped so a future kind added
/// to the list with a malformed tag is refused at its first call instead of
/// panicking inside a store thread.
pub fn derive_own_actor_scopes(actor: [u8; 32]) -> Result<Vec<ContentScope>, ScopeError> {
    OWN_ACTOR_KINDS
        .iter()
        .map(|kind| ContentScope::new(kind, actor))
        .collect()
}

/// The full content-scope set: the own-actor half plus one `conv` scope per
/// joined MLS channel.
///
/// Deterministic and duplicate-free: the result is sorted by canonical scope
/// string, so two devices deriving from the same inputs register the same set
/// in the same order, and a channel listed twice contributes one scope.
pub fn derive_content_scopes(
    actor: [u8; 32],
    joined_channels: &[[u8; 32]],
) -> Result<Vec<ContentScope>, ScopeError> {
    let mut scopes = derive_own_actor_scopes(actor)?;
    for channel in joined_channels {
        scopes.push(ContentScope::new(CONV_KIND, *channel)?);
    }
    scopes.sort_by_key(|s| s.to_string());
    scopes.dedup();
    Ok(scopes)
}

/// The member-scope kind: an MLS channel's shared conversation scope. Its
/// scope id is the channel, never an actor — the audience is the group
/// (`message-segment-store.md` § Layout).
pub const CONV_KIND: &str = "conv";

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR: [u8; 32] = [0xA1; 32];

    fn strings(scopes: &[ContentScope]) -> Vec<String> {
        scopes.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_own_actor_half_is_one_scope_per_kind_at_this_actor() {
        let scopes = derive_own_actor_scopes(ACTOR).expect("derive");
        assert_eq!(scopes.len(), OWN_ACTOR_KINDS.len());
        for (scope, kind) in scopes.iter().zip(OWN_ACTOR_KINDS) {
            assert_eq!(scope.kind(), kind);
            assert_eq!(
                scope.scope_id(),
                &ACTOR,
                "own-actor kinds scope to the actor"
            );
        }
    }

    /// `conv` is a member scope, so it must never appear from the account
    /// alone — deriving it from the actor id would name a *channel* whose id
    /// happened to equal an actor's, which is a different scope entirely.
    #[test]
    fn conv_is_never_an_own_actor_scope() {
        assert!(!OWN_ACTOR_KINDS.contains(&CONV_KIND));
        let scopes = derive_own_actor_scopes(ACTOR).expect("derive");
        assert!(scopes.iter().all(|s| s.kind() != CONV_KIND));
    }

    #[test]
    fn joined_channels_add_one_conv_scope_each() {
        let channels = [[0x11; 32], [0x22; 32]];
        let scopes = derive_content_scopes(ACTOR, &channels).expect("derive");
        assert_eq!(scopes.len(), OWN_ACTOR_KINDS.len() + channels.len());
        for channel in channels {
            let want = ContentScope::new(CONV_KIND, channel).unwrap().to_string();
            assert!(strings(&scopes).contains(&want), "{want} derived");
        }
    }

    /// Two devices of one account derive byte-identical sets in one order —
    /// the property that lets a caller diff a re-derivation against what it
    /// last registered.
    #[test]
    fn the_set_is_sorted_and_duplicate_free() {
        let dupes = [[0x22; 32], [0x11; 32], [0x22; 32]];
        let a = derive_content_scopes(ACTOR, &dupes).expect("derive");
        let b = derive_content_scopes(ACTOR, &[[0x11; 32], [0x22; 32]]).expect("derive");
        assert_eq!(strings(&a), strings(&b), "duplicates collapse");
        let mut sorted = strings(&a);
        sorted.sort();
        assert_eq!(strings(&a), sorted, "sorted by canonical string");
    }

    /// A different account derives a disjoint set — the per-account placement
    /// the store's own identity check exists to protect, at the scope layer.
    #[test]
    fn a_different_actor_derives_disjoint_scopes() {
        let mine = strings(&derive_own_actor_scopes(ACTOR).expect("derive"));
        let theirs = strings(&derive_own_actor_scopes([0xB2; 32]).expect("derive"));
        assert!(mine.iter().all(|s| !theirs.contains(s)));
    }
}
