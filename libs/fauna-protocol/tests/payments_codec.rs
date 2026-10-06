//! Codec round-trip tests for all `fauna.payments.*` request/reply types.
//!
//! Each type is encoded via `encode_canonical` and decoded via `decode`,
//! then compared against the original value (mirrors subscriptions_codec.rs).
//!
//! Whole-file gated on the `payments` feature: the types under test compile
//! away in an excised flavor (`dynamic-features.md` § The feature-matrix test
//! story — both flavors build and run their subset, and the excised one's
//! subset is simply this file minus itself).
#![cfg(feature = "payments")]

use fauna_core::data::Timestamp;
use fauna_core::identity::ActorId;
use fauna_protocol::payments::*;
use fauna_protocol::{decode_strict as decode, encode_canonical};

fn roundtrip<T>(value: T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = encode_canonical(&value).expect("encode");
    let decoded: T = decode(bytes.as_ref()).expect("decode");
    assert_eq!(decoded, value);
}

// ── providers.set ──────────────────────────────────────────────

#[test]
fn provider_set_request_roundtrip() {
    roundtrip(ProviderSetRequest {
        kind: "fake".into(),
        webhook_secret: "whsec_abc123".into(),
        tier: "gold".into(),
        extra: Default::default(),
    });
}

#[test]
fn provider_set_reply_roundtrip() {
    roundtrip(ProviderSetReply {
        saved: true,
        extra: Default::default(),
    });
}

// ── providers.list ─────────────────────────────────────────────

#[test]
fn providers_list_request_roundtrip() {
    roundtrip(ProvidersListRequest {});
}

#[test]
fn providers_list_reply_roundtrip() {
    roundtrip(ProvidersListReply {
        providers: vec![
            ProviderItem {
                kind: "fake".into(),
                tier: "gold".into(),
                created_at: Timestamp(1_750_000_000_000_000),
                last_verified_at: None,
                last_rejected_at: None,
                extra: Default::default(),
            },
            ProviderItem {
                kind: "stripe".into(),
                tier: "silver".into(),
                created_at: Timestamp(1_750_000_001_000_000),
                last_verified_at: Some(1_750_000_050),
                last_rejected_at: Some(1_750_000_040),
                extra: Default::default(),
            },
        ],
        extra: Default::default(),
    });
    roundtrip(ProvidersListReply::default());
}

// ── providers.remove ───────────────────────────────────────────

#[test]
fn provider_remove_request_roundtrip() {
    roundtrip(ProviderRemoveRequest {
        kind: "stripe".into(),
        extra: Default::default(),
    });
}

#[test]
fn provider_remove_reply_roundtrip() {
    roundtrip(ProviderRemoveReply {
        removed: false,
        extra: Default::default(),
    });
}

// ── claims.redeem ──────────────────────────────────────────────

#[test]
fn claim_redeem_request_roundtrip() {
    roundtrip(ClaimRedeemRequest {
        code: "ABCD2345EF".into(),
        extra: Default::default(),
    });
}

#[test]
fn claim_redeem_reply_roundtrip() {
    roundtrip(ClaimRedeemReply {
        author: ActorId([7u8; 32]),
        tier: "gold".into(),
        valid_until: Some(Timestamp(1_760_000_000_000_000)),
        queued: true,
        extra: Default::default(),
    });
    roundtrip(ClaimRedeemReply {
        author: ActorId([9u8; 32]),
        tier: "gold".into(),
        valid_until: None,
        queued: false,
        extra: Default::default(),
    });
}

// ── claims.mint ─────────────────────────────────────────────────

#[test]
fn claim_mint_request_roundtrip() {
    roundtrip(ClaimMintRequest {
        tier: "gold".into(),
        valid_until: Some(Timestamp(1_760_000_000_000_000)),
        extra: Default::default(),
    });
    roundtrip(ClaimMintRequest {
        tier: "gold".into(),
        valid_until: None,
        extra: Default::default(),
    });
}

#[test]
fn claim_mint_reply_roundtrip() {
    roundtrip(ClaimMintReply {
        code: "ABCD2345EF".into(),
        tier: "gold".into(),
        valid_until: Some(Timestamp(1_760_000_000_000_000)),
        extra: Default::default(),
    });
}

// ── claims.list ─────────────────────────────────────────────────

#[test]
fn claims_list_request_roundtrip() {
    roundtrip(ClaimsListRequest {});
}

#[test]
fn claims_list_reply_roundtrip() {
    roundtrip(ClaimsListReply {
        claims: vec![
            ClaimItem {
                code: "ABCD2345EF".into(),
                tier: "gold".into(),
                provider: "manual".into(),
                valid_until: Some(Timestamp(1_760_000_000_000_000)),
                created_at: Timestamp(1_750_000_000_000_000),
                redeemed_by: None,
                redeemed_at: None,
                voided_at: None,
                extra: Default::default(),
            },
            ClaimItem {
                code: "GH2345JKMN".into(),
                tier: "silver".into(),
                provider: "fake".into(),
                valid_until: None,
                created_at: Timestamp(1_750_000_001_000_000),
                redeemed_by: Some(ActorId([3u8; 32])),
                redeemed_at: Some(Timestamp(1_750_000_002_000_000)),
                voided_at: None,
                extra: Default::default(),
            },
            ClaimItem {
                code: "PQ2345RSTU".into(),
                tier: "gold".into(),
                provider: "manual".into(),
                valid_until: None,
                created_at: Timestamp(1_750_000_003_000_000),
                redeemed_by: None,
                redeemed_at: None,
                voided_at: Some(Timestamp(1_750_000_004_000_000)),
                extra: Default::default(),
            },
        ],
        extra: Default::default(),
    });
    roundtrip(ClaimsListReply::default());
}
