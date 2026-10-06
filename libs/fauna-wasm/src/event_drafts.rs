//! The **events rail** of draft-persistence v2, as the web SPA sees it
//! (`docs/goal/behavior/reserved-folders.md` § Drafts Sync;
//! `docs/goal/ui/events.md` § Persistence).
//!
//! The third rail's web face, and deliberately **not** shaped like the other
//! two. `WasmFeedManager` and `WasmConversationsManager` each hang their
//! `DraftsSync` off a shared *manager* that already owns the compose state, so
//! their `restoreDrafts`/`saveDrafts` need no arguments and no return value —
//! the bytes never leave Rust. The Events page has no manager on any app
//! (`reserved-folders.md` § Drafts Sync's 2026-08-17 ruling: the three trigger
//! shapes stay three), and web's compose lives in five Svelte `$state` strings,
//! so this face **carries the record across the boundary** instead.
//!
//! What stays in Rust is everything that matters: the canonical encoding, the
//! seal under the owner's `BackupKey`, the `fauna.drafts.{get,put}` calls, the
//! launch gate and the last-saved baseline. JS hands over five strings and gets
//! five strings back — it never sees a sealed blob, never picks the rail name,
//! and never decides whether a save is safe (priority #2).

/// The event composer's rail key within `__drafts` — one of the three frozen
/// constants on the wire (`fauna_protocol::drafts::DRAFT_RAILS`), never this
/// app's choice: a leg that minted its own rail name would round-trip only with
/// itself and silently lose every draft the user's other devices wrote
/// (`reserved-folders.md` § Drafts Sync step 1).
///
/// Declared outside the `wasm32` gate below so the constant — the one piece of
/// this file that is a *product* decision rather than a binding — is checkable
/// on every target, exactly as `feed.rs` pins its own rail.
const EVENTS_RAIL: &str = fauna_protocol::drafts::RAIL_EVENTS;

// The face itself is wasm-only: it names `WsRpcClient` (the `Rc`-based browser
// transport) and `wasm_bindgen`, so the whole type is gated, exactly as
// `feed.rs`'s `manager` module is. The at-rest record's own round-trip is proven
// tier_1 in `fauna-client-caldav` on every target; this wrapper's proof is the
// `wasm32` build plus the tier_3 restart witness.
#[cfg(target_arch = "wasm32")]
mod face {
    use std::rc::Rc;

    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::future_to_promise;

    use fauna_client_caldav::drafts::EventDrafts;
    use fauna_client_drafts::DraftsSync;
    use fauna_core::identity::ActorKeypair;
    use fauna_rpc_wasm::WsRpcClient;

    use super::EVENTS_RAIL;

    /// The owner's events-rail draft persistence for one logged-in actor. Built
    /// by the `WsRpcClient::eventDrafts` factory and held by the Events page for
    /// the session.
    #[wasm_bindgen]
    pub struct WasmEventDrafts {
        sync: Rc<DraftsSync<WsRpcClient>>,
    }

    impl WasmEventDrafts {
        /// Build over the browser WS-RPC `client` + the local actor's 32-byte
        /// ed25519 `secret`. Plain (non-`#[wasm_bindgen]`) constructor —
        /// `WsRpcClient` is the inner transport, not a JS type — mirroring
        /// `WasmFeedManager::with_client`.
        ///
        /// The drafts client derives the at-rest `BackupKey` from the seed
        /// internally and keeps only that (drafts are owner-only — no signing),
        /// so no keypair is retained here.
        pub fn with_client(client: WsRpcClient, secret: Vec<u8>) -> Result<Self, JsValue> {
            let secret: [u8; 32] = secret
                .try_into()
                .map_err(|_| JsValue::from_str("secret must be 32 bytes"))?;
            let keypair = ActorKeypair::from_secret(secret);
            Ok(Self {
                sync: Rc::new(DraftsSync::new(client, &keypair, EVENTS_RAIL)),
            })
        }
    }

    #[wasm_bindgen]
    impl WasmEventDrafts {
        /// Restore the owner's persisted event-composer draft on launch: fetch +
        /// unseal the `__drafts` blob at `path = "events"` (`fauna.drafts.get`)
        /// and resolve to a plain JS object carrying the five `event-form`
        /// inputs (`{ summary, dtstart, dtend, description, location }`).
        ///
        /// Resolves to `undefined` for a first-run empty rail, and for a blob
        /// that decodes to an all-empty record — indistinguishable from no
        /// draft, and the form's default already is it. A blob that will not
        /// decode is `undefined` too rather than a rejection: a corrupt or
        /// newer-shape record must never break composing (the shared record's
        /// own contract), and the load itself succeeded, so the save gate is
        /// correctly lifted.
        ///
        /// A transport/seal *failure* rejects WITHOUT lifting the `DraftsSync`
        /// save gate, so a later `saveDrafts` stays a no-op for the session and
        /// can never clobber the user's unread draft; the next launch retries.
        #[wasm_bindgen(js_name = restoreDrafts)]
        pub fn restore_drafts(&self) -> js_sys::Promise {
            let sync = self.sync.clone();
            future_to_promise(async move {
                let bytes = match sync.load().await {
                    Ok(Some(bytes)) => bytes,
                    Ok(None) => return Ok(JsValue::UNDEFINED),
                    Err(e) => {
                        return Err(JsValue::from_str(&format!("restore event drafts: {e}")));
                    }
                };
                match EventDrafts::restore_from_bytes(&bytes) {
                    Ok(draft) if draft.is_empty() => Ok(JsValue::UNDEFINED),
                    Ok(draft) => serde_wasm_bindgen::to_value(&draft)
                        .map_err(|e| JsValue::from_str(&format!("restore event drafts: {e}"))),
                    Err(_) => Ok(JsValue::UNDEFINED),
                }
            })
        }

        /// Persist the owner's current event-composer draft after a compose
        /// change (the SPA debounces on the shared `autosaveDebounceMs()`
        /// window): build the canonical record from the five inputs and hand it
        /// to `DraftsSync::save_if_changed`, which seals under the owner's
        /// `BackupKey` and overwrites the `__drafts` blob (`fauna.drafts.put`)
        /// **iff** a launch restore has succeeded *and* the record differs from
        /// the last-saved baseline.
        ///
        /// The datetimes are stored **raw as typed** — normalization to the
        /// wire's shape stays at submit (`events.md` § Persistence), so a
        /// half-typed value rests exactly as the user left it. Passing five
        /// empty strings is how the page clears the rail after a successful
        /// create or a day-cell fresh start.
        ///
        /// Resolves to `undefined`; rejects with the transport/seal error (the
        /// SPA logs + swallows — a transient draft-save failure must not surface
        /// on the page).
        #[wasm_bindgen(js_name = saveDrafts)]
        pub fn save_drafts(
            &self,
            summary: String,
            dtstart: String,
            dtend: String,
            description: String,
            location: String,
        ) -> js_sys::Promise {
            let sync = self.sync.clone();
            future_to_promise(async move {
                fauna_client_caldav::drafts::save_event_draft(
                    &sync,
                    summary,
                    dtstart,
                    dtend,
                    description,
                    location,
                )
                .await
                .map(|_| JsValue::UNDEFINED)
                .map_err(|e| JsValue::from_str(&format!("save event drafts: {e}")))
            })
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use face::WasmEventDrafts;

#[cfg(test)]
mod tests {
    use super::*;

    /// The rail name is the wire's closed enumeration, not this app's choice —
    /// the property that makes a web-written draft restore in the user's tui.
    #[test]
    fn the_rail_is_the_ratified_events_constant() {
        assert_eq!(EVENTS_RAIL, "events");
        assert!(
            fauna_protocol::drafts::is_ratified_rail(EVENTS_RAIL),
            "the rail must be one of the nest-validated DRAFT_RAILS",
        );
    }
}
