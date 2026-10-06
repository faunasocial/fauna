//! Owner-side WS-RPC seam for the `fauna.capabilities.*` grant methods — the
//! thin transport wrapper the Nests-page view-model calls to deposit, renew,
//! and revoke client-minted capability grants.
//!
//! **Owner-callable subset only** (`mint` / `renew` / `revoke` / `reconcile`,
//! each gated to `class == User`, the content-owning user —
//! `bridge_method_allowlist.rs:1324`).
//! `fauna.capabilities.fetch` is deliberately absent: it is **holder**-scoped
//! (`BridgeMda | ContentProcessor`, self-pubkey-scoped — `:1327`) and lives in
//! the Go bridge (`bins/fauna-bridges/internal/capability/registry.go`,
//! `wsrpc/methods.go:152`). An owner can never call it, and the owner's
//! audit/History view is client-authoritative grant-event-log state
//! ([`crate::grant_log`]), **not** a nest read (design § No modes `:302`).
//!
//! **The one owner-side nest read, and why it is not the banned one.** The
//! standing gotcha here used to read *"do NOT add an owner-facing list-my-grants
//! RPC"*. [`CapabilitiesClient::reconcile`] is not that RPC, and the distinction
//! is trust polarity rather than shape (`ui/nests.md` § Trust facet — grants →
//! *Reconcile*, ratified 2026-08-15, which owns every constraint): a
//! list-my-grants read would **inform** the audit view, where believing a lying
//! nest is fatal. Reconcile informs nothing — it returns ids and nothing else,
//! the client acts only on *absence from its own log*, and the sole action the
//! answer can trigger is a revoke aimed back at the answering nest, i.e. the
//! surrender of that nest's own holdings. So the gotcha still stands in full for
//! any read that would feed the Now or History lens; the reply here is
//! structurally incapable of feeding either.
//!
//! **Pattern:** the wasm-clean generic `BridgesClient` seam
//! (`libs/fauna-client-bridges`) — `struct .. <R: RpcRequester>`, one `async fn`
//! per kind delegating to `self.nest.request(kind, typed_req)`, no state
//! machine, no concrete transport (native `Arc<NestClient>` / wasm `WsRpcClient`
//! injected by the caller via the `Arc<T>` blanket impl). The
//! `GrantBlob`/`WrappedScopeKey` -> canonical-bytes encoding stays with the
//! caller (next to its `WrapError`), so this wrapper is pure transport and
//! returns `R::Error` unwrapped — exactly as `MailAdminClient::provision_*`
//! takes pre-sealed `Vec<u8>` bytes.
//!
//! Design authority: the capability-mediated content-processing design
//! (tracked internally), § 2.3 (the `fauna.capabilities.*` RPCs — mint /
//! fetch / renew / revoke + owner-vs-bridge gating) and § 2.6 ("the nest
//! handlers + `is_permitted` arms"). Wire types: `fauna_protocol::wrapped_blob`
//! (`wrapped_blob.rs:1082`+).

use fauna_protocol::ByteBuf;
use fauna_protocol::RpcRequester;
use fauna_protocol::wrapped_blob::{
    MintGrantReply, MintGrantRequest, ReconcileGrantsReply, ReconcileGrantsRequest,
    RenewGrantReply, RenewGrantRequest, RevokeGrantReply, RevokeGrantRequest,
};

/// A thin owner-side wrapper over the `fauna.capabilities.{mint,renew,revoke}`
/// WS-RPC kinds, generic over the `R: RpcRequester` transport — the same seam
/// `BridgesClient` uses, so it stays wasm-clean (no `fauna-client` /
/// `fauna-rpc-wasm` dependency): native call sites pass `Arc<NestClient>`, the
/// wasm SPA passes `WsRpcClient`, both satisfying `R: RpcRequester`.
pub struct CapabilitiesClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> CapabilitiesClient<R> {
    /// Wrap a transport that can talk to the owner's nest.
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.capabilities.mint` — deposit a client-built, HPKE-sealed
    /// `GrantBlob` (canonically encoded via `GrantBlob::to_canonical_bytes`;
    /// the caller mints it with [`crate::mint_grant`]). The nest stores it
    /// keyed `(owner, grant_id)` and never opens it; the reply echoes the
    /// stored 16-byte `grant_id` (the handle a later `renew`/`revoke` names).
    pub async fn mint(&self, grant_blob: Vec<u8>) -> Result<MintGrantReply, R::Error> {
        self.nest
            .request(
                "fauna.capabilities.mint",
                MintGrantRequest {
                    grant_blob: ByteBuf::from(grant_blob),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.capabilities.renew` — slide a grant's window to
    /// `[new_epoch_start, new_epoch_end]` (`grant_log::renewal_window`; the
    /// nest prunes the per-epoch wraps below the new start) and, for an
    /// epoch-sealed kind, append the next window's `WrappedScopeKey`s (each
    /// canonically encoded via `WrappedScopeKey::to_canonical_bytes` by the
    /// caller). A master-key grant renews as a window move only, so
    /// `appended_keys` is empty.
    pub async fn renew(
        &self,
        grant_id: [u8; 16],
        new_epoch_start: u64,
        new_epoch_end: u64,
        appended_keys: Vec<Vec<u8>>,
    ) -> Result<RenewGrantReply, R::Error> {
        self.nest
            .request(
                "fauna.capabilities.renew",
                RenewGrantRequest {
                    grant_id: ByteBuf::from(grant_id.to_vec()),
                    new_epoch_start: Some(new_epoch_start),
                    new_epoch_end,
                    appended_keys: appended_keys.into_iter().map(ByteBuf::from).collect(),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.capabilities.revoke` — delete the `(owner, grant_id)` row; the
    /// holder's next `fetch` returns nothing and it goes dark (honest box —
    /// design § 2.3). Revocation stops **re-acquisition** and future content on
    /// an honest box; it does **not** recall a key a dishonest holder already
    /// unwrapped (the honest bound the Nests-page copy must state — § Phase 2
    /// Step 1 (e)).
    pub async fn revoke(&self, grant_id: [u8; 16]) -> Result<RevokeGrantReply, R::Error> {
        self.nest
            .request(
                "fauna.capabilities.revoke",
                RevokeGrantRequest {
                    grant_id: ByteBuf::from(grant_id.to_vec()),
                    extra: Default::default(),
                },
            )
            .await
    }

    /// `fauna.capabilities.reconcile` — enumerate every `grant_id` this owner
    /// holds on this nest (all rows, expired included; owner-scoped from the
    /// authenticated caller, so the request carries no fields).
    ///
    /// The reply's **only** admissible consumer is the revoke-the-unrecognized
    /// sweep ([`crate::grant_log::unrecognized_grant_ids`] →
    /// `fauna.capabilities.revoke` back to this same nest). Nothing here may
    /// reach the Now or History lens: see the module docs for why that is a
    /// polarity rule and not a shape rule, and `ui/nests.md` § Trust facet —
    /// grants → *Reconcile* for the constraints this call is admissible under.
    ///
    /// Ill-formed ids (anything not exactly 16 bytes) are dropped here rather
    /// than surfaced: the caller's next step is a revoke keyed by a `[u8; 16]`,
    /// so an id that cannot name a row cannot name a revoke either, and a
    /// hostile nest padding the reply with malformed entries must not be able
    /// to fail an honest owner's whole sweep.
    pub async fn reconcile(&self) -> Result<Vec<[u8; 16]>, R::Error> {
        let reply: ReconcileGrantsReply = self
            .nest
            .request(
                "fauna.capabilities.reconcile",
                ReconcileGrantsRequest {
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply
            .grant_ids
            .into_iter()
            .filter_map(|b| <[u8; 16]>::try_from(b.as_ref()).ok())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            "fauna.capabilities.mint" => fauna_protocol::encode_canonical(&MintGrantReply {
                grant_id: ByteBuf::from(vec![0x11u8; 16]),
                ok: true,
                extra: Default::default(),
            }),
            "fauna.capabilities.renew" => fauna_protocol::encode_canonical(&RenewGrantReply {
                ok: true,
                extra: Default::default(),
            }),
            "fauna.capabilities.revoke" => fauna_protocol::encode_canonical(&RevokeGrantReply {
                ok: true,
                extra: Default::default(),
            }),
            other => panic!("unexpected kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn constructor_builds_over_generic_requester() {
        // The wasm-clean construction surface compiles without pulling a
        // concrete transport.
        let _c = CapabilitiesClient::new(MockRequester);
    }

    #[test]
    fn mint_composes_kind_and_grant_blob() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = CapabilitiesClient::new(rec.clone());
        let blob = vec![0xABu8; 64];

        let reply = block_on(client.mint(blob.clone())).expect("infallible mock");
        assert_eq!(reply.grant_id.as_ref(), &[0x11u8; 16], "grant_id echoed");
        assert!(reply.ok);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.capabilities.mint");
        let req: MintGrantRequest =
            fauna_protocol::decode_strict(&payload).expect("payload decodes as MintGrantRequest");
        assert_eq!(req.grant_blob.as_ref(), blob.as_slice());
    }

    #[test]
    fn renew_composes_kind_grant_id_and_appended_keys() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = CapabilitiesClient::new(rec.clone());
        let grant_id = [0x07u8; 16];
        let keys = vec![vec![0x01u8; 8], vec![0x02u8; 8]];

        let reply = block_on(client.renew(grant_id, 1_800_000_000, 1_900_000_000, keys.clone()))
            .expect("infallible mock");
        assert!(reply.ok);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.capabilities.renew");
        let req: RenewGrantRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.grant_id.as_ref(), &grant_id);
        assert_eq!(req.new_epoch_start, Some(1_800_000_000));
        assert_eq!(req.new_epoch_end, 1_900_000_000);
        assert_eq!(req.appended_keys.len(), 2);
        assert_eq!(req.appended_keys[0].as_ref(), keys[0].as_slice());
        assert_eq!(req.appended_keys[1].as_ref(), keys[1].as_slice());
    }

    #[test]
    fn renew_with_no_appended_keys_is_a_master_key_window_bump() {
        // Master-key regime today: `appended_keys` empty (design § 2.1).
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = CapabilitiesClient::new(rec.clone());
        let grant_id = [0x07u8; 16];

        block_on(client.renew(grant_id, 40, 42, vec![])).expect("infallible mock");

        let (_, payload) = rec.recorded();
        let req: RenewGrantRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(
            req.appended_keys.is_empty(),
            "master-key = window bump only"
        );
        assert_eq!(req.new_epoch_end, 42);
    }

    #[test]
    fn revoke_composes_kind_and_grant_id() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = CapabilitiesClient::new(rec.clone());
        let grant_id = [0x09u8; 16];

        let reply = block_on(client.revoke(grant_id)).expect("infallible mock");
        assert!(reply.ok);

        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.capabilities.revoke");
        let req: RevokeGrantRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.grant_id.as_ref(), &grant_id);
    }
}
