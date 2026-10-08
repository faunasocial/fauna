//! UniFFI façade for the shared, stateful Feed page (`fauna_feed::FeedManager`)
//! — the native-client (Windows / Apple / Android) seam onto the snapshot the
//! Rust-native Linux app drives `FeedManager<Arc<NestClient>>` directly, and
//! web reaches via `fauna-wasm` (`docs/goal/ui/feed.md` § State & data shape,
//! ratified 2026-06-14: "the manager is exposed to native apps via a
//! `#[uniffi::export]` façade in `libs/fauna-ffi` … exactly as
//! `ConversationsManager` is").
//!
//! [`FeedManager<R>`] is **generic over the WS-RPC transport** and a generic
//! type can't be `#[uniffi::export]`ed, so — exactly like [`FfiFeedClient`] —
//! the export lives on this **concrete** façade wrapping
//! `FeedManager<Arc<NestClient>>`. Construct via
//! [`crate::nest_client::FfiNestClient::feed_manager`].
//!
//! ## Why the whole module is gated `feed-manager`
//!
//! The snapshot types (`FeedSnapshot` / `PostSummary` / `QuotedPostView` / …)
//! and the `FeedSnapshotObserver` foreign trait are `fauna_feed` types, returned
//! **directly** (no fauna-ffi-local mirrors — priority #2; the gated `logs` /
//! `value-format` / `web-content` features already prove a fauna-ffi export can
//! return a cross-crate `uniffi::Record` for the Swift/Kotlin bindings). But
//! `uniffi-bindgen-go` emits an uncompilable bare `fauna_feed` cross-namespace
//! import for them, so the feature is **default-on for the native app FFI**
//! and **off in the Go mail-bridge `--no-default-features` build** — the bridge
//! is a server with no Feed UI (same Go-incompatibility gate as
//! `conversations-session`). The feature also turns on `fauna-feed/uniffi` so
//! the snapshot types get their `uniffi::Record`/`Enum` registration.
//!
//! [`FeedManager<R>`]: fauna_feed::FeedManager
//! [`FfiFeedClient`]: crate::feed_client::FfiFeedClient

use std::sync::Arc;

use crate::FfiError;
use crate::moderation_client::FfiReportShareStatus;
use fauna_client::NestClient;
use fauna_feed::{
    AttachedFile, CueObservation, CueRow, CueTracker, FactorWeightInput, FeedManager, FeedSnapshot,
    FeedSnapshotObserver, FilterRuleInput, LeaveModel, QuotedPostView, ScoredExemplar,
    SellComposeState, TrainResult, TrainVerb, TrainedModelReview,
};

/// UniFFI handle wrapping the shared, stateful `FeedManager<Arc<NestClient>>`.
/// Methods are exposed to Swift as `async`/`func`, Kotlin as `suspend fun`/`fun`,
/// and C# as `Task`/sync. The snapshot read + observer wiring + compose edits
/// are synchronous; every user action that drives a WS-RPC call is async.
#[derive(uniffi::Object)]
pub struct FfiFeedManager {
    inner: Arc<FeedManager<Arc<NestClient>>>,
}

impl FfiFeedManager {
    /// Build over an authed [`NestClient`] + the local actor's 32-byte ed25519
    /// signing secret (needed to build + sign posts on `submit_post`). The
    /// snapshot starts empty + `Loading`; the client calls `refresh_feeds` /
    /// `select_feed` on entering the Feed page.
    pub(crate) fn new(nest: Arc<NestClient>, actor_secret: [u8; 32]) -> Arc<Self> {
        let inner = FeedManager::new(nest, actor_secret);
        inner.set_period_key_store(crate::account_runtime::period_key_store());
        inner.set_preference_store(Arc::new(crate::account_runtime::handle_source()));
        Arc::new(Self {
            inner: Arc::new(inner),
        })
    }
}

// ── Synchronous surface ──────────────────────────────────────────────────────
// snapshot read + observer reactivity + composer edits — no WS-RPC, so no async.
#[uniffi::export]
impl FfiFeedManager {
    /// A cheap clone of the current [`FeedSnapshot`]. The client's
    /// [`FeedSnapshotObserver`] re-reads this on every `on_changed()`.
    pub fn snapshot(&self) -> FeedSnapshot {
        self.inner.snapshot()
    }

    /// Register a foreign (Swift/Kotlin/C#) observer notified after every state
    /// mutation. `FeedSnapshotObserver` is `#[uniffi::export(with_foreign)]`, so the
    /// foreign object implements it directly.
    pub fn add_observer(&self, observer: Arc<dyn FeedSnapshotObserver>) {
        self.inner.add_observer(observer);
    }

    /// Drop all registered observers (call at sign-out so stale receiver loops
    /// close — each authenticated-window build attaches a fresh observer).
    pub fn clear_observers(&self) {
        self.inner.clear_observers();
    }

    /// Update the composer (`compose-text-field` / `compose-tags-field` / staged
    /// file) and clear any stale compose error. Validation (non-empty, tag
    /// normalization, media build) happens at `submit_post`.
    pub fn update_compose(&self, text: String, tags: String, attached_file: Option<AttachedFile>) {
        self.inner.update_compose(text, tags, attached_file);
    }

    /// Stage the composer's gate-to-tier fields (`compose-gate-tier-select` /
    /// `compose-gate-preview-field`) — the gate sibling of [`update_compose`](Self::update_compose).
    /// `gate_tier: None` composes a normal (ungated) post.
    pub fn update_compose_gate(&self, gate_tier: Option<String>, gate_preview: String) {
        self.inner.update_compose_gate(gate_tier, gate_preview);
    }

    /// Stage the composer's **"Sell this post…"** fields — the sell sibling of
    /// [`update_compose_gate`](Self::update_compose_gate), sharing its teaser.
    /// `Some` enters sell mode and clears any selected gate tier; `None` leaves
    /// it. `monetization.md` § Per-post pay-to-unlock.
    // `payments`-gated doc line: the ids are the price-and-route class
    // (`dynamic-features.md` § Platform-family surface excision), and a UniFFI
    // docstring rides every artifact (criterion 1, prose included) — gate the
    // line, never reword it (`value_format.rs` § Gated element ids live in
    // gated DOC LINES).
    #[cfg_attr(
        feature = "payments",
        doc = " The fields paint `compose-sell-price` / `compose-sell-subscribers-free`."
    )]
    pub fn update_compose_sell(&self, sell: Option<SellComposeState>, gate_preview: String) {
        self.inner.update_compose_sell(sell, gate_preview);
    }

    /// Stage the composer's **room** answer — `compose-gate-tier-select`'s
    /// room option, a room from `snapshot().own_rooms` by its hex channel id —
    /// the room sibling of [`update_compose_gate`](Self::update_compose_gate),
    /// sharing its teaser. `Some` clears a tier and a sale; `None` leaves the
    /// room answer (back to Public). `ui/feed.md` § Encryption at rest →
    /// *Room-restricted — the app half*.
    pub fn update_compose_room(&self, gate_room: Option<String>, gate_preview: String) {
        self.inner.update_compose_room(gate_room, gate_preview);
    }

    /// Stage the teaser (`compose-gate-preview-field`) ALONE — touching no
    /// audience answer. Write the teaser through this, never through the
    /// setter of whichever answer is selected: `update_compose_gate` re-reads
    /// the tier and would drop a room answer.
    pub fn update_compose_preview(&self, gate_preview: String) {
        self.inner.update_compose_preview(gate_preview);
    }

    /// The DAG-CBOR `UploadSidecar` bytes the **staged** gated post's sealed
    /// body uploads under — `GroupRestrictedPost` for a room post, a tier's
    /// `PeriodRestrictedPost` otherwise. Call it after
    /// [`prepare_gated_blob`](Self::prepare_gated_blob) /
    /// [`prepare_sell_post`](Self::prepare_sell_post) and POST it as the
    /// `sidecar` part beside the sealed bytes, in place of the free
    /// `gated_post_sidecar()`, which knows only the tier class and would
    /// mis-tag a room post's body. The class is decided off the staged post,
    /// never by an app (`FeedManager::gated_upload_sidecar`).
    pub fn gated_upload_sidecar(&self) -> Vec<u8> {
        self.inner.gated_upload_sidecar().to_dag_cbor()
    }

    /// The composer's canonical at-rest draft bytes — the posts-rail twin of
    /// `FfiConversationsManager.draftsSnapshotBytes`. The client leg hands these
    /// to `FfiDraftsSync.saveIfChanged` from its compose debounce; the seal and
    /// the `fauna.drafts.put` call live in the shared crate, never here
    /// (`reserved-folders.md` § Drafts Sync).
    pub fn drafts_snapshot_bytes(&self) -> Vec<u8> {
        self.inner.drafts_snapshot_bytes()
    }

    /// Restore the composer from bytes `FfiDraftsSync.load` returned for the
    /// `"posts"` rail — the load-on-launch / cross-device catch-up path. An
    /// unreadable or empty blob is a no-op (the composer stays usable), so the
    /// leg needs no error arm of its own.
    pub fn restore_drafts(&self, bytes: Vec<u8>) {
        self.inner.restore_drafts(bytes);
    }
}

// ── Asynchronous surface ─────────────────────────────────────────────────────
// One method per user action; each drives the WS-RPC call, mutates the snapshot,
// and notifies. `Result<_, String>` errors map onto `FfiError` (the page-/form-
// level errors also land in the snapshot's `error`/`compose.error`/`bridge_form`
// fields, exactly as the Rust-native Linux app sees them).
#[fauna_uniffi_async::export]
impl FfiFeedManager {
    /// Refresh the feed selector list (`fauna.feed.list`). Called once on
    /// entering the Feed page; create/delete refresh it.
    pub async fn refresh_feeds(&self) {
        self.inner.refresh_feeds().await;
    }

    /// Refresh the subscribed bridge-feed list (`fauna.bridges.feeds.list`) — the
    /// `bridge-feed-unsubscribe-button` rows. Called once on entering the page;
    /// subscribe/unsubscribe refresh it.
    pub async fn refresh_bridge_feeds(&self) {
        self.inner.refresh_bridge_feeds().await;
    }

    /// Refresh the nest's available bridges (`snapshot().available_bridges`) — the
    /// `bridge-form-bridge-select` option set. Called once on entering the page,
    /// beside `refresh_bridge_feeds`. The client populates the selector from the
    /// snapshot instead of a hard-coded protocol list, so it never offers a
    /// protocol the nest can't serve (`version-compatibility.md` § Dim 3).
    pub async fn refresh_available_bridges(&self) {
        self.inner.refresh_available_bridges().await;
    }

    /// Refresh the composer's gate-to-tier option set (`compose-gate-tier-select`)
    /// from the local actor's own tiers (`fauna.subscriptions.tiers.list`). A
    /// Rust-native app that reloads on feed navigation (Linux) gets this for
    /// free inside `reload()`; a client whose feed manager persists across nav
    /// (Android — the singleton `FeedManagerHost`, like the web SPA) calls this
    /// when the composer opens so a tier the user just minted shows up in the gate
    /// select. Best-effort — a transport failure leaves the prior set (an empty set
    /// just means "no gating offered"), never a page error. The native twin of the
    /// wasm `refreshOwnTiers`.
    pub async fn refresh_own_tiers(&self) {
        self.inner.refresh_own_tiers().await;
    }

    /// Re-read the rooms the composer offers (`snapshot().own_rooms`) from the
    /// installed room-post seam — the room sibling of
    /// [`refresh_own_tiers`](Self::refresh_own_tiers). A local read, no
    /// WS-RPC; notifies only when the list changed. `refresh_feeds` runs it
    /// too, but the list is a projection of the conversations plane, so call
    /// it on that plane's own change tick as well: a room joined or left then
    /// reaches the audience select without re-entering the feed.
    pub async fn refresh_own_rooms(&self) {
        self.inner.refresh_own_rooms().await;
    }

    /// Select a feed (`feed-item`) and load its first page. `None` ⇒ the nest's
    /// local feed. Clears any Trending selection.
    pub async fn select_feed(&self, feed_id: Option<String>) {
        self.inner.select_feed(feed_id).await;
    }

    /// Select the built-in **Trending** virtual feed (`feed-trending-item`) and
    /// load its first page (`trending.md` § The Trending feed) — the scored
    /// sibling of the local feed over `fauna.feed.trending.posts`, no feed row.
    /// Mirrors `select_feed(None)` = local; sets `snapshot().trending_selected`.
    pub async fn select_trending_feed(&self) {
        self.inner.select_trending_feed().await;
    }

    /// Update the search term and **re-query** the selected feed
    /// (`search = Some(term)`) — never a client-side filter. An empty/whitespace
    /// term clears the search.
    pub async fn set_search_query(&self, term: Option<String>) {
        self.inner.set_search_query(term).await;
    }

    /// Clear the search (`feed-search-clear`) and re-query.
    pub async fn clear_search(&self) {
        self.inner.clear_search().await;
    }

    /// Load the next page (`Load more`) and append it (dedup by `post_id`,
    /// nest order preserved). No-op when no further page or a load is in flight.
    pub async fn load_more(&self) {
        self.inner.load_more().await;
    }

    /// Submit the composed post (`post-submit-button`): validate non-empty, build
    /// + sign via the shared `build_post` / `build_post_with_media`, create over
    /// `fauna.posts.create`, then clear the composer + refresh the list.
    pub async fn submit_post(&self) -> Result<(), FfiError> {
        self.inner.submit_post().await.map_err(FfiError::from)
    }

    /// Process + seal one compose attachment for the composer's **current**
    /// audience, returning the multipart parts to POST to `/api/v1/blob`.
    ///
    /// The native apps' door onto the shared-Rust seal-by-id helper
    /// (`ui/media.md` § Encryption at rest). Use it in place of
    /// [`process_and_seal_upload`](crate::media_upload::process_and_seal_upload)
    /// for a **feed compose**: that binding expresses only the two client-key
    /// audiences by design, because a tier's period key must never cross this
    /// boundary — so an audience-restricted post's photo can only be sealed on
    /// the far side of it, here.
    ///
    /// **Call it at submit, after the audience is final.** Uploading at pick
    /// time publishes a plaintext copy of a restricted post's picture that no
    /// blob DELETE exists to remove. POST
    /// [`thumbnail`](fauna_feed::ComposeAttachmentUpload::thumbnail) first
    /// (best-effort), then `primary`, then stage the returned hash with
    /// [`update_compose`](Self::update_compose) — with `media_type` taken from
    /// this reply, never from the sidecar (a sealed sidecar says
    /// `application/octet-stream`) and never from the filename.
    pub async fn seal_compose_attachment(
        &self,
        raw: Vec<u8>,
    ) -> Result<fauna_feed::ComposeAttachmentUpload, FfiError> {
        self.inner
            .seal_compose_attachment(raw)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Build + sign the staged **gated** post and return its sealed full-body
    /// blob for the client to upload (`POST /api/v1/blob`, sidecar class
    /// `PeriodRestrictedPost`, mime `application/octet-stream` — the strict
    /// verifier's sealed-class shape; platform glue by convention, exactly like
    /// media). `Ok(None)` means the composer isn't gated — call
    /// [`submit_post`](Self::submit_post) instead. On success the signed post
    /// is staged for [`submit_gated_post`](Self::submit_gated_post).
    pub async fn prepare_gated_blob(&self) -> Result<Option<Vec<u8>>, FfiError> {
        self.inner
            .prepare_gated_blob()
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// The reply dialog's twin of [`prepare_gated_blob`](Self::prepare_gated_blob)
    /// (`ui/feed.md` § Encryption at rest → *Ruling 5's build — the shape*, (c)):
    /// when `post_id` is a restricted post this device can author under —
    /// `PostSummary.reply_audience` says so — the reply is built sealed to that
    /// same audience and its sealed body returned for the upload the app
    /// already has ([`gated_upload_sidecar`](Self::gated_upload_sidecar), then
    /// [`submit_gated_post`](Self::submit_gated_post)). `Ok(None)` means
    /// nothing to upload — call [`reply`](Self::reply) as before.
    pub async fn prepare_sealed_reply(
        &self,
        post_id: String,
        body: String,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        self.inner
            .prepare_sealed_reply(post_id, body)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// The quote twin of [`prepare_sealed_reply`](Self::prepare_sealed_reply);
    /// a wordless quote answers `Ok(None)` — call [`quote`](Self::quote).
    pub async fn prepare_sealed_quote(
        &self,
        post_id: String,
        body: String,
    ) -> Result<Option<Vec<u8>>, FfiError> {
        self.inner
            .prepare_sealed_quote(post_id, body)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// **Sell this post** — auto-mint a degenerate single-post subscription
    /// tier and gate the staged composer text to it, returning the sealed blob
    /// to upload (`monetization.md` § Per-post pay-to-unlock).
    ///
    /// The upload + create glue is **shared with the ordinary gated flow**:
    /// finish with [`submit_gated_post`](Self::submit_gated_post), or
    /// [`abort_gated_submit`](Self::abort_gated_submit) if the upload fails.
    /// The composer supplies the sold text and the public teaser exactly as for
    /// a gated post; the gate *tier* select is unused, since this mints its own.
    ///
    /// `subscribers_get_it_free` is the single ratified knob: `true` ⇒ included
    /// in every paid subscription, `false` ⇒ pure pay-per-view.
    ///
    /// `asking_price_sats` is the **machine-comparable** price in the author's
    /// own unit (`monetization.md` § The asking price), distinct from
    /// `price_hint`'s human string: `None` leaves the post buyable only
    /// through an explicit-intent mechanism, and a zap on it stays a tip.
    /// **Phase one of "Sell this post…", for a compose that carries an
    /// attachment** — mint the unlock tier and persist its period key, creating
    /// nothing server-side.
    ///
    /// Call it between `update_compose_sell` and
    /// [`seal_compose_attachment`](Self::seal_compose_attachment): a sold
    /// post's photo seals under the tier the sale mints, and that tier does not
    /// exist when the author picks the file. **With no attachment, do not call
    /// it** — [`prepare_sell_post`](Self::prepare_sell_post) runs this itself,
    /// so the existing one-call flow is unchanged.
    ///
    /// Pass the same `subscribers_get_it_free` / `asking_price_sats` the
    /// following `prepare_sell_post` will: this call decides the tier's rank
    /// (so the sale cannot change arms after the photo sealed) and refuses an
    /// unconvertible asking price while refusing is still free. Editing the
    /// sale afterwards drops the stage, and `prepare_sell_post` then refuses
    /// rather than publishing a photo nobody can open.
    pub async fn stage_sell_tier(
        &self,
        subscribers_get_it_free: bool,
        asking_price_sats: Option<u64>,
    ) -> Result<(), FfiError> {
        self.inner
            .stage_sell_tier(subscribers_get_it_free, asking_price_sats)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    pub async fn prepare_sell_post(
        &self,
        price_hint: Option<String>,
        subscribers_get_it_free: bool,
        asking_price_sats: Option<u64>,
    ) -> Result<Vec<u8>, FfiError> {
        self.inner
            .prepare_sell_post(price_hint, subscribers_get_it_free, asking_price_sats)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Abort a staged gated submit whose **blob upload failed** (the platform
    /// glue between [`prepare_gated_blob`](Self::prepare_gated_blob) and
    /// [`submit_gated_post`](Self::submit_gated_post)): drop the staged post,
    /// clear `submitting`, and surface the upload error on `compose-error` —
    /// the composer keeps its text for a manual retry. Serves the
    /// [`prepare_sell_post`](Self::prepare_sell_post) flow too.
    pub fn abort_gated_submit(&self, message: String) {
        self.inner.abort_gated_submit(message);
    }

    /// Create the gated post staged by [`prepare_gated_blob`](Self::prepare_gated_blob),
    /// after the client uploaded the sealed blob. `uploaded_hash` is the
    /// upload reply's hex hash — it must echo the staged post's
    /// `encrypted_ref` (a mismatch means the upload glue mangled the bytes).
    pub async fn submit_gated_post(&self, uploaded_hash: String) -> Result<(), FfiError> {
        self.inner
            .submit_gated_post(uploaded_hash)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Create a feed (`create_feed` submit) — encodes each `(type, value,
    /// required)` rule via the shared `encode_filter_rule`, splits `factors`
    /// (the `feed-factor-*` editor's entries) into this feed's own
    /// composition vs. the caller's global factor set, calls
    /// `fauna.feed.create`, refreshes the list. Returns the new `feed_id`.
    pub async fn create_feed(
        &self,
        name: String,
        rules: Vec<FilterRuleInput>,
        combination: String,
        scope: Option<String>,
        contributor_seeds: Option<Vec<String>>,
        factors: Vec<FactorWeightInput>,
    ) -> Result<String, FfiError> {
        self.inner
            .create_feed(name, rules, combination, scope, contributor_seeds, factors)
            .await
            .map_err(FfiError::from)
    }

    /// Delete a feed (`feed-delete-button`; confirmation is client glue), then
    /// refresh the list.
    pub async fn delete_feed(&self, feed_id: String) -> Result<(), FfiError> {
        self.inner
            .delete_feed(feed_id)
            .await
            .map_err(FfiError::from)
    }

    /// Re-query the **currently selected** feed source, whatever it is.
    ///
    /// Always prefer this to `select_feed(snapshot().selected_feed)` at a
    /// refresh/reconnect/remount site: `selected_feed` is `None` both for the
    /// local feed *and* while Trending is selected, so re-selecting it silently
    /// drops a Trending viewer into Local (`trending.md` § The Trending feed).
    /// Six apps each hit and hand-fixed that bug during the Trending rollout,
    /// every one of them re-deriving a branch shared Rust already owns — this
    /// face exists so they stop.
    pub async fn refresh_current_feed(&self) {
        self.inner.refresh_current_feed().await
    }

    /// The manager's `{"started": N, "completed": M, "committed_gen": G}` reload
    /// triple as a JSON string — the native read of
    /// `fauna_e2e_agent::FEED_RELOADS_KEY`
    /// (counting and JSON shape both live in shared Rust:
    /// `FeedManager::reload_counts` + `fauna_feed::feed_reloads_json`).
    /// Synchronous — three atomic reads, no I/O — so an app's e2e state provider
    /// may call it on the ack path (convention 11 corollary). An app with no
    /// manager built yet publishes the shared derivation's zeros itself rather
    /// than calling in; an app without the leg publishes nothing at all.
    pub fn feed_reloads_json(&self) -> String {
        fauna_feed::feed_reloads_json(Some(self.inner.reload_counts())).to_string()
    }

    /// The `data.feed.posts` state-dump array as a JSON string — the native
    /// read of the shared derivation (shape + `is_muted` threading both live
    /// in `fauna_feed::feed_posts_json`). Synchronous — a snapshot clone plus
    /// one `is_muted` lookup per post, no I/O — so an app's e2e state
    /// provider may call it on the ack path (convention 11 corollary), the
    /// `feed_reloads_json` sibling above.
    pub fn posts_json(&self) -> String {
        let snap = self.inner.snapshot();
        fauna_feed::feed_posts_json(&snap.posts, |post_id| self.inner.is_muted(post_id)).to_string()
    }

    /// Act on a post from the interaction bar — `feed-{like,reply,repost,quote}
    /// -button` (`feed.md` § Interaction bar) — over `fauna.posts.interact`,
    /// folding the nest's post-act counters into the loaded window so the tapped
    /// count moves at once.
    ///
    /// **Call this instead of `PostsClient::posts_interact` directly.** The raw
    /// client throws the reply away, which is precisely why a tapped ♥ never
    /// moved on six of seven apps; the counts on screen come from the snapshot,
    /// and only this seam writes them. `body` is the reply/quote text (`None`
    /// for like/repost).
    pub async fn interact(
        &self,
        post_id: String,
        action: String,
        body: Option<String>,
    ) -> Result<(), FfiError> {
        self.inner
            .interact(post_id, action, body)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Reply to a post (`feed-reply-button`) — composes a real post carrying
    /// `Reference::Reply`, which is the only thing that moves the target's
    /// `reply_count`.
    ///
    /// **Call this, never `interact(id, "reply", text)`.** That call looks
    /// identical and is not a reply: the nest's native arm discards `body`
    /// entirely, so the user's text was accepted and dropped. A bridged post
    /// still routes through interact internally — the manager decides, so the
    /// app leg does not have to know the source.
    pub async fn reply(&self, post_id: String, body: String) -> Result<(), FfiError> {
        self.inner
            .reply(post_id, body)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Quote-repost a post (`feed-quote-button`) — composes a post carrying
    /// `Reference::Quote`, which the shipped `quoted-post` embed renders on
    /// every app with no further work. `body` is the commentary and may be
    /// empty (§ Interaction bar's ratified direct quote-repost).
    pub async fn quote(&self, post_id: String, body: String) -> Result<(), FfiError> {
        self.inner
            .quote(post_id, body)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// The confirmed-public reply (`ui/feed.md` § Encryption at rest →
    /// *Ruling 5's build — the shape*, (e)): the reply dialog took the user's
    /// explicit answer under `feed-reply-public-confirm`, so the words go out
    /// as the public reference [`reply`](Self::reply) refuses under a
    /// restricted target. Call it only while that checkbox is checked and
    /// `PostSummary.reply_audience` is *public by confirmation*; a reply this
    /// device could seal is refused here, and a public target composes as
    /// `reply` would. Additive — `reply`'s signature is unchanged.
    pub async fn reply_public_confirmed(
        &self,
        post_id: String,
        body: String,
    ) -> Result<(), FfiError> {
        self.inner
            .reply_public_confirmed(post_id, body)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Repost / un-repost a post (`feed-repost-button`) — one verb, toggle
    /// semantics off the target row's `viewer_repost_id` (`feed.md`
    /// § Interaction bar → Repost, ratified 2026-08-10): absent → composes the
    /// caller's empty-body `Reference::Repost` post; present → un-reposts it
    /// through the interact door. No confirmation dialog — instantly
    /// reversible by the same toggle. A bridged post routes through interact
    /// verbatim; the manager decides, so the app leg does not have to know
    /// the source.
    pub async fn repost(&self, post_id: String) -> Result<(), FfiError> {
        self.inner
            .repost(post_id)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Like / un-like a post (`feed-like-button`) — one verb, toggle semantics
    /// off the target row's `viewer_liked` (`feed.md` § Interaction bar). Both
    /// directions ride the same interact door on the same post id and the
    /// nest's post-act counters are folded either way, so the count moves on a
    /// like AND on an un-like.
    ///
    /// **Call this, not `interact(id, "like", None)`.** That call is one-way:
    /// the nest's like arm is idempotent per (actor, post), so a second tap
    /// moves nothing and the user can never take a like back. A bridged post
    /// keeps the shipped one-way path internally — the manager decides, so the
    /// app leg does not have to know the source.
    pub async fn like(&self, post_id: String) -> Result<(), FfiError> {
        self.inner
            .like(post_id)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Delete an own post (`feed-post-delete-confirm-button`; confirmation is
    /// client glue): builds + signs a `Tombstone` over `fauna.posts.delete`,
    /// then drops it from the loaded window on success
    /// (`feed.md` § State & data shape → *Post deletion*).
    pub async fn delete_post(&self, post_id: String) -> Result<(), FfiError> {
        self.inner
            .delete_post(post_id)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Subscribe to a bridge feed (`bridge-form-subscribe-button`) over
    /// `fauna.bridges.feeds.create`; reflects the result into `BridgeFormState`
    /// and returns the row id.
    pub async fn subscribe_bridge(
        &self,
        kind: String,
        uri: String,
        name: String,
    ) -> Result<i64, FfiError> {
        self.inner
            .subscribe_bridge(kind, uri, name)
            .await
            .map_err(FfiError::from)
    }

    /// Unsubscribe from a bridge feed (`bridge-feed-unsubscribe-button`) over
    /// `fauna.bridges.feeds.delete`, then refresh the bridge-feed list.
    pub async fn unsubscribe_bridge(&self, id: i64) -> Result<(), FfiError> {
        self.inner
            .unsubscribe_bridge(id)
            .await
            .map_err(FfiError::from)
    }

    /// Project the embedded quoted-post card for `quoted_post_id` — from the
    /// loaded set with no fetch when possible, else a single `fauna.posts.get`.
    /// `None` when the quote can't be resolved.
    pub async fn resolve_quoted_post(&self, quoted_post_id: String) -> Option<QuotedPostView> {
        self.inner.resolve_quoted_post(quoted_post_id).await
    }

    /// Resolve the first media blob hash for a loaded `has_media` post and write
    /// it into the matching `PostSummary.media_hash`, then notify. A no-op unless
    /// a loaded post flags media and isn't resolved.
    pub async fn resolve_media(&self, post_id: String) {
        self.inner.resolve_media(post_id).await;
    }

    /// Resolve the buyer's price read for a sold post (`monetization.md` §
    /// Per-post pay-to-unlock → *the buyer's price read is post-addressed*)
    /// and fold it into the matching `PostSummary.unlock_offer`, then notify.
    /// A no-op unless `gated_tier` names a `post-unlock-*` tier and the offer
    /// isn't already resolved. The purchase itself is the existing
    /// `FfiSubscriptionsClient::subscribe` against the resolved `tier_name`,
    /// no new call.
    // `payments`-gated doc line — see `update_compose_sell` above.
    #[cfg_attr(
        feature = "payments",
        doc = " Drives `gated-post-price` / `gated-post-payment-link` / `gated-post-buy-button`."
    )]
    pub async fn resolve_post_unlock_offer(&self, post_id: String) {
        self.inner.resolve_post_unlock_offer(post_id).await;
    }

    /// Make the post `post_id` names renderable whether or not the feed query
    /// ever loaded it (`ui/search.md` § Where logic lives → *Result navigation
    /// (deep link)*) — the deep-link door a search hit needs, since it can
    /// name a post the timeline never scrolled to. Cheap and idempotent: a
    /// post already in [`fauna_feed::FeedSnapshot::posts`] or already parked
    /// in [`fauna_feed::FeedSnapshot::deep_linked_post`] costs no round trip.
    pub async fn resolve_post(&self, post_id: String) -> fauna_feed::PostResolution {
        self.inner.resolve_post(post_id).await
    }

    /// Buy a sold post via the self-serve teaser affordance — the existing
    /// subscribe flow against the resolved offer's `tier_name`, no new nest
    /// write. `Ok(Some(true))` =
    /// queued (pending author approval, the client-minted-tier norm),
    /// `Ok(Some(false))` = approved outright, `Ok(None)` = the post isn't
    /// loaded or its offer hasn't resolved yet (flattened from
    /// `Option<Result<_>>` — UniFFI can't lower that nesting directly).
    // `payments`-gated doc line — see `update_compose_sell` above.
    #[cfg_attr(
        feature = "payments",
        doc = " The affordance is `gated-post-buy-button`."
    )]
    pub async fn buy_unlock_offer(&self, post_id: String) -> Result<Option<bool>, FfiError> {
        match self.inner.buy_unlock_offer(post_id).await {
            Some(Ok(queued)) => Ok(Some(queued)),
            Some(Err(msg)) => Err(FfiError::General { msg }),
            None => Ok(None),
        }
    }

    /// Resolve a loaded gated post's sealed-blob hash (hex `encrypted_ref`) for
    /// the client to fetch (`GET /api/v1/blob/{hash}` — platform glue), caching
    /// the decoded gate info for [`unlock_gated_post`](Self::unlock_gated_post).
    /// The `resolve_media` pattern: one lazy `fauna.posts.get` + decode per
    /// post. `None` when the post isn't loaded, isn't gated, or can't be decoded.
    pub async fn gated_blob_hash(&self, post_id: String) -> Option<String> {
        self.inner.gated_blob_hash(post_id).await
    }

    /// Decrypt a gated post's full body from its fetched sealed blob (`blob_bytes`,
    /// fetched by the client after [`gated_blob_hash`](Self::gated_blob_hash)) and
    /// swap it into the snapshot (`body` + rebuilt `document`, `gated_unlocked`),
    /// then notify. The period key comes from custody when the local actor is the
    /// author, else from the reader's own wrap entry in the tier's live KeyBlob.
    /// A post sealed under a rotated-out period the KeyBlob no longer carries
    /// stays locked (best-effort backfill — the archival path is a follow-on).
    pub async fn unlock_gated_post(
        &self,
        post_id: String,
        blob_bytes: Vec<u8>,
    ) -> Result<(), FfiError> {
        self.inner
            .unlock_gated_post(post_id, blob_bytes)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Open a post-media blob the app fetched by hash, for rendering.
    ///
    /// **Every app's post-image path calls this**, gated post or not: a public
    /// post's blob is plaintext on the wire and comes straight back, while a
    /// gated post's attachment is AEAD-sealed under the same per-post key its
    /// body opened under and must be opened before the bytes are an image
    /// (`ui/media.md` § Encryption at rest — one per-post key seals body and
    /// attachments alike). Routing every hash through the one call is what keeps
    /// the seven post cards free of an is-this-post-gated branch.
    ///
    /// `None` means the blob IS a sealed item of an unlocked post and did not
    /// open — paint the placeholder, exactly as for bytes that fail to decode.
    /// The fetch stays platform glue, like [`unlock_gated_post`](Self::unlock_gated_post)'s.
    pub fn open_media_bytes(&self, blob_hash: String, fetched: Vec<u8>) -> Option<Vec<u8>> {
        self.inner.open_media_bytes(&blob_hash, fetched)
    }

    /// Whether this blob must be fetched and opened rather than linked.
    ///
    /// **Only apple needs this of the native apps.** windows and android hold the
    /// fetched bytes and route every hash through
    /// [`open_media_bytes`](Self::open_media_bytes) unconditionally — no branch, no
    /// predicate. But apple's post image is `AsyncImage(url:)`, which does the GET
    /// and the decode inside SwiftUI, so no Swift code ever sees the bytes: it must
    /// know before rendering whether this hash can stay a URL (the common case, and
    /// the only one the nest's `?thumb=1` smaller blob exists for) or has to become
    /// a fetched-and-opened `FaunaImage.decode`, as `DmMessageBubble` already does
    /// for sealed attachment bytes.
    ///
    /// `false` for any unregistered hash — public media, avatars, link-preview
    /// images, and a still-sealed post's item (whose card paints its placeholder
    /// until a detail-open unlock registers it).
    ///
    /// Ask in the view model, not the post card: the card takes a resolved image
    /// source either way.
    pub fn is_sealed_media(&self, blob_hash: String) -> bool {
        self.inner.is_sealed_media(&blob_hash)
    }

    /// What a tapped `video-thumbnail`'s block plays from (render-model.md § D6c →
    /// *Inline playback*): `Url` (nest-relative — prefix the nest origin and hand it
    /// to the platform's native player), `Sealed` (open the blob through
    /// [`open_media_bytes`](Self::open_media_bytes) and play the plaintext from a
    /// hardened temp file), or `Unplayable`. The player and its transient state are
    /// the app's; this is only the shared decision of what plays.
    pub async fn playback_source(
        &self,
        block: fauna_core::render::RenderBlock,
    ) -> fauna_feed::PlaybackSource {
        self.inner.playback_source(&block).await
    }

    /// Resolve the link-preview metadata for the bare `url` of a loaded post's
    /// `RenderBlock::LinkPreview` block (render-model.md § D4) via
    /// `fauna.linkpreview.resolve`, then notify so the next `snapshot()` projects
    /// the block `Resolved`/`Failed`. A no-op for an already-resolved URL.
    pub async fn resolve_link_preview(&self, url: String) {
        self.inner.resolve_link_preview(url).await;
    }

    /// Opt this post into loading its remote images (render-model.md § D3) — the
    /// `load-remote-content-button` on a feed card/detail dispatches here. Flips the
    /// manager-owned reveal set and re-emits; the next `snapshot()` projects
    /// `RemoteImage.revealed: true`. In-memory only (no persistence).
    pub fn reveal_remote_images(&self, post_id: String) {
        self.inner.reveal_remote_images(post_id);
    }

    // ── Engagement cues (engagement-cues.md §§ Cue vocabulary / At rest) ──────

    /// **Fetch-on-session-start** for the sealed `cues:v1` rollup — call once on
    /// entering the Feed page, before reporting observations. An absent rollup is
    /// a fresh capture; an unopenable one surfaces as an error (never a silent
    /// fresh rollup, which would erase other devices' cues on the next put).
    pub async fn hydrate_cues(&self) -> Result<(), FfiError> {
        self.inner
            .hydrate_cues()
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Report one per-exposure engagement observation when a card leaves the
    /// viewport (or its media ends). All *derivation* is shared Rust — the shell
    /// only supplies what a visibility/playback observer can honestly compute:
    /// the peak playback fraction (`media_played_pm`, per-mille; `None` for a
    /// non-media post) and the cumulative substantially-visible dwell at the two
    /// gate fractions. The manager derives the `watch-complete`/`skip` verdict,
    /// folds it into the rollup, and puts it on the debounce (`CUE_PUT_DEBOUNCE_S`
    /// or [`flush_cues`](Self::flush_cues)). `observed_at_ms` is the shell's event
    /// time — the manager reads no clock.
    pub async fn record_observation(
        &self,
        content_id: String,
        is_media: bool,
        media_played_pm: Option<u32>,
        dwell_ms_at_skip_visibility: u64,
        dwell_ms_at_long_visibility: u64,
        observed_at_ms: u64,
    ) -> Result<(), FfiError> {
        self.inner
            .record_observation(CueObservation {
                content_id,
                is_media,
                media_played_pm,
                dwell_ms_at_skip_visibility,
                dwell_ms_at_long_visibility,
                observed_at_ms,
            })
            .await
            .map(|_verdict| ())
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Force a put of any unsaved cues — the **background / app-close flush**. A
    /// no-op when nothing is dirty or before hydration.
    pub async fn flush_cues(&self) -> Result<(), FfiError> {
        self.inner
            .flush_cues()
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Delete the sealed `cues:v1` rollup — the user's own destruction of their
    /// revocable cue data (Personalization home). Drops the nest row and resets
    /// the live engine.
    pub async fn delete_cue_rollup(&self) -> Result<(), FfiError> {
        self.inner
            .delete_cue_rollup()
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    // ── Layer-B signal sharing (engagement-cues.md § Layer B) ────────────────

    /// **Session-start hydrate** of the signal-sharing opt-in — call once on
    /// entering the Feed page, beside [`hydrate_cues`](Self::hydrate_cues), so the
    /// producer respects the persisted opt-in before the user opens the
    /// Personalization page. Returns the cached opt-in (default off).
    pub async fn hydrate_signal_optin(&self) -> Result<bool, FfiError> {
        self.inner
            .hydrate_signal_optin()
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Score the loaded window's **public** posts with one trained factor, best
    /// first, bounded to the shared review cap
    /// (`fauna_client_personalization::publish::REVIEW_TOP_N`) — the publish
    /// review-prune sheet's corpus read (`topic-factors.md` § Publishing a
    /// trained factor).
    ///
    /// The corpus is deliberately the loaded window and nothing more (§
    /// Publishing's accepted limitation), which is why this belongs to the
    /// manager: it is the thing that *owns* the window. A shell that scores
    /// from a freshly-built manager gets an empty list, correctly — there is no
    /// window to score.
    ///
    /// `factor` is the `topic:<hex>` key, and may be **any** trained factor —
    /// not only one the current feed composes: the user publishes from the
    /// Personalization home, whose factor need not be the feed's.
    pub async fn score_corpus_for_factor(
        &self,
        factor: String,
    ) -> Result<Vec<ScoredExemplar>, FfiError> {
        // The review bound is product behavior, identical on every app, so
        // the boundary applies it itself rather than letting each shell pick
        // its own N (the `content_kind`-is-never-a-parameter pattern).
        self.inner
            .score_corpus_for_factor(&factor, fauna_client_personalization::publish::REVIEW_TOP_N)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Rebuild one trained factor's **publishable vocabulary** from its public,
    /// still-fetchable explicit examples — the Model half of the publish
    /// review-prune sheet's corpus read (`topic-factors.md` § Publishing a
    /// trained factor, v2).
    ///
    /// The List twin above scores the *loaded window*; this one walks the
    /// factor's own example markers and re-fetches each post, so what a shell
    /// reviews is a publish-time rebuild over public text and never a
    /// serialization of private model state. Every example that cannot be
    /// confirmed public is an **exclusion**, not a fallback — which is why the
    /// reply carries `included_examples` beside `marked_examples`: the drop is
    /// the publisher's to see, not the boundary's to hide.
    ///
    /// Unlike the List's review there is no top-N here, and none may be added:
    /// the vocabulary **is** the disclosure, so every survivor of the shared
    /// prune floor is listed (§ Publishing's vocabulary bound, which is size
    /// bound and review bound at once).
    ///
    /// `factor` is the `topic:<hex>` key, and may be **any** trained factor —
    /// the user publishes from the Personalization home, whose factor need not
    /// be the feed's.
    pub async fn scrub_corpus_for_factor(
        &self,
        factor: String,
    ) -> Result<TrainedModelReview, FfiError> {
        self.inner
            .scrub_corpus_for_factor(&factor)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Read the caller's signal-sharing opt-in state + the transparency export
    /// list — the Personalization home's "share signals" pane renders the toggle
    /// from `.share` and the published list from `.published` (reusing
    /// [`FfiReportShareStatus`], the identical shape the report-share pane uses;
    /// the export view is nest-wide, `report:*` and `signal:*` alike). Also caches
    /// `share` for the producer.
    pub async fn signal_share_status(&self) -> Result<FfiReportShareStatus, FfiError> {
        let reply = self
            .inner
            .signal_share_status()
            .await
            .map_err(|e| FfiError::General { msg: e })?;
        Ok(FfiReportShareStatus {
            share: reply.share,
            published: reply.published.into_iter().map(Into::into).collect(),
        })
    }

    /// Set the caller's signal-sharing opt-in, then return the re-read status
    /// (opting out withdraws this actor's `signal:*` rows, so the export list may
    /// shrink). Caches the nest-confirmed `share` for the producer — the toggle
    /// reflects `.share`, non-optimistically.
    pub async fn set_signal_sharing(&self, share: bool) -> Result<FfiReportShareStatus, FfiError> {
        let reply = self
            .inner
            .set_signal_sharing(share)
            .await
            .map_err(|e| FfiError::General { msg: e })?;
        Ok(FfiReportShareStatus {
            share: reply.share,
            published: reply.published.into_iter().map(Into::into).collect(),
        })
    }

    // ── Trained topic factors (topic-factors.md § Training signals) ──────────

    /// **More like this** / **less like this** on a post
    /// (`feed-post-more-like-this` / `feed-post-less-like-this` in the post-card
    /// overflow menu). Trains the user's sealed `topic:<hex>` model on the post's
    /// full text, re-seals it under their BackupKey, stores it nest-opaque, and
    /// re-ranks the loaded window immediately — the nest learns nothing.
    ///
    /// Re-tapping the same verb is a [`TrainResult::DuplicateSignal`] that writes
    /// nothing; tapping the other verb flips exactly (no double-count).
    pub async fn train_post(
        &self,
        post_id: String,
        factor: String,
        verb: TrainVerb,
    ) -> Result<TrainResult, FfiError> {
        self.inner
            .train_post(post_id, factor, verb)
            .await
            .map_err(FfiError::from)
    }

    /// Un-mark a post (tapping its active verb off): applies the exact inverse of
    /// the delta it trained and drops the marker. If the post's body is no longer
    /// fetchable the marker is still removed — its statistical trace stays, the
    /// documented undo limitation (§ Training signals).
    pub async fn untrain_post(&self, post_id: String, factor: String) -> Result<(), FfiError> {
        self.inner
            .untrain_post(post_id, factor)
            .await
            .map_err(FfiError::from)
    }

    /// The trained factor a gesture on this feed trains **in context** — the feed's
    /// single `topic:*` factor, if it has exactly one. `None` ⇒ the client opens
    /// `feed-post-train-target-sheet` rather than guessing.
    pub fn train_target_factor(&self) -> Option<String> {
        self.inner.train_target_factor()
    }

    /// This post's current toggle state for `factor` — what paints the
    /// more/less-like-this menu items as active. Survives restarts and reaches
    /// every device: the markers live inside the sealed model.
    pub fn example_label_for(&self, post_id: String, factor: String) -> Option<TrainVerb> {
        self.inner.example_label_for(&post_id, &factor)
    }

    /// Does this post match one of the user's muted words? Drives the
    /// collapse-to-placeholder render treatment — which applies **everywhere**,
    /// including chronological feeds where a mute cannot sink a post
    /// (topic-factors.md § Scoring).
    pub fn is_muted(&self, post_id: String) -> bool {
        self.inner.is_muted(&post_id)
    }
}

/// The room-post seam's door (`ui/feed.md` § Encryption at rest →
/// *Room-restricted — the app half*).
///
/// **Its own `#[uniffi::export]` block** for the reason the payments block
/// below spells out: it takes the cross-crate `ConversationsSession`, which
/// only the `conversations-session` feature compiles, so the whole block is
/// gated rather than the method.
#[cfg(feature = "conversations-session")]
#[uniffi::export]
impl FfiFeedManager {
    /// Install the conversations session as the feed's room-post key seam —
    /// call it where the feed manager and the session first coexist, and again
    /// after a re-auth hands over a fresh session (the last one wins). Without
    /// it every room-restricted post stays locked and no room is offered on
    /// `compose-gate-tier-select`, the honest state for a device with no
    /// conversations plane. Follow it with
    /// [`refresh_own_rooms`](FfiFeedManager::refresh_own_rooms).
    ///
    /// Installed WEAKLY (`ConversationsSession::weak_room_post_keys`): the feed
    /// manager is held by app glue this crate cannot see, and a strong install
    /// kept a departed session's receive loop alive for as long as an
    /// undisposed manager lingered.
    pub fn set_room_post_keys(&self, session: Arc<fauna_conversations::ConversationsSession>) {
        self.inner.set_room_post_keys(session.weak_room_post_keys());
    }
}

/// The tip surface's one gated member (`dynamic-features.md` § Charter members —
/// tips are a named buy-side gate surface), so a store-safe build exports no way
/// to reach `fauna.tips.list` and `PostSummary.tips` stays permanently null.
///
/// **Its own `#[uniffi::export]` block, and it has to be:** the export macro
/// emits scaffolding for every method it is handed *before* cfg-stripping runs,
/// so a `#[cfg]` on the method alone leaves the generated code calling a method
/// that no longer exists — the excised build then fails to compile, which is how
/// this was caught. Gating the whole block is what actually removes both halves.
#[cfg(feature = "payments")]
#[fauna_uniffi_async::export]
impl FfiFeedManager {
    /// Resolve this post's tip surface (`monetization.md` § Tips) and fold it
    /// into the matching `PostSummary.tips`, then notify — drives
    /// `post-tip-total` / `post-tip-count` / `post-tip-list-button`.
    ///
    /// Call it once per rendered post from the same pump that drives
    /// `resolve_media`, guarded on `tips == null`: unlike the unlock offer
    /// there is no data trigger, because nothing in the feed projection says
    /// whether a post has tips. The resolve is fire-once by construction — it
    /// writes a view on *every* outcome, including "no tips" and any error
    /// (a transport error included), so the guard closes and the pump settles.
    pub async fn resolve_post_tips(&self, post_id: String) {
        self.inner.resolve_post_tips(post_id).await;
    }
}

// ── Cue capture: the shared tracker ──────────────────────────────────────────

/// The shared engagement-cue capture tracker ([`fauna_feed::CueTracker`]) for
/// the non-Rust shells (`engagement-cues.md` § Cue vocabulary & derivation, the
/// boundary revised 2026-07-29).
///
/// A shell owns only its geometry probe, its tick scheduling, its lifecycle and
/// its emit glue: each tick it hands over one [`CueRow`] per row it could read
/// plus the viewport bounds and the two clocks, and passes every returned
/// [`CueObservation`] straight to
/// [`FfiFeedManager::record_observation`](FfiFeedManager::record_observation)
/// (the record's fields are exactly that call's arguments). It must **not**
/// bucket dwell, accumulate credit, decide leaves, or filter noise itself —
/// four apps each hand-writing that arithmetic is what the revision retired.
///
/// **Interior `Mutex`, not `&mut self`:** a UniFFI object is shared across the
/// binding's threads, and the tracker is stateful. Sampling is a few hundred
/// microseconds of map arithmetic at 4 Hz, so the lock is never contended in
/// practice; a poisoned lock cannot lose user data (the worst case is one
/// dropped exposure) so it is recovered rather than surfaced.
///
/// Lives in the `feed-manager`-gated module, so it is absent from the Go
/// mail-bridge `--no-default-features` build.
#[derive(uniffi::Object)]
pub struct FfiCueTracker {
    inner: std::sync::Mutex<CueTracker>,
}

#[uniffi::export]
impl FfiCueTracker {
    /// A tracker for a container with the given leave model — the one genuine
    /// platform divergence (see [`LeaveModel`]). Construct once per feed
    /// wire-up, alongside the observer that drives it.
    #[uniffi::constructor]
    pub fn new(leave_model: LeaveModel) -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::Mutex::new(CueTracker::new(leave_model)),
        })
    }

    /// One probe read. `rows` is every row the shell could read this tick (a row
    /// it measured as not-yet-arranged is included with a non-positive `height`;
    /// one it could not read at all is simply omitted — the tracker holds both).
    /// `window_post_ids` is every post in the loaded window, not just the
    /// realized rows. `mono_now_ms` MUST be a monotonic reading (Kotlin
    /// `SystemClock.elapsedRealtime`, Swift `DispatchTime`, C#
    /// `Environment.TickCount64`) — a wall clock here lets an NTP step or a date
    /// change inflate dwell, which is the drift this revision fixed.
    ///
    /// Returns the finished exposures, already past the single-sample noise
    /// floor.
    pub fn sample(
        &self,
        rows: Vec<CueRow>,
        window_post_ids: Vec<String>,
        viewport_start: f64,
        viewport_end: f64,
        mono_now_ms: u64,
        wall_now_ms: u64,
    ) -> Vec<CueObservation> {
        self.lock().sample(
            &rows,
            &window_post_ids,
            viewport_start,
            viewport_end,
            mono_now_ms,
            wall_now_ms,
        )
    }

    /// Everything tracked has left the viewport (the page navigated away, or the
    /// observer was cancelled) — drain, emit, and reset the credit baseline.
    pub fn drain_all(&self, wall_now_ms: u64) -> Vec<CueObservation> {
        self.lock().drain_all(wall_now_ms)
    }
}

impl FfiCueTracker {
    /// Recover a poisoned lock rather than surfacing it: a panic mid-sample can
    /// cost at most one exposure's bookkeeping, and refusing to capture cues for
    /// the rest of the session is strictly worse than resuming.
    fn lock(&self) -> std::sync::MutexGuard<'_, CueTracker> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The shared sampling cadence ([`fauna_core::scoring::cues::CUE_SAMPLE_INTERVAL_MS`])
/// — a shell reads its tick interval from here and never re-declares it, so the
/// tick rate and the dwell thresholds stay one calibration.
#[uniffi::export]
pub fn cue_sample_interval_ms() -> u64 {
    fauna_core::scoring::cues::CUE_SAMPLE_INTERVAL_MS
}

/// The i18n key to paint for a feed-manager error that is a stated refusal
/// (today only `feed.reference_restricted` — `ui/feed.md` § Encryption at rest,
/// ruling 6), `None` for every other error text; the app falls back to the raw
/// text. UniFFI face of [`fauna_feed::refusal_i18n_key`] — the verbs' errors
/// cross as their stable text, so this is the one place that recognizes one.
#[uniffi::export]
pub fn feed_refusal_i18n_key(err: String) -> Option<String> {
    fauna_feed::refusal_i18n_key(&err).map(str::to_string)
}

// ── E2E test-injection seam (test-helpers) ───────────────────────────────────
// The feed twin of `ConversationsManager`'s inject seam: replace the snapshot
// with a synthetic post list so a tier_2 cross-app test reaches the
// unverified-source-badge `Failed` arm — unreachable from a real nest, which
// serves only `Unchecked`/`Verified` (security.md § Client display of unverified
// content). Compiled only into the test-flavored native FFI build (windows-ffi /
// apple-ffi-test enable `test-helpers`); inert in production and absent from the
// Go bridge (doubly gated — `feed-manager` is off in the Go `--no-default-features`
// build, and `test-helpers` is never on there). The `set_feed_snapshot_for_test`
// it calls is itself `fauna-feed/test-helpers`-gated.
// ⚠ **`async_runtime = "tokio"` is load-bearing here, not boilerplate.**
// `set_cue_rollup_for_test` below is the one async member, and it does a REAL
// nest `PUT` rather than only touching local state. Without this attribute
// UniFFI drives the future on its own foreign executor with no Tokio reactor
// installed, so the call panicked `there is no reactor running, must be called
// from the context of a Tokio 1.x runtime` — on EVERY UniFFI consumer, not one
// app. It surfaced as both apple legs of `test_engagement_cues.py::
// test_engagement_toggle_and_clear_activity_data` refusing, but windows drives the identical face
// (`FfiFeedManager.SetCueRollupForTest`) and was red on the same outcome. The
// sync members are unaffected — the attribute only changes how async ones are
// driven — and the other blocks doing real nest I/O already carry it.
#[cfg(feature = "test-helpers")]
#[fauna_uniffi_async::export]
impl FfiFeedManager {
    /// Seed the feed post list for an e2e test. `specs_json` is the JSON array of
    /// [`fauna_feed::test_support::TestPostSpec`] the cross-app `feed_inject_posts`
    /// command carries — the **same** payload linux's `handle_feed_inject_posts`
    /// deserializes (one shared spec shape; the C# `FeedCommands.InjectPosts` forwards
    /// the raw `posts` array verbatim, parsing nothing). Replaces the snapshot with a
    /// `Loaded` list built via [`feed_snapshot_with_posts`](fauna_feed::test_support::feed_snapshot_with_posts)
    /// and notifies observers. A malformed payload is a logged no-op (lenient, matching
    /// linux) so a bad command never wedges the page.
    pub fn inject_posts_for_test(&self, specs_json: String) {
        match serde_json::from_str::<Vec<fauna_feed::test_support::TestPostSpec>>(&specs_json) {
            Ok(specs) => self.inner.set_feed_snapshot_for_test(
                fauna_feed::test_support::feed_snapshot_with_posts(specs),
            ),
            Err(e) => eprintln!("[fauna-ffi] inject_posts_for_test: bad specs_json: {e}"),
        }
    }

    /// Drive the feed page's `error-message` directly for an e2e test — the
    /// apple twin of `ConversationsManager::inject_page_error_for_test`
    /// (`conversations_inject_page_error` on web). `key`/`message` build a
    /// `LocalizedText::key_arg`, the same carrier a real failed fetch uses
    /// (`feed.error_load`), so every app resolves it through its own i18n
    /// pipeline exactly as it would a real failure.
    pub fn inject_error_for_test(&self, key: String, message: String) {
        self.inner
            .inject_error_for_test(fauna_core::localized::LocalizedText::key_arg(
                key, "message", message,
            ));
    }

    /// Seed the live engagement-cue engine with `content_ids` (each recorded a
    /// `WatchComplete` verdict) and PUT the sealed rollup to the nest for real —
    /// unlike its sibling seams, a real nest round trip so a capture-less client
    /// can reach "Clear activity data" with an actual `cues:v1` row to delete.
    /// See [`fauna_feed::FeedManager::set_cue_rollup_for_test`].
    pub async fn set_cue_rollup_for_test(&self, content_ids: Vec<String>) -> Result<(), FfiError> {
        self.inner
            .set_cue_rollup_for_test(content_ids)
            .await
            .map_err(|e| FfiError::General { msg: e })
    }

    /// Arm the one-shot hold on the NEXT reload (`feed_hold_next_reload`). See
    /// [`fauna_feed::FeedManager::hold_next_reload_for_test`].
    pub fn hold_next_reload_for_test(&self) {
        self.inner.hold_next_reload_for_test();
    }

    /// Release the held reload, or disarm a hold no reload reached yet
    /// (`feed_release_held_reload`). See
    /// [`fauna_feed::FeedManager::release_held_reload_for_test`].
    pub fn release_held_reload_for_test(&self) {
        self.inner.release_held_reload_for_test();
    }

    /// Whether a hold is armed and no reload has reached it yet — while it is,
    /// the app's e2e agent STARTS a feed op instead of awaiting it. See
    /// [`fauna_feed::FeedManager::reload_hold_armed_for_test`].
    pub fn reload_hold_armed_for_test(&self) -> bool {
        self.inner.reload_hold_armed_for_test()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;
    use fauna_feed::FeedStatus;

    /// The exported cadence IS the shared constant — same anti-drift argument as
    /// the gates above: a shell that re-declared 250 would silently decouple its
    /// tick rate from the dwell thresholds it is being measured against.
    #[test]
    fn cue_sample_interval_mirrors_the_shared_constant() {
        assert_eq!(
            cue_sample_interval_ms(),
            fauna_core::scoring::cues::CUE_SAMPLE_INTERVAL_MS
        );
    }

    /// The façade holds ONE tracker across calls — the whole point of the object.
    /// A face that built a fresh tracker per `sample` would accumulate no dwell
    /// and emit nothing, and every behavioural test lives in `fauna_feed`, so
    /// this is the wiring mistake nothing else would catch.
    #[test]
    fn the_tracker_face_keeps_its_state_across_samples() {
        let t = FfiCueTracker::new(LeaveModel::HoldUnmeasured);
        let row = |top: f64| CueRow {
            post_id: "p".to_string(),
            top,
            height: 1000.0,
            is_media: false,
            media_played_pm: None,
        };
        let win = vec!["p".to_string()];

        // Two fully-visible samples 250 ms apart, then measured out of view.
        assert!(
            t.sample(vec![row(0.0)], win.clone(), 0.0, 1000.0, 0, 100)
                .is_empty()
        );
        assert!(
            t.sample(vec![row(0.0)], win.clone(), 0.0, 1000.0, 250, 350)
                .is_empty()
        );
        let left = t.sample(vec![row(1000.0)], win, 0.0, 1000.0, 500, 600);

        assert_eq!(left.len(), 1, "a stateless face would emit nothing here");
        assert_eq!(left[0].content_id, "p");
        assert_eq!(left[0].dwell_ms_at_skip_visibility, 250);
        assert_eq!(
            left[0].observed_at_ms, 600,
            "the stamp is the WALL clock argument, not the monotonic one"
        );
    }

    /// `drain_all` reaches the same live tracker the samples fed.
    #[test]
    fn the_tracker_face_drains_what_it_tracked() {
        let t = FfiCueTracker::new(LeaveModel::AbsenceIsLeave);
        let row = CueRow {
            post_id: "p".to_string(),
            top: 0.0,
            height: 1000.0,
            is_media: true,
            media_played_pm: None,
        };
        let win = vec!["p".to_string()];
        t.sample(vec![row.clone()], win.clone(), 0.0, 1000.0, 0, 10);
        t.sample(vec![row], win, 0.0, 1000.0, 250, 260);

        let drained = t.drain_all(999);
        assert_eq!(drained.len(), 1);
        assert!(drained[0].is_media);
        assert_eq!(drained[0].observed_at_ms, 999);
        assert!(t.drain_all(999).is_empty(), "drained rows are forgotten");
    }

    /// A freshly-built façade exposes the manager's default snapshot (empty +
    /// `Loading`) — pins that `new` wraps `FeedManager::new` and `snapshot()`
    /// passes through. `NestClient::new` doesn't open a socket, so no nest.
    #[test]
    fn new_manager_exposes_default_snapshot() {
        let nest = NestClient::new(
            "wss://127.0.0.1:0/ws".into(),
            ActorKeypair::from_secret([3u8; 32]),
        );
        let mgr = FfiFeedManager::new(nest, [3u8; 32]);
        let snap = mgr.snapshot();
        assert_eq!(snap.status, FeedStatus::Loading);
        assert!(snap.posts.is_empty());
        assert!(snap.feeds.is_empty());
        assert!(snap.bridge_feeds.is_empty());
        assert_eq!(snap.selected_feed, None);
        assert!(!snap.trending_selected);
    }

    /// The room compose door stages the room answer and its teaser, and the
    /// teaser-alone door moves the teaser WITHOUT dropping that answer — the
    /// exact mistake an app writing the teaser through `update_compose_gate`
    /// makes (it re-reads the tier and clears the room).
    #[test]
    fn the_room_answer_survives_a_teaser_edit() {
        let nest = NestClient::new(
            "wss://127.0.0.1:0/ws".into(),
            ActorKeypair::from_secret([7u8; 32]),
        );
        let mgr = FfiFeedManager::new(nest, [7u8; 32]);
        mgr.update_compose_gate(Some("gold".into()), "old".into());
        mgr.update_compose_room(Some("ab".repeat(32)), "teaser".into());
        let compose = mgr.snapshot().compose;
        assert_eq!(compose.gate_room.as_deref(), Some("ab".repeat(32).as_str()));
        assert_eq!(compose.gate_tier, None, "a room answer clears the tier");
        assert_eq!(compose.gate_preview, "teaser");

        mgr.update_compose_preview("edited".into());
        let compose = mgr.snapshot().compose;
        assert_eq!(compose.gate_preview, "edited");
        assert_eq!(
            compose.gate_room.as_deref(),
            Some("ab".repeat(32).as_str()),
            "writing the teaser alone must keep the room answer"
        );
    }

    /// With nothing staged the sidecar is the tier class — byte-identical to
    /// the free `gated_post_sidecar()` it supersedes for a gated compose — so
    /// an app switching to the staged door changes nothing for a tier post.
    #[test]
    fn the_staged_sidecar_is_the_tier_class_with_no_room_post_staged() {
        let nest = NestClient::new(
            "wss://127.0.0.1:0/ws".into(),
            ActorKeypair::from_secret([8u8; 32]),
        );
        let mgr = FfiFeedManager::new(nest, [8u8; 32]);
        assert_eq!(
            mgr.gated_upload_sidecar(),
            crate::media_upload::gated_post_sidecar()
        );
        assert_ne!(
            fauna_media::sidecar::UploadSidecar::room_post().to_dag_cbor(),
            crate::media_upload::gated_post_sidecar(),
            "the room class must be a different sidecar, or the staged door proves nothing"
        );
    }

    /// The refusal key crosses for the manager's own refusal text and for
    /// nothing else — an app maps a real failure to its raw text, never to
    /// `feed.reference_restricted`.
    #[test]
    fn the_refusal_key_names_only_the_restricted_reference_refusal() {
        assert_eq!(
            feed_refusal_i18n_key(
                fauna_client_core::post::REFERENCE_REFUSED_RESTRICTED.to_string()
            )
            .as_deref(),
            Some("feed.reference_restricted")
        );
        assert_eq!(feed_refusal_i18n_key("network down".into()), None);
    }

    /// The hold's three faces drive the ONE manager hold: arming reads armed,
    /// releasing an un-reached hold disarms it.
    #[cfg(feature = "test-helpers")]
    #[test]
    fn the_reload_hold_arms_and_disarms_through_the_ffi_face() {
        let nest = NestClient::new(
            "wss://127.0.0.1:0/ws".into(),
            ActorKeypair::from_secret([9u8; 32]),
        );
        let mgr = FfiFeedManager::new(nest, [9u8; 32]);
        assert!(!mgr.reload_hold_armed_for_test());
        mgr.hold_next_reload_for_test();
        assert!(mgr.reload_hold_armed_for_test());
        mgr.release_held_reload_for_test();
        assert!(!mgr.reload_hold_armed_for_test());
    }

    /// `inject_posts_for_test` deserializes the cross-app `TestPostSpec` JSON
    /// the e2e `feed_inject_posts` command carries (the SAME payload linux's
    /// `handle_feed_inject_posts` parses) and replaces the snapshot via the shared
    /// `set_feed_snapshot_for_test` — the only way a tier_2 test reaches the
    /// `Failed`-verification badge arm (a real nest serves only Unchecked/Verified).
    /// Pins the JSON contract the windows C# `FeedCommands.InjectPosts` forwards.
    #[cfg(feature = "test-helpers")]
    #[test]
    fn inject_posts_for_test_seeds_snapshot_from_json() {
        use fauna_core::render::VerificationStatus;
        let nest = NestClient::new(
            "wss://127.0.0.1:0/ws".into(),
            ActorKeypair::from_secret([5u8; 32]),
        );
        let mgr = FfiFeedManager::new(nest, [5u8; 32]);
        // Focal verification arms + a pre-folded quoted embed (Slice 2b) — the
        // exact shape the cross-app `feed_inject_posts` command sends.
        let json = r#"[
            {"post_id":"aa","author":"11","body":"unverified","verification":"Failed"},
            {"post_id":"bb","author":"22","body":"ok","verification":"Verified"},
            {"post_id":"cc","author":"33","body":"quoting","verification":"Unchecked",
             "quoted":{"post_id":"dd","author":"44","body":"q","verification":"Failed"}}
        ]"#;
        mgr.inject_posts_for_test(json.to_string());
        let snap = mgr.snapshot();
        assert_eq!(snap.status, FeedStatus::Loaded);
        assert_eq!(snap.posts.len(), 3);
        assert_eq!(snap.posts[0].verification, VerificationStatus::Failed);
        assert_eq!(snap.posts[1].verification, VerificationStatus::Verified);
        assert_eq!(snap.posts[2].verification, VerificationStatus::Unchecked);
        // The quoted embed folds into post 2's document (a `QuotedPost` render block,
        // the source web/linux/windows paint the embed from) + sets `quoted_post_id`.
        assert_eq!(snap.posts[2].quoted_post_id.as_deref(), Some("dd"));
        use fauna_core::render::RenderBlock;
        assert!(
            snap.posts[2]
                .document
                .blocks
                .iter()
                .any(|b| matches!(b, RenderBlock::QuotedPost { .. })),
            "the quoting post's document must carry a folded QuotedPost block",
        );
    }

    /// A malformed `specs_json` leaves the snapshot untouched (lenient parse —
    /// matches linux's `handle_feed_inject_posts`, which logs + returns).
    #[cfg(feature = "test-helpers")]
    #[test]
    fn inject_posts_for_test_ignores_malformed_json() {
        let nest = NestClient::new(
            "wss://127.0.0.1:0/ws".into(),
            ActorKeypair::from_secret([6u8; 32]),
        );
        let mgr = FfiFeedManager::new(nest, [6u8; 32]);
        mgr.inject_posts_for_test("not json".to_string());
        assert!(mgr.snapshot().posts.is_empty());
    }
}
