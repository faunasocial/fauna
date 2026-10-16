//! e2e-only HOLDS on the runtime's two network legs of the fleet plane — its
//! own publish and its walk — so a journey can stand a replica in the window
//! a timing race would otherwise open only sometimes
//! (`docs/goal/behavior/mail-credentials.md` § Hard revoke → *Published
//! before the nest moves*, the witness paragraphs).
//!
//! Each hold keeps back only the runtime's UNPROMPTED leg — the local-write
//! wake's publish and a pass's, a nudge's walk and a pass's — and never the
//! AWAITED one a caller asks for and waits on ([`AccountStoreHandle::
//! publish_mail`]'s publish, [`AccountStoreHandle::walk_fleet`]'s walk): the
//! awaited legs are what the rules under test rest on, so a hold can only make
//! a tree that leans on the unprompted leg's latency go red, never a correct
//! one. One variable per leg, each read at this one door
//! (`e2e-automation-surface-gating.md` § The nest-leg trust seed, term 3):
//!
//! - `FAUNA_E2E_HOLD_PUBLISH_KIND=<kind>` — while this device owes the nest
//!   an own fleet row of `<kind>` (`fauna.state.mail`), an unprompted publish
//!   of the fleet plane publishes nothing; the plane is one ordered log, so
//!   the rows behind it wait with it. The window closes at the next awaited
//!   publish, which ships the whole log.
//! - `FAUNA_E2E_HOLD_FLEET_WALK=1` — once this process has listed the fleet
//!   scope (its launch's first listing is never held — the read gate waits on
//!   it), an unprompted walk of the fleet scope walks nothing, so the replica
//!   stays at what it listed until an awaited walk.
//!
//! **Gated** (convention 15 rule (a), the `pass.rs::stated_row` shape):
//! compile-gated outer, env inner — a release build has only the no-op twins
//! and never reads either variable, and an unset or empty value holds
//! nothing, so a missing hold is production-safe.
//!
//! [`AccountStoreHandle::publish_mail`]: super::AccountStoreHandle::publish_mail
//! [`AccountStoreHandle::walk_fleet`]: super::AccountStoreHandle::walk_fleet

use fauna_account_store::backend::StoreBackend;
use fauna_protocol::RpcRequester;

use crate::account_state_plane::AccountStatePlane;

/// Whether a leg of the fleet plane runs because a caller asked for it and
/// waits on it, or on the runtime's own cadence. Only an unprompted leg is
/// ever held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prompt {
    /// The local-write wake, a pass (prologue, ticker, reconnect, an
    /// ordinary `reconcile_now`), a nudge.
    Unprompted,
    /// `publish_mail` / `publish_ledger`'s publish; `walk_fleet`'s pass.
    Awaited,
}

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
const PUBLISH_HOLD_ENV: &str = "FAUNA_E2E_HOLD_PUBLISH_KIND";
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
const WALK_HOLD_ENV: &str = "FAUNA_E2E_HOLD_FLEET_WALK";

/// Does the publish hold keep this unprompted publish of the fleet plane
/// back? Only while the plane owes an own row of the held kind; a failed read
/// of the owed set holds nothing (the publish then runs as it would).
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub(crate) async fn publish_held<B, R>(fleet: &AccountStatePlane<'_, B, R>, prompt: Prompt) -> bool
where
    B: StoreBackend,
    R: RpcRequester,
{
    if prompt == Prompt::Awaited {
        return false;
    }
    let Ok(kind) = std::env::var(PUBLISH_HOLD_ENV) else {
        return false;
    };
    if kind.is_empty() {
        return false;
    }
    let held = fleet
        .owes_own_state(|row_kind, _| row_kind == kind)
        .await
        .unwrap_or(false);
    if held {
        tracing::info!("account runtime: e2e hold — unprompted fleet publish held ({kind} owed)");
    }
    held
}

#[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
pub(crate) async fn publish_held<B, R>(
    _fleet: &AccountStatePlane<'_, B, R>,
    _prompt: Prompt,
) -> bool
where
    B: StoreBackend,
    R: RpcRequester,
{
    false
}

/// Does the walk hold keep this unprompted walk of the fleet scope back? Only
/// once this process has listed the scope (the runtime's [`FirstListings`]),
/// so the launch's first listing — the one every first read waits on —
/// always lands.
///
/// [`FirstListings`]: crate::account_state_plane::FirstListings
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub(crate) fn fleet_walk_held<B, R>(fleet: &AccountStatePlane<'_, B, R>, prompt: Prompt) -> bool
where
    B: StoreBackend,
    R: RpcRequester,
{
    if prompt == Prompt::Awaited {
        return false;
    }
    if !std::env::var(WALK_HOLD_ENV).is_ok_and(|v| !v.is_empty()) {
        return false;
    }
    let held = fleet
        .first_listings()
        .is_some_and(|f| f.listed(fleet.scope()));
    if held {
        tracing::info!("account runtime: e2e hold — unprompted fleet walk held");
    }
    held
}

#[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
pub(crate) fn fleet_walk_held<B, R>(_fleet: &AccountStatePlane<'_, B, R>, _prompt: Prompt) -> bool
where
    B: StoreBackend,
    R: RpcRequester,
{
    false
}
