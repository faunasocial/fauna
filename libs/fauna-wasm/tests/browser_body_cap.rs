//! The browser readers' body caps, end to end through the public fetcher a
//! foreign home is dialled with (`WasmPublicChunkFetcher`): a stranger's nest
//! that answers with a chunked body — no `Content-Length` — must not make the
//! tab buffer all of it, on a 2xx (`read_body_capped`) or on an error
//! (`read_error_body`, which bounds the diagnostic text at
//! `ERROR_BODY_READ_CAP`). `transport-trust.md` § Transport trust owns the
//! stance: a pinned home nest is authenticated, not trusted to behave.
//!
//! The reply is a JS `Response` over a `ReadableStream` that would run on for
//! 100 chunks; the global `fetch` is swapped for one that returns it (the
//! reader's `gloo-net` call looks `fetch` up at send time), so the real reader
//! code runs unchanged. A reader that buffered the body would pull every chunk;
//! one that streams under the cap cancels after a few, which the stream's pull
//! counter shows. No new manifest entry: the stream is driven through
//! `js_sys`. `run_in_browser`: this project has no Node.js.

use fauna_core::chunker::MAX_MANIFEST_BODY;
use fauna_core::file_download::{BlobFetcher, ERROR_BODY_READ_CAP, WasmPublicChunkFetcher};
use js_sys::{Function, Reflect};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

const CHUNKS: u32 = 100;

/// Install a `fetch` answering `status` with `CHUNKS` chunks of `chunk_len`
/// bytes and no `Content-Length`; returns the stream state (`pulls`) and the
/// original `fetch` to restore.
fn install_fetch(status: u32, chunk_len: u64) -> (JsValue, JsValue) {
    let install = Function::new_with_args(
        "status, chunkLen, chunks",
        "const state = { pulls: 0 };
         const original = globalThis.fetch;
         globalThis.fetch = () => Promise.resolve(new Response(
             new ReadableStream({
                 pull(controller) {
                     if (state.pulls >= chunks) { controller.close(); return; }
                     state.pulls += 1;
                     controller.enqueue(new Uint8Array(chunkLen).fill(0x41));
                 },
             }),
             { status },
         ));
         return { state, original };",
    );
    let out = install
        .call3(
            &JsValue::NULL,
            &JsValue::from_f64(f64::from(status)),
            &JsValue::from_f64(chunk_len as f64),
            &JsValue::from_f64(f64::from(CHUNKS)),
        )
        .expect("install the fetch stand-in");
    let get = |k: &str| Reflect::get(&out, &JsValue::from_str(k)).expect(k);
    (get("state"), get("original"))
}

fn restore_fetch(original: &JsValue) {
    let global = js_sys::global();
    Reflect::set(&global, &JsValue::from_str("fetch"), original).expect("restore fetch");
}

fn pulls(state: &JsValue) -> u32 {
    Reflect::get(state, &JsValue::from_str("pulls"))
        .expect("pulls")
        .as_f64()
        .expect("pulls is a number") as u32
}

fn hash() -> fauna_core::data::ContentHash {
    fauna_core::data::ContentHash::of_raw(b"manifest")
}

/// An over-cap 2xx body with no `Content-Length` is refused and the stream is
/// cancelled long before it ends.
#[wasm_bindgen_test]
async fn over_cap_2xx_body_without_content_length_is_cut_off() {
    // Two chunks of this size pass the manifest cap; the first alone does not.
    let (state, original) = install_fetch(200, MAX_MANIFEST_BODY / 2 + 1);
    let result = WasmPublicChunkFetcher::new("https://stranger.invalid")
        .fetch_manifest(&hash())
        .await;
    restore_fetch(&original);
    let err = result.expect_err("an over-cap body must be refused");
    assert!(
        err.to_string().contains("size cap"),
        "refused for the cap, got: {err}"
    );
    assert!(
        pulls(&state) < 10,
        "the read must stop at the cap, pulled {} of {CHUNKS} chunks",
        pulls(&state)
    );
}

/// An over-cap error body with no `Content-Length` is cut off at
/// `ERROR_BODY_READ_CAP`: the refusal is the empty message, and the stream is
/// cancelled long before it ends.
#[wasm_bindgen_test]
async fn over_cap_error_body_without_content_length_is_cut_off() {
    let (state, original) = install_fetch(500, 1024);
    let result = WasmPublicChunkFetcher::new("https://stranger.invalid")
        .fetch_manifest(&hash())
        .await;
    restore_fetch(&original);
    let err = result.expect_err("a 500 is an error");
    assert!(
        err.to_string().ends_with("(500): "),
        "an over-cap error body yields the empty message, got: {err}"
    );
    assert!(
        pulls(&state) < 20,
        "the error read must stop at the {ERROR_BODY_READ_CAP}-byte cap, pulled {} of {CHUNKS} chunks",
        pulls(&state)
    );
}
