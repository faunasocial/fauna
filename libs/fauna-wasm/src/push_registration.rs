//! Web's half of the shared push-registration machine
//! (`fauna_client_push::registration`): the `localStorage` [`IntentStore`] and
//! the read the Settings toggle renders. The machine's verbs ride the session's
//! `WsRpcClient` (`rpc.rs`, `pushEnable` / `pushDisable` / `pushRearm` /
//! `pushDropActorRow`); the browser `serviceWorker` / `PushManager` dance that
//! produces a subscription stays in the SPA's `push.ts` — the one genuinely
//! platform-only step, handed to the machine as a plain subscription.
//!
//! Both records are install-scoped (`account-scoping.md` § The scoping
//! taxonomy, class 2): they describe this browser's push transport, so they
//! are deliberately not keyed by actor id and no sign-out or account-removal
//! erase touches them. The two keys are the ones web has always used, so a
//! browser that opted in before the lift is still opted in after it.

#![cfg(target_arch = "wasm32")]

use fauna_client_push::registration::{IntentStore, PushIntent};
use wasm_bindgen::prelude::*;

/// The intent bit: `"true"` once this browser completed an Enable and the user
/// has not since disabled it.
const SUBSCRIBED_KEY: &str = "fauna-push-subscribed";
/// Which actor's nest row is live for this browser, absent once a drop removed
/// it.
const SUBSCRIBED_ACTOR_KEY: &str = "fauna-push-subscribed-actor";

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// The [`PushIntent`] the two stored strings encode. An absent or unreadable
/// bit reads as not opted in — the safe direction, since a re-arm never opts
/// in.
fn intent_from(subscribed: Option<String>, actor: Option<String>) -> PushIntent {
    PushIntent {
        opted_in: subscribed.as_deref() == Some("true"),
        actor,
    }
}

/// Web's [`IntentStore`]: the two `localStorage` keys. Stateless — every call
/// goes straight to `window.localStorage` and holds no `JsValue`, which is what
/// lets it meet the trait's `Send + Sync` bound on a single-threaded target.
pub struct LocalStorageIntentStore;

impl IntentStore for LocalStorageIntentStore {
    fn load(&self) -> PushIntent {
        let Some(storage) = local_storage() else {
            return PushIntent::default();
        };
        intent_from(
            storage.get_item(SUBSCRIBED_KEY).ok().flatten(),
            storage.get_item(SUBSCRIBED_ACTOR_KEY).ok().flatten(),
        )
    }

    fn save(&self, intent: &PushIntent) -> Result<(), String> {
        let storage = local_storage().ok_or("no localStorage in this context")?;
        let failed = |e: JsValue| format!("{e:?}");
        if intent.opted_in {
            storage.set_item(SUBSCRIBED_KEY, "true").map_err(failed)?;
        } else {
            storage.remove_item(SUBSCRIBED_KEY).map_err(failed)?;
        }
        match &intent.actor {
            Some(actor) => storage
                .set_item(SUBSCRIBED_ACTOR_KEY, actor)
                .map_err(failed),
            None => storage.remove_item(SUBSCRIBED_ACTOR_KEY).map_err(failed),
        }
    }
}

/// Has this browser opted in to push? What the Settings toggle renders — the
/// stored bit, never `Notification.permission` (`settings.md` § Push
/// notifications).
#[wasm_bindgen(js_name = pushOptedIn)]
pub fn push_opted_in() -> bool {
    LocalStorageIntentStore.load().opted_in
}
