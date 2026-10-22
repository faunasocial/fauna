//! Property tests over the Nostr wire parsers a stranger reaches. `ClientMessage::
//! from_json` is the first thing the `/nostr` relay does with every frame (cap
//! 512 KiB, pre-auth); `RelayMessage::from_json` reads what a remote relay
//! sends the bridge's client side. In safe Rust the residual bug class is a
//! panic or unbounded work on crafted JSON, so the properties are: never panic,
//! hostile nesting and oversize search strings stay bounded, a message the
//! serializer writes parses back to itself, and the advertised NIP-11 limits are
//! the ones the relay enforces.

use crate::filter;
use crate::nip01::{ClientMessage, RelayMessage};
use crate::nip11::{
    MAX_EVENT_SIZE, MAX_FILTERS_PER_REQ, MAX_SUBSCRIPTIONS_PER_CONN, fauna_relay_info,
};
use crate::nip50::{MAX_SEARCH_BYTES, MAX_SEARCH_TOKENS, exceeds_search_caps, search_tokens};
use crate::types::{Event, Filter};
use proptest::prelude::*;
use serde_json::{Value, json};

fn hex(len: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<u8>(), len / 2)
        .prop_map(|b| b.iter().map(|x| format!("{x:02x}")).collect())
}

fn arb_event_json() -> impl Strategy<Value = Value> {
    (
        hex(64),
        hex(64),
        any::<u32>(),
        0u64..70_000,
        proptest::collection::vec(proptest::collection::vec("\\PC{0,12}", 0..4), 0..4),
        "\\PC{0,64}",
        hex(128),
    )
        .prop_map(|(id, pubkey, created_at, kind, tags, content, sig)| {
            json!({
                "id": id, "pubkey": pubkey, "created_at": created_at, "kind": kind,
                "tags": tags, "content": content, "sig": sig,
            })
        })
}

fn arb_filter_json() -> impl Strategy<Value = Value> {
    (
        proptest::option::of(proptest::collection::vec("[0-9a-f]{0,64}", 0..4)),
        proptest::option::of(proptest::collection::vec(0u64..70_000, 0..4)),
        proptest::option::of(any::<u32>()),
        proptest::option::of(any::<u32>()),
        proptest::option::of(0u64..5_000),
        proptest::option::of("\\PC{0,400}"),
    )
        .prop_map(|(ids, kinds, since, until, limit, search)| {
            let mut m = serde_json::Map::new();
            if let Some(v) = ids {
                m.insert("ids".into(), json!(v));
            }
            if let Some(v) = kinds {
                m.insert("kinds".into(), json!(v));
            }
            if let Some(v) = since {
                m.insert("since".into(), json!(v));
            }
            if let Some(v) = until {
                m.insert("until".into(), json!(v));
            }
            if let Some(v) = limit {
                m.insert("limit".into(), json!(v));
            }
            if let Some(v) = search {
                m.insert("search".into(), json!(v));
            }
            Value::Object(m)
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn client_message_never_panics_on_text(s in "\\PC{0,512}") {
        let _ = ClientMessage::from_json(&s);
        let _ = RelayMessage::from_json(&s);
    }

    /// JSON-array-shaped soup: the right verbs with wrong or missing operands.
    #[test]
    fn client_message_never_panics_on_verb_soup(
        verb in prop_oneof![
            Just("EVENT"), Just("REQ"), Just("CLOSE"), Just("COUNT"), Just("AUTH"),
            Just("OK"), Just("EOSE"), Just("NOTICE"), Just("CLOSED"), Just("bogus"),
        ],
        operands in proptest::collection::vec(
            prop_oneof![
                Just(json!(null)), Just(json!(1)), Just(json!("sub")), Just(json!({})),
                Just(json!([])), Just(json!({"kinds": "x"})), Just(json!({"ids": [1]})),
                Just(json!(true)), Just(json!(-5)), Just(json!(1.5e300)),
            ],
            0..5,
        ),
    ) {
        let mut arr = vec![json!(verb)];
        arr.extend(operands);
        let s = Value::Array(arr).to_string();
        let _ = ClientMessage::from_json(&s);
        let _ = RelayMessage::from_json(&s);
    }

    /// Nesting deep enough to threaten the stack is refused by the JSON reader.
    #[test]
    fn deep_json_is_refused_not_overflowed(depth in 129usize..20_000) {
        let s = format!("[\"REQ\",\"s\",{}{}]", "[".repeat(depth), "]".repeat(depth));
        prop_assert!(ClientMessage::from_json(&s).is_err());
    }

    #[test]
    fn serialised_messages_parse_back(
        sub in "[a-zA-Z0-9]{1,16}",
        event in arb_event_json(),
        filters in proptest::collection::vec(arb_filter_json(), 0..4),
    ) {
        let ev: Event = serde_json::from_value(event).expect("generated events are well-formed");
        for m in [
            ClientMessage::Event(ev.clone()),
            ClientMessage::Auth(ev),
            ClientMessage::Close(sub.clone()),
            ClientMessage::Req {
                subscription_id: sub.clone(),
                filters: filters.iter().map(|f| serde_json::from_value(f.clone()).unwrap()).collect(),
            },
        ] {
            let json = m.to_json();
            let back = ClientMessage::from_json(&json).expect("round trip parses");
            prop_assert_eq!(back.to_json(), json);
        }
    }

    /// Filter matching over arbitrary filters × events never panics.
    #[test]
    fn filter_matching_never_panics(event in arb_event_json(), f in arb_filter_json()) {
        let ev: Event = serde_json::from_value(event).unwrap();
        let filter: Filter = serde_json::from_value(f).unwrap();
        let _ = filter.matches(&ev);
        let _ = filter::matches_any(std::slice::from_ref(&filter), &ev);
    }

    /// The search work is bounded however long the hostile search string is.
    #[test]
    fn search_work_is_structurally_bounded(q in "\\PC{0,4096}") {
        let toks = search_tokens(&q);
        prop_assert!(toks.len() <= MAX_SEARCH_TOKENS);
        let total: usize = toks.iter().map(String::len).sum();
        // Lowercasing can lengthen a char, never by more than a small factor.
        prop_assert!(total <= 3 * MAX_SEARCH_BYTES);
        if q.len() > MAX_SEARCH_BYTES {
            prop_assert!(exceeds_search_caps(&q));
        }
    }
}

/// The NIP-11 document advertises exactly the limits the relay enforces.
#[test]
fn nip11_advertises_the_enforced_limits() {
    let info = fauna_relay_info("n", "0");
    let l = info.limitation.expect("limitation present");
    assert_eq!(l.max_message_length, Some(MAX_EVENT_SIZE as u64));
    assert_eq!(l.max_subscriptions, Some(MAX_SUBSCRIPTIONS_PER_CONN as u64));
    assert_eq!(l.max_filters, Some(MAX_FILTERS_PER_REQ as u64));
}
