//! The user side of the bridged-conversation family — the four caller-scoped
//! `fauna.bridges.conversation.*` kinds and the push's wire kind
//! (`docs/goal/architecture/apps/bridges.md` § Bridge-kind catalogue → Phase G;
//! payload semantics `docs/goal/ui/conversations.md` § Where logic lives →
//! *The `Bridged` adapter*).
//!
//! Generic over [`RpcRequester`], so the same wrapper serves every app — WASM
//! and UniFFI alike. It carries ciphertext only: sealing to `bridge_x25519` and
//! to the user's own recipient key, and opening what `inbox_fetch` returns, are
//! the app glue's, which implements `fauna_conversations`' `BridgedSink` /
//! `BridgedSource` seams over this client.

use fauna_protocol::ByteBuf;
use fauna_protocol::RpcRequester;
use fauna_protocol::bridged_conversations::{
    BridgedMessageInfo, BridgedRoomInfo, InboxFetchReply, InboxFetchRequest, KIND_INBOX_FETCH,
    KIND_ROOMS_LIST, KIND_ROOMS_OPEN, KIND_SEND, PUSH_CONVERSATION_CHANGED, RoomsListReply,
    RoomsListRequest, RoomsOpenReply, RoomsOpenRequest, SendReply, SendRequest,
};

/// The push wire kind the bridged poll subscribes to — a room changed.
pub const CONVERSATION_CHANGED_PUSH_KIND: &str = PUSH_CONVERSATION_CHANGED;

/// One page of `inbox_fetch`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BridgedInboxPage {
    /// Rows in arrival order, sealed to the user.
    pub messages: Vec<BridgedMessageInfo>,
    /// The family marker for the requested room's peer (one-room reads only).
    pub guardian_state: Option<String>,
}

/// The four user-side calls.
pub struct BridgedConversationClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> BridgedConversationClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `conversation.rooms.list` — the caller's bridged rooms across every
    /// bridge serving the account, newest activity first. Each row carries
    /// the identity and key the glue fills `BridgedBackend`'s registry from.
    pub async fn rooms_list(&self) -> Result<Vec<BridgedRoomInfo>, R::Error> {
        let reply: RoomsListReply = self
            .nest
            .request(KIND_ROOMS_LIST, RoomsListRequest::default())
            .await?;
        Ok(reply.rooms)
    }

    /// `conversation.rooms.open` — find or mint the 1:1 room for `address`.
    /// `bridge_id: None` asks the nest which one consented bridge's grammar
    /// admits the address (the backend's `resolve_address`); the refusal
    /// `fauna.bridges.address_refused` means none, or more than one, does.
    pub async fn rooms_open(
        &self,
        bridge_id: Option<String>,
        address: String,
    ) -> Result<BridgedRoomInfo, R::Error> {
        let reply: RoomsOpenReply = self
            .nest
            .request(
                KIND_ROOMS_OPEN,
                RoomsOpenRequest {
                    bridge_id,
                    address,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(reply.room)
    }

    /// `conversation.inbox.fetch` — rows with `id > after_id`, oldest first;
    /// one room's when `room_id` is given, every room's otherwise (the
    /// shared driver's poll). `limit` `0` is the nest's default page.
    pub async fn inbox_fetch(
        &self,
        room_id: Option<Vec<u8>>,
        after_id: i64,
        limit: u32,
    ) -> Result<BridgedInboxPage, R::Error> {
        let reply: InboxFetchReply = self
            .nest
            .request(
                KIND_INBOX_FETCH,
                InboxFetchRequest {
                    room_id: room_id.map(ByteBuf::from),
                    after_id,
                    limit,
                    extra: Default::default(),
                },
            )
            .await?;
        Ok(BridgedInboxPage {
            messages: reply.messages,
            guardian_state: reply.guardian_state,
        })
    }

    /// `conversation.send` — queue `sealed_for_bridge` (sealed to the room's
    /// `bridge_x25519`) and store `sealed_for_self` (sealed to the user's own
    /// recipient key) as the Sent row. Answers the Sent row's id — the one
    /// `inbox_fetch` serves it under — and the account's far address.
    pub async fn send(
        &self,
        room_id: Vec<u8>,
        sealed_for_bridge: Vec<u8>,
        sealed_for_self: Vec<u8>,
    ) -> Result<SendReply, R::Error> {
        self.nest
            .request(
                KIND_SEND,
                SendRequest {
                    room_id,
                    sealed_for_bridge,
                    sealed_for_self,
                    extra: Default::default(),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};

    fn reply(kind: &'static str) -> Vec<u8> {
        match kind {
            KIND_ROOMS_LIST => fauna_protocol::encode_canonical(&RoomsListReply::default()),
            KIND_ROOMS_OPEN => fauna_protocol::encode_canonical(&RoomsOpenReply::default()),
            KIND_INBOX_FETCH => fauna_protocol::encode_canonical(&InboxFetchReply {
                guardian_state: Some("held".into()),
                ..Default::default()
            }),
            KIND_SEND => fauna_protocol::encode_canonical(&SendReply {
                id: 7,
                self_address: "@me:example.org".into(),
                ..Default::default()
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    #[test]
    fn each_call_sends_its_kind_with_the_request_it_names() {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = BridgedConversationClient::new(rec.clone());

        block_on(client.rooms_open(None, "@bob:example.org".into())).expect("mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, KIND_ROOMS_OPEN);
        let req: RoomsOpenRequest = fauna_protocol::decode_strict(&payload).unwrap();
        assert_eq!(
            (req.bridge_id, req.address.as_str()),
            (None, "@bob:example.org")
        );

        let page = block_on(client.inbox_fetch(Some(vec![1; 32]), 41, 0)).expect("mock");
        assert_eq!(page.guardian_state.as_deref(), Some("held"));
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, KIND_INBOX_FETCH);
        let req: InboxFetchRequest = fauna_protocol::decode_strict(&payload).unwrap();
        assert_eq!(
            req.room_id.as_deref().map(|r| r.as_slice()),
            Some(&[1u8; 32][..])
        );
        assert_eq!(req.after_id, 41);

        let sent = block_on(client.send(vec![2; 32], b"b".to_vec(), b"s".to_vec())).expect("mock");
        assert_eq!(
            (sent.id, sent.self_address.as_str()),
            (7, "@me:example.org")
        );
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, KIND_SEND);
        let req: SendRequest = fauna_protocol::decode_strict(&payload).unwrap();
        assert_eq!(
            (req.sealed_for_bridge, req.sealed_for_self),
            (b"b".to_vec(), b"s".to_vec())
        );

        assert!(block_on(client.rooms_list()).expect("mock").is_empty());
        assert_eq!(rec.recorded().0, KIND_ROOMS_LIST);
    }
}
