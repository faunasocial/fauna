//! `threadControlPossibility` — the wasm twin of
//! `fauna_conversations::thread_control_possibility` the SPA paints every
//! role- or rail-gated thread and room control from
//! (`docs/goal/ui/conversations.md` § Errors & edge cases → *Controls this
//! person can never use*). Pinned through the JS shapes the SPA hands it: the
//! snapshot's `capabilities` object, a snake_case control token and the room's
//! `my_role` (`null` on a policy-less thread), answered in `Possibility`'s
//! serde form `#lib/possibility` reads. `run_in_browser`: this project has no
//! Node.js.

use fauna_conversations::capabilities::derive_capabilities;
use fauna_conversations::{Rail, ThreadCapabilities, ThreadFlavor};
use fauna_wasm::thread_control_possibility;
use serde_json::{Value, json};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

fn js<T: serde::Serialize>(value: &T) -> JsValue {
    serde::Serialize::serialize(value, &serde_wasm_bindgen::Serializer::json_compatible())
        .expect("serializes")
}

fn answer(caps: &ThreadCapabilities, control: &str, my_role: Option<&str>) -> Value {
    let role = my_role.map_or(JsValue::NULL, JsValue::from_str);
    let out = thread_control_possibility(js(caps), JsValue::from_str(control), role)
        .expect("a known control answers");
    serde_wasm_bindgen::from_value(out).expect("the answer is plain JSON")
}

/// A governed room's owner: every capability the gate grants an owner, and the
/// walk-out closed (a room is never owner-less).
fn owner_seat() -> ThreadCapabilities {
    ThreadCapabilities {
        can_invite: true,
        can_remove_members: true,
        can_set_policy: true,
        can_appoint_admins: true,
        can_transfer_ownership: true,
        can_leave_room: false,
        ..derive_capabilities(Rail::FaunaMls, ThreadFlavor::MlsGroup)
    }
}

/// A plain member of the same room: none of the role-granted capabilities,
/// the walk-out open.
fn member_seat() -> ThreadCapabilities {
    ThreadCapabilities {
        can_invite: false,
        can_remove_members: false,
        can_set_policy: false,
        can_appoint_admins: false,
        can_transfer_ownership: false,
        can_leave_room: true,
        ..derive_capabilities(Rail::FaunaMls, ThreadFlavor::MlsGroup)
    }
}

#[wasm_bindgen_test]
fn the_owners_walk_out_is_blocked_with_the_hand_over_sentence() {
    let got = answer(&owner_seat(), "leave_room", Some("Owner"));
    assert_eq!(got["state"], "blocked", "{got}");
    assert_eq!(
        got["reason"]["key"], "error.send.room_owner_cannot_leave",
        "{got}"
    );
    assert_eq!(
        answer(&owner_seat(), "set_policy", Some("Owner")),
        json!({"state": "possible"})
    );
}

#[wasm_bindgen_test]
fn a_member_never_gets_a_control_only_a_role_could_give() {
    for control in [
        "invite",
        "remove_member",
        "set_policy",
        "appoint_admins",
        "transfer_ownership",
    ] {
        assert_eq!(
            answer(&member_seat(), control, Some("Member")),
            json!({"state": "absent"}),
            "{control}"
        );
    }
    assert_eq!(
        answer(&member_seat(), "leave_room", Some("Member")),
        json!({"state": "possible"})
    );
}

#[wasm_bindgen_test]
fn the_rail_decides_local_remove_and_the_topic_with_no_role() {
    let bridged = derive_capabilities(Rail::Bridged, ThreadFlavor::OneToOne);
    assert_eq!(
        answer(&bridged, "local_remove", None),
        json!({"state": "possible"})
    );
    assert_eq!(
        answer(&bridged, "subject", None),
        json!({"state": "absent"})
    );
    let mls = derive_capabilities(Rail::FaunaMls, ThreadFlavor::OneToOne);
    assert_eq!(
        answer(&mls, "local_remove", None),
        json!({"state": "absent"})
    );
    assert_eq!(answer(&mls, "subject", None), json!({"state": "possible"}));
}

#[wasm_bindgen_test]
fn an_unknown_control_token_is_refused_not_guessed() {
    let caps = js(&member_seat());
    assert!(thread_control_possibility(caps, JsValue::from_str("fly"), JsValue::NULL).is_err());
}
