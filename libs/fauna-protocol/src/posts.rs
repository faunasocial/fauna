//! User-facing WS-RPC payload types for the posts surface — the
//! create / get / interact plane end-user clients call from the feed +
//! post-composer + interaction affordances. T1 of the WS-RPC-everywhere
//! migration (tracked internally) ships the posts cluster
//! (`fauna.posts.{create,get,interact}`); the feed cluster
//! (`fauna.feed.*`) lands in T2.
//!
//! This is a **behavior-preserving** transport migration of the existing
//! HTTP routes (`POST /api/v1/posts`, `GET /api/v1/posts/{id}`,
//! `POST /api/v1/posts/{id}/interact`); the request/reply shapes mirror
//! those routes exactly. The handlers reuse the existing ingest/classify
//! pipeline (`posts_handlers.rs` calls the refactored `routes::` core
//! fns) — no logic is duplicated.
//!
//! - `create` carries the raw signed-post bytes (embed-as-bytes wire of a
//!   signed `Post`; an unsigned bare `Post` is refused) as a CBOR `bstr`;
//!   the reply echoes the content-addressed `post_id` (hex of
//!   `blake3(body)`), matching the HTTP twin's `{"post_id": hex}`.
//! - `get` carries the `post_id` (hex); the reply carries the raw resolved
//!   post bytes as a CBOR `bstr` (the HTTP twin's
//!   `application/octet-stream` body), or a not-found `RpcError`.
//! - `interact` mirrors the HTTP twin's `{action, body?, media?}` request;
//!   the reply echoes the resolved `action` + origin `source` and carries
//!   the protocol-specific result payload as a JSON string (`result`),
//!   preserving the heterogeneous HTTP twin reply bodies exactly.
//! - `delete` (no HTTP twin — net-new 2026-07-15, `feed.md` § State & data
//!   shape → *Post deletion*) carries the embed-as-bytes wire of a signed
//!   `Tombstone` as a CBOR `bstr`; the reply echoes the post digest and an
//!   idempotent `deleted` flag.
//! - `room_labels` (net-new 2026-09-10, `conversation-rooms.md` § The three
//!   classes → *What the home nest does with its read*, purpose 3) carries a
//!   batch of post ids; the reply carries, per post the caller may see them
//!   for, the verdicts a community room's named labelers derived for that
//!   room-restricted post — floor-gated, where the envelope reads are not.
//!
//! Kind registry entries live in `kind.rs::register_posts_kinds`.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use std::collections::BTreeMap;

use crate::Value;

// ── fauna.posts.create ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostCreateRequest {
    /// Raw signed-post bytes (embed-as-bytes wire of a signed `Post`; an
    /// unsigned bare `Post` is refused) — opaque on the wire, the nest
    /// runs the full ingest pipeline. The HTTP twin took these as the
    /// request body; the WS-RPC plane carries them as a CBOR `bstr`.
    pub body: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostCreateReply {
    /// Hex-encoded `post_id` (`blake3(body)`, 32 bytes) — the same value
    /// the HTTP twin returned as `{"post_id": hex}` (201). Content-
    /// addressed and deterministic.
    pub post_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.posts.get ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostGetRequest {
    /// Hex-encoded `post_id` (32 bytes). The HTTP twin took it as the
    /// `{post_id}` path param.
    pub post_id: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The visible **legal-takedown tombstone** carried on `PostGetReply` when a
/// post has been taken down under legal compulsion (`moderation.md` § Categories
/// & enforcement item 1 — "removed under legal obligation [reference]"). Its
/// presence means the `body` is **withheld** (empty) and the client renders the
/// tombstone in its place via the shared `legalTakedownTombstone(reference)`
/// (`fauna_core::obligation::legal_takedown_tombstone`) — no client hand-rolls
/// the string. Carried as an **additive** `Option` so an older client that
/// doesn't know the field still receives an empty body (never the illegal
/// content) and simply doesn't render the tombstone (`version-compatibility.md`
/// — additive-everywhere).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LegalTakedownMarker {
    /// The legal-obligation reference the takedown cites; the `{reference}`
    /// substitution the shared tombstone text renders.
    pub reference: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl LegalTakedownMarker {
    /// Lift a stored `legal_takedown_ref` column (`None` = live, `Some` = taken
    /// down under that reference) into the wire marker — the mapping every serve
    /// path uses to turn the withhold flag into `Option<LegalTakedownMarker>`.
    pub fn from_ref(reference: Option<String>) -> Option<Self> {
        reference.map(|reference| Self {
            reference,
            extra: BTreeMap::new(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostGetReply {
    /// Raw resolved post bytes (the HTTP twin's
    /// `application/octet-stream` body). A missing/quarantine-gated post
    /// surfaces as a `fauna.posts.not_found` `RpcError`, not an empty
    /// `body` — matching the HTTP twin's `404`. When `legal_takedown` is
    /// `Some`, `body` is **empty**: the nest withholds the body of a
    /// legally-taken-down post from every viewer (§ legal-obligation carve-out).
    pub body: ByteBuf,
    /// Present iff the post has been taken down under a legal obligation — the
    /// `body` is then withheld and the client renders this tombstone in its
    /// place. Absent (and omitted from the wire) for every normal post, so a
    /// normal reply is byte-identical to before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal_takedown: Option<LegalTakedownMarker>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.posts.delete ─────────────────────────────────────────────────

/// `fauna.posts.delete` — the author-only self-service post deletion
/// (`feed.md` § State & data shape → *Post deletion*, ratified 2026-07-15).
/// Carries the embed-as-bytes wire of a **signed** `fauna_core::data::Tombstone
/// { author, post_id, created_at }` (sign-over-CID envelope, the same
/// discipline as the signed `Post` on `fauna.posts.create`; signed-only — no
/// bare fallback on this new surface). The nest verifies the envelope
/// signature against `tombstone.author`, requires the connection actor to BE
/// that author, and requires the stored post's author to match.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostDeleteRequest {
    /// Embed-as-bytes wire of the signed `Tombstone`, as a CBOR `bstr`.
    pub body: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostDeleteReply {
    /// Hex-encoded 32-byte post digest the tombstone named (echo).
    pub post_id: String,
    /// `true` when this call newly removed the post; `false` when the post
    /// was already gone — the idempotent success, never an error.
    pub deleted: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.posts.interact ───────────────────────────────────────────────

/// Mirrors the HTTP twin's `InteractRequest` (`interact_routes.rs`).
/// `action` is one of `like` / `unlike` / `reply` / `repost` /
/// `unrepost` / `quote`. `body` + `media` were the reply/quote text and
/// attachments of the retired nest-mint shape; since 2026-09-26 the
/// `reply`/`quote` verbs are an **eligibility door** on every source
/// (`ui/feed.md` § Interaction bar → *Reply and quote on a bridged post*):
/// the words travel in the signed post the app creates on the ack, and no
/// arm reads either field. Both stay on the wire as absorbed, ignored
/// fields — a caller that still sends them is answered the same ack.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostInteractRequest {
    /// Hex-encoded `post_id` (32 bytes) — the target. The HTTP twin took
    /// it as the `{post_id}` path param.
    pub post_id: String,
    pub action: String,
    /// Ignored by every door (see the struct doc); a client sends `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<Vec<Value>>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The target post's four interaction counters as the nest holds them **after**
/// an interaction was applied — the authoritative post-act values, read from
/// `content_meta` in the same handler call.
///
/// Exists so a client never has to *guess* the new number. The nest's `like`
/// counter is idempotent per (actor, post) — a repeat like by the same actor is
/// a counter no-op — so a client-side optimistic `+1` is wrong on the second tap
/// and right on the first, with no local way to tell them apart. Returning the
/// real value costs one indexed single-row read on a call that already wrote
/// that row, and no extra round-trip.
///
/// All four counters ride together (not just the one the action touched) because
/// they are one row: sending the row is cheaper than deciding which field the
/// caller may trust.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostEngagementCounts {
    pub like_count: i64,
    pub reply_count: i64,
    pub repost_count: i64,
    pub quote_count: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostInteractReply {
    /// Echoes the resolved action (the HTTP twin echoed it in the
    /// `reply`/`repost`/`quote` paths).
    pub action: String,
    /// The post's origin protocol (`fauna` / `bluesky` / `nostr` /
    /// `activitypub` / `email`) — the routing key the handler resolved
    /// via `db.get_post_source`.
    pub source: String,
    /// The protocol-specific result payload as a JSON string — the exact
    /// JSON body the HTTP twin returned (`{"ok":true}`, `{action,
    /// target_post_id,source}`, or a bridged protocol's response). The
    /// client deserializes it per `action`. Carried as a string (not a
    /// CBOR map) to round-trip the heterogeneous HTTP shapes byte-for-byte
    /// without a serde_json↔CBOR bridge.
    pub result: String,
    /// The target post's counters after the act (additive, 2026-08-10).
    ///
    /// `None` means "this nest is not telling you" — a bridged (`bluesky`/`nostr`/`activitypub`) source whose
    /// counters live in the origin protocol rather than `content_meta` — save
    /// the `reply`/`quote` eligibility ack on an `activitypub` or `nostr`
    /// target, whose ingested note IS a local row, so its ack carries that
    /// row's counters exactly as the native arm does (`feed.md` § Interaction
    /// bar → *Reply and quote on a bridged post*) — or
    /// `unrepost`, whose `post_id` names the caller's *own repost post* and not
    /// the post whose counter moved. A client treats `None` as "leave the
    /// rendered counts alone" — so the field is additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<PostEngagementCounts>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.posts.list ───────────────────────────────────────────────────

/// `fauna.posts.list` — the **self-scoped author enumeration** of the calling
/// actor's own posts (`content-index.md` § Ingest triggers, v1 → *Posts are
/// gated…*, ruled 2026-08-05).
///
/// **Self-scoped by construction: there is deliberately no `actor_id` field.**
/// The corpus is the connection actor's own posts, exactly like the index
/// rail's user plane (`fauna.index.{record,list}` — `content-index.md` § *The
/// rail has two planes*): with no field to name a target, reading another
/// actor's posts is unrepresentable on this wire rather than merely refused.
///
/// **Why it exists** (do not re-litigate — `content-index.md:110-111`): every
/// other listing surface is *feed*-scoped (`fauna.feed.posts` / `.local` /
/// `.trending`), and `fauna.posts.*` was `create`/`get`/`delete`/`interact`
/// only, so the client-side index builder had no door to enumerate the user's
/// own corpus through — in *any* direction. The "index from now on" alternative
/// is **refuted**, not merely disfavored: `CLIENT_BUILDS_INDEX` makes phones
/// queriers only, so a post created on a phone would be indexed by no seat ever.
///
/// **Paging is a two-half keyset cursor, newest-first.** Both halves are echoed
/// back from [`PostsListReply`] and resent verbatim. The tiebreak half is not
/// optional polish: `created_at` alone is not a total order, so a key-only
/// `created_at < cursor` predicate drops **every** row sharing the boundary
/// timestamp — the exact defect [`crate::feed::FeedPostsRequest::score_cursor_created_at`]
/// documents and was fixed for on the scored feed branch. A page boundary
/// landing mid-tie here neither skips nor duplicates.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PostsListRequest {
    /// Keyset cursor, **key half**: the `created_at` (epoch micros) of the last
    /// row of the previous page. Absent on the first page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_created_at: Option<i64>,
    /// Keyset cursor, **tiebreak half**: the hex-lowercase `post_id` of that
    /// very same row. Sent with `cursor_created_at` or not at all: a nest
    /// refuses either half alone (`invalid_params`) — the key-only shape, which
    /// re-inherited the tie-skipping this pair exists to fix, left the wire
    /// with the compat-remnant sweep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_post_id: Option<String>,
    /// Page size. Clamped nest-side to a sane ceiling; omitted means the
    /// nest's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One row of a [`PostsListReply`] page: enough for the index walk to stage a
/// document **without a second read per post**, which is the whole point of the
/// kind.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PostsListItem {
    /// Hex-lowercase 32-byte post digest — the inherited spelling
    /// (`fauna_core::hex32`), identical to `fauna.posts.get`'s `post_id` and to
    /// what `ui/search.md` § The page's wire surface pins.
    pub post_id: String,
    /// Epoch **microseconds** — the one unit `content.created_at` uses for
    /// every writer (`db/schema.rs`, ratified 2026-08-03).
    pub created_at: i64,
    /// The post's plain body text, from the *same* extraction that feeds the
    /// nest's own search corpus (`Post::body_text()` → `content_fts.body`), so
    /// the client index and the nest index can never disagree about what a
    /// post's text is.
    ///
    /// **Empty when the body is withheld.** Two cases, both deliberate: a post
    /// taken down under legal obligation (withheld from *every* viewer, its
    /// author included — `routes::get_post_core`, `moderation.md` § Categories
    /// & enforcement item 1), and a post whose best-effort FTS extraction never
    /// landed. The row is still listed either way: dropping it would make the
    /// enumeration silently partial, which is the failure class this kind
    /// exists to close.
    pub body: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PostsListReply {
    /// The page, newest-first (`created_at DESC, post_id DESC`).
    pub posts: Vec<PostsListItem>,
    /// Key half of the cursor for the *next* page — the `created_at` of the
    /// last row above. Absent exactly when this page is the last one, which is
    /// how a walk knows to stop without an extra empty round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_created_at: Option<i64>,
    /// Tiebreak half of that cursor — the `post_id` of the same last row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_post_id: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.posts.room_labels ────────────────────────────────────────────

/// `fauna.posts.room_labels` — the verdicts a **community room's** named
/// labelers derived for room-restricted posts, read by a live floor member
/// (`conversation-rooms.md` § The three classes → *What the home nest does
/// with its read*, purpose 3; `ui/feed.md` § Encryption at rest →
/// *Room-restricted — the ruling*, ruling 7).
///
/// Why a door of its own rather than a field on `fauna.posts.get`: a room
/// post is its author's ordinary post, served as a sealed envelope to every
/// follower — most of whom are not on the room's floor — through `posts.get`,
/// the feed pages and the deep-link read alike, and none of those reads is
/// floor-gated because the envelope reveals nothing. A verdict was derived
/// from the plaintext and is served exactly where the floor says the caller
/// stands; putting it on a read every follower makes would either gate the
/// envelope or leak the verdict. So the verdicts have a read of their own,
/// keyed by post id (the one name a member already holds for a post), and
/// the app asks it for the posts it has opened.
///
/// Post-scoped, not room-scoped, so a caller need not know which room indexed
/// a post before asking: the nest finds the room from its own reception-pass
/// map and gates on that room's floor.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PostRoomLabelsRequest {
    /// Hex-encoded post ids (32 bytes each). More than
    /// [`POST_ROOM_LABELS_MAX_IDS`] is refused as invalid params.
    pub post_ids: Vec<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The ceiling on one request's `post_ids` — a feed page's worth, owned here
/// so a client can batch against the same number.
pub const POST_ROOM_LABELS_MAX_IDS: usize = 200;

/// One post's verdicts — the same two planes `ChannelFetchEntry` carries
/// beside a room message (`labels` the category verdicts, one per category,
/// the highest-confidence one; `scores` the per-labeler factor rows).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PostRoomLabelsEntry {
    /// Hex-encoded post id, as requested.
    pub post_id: String,
    /// Category verdicts, strongest first — exactly the per-row unit
    /// `FeedPostItem.labels` carries, so the app merges them into the post's
    /// own labels the way it merges a room message's
    /// (`fauna_core::content_category::merge_server_labels`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    /// The factor rows — one tier-3 `labeler:<id>` score per named labeler
    /// that ran, `wasm` and `text-model` alike.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scores: Vec<fauna_core::scoring::ScoreEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One entry per requested post that has verdicts **the caller may see**: a
/// post the caller's floor did not index, one no labeler labelled, one the
/// revoke's purge emptied, and every post of a room the caller is not a live
/// member of all simply have no entry — indistinguishable on purpose, for the
/// room search door's reason (an empty answer is a real answer, and the
/// difference between "no verdict" and "not yours to read" is itself a fact
/// about the floor).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PostRoomLabelsReply {
    pub posts: Vec<PostRoomLabelsEntry>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.posts.room_labels_remote ─────────────────────────────────────

/// The same read as [`PostRoomLabelsRequest`], for a room homed on **another
/// nest**: the caller's own nest originates
/// `fauna.federation.conversation.room_labels.fetch` to `nest_url` and hands
/// back the room home's answer, unchanged and unstored
/// (`conversation-rooms.md` § The home nest — "a member on a foreign nest
/// reaches the room only through their own home nest").
///
/// Without it a foreign-homed member reads **no** verdicts at all. A room
/// post's post → room map is written by the reception pass, which runs only on
/// the nest that stores the post and homes its room
/// (`room_post_view::index_room_post`), so the member's own nest resolves no
/// room for the post and answers an empty reply — and an empty reply is
/// deliberately how "nobody labelled" and "not yours to read" are made
/// indistinguishable ([`PostRoomLabelsReply`]), so the card simply renders
/// with no badge and nothing says why.
///
/// A **distinct kind** rather than an additive `nest_url` on
/// `posts.room_labels`, for the `room.list_roster_remote` reason: an old
/// own-nest that ignored the field would answer from its own reception-pass
/// map, where the post is absent — a clean empty success the seam cannot tell
/// from "nobody labelled this post", so the verdicts would stay silently
/// missing with no signal that a newer nest could have served them. An unknown
/// kind fails loud, and the feed's best-effort read leaves the card's own
/// labels exactly as they were.
///
/// ⚠ **`room_id` is carried, where the same-nest request deliberately is
/// not** — the one shape difference, and it is the federation gate's. The
/// same-nest door is post-scoped precisely so a caller need not know which
/// room indexed a post; the relayed door's first act is the structural
/// `require_foreign_member` check on the room's channel id (a room id *is* its
/// channel id), exactly as on `channel.fetch`, `generations.fetch` and
/// `roster.fetch`. The reader always holds it: a room post names its room in
/// its own [`KeyAccess::Room`](fauna_core::subscription::types::KeyAccess)
/// arm, which is what it opened the post by. The home then answers only for
/// posts its own map assigns to *that* room, so one request cannot fish across
/// rooms.
///
/// The reply is [`PostRoomLabelsReply`], the same shape the same-nest door
/// returns, because it is literally the room home's reply forwarded.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PostRoomLabelsRemoteRequest {
    /// Hex-encoded room id (32 bytes). A room id *is* its channel id.
    pub room_id: String,
    /// Hex-encoded post ids (32 bytes each). More than
    /// [`POST_ROOM_LABELS_MAX_IDS`] is refused as invalid params, on the same
    /// ceiling the same-nest door applies.
    pub post_ids: Vec<String>,
    /// The room's home nest — where the canonical log, the reception-pass map
    /// and the verdicts live.
    pub nest_url: String,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn sample_create_request() -> PostCreateRequest {
        PostCreateRequest {
            body: ByteBuf::from(vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0x11]),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn post_create_request_round_trips() {
        let req = sample_create_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: PostCreateRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn post_room_labels_round_trips_and_an_empty_entry_omits_both_planes() {
        let req = PostRoomLabelsRequest {
            post_ids: vec!["ab".repeat(32), "cd".repeat(32)],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: PostRoomLabelsRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);

        let reply = PostRoomLabelsReply {
            posts: vec![PostRoomLabelsEntry {
                post_id: "ab".repeat(32),
                labels: vec![fauna_core::content_category::ContentLabelEntry {
                    category: "spam".into(),
                    confidence_per_mille: 900,
                }],
                scores: vec![fauna_core::scoring::ScoreEntry {
                    factor: "labeler:ab".into(),
                    score: 900,
                    tier: fauna_core::scoring::TIER_COMMUNITY,
                    scorer_version: 1,
                }],
                extra: BTreeMap::new(),
            }],
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: PostRoomLabelsReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);

        // Additive in both directions: an entry with no verdicts on either
        // plane is just its id on the wire, and decodes back to empty planes.
        let bare = encode_canonical(&PostRoomLabelsEntry {
            post_id: "ab".repeat(32),
            ..Default::default()
        })
        .unwrap();
        let decoded: PostRoomLabelsEntry = decode(&bare).unwrap();
        assert!(decoded.labels.is_empty() && decoded.scores.is_empty());
        let as_map: BTreeMap<String, Value> = decode(&bare).unwrap();
        assert_eq!(as_map.len(), 1, "only post_id is on the wire: {as_map:?}");
    }

    #[test]
    fn post_room_labels_remote_round_trips_and_names_its_room() {
        let req = PostRoomLabelsRemoteRequest {
            room_id: "c7".repeat(32),
            post_ids: vec!["ab".repeat(32), "cd".repeat(32)],
            nest_url: "https://home.example".into(),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: PostRoomLabelsRemoteRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);

        // The three fields are all of it, and `room_id` is one of them: the
        // relayed read's first act is the structural gate on the room's
        // channel id, which the same-nest post-scoped shape cannot supply.
        let as_map: BTreeMap<String, Value> = decode(&bytes).unwrap();
        assert_eq!(as_map.len(), 3, "{as_map:?}");
        assert!(as_map.contains_key("room_id"));
    }

    #[test]
    fn post_create_request_canonical_re_encodes_identically() {
        let req = sample_create_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: PostCreateRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn post_create_reply_round_trips() {
        let reply = PostCreateReply {
            post_id: "ab".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: PostCreateReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn post_get_request_round_trips() {
        let req = PostGetRequest {
            post_id: "cd".repeat(32),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: PostGetRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    fn sample_get_reply() -> PostGetReply {
        PostGetReply {
            body: ByteBuf::from(vec![0x01, 0x02, 0x03, 0x04, 0x05]),
            legal_takedown: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn post_get_reply_round_trips() {
        let reply = sample_get_reply();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: PostGetReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    #[test]
    fn post_get_reply_canonical_re_encodes_identically() {
        let reply = sample_get_reply();
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: PostGetReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn post_get_reply_legal_takedown_round_trips_and_is_additive() {
        // A normal reply omits the field entirely: adding `legal_takedown`
        // must not change the wire of a live post (skip_serializing_if=None).
        let normal = sample_get_reply();
        let normal_bytes = encode_canonical(&normal).unwrap();
        let bare = encode_canonical(&PostGetReply {
            body: ByteBuf::from(vec![0x01, 0x02, 0x03, 0x04, 0x05]),
            legal_takedown: None,
            extra: BTreeMap::new(),
        })
        .unwrap();
        assert_eq!(normal_bytes, bare, "None omits the field from the wire");

        // A taken-down reply: body withheld (empty) + tombstone reference.
        let taken = PostGetReply {
            body: ByteBuf::new(),
            legal_takedown: Some(LegalTakedownMarker {
                reference: "EU-DSA-2024/12345".into(),
                extra: BTreeMap::new(),
            }),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&taken).unwrap();
        let decoded: PostGetReply = decode(&bytes).unwrap();
        assert_eq!(taken, decoded);
        assert!(decoded.body.is_empty());
        assert_eq!(
            decoded.legal_takedown.as_ref().unwrap().reference,
            "EU-DSA-2024/12345"
        );
        // Canonical re-encode is byte-stable.
        assert_eq!(encode_canonical(&decoded).unwrap(), bytes);
    }

    #[test]
    fn post_delete_request_round_trips() {
        let req = PostDeleteRequest {
            body: ByteBuf::from(vec![0xAA, 0xBB, 0xCC]),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: PostDeleteRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes, bytes2, "canonical re-encode is byte-stable");
    }

    #[test]
    fn post_delete_reply_round_trips() {
        for deleted in [true, false] {
            let reply = PostDeleteReply {
                post_id: "ab".repeat(32),
                deleted,
                extra: BTreeMap::new(),
            };
            let bytes = encode_canonical(&reply).unwrap();
            let decoded: PostDeleteReply = decode(&bytes).unwrap();
            assert_eq!(reply, decoded);
        }
    }

    fn sample_interact_request() -> PostInteractRequest {
        PostInteractRequest {
            post_id: "ef".repeat(32),
            action: "reply".into(),
            body: Some("hello".into()),
            media: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn post_interact_request_round_trips() {
        let req = sample_interact_request();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: PostInteractRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn post_interact_request_canonical_re_encodes_identically() {
        let req = sample_interact_request();
        let bytes1 = encode_canonical(&req).unwrap();
        let decoded: PostInteractRequest = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }

    #[test]
    fn post_interact_request_like_omits_optional_fields() {
        let req = PostInteractRequest {
            post_id: "12".repeat(32),
            action: "like".into(),
            body: None,
            media: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: PostInteractRequest = decode(&bytes).unwrap();
        assert_eq!(req, decoded);
        assert!(decoded.body.is_none());
        assert!(decoded.media.is_none());
    }

    #[test]
    fn post_interact_reply_round_trips() {
        let reply = PostInteractReply {
            action: "like".into(),
            source: "fauna".into(),
            result: r#"{"ok":true}"#.into(),
            counts: Some(PostEngagementCounts {
                like_count: 1,
                reply_count: 0,
                repost_count: 0,
                quote_count: 0,
                extra: BTreeMap::new(),
            }),
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: PostInteractReply = decode(&bytes).unwrap();
        assert_eq!(reply, decoded);
    }

    /// The additive `counts` field is absent-safe: a reply
    /// encoded WITHOUT it (bridged source, unrepost) decodes to `None` rather than failing,
    /// which is the whole basis of the client's "leave the counts alone"
    /// fallback (`version-compatibility.md` — additive-everywhere).
    #[test]
    fn post_interact_reply_without_counts_decodes_as_none() {
        let old = PostInteractReply {
            action: "like".into(),
            source: "fauna".into(),
            result: r#"{"ok":true}"#.into(),
            counts: None,
            extra: BTreeMap::new(),
        };
        let bytes = encode_canonical(&old).unwrap();
        let decoded: PostInteractReply = decode(&bytes).unwrap();
        assert!(decoded.counts.is_none());
        assert_eq!(old, decoded);
    }

    #[test]
    fn post_interact_reply_canonical_re_encodes_identically() {
        let reply = PostInteractReply {
            action: "reply".into(),
            source: "fauna".into(),
            result: r#"{"action":"reply","target_post_id":"ef","source":"fauna"}"#.into(),
            counts: None,
            extra: BTreeMap::new(),
        };
        let bytes1 = encode_canonical(&reply).unwrap();
        let decoded: PostInteractReply = decode(&bytes1).unwrap();
        let bytes2 = encode_canonical(&decoded).unwrap();
        assert_eq!(bytes1, bytes2);
    }
}
