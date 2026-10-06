//! Rule 3's unknown arms (`transport.md` § Schema and forward-compat
//! discipline, *Rule 3 in full*), one test per enum: a newer writer is modelled
//! as a twin enum with one variant this build does not know; its bytes must
//! decode — the containing value included — into the `Unknown` arm, which a
//! collapsing arm refuses to re-emit and a carrying arm re-emits byte for byte.
//! The restrictive behaviour each arm owes lives with its reader and is pinned
//! there (the email filter and abuse-report subject arms in their own modules,
//! the nest and the mail bridge for their projections).

use fauna_protocol::admin::{AdminLogEntry, AdminLogLevel};
use fauna_protocol::bluesky::{BlueskyFacet, BlueskyFacetType};
use fauna_protocol::codec::{decode_strict as decode, encode_canonical};
use fauna_protocol::linkpreview::LinkPreviewResolveReply;
use fauna_protocol::stats::StatsGetReply;
use fauna_protocol::subscriptions::{SubscribeReply, UnsubscribeReply};
use fauna_protocol::tls::CertHealthState;
use fauna_protocol::wrapped_blob::PutSpamModelOutcome;
use serde::{Deserialize, Serialize};

/// Encode a newer writer's value and decode it as this build's type.
fn read_as<T: serde::de::DeserializeOwned>(newer: &impl Serialize) -> (T, Vec<u8>) {
    let bytes = encode_canonical(newer).expect("encode").to_vec();
    (
        decode(&bytes).expect("an unknown variant must not fail the decode"),
        bytes,
    )
}

#[test]
fn admin_log_level_collapses_inside_its_entry() {
    #[derive(Serialize)]
    enum NewerLevel {
        Fatal,
    }
    #[derive(Serialize)]
    struct NewerEntry {
        timestamp_ms: i64,
        level: NewerLevel,
        target: String,
        message: String,
    }
    let (entry, _): (AdminLogEntry, _) = read_as(&NewerEntry {
        timestamp_ms: 1,
        level: NewerLevel::Fatal,
        target: "fauna_nest".into(),
        message: "m".into(),
    });
    assert_eq!(entry.level, AdminLogLevel::Unknown);
    assert_eq!(entry.message, "m");
    assert!(encode_canonical(&AdminLogLevel::Unknown).is_err());
}

#[test]
fn bluesky_facet_type_is_carried_byte_for_byte_inside_its_facet() {
    #[derive(Serialize)]
    enum NewerFacetType {
        Highlight { colour: String },
    }
    #[derive(Serialize)]
    struct NewerFacet {
        start: u64,
        end: u64,
        facet_type: NewerFacetType,
    }
    let (facet, bytes): (BlueskyFacet, _) = read_as(&NewerFacet {
        start: 0,
        end: 4,
        facet_type: NewerFacetType::Highlight {
            colour: "red".into(),
        },
    });
    assert!(matches!(facet.facet_type, BlueskyFacetType::Unknown(_)));
    assert_eq!(facet.end, 4);
    assert_eq!(encode_canonical(&facet).unwrap().to_vec(), bytes);
}

#[test]
fn link_preview_outcome_collapses() {
    #[derive(Serialize)]
    #[serde(tag = "outcome", rename_all = "snake_case")]
    enum NewerReply {
        Paywalled { site: String },
    }
    let (reply, _): (LinkPreviewResolveReply, _) = read_as(&NewerReply::Paywalled {
        site: "example.com".into(),
    });
    assert_eq!(reply, LinkPreviewResolveReply::Unknown);
    assert!(encode_canonical(&LinkPreviewResolveReply::Unknown).is_err());
}

#[test]
fn stats_scope_collapses() {
    #[derive(Serialize)]
    #[serde(tag = "scope", rename_all = "snake_case")]
    enum NewerReply {
        Device { device_id: String, total_blobs: i64 },
    }
    let (reply, _): (StatsGetReply, _) = read_as(&NewerReply::Device {
        device_id: "d".into(),
        total_blobs: 3,
    });
    assert_eq!(reply, StatsGetReply::Unknown);
    assert!(encode_canonical(&StatsGetReply::Unknown).is_err());
}

#[test]
fn subscribe_and_unsubscribe_outcomes_collapse() {
    #[derive(Serialize)]
    #[serde(tag = "outcome", rename_all = "snake_case")]
    enum NewerReply {
        Waitlisted { position: i64 },
        Deferred,
    }
    let (sub, _): (SubscribeReply, _) = read_as(&NewerReply::Waitlisted { position: 4 });
    assert_eq!(sub, SubscribeReply::Unknown);
    let (unsub, _): (UnsubscribeReply, _) = read_as(&NewerReply::Deferred);
    assert_eq!(unsub, UnsubscribeReply::Unknown);
    assert!(encode_canonical(&SubscribeReply::Unknown).is_err());
    assert!(encode_canonical(&UnsubscribeReply::Unknown).is_err());
}

#[test]
fn cert_health_state_collapses() {
    #[derive(Serialize)]
    enum NewerState {
        ValidTrusted,
        Revoked,
    }
    let (states, _): (Vec<CertHealthState>, _) =
        read_as(&vec![NewerState::ValidTrusted, NewerState::Revoked]);
    assert_eq!(
        states,
        vec![CertHealthState::ValidTrusted, CertHealthState::Unknown]
    );
    assert!(encode_canonical(&CertHealthState::Unknown).is_err());
}

#[test]
fn put_spam_model_outcome_collapses() {
    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum NewerOutcome {
        Throttled,
    }
    let (outcome, _): (PutSpamModelOutcome, _) = read_as(&NewerOutcome::Throttled);
    assert_eq!(outcome, PutSpamModelOutcome::Unknown);
    assert!(encode_canonical(&PutSpamModelOutcome::Unknown).is_err());
}
