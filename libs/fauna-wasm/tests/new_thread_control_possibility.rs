//! `newThreadControlPossibility` — the wasm twin of
//! `fauna_conversations::new_thread_control_possibility` the SPA paints the
//! new-thread composer's Send, attach and topic from
//! (`docs/goal/ui/conversations.md` § Errors & edge cases → *The new-thread
//! composer offers only what it can send*). Pinned through the JS shapes the
//! SPA hands it: the snapshot's `new_thread_compose` object (`null` when no
//! composer is open) and a snake_case control token, answered in
//! `Possibility`'s serde form `#lib/possibility` reads. `run_in_browser`: this
//! project has no Node.js.

use fauna_conversations::TypedAddress;
use fauna_conversations::compose::{ComposeState, RecipientPickerState};
use fauna_wasm::new_thread_control_possibility;
use serde_json::{Value, json};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

fn js<T: serde::Serialize>(value: &T) -> JsValue {
    serde::Serialize::serialize(value, &serde_wasm_bindgen::Serializer::json_compatible())
        .expect("serializes")
}

fn answer(compose: JsValue, control: &str) -> Value {
    let out = new_thread_control_possibility(compose, JsValue::from_str(control))
        .expect("a known control answers");
    serde_wasm_bindgen::from_value(out).expect("the answer is plain JSON")
}

fn compose_with(chips: Vec<TypedAddress>, raw_input: &str) -> JsValue {
    js(&ComposeState {
        recipient_picker: Some(RecipientPickerState {
            raw_input: raw_input.to_string(),
            chips,
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[wasm_bindgen_test]
fn no_composer_is_absent() {
    for control in ["send", "attach", "topic"] {
        assert_eq!(
            answer(JsValue::NULL, control),
            json!({"state": "absent"}),
            "{control}"
        );
    }
}

#[wasm_bindgen_test]
fn send_waits_for_a_recipient_while_attach_and_topic_take_the_draft() {
    let empty = compose_with(Vec::new(), "");
    let send = answer(empty.clone(), "send");
    assert_eq!(send["state"], "blocked", "{send}");
    assert_eq!(
        send["reason"]["key"], "conversations.compose.needs_recipient",
        "{send}"
    );
    for control in ["attach", "topic"] {
        assert_eq!(
            answer(empty.clone(), control),
            json!({"state": "possible"}),
            "{control}"
        );
    }
    assert_eq!(
        answer(compose_with(Vec::new(), "someone@example.test"), "send"),
        json!({"state": "possible"}),
        "a typed recipient is enough — Send flushes it"
    );
}

#[wasm_bindgen_test]
fn a_bridged_recipient_blocks_attach_and_topic_with_their_reasons() {
    let bridged = compose_with(
        vec![TypedAddress::Bridged {
            bridge_id: "bridge".to_string(),
            address: "far".to_string(),
        }],
        "",
    );
    assert_eq!(
        answer(bridged.clone(), "send"),
        json!({"state": "possible"})
    );
    let attach = answer(bridged.clone(), "attach");
    assert_eq!(
        attach["reason"]["key"], "conversations.compose.no_attachments_here",
        "{attach}"
    );
    let topic = answer(bridged, "topic");
    assert_eq!(
        topic["reason"]["key"], "conversations.compose.no_topic_here",
        "{topic}"
    );
}

#[wasm_bindgen_test]
fn an_unknown_control_token_is_refused_not_guessed() {
    assert!(new_thread_control_possibility(JsValue::NULL, JsValue::from_str("fly")).is_err());
}
