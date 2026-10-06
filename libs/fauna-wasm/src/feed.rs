//! Feed-rendering bindings: post source classification.
//!
//! Thin wasm-bindgen wrapper over `fauna-feed`. The web feed page
//! (`apps/fauna-web/src/routes/feed/+page.svelte`) already loads this
//! core `fauna_wasm` chunk to decode posts, so the classifier rides
//! along here rather than in a separate lazy chunk — it replaces the
//! hand-written `protocolIcon`/`protocolLabel` switch in
//! `apps/fauna-web/src/lib/feed-utils.ts`, keeping only web's
//! `SourceGlyph → emoji` map (`apps/fauna-web/src/lib/source-glyph.ts`),
//! keyed off the precomputed `glyph` concept the classifier returns — the
//! same map the conversations rail uses (render-model.md § Deltas → D5).
//!
//! See `libs/fauna-feed` for the classifier and `docs/goal/ui/feed.md`
//! § Posts for the `source` field shape.
//!
//! The `structuredView` export projects a decoded `PostBody::Structured` into
//! its typed feed-card view (article / community / classified / live-activity)
//! over the shared `fauna_core::structured` contract — replacing the
//! `getArticleData`/`getCommunityData`/… field projections in `feed-utils.ts`,
//! so the per-schema field keys live once in Rust (shared with the nostr bridge
//! writer) instead of being re-derived in TS. See `docs/goal/ui/feed.md`
//! § Post content types + § Where logic lives.
//!
//! [`WasmFeedManager`] (below, wasm-only) is the web twin of the native
//! `FfiFeedManager` UniFFI façade: a thin `wasm_bindgen` wrapper over the shared
//! stateful `fauna_feed::FeedManager<WsRpcClient>`, so the Svelte Feed page
//! renders entirely from `snapshot()` and forwards gestures to the async
//! manager methods — the priority-#1/#2 lift off the web component's ad-hoc
//! post-list state (`docs/goal/ui/feed.md` § State & data shape, ratified
//! 2026-06-14: "the manager is exposed … to web via `fauna-wasm`, exactly as
//! `ConversationsManager` is"). The browser owns the poll/lifecycle loop and
//! re-reads `snapshot()` after each call resolves, so — like
//! `WasmConversationsManager` — no foreign `SnapshotObserver` callback crosses.

use fauna_core::data::StructuredField;
use fauna_core::source_glyph::SourceGlyph;
use fauna_core::structured::structured_view_from_parts;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

/// JS-facing badge: a stable `id` for icon lookup, the canonical display
/// `label`, and the shared `glyph` icon concept. Hides the serde-tagged Rust
/// enum shape from JS.
#[derive(Serialize)]
struct SourceBadge {
    /// `"fauna" | "bluesky" | "nostr" | "activitypub" | "email" |
    /// "facebook" | "instagram" | "other"`.
    id: String,
    /// Canonical user-facing label (e.g. "Fediverse" for activitypub).
    label: String,
    /// The shared source-icon concept (`SourceKind::glyph()`), serialized to
    /// its lowercase id (`"fox" | "envelope" | "butterfly" | "bolt" | "globe"
    /// | "unknown"`). Web keys the SAME `SourceGlyph → emoji` map off this for
    /// the feed badge that the conversations rail already uses — killing the
    /// within-client rail-vs-badge split (render-model.md § Deltas → D5).
    glyph: SourceGlyph,
}

/// The i18n key to paint for a feed-manager error that is a stated refusal
/// (today only `feed.reference_restricted` — `ui/feed.md` § Encryption at rest,
/// ruling 6), `undefined` for every other error text; the SPA falls back to the
/// raw text. The browser twin of the UniFFI `feed_refusal_i18n_key` face over
/// `fauna_feed::refusal_i18n_key`.
#[wasm_bindgen(js_name = feedRefusalI18nKey)]
pub fn feed_refusal_i18n_key(err: &str) -> Option<String> {
    fauna_feed::refusal_i18n_key(err).map(str::to_string)
}

/// Classify the comma-separated wire `source` field (e.g. `"fauna,
/// bluesky"`) into an ordered, deduplicated array of `{ id, label, glyph }`
/// badges. Returns a JS array.
#[wasm_bindgen(js_name = classifySources)]
pub fn classify_sources(source_field: &str) -> Result<JsValue, JsValue> {
    // No bridges roster yet: `BridgeStatus.glyph` is filled nest-side by the
    // bridged family's nest slice, and the roster threads through here with the
    // render slices (`ui/feed.md` § Implementation status today, `SourceKind::Bridged`).
    let badges: Vec<SourceBadge> = fauna_feed::classify_sources(source_field, &[])
        .into_iter()
        .map(|k| SourceBadge {
            id: k.id(),
            label: k.label(),
            glyph: k.glyph(),
        })
        .collect();
    serde_wasm_bindgen::to_value(&badges).map_err(crate::rpc::err_to_js)
}

/// The `Structured` arm of a decoded `PostBody`, captured loosely so the other
/// variants (Text / Media / Video) are *skipped* rather than deserialized — we
/// only ever project structured posts, and reconstructing the heavier variants
/// would couple this to their `serde_json`↔`serde_wasm_bindgen` number/tuple
/// representations. A non-structured body simply leaves `structured` `None`.
#[derive(Deserialize)]
struct MaybeStructured {
    #[serde(rename = "Structured", default)]
    structured: Option<StructuredParts>,
}

#[derive(Deserialize)]
struct StructuredParts {
    schema: String,
    fields: Vec<StructuredField>,
    #[serde(default)]
    content: Option<String>,
}

/// Project a decoded post body (the JS object from `decodePost`) into its typed
/// structured feed-card view, or `null` for any body that isn't a recognized
/// structured schema (plain / media / video posts and the `nostr/kind-N`
/// unknown-kind fallback). Returns the flat `{ kind: "article", title, … }`
/// shape `PostCard.svelte` discriminates on. The schema + field-key contract
/// lives in `fauna_core::structured`, shared with the nostr bridge writer.
#[wasm_bindgen(js_name = structuredView)]
pub fn structured_view(decoded: JsValue) -> Result<JsValue, JsValue> {
    let body: MaybeStructured =
        serde_wasm_bindgen::from_value(decoded).map_err(crate::rpc::err_to_js)?;
    let view = body
        .structured
        .and_then(|s| structured_view_from_parts(&s.schema, &s.fields, s.content.as_deref()));
    match view {
        Some(v) => serde_wasm_bindgen::to_value(&v).map_err(crate::rpc::err_to_js),
        None => Ok(JsValue::NULL),
    }
}

// ── WasmFeedManager — the shared Feed-page snapshot, for the SPA ──────────────
//
// Wasm-only: it wraps `FeedManager<WsRpcClient>` over the `Rc`-based browser
// transport, so the whole type is gated (the manager's state-machine logic is
// already exercised transport-free by `fauna-feed`'s 36 unit tests on every
// target — the wasm wrapper's proof is the `wasm32` build). The async methods
// bridge to JS via `future_to_promise`, exactly as `WasmConversationsManager`'s
// do; the SPA `await`s each Promise then re-reads `snapshot()`.
#[cfg(target_arch = "wasm32")]
mod manager {
    use std::rc::Rc;

    use fauna_client_drafts::DraftsSync;
    use fauna_core::identity::ActorKeypair;
    use fauna_feed::{
        AttachedFile, CueObservation, CueRow, CueTracker, FactorWeightInput, FeedManager,
        FilterRuleInput, LeaveModel, TrainResult, TrainVerb,
    };
    use fauna_rpc_wasm::WsRpcClient;
    use serde::Serialize;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::future_to_promise;

    /// The feed composer's rail key within `__drafts` — one of the three frozen
    /// constants on the wire (`fauna_protocol::drafts::DRAFT_RAILS`), never a
    /// web-local string: a leg that minted its own name would round-trip only
    /// with itself and silently lose every draft the user's other devices wrote
    /// (`reserved-folders.md` § Drafts Sync step 1).
    const POSTS_RAIL: &str = fauna_protocol::drafts::RAIL_POSTS;

    /// The JS shape of one scored exemplar in the publish review-prune sheet
    /// (`topic-factors.md` § Publishing a trained factor). `post_id` is the hex
    /// content id the published `ListEntry` carries; `preview` is render-only
    /// and **never published** — a List is `content_id → score` and nothing
    /// else. Field names mirror `JsTrainedTopicRow`'s snake_case.
    #[derive(Serialize)]
    struct JsScoredExemplar {
        post_id: String,
        preview: String,
        score: i64,
    }

    /// The JS shape of one surviving n-gram in the publish review-prune sheet's
    /// **Model** half (`topic-factors.md` § Publishing a trained factor, v2).
    ///
    /// Unlike [`JsScoredExemplar`] — whose rows bound *endorsement* of already
    /// public ids — these rows **are** the disclosure, which is why every
    /// survivor crosses rather than a top-N of them. Field names mirror the
    /// snake_case the SPA already reads off the other rows.
    #[derive(Serialize)]
    struct JsReviewNgram {
        ngram: String,
        more: u32,
        less: u32,
    }

    /// The JS shape of a scrubbed vocabulary and the corpus facts the sheet's
    /// mandated copy states (mirrors `fauna_feed::TrainedModelReview`).
    ///
    /// `included_examples` and `marked_examples` are the N and M of "built from
    /// N public examples of your M marked posts": `M - N` is what the rebuild
    /// dropped as restricted, deleted, or unfetchable, and carrying both is what
    /// makes that drop visible to the publisher rather than silent.
    #[derive(Serialize)]
    struct JsTrainedModelReview {
        more_docs: u32,
        less_docs: u32,
        included_examples: u32,
        marked_examples: u32,
        ngrams: Vec<JsReviewNgram>,
    }

    /// The JS spelling of a training outcome. A stable string (not the enum's
    /// Rust name) because the SPA branches on it — the same stable-key discipline
    /// the pickers use.
    fn train_result_js(outcome: TrainResult) -> &'static str {
        match outcome {
            TrainResult::Trained => "trained",
            TrainResult::DuplicateSignal => "duplicate",
            TrainResult::Flipped => "flipped",
        }
    }

    #[wasm_bindgen]
    extern "C" {
        /// A JS object `{ onChanged(): void }` the SPA subscribes to the manager
        /// (`subscribe`) — the web end of the one `FeedSnapshotObserver` every
        /// app renders from (`ui/feed.md` § Architectural rules #1).
        pub type JsFeedSnapshotObserver;
        #[wasm_bindgen(method, catch, js_name = onChanged)]
        fn on_changed(this: &JsFeedSnapshotObserver) -> Result<(), JsValue>;
    }

    /// Adapts a [`JsFeedSnapshotObserver`] to the manager's observer trait.
    struct FeedObserverShim(JsFeedSnapshotObserver);
    // SAFETY: wasm32 is single-threaded; the JS object never crosses a thread.
    unsafe impl Send for FeedObserverShim {}
    unsafe impl Sync for FeedObserverShim {}
    impl fauna_feed::FeedSnapshotObserver for FeedObserverShim {
        fn on_changed(&self) {
            // `catch`: a throwing page callback must not unwind through the
            // manager mutation that notified it; the next notify repaints.
            let _ = self.0.on_changed();
        }
    }

    /// The shared, stateful Feed page exposed to the Svelte SPA — the web twin of
    /// the native `FfiFeedManager`. Holds `Rc<FeedManager<WsRpcClient>>` so each
    /// `future_to_promise` body owns a cheap clone across its `await`.
    #[wasm_bindgen]
    pub struct WasmFeedManager {
        manager: Rc<FeedManager<WsRpcClient>>,
        /// Owns this actor's **posts-rail** [`DraftsSync`] — the shared launch
        /// gate + last-saved baseline over `DraftsClient`
        /// (`fauna.drafts.{get,put}`, sealed under the owner's `BackupKey`;
        /// `reserved-folders.md` § Drafts Sync), built alongside the manager
        /// so the two share one transport + identity. The exact twin of
        /// `WasmConversationsManager::drafts_sync`, one rail over: `restoreDrafts`
        /// (launch) runs `DraftsSync::load` → `FeedManager::restore_drafts`;
        /// `saveDrafts` (debounced compose change) runs
        /// `save_if_changed(drafts_snapshot_bytes())`. Keeping it in Rust is what
        /// stops JS ever handling the sealed blob (priority #2).
        drafts_sync: Rc<DraftsSync<WsRpcClient>>,
    }

    impl WasmFeedManager {
        /// Build over the browser WS-RPC `client` + the local actor's 32-byte
        /// ed25519 `secret` (needed to build + sign posts on `submitPost`). Plain
        /// (non-`#[wasm_bindgen]`) constructor — `WsRpcClient` is the inner
        /// transport, not a JS type, so the JS entry point is the
        /// `WsRpcClient::feedManager` factory which owns it (mirrors
        /// `WasmConversationsManager::with_conversations`). The snapshot starts
        /// empty + `Loading`; the SPA calls `refreshFeeds` / `selectFeed` on
        /// entering the Feed page.
        pub fn with_client(
            client: WsRpcClient,
            secret: Vec<u8>,
        ) -> Result<WasmFeedManager, JsValue> {
            let secret: [u8; 32] = secret
                .try_into()
                .map_err(|_| JsValue::from_str("secret must be 32 bytes"))?;
            // The drafts client derives the at-rest `BackupKey` from the seed
            // internally and keeps only that (drafts are owner-only — no
            // signing), so the keypair is not retained here either.
            let keypair = ActorKeypair::from_secret(secret);
            let drafts_sync = Rc::new(DraftsSync::new(client.clone(), &keypair, POSTS_RAIL));
            let manager = FeedManager::new(client, secret);
            manager.set_period_key_store(crate::account_runtime::period_key_store());
            manager
                .set_preference_store(std::sync::Arc::new(crate::account_runtime::handle_source()));
            Ok(Self {
                manager: Rc::new(manager),
                drafts_sync,
            })
        }
    }

    #[wasm_bindgen]
    impl WasmFeedManager {
        /// Subscribe `observer` to every snapshot change — the web twin of the
        /// native apps' `add_observer` (`ui/feed.md` § Architectural rules #1:
        /// the app re-renders on every notification). Without it the page saw
        /// only the state a call RESOLVED with, never a publish mid-call — so a
        /// switch's up-front clear (§ The read model) stayed invisible until the
        /// new page landed, and the previous query's posts stood under the new
        /// one. `onChanged` runs synchronously inside the notifying mutation:
        /// it must only SCHEDULE a re-read, never call back into the manager.
        #[wasm_bindgen(js_name = subscribe)]
        pub fn subscribe(&self, observer: JsFeedSnapshotObserver) {
            self.manager
                .add_observer(std::sync::Arc::new(FeedObserverShim(observer)));
        }

        /// The current [`fauna_feed::FeedSnapshot`] as a plain JS object — the SPA
        /// renders the feed selector, post list, compose bar, bridge form, and
        /// page error entirely from it. `json_compatible` so numbers stay numbers
        /// and unit enums (`status`) serialize to strings (matches `snapshot()`
        /// on the conversations manager).
        #[wasm_bindgen(js_name = "snapshot")]
        pub fn snapshot(&self) -> Result<JsValue, JsValue> {
            crate::rpc::to_js(&self.manager.snapshot())
        }

        /// Update the composer (`compose-text-field` / `compose-tags-field` /
        /// staged file) and clear any stale compose error. `attached_file` is a JS
        /// `{ name, size, blob_hash?, media_type? }` object or `null`/`undefined`.
        /// Validation happens on `submitPost`.
        #[wasm_bindgen(js_name = updateCompose)]
        pub fn update_compose(
            &self,
            text: String,
            tags: String,
            attached_file: JsValue,
        ) -> Result<(), JsValue> {
            let attached_file: Option<AttachedFile> =
                serde_wasm_bindgen::from_value(attached_file).map_err(crate::rpc::err_to_js)?;
            self.manager.update_compose(text, tags, attached_file);
            Ok(())
        }

        /// Stage the composer's gate-to-tier fields (`compose-gate-tier-select` /
        /// `compose-gate-preview-field`) — the gate sibling of
        /// [`update_compose`](Self::update_compose). `gate_tier` `null`/`undefined`
        /// composes a normal (ungated) post; `Some(tier)` gates the full body to
        /// that tier at submit (`ui/feed.md` § Encryption at rest; monetization.md
        /// § Pillars 2+3). Web twin of `FfiFeedManager::update_compose_gate`.
        #[wasm_bindgen(js_name = updateComposeGate)]
        pub fn update_compose_gate(&self, gate_tier: Option<String>, gate_preview: String) {
            self.manager.update_compose_gate(gate_tier, gate_preview);
        }

        /// Stage the composer's **"Sell this post…"** fields
        /// (`compose-sell-price` / `compose-sell-subscribers-free`) — the sell
        /// sibling of [`update_compose_gate`](Self::update_compose_gate),
        /// sharing its teaser. `selling: false` leaves sell mode (back to
        /// Public); `true` enters it and clears any selected gate tier, since
        /// both are answers to the one gate select. Web twin of
        /// `FfiFeedManager::update_compose_sell`; `monetization.md` § Per-post
        /// pay-to-unlock.
        ///
        /// Takes the sell params flat rather than a struct because that is
        /// the ergonomic JS face; the `Option<SellComposeState>` the manager
        /// wants is rebuilt here.
        #[wasm_bindgen(js_name = updateComposeSell)]
        pub fn update_compose_sell(
            &self,
            selling: bool,
            price: String,
            asking_price: String,
            subscribers_get_it_free: bool,
            gate_preview: String,
        ) {
            let sell = selling.then_some(fauna_feed::SellComposeState {
                price,
                asking_price,
                subscribers_get_it_free,
            });
            self.manager.update_compose_sell(sell, gate_preview);
        }

        /// Stage the composer's **room** answer — a room from
        /// `snapshot().own_rooms` by its hex channel id (`null`/`undefined`
        /// leaves it, back to Public). Clears a tier and a sale. Web twin of
        /// `FfiFeedManager::update_compose_room`; `ui/feed.md` § Encryption at
        /// rest → *Room-restricted — the app half*.
        #[wasm_bindgen(js_name = updateComposeRoom)]
        pub fn update_compose_room(&self, gate_room: Option<String>, gate_preview: String) {
            self.manager.update_compose_room(gate_room, gate_preview);
        }

        /// Stage the teaser (`compose-gate-preview-field`) ALONE, touching no
        /// audience answer — `updateComposeGate` re-reads the tier and would
        /// drop a room answer. Web twin of
        /// `FfiFeedManager::update_compose_preview`.
        #[wasm_bindgen(js_name = updateComposePreview)]
        pub fn update_compose_preview(&self, gate_preview: String) {
            self.manager.update_compose_preview(gate_preview);
        }

        /// The DAG-CBOR `UploadSidecar` bytes the **staged** gated post's
        /// sealed body uploads under — `GroupRestrictedPost` for a room post,
        /// the tier class otherwise. Read it after `prepareGatedBlob` /
        /// `prepareSellPost`, in place of the free `gated_post_sidecar`, which
        /// knows only the tier class. Web twin of
        /// `FfiFeedManager::gated_upload_sidecar`.
        #[wasm_bindgen(js_name = gatedUploadSidecar)]
        pub fn gated_upload_sidecar(&self) -> Vec<u8> {
            self.manager.gated_upload_sidecar().to_dag_cbor()
        }

        /// Install the conversations manager's room-post seam on this feed
        /// (`ui/feed.md` § Encryption at rest → *Room-restricted — the app
        /// half*): the feed then opens room posts and offers the user's rooms.
        /// Resolves nothing and returns `false` when that manager was built
        /// without the FaunaMls rail (a receive-only build), which leaves
        /// every room post locked — the honest state. Follow it with
        /// `refreshOwnRooms`. Web twin of `FfiFeedManager::set_room_post_keys`.
        #[wasm_bindgen(js_name = setRoomPostKeys)]
        pub fn set_room_post_keys(
            &self,
            conversations: &crate::conversations::WasmConversationsManager,
        ) -> bool {
            match conversations.room_post_seam() {
                Some(seam) => {
                    // `Arc` because that is the seam slot's type; the browser
                    // is single-threaded, so the non-`Send` backend inside
                    // never crosses a thread.
                    #[allow(clippy::arc_with_non_send_sync)]
                    let seam = std::sync::Arc::new(seam);
                    self.manager.set_room_post_keys(seam);
                    true
                }
                None => false,
            }
        }

        // ── Draft persistence (posts rail) ───────────────────────────────────
        //
        // The web leg of draft-persistence v2 for the Feed composer, the exact
        // shape `WasmConversationsManager` already uses one rail over: these two
        // composites keep the snapshot bytes + seal + WS round-trip entirely in
        // Rust, so the SPA only decides *when* (onMount → restore; debounced
        // compose change → save) and JS never shuttles the sealed blob
        // (priority #2). Owner-only content; no signing.

        /// Restore the owner's persisted feed-composer draft on launch: fetch +
        /// unseal the `__drafts` blob at `path = "posts"` (`fauna.drafts.get`)
        /// and hand the canonical snapshot to the shared `FeedManager`,
        /// refreshing the composer. The SPA awaits this once, right after
        /// building the manager, so it precedes any `saveDrafts`. Resolves to
        /// `undefined` on a successful restore or a first-run empty rail
        /// (`None`). Any failure — a present-but-undecryptable blob or a
        /// transient transport failure — rejects WITHOUT lifting the
        /// [`DraftsSync`] save gate, so a later `saveDrafts` stays a no-op for
        /// the session and can never clobber the user's unread draft; the next
        /// launch retries the load.
        #[wasm_bindgen(js_name = restoreDrafts)]
        pub fn restore_drafts(&self) -> js_sys::Promise {
            let sync = self.drafts_sync.clone();
            let mgr = self.manager.clone();
            future_to_promise(async move {
                match sync.load().await {
                    Ok(Some(bytes)) => {
                        mgr.restore_drafts(bytes);
                        Ok(JsValue::UNDEFINED)
                    }
                    Ok(None) => Ok(JsValue::UNDEFINED),
                    Err(e) => Err(JsValue::from_str(&format!("restore feed drafts: {e}"))),
                }
            })
        }

        /// Persist the owner's current feed-composer draft after a compose
        /// change (the SPA debounces): snapshot the composer and hand it to
        /// [`DraftsSync::save_if_changed`], which seals under the owner's
        /// `BackupKey` and overwrites the `__drafts` blob (`fauna.drafts.put`)
        /// **iff** a launch restore has succeeded *and* the snapshot differs
        /// from the last-saved baseline. A no-op before/without a successful
        /// restore (the gate that prevents clobbering an unread draft) or for an
        /// unchanged composer. Resolves to `undefined`; rejects with the
        /// transport/seal error (the SPA logs + swallows — a transient
        /// draft-save failure must not surface on the page).
        #[wasm_bindgen(js_name = saveDrafts)]
        pub fn save_drafts(&self) -> js_sys::Promise {
            let sync = self.drafts_sync.clone();
            let snapshot = self.manager.drafts_snapshot_bytes();
            future_to_promise(async move {
                sync.save_if_changed(&snapshot)
                    .await
                    .map(|_| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&format!("save feed drafts: {e}")))
            })
        }

        /// Refresh the feed selector list (`fauna.feed.list`). Resolves `undefined`.
        #[wasm_bindgen(js_name = refreshFeeds)]
        pub fn refresh_feeds(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.refresh_feeds().await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Refresh the composer's gate-to-tier option set (`compose-gate-tier-select`)
        /// from the local actor's own tiers (`fauna.subscriptions.tiers.list`).
        /// The native apps get this for free inside `reload()` on every feed
        /// navigation; the web SPA's manager persists across nav, so the page calls
        /// this when the compose dialog opens so a tier the user just minted shows up
        /// in the gate select. Best-effort (a transport failure leaves the prior set);
        /// resolves `undefined`.
        #[wasm_bindgen(js_name = refreshOwnTiers)]
        pub fn refresh_own_tiers(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.refresh_own_tiers().await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Re-read the rooms the composer offers (`snapshot().own_rooms`) from
        /// the installed room-post seam — a local read, no WS-RPC. `refreshFeeds`
        /// runs it too; the SPA also calls it on the conversations plane's own
        /// change tick, so a room joined or left reaches the audience select
        /// without re-entering the feed. Resolves `true` when the list changed —
        /// re-read `snapshot()` then, and only then: the tick asks far more
        /// often than the list moves. Web twin of
        /// `FfiFeedManager::refresh_own_rooms`.
        #[wasm_bindgen(js_name = refreshOwnRooms)]
        pub fn refresh_own_rooms(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                let changed = m.refresh_own_rooms().await;
                Ok(JsValue::from_bool(changed))
            })
        }

        /// Refresh the subscribed bridge-feed list (`fauna.bridges.feeds.list`).
        #[wasm_bindgen(js_name = refreshBridgeFeeds)]
        pub fn refresh_bridge_feeds(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.refresh_bridge_feeds().await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Refresh the nest's available bridges (`snapshot().available_bridges`)
        /// — the `bridge-form-bridge-select` option set. The SPA reads the
        /// snapshot to populate the selector instead of a hard-coded list, so it
        /// never offers a protocol the nest can't serve
        /// (`version-compatibility.md` § Dim 3).
        #[wasm_bindgen(js_name = refreshAvailableBridges)]
        pub fn refresh_available_bridges(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.refresh_available_bridges().await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Select a feed (`feed-item`) and load its first page. `feed_id`
        /// `null`/`undefined` ⇒ the nest's local feed.
        #[wasm_bindgen(js_name = selectFeed)]
        pub fn select_feed(&self, feed_id: Option<String>) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.select_feed(feed_id).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Select the built-in **Trending** virtual feed (`feed-trending-item`)
        /// and load its first page (`trending.md` § The Trending feed) — the
        /// scored sibling of the local feed over `fauna.feed.trending.posts`, no
        /// feed row. Mirrors `selectFeed(null)` = local; sets
        /// `snapshot().trending_selected`.
        #[wasm_bindgen(js_name = selectTrendingFeed)]
        pub fn select_trending_feed(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.select_trending_feed().await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Update the search term and **re-query** (never a client-side filter).
        /// An empty/whitespace term clears the search.
        #[wasm_bindgen(js_name = setSearchQuery)]
        pub fn set_search_query(&self, term: Option<String>) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.set_search_query(term).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Clear the search (`feed-search-clear`) and re-query.
        #[wasm_bindgen(js_name = clearSearch)]
        pub fn clear_search(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.clear_search().await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Load the next page (`Load more`) and append it (dedup by `post_id`,
        /// nest order preserved). No-op when no further page or a load is in flight.
        #[wasm_bindgen(js_name = loadMore)]
        pub fn load_more(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.load_more().await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Submit the composed post (`post-submit-button`). Resolves `undefined`;
        /// rejects with the error string (the compose error also lands in the
        /// snapshot's `compose.error`).
        #[wasm_bindgen(js_name = submitPost)]
        pub fn submit_post(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.submit_post()
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        // ── Gate-to-tier compose (feed.md § Encryption at rest; monetization.md
        //    § Pillars 2+3 — app UX). The sealed-blob bytes ride the browser's
        //    bulk-binary HTTP plane (the one production HTTP carve-out): the
        //    manager hands the sealed bytes out (`prepareGatedBlob`) / takes them
        //    back in (`unlockGatedPost`) and stays WS-RPC-only, exactly like the
        //    media-attachment path. Web twins of the six `FfiFeedManager` gated
        //    methods; the sealed-class sidecar is the shared `gatedPostSidecar()`. ──

        /// Process + seal one compose attachment for the composer's **current**
        /// audience, resolving the same `WasmUploadPayload`
        /// `processAndSealPublicPost` returns — so `$lib/wasm`'s
        /// `takeUploadPayload` normalizes both and the SPA has one upload
        /// shape, not two (priority #1).
        ///
        /// The web twin of `FfiFeedManager::seal_compose_attachment`, and the
        /// SPA's door onto the shared-Rust seal-by-id helper (`ui/media.md`
        /// § Encryption at rest). Use it instead of
        /// `processAndSealPublicPost` for a **feed compose**: that entry point
        /// seals only the public audience, because a tier's period key never
        /// crosses this boundary, so an audience-restricted post's photo can
        /// only be sealed on the far side of it.
        ///
        /// **Call it at submit, after the audience is final** — uploading at
        /// pick time publishes a plaintext copy of a restricted post's picture
        /// that no blob DELETE exists to remove. POST `thumbnail` first
        /// (best-effort), then `primary`, then stage the returned hash with
        /// [`updateCompose`](Self::update_compose), taking `mime` from this
        /// reply — for a sealed attachment it is the **plaintext's** real type,
        /// which the sidecar (`application/octet-stream`) cannot carry.
        /// Rejects with the (already `compose-error`-stamped) string.
        #[wasm_bindgen(js_name = sealComposeAttachment)]
        pub fn seal_compose_attachment(&self, raw: Vec<u8>) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                match m.seal_compose_attachment(raw).await {
                    Ok(sealed) => {
                        Ok(crate::WasmUploadPayload::from_compose_attachment(sealed).into())
                    }
                    Err(e) => Err(JsValue::from_str(&e)),
                }
            })
        }

        /// Build + sign the staged **gated** post and resolve its sealed full-body
        /// blob (a `Uint8Array`) for the SPA to upload (`POST /api/v1/blob`,
        /// sidecar `gatedPostSidecar()`, mime `application/octet-stream`). Resolves
        /// `null` when the composer isn't gated — call [`submitPost`](Self::submit_post)
        /// instead. On success the signed post is staged for [`submitGatedPost`](Self::submit_gated_post);
        /// rejects with the (already `compose-error`-stamped) validation string.
        #[wasm_bindgen(js_name = prepareGatedBlob)]
        pub fn prepare_gated_blob(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                match m.prepare_gated_blob().await {
                    Ok(Some(bytes)) => Ok(js_sys::Uint8Array::from(bytes.as_slice()).into()),
                    Ok(None) => Ok(JsValue::NULL),
                    Err(e) => Err(JsValue::from_str(&e)),
                }
            })
        }

        /// The reply dialog's twin of [`prepareGatedBlob`](Self::prepare_gated_blob)
        /// (`ui/feed.md` § Encryption at rest → *Ruling 5's build — the shape*,
        /// (c)): resolves the sealed body (a `Uint8Array`) when `postId` is a
        /// restricted post this device can author under — the post's
        /// `reply_audience` says so — to upload under `gatedUploadSidecar()` and
        /// finish with [`submitGatedPost`](Self::submit_gated_post). Resolves
        /// `null` when there is nothing to upload — call [`reply`](Self::reply).
        #[wasm_bindgen(js_name = prepareSealedReply)]
        pub fn prepare_sealed_reply(&self, post_id: String, body: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                match m.prepare_sealed_reply(post_id, body).await {
                    Ok(Some(bytes)) => Ok(js_sys::Uint8Array::from(bytes.as_slice()).into()),
                    Ok(None) => Ok(JsValue::NULL),
                    Err(e) => Err(JsValue::from_str(&e)),
                }
            })
        }

        /// The quote twin of [`prepareSealedReply`](Self::prepare_sealed_reply);
        /// a wordless quote resolves `null` — call [`quote`](Self::quote).
        #[wasm_bindgen(js_name = prepareSealedQuote)]
        pub fn prepare_sealed_quote(&self, post_id: String, body: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                match m.prepare_sealed_quote(post_id, body).await {
                    Ok(Some(bytes)) => Ok(js_sys::Uint8Array::from(bytes.as_slice()).into()),
                    Ok(None) => Ok(JsValue::NULL),
                    Err(e) => Err(JsValue::from_str(&e)),
                }
            })
        }

        /// **Sell this post** — auto-mint a degenerate single-post subscription
        /// tier and gate the staged composer text to it, resolving the sealed
        /// blob (a `Uint8Array`) to upload (`monetization.md` § Per-post
        /// pay-to-unlock). Finish with
        /// [`submitGatedPost`](Self::submit_gated_post) — the upload + create
        /// glue is shared with the ordinary gated flow. `subscribersGetItFree`
        /// is the single ratified knob (`true` ⇒ inside every paid
        /// subscription, `false` ⇒ pure pay-per-view). Rejects with the
        /// (already `compose-error`-stamped) validation string.
        ///
        /// `askingPriceSats` is the machine-comparable price in **sats**
        /// (`monetization.md` § The asking price). `f64` at this boundary,
        /// not `u64`: a 64-bit integer parameter maps to a JS `bigint` the
        /// SPA cannot satisfy — the house convention is `f64` in, checked
        /// cast inside. A negative or non-integral amount is refused rather
        /// than truncated.
        /// **Phase one of "Sell this post…", for a compose that carries an
        /// attachment** — mint the unlock tier and persist its period key,
        /// creating nothing server-side. Resolves `undefined`; rejects with the
        /// (already `compose-error`-stamped) validation string.
        ///
        /// Call it between `updateComposeSell` and
        /// [`sealComposeAttachment`](Self::seal_compose_attachment): a sold
        /// post's photo seals under the tier the sale mints, and that tier does
        /// not exist when the file is picked. **With no attachment, do not call
        /// it** — [`prepareSellPost`](Self::prepare_sell_post) runs this
        /// itself, so the existing one-call flow is unchanged.
        ///
        /// Pass the same `subscribersGetItFree` / `askingPriceSats` the
        /// following `prepareSellPost` will: this call decides the tier's rank
        /// and refuses an unconvertible asking price while refusing is free.
        /// Editing the sale afterwards drops the stage, and `prepareSellPost`
        /// then refuses rather than publishing a photo nobody can open.
        #[wasm_bindgen(js_name = stageSellTier)]
        pub fn stage_sell_tier(
            &self,
            subscribers_get_it_free: bool,
            asking_price_sats: Option<f64>,
        ) -> js_sys::Promise {
            let m = self.manager.clone();
            let asking_price_sats = match asking_price_sats {
                Some(s) if !(s.is_finite() && s >= 0.0 && s.fract() == 0.0) => {
                    return js_sys::Promise::reject(&JsValue::from_str(
                        "asking price must be a whole, non-negative number of sats",
                    ));
                }
                other => other.map(|s| s as u64),
            };
            future_to_promise(async move {
                m.stage_sell_tier(subscribers_get_it_free, asking_price_sats)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        #[wasm_bindgen(js_name = prepareSellPost)]
        pub fn prepare_sell_post(
            &self,
            price_hint: Option<String>,
            subscribers_get_it_free: bool,
            asking_price_sats: Option<f64>,
        ) -> js_sys::Promise {
            let m = self.manager.clone();
            let asking_price_sats = match asking_price_sats {
                Some(s) if !(s.is_finite() && s >= 0.0 && s.fract() == 0.0) => {
                    return js_sys::Promise::reject(&JsValue::from_str(
                        "asking price must be a whole, non-negative number of sats",
                    ));
                }
                other => other.map(|s| s as u64),
            };
            future_to_promise(async move {
                m.prepare_sell_post(price_hint, subscribers_get_it_free, asking_price_sats)
                    .await
                    .map(|bytes| js_sys::Uint8Array::from(bytes.as_slice()).into())
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Abort a staged gated submit whose **blob upload failed** (the glue
        /// between [`prepareGatedBlob`](Self::prepare_gated_blob) and
        /// [`submitGatedPost`](Self::submit_gated_post)): drop the staged post,
        /// clear `submitting`, and surface `message` on `compose-error` — the
        /// composer keeps its text for a manual retry.
        #[wasm_bindgen(js_name = abortGatedSubmit)]
        pub fn abort_gated_submit(&self, message: String) {
            self.manager.abort_gated_submit(message);
        }

        /// Create the gated post staged by [`prepareGatedBlob`](Self::prepare_gated_blob),
        /// after the SPA uploaded the sealed blob. `uploaded_hash` is the upload
        /// reply's hex hash — it must echo the staged post's `encrypted_ref`.
        /// Resolves `undefined`; rejects with the error string.
        #[wasm_bindgen(js_name = submitGatedPost)]
        pub fn submit_gated_post(&self, uploaded_hash: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.submit_gated_post(uploaded_hash)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Create a feed (`create_feed` submit). `rules` is a JS array of
        /// `{ rule_type, value, required }`; `factors` is a JS array of
        /// `{ factor, weight_permille, global }` (the `feed-factor-*` editor's
        /// entries — content-moderation-and-ranking.md § Composition). Resolves
        /// the new `feed_id`; rejects with the error string.
        #[wasm_bindgen(js_name = createFeed)]
        pub fn create_feed(
            &self,
            name: String,
            rules: JsValue,
            combination: String,
            scope: Option<String>,
            contributor_seeds: Option<Vec<String>>,
            factors: JsValue,
        ) -> js_sys::Promise {
            let m = self.manager.clone();
            let rules: Result<Vec<FilterRuleInput>, _> = serde_wasm_bindgen::from_value(rules);
            let factors: Result<Vec<FactorWeightInput>, _> =
                serde_wasm_bindgen::from_value(factors);
            future_to_promise(async move {
                let rules = rules.map_err(|e| JsValue::from_str(&format!("invalid rules: {e}")))?;
                let factors =
                    factors.map_err(|e| JsValue::from_str(&format!("invalid factors: {e}")))?;
                m.create_feed(name, rules, combination, scope, contributor_seeds, factors)
                    .await
                    .map(|id| JsValue::from_str(&id))
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Delete a feed (`feed-delete-button`; confirmation is client glue).
        #[wasm_bindgen(js_name = deleteFeed)]
        pub fn delete_feed(&self, feed_id: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.delete_feed(feed_id)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Subscribe to a bridge feed (`bridge-form-subscribe-button`). Resolves
        /// the new row id (a JS number); rejects with the error string (also
        /// reflected into the snapshot's `bridge_form.error`).
        #[wasm_bindgen(js_name = subscribeBridge)]
        pub fn subscribe_bridge(&self, kind: String, uri: String, name: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.subscribe_bridge(kind, uri, name)
                    .await
                    .map(|id| JsValue::from_f64(id as f64))
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Unsubscribe from a bridge feed (`bridge-feed-unsubscribe-button`).
        #[wasm_bindgen(js_name = unsubscribeBridge)]
        pub fn unsubscribe_bridge(&self, id: f64) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.unsubscribe_bridge(id as i64)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Project the embedded quoted-post card for `quoted_post_id` — from the
        /// loaded set with no fetch when possible, else a single `fauna.posts.get`.
        /// Resolves the [`fauna_feed::QuotedPostView`] as a JS object, or `null`.
        #[wasm_bindgen(js_name = resolveQuotedPost)]
        pub fn resolve_quoted_post(&self, quoted_post_id: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                match m.resolve_quoted_post(quoted_post_id).await {
                    Some(view) => crate::rpc::to_js(&view),
                    None => Ok(JsValue::NULL),
                }
            })
        }

        /// Make the post `post_id` names renderable whether or not the feed query
        /// ever loaded it (`ui/search.md` § Where logic lives → *Result
        /// navigation (deep link)*) — the deep-link door a search hit needs,
        /// since it can name a post the timeline never scrolled to. Cheap and
        /// idempotent: a post already in the loaded list or already parked in
        /// the deep-link slot costs no round trip. Resolves the
        /// [`fauna_feed::PostResolution`] as a JS object (`{Loaded: null}` /
        /// `{Fetched: null}` / `{TakenDown: {reference}}` / `{Unavailable:
        /// null}` — the tagged-enum JSON shape every wasm export uses).
        #[wasm_bindgen(js_name = resolvePost)]
        pub fn resolve_post(&self, post_id: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                let resolution = m.resolve_post(post_id).await;
                crate::rpc::to_js(&resolution)
            })
        }

        /// Resolve the first media blob hash for a loaded `has_media` post and
        /// write it into the matching `PostSummary.media_hash`. No-op unless a
        /// loaded post flags media and isn't resolved. Resolves `undefined`.
        #[wasm_bindgen(js_name = resolveMedia)]
        pub fn resolve_media(&self, post_id: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.resolve_media(post_id).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Resolve the buyer's price read for a sold post (`monetization.md` §
        /// Per-post pay-to-unlock → *the buyer's price read is post-addressed*)
        /// and fold it into the matching `PostSummary.unlock_offer`. A no-op
        /// unless `gated_tier` names a `post-unlock-*` tier and the offer isn't
        /// already resolved. Drives `gated-post-price` / `gated-post-payment-link`
        /// / `gated-post-buy-button` — the purchase itself is the existing
        /// subscribe call against the resolved `tier_name`, no new RPC face.
        /// Resolves `undefined`.
        #[wasm_bindgen(js_name = resolvePostUnlockOffer)]
        pub fn resolve_post_unlock_offer(&self, post_id: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.resolve_post_unlock_offer(post_id).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Resolve this post's tip surface (`monetization.md` § Tips) and fold
        /// it into the matching `PostSummary.tips`. Drives `post-tip-total` /
        /// `post-tip-count` / `post-tip-list-button`. Resolves `undefined`.
        ///
        /// Call once per rendered post from the same pump that drives
        /// `resolveMedia`, guarded on `tips == null`: unlike the unlock offer
        /// there is no data trigger, because nothing in the feed projection
        /// says whether a post has tips. Fire-once by construction — a view is
        /// written on *every* outcome, "no tips" and a refusal
        /// (`unknown_kind` included), so the guard closes and the pump settles.
        ///
        /// Absent from a `payments`-excised build (`dynamic-features.md` §
        /// Charter members); `tips` then stays null forever.
        #[cfg(feature = "payments")]
        #[wasm_bindgen(js_name = resolvePostTips)]
        pub fn resolve_post_tips(&self, post_id: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.resolve_post_tips(post_id).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Buy a sold post via the self-serve teaser affordance
        /// (`gated-post-buy-button`) — the existing subscribe flow against the
        /// resolved offer's `tier_name`, no new nest write. Resolves `true` if
        /// queued (pending author approval), `false` if approved outright;
        /// rejects if the post isn't loaded, its offer hasn't resolved, or the
        /// subscribe call itself fails.
        #[wasm_bindgen(js_name = buyUnlockOffer)]
        pub fn buy_unlock_offer(&self, post_id: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                match m.buy_unlock_offer(post_id).await {
                    Some(Ok(queued)) => Ok(JsValue::from_bool(queued)),
                    Some(Err(e)) => Err(JsValue::from_str(&e)),
                    None => Err(JsValue::from_str(
                        "buyUnlockOffer: post not loaded or offer not resolved",
                    )),
                }
            })
        }

        /// Resolve a loaded gated post's sealed-blob hash (hex `encrypted_ref`) for
        /// the SPA to fetch (`GET /api/v1/blob/{hash}` — browser bulk plane),
        /// caching the decoded gate info for [`unlockGatedPost`](Self::unlock_gated_post).
        /// The `resolveMedia` pattern: one lazy `fauna.posts.get` + decode per post.
        /// Resolves `null` when the post isn't loaded, isn't gated, or can't be
        /// decoded.
        #[wasm_bindgen(js_name = gatedBlobHash)]
        pub fn gated_blob_hash(&self, post_id: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                match m.gated_blob_hash(post_id).await {
                    Some(hash) => Ok(JsValue::from_str(&hash)),
                    None => Ok(JsValue::NULL),
                }
            })
        }

        /// Decrypt a gated post's full body from its fetched sealed blob
        /// (`blob_bytes`, a `Uint8Array` fetched after [`gatedBlobHash`](Self::gated_blob_hash))
        /// and swap it into the snapshot (`body` + rebuilt `document`,
        /// `gated_unlocked`), then notify — the SPA re-reads `snapshot()` and the
        /// detail repaints the full body. The period key comes from custody when the
        /// local actor is the author, else from the reader's own wrap entry in the
        /// tier's live KeyBlob. A post sealed under a rotated-out period the KeyBlob
        /// no longer carries stays locked (best-effort backfill — the archival path
        /// is a follow-on). Resolves `undefined`; rejects with the error string.
        #[wasm_bindgen(js_name = unlockGatedPost)]
        pub fn unlock_gated_post(&self, post_id: String, blob_bytes: Vec<u8>) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.unlock_gated_post(post_id, blob_bytes)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Open a post-media blob the SPA fetched by hash, for rendering.
        ///
        /// **Every post-image path calls this**, gated post or not: a public
        /// post's blob is plaintext on the wire and comes straight back, while a
        /// gated post's attachment is AEAD-sealed under the same per-post key its
        /// body opened under and must be opened before the bytes are an image
        /// (`ui/media.md` § Encryption at rest — one per-post key seals body and
        /// attachments alike). Routing every hash through the one call is what
        /// keeps the seven post cards free of an is-this-post-gated branch.
        ///
        /// Returns `null` when the blob IS a sealed item of an unlocked post and
        /// did not open — paint the placeholder, exactly as for bytes that fail
        /// to decode. Synchronous: the fetch is the SPA's, as for
        /// [`unlockGatedPost`](Self::unlock_gated_post).
        #[wasm_bindgen(js_name = openMediaBytes)]
        pub fn open_media_bytes(&self, blob_hash: String, fetched: Vec<u8>) -> Option<Vec<u8>> {
            self.manager.open_media_bytes(&blob_hash, fetched)
        }

        /// Whether this blob must be fetched and opened rather than linked.
        ///
        /// The SPA's post image is an `<img src>`, so the browser does the GET and
        /// the decode natively and no JS ever holds the bytes — which is why web
        /// needs this predicate where tui/linux/windows/android do not (they hold
        /// the bytes and route every hash through
        /// [`openMediaBytes`](Self::open_media_bytes) unconditionally).
        ///
        /// `false` — the overwhelmingly common answer — means the plain
        /// `/api/v1/blob/<hash>` URL IS the image, including its `?thumb=1` smaller
        /// blob. `true` means this is an item of a post this reader has unlocked:
        /// fetch it, hand it to `openMediaBytes`, and render the result as an
        /// object URL (there is no server-openable thumbnail for a sealed item —
        /// the nest cannot read the bytes).
        ///
        /// Ask in the page, not the post card: the card takes a resolved URL either
        /// way.
        #[wasm_bindgen(js_name = isSealedMedia)]
        pub fn is_sealed_media(&self, blob_hash: String) -> bool {
            self.manager.is_sealed_media(&blob_hash)
        }

        /// What a tapped `video-thumbnail`'s block plays from (render-model.md
        /// § D6c → *Inline playback*). Takes the media block exactly as
        /// `renderDocumentMediaBlocks` handed it (`{Video: {hash, alt}}`) and
        /// resolves the [`fauna_feed::PlaybackSource`]: `{Url: {url}}` (nest-relative —
        /// prefix the nest origin and set it as the `<video src>`), `{Sealed: {hash}}`
        /// (fetch, `openMediaBytes`, play the object URL), or `{Unplayable:
        /// {reason}}`. The `<video>` and its state are the SPA's; this is only the
        /// shared decision of what plays.
        #[wasm_bindgen(js_name = playbackSource)]
        pub fn playback_source(&self, block: JsValue) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                let block: fauna_core::render::RenderBlock =
                    serde_wasm_bindgen::from_value(block).map_err(crate::rpc::err_to_js)?;
                let source = m.playback_source(&block).await;
                crate::rpc::to_js(&source)
            })
        }

        /// Resolve the link-preview metadata for the bare `url` of a loaded post's
        /// `RenderBlock::LinkPreview` block (render-model.md § D4) via
        /// `fauna.linkpreview.resolve`, then re-emit so the next `snapshot()`
        /// projects the block `Resolved`/`Failed`. No-op for an already-resolved
        /// URL. Resolves `undefined`.
        #[wasm_bindgen(js_name = resolveLinkPreview)]
        pub fn resolve_link_preview(&self, url: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.resolve_link_preview(url).await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// Opt a post into loading its remote images (`load-remote-content-button`
        /// on a feed card/detail). Flips the manager-owned reveal set and re-emits,
        /// so the next `snapshot()` projects `RemoteImage.revealed: true` for it
        /// (render-model.md § D3) — the web app no longer keeps a per-card reveal
        /// flag. Synchronous (no fetch); in-memory only.
        #[wasm_bindgen(js_name = revealRemoteImages)]
        pub fn reveal_remote_images(&self, post_id: String) {
            self.manager.reveal_remote_images(post_id);
        }

        /// Re-query the **currently selected** feed source, whatever it is.
        /// Resolves `undefined`.
        ///
        /// Always prefer this to `selectFeed(snapshot.selected_feed)` at a
        /// refresh/reconnect/remount site: `selected_feed` is `null` both for the
        /// local feed *and* while Trending is selected, so re-selecting it
        /// silently drops a Trending viewer into Local (`trending.md` § The
        /// Trending feed) — a bug six apps each hit and hand-fixed during the
        /// Trending rollout, every one re-deriving a branch shared Rust owns.
        #[wasm_bindgen(js_name = refreshCurrentFeed)]
        pub fn refresh_current_feed(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.refresh_current_feed().await;
                Ok(JsValue::UNDEFINED)
            })
        }

        /// The manager's `{started, completed, committed_gen}` reload triple
        /// (`fauna_e2e_agent::FEED_RELOADS_KEY`, which owns the contract; the
        /// JSON shape is derived once in `fauna_feed::feed_reloads_json`, the
        /// same function the native apps publish). Synchronous — three atomic
        /// reads, no fetch — so the e2e state assembly may call it on the ack
        /// path.
        #[wasm_bindgen(js_name = feedReloads)]
        pub fn feed_reloads(&self) -> Result<JsValue, JsValue> {
            crate::rpc::to_js(&fauna_feed::feed_reloads_json(Some(
                self.manager.reload_counts(),
            )))
        }

        /// Act on a post from the interaction bar — `feed-{like,reply,repost,
        /// quote}-button` (`feed.md` § Interaction bar) — over
        /// `fauna.posts.interact`, folding the nest's post-act counters into the
        /// loaded window so the tapped count moves at once.
        ///
        /// **Use this instead of a raw `postsInteract` call.** The counts on
        /// screen come from the snapshot, and only this seam writes them — a
        /// direct RPC throws the reply away, which is why a tapped ♥ never moved
        /// here. `body` is the reply/quote text (`null` for like/repost).
        /// Resolves `undefined`; rejects with the error string.
        #[wasm_bindgen(js_name = interact)]
        pub fn interact(
            &self,
            post_id: String,
            action: String,
            body: Option<String>,
        ) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.interact(post_id, action, body)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Reply to a post (`feed-reply-button`) — composes a real post
        /// carrying `Reference::Reply`, which is the only thing that moves the
        /// target's `reply_count`. Resolves `undefined`; rejects with the
        /// error string.
        ///
        /// **Use this, never `interact(id, "reply", text)`.** That call looks
        /// identical and is not a reply: the nest's native arm discards `body`
        /// entirely. A bridged post still routes through interact internally.
        #[wasm_bindgen(js_name = reply)]
        pub fn reply(&self, post_id: String, body: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.reply(post_id, body)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Quote-repost a post (`feed-quote-button`) — composes a post carrying
        /// `Reference::Quote`, which the shipped `quoted-post` embed already
        /// renders. `body` is the commentary and may be empty. Resolves
        /// `undefined`; rejects with the error string.
        #[wasm_bindgen(js_name = quote)]
        pub fn quote(&self, post_id: String, body: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.quote(post_id, body)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// The confirmed-public reply (`ui/feed.md` § Encryption at rest →
        /// *Ruling 5's build — the shape*, (e)): the reply dialog took the
        /// user's explicit answer under `feed-reply-public-confirm`, so the
        /// words go out as the public reference [`reply`](Self::reply)
        /// rejects under a restricted target. Call it only while that
        /// checkbox is checked and the post's `reply_audience` is *public by
        /// confirmation*; a reply this device could seal rejects here, and a
        /// public target composes as `reply` would. Resolves `undefined`;
        /// rejects with the error string. Additive — `reply` is unchanged.
        #[wasm_bindgen(js_name = replyPublicConfirmed)]
        pub fn reply_public_confirmed(&self, post_id: String, body: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.reply_public_confirmed(post_id, body)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Repost / un-repost a post (`feed-repost-button`) — one verb, toggle
        /// semantics off the target row's `viewer_repost_id` (`feed.md`
        /// § Interaction bar → Repost, ratified 2026-08-10): absent → composes
        /// the caller's empty-body `Reference::Repost` post; present →
        /// un-reposts it through the interact door. A bridged post routes
        /// through interact verbatim — the manager decides. Resolves
        /// `undefined`; rejects with the error string.
        #[wasm_bindgen(js_name = repost)]
        pub fn repost(&self, post_id: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.repost(post_id)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Like / un-like a post (`feed-like-button`) — one verb, toggle
        /// semantics off the target row's `viewer_liked` (`feed.md`
        /// § Interaction bar). Both directions ride the same interact door on
        /// the same post id and fold the nest's post-act counters, so the
        /// count moves on a like AND on an un-like. Resolves `undefined`;
        /// rejects with the error string.
        ///
        /// **Use this, never `interact(id, "like", null)`.** That call is
        /// one-way: the nest's like arm is idempotent per (actor, post), so a
        /// second tap moves nothing and a like can never be taken back. A
        /// bridged post keeps the shipped one-way path internally — the
        /// manager decides.
        #[wasm_bindgen(js_name = like)]
        pub fn like(&self, post_id: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.like(post_id)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Delete a post the local actor authored (`feed-post-delete-confirm-button`).
        /// Builds + signs a verifiable tombstone, submits it, and drops the post
        /// from the loaded window at once — no refetch needed. Resolves
        /// `undefined`; rejects with the error string.
        #[wasm_bindgen(js_name = deletePost)]
        pub fn delete_post(&self, post_id: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.delete_post(post_id)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        // ── Engagement cues (engagement-cues.md §§ Cue vocabulary / At rest) ──

        /// Score the loaded window's **public** posts with one trained factor,
        /// best first, bounded to the shared review cap
        /// (`fauna_client_personalization::publish::REVIEW_TOP_N` — product
        /// behavior, identical on every app, so the boundary applies it
        /// itself rather than letting the SPA pick its own N) — the publish
        /// review-prune sheet's corpus read (`topic-factors.md` § Publishing a
        /// trained factor).
        ///
        /// Resolves an array of `{post_id, preview, score}` (the snake_case the
        /// SPA already reads off `listTrainedTopics`' rows). The corpus is
        /// deliberately the loaded window and nothing more (§ Publishing's
        /// accepted limitation), which is why this rides the manager: it owns
        /// the window. `factor` is the `topic:<hex>` key and may be **any**
        /// trained factor, not only one this feed composes — the user publishes
        /// from the Personalization home.
        #[wasm_bindgen(js_name = scoreCorpusForFactor)]
        pub fn score_corpus_for_factor(&self, factor: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                let scored = m
                    .score_corpus_for_factor(
                        &factor,
                        fauna_client_personalization::publish::REVIEW_TOP_N,
                    )
                    .await
                    .map_err(|e| JsValue::from_str(&e))?;
                let rows: Vec<JsScoredExemplar> = scored
                    .into_iter()
                    .map(|e| JsScoredExemplar {
                        post_id: e.post_id,
                        preview: e.preview,
                        score: e.score,
                    })
                    .collect();
                crate::rpc::to_js(&rows)
            })
        }

        /// Rebuild one trained factor's **publishable vocabulary** from its
        /// public, still-fetchable explicit examples — the Model half of the
        /// publish review-prune sheet's corpus read (`topic-factors.md` §
        /// Publishing a trained factor, v2).
        ///
        /// The List twin above scores the *loaded window*; this one walks the
        /// factor's own example markers and re-fetches each post, so what the
        /// SPA reviews is a publish-time rebuild over public text and never a
        /// serialization of private model state. Every example that cannot be
        /// confirmed public is an **exclusion**, not a fallback — hence
        /// `included_examples` beside `marked_examples` in the reply.
        ///
        /// There is no top-N here, and none may be added: the vocabulary **is**
        /// the disclosure, so every survivor of the shared prune floor crosses
        /// (§ Publishing's vocabulary bound is size bound and review bound at
        /// once). Resolves `{more_docs, less_docs, included_examples,
        /// marked_examples, ngrams: [{ngram, more, less}]}`.
        ///
        /// `factor` is the `topic:<hex>` key and may be **any** trained factor,
        /// not only one this feed composes — the user publishes from the
        /// Personalization home.
        #[wasm_bindgen(js_name = scrubCorpusForFactor)]
        pub fn scrub_corpus_for_factor(&self, factor: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                let review = m
                    .scrub_corpus_for_factor(&factor)
                    .await
                    .map_err(|e| JsValue::from_str(&e))?;
                crate::rpc::to_js(&JsTrainedModelReview {
                    more_docs: review.more_docs,
                    less_docs: review.less_docs,
                    included_examples: review.included_examples,
                    marked_examples: review.marked_examples,
                    ngrams: review
                        .ngrams
                        .into_iter()
                        .map(|n| JsReviewNgram {
                            ngram: n.ngram,
                            more: n.more,
                            less: n.less,
                        })
                        .collect(),
                })
            })
        }

        /// **Fetch-on-session-start** for the sealed `cues:v1` rollup — call once
        /// on entering the Feed page, before reporting observations. Absent ⇒ a
        /// fresh capture; unopenable ⇒ rejects (never a silent fresh rollup, which
        /// would erase other devices' cues on the next put). Resolves `undefined`.
        #[wasm_bindgen(js_name = hydrateCues)]
        pub fn hydrate_cues(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.hydrate_cues()
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Report one per-exposure engagement observation when a card leaves the
        /// viewport (or its media ends). The shell supplies only what a
        /// visibility/playback observer can honestly compute — the peak playback
        /// fraction (`mediaPlayedPm`, per-mille; `undefined` for a non-media post)
        /// and the cumulative substantially-visible dwell at the two gate
        /// fractions; **all derivation is shared Rust**. `observedAtMs` is the
        /// shell's event time (the manager reads no clock). Resolves `undefined`.
        #[wasm_bindgen(js_name = recordObservation)]
        pub fn record_observation(
            &self,
            content_id: String,
            is_media: bool,
            media_played_pm: Option<u32>,
            dwell_ms_at_skip_visibility: u64,
            dwell_ms_at_long_visibility: u64,
            observed_at_ms: u64,
        ) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.record_observation(CueObservation {
                    content_id,
                    is_media,
                    media_played_pm,
                    dwell_ms_at_skip_visibility,
                    dwell_ms_at_long_visibility,
                    observed_at_ms,
                })
                .await
                .map(|_verdict| JsValue::UNDEFINED)
                .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Force a put of any unsaved cues — the **background / app-close flush**
        /// (`beforeunload` / `visibilitychange`). A no-op when nothing is dirty or
        /// before hydration. Resolves `undefined`.
        #[wasm_bindgen(js_name = flushCues)]
        pub fn flush_cues(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.flush_cues()
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Delete the sealed `cues:v1` rollup — the user's own destruction of
        /// their revocable cue data (Personalization home). Drops the nest row and
        /// resets the live engine. Resolves `undefined`.
        #[wasm_bindgen(js_name = deleteCueRollup)]
        pub fn delete_cue_rollup(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.delete_cue_rollup()
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        // ── Layer-B signal sharing (engagement-cues.md § Layer B) ─────────────
        // The wasm twins of `FfiFeedManager::{hydrate_signal_optin,
        // signal_share_status, set_signal_sharing}`. They drive THIS manager —
        // the live one whose producer reads the cached opt-in — never a fresh
        // moderation client, which would neither update that cache nor be the
        // manager the capture shell reports into.

        /// Session-start hydrate of the Layer-B opt-in cache, so the producer
        /// honours a persisted opt-in before the user ever opens Personalization
        /// (the feed page calls it beside `hydrateCues`). Resolves the cached
        /// `boolean`.
        #[wasm_bindgen(js_name = hydrateSignalOptin)]
        pub fn hydrate_signal_optin(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.hydrate_signal_optin()
                    .await
                    .map(JsValue::from_bool)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// The caller's signal-sharing opt-in and the transparency export list:
        /// resolves `{ share, published: [{ content_hash, factor, count }] }`
        /// (the nest-wide ≥k export view, `report:*` and `signal:*` alike).
        /// Also caches `share` for the producer.
        #[wasm_bindgen(js_name = signalShareStatus)]
        pub fn signal_share_status(&self) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                let reply = m
                    .signal_share_status()
                    .await
                    .map_err(|e| JsValue::from_str(&e))?;
                crate::rpc::to_js(&reply)
            })
        }

        /// Set the opt-in, then resolve the re-read status (same shape as
        /// `signalShareStatus`) — the toggle renders the reply's `share`, never
        /// the click. Opting out withdraws this actor's `signal:*` rows.
        #[wasm_bindgen(js_name = setSignalSharing)]
        pub fn set_signal_sharing(&self, share: bool) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                let reply = m
                    .set_signal_sharing(share)
                    .await
                    .map_err(|e| JsValue::from_str(&e))?;
                crate::rpc::to_js(&reply)
            })
        }

        // ── Trained topic factors (topic-factors.md § Training signals) ──────

        /// **More like this** / **less like this** on a post. `verb` is
        /// `"more"` or `"less"`. Trains the user's sealed `topic:<hex>` model on
        /// the post's full text, re-seals it under their BackupKey, stores it
        /// nest-opaque, and re-ranks the loaded window at once — the nest learns
        /// nothing about what the model contains or which posts matched.
        ///
        /// Resolves the outcome as `"trained" | "duplicate" | "flipped"`:
        /// re-tapping the same verb is a duplicate that writes nothing, and
        /// tapping the other verb flips exactly (never a double-count).
        #[wasm_bindgen(js_name = trainPost)]
        pub fn train_post(&self, post_id: String, factor: String, verb: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                let verb = match verb.as_str() {
                    "more" => TrainVerb::MoreLikeThis,
                    "less" => TrainVerb::LessLikeThis,
                    other => {
                        return Err(JsValue::from_str(&format!(
                            "trainPost: verb must be \"more\" or \"less\", got {other:?}"
                        )));
                    }
                };
                m.train_post(post_id, factor, verb)
                    .await
                    .map(|outcome| JsValue::from_str(train_result_js(outcome)))
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Un-mark a post (tapping its active verb off): applies the exact
        /// inverse of the delta it trained and drops the marker. Resolves
        /// `undefined`.
        #[wasm_bindgen(js_name = untrainPost)]
        pub fn untrain_post(&self, post_id: String, factor: String) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.untrain_post(post_id, factor)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// The trained factor a gesture on this feed trains **in context** — the
        /// feed's single `topic:*` factor, if it has exactly one. `null` ⇒ open
        /// the target sheet rather than guess.
        #[wasm_bindgen(js_name = trainTargetFactor)]
        pub fn train_target_factor(&self) -> Option<String> {
            self.manager.train_target_factor()
        }

        /// This post's toggle state for `factor` — `"more"`, `"less"`, or `null`.
        /// Paints the more/less-like-this menu items as active; survives restarts
        /// and reaches every device (the markers live inside the sealed model).
        #[wasm_bindgen(js_name = exampleLabelFor)]
        pub fn example_label_for(&self, post_id: String, factor: String) -> Option<String> {
            self.manager
                .example_label_for(&post_id, &factor)
                .map(|v| match v {
                    TrainVerb::MoreLikeThis => "more".to_string(),
                    TrainVerb::LessLikeThis => "less".to_string(),
                })
        }

        /// Does this post match one of the user's muted words? Drives the
        /// collapse-to-placeholder render treatment, which applies **everywhere**
        /// — including chronological feeds, where a mute cannot sink a post but
        /// must still collapse it.
        #[wasm_bindgen(js_name = isMuted)]
        pub fn is_muted(&self, post_id: String) -> bool {
            self.manager.is_muted(&post_id)
        }

        // ── e2e test-helper surface ──────────────────────────────────────────
        //
        // The browser twin of the manager's `#[cfg(any(test, debug_assertions,
        // feature = "test-helpers"))]` injection seam, driven from the Playwright
        // e2e command bridge via `window.__fauna_callCommand`. Lets the tier_2
        // unverified-source-badge test inject a post list — including one whose
        // `verification` is `Failed` — without a real nest query (a real signed
        // post is only ever `Unchecked`/`Verified`).
        //
        // ⚠ Gated on this crate's off-by-default `test-helpers` feature, so a
        // production (release) wasm build does not export it — see the fuller note
        // on the conversations seam block in `src/conversations.rs`. This comment
        // used to say "compiled in always … inert in production"; it shipped
        // callable instead, which is what testing.md § convention 15 forbids.

        /// Replace the feed snapshot with a `Loaded` list built from `specs` (a JS
        /// array of `fauna_feed::test_support::TestPostSpec`: `{ post_id, author,
        /// body?, verification?, timestamp?, source?, tags?, has_media?, is_reply?,
        /// quoted? }`, where `quoted` is `{ post_id, author, body?, verification? }`)
        /// and fire the observer, so the next `snapshot()` paints them. Each post's
        /// `document` is rendered from its `body` (and any `quoted` embed) in Rust —
        /// JS never builds a `RenderDocument`. Browser twin of linux
        /// `handle_feed_inject_posts`.
        #[cfg(feature = "test-helpers")]
        #[wasm_bindgen(js_name = injectPostsForTest)]
        pub fn inject_posts_for_test(&self, specs: JsValue) -> Result<(), JsValue> {
            let specs: Vec<fauna_feed::test_support::TestPostSpec> =
                serde_wasm_bindgen::from_value(specs)
                    .map_err(|e| JsValue::from_str(&format!("inject posts: {e}")))?;
            self.manager.set_feed_snapshot_for_test(
                fauna_feed::test_support::feed_snapshot_with_posts(specs),
            );
            Ok(())
        }

        /// Stamp `FeedSnapshot.error` — the exact observable state a failed
        /// background fetch leaves — and fire the observer, so the page surfaces
        /// it on `error-message` (`feed.md` § Errors & edge cases). `key` is an
        /// i18n key and `message` its `{message}` substitution, the same
        /// `LocalizedText::key_arg` carrier a real failure (`feed.error_load`)
        /// uses. Browser twin of the ffi `inject_error_for_test` and tui's
        /// `feed_inject_error`.
        #[cfg(feature = "test-helpers")]
        #[wasm_bindgen(js_name = injectErrorForTest)]
        pub fn inject_error_for_test(&self, key: String, message: String) {
            self.manager
                .inject_error_for_test(fauna_core::localized::LocalizedText::key_arg(
                    key, "message", message,
                ));
        }

        /// Seed the live engagement-cue engine with `content_ids` (each recorded a
        /// `WatchComplete` verdict) and PUT the sealed rollup to the nest for
        /// real — unlike `injectPostsForTest`, a real nest round trip so web's
        /// capture-less client can reach "Clear activity data" with an actual
        /// `cues:v1` row to delete. See
        /// `fauna_feed::FeedManager::set_cue_rollup_for_test`. Resolves `undefined`.
        #[cfg(feature = "test-helpers")]
        #[wasm_bindgen(js_name = setCueRollupForTest)]
        pub fn set_cue_rollup_for_test(&self, content_ids: Vec<String>) -> js_sys::Promise {
            let m = self.manager.clone();
            future_to_promise(async move {
                m.set_cue_rollup_for_test(content_ids)
                    .await
                    .map(|()| JsValue::UNDEFINED)
                    .map_err(|e| JsValue::from_str(&e))
            })
        }

        /// Arm the one-shot hold on the NEXT reload (`feed_hold_next_reload`).
        /// See `fauna_feed::FeedManager::hold_next_reload_for_test`.
        #[cfg(feature = "test-helpers")]
        #[wasm_bindgen(js_name = holdNextReloadForTest)]
        pub fn hold_next_reload_for_test(&self) {
            self.manager.hold_next_reload_for_test();
        }

        /// Release the held reload, or disarm a hold no reload reached yet
        /// (`feed_release_held_reload`). See
        /// `fauna_feed::FeedManager::release_held_reload_for_test`.
        #[cfg(feature = "test-helpers")]
        #[wasm_bindgen(js_name = releaseHeldReloadForTest)]
        pub fn release_held_reload_for_test(&self) {
            self.manager.release_held_reload_for_test();
        }

        /// Whether a hold is armed and no reload has reached it yet — while it
        /// is, the SPA's e2e agent STARTS a feed op instead of awaiting it. See
        /// `fauna_feed::FeedManager::reload_hold_armed_for_test`.
        #[cfg(feature = "test-helpers")]
        #[wasm_bindgen(js_name = reloadHoldArmedForTest)]
        pub fn reload_hold_armed_for_test(&self) -> bool {
            self.manager.reload_hold_armed_for_test()
        }
    }

    // ── Cue capture: the shared tracker ──────────────────────────────────────

    /// The shared engagement-cue capture tracker ([`fauna_feed::CueTracker`]) for
    /// the SPA — the web twin of the native `FfiCueTracker`
    /// (`engagement-cues.md` § Cue vocabulary & derivation, boundary revised
    /// 2026-07-29).
    ///
    /// Web's capture shell (`apps/fauna-web/src/lib/feed-cues.ts`) consumes the
    /// shared bookkeeping through this face rather than being a hand-written
    /// copy — which is exactly how the first four shells accumulated the drift
    /// the revision retired.
    ///
    /// The shell owns only its geometry probe (per-row `top`/`height` from
    /// `getBoundingClientRect`, plus the scroll container's bounds), its tick
    /// scheduling, its lifecycle, and passing each returned observation to
    /// `WasmFeedManager.recordObservation`. It must **not** bucket dwell,
    /// accumulate credit, decide leaves, or filter noise itself.
    ///
    /// `RefCell`, not a lock: wasm is single-threaded, and every method borrows
    /// only for the duration of one synchronous call.
    #[wasm_bindgen]
    pub struct WasmCueTracker {
        inner: std::cell::RefCell<CueTracker>,
    }

    #[wasm_bindgen]
    impl WasmCueTracker {
        /// A tracker for a container with the given leave model, as a stable
        /// string key — `"hold-unmeasured"` for an eager/retained list (every row
        /// keeps a node), `"absence-is-leave"` for a virtualizing one (an
        /// off-screen row is removed from the DOM). A plain DOM list is the
        /// former; a windowed/virtual-scroller list is the latter. Throws on any
        /// other value rather than guessing — picking the wrong model silently
        /// either strands dwell forever or fabricates leaves.
        #[wasm_bindgen(constructor)]
        pub fn new(leave_model: &str) -> Result<WasmCueTracker, JsValue> {
            let leave_model = match leave_model {
                "hold-unmeasured" => LeaveModel::HoldUnmeasured,
                "absence-is-leave" => LeaveModel::AbsenceIsLeave,
                other => {
                    return Err(JsValue::from_str(&format!(
                        "unknown leave model {other:?} — expected \
                         \"hold-unmeasured\" or \"absence-is-leave\""
                    )));
                }
            };
            Ok(Self {
                inner: std::cell::RefCell::new(CueTracker::new(leave_model)),
            })
        }

        /// One probe read. `rows` is an array of
        /// `{ post_id, top, height, is_media, media_played_pm }` — every row the
        /// shell could read this tick (a row it measured as not-yet-arranged is
        /// included with a non-positive `height`; one it could not read at all is
        /// simply omitted — the tracker holds both). `windowPostIds` is every
        /// post in the loaded window, not just the rendered rows.
        ///
        /// `monoNowMs` MUST come from `performance.now()`, never `Date.now()` —
        /// a wall clock here lets an NTP step or a date change inflate dwell,
        /// which is the drift this revision fixed. `wallNowMs` is `Date.now()`
        /// and only stamps the observation. Both are `f64` because a `u64` wasm
        /// parameter reaches JS as a `BigInt` and a plain number argument then
        /// throws at the boundary.
        ///
        /// Returns the finished exposures — each is a plain object whose fields
        /// are exactly `recordObservation`'s arguments.
        #[wasm_bindgen(js_name = sample)]
        pub fn sample(
            &self,
            rows: JsValue,
            window_post_ids: Vec<String>,
            viewport_start: f64,
            viewport_end: f64,
            mono_now_ms: f64,
            wall_now_ms: f64,
        ) -> Result<JsValue, JsValue> {
            let rows: Vec<CueRow> = crate::rpc::from_js(rows)?;
            let left = self.inner.borrow_mut().sample(
                &rows,
                &window_post_ids,
                viewport_start,
                viewport_end,
                ms(mono_now_ms),
                ms(wall_now_ms),
            );
            crate::rpc::to_js(&left)
        }

        /// Everything tracked has left the viewport (the SPA navigated off the
        /// Feed route, or the page is unloading) — drain, emit, and reset the
        /// credit baseline. `wallNowMs` is `Date.now()`.
        #[wasm_bindgen(js_name = drainAll)]
        pub fn drain_all(&self, wall_now_ms: f64) -> Result<JsValue, JsValue> {
            let left = self.inner.borrow_mut().drain_all(ms(wall_now_ms));
            crate::rpc::to_js(&left)
        }
    }

    /// A JS millisecond reading as the `u64` the tracker takes. Negative and
    /// non-finite readings clamp to 0 — a clock a browser should never produce,
    /// and crediting nothing beats crediting garbage.
    fn ms(value: f64) -> u64 {
        if value.is_finite() && value > 0.0 {
            value as u64
        } else {
            0
        }
    }

    /// The shared sampling cadence
    /// ([`fauna_core::scoring::cues::CUE_SAMPLE_INTERVAL_MS`]) — a shell reads
    /// its tick interval from here and never re-declares it, so the tick rate
    /// and the dwell thresholds stay one calibration. Returned as a plain number
    /// (not a `BigInt`), so it drops straight into `setInterval`.
    #[wasm_bindgen(js_name = cueSampleIntervalMs)]
    pub fn cue_sample_interval_ms() -> f64 {
        fauna_core::scoring::cues::CUE_SAMPLE_INTERVAL_MS as f64
    }
}

#[cfg(target_arch = "wasm32")]
pub use manager::{WasmCueTracker, WasmFeedManager, cue_sample_interval_ms};
