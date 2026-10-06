use fauna_core::data::{ContentHash, Post, Timestamp};
use fauna_core::identity::ActorId;
use fauna_core::subscription::types::*;

#[test]
fn gated_info_roundtrip() {
    let info = GatedInfo {
        encrypted_ref: ContentHash::from_digest_raw([1u8; 32]),
        key_access: KeyAccess::Broadcast {
            key_blob_ref: ContentHash::from_digest_raw([2u8; 32]),
        },
        tier: "Gold".to_string(),
        tier_rank: 2,
        seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
        attachment_refs: vec![],
    };

    let json = serde_json::to_string(&info).unwrap();
    let deserialized: GatedInfo = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.tier, "Gold");
    assert_eq!(deserialized.tier_rank, 2);
    assert!(matches!(
        deserialized.key_access,
        KeyAccess::Broadcast { .. }
    ));
}

/// `attachment_refs` (the 2026-09-08 floor ruling) is additive: a record
/// that carries none encodes byte-identically to the pre-field shape — so
/// every existing post id and signature stays valid — and a pre-field
/// decoder's bytes decode with the list empty. A record that carries the
/// list round-trips it in order through the canonical wire encoding.
#[test]
fn gated_info_attachment_refs_are_additive_and_round_trip() {
    use fauna_core::encoding::{canonical_decode, canonical_encode};

    let bare = GatedInfo {
        encrypted_ref: ContentHash::from_digest_raw([1u8; 32]),
        key_access: KeyAccess::Broadcast {
            key_blob_ref: ContentHash::from_digest_raw([2u8; 32]),
        },
        tier: "Gold".to_string(),
        tier_rank: 2,
        seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
        attachment_refs: vec![],
    };
    let bare_bytes = canonical_encode(&bare).unwrap();
    assert!(
        !String::from_utf8_lossy(&bare_bytes).contains("attachment_refs"),
        "an empty list must be skipped so pre-field records stay byte-identical"
    );
    let decoded: GatedInfo = canonical_decode(&bare_bytes).unwrap();
    assert_eq!(decoded, bare);

    let photo = ContentHash::from_digest_raw([7u8; 32]);
    let thumb = ContentHash::from_digest_raw([8u8; 32]);
    let with_refs = GatedInfo {
        attachment_refs: vec![photo, thumb],
        ..bare.clone()
    };
    let bytes = canonical_encode(&with_refs).unwrap();
    let decoded: GatedInfo = canonical_decode(&bytes).unwrap();
    assert_eq!(decoded.attachment_refs, vec![photo, thumb]);
    assert_eq!(decoded, with_refs);
}

#[test]
fn subscription_tier_roundtrip() {
    let tier = SubscriptionTier {
        author: ActorId([0u8; 32]),
        name: "Pro".to_string(),
        description: Some("Weekly essays + archive".to_string()),
        rank: 2,
        price_hint: Some("$10/month".to_string()),
        payment_url: Some("https://pay.example.com/pro".to_string()),
        created_at: Timestamp(1000000),
        signature: vec![0u8; 64],
    };

    let json = serde_json::to_string(&tier).unwrap();
    let deserialized: SubscriptionTier = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.name, "Pro");
    assert_eq!(deserialized.rank, 2);
}

#[test]
fn subscribe_request_roundtrip() {
    let req = SubscribeRequest {
        subscriber: ActorId([1u8; 32]),
        author: ActorId([2u8; 32]),
        tier: "Gold".to_string(),
        created_at: Timestamp(2000000),
        signature: vec![0u8; 64],
    };

    let json = serde_json::to_string(&req).unwrap();
    let deserialized: SubscribeRequest = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.tier, "Gold");
}

#[test]
fn key_access_room_variant() {
    let access = KeyAccess::Room {
        group_id: MlsGroupId(vec![42u8; 32]),
        epoch: 7,
        generation: None,
    };
    let json = serde_json::to_string(&access).unwrap();
    let deserialized: KeyAccess = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized, access);
}

#[test]
fn post_with_gated_info() {
    let post = Post {
        author: ActorId([0u8; 32]),
        created_at: Timestamp(1000000),
        body: fauna_core::data::PostBody::Text {
            content: "Preview: subscribe for more!".to_string(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: Some(GatedInfo {
            encrypted_ref: ContentHash::from_digest_raw([3u8; 32]),
            key_access: KeyAccess::Broadcast {
                key_blob_ref: ContentHash::from_digest_raw([4u8; 32]),
            },
            tier: "Pro".to_string(),
            tier_rank: 2,
            seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
            attachment_refs: vec![],
        }),
        content_warning: None,
        origin: None,
    };

    let json = serde_json::to_string(&post).unwrap();
    let deserialized: Post = serde_json::from_str(&json).unwrap();

    assert!(deserialized.gated.is_some());
    assert_eq!(deserialized.gated.unwrap().tier, "Pro");
}

#[test]
fn post_without_gated_info() {
    let post = Post {
        author: ActorId([0u8; 32]),
        created_at: Timestamp(1000000),
        body: fauna_core::data::PostBody::Text {
            content: "Normal public post".to_string(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };

    let json = serde_json::to_string(&post).unwrap();
    let deserialized: Post = serde_json::from_str(&json).unwrap();

    assert!(deserialized.gated.is_none());
}
