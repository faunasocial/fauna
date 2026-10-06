//! Codec round-trip tests for all `fauna.subscriptions.*` request/reply types.
//!
//! Each type is encoded via `encode_canonical` and decoded via `decode`,
//! then compared against the original value. ByteBuf fields are also
//! verified byte-for-byte against a non-trivial byte pattern.

use fauna_core::data::Timestamp;
use fauna_core::identity::ActorId;
use fauna_protocol::subscriptions::*;
use fauna_protocol::{decode_strict as decode, encode_canonical};
use serde_bytes::ByteBuf;

fn roundtrip<T>(value: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = encode_canonical(&value).expect("encode");
    let decoded: T = decode(bytes.as_ref()).expect("decode");
    assert_eq!(decoded, value);
}

// ── tiers.create ───────────────────────────────────────────────

/// An opaque birth-blob envelope — the codec only carries the bytes; the
/// signature chain is the nest's to verify.
fn birth_upload() -> EncryptedKeyBlobUpload {
    EncryptedKeyBlobUpload {
        key_blob: fauna_core::encoding::EmbedAsBytes {
            envelope: vec![0xB1; 100],
            bytes: vec![0xB2; 8],
            signer_auth: None,
        },
        signer_auth: fauna_core::encoding::EmbedAsBytes {
            envelope: vec![0xB3; 100],
            bytes: vec![0xB4; 8],
            signer_auth: None,
        },
        extra: Default::default(),
    }
}

/// A `tiers.create` for `name` at `rank` with every optional field unset —
/// the struct-update base for the cases below.
fn tier_create(name: &str, rank: u32) -> TierCreateRequest {
    TierCreateRequest {
        name: name.into(),
        rank,
        description: None,
        price_hint: None,
        payment_url: None,
        auto_approve: false,
        encrypted_upload: birth_upload(),
        unlocks_post: None,
        asking_price: None,
        hidden: false,
        extra: Default::default(),
    }
}

#[test]
fn tier_create_request_roundtrip() {
    roundtrip(TierCreateRequest {
        description: Some("Gold tier".into()),
        price_hint: Some("$5/month".into()),
        payment_url: Some("https://example.com/pay".into()),
        auto_approve: true,
        ..tier_create("gold", 1)
    });
}

/// The birth envelope is **required**: the envelope-less create an older
/// client once sent was removed by the compat-remnant sweep
/// (`version-compatibility.md` § Dimension 2, the fourth ratified exception),
/// so it fails to decode rather than landing a keyless tier.
#[test]
fn an_envelope_less_tier_create_is_refused_at_decode() {
    #[derive(serde::Serialize)]
    struct EnvelopeLess {
        name: String,
        rank: u32,
        auto_approve: bool,
    }
    let bytes = encode_canonical(&EnvelopeLess {
        name: "gold".into(),
        rank: 1,
        auto_approve: true,
    })
    .expect("encode");
    assert!(
        decode::<TierCreateRequest>(bytes.as_ref()).is_err(),
        "a create without `encrypted_upload` must not decode"
    );
}

/// The per-post pay-to-unlock designation (`monetization.md` § Per-post
/// pay-to-unlock) survives the wire on all three tier types, and is
/// **additive**: a pre-field encoding from an older peer still decodes, with
/// the designation absent (`version-compatibility.md` I4).
#[test]
fn the_post_unlock_designation_roundtrips_and_decodes_additively() {
    const POST: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    roundtrip(TierCreateRequest {
        unlocks_post: Some(POST.into()),
        ..tier_create("unlock-a", 9)
    });
    roundtrip(TierUpdateRequest {
        name: "unlock-a".into(),
        unlocks_post: Some(POST.into()),
        ..Default::default()
    });
    roundtrip(TierItem {
        name: "unlock-a".into(),
        rank: 9,
        description: None,
        price_hint: None,
        payment_url: None,
        auto_approve: false,
        created_at: Timestamp(1_700_000_000_000_000),
        unlocks_post: Some(POST.into()),
        ..Default::default()
    });

    // An older peer's encoding carries no `unlocks_post` key at all; it must
    // decode as an ordinary undesignated tier rather than failing strictly.
    let pre_field = TierCreateRequestPreField {
        name: "gold".into(),
        rank: 1,
        auto_approve: true,
        encrypted_upload: birth_upload(),
    };
    let bytes = encode_canonical(&pre_field).expect("encode pre-field");
    let decoded: TierCreateRequest = decode(bytes.as_ref()).expect("pre-field create decodes");
    assert_eq!(decoded.name, "gold");
    assert_eq!(
        decoded.unlocks_post, None,
        "a pre-field create must land undesignated, never fail to decode"
    );
}

/// The machine-comparable asking price (`monetization.md` § The asking price)
/// survives the wire on all three tier types, and is **additive**: a pre-field
/// encoding from an older peer still decodes, unpriced.
///
/// The **unknown-unit** case is the load-bearing one. The ratified rule says a
/// unit this build does not know compares as *not met*, fail-closed — which
/// presupposes the value round-trips through a peer that cannot interpret it.
/// If an unknown unit were dropped or rejected on the wire, an older nest
/// would silently blank a newer client's price instead of merely refusing to
/// act on it.
#[test]
fn the_asking_price_roundtrips_and_decodes_additively() {
    let msat = TierAskingPrice {
        value: 21_000,
        unit: "msat".into(),
        ..Default::default()
    };
    // A denomination from the future: this build has no idea what it means and
    // must still carry it byte-for-byte.
    let unknown = TierAskingPrice {
        value: 499,
        unit: "future-coin".into(),
        ..Default::default()
    };

    roundtrip(TierCreateRequest {
        asking_price: Some(msat.clone()),
        ..tier_create("unlock-b", 9)
    });
    roundtrip(TierUpdateRequest {
        name: "unlock-b".into(),
        asking_price: Some(unknown.clone()),
        ..Default::default()
    });
    roundtrip(TierItem {
        name: "unlock-b".into(),
        rank: 9,
        asking_price: Some(unknown),
        ..Default::default()
    });

    // Zero is a real price ("any amount in this unit buys it"), and must not
    // collapse into "no price" across the wire — `Some(0)` and `None` are
    // different tiers.
    roundtrip(TierCreateRequest {
        asking_price: Some(TierAskingPrice {
            value: 0,
            unit: "msat".into(),
            ..Default::default()
        }),
        ..tier_create("name-your-price", 9)
    });

    // An older peer's encoding carries no `asking_price` key at all.
    let pre_field = TierCreateRequestPreField {
        name: "gold".into(),
        rank: 1,
        auto_approve: true,
        encrypted_upload: birth_upload(),
    };
    let bytes = encode_canonical(&pre_field).expect("encode pre-field");
    let decoded: TierCreateRequest = decode(bytes.as_ref()).expect("pre-field create decodes");
    assert_eq!(
        decoded.asking_price, None,
        "a pre-field create must land unpriced — which is the ratified permanent \
         behavior for an unpriced tier, not a degraded one"
    );
}

/// The `tiers.create` request exactly as a client predating the per-post
/// unlock designation, the asking price and `hidden` encodes it (the birth
/// envelope included — it is required).
#[derive(serde::Serialize)]
struct TierCreateRequestPreField {
    name: String,
    rank: u32,
    auto_approve: bool,
    encrypted_upload: EncryptedKeyBlobUpload,
}

#[test]
fn tier_create_reply_roundtrip() {
    roundtrip(TierCreateReply {
        created: true,
        extra: Default::default(),
    });
    roundtrip(TierCreateReply {
        created: false,
        extra: Default::default(),
    });
}

// ── tiers.update ───────────────────────────────────────────────

#[test]
fn tier_update_request_all_none_roundtrip() {
    roundtrip(TierUpdateRequest {
        name: "silver".into(),
        rank: None,
        description: None,
        price_hint: None,
        payment_url: None,
        auto_approve: None,
        unlocks_post: None,
        ..Default::default()
    });
}

#[test]
fn tier_update_request_all_some_roundtrip() {
    roundtrip(TierUpdateRequest {
        name: "silver".into(),
        rank: Some(2),
        description: Some("Silver tier".into()),
        price_hint: Some("$3/month".into()),
        payment_url: Some("https://example.com/silver".into()),
        auto_approve: Some(false),
        unlocks_post: None,
        ..Default::default()
    });
}

#[test]
fn tier_update_reply_roundtrip() {
    roundtrip(TierUpdateReply {
        updated: true,
        extra: Default::default(),
    });
    roundtrip(TierUpdateReply {
        updated: false,
        extra: Default::default(),
    });
}

// ── tiers.delete ───────────────────────────────────────────────

#[test]
fn tier_delete_request_roundtrip() {
    roundtrip(TierDeleteRequest {
        name: "bronze".into(),
        extra: Default::default(),
    });
}

#[test]
fn tier_delete_reply_roundtrip() {
    roundtrip(TierDeleteReply {
        deleted: true,
        extra: Default::default(),
    });
    roundtrip(TierDeleteReply {
        deleted: false,
        extra: Default::default(),
    });
}

// ── subscribe ─────────────────────────────────────────────────

#[test]
fn subscribe_request_roundtrip() {
    roundtrip(SubscribeRequest {
        author_id: ActorId([1u8; 32]),
        tier: "gold".into(),
        mlkem_encaps_key: None,
        extra: Default::default(),
    });
}

#[test]
fn subscribe_request_with_mlkem_ek_roundtrip() {
    // Post-quantum surface B (S4): a subscriber publishes their 1184-byte
    // ML-KEM-768 encapsulation key on the subscribe request.
    roundtrip(SubscribeRequest {
        author_id: ActorId([1u8; 32]),
        tier: "gold".into(),
        mlkem_encaps_key: Some(ByteBuf::from(vec![0xABu8; 1184])),
        extra: Default::default(),
    });
}

#[test]
fn subscribe_request_mlkem_ek_absent_on_wire_when_none() {
    // Forward-compat: a new client with no published ek encodes byte-identically
    // to the old shape (skip_serializing_if), so an old nest reads the old shape.
    let with = SubscribeRequest {
        author_id: ActorId([1u8; 32]),
        tier: "gold".into(),
        mlkem_encaps_key: None,
        extra: Default::default(),
    };
    let bytes = encode_canonical(&with).expect("encode");
    let decoded: SubscribeRequest = decode(bytes.as_ref()).expect("decode");
    assert_eq!(decoded.mlkem_encaps_key, None);
    // The key must not appear on the wire when None.
    assert!(
        !bytes
            .as_ref()
            .windows(b"mlkem_encaps_key".len())
            .any(|w| w == b"mlkem_encaps_key"),
        "mlkem_encaps_key must be omitted from the wire when None"
    );
}

#[test]
fn subscribe_reply_approved_variant_roundtrip() {
    roundtrip(SubscribeReply::Approved {
        tier: "gold".into(),
        expires_at: Some(Timestamp(1_700_000_000_000_000)),
    });
    roundtrip(SubscribeReply::Approved {
        tier: "gold".into(),
        expires_at: None,
    });
}

#[test]
fn subscribe_reply_queued_variant_roundtrip() {
    roundtrip(SubscribeReply::Queued { request_id: 42 });
}

// ── unsubscribe ───────────────────────────────────────────────

#[test]
fn unsubscribe_request_roundtrip() {
    roundtrip(UnsubscribeRequest {
        author_id: ActorId([2u8; 32]),
        extra: Default::default(),
    });
}

#[test]
fn unsubscribe_reply_removed_variant_roundtrip() {
    roundtrip(UnsubscribeReply::Removed);
}

#[test]
fn unsubscribe_reply_queued_variant_roundtrip() {
    roundtrip(UnsubscribeReply::Queued { request_id: 99 });
}

// ── status.get ─────────────────────────────────────────────────

#[test]
fn status_get_request_roundtrip() {
    roundtrip(StatusGetRequest {
        author_id: ActorId([3u8; 32]),
        extra: Default::default(),
    });
}

#[test]
fn status_get_reply_with_tier_roundtrip() {
    roundtrip(StatusGetReply {
        tier: Some("gold".into()),
        expires_at: Some(Timestamp(1_700_000_000_000_000)),
        auto_approve: true,
        extra: Default::default(),
    });
}

#[test]
fn status_get_reply_no_tier_roundtrip() {
    roundtrip(StatusGetReply {
        tier: None,
        expires_at: None,
        auto_approve: false,
        extra: Default::default(),
    });
}

// ── requests.list ─────────────────────────────────────────────

#[test]
fn requests_list_request_roundtrip() {
    roundtrip(RequestsListRequest {});
}

#[test]
fn requests_list_reply_roundtrip() {
    // Mix of "subscribe" and "unsubscribe" kinds in the same vector.
    roundtrip(RequestsListReply {
        requests: vec![
            // A subscriber who published a post-quantum ek (surface B, S4b),
            // whose request a payment provider verified (Pillar 3).
            PendingRequest {
                request_id: 1,
                subscriber_id: ActorId([4u8; 32]),
                tier_name: "gold".into(),
                kind: "subscribe".into(),
                created_at: Timestamp(1_700_000_000_000_000),
                mlkem_encaps_key: Some(ByteBuf::from(vec![7u8; 1184])),
                payment_entitled: true,
                extra: Default::default(),
            },
            // A classical subscriber (no ek).
            PendingRequest {
                request_id: 2,
                subscriber_id: ActorId([5u8; 32]),
                tier_name: "silver".into(),
                kind: "unsubscribe".into(),
                created_at: Timestamp(1_700_000_001_000_000),
                mlkem_encaps_key: None,
                payment_entitled: false,
                extra: Default::default(),
            },
        ],
        extra: Default::default(),
    });
}

#[test]
fn pending_request_mlkem_ek_absent_on_wire_when_none() {
    // Forward-compat: a classical (no-ek) PendingRequest must encode to the
    // old shape — `mlkem_encaps_key` omitted entirely — so a new nest's
    // requests.list reply decodes on an old client (mirrors the SubscriberEntry
    // / SubscribeRequest gate).
    let req = PendingRequest {
        request_id: 9,
        subscriber_id: ActorId([1u8; 32]),
        tier_name: "gold".into(),
        kind: "subscribe".into(),
        created_at: Timestamp(42),
        mlkem_encaps_key: None,
        payment_entitled: false,
        extra: Default::default(),
    };
    let bytes = encode_canonical(&req).expect("encode");
    let decoded: PendingRequest = decode(bytes.as_ref()).expect("decode");
    assert_eq!(decoded.mlkem_encaps_key, None);
    assert!(
        !bytes
            .as_ref()
            .windows(b"mlkem_encaps_key".len())
            .any(|w| w == b"mlkem_encaps_key"),
        "mlkem_encaps_key must be omitted from the wire when None"
    );
}

// ── approve ───────────────────────────────────────────────────

#[test]
fn approve_request_with_encrypted_upload_roundtrip() {
    // After the sign-over-CID migration, `EncryptedKeyBlobUpload.key_blob`
    // and `.signer_auth` carry the `EmbedAsBytes` wire shape (envelope +
    // canonical bytes), not raw byte buffers.
    let blob_env: Vec<u8> = vec![0x11u8; 100];
    let blob_bytes: Vec<u8> = vec![0u8, 1, 2, 3, 0xff, 0xfe, 0xfd];
    let auth_env: Vec<u8> = vec![0x22u8; 100];
    let auth_bytes: Vec<u8> = vec![0xa0, 0xb1, 0xc2, 0xd3];
    let req = ApproveRequestRequest {
        request_id: 7,
        encrypted_upload: Some(EncryptedKeyBlobUpload {
            key_blob: fauna_core::encoding::EmbedAsBytes {
                envelope: blob_env.clone(),
                bytes: blob_bytes.clone(),
                signer_auth: None,
            },
            signer_auth: fauna_core::encoding::EmbedAsBytes {
                envelope: auth_env.clone(),
                bytes: auth_bytes.clone(),
                signer_auth: None,
            },
            extra: Default::default(),
        }),
        extra: Default::default(),
    };
    let bytes = encode_canonical(&req).expect("encode");
    let decoded: ApproveRequestRequest = decode(bytes.as_ref()).expect("decode");
    assert_eq!(decoded, req);
    // byte-identity assertion (envelope + bytes survive round-trip).
    let upload = decoded.encrypted_upload.unwrap();
    assert_eq!(upload.key_blob.envelope, blob_env);
    assert_eq!(upload.key_blob.bytes, blob_bytes);
    assert_eq!(upload.signer_auth.envelope, auth_env);
    assert_eq!(upload.signer_auth.bytes, auth_bytes);
}

#[test]
fn approve_request_without_encrypted_upload_roundtrip() {
    roundtrip(ApproveRequestRequest {
        request_id: 8,
        encrypted_upload: None,
        extra: Default::default(),
    });
}

#[test]
fn approve_request_reply_roundtrip() {
    roundtrip(ApproveRequestReply {
        subscriber: ActorId([6u8; 32]),
        tier: "gold".into(),
        key_version: 3,
        extra: Default::default(),
    });
}

// ── reject ────────────────────────────────────────────────────

#[test]
fn reject_request_request_roundtrip() {
    roundtrip(RejectRequestRequest {
        request_id: 10,
        extra: Default::default(),
    });
}

#[test]
fn reject_request_reply_roundtrip() {
    roundtrip(RejectRequestReply {
        rejected: true,
        extra: Default::default(),
    });
    roundtrip(RejectRequestReply {
        rejected: false,
        extra: Default::default(),
    });
}

// ── key_blob.get ───────────────────────────────────────────────

#[test]
fn key_blob_get_request_roundtrip() {
    roundtrip(KeyBlobGetRequest {
        author_id: ActorId([7u8; 32]),
        tier_name: "gold".into(),
        extra: Default::default(),
    });
}

#[test]
fn key_blob_get_reply_carries_bare_bytes_verbatim() {
    let blob_bytes: Vec<u8> = vec![0u8, 1, 2, 3, 0xff, 0xfe, 0xfd];
    let hash_bytes: Vec<u8> = vec![0xde, 0xad, 0xbe, 0xef, 0xca, 0xfe, 0xba, 0xbe];
    let reply = KeyBlobGetReply {
        version: 5,
        blob_hash: ByteBuf::from(hash_bytes.clone()),
        blob_data: ByteBuf::from(blob_bytes.clone()),
        extra: Default::default(),
    };
    let bytes = encode_canonical(&reply).expect("encode");
    let decoded: KeyBlobGetReply = decode(bytes.as_ref()).expect("decode");
    assert_eq!(decoded, reply);
    // byte-identity assertions
    assert_eq!(decoded.blob_hash.as_ref(), hash_bytes.as_slice());
    assert_eq!(decoded.blob_data.as_ref(), blob_bytes.as_slice());
}

// ── subscribers.list ───────────────────────────────────────────

#[test]
fn subscribers_list_request_roundtrip() {
    roundtrip(SubscribersListRequest {
        tier_name: "gold".into(),
        extra: Default::default(),
    });
}

#[test]
fn subscribers_list_reply_roundtrip() {
    roundtrip(SubscribersListReply {
        subscribers: vec![
            // A subscriber who published an ML-KEM ek (S4 hybrid-eligible)...
            SubscriberEntry {
                subscriber_id: ActorId([8u8; 32]),
                joined_at: Timestamp(1_700_000_000_000_000),
                mlkem_encaps_key: Some(ByteBuf::from(vec![0xCDu8; 1184])),
                extra: Default::default(),
            },
            // ...and one who did not (classical-only).
            SubscriberEntry {
                subscriber_id: ActorId([9u8; 32]),
                joined_at: Timestamp(1_700_000_002_000_000),
                mlkem_encaps_key: None,
                extra: Default::default(),
            },
        ],
        extra: Default::default(),
    });
}

// ── remove_subscriber ─────────────────────────────────────────

#[test]
fn remove_subscriber_request_with_encrypted_upload_roundtrip() {
    let blob_env: Vec<u8> = vec![0x11u8; 100];
    let blob_bytes: Vec<u8> = vec![0u8, 1, 2, 3, 0xff, 0xfe, 0xfd];
    let auth_env: Vec<u8> = vec![0x22u8; 100];
    let auth_bytes: Vec<u8> = vec![0xa0, 0xb1, 0xc2, 0xd3];
    roundtrip(RemoveSubscriberRequest {
        tier_name: "gold".into(),
        subscriber_id: ActorId([10u8; 32]),
        encrypted_upload: Some(EncryptedKeyBlobUpload {
            key_blob: fauna_core::encoding::EmbedAsBytes {
                envelope: blob_env,
                bytes: blob_bytes,
                signer_auth: None,
            },
            signer_auth: fauna_core::encoding::EmbedAsBytes {
                envelope: auth_env,
                bytes: auth_bytes,
                signer_auth: None,
            },
            extra: Default::default(),
        }),
        extra: Default::default(),
    });
}

#[test]
fn remove_subscriber_request_without_encrypted_upload_roundtrip() {
    roundtrip(RemoveSubscriberRequest {
        tier_name: "silver".into(),
        subscriber_id: ActorId([11u8; 32]),
        encrypted_upload: None,
        extra: Default::default(),
    });
}

#[test]
fn remove_subscriber_reply_roundtrip() {
    roundtrip(RemoveSubscriberReply {
        subscriber: ActorId([12u8; 32]),
        tier: "gold".into(),
        key_version: 7,
        extra: Default::default(),
    });
}

// ── delegate.upload ────────────────────────────────────────────

#[test]
fn delegate_upload_request_roundtrip() {
    let auth_env: Vec<u8> = vec![0x33u8; 100];
    let auth_bytes: Vec<u8> = vec![0u8, 1, 2, 3, 0xff, 0xfe, 0xfd];
    let req = DelegateUploadRequest {
        authorization: fauna_core::encoding::EmbedAsBytes {
            envelope: auth_env.clone(),
            bytes: auth_bytes.clone(),
            signer_auth: None,
        },
        extra: Default::default(),
    };
    let bytes = encode_canonical(&req).expect("encode");
    let decoded: DelegateUploadRequest = decode(bytes.as_ref()).expect("decode");
    assert_eq!(decoded, req);
    // byte-identity assertion
    assert_eq!(decoded.authorization.envelope, auth_env);
    assert_eq!(decoded.authorization.bytes, auth_bytes);
}

#[test]
fn delegate_upload_reply_roundtrip() {
    roundtrip(DelegateUploadReply {
        uploaded: true,
        extra: Default::default(),
    });
    roundtrip(DelegateUploadReply {
        uploaded: false,
        extra: Default::default(),
    });
}

// ── post_unlock.get ────────────────────────────────────────────

#[test]
fn post_unlock_get_request_roundtrip() {
    roundtrip(PostUnlockGetRequest {
        author_id: ActorId([7u8; 32]),
        post_id: "11".repeat(32),
        extra: Default::default(),
    });
}

#[test]
fn post_unlock_get_reply_roundtrips_both_arms() {
    roundtrip(PostUnlockGetReply {
        offer: None,
        extra: Default::default(),
    });
    roundtrip(PostUnlockGetReply {
        offer: Some(PostUnlockOffer {
            tier_name: "post-unlock-00aa11bb22cc33dd".into(),
            price_hint: Some("$3".into()),
            payment_url: Some("https://pay.example/x".into()),
            extra: Default::default(),
        }),
        extra: Default::default(),
    });
    // The priceless arm — a designated tier whose author set no public fields
    // still names the tier the claim call needs.
    roundtrip(PostUnlockGetReply {
        offer: Some(PostUnlockOffer {
            tier_name: "post-unlock-ffee".into(),
            price_hint: None,
            payment_url: None,
            extra: Default::default(),
        }),
        extra: Default::default(),
    });
}

/// The `hidden` flag (`monetization.md` § The unifying model — the hidden
/// owner-only tier) rides create + item and is **additive**: a pre-field
/// encoding decodes with `hidden == false`, which is every existing tier.
#[test]
fn the_hidden_flag_roundtrips_and_decodes_additively() {
    roundtrip(TierCreateRequest {
        hidden: true,
        ..tier_create("only-me", u32::MAX)
    });
    roundtrip(TierItem {
        name: "only-me".into(),
        rank: u32::MAX,
        hidden: true,
        ..Default::default()
    });

    let pre_field = TierCreateRequestPreField {
        name: "gold".into(),
        rank: 1,
        auto_approve: true,
        encrypted_upload: birth_upload(),
    };
    let bytes = encode_canonical(&pre_field).expect("encode pre-field");
    let decoded: TierCreateRequest = decode(bytes.as_ref()).expect("pre-field create decodes");
    assert!(
        !decoded.hidden,
        "a pre-field create is an ordinary, offered tier"
    );
}
