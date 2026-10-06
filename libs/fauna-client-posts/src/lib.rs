//! Typed-call wrapper for the user-facing `fauna.posts.*` WS-RPC kinds —
//! the create / get / interact plane clients hit from the feed +
//! post-composer + interaction affordances. Part of the WS-RPC-everywhere
//! migration (tracked internally).
//!
//! Pattern: same shape as `fauna-client-bridges` — a thin
//! `pub struct PostsClient<R: RpcRequester> { nest: R }`, one async method
//! per kind, no state machine, generic over the WS-RPC transport so the
//! kind-composition logic is written once and shared across native +
//! wasm (priority #2). This crate is the transport surface only; post
//! construction / signing lives in `fauna-client-core` (the caller builds
//! the signed-post bytes and hands them to `posts_create`).

use fauna_protocol::ByteBuf;
use fauna_protocol::RpcRequester;
use fauna_protocol::posts::{
    PostCreateReply, PostCreateRequest, PostDeleteReply, PostDeleteRequest, PostGetReply,
    PostGetRequest, PostInteractReply, PostInteractRequest, PostRoomLabelsRemoteRequest,
    PostRoomLabelsReply, PostRoomLabelsRequest, PostsListReply, PostsListRequest,
};

pub use fauna_protocol::posts;

/// Typed `fauna.posts.*` call surface, generic over the WS-RPC transport
/// (`R: RpcRequester`): native call sites pass `Arc<NestClient>`, the wasm SPA
/// passes its `WsRpcClient`. Errors propagate as the transport's `R::Error`
/// (native `NestClientError`, wasm rpc-wasm error).
pub struct PostsClient<R: RpcRequester> {
    nest: R,
}

impl<R: RpcRequester> PostsClient<R> {
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.posts.create` — store a post. `body` is the raw signed-post
    /// bytes (embed-as-bytes wire of a signed `Post`; an unsigned bare
    /// `Post` is refused) — the nest runs the full ingest/classify pipeline and
    /// echoes the content-addressed `post_id` (hex of `blake3(body)`).
    /// Replay-safe at 30 s (content-addressed; `put_post` + replicate +
    /// bluesky write-through idempotent on a byte-identical re-submit).
    pub async fn posts_create(&self, body: Vec<u8>) -> Result<PostCreateReply, R::Error> {
        self.nest
            .request(
                "fauna.posts.create",
                PostCreateRequest {
                    body: ByteBuf::from(body),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.posts.get` — fetch the raw resolved post bytes for `post_id`
    /// (hex `[u8; 32]`). Quarantined posts are visible only to the author /
    /// admins (the gate keys on the connection actor); a missing or
    /// quarantine-gated post surfaces as a `fauna.posts.not_found`
    /// `RpcError`. Pure read; replay-safe at 5 s.
    pub async fn posts_get(&self, post_id: impl Into<String>) -> Result<PostGetReply, R::Error> {
        self.nest
            .request(
                "fauna.posts.get",
                PostGetRequest {
                    post_id: post_id.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.posts.room_labels` — the verdicts a community room's named
    /// labelers derived for room-restricted posts (`conversation-rooms.md`
    /// § The three classes → *What the home nest does with its read*, purpose
    /// 3; `ui/feed.md` § Encryption at rest → ruling 7), for the caller's
    /// posts among `post_ids` (hex `[u8; 32]`, at most
    /// [`posts::POST_ROOM_LABELS_MAX_IDS`]). Floor-gated where the envelope
    /// reads are not: an entry comes back only for a post whose indexing
    /// room the caller is a live member of, so a reader off the floor — or
    /// on a failed read — gets nothing and keeps
    /// the card's own labels. Pure read; replay-safe at 5 s.
    pub async fn posts_room_labels(
        &self,
        post_ids: Vec<String>,
    ) -> Result<PostRoomLabelsReply, R::Error> {
        self.nest
            .request(
                "fauna.posts.room_labels",
                PostRoomLabelsRequest {
                    post_ids,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.posts.room_labels_remote` — the same verdict read for a room
    /// homed on **another** nest (`conversation-rooms.md` § The home nest —
    /// "a member on a foreign nest reaches the room only through their own
    /// home nest"): this caller's own nest originates
    /// `fauna.federation.conversation.room_labels.fetch` to `nest_url` and
    /// hands back the room home's reply, forwarded and unstored.
    ///
    /// A **distinct kind**, not an additive `nest_url` on
    /// [`Self::posts_room_labels`]: an old own-nest that ignored the field
    /// would answer from its own reception-pass map, which indexes no post of
    /// a room it does not home — a clean empty success the caller cannot tell
    /// from "nobody labelled this post". An unknown kind fails loud instead,
    /// and the feed's best-effort read keeps the card's own labels.
    ///
    /// `room_id` names the room the posts belong to (hex `[u8; 32]`; a room
    /// id *is* its channel id), because the room home's first act is the
    /// structural foreign-member gate on it.
    pub async fn posts_room_labels_remote(
        &self,
        room_id: String,
        post_ids: Vec<String>,
        nest_url: String,
    ) -> Result<PostRoomLabelsReply, R::Error> {
        self.nest
            .request(
                "fauna.posts.room_labels_remote",
                PostRoomLabelsRemoteRequest {
                    room_id,
                    post_ids,
                    nest_url,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.posts.delete` — destroy the caller's own post (`feed.md`
    /// § State & data shape → *Post deletion*). `tombstone` is the raw
    /// signed-tombstone bytes (embed-as-bytes wire of a signed
    /// `fauna_core::data::Tombstone`; build with `sign_and_pack`, exactly
    /// as compose builds signed-post bytes — signing lives with the caller,
    /// matching `posts_create`). The nest verifies the signature, requires
    /// the connection actor to be the author, and removes the post; the
    /// reply's `deleted:false` is the idempotent already-gone success.
    /// Replay-safe at 10 s (idempotent end state).
    pub async fn posts_delete(&self, tombstone: Vec<u8>) -> Result<PostDeleteReply, R::Error> {
        self.nest
            .request(
                "fauna.posts.delete",
                PostDeleteRequest {
                    body: ByteBuf::from(tombstone),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.posts.interact` — like / unlike / reply / repost / unrepost /
    /// quote on `post_id` (hex `[u8; 32]`). `body` is the optional text for
    /// reply/quote. The nest resolves the post's origin protocol and routes
    /// the interaction accordingly; the reply echoes the resolved `action` +
    /// `source` and carries the protocol-specific result as a JSON string
    /// (`result`). **Forbids replay** at 5 s — the `like` action increments
    /// a non-idempotent score and notifies, so re-issue explicitly on a
    /// dropped connection rather than auto-retrying.
    pub async fn posts_interact(
        &self,
        post_id: impl Into<String>,
        action: impl Into<String>,
        body: Option<String>,
    ) -> Result<PostInteractReply, R::Error> {
        self.nest
            .request(
                "fauna.posts.interact",
                PostInteractRequest {
                    post_id: post_id.into(),
                    action: action.into(),
                    body,
                    media: None,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.posts.list` — one page of the calling actor's **own** posts,
    /// newest-first (self-scoped by construction: the request carries no
    /// `actor_id` — `content-index.md` § Ingest triggers, v1 → the posts
    /// ruling). Pass both cursor halves back verbatim from the previous
    /// page's reply; an absent reply cursor means the page was the last one.
    /// Pure read; replay-safe.
    pub async fn posts_list(&self, request: PostsListRequest) -> Result<PostsListReply, R::Error> {
        self.nest.request("fauna.posts.list", request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{MockRequester, RecordingRequester, block_on};

    #[test]
    fn constructor_builds_over_generic_requester() {
        let _c = PostsClient::new(MockRequester);
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // The construction-only `MockRequester` above can't catch a wrong kind
    // string or a request that no longer serializes to the shape the nest
    // handler decodes. These tests pin both: each `PostsClient` method must send
    // its exact `fauna.posts.*` kind and a payload that round-trips back to the
    // typed request. No nest-side conformance test routes through this adapter
    // (they use literal kind strings), so an adapter-method kind rename would
    // otherwise break silently. The pattern mirrors `fauna-client-events` /
    // `-conversations`'s `RecordingRequester` (transport-free, so it runs on
    // every target including wasm); real end-to-end round-trip conformance lives
    // in `bins/fauna-nest/tests/conformance_posts.rs`.

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        // Answer with a reply the requested `Reply` type decodes — one arm
        // per kind, each the minimal valid shape.
        match kind {
            "fauna.posts.create" => fauna_protocol::encode_canonical(&PostCreateReply {
                post_id: "ab".repeat(32),
                extra: Default::default(),
            }),
            "fauna.posts.get" => fauna_protocol::encode_canonical(&PostGetReply {
                body: ByteBuf::from(vec![0x01, 0x02]),
                legal_takedown: None,
                extra: Default::default(),
            }),
            "fauna.posts.delete" => fauna_protocol::encode_canonical(&PostDeleteReply {
                post_id: "ab".repeat(32),
                deleted: true,
                extra: Default::default(),
            }),
            "fauna.posts.room_labels" | "fauna.posts.room_labels_remote" => {
                fauna_protocol::encode_canonical(&PostRoomLabelsReply::default())
            }
            "fauna.posts.interact" => fauna_protocol::encode_canonical(&PostInteractReply {
                action: "like".into(),
                source: "fauna".into(),
                result: "{\"ok\":true}".into(),
                counts: None,
                extra: Default::default(),
            }),
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    /// 64-char hex id (`post_id` is a hex string on this surface, not raw bytes).
    fn hex32() -> String {
        "ab".repeat(32)
    }

    fn client() -> (
        std::sync::Arc<RecordingRequester>,
        PostsClient<std::sync::Arc<RecordingRequester>>,
    ) {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = PostsClient::new(rec.clone());
        (rec, client)
    }

    #[test]
    fn posts_create_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.posts_create(vec![0xDE, 0xAD, 0xBE, 0xEF])).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.posts.create");
        let req: PostCreateRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.body, ByteBuf::from(vec![0xDE, 0xAD, 0xBE, 0xEF]));
    }

    #[test]
    fn posts_get_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.posts_get(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.posts.get");
        let req: PostGetRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.post_id, hex32());
    }

    #[test]
    fn posts_room_labels_composes_kind_and_payload() {
        let (rec, c) = client();
        let reply =
            block_on(c.posts_room_labels(vec![hex32(), "cd".repeat(32)])).expect("infallible mock");
        assert!(
            reply.posts.is_empty(),
            "the mock answers a reader off every floor"
        );
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.posts.room_labels");
        let req: PostRoomLabelsRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.post_ids, vec![hex32(), "cd".repeat(32)]);
    }

    #[test]
    fn posts_room_labels_remote_composes_the_distinct_kind_and_names_the_room() {
        let (rec, c) = client();
        block_on(c.posts_room_labels_remote(
            "c7".repeat(32),
            vec![hex32()],
            "https://home.example".into(),
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(
            kind, "fauna.posts.room_labels_remote",
            "a distinct kind, never an additive field on the same-nest read"
        );
        let req: PostRoomLabelsRemoteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.room_id, "c7".repeat(32));
        assert_eq!(req.post_ids, vec![hex32()]);
        assert_eq!(req.nest_url, "https://home.example");
    }

    #[test]
    fn posts_delete_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.posts_delete(vec![0xC0, 0xFF, 0xEE])).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.posts.delete");
        let req: PostDeleteRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.body, ByteBuf::from(vec![0xC0, 0xFF, 0xEE]));
    }

    #[test]
    fn posts_interact_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.posts_interact(hex32(), "reply", Some("nice post".into())))
            .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.posts.interact");
        let req: PostInteractRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.post_id, hex32());
        assert_eq!(req.action, "reply");
        assert_eq!(req.body.as_deref(), Some("nice post"));
    }
}
