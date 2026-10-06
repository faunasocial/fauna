//! The custody acts that are **wasm-clean**, so every app — web included —
//! drives them through one shared assembly.
//!
//! ## Why this module exists, and what it settled
//!
//! `fauna-client-custody` owns the act half for native apps, and it is
//! native-only by construction: its accept needs the T10 writer-key resolve and
//! its budget/stop writes need the R14 (account-data-plane.md § The ratified decisions) account store, neither of which has a
//! wasm twin. That left "the web leg's act path" recorded as an open design
//! question.
//!
//! Measured 2026-08-17, the question is much narrower than it looked. **Revoke
//! — piece 2's one gesture — needs nothing native.** It is
//! [`crate::rpc::CapabilitiesClient::revoke`] (this crate, generic over
//! `RpcRequester`) followed by [`crate::grant_log::record_revoke`] (this crate,
//! pure) through the succession-ledger seam (`fauna-client-config`'s
//! `SuccessionLedgerStore`, wasm-clean — every app's account-store handle
//! implements it, web's included). So web does not need a native bridge for
//! piece 2 — it needs this function.
//!
//! Which is why the assembly lives HERE rather than being written twice: the
//! ordering below is load-bearing, and a second hand-rolled copy in a wasm
//! crate is exactly where it would be got wrong.
//!
//! What stays native-only, and stays in `fauna-client-custody`: accept (binds
//! the T10 writer key), the mint (needs a conversations session to post on),
//! and set-budget/stop (write the R14 registry row). Those are gated on
//! capabilities web does not have yet, not on where the code lives.

use fauna_client_config::SuccessionLedgerStore;
use fauna_core::identity::ActorKeypair;
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::grant_log;
use crate::rpc::CapabilitiesClient;

/// Revoke a custody grant, in the one order that is safe.
///
/// **The nest's revoke happens BEFORE the signed record.** A recorded revoke the
/// nest never saw would leave the capability live — the grant log would read
/// "revoked" while the holder kept serving. This is the same order the pair
/// machine uses for content grants, and stating it once here is the point of
/// the function.
///
/// Returns `None` on success, or the error string to put on the page's
/// `error-message` element — a custody gesture is never silently dropped (e2e
/// convention 11). A grant whose ceremony is still pending has no bound
/// custodian key, and that answers with an error rather than pretending: the
/// caller's revoke control should already be disabled while the row is pending.
///
/// The caller re-folds the facet afterwards; this function does not, because the
/// two boundaries reload it through different doors.
pub async fn revoke_custody<R>(
    nest: R,
    ledger: &dyn SuccessionLedgerStore,
    secret: [u8; 32],
    grant_id: &[u8],
    holder: Option<[u8; 32]>,
) -> Option<String>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let Ok(arr) = <[u8; 16]>::try_from(grant_id) else {
        return Some("this custody's grant id is malformed".to_string());
    };
    let Some(holder) = holder else {
        return Some("this custody has no bound device to revoke against yet".to_string());
    };

    let rpc = CapabilitiesClient::new(nest.clone());
    if let Err(e) = rpc.revoke(arr).await {
        return Some(e.to_string());
    }

    // Only now is the signed Revoke event recorded.
    let keypair = ActorKeypair::from_secret(secret);
    let now_secs = fauna_core::data::Timestamp::now().0 / 1_000_000;
    let mut intent = SuccessionLedger::events_replica(keypair.actor_id(), Vec::new());
    if let Err(e) =
        grant_log::record_revoke(&mut intent, keypair.signing_key(), arr, holder, now_secs)
    {
        return Some(e.to_string());
    }
    match ledger.merge(intent).await {
        Ok(_) => None,
        Err(e) => Some(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A malformed grant id is refused before any nest call — the caller gets a
    /// string for `error-message`, not a panic and not a silent no-op.
    #[test]
    fn a_malformed_grant_id_is_refused_without_touching_the_nest() {
        // A requester that panics if used: reaching it would be the bug.
        #[derive(Clone)]
        struct NeverCalled;
        impl RpcRequester for NeverCalled {
            // `Infallible` so the `RpcErrorClass` bound is satisfied by the
            // blanket impl in fauna-protocol, same as the testkit doubles.
            type Error = core::convert::Infallible;
            async fn request<Req, Reply>(
                &self,
                _kind: &'static str,
                _payload: Req,
            ) -> Result<Reply, Self::Error>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                panic!("the nest must not be reached for a malformed grant id")
            }
        }

        let err = fauna_client_testkit::block_on(revoke_custody(
            NeverCalled,
            &fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(
                fauna_core::identity::ActorId([1u8; 32]),
            ),
            [1u8; 32],
            &[0u8; 4],
            Some([2u8; 32]),
        ));
        assert!(err.is_some(), "a malformed grant id must surface an error");
    }

    /// A pending custody (no bound custodian key) is refused the same way, and
    /// also without a nest call — revoking "nothing" must not look like success.
    #[test]
    fn a_pending_custody_is_refused_without_touching_the_nest() {
        #[derive(Clone)]
        struct NeverCalled;
        impl RpcRequester for NeverCalled {
            // `Infallible` so the `RpcErrorClass` bound is satisfied by the
            // blanket impl in fauna-protocol, same as the testkit doubles.
            type Error = core::convert::Infallible;
            async fn request<Req, Reply>(
                &self,
                _kind: &'static str,
                _payload: Req,
            ) -> Result<Reply, Self::Error>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                panic!("the nest must not be reached for a pending custody")
            }
        }

        let err = fauna_client_testkit::block_on(revoke_custody(
            NeverCalled,
            &fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(
                fauna_core::identity::ActorId([1u8; 32]),
            ),
            [1u8; 32],
            &[7u8; 16],
            None,
        ));
        assert_eq!(
            err.as_deref(),
            Some("this custody has no bound device to revoke against yet")
        );
    }
}
