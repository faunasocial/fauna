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
//! **The mint is the second such act (2026-11-11).** It needs a conversations
//! channel to post on, which web's rail holds (the same `FaunaMlsBackend`), and
//! its write is the ceremony-records seam every app's account store implements
//! — so [`mint_custody`] and its candidate check live here too, and the native
//! `Mint` arm calls them.
//!
//! What stays native-only, and stays in `fauna-client-custody`: accept (binds
//! the T10 writer key) and set-budget/stop (write the R14 registry row). Those
//! are gated on capabilities web does not have yet, not on where the code lives.

use fauna_client_config::SuccessionLedgerStore;
use fauna_core::data::Timestamp;
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::custody_ceremony::{self, CeremonyRecords};
use crate::custody_view::CustodyMintCandidateView;
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

/// The refusal a mint with no conversations channel answers — no session on a
/// native app, no MLS rail on web. An offer recorded with nothing to post it on
/// would never be sent and never reported (e2e convention 11).
pub const MINT_NO_CHANNEL: &str =
    "no conversation channel to send the request over — start a conversation first";

/// The refusal a mint whose `(host, channel)` pair the candidate list never
/// offered answers.
pub const MINT_NOT_A_CANDIDATE: &str =
    "that person is not an eligible custodian — refresh and pick again";

/// Does `(host, channel_hex)` name a real row of `candidates`? The enforcement
/// the mint's boundary documents: `host`/`channel_hex`
/// arrive as free parameters, so nothing else stops a leg from hand-assembling a
/// pair the candidate fold (`fauna_client_conversations::
/// mint_candidates_from_channels`) never offered. Lives here, wasm-clean, so
/// the native mint and web's run the same check.
///
/// This one check also carries the candidate rules' self-exclusion for free:
/// the fold never lists the owner's own actor as a host, so a `host == own
/// actor` pair is already "not a candidate" here, with no second check needed.
pub fn host_channel_is_a_mint_candidate(
    candidates: &[CustodyMintCandidateView],
    host: &[u8; 32],
    channel_hex: &str,
) -> bool {
    candidates
        .iter()
        .any(|c| c.host == host.as_slice() && c.channel_hex == channel_hex)
}

/// Offer initiation (`custody-mint-confirm-button`) — the owner's side of the
/// ceremony: record the offer on a fresh ceremony record (Account scope, the
/// default window) through the ceremony-records seam. The caller then runs one
/// drive pass, which posts it over the chosen conversation's channel.
///
/// `candidates` is the list the page offered, read fresh from the live rail;
/// `None` = there is no rail to post on at all ([`MINT_NO_CHANNEL`]). A pair not
/// in the list is refused ([`MINT_NOT_A_CANDIDATE`]). `owner_devices` is the
/// offer's discovery seed (this device's principal when the app can resolve it,
/// else empty — the seed narrows discovery, it is not a precondition).
///
/// Returns `None` once the offer is recorded, or the error string for the
/// page's `error-message` — never a silent drop (e2e convention 11). Shared by
/// `fauna_client_custody::run_custody_act`'s `Mint` arm and web's rail, so the
/// two refusals and the offer's shape are stated once (`ui/devices.md` § Where
/// logic lives).
#[allow(clippy::too_many_arguments)]
pub async fn mint_custody(
    records: &impl CeremonyRecords,
    owner: &ActorKeypair,
    candidates: Option<&[CustodyMintCandidateView]>,
    host: ActorId,
    channel_hex: String,
    owner_devices: Vec<DeviceEndpoints>,
    owner_nest_url: String,
    now: Timestamp,
) -> Option<String> {
    let Some(candidates) = candidates else {
        return Some(MINT_NO_CHANNEL.to_string());
    };
    if !host_channel_is_a_mint_candidate(candidates, &host.0, &channel_hex) {
        return Some(MINT_NOT_A_CANDIDATE.to_string());
    }
    let params = custody_ceremony::OfferParams {
        host,
        channel_hex,
        scopes: fauna_core::custody_grant::CustodyScopeSet::Account,
        duration_secs: crate::DEFAULT_GRANT_WINDOW_SECS,
        owner_devices,
        owner_nest_url: Some(owner_nest_url),
        grant_id: fauna_core::custody_grant::random_grant_id(),
    };
    match records
        .update(|cfg| custody_ceremony::begin_offer(cfg, owner, params, now))
        .await
    {
        Ok(Ok(_)) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(e) => Some(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(host: u8, channel: &str) -> CustodyMintCandidateView {
        CustodyMintCandidateView {
            host: vec![host; 32],
            channel_hex: channel.to_string(),
            label: "Bo".to_string(),
        }
    }

    #[test]
    fn a_listed_candidates_own_pair_is_a_candidate() {
        assert!(host_channel_is_a_mint_candidate(
            &[candidate(7, "c0ffee")],
            &[7u8; 32],
            "c0ffee"
        ));
    }

    #[test]
    fn a_pair_with_the_right_channel_but_the_wrong_host_is_not_a_candidate() {
        assert!(!host_channel_is_a_mint_candidate(
            &[candidate(7, "c0ffee")],
            &[9u8; 32],
            "c0ffee"
        ));
    }

    #[test]
    fn a_pair_with_the_right_host_but_the_wrong_channel_is_not_a_candidate() {
        assert!(!host_channel_is_a_mint_candidate(
            &[candidate(7, "c0ffee")],
            &[7u8; 32],
            "deadbeef"
        ));
    }

    #[test]
    fn an_empty_candidate_list_refuses_every_pair() {
        assert!(!host_channel_is_a_mint_candidate(&[], &[7u8; 32], "c0ffee"));
    }

    fn mint(
        store: &fauna_client_config::test_helpers::FakeCustodyCeremonyStore,
        candidates: Option<&[CustodyMintCandidateView]>,
        host: u8,
        channel: &str,
    ) -> Option<String> {
        fauna_client_testkit::block_on(mint_custody(
            store,
            &ActorKeypair::from_secret([1u8; 32]),
            candidates,
            ActorId([host; 32]),
            channel.to_string(),
            Vec::new(),
            "https://owner.example".to_string(),
            Timestamp::now(),
        ))
    }

    /// No rail to post on → the named refusal, and nothing recorded: an offer
    /// that could never be sent must not sit in the records looking pending.
    #[test]
    fn a_mint_with_no_channel_is_refused_and_records_nothing() {
        let store = fauna_client_config::test_helpers::FakeCustodyCeremonyStore::empty();
        assert_eq!(
            mint(&store, None, 2, "aa").as_deref(),
            Some(MINT_NO_CHANNEL)
        );
        assert!(store.current().granted.is_empty());
    }

    /// A pair the candidate list never offered is refused before any write.
    #[test]
    fn a_mint_to_a_pair_never_offered_is_refused_and_records_nothing() {
        let store = fauna_client_config::test_helpers::FakeCustodyCeremonyStore::empty();
        let offered = [candidate(2, "aa")];
        assert_eq!(
            mint(&store, Some(&offered), 3, "aa").as_deref(),
            Some(MINT_NOT_A_CANDIDATE)
        );
        assert!(store.current().granted.is_empty());
    }

    /// An offered pair records one pending offer addressed to that host, on
    /// that channel — what the next drive pass posts.
    #[test]
    fn a_mint_to_an_offered_pair_records_one_offer() {
        let store = fauna_client_config::test_helpers::FakeCustodyCeremonyStore::empty();
        let offered = [candidate(2, "aa")];
        assert_eq!(mint(&store, Some(&offered), 2, "aa"), None);
        let state = store.current();
        assert_eq!(state.granted.len(), 1, "got {state:?}");
        assert_eq!(state.granted[0].host, [2u8; 32]);
        assert_eq!(state.granted[0].channel_hex, "aa");
    }

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
