//! Web's serving-presence lock, through the wasm faces the SPA calls: the
//! identity subscription's take and release (`takeServingPresence`,
//! `releaseServingPresence`) and the erase's question
//! (`actorsServedByAnotherTab`, `removeAccountBlock`), which asks the
//! conversations-engine role and then this lock
//! (`fauna_client_accounts::web_serving_presence` owns the model;
//! `apps/account-scoping.md` § Concurrent instances → *Web's serving-presence
//! lock* owns the ruling).
//!
//! "Another tab" is a shared hold taken straight over the shared primitive
//! (`fauna_core::web_lock`) under the lock's one name — one browser context has
//! one wasm instance, and the lock manager cannot tell that hold from a sibling
//! tab's. `run_in_browser`: this project has no Node.js.

use fauna_client_accounts::web_serving_presence::{
    holds_serving_presence_here, web_serving_lock_name,
};
use fauna_core::identity::ActorKeypair;
use fauna_core::web_lock::{WebLockAsk, WebLockHold};
use fauna_wasm::account_scope::{
    actors_served_by_another_tab, release_serving_presence_js, remove_account_block,
    take_serving_presence_js,
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

fn new_actor() -> String {
    ActorKeypair::generate().actor_id_hex()
}

/// A shared serving hold on `actor` from outside this tab's registry — another
/// tab whose identity is that account and which runs no engine.
async fn another_tab_serves(actor: &str) -> WebLockHold {
    WebLockHold::take_shared(&web_serving_lock_name(actor))
        .await
        .expect("the other tab's shared take is granted")
}

/// Whether nobody holds `actor`'s serving lock (an exclusive try, let go).
async fn serving_lock_is_free(actor: &str) -> bool {
    match WebLockHold::try_take(&web_serving_lock_name(actor)).await {
        WebLockAsk::Held(hold) => {
            hold.release().await;
            true
        }
        _ => false,
    }
}

async fn take(actor: &str) {
    JsFuture::from(take_serving_presence_js(actor.to_string()))
        .await
        .expect("the take resolves");
}

async fn release(actor: Option<&str>) {
    JsFuture::from(release_serving_presence_js(actor.map(str::to_string)))
        .await
        .expect("the release resolves");
}

async fn served_by_another_tab(actors: &[&str]) -> Vec<String> {
    let ids = actors.iter().map(|a| a.to_string()).collect();
    let answer = JsFuture::from(actors_served_by_another_tab(ids))
        .await
        .expect("the probe never rejects");
    js_sys::Array::from(&answer)
        .iter()
        .filter_map(|v| v.as_string())
        .collect()
}

#[wasm_bindgen_test]
fn the_serving_lock_name_is_one_normalized_derivation() {
    assert_eq!(
        web_serving_lock_name("  AbC123 "),
        "fauna.accounts.serving/abc123"
    );
}

/// The gap this lock closes: a tab that holds the account's identity but not
/// its engine role is served elsewhere, though the role is free.
#[wasm_bindgen_test]
async fn a_tab_holding_the_identity_without_the_role_is_served_elsewhere() {
    let (a, b) = (new_actor(), new_actor());
    let other_tab = another_tab_serves(&a).await;
    assert_eq!(
        served_by_another_tab(&[&a, &b, &a.to_uppercase()]).await,
        vec![a.clone()],
        "a's sibling declares it; b is served by nobody"
    );
    assert_eq!(
        JsFuture::from(remove_account_block(b.clone(), None))
            .await
            .unwrap()
            .as_string(),
        None,
        "an idle account still proceeds"
    );
    assert_eq!(
        JsFuture::from(remove_account_block(a.clone(), Some(b.clone())))
            .await
            .unwrap()
            .as_string()
            .as_deref(),
        Some("other_tab"),
        "remove-account's other-tab arm gains the half through the same probe"
    );
    other_tab.release().await;
    assert!(served_by_another_tab(&[&a]).await.is_empty());
}

/// This tab's own declaration alone is not another tab: the probe puts it down,
/// finds the lock free, and the hold is taken again afterwards.
#[wasm_bindgen_test]
async fn this_tabs_own_hold_alone_does_not_refuse_and_is_retaken() {
    let a = new_actor();
    take(&a).await;
    take(&a.to_uppercase()).await; // idempotent per account
    assert!(holds_serving_presence_here(&a));
    assert!(!serving_lock_is_free(&a).await, "the declaration is held");

    assert!(
        served_by_another_tab(&[&a]).await.is_empty(),
        "the lone tab proceeds"
    );
    assert!(
        holds_serving_presence_here(&a),
        "and declares the account again after the probe"
    );
    assert!(!serving_lock_is_free(&a).await);

    release(Some(&a)).await;
    assert!(!holds_serving_presence_here(&a));
    assert!(serving_lock_is_free(&a).await, "the release lets it go");
}

/// Two tabs on one account: both shared holds coexist, each one's probe sees
/// the other — and this tab's hold is retaken whatever the answer.
#[wasm_bindgen_test]
async fn two_shared_holds_coexist_and_each_sees_the_other() {
    let a = new_actor();
    let other_tab = another_tab_serves(&a).await;
    take(&a).await;
    assert!(
        holds_serving_presence_here(&a),
        "a shared take beside a sibling's is granted, not refused"
    );
    assert_eq!(served_by_another_tab(&[&a]).await, vec![a.clone()]);
    assert!(
        holds_serving_presence_here(&a),
        "retaken after a probe that answered served"
    );
    other_tab.release().await;
    assert!(served_by_another_tab(&[&a]).await.is_empty());
    release(None).await;
    assert!(
        serving_lock_is_free(&a).await,
        "release-all lets every hold go"
    );
}

/// Without a lock manager the take is skipped (the tab serves undeclared) and
/// the probe degrades open.
#[wasm_bindgen_test]
async fn without_a_lock_manager_the_take_is_skipped_and_the_probe_is_open() {
    let a = new_actor();
    let navigator = js_sys::Reflect::get(&js_sys::global(), &"navigator".into()).unwrap();
    let hidden = js_sys::Object::new();
    js_sys::Reflect::set(&hidden, &"value".into(), &JsValue::UNDEFINED).unwrap();
    js_sys::Reflect::set(&hidden, &"configurable".into(), &JsValue::TRUE).unwrap();
    js_sys::Reflect::define_property(
        navigator.unchecked_ref::<js_sys::Object>(),
        &"locks".into(),
        &hidden,
    )
    .unwrap();

    take(&a).await;
    let held = holds_serving_presence_here(&a);
    let probed = served_by_another_tab(&[&a]).await;

    js_sys::Reflect::delete_property(navigator.unchecked_ref::<js_sys::Object>(), &"locks".into())
        .unwrap();
    assert!(!held, "no lock manager, no hold");
    assert!(probed.is_empty(), "the probe degrades open");
    release(None).await;
}
