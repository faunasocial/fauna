//! Typed-call wrapper for the user-facing connection-management WS-RPC kinds
//! — knocks (`fauna.knocks.{list,accept,block,unblock,dismiss}`), the contact
//! roster (`fauna.contacts.{list,confirm}`), and the inbox-acceptance policy
//! (`fauna.inbox.mode.{get,set}`). Part of the WS-RPC-everywhere migration
//! (tracked internally; `knocks.unblock` added in a follow-up slice).
//!
//! Pattern: same shape as `fauna-client-feed` / `fauna-client-notifications`
//! — a thin `pub struct ContactsClient<R: RpcRequester> { nest: R }`, one
//! async method per kind, no state machine, generic over the WS-RPC transport
//! so the kind-composition logic is written once and shared across native +
//! wasm (priority #2). This crate is the transport surface only; the
//! knocks-inbox + contacts-list view-models live in the client's shared state
//! layer. The three top namespaces are one connection-management feature, so
//! the methods are grouped by namespace (`knocks_*` / `contacts_*` /
//! `inbox_mode_*`) on a single client.

use fauna_protocol::RpcRequester;
use fauna_protocol::contacts::{
    ContactConfirmReply, ContactListReply, ContactListRequest, ContactStatusReply,
    InboxModeGetReply, InboxModeGetRequest, InboxModeSetReply, InboxModeSetRequest,
    KnockActionReply, KnockActionRequest, KnockListReply, KnockListRequest,
};

pub use fauna_protocol::contacts;

/// Typed connection-management call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the wasm SPA
/// passes its `WsRpcClient`. Errors propagate as the transport's `R::Error`
/// (native `NestClientError`, wasm rpc-wasm error).
pub struct ContactsClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> ContactsClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    // ── knocks ──────────────────────────────────────────────────

    /// `fauna.knocks.list` — list the calling actor's pending (inbound,
    /// undelivered) knocks. Pure read; replay-safe at 5 s.
    pub async fn knocks_list(&self) -> Result<KnockListReply, R::Error> {
        self.nest
            .request(
                "fauna.knocks.list",
                KnockListRequest {
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.knocks.accept` — accept the knock from `peer_id` (hex), adding
    /// them to the contact roster as `accepted` and dismissing the knock.
    /// Idempotent; replay-safe at 5 s.
    pub async fn knocks_accept(&self, peer_id: String) -> Result<KnockActionReply, R::Error> {
        self.nest
            .request(
                "fauna.knocks.accept",
                KnockActionRequest {
                    peer_id,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.knocks.block` — block the knock sender `peer_id` (hex),
    /// marking the contact `blocked` and dismissing the knock. It trains
    /// nothing (a block is not a spam verdict — `mail-spam.md` § Implicit
    /// signals are forbidden). Idempotent; replay-safe at 5 s.
    pub async fn knocks_block(&self, peer_id: String) -> Result<KnockActionReply, R::Error> {
        self.nest
            .request(
                "fauna.knocks.block",
                KnockActionRequest {
                    peer_id,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.knocks.unblock` — clear a `blocked` contact edge for `peer_id`
    /// (hex), returning the relationship to no-edge (`ContactStatus` → `None`).
    /// The inverse of [`knocks_block`](Self::knocks_block); guarded nest-side
    /// to act only on a `blocked` edge (a no-op otherwise). Idempotent;
    /// replay-safe at 5 s.
    pub async fn knocks_unblock(&self, peer_id: String) -> Result<KnockActionReply, R::Error> {
        self.nest
            .request(
                "fauna.knocks.unblock",
                KnockActionRequest {
                    peer_id,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.knocks.dismiss` — dismiss the knock from `peer_id` (hex),
    /// deleting the knock + the contact so the peer can knock again.
    /// Idempotent; replay-safe at 5 s.
    pub async fn knocks_dismiss(&self, peer_id: String) -> Result<KnockActionReply, R::Error> {
        self.nest
            .request(
                "fauna.knocks.dismiss",
                KnockActionRequest {
                    peer_id,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    // ── contacts ────────────────────────────────────────────────

    /// `fauna.contacts.list` — list the calling actor's full contact roster
    /// (`{peer_id, status, accepted_at, created_at}` rows). Pure read;
    /// replay-safe at 5 s.
    pub async fn contacts_list(&self) -> Result<ContactListReply, R::Error> {
        self.nest
            .request(
                "fauna.contacts.list",
                ContactListRequest {
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.contacts.status` — the calling actor's contact-status toward one
    /// `peer_id` (hex): `Some("pending"|"accepted"|"confirmed"|"blocked")` or
    /// `None` for no relationship (a stranger). The single-peer narrowing of
    /// `contacts_list`, O(1) on the recipient contact-gate drain path
    /// (`docs/goal/ui/folders.md` § Sharing); parse the token with
    /// `fauna_core::data::ContactStatus::from_wire`. Pure read; replay-safe at 5 s.
    pub async fn contacts_status(&self, peer_id: String) -> Result<ContactStatusReply, R::Error> {
        self.nest
            .request(
                "fauna.contacts.status",
                KnockActionRequest {
                    peer_id,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.contacts.confirm` — promote the `accepted` contact `peer_id`
    /// (hex) to `confirmed` (a no-op once already confirmed). Idempotent;
    /// replay-safe at 5 s.
    pub async fn contacts_confirm(&self, peer_id: String) -> Result<ContactConfirmReply, R::Error> {
        self.nest
            .request(
                "fauna.contacts.confirm",
                KnockActionRequest {
                    peer_id,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    // ── inbox mode ──────────────────────────────────────────────

    /// `fauna.inbox.mode.get` — the calling actor's inbox-acceptance mode
    /// (`open` / `allow_knock` / `contacts_only` / `closed`; default
    /// `allow_knock`). Pure read; replay-safe at 5 s.
    pub async fn inbox_mode_get(&self) -> Result<InboxModeGetReply, R::Error> {
        self.nest
            .request(
                "fauna.inbox.mode.get",
                InboxModeGetRequest {
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.inbox.mode.set` — set the calling actor's inbox-acceptance
    /// `mode`. An unrecognized mode yields an invalid-params `RpcError`.
    /// Idempotent overwrite; replay-safe at 5 s.
    pub async fn inbox_mode_set(&self, mode: String) -> Result<InboxModeSetReply, R::Error> {
        self.nest
            .request(
                "fauna.inbox.mode.set",
                InboxModeSetRequest {
                    mode,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{
        FailedRequest, FailingRequester, MockRequester, RecordingRequester, block_on,
    };

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = ContactsClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // The smoke test above only proves the generic surface compiles; it never
    // issues a call, so it can't catch a wrong kind string or a request that no
    // longer serializes to the shape the nest handler decodes. These tests pin
    // both: each `ContactsClient` method must send its exact
    // `fauna.{knocks,contacts,inbox.mode}.*` kind and a payload that round-trips
    // back to the typed request. No nest-side conformance test routes through
    // this adapter (they use literal kind strings), so an adapter-method kind
    // rename was previously caught by nothing — this closes that gap. The
    // pattern mirrors the `RecordingRequester` in `fauna-client-events` /
    // `-snapshots` / `-sync` / `-conversations` (transport-free, so it runs on
    // every target including wasm); real end-to-end round-trip conformance lives
    // in `bins/fauna-nest/tests/conformance_contacts.rs` (real router dispatch).

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        use contacts::*;
        match kind {
            "fauna.knocks.list" => fauna_protocol::encode_canonical(&KnockListReply {
                knocks: vec![],
                extra: Default::default(),
            }),
            "fauna.knocks.accept"
            | "fauna.knocks.block"
            | "fauna.knocks.unblock"
            | "fauna.knocks.dismiss" => fauna_protocol::encode_canonical(&KnockActionReply {
                extra: Default::default(),
            }),
            "fauna.contacts.list" => fauna_protocol::encode_canonical(&ContactListReply {
                contacts: vec![],
                extra: Default::default(),
            }),
            "fauna.contacts.status" => fauna_protocol::encode_canonical(&ContactStatusReply {
                status: Some("confirmed".into()),
                extra: Default::default(),
            }),
            "fauna.contacts.confirm" => fauna_protocol::encode_canonical(&ContactConfirmReply {
                extra: Default::default(),
            }),
            "fauna.inbox.mode.get" => fauna_protocol::encode_canonical(&InboxModeGetReply {
                mode: "allow_knock".into(),
                extra: Default::default(),
            }),
            "fauna.inbox.mode.set" => fauna_protocol::encode_canonical(&InboxModeSetReply {
                extra: Default::default(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    /// 64-char hex id (`peer_id` is a hex string on this surface).
    fn hex32() -> String {
        "ab".repeat(32)
    }

    fn client() -> (
        std::sync::Arc<RecordingRequester>,
        ContactsClient<std::sync::Arc<RecordingRequester>>,
    ) {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ContactsClient::new(rec.clone());
        (rec, client)
    }

    #[test]
    fn knocks_list_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.knocks_list()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.knocks.list");
        let _req: contacts::KnockListRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn knocks_accept_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.knocks_accept(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.knocks.accept");
        let req: contacts::KnockActionRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.peer_id, hex32());
    }

    #[test]
    fn knocks_block_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.knocks_block(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.knocks.block");
        let req: contacts::KnockActionRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.peer_id, hex32());
    }

    #[test]
    fn knocks_unblock_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.knocks_unblock(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.knocks.unblock");
        let req: contacts::KnockActionRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.peer_id, hex32());
    }

    #[test]
    fn knocks_dismiss_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.knocks_dismiss(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.knocks.dismiss");
        let req: contacts::KnockActionRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.peer_id, hex32());
    }

    #[test]
    fn contacts_list_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.contacts_list()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.contacts.list");
        let _req: contacts::ContactListRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn contacts_status_composes_kind_and_payload() {
        let (rec, c) = client();
        let reply = block_on(c.contacts_status(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.contacts.status");
        let req: contacts::KnockActionRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.peer_id, hex32());
        assert_eq!(reply.status, Some("confirmed".to_string()));
    }

    /// `contacts_status`'s `None` branch (no relationship — a true stranger)
    /// isn't reachable through `RecordingRequester`'s single canned reply per
    /// kind; a dedicated one-shot mock pins it distinctly from the `Some`
    /// case above, so a future change can't silently drop the "no record"
    /// path (e.g. by always defaulting to some placeholder status).
    struct StrangerMock;

    impl RpcRequester for StrangerMock {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let reply = fauna_protocol::encode_canonical(&contacts::ContactStatusReply {
                status: None,
                extra: Default::default(),
            })
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    #[test]
    fn contacts_status_returns_none_for_a_stranger() {
        let c = ContactsClient::new(StrangerMock);
        let reply = block_on(c.contacts_status(hex32())).expect("infallible mock");
        assert_eq!(reply.status, None);
    }

    #[test]
    fn contacts_confirm_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.contacts_confirm(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.contacts.confirm");
        // `contacts.confirm` reuses `KnockActionRequest` (the shared `{peer_id}`
        // body); pin both the kind and that the peer id round-trips.
        let req: contacts::KnockActionRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.peer_id, hex32());
    }

    #[test]
    fn inbox_mode_get_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.inbox_mode_get()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.inbox.mode.get");
        let _req: contacts::InboxModeGetRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn inbox_mode_set_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.inbox_mode_set("contacts_only".into())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.inbox.mode.set");
        let req: contacts::InboxModeSetRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.mode, "contacts_only");
    }

    #[test]
    fn wrappers_propagate_transport_error_unchanged() {
        let c = ContactsClient::new(FailingRequester::new("transport unreachable"));
        let err = FailedRequest("transport unreachable".to_string());
        assert_eq!(block_on(c.knocks_list()).unwrap_err(), err);
        assert_eq!(block_on(c.knocks_accept(hex32())).unwrap_err(), err);
        assert_eq!(block_on(c.knocks_block(hex32())).unwrap_err(), err);
        assert_eq!(block_on(c.knocks_unblock(hex32())).unwrap_err(), err);
        assert_eq!(block_on(c.knocks_dismiss(hex32())).unwrap_err(), err);
        assert_eq!(block_on(c.contacts_list()).unwrap_err(), err);
        assert_eq!(block_on(c.contacts_status(hex32())).unwrap_err(), err);
        assert_eq!(block_on(c.contacts_confirm(hex32())).unwrap_err(), err);
        assert_eq!(block_on(c.inbox_mode_get()).unwrap_err(), err);
        assert_eq!(
            block_on(c.inbox_mode_set("closed".into())).unwrap_err(),
            err
        );
    }
}
