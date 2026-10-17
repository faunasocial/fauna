//! The signed-out door's ceremony — `fauna.account.lockout`, signed with the
//! identity seed and sent over an anonymous connection
//! (`docs/goal/behavior/devices.md` § The signed-out door; screen:
//! `docs/goal/ui/sessions.md` § Layout & flow → *The signed-out door*).
//!
//! The door exists for one case: the owner wants an account locked from a
//! device they do not want to sign in on. It therefore takes the seed for the
//! length of one call and **persists nothing** — this module has no store seam,
//! like [`crate::kit`].
//!
//! It lives here, not in `fauna-client-account`, because that crate scopes
//! itself to the bearer session kinds; this crate owns the seed-signed
//! emergency plane. The signed-in sibling is `fauna.sessions.lockout`.

use fauna_core::identity::ActorKeypair;
use fauna_protocol::account::{AccountLockoutRequest, account_lockout_signed_message};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::error::{RecoveryError, Result};
use crate::nest::RecoveryClient;

/// What the nest applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockedAccount {
    /// Unix **seconds** until which every sign-in for the identity is refused —
    /// the nest's hard-coded window past the request time.
    pub locked_until: i64,
}

/// Lock the account `identity` names, over `client`'s connection.
///
/// `client` must ride an **anonymous** connection (the whole point is that the
/// caller has no session). The request carries the client clock
/// (`fauna_protocol::client_clock`); the nest refuses one more than ±300 s from
/// its own, which surfaces as [`RecoveryError::Rejected`].
///
/// A superseded identity is refused with [`RecoveryError::Superseded`] — the
/// account already moved, and the refusal says where.
pub async fn lock_account_with_seed<R>(
    client: &RecoveryClient<R>,
    identity: &ActorKeypair,
) -> Result<LockedAccount>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let timestamp = fauna_protocol::client_clock::now_secs()
        .ok_or_else(|| RecoveryError::Crypto("the device clock cannot be read".into()))?;
    let actor_id = identity.actor_id();
    let signature = identity.sign_detached(&account_lockout_signed_message(&actor_id.0, timestamp));
    let reply = client
        .account_lockout(AccountLockoutRequest {
            actor_id: actor_id.to_hex(),
            timestamp,
            signature: fauna_core::format::hex_full(&signature),
            ..Default::default()
        })
        .await?;
    Ok(LockedAccount {
        locked_until: reply.locked_until,
    })
}
