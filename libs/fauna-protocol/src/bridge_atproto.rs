//! `fauna.bridges.atproto.*` — nest ↔ `atproto.pds` bridge wire shapes.
//!
//! S2 (identity) surface: the bridge reads the per-user identity roster (with
//! the ATProto handle derived at read time), fetches its sealed key blob
//! (provision-on-read, the DKIM pattern), and reports back the DID it minted
//! at the PLC directory. Named inside the dotted `fauna.bridges.atproto.*`
//! sub-namespace (the `fauna.bridges.feeds.*` precedent) — S3+ grow this same
//! family. Allowlisted for `CallerClass::BridgeAtprotoPds` only.
//!
//! Same-artifact deployment (bridge + nest ship in one container), so
//! `deny_unknown_fields` follows this crate's bridge-wire convention.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

// The USER-class integration-level surface (`set_integration_level`, the
// `get_integration_status` read) lives in [`crate::atproto_pds`], the
// client↔nest file whose `extra`-flatten convention that surface needs —
// `deny_unknown_fields` here is safe only because bridge and nest ship in one
// artifact, which is never true of a client. The one-time
// `enable_identity` kind was retired in S4-B: the depth selector's transition
// kind is the only level/enable mutation path (`docs/goal/ui/atproto.md`
// § Don't do these — the selector is the only level control).

/// `fauna.bridges.atproto.fetch_identities` request (no parameters).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchAtprotoIdentitiesRequest {}

/// One user's ATProto identity as the bridge sees it. The `handle` is derived
/// from the *current* Fauna handle + primary domain on every read — never
/// stored (`atproto-pds-bridge.md` § Identity, derived-at-read).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AtprotoIdentityView {
    /// 32-byte owning actor id.
    pub actor_id: ByteBuf,
    /// The derived ATProto handle (`alice.example.com`).
    pub handle: String,
    /// DID method: `"plc"` or `"web"`.
    pub method: String,
    /// `"pending"` (mint owed), `"active"` (DID recorded), or
    /// `"deactivated"` (layer-2 step-down — the bridge must not serve the
    /// repo, and the mint loop must not mint).
    pub status: String,
    /// The stored DID once minted/imported (DID-is-data — key everything off
    /// this value, never re-derive it).
    pub did: Option<String>,
    /// The USER-custodied senior rotation key's `did:key` pubkey (did:plc
    /// only; empty for did:web, which has no rotation keys). The secret lives
    /// in the user's client credential store and never crosses this wire.
    pub user_rotation_pub_did_key: String,
    /// Bridge-custodied signing key pubkey, once provisioned.
    pub signing_pub_did_key: Option<String>,
    /// Bridge-custodied junior rotation key pubkey, once provisioned.
    pub bridge_rotation_pub_did_key: Option<String>,
    /// The PDS endpoint for the genesis op's `services.atproto_pds`
    /// (`https://<primary-domain>`).
    pub pds_endpoint: String,
}

/// `fauna.bridges.atproto.fetch_identities` reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchAtprotoIdentitiesReply {
    pub identities: Vec<AtprotoIdentityView>,
}

/// `fauna.bridges.atproto.fetch_identity_key_blob` request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchAtprotoIdentityKeyBlobRequest {
    /// 32-byte actor id of the identity whose sealed keys the bridge needs.
    pub actor_id: ByteBuf,
}

/// `fauna.bridges.atproto.fetch_identity_key_blob` reply. The blob is a
/// canonical-CBOR `AtprotoIdentityBlob` sealed to THIS bridge's attested
/// x25519 — nest provisions it on first read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchAtprotoIdentityKeyBlobReply {
    pub blob: ByteBuf,
    /// Unsealed public halves (also persisted nest-side as data).
    pub signing_pub_did_key: String,
    pub bridge_rotation_pub_did_key: String,
}

/// `fauna.bridges.atproto.fetch_session_secret_blob` request. Empty by design:
/// the secret is bridge-wide, and nest keys it by the CALLER's enrolled
/// `(bridge_role, bridge_id)` — a bridge can only ever fetch its own secret.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchAtprotoSessionSecretBlobRequest {}

/// `fauna.bridges.atproto.fetch_session_secret_blob` reply. The blob is a
/// canonical-CBOR `AtprotoSessionSecretBlob` sealed to THIS bridge's attested
/// x25519 — nest provisions it on first read
/// and stores only ciphertext.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchAtprotoSessionSecretBlobReply {
    pub blob: ByteBuf,
}

/// `fauna.bridges.atproto.fetch_issuer_jwks` request (TP5 / S2d leg 1). Empty
/// for the same reason its `fetch_session_secret_blob` sibling is: what the
/// caller may read is decided by its enrolled `(bridge_role, bridge_id)`, never
/// by a field it chooses. Unlike that sibling this reply carries **no secret at
/// all** — the issuer key set's public halves are the same bytes `/oauth/jwks`
/// serves to the open internet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchAtprotoIssuerJwksRequest {}

/// One public issuer key, in the JWKS spelling `/oauth/jwks` already serves —
/// a P-256 `kid` and its base64url coordinates. The bridge needs no `kty`,
/// `crv`, `alg` or `use`: this plane carries exactly one key type, and a
/// resource server that read those members would be deciding something the
/// issuer has already decided.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IssuerJwk {
    pub kid: String,
    pub x: String,
    pub y: String,
}

/// `fauna.bridges.atproto.fetch_issuer_jwks` reply — the replacement source
/// for the resource server's `asKeyForKID`, and the other half of the same
/// teaching (`authorization-server.md` § The issuer → *This is the gap that
/// makes the resource server the retirement slice's first job*).
///
/// **`keys` is the SERVED set, and absence from it is the whole meaning.** The
/// nest applies the retirement horizon lazily on this read, exactly as it does
/// for `/oauth/jwks`, so a key that has left the set must stop verifying — that
/// is what bounds the forced rotation arm's compromise window at every verifier
/// (§ The issuer → *Two rotation arms*, "what the forced arm does not bound").
/// A resource server therefore holds this set whole and never merges it with a
/// set it held before.
///
/// **`issuer` is the nest's issuer identifier — the one the deployment's
/// protected-resource metadata names — or `None` on a nest with no claimed
/// domain,** which has no issuer identity to state at all. A resource server
/// accepts a token whose `iss` is this nest **only** while it holds a `Some`.
/// (It was also `None` while the deployment's pin still named the bridge's own
/// authorization server: leg 1 of the retirement landed the feed ahead of the
/// re-point without turning a pre-flip grant live — § The issuer → *Grants
/// recorded while the flow is unhonoured are ended at the re-point*.)
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchAtprotoIssuerJwksReply {
    #[serde(default)]
    pub issuer: Option<String>,
    pub keys: Vec<IssuerJwk>,
}

/// `fauna.bridges.atproto.record_minted_identity` request — the bridge reports
/// the DID it minted (did:plc submitted to the directory, or the constructed
/// did:web). Nest derives provenance from the row's method; the bridge never
/// chooses provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecordMintedIdentityRequest {
    pub actor_id: ByteBuf,
    pub did: String,
    /// CID of the signed PLC genesis operation (did:plc only).
    pub genesis_cid: Option<String>,
}

/// `fauna.bridges.atproto.record_minted_identity` reply.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RecordMintedIdentityReply {}

// ── S3 projection reads (`atproto-pds-bridge.md` § Where logic lives) ──
//
// The bridge's projection loop pulls a user's public post stream (posts +
// delete-tombstone journal rows, one interleaved oldest-first sequence) and
// the current profile, translates bridge-side via the shared-Rust FFI
// (D-s3-1), and applies the results to the per-user MST repo. Nest kinds
// speak Fauna domain shapes: payloads are the stored bytes, never ATProto
// records.

/// Keyset cursor over the public projection stream, ordered by
/// `(created_at_micros ASC, post_id ASC)`. `post_id` is the lowercase-hex
/// 32-byte **content-row id** of the last item served — for post items that
/// is the post digest; for tombstone items it is the journal row's own id
/// (the deleted post's digest lives inside the payload's `Tombstone.post_id`).
/// Treat the pair as opaque resume state: echo the reply's `next_cursor`
/// back verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PublicPostsCursor {
    /// `content.created_at` of the last served row (epoch **micro**seconds —
    /// the content-table convention, unlike the epoch-millis timestamps on
    /// the F1 session surface).
    pub created_at_micros: i64,
    /// Lowercase-hex 32-byte content-row id of the last served row (the
    /// ordering tie-break; hex preserves the DB's byte-wise BLOB order).
    pub post_id: String,
}

/// `fauna.bridges.atproto.fetch_public_posts` request — one page of a user's
/// public projection stream, oldest-first from `cursor` (exclusive), or from
/// the beginning when `cursor` is `None`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchPublicPostsRequest {
    /// 32-byte actor id whose public stream to page.
    pub actor_id: ByteBuf,
    /// Resume point (exclusive), from a prior reply's `next_cursor`.
    pub cursor: Option<PublicPostsCursor>,
    /// Page size; the nest clamps to its own ceiling.
    pub limit: u32,
}

/// `PublicPostItem.kind` values (strings on the wire for cross-language
/// forward-compat, the `refresh_status` convention).
pub mod public_post_item_kind {
    /// A live public post; `payload` is the stored post bytes verbatim
    /// (embed-as-bytes signed wire, or bare canonical `Post` for
    /// bridge-translated unsigned posts — `Post::decode_resolved_bytes`
    /// accepts both).
    pub const POST: &str = "post";
    /// A post-delete journal row; `payload` is a bare canonical
    /// `fauna_core::data::Tombstone` (author, deleted post id, delete time).
    pub const TOMBSTONE: &str = "tombstone";
}

/// One item of the public projection stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PublicPostItem {
    /// Lowercase-hex 32-byte content-row id (see [`PublicPostsCursor`]).
    pub post_id: String,
    /// `content.created_at`, epoch microseconds: the post's creation instant
    /// for post items, the delete instant for tombstone items — so deletes
    /// interleave at the time they happened and a resumed cursor still sees
    /// a delete of a long-passed post.
    pub created_at_micros: i64,
    /// One of [`public_post_item_kind`].
    pub kind: String,
    /// The stored bytes, verbatim (shape per [`public_post_item_kind`]).
    pub payload: ByteBuf,
    /// For a `tombstone` item, the lowercase-hex 32-byte digest of the
    /// **deleted** post — the nest decodes it from the payload's
    /// `Tombstone.post_id` so the Go bridge never has to decode dag-cbor;
    /// the projection loop resolves it against its PostId→AT-URI map to emit
    /// the delete op. `None` for a `post` item (whose own id is `post_id`).
    pub deleted_post_id: Option<String>,
}

/// `fauna.bridges.atproto.fetch_public_posts` reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchPublicPostsReply {
    pub items: Vec<PublicPostItem>,
    /// `Some` when the page filled `limit` — resume from here. `None` means
    /// the stream is exhausted (until the next `projection_ready` nudge or
    /// poll).
    pub next_cursor: Option<PublicPostsCursor>,
}

/// `fauna.bridges.atproto.fetch_profile` request — the target user's current
/// profile for the `app.bsky.actor.profile` record at rkey `self`.
/// Bridge-class: `fauna.profile.get` is User-class and unusable here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchProfileRequest {
    /// 32-byte actor id.
    pub actor_id: ByteBuf,
}

/// `fauna.bridges.atproto.fetch_profile` reply.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FetchProfileReply {
    /// The stored profile bytes verbatim (embed-as-bytes signed wire, the
    /// `fauna.profile.set` at-rest shape), or `None` when the user has never
    /// set a profile.
    pub profile: Option<ByteBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_strict, encode_canonical};

    /// `Option<ByteBuf>` and `Option<PublicPostsCursor>` both round-trip in
    /// canonical dag-cbor in the `Some` and `None` shapes (no nested Options
    /// anywhere on this surface — dag-cbor cannot round-trip those).
    #[test]
    fn fetch_public_posts_round_trips() {
        let req = FetchPublicPostsRequest {
            actor_id: ByteBuf::from(vec![0x42u8; 32]),
            cursor: Some(PublicPostsCursor {
                created_at_micros: 1_750_000_000_000_000,
                post_id: "aa".repeat(32),
            }),
            limit: 100,
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FetchPublicPostsRequest = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, req);

        let reply = FetchPublicPostsReply {
            items: vec![
                PublicPostItem {
                    post_id: "bb".repeat(32),
                    created_at_micros: 1,
                    kind: public_post_item_kind::POST.into(),
                    payload: ByteBuf::from(b"post-bytes".to_vec()),
                    deleted_post_id: None,
                },
                PublicPostItem {
                    post_id: "cc".repeat(32),
                    created_at_micros: 2,
                    kind: public_post_item_kind::TOMBSTONE.into(),
                    payload: ByteBuf::from(b"tombstone-bytes".to_vec()),
                    deleted_post_id: Some("dd".repeat(32)),
                },
            ],
            next_cursor: None,
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: FetchPublicPostsReply = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, reply);
        assert_eq!(decoded.next_cursor, None);
    }

    #[test]
    fn fetch_profile_round_trips_present_and_absent() {
        let req = FetchProfileRequest {
            actor_id: ByteBuf::from(vec![0x42u8; 32]),
        };
        let bytes = encode_canonical(&req).unwrap();
        let decoded: FetchProfileRequest = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, req);

        let present = FetchProfileReply {
            profile: Some(ByteBuf::from(b"profile-bytes".to_vec())),
        };
        let bytes = encode_canonical(&present).unwrap();
        let decoded: FetchProfileReply = decode_strict(&bytes).unwrap();
        assert_eq!(decoded, present);

        let absent = FetchProfileReply { profile: None };
        let bytes = encode_canonical(&absent).unwrap();
        let decoded: FetchProfileReply = decode_strict(&bytes).unwrap();
        assert_eq!(decoded.profile, None);
    }
}
