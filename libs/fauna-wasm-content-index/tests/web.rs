//! In-WASM smoke test for content-index bindings.
//!
//! Run with `wasm-pack test libs/fauna-wasm-content-index --headless --firefox`
//! (or `--chrome`). Adjust browser flag per local availability.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

use fauna_wasm_content_index::{
    content_index_add_doc, content_index_commit, content_index_create_in_ram, content_index_query,
};

#[wasm_bindgen_test]
fn round_trip() {
    content_index_create_in_ram().expect("create");
    let doc_json = serde_json::json!({
        "kind": "mail",
        "content_id": [0xAA, 0xBB, 0xCC, 0xDD],
        "timestamp_ns": 1_000,
        "sender_actor_id": null,
        "fields": [{ "kind": "body", "text": "hello from wasm" }]
    })
    .to_string();
    content_index_add_doc(&doc_json).expect("add_doc");
    content_index_commit().expect("commit");

    let args_json = serde_json::json!({
        "query": "wasm",
        "kinds": ["mail"],
        "range": null,
        "limit": 10
    })
    .to_string();
    let hits_json = content_index_query(&args_json).expect("query");
    let hits: serde_json::Value = serde_json::from_str(&hits_json).unwrap();
    assert_eq!(hits.as_array().unwrap().len(), 1);
}

#[wasm_bindgen_test]
fn empty_query_yields_no_hits() {
    content_index_create_in_ram().expect("create");
    let doc_json = serde_json::json!({
        "kind": "post",
        "content_id": [1, 2, 3],
        "timestamp_ns": 0,
        "sender_actor_id": null,
        "fields": [{ "kind": "body", "text": "anything" }]
    })
    .to_string();
    content_index_add_doc(&doc_json).expect("add_doc");
    content_index_commit().expect("commit");

    let args_json = serde_json::json!({
        "query": "",
        "kinds": ["post"],
        "range": null,
        "limit": 10
    })
    .to_string();
    let hits_json = content_index_query(&args_json).expect("query");
    let hits: serde_json::Value = serde_json::from_str(&hits_json).unwrap();
    assert_eq!(hits.as_array().unwrap().len(), 0);
}
