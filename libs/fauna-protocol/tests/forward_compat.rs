//! Future-compat: synthesize frames with unknown kinds + extra fields.
//! Verify decoders preserve them as Unknown / extra-keys.

use std::collections::BTreeMap;

use fauna_protocol::Value;
use fauna_protocol::codec::{decode_strict as decode, encode_canonical};
use fauna_protocol::envelope::{Frame, Push, decode_frame, encode_frame};
use fauna_protocol::push_events::{KnockPayload, PushEvent};

#[test]
fn unknown_kind_falls_through_to_unknown_variant() {
    let push = Frame::Push(Push {
        ty: Push::TYPE,
        kind: "com.acme.experimental.notify".into(),
        payload: Value::Map(BTreeMap::from([("custom".to_string(), Value::Integer(42))])),
        seq: 1,
    });
    let bytes = encode_frame(&push).unwrap();
    let frame = decode_frame(&bytes).unwrap();
    match frame {
        Frame::Push(p) => {
            let event = PushEvent::from_push(&p.kind, p.payload);
            match event {
                PushEvent::Unknown(u) => {
                    assert_eq!(u.kind, "com.acme.experimental.notify");
                }
                other => panic!("expected Unknown, got {:?}", other),
            }
        }
        _ => panic!("expected Push"),
    }
}

#[test]
fn extra_fields_in_known_payload_preserved() {
    // Synthesize a Knock payload with an extra unknown field.
    let v = Value::Map(BTreeMap::from([
        ("sender_id".to_string(), Value::String("abc".into())),
        ("summary".to_string(), Value::String("hi".into())),
        ("future_field".to_string(), Value::Integer(99)),
    ]));
    let bytes = encode_canonical(&v).unwrap();
    let p: KnockPayload = decode(&bytes).unwrap();
    assert_eq!(p.sender_id, "abc");
    assert_eq!(p.summary, "hi");
    assert!(p.extra.contains_key("future_field"));
    assert_eq!(p.extra.get("future_field"), Some(&Value::Integer(99)));
}

#[test]
fn extra_fields_round_trip() {
    let mut p = KnockPayload {
        sender_id: "abc".into(),
        summary: "hi".into(),
        ..Default::default()
    };
    p.extra.insert("future_field".into(), Value::Integer(99));
    let bytes = encode_canonical(&p).unwrap();
    let decoded: KnockPayload = decode(&bytes).unwrap();
    assert_eq!(decoded, p);
}

// ── Universal `extra` catch-all sweep (version-compatibility.md Dim 2 / § 5 item 6) ──
//
// The catch-all was extended to every genuine client↔nest wire-payload struct
// across the payload modules. `bridge_routing` is excluded only where it really
// is the version-locked, in-image mail-bridge data-plane; its APP-CALLABLE kinds
// (the `admin-mail` read/write twins — `get_mail_config`, the five
// `put_<substruct>_policy` writes, the alias twin, `rotate_srs_secret`) carry the
// catch-all like any other client↔nest struct, since an app decodes them and
// rule 4's in-image opt-out does not reach that wire. Owner: transport.md
// § Schema and forward-compat discipline, rule 4.
// These representative tests assert both forward-compat directions on one struct
// per category — a content row (bluesky), an admin row (dns), and a
// bridge-service request (wrapped_blob) — proving a newer peer's unknown field
// survives decode→encode rather than being silently dropped on relay.

use fauna_protocol::bluesky::BlueskyImage;
use fauna_protocol::dns::DnsRecordView;
use fauna_protocol::wrapped_blob::GetMailServingEnabledRequest;

#[test]
fn bluesky_image_decodes_and_reemits_unknown_field() {
    // A newer peer adds `aspect_ratio` to the image embed.
    let v = Value::Map(BTreeMap::from([
        ("thumb".to_string(), Value::String("t".into())),
        ("fullsize".to_string(), Value::String("f".into())),
        ("alt".to_string(), Value::String("a".into())),
        ("aspect_ratio".to_string(), Value::Integer(177)),
    ]));
    let bytes = encode_canonical(&v).unwrap();
    let img: BlueskyImage = decode(&bytes).unwrap();
    assert_eq!(img.alt, "a");
    assert_eq!(img.extra.get("aspect_ratio"), Some(&Value::Integer(177)));
    // Re-emit: the unknown field must round-trip out again (the relay guarantee).
    let reencoded = encode_canonical(&img).unwrap();
    let back: BlueskyImage = decode(&reencoded).unwrap();
    assert_eq!(back.extra.get("aspect_ratio"), Some(&Value::Integer(177)));
}

#[test]
fn dns_record_view_round_trips_extra() {
    let mut rec = DnsRecordView {
        name: "mail".into(),
        record_type: "A".into(),
        expected: "203.0.113.7".into(),
        ttl_seconds: 300,
        ..Default::default()
    };
    rec.extra
        .insert("future_axis".into(), Value::String("x".into()));
    let bytes = encode_canonical(&rec).unwrap();
    let decoded: DnsRecordView = decode(&bytes).unwrap();
    assert_eq!(decoded, rec);
    assert_eq!(
        decoded.extra.get("future_axis"),
        Some(&Value::String("x".into()))
    );
}

#[test]
fn wrapped_blob_request_round_trips_extra() {
    // The bridge-service surface is included in the sweep (not deliberately
    // strict like bridge_routing); a newer field on this request survives
    // decode→encode.
    let mut req = GetMailServingEnabledRequest::default();
    req.extra.insert("newer_flag".into(), Value::Integer(1));
    let bytes = encode_canonical(&req).unwrap();
    let decoded: GetMailServingEnabledRequest = decode(&bytes).unwrap();
    assert_eq!(decoded.extra.get("newer_flag"), Some(&Value::Integer(1)));
}

/// The account-state feed reply's `replica_id` (`account-sync-plane.md`
/// § The bind leg, ruling 2) is additive both ways: a reply from a non-class-2
/// arm, which carries none, decodes to `None` — read as "unknown replica" —
/// and a class-2 reply round-trips it, while a reader that does not know
/// the field keeps it as an extra key rather than refusing the frame.
#[test]
fn a_feed_reply_without_a_replica_id_decodes_and_one_with_it_round_trips() {
    use fauna_protocol::sync::SyncChangesListReply;
    let old = Value::Map(BTreeMap::from([
        ("changes".to_string(), Value::List(Vec::new())),
        ("complete_through_seq".to_string(), Value::Integer(7)),
    ]));
    let reply: SyncChangesListReply = decode(&encode_canonical(&old).unwrap()).unwrap();
    assert_eq!(reply.replica_id, None);
    assert_eq!(reply.complete_through_seq, Some(7));

    let new = SyncChangesListReply {
        replica_id: Some(serde_bytes::ByteBuf::from(vec![0xAB; 16])),
        ..Default::default()
    };
    let bytes = encode_canonical(&new).unwrap();
    let back: SyncChangesListReply = decode(&bytes).unwrap();
    assert_eq!(back, new);
    let as_value: Value = decode(&bytes).unwrap();
    let Value::Map(map) = as_value else {
        panic!("a reply is a map")
    };
    assert!(map.contains_key("replica_id"), "the wire key is replica_id");
}
