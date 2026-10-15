//! The conversations-engine role's web leg, end to end through the wasm faces
//! the SPA calls: `conversationsManager` takes the role before any engine
//! exists and refuses with the typed error; the erase's probe
//! (`actorsServedByAnotherTab`, `removeAccountBlock`) consults this tab's own
//! hold and probes everyone else's (`fauna_mls::engine_role` owns the model;
//! `apps/account-scoping.md` § Concurrent instances owns the rulings).
//!
//! "Another tab" is a hold taken straight over the shared primitive
//! (`fauna_core::web_lock`) under the role's one name — one browser context
//! has one wasm instance, so that is the only holder outside this tab's
//! registry a test can make, and the lock manager cannot tell it from a
//! sibling tab's. `run_in_browser`: this project has no Node.js.

use fauna_core::identity::ActorKeypair;
use fauna_core::web_lock::{WebLockAsk, WebLockHold};
use fauna_mls::engine_role::web_role_lock_name;
use fauna_wasm::account_scope::{actors_served_by_another_tab, remove_account_block};
use fauna_wasm::{
    WsRpcClient, holds_conversations_engine_role, is_conversations_engine_role_refusal,
    release_conversations_engine_role,
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

/// A hold on `actor`'s role from outside this tab's registry — "another tab".
async fn another_tab_takes(actor: &str) -> WebLockHold {
    match WebLockHold::try_take(&web_role_lock_name(actor)).await {
        WebLockAsk::Held(hold) => hold,
        other => panic!("the other tab must win a free role, got {other:?}"),
    }
}

/// Whether a holder outside this tab's registry could take `actor`'s role now
/// (the probe lets it go again before answering).
async fn role_is_free(actor: &str) -> bool {
    match WebLockHold::try_take(&web_role_lock_name(actor)).await {
        WebLockAsk::Held(hold) => {
            hold.release().await;
            true
        }
        _ => false,
    }
}

/// A client for a nest nobody runs: `conversationsManager` builds from the
/// identity alone (the engine is in memory; only a later `load()` reaches the
/// wire).
fn client_for(keypair: &ActorKeypair) -> WsRpcClient {
    WsRpcClient::new(
        "ws://127.0.0.1:9/".to_string(),
        keypair.actor_id_hex(),
        js_sys::Function::new_no_args("return Promise.resolve('')"),
    )
}

async fn build_manager(client: &WsRpcClient, keypair: &ActorKeypair) -> Result<JsValue, JsValue> {
    JsFuture::from(client.conversations_manager(
        "self@example.test".to_string(),
        keypair.secret_bytes().to_vec(),
    ))
    .await
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

async fn remove_block(actor: &str, served_here: Option<&str>) -> Option<String> {
    JsFuture::from(remove_account_block(
        actor.to_string(),
        served_here.map(str::to_string),
    ))
    .await
    .expect("the remove-account question never rejects")
    .as_string()
}

async fn release_all() {
    JsFuture::from(release_conversations_engine_role(None))
        .await
        .expect("release resolves");
}

/// The name's spelling — the one derivation, normalized like the per-tab pin.
#[wasm_bindgen_test]
fn the_role_name_is_one_normalized_derivation() {
    assert_eq!(
        web_role_lock_name("  AbC123 "),
        "fauna.mls.conversations-engine/abc123"
    );
}

/// One holder at a time: while another tab holds the account's role the
/// manager build is refused — typed, never queued — and once it lets go this
/// tab wins the role, holds it for the manager's life, and the tab's teardown
/// hands it on.
#[wasm_bindgen_test]
async fn the_manager_build_takes_the_role_and_another_tab_refuses_it() {
    let keypair = ActorKeypair::generate();
    let actor = keypair.actor_id_hex();
    let client = client_for(&keypair);

    let other_tab = another_tab_takes(&actor).await;
    let refused = build_manager(&client, &keypair)
        .await
        .expect_err("a role another tab holds refuses the build");
    assert!(
        is_conversations_engine_role_refusal(refused),
        "the refusal is the typed one the page renders as served-elsewhere"
    );
    assert!(!holds_conversations_engine_role(actor.clone()));

    other_tab.release().await;
    build_manager(&client, &keypair)
        .await
        .expect("with the role free the build succeeds");
    assert!(
        holds_conversations_engine_role(actor.clone()),
        "the build holds the role"
    );
    assert!(
        !role_is_free(&actor).await,
        "and holds it against every other tab"
    );

    // A later build in this tab (an abandoned build's replacement) serves under
    // the hold the tab already has — a Web Lock does not re-enter.
    build_manager(&client, &keypair)
        .await
        .expect("the tab's own hold does not refuse it");

    release_all().await;
    assert!(!holds_conversations_engine_role(actor.clone()));
    assert!(role_is_free(&actor).await, "the teardown hands the role on");
    client.close();
}

/// A per-actor release lets go of that account's role alone.
#[wasm_bindgen_test]
async fn a_per_account_release_leaves_the_other_accounts_role_held() {
    let (a, b) = (
        ActorKeypair::generate().actor_id_hex(),
        ActorKeypair::generate().actor_id_hex(),
    );
    fauna_mls::engine_role::take_conversations_engine_role(&a)
        .await
        .expect("a is free");
    fauna_mls::engine_role::take_conversations_engine_role(&b)
        .await
        .expect("b is free");
    JsFuture::from(release_conversations_engine_role(Some(a.to_uppercase())))
        .await
        .expect("release resolves");
    assert!(
        role_is_free(&a).await,
        "a's role is handed on, whatever the spelling"
    );
    assert!(
        holds_conversations_engine_role(b.clone()),
        "b's is untouched"
    );
    release_all().await;
}

/// The sign-out guard's question: a role another tab holds is served
/// elsewhere, a free account is not, and a granted probe is dropped at once.
#[wasm_bindgen_test]
async fn a_role_another_tab_holds_is_served_elsewhere_and_a_free_one_is_not() {
    let (a, b) = (
        ActorKeypair::generate().actor_id_hex(),
        ActorKeypair::generate().actor_id_hex(),
    );
    let other_tab = another_tab_takes(&a).await;
    assert_eq!(
        served_by_another_tab(&[&a, &b, &b.to_uppercase()]).await,
        vec![a.clone()]
    );
    assert!(
        role_is_free(&b).await,
        "the probe of a free account let it go at once"
    );
    other_tab.release().await;
}

/// This tab's own role is consulted, never probed — so the elected tab can
/// sign out; probing it would meet its own reflection and refuse.
#[wasm_bindgen_test]
async fn this_tabs_own_role_is_consulted_never_probed() {
    let a = ActorKeypair::generate().actor_id_hex();
    fauna_mls::engine_role::take_conversations_engine_role(&a)
        .await
        .expect("a is free");
    assert!(
        served_by_another_tab(&[&a.to_uppercase()]).await.is_empty(),
        "the lone tab proceeds"
    );
    assert!(
        holds_conversations_engine_role(a.clone()),
        "and still holds its role"
    );
    release_all().await;
}

/// Remove-account's question, in the ruling's order.
#[wasm_bindgen_test]
async fn remove_account_refuses_this_tabs_account_then_another_tabs() {
    let (a, b, idle) = (
        ActorKeypair::generate().actor_id_hex(),
        ActorKeypair::generate().actor_id_hex(),
        ActorKeypair::generate().actor_id_hex(),
    );
    fauna_mls::engine_role::take_conversations_engine_role(&a)
        .await
        .expect("a is free");
    assert_eq!(
        remove_block(&a.to_uppercase(), Some(&a)).await.as_deref(),
        Some("this_tab"),
        "the account this tab serves refuses as this_tab, even while it holds the role"
    );
    let other_tab = another_tab_takes(&b).await;
    assert_eq!(
        remove_block(&b, Some(&a)).await.as_deref(),
        Some("other_tab")
    );
    assert_eq!(
        remove_block(&idle, Some(&a)).await,
        None,
        "an idle account proceeds"
    );
    other_tab.release().await;
    release_all().await;
}

/// The two postures, opposite on purpose: with no lock manager the role fails
/// CLOSED (typed refusal, no engine) and the probe degrades OPEN (nobody can
/// hold a role, so nobody serves).
#[wasm_bindgen_test]
async fn without_a_lock_manager_the_role_fails_closed_and_the_probe_degrades_open() {
    let keypair = ActorKeypair::generate();
    let actor = keypair.actor_id_hex();
    let client = client_for(&keypair);
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

    let refused = build_manager(&client, &keypair)
        .await
        .expect_err("no lock manager, no engine");
    let probed = served_by_another_tab(&[&actor]).await;

    js_sys::Reflect::delete_property(navigator.unchecked_ref::<js_sys::Object>(), &"locks".into())
        .unwrap();
    assert!(is_conversations_engine_role_refusal(refused));
    assert!(probed.is_empty(), "the probe degrades open");
    assert!(!holds_conversations_engine_role(actor));
    client.close();
}
