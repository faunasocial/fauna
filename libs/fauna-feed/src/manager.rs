//! [`FeedManager`] — the shared, stateful feed snapshot + its mutating logic
//! (`docs/goal/ui/feed.md` § State & data shape, ratified 2026-06-14). The
//! direct analogue of `fauna_conversations::ConversationsManager`: it owns the
//! [`FeedSnapshot`], exposes `snapshot()` + `add_observer()` reactivity, and
//! `notify()`s after every state mutation; the view re-reads via `snapshot()`.
//! No client open-codes post-list state (priorities #1/#2/#3).
//!
//! It is **generic over the WS-RPC transport** (`R: RpcRequester + Clone`),
//! exactly like the transport clients it wraps (`FeedClient<R>` /
//! `PostsClient<R>` / `BridgesClient<R>`): the Rust-native Linux app uses
//! `FeedManager<Arc<NestClient>>` directly (no FFI hop); the wasm SPA uses
//! `FeedManager<WsRpcClient>`; the UniFFI apps (windows/macos/ios/android)
//! reach a concrete `FfiFeedManager` façade in `libs/fauna-ffi` that wraps
//! `FeedManager<Arc<NestClient>>` — the same pattern `FfiFeedClient` already
//! uses (a generic type can't be `#[uniffi::export]`ed, so the export lives on
//! the concrete façade, not here).
//!
//! ## Where the read model lives
//!
//! Selection / filter / sort / FTS / pagination is the **nest's** job
//! (`query_feed`); the manager only *assembles* the snapshot client-side
//! (`feed.md` § The read model): map each `FeedPostItem` → [`PostSummary`]
//! (micros→millis, `classify_sources` badges, `quoted_post_id` embed), append
//! each page **deduplicating by `post_id`**, **preserve the nest-supplied
//! order** (never re-sort — § Anti-patterns), and track `has_more` from the
//! returned cursor.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use fauna_client_bridges::BridgesClient;
use fauna_client_core::post::{
    ReferenceKind, ReferencedAudience, build_post, build_post_with_media, build_referencing_post,
    decode_post, decode_post_fetched_as, wire_post_id,
};
use fauna_client_feed::FeedClient;
use fauna_client_feed::encode_filter_rule;
use fauna_client_feed::feed::{FeedCompositionEntry, FeedPostItem};
use fauna_client_linkpreview::LinkPreviewClient;
use fauna_client_posts::PostsClient;
use fauna_core::data::{PostId, Timestamp, Tombstone};
use fauna_core::encoding::sign_and_pack;
use fauna_core::identity::ActorKeypair;
use fauna_core::localized::LocalizedText;
use fauna_core::render::{
    AuthoringOriginStatus, PreviewState, RenderBlock, VerificationStatus,
    resolve_link_preview_cached,
};
use fauna_protocol::RpcRequester;
use fauna_protocol::linkpreview::LinkPreviewResolveReply;

use fauna_client_labelers::LabelersClient;
use fauna_client_moderation::ModerationClient;
use fauna_client_moderation::moderation::ModerationSignalShareStatusReply;
use fauna_client_personalization::{
    PersonalizationClient, model_seal_keys, seal_topic_model, unseal_topic_model,
};
use fauna_core::crypto::BackupKey;
use fauna_core::scoring::cues::CUE_PUT_DEBOUNCE_S;
use fauna_core::scoring::validate_text_model_artifact;
use fauna_core::scoring::{CUES_ROLLUP_FACTOR_V1, FilterRule};
use fauna_text_model::publish::{PublishedTextModel, scrub_corpus};
use fauna_text_model::topic::{ExampleLabel, TopicModel, TrainOutcome};

use crate::compose::{
    AttachedFile, ComposeAttachmentUpload, ComposeUploadPart, FactorWeightInput, FeedComposeState,
    FilterRuleInput, SellComposeState,
};
use crate::cues::{
    CueEngine, CueObservation, CueRollup, CueVerdict, seal_cue_rollup, unseal_cue_rollup,
};
use crate::observer::FeedSnapshotObserver;
use crate::personalization::{
    ReviewNgram, ScoredExemplar, SealedEntry, SealedScorer, SealedScorers, TrainResult, TrainVerb,
    TrainedModelReview, model_text,
};
use crate::quote;
use crate::sealed_compose::{adjusted_score_micro, rerank_loaded_window};
use crate::snapshot::{
    AvailableBridge, BridgeFeedView, FeedSnapshot, FeedStatus, FeedSummaryView, PlaybackSource,
    PostResolution, PostSummary, QuotedPostView, ReplyAudience, UnlockOfferView,
};
// The tip projection's own types — reached only by `resolve_post_tips`, which
// the `payments` feature gates (`dynamic-features.md` § Charter members). The
// types themselves stay ungated on `PostSummary`, inert in an excised build.
#[cfg(feature = "payments")]
use crate::snapshot::{TipSenderView, TipView};

/// The number of posts requested per page. The nest clamps its own bounds; this
/// is the manager's page size for the initial load and each `load_more`.
const PAGE_LIMIT: i64 = 50;

/// The weight the muted-keyword penalty composes at when no stored composition
/// names it — i.e. essentially always, since a user mutes *words*, not
/// composition entries. `1000` = 1.0× (per-mille), so the scorer's −1000
/// per-mille verdict lands at its full documented strength.
///
/// Why implicit at all: the frame requires a muted keyword to apply globally
/// ("a global muted keyword mutes it everywhere"), and the factor is sealed —
/// its nest-side term is 0 no matter what, so a nest-side global-factor row
/// would buy nothing while telling the nest that this user mutes something. A
/// mute that depends on a row having been written is a mute that can silently be
/// missing; nothing to write means nothing to fail. See [`crate::personalization`].
const MUTED_KEYWORDS_IMPLICIT_WEIGHT: i64 = 1000;

/// One fetched page: the mapped posts plus whichever cursor the query's ordering
/// mode advances on. Exactly one of the two is `Some` for a non-final page —
/// chronological queries advance `next_cursor`, score-ordered ones advance the
/// `next_score_cursor` keyset pair.
struct FetchedPage {
    items: Vec<PostSummary>,
    next_cursor: Option<i64>,
    next_score_cursor: Option<(i64, i64)>,
}

impl FetchedPage {
    /// Whether a further page may exist — i.e. the nest handed back a cursor to
    /// resume from, in whichever ordering mode this query runs.
    fn has_more(&self) -> bool {
        self.next_cursor.is_some() || self.next_score_cursor.is_some()
    }
}

/// Internal pagination state — the cursor for the *next* page, kept off the
/// snapshot (the snapshot surfaces only `has_more`).
///
/// Two ordering modes, decided per query from the selected feed's **effective
/// composition** (its own composition plus the user's global factor set):
///
/// * **Chronological** (`scored == false`) — the local feed, and any feed with
///   no composition at all. Paginates on [`cursor`](Self::cursor).
/// * **Score** (`scored == true`) — a feed that composes at least one factor.
///   Paginates on the [`score_cursor`](Self::score_cursor) keyset pair, and its
///   `FeedPostItem.score` is the base key [`crate::sealed_compose`] adjusts.
///
/// A feed with **no** nest-side composition is deliberately *not* promoted to
/// score order just because the user has sealed scorers (muted keywords) — that
/// would silently re-sort a chronological timeline by engagement. See
/// [`crate::personalization`] (§ Where a mute sinks, and where it only collapses).
#[derive(Default)]
struct PageState {
    /// Chronological cursor (epoch micros) for the next page; `None` at the
    /// start of a fresh query and once the stream is exhausted.
    cursor: Option<i64>,
    /// Score-order keyset cursor `(key micro-units, created_at micros)` — both
    /// halves of the boundary row, per `feed.md` § The read model. A key alone
    /// is not a position in the compound `key DESC, created_at DESC` sort: it
    /// skips rows tied on the key, and never advances at all when the key is
    /// flat (which it is for a composition of only sealed factors — every
    /// nest-side term is 0).
    score_cursor: Option<(i64, i64)>,
    /// Whether the current query is score-ordered.
    scored: bool,
    /// Guards against a concurrent / re-entrant `load_more`.
    in_flight: bool,
}

/// Which feed the manager is currently querying — the doc's "variant of the
/// selection" (`trending.md` § The Trending feed), derived from the two
/// snapshot fields (`selected_feed` + `trending_selected`) so the sum-type
/// lives at the *dispatch* layer while the wire-/binding-facing snapshot stays
/// additive. `Local` and `Custom` mirror the pre-existing `Option<String>`
/// convention (`None` = local); `Trending` is the new virtual read.
///
/// `PartialEq`/`Clone` because `reload` compares the query it is about to run
/// against the one the posts ON SCREEN were loaded for — see `loaded_query`.
#[derive(Clone, PartialEq)]
enum Selection {
    /// The nest's local feed (`fauna.feed.local.posts`) — always chronological.
    Local,
    /// The built-in Trending virtual feed (`fauna.feed.trending.posts`) — always
    /// score-ordered over public posts, no feed row.
    Trending,
    /// A user-defined feed (`fauna.feed.posts`), keyed by hex feed id.
    Custom(String),
}

/// The shared feed manager. Construct over an authed transport + the local
/// actor's signing secret (needed to build + sign posts on `submit_post`).
pub struct FeedManager<R> {
    nest: R,
    /// The local actor's Ed25519 secret seed (`submit_post` builds a keypair
    /// from it). Held for the manager's lifetime, exactly as the linux app
    /// holds its `secret_hex`.
    actor_secret: [u8; 32],
    state: RwLock<FeedSnapshot>,
    page: RwLock<PageState>,
    /// Reload generation — bumped at the start of every [`reload`](Self::reload).
    /// Each reload (and each `load_more`) commits its fetched page only if the
    /// generation is still its own; a result whose generation was superseded is
    /// DROPPED. Without this, overlapping reloads race last-write-wins on
    /// `state.posts`: a slow stale fetch (a `clear_search` fired just before a
    /// debounced `set_search_query`, or feed A selected just before feed B) lands
    /// AFTER the newer one and clobbers it — the feed then shows feed-A/unfiltered
    /// posts under feed-B/the-committed-search (the
    /// `test_feed_search_filters_posts` apple red, 2026-07-17: committed
    /// `search_query` + unfiltered posts).
    reload_gen: std::sync::atomic::AtomicU64,
    /// Count of [`reload`](Self::reload)s that **committed** their result —
    /// bumped once per reload, on the Ok *and* the Err arm alike, after every
    /// state write of that reload and before its observer notify; a superseded
    /// reload (dropped by the `reload_gen` guard) never counts. The Err arm
    /// counts on purpose: the pair proves the mechanism *ran*; whether it
    /// worked is the consumer's own positive assert.
    ///
    /// ⚠ This is a **diagnostic**, never the barrier's release condition. It
    /// was one for a month and the arithmetic is wrong: because a superseded
    /// reload never commits, every supersede widens `started - completed`
    /// **permanently**, so `completed > started-at-baseline` is unsatisfiable
    /// on any manager that ever dropped a stale reload — however healthy. Web
    /// measured it on 2026-08-23: a reconnect fires two
    /// overlapping reloads by construction, the older is superseded, the newer
    /// commits and RENDERS ITS POSTS — and the barrier still reported "no
    /// re-query ever committed" for the full 300 s budget. What the pair is
    /// still good for is separating the two failure classes at a timeout:
    /// `started` stuck at baseline = the mechanism is gone; `started` past it =
    /// the mechanism ran.
    reload_commits: std::sync::atomic::AtomicU64,
    /// The **generation of the most recently committed reload** — the barrier's
    /// actual release condition, and the third member of the
    /// `{started, completed, committed_gen}` triple behind
    /// `fauna_e2e_agent::FEED_RELOADS_KEY`.
    ///
    /// Written at the same statement as `reload_commits`, carrying that
    /// reload's own `generation`. Generations are claimed by `fetch_add` at
    /// [`reload`](Self::reload)'s first statement, so
    /// `committed_gen > started-at-baseline` says precisely **"a re-query that
    /// BEGAN after the baseline read has landed its verdict"** — the causal
    /// anchor the reconnect re-hydrate e2e uses instead of asserting that no
    /// other delivery path exists (convention 14,
    /// `e2e-latency-independent-assertions.md`). Unlike a commit *count* it
    /// cannot be knocked off by a supersede: a dropped reload contributes
    /// nothing to either side of the comparison.
    reload_committed_gen: std::sync::atomic::AtomicU64,
    /// The query — `(selection, search_query)` — that the posts currently in
    /// `state.posts` were fetched for, set at each committed `reload`.
    ///
    /// This is what lets `reload` tell a **selection change** from a **refresh
    /// of what is already on screen**. The first must clear the list up front
    /// (feed A's posts under feed B's header is a lie); the second must NOT —
    /// the list stays valid until the new page lands, and blanking it strands
    /// the reader on an empty feed for as long as the fetch is in flight, with
    /// `status = Loading` and no error to explain it. `None` before the first
    /// commit, so the very first load still clears (nothing to preserve).
    loaded_query: RwLock<Option<(Selection, Option<String>)>>,
    /// The test-only one-shot reload hold — see
    /// [`hold_next_reload_for_test`](Self::hold_next_reload_for_test).
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    reload_hold: ReloadHold,
    observers: RwLock<Vec<Arc<dyn FeedSnapshotObserver>>>,
    /// Resolved quoted-post views, keyed by the **quoted** post's hex id — the
    /// store that lets [`resolve_media`](Self::resolve_media)'s document rebuild
    /// re-include a quote resolved by an earlier
    /// [`resolve_quoted_post`](Self::resolve_quoted_post) (and vice versa), so the
    /// embed-fold (render-model.md § D6) survives whichever embed resolves last.
    /// Identity-stable (a quoted post's content doesn't depend on the viewing
    /// feed), so it is *not* cleared on `reload`. Internal only — the render
    /// surface is `PostSummary.document`, never a parallel sibling field.
    resolved_quotes: RwLock<HashMap<String, QuotedPostView>>,
    /// Per-post remote-image reveal set (render-model.md § D3) — the feed twin of
    /// `ConversationsManager`'s set, keyed by the post's hex id. The single source
    /// of truth for "the user opted into loading this post's remote images";
    /// [`snapshot`](Self::snapshot) projects it onto each `PostSummary.document`
    /// (`RemoteImage.revealed`), so no client keeps a per-card reveal flag.
    /// **In-memory only** — the no-persistence posture (html-mail.md § Rendering)
    /// is unchanged; a reveal does not survive a restart.
    revealed_remote: RwLock<HashSet<String>>,
    /// Post ids whose unlock tier this actor has already bought
    /// ([`buy_unlock_offer`](Self::buy_unlock_offer) succeeded), so
    /// [`resolve_post_unlock_offer`](Self::resolve_post_unlock_offer) stops
    /// re-offering them.
    ///
    /// **Why the client has to remember this at all.** The nest read is
    /// *post*-addressed and deliberately carries **no caller axis** — it answers
    /// iff one of the author's tiers genuinely sells the post
    /// (`behavior/monetization.md` § Per-post pay-to-unlock → *The buyer's price
    /// read is post-addressed*), and § *No rotation* is why it must keep
    /// answering even for a buyer whose entitlement lapsed or whose request the
    /// author rejected. So the nest cannot suppress the re-offer without
    /// breaking two ratified properties, and "this actor already bought it" is
    /// knowledge only the buyer's own client holds. A buy leaves the actor
    /// `Queued`, not subscribed, so nothing in the post's own snapshot records
    /// it either.
    ///
    /// Actor-scoped by construction — a [`FeedManager`] is built per actor
    /// (`new(nest, actor_secret)`), so a different actor's manager starts empty
    /// and still sees the offer. Deliberately **not** cleared by
    /// [`reload`](Self::reload): re-offering a bought post after a refresh is
    /// precisely the defect. In-memory only, like
    /// [`revealed_remote`](Self::revealed_remote) — a restart re-offers, and
    /// that is correct, since the author may by then have approved (the offer
    /// resolves against live tier state).
    unlock_purchase_requested: RwLock<HashSet<String>>,
    /// Resolved link-preview metadata (render-model.md § D4), keyed by the bare
    /// URL of a `RenderBlock::LinkPreview` block — the store
    /// [`snapshot`](Self::snapshot) projects onto each post's document so a
    /// `Resolving` block flips to its terminal `Resolved`/`Failed`, the exact twin
    /// of the [`revealed_remote`](Self::revealed_remote) D3 projection. Only ever
    /// holds terminal states (`Resolved`/`Failed`); a URL absent here stays
    /// `Resolving`. Identity-stable (a URL's preview doesn't depend on the viewing
    /// feed), so it is *not* cleared on `reload`. Internal only — the render
    /// surface is `PostSummary.document`, never a parallel sibling field.
    /// **In-memory only** (the no-persistence posture is unchanged).
    resolved_previews: RwLock<HashMap<String, PreviewState>>,
    /// `post_id` → the nest-served composed ordering key (`FeedPostItem.score`,
    /// micro-units) — the base [`crate::sealed_compose`] adds sealed
    /// contributions to.
    ///
    /// **Manager-internal on purpose.** The obvious alternative — a `score`
    /// field on [`PostSummary`] — is wrong twice over: `PostSummary` is a
    /// `uniffi::Record` every app's bindings mirror, so the field would churn
    /// four language bindings (and break Kotlin's named-arg constructors) to
    /// carry a number **no shell ever renders**; and `feed.md` § Where logic
    /// lives requires the *snapshot* to keep supplying the visible order, which
    /// means the adjustment belongs to the manager, not to the render surface.
    /// So the key rides here, exactly like `resolved_quotes` / `revealed_remote`
    /// / `resolved_previews` (`topic-factors.md` § Implementation status today).
    /// Cleared on `reload` — a fresh query re-keys the window.
    base_scores: RwLock<HashMap<String, i64>>,
    /// The sealed half of the current feed's effective composition: trained
    /// `topic:*` models + the muted-keyword penalty, loaded post-decrypt
    /// (`topic-factors.md` § Scoring). Refreshed per `reload`; a train swaps its
    /// model in place. Empty ⇒ the seam is inert.
    sealed: RwLock<SealedScorers>,
    /// The current feed's effective composition (own + global), as resolved at
    /// the last `reload`. Drives the train-in-context target and the sealed
    /// reload; kept so `load_more` need not re-resolve it mid-scroll.
    composition: RwLock<Vec<FeedCompositionEntry>>,
    /// The signed gated post staged by [`prepare_gated_blob`](Self::prepare_gated_blob),
    /// awaiting its sealed-blob upload (platform glue — the bulk-binary HTTP
    /// carve-out) before [`submit_gated_post`](Self::submit_gated_post) creates it.
    pending_gated: RwLock<Option<PendingGatedPost>>,
    /// The per-post `seal_id` minted by
    /// [`seal_compose_attachment`](Self::seal_compose_attachment) for an
    /// audience-restricted compose, waiting for
    /// [`prepare_gated_blob`](Self::prepare_gated_blob) to seal the body under
    /// the **same** id — that is what makes one key open the body and its
    /// attachments (`ui/media.md` § Encryption at rest). Carries the tier it
    /// was minted against so a composer that changed its gate between the two
    /// calls is refused rather than sealing the two halves under different
    /// period keys.
    pending_seal: RwLock<Option<PendingComposeSeal>>,
    /// The auto-minted unlock tier staged by
    /// [`stage_sell_tier`](Self::stage_sell_tier), waiting for
    /// [`prepare_sell_post`](Self::prepare_sell_post) to build its post and
    /// commit it. Its whole reason to exist is that a sold post's attachment
    /// must seal under a tier that does not exist yet when the author picks the
    /// file: staging mints and *persists* the period key, so
    /// [`seal_compose_attachment`](Self::seal_compose_attachment) has something
    /// to reach. Dropped by any `update_compose_sell` / `update_compose_gate`,
    /// for the same reason `pending_seal` is.
    pending_sell_tier: RwLock<Option<PendingSellTier>>,
    /// Gated-post metadata resolved by
    /// [`gated_blob_hash`](Self::gated_blob_hash) (a `fauna.posts.get` decode,
    /// the `resolve_media` pattern), keyed by hex post id — what
    /// [`unlock_gated_post`](Self::unlock_gated_post) decrypts against.
    resolved_gated: RwLock<HashMap<String, ResolvedGated>>,
    /// Decrypted full bodies of gated posts this reader has already unsealed
    /// (`post_id` → the opened [`PostBody`](fauna_core::data::PostBody)). A feed `reload` / `load_more` replaces the
    /// post list with freshly-fetched **sealed** posts (`reload`: `s.posts =
    /// fresh`), which would otherwise revert an entitled reader to the teaser
    /// after they unlocked — the reload-revert that stranded the iOS subscriber
    /// leg of `test_gated_post_compose` on the teaser (the unlock landed, a
    /// following reload overwrote it). [`reapply_unlocked`](Self::reapply_unlocked)
    /// re-folds these onto every rebuilt list, the `resolved_gated`/`resolved_quotes`
    /// resolve-once-then-re-fold pattern. In-memory only (no persistence — the
    /// plaintext already rests in `s.posts` for the open reader; feed.md §
    /// Encryption at rest).
    ///
    /// The whole body, not just its text: a gated post's **media items** live
    /// only here — the public preview body carries none (`post.rs`'s
    /// `PostBody::Text { content: preview }`), so the re-fold has no other
    /// source for them. Holding a `String` is what made
    /// [`reapply_unlocked`](Self::reapply_unlocked) structurally unable to
    /// restore a reader's photos across a reload even once the unlock folded
    /// them.
    unlocked_bodies: RwLock<HashMap<String, fauna_core::data::PostBody>>,
    /// The verdicts a community room's named labelers derived for a
    /// room-restricted post this reader has unsealed, keyed by post id
    /// (`ui/feed.md` § Encryption at rest → *Room-restricted — the ruling*,
    /// ruling 7). Served by `fauna.posts.room_labels` to a live floor member
    /// at unlock time, merged into the card's own `labels` by the moderation
    /// queue's superset rule, and re-folded across a reload exactly like
    /// [`unlocked_bodies`](Self::unlocked_bodies) — a fresh page always
    /// arrives with the envelope's labels alone.
    room_post_labels: RwLock<HashMap<String, Vec<fauna_core::content_category::ContentLabelEntry>>>,
    /// The per-post seal key for every media blob of a gated post this reader
    /// has unsealed, keyed by the blob's 64-hex content hash.
    ///
    /// A gated post's attachments are sealed under **the same** per-post key as
    /// its body — `derive_post_key(period_key, seal_id)`, one key with random
    /// nonces per blob — so "recipients who can decrypt the body can decrypt the
    /// attachments by construction" (`docs/goal/ui/media.md` § Encryption at rest,
    /// Restricted-post-attached media). This map is that construction made
    /// usable by a renderer: [`unlock_gated_post`](Self::unlock_gated_post)
    /// registers each item (and its thumbnail) as it opens the body, and
    /// [`open_media_bytes`](Self::open_media_bytes) opens the bytes an app
    /// fetched by hash.
    ///
    /// Keyed by blob hash rather than by post id because that is all a renderer
    /// holds: the document carries `RenderBlock::Image { hash, .. }`, and each
    /// app's existing by-hash blob loader is the fetch. Same shape as
    /// conversations' `attachment_bytes` cache, which is likewise keyed by the
    /// handle the rendered bubble has.
    ///
    /// In-memory only, and zeroized on drop — this is key material, unlike the
    /// plaintext beside it.
    sealed_media_keys: RwLock<HashMap<String, zeroize::Zeroizing<[u8; 32]>>>,
    /// The live engagement-cue capture state (engagement-cues.md § At rest):
    /// the fetched `CueRollup` plus its put-debounce bookkeeping. Fed by
    /// [`record_observation`](Self::record_observation), sealed under the
    /// `cues:v1` key for cross-device continuity. Not a composition factor —
    /// `is_topic_factor` rejects `cues:`, so it never touches the sealed scorers.
    cue: RwLock<CueState>,
    /// The composed `topic:*` factors whose registry meta has
    /// `learn_from_engagement = on` (`topic-factors.md` § Training signals v2) —
    /// the ones a cue-verdict transition weakly trains. Resolved **once per
    /// `reload`** from the sealed registry (batched, alongside the muted-keyword
    /// config read in [`load_sealed_scorers`](Self::load_sealed_scorers)), never
    /// per observation. Empty in the default-off common case, which makes the
    /// [`record_observation`](Self::record_observation) training hook a cheap
    /// no-op.
    engagement_factors: RwLock<Vec<String>>,
    /// The cached Layer-B signal-sharing opt-in (`spam_preferences.share_signals`,
    /// default off; engagement-cues.md § Layer B). The
    /// [`record_observation`](Self::record_observation) producer reads it to gate a
    /// `signal_contribute` without a per-verdict fetch. Refreshed by
    /// [`hydrate_signal_optin`](Self::hydrate_signal_optin) at session start and by
    /// [`set_signal_sharing`](Self::set_signal_sharing) /
    /// [`signal_share_status`](Self::signal_share_status) from the nest-confirmed
    /// reply — never optimistically, so the producer never contributes on an
    /// opt-in the nest didn't persist.
    share_signals: AtomicBool,
    /// The local index's ear at the posts **trickle chokepoint** — this manager
    /// owns the two flows that create posts (`submit_post`,
    /// `submit_gated_post`), which is what makes it the hook site: `PostsClient`
    /// itself is a stateless per-call mint (`content-index.md` § Ingest
    /// triggers, v1 — the posts ruling's BUILT note). Set by glue on the seats
    /// that build (`NestMailIndexLauncher::own_post_observer`); never set on
    /// wasm — web is a querier by ratified design.
    post_index_observer: RwLock<Option<Arc<dyn fauna_client_search::OwnPostIndexObserver>>>,
    /// Where a **room-restricted** post's base key comes from
    /// (`ui/feed.md` § Encryption at rest → *Room-restricted*): the room's keys
    /// live in the conversations plane — an end-to-end room's MLS group, a
    /// community member's generation wraps — which this manager does not hold,
    /// so glue installs that plane's answer here
    /// ([`set_room_post_keys`](Self::set_room_post_keys)). Unset ⇒ every room
    /// post stays locked, the honest state for a seat that could not open one
    /// anyway.
    room_post_keys: RwLock<Option<Arc<dyn fauna_core::room_post::RoomPostKeys>>>,
    /// The author's period-key custody (`fauna.state.subscriptions`, through
    /// the account runtime's handle) — what the gated compose seals under,
    /// what the author's own unlock tries, and where the "sell this post"
    /// flow records its tier's key. Glue installs it
    /// ([`set_period_key_store`](Self::set_period_key_store)); unset, every
    /// custody read fails as unreadable — never as "no key held".
    period_keys: RwLock<Option<fauna_client_subscriptions::SharedPeriodKeyStore>>,
    /// The preference cluster's read seam (the account runtime's store) —
    /// where the feed reads the muted words its scorer sinks and the trained
    /// factors that opt into engagement training. Glue installs it
    /// ([`set_preference_store`](Self::set_preference_store)); unset, the read
    /// fails and the feed says its filters are not applied.
    preferences: RwLock<Option<fauna_client_config::SharedPreferenceStore>>,
}

/// The FeedManager's live engagement-cue capture state (engagement-cues.md
/// § At rest). Held under one lock so `record_observation` can fold a verdict,
/// consult the debounce, and stage a put atomically — then release the lock and
/// seal + `model_put` *outside* it (never a lock held across an await).
///
/// `Default` is the fresh, un-hydrated engine (`CueEngine::empty()`, not dirty,
/// no put anchor) — the state a manager starts in before `hydrate_cues`.
#[derive(Default)]
struct CueState {
    /// The live engine over the fetched-then-accumulating [`CueRollup`].
    engine: CueEngine,
    /// Whether [`FeedManager::hydrate_cues`] has fetched the nest's rollup. A
    /// put before hydration would overwrite another device's cues with this
    /// device's partial state, so puts are suppressed until this is true.
    hydrated: bool,
    /// Set when a folded observation produced a verdict; cleared when a put
    /// seals the current rollup. A clean rollup is never re-put (no pointless
    /// last-put-wins race with the user's other devices — the `train_post` rule).
    dirty: bool,
    /// The `observed_at_ms` anchoring the debounce window — the last put's time,
    /// or the first observation's time before any put. A put fires when an
    /// observation arrives ≥ `CUE_PUT_DEBOUNCE_S` later, or on an explicit flush.
    /// Clock-free: the cadence rides the observation stream's own timestamps,
    /// never a wall clock (the CueEngine's honest-time discipline).
    last_put_ms: u64,
}

/// Map a derived cue verdict to the topic-model example label it trains as
/// (owner doc § Layer A): a `watch-complete` is a weak *more like this*, a
/// `skip` a weak *less like this*. The mapping lives at the `FeedManager` call
/// site so `fauna-text-model` (which owns [`ExampleLabel`]) stays free of a
/// `fauna-feed` / cue-vocabulary dependency.
///
/// `None` for a verdict this build does not name ([`CueVerdict::Other`]): it
/// trains as no example at all — the restrictive reading, never a live label
/// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in full*:
/// a hand-written projection obeys the unknown arm's duty).
fn example_label_from_verdict(v: &CueVerdict) -> Option<ExampleLabel> {
    match v {
        CueVerdict::WatchComplete => Some(ExampleLabel::MoreLikeThis),
        CueVerdict::Skip => Some(ExampleLabel::LessLikeThis),
        CueVerdict::Other(_) => None,
    }
}

/// The `fauna_e2e_agent::FEED_RELOADS_KEY` JSON — `{"started": N, "completed":
/// M}` from [`FeedManager::reload_counts`], derived once here so every app leg
/// is a read, never a reimplementation (the `conv_receive_cycles_json` shape).
/// `None` (no manager built yet — pre-auth) is the legitimate zero, mirroring a
/// fresh manager's own `(0, 0)`; an app *without* the leg publishes nothing at
/// all, and the two must stay different answers (convention 11).
pub fn feed_reloads_json(counts: Option<(u64, u64, u64)>) -> serde_json::Value {
    let (started, completed, committed_gen) = counts.unwrap_or((0, 0, 0));
    serde_json::json!({
        "started": started,
        "completed": completed,
        "committed_gen": committed_gen,
    })
}

/// The `data.feed.posts` JSON array every app's e2e test-agent state dump
/// serializes into (`feed.md` § Interaction bar) — one dict per loaded
/// [`PostSummary`], derived once here so every app leg is a read, never a
/// hand-spelled reimplementation (the `feed_reloads_json` idiom).
///
/// Field-for-field, this is linux's/windows' own dump
/// (`apps/fauna-linux/src/main.rs::sync_state_json`,
/// `AppDataSnapshot.cs`) plus `is_muted` — apple's richer pattern
/// (`FaunaMacApp.swift`'s `manager.isMuted` read): the e2e harness's
/// `_feed_posts_from_state` (`actions/feed.py`) filters muted posts out of
/// every state-backed reader to stay index-aligned with the element-backed
/// ones, so an app that never emits the key can silently drift out of that
/// alignment the moment a real mute exists. `is_muted` is threaded in as a
/// closure rather than read off `PostSummary` because
/// [`FeedManager::is_muted`] needs the live manager (mute keywords), not just
/// the cloned snapshot.
///
/// `reposted_post_id`/`viewer_repost_id` serialize `None` as JSON `null`
/// (linux's/windows' shape) rather than `""` (apple's) — the harness's
/// truthiness-based readers accept either, and `null` is the more honest
/// "absent" for a field callers key off `.get(...)`.
pub fn feed_posts_json(
    posts: &[PostSummary],
    is_muted: impl Fn(&str) -> bool,
) -> serde_json::Value {
    serde_json::Value::Array(
        posts
            .iter()
            .map(|p| {
                serde_json::json!({
                    "post_id": p.post_id,
                    "author": p.author,
                    "body": p.body,
                    "timestamp": p.timestamp,
                    "tags": p.tags,
                    "has_media": p.has_media,
                    "media_hash": p.media_hash.clone().unwrap_or_default(),
                    "is_reply": p.is_reply,
                    "is_muted": is_muted(&p.post_id),
                    "like_count": p.like_count,
                    "reply_count": p.reply_count,
                    "repost_count": p.repost_count,
                    "quote_count": p.quote_count,
                    "viewer_liked": p.viewer_liked,
                    "reposted_post_id": p.reposted_post_id,
                    "viewer_repost_id": p.viewer_repost_id,
                    // Every link preview in the body with its state, in body
                    // order (`RenderDocument::link_previews`, render-model.md
                    // § D4): a card is absent while a preview is still
                    // resolving too, so this is what lets a test wait until a
                    // preview has FAILED before it reads "no card". tui's key.
                    "link_previews": p
                        .document
                        .link_previews()
                        .into_iter()
                        .map(|(url, state)| serde_json::json!({ "url": url, "state": state.name() }))
                        .collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// A gated post built + signed but not yet created: the client uploads
/// `GatedPostBuild.encrypted_blob` first (the post's `encrypted_ref` must
/// resolve on the nest), then `submit_gated_post` sends `post_bytes`.
struct PendingGatedPost {
    post_bytes: Vec<u8>,
    /// Hex BLAKE3 of the sealed blob — the upload reply must echo it.
    encrypted_ref_hex: String,
    /// The composer as it stood when this post was built — what a successful
    /// `submit_gated_post` clears against ([`FeedComposeState::clear_sent`]).
    /// Carried from the prepare because the upload runs between the two calls,
    /// and the composer stays editable through it. `None` for a sealed reply or
    /// quote, which never came from the composer and clears nothing in it.
    sent: Option<FeedComposeState>,
    /// A room-restricted post, whose sealed body declares the room arm's
    /// sidecar class rather than a tier's (`FeedManager::gated_upload_sidecar`).
    room: bool,
    /// `Some` when the staged post is a **sealed reply or quote**
    /// ([`FeedManager::prepare_sealed_reply`]): the submit then behaves as
    /// `compose_referencing_post` does — the target's counters are read back,
    /// the feed is not reloaded, and the composer is left alone.
    reference: Option<PendingReference>,
}

/// What a staged sealed reply or quote answers — enough for the submit to
/// refresh the target's counters the way `compose_referencing_post` does.
struct PendingReference {
    post_id: String,
    kind: ReferenceKind,
    /// The nest's interact door refuses reply/quote on any non-`fauna` native
    /// token, so only a `fauna` target's counters are read back.
    refresh_counters: bool,
}

/// The per-post seal id an audience-restricted compose minted for its
/// attachment, held between the attachment seal and the body seal.
///
/// **Not on `FeedComposeState`.** That record is a `uniffi::Record` /
/// `Serialize` every app's bindings mirror and `save_draft` persists; a raw
/// derive input has no business in either (a persisted draft would carry a key
/// input for a post that may never exist). It rides here beside
/// `pending_gated`, the other manager-internal half-built-post slot.
struct PendingComposeSeal {
    seal_id: [u8; 32],
    audience: SealAudience,
}

/// The audience a [`PendingComposeSeal`] was minted for.
enum SealAudience {
    /// The `compose.gate_tier` value in force when the id was minted — or, for
    /// a sold post, the auto-minted unlock tier
    /// [`FeedManager::stage_sell_tier`] had staged by then.
    Tier(String),
    /// The `compose.gate_room` in force, and the seal its base key was
    /// resolved under — so the body re-derives that SAME base through
    /// `RoomPostKeys::room_post_base_key` rather than asking for the room's
    /// key "right now" a second time, which after an epoch advance or a
    /// rotation would seal the body and its photos under two keys.
    Room {
        room: [u8; 32],
        seal: fauna_core::room_post::RoomPostSeal,
    },
}

impl PendingComposeSeal {
    fn for_tier(&self, tier: &str) -> bool {
        matches!(&self.audience, SealAudience::Tier(t) if t == tier)
    }
}

/// The auto-minted unlock tier a "Sell this post…" compose staged, held between
/// [`FeedManager::stage_sell_tier`] and [`FeedManager::prepare_sell_post`].
///
/// Phase one exists **only** so an attachment has a period key to seal against:
/// `stage_tier` persists the fresh key into `fauna.state.subscriptions` custody and derives the
/// birth blob's address locally, both of which must happen before the photo can
/// be sealed, while the tier itself is not created server-side until
/// `commit_tier` in phase two. An app with no attachment never calls phase one
/// — `prepare_sell_post` runs it inline, so its single-call flow is unchanged.
struct PendingSellTier {
    /// The minted `unlock:…` tier name.
    tier: String,
    /// The rank the toggle resolved to — decided at stage time, so a
    /// `prepare_sell_post` argument cannot silently move a post between the
    /// "subscribers get it free" and pay-per-view arms after the photo sealed.
    rank: u32,
    staged: fauna_client_subscriptions::orchestration::StagedTier,
}

/// The decoded gated envelope of a loaded post (author + `GatedInfo`), cached
/// by [`FeedManager::gated_blob_hash`] for the unlock decrypt.
struct ResolvedGated {
    author_hex: String,
    gated: fauna_core::subscription::types::GatedInfo,
}

impl<R: RpcRequester + Clone> FeedManager<R> {
    /// Construct over the WS-RPC transport `nest` and the local actor's 32-byte
    /// signing secret. The snapshot starts empty + `Loading`.
    pub fn new(nest: R, actor_secret: [u8; 32]) -> Self {
        Self {
            nest,
            actor_secret,
            state: RwLock::new(FeedSnapshot::default()),
            page: RwLock::new(PageState::default()),
            reload_gen: std::sync::atomic::AtomicU64::new(0),
            reload_commits: std::sync::atomic::AtomicU64::new(0),
            reload_committed_gen: std::sync::atomic::AtomicU64::new(0),
            loaded_query: RwLock::new(None),
            #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
            reload_hold: ReloadHold::default(),
            observers: RwLock::new(Vec::new()),
            resolved_quotes: RwLock::new(HashMap::new()),
            revealed_remote: RwLock::new(HashSet::new()),
            unlock_purchase_requested: RwLock::new(HashSet::new()),
            resolved_previews: RwLock::new(HashMap::new()),
            base_scores: RwLock::new(HashMap::new()),
            sealed: RwLock::new(SealedScorers::default()),
            composition: RwLock::new(Vec::new()),
            pending_gated: RwLock::new(None),
            pending_seal: RwLock::new(None),
            pending_sell_tier: RwLock::new(None),
            resolved_gated: RwLock::new(HashMap::new()),
            unlocked_bodies: RwLock::new(HashMap::new()),
            room_post_labels: RwLock::new(HashMap::new()),
            sealed_media_keys: RwLock::new(HashMap::new()),
            cue: RwLock::new(CueState::default()),
            engagement_factors: RwLock::new(Vec::new()),
            share_signals: AtomicBool::new(false),
            post_index_observer: RwLock::new(None),
            room_post_keys: RwLock::new(None),
            period_keys: RwLock::new(None),
            preferences: RwLock::new(None),
        }
    }

    /// Install the room-post key seam (see the field's docs) — the
    /// conversations plane's answer to "which key opens this room's post".
    /// Replaces any earlier one: a re-authed session hands over a fresh plane.
    pub fn set_room_post_keys(&self, keys: Arc<dyn fauna_core::room_post::RoomPostKeys>) {
        *self.room_post_keys.write().unwrap() = Some(keys);
    }

    /// Install the period-key store (see the field's docs). Replaces any
    /// earlier one: a re-authed session hands over its own account's store.
    pub fn set_period_key_store(&self, store: fauna_client_subscriptions::SharedPeriodKeyStore) {
        *self.period_keys.write().unwrap() = Some(store);
    }

    /// Install the preference read seam (see the field's docs). Replaces any
    /// earlier one: a re-authed session hands over its own account's store.
    pub fn set_preference_store(&self, store: fauna_client_config::SharedPreferenceStore) {
        *self.preferences.write().unwrap() = Some(store);
    }

    /// The muted words and the trained-topic registry, off the preference
    /// read seam — or why they could not be read.
    async fn muted_words_and_factors(
        &self,
    ) -> Result<
        (
            Vec<fauna_core::data::MutedKeyword>,
            Vec<fauna_core::data::TrainedFactorMeta>,
        ),
        String,
    > {
        let store = self
            .preferences
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| "the account store is not ready yet".to_string())?;
        let moderation = store.moderation().await.map_err(|e| e.to_string())?;
        let personalization = store.personalization().await.map_err(|e| e.to_string())?;
        Ok((moderation.muted_keywords, personalization.trained_factors))
    }

    /// The installed period-key store, or why there is none.
    fn period_key_store(&self) -> Result<fauna_client_subscriptions::SharedPeriodKeyStore, String> {
        self.period_keys
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| "the account store is not ready yet".to_string())
    }

    /// The author's period-key custody, folded.
    async fn period_key_custody(&self) -> Result<fauna_core::data::SubscriptionsConfig, String> {
        self.period_key_store()?
            .custody()
            .await
            .map_err(|e| format!("period keys: {e}"))
    }

    /// An author-side orchestration over this manager's nest, identity and
    /// period-key custody.
    fn subscriptions_author(
        &self,
    ) -> Result<fauna_client_subscriptions::orchestration::SubscriptionsAuthor<R>, String> {
        Ok(
            fauna_client_subscriptions::orchestration::SubscriptionsAuthor::over(
                self.nest.clone(),
                ActorKeypair::from_secret(self.actor_secret),
                self.period_key_store()?,
            ),
        )
    }

    /// Register the local index's trickle observer (see the field's docs).
    /// One slot, last-set-wins — there is one local index per login.
    pub fn set_post_index_observer(
        &self,
        observer: Arc<dyn fauna_client_search::OwnPostIndexObserver>,
    ) {
        *self.post_index_observer.write().unwrap() = Some(observer);
    }

    /// Hand a nest-confirmed create to the index trickle, deriving the text
    /// from the **bytes that were sent** — never the composer state — so the
    /// staged text is the same `Post::body_text` extraction the nest's own
    /// enumeration rows carry, whatever body variant the flow built.
    fn observe_own_post_created(&self, post_id: &str, sent_bytes: &[u8]) {
        let observer = self.post_index_observer.read().unwrap().clone();
        let Some(observer) = observer else {
            return;
        };
        let Some(post) = fauna_core::data::Post::decode_resolved_bytes(sent_bytes) else {
            return;
        };
        observer.own_post_created(post_id, &post.body_text());
    }

    // ── Reactivity ───────────────────────────────────────────────

    /// A cheap clone of the current state. The observer reads this on every
    /// `on_changed()`.
    ///
    /// D3 (render-model.md § D3): the manager-owned per-post reveal set is
    /// projected onto each `PostSummary.document` here — a post in the set has its
    /// `RemoteImage` blocks stamped `revealed:true`, so the client walks one
    /// authoritative document instead of OR-ing a per-card reveal flag. A no-op
    /// walk when nothing is revealed (the common case). This covers BOTH the feed
    /// list card and the post-detail, since both read the same snapshot post.
    pub fn snapshot(&self) -> FeedSnapshot {
        let mut snap = self.state.read().unwrap().clone();
        // D4 (render-model.md § D4): project the resolved link-preview state onto each
        // post's `LinkPreview` block FIRST — the producer emits the block `Resolving`; once
        // `resolve_link_preview` records a terminal state for its URL, every snapshot flips
        // the matching block to `Resolved`/`Failed`, so the client walks one authoritative
        // document, never an out-of-band preview map. This MUST run before the reveal walk
        // below: the cached `PreviewState::Resolved` carries `revealed: false`, so folding it
        // in after the reveal walk would clobber a just-revealed og:image. A no-op walk when
        // nothing is resolved (the common case).
        let previews = self.resolved_previews.read().unwrap();
        if !previews.is_empty() {
            for p in &mut snap.posts {
                for block in &mut p.document.blocks {
                    if let fauna_core::render::RenderBlock::LinkPreview { url, state } = block
                        && let Some(resolved) = previews.get(url)
                    {
                        *state = resolved.clone();
                    }
                }
            }
        }
        // D3 + D4 reveal (render-model.md § D3/D4): project the manager-owned per-post reveal
        // set onto each `PostSummary.document` — a post in the set has its `RemoteImage` AND
        // its Resolved link-preview og:image blocks stamped `revealed:true`, so the client
        // walks one authoritative document instead of OR-ing a per-card reveal flag. A no-op
        // walk when nothing is revealed (the common case). Covers BOTH the feed list card and
        // the post-detail, since both read the same snapshot post.
        let revealed = self.revealed_remote.read().unwrap();
        if !revealed.is_empty() {
            for p in &mut snap.posts {
                if revealed.contains(&p.post_id) {
                    p.document.set_remote_images_revealed(true);
                }
            }
        }
        label_room_posts(&mut snap);
        let me_hex = hex::encode(ActorKeypair::from_secret(self.actor_secret).actor_id().0);
        state_reply_audiences(&mut snap, &me_hex);
        snap
    }

    pub fn add_observer(&self, obs: Arc<dyn FeedSnapshotObserver>) {
        self.observers.write().unwrap().push(obs);
    }

    /// Drop all registered observers (mirrors `ConversationsManager`: each
    /// authenticated-window build attaches a fresh observer; call this at
    /// sign-out so stale receiver loops close).
    pub fn clear_observers(&self) {
        self.observers.write().unwrap().clear();
    }

    fn notify(&self) {
        for o in self.observers.read().unwrap().iter() {
            o.on_changed();
        }
    }

    // ── Draft persistence (v2, posts rail) ───────────────────────

    /// The composer's in-progress state serialised to its canonical at-rest
    /// bytes. The client glue seals these under the owner's `BackupKey` and
    /// PUTs them to the `__drafts` reserved folder at `path = "posts"` after a
    /// compose change (`reserved-folders.md` § Drafts Sync). Byte-stable for
    /// equal logical state, so an unchanged draft re-uploads identically —
    /// which is what lets `DraftsSync::save_if_changed` skip the no-op write.
    ///
    /// The exact twin of `ConversationsManager::drafts_snapshot_bytes` for the
    /// posts rail; what rests (and what deliberately does not) is
    /// [`crate::drafts::PostDrafts`]'s contract.
    pub fn drafts_snapshot_bytes(&self) -> Vec<u8> {
        let compose = self.state.read().unwrap().compose.clone();
        crate::drafts::PostDrafts::from_compose(&compose).snapshot_bytes()
    }

    /// Restore the composer from bytes the client glue fetched from `__drafts`
    /// and unsealed with the owner's `BackupKey` — the load-on-launch /
    /// cross-device catch-up path.
    ///
    /// A corrupt or unreadable blob is logged and ignored (keep the empty
    /// composer) rather than failing the surface: an unopenable draft must never
    /// cost the user the ability to post. An all-empty draft is also a no-op —
    /// the composer's default already *is* that state, so notifying would only
    /// churn observers (and, via the autosave observer, cost an upload tick).
    pub fn restore_drafts(&self, bytes: Vec<u8>) {
        let drafts = match crate::drafts::PostDrafts::restore_from_bytes(&bytes) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("failed to restore posts drafts: {e}");
                return;
            }
        };
        if drafts.is_empty() {
            return;
        }
        drafts.apply_to(&mut self.state.write().unwrap().compose);
        self.notify();
    }

    // ── e2e test-helper surface ──────────────────────────────────
    //
    // The feed twin of `ConversationsManager`'s injection seam, and it carries the
    // same two-gate rule (full rationale on that impl block in
    // `fauna-conversations/src/manager.rs`): **visibility** is
    // `any(test, debug_assertions, feature = "test-helpers")` so a plain debug
    // build of an in-process consumer reaches it, while a release build strips it
    // unless a release-profile e2e build opts in via the feature; any FFI/wasm
    // export of these stays keyed on the FEATURE ONLY, so generated faces never
    // depend on the build profile. `docs/goal/architecture/testing.md` convention
    // 15 owns the rule. Driven solely from the e2e command bridge
    // (`window.__fauna_callCommand` on web; the `feed_inject_posts` test-agent
    // command on linux/tui).
    //
    // NOTE: a *runtime*-inert seam is not sufficient on its own — convention 15
    // ratified that a runtime gate alone still ships scripted-control code in the
    // release binary. Hence the compile-time arm above.

    /// Replace the entire feed snapshot and notify observers — lets a tier_2
    /// cross-app test inject a post list (e.g. one with
    /// [`VerificationStatus::Failed`] driving the unverified-source-badge) so the
    /// Feed page renders it without a real nest query. A real signed post is only
    /// ever `Unchecked`/`Verified`, so this is the only way to exercise the
    /// `Failed` badge render (`security.md` § Client display of unverified
    /// content). Build the snapshot with
    /// [`crate::test_support::feed_snapshot_with_posts`].
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn set_feed_snapshot_for_test(&self, snapshot: FeedSnapshot) {
        *self.state.write().unwrap() = snapshot;
        self.notify();
    }

    /// Test-only: drive [`FeedSnapshot::error`] directly — the exact observable
    /// state the real `Err` arm of a background fetch leaves (see the
    /// `s.error = Some(LocalizedText::key_arg("feed.error_load", ...))` site
    /// above), without needing a real nest-side failure to reach it. There is
    /// no *product* path that fails a feed fetch on demand, mirroring why
    /// `ConversationsManager::inject_page_error_for_test` exists. Unlike
    /// [`Self::set_feed_snapshot_for_test`] (wholesale replace), this only
    /// touches `error`, so an injected error can be asserted against whatever
    /// posts/feeds state a fixture already built.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn inject_error_for_test(&self, error: LocalizedText) {
        self.state.write().unwrap().error = Some(error);
        self.notify();
    }

    /// Test-only: arm a one-shot hold on the NEXT reload. That reload still
    /// decides whether to keep or clear the list and publishes the result (the
    /// pre-fetch notify), then parks before fetching until
    /// [`release_held_reload_for_test`](Self::release_held_reload_for_test).
    ///
    /// It exists for one question no clock may answer (`e2e-conventions.md`
    /// convention 14): what is on screen *while* a reload is in flight
    /// (`ui/feed.md` § The read model — a refresh keeps its posts until the new
    /// page lands, a switch clears them up front). Without a hold that window is
    /// a race the fetch usually wins. The generation counters show the parked
    /// state causally: [`reload_counts`](Self::reload_counts) reads
    /// `started > committed_gen` for as long as it is held.
    ///
    /// Why a client-side hold and not the nest's reply hold (`rpc_hold_test_hook`,
    /// which serves the PRE-fetch windows): an app whose e2e agent awaits the
    /// reload (tui awaits its feed ops so a click's reply means the op landed)
    /// cannot tell a held reply from a slow nest, and would park on it — no reads,
    /// no release. A hold the manager owns is one the agent can see
    /// ([`reload_hold_armed_for_test`](Self::reload_hold_armed_for_test)) before
    /// it decides to await.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn hold_next_reload_for_test(&self) {
        self.reload_hold.arm();
    }

    /// Test-only: release the reload [`hold_next_reload_for_test`](Self::hold_next_reload_for_test)
    /// parked (or disarm a hold no reload has reached yet). It then fetches and
    /// commits as usual.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn release_held_reload_for_test(&self) {
        self.reload_hold.release();
    }

    /// Test-only: whether a hold is armed and no reload has reached it yet. An
    /// app's e2e agent reads this to START a feed op rather than await it while a
    /// hold is armed — the held reload cannot land until it is released, so
    /// awaiting it would park the agent (and every command that could release
    /// it) behind it.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn reload_hold_armed_for_test(&self) -> bool {
        self.reload_hold.is_armed()
    }

    /// Install a `MutedKeywords` sealed scorer directly, so a client unit test
    /// can exercise its **collapse render** ([`Self::is_muted`]) without a real
    /// nest.
    ///
    /// The production path installs this entry inside `load_sealed_scorers`,
    /// which runs only during a real `fetch_page` — so a snapshot injected with
    /// [`Self::set_feed_snapshot_for_test`] can never collapse, and every
    /// app's `feed-post-muted` branch was untestable below tier_3 (the gap
    /// `test_feed_muted_posts.py`'s docstring calls out). The entry built here is
    /// the same shape and the same implicit weight the production path builds.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn set_muted_keywords_for_test(&self, words: Vec<String>) {
        let words: Vec<fauna_core::data::MutedKeyword> =
            words.iter().map(|w| w.as_str().into()).collect();
        let entries = if words.is_empty() {
            Vec::new()
        } else {
            vec![SealedEntry {
                factor: fauna_core::scoring::factor::MUTED_KEYWORDS.to_string(),
                weight_permille: MUTED_KEYWORDS_IMPLICIT_WEIGHT,
                scorer: SealedScorer::MutedKeywords(words),
            }]
        };
        *self.sealed.write().unwrap() = SealedScorers::new(entries);
        self.notify();
    }

    /// Install a composed **trained topic factor** and its sealed model
    /// directly, so a client unit test can exercise the per-post training
    /// gestures' render — [`Self::train_target_factor`] (does a menu show the
    /// verbs or the target sheet?) and [`Self::example_label_for`] (is the verb
    /// marked?) — without a real nest.
    ///
    /// The production path installs both inside `load_sealed_scorers` +
    /// `set_composition`, which run only during a real `fetch_page` — so, exactly
    /// as with [`Self::set_muted_keywords_for_test`], every app's train-verb
    /// branch was untestable below tier_3. Same shape and same weight the
    /// production path builds; `model` is the sealed model the markers live in,
    /// so a caller can seed an already-marked post.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn set_trained_factor_for_test(&self, factor: &str, model: TopicModel) {
        *self.composition.write().unwrap() = vec![FeedCompositionEntry {
            factor: factor.to_string(),
            weight_permille: 1000,
            extra: Default::default(),
        }];
        *self.sealed.write().unwrap() = SealedScorers::new(vec![SealedEntry {
            factor: factor.to_string(),
            weight_permille: 1000,
            scorer: SealedScorer::Topic(Box::new(model)),
        }]);
        self.notify();
    }

    /// Seed the live engagement-cue engine with `content_ids` (each recorded a
    /// `WatchComplete` verdict) and — unlike
    /// [`Self::set_muted_keywords_for_test`] / [`Self::set_trained_factor_for_test`],
    /// which only touch local state — actually `PUT` the sealed rollup to the
    /// nest, bypassing the put-debounce. This is the seam a capture-less client
    /// (web, windows; tui's own dwell path already produces one for real) needs
    /// to reach "Clear activity data"
    /// (`engagement-cues.md` § At rest → [`Self::delete_cue_rollup`]) with an
    /// actual `cues:v1` row to delete, so the clear-button e2e assertion can
    /// check the row is **gone** rather than merely that the click didn't error.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub async fn set_cue_rollup_for_test(&self, content_ids: Vec<String>) -> Result<(), String> {
        let mut rollup = CueRollup::new();
        for (i, content_id) in content_ids.iter().enumerate() {
            rollup.record(content_id, CueVerdict::WatchComplete, 10_000, i as u64 + 1);
        }
        self.put_cue_rollup(&rollup).await?;
        let mut c = self.cue.write().unwrap();
        c.engine = CueEngine::new(rollup);
        c.hydrated = true;
        c.dirty = false;
        Ok(())
    }

    /// Register `per_post_key` as what opens the sealed media blob `blob_hash`,
    /// so a client unit test can exercise both non-trivial arms of its post-image
    /// path's [`Self::open_media_bytes`] call — the item that opens, and the item
    /// that does not — without a real nest.
    ///
    /// The production path registers these only inside
    /// [`Self::unlock_gated_post`], which needs a gated post resolved against a
    /// nest and a held key that opens its body — so, exactly as with
    /// [`Self::set_muted_keywords_for_test`], an app's routing of fetched blob
    /// bytes through the open was untestable without one. Same map, same key
    /// shape the unlock writes.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn register_sealed_media_for_test(&self, blob_hash: &str, per_post_key: [u8; 32]) {
        self.sealed_media_keys
            .write()
            .unwrap()
            .insert(blob_hash.to_string(), zeroize::Zeroizing::new(per_post_key));
    }

    // ── Feed list ────────────────────────────────────────────────

    /// Refresh the feed selector list (`fauna.feed.list` → `feeds`). Plumbing
    /// the user-action methods rely on (create/delete refresh it); a client
    /// calls it once on entering the Feed page.
    pub async fn refresh_feeds(&self) {
        let feed = FeedClient::new(self.nest.clone());
        match feed.feed_list().await {
            Ok(reply) => {
                let feeds = reply
                    .feeds
                    .into_iter()
                    .map(|f| FeedSummaryView {
                        feed_id: f.feed_id,
                        name: f.name,
                        combination: f.combination,
                        scope: f.scope,
                        contributor_seeds: f.contributor_seeds,
                    })
                    .collect();
                self.state.write().unwrap().feeds = feeds;
            }
            Err(e) => {
                self.state.write().unwrap().error = Some(LocalizedText::key_arg(
                    "feed.error_feeds",
                    "message",
                    e.to_string(),
                ));
            }
        }
        // Keep the composer's gate-to-tier option set (`compose-gate-tier-select`
        // ← `own_tiers`) fresh alongside the feed page itself. This rides the
        // page-*load* read (`refresh_feeds`, which every app calls on entering
        // the Feed page) rather than the post-*query* path (`reload`, which only
        // runs once a feed/trending is selected): on a fresh nest with no feeds
        // selected — exactly the gate-to-tier compose scenario — `reload` never
        // ran, so a tier the author just minted was absent from the picker
        // (spurious `feed.compose_gate_no_key` at submit). Living here delivers
        // the "tracks tier CRUD with no per-app wiring" invariant for every
        // app, retiring web's per-app explicit `refreshOwnTiers()` workaround
        // (priority #2/#4). Cheap own-read; best-effort.
        self.refresh_own_tiers().await;
        // The room options ride the same read for the same reason: a room the
        // user joined since the last load is offered with no per-app wiring.
        // A local read through the seam — no nest round trip.
        self.refresh_own_rooms().await;
        self.notify();
    }

    /// Refresh the subscribed bridge-feed list (`fauna.bridges.feeds.list` →
    /// `bridge_feeds`). The `bridge-feed-unsubscribe-button` rows. A client
    /// calls it once on entering the Feed page; subscribe/unsubscribe refresh it
    /// after a successful mutation. Kept separate from `refresh_feeds` because
    /// bridge subscriptions are a distinct nest table.
    pub async fn refresh_bridge_feeds(&self) {
        let bridges = BridgesClient::new(self.nest.clone());
        match bridges.feeds_list().await {
            Ok(reply) => {
                let bridge_feeds = reply
                    .subscriptions
                    .into_iter()
                    .map(|s| BridgeFeedView {
                        id: s.id,
                        bridge: s.bridge,
                        feed_uri: s.feed_uri,
                        name: s.name,
                    })
                    .collect();
                self.state.write().unwrap().bridge_feeds = bridge_feeds;
            }
            Err(e) => {
                self.state.write().unwrap().error = Some(LocalizedText::key_arg(
                    "feed.error_feeds",
                    "message",
                    e.to_string(),
                ));
            }
        }
        self.notify();
    }

    /// Refresh the bridges the nest can actually serve (`fauna.bridges.list`,
    /// server-filtered by build feature *and* the per-bridge runtime `available`
    /// flag) → `available_bridges`, the `bridge-form-bridge-select` option set. A
    /// client calls it once on entering the Feed page (alongside
    /// `refresh_bridge_feeds`). Populating the selector from this set — never a
    /// hard-coded per-app protocol list — is the capability *consumption* of
    /// `version-compatibility.md` § Dimension 3: the client never offers a
    /// protocol the nest's build doesn't support (which would otherwise let the
    /// tester subscribe to a feed no provider can ever fetch). On error the prior
    /// set is left intact (the bridge list being momentarily unreachable must not
    /// error the whole Feed page — that path is owned by `refresh_feeds`); an
    /// empty set ⇒ the client hides the subscribe form.
    pub async fn refresh_available_bridges(&self) {
        let bridges = BridgesClient::new(self.nest.clone());
        if let Ok(reply) = bridges.list().await {
            let roster = crate::bridge_roster(&reply.bridges);
            let available = reply
                .bridges
                .into_iter()
                .filter(|b| b.available)
                .map(|b| AvailableBridge {
                    id: b.id,
                    name: b.name,
                })
                .collect();
            {
                let mut s = self.state.write().unwrap();
                s.available_bridges = available;
                s.bridge_roster = roster;
            }
            self.notify();
        }
    }

    // ── Selection / search (each drives a fresh query) ───────────

    /// Select a feed (`feed-item`) and load its first page. `None` ⇒ the nest's
    /// local feed. Clears the prior list + search-independent error and
    /// re-queries. Also clears [`trending_selected`](crate::FeedSnapshot::trending_selected)
    /// so the two selection fields never both point somewhere (§ Selection).
    pub async fn select_feed(&self, feed_id: Option<String>) {
        {
            let mut s = self.state.write().unwrap();
            s.selected_feed = feed_id;
            s.trending_selected = false;
        }
        self.reload().await;
    }

    /// Select the built-in **Trending** virtual feed (`feed-trending-item`) and
    /// load its first page (`trending.md` § The Trending feed): the scored
    /// sibling of the local feed over `fauna.feed.trending.posts`, no feed row.
    /// Sets `trending_selected` and clears `selected_feed` in one write, the
    /// exact mirror of `select_feed(None)` = local — the two fields are never
    /// both set.
    pub async fn select_trending_feed(&self) {
        {
            let mut s = self.state.write().unwrap();
            s.trending_selected = true;
            s.selected_feed = None;
        }
        self.reload().await;
    }

    /// Re-run the current query — same selection (Local / Trending / a custom
    /// feed), same search term — and with it **reload the sealed scorers**.
    ///
    /// The named seam a client calls when it re-enters its feed surface. It
    /// exists because the sealed scorers (the user's muted keywords and trained
    /// topic factors) are loaded inside [`Self::reload`] and nowhere else, so a
    /// client that edits them on a *different* screen — the `muted-words`
    /// Settings sub-page — and then navigates back would keep rendering against
    /// the pre-edit scorers, silently showing content the user just asked never
    /// to see. iOS shipped exactly that bug (`test_feed_muted_posts.py`'s
    /// note); tui had it too until this seam existed.
    ///
    /// Prefer this to `select_feed(current)`: that resets `trending_selected`,
    /// so re-selecting "the current feed" would quietly drop a viewer out of
    /// Trending back into Local.
    pub async fn refresh_current_feed(&self) {
        self.reload().await;
    }

    /// The current [`Selection`], derived from the snapshot's two selection
    /// fields. `trending_selected` wins (it and `selected_feed` are never both
    /// set), then a `Some(feed_id)`, else the local feed.
    fn current_selection(&self) -> Selection {
        let s = self.state.read().unwrap();
        if s.trending_selected {
            Selection::Trending
        } else if let Some(fid) = &s.selected_feed {
            Selection::Custom(fid.clone())
        } else {
            Selection::Local
        }
    }

    /// Update the search term and **re-query** the selected feed with
    /// `search=Some(term)` (`feed.md` § Where logic lives: search is a
    /// re-query, never a client-side filter). An empty/whitespace term clears
    /// the search.
    pub async fn set_search_query(&self, term: Option<String>) {
        let term = term.filter(|t| !t.trim().is_empty());
        self.state.write().unwrap().search_query = term;
        self.reload().await;
    }

    /// Clear the search (`feed-search-clear`) and re-query.
    pub async fn clear_search(&self) {
        self.set_search_query(None).await;
    }

    /// Load the next page (`Load more`) and append it, deduplicating by
    /// `post_id` and preserving the nest order. No-op when there's no further
    /// page or a load is already in flight.
    ///
    /// The sealed scorers resolved at `reload` are reused as-is — the models do
    /// not change mid-scroll (a train re-ranks explicitly), so paging costs no
    /// extra crypto or round-trips. The grown window is re-ranked as a whole:
    /// the newly-appended page's sealed contributions can legitimately lift a
    /// post above ones already on screen, which is exactly the loaded-window
    /// semantics § Scoring specifies.
    pub async fn load_more(&self) {
        use std::sync::atomic::Ordering;
        // The page belongs to the CURRENT reload generation; if a reload
        // supersedes it mid-fetch, this page is stale and must not append
        // (same drop rule as a stale `reload` — see `reload_gen`).
        let generation = self.reload_gen.load(Ordering::SeqCst);
        let (has_more, cursor, score_cursor, scored, in_flight) = {
            let s = self.state.read().unwrap();
            let p = self.page.read().unwrap();
            (s.has_more, p.cursor, p.score_cursor, p.scored, p.in_flight)
        };
        let exhausted = if scored {
            score_cursor.is_none()
        } else {
            cursor.is_none()
        };
        if !has_more || in_flight || exhausted {
            return;
        }
        self.page.write().unwrap().in_flight = true;

        let result = self.fetch_page(cursor, score_cursor, scored).await;
        if self.reload_gen.load(Ordering::SeqCst) != generation {
            return; // superseded — the newer reload owns the page state now
        }
        match result {
            Ok(page) => {
                let has_more = page.has_more();
                {
                    let mut s = self.state.write().unwrap();
                    append_deduped(&mut s.posts, page.items);
                    // Newly-paged posts arrive sealed; re-fold this reader's open
                    // unlocks so scrolling never reverts a post to its teaser.
                    self.reapply_unlocked(&mut s.posts);
                    s.has_more = has_more;
                }
                {
                    let mut p = self.page.write().unwrap();
                    p.cursor = page.next_cursor;
                    p.score_cursor = page.next_score_cursor;
                    p.in_flight = false;
                }
                self.rerank_window();
            }
            Err(e) => {
                // Keep the list we have; surface the page failure (manual
                // retry — `feed.md` § Errors & edge cases).
                self.state.write().unwrap().error =
                    Some(LocalizedText::key_arg("feed.error_load", "message", e));
                self.page.write().unwrap().in_flight = false;
            }
        }
        self.notify();
    }

    /// Fetch page 1 for the current `selected_feed` + `search_query`, replacing
    /// the list. The shared body of `select_feed` / `set_search_query` /
    /// `clear_search` and the post-submit refresh.
    ///
    /// This is where the feed's **effective composition** is resolved and its
    /// **sealed scorers** are loaded, because both are properties of the query,
    /// not of the page: the composition decides whether the query is score-
    /// ordered at all, and the scorers are what re-rank the result.
    async fn reload(&self) {
        // Claim a fresh generation. Every commit below (and `load_more`'s) is
        // gated on the generation still being current, so when reloads overlap
        // (clear_search racing a debounced set_search_query; feed A selected
        // just before feed B) a SLOW stale fetch landing last is dropped
        // instead of clobbering the newer reload's committed state.
        use std::sync::atomic::Ordering;
        let generation = self.reload_gen.fetch_add(1, Ordering::SeqCst) + 1;
        // Is this a SWITCH to a different query, or a REFRESH of the one whose
        // posts are on screen? Only the switch may clear the list up front:
        // showing feed A's posts under feed B's header is a lie, so those go
        // immediately. A refresh's posts are still true until the new page
        // lands — and clearing them is how a refresh whose fetch never returns
        // (a socket still flapping after a nest flip, no deadline under the
        // call) leaves the reader on an empty feed with `status = Loading`, no
        // error, and no second re-hydrate coming: the reconnect that would fire
        // one is suppressed while this reload is still running. The landed page
        // replaces the list wholesale either way, so a refresh that keeps its
        // posts still converges — a server-side deletion does not linger.
        let query_now = (
            self.current_selection(),
            self.state.read().unwrap().search_query.clone(),
        );
        let refreshing_same_query = self.loaded_query.read().unwrap().as_ref() == Some(&query_now);
        {
            let mut s = self.state.write().unwrap();
            s.status = FeedStatus::Loading;
            s.error = None;
            if !refreshing_same_query {
                s.posts.clear();
            }
        }
        {
            let mut p = self.page.write().unwrap();
            p.cursor = None;
            p.score_cursor = None;
            p.in_flight = true;
        }
        self.base_scores.write().unwrap().clear();
        self.notify();
        // Test-only: a reload the hold was armed for parks HERE — after the list
        // it kept or cleared is published, before anything is fetched — until it
        // is released. A no-op unless armed.
        #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
        self.reload_hold.park_if_armed().await;

        // (The composer's gate-to-tier option set is refreshed on feed-page load
        // in `refresh_feeds`, not here — `reload` doesn't run on a fresh nest with
        // no feed selected, which is exactly when a just-minted tier must appear.)

        // Resolve the composition + load the sealed scorers before the query:
        // the composition decides the ordering mode the query must ask for.
        let selection = self.current_selection();
        let composition = match &selection {
            Selection::Local => Vec::new(),
            // Trending has no feed row, but the caller's GLOBAL factor set still
            // composes with it (`trending.md` § The Trending feed — "composes
            // with every tier-1 factor they own"): the nest folds the
            // *transparent* globals into the served key, and fetching them here
            // lets the sealed-compose seam fold any *sealed* global (topic model)
            // on top, exactly as it does for a custom feed.
            Selection::Trending => self.resolve_global_factors().await,
            Selection::Custom(fid) => self.resolve_effective_composition(Some(fid)).await,
        };
        // The Trending read is always score-ordered on the nest (the implicit
        // `[(trending, 1000)]` composition), so it paginates on the keyset
        // cursor and the sealed-compose seam re-ranks on top — force scored even
        // when the globals-only composition is empty.
        let scored = matches!(selection, Selection::Trending) || !composition.is_empty();
        let sealed_error = self.load_sealed_scorers(&composition).await;
        // Superseded during the pre-flight reads → stop before writing the
        // (stale) composition or paying for a fetch whose result would drop.
        if self.reload_gen.load(Ordering::SeqCst) != generation {
            return;
        }
        *self.composition.write().unwrap() = composition;
        self.page.write().unwrap().scored = scored;

        let result = self.fetch_page(None, None, scored).await;
        // Superseded while fetching → drop the stale result untouched. The
        // newer reload owns `state`/`page` now (its own commit or error path
        // resets `in_flight`), and it already notified.
        if self.reload_gen.load(Ordering::SeqCst) != generation {
            return;
        }
        match result {
            Ok(page) => {
                let has_more = page.has_more();
                {
                    let mut s = self.state.write().unwrap();
                    let mut fresh = Vec::new();
                    append_deduped(&mut fresh, page.items);
                    s.posts = fresh;
                    // Re-fold any gated posts this reader already unsealed — the
                    // freshly-fetched list arrives sealed and would otherwise revert
                    // an open unlock to the teaser (the iOS reload-revert).
                    self.reapply_unlocked(&mut s.posts);
                    s.has_more = has_more;
                    s.status = FeedStatus::Loaded;
                    // A sealed scorer that failed to load is a *non-fatal*
                    // notice: the posts are here, only the personalized
                    // re-ranking is missing. Surfacing it beats silently serving
                    // an un-personalized feed the user asked to be personalized.
                    s.error = sealed_error;
                }
                {
                    let mut p = self.page.write().unwrap();
                    p.cursor = page.next_cursor;
                    p.score_cursor = page.next_score_cursor;
                    p.in_flight = false;
                }
                // These posts belong to this query now — the next reload reads
                // it to tell a refresh of them from a switch away. Written only
                // on the Ok arm: a failed fetch replaced nothing.
                *self.loaded_query.write().unwrap() = Some(query_now);
                self.rerank_window();
            }
            Err(e) => {
                {
                    let mut s = self.state.write().unwrap();
                    s.status = FeedStatus::Error;
                    s.error = Some(LocalizedText::key_arg("feed.error_load", "message", e));
                }
                self.page.write().unwrap().in_flight = false;
            }
        }
        // This reload committed (Ok or Err — both are a landed verdict; only the
        // superseded early-returns above never reach here). Recorded after every
        // state write and before the notify, so a reader that observes either
        // value can read the committed snapshot (`reload_commits`' field doc).
        // `generation` is monotonic and this is its only writer, so the store is
        // a max even though it is not spelled as one.
        self.reload_commits.fetch_add(1, Ordering::SeqCst);
        self.reload_committed_gen
            .store(generation, Ordering::SeqCst);
        self.notify();
    }

    /// The `{started, completed, committed_gen}` reload triple — initiations
    /// (`reload_gen`, the same value the supersede guard claims), committed
    /// results (`reload_commits`) and the generation of the newest commit
    /// (`reload_committed_gen`, the barrier's release condition). E2e plumbing
    /// for `fauna_e2e_agent::FEED_RELOADS_KEY`; pure atomic reads, safe on the
    /// agent ack path (no blocking I/O).
    pub fn reload_counts(&self) -> (u64, u64, u64) {
        use std::sync::atomic::Ordering;
        (
            self.reload_gen.load(Ordering::SeqCst),
            self.reload_commits.load(Ordering::SeqCst),
            self.reload_committed_gen.load(Ordering::SeqCst),
        )
    }

    /// The selected feed's **effective composition** — its own composition
    /// (`fauna.feed.get`) plus the user's global factor set
    /// (`fauna.feed.factors.get`), concatenated.
    ///
    /// Concatenated, not merged: a factor present in *both* containers
    /// contributes **both** terms, because that is how the nest composes it
    /// ("the same factor in both containers → the two terms sum; conflicts
    /// resolve by arithmetic" — frame § Composition, and `feed_routes.rs`
    /// `query_feed_core` does the same `composition.extend(global)`). De-duping
    /// here would silently disagree with the nest's key on exactly the posts a
    /// user cared enough about to weight twice.
    ///
    /// The local feed (`None`) has no composition and is always chronological.
    /// A failed read yields an empty composition — the feed then loads
    /// chronologically rather than not at all.
    async fn resolve_effective_composition(
        &self,
        feed_id: Option<&str>,
    ) -> Vec<FeedCompositionEntry> {
        let Some(fid) = feed_id else {
            return Vec::new();
        };
        let feed = FeedClient::new(self.nest.clone());
        let mut effective = feed
            .feed_get(fid)
            .await
            .ok()
            .and_then(|r| r.composition)
            .unwrap_or_default();
        effective.extend(self.resolve_global_factors().await);
        effective
    }

    /// The caller's **global** factor set (`fauna.feed.factors.get`) — the
    /// per-user `(factor, weight)` entries the nest folds into *every* one of
    /// their feeds' scored orderings (frame § Composition). It is both the
    /// global half of a custom feed's effective composition (above) and the
    /// **only** composition source for the `Trending` virtual feed, which has no
    /// feed row of its own. A failed read yields an empty set.
    async fn resolve_global_factors(&self) -> Vec<FeedCompositionEntry> {
        let feed = FeedClient::new(self.nest.clone());
        feed.feed_factors_get()
            .await
            .map(|r| r.factors)
            .unwrap_or_default()
    }

    /// Load the sealed scorers for `composition` into [`Self::sealed`], and
    /// return a non-fatal error to surface if one of them could not be opened.
    ///
    /// Two sealed scorers (§ Scoring):
    ///
    /// * every `topic:<hex>` key in the composition → fetch its sealed blob and
    ///   unseal it under the personalization seal keys. An **absent** blob is not an error: the
    ///   factor exists but was never trained on any device, and a fresh model
    ///   scores a flat neutral 500 (zero ordering effect), so it is simply
    ///   skipped.
    /// * the **muted-keyword penalty**, whenever the user has any muted words —
    ///   implicitly, at unit weight, whether or not any composition names it
    ///   (see [`crate::personalization`] for why it is implicit). An explicit
    ///   entry, if one ever exists, replaces the implicit weight — and if the
    ///   user somehow has it in *both* containers, the weights sum, matching the
    ///   nest's arithmetic for every other doubly-listed factor.
    ///
    /// Loaded even for a chronological feed: the muted list also drives the
    /// collapse render treatment ([`Self::is_muted`]), which applies everywhere,
    /// not only where a mute can sink.
    async fn load_sealed_scorers(
        &self,
        composition: &[FeedCompositionEntry],
    ) -> Option<LocalizedText> {
        let seal_key = model_seal_keys(&BackupKey::derive(&self.actor_secret));
        let mut entries: Vec<SealedEntry> = Vec::new();
        let mut error: Option<LocalizedText> = None;

        let models = PersonalizationClient::new(self.nest.clone());
        for entry in composition
            .iter()
            .filter(|e| fauna_core::scoring::is_topic_factor(&e.factor))
        {
            match models.model_fetch(entry.factor.clone()).await {
                Ok(reply) => {
                    // No blob yet ⇒ the factor exists but was never taught
                    // anything on any device. Compose it anyway, as a **fresh
                    // model**: it scores a flat neutral 500, so it is provably
                    // inert (no ordering effect), and having the entry present is
                    // what gives the user's *first* training gesture a slot to
                    // swap its model into — without it, the first tap on a brand-
                    // new topic would train the model, put it, and then fail to
                    // re-rank the feed until the next reload.
                    let model = match reply.sealed_blob {
                        None => Some(TopicModel::new()),
                        Some(blob) => match unseal_topic_model(&blob, &seal_key) {
                            Ok(model) => Some(model),
                            // A blob that exists but will not open is a real
                            // fault (corruption, or a newer client's layout). Do
                            // NOT fall back to a fresh model: it would score as
                            // untrained AND the next train would overwrite
                            // everything the user has taught it.
                            Err(e) => {
                                error = Some(LocalizedText::key_arg(
                                    "feed.error_trained_factor",
                                    "message",
                                    e.to_string(),
                                ));
                                None
                            }
                        },
                    };
                    if let Some(model) = model {
                        entries.push(SealedEntry {
                            factor: entry.factor.clone(),
                            weight_permille: entry.weight_permille,
                            scorer: SealedScorer::Topic(Box::new(model)),
                        });
                    }
                }
                Err(e) => {
                    error = Some(LocalizedText::key_arg(
                        "feed.error_trained_factor",
                        "message",
                        e.to_string(),
                    ));
                }
            }
        }

        // Subscribed tier-3 `text-model` labelers (frame § Tier-3 artifact
        // kinds). The client fetches the raw artifact via `inspect` at feed
        // (re)load and follows version bumps there — there is no nest-side row
        // to read, because the placement matrix forbids the nest evaluating
        // content even over public posts. Only `text-model` rides this seam: a
        // `list` labeler is materialized nest-side and already arrives as a bus
        // term, and a `wasm` labeler is a holder's drain output.
        let labelers = LabelersClient::new(self.nest.clone());
        for entry in composition
            .iter()
            .filter(|e| fauna_core::scoring::is_labeler_factor(&e.factor))
        {
            let Some(id) = fauna_core::scoring::labeler_factor_id(&entry.factor) else {
                continue;
            };
            match labelers.inspect(id.0.to_vec()).await {
                Ok(reply) => {
                    if reply.artifact_kind != fauna_core::scoring::artifact_kind::TEXT_MODEL {
                        continue;
                    }
                    match validate_text_model_artifact(reply.wasm_bytes.as_ref()) {
                        Ok(artifact) => {
                            // ⚠ An artifact whose tokenizer contract this build
                            // does not implement is left **INERT** — no entry, so
                            // no ordering effect — rather than scored by a
                            // tokenizer that would split its n-grams differently.
                            // A silent mis-score is the one outcome the version
                            // exists to prevent. The user-facing "needs a newer
                            // app" signal is the catalog row's kind badge, not a
                            // feed banner: the feed is *correct*, just missing a
                            // factor.
                            if !fauna_core::scoring::text_model_version_supported(artifact.version)
                            {
                                continue;
                            }
                            entries.push(SealedEntry {
                                factor: entry.factor.clone(),
                                weight_permille: entry.weight_permille,
                                scorer: SealedScorer::SubscribedModel(Box::new(
                                    PublishedTextModel::new(
                                        artifact.more_docs,
                                        artifact.less_docs,
                                        artifact
                                            .ngrams
                                            .into_iter()
                                            .map(|n| (n.ngram, n.more, n.less)),
                                    ),
                                )),
                            });
                        }
                        // The nest validates this at publish, so bytes that fail
                        // here are a fault, not a normal state: surface it rather
                        // than composing a factor we could not read.
                        Err(e) => {
                            error = Some(LocalizedText::key_arg(
                                "feed.error_subscribed_model",
                                "message",
                                e.to_string(),
                            ));
                        }
                    }
                }
                Err(e) => {
                    error = Some(LocalizedText::key_arg(
                        "feed.error_subscribed_model",
                        "message",
                        e.to_string(),
                    ));
                }
            }
        }

        let (muted, engagement_on) = match self.muted_words_and_factors().await {
            Ok((muted, factors)) => {
                // Batched with the muted-keyword read (never a per-observation
                // store read): the composed topic factors whose registry meta
                // opts into engagement training (`topic-factors.md` § Training
                // signals v2). A cue-verdict transition trains exactly these.
                let on: Vec<String> = factors
                    .iter()
                    .filter(|m| m.learn_from_engagement)
                    .filter_map(|m| m.factor_key())
                    .filter(|key| composition.iter().any(|e| e.factor == *key))
                    .collect();
                (muted, on)
            }
            // Surfaced, never swallowed: if the record cannot be read we do not
            // know the user's muted words, so muted content would render with no
            // hint that their filters are off — precisely the content they asked
            // never to see. The feed still loads (the posts are here); the notice
            // says the filters are not applied. Engagement training also stands
            // down for this feed (no registry ⇒ no known opt-ins).
            Err(e) => {
                error = Some(LocalizedText::key_arg(
                    "feed.error_muted_keywords",
                    "message",
                    e.to_string(),
                ));
                (Vec::new(), Vec::new())
            }
        };
        *self.engagement_factors.write().unwrap() = engagement_on;
        if !muted.is_empty() {
            let explicit: i64 = composition
                .iter()
                .filter(|e| crate::personalization::is_muted_keywords_factor(&e.factor))
                .map(|e| e.weight_permille)
                .sum();
            let weight = if explicit == 0 {
                MUTED_KEYWORDS_IMPLICIT_WEIGHT
            } else {
                explicit
            };
            entries.push(SealedEntry {
                factor: fauna_core::scoring::factor::MUTED_KEYWORDS.to_string(),
                weight_permille: weight,
                scorer: SealedScorer::MutedKeywords(muted),
            });
        }

        *self.sealed.write().unwrap() = SealedScorers::new(entries);
        error
    }

    /// One page of the current feed query. Branches `local` vs custom feed,
    /// chronological vs `order=score`, and folds in the active search term.
    /// Records each item's base ordering key into [`Self::base_scores`] before
    /// mapping (`map_post` drops it — `PostSummary` has no `score` field, by
    /// design).
    async fn fetch_page(
        &self,
        cursor: Option<i64>,
        score_cursor: Option<(i64, i64)>,
        scored: bool,
    ) -> Result<FetchedPage, String> {
        let search = self.state.read().unwrap().search_query.clone();
        let feed = FeedClient::new(self.nest.clone());
        // Both halves of the keyset cursor or neither — a key without its
        // tiebreak would silently re-inherit the tie-skipping the compound
        // cursor exists to prevent. Unused by the chronological local branch.
        let (key, tiebreak) = match score_cursor {
            Some((k, t)) => (Some(k), Some(t)),
            None => (None, None),
        };
        let (items, next_cursor, next_score_cursor) = match self.current_selection() {
            Selection::Local => {
                let r = feed
                    .feed_local_posts(cursor, Some(PAGE_LIMIT), search)
                    .await
                    .map_err(|e| e.to_string())?;
                (r.posts, r.cursor, None)
            }
            Selection::Trending => {
                // The Trending virtual read is always score-ordered on the nest
                // (implicit `[(trending, 1000)]` + globals), so there is no
                // chronological cursor and no `order` param — the keyset pair is
                // the only cursor.
                let r = feed
                    .feed_trending_posts(Some(PAGE_LIMIT), key, tiebreak, search)
                    .await
                    .map_err(|e| e.to_string())?;
                let next_score = r.score_cursor.zip(r.score_cursor_created_at);
                (r.posts, None, next_score)
            }
            Selection::Custom(fid) => {
                let r = feed
                    .feed_posts(
                        fid,
                        cursor,
                        Some(PAGE_LIMIT),
                        scored.then(|| "score".to_string()),
                        key,
                        tiebreak,
                        search,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let next_score = r.score_cursor.zip(r.score_cursor_created_at);
                (r.posts, r.cursor, next_score)
            }
        };

        {
            let mut base = self.base_scores.write().unwrap();
            for item in &items {
                if let Some(score) = item.score {
                    base.insert(item.post_id.clone(), score);
                }
            }
        }

        Ok(FetchedPage {
            items: items.into_iter().map(map_post).collect(),
            next_cursor,
            next_score_cursor,
        })
    }

    /// Re-rank the loaded window by the adjusted key `FeedPostItem.score + Σ
    /// sealed contributions` (`topic-factors.md` § Scoring). The **one**
    /// sanctioned client-side ordering adjustment (`feed.md` § Where logic
    /// lives): the manager owns it, so the snapshot still supplies the visible
    /// order and the shells never sort.
    ///
    /// A no-op unless the query is score-ordered *and* something sealed is
    /// loaded. Both guards matter: adding sealed terms to a chronological page's
    /// uniformly-absent base key would turn a chronological feed into a
    /// sealed-factor-ordered one, which no user asked for.
    fn rerank_window(&self) {
        let sealed = self.sealed.read().unwrap();
        if !self.page.read().unwrap().scored || sealed.is_empty() {
            return;
        }
        let base = self.base_scores.read().unwrap();
        let mut s = self.state.write().unwrap();
        let adjusted: HashMap<String, i64> = s
            .posts
            .iter()
            .filter_map(|p| {
                let base_micro = base.get(&p.post_id)?;
                Some((
                    p.post_id.clone(),
                    adjusted_score_micro(*base_micro, &sealed.terms_for(p)),
                ))
            })
            .collect();
        rerank_loaded_window(&mut s.posts, &adjusted);
    }

    // ── Compose ──────────────────────────────────────────────────

    /// Update the composer (`compose-text-field` / `compose-tags-field` /
    /// staged file) and clear any stale compose error — the user is editing, so
    /// a prior failure is no longer current. Non-empty enforcement happens at
    /// `submit_post`; tag normalization happens there too.
    pub fn update_compose(&self, text: String, tags: String, attached_file: Option<AttachedFile>) {
        {
            let mut s = self.state.write().unwrap();
            s.compose.text = text;
            s.compose.tags = tags;
            s.compose.attached_file = attached_file;
            s.compose.error = None;
        }
        self.notify();
    }

    /// The submit-time half of `feed.md` § Persistence → *Attachments by
    /// content address*: a staged `attached_file` whose `blob_hash` is still
    /// `None` when a submit path reads the composer is a handle with no blob
    /// behind it — a draft restored after a relaunch, or synced from another
    /// device, whose bytes this device never held (this rail uploads only at
    /// submit, `ui/media.md` § Encryption at rest). Refuse, naming the file so
    /// the author attaches it again, and keep the draft; never publish the
    /// text alone. Every submit path — [`submit_post`](Self::submit_post), the
    /// tier and room arms of [`prepare_gated_blob`](Self::prepare_gated_blob),
    /// [`prepare_sell_post`](Self::prepare_sell_post) — runs this before it
    /// stages or mints anything, so the refusal is the same on all seven apps;
    /// the conversations rail's `resolve_attachments` is its twin.
    fn refuse_unresolved_attachment(&self, file: Option<&AttachedFile>) -> Result<(), String> {
        let Some(file) = file else { return Ok(()) };
        if file.blob_hash.is_some() {
            return Ok(());
        }
        {
            let mut s = self.state.write().unwrap();
            s.compose.error = Some(LocalizedText::key_arg(
                "feed.compose_attachment_missing",
                "filename",
                file.name.clone(),
            ));
        }
        self.notify();
        Err(format!(
            "staged attachment {:?} has no uploaded blob — attach it again",
            file.name
        ))
    }

    /// Submit the composed post (`post-submit-button`): validate non-empty,
    /// build + sign a `Post` from the compose text + normalized tag facets via
    /// the shared `fauna_client_core::post::build_post`, and create it over
    /// `fauna.posts.create`. On success clears the composer and refreshes the
    /// list; on failure stamps `FeedComposeState.error`.
    ///
    /// Media path: a staged `attached_file` whose `blob_hash` the client has
    /// already resolved (the blob upload is platform glue — the manager stays
    /// WS-RPC-only) is inlined as a single `MediaItem` and the post is built
    /// via the shared `build_post_with_media` (`PostBody::TextWithMedia`). A
    /// staged file with no uploaded blob behind it (`blob_hash == None`) is
    /// **refused**, naming the file, never posted text-only
    /// ([`refuse_unresolved_attachment`](Self::refuse_unresolved_attachment)).
    /// This is the uniform shape all seven apps inherit (`feed.md` § Where
    /// logic lives — the picker/upload are client glue, the validation +
    /// post-build are shared).
    pub async fn submit_post(&self) -> Result<(), String> {
        // What this submit sends, read once — and what the success arm clears
        // against, so an edit made during the create survives it.
        let sent = self.state.read().unwrap().compose.clone();
        let (text, tags_raw, attached_file) = (
            sent.text.clone(),
            sent.tags.clone(),
            sent.attached_file.clone(),
        );
        if text.trim().is_empty() {
            let mut s = self.state.write().unwrap();
            s.compose.error = Some(LocalizedText::key("feed.compose_empty"));
            drop(s);
            self.notify();
            return Err("empty post".to_string());
        }
        self.refuse_unresolved_attachment(attached_file.as_ref())?;

        {
            let mut s = self.state.write().unwrap();
            s.compose.submitting = true;
            s.compose.error = None;
        }
        self.notify();

        let bytes = {
            let kp = ActorKeypair::from_secret(self.actor_secret);
            let tags = normalize_tags(&tags_raw);
            match media_item_from_staged(attached_file.as_ref()) {
                Ok(Some(media)) => build_post_with_media(&kp, &text, vec![media], &tags, None)
                    .map_err(|e| e.to_string()),
                Ok(None) => build_post(&kp, &text, &tags, None).map_err(|e| e.to_string()),
                Err(e) => Err(e),
            }
        };

        let result = match bytes {
            Ok(bytes) => {
                let posts = PostsClient::new(self.nest.clone());
                match posts.posts_create(bytes.clone()).await {
                    Ok(reply) => {
                        // The trickle chokepoint: the create is nest-confirmed,
                        // so the just-composed post becomes locally searchable
                        // now instead of at the next reconcile walk.
                        self.observe_own_post_created(&reply.post_id, &bytes);
                        Ok(())
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
            Err(e) => Err(e),
        };

        match result {
            Ok(()) => {
                {
                    let mut s = self.state.write().unwrap();
                    s.compose.clear_sent(&sent);
                }
                // Refresh the list so the new post shows.
                self.reload().await;
                Ok(())
            }
            Err(e) => {
                {
                    let mut s = self.state.write().unwrap();
                    s.compose.submitting = false;
                    s.compose.error = Some(LocalizedText::key_arg(
                        "feed.error_submit",
                        "message",
                        e.clone(),
                    ));
                }
                self.notify();
                Err(e)
            }
        }
    }

    /// Act on a post from the interaction bar — `feed-like-button` /
    /// `feed-reply-button` / `feed-repost-button` / `feed-quote-button`
    /// (`feed.md` § Interaction bar, ratified 2026-06-27) — and fold the
    /// resulting counters back into the loaded window.
    ///
    /// **This is the one interact path for all seven apps.** It used to be
    /// per-app client glue calling `PostsClient::posts_interact` directly and
    /// throwing the reply away, which is why on six of seven apps a tapped ♥
    /// never moved: the count renders from
    /// [`PostSummary::like_count`](crate::PostSummary::like_count) and nothing
    /// wrote it. linux alone re-queried the whole feed afterwards — correct, but
    /// a full round-trip that also **re-ranks the window under the user's
    /// finger** (a like moves `content_meta.score`, which is a ranking input),
    /// so a tap could reorder the timeline. Neither shape is right per-app.
    ///
    /// The counters applied here are the nest's own post-act values
    /// (`PostInteractReply::counts`), never a local guess. That distinction is
    /// load-bearing rather than fastidious: the nest's like counter is
    /// **idempotent per (actor, post)**, so a second tap by the same actor moves
    /// nothing — an optimistic client-side `+1` would be right once and wrong
    /// every time after, with no local way to tell which. `None` (a bridged
    /// source, `unrepost`) leaves the rendered counts exactly as they
    /// were.
    ///
    /// Only the target post's counters change; ordering, selection and the rest
    /// of the window are untouched — the [`delete_post`](Self::delete_post)
    /// shape (mutate the loaded window, `notify`), not a reload.
    pub async fn interact(
        &self,
        post_id: String,
        action: String,
        body: Option<String>,
    ) -> Result<(), String> {
        let posts = PostsClient::new(self.nest.clone());
        let reply = posts
            .posts_interact(post_id.clone(), action, body)
            .await
            .map_err(|e| e.to_string())?;
        let Some(counts) = reply.counts else {
            return Ok(());
        };
        let mut changed = false;
        {
            let mut s = self.state.write().unwrap();
            let FeedSnapshot {
                posts,
                deep_linked_post,
                ..
            } = &mut *s;
            for p in posts
                .iter_mut()
                .chain(deep_linked_post.iter_mut())
                .filter(|p| p.post_id == post_id)
            {
                p.like_count = counts.like_count;
                p.reply_count = counts.reply_count;
                p.repost_count = counts.repost_count;
                p.quote_count = counts.quote_count;
                changed = true;
            }
        }
        // A post the window does not hold needs no repaint — and `notify` is not
        // free on every app (it walks observers and re-renders a frame).
        if changed {
            self.notify();
        }
        Ok(())
    }

    /// Reply to a post from the interaction bar (`feed-reply-button`).
    ///
    /// **This exists because `interact(id, "reply", text)` never was a reply.**
    /// The nest's interact arm only returns target info "so the client can
    /// compose a post" and *discards `body` entirely*
    /// (`bins/fauna-nest/src/interact_routes.rs` — since 2026-09-26 on every
    /// source; the bridged arms are the eligibility door). Every app called it anyway, so a reply typed into
    /// linux's reply dialog or handed to apple's `replyToPost` was accepted,
    /// acked as success, and dropped on the floor. A reply is a *post* that
    /// references its target; composing it is the only thing that moves
    /// `reply_count`.
    ///
    /// See [`compose_referencing_post`](Self::compose_referencing_post) for the
    /// source routing and why the feed is deliberately not reloaded.
    pub async fn reply(&self, post_id: String, body: String) -> Result<(), String> {
        if body.trim().is_empty() {
            return Err("a reply needs a body".to_string());
        }
        self.compose_referencing_post(post_id, ReferenceKind::Reply, body)
            .await
    }

    /// Quote-repost a post from the interaction bar (`feed-quote-button`).
    ///
    /// `body` is the commentary and **may be empty** — `feed.md` § Interaction
    /// bar ratifies today's button as firing a *direct* quote-repost, with the
    /// commentary composer as a deferred fleet-wide follow-on. A quote needs no
    /// wire or render work anywhere: `query_feed` already projects
    /// `Reference::Quote` into `FeedPostItem.quoted_post_id` and the
    /// `quoted-post` embed is shipped on all seven apps, so the composed post
    /// renders correctly the moment it lands.
    pub async fn quote(&self, post_id: String, body: String) -> Result<(), String> {
        self.compose_referencing_post(post_id, ReferenceKind::Quote, body)
            .await
    }

    /// **The confirmed-public reply** — ruling 5's confirmation arm
    /// (`ui/feed.md` § Encryption at rest → *Ruling 5's build — the shape*,
    /// (e)). The reply dialog took the user's explicit per-reply answer under
    /// `feed-reply-public-confirm`, so the words go out as the ordinary public
    /// reference that [`reply`](Self::reply) refuses under a restricted target
    /// (ruling 6's refusal stays the unconfirmed default).
    ///
    /// Its own verb, additive on every face: `reply`'s signature does not
    /// change, so an app built before it keeps refusing — the safe direction.
    /// The gate is the manager's, in two halves. The target must be loaded,
    /// or nothing is composed (the same fall-through `reply` has). And it must
    /// be a post this reader **cannot** write for — [`ReplyAudience::PublicByConfirmation`],
    /// the one case the dialog offers the answer in: a reply this device could
    /// seal to its room or tier is refused here with
    /// [`REPLY_SEALS_INSTEAD`] rather than sent past the arm, because no
    /// dialog ever asked that question. A public target has nothing to
    /// confirm and composes as `reply` would.
    pub async fn reply_public_confirmed(
        &self,
        post_id: String,
        body: String,
    ) -> Result<(), String> {
        if body.trim().is_empty() {
            return Err("a reply needs a body".to_string());
        }
        let me_hex = hex::encode(ActorKeypair::from_secret(self.actor_secret).actor_id().0);
        let audience = {
            let s = self.state.read().unwrap();
            let p = s
                .find_post(&post_id)
                .ok_or_else(|| "the post is no longer loaded".to_string())?;
            reply_audience_of(p, &s.own_rooms, &s.own_tiers, &me_hex)
        };
        match audience {
            Some(ReplyAudience::PublicByConfirmation) => {
                self.compose_referencing_post_as(post_id, ReferenceKind::Reply, body, true)
                    .await
            }
            Some(ReplyAudience::SealedToRoom | ReplyAudience::SealedToTier) => {
                Err(REPLY_SEALS_INSTEAD.to_string())
            }
            None => {
                self.compose_referencing_post(post_id, ReferenceKind::Reply, body)
                    .await
            }
        }
    }

    /// Repost / un-repost from the interaction bar (`feed-repost-button`) —
    /// **one verb, toggle semantics** (`feed.md` § Interaction bar → Repost,
    /// ratified 2026-08-10; no confirmation dialog — a repost is cheap and
    /// instantly reversible by the same toggle).
    ///
    /// Off → on: composes the caller's repost post — empty body,
    /// `Reference::Repost` — through the same
    /// [`build_referencing_post`] door `reply`/`quote` use, folds the created
    /// post's id into the target row's `viewer_repost_id` (the create reply
    /// already returns it, so no wire change), and reads the target's post-act
    /// counters back through the interact door. The feed is deliberately
    /// **not** reloaded (the `compose_referencing_post` rule: a reload
    /// re-ranks the timeline under the user's finger); the caller's new repost
    /// row appears on the next natural reload.
    ///
    /// On → off: `interact(viewer_repost_id, "unrepost")` — the id names the
    /// caller's OWN repost post, which is the whole reason the projection
    /// carries it (`unrepost` shipped 2026-07-15 and was unreachable until a
    /// client could name that id) — then drops that row from the loaded window
    /// (it is the caller's own just-deleted post, the `delete_post` shape),
    /// clears the field, and reads the ORIGINAL's counters back — the unrepost
    /// reply carries `counts: None` by design, since its `post_id` names the
    /// dying repost, not the post on screen.
    ///
    /// A **bridged** post keeps the shipped `interact(post_id, "repost")` path
    /// verbatim (the nest's bridged arms drive the origin protocol's own
    /// repost API) — the same source routing as `compose_referencing_post`,
    /// so no app leg knows the difference. Two devices racing the toggle
    /// converge: each repost post is individually unrepostable and the nest
    /// serves the latest, so a stale `viewer_repost_id` un-reposts one of
    /// them and the next reload reports what remains.
    pub async fn repost(&self, post_id: String) -> Result<(), String> {
        let (is_native, is_fauna_native, viewer_repost_id, audience) = {
            let s = self.state.read().unwrap();
            match s.find_post(&post_id) {
                Some(p) => {
                    let source = crate::SourceKind::classify(&p.source);
                    (
                        source.is_native(),
                        matches!(source, crate::SourceKind::Fauna),
                        p.viewer_repost_id.clone(),
                        referenced_audience(p),
                    )
                }
                // A post the window can't answer for falls back to the
                // interact path — exactly today's behaviour, the safe
                // direction (`compose_referencing_post`'s rule).
                None => (false, false, None, ReferencedAudience::Public),
            }
        };
        if !is_native {
            return self.interact(post_id, "repost".to_string(), None).await;
        }
        match viewer_repost_id {
            None => {
                let digest = fauna_core::hex32::decode(&post_id).map_err(|e| e.to_string())?;
                let target = *PostId::from_digest_dag_cbor(digest).as_bytes();
                let kp = ActorKeypair::from_secret(self.actor_secret);
                // A repost carries no words, so it stays public for a
                // restricted target too — the builder's rule, stated to it
                // rather than assumed here.
                let bytes =
                    build_referencing_post(&kp, "", &[], target, audience, ReferenceKind::Repost)
                        .map_err(|e| e.to_string())?;
                let posts = PostsClient::new(self.nest.clone());
                let reply = posts
                    .posts_create(bytes.clone())
                    .await
                    .map_err(|e| e.to_string())?;
                // The caller's own post owes the same index trickle any
                // composed post does.
                self.observe_own_post_created(&reply.post_id, &bytes);
                self.set_viewer_repost(&post_id, Some(reply.post_id));
                // Best-effort: the repost exists either way, so a failed
                // count read must not read back as a failed repost. Ruling 3
                // (`archive-import.md` § Compatibility → *Slice-3 rulings*):
                // the nest's interact door refuses `repost` on any
                // non-`fauna` native token, so an archive-imported target's
                // refresh is skipped here — the count catches up on the
                // caller's next natural reload instead.
                if is_fauna_native {
                    let _ = self.interact(post_id, "repost".to_string(), None).await;
                }
                Ok(())
            }
            Some(repost_id) => {
                let posts = PostsClient::new(self.nest.clone());
                posts
                    .posts_interact(repost_id.clone(), "unrepost".to_string(), None)
                    .await
                    .map_err(|e| e.to_string())?;
                {
                    let mut s = self.state.write().unwrap();
                    s.posts.retain(|p| p.post_id != repost_id);
                    if s.deep_linked_post
                        .as_ref()
                        .is_some_and(|p| p.post_id == repost_id)
                    {
                        s.deep_linked_post = None;
                    }
                }
                self.set_viewer_repost(&post_id, None);
                // Ruling 3 (`archive-import.md` § Compatibility → *Slice-3
                // rulings*): same skip as the off→on arm above — the door
                // refuses `repost` on an archive-imported target, so the
                // refresh is not worth a refused round trip; the next reload
                // catches up.
                if is_fauna_native {
                    let _ = self.interact(post_id, "repost".to_string(), None).await;
                }
                Ok(())
            }
        }
    }

    /// Fold a new viewer-repost state into every rendered row for `post_id`
    /// and re-emit — the toggle's local half of the per-viewer pair (the nest
    /// projects it fresh on the next reload).
    fn set_viewer_repost(&self, post_id: &str, repost_id: Option<String>) {
        let mut changed = false;
        {
            let mut s = self.state.write().unwrap();
            for p in rendered_posts_mut(&mut s).filter(|p| p.post_id == post_id) {
                p.viewer_repost_id = repost_id.clone();
                changed = true;
            }
        }
        if changed {
            self.notify();
        }
    }

    /// Like / un-like from the interaction bar (`feed-like-button`) — **one
    /// verb, toggle semantics** off the target row's `viewer_liked` (`feed.md`
    /// § Interaction bar → Repost ratifies the carrier; § User actions puts
    /// like/unlike on the *recorded* side of the two-verb split).
    ///
    /// **This is the toggle `unlike` was waiting for.** The nest has shipped
    /// `unlike` since the counters landed, but `PostSummary` carried no viewer
    /// state until the per-viewer pair was ratified 2026-08-10, so every app's
    /// ♥ fired a bare `interact(id, "like")` and un-liking was unreachable from
    /// any UI: a second tap hit the nest's *idempotent* like arm and moved
    /// nothing, on all seven apps.
    ///
    /// Both directions ride the **same door and the same post id** — the
    /// difference from [`repost`](Self::repost), whose off-direction names the
    /// caller's own repost *post* and deletes it. A like composes nothing, so
    /// no row joins or leaves the window; the nest's `like` and `unlike` arms
    /// each return the target's post-act counters and
    /// [`interact`](Self::interact) folds them, so the number on screen moves in
    /// **both** directions with no re-query and no locally-guessed `±1`
    /// (§ Interaction bar forbids the guess: the nest's counter is idempotent
    /// per (actor, post), so a guess is right once and wrong every time after).
    ///
    /// **Why the order differs from `repost`.** There the compose has already
    /// succeeded by the time the viewer field is folded, so the state is
    /// authoritative when written. Here the interact call *is* the recording,
    /// so it goes first and the field flips only on success — a failed call
    /// must not leave a lit ♥ the nest disagrees with.
    ///
    /// A **bridged** post keeps the shipped one-way `interact(post_id, "like")`
    /// verbatim: its interactions live in the origin protocol, which is why the
    /// projection generally carries no viewer state for those rows. A post the
    /// loaded window cannot answer for falls back the same way — the safe
    /// direction, since it is exactly today's behaviour.
    pub async fn like(&self, post_id: String) -> Result<(), String> {
        let (is_native, liked) = {
            let s = self.state.read().unwrap();
            match s.find_post(&post_id) {
                Some(p) => (
                    crate::SourceKind::classify(&p.source).is_native(),
                    p.viewer_liked,
                ),
                None => (false, false),
            }
        };
        if !is_native {
            return self.interact(post_id, "like".to_string(), None).await;
        }
        let action = if liked { "unlike" } else { "like" };
        self.interact(post_id.clone(), action.to_string(), None)
            .await?;
        self.set_viewer_liked(&post_id, !liked);
        Ok(())
    }

    /// Fold a new viewer-like state into every rendered row for `post_id` and
    /// re-emit — the toggle's local half of the per-viewer pair (the nest
    /// projects it fresh on the next reload), twin of
    /// [`set_viewer_repost`](Self::set_viewer_repost).
    fn set_viewer_liked(&self, post_id: &str, liked: bool) {
        let mut changed = false;
        {
            let mut s = self.state.write().unwrap();
            for p in rendered_posts_mut(&mut s).filter(|p| p.post_id == post_id) {
                p.viewer_liked = liked;
                changed = true;
            }
        }
        if changed {
            self.notify();
        }
    }

    /// The shared composer behind [`reply`](Self::reply) and
    /// [`quote`](Self::quote) — build + sign a post referencing `post_id`,
    /// create it, then refresh the target's counters.
    ///
    /// **Source routing** (`ui/feed.md` § Interaction bar → *Reply and quote on
    /// a bridged post*, ratified 2026-09-26). A native post is composed for
    /// directly. A bridged post (bluesky / nostr / activitypub) first asks the
    /// nest's interact door for **one eligibility ack** — `interact(id,
    /// action, None)`, no body — and composes the same signed referencing post
    /// only on `Ok`: the nest's create-side bridge fan-out then derives the
    /// origin protocol's reply or quote from the post's own `Reference`. A
    /// refusal (no linked account, no relays, replies switched off) is returned as the affordance's inline
    /// failure and nothing is composed. There is no interact-with-body path
    /// any more: no arm consumes `body`, the words travel in the signed post.
    /// A post the loaded window can't answer for is routed as bridged — the
    /// door decides, and a native target simply acks.
    ///
    /// **Why the counter refresh is a second call.** The target's counter moves
    /// nest-side when this new post lands (`record_reference_engagements`), but
    /// `fauna.posts.create`'s reply carries no counters. Rather than guess `+1`
    /// — which § Interaction bar forbids, and which the manager has no way to
    /// verify — the post-act values are read back through the interact door,
    /// whose native arm is a pure read that returns exactly them. If a future
    /// `PostCreateReply` carries `counts`, this collapses to one call.
    ///
    /// **Why no reload.** `submit_post` reloads because a new top-level post
    /// belongs at the top of the window. A reply or quote does not: reloading
    /// would re-rank the timeline under the user's finger, which is the precise
    /// defect deleted from linux when the interaction bar was fixed.
    async fn compose_referencing_post(
        &self,
        post_id: String,
        kind: ReferenceKind,
        body: String,
    ) -> Result<(), String> {
        self.compose_referencing_post_as(post_id, kind, body, false)
            .await
    }

    /// [`compose_referencing_post`](Self::compose_referencing_post) with the
    /// audience stated to the builder: `confirmed` upgrades a restricted
    /// target to [`ReferencedAudience::RestrictedPublicConfirmed`] — only
    /// [`reply_public_confirmed`](Self::reply_public_confirmed) passes `true`,
    /// after its own gate; every other door composes unconfirmed and meets the
    /// builder's refusal under a restricted target.
    async fn compose_referencing_post_as(
        &self,
        post_id: String,
        kind: ReferenceKind,
        body: String,
        confirmed: bool,
    ) -> Result<(), String> {
        let (is_native, is_fauna_native, audience) = self
            .state
            .read()
            .unwrap()
            .find_post(&post_id)
            .map(|p| {
                let source = crate::SourceKind::classify(&p.source);
                let audience = match referenced_audience(p) {
                    ReferencedAudience::Restricted if confirmed => {
                        ReferencedAudience::RestrictedPublicConfirmed
                    }
                    other => other,
                };
                (
                    source.is_native(),
                    matches!(source, crate::SourceKind::Fauna),
                    audience,
                )
            })
            .unwrap_or((false, false, ReferencedAudience::Public));
        if !is_native {
            // The eligibility ack. `interact` folds the ack's `counts` when
            // the nest speaks for them (an AP/nostr target — a local row),
            // and leaves the rendered numbers alone when it does not (a
            // bluesky target — its counters live at the origin).
            self.interact(post_id.clone(), kind.action().to_string(), None)
                .await?;
        }

        let digest = fauna_core::hex32::decode(&post_id).map_err(|e| e.to_string())?;
        let target = *PostId::from_digest_dag_cbor(digest).as_bytes();
        let kp = ActorKeypair::from_secret(self.actor_secret);
        // Words under a restricted post are refused here, before anything is
        // created (`ui/feed.md` § Encryption at rest → *A reply, quote or
        // repost of a restricted post*) — the builder owns the rule.
        let bytes = build_referencing_post(&kp, &body, &[], target, audience, kind)
            .map_err(|e| e.to_string())?;

        let posts = PostsClient::new(self.nest.clone());
        let reply = posts
            .posts_create(bytes.clone())
            .await
            .map_err(|e| e.to_string())?;
        // A reply/quote is one of the caller's own posts, so it owes the same
        // index trickle a composed post does (`submit_post`'s chokepoint).
        self.observe_own_post_created(&reply.post_id, &bytes);

        // Best-effort: the post is created either way, so a failed count
        // read must not read back as a failed reply. Ruling 3
        // (`archive-import.md` § Compatibility → *Slice-3 rulings*): the
        // nest's interact door refuses reply/repost/quote on any non-`fauna`
        // native token, so an archive-imported target's refresh is skipped
        // here — the count catches up on the caller's next natural reload
        // instead.
        if is_fauna_native {
            let _ = self
                .interact(post_id, kind.action().to_string(), None)
                .await;
        }
        Ok(())
    }

    /// Delete the caller's own post (`feed-post-delete-button` →
    /// `feed-post-delete-confirm-button`, `feed.md` § State & data shape →
    /// *Post deletion*, IDs user-approved 2026-07-16). Builds + signs a
    /// `Tombstone` naming `post_id` (mirrors `submit_post`'s
    /// build-sign-call shape) and calls the shared `PostsClient::posts_delete`
    /// — `deleted:false` (already gone) is the RPC's own idempotent success,
    /// not an error, so no special-casing here. On success the post drops
    /// from the loaded window immediately rather than waiting on a full
    /// reload (the nest's own projection removal is authoritative for any
    /// future load regardless).
    pub async fn delete_post(&self, post_id: String) -> Result<(), String> {
        let digest = fauna_core::hex32::decode(&post_id).map_err(|e| e.to_string())?;
        let kp = ActorKeypair::from_secret(self.actor_secret);
        let tombstone = Tombstone {
            author: kp.actor_id(),
            post_id: PostId::from_digest_dag_cbor(digest),
            created_at: Timestamp::now(),
        };
        let bytes = sign_and_pack(&kp, &tombstone).map_err(|e| e.to_string())?;
        let posts = PostsClient::new(self.nest.clone());
        posts.posts_delete(bytes).await.map_err(|e| e.to_string())?;
        // Every embed of the deleted post now says it is gone (`ui/feed.md` § Post
        // deletion: other people's quotes and reposts of it stay, and show the
        // not-found state). This device most likely resolved those embeds from
        // its own loaded page, so the cached projection still carries the words
        // just deleted — replace it, and re-fold every loaded card that embeds
        // it, rather than wait for a refetch that the cache would short-circuit.
        let gone = quote::project_not_found(&post_id);
        self.resolved_quotes
            .write()
            .unwrap()
            .insert(post_id.clone(), gone.clone());
        {
            let mut s = self.state.write().unwrap();
            for p in rendered_posts_mut(&mut s).filter(|p| {
                p.quoted_post_id.as_deref() == Some(post_id.as_str())
                    || p.reposted_post_id.as_deref() == Some(post_id.as_str())
            }) {
                let media = p.document.media_blocks();
                p.document = build_post_document(&p.body, Some(&gone), &media);
            }
            s.posts.retain(|p| p.post_id != post_id);
            // A deleted post must not survive in the deep-link slot either —
            // otherwise deleting the post you are viewing leaves it on screen.
            if s.deep_linked_post
                .as_ref()
                .is_some_and(|p| p.post_id == post_id)
            {
                s.deep_linked_post = None;
            }
        }
        self.notify();
        Ok(())
    }

    // ── Gate-to-tier compose + gated unlock ──────────────────────
    // `ui/feed.md` § Encryption at rest; monetization.md § Pillars 2+3 —
    // client UX. The seal/decrypt crypto is `fauna-core::subscription`; the
    // sealed-blob bytes ride the platform's bulk-binary HTTP plane (the one
    // production HTTP carve-out), so the manager hands the bytes out / takes
    // them in and stays WS-RPC-only, exactly like the media-attachment path.

    /// Refresh the composer's gate-to-tier option set
    /// (`compose-gate-tier-select`) from the local actor's own tiers
    /// (`fauna.subscriptions.tiers.list`). Best-effort: a transport failure
    /// leaves the prior set (an empty set just means "no gating offered"),
    /// never a page error.
    pub async fn refresh_own_tiers(&self) {
        let subs = fauna_client_subscriptions::SubscriptionsClient::new(self.nest.clone());
        let Ok(tiers) = subs.tiers_list().await else {
            return;
        };
        let options: Vec<crate::compose::GateTierOption> = tiers
            .into_iter()
            // A per-post pay-to-unlock tier never appears in the compose gate
            // picker (`monetization.md` § Per-post pay-to-unlock — degenerate
            // tier rules): it is auto-minted for one post and gated to it by
            // the "sell this post" flow, so offering it as a general gate
            // target would let the author silently widen what a buyer paid
            // for. The author's `tiers.list` read carries it; this surface
            // filters it out.
            //
            // …and a hidden tier (the reserved owner-only tier the archive
            // import mints) is never a compose-time choice either —
            // monetization.md § The unifying model.
            .filter(|t| t.unlocks_post.is_none() && !t.hidden)
            .map(|t| crate::compose::GateTierOption {
                name: t.name,
                rank: t.rank,
            })
            .collect();
        {
            let mut s = self.state.write().unwrap();
            s.own_tiers = options;
        }
        self.notify();
    }

    /// Stage the composer's gate-to-tier fields (`compose-gate-tier-select` /
    /// `compose-gate-preview-field`) — the gate sibling of
    /// [`update_compose`](Self::update_compose).
    ///
    /// **Clears sell mode**, including on the `None` ("Public") arm: this and
    /// [`update_compose_sell`](Self::update_compose_sell) are the two answers
    /// to one select, so setting either *is* deselecting the other
    /// (`FeedComposeState::sell`).
    pub fn update_compose_gate(&self, gate_tier: Option<String>, gate_preview: String) {
        {
            let mut s = self.state.write().unwrap();
            s.compose.gate_tier = gate_tier;
            s.compose.gate_preview = gate_preview;
            s.compose.sell = None;
            s.compose.gate_room = None;
        }
        // The audience just changed, so any seal id minted for the old one is
        // dead: an attachment sealed under it opens with a key this post will
        // never derive. `prepare_gated_blob` would catch a *tier* change on its
        // own, but not a re-select of the same tier, and dropping it here is
        // the cheaper, uniform rule. A staged *sale* goes with it — leaving
        // this compose gated to a real tier, its unlock tier abandoned.
        *self.pending_seal.write().unwrap() = None;
        *self.pending_sell_tier.write().unwrap() = None;
        self.notify();
    }

    /// Stage the composer's **"Sell this post…"** fields (`compose-sell-price`
    /// / `compose-sell-subscribers-free`) — the sell sibling of
    /// [`update_compose_gate`](Self::update_compose_gate), sharing its teaser
    /// (`compose-gate-preview-field`, which a sold post needs exactly as an
    /// ordinary gated post does).
    ///
    /// `Some` enters sell mode and **clears `gate_tier`**; `None` leaves it
    /// (back to Public). `monetization.md` § Per-post pay-to-unlock.
    pub fn update_compose_sell(&self, sell: Option<SellComposeState>, gate_preview: String) {
        {
            let mut s = self.state.write().unwrap();
            if sell.is_some() {
                s.compose.gate_tier = None;
                s.compose.gate_room = None;
            }
            s.compose.sell = sell;
            s.compose.gate_preview = gate_preview;
        }
        // Same reason as `update_compose_gate`: the audience moved. The staged
        // unlock tier goes too — its period key is what the old seal used, so
        // re-editing the sale invalidates the photo exactly as a gate edit does
        // (`prepare_sell_post` then refuses rather than stranding it).
        *self.pending_seal.write().unwrap() = None;
        *self.pending_sell_tier.write().unwrap() = None;
        self.notify();
    }

    /// Stage the composer's **room** answer — `compose-gate-tier-select`'s
    /// fourth, a room from [`FeedSnapshot::own_rooms`](crate::FeedSnapshot)
    /// by its hex channel id — the room sibling of
    /// [`update_compose_gate`](Self::update_compose_gate), sharing its teaser.
    ///
    /// `Some` makes the post room-restricted and clears `gate_tier` and
    /// `sell`; `None` leaves it (back to Public). Drops any staged seal for the
    /// same reason the other two setters do: the audience moved.
    pub fn update_compose_room(&self, gate_room: Option<String>, gate_preview: String) {
        {
            let mut s = self.state.write().unwrap();
            if gate_room.is_some() {
                s.compose.gate_tier = None;
                s.compose.sell = None;
            }
            s.compose.gate_room = gate_room;
            s.compose.gate_preview = gate_preview;
        }
        *self.pending_seal.write().unwrap() = None;
        *self.pending_sell_tier.write().unwrap() = None;
        self.notify();
    }

    /// Stage the composer's teaser (`compose-gate-preview-field`) ALONE — the
    /// one field every restricted answer shares (a tier, a sale, a room). An
    /// app writing it through the setter of "whichever mode is selected" had
    /// to re-read that mode and could flip it (the room answer would have been
    /// dropped by `update_compose_gate`); this touches no answer, and drops no
    /// staged seal, because the teaser is no part of any key.
    pub fn update_compose_preview(&self, gate_preview: String) {
        self.state.write().unwrap().compose.gate_preview = gate_preview;
        self.notify();
    }

    /// Re-read the rooms the composer offers (`own_rooms`) from the installed
    /// room-post seam (`RoomPostKeys::room_post_rooms`) — the room sibling of
    /// [`refresh_own_tiers`](Self::refresh_own_tiers). No seam installed ⇒
    /// none offered, the honest answer for a device with no conversations
    /// plane. Best-effort like its sibling: never a page error.
    ///
    /// Returns whether the list changed (and so whether observers were
    /// notified) — what lets a shell with no observer callback (web, which
    /// re-reads the snapshot after a call) skip a whole-snapshot read on the
    /// conversations tick that re-asks this, the common no-change answer.
    pub async fn refresh_own_rooms(&self) -> bool {
        let keys = self.room_post_keys.read().unwrap().clone();
        let options = match keys {
            Some(keys) => keys
                .room_post_rooms()
                .await
                .into_iter()
                .map(|r| crate::compose::GateRoomOption {
                    room: hex::encode(r.room),
                    label: r.label,
                })
                .collect(),
            None => Vec::new(),
        };
        {
            let mut s = self.state.write().unwrap();
            if s.own_rooms == options {
                return false;
            }
            s.own_rooms = options;
        }
        self.notify();
        true
    }

    /// The installed room-post seam, or the composer error a room compose
    /// stamps when this device has none (it could not seal for the room).
    fn compose_room_keys(
        &self,
    ) -> Result<std::sync::Arc<dyn fauna_core::room_post::RoomPostKeys>, String> {
        let keys = self.room_post_keys.read().unwrap().clone();
        keys.ok_or_else(|| {
            let mut s = self.state.write().unwrap();
            s.compose.error = Some(LocalizedText::key("feed.compose_room_no_key"));
            drop(s);
            self.notify();
            "this device holds no room keys".to_string()
        })
    }

    /// The sidecar the staged gated post's sealed body is uploaded under —
    /// the room arm's `GroupRestrictedPost` for a room-restricted post, a
    /// tier's `PeriodRestrictedPost` otherwise. Hand it to
    /// `fauna_client::upload_sealed_post_blob`, so the class is decided off
    /// the post that was staged and never by an app.
    pub fn gated_upload_sidecar(&self) -> fauna_media::sidecar::UploadSidecar {
        match self.pending_gated.read().unwrap().as_ref() {
            Some(p) if p.room => fauna_media::sidecar::UploadSidecar::room_post(),
            _ => fauna_media::sidecar::UploadSidecar::gated_post(),
        }
    }

    /// Process + seal one compose attachment for **the composer's current
    /// audience**, returning the multipart parts the app POSTs.
    ///
    /// This is the "shared-Rust seal-by-id helper" `ui/media.md` § Encryption
    /// at rest names: a tier's period key must never cross the FFI/wasm
    /// boundary (`fauna_ffi::FfiUploadAudience` deliberately expresses only the
    /// two client-key audiences), so the seal happens here and the app receives
    /// ciphertext it merely transports.
    ///
    /// **Call it at SUBMIT, after the audience is final — never at pick time.**
    /// The audience decides the seal, and a blob POSTed before the author chose
    /// a tier is a plaintext copy of a restricted post's photo sitting on the
    /// nest under a hash anyone can fetch. Blob GET is unauthenticated by
    /// design and the nest exposes no blob DELETE, so the only way to satisfy
    /// "no plaintext copy remains fetchable" is to never upload one.
    ///
    /// - **Public compose** (`gate_tier == None`) — `PublicPost`: plaintext
    ///   passthrough, real MIME on the sidecar, exactly what
    ///   `fauna_client::upload_public_post_blob` produces.
    /// - **Audience-restricted compose** — mints this post's `seal_id`, stashes
    ///   it, and seals under `derive_post_key(period_key, seal_id)`. The
    ///   following [`prepare_gated_blob`](Self::prepare_gated_blob) seals the
    ///   body under the same id, so one key opens the body and its attachments.
    ///
    /// A **sell** compose (`compose.sell`) has no tier of its own until the
    /// sale mints one, so the caller runs
    /// [`stage_sell_tier`](Self::stage_sell_tier) first and this seals under
    /// the tier it staged. A sell compose that reaches here *without* that
    /// first half is refused rather than silently sealed public — the whole
    /// point being that no plaintext copy is ever POSTed.
    pub async fn seal_compose_attachment(
        &self,
        raw: Vec<u8>,
    ) -> Result<ComposeAttachmentUpload, String> {
        let (tier, selling, room) = {
            let s = self.state.read().unwrap();
            (
                s.compose.gate_tier.clone(),
                s.compose.sell.is_some(),
                s.compose.gate_room.clone(),
            )
        };
        // A room-restricted compose: the photo seals as the room arm's
        // `Group` audience under the per-post key the body will use —
        // `derive_post_key(base, seal_id)` — so the seal the base was resolved
        // under is stashed with the id (`SealAudience::Room`), and the body
        // re-derives that same base (`ui/feed.md` § Encryption at rest →
        // *Room-restricted — the ruling*, ruling 4).
        if let Some(room_hex) = room {
            let room = parse_room_id(&room_hex)?;
            let keys = self.compose_room_keys()?;
            let (seal, base) = keys.room_post_seal_key(room).await.inspect_err(|_| {
                self.state.write().unwrap().compose.error =
                    Some(LocalizedText::key("feed.compose_room_no_key"));
                self.notify();
            })?;
            let seal_id = fauna_client_core::post::mint_seal_id().map_err(|e| e.to_string())?;
            let audience = fauna_media::audience::Audience::RestrictedPost {
                post_id: fauna_core::data::ContentHash::from_digest_raw(seal_id),
                audience: fauna_media::audience::RestrictedPostAudience::Group {
                    group_id: fauna_core::subscription::types::MlsGroupId(room.to_vec()),
                    epoch_secret: base,
                },
            };
            *self.pending_seal.write().unwrap() = Some(PendingComposeSeal {
                seal_id,
                audience: SealAudience::Room { room, seal },
            });
            return Ok(seal_compose_upload(&raw, &audience));
        }
        // A sold post seals under the unlock tier the sale itself mints, which
        // exists only once `stage_sell_tier` has run. Reaching for it here is
        // the ENTIRE sell-arm mechanism: the staged tier's period key is
        // already in `fauna.state.subscriptions` custody, so the branch below is the ordinary
        // audience-restricted one with a different tier name — no second
        // derivation, no second key path, nothing new crossing FFI.
        let tier = match (tier, selling) {
            (None, true) => self
                .pending_sell_tier
                .read()
                .unwrap()
                .as_ref()
                .map(|p| p.tier.clone()),
            (tier, _) => tier,
        };

        let audience = match tier {
            None if selling => {
                // Not a hard failure of the post: the caller drops the
                // attachment and submits the text, exactly as every app does
                // today. What it must NOT do is upload the bytes public.
                return Err(
                    "selling a post with an attachment is not supported yet — call \
                     stage_sell_tier first so the photo has a tier to seal under"
                        .into(),
                );
            }
            None => fauna_media::audience::Audience::PublicPost {
                post_id: fauna_core::data::ContentHash::from_digest_raw([0u8; 32]),
            },
            Some(tier) => {
                let period = self.compose_period_key(&tier).await?;
                let seal_id = fauna_client_core::post::mint_seal_id().map_err(|e| e.to_string())?;
                let audience = fauna_media::audience::Audience::RestrictedPost {
                    post_id: fauna_core::data::ContentHash::from_digest_raw(seal_id),
                    audience: fauna_media::audience::RestrictedPostAudience::Period {
                        tier: tier.clone(),
                        period_epoch: period.version,
                        period_key: zeroize::Zeroizing::new(period.key.clone().into()),
                    },
                };
                *self.pending_seal.write().unwrap() = Some(PendingComposeSeal {
                    seal_id,
                    audience: SealAudience::Tier(tier),
                });
                audience
            }
        };
        Ok(seal_compose_upload(&raw, &audience))
    }

    /// The author's own current period key for `tier`, read fresh from Pillar-1
    /// custody (the period-key store — the compose leg never mints one).
    ///
    /// Shared by [`seal_compose_attachment`](Self::seal_compose_attachment) and
    /// [`prepare_gated_blob`](Self::prepare_gated_blob) so an attachment and
    /// the body it rides in can never resolve different keys. Stamps
    /// `compose-error` on failure, like every other compose validation.
    async fn compose_period_key(&self, tier: &str) -> Result<fauna_core::data::TierPeriod, String> {
        self.own_period_key(tier).await.inspect_err(|_| {
            self.state.write().unwrap().compose.error = Some(LocalizedText::key_arg(
                "feed.compose_gate_no_key",
                "tier",
                tier.to_string(),
            ));
            self.notify();
        })
    }

    /// The custody read under [`compose_period_key`](Self::compose_period_key),
    /// with no composer side effect — the sealed reply's read, whose failure is
    /// the reply dialog's to show and never the composer's.
    async fn own_period_key(&self, tier: &str) -> Result<fauna_core::data::TierPeriod, String> {
        let custody = self.period_key_custody().await?;
        fauna_client_subscriptions::custody::current_period(&custody, tier)
            .ok_or_else(|| format!("no period key for tier {tier:?}"))
    }

    /// Build + sign the staged **gated** post and return its sealed full-body
    /// blob for the client to upload (`POST /api/v1/blob`, sidecar class
    /// `PeriodRestrictedPost`, mime `application/octet-stream` — the strict
    /// verifier's sealed-class shape). Returns `Ok(None)` when the composer
    /// isn't gated (callers fall through to [`submit_post`](Self::submit_post)).
    ///
    /// Reads the tier's period key from Pillar-1 custody
    /// (the period-key store — the compose leg never mints its own) and
    /// the live KeyBlob's content address from `fauna.subscriptions
    /// .key_blob.get` (author-readable; the birth blob exists from
    /// `create_tier`). On success the signed post is staged for
    /// [`submit_gated_post`](Self::submit_gated_post); on failure the
    /// composer error is stamped and `Err` returned.
    pub async fn prepare_gated_blob(&self) -> Result<Option<Vec<u8>>, String> {
        let room = self.state.read().unwrap().compose.gate_room.clone();
        if let Some(room) = room {
            return self.prepare_room_blob(&room).await.map(Some);
        }
        let (text, preview, tier, attached_file, sent) = {
            let s = self.state.read().unwrap();
            let Some(tier) = s.compose.gate_tier.clone() else {
                return Ok(None);
            };
            (
                s.compose.text.clone(),
                s.compose.gate_preview.clone(),
                tier,
                s.compose.attached_file.clone(),
                // What this submit sends — staged below, and what
                // `submit_gated_post` clears against once the create confirms.
                s.compose.clone(),
            )
        };

        let fail = |key: &str, arg: Option<(&str, String)>| {
            let msg = match arg {
                Some((k, v)) => LocalizedText::key_arg(key, k, v),
                None => LocalizedText::key(key),
            };
            let mut s = self.state.write().unwrap();
            s.compose.error = Some(msg);
        };

        if text.trim().is_empty() {
            fail("feed.compose_empty", None);
            self.notify();
            return Err("empty post".into());
        }
        if preview.trim().is_empty() {
            fail("feed.compose_gate_preview_empty", None);
            self.notify();
            return Err("empty gated preview".into());
        }
        self.refuse_unresolved_attachment(attached_file.as_ref())?;

        let keypair = ActorKeypair::from_secret(self.actor_secret);

        // The tier's rank drives the sealed post's gate metadata. It normally
        // comes from the compose option cache (`own_tiers`, refreshed on every
        // feed-page load via `refresh_feeds`), but a client can still reach
        // compose with a stale/empty cache (a tier minted since the last feed
        // reload). Resolve it authoritatively: on a cache miss, refresh from the
        // nest once and retry rather than surfacing a spurious "no key for tier".
        // `prepare_gated_blob` already does fresh config + key-blob reads, so
        // resolving the rank the same way keeps the whole seal self-sufficient —
        // the guard lives in shared Rust, so every app inherits it (#2/#4).
        let rank = {
            let s = self.state.read().unwrap();
            s.own_tiers.iter().find(|t| t.name == tier).map(|t| t.rank)
        };
        let rank = match rank {
            Some(r) => r,
            None => {
                self.refresh_own_tiers().await;
                let s = self.state.read().unwrap();
                match s.own_tiers.iter().find(|t| t.name == tier).map(|t| t.rank) {
                    Some(r) => r,
                    None => {
                        drop(s);
                        fail("feed.compose_gate_no_key", Some(("tier", tier.clone())));
                        self.notify();
                        return Err(format!("unknown tier {tier:?}"));
                    }
                }
            }
        };

        // Pillar-1 custody: the author's period key for this tier — the same
        // read `seal_compose_attachment` made, so a staged attachment and this
        // body can never seal under different keys.
        let period = self.compose_period_key(&tier).await?;

        // The live KeyBlob's content address (the post's `key_blob_ref`).
        let subs = fauna_client_subscriptions::SubscriptionsClient::new(self.nest.clone());
        let blob_reply = subs
            .key_blob_get(keypair.actor_id(), &tier)
            .await
            .map_err(|e| {
                fail("feed.compose_gate_no_key", Some(("tier", tier.clone())));
                format!("key_blob.get: {e}")
            })?;
        let key_blob_ref: [u8; 32] = blob_reply
            .blob_hash
            .as_ref()
            .try_into()
            .map_err(|_| "key_blob.get returned a non-32-byte hash".to_string())?;

        let period_key: [u8; 32] = period.key.clone().into();

        // The attachment, if the app sealed + uploaded one for this compose.
        // `seal_compose_attachment` minted the `seal_id` it sealed under, so
        // the body must reuse that exact id — a fresh one would produce a key
        // that opens the body but none of its photos (`ui/media.md`
        // § Encryption at rest: one per-post key seals both). A stash minted
        // against a *different* tier means the composer's gate changed between
        // the two calls; refuse rather than ship a post whose photo nobody can
        // open.
        let media = media_item_from_staged(attached_file.as_ref()).inspect_err(|_| {
            fail("feed.error_submit", None);
            self.notify();
        })?;
        let stashed = self.pending_seal.write().unwrap().take();
        let seal_id = match (&media, stashed) {
            (Some(_), Some(seal)) if seal.for_tier(&tier) => seal.seal_id,
            (Some(_), _) => {
                // NOT `compose_gate_no_key`: the device holds the key fine —
                // the *attachment* is the stale half, and the only thing the
                // author can do is attach it again.
                fail("feed.compose_attachment_stale", None);
                self.notify();
                return Err(format!(
                    "staged attachment was not sealed for tier {tier:?} — re-attach it"
                ));
            }
            (None, _) => fauna_client_core::post::mint_seal_id().map_err(|e| e.to_string())?,
        };
        let full_body = match media {
            Some(item) => fauna_core::data::PostBody::TextWithMedia {
                content: text.clone(),
                facets: vec![],
                items: vec![item],
            },
            None => fauna_core::data::PostBody::Text {
                content: text.clone(),
                facets: vec![],
            },
        };
        let build = fauna_client_core::post::build_gated_post_at(
            &keypair,
            &preview,
            full_body,
            &tier,
            rank,
            key_blob_ref,
            &period_key,
            seal_id,
            &fauna_client_core::post::PostAuthoring::now(),
        )
        .map_err(|e| e.to_string())?;

        let encrypted_ref_hex = hex::encode(build.encrypted_ref);
        {
            let mut s = self.state.write().unwrap();
            s.compose.submitting = true;
            s.compose.error = None;
        }
        *self.pending_gated.write().unwrap() = Some(PendingGatedPost {
            post_bytes: build.post_bytes,
            encrypted_ref_hex,
            sent: Some(sent),
            room: false,
            reference: None,
        });
        self.notify();
        Ok(Some(build.encrypted_blob))
    }

    /// Build + sign a reply to a **restricted** post, sealed to that post's
    /// own audience, and return its sealed body for the app to upload — the
    /// reply dialog's twin of [`prepare_gated_blob`](Self::prepare_gated_blob),
    /// with the same fall-through (`ui/feed.md` § Encryption at rest → *Ruling
    /// 5's build — the shape*, (c)):
    ///
    /// - `Ok(Some(blob))` — the target is restricted **and** this device can
    ///   author under its arm ([`ReplyAudience::SealedToRoom`] /
    ///   [`ReplyAudience::SealedToTier`]). The post is staged in the one gated
    ///   slot, marked as a reference; the app uploads `blob` under
    ///   [`gated_upload_sidecar`](Self::gated_upload_sidecar) and finishes with
    ///   [`submit_gated_post`](Self::submit_gated_post), exactly as the
    ///   composer does.
    /// - `Ok(None)` — nothing to upload: the target is public, not loaded, or
    ///   restricted under an arm this reader cannot author under. The app calls
    ///   [`reply`](Self::reply) as it always has, which composes public or
    ///   refuses — the builder's rule, untouched.
    ///
    /// The composer is never read, stamped or cleared: these are not its words.
    pub async fn prepare_sealed_reply(
        &self,
        post_id: String,
        body: String,
    ) -> Result<Option<Vec<u8>>, String> {
        if body.trim().is_empty() {
            return Err("a reply needs a body".to_string());
        }
        self.prepare_sealed_reference(post_id, ReferenceKind::Reply, body)
            .await
    }

    /// The quote twin of [`prepare_sealed_reply`](Self::prepare_sealed_reply).
    /// A **wordless** quote has nothing to seal and stays the public reference
    /// it always was (ruling 3), so an empty `body` answers `Ok(None)`.
    pub async fn prepare_sealed_quote(
        &self,
        post_id: String,
        body: String,
    ) -> Result<Option<Vec<u8>>, String> {
        if body.trim().is_empty() {
            return Ok(None);
        }
        self.prepare_sealed_reference(post_id, ReferenceKind::Quote, body)
            .await
    }

    async fn prepare_sealed_reference(
        &self,
        post_id: String,
        kind: ReferenceKind,
        body: String,
    ) -> Result<Option<Vec<u8>>, String> {
        let me_hex = hex::encode(ActorKeypair::from_secret(self.actor_secret).actor_id().0);
        let (audience, tier, room_hex, refresh_counters) = {
            let s = self.state.read().unwrap();
            let Some(p) = s.find_post(&post_id) else {
                return Ok(None);
            };
            // One slot, and the composer's upload may be holding it: a reply
            // staged over it would make the composer's submit create the reply.
            if s.compose.submitting {
                return Err("a post is still being sent — try again in a moment".to_string());
            }
            (
                reply_audience_of(p, &s.own_rooms, &s.own_tiers, &me_hex),
                p.gated_tier.clone(),
                p.gated_room.clone(),
                matches!(
                    crate::SourceKind::classify(&p.source),
                    crate::SourceKind::Fauna
                ),
            )
        };
        let digest = fauna_core::hex32::decode(&post_id).map_err(|e| e.to_string())?;
        let target = *PostId::from_digest_dag_cbor(digest).as_bytes();
        let kp = ActorKeypair::from_secret(self.actor_secret);
        let seal_id = fauna_client_core::post::mint_seal_id().map_err(|e| e.to_string())?;

        let (build, room) = match audience {
            None | Some(ReplyAudience::PublicByConfirmation) => return Ok(None),
            Some(ReplyAudience::SealedToRoom) => {
                let room = parse_room_id(room_hex.as_deref().unwrap_or_default())?;
                let keys = self
                    .room_post_keys
                    .read()
                    .unwrap()
                    .clone()
                    .ok_or_else(|| "this device holds no room keys".to_string())?;
                // Keyed *now*, through the seam — the tip the room holds at
                // this moment, never a key remembered from the target.
                let (seal, base) = keys.room_post_seal_key(room).await?;
                let build = fauna_client_core::post::build_sealed_referencing_post(
                    &kp,
                    &body,
                    target,
                    kind,
                    fauna_client_core::post::SealedAudience::Room {
                        room,
                        seal,
                        base_key: &base,
                    },
                    seal_id,
                )
                .map_err(|e| e.to_string())?;
                (build, true)
            }
            Some(ReplyAudience::SealedToTier) => {
                let tier = tier.unwrap_or_default();
                let tier_rank = self
                    .state
                    .read()
                    .unwrap()
                    .own_tiers
                    .iter()
                    .find(|t| t.name == tier)
                    .map(|t| t.rank)
                    .ok_or_else(|| format!("unknown tier {tier:?}"))?;
                let period = self.own_period_key(&tier).await?;
                let blob_reply =
                    fauna_client_subscriptions::SubscriptionsClient::new(self.nest.clone())
                        .key_blob_get(kp.actor_id(), &tier)
                        .await
                        .map_err(|e| format!("key_blob.get: {e}"))?;
                let key_blob_ref: [u8; 32] = blob_reply
                    .blob_hash
                    .as_ref()
                    .try_into()
                    .map_err(|_| "key_blob.get returned a non-32-byte hash".to_string())?;
                let period_key: [u8; 32] = period.key.clone().into();
                let build = fauna_client_core::post::build_sealed_referencing_post(
                    &kp,
                    &body,
                    target,
                    kind,
                    fauna_client_core::post::SealedAudience::Tier {
                        tier: &tier,
                        tier_rank,
                        key_blob_ref,
                        period_key: &period_key,
                    },
                    seal_id,
                )
                .map_err(|e| e.to_string())?;
                (build, false)
            }
        };

        *self.pending_gated.write().unwrap() = Some(PendingGatedPost {
            post_bytes: build.post_bytes,
            encrypted_ref_hex: hex::encode(build.encrypted_ref),
            sent: None,
            room,
            reference: Some(PendingReference {
                post_id,
                kind,
                refresh_counters,
            }),
        });
        Ok(Some(build.encrypted_blob))
    }

    /// The **room** arm of [`prepare_gated_blob`](Self::prepare_gated_blob):
    /// build + sign a room-restricted post addressed to `room_hex`
    /// (`compose.gate_room`) and return its sealed full-body blob. The seal is
    /// the Posts row's own (`build_room_post_at`); the base key comes through
    /// the installed room-post seam, because the room's keys live in the
    /// conversations plane (`ui/feed.md` § Encryption at rest →
    /// *Room-restricted — the ruling*). The staged post is uploaded under
    /// [`gated_upload_sidecar`](Self::gated_upload_sidecar) and finished by
    /// the same [`submit_gated_post`](Self::submit_gated_post) as a tier's.
    async fn prepare_room_blob(&self, room_hex: &str) -> Result<Vec<u8>, String> {
        let (text, preview, attached_file, sent) = {
            let s = self.state.read().unwrap();
            (
                s.compose.text.clone(),
                s.compose.gate_preview.clone(),
                s.compose.attached_file.clone(),
                // What this submit sends — staged below, and what
                // `submit_gated_post` clears against once the create confirms.
                s.compose.clone(),
            )
        };
        let fail = |key: &str| {
            self.state.write().unwrap().compose.error = Some(LocalizedText::key(key));
            self.notify();
        };
        if text.trim().is_empty() {
            fail("feed.compose_empty");
            return Err("empty post".into());
        }
        if preview.trim().is_empty() {
            fail("feed.compose_gate_preview_empty");
            return Err("empty gated preview".into());
        }
        self.refuse_unresolved_attachment(attached_file.as_ref())?;
        let room = parse_room_id(room_hex)?;
        let keys = self.compose_room_keys()?;
        let no_key = |e: String| {
            fail("feed.compose_room_no_key");
            e
        };

        // The attachment, if one was sealed for this compose: its seal id AND
        // the seal its base was resolved under, so the body opens with the
        // photo's key. Sealed for another room (or a tier) means the audience
        // moved between the two calls — refuse, as the tier arm does.
        let media = media_item_from_staged(attached_file.as_ref())
            .inspect_err(|_| fail("feed.error_submit"))?;
        let stashed = self.pending_seal.write().unwrap().take();
        let (seal_id, seal, base) = match (&media, stashed) {
            (
                Some(_),
                Some(PendingComposeSeal {
                    seal_id,
                    audience:
                        SealAudience::Room {
                            room: sealed_for,
                            seal,
                        },
                }),
            ) if sealed_for == room => {
                let base = keys.room_post_base_key(room, seal).await.map_err(no_key)?;
                (seal_id, seal, base)
            }
            (Some(_), _) => {
                fail("feed.compose_attachment_stale");
                return Err("staged attachment was not sealed for this room — re-attach it".into());
            }
            (None, _) => {
                let (seal, base) = keys.room_post_seal_key(room).await.map_err(no_key)?;
                let seal_id = fauna_client_core::post::mint_seal_id().map_err(|e| e.to_string())?;
                (seal_id, seal, base)
            }
        };
        let full_body = match media {
            Some(item) => fauna_core::data::PostBody::TextWithMedia {
                content: text.clone(),
                facets: vec![],
                items: vec![item],
            },
            None => fauna_core::data::PostBody::Text {
                content: text.clone(),
                facets: vec![],
            },
        };
        let keypair = ActorKeypair::from_secret(self.actor_secret);
        let build = fauna_client_core::post::build_room_post_at(
            &keypair,
            &preview,
            full_body,
            room,
            seal,
            &base,
            seal_id,
            &fauna_client_core::post::PostAuthoring::now(),
        )
        .map_err(|e| e.to_string())?;

        {
            let mut s = self.state.write().unwrap();
            s.compose.submitting = true;
            s.compose.error = None;
        }
        *self.pending_gated.write().unwrap() = Some(PendingGatedPost {
            post_bytes: build.post_bytes,
            encrypted_ref_hex: hex::encode(build.encrypted_ref),
            sent: Some(sent),
            room: true,
            reference: None,
        });
        self.notify();
        Ok(build.encrypted_blob)
    }

    /// **Phase one of "Sell this post…"** — mint the unlock tier and persist
    /// its period key, without creating anything server-side.
    ///
    /// Call this **only when the compose carries an attachment**, and call it
    /// before [`seal_compose_attachment`](Self::seal_compose_attachment). A
    /// sold post's photo must seal under the tier the sale mints, and that tier
    /// does not exist when the author picks the file — so the mint is split in
    /// two and this half runs first. With no attachment there is nothing to
    /// seal and nothing to order:
    /// [`prepare_sell_post`](Self::prepare_sell_post) runs this itself, so the
    /// one-call flow every app already has keeps working untouched.
    ///
    /// The full sequence, for an app that has an attachment:
    ///
    /// 1. `update_compose_sell` — stage the sale's fields.
    /// 2. **`stage_sell_tier`** — mint + persist the period key.
    /// 3. `seal_compose_attachment` → POST the parts → `update_compose` with
    ///    the returned hash and the seal's own `media_type`.
    /// 4. `prepare_sell_post` → upload the body blob → `submit_gated_post`.
    ///
    /// Every validation runs **here**, ahead of the mint, exactly as it did
    /// when this was one call: a tier minted for a compose that never becomes a
    /// post is litter in the author's tier list. What the split does widen is
    /// the window in which an *abandoned* compose leaves a persisted period key
    /// with no tier behind it — from one build to one blob upload. That residue
    /// is invisible (`fauna.state.subscriptions` custody only; `commit_tier` never ran, so no
    /// tier exists to filter), and it is the price of sealing a photo under a
    /// key that must exist before the photo is uploaded.
    ///
    /// A second call while a stage is live is a no-op — re-minting would
    /// abandon the key the already-sealed photo used. Any
    /// `update_compose_sell` / `update_compose_gate` drops the stage, which is
    /// what makes an edited sale refuse in phase two rather than publish an
    /// unopenable photo.
    pub async fn stage_sell_tier(
        &self,
        subscribers_get_it_free: bool,
        asking_price_sats: Option<u64>,
    ) -> Result<(), String>
    where
        R::Error: fauna_protocol::RpcErrorClass,
    {
        if self.pending_sell_tier.read().unwrap().is_some() {
            return Ok(());
        }

        let (text, preview) = {
            let s = self.state.read().unwrap();
            (s.compose.text.clone(), s.compose.gate_preview.clone())
        };

        let fail = |key: &str| {
            let mut s = self.state.write().unwrap();
            s.compose.error = Some(LocalizedText::key(key));
        };

        // Validate BEFORE minting anything: a tier minted for a compose that
        // never becomes a post is pure litter in the author's tier list.
        if text.trim().is_empty() {
            fail("feed.compose_empty");
            self.notify();
            return Err("empty post".into());
        }
        if preview.trim().is_empty() {
            fail("feed.compose_gate_preview_empty");
            self.notify();
            return Err("empty gated preview".into());
        }
        // Converted before anything is minted, for the same reason the two
        // emptiness checks above run here. The value itself is re-derived in
        // phase two, which is what actually commits it; this call exists to
        // refuse an unconvertible amount while refusing is still free.
        if let Some(sats) = asking_price_sats
            && fauna_protocol::subscriptions::TierAskingPrice::from_sats(sats).is_none()
        {
            fail("feed.compose_sell_price_invalid");
            self.notify();
            return Err("asking price too large".into());
        }

        // Rank from the single ratified toggle (`monetization.md:126`). The
        // subscribers-free arm is the CONSTANT rank 1 — no read involved. The
        // pay-per-view arm derives "above the author's highest regular tier"
        // from a fresh, fallible tiers read and FAILS CLOSED on a failed one
        // — never through `refresh_own_tiers`, which swallows its transport
        // error: reaching a `max().unwrap_or(0) + 1` fallback over stale or
        // empty state would mint at rank 1, "subscribers get it free", the
        // very arm the author declined (monetization.md § Implementation
        // status (2d), obligation (iii)). Designated tiers are excluded so a
        // prior unlock tier never pushes the next one's rank up.
        let rank = if subscribers_get_it_free {
            1
        } else {
            let subs = fauna_client_subscriptions::SubscriptionsClient::new(self.nest.clone());
            let tiers = subs.tiers_list().await.map_err(|e| {
                fail("feed.compose_sell_rank_unavailable");
                self.notify();
                format!("read own tiers for pay-per-view rank: {e}")
            })?;
            tiers
                .iter()
                .filter(|t| t.unlocks_post.is_none())
                .map(|t| t.rank)
                .max()
                .unwrap_or(0)
                + 1
        };

        let author = self.subscriptions_author().inspect_err(|_| {
            fail("feed.compose_gate_no_key");
            self.notify();
        })?;
        let tier = fauna_client_subscriptions::mint_unlock_tier_name();
        let staged = author.stage_tier(&tier).await.map_err(|e| {
            fail("feed.compose_gate_no_key");
            self.notify();
            format!("stage unlock tier: {e}")
        })?;

        *self.pending_sell_tier.write().unwrap() = Some(PendingSellTier { tier, rank, staged });
        Ok(())
    }

    /// **Sell this post**: auto-mint a degenerate single-post subscription tier
    /// and gate the staged composer text to it, returning the sealed full-body
    /// blob for the client to upload — exactly like
    /// [`prepare_gated_blob`](Self::prepare_gated_blob), which is why
    /// [`submit_gated_post`](Self::submit_gated_post) and
    /// [`abort_gated_submit`](Self::abort_gated_submit) finish **both** flows
    /// and no app needs new upload glue (priorities #1/#2).
    ///
    /// `monetization.md` § Per-post pay-to-unlock. The composer supplies the
    /// sold text (`update_compose`) and the public teaser
    /// (`update_compose_gate`'s preview — the gate *tier* select is unused here,
    /// since this flow mints its own tier).
    ///
    /// `subscribers_get_it_free` is the ratified single knob
    /// (`monetization.md:126`): `true` mints at rank 1, inside every paid
    /// subscription; `false` mints above the author's highest regular tier, i.e.
    /// pure pay-per-view. Nothing else in the model changes.
    ///
    /// **The step order is forced and not rearrangeable.** The tier must exist
    /// before the post is submitted, and the post id must be known before the
    /// tier is created — satisfiable only because both content addresses are
    /// derived locally:
    ///
    /// 1. mint + persist the period key and build the birth `KeyBlob`
    ///    ([`stage_tier`](fauna_client_subscriptions::orchestration::SubscriptionsAuthor::stage_tier)),
    /// 2. build the gated body, which names the tier and its birth blob,
    /// 3. `post_id = blake3(body)` — the nest's own derivation,
    /// 4. `tiers.create` carrying `unlocks_post = post_id` (create-time
    ///    immutable — this is the only call that can ever set it),
    /// 5. the caller uploads the returned blob, then `submit_gated_post`
    ///    creates the post.
    ///
    /// A failure before step 4 mints nothing. A failure *after* step 4 leaves a
    /// designated tier whose post was never created — harmless and invisible by
    /// design (`monetization.md:131`: the tier outlives the post; every generic
    /// surface filters designated tiers out), and the staged post survives in
    /// `pending_gated` so a retry of the upload+submit reuses that same tier.
    /// `asking_price_sats` is the **machine-comparable** price
    /// (`monetization.md` § The asking price), distinct from `price_hint`'s
    /// human string beside it: the hint is what a reader sees, this is what an
    /// inferring mechanism compares a zap against. `None` — the state every
    /// shipped app sends until its price input lands — leaves the post
    /// purchasable only through the explicit-intent mechanisms (claim code,
    /// provider webhook), and a zap on it stays a tip. That is the ratified
    /// permanent behavior for an unpriced tier, so no app is broken by not
    /// sending one.
    ///
    /// Sats in, msats on the wire, converted once by
    /// [`fauna_protocol::subscriptions::TierAskingPrice::from_sats`]; an amount too
    /// large to convert is refused here rather than clamped.
    pub async fn prepare_sell_post(
        &self,
        price_hint: Option<String>,
        subscribers_get_it_free: bool,
        asking_price_sats: Option<u64>,
    ) -> Result<Vec<u8>, String>
    where
        R::Error: fauna_protocol::RpcErrorClass,
    {
        let fail = |key: &str| {
            let mut s = self.state.write().unwrap();
            s.compose.error = Some(LocalizedText::key(key));
        };

        // Before phase one mints anything: a restored handle with no blob
        // behind it refuses HERE, so no tier is minted for a compose that
        // never becomes a post. It cannot live in `stage_sell_tier`: an app
        // holding the bytes calls that BEFORE it uploads, while its staged
        // file legitimately has no hash yet. By the time this runs, every app
        // has uploaded what it holds — so a hash still missing is a file this
        // device never had.
        {
            let file = self.state.read().unwrap().compose.attached_file.clone();
            self.refuse_unresolved_attachment(file.as_ref())?;
        }

        // Phase one, run inline when the app did not need it. An app with an
        // attachment must call `stage_sell_tier` itself (the photo has to seal
        // under the tier before it can be uploaded); an app without one never
        // does, so its single call still mints and commits in one go. Either
        // way exactly ONE tier is staged — this is a take, not a peek, so a
        // retry after an upload failure reuses `pending_gated` rather than
        // minting again.
        if self.pending_sell_tier.read().unwrap().is_none() {
            self.stage_sell_tier(subscribers_get_it_free, asking_price_sats)
                .await?;
        }
        let PendingSellTier { tier, rank, staged } = self
            .pending_sell_tier
            .write()
            .unwrap()
            .take()
            .ok_or_else(|| "no staged unlock tier".to_string())?;

        let (text, preview, attached_file, sent) = {
            let s = self.state.read().unwrap();
            (
                s.compose.text.clone(),
                s.compose.gate_preview.clone(),
                s.compose.attached_file.clone(),
                // What this submit sends — see `prepare_gated_blob`'s twin.
                s.compose.clone(),
            )
        };
        // Re-converted from the caller's argument rather than carried on the
        // stage: this is the value that actually commits, and `from_sats` is
        // pure. Phase one already refused an unconvertible one before minting.
        let asking_price = match asking_price_sats {
            Some(sats) => match fauna_protocol::subscriptions::TierAskingPrice::from_sats(sats) {
                Some(price) => Some(price),
                None => {
                    fail("feed.compose_sell_price_invalid");
                    self.notify();
                    return Err("asking price too large".into());
                }
            },
            None => None,
        };

        let keypair = ActorKeypair::from_secret(self.actor_secret);
        let author = self.subscriptions_author().inspect_err(|_| {
            fail("feed.compose_gate_no_key");
            self.notify();
        })?;

        // The attachment, if the app sealed + uploaded one against the staged
        // tier — the same reconciliation `prepare_gated_blob` performs, and for
        // the same reason: the body must reuse the exact `seal_id` the photo
        // sealed under, or one key would open the body and none of its items
        // (`ui/media.md` § Encryption at rest). A stash minted against a
        // different tier means the sale was edited between the two calls, which
        // dropped this stage and minted a new one; refuse rather than ship a
        // post whose photo nobody can open.
        let media = media_item_from_staged(attached_file.as_ref()).inspect_err(|_| {
            fail("feed.error_submit");
            self.notify();
        })?;
        let stashed = self.pending_seal.write().unwrap().take();
        let seal_id = match (&media, stashed) {
            (Some(_), Some(seal)) if seal.for_tier(&tier) => seal.seal_id,
            (Some(_), _) => {
                fail("feed.compose_attachment_stale");
                self.notify();
                return Err(format!(
                    "staged attachment was not sealed for tier {tier:?} — re-attach it"
                ));
            }
            (None, _) => fauna_client_core::post::mint_seal_id().map_err(|e| e.to_string())?,
        };
        let full_body = match media {
            Some(item) => fauna_core::data::PostBody::TextWithMedia {
                content: text.clone(),
                facets: vec![],
                items: vec![item],
            },
            None => fauna_core::data::PostBody::Text {
                content: text.clone(),
                facets: vec![],
            },
        };

        let build = fauna_client_core::post::build_gated_post_at(
            &keypair,
            &preview,
            full_body,
            &tier,
            rank,
            staged.key_blob_ref,
            &staged.period_key,
            seal_id,
            &fauna_client_core::post::PostAuthoring::now(),
        )
        .map_err(|e| e.to_string())?;

        // The nest derives a post's id from the exact wire bytes it is handed,
        // NOT the decoded value, so hash the signed envelope bytes that
        // `posts.create` will carry.
        let post_id = wire_post_id(&build.post_bytes);

        author
            .commit_tier(
                &tier,
                rank,
                None,
                price_hint,
                None,
                // NOT auto-approve: a sold post must not grant to anyone who
                // merely asks. A *paid* entitlement still drains, because
                // `drain_auto_approvals` approves `payment_entitled` requests on
                // non-auto tiers too (`monetization.md` § Per-post
                // pay-to-unlock — the unchanged waist).
                false,
                staged,
                Some(post_id),
                asking_price,
                // an unlock tier is offered (the buyer finds it through the post's teaser)
                false,
            )
            .await
            .map_err(|e| {
                fail("feed.error_submit");
                self.notify();
                format!("create unlock tier: {e}")
            })?;

        let encrypted_ref_hex = hex::encode(build.encrypted_ref);
        {
            let mut s = self.state.write().unwrap();
            s.compose.submitting = true;
            s.compose.error = None;
        }
        *self.pending_gated.write().unwrap() = Some(PendingGatedPost {
            post_bytes: build.post_bytes,
            encrypted_ref_hex,
            sent: Some(sent),
            room: false,
            reference: None,
        });
        self.notify();
        Ok(build.encrypted_blob)
    }

    /// Abort a staged gated submit whose **blob upload failed** (the platform
    /// glue between [`prepare_gated_blob`](Self::prepare_gated_blob) and
    /// [`submit_gated_post`](Self::submit_gated_post)): drop the staged post,
    /// clear `submitting`, and surface the upload error on `compose-error` —
    /// the composer keeps its text for a manual retry.
    ///
    /// A staged **sealed reply or quote** is dropped and nothing else: its
    /// failure is the reply dialog's to show (the app already holds the
    /// message), and the composer — whose words these never were — is not
    /// stamped with it.
    pub fn abort_gated_submit(&self, message: String) {
        let staged = self.pending_gated.write().unwrap().take();
        if staged.is_some_and(|p| p.reference.is_some()) {
            return;
        }
        {
            let mut s = self.state.write().unwrap();
            s.compose.submitting = false;
            s.compose.error = Some(LocalizedText::key_arg(
                "feed.error_submit",
                "message",
                message,
            ));
        }
        self.notify();
    }

    /// Create the gated post staged by
    /// [`prepare_gated_blob`](Self::prepare_gated_blob), after the client
    /// uploaded the sealed blob. `uploaded_hash` is the upload reply's hex
    /// hash — it must echo the staged post's `encrypted_ref` (the nest blob
    /// store is content-addressed; a mismatch means the upload glue mangled
    /// the bytes and the post would reference a missing blob).
    pub async fn submit_gated_post(&self, uploaded_hash: String) -> Result<(), String> {
        let pending = self
            .pending_gated
            .write()
            .unwrap()
            .take()
            .ok_or_else(|| "no staged gated post".to_string())?;
        let result = if !uploaded_hash.eq_ignore_ascii_case(&pending.encrypted_ref_hex) {
            Err(format!(
                "uploaded blob hash {uploaded_hash} != staged encrypted_ref {}",
                pending.encrypted_ref_hex
            ))
        } else {
            match PostsClient::new(self.nest.clone())
                .posts_create(pending.post_bytes.clone())
                .await
            {
                Ok(reply) => {
                    // Same trickle chokepoint as `submit_post` — a gated post's
                    // public teaser text is what `body_text` yields here, which
                    // is exactly what the nest's own enumeration would stage.
                    self.observe_own_post_created(&reply.post_id, &pending.post_bytes);
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        };

        // A sealed reply or quote is `compose_referencing_post`'s shape, not
        // the composer's: the target's counters are read back (best-effort —
        // the post exists either way), the timeline is not re-ranked under the
        // user's finger, and the composer — whose words these never were — is
        // neither cleared on success nor stamped on failure.
        if let Some(reference) = &pending.reference {
            result?;
            if reference.refresh_counters {
                let _ = self
                    .interact(
                        reference.post_id.clone(),
                        reference.kind.action().to_string(),
                        None,
                    )
                    .await;
            }
            return Ok(());
        }

        match result {
            Ok(()) => {
                if let Some(sent) = &pending.sent {
                    let mut s = self.state.write().unwrap();
                    s.compose.clear_sent(sent);
                }
                self.reload().await;
                Ok(())
            }
            Err(e) => {
                {
                    let mut s = self.state.write().unwrap();
                    s.compose.submitting = false;
                    s.compose.error = Some(LocalizedText::key_arg(
                        "feed.error_submit",
                        "message",
                        e.clone(),
                    ));
                }
                self.notify();
                Err(e)
            }
        }
    }

    /// Whether `author_hex` is this manager's own actor — the one place the
    /// "is this mine?" comparison is spelled, so the three call sites that turn
    /// on it (custody unseal, the buyer's price read, the buy mutation) cannot
    /// drift apart. Hex, because that is the form the feed projection carries
    /// authors in.
    fn is_local_actor(&self, author_hex: &str) -> bool {
        hex::encode(ActorKeypair::from_secret(self.actor_secret).actor_id().0) == author_hex
    }

    /// Resolve a loaded gated post's sealed-blob hash (hex `encrypted_ref`)
    /// for the client to fetch (`GET /api/v1/blob/{hash}` — platform glue),
    /// caching the decoded `GatedInfo` for
    /// [`unlock_gated_post`](Self::unlock_gated_post). The `resolve_media`
    /// pattern: one lazy `fauna.posts.get` + decode per post. `None` when the
    /// post isn't loaded, isn't gated, or can't be decoded.
    pub async fn gated_blob_hash(&self, post_id: String) -> Option<String> {
        {
            let cache = self.resolved_gated.read().unwrap();
            if let Some(r) = cache.get(&post_id) {
                return Some(hex::encode(r.gated.encrypted_ref.digest()));
            }
        }
        let loaded = {
            let s = self.state.read().unwrap();
            s.rendered_posts()
                .any(|p| p.post_id == post_id && p.gated_tier.is_some())
        };
        if !loaded {
            return None;
        }
        let posts = PostsClient::new(self.nest.clone());
        let reply = posts.posts_get(post_id.clone()).await.ok()?;
        let (post, _origin) = decode_post(reply.body.as_ref()).ok()?;
        let gated = post.gated?;
        let hash_hex = hex::encode(gated.encrypted_ref.digest());
        self.resolved_gated.write().unwrap().insert(
            post_id,
            ResolvedGated {
                author_hex: hex::encode(post.author.0),
                gated,
            },
        );
        Some(hash_hex)
    }

    /// Decrypt a gated post's full body from its fetched sealed blob and swap
    /// it into the snapshot (`body` + rebuilt `document`, `gated_unlocked`),
    /// then notify — the reader-side leg of `ui/feed.md` § Encryption at
    /// rest. Which base key opens it is the post's own `key_access` arm:
    ///
    /// - **`Broadcast`** (audience-restricted): the period key comes from
    ///   custody when the local actor **is** the author (current period first,
    ///   then rotated-out priors), else from the reader's own wrap entry in the
    ///   tier's live KeyBlob (`key_blob.get` → `decrypt_key_blob_entry_for`). A
    ///   post sealed under a rotated-out period the KeyBlob no longer carries
    ///   stays locked (best-effort backfill — the archival path is a
    ///   follow-on).
    /// - **`Room`** (room-restricted): the base comes from the room-post key
    ///   seam — the conversations plane's MLS epoch secret or generation wrap
    ///   ([`set_room_post_keys`](Self::set_room_post_keys)). Never custody or a
    ///   KeyBlob: a room post's readers are the room's floor, and the author is
    ///   one of them, so the author opens it the same way every member does.
    /// - **`Unknown`**: an arm this build cannot open. Stays locked.
    pub async fn unlock_gated_post(
        &self,
        post_id: String,
        blob_bytes: Vec<u8>,
    ) -> Result<(), String> {
        use fauna_core::subscription::crypto::{
            decrypt_content, decrypt_key_blob_entry_for, derive_post_key,
        };
        use fauna_core::subscription::types::KeyAccess;

        let (author_hex, tier, seal_id, key_access) = {
            let cache = self.resolved_gated.read().unwrap();
            let r = cache.get(&post_id).ok_or_else(|| {
                "gated post not resolved (call gated_blob_hash first)".to_string()
            })?;
            let seal_id = r.gated.seal_id;
            (
                r.author_hex.clone(),
                r.gated.tier.clone(),
                seal_id,
                r.gated.key_access.clone(),
            )
        };

        let keypair = ActorKeypair::from_secret(self.actor_secret);
        let is_author = self.is_local_actor(&author_hex);

        // Candidate base keys, most-likely first.
        let mut candidates: Vec<zeroize::Zeroizing<[u8; 32]>> = Vec::new();
        match &key_access {
            KeyAccess::Broadcast { .. } if is_author => {
                let custody = self.period_key_custody().await?;
                if let Some(keys) = custody.tiers.iter().find(|t| t.tier_name == tier) {
                    let current: [u8; 32] = keys.current.key.clone().into();
                    candidates.push(zeroize::Zeroizing::new(current));
                    for prior in &keys.prior {
                        let prior: [u8; 32] = prior.key.clone().into();
                        candidates.push(zeroize::Zeroizing::new(prior));
                    }
                }
            }
            KeyAccess::Broadcast { .. } => {
                let author_id = fauna_core::identity::ActorId(
                    hex::decode(&author_hex)
                        .ok()
                        .and_then(|b| <[u8; 32]>::try_from(b).ok())
                        .ok_or_else(|| "bad author id".to_string())?,
                );
                let subs = fauna_client_subscriptions::SubscriptionsClient::new(self.nest.clone());
                let reply = subs
                    .key_blob_get(author_id, &tier)
                    .await
                    .map_err(|e| format!("key_blob.get: {e}"))?;
                // Stored blob bytes are the dag-cbor EmbedAsBytes wire shape;
                // the inner canonical bytes decode via sign-over-CID.
                let wire: fauna_core::encoding::EmbedAsBytes =
                    fauna_core::encoding::canonical_decode(reply.blob_data.as_ref())
                        .map_err(|e| format!("decode key blob wire: {e}"))?;
                let blob: fauna_core::subscription::types::KeyBlob =
                    fauna_core::encoding::decode_signed_bytes(&wire.bytes)
                        .map_err(|e| format!("decode key blob: {e}"))?;
                let me = keypair.actor_id();
                let entry = blob
                    .entries
                    .iter()
                    .find(|e| e.subscriber == me)
                    .ok_or_else(|| "no wrap entry for this reader in the KeyBlob".to_string())?;
                candidates.push(zeroize::Zeroizing::new(
                    decrypt_key_blob_entry_for(&keypair, entry)
                        .map_err(|e| format!("unwrap period key: {e}"))?,
                ));
            }
            KeyAccess::Room { .. } => {
                let (room, seal) = fauna_core::room_post::room_post_of(&key_access)
                    .ok_or_else(|| "this post's room arm names no room".to_string())?;
                // Clone the seam out so no lock is held across its await.
                let keys = self
                    .room_post_keys
                    .read()
                    .unwrap()
                    .clone()
                    .ok_or_else(|| "this device holds no room keys".to_string())?;
                candidates.push(keys.room_post_base_key(room, seal).await?);
            }
            KeyAccess::Unknown { kind, .. } => {
                return Err(format!(
                    "this post is sealed a way this app does not know ({kind})"
                ));
            }
        }

        // Keep the per-post key that actually opened the body, not just the
        // plaintext: the post's media items are sealed under that same key
        // (`media.md` § Encryption at rest — one per-post key, random nonces per
        // blob), so this is the only moment the reader holds what opens them.
        let (per_post_key, plain) = candidates
            .iter()
            .find_map(|base_key| {
                let key = zeroize::Zeroizing::new(derive_post_key(base_key, &seal_id));
                decrypt_content(&key, &blob_bytes).ok().map(|p| (key, p))
            })
            .ok_or_else(|| "no held key opens this post".to_string())?;

        let body: fauna_core::data::PostBody = fauna_core::encoding::canonical_decode(&plain)
            .map_err(|e| format!("decode full body: {e}"))?;
        // The accepted variants are unchanged; what changed is that the body
        // survives the check instead of being reduced to its text here — its
        // `items` are the post's media, and dropping them was why a gated photo
        // post rendered its caption and never its photos.
        match &body {
            fauna_core::data::PostBody::Text { .. }
            | fauna_core::data::PostBody::TextWithMedia { .. } => {}
            other => return Err(format!("unsupported gated body variant: {other:?}")),
        }

        // Register what opens this post's attachments before anything renders
        // them, keyed by the handle a renderer holds (the blob hash in
        // `RenderBlock::Image`). Thumbnails too: a thumbnail is a separate blob
        // sealed under the same audience as its parent (`media.md` § What's in
        // the floor), so an app that resolves `?thumb=1` needs the same key.
        {
            let mut keys = self.sealed_media_keys.write().unwrap();
            for item in body.media_items() {
                keys.insert(hex::encode(item.blob_hash.digest()), per_post_key.clone());
                if let Some(thumb) = &item.thumbnail {
                    keys.insert(hex::encode(thumb.digest()), per_post_key.clone());
                }
            }
        }

        // A room post's verdicts (`ui/feed.md` § Encryption at rest →
        // *Room-restricted — the ruling*, ruling 7): the room's home nest ran
        // the room's named labelers when it indexed the post, and serves the
        // result only to a live floor member, through a read of its own —
        // every envelope read carries none. Asked at unlock because the
        // unlock is the act that proves this reader holds the room's key; the
        // nest gates on the floor regardless. Best effort, the `tips` rule:
        // a refusal, a transport error, and a reader off
        // the floor all leave the card's own labels exactly as they were.
        if let Some((room, _)) = fauna_core::room_post::room_post_of(&key_access) {
            // The kind pick lives HERE, once (the conversations plane's
            // `read_roster` precedent): a room homed on another nest rides the
            // distinct relay kind `posts.room_labels_remote`, which this
            // reader's own nest originates on to the room's home; a same-nest
            // room stays on the plain read. Both answer the same reply,
            // because the relay forwards the room home's answer unchanged.
            //
            // The home has to be asked, not this nest: the verdicts were
            // derived by the room home's reception pass, which indexes only
            // the posts that nest stores, so a member's own nest resolves no
            // room for the post and answers empty — indistinguishable from
            // "nobody labelled it".
            // Clone the seam out so no lock is held across its await.
            let seam = self.room_post_keys.read().unwrap().clone();
            let home = match seam {
                Some(keys) => keys.room_home_nest_url(room).await,
                None => None,
            };
            let client = PostsClient::new(self.nest.clone());
            let read = match home {
                Some(url) => {
                    client
                        .posts_room_labels_remote(hex::encode(room), vec![post_id.clone()], url)
                        .await
                }
                None => client.posts_room_labels(vec![post_id.clone()]).await,
            };
            let served = read
                .ok()
                .and_then(|reply| reply.posts.into_iter().find(|e| e.post_id == post_id))
                .map(|e| e.labels)
                .unwrap_or_default();
            if !served.is_empty() {
                self.room_post_labels
                    .write()
                    .unwrap()
                    .insert(post_id.clone(), served);
            }
        }

        // Cache the decrypted body first, so it survives even if the post is
        // momentarily absent from `s.posts` here, and — the load-bearing part —
        // so a later `reload`/`load_more` re-folds it via `reapply_unlocked`
        // instead of reverting this reader to the sealed teaser.
        self.unlocked_bodies
            .write()
            .unwrap()
            .insert(post_id.clone(), body.clone());

        let changed = {
            let mut s = self.state.write().unwrap();
            match rendered_posts_mut(&mut s).find(|p| p.post_id == post_id) {
                Some(p) => {
                    let quote = match &p.quoted_post_id {
                        Some(q) => self.resolved_quotes.read().unwrap().get(q).cloned(),
                        None => None,
                    };
                    fold_unlocked_body(p, &body, quote.as_ref());
                    self.fold_room_post_labels(p);
                    true
                }
                None => false,
            }
        };
        if changed {
            self.notify();
        }
        Ok(())
    }

    /// Re-fold this reader's already-unsealed gated bodies
    /// ([`unlocked_bodies`](Self::unlocked_bodies)) onto a freshly-fetched post
    /// list — which always arrives **sealed** — so an unlock survives a feed
    /// `reload` / `load_more` instead of snapping back to the teaser. The
    /// `resolved_quotes`/`resolved_media` re-fold pattern; a no-op (one map read)
    /// when the reader has unlocked nothing. Caller holds the `state` write lock;
    /// the inner `resolved_quotes` read keeps the same lock order as
    /// [`unlock_gated_post`](Self::unlock_gated_post) (state → resolved_quotes).
    fn reapply_unlocked(&self, posts: &mut [PostSummary]) {
        let cache = self.unlocked_bodies.read().unwrap();
        if cache.is_empty() {
            return;
        }
        for p in posts.iter_mut() {
            if p.gated_unlocked {
                continue;
            }
            if let Some(full) = cache.get(&p.post_id) {
                let quote = match &p.quoted_post_id {
                    Some(q) => self.resolved_quotes.read().unwrap().get(q).cloned(),
                    None => None,
                };
                // The same fold as the unlock, from the same cached body — so a
                // reload restores the reader's photos and not merely their text.
                fold_unlocked_body(p, full, quote.as_ref());
                self.fold_room_post_labels(p);
            }
        }
    }

    /// Merge the verdicts the room's nest served for `p`
    /// ([`room_post_labels`](Self::room_post_labels)) into the card's own
    /// labels, a server entry winning its category
    /// (`fauna_core::content_category::merge_server_labels`) — the rule the
    /// conversations manager applies to a room message's, so the same badge
    /// paints both. Idempotent, so the unlock and every re-fold after it
    /// agree; a no-op for a post the nest served nothing for.
    fn fold_room_post_labels(&self, p: &mut PostSummary) {
        let cache = self.room_post_labels.read().unwrap();
        if let Some(served) = cache.get(&p.post_id) {
            p.labels = fauna_core::content_category::merge_server_labels(served, &p.labels);
        }
    }

    /// Open a post-media blob an app fetched by hash, for rendering.
    ///
    /// Public-post media is plaintext on the wire, so the fetched bytes *are*
    /// the image and this hands them straight back. A gated post's attachments
    /// are AEAD-sealed under the same per-post key its body opened under, so
    /// they must be opened before any app can decode them
    /// (`docs/goal/ui/media.md` § Encryption at rest — "recipients who can
    /// decrypt the body can decrypt the attachments by construction";
    /// `docs/goal/ui/feed.md` § Encryption at rest — "unsealing at render time
    /// on the reader's client").
    ///
    /// **One call on every app's image path**, so the seven post cards keep one
    /// shape and no app has to know which posts are gated: pass whatever the
    /// by-hash blob GET returned and decode what comes back. `None` means this
    /// blob IS a sealed item of an unlocked post and did not open — the app
    /// degrades to its existing placeholder, exactly as it does for bytes it
    /// cannot decode.
    ///
    /// The manager does no fetching here, deliberately: it is WS-RPC-only, and
    /// bulk blob bytes are the platform-glue carve-out each app already owns
    /// (the same division as [`unlock_gated_post`](Self::unlock_gated_post),
    /// which likewise takes bytes the app fetched).
    pub fn open_media_bytes(&self, blob_hash: &str, fetched: Vec<u8>) -> Option<Vec<u8>> {
        let key = self
            .sealed_media_keys
            .read()
            .unwrap()
            .get(blob_hash)
            .cloned();
        match key {
            None => Some(fetched),
            Some(key) => fauna_core::subscription::crypto::decrypt_content(&key, &fetched).ok(),
        }
    }

    /// Whether this blob is a sealed item of a post whose body this reader has
    /// opened — i.e. whether it needs [`open_media_bytes`](Self::open_media_bytes)
    /// before it is an image.
    ///
    /// **Only an app whose image path is a URL needs to ask.** The apps that hold
    /// the fetched bytes (tui, linux, windows, android) route every hash through
    /// `open_media_bytes` unconditionally and never call this — that is what keeps
    /// their post cards free of an is-this-post-gated branch. But web's `<img src>`
    /// and apple's `AsyncImage(url:)` do the GET *and* the decode natively, so no
    /// app code ever sees the bytes: those two must decide, before they render,
    /// whether this hash can be served as a plain URL (the overwhelmingly common
    /// case, and the only one that can use the nest's `?thumb=1` smaller blob —
    /// which a sealed item has no server-openable equivalent of) or must instead be
    /// fetched and opened into a local object URL / in-memory image.
    ///
    /// Answering `false` for an unregistered hash is the same passthrough rule
    /// `open_media_bytes` applies, stated as a predicate: a public post's media, an
    /// avatar, a link-preview image, and a still-sealed post's item all take the URL
    /// path — the last because a reader with no key cannot render it either way, and
    /// its card paints the placeholder until a detail-open unlock registers it.
    ///
    /// The branch this enables belongs in the app's *page*, never its post card:
    /// the card takes a resolved URL either way (`docs/goal/ui/feed.md`
    /// § Encryption at rest — unsealing happens at render time on the reader's
    /// client).
    pub fn is_sealed_media(&self, blob_hash: &str) -> bool {
        self.sealed_media_keys
            .read()
            .unwrap()
            .contains_key(blob_hash)
    }

    /// What a media block plays from when the reader taps its `video-thumbnail`
    /// (`render-model.md` § D6c → *Inline playback*, answer 1) — the one shared
    /// decision behind every app's native player. A `Video` whose item this reader
    /// unlocked is [`PlaybackSource::Sealed`] (the [`is_sealed_media`] branch web's
    /// images already take: no URL can play it); any other `Video` is the
    /// unauthenticated blob route, nest-relative; every other block is
    /// [`PlaybackSource::Unplayable`].
    ///
    /// Async because the proxied arm mints a `fauna.media.playback_ticket` — the
    /// faces are stable across that arm's landing. A bridged video now folds to a
    /// `ProxiedVideo` block, but its ticket arm is not built yet:
    /// until it lands such a block falls to `Unplayable` here.
    ///
    /// [`is_sealed_media`]: Self::is_sealed_media
    pub async fn playback_source(&self, block: &RenderBlock) -> PlaybackSource {
        match block {
            RenderBlock::Video { hash, .. } if self.is_sealed_media(hash) => {
                PlaybackSource::Sealed { hash: hash.clone() }
            }
            RenderBlock::Video { hash, .. } => PlaybackSource::Url {
                url: format!("/api/v1/blob/{hash}"),
            },
            _ => PlaybackSource::Unplayable {
                reason: "not a video block".into(),
            },
        }
    }

    // ── Feed CRUD ────────────────────────────────────────────────

    /// Create a feed (`create_feed` submit). Encodes each `(type, value,
    /// required)` rule via the shared `encode_filter_rule` into the typed wire
    /// `rules` list, splits `factors` (the `feed-factor-*` editor's
    /// entries — content-moderation-and-ranking.md § Composition) into this
    /// feed's own `composition` vs. the caller's global factor set (merged
    /// into `fauna.feed.factors.set`, never a blind overwrite — see
    /// [`apply_global_factors`]), calls `fauna.feed.create`, then refreshes
    /// the feed list. Returns the new `feed_id`. A global-factor-set write
    /// failure surfaces as `Err` **after** the feed list has already been
    /// refreshed (the feed itself was created; only the global toggle
    /// failed), so the caller's list stays consistent even on that partial
    /// failure A global write that lands re-queries the current
    /// selection, whose order it just changed.
    pub async fn create_feed(
        &self,
        name: String,
        rules: Vec<FilterRuleInput>,
        combination: String,
        scope: Option<String>,
        contributor_seeds: Option<Vec<String>>,
        factors: Vec<FactorWeightInput>,
    ) -> Result<String, String> {
        let rules = encode_rules(&rules)?;
        let (composition, global_updates) = split_factors(factors)?;
        let feed = FeedClient::new(self.nest.clone());
        let reply = feed
            .feed_create(
                name,
                rules,
                combination,
                scope,
                contributor_seeds,
                composition,
            )
            .await
            .map_err(|e| e.to_string())?;
        let global_changed = !global_updates.is_empty();
        let global_result = if global_changed {
            apply_global_factors(&feed, global_updates).await
        } else {
            Ok(())
        };
        self.refresh_feeds().await;
        // The global set recomposes every feed the nest serves — the page on
        // screen included (Trending composes it too, `trending.md` § The
        // Trending feed) — so re-query the current selection rather than keep
        // painting the order it had before the write.
        if global_changed && global_result.is_ok() {
            self.reload().await;
        }
        global_result?;
        Ok(reply.feed_id)
    }

    /// Delete a feed (`feed-delete-button`; confirmation is client glue), then
    /// refresh the list.
    pub async fn delete_feed(&self, feed_id: String) -> Result<(), String> {
        let feed = FeedClient::new(self.nest.clone());
        feed.feed_delete(feed_id).await.map_err(|e| e.to_string())?;
        self.refresh_feeds().await;
        Ok(())
    }

    // ── Bridge subscription ──────────────────────────────────────

    /// Subscribe to a bridge feed (`bridge-form-subscribe-button`) over
    /// `fauna.bridges.feeds.create`. Reflects the result into
    /// `BridgeFormState` (cleared on success, error stamped on failure) and
    /// returns the row id.
    pub async fn subscribe_bridge(
        &self,
        kind: String,
        uri: String,
        name: String,
    ) -> Result<i64, String> {
        {
            let mut s = self.state.write().unwrap();
            s.bridge_form.submitting = true;
            s.bridge_form.error = None;
        }
        self.notify();

        let bridges = BridgesClient::new(self.nest.clone());
        match bridges.feeds_create(kind, uri, name).await {
            Ok(id) => {
                {
                    let mut s = self.state.write().unwrap();
                    s.bridge_form = Default::default();
                }
                // Reflect the new subscription into the selector list.
                self.refresh_bridge_feeds().await;
                Ok(id)
            }
            Err(e) => {
                let e = e.to_string();
                {
                    let mut s = self.state.write().unwrap();
                    s.bridge_form.submitting = false;
                    s.bridge_form.error = Some(LocalizedText::key_arg(
                        "feed.error_subscribe",
                        "message",
                        e.clone(),
                    ));
                }
                self.notify();
                Err(e)
            }
        }
    }

    /// Unsubscribe from a bridge feed (`bridge-feed-unsubscribe-button`) over
    /// `fauna.bridges.feeds.delete`, then refresh the bridge-feed list.
    pub async fn unsubscribe_bridge(&self, id: i64) -> Result<(), String> {
        let bridges = BridgesClient::new(self.nest.clone());
        bridges.feeds_delete(id).await.map_err(|e| e.to_string())?;
        self.refresh_bridge_feeds().await;
        Ok(())
    }

    // ── Quoted-post embed ────────────────────────────────────────

    /// Project the embedded quoted-post card for `quoted_post_id` and **fold it
    /// into the document** of every loaded post that quotes it (render-model.md
    /// § D6 embed-fold), then re-emit. The common case is in-page (the quoted
    /// post is already loaded) — resolved with no fetch by
    /// [`quote::project_from_loaded`]. For a quote *outside* the loaded page,
    /// falls back to a single `fauna.posts.get` + decode. Returns the resolved
    /// view (retained for the windows/apple legs, which still read it directly
    /// until they walk `document`), or `None` if the quote can't be resolved
    /// (e.g. quarantined / not found).
    ///
    /// The resolved view is stored in [`resolved_quotes`](Self::resolved_quotes)
    /// so a later [`resolve_media`](Self::resolve_media) document rebuild re-folds
    /// it — the lazy-resolve→rebuild-document discipline (feed.md § The read
    /// model). The projection *path* is unchanged (D2); only the embed's
    /// *placement* moves into the document.
    pub async fn resolve_quoted_post(&self, quoted_post_id: String) -> Option<QuotedPostView>
    where
        R::Error: fauna_protocol::RpcErrorClass,
    {
        // Reuse a previously-resolved view (the embed-fold below is idempotent),
        // so a repeat call only projects/fetches on the first resolution.
        let cached = self
            .resolved_quotes
            .read()
            .unwrap()
            .get(&quoted_post_id)
            .cloned();
        let view = match cached {
            Some(v) => v,
            None => {
                let view = {
                    let s = self.state.read().unwrap();
                    quote::project_from_loaded(&s.posts, &quoted_post_id)
                };
                let view = match view {
                    Some(v) => v,
                    None => {
                        // Fallback: fetch + decode the single post. This path
                        // decodes a raw signed envelope, so we carry its
                        // verification status into the quoted-post card instead of
                        // discarding the validity flag (F-CL3).
                        let posts = PostsClient::new(self.nest.clone());
                        match posts.posts_get(quoted_post_id.clone()).await {
                            // The nest has no such post for this reader — deleted
                            // by its author (or never served to this caller): the
                            // reference dangles by design, and the embed says the
                            // post is not there (`ui/feed.md` § Post deletion).
                            // Cached below like any resolution, so a dead target
                            // is asked for once, not on every notify.
                            Err(e) if is_post_not_found(&e) => {
                                quote::project_not_found(&quoted_post_id)
                            }
                            // Unreachable, or refused for another reason: fold
                            // nothing, so the next notify's resolve asks again.
                            Err(_) => return None,
                            Ok(reply) => {
                                // A legally-taken-down post has its body **withheld**
                                // (empty) with the takedown reference on the reply
                                // (moderation.md § Categories & enforcement item 1). There
                                // is no envelope to decode — project the tombstone card so
                                // the quote renders "Removed under legal obligation
                                // ({reference})" in place of the withheld content, never a
                                // blank/broken embed.
                                if let Some(marker) = reply.legal_takedown {
                                    quote::project_taken_down(&quoted_post_id, marker.reference)
                                } else {
                                    // Bound to the requested id: a nest serving
                                    // another signed post under this id is `Failed`.
                                    let (post, verification, origin) =
                                        decode_fetched_post(&quoted_post_id, reply.body.as_ref())?;
                                    quote::project_decoded(
                                        &quoted_post_id,
                                        &post,
                                        verification,
                                        origin,
                                    )
                                }
                            }
                        }
                    }
                };
                // Store the resolved view so a later `resolve_media` document
                // rebuild re-folds it, and so a repeat resolution is a cache hit.
                // Also returned, for the windows/apple legs that still read it
                // directly until they walk `document`.
                self.resolved_quotes
                    .write()
                    .unwrap()
                    .insert(quoted_post_id.clone(), view.clone());
                view
            }
        };
        // Fold a `QuotedPost` block (after the body) into every post that quotes
        // this id but does NOT already carry the block, so the document is the
        // complete tree once a client walks it (render-model.md § D6 embed-fold).
        // `notify()` fires ONLY when a fold actually happened — and that
        // conditional re-emit is exactly what makes the notify safe for EVERY
        // app regardless of its trigger discipline. A client that re-calls
        // `resolve_quoted_post` from a notify-driven re-render (linux's old
        // per-tick embed widget; windows' un-guarded `ResolveQuotedAsync`, which
        // observes the manager but has no fire-once guard) gets a no-op second
        // call with NO notify, so its render settles instead of looping; a
        // fire-once-triggering client (the adopted linux paint leg) simply never
        // makes the redundant call. The projection *path* is unchanged (D2); only
        // the embed's *placement* moves into the document.
        let changed = {
            let mut s = self.state.write().unwrap();
            let mut changed = false;
            // A REPOST row embeds its original through the exact same fold —
            // `quoted-post` is ui.yaml's "quoted/reposted post display" — so
            // the filter matches either embed field (`feed.md` § Interaction
            // bar → Repost, ratified 2026-08-10). A repost row's body is empty
            // by construction, so the folded block is its whole document.
            for p in rendered_posts_mut(&mut s).filter(|p| {
                p.quoted_post_id.as_deref() == Some(quoted_post_id.as_str())
                    || p.reposted_post_id.as_deref() == Some(quoted_post_id.as_str())
            }) {
                if !p.document.has_quoted_post() {
                    // Same media carry-across as the two body rebuilds above.
                    let media = p.document.media_blocks();
                    p.document = build_post_document(&p.body, Some(&view), &media);
                    changed = true;
                }
            }
            changed
        };
        if changed {
            self.notify();
        }
        Some(view)
    }

    // ── Single-post deep link ────────────────────────────────────

    /// Make the post `post_id` names renderable, whether or not the feed query
    /// ever loaded it — the shared half of `ui/search.md` § Where logic lives →
    /// *Result navigation (deep link)*.
    ///
    /// Every pre-deep-link caller of a `post_detail` surface clicked a
    /// `post-card` that was on screen, so the post was in
    /// [`FeedSnapshot::posts`] by construction. A search hit is the first caller
    /// that can name a post the timeline never loaded (old content, an account
    /// the viewer doesn't follow closely, a post scrolled past a page boundary),
    /// and a detail surface reading `snapshot.posts` alone then paints a blank
    /// dialog. This fetches that post by id and parks it in
    /// [`FeedSnapshot::deep_linked_post`] — see that field for why it is one slot
    /// beside the list rather than a push into it.
    ///
    /// Cheap and idempotent by construction: a post the timeline already holds
    /// costs no round trip (and clears the slot, so nothing stale lingers), and a
    /// post already in the slot costs none either. Unlike the list path this
    /// decodes the post's **raw signed envelope**, so the projection carries a
    /// real `Verified`/`Failed` and the full body — not the nest's 500-char FTS
    /// preview — and resolves the media hash from the body it already holds
    /// rather than paying [`resolve_media`](Self::resolve_media)'s second fetch.
    pub async fn resolve_post(&self, post_id: String) -> PostResolution {
        // Already covered by the timeline (the ordinary card-click case), or
        // already parked in the slot: nothing to fetch either way.
        {
            let mut s = self.state.write().unwrap();
            if s.posts.iter().any(|p| p.post_id == post_id) {
                if s.deep_linked_post.is_some() {
                    s.deep_linked_post = None;
                }
                return PostResolution::Loaded;
            }
            if s.deep_linked_post
                .as_ref()
                .is_some_and(|p| p.post_id == post_id)
            {
                return PostResolution::Fetched;
            }
        }

        let posts = PostsClient::new(self.nest.clone());
        let Ok(reply) = posts.posts_get(post_id.clone()).await else {
            // Missing, quarantined, or unreachable — `fauna.posts.not_found` is
            // the nest's answer for the first two. Leave the slot alone: the
            // caller's surface degrades exactly as it always has for an
            // unresolvable id, and the outcome tells it so.
            return PostResolution::Unavailable;
        };
        // A legally-taken-down post has its body **withheld** (empty) with the
        // reference on the reply (`moderation.md` § Categories & enforcement item
        // 1), so there is no post to project — but there IS something to render:
        // the tombstone belongs in the detail surface's **body area**, where the
        // withheld post would have been, exactly as the quoted-post embed and the
        // DM bubble already paint theirs. So the slot takes a tombstone summary
        // (`PostSummary::taken_down`) rather than being emptied: `find_post` then
        // answers for this id, and the app branches on `legal_takedown_ref`
        // instead of standing the page error surface in for a body.
        //
        // Overwriting the slot is what the old `clear_deep_linked_post()` was
        // protecting — a previous deep link's post must not sit on screen under a
        // *different* post's takedown notice — and replacing it protects that
        // strictly better than emptying it did.
        if let Some(marker) = reply.legal_takedown {
            {
                let mut s = self.state.write().unwrap();
                s.deep_linked_post = Some(PostSummary::taken_down(&post_id, &marker.reference));
            }
            self.notify();
            return PostResolution::TakenDown {
                reference: marker.reference,
            };
        }
        // Bound to the requested id: a body that verifies but is some
        // other post renders here as `Failed`, never as this one verified.
        let Some((post, verification, origin)) = decode_fetched_post(&post_id, reply.body.as_ref())
        else {
            return PostResolution::Unavailable;
        };
        let mut summary = map_fetched_post(&post_id, &post, verification, origin);
        {
            let mut s = self.state.write().unwrap();
            // A gated post this reader already unsealed must not revert to its
            // teaser just because it arrived through the deep-link door — the
            // same re-fold `reload` owes the list (`reapply_unlocked`).
            self.reapply_unlocked(std::slice::from_mut(&mut summary));
            s.deep_linked_post = Some(summary);
        }
        self.notify();
        PostResolution::Fetched
    }

    // (There is deliberately no `clear_deep_linked_post`. The slot is bounded at
    // one post by construction, so no app needs a "release on navigating away"
    // call, and every write to it now *replaces* rather than empties — the
    // takedown arm above was its last caller. An affordance nobody drives is a
    // dark capability; a future surface that genuinely needs one adds it in the
    // same commit that calls it.)

    // ── Media resolution ─────────────────────────────────────────

    /// Resolve the first media blob hash for a loaded `has_media` post and
    /// write it into the matching `PostSummary.media_hash`, then notify. The
    /// feed-index projection never reads `content.payload` (`feed.md` § The read
    /// model), so the blob hash — which lives in the post body — is resolved
    /// lazily per `has_media` post via a single `fauna.posts.get` + decode (the
    /// direct analogue of the quoted-post fallback, and of each app's old
    /// per-post media-hash decode). A no-op when the post isn't loaded, already
    /// resolved, doesn't flag media, or carries no resolvable attachment.
    pub async fn resolve_media(&self, post_id: String) {
        // Skip the fetch unless a loaded post flags media and isn't resolved.
        let needs = {
            let s = self.state.read().unwrap();
            s.rendered_posts()
                .any(|p| p.post_id == post_id && p.has_media && p.media_hash.is_none())
        };
        if !needs {
            return;
        }
        // Both failures are logged, never shown: the card simply renders
        // without its media. Unlogged, they left an empty `media_hash` with
        // nothing anywhere to say why.
        let posts = PostsClient::new(self.nest.clone());
        let reply = match posts.posts_get(post_id.clone()).await {
            Ok(reply) => reply,
            Err(e) => {
                tracing::debug!(%post_id, "resolve_media: posts.get failed: {e}");
                return;
            }
        };
        // Bound to the requested id: otherwise a nest could pair this
        // card with another signed post's media and a `Verified` status.
        let Some((post, status, origin_status)) =
            decode_fetched_post(&post_id, reply.body.as_ref())
        else {
            tracing::warn!(%post_id, "resolve_media: the post body does not decode");
            return;
        };
        // This is the one feed-list path that decodes the post's *raw signed
        // body*, so `status` is its verification status (F-CL2/F-CL3) — the card
        // flips from the default `Unchecked` to `Verified`/`Failed`, driving the
        // unverified-source badge even before the post is opened. Set it whether
        // or not a media hash resolves (a `has_media` post whose body fails to
        // verify must still surface the badge). A bridged post's bare body keeps
        // `Unchecked` — there is no envelope to verify ([`decode_fetched_post`]).
        // Same path, same rule, for the D10 audit answer (`origin_status`): this is
        // where a list card can first learn an external app authored the post as
        // the account (`atproto-pds-full.md` § D10 → *Audit*). A failed verify — a
        // bad signature or a body that is not this id — yields `Unknown`, never a
        // cert claim nothing authenticated.
        let media_hash = first_media_hash(&post.body);
        // The typed, in-body-order fold — this is the only path that holds the decoded body,
        // so it is where image-vs-video is decided for every app (render-model.md § D6).
        let media = media_blocks(&post.body);
        let changed = {
            let mut s = self.state.write().unwrap();
            match rendered_posts_mut(&mut s).find(|p| p.post_id == post_id) {
                Some(p) => {
                    let mut changed = false;
                    if p.verification != status {
                        p.verification = status;
                        changed = true;
                    }
                    if p.authoring_origin != origin_status {
                        p.authoring_origin = origin_status;
                        changed = true;
                    }
                    if let Some(hash) = media_hash.as_deref()
                        && p.media_hash.as_deref() != Some(hash)
                    {
                        p.media_hash = Some(hash.to_string());
                        // Rebuild the document with the resolved media folded
                        // in as an `Image` block (render-model.md § D6),
                        // re-including any quote already resolved for this post
                        // so the embed-fold survives whichever embed resolved
                        // last.
                        let quote = match &p.quoted_post_id {
                            Some(q) => self.resolved_quotes.read().unwrap().get(q).cloned(),
                            None => None,
                        };
                        p.document = build_post_document(&p.body, quote.as_ref(), &media);
                        changed = true;
                    }
                    changed
                }
                None => false,
            }
        };
        if changed {
            self.notify();
        }
    }

    /// Resolve the buyer's price read for a sold post (`monetization.md` §
    /// Per-post pay-to-unlock → *the buyer's price read is post-addressed*,
    /// ruled 2026-07-29): `fauna.subscriptions.post_unlock.get`, keyed by this
    /// post's own `(author, post_id)`. Triggered when
    /// [`PostSummary::gated_tier`] names a `post-unlock-*` tier
    /// (`fauna_client_subscriptions::UNLOCK_TIER_PREFIX`) — the **same**
    /// post, since a designated tier's `unlocks_post` is set to the post it
    /// gates at creation time, so a well-formed sold post always answers
    /// `Some`. A refusal, an undesignated/foreign id, or
    /// a transport error all fold to `None` — the teaser then shows no price,
    /// and claim-code redemption (§5) stays the fallback purchase path
    /// (`monetization.md:129`, additive-everywhere). The `resolve_media`
    /// pattern: fire-once via the `unlock_offer.is_none()` guard.
    pub async fn resolve_post_unlock_offer(&self, post_id: String) {
        // Already bought by this actor — the nest would re-offer (its read has
        // no caller axis by design), so the teaser would reappear on the very
        // notification the buy emitted. See `unlock_purchase_requested`.
        if self
            .unlock_purchase_requested
            .read()
            .unwrap()
            .contains(&post_id)
        {
            return;
        }
        let (needs, author_hex) = {
            let s = self.state.read().unwrap();
            match s.rendered_posts().find(|p| p.post_id == post_id) {
                Some(p)
                    if p.unlock_offer.is_none()
                        && p.gated_tier.as_deref().is_some_and(|t| {
                            t.starts_with(fauna_client_subscriptions::UNLOCK_TIER_PREFIX)
                        }) =>
                {
                    (true, p.author.clone())
                }
                _ => (false, String::new()),
            }
        };
        if !needs {
            return;
        }
        // The author is never a prospective buyer of their own post. This read
        // is scoped to "a prospective buyer's client" (`monetization.md`
        // § Per-post pay-to-unlock → *the buyer's price read is post-addressed*),
        // and the author's own access to the body is custody, not a purchase
        // (`unlock_gated_post`'s `is_author` branch). Resolving here would paint
        // a buy affordance on the author's OWN card, whose click subscribes them
        // to their own unlock tier — see `buy_unlock_offer`'s matching refusal.
        if self.is_local_actor(&author_hex) {
            return;
        }
        let Some(author_id) = hex::decode(&author_hex)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .map(fauna_core::identity::ActorId)
        else {
            return;
        };
        let subs = fauna_client_subscriptions::SubscriptionsClient::new(self.nest.clone());
        let Ok(Some(offer)) = subs.post_unlock_get(author_id, post_id.clone()).await else {
            return;
        };
        let view = UnlockOfferView {
            tier_name: offer.tier_name,
            price_hint: offer.price_hint,
            payment_url: offer.payment_url,
        };
        let changed = {
            let mut s = self.state.write().unwrap();
            match rendered_posts_mut(&mut s).find(|p| p.post_id == post_id) {
                Some(p) => {
                    p.unlock_offer = Some(view);
                    true
                }
                None => false,
            }
        };
        if changed {
            self.notify();
        }
    }

    /// Resolve this post's tip surface (`monetization.md` § Tips) and fold it
    /// into the matching [`PostSummary::tips`], then notify — the read that
    /// delivers a tip's ratified consequence, which is *attribution / display /
    /// notification, never a grant*.
    ///
    /// **Every outcome writes `Some`, deliberately** — including a post with no
    /// tips, a refusal, and a transport error. Two reasons,
    /// and the first is load-bearing:
    ///
    /// 1. **`Some` is the fire-once guard.** Unlike
    ///    [`resolve_post_unlock_offer`](Self::resolve_post_unlock_offer), which
    ///    has `gated_tier` as a cheap data trigger, *nothing in the feed-index
    ///    projection says whether a post has tips*. A resolver that left an
    ///    untipped post at `None` would be re-asked by the caller's pump on
    ///    every snapshot notify — and since this call itself notifies, that is
    ///    an unbounded RPC loop, one per rendered post.
    /// 2. **An empty answer is a truthful answer.** `TipView::default()` says
    ///    "no tips", which is exactly what an untipped post, a nest with no tip
    ///    mechanism compiled in (it answers zero), and a failed
    ///    read all mean to a reader — the
    ///    ratified single degradation (`monetization.md` § Implementation status
    ///    today, the Tips bullet). Apps render one empty surface for all three.
    ///
    /// The cost of folding a transport error to the same empty view is that a
    /// blip hides a tipped post's total until the next resolve; that is bounded,
    /// because every `query_feed` rebuilds `PostSummary` with `tips: None` and
    /// so re-resolves. The alternative — retrying in place — is the loop above.
    ///
    /// **No trust check here, by construction.** The nest settles a tip's
    /// authenticity at ingest and never at read (`monetization.md` § Zap
    /// receipts), so a client renders every row it receives, unfiltered; adding
    /// a client-side filter would re-open exactly the per-reader re-checking
    /// that discipline exists to prevent.
    ///
    /// Compiled out with the `payments` feature (`dynamic-features.md` §
    /// Charter members — tips are a buy-side gate surface): an excised build
    /// keeps the inert [`PostSummary::tips`] field with no way to populate it,
    /// and never names the `fauna.tips.list` kind.
    #[cfg(feature = "payments")]
    pub async fn resolve_post_tips(&self, post_id: String) {
        {
            let s = self.state.read().unwrap();
            match s.rendered_posts().find(|p| p.post_id == post_id) {
                Some(p) if p.tips.is_none() => {}
                // Already resolved, or the post isn't rendered.
                _ => return,
            }
        }
        let tips = fauna_client_payments::TipsClient::new(self.nest.clone());
        let view = match tips.tips_list(post_id.clone(), None).await {
            Ok(reply) => TipView {
                total_msats: reply.total_msats,
                tip_count: reply.tip_count,
                senders: reply
                    .tips
                    .into_iter()
                    .map(|t| TipSenderView {
                        sender: t.sender.map(|a| hex::encode(a.0)),
                        sender_ref: t.sender_ref,
                        amount_msats: t.amount_msats,
                        mechanism: t.mechanism,
                        received_at: t.received_at,
                    })
                    .collect(),
                has_more: reply.has_more,
            },
            // a refusal (`unknown_kind` included) or a transport error — the
            // same empty surface, per the doc comment above.
            Err(_) => TipView::default(),
        };
        let changed = {
            let mut s = self.state.write().unwrap();
            match rendered_posts_mut(&mut s).find(|p| p.post_id == post_id) {
                Some(p) => {
                    p.tips = Some(view);
                    true
                }
                None => false,
            }
        };
        if changed {
            self.notify();
        }
    }

    /// Buy a sold post via the self-serve teaser affordance
    /// (`gated-post-buy-button`, `monetization.md` § Per-post pay-to-unlock →
    /// *the buyer's price read is post-addressed*) — the **existing**
    /// subscribe flow against the resolved offer's `tier_name`, no new nest
    /// write: the same call `subscription-offer-subscribe-button` makes,
    /// targeted by post id instead of a hand-typed tier name. `Ok(true)` =
    /// queued (pending author approval, the client-minted-tier norm),
    /// `Ok(false)` = approved outright. `None` when the post isn't loaded or
    /// its offer hasn't resolved yet — the button isn't reachable then, since
    /// its render is gated on the same [`PostSummary::unlock_offer`] field.
    ///
    /// On `Ok(_)` the post's `unlock_offer` is cleared and re-emitted: the
    /// purchase (queued or approved) has been requested, so re-showing the
    /// price + buy affordance would invite a duplicate subscribe. Before this,
    /// a successful buy left client state untouched — an unconfirmed click a
    /// caller could not distinguish from one still in flight (found
    /// diagnosing an e2e test whose buyer click was immediately followed by
    /// an actor switch, which raced the in-flight subscribe since nothing
    /// here gave the caller a real completion signal to wait on).
    pub async fn buy_unlock_offer(&self, post_id: String) -> Option<Result<bool, String>> {
        let (author_hex, tier) = {
            let s = self.state.read().unwrap();
            let p = s.rendered_posts().find(|p| p.post_id == post_id)?;
            let offer = p.unlock_offer.as_ref()?;
            (p.author.clone(), offer.tier_name.clone())
        };
        // Refuse the author's own post, at the mutation and not only at
        // `resolve_post_unlock_offer`'s render trigger: an offer resolved into a
        // snapshot BEFORE an actor switch is still clickable after it, which is
        // precisely how a seller came to hold the only seat on their own unlock
        // tier while the real buyer's `key_blob.get` answered `not_subscribed`
        // (measured 2026-08-25 on macOS/iOS). `None` is the established "not
        // buyable" answer this returns for an unresolved offer, and every app
        // already renders no affordance for it.
        if self.is_local_actor(&author_hex) {
            return None;
        }
        let author_id = fauna_core::identity::ActorId(
            hex::decode(&author_hex)
                .ok()
                .and_then(|b| <[u8; 32]>::try_from(b).ok())?,
        );
        let kp = ActorKeypair::from_secret(self.actor_secret);
        let subs = fauna_client_subscriptions::SubscriptionsClient::new(self.nest.clone());
        let result = subs
            .subscribe_publishing_ek(author_id, tier, &kp)
            .await
            .map(|reply| {
                matches!(
                    reply,
                    fauna_protocol::subscriptions::SubscribeReply::Queued { .. }
                )
            })
            .map_err(|e| e.to_string());
        if result.is_ok() {
            // Record the purchase BEFORE clearing the offer and notifying: the
            // `notify()` below is what drives every app's render trigger back
            // into `resolve_post_unlock_offer`, and it must find this set
            // already populated or the teaser re-offers itself.
            self.unlock_purchase_requested
                .write()
                .unwrap()
                .insert(post_id.clone());
            let changed = {
                let mut s = self.state.write().unwrap();
                match rendered_posts_mut(&mut s).find(|p| p.post_id == post_id) {
                    Some(p) => {
                        p.unlock_offer = None;
                        true
                    }
                    None => false,
                }
            };
            if changed {
                self.notify();
            }
        }
        Some(result)
    }

    // ── Remote-image reveal (D3) ─────────────────────────────────

    /// Opt this post into loading its remote images (render-model.md § D3) — the
    /// feed twin of `ConversationsManager::reveal_remote_images`. Adds the post to
    /// the in-memory reveal set and re-emits, so the next [`snapshot`](Self::snapshot)
    /// projects `RemoteImage.revealed: true` for it (covering both its list card and
    /// its detail). The `load-remote-content-button` on a feed card/detail dispatches
    /// here instead of each app keeping a per-card reveal flag. **In-memory only**
    /// — the no-persistence posture is unchanged; idempotent on a repeat tap.
    pub fn reveal_remote_images(&self, post_id: String) {
        self.revealed_remote.write().unwrap().insert(post_id);
        self.notify();
    }

    // ── Trained topic factors: the per-post gesture ──────────────
    //
    // `topic-factors.md` § Training signals. The model trains **instantly, on
    // device**, at each tap: no background job, no nest involvement beyond
    // storing a blob it cannot read.

    /// The trained factor a per-post gesture trains **in context** — the single
    /// `topic:*` factor in the current feed's composition, if there is exactly
    /// one ("more like this" inside the Cats feed means *more cats*).
    ///
    /// `None` when the feed composes no trained topic, or composes several and
    /// the gesture is therefore ambiguous — the client then opens the target
    /// sheet (`feed-post-train-target-sheet`) instead of guessing
    /// (§ Authoring surface & picker).
    pub fn train_target_factor(&self) -> Option<String> {
        let composition = self.composition.read().unwrap();
        let mut topics = composition
            .iter()
            .filter(|e| fauna_core::scoring::is_topic_factor(&e.factor));
        match (topics.next(), topics.next()) {
            (Some(only), None) => Some(only.factor.clone()),
            _ => None,
        }
    }

    /// This post's current toggle state for `factor` — `Some(MoreLikeThis)` /
    /// `Some(LessLikeThis)` if the user has already marked it, `None` if not.
    /// Renders the gesture's checked state across sessions (the markers live in
    /// the sealed model, so they survive a restart and reach every device).
    ///
    /// Answers only for a factor **this feed composes** (that is the one whose
    /// model is loaded); a client asking about any other factor gets `None`.
    pub fn example_label_for(&self, post_id: &str, factor: &str) -> Option<TrainVerb> {
        self.sealed
            .read()
            .unwrap()
            .topic(factor)
            .and_then(|m| m.example_label(post_id))
            .and_then(TrainVerb::from_label)
    }

    /// Score the loaded window's **public** posts with `factor`'s trained model,
    /// best first, truncated to `top_n` — the candidate exemplars the publish
    /// sheet lists for review-and-prune (`topic-factors.md` § Publishing a
    /// trained factor).
    ///
    /// **The corpus is the loaded window, and that is the accepted limitation,**
    /// not an oversight: a List "scores only content the publisher saw — it does
    /// not generalize to unseen posts" (§ Publishing), which the sheet states in
    /// its own copy. Paging the feed to widen it would still not generalize, and
    /// exact cross-page ordering is precisely what the tier-1 seal forbids
    /// ([`crate::sealed_compose`] § Page-boundary honesty).
    ///
    /// Non-public posts are excluded at the source: a published List is public
    /// (frame § Tier-3 artifact kinds — a `mail` List is rejected at publish),
    /// so a gated post must never reach the review sheet where a user could
    /// endorse it into one. This is [`Self::is_public_post`]'s predicate, the
    /// same gate the Layer-B signal producer uses.
    ///
    /// Unlike [`Self::example_label_for`], this answers for **any** trained
    /// factor, not only one this feed composes: the user publishes from the
    /// Personalization home, whose factor need not be the current feed's.
    pub async fn score_corpus_for_factor(
        &self,
        factor: &str,
        top_n: usize,
    ) -> Result<Vec<ScoredExemplar>, String> {
        let model = self.fetch_model(factor).await?;
        let mut scored: Vec<ScoredExemplar> = {
            let s = self.state.read().unwrap();
            s.posts
                .iter()
                .filter(|p| p.gated_tier.is_none())
                .map(|p| ScoredExemplar {
                    post_id: p.post_id.clone(),
                    preview: p.body.clone(),
                    score: model.damped_score(
                        &model_text(None, &p.body, &p.tags),
                        fauna_core::scoring::cues::CUE_ENGAGEMENT_WEIGHT_PM,
                    ),
                })
                .collect()
        };
        // Best first; ties broken by id so the sheet — and the artifact built
        // from it — are reproducible rather than hash-order dependent.
        scored.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.post_id.cmp(&b.post_id))
        });
        scored.truncate(top_n);
        Ok(scored)
    }

    /// Rebuild a trained factor's **publishable vocabulary** — the Model half of
    /// the publish sheet's review (`topic-factors.md` § Publishing a trained
    /// factor, v2).
    ///
    /// This is the *rebuild, not a redaction*, and every step of it is a privacy
    /// step:
    ///
    /// 1. read the factor's own example **markers** (ids only — ids never enter
    ///    an artifact; they are here solely to decide what to fetch);
    /// 2. fetch each marked post and **drop everything not publicly readable**:
    ///    tier-gated (`Post.gated`), legally taken down, deleted, or otherwise
    ///    unfetchable. ⚠ The gate must be read off the post, NOT inferred from
    ///    the fetch succeeding — the publisher can read their *own* restricted
    ///    posts, so "it fetched" proves nothing about what their subscribers
    ///    could see;
    /// 3. re-tokenize with [`model_text`], the same shape training used, and
    ///    scrub.
    ///
    /// The private model is never consulted for counts — only for its marker
    /// list — which is what makes an engagement- and restricted-trained factor
    /// publish byte-identically to its explicit-public-only twin.
    pub async fn scrub_corpus_for_factor(
        &self,
        factor: &str,
    ) -> Result<TrainedModelReview, String> {
        let model = self.fetch_model(factor).await?;
        let markers: Vec<(String, ExampleLabel)> = model
            .example_markers()
            .map(|(id, label)| (id.to_string(), label))
            .collect();
        let marked_examples = markers.len() as u32;

        let posts = PostsClient::new(self.nest.clone());
        let mut corpus: Vec<(String, ExampleLabel)> = Vec::with_capacity(markers.len());
        for (post_id, label) in markers {
            // Every failure mode below is an EXCLUSION, never a fallback: an
            // example we cannot confirm is public must not teach the artifact.
            let Ok(reply) = posts.posts_get(post_id).await else {
                continue;
            };
            if reply.legal_takedown.is_some() {
                continue;
            }
            let Ok((post, _origin)) = decode_post(reply.body.as_ref()) else {
                continue;
            };
            if post.gated.is_some() {
                continue;
            }
            corpus.push((
                model_text(
                    post.content_warning.as_deref(),
                    &post.body_text(),
                    &post.tags(),
                ),
                label,
            ));
        }
        let included_examples = corpus.len() as u32;

        let vocab = scrub_corpus(
            &corpus,
            fauna_core::scoring::TEXT_MODEL_PUBLISH_MIN_DOCS,
            fauna_core::scoring::TEXT_MODEL_PUBLISH_MAX_NGRAMS,
        );
        Ok(TrainedModelReview {
            more_docs: vocab.more_docs,
            less_docs: vocab.less_docs,
            included_examples,
            marked_examples,
            ngrams: vocab
                .ngrams
                .into_iter()
                .map(|n| ReviewNgram {
                    ngram: n.ngram,
                    more: n.more,
                    less: n.less,
                })
                .collect(),
        })
    }

    /// **More like this** / **less like this** on a post
    /// (`feed-post-more-like-this` / `feed-post-less-like-this`).
    ///
    /// The full gesture, per § Training signals:
    ///
    /// 1. fetch the post's **full text** (`fauna.posts.get` → title + body +
    ///    tags). The feed card's 500-char preview is what *scoring* reads, but it
    ///    is too lossy to *train* on;
    /// 2. fetch + unseal the model (absent ⇒ start from a fresh one — this is
    ///    how a factor's first example creates its blob);
    /// 3. apply the forward delta. Tapping the **other** verb on an
    ///    already-marked post applies the inverse of the old delta and then the
    ///    forward of the new — [`TopicModel::train`] owns that, so a flip is
    ///    exactly a flip and never a double-count;
    /// 4. re-seal and `put`;
    /// 5. swap the model into the loaded scorers and re-rank, so the feed
    ///    responds to the tap immediately rather than on the next reload.
    ///
    /// Idempotent by construction: re-tapping the *same* verb is a
    /// [`TrainOutcome::DuplicateSignal`] — the model is not mutated, so no put is
    /// issued (the guard is in the model, not in the UI).
    pub async fn train_post(
        &self,
        post_id: String,
        factor: String,
        verb: TrainVerb,
    ) -> Result<TrainResult, String> {
        let text = self.full_text_for_training(&post_id).await?;
        let mut model = self.fetch_model(&factor).await?;

        let outcome = model.train(&post_id, &text, verb.into());
        if outcome == TrainOutcome::DuplicateSignal {
            // The model is byte-identical; writing it back would be a pointless
            // round-trip and a pointless last-put-wins race with the user's
            // other devices.
            return Ok(outcome.into());
        }

        self.put_model(&factor, &model).await?;
        self.sealed.write().unwrap().set_topic(&factor, model);
        self.rerank_window();
        self.notify();
        Ok(outcome.into())
    }

    /// Un-mark a post (tapping its already-active verb off): apply the exact
    /// inverse of the delta it trained, drop the marker, re-seal, put.
    ///
    /// **The declared undo limitation** (§ Training signals): the inverse is
    /// recomputed from the post's content *at undo time*. If the body is no
    /// longer fetchable (deleted, or no longer readable), the marker is removed
    /// but its statistical ghost stays in the counts — harmless in a personal
    /// statistical model, and strictly better than refusing to un-mark. That
    /// fallback is [`TopicModel::remove_marker`], which is explicitly counts-
    /// preserving; it is not an error path.
    pub async fn untrain_post(&self, post_id: String, factor: String) -> Result<(), String> {
        let mut model = self.fetch_model(&factor).await?;

        let changed = match self.full_text_for_training(&post_id).await {
            Ok(text) => model.untrain(&post_id, &text),
            // Body gone: marker-only removal, the documented ghost.
            Err(_) => model.remove_marker(&post_id),
        };
        if !changed {
            // No marker for this post — nothing to undo.
            return Ok(());
        }

        self.put_model(&factor, &model).await?;
        self.sealed.write().unwrap().set_topic(&factor, model);
        self.rerank_window();
        self.notify();
        Ok(())
    }

    /// The post's full trainable text — title (content warning / summary) +
    /// body + tags, in the same shape the scorer reads, so a post trains under
    /// the feature set it will later be scored against.
    async fn full_text_for_training(&self, post_id: &str) -> Result<String, String> {
        let posts = PostsClient::new(self.nest.clone());
        let reply = posts
            .posts_get(post_id.to_string())
            .await
            .map_err(|e| e.to_string())?;
        let (post, _origin) = decode_post(reply.body.as_ref()).map_err(|e| e.to_string())?;
        Ok(model_text(
            post.content_warning.as_deref(),
            &post.body_text(),
            &post.tags(),
        ))
    }

    /// Fetch + unseal the trained model for `factor`. An absent blob is a fresh
    /// model (the factor's first example is what creates its row); an
    /// **unopenable** one is an error — never a fresh model, which would train
    /// over everything the user has taught it.
    async fn fetch_model(&self, factor: &str) -> Result<TopicModel, String> {
        let reply = PersonalizationClient::new(self.nest.clone())
            .model_fetch(factor.to_string())
            .await
            .map_err(|e| e.to_string())?;
        match reply.sealed_blob {
            None => Ok(TopicModel::new()),
            Some(blob) => unseal_topic_model(
                &blob,
                &model_seal_keys(&BackupKey::derive(&self.actor_secret)),
            )
            .map_err(|e| e.to_string()),
        }
    }

    /// Seal + `put` the model. `sample_count` is advisory metadata (the nest
    /// cannot verify it against the opaque blob) used for the settings display
    /// and the adopt-if-larger cross-device reconcile hint.
    async fn put_model(&self, factor: &str, model: &TopicModel) -> Result<(), String> {
        let keys = model_seal_keys(&BackupKey::derive(&self.actor_secret));
        let sealed = seal_topic_model(model, &keys).map_err(|e| e.to_string())?;
        PersonalizationClient::new(self.nest.clone())
            .model_put(factor.to_string(), sealed, model.example_count())
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ── Engagement cues (engagement-cues.md §§ Cue vocabulary / At rest) ──────

    /// **Fetch-on-session-start.** Load the sealed `cues:v1` rollup from the nest
    /// and install it as the live engine. An absent row is a fresh empty engine
    /// (first-ever capture, or after a delete). An **unopenable** blob is a
    /// surfaced error, **never** a silent fresh rollup — starting fresh would
    /// erase every cue the user's other devices recorded on the next put (the
    /// exact `fetch_model` hard-fail rule).
    ///
    /// Call once at session start, **before** feeding observations: the manager
    /// suppresses puts until this runs, and a verdict folded before hydration is
    /// discarded when the fetched rollup is installed here.
    pub async fn hydrate_cues(&self) -> Result<(), String> {
        let reply = PersonalizationClient::new(self.nest.clone())
            .model_fetch(CUES_ROLLUP_FACTOR_V1)
            .await
            .map_err(|e| e.to_string())?;
        let rollup = match reply.sealed_blob {
            None => CueRollup::new(),
            Some(blob) => unseal_cue_rollup(
                &blob,
                &model_seal_keys(&BackupKey::derive(&self.actor_secret)),
            )
            .map_err(|e| e.to_string())?,
        };
        let mut c = self.cue.write().unwrap();
        c.engine = CueEngine::new(rollup);
        c.hydrated = true;
        // The installed rollup is authoritative; nothing local is unsaved.
        c.dirty = false;
        Ok(())
    }

    /// Feed one shell-derived [`CueObservation`] to the live engine (owner doc
    /// § Cue vocabulary): derive its verdict, fold it into the rollup (last-wins),
    /// and — once the debounce window has elapsed since the last put — seal +
    /// `model_put` the rollup. The put cadence is driven by the observation's own
    /// `observed_at_ms` (clock-free); between puts the rollup accumulates in
    /// memory. Returns the derived verdict, if any.
    ///
    /// **Layer A** (`topic-factors.md` § Training signals): if this exposure
    /// *changed* the item's rollup verdict, it also weakly trains each composed
    /// `learn_from_engagement`-on trained factor (a `watch-complete` as a weak
    /// *more like this*, a `skip` as a weak *less like this*) and re-ranks the
    /// window — a cheap no-op in the default case where no factor opted in.
    ///
    /// Puts are suppressed before [`hydrate_cues`](Self::hydrate_cues): the fold
    /// still happens, but nothing is sent until the nest's rollup has been
    /// fetched (else this device's partial state would overwrite another
    /// device's cues). The batched put lands on the *next* observation past the
    /// window; a trailing tail (the user stops scrolling) is caught by
    /// [`flush_cues`](Self::flush_cues) on background/close.
    pub async fn record_observation(
        &self,
        obs: CueObservation,
    ) -> Result<Option<CueVerdict>, String> {
        let content_id = obs.content_id.clone();
        let (verdict, old_verdict, new_verdict, staged) = {
            let mut c = self.cue.write().unwrap();
            let now = obs.observed_at_ms;
            // The rollup verdict for this item BEFORE and AFTER folding this
            // exposure — the Layer-A training transition (`old` may equal `new`,
            // e.g. a verdict-less exposure or a re-watch, in which case nothing
            // trains). Read back from the rollup so it reflects whatever last-wins
            // record applied, not just the derived verdict.
            let old_verdict = c.engine.verdict(&content_id);
            let verdict = c.engine.observe(obs);
            let new_verdict = c.engine.verdict(&content_id);
            if verdict.is_some() {
                c.dirty = true;
            }
            // Anchor the debounce clock to the first observation seen, so the
            // window is measured against real observation time rather than a
            // zero that an epoch-scale `now` would clear on the first fold.
            if c.last_put_ms == 0 {
                c.last_put_ms = now;
            }
            let due = c.hydrated
                && c.dirty
                && now.saturating_sub(c.last_put_ms) >= CUE_PUT_DEBOUNCE_S.saturating_mul(1000);
            let staged = if due {
                c.last_put_ms = now;
                c.dirty = false;
                Some(c.engine.rollup().clone())
            } else {
                None
            };
            (verdict, old_verdict, new_verdict, staged)
        };
        // Primary durable state first: the debounced cue-rollup put (the rollup
        // is the source of truth for verdicts; the trained models are derived).
        if let Some(rollup) = staged {
            self.put_cue_rollup(&rollup).await.inspect_err(|_| {
                // The put failed — re-mark dirty so a later observation or an
                // explicit flush retries rather than silently dropping the batch.
                self.cue.write().unwrap().dirty = true;
            })?;
        }
        // Layer-A weak training: fold the verdict transition into each composed
        // learn_from_engagement factor (owner doc § Layer A). A cheap no-op when
        // the verdict did not change or no factor opted in (the default).
        if old_verdict != new_verdict {
            self.train_engagement_for_transition(
                &content_id,
                old_verdict.clone(),
                new_verdict.clone(),
            )
            .await?;
        }
        // Layer-B opt-in contribution (owner doc § Layer B write path): the SAME
        // verdict transition, submitted to the k-anon aggregate — gated on the
        // cached opt-in + public-post-only inside `contribute_signal`. Best-effort:
        // a failed contribution is dropped (informs-never-compels; the sealed
        // rollup + Layer-A training above are the durable state), so a nest error
        // never fails the observation.
        if old_verdict != new_verdict
            && let Some(v) = new_verdict
        {
            self.contribute_signal(&content_id, v).await;
        }
        Ok(verdict)
    }

    /// Read the caller's Layer-B signal-sharing opt-in state **and** the
    /// transparency export list (owner doc § Layer B). Caches `share` for the
    /// [`record_observation`](Self::record_observation) producer, then hands the
    /// UI the full reply to render the toggle + the "what this nest publishes"
    /// pane. `published` is the nest-wide ≥k export view (`report:*` and
    /// `signal:*` alike), byte-identical to `report_share.status`.
    pub async fn signal_share_status(&self) -> Result<ModerationSignalShareStatusReply, String> {
        let client = ModerationClient::new(self.nest.clone());
        let status = client
            .signal_share_status()
            .await
            .map_err(|e| e.to_string())?;
        self.share_signals.store(status.share, Ordering::Relaxed);
        Ok(status)
    }

    /// Session-start hydrate of the opt-in cache (the linux feed view calls it
    /// beside [`hydrate_cues`](Self::hydrate_cues)) so the producer respects the
    /// persisted opt-in before the user ever opens the Personalization page.
    /// Returns the cached value.
    pub async fn hydrate_signal_optin(&self) -> Result<bool, String> {
        Ok(self.signal_share_status().await?.share)
    }

    /// Set the caller's signal-sharing opt-in, then re-read status so the returned
    /// reply reflects the persisted value (opting out withdraws this actor's
    /// `signal:*` rows, so the export list may shrink). Caches the nest-confirmed
    /// `share` for the producer — never optimistically. The UI renders the reply.
    pub async fn set_signal_sharing(
        &self,
        share: bool,
    ) -> Result<ModerationSignalShareStatusReply, String> {
        let client = ModerationClient::new(self.nest.clone());
        client
            .signal_share_set(share)
            .await
            .map_err(|e| e.to_string())?;
        let status = client
            .signal_share_status()
            .await
            .map_err(|e| e.to_string())?;
        self.share_signals.store(status.share, Ordering::Relaxed);
        Ok(status)
    }

    /// The Layer-B producer (owner doc § Layer B write path): contribute one
    /// derived cue verdict to the k-anon aggregate — but ONLY when the user opted
    /// in and the post is **public**. A verdict about restricted content is
    /// excluded here (its existence would leak readership), matching the nest's
    /// public-posts-only write gate. Best-effort — a failed contribution is
    /// dropped (the durable state is the sealed rollup + Layer-A training).
    async fn contribute_signal(&self, content_id: &str, verdict: CueVerdict) {
        if !self.share_signals.load(Ordering::Relaxed) {
            return;
        }
        if !self.is_public_post(content_id) {
            return;
        }
        // A verdict this build does not name is never contributed.
        let Some(wire) = verdict.wire_str() else {
            return;
        };
        let client = ModerationClient::new(self.nest.clone());
        let _ = client
            .signal_contribute(content_id.to_string(), wire.to_string())
            .await;
    }

    /// Whether `content_id` is a **public** post in the current loaded window (its
    /// `gated_tier` is `None`). A post absent from the window is treated as
    /// non-public: the producer never contributes a verdict it cannot confirm is
    /// about public content.
    fn is_public_post(&self, content_id: &str) -> bool {
        self.state
            .read()
            .unwrap()
            .posts
            .iter()
            .any(|p| p.post_id == content_id && p.gated_tier.is_none())
    }

    /// Weakly train each composed `learn_from_engagement` factor from one
    /// cue-verdict transition (owner doc § Layer A; `topic-factors.md`
    /// § Training signals). `old`/`new` are this item's rollup verdict before and
    /// after the observation (`watch-complete` ⇒ a weak *more like this*, `skip`
    /// ⇒ a weak *less like this*, no verdict ⇒ no example); the caller guarantees
    /// `old != new`.
    ///
    /// The item's text is the **loaded-window preview** (`PostSummary`) — the
    /// very `model_text(None, body, tags)` the feed *scores* against, so a post
    /// trains under the feature set it is scored by, and no `posts.get` is spent
    /// per scrolled item (the documented cost bound). An item no longer in the
    /// window is skipped (the "text still fetchable" clause); its verdict stays in
    /// the rollup either way.
    ///
    /// Trains the **already-loaded** composed model (no fresh fetch per
    /// observation: engagement is a high-frequency weak signal, and the accepted
    /// last-put-wins race, § Placement, costs at most one weak signal), re-seals +
    /// puts it, swaps it into the live scorers, and re-ranks so the feed responds.
    /// A no-op when no factor opted in — the default-off common case, in which
    /// [`Self::engagement_factors`] is empty.
    async fn train_engagement_for_transition(
        &self,
        content_id: &str,
        old: Option<CueVerdict>,
        new: Option<CueVerdict>,
    ) -> Result<(), String> {
        let factors = self.engagement_factors.read().unwrap().clone();
        if factors.is_empty() {
            return Ok(());
        }
        // Loaded-window preview text; absent ⇒ the item has been paged out, so
        // its text is no longer cheaply fetchable — skip training (the rollup
        // verdict is already recorded).
        let Some(text) = self.window_model_text(content_id) else {
            return Ok(());
        };
        let old_label = old.as_ref().and_then(example_label_from_verdict);
        let new_label = new.as_ref().and_then(example_label_from_verdict);
        // An unknown verdict on both sides trains as no transition at all.
        if old_label == new_label {
            return Ok(());
        }

        let mut trained_any = false;
        for factor in factors {
            // Base off the already-open loaded model. `topic()` returns `None`
            // for a factor this feed does not actually compose (belt-and-braces:
            // the set was composed-filtered at load) or one whose blob failed to
            // open — either way, skip rather than resurrect a fresh model over
            // the user's taught one.
            let Some(mut model) = self.sealed.read().unwrap().topic(&factor).cloned() else {
                continue;
            };
            model.train_engagement(&text, old_label.clone(), new_label.clone());
            self.put_model(&factor, &model).await?;
            self.sealed.write().unwrap().set_topic(&factor, model);
            trained_any = true;
        }
        if trained_any {
            self.rerank_window();
            self.notify();
        }
        Ok(())
    }

    /// The loaded-window preview text for `content_id` — the same
    /// `model_text(None, body, tags)` shape [`SealedScorers::terms_for`] scores
    /// with — or `None` if the item is not in the current window.
    fn window_model_text(&self, content_id: &str) -> Option<String> {
        let s = self.state.read().unwrap();
        let post = s.posts.iter().find(|p| p.post_id == content_id)?;
        Some(model_text(None, &post.body, &post.tags))
    }

    /// Force a put of the current rollup if it holds unsaved verdicts — the
    /// **background / app-close flush** (owner doc § At rest: "or on
    /// background/close"). A no-op when nothing is dirty, or before hydration.
    pub async fn flush_cues(&self) -> Result<(), String> {
        let staged = {
            let mut c = self.cue.write().unwrap();
            if c.hydrated && c.dirty {
                c.dirty = false;
                Some(c.engine.rollup().clone())
            } else {
                None
            }
        };
        if let Some(rollup) = staged {
            self.put_cue_rollup(&rollup).await.inspect_err(|_| {
                self.cue.write().unwrap().dirty = true;
            })?;
        }
        Ok(())
    }

    /// Delete the sealed `cues:v1` rollup — the user's own destruction of their
    /// revocable cue data (Personalization home; owner doc § At rest). Drops the
    /// nest row and resets the live engine to empty, so a later capture starts a
    /// fresh rollup rather than resurrecting the deleted one on the next put.
    pub async fn delete_cue_rollup(&self) -> Result<(), String> {
        PersonalizationClient::new(self.nest.clone())
            .model_delete(CUES_ROLLUP_FACTOR_V1)
            .await
            .map_err(|e| e.to_string())?;
        let mut c = self.cue.write().unwrap();
        c.engine = CueEngine::empty();
        c.dirty = false;
        c.last_put_ms = 0;
        Ok(())
    }

    /// Seal `rollup` under the actor's personalization seal keys and `model_put` it under the
    /// `cues:v1` key (advisory `sample_count` = item count). The nest stores it
    /// verbatim-opaque; `is_topic_factor` rejects `cues:`, so it never composes —
    /// the row rides the sealed-model wire for cross-device continuity only.
    async fn put_cue_rollup(&self, rollup: &CueRollup) -> Result<(), String> {
        let blob = seal_cue_rollup(
            rollup,
            &model_seal_keys(&BackupKey::derive(&self.actor_secret)),
        )
        .map_err(|e| e.to_string())?;
        PersonalizationClient::new(self.nest.clone())
            .model_put(CUES_ROLLUP_FACTOR_V1, blob, rollup.len() as u32)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Does this post match one of the user's muted keywords? The **render**
    /// signal behind the collapse-to-placeholder treatment.
    ///
    /// Independent of ordering: a mute *sinks* a post only in a score-ordered
    /// feed (there is no key to adjust in a chronological one), but it
    /// *collapses* everywhere — which is what "a global muted keyword mutes it
    /// everywhere" (frame § Composition) actually requires of a client.
    pub fn is_muted(&self, post_id: &str) -> bool {
        // Release `state` BEFORE taking `sealed`. `rerank_window` acquires them
        // in the other order (`sealed` → `state`), so holding both here in the
        // reverse order would be a lock-order inversion: a re-rank holding
        // `sealed.read()` and waiting on `state.write()`, against a render
        // holding `state.read()` and waiting on `sealed.read()`, can wedge as
        // soon as a third caller queues a `sealed` write. Cloning one
        // `PostSummary` is cheap next to a deadlock the shells would hit only
        // under load.
        let post = {
            let s = self.state.read().unwrap();
            s.rendered_posts().find(|p| p.post_id == post_id).cloned()
        };
        match post {
            Some(post) => self.sealed.read().unwrap().is_muted(&post),
            None => false,
        }
    }

    // ── Link-preview resolution (D4) ─────────────────────────────

    /// Resolve link-preview metadata (render-model.md § D4) for the bare `url`
    /// carried by a `RenderBlock::LinkPreview { Resolving }` block in a loaded
    /// post. Calls `fauna.linkpreview.resolve` once per URL (the result is cached
    /// in [`resolved_previews`](Self::resolved_previews)), maps the reply onto
    /// `PreviewState`, and notifies so the next [`snapshot`](Self::snapshot)
    /// projects the matching block `Resolved`/`Failed`. A transport error **or** an
    /// explicit `Failed` reply both collapse to the render model's terminal
    /// `Failed` — the client falls back to the plain inline link (a non-retried
    /// failure for render purposes, § D4). Idempotent: a repeat call for an
    /// already-resolved URL is a no-op with NO further notify, the same
    /// render-loop-safe discipline as
    /// [`resolve_quoted_post`](Self::resolve_quoted_post) — a non-fire-once client
    /// observer that re-calls from a notify-driven re-render gets a no-op.
    pub async fn resolve_link_preview(&self, url: String) {
        let nest = self.nest.clone();
        let notified =
            resolve_link_preview_cached(&self.resolved_previews, url.clone(), async move {
                Some(match LinkPreviewClient::new(nest).resolve(url).await {
                    Ok(LinkPreviewResolveReply::Resolved {
                        title,
                        description,
                        image_hash,
                    }) => PreviewState::Resolved {
                        title,
                        description,
                        image_hash,
                        // Blocked-by-default at resolution (render-model.md § D4): the snapshot
                        // reveal walk flips this `true` once the user reveals the post's remote
                        // content.
                        revealed: false,
                    },
                    Ok(LinkPreviewResolveReply::Failed | LinkPreviewResolveReply::Unknown)
                    | Err(_) => PreviewState::Failed,
                })
            })
            .await;
        if notified {
            self.notify();
        }
    }
}

/// Map a wire `FeedPostItem` to a snapshot `PostSummary` (micros→millis on the
/// timestamp; the rest is a 1:1 carry — badges and the quoted-post embed are
/// rendered off `source` / `quoted_post_id` downstream).
/// The card's room reading (`ui/feed.md` § Encryption at rest →
/// *Room-restricted — the app half*, the card bullet): every rendered post
/// whose [`PostSummary::gated_room`] names a room in
/// [`FeedSnapshot::own_rooms`] gets that room's label — the reader's own
/// conversation-list label, never anything the author sent — as its
/// [`PostSummary::room_label`]; every other post's is `None`. Applied on
/// every [`FeedManager::snapshot`] read rather than stored, so the label
/// follows the conversations plane (`refresh_own_rooms`) and a room the
/// device has lost its seat on drops back to the reserved-tier badge with no
/// second bookkeeping path. Covers the deep-link slot for the same reason
/// every other per-post projection does (`FeedSnapshot::rendered_posts`).
fn label_room_posts(snap: &mut FeedSnapshot) {
    let label_of = |room: &str| {
        snap.own_rooms
            .iter()
            .find(|r| r.room == room)
            .map(|r| r.label.clone())
    };
    let labels: Vec<Option<String>> = snap
        .posts
        .iter()
        .map(|p| p.gated_room.as_deref().and_then(label_of))
        .collect();
    for (p, label) in snap.posts.iter_mut().zip(labels) {
        p.room_label = label;
    }
    let deep = snap
        .deep_linked_post
        .as_ref()
        .and_then(|p| p.gated_room.as_deref().and_then(label_of));
    if let Some(p) = snap.deep_linked_post.as_mut() {
        p.room_label = deep;
    }
}

/// Where `me`'s reply to `p` would go — `None` for a public post
/// (`ui/feed.md` § Encryption at rest → *Ruling 5's build — the shape*, (d)).
/// The one place the three-way answer is decided: the snapshot states it and
/// the prepare verb acts on it, so the dialog can never promise an audience the
/// send does not use.
fn reply_audience_of(
    p: &PostSummary,
    own_rooms: &[crate::compose::GateRoomOption],
    own_tiers: &[crate::compose::GateTierOption],
    me_hex: &str,
) -> Option<ReplyAudience> {
    if let Some(room) = p.gated_room.as_deref() {
        // The *rooms offered* test, seat included: `own_rooms` lists only
        // rooms this device can address a post to right now.
        return Some(if own_rooms.iter().any(|r| r.room == room) {
            ReplyAudience::SealedToRoom
        } else {
            ReplyAudience::PublicByConfirmation
        });
    }
    let tier = p.gated_tier.as_deref()?;
    // A subscriber holds a period key too, but a post of theirs gated to
    // another author's tier is no shape the protocol has: only the owner.
    Some(
        if p.author == me_hex && own_tiers.iter().any(|t| t.name == tier) {
            ReplyAudience::SealedToTier
        } else {
            ReplyAudience::PublicByConfirmation
        },
    )
}

/// [`PostSummary::reply_audience`] for every rendered post, derived at every
/// [`FeedManager::snapshot`] read beside [`label_room_posts`] and for the same
/// reason: it follows a lost seat or a retired tier with no second path.
fn state_reply_audiences(snap: &mut FeedSnapshot, me_hex: &str) {
    let answers: Vec<Option<ReplyAudience>> = snap
        .posts
        .iter()
        .map(|p| reply_audience_of(p, &snap.own_rooms, &snap.own_tiers, me_hex))
        .collect();
    for (p, answer) in snap.posts.iter_mut().zip(answers) {
        p.reply_audience = answer;
    }
    let deep = snap
        .deep_linked_post
        .as_ref()
        .and_then(|p| reply_audience_of(p, &snap.own_rooms, &snap.own_tiers, me_hex));
    if let Some(p) = snap.deep_linked_post.as_mut() {
        p.reply_audience = deep;
    }
}

fn map_post(p: FeedPostItem) -> PostSummary {
    // Produce the shared semantic document once, here in the manager (D6 —
    // render-model.md § Where logic lives): clients walk `document` with their
    // conversations document-walker instead of re-parsing `body` at render time.
    // The 500-char FTS preview is raw markdown source (`Post::body_text`
    // indexes the post body's `content`), so `markdown_to_document` is meaningful.
    // At first map the embeds are unresolved, so the document is body-only; the
    // lazy `resolve_quoted_post` / `resolve_media` rebuild it with the
    // `QuotedPost` / `Image` blocks folded in (render-model.md § D6 embed-fold).
    let document = build_post_document(&p.body, None, &[]);
    PostSummary {
        post_id: p.post_id,
        author: p.author,
        // A bridged author's face, carried straight from the wire item
        // (`bridges.md` § Unified feed ingestion → *Bridged authors*).
        author_display: p.author_display.map(Into::into),
        body: p.body,
        document,
        timestamp: p.created_at / 1000,
        tags: p.tags,
        has_media: p.has_media,
        is_reply: p.is_reply,
        source: p.source,
        quoted_post_id: p.quoted_post_id,
        // The repost carrier + per-viewer pair, carried straight from the wire
        // item (`feed.md` § Interaction bar → Repost, ratified 2026-08-10).
        // `viewer_repost_id` is thereafter maintained locally by the `repost`
        // toggle until the next reload.
        reposted_post_id: p.reposted_post_id,
        viewer_repost_id: p.viewer_repost_id,
        viewer_liked: p.viewer_liked,
        // A `FeedPostItem` is the home nest's trusted index projection with no
        // envelope to verify, so neither the verification nor the D10 origin
        // question has a client-authoritative answer yet — both default, and
        // `resolve_media`'s raw-body decode is what fills them in.
        authoring_origin: AuthoringOriginStatus::Unknown,
        // Resolved lazily per `has_media` post by `resolve_media` — the feed
        // item never carries the blob hash (it lives in the post body, which
        // the feed-index projection doesn't read).
        media_hash: None,
        // A `FeedPostItem` is the home nest's trusted index projection — it
        // carries no signed envelope, so the list card is `Unchecked` (no badge,
        // the trusted-home-nest case; `security.md` § Client display of
        // unverified content). The manager flips it only where it actually
        // decodes the raw signed body (`resolve_media`).
        verification: VerificationStatus::Unchecked,
        // Interaction-bar counters carried straight from the wire item (the
        // home nest's trusted index projection); the icon+count bar reads them.
        like_count: p.like_count,
        reply_count: p.reply_count,
        repost_count: p.repost_count,
        quote_count: p.quote_count,
        // Gated-to-tier marker → `gated-post-badge`; the list body of a gated
        // post is its plaintext teaser until `unlock_gated_post` decrypts.
        gated_tier: p.gated_tier,
        // The room a room-restricted post addresses, carried straight from the
        // wire item; its label is derived against `own_rooms` at every
        // `snapshot()` read, never stored.
        gated_room: p.gated_room,
        room_label: None,
        reply_audience: None,
        // Web-publish state → the ⋯ overflow's own-post web verbs. Carried
        // straight from the wire item; no per-row query.
        web_slug: p.web_slug,
        gated_unlocked: false,
        labels: p.labels,
        // Resolved lazily by `resolve_post_unlock_offer` when `gated_tier`
        // names a `post-unlock-*` tier — the feed-index projection has no
        // read for it.
        unlock_offer: None,
        // The nest omits a legally-taken-down post from feed queries outright,
        // so no list item can carry a reference; only the single-post deep-link
        // door (`resolve_post`) ever sees the marker.
        legal_takedown_ref: None,
        // Resolved lazily by `resolve_post_tips` — the feed-index projection
        // carries no tip totals, which is *why* the resolver can't use a data
        // trigger and writes `Some` on every outcome instead.
        tips: None,
    }
}

/// Project a `PostSummary` from a post **fetched by id and decoded** — the
/// deep-link door's twin of [`map_post`] (which projects the nest's feed-index
/// item). Used by [`FeedManager::resolve_post`].
///
/// The two projections must agree about the same post, so every derivation that
/// both ends make is a shared `fauna_core::data::Post` accessor rather than a
/// second walk here: `has_media`, `is_reply`, `indexed_tags` (lowercased — the
/// spelling `content_links` holds) and `quoted_post_id` are exactly what the
/// nest's own indexer writes (`bins/fauna-nest/src/db/mod.rs
/// ::extract_post_metadata`).
///
/// What this path knows *better* than the list path, because it decoded the raw
/// signed envelope and holds the whole body:
/// - `body` is the **full** text, not the nest's 500-char FTS preview;
/// - `verification` / `authoring_origin` carry real answers instead of the
///   list card's `Unchecked`/`Unknown` (F-CL2/F-CL3);
/// - `media_hash` resolves here, with no second `fauna.posts.get`.
///
/// What it cannot know, because those live only in the nest's feed-index
/// projection and no single-post read serves them: the interaction counters,
/// the content labels, the web-publish slug, and the protocol source (defaulted
/// to the create-path `fauna`). None of them are in ui.yaml's
/// `feed.sub_pages.post_detail` element scope, which is what this projection
/// exists to render; a surface that grows to need one of them needs the wire to
/// carry it, not a guess here.
fn map_fetched_post(
    post_id: &str,
    post: &fauna_core::data::Post,
    verification: VerificationStatus,
    authoring_origin: AuthoringOriginStatus,
) -> PostSummary {
    let body = post.body_text();
    let media_hash = first_media_hash(&post.body);
    // Embeds fold in exactly as they do for a list post: the document starts
    // body + media (already known here, and typed — this path holds the decoded
    // body too) and `resolve_quoted_post` rebuilds it with the `QuotedPost` block
    // when the quote resolves (render-model.md § D6).
    let document = build_post_document(&body, None, &media_blocks(&post.body));
    PostSummary {
        post_id: post_id.to_string(),
        author: hex::encode(post.author.0),
        // The raw post carries no bridged-author face; `fauna.posts.get` does
        // not project one yet (captured as the deep-link carrier follow-on to
        // `bridges.md` § Unified feed ingestion → *Bridged authors*), so the
        // deep-linked detail paints the short id where the timeline card
        // paints the name until it does.
        author_display: None,
        body,
        document,
        timestamp: (post.created_at.0 as i64) / 1000,
        tags: post.indexed_tags(),
        has_media: post.has_media(),
        is_reply: post.is_reply(),
        source: "fauna".to_string(),
        quoted_post_id: post.quoted_post_id().map(hex::encode),
        // The shared accessor — the same derivation the nest's indexer makes,
        // so the two doors agree (the no-drift rule above).
        reposted_post_id: post.reposted_post_id().map(hex::encode),
        // The per-viewer pair lives only in the feed-index projection; the
        // deep-link door deliberately doesn't carry it (same class as the
        // counters/labels below — `feed.md` § The read model).
        viewer_repost_id: None,
        viewer_liked: false,
        authoring_origin,
        media_hash,
        verification,
        like_count: 0,
        reply_count: 0,
        repost_count: 0,
        quote_count: 0,
        gated_tier: post.gated.as_ref().map(|g| g.tier.clone()),
        // The same accessor the nest's indexer projects `gated_room` through
        // (`fauna_core::room_post::room_post_of`), so the two doors name the
        // same room (the no-drift rule above).
        gated_room: post
            .gated
            .as_ref()
            .and_then(|g| fauna_core::room_post::room_post_of(&g.key_access))
            .map(|(room, _)| hex::encode(room)),
        room_label: None,
        reply_audience: None,
        web_slug: None,
        gated_unlocked: false,
        labels: Vec::new(),
        unlock_offer: None,
        // Same lazy resolve as the list door — the deep-linked post owes its
        // tip surface exactly as a list post does (`fire_resolves` walks the
        // RENDERED set, which includes the deep-link slot).
        tips: None,
        // A decoded post is a *live* post: `resolve_post` answers a takedown
        // from the reply's marker before it ever reaches this projection, via
        // `PostSummary::taken_down`.
        legal_takedown_ref: None,
    }
}

/// Build a feed post's `PostSummary.document` from its body plus any resolved
/// embeds (render-model.md § D6 embed-fold). The body markdown is the base
/// (`fauna_core::render::markdown_to_document`); a resolved quoted post folds in
/// as a [`RenderBlock::QuotedPost`] and a resolved media blob hash as a
/// [`RenderBlock::Image`], **after** the body in the order all seven apps
/// already render (body → quoted post → media). Both embeds are lazily resolved
/// (`resolve_quoted_post` / `resolve_media`), so this is re-run on each
/// resolution to rebuild the document, then the manager re-emits — the same
/// lazy-resolve→rebuild→re-emit mechanism the feed already uses (feed.md § The
/// read model). Pure (no fetch); the resolution paths are unchanged (D2) —
/// only the embeds' *placement* moves into the document.
pub(crate) fn build_post_document(
    body: &str,
    quote: Option<&QuotedPostView>,
    media: &[RenderBlock],
) -> fauna_core::render::RenderDocument {
    let mut doc = fauna_core::render::markdown_to_document(body);
    if let Some(q) = quote {
        doc.blocks.push(RenderBlock::QuotedPost {
            post_id: q.post_id.clone(),
            author: q.author.clone(),
            body: q.body.clone(),
            // Carry the quoted post's verification straight through so the
            // quoted-embed card paints the "unverified source" badge iff `Failed`
            // (security.md § App display of unverified content). The status is
            // populated on the `project_decoded` fallback (which decodes a raw
            // envelope); a quote already in the loaded page inherits the source
            // card's `Unchecked` (no envelope was decoded).
            verification: q.verification,
            // Same straight-through carry for the D10 audit answer, so the
            // quoted-embed card paints the `delegated-origin-badge` iff the
            // QUOTED post was authored by an external app — independently of
            // whatever the focal card's own origin is.
            authoring_origin: q.authoring_origin,
            // Carry the legal-takedown reference straight through so the
            // quoted-embed card renders the shared tombstone in place of the
            // (withheld, empty) body iff `Some` (moderation.md § Categories &
            // enforcement item 1). `None` for every normal quote.
            legal_takedown_ref: q.legal_takedown_ref.clone(),
            // …and the not-found state for a quoted post that is gone
            // (`ui/feed.md` § Post deletion). `false` for every live quote.
            not_found: q.not_found,
        });
    }
    // The resolved media as trusted, content-addressed blocks, in body order — an
    // `Image` or a `Video` per attachment, the branch already made by [`media_blocks`].
    // The alt is empty (a post's `MediaItem` carries none); each app paints the bytes
    // through its existing blob loader, the same per-platform async byte-load as the
    // conversations `Attachment` image.
    doc.blocks.extend(media.iter().cloned());
    doc
}

/// Fold an opened gated body onto its rendered post — the text, the media the
/// public preview never carried, and the `has_media` / `media_hash` a gated
/// post's index projection could not know.
///
/// Shared by [`FeedManager::unlock_gated_post`] and
/// [`FeedManager::reapply_unlocked`] so the first unlock and every re-fold after
/// a `reload` paint the same card; before this the two hand-rolled the fold and
/// both took the media from the *document* they were about to overwrite, which
/// for a gated post is always empty (the preview body has no items, so nothing
/// ever folded one in — `resolve_media` never even fires, since the index sees
/// `has_media = false` off that same preview).
///
/// The opened body is the authority for all four, which is why nothing is
/// carried across from the old document: a gated post's media has exactly one
/// source, and it is this body.
fn fold_unlocked_body(
    p: &mut PostSummary,
    body: &fauna_core::data::PostBody,
    quote: Option<&QuotedPostView>,
) {
    p.body = body.text();
    p.has_media = body.has_media();
    p.media_hash = first_media_hash(body);
    p.document = build_post_document(&p.body, quote, &media_blocks(body));
    p.gated_unlocked = true;
}

/// Decode a post body fetched **by id** (`fauna.posts.get`) into the post and what this
/// client could verify of it — the one decode the three by-id paths share
/// (`resolve_media`, the quoted-post fallback, the deep link).
///
/// A signed envelope verifies only if validly signed **and** the post asked for
/// (`decode_post_fetched_as`): `Verified`/`Failed` plus its authoring origin. A
/// bridge-translated post rests on the nest as a **bare** canonical `Post` with no
/// envelope (Bluesky, ActivityPub and nostr ingest alike); it decodes when its bytes
/// hash to the id asked for, and stays `Unchecked` / `Unknown` — nothing to verify,
/// so no badge (`security.md` § App display of unverified content), the protocol
/// badge carrying its trust class. Before this, every bridged body failed to decode,
/// so no bridged post's media, quote or deep link ever resolved (render-model.md
/// § D6c's e2e witness found it). `None` when the bytes are neither.
fn decode_fetched_post(
    post_id: &str,
    body: &[u8],
) -> Option<(
    fauna_core::data::Post,
    VerificationStatus,
    AuthoringOriginStatus,
)> {
    match decode_post_fetched_as(post_id, body) {
        Ok((post, origin)) => Some((
            post,
            VerificationStatus::from_valid(origin.is_some()),
            AuthoringOriginStatus::from_origin(origin.as_ref()),
        )),
        Err(_) => fauna_client_core::post::decode_bare_post_fetched_as(post_id, body).map(|post| {
            (
                post,
                VerificationStatus::Unchecked,
                AuthoringOriginStatus::Unknown,
            )
        }),
    }
}

/// The post's media as typed render blocks, in body order — `image/*` folds to
/// [`RenderBlock::Image`], `video/*` to [`RenderBlock::Video`]
/// (render-model.md § Implementation status today — the D6 media fold).
///
/// **This is the one place the image-vs-video branch is made**, which is the whole point of
/// the typed variant: before it, `first_media_hash` reduced the `MediaItem` to a blob hash and
/// dropped `media_type`, so `video-thumbnail` was unbuildable from the document and web had to
/// decode the post a *second* time in app code to paint it.
///
/// **Every** item folds, not just the first (priority #4 — the richest existing pattern is
/// web's, which renders them all; `ui.yaml`'s `image-grid` is likewise specced for 1, 2 or 4).
/// An item that is neither image nor video folds to nothing, matching that same web path —
/// no app has ever rendered one, and inventing a block for it here would put an untyped
/// affordance in front of all 7 walkers.
///
/// **A bridged item — no blob (a zero `blob_hash`) and a `remote_url` — folds here too**
/// (render-model.md § D6c): an `image/*` one to [`RenderBlock::ProxiedImage`] and a `video/*`
/// one to [`RenderBlock::ProxiedVideo`], each carrying the nest-relative path the reader's own
/// nest serves it at. An absolute `https://` `remote_url` (a row written before the ingest
/// rewrite, a writer that forgot) goes through the shared
/// [`shared_media_proxy_url`](fauna_core::data::shared_media_proxy_url) first, so the snapshot
/// never carries a cross-origin URL; one that is neither nest-relative nor rewritable folds to
/// nothing, as does any other zero-hash type (audio), exactly as for a blob item.
///
/// **Each block's `alt` is its item's own `alt`** (what a bridge ingest path wrote from the
/// remote attachment's description), empty when the item has none. The post-level `alt_text`
/// of `PostBody::Media` is that body's *text* and is never copied onto a block.
fn media_blocks(body: &fauna_core::data::PostBody) -> Vec<RenderBlock> {
    body.media_items()
        .iter()
        .filter_map(|item| {
            let alt = item.alt.clone().unwrap_or_default();
            if !has_blob(item) {
                let is_image = item.media_type.starts_with("image/");
                if !is_image && !item.media_type.starts_with("video/") {
                    return None;
                }
                let path = proxied_media_path(item.remote_url.as_deref()?)?;
                return Some(if is_image {
                    RenderBlock::ProxiedImage { path, alt }
                } else {
                    RenderBlock::ProxiedVideo { path, alt }
                });
            }
            let hash = hex::encode(item.blob_hash.digest());
            if item.media_type.starts_with("image/") {
                Some(RenderBlock::Image { hash, alt })
            } else if item.media_type.starts_with("video/") {
                Some(RenderBlock::Video { hash, alt })
            } else {
                None
            }
        })
        .collect()
}

/// Whether a media item's bytes live in a blob the nest stores — a bridged item's zero
/// `blob_hash` says they do not (its bytes sit behind `remote_url`).
fn has_blob(item: &fauna_core::data::MediaItem) -> bool {
    item.blob_hash.digest() != [0u8; 32]
}

/// The nest-relative path a bridged item's `remote_url` is fetched at (bridges.md § Unified
/// feed ingestion ruling 4): already nest-relative → as-is; absolute `https://` → the shared
/// proxy form; anything else → `None`.
fn proxied_media_path(remote_url: &str) -> Option<String> {
    let url = remote_url.trim();
    if url.starts_with('/') && !url.starts_with("//") {
        Some(url.to_string())
    } else {
        fauna_core::data::shared_media_proxy_url(url)
    }
}

/// The first blob-carrying media attachment's hash (64-hex) across the body variants that
/// carry media — the shared analogue of each app's `first_media_hash` (Phase 1 renders one
/// image per post, matching the single `post-image` element, so only the first is used).
///
/// It is also the fire-once `resolve_media` guard, so a post with media never resolves to
/// `None`: one whose attachments carry no blob at all (every item a bridged `remote_url`)
/// resolves to `Some("")` (render-model.md § D6c; feed.md § State & data shape), and its
/// pictures paint from the document's [`RenderBlock::ProxiedImage`] blocks instead.
fn first_media_hash(body: &fauna_core::data::PostBody) -> Option<String> {
    let items = body.media_items();
    if items.is_empty() {
        return None;
    }
    Some(
        items
            .iter()
            .find(|item| has_blob(item))
            .map(|item| hex::encode(item.blob_hash.digest()))
            .unwrap_or_default(),
    )
}

/// Every post the snapshot renders, mutably — the timeline list chained with the
/// deep-link slot, the `&mut` twin of [`FeedSnapshot::rendered_posts`]. Each
/// per-post lazy resolve writes through this, so a deep-linked post gets the same
/// media fold, unseal, offer and badge treatment as a post the feed delivered.
///
/// Ranking paths deliberately do **not** use it (the scored window, exemplar
/// picking, `rerank_loaded_window`): those are about the ordered list, and a post
/// reached by deep link holds no position in it.
fn rendered_posts_mut(s: &mut FeedSnapshot) -> impl Iterator<Item = &mut PostSummary> {
    s.posts.iter_mut().chain(s.deep_linked_post.iter_mut())
}

/// The audience class a reply / quote / repost states to
/// `build_referencing_post` for its target. Either projection alone marks the
/// post restricted: `gated_tier` is non-`None` for a tier post **and** for a
/// room post (the reserved tier `room`), and `gated_room` still says so where
/// only the deep-link door's own decode filled the row.
/// The i18n key an app paints for a manager error that is a **stated refusal**
/// rather than a failure, `None` for every other error text. The verbs return
/// `Result<(), String>` across the FFI/wasm faces, so the refusal travels as
/// its stable text and this is the one place that recognizes it — an app leg
/// resolves the key and falls back to the raw text.
/// What [`FeedManager::reply_public_confirmed`] answers for a reply this
/// device could have sealed to its room or tier: no dialog offers the public
/// answer there, so a confirmed-public call is a caller's mistake, and the
/// words are not sent past the arm. A stable string, like
/// [`REFERENCE_REFUSED_RESTRICTED`](fauna_client_core::post::REFERENCE_REFUSED_RESTRICTED).
pub const REPLY_SEALS_INSTEAD: &str =
    "this reply can be sealed to the post's own audience — send it without confirming";

pub fn refusal_i18n_key(err: &str) -> Option<&'static str> {
    (err == fauna_client_core::post::REFERENCE_REFUSED_RESTRICTED)
        .then_some("feed.reference_restricted")
}

/// The test-only one-shot reload hold behind
/// [`FeedManager::hold_next_reload_for_test`]. A waker-registering park, so it
/// wakes under any executor (tokio natively, the browser's on wasm) — the unit
/// tests' mock gate re-polls instead, which only its own executor tolerates.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Default)]
struct ReloadHold {
    inner: std::sync::Mutex<ReloadHoldState>,
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Default)]
struct ReloadHoldState {
    /// The next reload to reach the park point parks there.
    armed: bool,
    /// The parked reload may proceed.
    released: bool,
    waker: Option<std::task::Waker>,
}

#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl ReloadHold {
    fn arm(&self) {
        let mut s = self.inner.lock().unwrap();
        s.armed = true;
        s.released = false;
    }

    fn release(&self) {
        let mut s = self.inner.lock().unwrap();
        s.armed = false;
        s.released = true;
        if let Some(waker) = s.waker.take() {
            waker.wake();
        }
    }

    fn is_armed(&self) -> bool {
        self.inner.lock().unwrap().armed
    }

    /// Park until [`Self::release`] if armed, consuming the arm; return at once if not.
    async fn park_if_armed(&self) {
        {
            let mut s = self.inner.lock().unwrap();
            if !std::mem::take(&mut s.armed) {
                return;
            }
        }
        std::future::poll_fn(|cx| {
            let mut s = self.inner.lock().unwrap();
            if std::mem::take(&mut s.released) {
                std::task::Poll::Ready(())
            } else {
                s.waker = Some(cx.waker().clone());
                std::task::Poll::Pending
            }
        })
        .await;
    }
}

/// Whether a `fauna.posts.get` error is the nest's "no such post for this caller"
/// answer — matched on the wire code through the [`RpcErrorClass`] seam, never the
/// detail text, so a transport fault ("the network is down") can never read as "the
/// post is gone".
///
/// [`RpcErrorClass`]: fauna_protocol::RpcErrorClass
fn is_post_not_found<E: fauna_protocol::RpcErrorClass>(e: &E) -> bool {
    e.as_rpc_error()
        .is_some_and(|r| r.code == fauna_protocol::RpcError::CODE_POST_NOT_FOUND)
}

fn referenced_audience(p: &PostSummary) -> ReferencedAudience {
    if p.gated_tier.is_some() || p.gated_room.is_some() {
        ReferencedAudience::Restricted
    } else {
        ReferencedAudience::Public
    }
}

/// Append `items` onto `into`, dropping any whose `post_id` is already present
/// (the only dedup the client does — guards the cursor-boundary repeat case).
/// Order is preserved: the nest supplies it, the manager never re-sorts.
fn append_deduped(into: &mut Vec<PostSummary>, items: Vec<PostSummary>) {
    let mut seen: std::collections::HashSet<String> =
        into.iter().map(|p| p.post_id.clone()).collect();
    for item in items {
        if seen.insert(item.post_id.clone()) {
            into.push(item);
        }
    }
}

/// Tokenize the raw comma-separated tags field into normalized tag tokens
/// (trimmed, leading `#` stripped, empties dropped) for `build_post`'s facet
/// param — the shared tag handling, replacing each app's ad-hoc hashtag
/// munging.
fn normalize_tags(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|t| t.trim().trim_start_matches('#').trim())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// Process + seal one compose attachment for `audience` into the multipart
/// parts the app POSTs — the one tail every audience arm of
/// `FeedManager::seal_compose_attachment` shares.
fn seal_compose_upload(
    raw: &[u8],
    audience: &fauna_media::audience::Audience,
) -> ComposeAttachmentUpload {
    let (payload, media_type) = fauna_media::pipeline::process_and_seal_with_mime(raw, audience);
    let sealed = payload.primary_sidecar.class.is_aead_sealed();
    let (primary, thumbnail) = payload.into_multipart_parts();
    ComposeAttachmentUpload {
        primary: ComposeUploadPart {
            sidecar_cbor: primary.sidecar_cbor,
            bytes: primary.bytes,
        },
        thumbnail: thumbnail.map(|t| ComposeUploadPart {
            sidecar_cbor: t.sidecar_cbor,
            bytes: t.bytes,
        }),
        media_type,
        sealed,
    }
}

/// A composer room id (`FeedComposeState::gate_room`, hex) as the 32-byte
/// channel id the room-post seam takes.
fn parse_room_id(hex_id: &str) -> Result<[u8; 32], String> {
    hex::decode(hex_id)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
        .ok_or_else(|| format!("compose room {hex_id:?} is not a 32-byte channel id"))
}

/// Project a staged `AttachedFile` into the `MediaItem` `submit_post` inlines.
/// No file ⇒ `Ok(None)` (the text path). A present `blob_hash` is the
/// lowercase-hex 32-byte BLAKE3 digest of the uploaded blob; a malformed one
/// is a compose error — and so is a file with **no** hash: every submit path
/// has already refused that handle by name
/// (`FeedManager::refuse_unresolved_attachment`), so reaching it here is an
/// invariant break, and it errs rather than taking the text path it used to
/// fall back to (a post published without the file the author picked, with
/// no word to them — `feed.md` § Persistence). `media_type` defaults to
/// `application/octet-stream`; `dimensions` / `thumbnail` are not part of the
/// staged metadata (matching the per-app media writers, which leave them
/// `None`).
fn media_item_from_staged(
    file: Option<&AttachedFile>,
) -> Result<Option<fauna_core::data::MediaItem>, String> {
    let Some(file) = file else { return Ok(None) };
    let Some(hash_hex) = file.blob_hash.as_deref() else {
        return Err(format!(
            "staged attachment {:?} has no uploaded blob",
            file.name
        ));
    };
    let digest: [u8; 32] = fauna_core::hex32::decode(hash_hex)
        .map_err(|_| "staged blob hash must be 32-byte lowercase hex".to_string())?;
    Ok(Some(fauna_core::data::MediaItem {
        blob_hash: fauna_core::data::ContentHash::from_digest_raw(digest),
        media_type: file
            .media_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string()),
        size_bytes: file.size,
        dimensions: None,
        thumbnail: None,
        remote_url: None,
        alt: None,
    }))
}

/// Build the typed wire `rules` list from the create-feed form rules by
/// encoding each through the shared `encode_filter_rule` (which builds the
/// real `FilterRule`, so a rule can't silently misencode). An unknown rule type
/// is an `Err`.
fn encode_rules(rules: &[FilterRuleInput]) -> Result<Vec<FilterRule>, String> {
    rules
        .iter()
        .map(|r| encode_filter_rule(&r.rule_type, &r.value, r.required))
        .collect()
}

/// Split the `feed-factor-*` editor's entries into this feed's own
/// `composition` (local, `global == false`) and the caller's global factor
/// set (`global == true`) — each container validated independently via the
/// shared `fauna_core::scoring::validate_composition` contract (≤ 64 unique
/// non-empty factors; a factor may legitimately repeat *across* the two
/// containers — arithmetic sum is the composition model's conflict
/// resolution, `content-moderation-and-ranking.md` § Composition). An empty
/// local list becomes `None` (no composition), matching the wire's
/// leave-unspecified convention.
fn split_factors(
    factors: Vec<FactorWeightInput>,
) -> Result<(Option<Vec<FeedCompositionEntry>>, Vec<FeedCompositionEntry>), String> {
    let mut local = Vec::new();
    let mut global = Vec::new();
    for f in factors {
        let entry = FeedCompositionEntry {
            factor: f.factor,
            weight_permille: f.weight_permille,
            extra: Default::default(),
        };
        if f.global {
            global.push(entry);
        } else {
            local.push(entry);
        }
    }
    validate_entries(&local)?;
    validate_entries(&global)?;
    let composition = if local.is_empty() { None } else { Some(local) };
    Ok((composition, global))
}

/// Pre-validate a composition container against the shared domain contract
/// before it ever reaches the wire (the nest re-validates authoritatively;
/// this only turns a client-side mistake into an immediate form error instead
/// of a round-trip).
fn validate_entries(entries: &[FeedCompositionEntry]) -> Result<(), String> {
    let domain: Vec<fauna_core::scoring::CompositionEntry> = entries
        .iter()
        .map(|e| fauna_core::scoring::CompositionEntry {
            factor: e.factor.clone(),
            weight_permille: e.weight_permille,
        })
        .collect();
    fauna_core::scoring::validate_composition(&domain).map_err(|e| e.to_string())
}

/// Merge `updates` into the caller's existing global factor set (upsert by
/// factor key) and write the merged set back. `fauna.feed.factors.set` is a
/// **whole-set overwrite** (`feed.md` § Frame reconciliation), so a
/// create-feed dialog toggling "apply to all feeds" for one or two factors
/// must never silently drop every other global factor the user has already
/// set — hence the read-merge-write instead of a blind `set(updates)`.
async fn apply_global_factors<R: RpcRequester>(
    feed: &FeedClient<R>,
    updates: Vec<FeedCompositionEntry>,
) -> Result<(), String> {
    let mut merged = feed
        .feed_factors_get()
        .await
        .map_err(|e| e.to_string())?
        .factors;
    for update in updates {
        match merged.iter_mut().find(|e| e.factor == update.factor) {
            Some(existing) => existing.weight_permille = update.weight_permille,
            None => merged.push(update),
        }
    }
    validate_entries(&merged)?;
    feed.feed_factors_set(merged)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hand-written verdict → label projection obeys the unknown arm's
    /// duty: a verdict this build does not name trains as no example
    /// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
    /// full*).
    #[test]
    fn an_unknown_verdict_trains_as_no_example() {
        assert_eq!(
            example_label_from_verdict(&CueVerdict::Other("rewatch".into())),
            None
        );
        assert_eq!(
            example_label_from_verdict(&CueVerdict::WatchComplete),
            Some(ExampleLabel::MoreLikeThis)
        );
        assert_eq!(
            example_label_from_verdict(&CueVerdict::Skip),
            Some(ExampleLabel::LessLikeThis)
        );
    }
    // The raw model enum: the tests build fixtures directly on `TopicModel`,
    // while the manager's own API speaks `TrainVerb` (the FFI-safe vocabulary).
    use fauna_text_model::topic::ExampleLabel;

    use crate::test_support::cats_model;
    use fauna_client_testkit::block_on;

    use fauna_client_feed::feed::*;
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    // ── Mock transport ───────────────────────────────────────────
    //
    // A `RpcRequester` that answers each `fauna.*` kind with a configured reply
    // and records the requests, so the manager's state machine can be driven
    // without a real nest (the established `RecordingRequester` pattern from
    // `fauna-client-feed`, runs on every target incl. wasm — transport-free).

    #[derive(Default)]
    struct MockInner {
        /// FIFO of post pages `(items, next_cursor)` answered to
        /// feed.posts / feed.local.posts in order.
        pages: VecDeque<(Vec<FeedPostItem>, Option<i64>)>,
        /// Reload-race gate: while armed, the FIRST `fauna.feed.local.posts`
        /// request PARKS (returns `Pending`, consuming the arm) until
        /// `open_gate` — letting a test drive a NEWER reload to completion
        /// while an older one is mid-fetch, then release the stale fetch and
        /// assert its result is dropped. Manual-poll friendly (no waker).
        gate_armed: bool,
        gate_open: bool,
        feeds: Vec<FeedSummary>,
        /// Recorded (kind, raw payload bytes) for every request.
        calls: Vec<(String, Vec<u8>)>,
        /// When set, the *next* request fails with this message.
        fail_next: Option<String>,
        /// When set, every request for this kind fails with this message.
        /// Kind-scoped (not "the next call"), because `reload` now issues
        /// pre-flight reads — `fauna.feed.get`, `fauna.feed.factors.get`,
        /// `fauna.personalization.model.fetch` — before the
        /// page query, and a test that means "the *page query* failed" must say
        /// so rather than race whichever call happens to go first.
        fail_kind: Option<(String, String)>,
        /// `labeler_id → (artifact_kind, artifact bytes)` answered to
        /// `fauna.labelers.inspect` — the read a subscribed **client-evaluated**
        /// labeler's compose seam makes at feed (re)load.
        labeler_artifacts: std::collections::HashMap<Vec<u8>, (String, Vec<u8>)>,
        /// Bytes answered to `fauna.posts.get`.
        posts_get_body: Vec<u8>,
        /// Per-`post_id` bytes for `fauna.posts.get`. When non-empty it takes
        /// precedence over [`Self::posts_get_body`], and an **unseeded** id
        /// answers `not_found` — which is how a test builds a corpus where some
        /// marked posts are fetchable and others are not.
        posts_get_by_id: std::collections::HashMap<String, Vec<u8>>,
        /// The reply answered to `fauna.posts.room_labels` and to its relayed
        /// twin `fauna.posts.room_labels_remote` alike — `None` is the empty
        /// answer a nest gives a caller off every floor.
        room_post_labels_reply: Option<fauna_protocol::posts::PostRoomLabelsReply>,
        /// The `counts` answered to `fauna.posts.interact`. `None` is the
        /// bridged-source / `unrepost` case the client must treat
        /// as "leave the rendered counts alone".
        interact_counts: Option<fauna_protocol::posts::PostEngagementCounts>,
        /// When set, `fauna.posts.get` answers a **legal-takedown** reply: the
        /// body is withheld (empty) and `legal_takedown` carries this reference
        /// (the taken-down-post wire shape — moderation.md § Categories item 1).
        posts_get_legal_takedown: Option<String>,
        /// Subscriptions answered to `fauna.bridges.feeds.list`.
        bridge_subs: Vec<fauna_protocol::bridges_ui::FeedSubscription>,
        /// Bridge-list answered to `fauna.bridges.list` (the build/runtime-gated
        /// available-bridge set the feed selector reads).
        bridges: Vec<fauna_protocol::bridges_ui::BridgeStatus>,
        /// Reply answered to `fauna.linkpreview.resolve`; `None` => `Failed`.
        linkpreview_reply: Option<LinkPreviewResolveReply>,
        /// Reply answered to `fauna.tips.list`. `None` => the default (zero
        /// total, zero count) — which is what an untipped post AND a nest with
        /// no tip mechanism compiled in both really answer.
        #[cfg(feature = "payments")]
        tips_reply: Option<fauna_protocol::tips::TipsListReply>,
        /// The caller's global factor set, answered to `fauna.feed.factors.get`
        /// (pre-seed to prove `apply_global_factors`'s upsert-merge never drops
        /// a pre-existing entry).
        global_factors: Vec<FeedCompositionEntry>,
        /// The stored composition answered to `fauna.feed.get` — what makes a
        /// feed score-ordered (`resolve_effective_composition`).
        feed_composition: Option<Vec<FeedCompositionEntry>>,
        /// `factor` → sealed model blob, the `personalization_models` table.
        /// A `put` writes here, so a fetch-after-train reads what was actually
        /// sealed onto the wire — the round-trip, not a stub.
        models: HashMap<String, Vec<u8>>,
        /// The keyset cursor `(key, created_at)` the next `fauna.feed.posts`
        /// reply carries in its `order=score` branch. `None` ⇒ last page.
        score_cursor: Option<(i64, i64)>,
        /// The caller's own tiers answered to `fauna.subscriptions.tiers.list`
        /// (the `compose-gate-tier-select` option set).
        tiers: Vec<fauna_protocol::subscriptions::TierItem>,
        /// `(version, blob_hash, blob_data)` answered to
        /// `fauna.subscriptions.key_blob.get`; `None` ⇒ panic (seed it).
        key_blob_reply: Option<(u64, Vec<u8>, Vec<u8>)>,
        /// The Layer-B opt-in state answered to `signal_share.status` — flipped by
        /// `signal_share.set`. The producer's cache mirrors this after a status
        /// read / set (engagement-cues.md § Layer B).
        share_signals: bool,
    }

    #[derive(Default)]
    struct MockNest {
        inner: Mutex<MockInner>,
        /// The author's period-key custody (`fauna.state.subscriptions`),
        /// with the production door's join semantics — installed on every
        /// manager [`mgr`] builds.
        period_keys: fauna_client_subscriptions::period_keys::MemoryPeriodKeyStore,
        /// The account's preference records, as the account store answers
        /// them — installed on every manager [`mgr`] builds.
        preferences: Arc<TestPreferences>,
    }

    /// An in-memory [`fauna_client_config::PreferenceStore`]: the moderation
    /// and personalization records a test seeds.
    #[derive(Default)]
    struct TestPreferences {
        moderation: Mutex<fauna_core::data::ModerationConfig>,
        personalization: Mutex<fauna_core::data::PersonalizationConfig>,
        /// Every read fails — the account store cannot be read.
        unreadable: AtomicBool,
    }

    impl TestPreferences {
        fn check(&self) -> Result<(), fauna_client_config::StoreError> {
            if self.unreadable.load(Ordering::SeqCst) {
                Err(fauna_client_config::StoreError::Load(
                    "the account runtime is not running".into(),
                ))
            } else {
                Ok(())
            }
        }
    }

    #[async_trait::async_trait]
    impl fauna_client_config::PreferenceStore for TestPreferences {
        async fn moderation(
            &self,
        ) -> Result<fauna_core::data::ModerationConfig, fauna_client_config::StoreError> {
            self.check()?;
            Ok(self.moderation.lock().unwrap().clone())
        }
        async fn personalization(
            &self,
        ) -> Result<fauna_core::data::PersonalizationConfig, fauna_client_config::StoreError>
        {
            self.check()?;
            Ok(self.personalization.lock().unwrap().clone())
        }
        async fn delegation(
            &self,
        ) -> Result<fauna_core::data::DelegationConfig, fauna_client_config::StoreError> {
            Ok(Default::default())
        }
    }

    impl MockNest {
        fn arc() -> Arc<Self> {
            Arc::new(Self::default())
        }
        fn push_page(&self, items: Vec<FeedPostItem>, next: Option<i64>) {
            self.inner.lock().unwrap().pages.push_back((items, next));
        }
        /// Arm the reload-race gate (see `MockInner::gate_armed`).
        fn gate_next_local_page(&self) {
            self.inner.lock().unwrap().gate_armed = true;
        }
        /// Release a parked page request; the next poll of its future completes.
        fn open_gate(&self) {
            self.inner.lock().unwrap().gate_open = true;
        }
        /// Arm `fauna.posts.interact` to answer with these post-act counters.
        fn set_interact_counts(&self, like: i64, reply: i64, repost: i64, quote: i64) {
            self.inner.lock().unwrap().interact_counts =
                Some(fauna_protocol::posts::PostEngagementCounts {
                    like_count: like,
                    reply_count: reply,
                    repost_count: repost,
                    quote_count: quote,
                    extra: Default::default(),
                });
        }
        fn set_linkpreview_reply(&self, reply: LinkPreviewResolveReply) {
            self.inner.lock().unwrap().linkpreview_reply = Some(reply);
        }
        /// Arm `fauna.tips.list` to answer with this reply.
        #[cfg(feature = "payments")]
        fn set_tips_reply(&self, reply: fauna_protocol::tips::TipsListReply) {
            self.inner.lock().unwrap().tips_reply = Some(reply);
        }
        /// Give the feed a stored composition — what makes `fauna.feed.get`
        /// report it as score-ordered.
        fn set_feed_composition(&self, entries: Vec<FeedCompositionEntry>) {
            self.inner.lock().unwrap().feed_composition = Some(entries);
        }
        /// Seed a trained model, sealed under the same seal keys the manager
        /// derives from `TEST_SECRET` — so the manager unseals a blob that was
        /// really sealed, not a plaintext stand-in.
        fn seed_model(&self, factor: &str, model: &TopicModel) {
            let blob = seal_topic_model(model, &test_seal_key()).unwrap();
            self.inner
                .lock()
                .unwrap()
                .models
                .insert(factor.to_string(), blob);
        }
        /// The sealed blob currently stored for `factor`, unsealed — the
        /// assertion surface for "the train actually reached the nest".
        fn stored_model(&self, factor: &str) -> Option<TopicModel> {
            let blob = self.inner.lock().unwrap().models.get(factor).cloned()?;
            Some(unseal_topic_model(&blob, &test_seal_key()).unwrap())
        }
        /// Seed the user's muted-keyword list into the account's moderation
        /// record.
        fn seed_muted_keywords(&self, words: &[&str]) {
            self.preferences.moderation.lock().unwrap().muted_keywords =
                words.iter().map(|w| (*w).into()).collect();
        }
        /// Seed the artifact `fauna.labelers.inspect` answers for `labeler_id` —
        /// what the compose seam fetches for a subscribed tier-3 labeler, since
        /// a client-evaluated kind has no nest-side bus row to read.
        fn seed_labeler_artifact(&self, labeler_id: [u8; 32], kind: &str, artifact: Vec<u8>) {
            self.inner
                .lock()
                .unwrap()
                .labeler_artifacts
                .insert(labeler_id.to_vec(), (kind.to_string(), artifact));
        }
        /// Register one trained factor (`id` → its `topic:<hex>` key) in the
        /// account's trained-topic registry, with the v2 `learn_from_engagement` flag —
        /// what [`FeedManager::load_sealed_scorers`] reads to decide which composed
        /// factors a cue verdict trains.
        fn seed_trained_factor(&self, id: [u8; 16], learn_from_engagement: bool) {
            self.preferences
                .personalization
                .lock()
                .unwrap()
                .trained_factors
                .push(fauna_core::data::TrainedFactorMeta {
                    id: id.to_vec(),
                    name: "Cats".into(),
                    learn_from_engagement,
                    created_at: 0,
                });
        }
        /// Seed one own tier into `fauna.subscriptions.tiers.list`.
        fn seed_tier(&self, name: &str, rank: u32) {
            self.inner
                .lock()
                .unwrap()
                .tiers
                .push(fauna_protocol::subscriptions::TierItem {
                    name: name.into(),
                    rank,
                    description: None,
                    price_hint: None,
                    payment_url: None,
                    auto_approve: false,
                    created_at: fauna_core::data::Timestamp(0),
                    unlocks_post: None,
                    asking_price: None,
                    hidden: false,
                    extra: Default::default(),
                });
        }
        /// Seed one HIDDEN own tier (`monetization.md` § The unifying model —
        /// *A tier may be hidden*) into `fauna.subscriptions.tiers.list`.
        fn seed_hidden_tier(&self, name: &str, rank: u32) {
            self.inner
                .lock()
                .unwrap()
                .tiers
                .push(fauna_protocol::subscriptions::TierItem {
                    name: name.into(),
                    rank,
                    hidden: true,
                    ..Default::default()
                });
        }
        /// Seed Pillar-1 custody for `tier` (period key `key`, version 1) into
        /// the period-key store — what the gated compose/unlock reads.
        fn seed_custody(&self, tier: &str, key: [u8; 32]) {
            let kp = ActorKeypair::from_secret(TEST_SECRET);
            let mut custody = fauna_core::data::SubscriptionsConfig::default();
            fauna_client_subscriptions::custody::record_new_tier(
                &mut custody,
                kp.actor_id(),
                tier,
                key,
                1,
            );
            block_on(fauna_client_subscriptions::PeriodKeyStore::merge_custody(
                &self.period_keys,
                custody,
            ))
            .unwrap();
        }
        /// Seed the `fauna.subscriptions.key_blob.get` reply.
        fn seed_key_blob(&self, version: u64, hash: Vec<u8>, data: Vec<u8>) {
            self.inner.lock().unwrap().key_blob_reply = Some((version, hash, data));
        }
        /// A model blob that is NOT openable under the manager's key — the
        /// corruption / newer-layout case.
        fn seed_unopenable_model(&self, factor: &str) {
            let wrong_key = model_seal_keys(&BackupKey::derive(&[0xEEu8; 32]));
            let blob = seal_topic_model(&TopicModel::new(), &wrong_key).unwrap();
            self.inner
                .lock()
                .unwrap()
                .models
                .insert(factor.to_string(), blob);
        }
        /// Seed a sealed `cues:v1` rollup under the manager's seal keys — the
        /// round-trip fetch source `hydrate_cues` unseals (not a plaintext stub).
        fn seed_cue_rollup(&self, rollup: &CueRollup) {
            let blob = seal_cue_rollup(rollup, &test_seal_key()).unwrap();
            self.inner
                .lock()
                .unwrap()
                .models
                .insert(CUES_ROLLUP_FACTOR_V1.to_string(), blob);
        }
        /// The sealed `cues:v1` blob currently stored, unsealed — the "the put
        /// actually reached the nest" assertion surface. `None` ⇒ no row.
        fn stored_cue_rollup(&self) -> Option<CueRollup> {
            let blob = self
                .inner
                .lock()
                .unwrap()
                .models
                .get(CUES_ROLLUP_FACTOR_V1)
                .cloned()?;
            Some(unseal_cue_rollup(&blob, &test_seal_key()).unwrap())
        }
        /// A `cues:v1` blob NOT openable under the manager's key — the corruption
        /// case that `hydrate_cues` must surface as an error, never a fresh rollup.
        fn seed_unopenable_cue_rollup(&self) {
            let wrong_key = model_seal_keys(&BackupKey::derive(&[0xEEu8; 32]));
            let blob = seal_cue_rollup(&CueRollup::new(), &wrong_key).unwrap();
            self.inner
                .lock()
                .unwrap()
                .models
                .insert(CUES_ROLLUP_FACTOR_V1.to_string(), blob);
        }
        fn calls(&self) -> Vec<(String, Vec<u8>)> {
            self.inner.lock().unwrap().calls.clone()
        }
        fn kinds(&self) -> Vec<String> {
            self.inner
                .lock()
                .unwrap()
                .calls
                .iter()
                .map(|(k, _)| k.clone())
                .collect()
        }
        /// Decode the recorded payload for the (first) call of `kind`.
        fn req<T: DeserializeOwned>(&self, kind: &str) -> T {
            let g = self.inner.lock().unwrap();
            let (_, bytes) = g
                .calls
                .iter()
                .find(|(k, _)| k == kind)
                .unwrap_or_else(|| panic!("no recorded call for {kind}"));
            fauna_protocol::decode_strict(bytes).expect("decode recorded payload")
        }
    }

    #[derive(Debug)]
    enum MockErr {
        /// A plain transport fault carrying no wire code (`fail_next`/`fail_kind`
        /// injection).
        Fault(String),
        /// A **server rejection** — a nest answer carrying a wire code (e.g.
        /// `CODE_POST_NOT_FOUND`), classified through `is_rejection` rather than
        /// treated as a code-less transport fault.
        Rejected(fauna_protocol::RpcError),
    }
    impl std::fmt::Display for MockErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Fault(msg) => write!(f, "{msg}"),
                Self::Rejected(e) => write!(f, "fake rejection ({})", e.code),
            }
        }
    }
    impl fauna_protocol::RpcErrorClass for MockErr {
        fn is_rejection(&self) -> bool {
            matches!(self, Self::Rejected(_))
        }
        fn as_rpc_error(&self) -> Option<&fauna_protocol::error::RpcError> {
            match self {
                Self::Fault(_) => None,
                Self::Rejected(e) => Some(e),
            }
        }
    }

    // Impl on `MockNest` itself; `Arc<MockNest>` (what `FeedManager` holds) gets
    // `RpcRequester` via fauna-protocol's blanket `impl for Arc<T>`, and is
    // `Clone`. Interior mutability is the `Mutex`, so `&self` suffices.
    impl RpcRequester for MockNest {
        type Error = MockErr;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, MockErr>
        where
            Req: Serialize,
            Reply: DeserializeOwned,
        {
            // Reload-race gate: park OUTSIDE the mutex so a later (newer)
            // request can be answered while this one is held.
            if kind == "fauna.feed.local.posts" {
                let armed = {
                    let mut g = self.inner.lock().unwrap();
                    std::mem::take(&mut g.gate_armed)
                };
                if armed {
                    std::future::poll_fn(|_cx| {
                        if self.inner.lock().unwrap().gate_open {
                            std::task::Poll::Ready(())
                        } else {
                            std::task::Poll::Pending
                        }
                    })
                    .await;
                }
            }
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let reply_bytes = {
                let mut g = self.inner.lock().unwrap();
                g.calls.push((kind.to_string(), bytes.to_vec()));
                if let Some(msg) = g.fail_next.take() {
                    return Err(MockErr::Fault(msg));
                }
                if let Some((k, msg)) = &g.fail_kind
                    && k == kind
                {
                    return Err(MockErr::Fault(msg.clone()));
                }
                match kind {
                    "fauna.feed.list" => fauna_protocol::encode_canonical(&FeedListReply {
                        feeds: g.feeds.clone(),
                        extra: Default::default(),
                    }),
                    "fauna.feed.local.posts" => {
                        let (posts, cursor) = g.pages.pop_front().unwrap_or_default();
                        fauna_protocol::encode_canonical(&FeedLocalPostsReply {
                            posts,
                            cursor,
                            extra: Default::default(),
                        })
                    }
                    "fauna.feed.posts" => {
                        let (posts, cursor) = g.pages.pop_front().unwrap_or_default();
                        let (score_cursor, score_cursor_created_at) = match g.score_cursor {
                            Some((k, t)) => (Some(k), Some(t)),
                            None => (None, None),
                        };
                        fauna_protocol::encode_canonical(&FeedPostsReply {
                            posts,
                            cursor,
                            score_cursor,
                            score_cursor_created_at,
                            extra: Default::default(),
                        })
                    }
                    "fauna.feed.trending.posts" => {
                        // The trending read is always score-ordered — it carries
                        // the keyset cursor pair, never a chronological one.
                        let (posts, _cursor) = g.pages.pop_front().unwrap_or_default();
                        let (score_cursor, score_cursor_created_at) = match g.score_cursor {
                            Some((k, t)) => (Some(k), Some(t)),
                            None => (None, None),
                        };
                        fauna_protocol::encode_canonical(&FeedTrendingPostsReply {
                            posts,
                            score_cursor,
                            score_cursor_created_at,
                            extra: Default::default(),
                        })
                    }
                    "fauna.feed.create" => fauna_protocol::encode_canonical(&FeedCreateReply {
                        feed_id: "feed-new".into(),
                        extra: Default::default(),
                    }),
                    "fauna.feed.delete" => fauna_protocol::encode_canonical(&FeedDeleteReply {
                        extra: Default::default(),
                    }),
                    "fauna.posts.create" => {
                        fauna_protocol::encode_canonical(&fauna_protocol::posts::PostCreateReply {
                            post_id: "ab".repeat(32),
                            extra: Default::default(),
                        })
                    }
                    "fauna.posts.delete" => {
                        fauna_protocol::encode_canonical(&fauna_protocol::posts::PostDeleteReply {
                            post_id: "ab".repeat(32),
                            deleted: true,
                            extra: Default::default(),
                        })
                    }
                    "fauna.posts.interact" => {
                        fauna_protocol::encode_canonical(&fauna_protocol::posts::PostInteractReply {
                            action: "like".into(),
                            source: "fauna".into(),
                            result: r#"{"ok":true}"#.into(),
                            counts: g.interact_counts.clone(),
                            extra: Default::default(),
                        })
                    }
                    "fauna.posts.get" => {
                        // A legal-takedown reply withholds the body (empty) and
                        // carries the takedown reference; otherwise the seeded
                        // body — the per-id one when the test seeded a corpus of
                        // differing posts, else the single shared body.
                        let (body, legal_takedown) = match &g.posts_get_legal_takedown {
                            Some(reference) => (
                                Vec::new(),
                                Some(fauna_protocol::posts::LegalTakedownMarker {
                                    reference: reference.clone(),
                                    extra: Default::default(),
                                }),
                            ),
                            None if !g.posts_get_by_id.is_empty() => {
                                let req: fauna_protocol::posts::PostGetRequest =
                                    fauna_protocol::decode_strict(&bytes).unwrap();
                                match g.posts_get_by_id.get(&req.post_id) {
                                    Some(b) => (b.clone(), None),
                                    // An id the test did NOT seed stands for an
                                    // unfetchable post: the corpus read must drop
                                    // it, so answer not_found rather than a body —
                                    // as the nest does, a rejection carrying the
                                    // wire code, never a code-less fault.
                                    None => {
                                        return Err(MockErr::Rejected(fauna_protocol::RpcError::new(
                                            fauna_protocol::RpcError::CODE_POST_NOT_FOUND,
                                            "error.posts.not_found",
                                        )));
                                    }
                                }
                            }
                            None => (g.posts_get_body.clone(), None),
                        };
                        fauna_protocol::encode_canonical(&fauna_protocol::posts::PostGetReply {
                            body: serde_bytes::ByteBuf::from(body),
                            legal_takedown,
                            extra: Default::default(),
                        })
                    }
                    "fauna.bridges.feeds.list" => fauna_protocol::encode_canonical(
                        &fauna_protocol::bridges_ui::ListFeedsReply {
                            extra: Default::default(),
                            subscriptions: g.bridge_subs.clone(),
                        },
                    ),
                    "fauna.bridges.list" => fauna_protocol::encode_canonical(
                        &fauna_protocol::bridges_ui::ListBridgesReply {
                            extra: Default::default(),
                            bridges: g.bridges.clone(),
                        },
                    ),
                    "fauna.bridges.feeds.create" => fauna_protocol::encode_canonical(
                        &fauna_protocol::bridges_ui::CreateFeedReply {
                            extra: Default::default(),
                            id: 77,
                        },
                    ),
                    "fauna.bridges.feeds.delete" => fauna_protocol::encode_canonical(
                        &fauna_protocol::bridges_ui::DeleteFeedReply {
                            extra: Default::default(),
                            ok: true,
                        },
                    ),
                    "fauna.linkpreview.resolve" => fauna_protocol::encode_canonical(
                        &g.linkpreview_reply
                            .clone()
                            .unwrap_or(LinkPreviewResolveReply::Failed),
                    ),
                    "fauna.feed.factors.get" => fauna_protocol::encode_canonical(
                        &fauna_client_feed::feed::FeedFactorsGetReply {
                            factors: g.global_factors.clone(),
                            extra: Default::default(),
                        },
                    ),
                    "fauna.feed.factors.set" => fauna_protocol::encode_canonical(
                        &fauna_client_feed::feed::FeedFactorsSetReply {
                            extra: Default::default(),
                        },
                    ),
                    "fauna.feed.get" => fauna_protocol::encode_canonical(&FeedGetReply {
                        feed_id: "feed-1".into(),
                        owner: "22".repeat(32),
                        name: "Cats".into(),
                        rules: vec![],
                        combination: "all".into(),
                        created_at: 0,
                        scope: "local".into(),
                        contributor_seeds: vec![],
                        composition: g.feed_composition.clone(),
                        extra: Default::default(),
                    }),
                    "fauna.labelers.inspect" => {
                        let req: fauna_protocol::labelers::InspectLabelerRequest =
                            fauna_protocol::decode_strict(&bytes).unwrap();
                        let (kind, artifact) = g
                            .labeler_artifacts
                            .get(req.labeler_id.as_ref())
                            .cloned()
                            .expect("MockNest: labelers.inspect called with no seeded artifact");
                        fauna_protocol::encode_canonical(
                            &fauna_protocol::labelers::InspectLabelerReply {
                                metadata_blob: Default::default(),
                                wasm_bytes: serde_bytes::ByteBuf::from(artifact),
                                artifact_kind: kind,
                                extra: Default::default(),
                            },
                        )
                    }
                    "fauna.personalization.model.fetch" => {
                        let req: fauna_protocol::personalization::PersonalizationModelFetchRequest =
                            fauna_protocol::decode_strict(&bytes).unwrap();
                        let blob = g.models.get(&req.factor).cloned();
                        fauna_protocol::encode_canonical(
                            &fauna_protocol::personalization::PersonalizationModelFetchReply {
                                sample_count: 0,
                                updated_at: 0,
                                sealed_blob: blob.map(serde_bytes::ByteBuf::from),
                                extra: Default::default(),
                            },
                        )
                    }
                    "fauna.personalization.model.put" => {
                        let req: fauna_protocol::personalization::PersonalizationModelPutRequest =
                            fauna_protocol::decode_strict(&bytes).unwrap();
                        g.models
                            .insert(req.factor.clone(), req.sealed_blob.into_vec());
                        fauna_protocol::encode_canonical(
                            &fauna_protocol::personalization::PersonalizationModelPutReply {
                                status: "ok".into(),
                                extra: Default::default(),
                            },
                        )
                    }
                    "fauna.personalization.model.delete" => {
                        let req: fauna_protocol::personalization::PersonalizationModelDeleteRequest =
                            fauna_protocol::decode_strict(&bytes).unwrap();
                        let deleted = g.models.remove(&req.factor).is_some();
                        fauna_protocol::encode_canonical(
                            &fauna_protocol::personalization::PersonalizationModelDeleteReply {
                                status: "ok".into(),
                                deleted,
                                extra: Default::default(),
                            },
                        )
                    }
                    "fauna.subscriptions.tiers.list" => fauna_protocol::encode_canonical(
                        &fauna_protocol::subscriptions::TiersListReply {
                            tiers: g.tiers.clone(),
                            extra: Default::default(),
                        },
                    ),
                    "fauna.subscriptions.tiers.create" => {
                        let req: fauna_protocol::subscriptions::TierCreateRequest =
                            fauna_protocol::decode_strict(&bytes).unwrap();
                        // Mirror the nest's UNIQUE(name) constraint.
                        if g.tiers.iter().any(|t| t.name == req.name) {
                            return Err(MockErr::Fault(
                                "fauna.subscriptions.tier_already_exists".into(),
                            ));
                        }
                        g.tiers.push(fauna_protocol::subscriptions::TierItem {
                            name: req.name.clone(),
                            rank: req.rank,
                            description: req.description.clone(),
                            price_hint: req.price_hint.clone(),
                            payment_url: req.payment_url.clone(),
                            auto_approve: req.auto_approve,
                            created_at: fauna_core::data::Timestamp(0),
                            unlocks_post: req.unlocks_post.clone(),
                            asking_price: None,
                            hidden: req.hidden,
                            extra: Default::default(),
                        });
                        fauna_protocol::encode_canonical(
                            &fauna_protocol::subscriptions::TierCreateReply {
                                created: true,
                                extra: Default::default(),
                            },
                        )
                    }
                    "fauna.subscriptions.key_blob.get" => {
                        let (version, hash, data) = g
                            .key_blob_reply
                            .clone()
                            .expect("MockNest: key_blob.get called with no seeded reply");
                        fauna_protocol::encode_canonical(
                            &fauna_protocol::subscriptions::KeyBlobGetReply {
                                version,
                                blob_hash: serde_bytes::ByteBuf::from(hash),
                                blob_data: serde_bytes::ByteBuf::from(data),
                                extra: Default::default(),
                            },
                        )
                    }
                    // Derived straight from `g.tiers`, so seeding a tier via
                    // `tiers.create` is enough to answer this read too, with no
                    // separate seed call.
                    //
                    // ⚠ **Deliberately weaker than the real nest**, which
                    // resolves through `CacheDb::get_tier_selling_post` and
                    // requires BOTH directions to agree — the post must be
                    // gated to the tier *and* the tier designate the post
                    // (`monetization.md` § Per-post pay-to-unlock → *The
                    // designation is corroboration*). This double holds no
                    // posts, so it can only see the designation half. That is
                    // fine for the client legs it serves (they seed a
                    // well-formed sold post), but never read this arm as a
                    // statement that the designation alone decides a sale: on a
                    // real nest it does not, and assuming so is the
                    // defect.
                    "fauna.subscriptions.post_unlock.get" => {
                        let req: fauna_protocol::subscriptions::PostUnlockGetRequest =
                            fauna_protocol::decode_strict(&bytes).unwrap();
                        let offer = g
                            .tiers
                            .iter()
                            .find(|t| t.unlocks_post.as_deref() == Some(req.post_id.as_str()))
                            .map(|t| fauna_protocol::subscriptions::PostUnlockOffer {
                                tier_name: t.name.clone(),
                                price_hint: t.price_hint.clone(),
                                payment_url: t.payment_url.clone(),
                                extra: Default::default(),
                            });
                        fauna_protocol::encode_canonical(
                            &fauna_protocol::subscriptions::PostUnlockGetReply {
                                offer,
                                extra: Default::default(),
                            },
                        )
                    }
                    #[cfg(feature = "payments")]
                    "fauna.tips.list" => fauna_protocol::encode_canonical(
                        &g.tips_reply.clone().unwrap_or_default(),
                    ),
                    // Both verdict doors answer the same seeded reply: the
                    // relayed one IS the room home's answer, forwarded. Which
                    // kind the manager picked is asserted off `calls`.
                    "fauna.posts.room_labels" | "fauna.posts.room_labels_remote" => {
                        fauna_protocol::encode_canonical(
                            &g.room_post_labels_reply.clone().unwrap_or_default(),
                        )
                    }
                    // A minimal no-capabilities nest.info — `subscribe_publishing_ek`'s
                    // PQ-hybrid probe, harmless without it (degrades to no encaps key).
                    "fauna.nest.info" => fauna_protocol::encode_canonical(
                        &fauna_protocol::discovery::NestInfoReply {
                            domain: "test".into(),
                            nest_id: String::new(),
                            version: String::new(),
                            software: "fauna".into(),
                            protocols: vec!["fauna".into()],
                            capabilities: vec![],
                            iroh_relay_url: None,
                            subhandles: false,
                            registration: None,
                            moderation: fauna_protocol::discovery::ModerationInfo {
                                extra: Default::default(),
                            },
                            ..Default::default()
                        },
                    ),
                    // A client-minted tier (every `post-unlock-*` tier, and any
                    // other tier this mock seeded) is never `auto_approve` —
                    // mirrors the real nest, which always queues those for the
                    // author's own §2 approve.
                    "fauna.subscriptions.subscribe" => {
                        let req: fauna_protocol::subscriptions::SubscribeRequest =
                            fauna_protocol::decode_strict(&bytes).unwrap();
                        let auto = g
                            .tiers
                            .iter()
                            .find(|t| t.name == req.tier)
                            .is_some_and(|t| t.auto_approve);
                        let reply = if auto {
                            fauna_protocol::subscriptions::SubscribeReply::Approved {
                                tier: req.tier,
                                expires_at: None,
                            }
                        } else {
                            fauna_protocol::subscriptions::SubscribeReply::Queued { request_id: 1 }
                        };
                        fauna_protocol::encode_canonical(&reply)
                    }
                    // ── Layer-B signal sharing (engagement-cues.md § Layer B) ──
                    "fauna.moderation.signal_share.set" => {
                        let req: fauna_protocol::moderation::ModerationSignalShareSetRequest =
                            fauna_protocol::decode_strict(&bytes).unwrap();
                        g.share_signals = req.share;
                        fauna_protocol::encode_canonical(
                            &fauna_protocol::moderation::ModerationSignalShareSetReply {
                                share: req.share,
                                extra: Default::default(),
                            },
                        )
                    }
                    "fauna.moderation.signal_share.status" => fauna_protocol::encode_canonical(
                        &fauna_protocol::moderation::ModerationSignalShareStatusReply {
                            share: g.share_signals,
                            // The transparency export is nest-wide + ≥k-gated; a
                            // single-contributor mock never crosses k, so it stays
                            // empty (the producer tests assert on the recorded call,
                            // not the aggregate — that is `test_signal_sharing.py`).
                            published: Vec::new(),
                            extra: Default::default(),
                        },
                    ),
                    "fauna.moderation.signal_contribute" => {
                        let req: fauna_protocol::moderation::ModerationSignalContributeRequest =
                            fauna_protocol::decode_strict(&bytes).unwrap();
                        fauna_protocol::encode_canonical(
                            &fauna_protocol::moderation::ModerationSignalContributeReply {
                                status: "recorded".into(),
                                signal: req.signal,
                                extra: Default::default(),
                            },
                        )
                    }
                    other => panic!("MockNest: unhandled kind {other}"),
                }
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply_bytes).expect("decode reply"))
        }
    }

    fn post(id: &str, created_at_micros: i64) -> FeedPostItem {
        FeedPostItem {
            post_id: id.into(),
            author: "22".repeat(32),
            body: "body".into(),
            created_at: created_at_micros,
            tags: vec![],
            has_media: false,
            is_reply: false,
            source: "fauna".into(),
            like_count: 0,
            reply_count: 0,
            repost_count: 0,
            quote_count: 0,
            score: None,
            quoted_post_id: None,
            gated_tier: None,
            ..Default::default()
        }
    }

    /// The identity seed every test manager runs on — the BackupKey the sealed
    /// scorers derive from, so the mock can seal blobs the manager really opens.
    const TEST_SECRET: [u8; 32] = [7u8; 32];

    fn test_seal_key() -> fauna_core::crypto::DelegableKindKeys {
        model_seal_keys(&BackupKey::derive(&TEST_SECRET))
    }

    fn mgr(nest: Arc<MockNest>) -> FeedManager<Arc<MockNest>> {
        let period_keys = nest.period_keys.shared();
        let preferences: fauna_client_config::SharedPreferenceStore = nest.preferences.clone();
        let m = FeedManager::new(nest, TEST_SECRET);
        m.set_period_key_store(period_keys);
        m.set_preference_store(preferences);
        m
    }

    const CATS: &str = "topic:aabbccddeeff00112233445566778899";
    /// The 16-byte id whose `topic_factor` key is [`CATS`] — used to register
    /// the factor in the sealed registry (with `learn_from_engagement`).
    const CATS_ID: [u8; 16] = [
        0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        0x99,
    ];

    fn entry(factor: &str, weight_permille: i64) -> FeedCompositionEntry {
        FeedCompositionEntry {
            factor: factor.to_string(),
            weight_permille,
            extra: Default::default(),
        }
    }

    /// A post carrying a nest-served composed key (micro-units) — what
    /// `order=score` returns and what the sealed seam adjusts.
    fn scored_post(id: &str, created_at_micros: i64, score_micro: i64, body: &str) -> FeedPostItem {
        let mut p = post(id, created_at_micros);
        p.score = Some(score_micro);
        p.body = body.to_string();
        p
    }

    fn ids(m: &FeedManager<Arc<MockNest>>) -> Vec<String> {
        m.snapshot()
            .posts
            .iter()
            .map(|p| p.post_id.clone())
            .collect()
    }

    #[test]
    fn select_local_feed_loads_page_one_and_maps_fields() {
        let nest = MockNest::arc();
        // created_at is micros; the snapshot timestamp is millis.
        nest.push_page(vec![post("aa", 1_700_000_000_000_000)], Some(123));
        let m = mgr(nest.clone());

        block_on(m.select_feed(None));
        let snap = m.snapshot();
        assert_eq!(snap.status, FeedStatus::Loaded);
        assert_eq!(snap.posts.len(), 1);
        assert_eq!(snap.posts[0].post_id, "aa");
        assert_eq!(snap.posts[0].timestamp, 1_700_000_000_000); // micros/1000
        assert!(snap.has_more); // a cursor was returned
        assert_eq!(snap.selected_feed, None);
        // The local feed was queried, not the custom-feed kind.
        assert!(nest.kinds().contains(&"fauna.feed.local.posts".to_string()));
    }

    #[test]
    fn select_custom_feed_uses_feed_posts_kind() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));
        assert_eq!(m.snapshot().selected_feed.as_deref(), Some("feed-1"));
        assert!(!m.snapshot().has_more); // no cursor ⇒ no more
        let req: FeedPostsRequest = nest.req("fauna.feed.posts");
        assert_eq!(req.feed_id, "feed-1");
    }

    // ── Trending virtual feed (trending.md § The Trending feed) ──────────────

    #[test]
    fn select_trending_feed_uses_trending_kind_and_sets_selection() {
        let nest = MockNest::arc();
        nest.push_page(vec![scored_post("t1", 9, 5_000_000, "a hot post")], None);
        let m = mgr(nest.clone());

        block_on(m.select_trending_feed());
        let snap = m.snapshot();
        assert_eq!(snap.status, FeedStatus::Loaded);
        assert_eq!(ids(&m), vec!["t1"]);
        // The two selection fields point at the virtual feed, never a custom one
        // — the `None` = local mirror (`selected_feed` cleared, the flag set).
        assert!(snap.trending_selected, "trending is the selection");
        assert_eq!(
            snap.selected_feed, None,
            "trending clears any custom feed id"
        );
        // The virtual Trending read was queried — not the local or custom kind.
        assert!(
            nest.kinds()
                .contains(&"fauna.feed.trending.posts".to_string())
        );
        assert!(!nest.kinds().contains(&"fauna.feed.local.posts".to_string()));
        assert!(!nest.kinds().contains(&"fauna.feed.posts".to_string()));
    }

    #[test]
    fn trending_feed_paginates_on_the_keyset_cursor() {
        let nest = MockNest::arc();
        nest.push_page(vec![scored_post("t1", 9, 5_000_000, "hot")], None);
        // Page 1's reply carries the keyset cursor; page 2 must echo both halves.
        {
            let mut g = nest.inner.lock().unwrap();
            g.score_cursor = Some((5_000_000, 9));
        }
        let m = mgr(nest.clone());
        block_on(m.select_trending_feed());
        assert!(m.snapshot().has_more, "a score cursor means another page");

        {
            let mut g = nest.inner.lock().unwrap();
            g.score_cursor = None; // page 2 is the last
        }
        nest.push_page(vec![scored_post("t2", 8, 1_000_000, "warm")], None);
        block_on(m.load_more());

        let reqs: Vec<FeedTrendingPostsRequest> = nest
            .calls()
            .iter()
            .filter(|(k, _)| k == "fauna.feed.trending.posts")
            .map(|(_, b)| fauna_protocol::decode_strict(b).unwrap())
            .collect();
        assert_eq!(reqs.len(), 2);
        // Page 2 echoes BOTH keyset halves — the trending read carries no
        // chronological cursor and no `order` field (it is always score-ordered).
        assert_eq!(reqs[1].score_cursor, Some(5_000_000), "the key half");
        assert_eq!(
            reqs[1].score_cursor_created_at,
            Some(9),
            "the tiebreak half — without it a flat-keyed feed never advances",
        );
        assert_eq!(
            ids(&m),
            vec!["t1", "t2"],
            "page 2 appended in nest order (never re-sorted client-side)",
        );
    }

    #[test]
    fn select_feed_clears_trending_and_vice_versa() {
        let nest = MockNest::arc();
        nest.push_page(vec![scored_post("t1", 9, 5_000_000, "hot")], None); // trending
        nest.push_page(vec![post("c1", 1)], None); // then a custom feed
        let m = mgr(nest.clone());

        block_on(m.select_trending_feed());
        assert!(m.snapshot().trending_selected);

        block_on(m.select_feed(Some("feed-1".into())));
        let snap = m.snapshot();
        assert!(
            !snap.trending_selected,
            "a custom-feed select clears trending"
        );
        assert_eq!(snap.selected_feed.as_deref(), Some("feed-1"));
    }

    #[test]
    fn load_more_appends_dedupes_by_post_id_and_preserves_order() {
        let nest = MockNest::arc();
        // Page 1 ends at "bb"; page 2 repeats "bb" at the cursor boundary then "cc".
        nest.push_page(vec![post("aa", 3), post("bb", 2)], Some(2));
        nest.push_page(vec![post("bb", 2), post("cc", 1)], None);
        let m = mgr(nest.clone());

        block_on(m.select_feed(None));
        assert_eq!(
            m.snapshot()
                .posts
                .iter()
                .map(|p| p.post_id.clone())
                .collect::<Vec<_>>(),
            vec!["aa", "bb"]
        );
        block_on(m.load_more());
        // "bb" is not duplicated; order is the nest order, never re-sorted.
        assert_eq!(
            m.snapshot()
                .posts
                .iter()
                .map(|p| p.post_id.clone())
                .collect::<Vec<_>>(),
            vec!["aa", "bb", "cc"]
        );
        assert!(!m.snapshot().has_more);
    }

    #[test]
    fn load_more_is_a_noop_when_no_further_page() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None); // no cursor ⇒ no more
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let before = nest.kinds().len();
        block_on(m.load_more());
        // No extra query was issued.
        assert_eq!(nest.kinds().len(), before);
    }

    #[test]
    fn reveal_remote_images_projects_revealed_onto_the_revealed_post_only() {
        // D3 (render-model.md § D3): feed remote-image reveal is manager-owned and
        // symmetric with conversations — `reveal_remote_images(post_id)` opts a post in,
        // and the next `snapshot()` projects `RemoteImage.revealed:true` for THAT post
        // only (covering both its list card and detail, which read the same post).
        let nest = MockNest::arc();
        let mut a = post("aa", 2);
        a.body = "a ![x](http://img.test/a.png)".into();
        let mut b = post("bb", 1);
        b.body = "b ![y](http://img.test/b.png)".into();
        nest.push_page(vec![a, b], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        // Before any reveal: both posts' documents carry a blocked remote image.
        let snap = m.snapshot();
        assert_eq!(snap.posts.len(), 2);
        for p in &snap.posts {
            assert!(
                p.document.has_blocked_remote_images(),
                "feed remote images are blocked by default",
            );
        }

        // Reveal only post "aa".
        m.reveal_remote_images("aa".into());
        let snap = m.snapshot();
        let by_id = |id: &str| snap.posts.iter().find(|p| p.post_id == id).unwrap();
        assert!(
            !by_id("aa").document.has_blocked_remote_images(),
            "the revealed post's remote images are projected revealed",
        );
        assert!(
            by_id("bb").document.has_blocked_remote_images(),
            "an un-revealed post stays blocked (reveal is scoped per post id)",
        );
    }

    // ── D4: link-preview resolution ──────────────────────────────────────────

    fn link_preview_state(
        snap: &FeedSnapshot,
        post_id: &str,
        url: &str,
    ) -> Option<fauna_core::render::PreviewState> {
        use fauna_core::render::RenderBlock;
        snap.posts
            .iter()
            .find(|p| p.post_id == post_id)?
            .document
            .blocks
            .iter()
            .find_map(|b| match b {
                RenderBlock::LinkPreview { url: u, state } if u == url => Some(state.clone()),
                _ => None,
            })
    }

    #[test]
    fn resolve_link_preview_folds_resolved_state_into_the_document() {
        use fauna_core::render::PreviewState;
        // A post whose body is a bare URL gets a `LinkPreview { Resolving }` block
        // from the producer; `resolve_link_preview` calls the kind and the next
        // snapshot projects the resolved title/description/image onto that block
        // (the D3-reveal twin — render-model.md § D4).
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.body = "[https://example.com/a](https://example.com/a)".into();
        nest.push_page(vec![p], None);
        let want_hash = "ab".repeat(32);
        nest.set_linkpreview_reply(LinkPreviewResolveReply::Resolved {
            title: "Example".into(),
            description: "An example page".into(),
            image_hash: Some(want_hash.clone()),
        });
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        // Before resolution the producer's block is `Resolving`.
        assert_eq!(
            link_preview_state(&m.snapshot(), "aa", "https://example.com/a"),
            Some(PreviewState::Resolving),
        );

        block_on(m.resolve_link_preview("https://example.com/a".into()));

        match link_preview_state(&m.snapshot(), "aa", "https://example.com/a") {
            Some(PreviewState::Resolved {
                title,
                description,
                image_hash,
                revealed,
            }) => {
                assert_eq!(title, "Example");
                assert_eq!(description, "An example page");
                assert_eq!(image_hash.as_deref(), Some(want_hash.as_str()));
                // D4 (render-model.md § D4): the og:image is blocked-by-default at resolution.
                assert!(
                    !revealed,
                    "a freshly-resolved og:image is blocked by default"
                );
            }
            other => panic!("expected a Resolved LinkPreview, got {other:?}"),
        }
        // The kind was called exactly once.
        assert_eq!(
            nest.kinds()
                .iter()
                .filter(|k| *k == "fauna.linkpreview.resolve")
                .count(),
            1,
        );

        // The Resolved og:image drives the post's blocked-remote predicate (so the
        // `load-remote-content-button` covers it), and revealing the post un-gates it —
        // the D3 twin (render-model.md § D4).
        assert!(
            m.snapshot().posts[0].document.has_blocked_remote_images(),
            "the un-revealed og:image is blocked remote content",
        );
        m.reveal_remote_images("aa".into());
        match link_preview_state(&m.snapshot(), "aa", "https://example.com/a") {
            Some(PreviewState::Resolved { revealed, .. }) => {
                assert!(
                    revealed,
                    "revealing the post projects revealed:true onto the og:image"
                );
            }
            other => panic!("expected a Resolved LinkPreview, got {other:?}"),
        }
        assert!(
            !m.snapshot().posts[0].document.has_blocked_remote_images(),
            "the revealed og:image is no longer blocked",
        );
    }

    #[test]
    fn resolve_link_preview_maps_failed_to_failed() {
        use fauna_core::render::PreviewState;
        // An explicit `Failed` reply (and, by the same arm, a transport error)
        // maps to the terminal `PreviewState::Failed` — the client falls back to
        // the plain inline link (§ D4).
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.body = "[https://example.com/a](https://example.com/a)".into();
        nest.push_page(vec![p], None);
        nest.set_linkpreview_reply(LinkPreviewResolveReply::Failed);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_link_preview("https://example.com/a".into()));

        assert_eq!(
            link_preview_state(&m.snapshot(), "aa", "https://example.com/a"),
            Some(PreviewState::Failed),
        );
    }

    #[test]
    fn resolve_link_preview_notifies_once_then_is_idempotent() {
        // The resolution notifies exactly once; a repeat call for the cached URL
        // is a no-op with NO further notify and NO second kind call — the same
        // render-loop-safe discipline as `resolve_quoted_post`.
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counter(Arc<AtomicUsize>);
        impl FeedSnapshotObserver for Counter {
            fn on_changed(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.body = "[https://example.com/a](https://example.com/a)".into();
        nest.push_page(vec![p], None);
        nest.set_linkpreview_reply(LinkPreviewResolveReply::Failed);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        // Observe AFTER the initial load so we count only the resolutions.
        let count = Arc::new(AtomicUsize::new(0));
        m.add_observer(Arc::new(Counter(count.clone())));

        block_on(m.resolve_link_preview("https://example.com/a".into()));
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "first resolution notifies once"
        );

        block_on(m.resolve_link_preview("https://example.com/a".into()));
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "a cached re-resolution does not notify (no render loop)",
        );
        assert_eq!(
            nest.kinds()
                .iter()
                .filter(|k| *k == "fauna.linkpreview.resolve")
                .count(),
            1,
            "the cached re-resolution does not re-call the kind",
        );
    }

    #[test]
    fn set_search_query_requeries_with_the_term() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None); // initial
        nest.push_page(vec![post("bb", 1)], None); // re-query
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.set_search_query(Some("rust".into())));
        assert_eq!(m.snapshot().search_query.as_deref(), Some("rust"));
        // The search term rode the re-query, not a local filter.
        let g = nest.inner.lock().unwrap();
        let (_, last) = g.calls.last().unwrap();
        let req: FeedLocalPostsRequest = fauna_protocol::decode_strict(last).unwrap();
        assert_eq!(req.search.as_deref(), Some("rust"));
    }

    /// One manual poll of a future that is allowed to park (unlike `block_on`,
    /// which panics on `Pending`) — the reload-race tests' interleaving tool.
    fn poll_once<F: std::future::Future>(
        fut: &mut std::pin::Pin<Box<F>>,
    ) -> std::task::Poll<F::Output> {
        use std::task::{Context, Waker};
        let mut cx = Context::from_waker(Waker::noop());
        fut.as_mut().poll(&mut cx)
    }

    /// The reload race behind the apple `test_feed_search_filters_posts` red
    /// (2026-07-17): a `clear_search` fired just before a debounced
    /// `set_search_query` (the driver's `clear_and_type` shape) starts a SLOW
    /// unfiltered reload whose result lands AFTER the fast filtered one — and
    /// under last-write-wins it clobbered `state.posts`, leaving a committed
    /// `search_query` with unfiltered posts. Same race: feed A selected just
    /// before feed B. A stale reload's page must be DROPPED at commit.
    #[test]
    fn a_stale_reload_result_never_clobbers_a_newer_one() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1), post("bb", 2)], None); // initial load
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.snapshot().posts.len(), 2);

        // Reload A — the stale one (clear_search's unfiltered re-query): its
        // page fetch PARKS at the gate.
        nest.gate_next_local_page();
        let mut stale = Box::pin(m.clear_search());
        assert!(
            poll_once(&mut stale).is_pending(),
            "reload A must park at the gate"
        );

        // Reload B — the newer debounced search: commits while A is parked.
        nest.push_page(vec![post("filtered", 1)], None);
        block_on(m.set_search_query(Some("needle".into())));
        assert_eq!(m.snapshot().search_query.as_deref(), Some("needle"));
        assert_eq!(m.snapshot().posts.len(), 1);

        // Release A. Its unfiltered result arrives LAST — and must be dropped:
        // the snapshot keeps reload B's committed search + filtered posts.
        nest.push_page(vec![post("aa", 1), post("bb", 2)], None); // A's (stale) reply
        nest.open_gate();
        assert!(
            poll_once(&mut stale).is_ready(),
            "released reload A must complete"
        );
        assert_eq!(
            m.snapshot().search_query.as_deref(),
            Some("needle"),
            "the newer reload's committed search survives"
        );
        assert_eq!(
            m.snapshot()
                .posts
                .iter()
                .map(|p| p.post_id.as_str())
                .collect::<Vec<_>>(),
            vec!["filtered"],
            "a stale reload's page must not clobber the newer reload's posts"
        );
    }

    /// A same-feed REFRESH must not blank the list it is refreshing while its
    /// own fetch is still in flight.
    ///
    /// `reload` clears `state.posts` unconditionally at its start, before the
    /// fetch. For a SELECTION CHANGE that is right — feed A's posts under feed
    /// B's header would be a lie. For a refresh of the feed already on screen
    /// (the reconnect re-hydrate, the post-submit refresh) it is not: it throws
    /// away a list that is still perfectly valid, and if the fetch never lands
    /// — a flapping socket mid-nest-flip, no RPC deadline under it — the reader
    /// is left staring at an EMPTY feed with `status = Loading`, `error = None`
    /// and nothing to retry from. Nothing recovers it: the reconnect that would
    /// re-fire the re-hydrate is suppressed while this one is still running.
    ///
    /// That is the windows `test_nest_flip_feed_rehydrate` shape: the counters prove a re-query committed, the error surface is
    /// empty, and the post the flip was supposed to deliver is not on screen —
    /// because the list it would have joined was cleared by a LATER refresh that
    /// never came back. Windows reaches it first only because it fires the most
    /// re-hydrates (its `Reconnected` is raised by the reconnect pump AND by
    /// every `Notification`/`ResyncRequired` push), but the hole is in the
    /// shared manager and every app inherits it.
    #[test]
    fn a_same_feed_refresh_keeps_its_posts_while_the_fetch_is_in_flight() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1), post("bb", 2)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.snapshot().posts.len(), 2, "baseline: two posts on screen");

        // The reconnect re-hydrate of the SAME feed — its page fetch parks, as
        // one issued into a socket that is still flapping does.
        nest.gate_next_local_page();
        let mut rehydrate = Box::pin(m.refresh_current_feed());
        assert!(
            poll_once(&mut rehydrate).is_pending(),
            "the re-hydrate must park at the gate"
        );

        // While it is in flight the reader still sees the feed they were
        // reading. Blanking here is what strands them when the fetch never
        // lands — with no error to explain it and no reload left to fix it.
        assert_eq!(
            m.snapshot()
                .posts
                .iter()
                .map(|p| p.post_id.as_str())
                .collect::<Vec<_>>(),
            vec!["aa", "bb"],
            "a same-feed refresh must not clear the list while its fetch is in flight"
        );

        // And it still converges: the landed page replaces the list wholesale,
        // so a post deleted server-side does not linger.
        nest.push_page(vec![post("bb", 2), post("probe", 3)], None);
        nest.open_gate();
        assert!(
            poll_once(&mut rehydrate).is_ready(),
            "the released re-hydrate completes"
        );
        assert_eq!(
            m.snapshot()
                .posts
                .iter()
                .map(|p| p.post_id.as_str())
                .collect::<Vec<_>>(),
            vec!["bb", "probe"],
            "the committed page is authoritative: `aa` is gone, `probe` arrived"
        );
    }

    /// The other half of the rule: a SWITCH clears the list up front — the
    /// previous query's posts under the new query are a lie (`ui/feed.md` § The
    /// read model). A changed search term is a switch on the same feed.
    #[test]
    fn a_switch_clears_its_posts_while_the_fetch_is_in_flight() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1), post("bb", 2)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.snapshot().posts.len(), 2, "baseline: two posts on screen");

        nest.gate_next_local_page();
        let mut search = Box::pin(m.set_search_query(Some("zz".into())));
        assert!(
            poll_once(&mut search).is_pending(),
            "the search's re-query must park at the gate"
        );
        assert!(
            m.snapshot().posts.is_empty(),
            "a switch must clear the list before its fetch lands"
        );

        nest.push_page(vec![post("zz", 3)], None);
        nest.open_gate();
        assert!(
            poll_once(&mut search).is_ready(),
            "the released search completes"
        );
        assert_eq!(
            m.snapshot()
                .posts
                .iter()
                .map(|p| p.post_id.as_str())
                .collect::<Vec<_>>(),
            vec!["zz"]
        );
    }

    /// The test-only reload hold behind the e2e witness of the rule above: a held
    /// reload publishes the list it kept, reads as in flight on the generation
    /// counters, and fetches only once released. The arm is consumed by the
    /// reload it parks, so the one after it runs free.
    #[test]
    fn a_held_reload_publishes_its_list_then_waits_for_release() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        m.hold_next_reload_for_test();
        assert!(m.reload_hold_armed_for_test());
        nest.push_page(vec![post("aa", 1), post("bb", 2)], None);
        let mut refresh = Box::pin(m.refresh_current_feed());
        assert!(
            poll_once(&mut refresh).is_pending(),
            "the held reload parks"
        );
        assert!(
            !m.reload_hold_armed_for_test(),
            "the arm is consumed by the reload it parked"
        );
        let (started, _, committed) = m.reload_counts();
        assert!(started > committed, "a parked reload reads as in flight");
        assert_eq!(
            m.snapshot()
                .posts
                .iter()
                .map(|p| p.post_id.as_str())
                .collect::<Vec<_>>(),
            vec!["aa"],
            "a held refresh shows the list it kept"
        );

        m.release_held_reload_for_test();
        assert!(poll_once(&mut refresh).is_ready(), "released, it completes");
        assert_eq!(
            m.snapshot()
                .posts
                .iter()
                .map(|p| p.post_id.as_str())
                .collect::<Vec<_>>(),
            vec!["aa", "bb"]
        );
        let (started, _, committed) = m.reload_counts();
        assert_eq!(started, committed, "and commits");

        // Nothing armed now: the next reload runs straight through.
        nest.push_page(vec![post("cc", 3)], None);
        block_on(m.refresh_current_feed());
        assert_eq!(m.snapshot().posts[0].post_id, "cc");
    }

    /// The `{started, completed, committed_gen}` reload triple behind
    /// `fauna_e2e_agent::FEED_RELOADS_KEY` (convention 14's causal anchor:
    /// `committed_gen > started-at-baseline` proves a re-query that *began
    /// after* the baseline read has landed its result). Sound only if a reload
    /// records itself exactly once, at COMMIT — never at initiation. This pin
    /// parks a reload at the gate and reads `(1, 0, 0)`: a write moved to the initiation
    /// point (the shape that would let a consumer release before the committed
    /// snapshot is readable) reds it there. The Err arm needs no separate pin:
    /// the bump is a single statement after the whole match, so an Ok/Err
    /// asymmetry is unrepresentable without restructuring `reload`.
    #[test]
    fn a_reload_counts_itself_only_at_commit() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        assert_eq!(
            m.reload_counts(),
            (0, 0, 0),
            "fresh manager: no reloads yet"
        );

        nest.gate_next_local_page();
        let mut parked = Box::pin(m.select_feed(None));
        assert!(
            poll_once(&mut parked).is_pending(),
            "reload must park at the gate"
        );
        assert_eq!(
            m.reload_counts(),
            (1, 0, 0),
            "a parked reload has started but must not count as completed"
        );

        nest.push_page(vec![post("aa", 1)], None);
        nest.open_gate();
        assert!(poll_once(&mut parked).is_ready());
        assert_eq!(
            m.reload_counts(),
            (1, 1, 1),
            "the committed reload records itself, generation and all"
        );
        assert_eq!(
            m.snapshot().posts.len(),
            1,
            "a completed bump implies the committed snapshot is readable"
        );
    }

    /// A reload whose result the `reload_gen` guard DROPPED never counts as
    /// completed — otherwise a consumer could credit "a post-baseline re-query
    /// landed" to a reload whose result never reached the snapshot. A mutant
    /// bumping on the superseded early-return path reds the middle assert.
    ///
    /// The tail is the other, harder half, and it is why `committed_gen`
    /// exists. Because a supersede never commits, it widens
    /// `started - completed` **permanently** — so a baseline taken *after* one
    /// leaves `completed > started-at-baseline` unsatisfiable no matter how
    /// healthy the manager is. Web spent a 300 s budget and two sessions on
    /// exactly that shape (measured 2026-08-23: the
    /// reconnect fires two overlapping reloads by construction, the newer
    /// commits and renders its posts, and the barrier still reported "no
    /// re-query ever committed"). `committed_gen` compares generation against
    /// generation and is untouched by the drop.
    #[test]
    fn a_superseded_reload_never_counts_as_completed() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1), post("bb", 2)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.reload_counts(), (1, 1, 1));

        // Reload A parks; reload B supersedes and commits; A's release drops
        // its stale result (the race test above pins the drop itself).
        nest.gate_next_local_page();
        let mut stale = Box::pin(m.clear_search());
        assert!(poll_once(&mut stale).is_pending());
        nest.push_page(vec![post("filtered", 1)], None);
        block_on(m.set_search_query(Some("needle".into())));
        assert_eq!(
            m.reload_counts(),
            (3, 2, 3),
            "B (generation 3) committed; parked A has only started"
        );

        nest.push_page(vec![post("aa", 1)], None); // A's (stale) reply
        nest.open_gate();
        assert!(poll_once(&mut stale).is_ready());
        assert_eq!(
            m.reload_counts(),
            (3, 2, 3),
            "a superseded reload's dropped result is not a completion"
        );

        // ...and now the consumer's own arithmetic, from a baseline taken right
        // here — one supersede behind. A single healthy reload follows and
        // commits.
        let (baseline_started, baseline_completed, _) = m.reload_counts();
        nest.push_page(vec![post("cc", 3)], None);
        block_on(m.clear_search());
        let (started, completed, committed_gen) = m.reload_counts();
        assert_eq!((started, completed, committed_gen), (4, 3, 4));

        assert!(
            completed <= baseline_started,
            "THE BUG this field exists for: the supersede above is never made \
             up, so a commit count can never pass a STARTED baseline again \
             (completed {completed} vs baseline {baseline_started}) — however \
             healthy the manager. A count-based barrier hangs here forever."
        );
        assert!(
            completed > baseline_completed,
            "the reload did commit — the count moved, just not past the wrong \
             baseline"
        );
        assert!(
            committed_gen > baseline_started,
            "the release condition that IS sound: generation {committed_gen} \
             was claimed after the baseline read saw {baseline_started} \
             started, and it committed"
        );
    }

    #[test]
    fn empty_search_term_clears_search() {
        let nest = MockNest::arc();
        nest.push_page(vec![], None);
        nest.push_page(vec![], None);
        let m = mgr(nest);
        block_on(m.select_feed(None));
        block_on(m.set_search_query(Some("   ".into())));
        assert_eq!(m.snapshot().search_query, None);
    }

    #[test]
    fn query_failure_sets_error_status() {
        let nest = MockNest::arc();
        // Fail the PAGE QUERY specifically. `reload` now issues pre-flight reads
        // (composition + sealed scorers) before it, and those are deliberately
        // non-fatal — so "the next call fails" would no longer test what this
        // test is named for.
        nest.inner.lock().unwrap().fail_kind =
            Some(("fauna.feed.local.posts".into(), "nest down".into()));
        let m = mgr(nest);
        block_on(m.select_feed(None));
        let snap = m.snapshot();
        assert_eq!(snap.status, FeedStatus::Error);
        assert!(snap.error.is_some());
        assert!(snap.posts.is_empty());
    }

    /// The pre-flight reads are non-fatal by design: a feed whose composition or
    /// sealed scorers cannot be read still **lists its posts** — only the
    /// personalized re-ranking is missing. But the failure is *surfaced*, never
    /// swallowed: silently dropping the user's muted words would render exactly
    /// the content they asked never to see, with no hint why.
    #[test]
    fn an_unreadable_config_still_loads_the_feed_but_says_the_filters_are_off() {
        let nest = MockNest::arc();
        nest.preferences.unreadable.store(true, Ordering::SeqCst);
        nest.push_page(vec![post("aa", 2), post("bb", 1)], None);
        let m = mgr(nest);
        block_on(m.select_feed(None));

        let snap = m.snapshot();
        assert_eq!(snap.status, FeedStatus::Loaded, "the posts still load");
        assert_eq!(snap.posts.len(), 2);
        assert!(
            snap.error.is_some(),
            "the user must be told their muted words are not being applied",
        );
    }

    #[test]
    fn update_compose_stores_and_clears_prior_error() {
        let nest = MockNest::arc();
        let m = mgr(nest);
        // Seed an error via an empty submit.
        block_on(m.submit_post()).unwrap_err();
        assert!(m.snapshot().compose.error.is_some());
        // Editing clears it.
        m.update_compose("hello".into(), "rust, fauna".into(), None);
        let c = m.snapshot().compose;
        assert_eq!(c.text, "hello");
        assert_eq!(c.tags, "rust, fauna");
        assert!(c.error.is_none());
    }

    #[test]
    fn submit_empty_post_is_rejected_with_a_compose_error() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        m.update_compose("   ".into(), String::new(), None);
        assert!(block_on(m.submit_post()).is_err());
        assert!(m.snapshot().compose.error.is_some());
        // No post was created.
        assert!(!nest.kinds().contains(&"fauna.posts.create".to_string()));
    }

    #[test]
    fn submit_post_builds_signs_creates_and_clears_compose() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None); // the post-submit reload
        let m = mgr(nest.clone());
        m.update_compose("hello world".into(), "rust".into(), None);
        block_on(m.submit_post()).expect("submit ok");

        // A real signed post was sent to fauna.posts.create.
        assert!(nest.kinds().contains(&"fauna.posts.create".to_string()));
        let req: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        assert!(!req.body.is_empty());
        // It decodes + verifies as a Post whose body is our text.
        let (decoded, origin) = decode_post(req.body.as_ref()).expect("decodes");
        assert_eq!(origin, Some(fauna_core::encoding::AuthoringOrigin::Direct));
        match decoded.body {
            fauna_core::data::PostBody::Text { content, .. } => assert_eq!(content, "hello world"),
            other => panic!("expected Text body, got {other:?}"),
        }
        // Compose was cleared and the list refreshed.
        assert_eq!(m.snapshot().compose, Default::default());
        assert_eq!(m.snapshot().status, FeedStatus::Loaded);
    }

    /// A recording [`fauna_client_search::OwnPostIndexObserver`] — what the
    /// trickle pin below asserts through, because the observer call *is* the
    /// contract: the launcher-side staging it fans into is pinned in
    /// `fauna-client-index` (`stage_own_post`), so what this manager owes is
    /// exactly "nest-confirmed create → one call, right id, right text".
    #[derive(Default)]
    struct RecordingPostObserver {
        seen: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl fauna_client_search::OwnPostIndexObserver for RecordingPostObserver {
        fn own_post_created(&self, post_id_hex: &str, body_text: &str) {
            self.seen
                .lock()
                .unwrap()
                .push((post_id_hex.to_string(), body_text.to_string()));
        }
    }

    /// **The posts trickle chokepoint fires on a confirmed create — with the
    /// text of the bytes that were sent** (`content-index.md` § Ingest
    /// triggers, v1 — the posts ruling: *index at create*). The text must come
    /// from the sent bytes' own `body_text`, not the composer state, so the
    /// staged doc can never disagree with what the nest's enumeration rows
    /// would later carry for the same id.
    #[test]
    fn a_confirmed_submit_hands_the_post_to_the_index_trickle() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None); // the post-submit reload
        let m = mgr(nest.clone());
        let observer = Arc::new(RecordingPostObserver::default());
        m.set_post_index_observer(observer.clone());

        m.update_compose("findable the moment it lands".into(), String::new(), None);
        block_on(m.submit_post()).expect("submit ok");

        let seen = observer.seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![(
                "ab".repeat(32), // MockNest's nest-echoed post id
                "findable the moment it lands".to_string()
            )],
            "one confirmed create, one trickle call — nest-echoed id, sent-bytes text"
        );
    }

    /// A failed create hands nothing to the index — staging an id the nest
    /// refused would plant a hit `fauna.posts.get` can never resolve.
    #[test]
    fn a_failed_submit_hands_nothing_to_the_index_trickle() {
        let nest = MockNest::arc();
        nest.inner.lock().unwrap().fail_next = Some("the nest refused the post".into());
        let m = mgr(nest.clone());
        let observer = Arc::new(RecordingPostObserver::default());
        m.set_post_index_observer(observer.clone());

        m.update_compose("never confirmed".into(), String::new(), None);
        assert!(block_on(m.submit_post()).is_err());

        assert!(
            observer.seen.lock().unwrap().is_empty(),
            "no confirmation, no trickle"
        );
    }

    /// Plays the user typing their NEXT post while a plain submit's create is
    /// in flight. The synchronous mock completes the RPC in one poll, so the
    /// only point inside that window where the manager calls out is the index
    /// trickle — fired after the create confirms and before the success arm
    /// clears, which is exactly the moment an edit has to survive.
    struct TypesTheNextPostDuringTheCreate {
        manager: std::sync::OnceLock<std::sync::Weak<FeedManager<Arc<MockNest>>>>,
    }

    impl fauna_client_search::OwnPostIndexObserver for TypesTheNextPostDuringTheCreate {
        fn own_post_created(&self, _post_id_hex: &str, _body_text: &str) {
            if let Some(m) = self.manager.get().and_then(std::sync::Weak::upgrade) {
                m.update_compose("the next post".into(), "rust".into(), None);
            }
        }
    }

    /// `submit_post` clears what it SENT, not the composer as it stands when
    /// the create confirms (`ui/feed.md` § User actions, `post-submit-button`).
    /// It used to reset `FeedComposeState` wholesale there, erasing whatever the
    /// user had typed since the click — on every app rendering its composer
    /// from this snapshot.
    #[test]
    fn an_edit_made_while_the_create_is_in_flight_survives_the_success_clear() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None); // the post-submit reload
        let m = Arc::new(mgr(nest.clone()));
        let observer = Arc::new(TypesTheNextPostDuringTheCreate {
            manager: std::sync::OnceLock::new(),
        });
        let _ = observer.manager.set(Arc::downgrade(&m));
        m.set_post_index_observer(observer);

        m.update_compose("first post".into(), "rust".into(), None);
        block_on(m.submit_post()).expect("submit ok");

        let req: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        let (decoded, _) = decode_post(req.body.as_ref()).expect("decodes");
        match decoded.body {
            fauna_core::data::PostBody::Text { content, .. } => assert_eq!(
                content, "first post",
                "the post carries what was sent, not the edit"
            ),
            other => panic!("expected Text body, got {other:?}"),
        }
        let compose = m.snapshot().compose;
        assert_eq!(
            compose.text, "the next post",
            "typed during the create — must not be erased by its clear"
        );
        assert_eq!(
            compose.tags, "",
            "the unchanged tags went out with the first post, so they clear"
        );
        assert!(!compose.submitting, "the attempt that just landed is over");
    }

    #[test]
    fn submit_post_with_staged_blob_builds_a_text_with_media_post() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None); // the post-submit reload
        let m = mgr(nest.clone());
        let hash_hex = "cd".repeat(32); // a 32-byte blob digest, lowercase hex
        m.update_compose(
            "look at this".into(),
            String::new(),
            Some(AttachedFile {
                name: "photo.png".into(),
                size: 4096,
                blob_hash: Some(hash_hex),
                media_type: Some("image/png".into()),
            }),
        );
        block_on(m.submit_post()).expect("submit ok");

        // The created post is TextWithMedia carrying the staged blob as a
        // MediaItem (the client uploaded the blob + staged the hash; the
        // manager built the post — the uniform media path for all 7 apps).
        let req: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        let (decoded, origin) = decode_post(req.body.as_ref()).expect("decodes");
        assert_eq!(origin, Some(fauna_core::encoding::AuthoringOrigin::Direct));
        match decoded.body {
            fauna_core::data::PostBody::TextWithMedia { content, items, .. } => {
                assert_eq!(content, "look at this");
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].media_type, "image/png");
                assert_eq!(items[0].size_bytes, 4096);
                assert_eq!(
                    items[0].blob_hash,
                    fauna_core::data::ContentHash::from_digest_raw([0xcd; 32])
                );
            }
            other => panic!("expected TextWithMedia body, got {other:?}"),
        }
        assert_eq!(m.snapshot().compose, Default::default());
    }

    // ── A restored draft's attachment is a handle, not a file ───────────────
    // (`feed.md` § Persistence → *Attachments by content address*). This rail
    // uploads a picked file only at submit, so a draft restored after a
    // relaunch, or synced from another device, names a file whose bytes this
    // device never held: `attached_file: Some`, `blob_hash: None`. Every
    // submit path refuses, naming the file, and keeps the draft — it never
    // publishes the text alone.

    fn restored_handle() -> AttachedFile {
        AttachedFile {
            name: "photo.png".into(),
            size: 4096,
            blob_hash: None,
            media_type: Some("image/png".into()),
        }
    }

    fn assert_refused_naming_the_file(m: &FeedManager<Arc<MockNest>>, err: &str) {
        assert!(err.contains("photo.png"), "{err}");
        let snap = m.snapshot();
        assert_eq!(
            snap.compose.error,
            Some(LocalizedText::key_arg(
                "feed.compose_attachment_missing",
                "filename",
                "photo.png",
            ))
        );
        assert!(!snap.compose.submitting, "the composer is handed back");
        assert_eq!(
            snap.compose.attached_file,
            Some(restored_handle()),
            "the draft is kept for the author to attach the file again"
        );
    }

    #[test]
    fn submit_post_refuses_a_restored_attachment_rather_than_posting_without_it() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        m.update_compose(
            "look at this".into(),
            String::new(),
            Some(restored_handle()),
        );
        let err = block_on(m.submit_post()).expect_err("unresolved attachment refused");
        assert_refused_naming_the_file(&m, &err);
        assert!(
            !nest.kinds().contains(&"fauna.posts.create".to_string()),
            "nothing was posted"
        );
        assert_eq!(m.snapshot().compose.text, "look at this");
    }

    #[test]
    fn a_gated_compose_refuses_a_restored_attachment_rather_than_sealing_a_text_only_body() {
        let nest = MockNest::arc();
        nest.seed_tier(TIER, 2);
        let m = mgr(nest.clone());
        m.update_compose("body".into(), String::new(), Some(restored_handle()));
        m.update_compose_gate(Some(TIER.into()), "teaser".into());
        let err = block_on(m.prepare_gated_blob()).expect_err("unresolved attachment refused");
        assert_refused_naming_the_file(&m, &err);
        assert!(m.pending_gated.read().unwrap().is_none(), "nothing staged");
    }

    #[test]
    fn a_room_compose_refuses_a_restored_attachment_rather_than_sealing_a_text_only_body() {
        use fauna_core::room_post::RoomPostSeal;
        let room = [0xC7u8; 32];
        let m = mgr(MockNest::arc());
        m.set_room_post_keys(Arc::new(OneRoomKey {
            room,
            seal: RoomPostSeal::EndToEnd { epoch: 1 },
            base: [0x44u8; 32],
        }));
        m.update_compose("body".into(), String::new(), Some(restored_handle()));
        m.update_compose_room(Some(hex::encode(room)), "teaser".into());
        let err = block_on(m.prepare_gated_blob()).expect_err("unresolved attachment refused");
        assert_refused_naming_the_file(&m, &err);
        assert!(m.pending_gated.read().unwrap().is_none(), "nothing staged");
    }

    #[test]
    fn a_sell_compose_refuses_a_restored_attachment_without_minting_a_tier() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        m.update_compose("body".into(), String::new(), Some(restored_handle()));
        m.update_compose_sell(Some(SellComposeState::default()), "teaser".into());
        let err = block_on(m.prepare_sell_post(None, false, None))
            .expect_err("unresolved attachment refused");
        assert_refused_naming_the_file(&m, &err);
        assert!(
            !nest
                .kinds()
                .contains(&"fauna.subscriptions.tiers.create".to_string()),
            "no tier is minted for a compose that never becomes a post"
        );
        assert!(
            m.pending_sell_tier.read().unwrap().is_none(),
            "nothing staged"
        );
    }

    #[test]
    fn delete_post_signs_a_verifiable_tombstone_and_drops_it_from_the_loaded_window() {
        let post_id = "cc".repeat(32);
        let other_id = "dd".repeat(32);
        let nest = MockNest::arc();
        nest.push_page(vec![post(&post_id, 2), post(&other_id, 1)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.snapshot().posts.len(), 2);

        block_on(m.delete_post(post_id.clone())).expect("delete ok");

        assert!(nest.kinds().contains(&"fauna.posts.delete".to_string()));
        let req: fauna_protocol::posts::PostDeleteRequest = nest.req("fauna.posts.delete");
        let tombstone =
            fauna_core::encoding::decode_tombstone(req.body.as_ref()).expect("decodes + verifies");
        let digest = fauna_core::hex32::decode(&post_id).unwrap();
        assert_eq!(
            tombstone.post_id,
            fauna_core::data::PostId::from_digest_dag_cbor(digest)
        );

        // The deleted post is gone from the loaded window; the other survives.
        let ids: Vec<String> = m
            .snapshot()
            .posts
            .iter()
            .map(|p| p.post_id.clone())
            .collect();
        assert_eq!(ids, vec![other_id]);
    }

    /// `ui/feed.md` § Post deletion: someone else's quote and repost of my post
    /// survive my delete, and where they showed my post they now say it is gone.
    /// Both embeds were resolved from the loaded page — the cached projection
    /// still carries the words — so the delete itself must replace them; no
    /// refetch would (the cache answers first).
    #[test]
    fn deleting_your_post_turns_every_embed_of_it_into_the_not_found_state() {
        use fauna_core::render::RenderBlock;
        let mine = "cc".repeat(32);
        let nest = MockNest::arc();
        let mut their_quote = post("q1", 3);
        their_quote.quoted_post_id = Some(mine.clone());
        let mut their_repost = post("r1", 2);
        their_repost.reposted_post_id = Some(mine.clone());
        let mut my_post = post(&mine, 1);
        my_post.body = "the words I am about to delete".into();
        nest.push_page(vec![their_quote, their_repost, my_post], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let view = block_on(m.resolve_quoted_post(mine.clone())).expect("resolves in-page");
        assert_eq!(view.body, "the words I am about to delete");

        block_on(m.delete_post(mine.clone())).expect("delete ok");

        let posts = m.snapshot().posts;
        assert_eq!(
            posts.iter().map(|p| p.post_id.as_str()).collect::<Vec<_>>(),
            vec!["q1", "r1"],
            "their quote and repost stand; my post is gone"
        );
        for p in &posts {
            match p.document.blocks.last() {
                Some(RenderBlock::QuotedPost {
                    not_found: true,
                    body,
                    ..
                }) => assert!(body.is_empty(), "{}: no deleted words left", p.post_id),
                other => panic!("{}: expected the not-found embed, got {other:?}", p.post_id),
            }
        }
        let cached = block_on(m.resolve_quoted_post(mine)).expect("cached");
        assert!(
            cached.not_found,
            "a later resolve answers the not-found state too"
        );
    }

    /// **The defect this whole path exists to close.** Before it, a reply was
    /// `interact(id, "reply", text)`, whose native nest arm discards `body` —
    /// so the assertion that matters is that a real signed post carrying
    /// `Reference::Reply` reaches `fauna.posts.create`, not merely that some
    /// call was made.
    #[test]
    fn a_reply_composes_a_real_referencing_post_not_a_bare_interact() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        nest.push_page(vec![post(&target, 2)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        block_on(m.reply(target.clone(), "well said".into())).expect("reply ok");

        assert!(
            nest.kinds().contains(&"fauna.posts.create".to_string()),
            "a reply must CREATE a post; kinds seen: {:?}",
            nest.kinds()
        );
        let req: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        let (decoded, _) = decode_post(req.body.as_ref()).expect("decodes + verifies");
        match decoded.body {
            fauna_core::data::PostBody::Text { ref content, .. } => {
                assert_eq!(content, "well said", "the typed text must survive");
            }
            ref other => panic!("expected Text body, got {other:?}"),
        }
        let digest = fauna_core::hex32::decode(&target).unwrap();
        match &decoded.references[..] {
            [fauna_core::data::Reference::Reply { post_id }] => {
                assert_eq!(*post_id, PostId::from_digest_dag_cbor(digest));
            }
            other => panic!("expected exactly one Reply reference, got {other:?}"),
        }
        assert!(decoded.is_reply());
    }

    /// A quote carries `Reference::Quote`, and an empty commentary is
    /// legitimate — § Interaction bar's ratified "direct quote-repost".
    #[test]
    fn a_quote_composes_a_quote_reference_and_accepts_empty_commentary() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        nest.push_page(vec![post(&target, 2)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        block_on(m.quote(target.clone(), String::new())).expect("an empty quote is allowed");

        let req: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        let (decoded, _) = decode_post(req.body.as_ref()).expect("decodes + verifies");
        assert!(
            matches!(
                &decoded.references[..],
                [fauna_core::data::Reference::Quote { .. }]
            ),
            "expected one Quote reference, got {:?}",
            decoded.references
        );
    }

    /// The repost toggle's ON direction (`feed.md` § Interaction bar → Repost,
    /// ratified 2026-08-10): composes an EMPTY-body `Reference::Repost` post
    /// and folds the created post's id into the target row's
    /// `viewer_repost_id` — the id `unrepost` takes, which is what makes the
    /// toggle's OFF direction expressible at all.
    #[test]
    fn a_repost_composes_an_empty_repost_reference_and_folds_viewer_state() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        nest.push_page(vec![post(&target, 2)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.snapshot().posts[0].viewer_repost_id, None);

        block_on(m.repost(target.clone())).expect("repost ok");

        let req: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        let (decoded, _) = decode_post(req.body.as_ref()).expect("decodes + verifies");
        match decoded.body {
            fauna_core::data::PostBody::Text { ref content, .. } => {
                assert_eq!(content, "", "a bare repost has no body of its own");
            }
            ref other => panic!("expected Text body, got {other:?}"),
        }
        let digest = fauna_core::hex32::decode(&target).unwrap();
        match &decoded.references[..] {
            [fauna_core::data::Reference::Repost { post_id }] => {
                assert_eq!(*post_id, PostId::from_digest_dag_cbor(digest));
            }
            other => panic!("expected exactly one Repost reference, got {other:?}"),
        }
        assert_eq!(
            m.snapshot().posts[0].viewer_repost_id,
            Some("ab".repeat(32)),
            "the created repost's id folds into the target row — unrepost's argument"
        );
    }

    /// The toggle's OFF direction: a second tap UN-reposts — it names the
    /// caller's OWN repost post through the interact door, composes nothing,
    /// drops that row from the loaded window (the caller's own just-deleted
    /// post), and clears the viewer state.
    #[test]
    fn a_second_repost_tap_unreposts_the_callers_own_repost() {
        let target = "cc".repeat(32);
        let repost_id = "dd".repeat(32);
        let nest = MockNest::arc();
        let mut target_item = post(&target, 2);
        target_item.viewer_repost_id = Some(repost_id.clone());
        let mut repost_item = post(&repost_id, 3);
        repost_item.body = String::new();
        repost_item.reposted_post_id = Some(target.clone());
        nest.push_page(vec![repost_item, target_item], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        block_on(m.repost(target.clone())).expect("unrepost ok");

        assert!(
            !nest.kinds().contains(&"fauna.posts.create".to_string()),
            "the OFF direction must not compose"
        );
        let req: fauna_protocol::posts::PostInteractRequest = nest.req("fauna.posts.interact");
        assert_eq!(
            req.post_id, repost_id,
            "unrepost names the caller's own repost post, never the original"
        );
        assert_eq!(req.action, "unrepost");
        let snap = m.snapshot();
        assert!(
            !snap.posts.iter().any(|p| p.post_id == repost_id),
            "the just-deleted repost row leaves the window"
        );
        let target_row = snap.posts.iter().find(|p| p.post_id == target).unwrap();
        assert_eq!(target_row.viewer_repost_id, None, "the toggle state clears");
    }

    /// A bridged row keeps the shipped interact path verbatim — the nest's
    /// bridged arms drive the origin protocol's own repost API, so composing
    /// a local fauna post would strand the repost on the wrong network.
    #[test]
    fn a_repost_on_a_bridged_row_keeps_the_interact_path() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        let mut bridged = post(&target, 2);
        bridged.source = "bluesky".into();
        nest.push_page(vec![bridged], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        block_on(m.repost(target.clone())).expect("repost ok");

        assert!(!nest.kinds().contains(&"fauna.posts.create".to_string()));
        let req: fauna_protocol::posts::PostInteractRequest = nest.req("fauna.posts.interact");
        assert_eq!(req.post_id, target);
        assert_eq!(req.action, "repost");
    }

    /// The like toggle's ON direction. A like is **recorded** against the
    /// target (`feed.md` § User actions' two-verb row), so unlike `repost` it
    /// composes nothing — and the row's `viewer_liked` folds on, which is what
    /// makes the OFF direction expressible at all.
    #[test]
    fn a_like_records_the_like_and_folds_viewer_state() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        nest.push_page(vec![post(&target, 2)], None);
        nest.set_interact_counts(1, 0, 0, 0);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert!(!m.snapshot().posts[0].viewer_liked);

        block_on(m.like(target.clone())).expect("like ok");

        assert!(
            !nest.kinds().contains(&"fauna.posts.create".to_string()),
            "a like is RECORDED, never composed"
        );
        let req: fauna_protocol::posts::PostInteractRequest = nest.req("fauna.posts.interact");
        assert_eq!(req.post_id, target);
        assert_eq!(req.action, "like");
        let snap = m.snapshot();
        let row = &snap.posts[0];
        assert!(row.viewer_liked, "the toggle state folds on");
        assert_eq!(
            row.like_count, 1,
            "the count is the nest's post-act value folded by `interact` — never a local +1"
        );
    }

    /// The toggle's OFF direction: a second tap UN-likes. The distinction from
    /// `unrepost` is load-bearing — `unlike` names **the target itself**, not
    /// the caller's own post, so nothing leaves the window and the count
    /// reverses from the nest's own post-act value.
    #[test]
    fn a_second_like_tap_unlikes_through_the_same_door() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        let mut liked = post(&target, 2);
        liked.viewer_liked = true;
        liked.like_count = 1;
        nest.push_page(vec![liked], None);
        nest.set_interact_counts(0, 0, 0, 0);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        block_on(m.like(target.clone())).expect("unlike ok");

        assert!(
            !nest.kinds().contains(&"fauna.posts.create".to_string()),
            "the OFF direction must not compose"
        );
        let req: fauna_protocol::posts::PostInteractRequest = nest.req("fauna.posts.interact");
        assert_eq!(
            req.post_id, target,
            "`unlike` names the TARGET — unlike `unrepost`, which names the caller's own repost"
        );
        assert_eq!(req.action, "unlike");
        let snap = m.snapshot();
        let row = &snap.posts[0];
        assert!(!row.viewer_liked, "the toggle state clears");
        assert_eq!(
            row.like_count, 0,
            "the count reverses from the nest's post-act value, not a local -1"
        );
        assert_eq!(
            snap.posts.len(),
            1,
            "un-liking removes no row — only `unrepost` deletes a post"
        );
    }

    /// A bridged row keeps the shipped one-way `interact(id, "like")` verbatim
    /// (`feed.md` § Interaction bar → Repost: bridged rows generally project
    /// no viewer state, and their interactions live in the origin protocol) —
    /// the same source routing as `repost`, so no app leg has to know.
    #[test]
    fn a_like_on_a_bridged_row_keeps_the_interact_path() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        let mut bridged = post(&target, 2);
        bridged.source = "bluesky".into();
        bridged.viewer_liked = true;
        nest.push_page(vec![bridged], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        block_on(m.like(target.clone())).expect("like ok");

        let req: fauna_protocol::posts::PostInteractRequest = nest.req("fauna.posts.interact");
        assert_eq!(req.post_id, target);
        assert_eq!(
            req.action, "like",
            "a bridged row must not be routed to `unlike` off a viewer field its origin owns"
        );
    }

    /// A post the loaded window cannot answer for falls back to the shipped
    /// path — the safe direction, since it is exactly today's behaviour
    /// (`repost`'s same rule).
    #[test]
    fn a_like_on_a_row_outside_the_window_keeps_the_interact_path() {
        let nest = MockNest::arc();
        nest.push_page(vec![post(&"cc".repeat(32), 2)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        let stranger = "ee".repeat(32);
        block_on(m.like(stranger.clone())).expect("like ok");

        let req: fauna_protocol::posts::PostInteractRequest = nest.req("fauna.posts.interact");
        assert_eq!(req.post_id, stranger);
        assert_eq!(req.action, "like");
    }

    /// A repost row embeds its original through the SAME quoted-post fold —
    /// `quoted-post` is ui.yaml's "quoted/reposted post display" — so the
    /// resolve keyed by the reposted id folds the block into the repost row's
    /// (empty-bodied) document.
    #[test]
    fn resolve_quoted_post_folds_the_embed_into_a_repost_row() {
        let target = "cc".repeat(32);
        let repost_id = "dd".repeat(32);
        let nest = MockNest::arc();
        let mut repost_item = post(&repost_id, 3);
        repost_item.body = String::new();
        repost_item.reposted_post_id = Some(target.clone());
        nest.push_page(vec![repost_item, post(&target, 2)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        let view = block_on(m.resolve_quoted_post(target.clone()));
        assert!(view.is_some(), "the original projects from the loaded set");

        let snap = m.snapshot();
        let repost_row = snap.posts.iter().find(|p| p.post_id == repost_id).unwrap();
        assert!(
            repost_row.document.has_quoted_post(),
            "the embed folds into the repost row's document"
        );
    }

    /// An empty *reply* is refused before any nest call — the user gets an
    /// error rather than an empty post appearing under someone's thread.
    #[test]
    fn an_empty_reply_is_refused_without_calling_the_nest() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        nest.push_page(vec![post(&target, 2)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        assert!(block_on(m.reply(target, "   ".into())).is_err());
        assert!(!nest.kinds().contains(&"fauna.posts.create".to_string()));
    }

    /// A restricted row as the nest projects it: a tier post carries its tier,
    /// a room post the reserved tier `room` plus its room's channel id.
    fn restricted_posts() -> Vec<(&'static str, FeedPostItem)> {
        let mut tier = post(&"c1".repeat(32), 3);
        tier.gated_tier = Some("patrons".into());
        let mut room = post(&"c2".repeat(32), 2);
        room.gated_tier = Some("room".into());
        room.gated_room = Some("ee".repeat(32));
        vec![("tier", tier), ("room", room)]
    }

    /// **The user's words never go public under a restricted post**
    /// (`ui/feed.md` § Encryption at rest → *A reply, quote or repost of a
    /// restricted post*). A reply, and a quote carrying commentary, to a tier
    /// post and to a room post are each refused with the stated reason — and
    /// the refusal lands before anything is created, so nothing reaches the
    /// user's followers.
    #[test]
    fn words_under_a_restricted_post_are_refused_and_nothing_is_created() {
        for (class, item) in restricted_posts() {
            let target = item.post_id.clone();
            let nest = MockNest::arc();
            nest.push_page(vec![item], None);
            let m = mgr(nest.clone());
            block_on(m.select_feed(None));

            let reply = block_on(m.reply(target.clone(), "said in confidence".into()));
            let quote = block_on(m.quote(target.clone(), "said in confidence".into()));
            for (verb, got) in [("reply", reply), ("quote", quote)] {
                assert_eq!(
                    got.expect_err("must be refused"),
                    fauna_client_core::post::REFERENCE_REFUSED_RESTRICTED,
                    "{verb} to a {class} post"
                );
            }
            assert!(
                !nest.kinds().contains(&"fauna.posts.create".to_string()),
                "{class}: a refused reply/quote must create nothing; kinds seen: {:?}",
                nest.kinds()
            );
        }
    }

    // ── Ruling 5's sealed arm (`ui/feed.md` § Encryption at rest → *Ruling 5's
    //    build — the shape*, (c) + (d)) ─────────────────────────────────────

    const SEALED_ROOM: [u8; 32] = [0xE7u8; 32];

    /// A room post addressed to [`SEALED_ROOM`], and a manager seated on it.
    fn seated_member(nest: Arc<MockNest>) -> (FeedManager<Arc<MockNest>>, String) {
        let target = "c2".repeat(32);
        let mut item = post(&target, 2);
        item.gated_tier = Some("room".into());
        item.gated_room = Some(hex::encode(SEALED_ROOM));
        nest.push_page(vec![item], None);
        let m = mgr(nest);
        m.set_room_post_keys(Arc::new(OneRoomKey {
            room: SEALED_ROOM,
            seal: fauna_core::room_post::RoomPostSeal::EndToEnd { epoch: 4 },
            base: [0x44u8; 32],
        }));
        block_on(m.select_feed(None));
        block_on(m.refresh_own_rooms());
        (m, target)
    }

    fn reply_audience(m: &FeedManager<Arc<MockNest>>, id: &str) -> Option<ReplyAudience> {
        m.snapshot()
            .posts
            .iter()
            .find(|p| p.post_id == id)
            .expect("the target is loaded")
            .reply_audience
    }

    /// What the staged sealed reply decodes to once submitted.
    fn created_post(nest: &MockNest) -> fauna_core::data::Post {
        let req: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        decode_post(req.body.as_ref())
            .expect("decodes + verifies")
            .0
    }

    /// **A seated member's reply to a room post seals to the room.** The
    /// snapshot states it, the prepare verb stages it in the one gated slot
    /// under the room arm's sidecar, and the submit is a *reference's* submit:
    /// the target's counter is read back, the feed is not reloaded, and the
    /// composer — whose words these never were — is left exactly as it stood.
    #[test]
    fn a_seated_members_reply_to_a_room_post_is_sealed_to_the_room() {
        use fauna_core::subscription::types::KeyAccess;
        let nest = MockNest::arc();
        nest.set_interact_counts(0, 5, 0, 0);
        let (m, target) = seated_member(nest.clone());
        assert_eq!(
            reply_audience(&m, &target),
            Some(ReplyAudience::SealedToRoom)
        );
        m.update_compose("an unrelated draft".into(), String::new(), None);
        let feed_loads = |n: &MockNest| {
            n.kinds()
                .iter()
                .filter(|k| *k == "fauna.feed.query")
                .count()
        };
        let loads_before = feed_loads(&nest);

        let blob = block_on(m.prepare_sealed_reply(target.clone(), "said in the room".into()))
            .expect("prepare ok")
            .expect("a seated member has a body to upload");
        assert!(
            m.pending_gated
                .read()
                .unwrap()
                .as_ref()
                .is_some_and(|p| p.room),
            "uploaded under the room arm's sidecar"
        );
        assert!(
            !m.snapshot().compose.submitting,
            "a reply never marks the composer as sending"
        );
        let hash = hex::encode(blake3::hash(&blob).as_bytes());
        block_on(m.submit_gated_post(hash)).expect("the sealed reply lands");

        let created = created_post(&nest);
        let gated = created.gated.expect("the reply is sealed");
        assert!(
            matches!(gated.key_access, KeyAccess::Room { ref group_id, epoch: 4, .. } if group_id.0 == SEALED_ROOM),
            "sealed under the room arm, keyed now: {:?}",
            gated.key_access
        );
        assert_eq!(created.references.len(), 1, "it names what it answers");
        match created.body {
            fauna_core::data::PostBody::Text { ref content, .. } => {
                assert_eq!(content, "", "none of the words are on the public envelope")
            }
            ref other => panic!("expected an empty Text preview, got {other:?}"),
        }
        assert_eq!(
            m.snapshot().posts[0].reply_count,
            5,
            "the target's counter is read back"
        );
        assert_eq!(
            feed_loads(&nest),
            loads_before,
            "the timeline is not re-ranked"
        );
        assert_eq!(m.snapshot().compose.text, "an unrelated draft");
    }

    /// **A tier's owner replying under their own tier post seals to the tier**
    /// — under the current period key and the live key blob, the composer's own
    /// two reads.
    #[test]
    fn a_tier_owners_reply_to_their_own_tier_post_is_sealed_to_the_tier() {
        use fauna_core::subscription::types::KeyAccess;
        let target = "c1".repeat(32);
        let nest = MockNest::arc();
        nest.seed_tier(TIER, 2);
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.seed_key_blob(1, vec![0x11u8; 32], vec![0xAA]);
        let mut item = post(&target, 3);
        item.author = hex::encode(ActorKeypair::from_secret(TEST_SECRET).actor_id().0);
        item.gated_tier = Some(TIER.into());
        nest.push_page(vec![item], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.refresh_own_tiers());
        assert_eq!(
            reply_audience(&m, &target),
            Some(ReplyAudience::SealedToTier)
        );

        let blob = block_on(m.prepare_sealed_reply(target.clone(), "for patrons only".into()))
            .expect("prepare ok")
            .expect("the owner has a body to upload");
        assert!(
            m.pending_gated
                .read()
                .unwrap()
                .as_ref()
                .is_some_and(|p| !p.room),
            "uploaded under the tier's sidecar"
        );
        let hash = hex::encode(blake3::hash(&blob).as_bytes());
        block_on(m.submit_gated_post(hash)).expect("the sealed reply lands");

        let created = created_post(&nest);
        let gated = created.gated.expect("the reply is sealed");
        assert_eq!(gated.tier, TIER);
        assert!(
            matches!(gated.key_access, KeyAccess::Broadcast { .. }),
            "sealed under the tier's broadcast arm: {:?}",
            gated.key_access
        );
        assert_eq!(created.references.len(), 1);
    }

    /// **Everyone else has nothing to upload — and is still refused.** A
    /// subscriber's reply to another author's tier post, and a reply to a room
    /// post from off its floor, answer `Ok(None)` and stage nothing; the app's
    /// fall-through to `reply` then meets the refusal, which does not retire
    /// (ruling 6's last sentence). A wordless quote of a post this reader COULD
    /// seal under has nothing to seal either.
    #[test]
    fn a_reader_who_cannot_author_under_the_arm_has_nothing_to_upload_and_is_refused() {
        for (class, item) in restricted_posts() {
            let target = item.post_id.clone();
            let nest = MockNest::arc();
            // The reader owns a tier of the SAME NAME as the other author's:
            // the name alone must never read as "mine".
            nest.seed_tier("patrons", 2);
            nest.push_page(vec![item], None);
            let m = mgr(nest.clone());
            block_on(m.select_feed(None));
            block_on(m.refresh_own_tiers());
            assert_eq!(
                reply_audience(&m, &target),
                Some(ReplyAudience::PublicByConfirmation),
                "{class}"
            );

            let staged = block_on(m.prepare_sealed_reply(target.clone(), "my words".into()))
                .expect("not an error — just nothing to upload");
            assert!(staged.is_none(), "{class}");
            assert!(
                m.pending_gated.read().unwrap().is_none(),
                "{class}: nothing staged"
            );
            assert_eq!(
                block_on(m.reply(target.clone(), "my words".into())).expect_err("still refused"),
                fauna_client_core::post::REFERENCE_REFUSED_RESTRICTED,
                "{class}"
            );
            assert!(
                !nest.kinds().contains(&"fauna.posts.create".to_string()),
                "{class}"
            );
        }

        let (m, target) = seated_member(MockNest::arc());
        assert!(
            block_on(m.prepare_sealed_quote(target, String::new()))
                .expect("ok")
                .is_none(),
            "a wordless quote stays the public reference it always was"
        );
        assert!(m.pending_gated.read().unwrap().is_none());
    }

    // ── Ruling 5's confirmation arm (`ui/feed.md` § Encryption at rest →
    //    *Ruling 5's build — the shape*, (e)) ────────────────────────────────

    /// **Confirmed, the words go out public — with the words, under the same
    /// door `reply` refuses.** A subscriber under another author's tier post
    /// and a reader off a room's floor each answer *public only by
    /// confirmation*; `reply_public_confirmed` then creates the ordinary public
    /// reference: `gated: None`, the typed text in the clear, one `Reply`
    /// reference to the target — and the unconfirmed `reply` on the same
    /// target is still refused first (ruling 6 does not retire).
    #[test]
    fn a_confirmed_public_reply_under_a_restricted_post_goes_out_public_with_the_words() {
        for (class, item) in restricted_posts() {
            let target = item.post_id.clone();
            let nest = MockNest::arc();
            nest.seed_tier("patrons", 2);
            nest.push_page(vec![item], None);
            let m = mgr(nest.clone());
            block_on(m.select_feed(None));
            block_on(m.refresh_own_tiers());
            assert_eq!(
                reply_audience(&m, &target),
                Some(ReplyAudience::PublicByConfirmation),
                "{class}"
            );

            assert_eq!(
                block_on(m.reply(target.clone(), "my words".into())).expect_err("unconfirmed"),
                fauna_client_core::post::REFERENCE_REFUSED_RESTRICTED,
                "{class}: the unconfirmed default is still the refusal"
            );
            assert!(
                !nest.kinds().contains(&"fauna.posts.create".to_string()),
                "{class}: nothing created by the refused reply"
            );

            block_on(m.reply_public_confirmed(target.clone(), "my words".into()))
                .expect("confirmed, the reply is sent");
            let created = created_post(&nest);
            assert!(
                created.gated.is_none(),
                "{class}: a confirmed reply is public, by the user's own answer"
            );
            match created.body {
                fauna_core::data::PostBody::Text { ref content, .. } => {
                    assert_eq!(content, "my words", "{class}");
                }
                ref other => panic!("{class}: expected Text body, got {other:?}"),
            }
            let digest = fauna_core::hex32::decode(&target).unwrap();
            match &created.references[..] {
                [fauna_core::data::Reference::Reply { post_id }] => {
                    assert_eq!(*post_id, PostId::from_digest_dag_cbor(digest), "{class}");
                }
                other => panic!("{class}: expected one Reply reference, got {other:?}"),
            }
        }
    }

    /// **The confirmation never bypasses the arm.** Where this device could
    /// seal the reply — a seated member under a room post, the owner under
    /// their own tier post — no dialog offers the public answer, so a
    /// confirmed-public call is refused with the stated reason and nothing is
    /// created. Removing the sealed-arm check from `reply_public_confirmed`
    /// reddens this pin (the confirmation gate's mutation check).
    #[test]
    fn a_confirmed_public_reply_is_refused_where_the_reply_would_seal() {
        let (m, target) = seated_member(MockNest::arc());
        assert_eq!(
            reply_audience(&m, &target),
            Some(ReplyAudience::SealedToRoom)
        );
        assert_eq!(
            block_on(m.reply_public_confirmed(target, "said in the room".into()))
                .expect_err("a room member's reply seals; it is never sent public"),
            REPLY_SEALS_INSTEAD
        );

        let target = "c1".repeat(32);
        let nest = MockNest::arc();
        nest.seed_tier(TIER, 2);
        let mut item = post(&target, 3);
        item.author = hex::encode(ActorKeypair::from_secret(TEST_SECRET).actor_id().0);
        item.gated_tier = Some(TIER.into());
        nest.push_page(vec![item], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.refresh_own_tiers());
        assert_eq!(
            reply_audience(&m, &target),
            Some(ReplyAudience::SealedToTier)
        );
        assert_eq!(
            block_on(m.reply_public_confirmed(target, "for patrons only".into()))
                .expect_err("the owner's reply seals; it is never sent public"),
            REPLY_SEALS_INSTEAD
        );
        assert!(
            !nest.kinds().contains(&"fauna.posts.create".to_string()),
            "nothing created: {:?}",
            nest.kinds()
        );
    }

    /// A public target has nothing to confirm: the confirmed verb composes
    /// exactly what `reply` composes, and an empty body is refused as there.
    #[test]
    fn a_confirmed_public_reply_under_a_public_post_is_the_ordinary_reply() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        nest.push_page(vec![post(&target, 2)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert!(block_on(m.reply_public_confirmed(target.clone(), "  ".into())).is_err());
        assert!(!nest.kinds().contains(&"fauna.posts.create".to_string()));

        block_on(m.reply_public_confirmed(target, "well said".into())).expect("reply ok");
        let created = created_post(&nest);
        assert!(created.gated.is_none());
        assert!(created.is_reply());
    }

    /// A public post has no audience to state, and nothing to seal.
    #[test]
    fn a_public_post_states_no_reply_audience_and_prepares_nothing() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        nest.push_page(vec![post(&target, 2)], None);
        let m = mgr(nest);
        block_on(m.select_feed(None));
        assert_eq!(reply_audience(&m, &target), None);
        assert!(
            block_on(m.prepare_sealed_reply(target, "hello".into()))
                .expect("ok")
                .is_none()
        );
    }

    /// A sealed reply whose upload failed is dropped — and the composer, whose
    /// words it never was, is not stamped with the failure.
    #[test]
    fn a_failed_sealed_reply_upload_drops_the_stage_and_leaves_the_composer_alone() {
        let (m, target) = seated_member(MockNest::arc());
        block_on(m.prepare_sealed_reply(target, "a reply".into()))
            .expect("prepare ok")
            .expect("staged");
        m.abort_gated_submit("upload refused".into());
        assert!(
            m.pending_gated.read().unwrap().is_none(),
            "the stage is dropped"
        );
        assert!(
            m.snapshot().compose.error.is_none(),
            "not the composer's error"
        );
    }

    /// One slot: a reply must not be staged over a composer post whose sealed
    /// body the app is still uploading — the composer's submit would otherwise
    /// create the reply and lose the post.
    #[test]
    fn a_sealed_reply_is_not_staged_over_a_composer_post_still_being_sent() {
        let (m, target) = seated_member(MockNest::arc());
        m.update_compose("a room post".into(), String::new(), None);
        m.update_compose_room(Some(hex::encode(SEALED_ROOM)), "teaser".into());
        block_on(m.prepare_gated_blob()).expect("composer prepare ok");
        let staged = m
            .pending_gated
            .read()
            .unwrap()
            .as_ref()
            .map(|p| p.post_bytes.clone());

        block_on(m.prepare_sealed_reply(target, "a reply".into()))
            .expect_err("refused while the composer's post is in flight");
        assert_eq!(
            m.pending_gated
                .read()
                .unwrap()
                .as_ref()
                .map(|p| p.post_bytes.clone()),
            staged,
            "the composer's staged post is untouched"
        );
    }

    /// The other half of the ruling: a **wordless** reference stays public.
    /// A repost and today's commentary-less quote of a restricted post still
    /// compose — an ungated, empty-body post naming the target — so closing
    /// the reply door does not silently change what repost does.
    #[test]
    fn a_repost_or_wordless_quote_of_a_restricted_post_still_composes_public() {
        for (class, item) in restricted_posts() {
            for verb in ["repost", "quote"] {
                let target = item.post_id.clone();
                let nest = MockNest::arc();
                nest.push_page(vec![item.clone()], None);
                let m = mgr(nest.clone());
                block_on(m.select_feed(None));

                match verb {
                    "repost" => block_on(m.repost(target.clone())),
                    _ => block_on(m.quote(target.clone(), String::new())),
                }
                .unwrap_or_else(|e| panic!("{verb} of a {class} post: {e}"));

                let req: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
                let (decoded, _) = decode_post(req.body.as_ref()).expect("decodes + verifies");
                assert!(decoded.gated.is_none(), "{verb} of a {class} post");
                match decoded.body {
                    fauna_core::data::PostBody::Text { ref content, .. } => {
                        assert_eq!(content, "", "{verb} of a {class} post carries no words");
                    }
                    ref other => panic!("expected Text body, got {other:?}"),
                }
                assert_eq!(decoded.references.len(), 1, "{verb} of a {class} post");
            }
        }
    }

    /// **The bridged half** (`ui/feed.md` § Interaction bar → *Reply and quote
    /// on a bridged post*, ratified 2026-09-26): a reply to a bridged post is
    /// ONE eligibility ack through the interact door — no body — and then the
    /// same signed referencing post a native reply is; the nest's bridge
    /// fan-out derives the origin form from the post's `Reference`. Every
    /// bridged source takes this path, so one test per source spelling.
    #[test]
    fn a_reply_to_a_bridged_post_acks_then_composes_a_signed_post() {
        for source in ["bluesky", "activitypub", "nostr"] {
            let target = "cc".repeat(32);
            let nest = MockNest::arc();
            let mut bridged = post(&target, 2);
            bridged.source = source.into();
            nest.push_page(vec![bridged], None);
            let m = mgr(nest.clone());
            block_on(m.select_feed(None));

            block_on(m.reply(target.clone(), "hello there".into())).expect("reply ok");

            let kinds = nest.kinds();
            let ack_at = kinds
                .iter()
                .position(|k| k == "fauna.posts.interact")
                .unwrap_or_else(|| panic!("{source}: the eligibility ack must be asked"));
            let create_at = kinds
                .iter()
                .position(|k| k == "fauna.posts.create")
                .unwrap_or_else(|| panic!("{source}: the reply must be COMPOSED"));
            assert!(
                ack_at < create_at,
                "{source}: the ack comes before the compose"
            );
            let ack: fauna_protocol::posts::PostInteractRequest = nest.req("fauna.posts.interact");
            assert_eq!(ack.action, "reply");
            assert_eq!(ack.body, None, "{source}: no arm consumes a body any more");
            let req: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
            let (decoded, _) = decode_post(req.body.as_ref()).expect("decodes + verifies");
            let digest = fauna_core::hex32::decode(&target).unwrap();
            assert!(
                matches!(
                    &decoded.references[..],
                    [fauna_core::data::Reference::Reply { post_id }]
                        if *post_id == PostId::from_digest_dag_cbor(digest)
                ),
                "{source}: the signed post must reference the bridged target"
            );
        }
    }

    /// The door's refusal — no linked account, no relays, replies switched
    /// off — is the affordance's inline
    /// failure: the message surfaces and NOTHING is composed. A reply composed
    /// past a refusal would rest on Fauna while the user believes it reached
    /// the other network.
    #[test]
    fn a_refused_bridged_ack_composes_nothing_and_surfaces_the_message() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        let mut bridged = post(&target, 2);
        bridged.source = "activitypub".into();
        nest.push_page(vec![bridged], None);
        nest.inner.lock().unwrap().fail_kind = Some((
            "fauna.posts.interact".into(),
            "enable federation from the Bridges page first".into(),
        ));
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        let err = block_on(m.quote(target, "look".into())).expect_err("the refusal surfaces");
        assert!(err.contains("Bridges page"), "the remedy travels: {err}");
        assert!(
            !nest.kinds().contains(&"fauna.posts.create".to_string()),
            "a refused ack must compose nothing"
        );
    }

    /// The composed reply moves the target's counter on screen — read back
    /// from the nest's own post-act values, never guessed.
    #[test]
    fn a_composed_reply_refreshes_the_targets_counter_from_the_nest() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        nest.push_page(vec![post(&target, 2)], None);
        nest.set_interact_counts(0, 7, 0, 0);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.snapshot().posts[0].reply_count, 0);

        block_on(m.reply(target, "well said".into())).expect("reply ok");

        assert_eq!(
            m.snapshot().posts[0].reply_count,
            7,
            "the target's rendered reply_count must take the nest's post-act value"
        );
    }

    /// Ruling 3 (`archive-import.md` § Compatibility → *Slice-3 rulings*): the
    /// nest's interact door refuses `reply`/`repost`/`quote` on an
    /// archive-imported post — so the client must not spend the best-effort
    /// counter-refresh call on it either. The compose itself is untouched: an
    /// archive-imported post still routes to `fauna.posts.create` exactly
    /// like a plain `fauna` post (`is_native` covers both), it just skips the
    /// doomed follow-up `fauna.posts.interact` refresh.
    #[test]
    fn a_reply_on_an_archive_imported_post_composes_but_skips_the_interact_refresh() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        let mut archived = post(&target, 2);
        archived.source = fauna_core::source::FACEBOOK.into();
        nest.push_page(vec![archived], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        block_on(m.reply(target.clone(), "well said".into())).expect("reply ok");

        assert!(
            nest.kinds().contains(&"fauna.posts.create".to_string()),
            "an archive-imported post must still be composed as a signed post: kinds seen {:?}",
            nest.kinds()
        );
        assert!(
            !nest.kinds().contains(&"fauna.posts.interact".to_string()),
            "the door refuses reply/repost/quote on an archive token, so the \
             client must not call it for the refresh: kinds seen {:?}",
            nest.kinds()
        );
    }

    #[test]
    fn interact_folds_the_nests_post_act_counts_into_the_target_post_only() {
        let target = "cc".repeat(32);
        let other = "dd".repeat(32);
        let nest = MockNest::arc();
        nest.push_page(vec![post(&target, 2), post(&other, 1)], None);
        nest.set_interact_counts(1, 0, 0, 0);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.snapshot().posts[0].like_count, 0);

        block_on(m.interact(target.clone(), "like".into(), None)).expect("interact ok");

        let s = m.snapshot();
        let hit = s.posts.iter().find(|p| p.post_id == target).unwrap();
        assert_eq!(hit.like_count, 1, "the tapped post takes the nest's count");
        let miss = s.posts.iter().find(|p| p.post_id == other).unwrap();
        assert_eq!(miss.like_count, 0, "an untouched post keeps its own counts");
        // Ordering is not disturbed — this is a window patch, not a reload.
        assert_eq!(
            s.posts
                .iter()
                .map(|p| p.post_id.clone())
                .collect::<Vec<_>>(),
            vec![target, other]
        );
    }

    /// The counts are the NEST's, never a local guess. The nest's like counter
    /// is idempotent per (actor, post), so a second tap answers the *same*
    /// number — a client that optimistically incremented would drift to 2 and
    /// stay wrong until an unrelated reload.
    #[test]
    fn a_repeat_like_re_applies_the_nests_number_rather_than_incrementing() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        nest.push_page(vec![post(&target, 1)], None);
        nest.set_interact_counts(1, 0, 0, 0);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        block_on(m.interact(target.clone(), "like".into(), None)).expect("first like");
        block_on(m.interact(target.clone(), "like".into(), None)).expect("repeat like");

        assert_eq!(m.snapshot().posts[0].like_count, 1);
    }

    /// A nest that sends no `counts` (bridged source, or `unrepost`)
    /// leaves the rendered numbers exactly as they were — the additive field's
    /// fallback IS to leave them alone.
    #[test]
    fn interact_without_counts_leaves_the_rendered_counts_untouched() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        let mut seeded = post(&target, 1);
        seeded.like_count = 7;
        nest.push_page(vec![seeded], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.snapshot().posts[0].like_count, 7);

        block_on(m.interact(target, "like".into(), None)).expect("interact ok");

        assert!(nest.kinds().contains(&"fauna.posts.interact".to_string()));
        assert_eq!(m.snapshot().posts[0].like_count, 7);
    }

    #[test]
    fn interact_sends_the_action_and_body_the_caller_asked_for() {
        let target = "cc".repeat(32);
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        block_on(m.interact(target.clone(), "quote".into(), Some("nice post".into())))
            .expect("interact ok");
        let req: fauna_protocol::posts::PostInteractRequest = nest.req("fauna.posts.interact");
        assert_eq!(req.post_id, target);
        assert_eq!(req.action, "quote");
        assert_eq!(req.body.as_deref(), Some("nice post"));
    }

    #[test]
    fn delete_post_rejects_a_malformed_post_id_without_calling_the_nest() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        assert!(block_on(m.delete_post("not-hex".into())).is_err());
        assert!(!nest.kinds().contains(&"fauna.posts.delete".to_string()));
    }

    #[test]
    fn create_feed_encodes_rules_and_refreshes() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        let rules = vec![FilterRuleInput {
            rule_type: "BodyContains".into(),
            value: "rust, svelte".into(),
            required: false,
        }];
        let id = block_on(m.create_feed("My Feed".into(), rules, "all".into(), None, None, vec![]))
            .expect("create ok");
        assert_eq!(id, "feed-new");
        // The rules were encoded into the typed wire `rules` via the shared encoder.
        let req: FeedCreateRequest = nest.req("fauna.feed.create");
        assert_eq!(
            req.rules,
            vec![FilterRule::BodyContains {
                terms: vec!["rust".into(), "svelte".into()]
            }]
        );
        // No factors supplied — composition stays unset (no composition).
        assert_eq!(req.composition, None);
        // A feed refresh followed.
        assert!(nest.kinds().contains(&"fauna.feed.list".to_string()));
        // No factor entries — the global-factor read/write kinds never fired.
        assert!(!nest.kinds().contains(&"fauna.feed.factors.get".to_string()));
        assert!(!nest.kinds().contains(&"fauna.feed.factors.set".to_string()));
    }

    #[test]
    fn create_feed_rejects_unknown_rule_type() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        let rules = vec![FilterRuleInput {
            rule_type: "Bogus".into(),
            value: "x".into(),
            required: false,
        }];
        assert!(
            block_on(m.create_feed("f".into(), rules, "all".into(), None, None, vec![])).is_err()
        );
        // It failed before any nest call.
        assert!(!nest.kinds().contains(&"fauna.feed.create".to_string()));
    }

    #[test]
    fn create_feed_sends_local_factors_as_composition() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        let factors = vec![FactorWeightInput {
            factor: "engagement".into(),
            weight_permille: 1500,
            global: false,
        }];
        block_on(m.create_feed("Cats".into(), vec![], "all".into(), None, None, factors))
            .expect("create ok");
        let req: FeedCreateRequest = nest.req("fauna.feed.create");
        let composition = req.composition.expect("composition set");
        assert_eq!(composition.len(), 1);
        assert_eq!(composition[0].factor, "engagement");
        assert_eq!(composition[0].weight_permille, 1500);
        // A local-only factor never touches the global factor set.
        assert!(!nest.kinds().contains(&"fauna.feed.factors.get".to_string()));
        assert!(!nest.kinds().contains(&"fauna.feed.factors.set".to_string()));
    }

    #[test]
    fn create_feed_merges_global_factor_into_existing_set() {
        let nest = MockNest::arc();
        nest.inner.lock().unwrap().global_factors = vec![FeedCompositionEntry {
            factor: "labeler:aabb".into(),
            weight_permille: -1000,
            extra: Default::default(),
        }];
        let m = mgr(nest.clone());
        let factors = vec![FactorWeightInput {
            factor: "engagement".into(),
            weight_permille: 2000,
            global: true,
        }];
        block_on(m.create_feed("Cats".into(), vec![], "all".into(), None, None, factors))
            .expect("create ok");
        // The feed itself carries no local composition.
        let req: FeedCreateRequest = nest.req("fauna.feed.create");
        assert_eq!(req.composition, None);
        // The global set was read, upserted (keeping the pre-existing entry),
        // and written back whole.
        let set_req: fauna_client_feed::feed::FeedFactorsSetRequest =
            nest.req("fauna.feed.factors.set");
        let mut got = set_req.factors.clone();
        got.sort_by(|a, b| a.factor.cmp(&b.factor));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].factor, "engagement");
        assert_eq!(got[0].weight_permille, 2000);
        assert_eq!(got[1].factor, "labeler:aabb");
        assert_eq!(got[1].weight_permille, -1000);
    }

    /// A global factor recomposes every feed the nest serves, the one on
    /// screen included (`trending.md` § The Trending feed: Trending composes
    /// the caller's global set) — so the create that writes one re-queries the
    /// current selection, and a viewer on Trending sees the new order without
    /// having to leave and come back.
    #[test]
    fn create_feed_with_a_global_factor_requeries_the_current_selection() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None); // Trending's first load
        nest.push_page(vec![post("aa", 1)], None); // the re-query
        let m = mgr(nest.clone());
        block_on(m.select_trending_feed());
        let factors = vec![FactorWeightInput {
            factor: "engagement".into(),
            weight_permille: -5000,
            global: true,
        }];
        block_on(m.create_feed("Sink".into(), vec![], "all".into(), None, None, factors))
            .expect("create ok");
        let kinds = nest.kinds();
        let set_at = kinds
            .iter()
            .position(|k| k == "fauna.feed.factors.set")
            .expect("global set written");
        assert!(
            kinds[set_at..]
                .iter()
                .any(|k| k == "fauna.feed.trending.posts"),
            "Trending must be re-queried after the global set changes: {kinds:?}"
        );
        assert!(m.snapshot().trending_selected, "the selection is unchanged");
    }

    /// A feed-local factor changes only the new feed, so nothing on screen is
    /// re-queried.
    #[test]
    fn create_feed_with_only_local_factors_leaves_the_current_page_alone() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        let m = mgr(nest.clone());
        block_on(m.select_trending_feed());
        let factors = vec![FactorWeightInput {
            factor: "engagement".into(),
            weight_permille: 2000,
            global: false,
        }];
        block_on(m.create_feed("Cats".into(), vec![], "all".into(), None, None, factors))
            .expect("create ok");
        let trending_reads = nest
            .kinds()
            .iter()
            .filter(|k| *k == "fauna.feed.trending.posts")
            .count();
        assert_eq!(trending_reads, 1, "only the selection's own first load");
    }

    #[test]
    fn create_feed_rejects_duplicate_factor_before_any_nest_call() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        let factors = vec![
            FactorWeightInput {
                factor: "engagement".into(),
                weight_permille: 1000,
                global: false,
            },
            FactorWeightInput {
                factor: "engagement".into(),
                weight_permille: 2000,
                global: false,
            },
        ];
        assert!(
            block_on(m.create_feed("f".into(), vec![], "all".into(), None, None, factors)).is_err()
        );
        assert!(!nest.kinds().contains(&"fauna.feed.create".to_string()));
    }

    #[test]
    fn subscribe_bridge_clears_form_on_success() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        let id =
            block_on(m.subscribe_bridge("bluesky".into(), "at://feed".into(), "Cool feed".into()))
                .expect("subscribe ok");
        assert_eq!(id, 77);
        assert_eq!(m.snapshot().bridge_form, Default::default());
        let req: fauna_protocol::bridges_ui::CreateFeedRequest =
            nest.req("fauna.bridges.feeds.create");
        assert_eq!(req.bridge, "bluesky");
        assert_eq!(req.feed_uri, "at://feed");
    }

    #[test]
    fn subscribe_bridge_stamps_form_error_on_failure() {
        let nest = MockNest::arc();
        nest.inner.lock().unwrap().fail_next = Some("bad uri".into());
        let m = mgr(nest);
        assert!(block_on(m.subscribe_bridge("bluesky".into(), "x".into(), "n".into())).is_err());
        let bf = m.snapshot().bridge_form;
        assert!(bf.error.is_some());
        assert!(!bf.submitting);
    }

    #[test]
    fn resolve_quoted_post_projects_from_the_loaded_set_without_a_fetch() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1), post("bb", 1)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let view = block_on(m.resolve_quoted_post("bb".into())).expect("bb is loaded");
        assert_eq!(view.post_id, "bb");
        // No posts.get fetch — the quote was already in the loaded page.
        assert!(!nest.kinds().contains(&"fauna.posts.get".to_string()));
    }

    #[test]
    fn observer_is_notified_on_mutation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counter(Arc<AtomicUsize>);
        impl FeedSnapshotObserver for Counter {
            fn on_changed(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let nest = MockNest::arc();
        nest.push_page(vec![], None);
        let m = mgr(nest);
        let count = Arc::new(AtomicUsize::new(0));
        m.add_observer(Arc::new(Counter(count.clone())));
        block_on(m.select_feed(None));
        // reload notifies at least twice (Loading, then Loaded).
        assert!(count.load(Ordering::SeqCst) >= 2);
    }

    #[test]
    fn set_feed_snapshot_for_test_replaces_state_and_notifies() {
        use crate::test_support::{TestPostSpec, feed_snapshot_with_posts};
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Counter(Arc<AtomicUsize>);
        impl FeedSnapshotObserver for Counter {
            fn on_changed(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let m = mgr(MockNest::arc());
        let count = Arc::new(AtomicUsize::new(0));
        m.add_observer(Arc::new(Counter(count.clone())));

        // A freshly-constructed manager starts empty + Loading (no list painted).
        assert_eq!(m.snapshot().status, FeedStatus::Loading);
        assert!(m.snapshot().posts.is_empty());

        let snap = feed_snapshot_with_posts(vec![
            TestPostSpec {
                post_id: "aa".repeat(32),
                author: "bb".repeat(32),
                body: "an unverified post".into(),
                verification: VerificationStatus::Failed,
                ..Default::default()
            },
            TestPostSpec {
                post_id: "cc".repeat(32),
                author: "dd".repeat(32),
                verification: VerificationStatus::Verified,
                ..Default::default()
            },
        ]);
        m.set_feed_snapshot_for_test(snap);

        // The snapshot is now the injected list — Loaded so the page paints it,
        // order preserved, each post's verification carried through, and the body
        // rendered into a real `document` (not the default-empty one).
        let out = m.snapshot();
        assert_eq!(out.status, FeedStatus::Loaded);
        assert_eq!(out.posts.len(), 2);
        assert_eq!(out.posts[0].verification, VerificationStatus::Failed);
        assert_eq!(out.posts[1].verification, VerificationStatus::Verified);
        assert!(!out.posts[0].document.blocks.is_empty());
        // The injection fired the observer exactly once.
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn refresh_bridge_feeds_projects_the_subscription_list() {
        let nest = MockNest::arc();
        nest.inner.lock().unwrap().bridge_subs =
            vec![fauna_protocol::bridges_ui::FeedSubscription {
                extra: Default::default(),
                id: 5,
                bridge: "bluesky".into(),
                feed_uri: "at://feed".into(),
                name: "Cool".into(),
                created_at: 0,
            }];
        let m = mgr(nest.clone());
        block_on(m.refresh_bridge_feeds());
        let bf = m.snapshot().bridge_feeds;
        assert_eq!(bf.len(), 1);
        assert_eq!(bf[0].id, 5);
        assert_eq!(bf[0].bridge, "bluesky");
        assert_eq!(bf[0].name, "Cool");
        assert!(
            nest.kinds()
                .contains(&"fauna.bridges.feeds.list".to_string())
        );
    }

    fn bridge_status(
        id: &str,
        name: &str,
        available: bool,
    ) -> fauna_protocol::bridges_ui::BridgeStatus {
        fauna_protocol::bridges_ui::BridgeStatus {
            id: id.into(),
            name: name.into(),
            available,
            linked: false,
            identity: None,
            mode: None,
            settings: vec![],
            supports_follows: false,
            supports_follow_requests: false,
            link_modes: None,
            glyph: None,
            error: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn refresh_available_bridges_projects_only_available_bridges() {
        // The nest advertises one available bridge (its build supports bluesky +
        // the provider is runtime-available) and one unavailable one. The selector
        // option set (`available_bridges`) must carry ONLY the available bridge —
        // a client never offers a protocol the nest can't serve
        // (`version-compatibility.md` § Dim 3 — capability consumption).
        let nest = MockNest::arc();
        nest.inner.lock().unwrap().bridges = vec![
            bridge_status("bluesky", "Bluesky", true),
            bridge_status("nostr", "Nostr", false),
        ];
        let m = mgr(nest.clone());
        block_on(m.refresh_available_bridges());
        let ab = m.snapshot().available_bridges;
        assert_eq!(ab.len(), 1, "only the available bridge is selectable");
        assert_eq!(ab[0].id, "bluesky");
        assert_eq!(ab[0].name, "Bluesky");
        assert!(nest.kinds().contains(&"fauna.bridges.list".to_string()));
    }

    #[test]
    fn refresh_available_bridges_empty_when_nest_supports_none() {
        // A nest whose build carries no bridge providers (the default e2e
        // `build_node()` — no `--features bluesky/nostr/activitypub`) returns an
        // empty bridge list → the selector option set is empty → the client hides
        // the bridge-feed-subscribe form. This is the red→green of the per-app
        // hard-coded dropdown drift.
        let nest = MockNest::arc();
        // bridges left empty (default).
        let m = mgr(nest.clone());
        block_on(m.refresh_available_bridges());
        assert!(m.snapshot().available_bridges.is_empty());
    }

    /// The same fetch feeds the bridges roster: a row carrying a declared glyph
    /// (a consented third-party bridge) becomes one identity, whether or not it
    /// is available right now; a first-party row (no glyph) does not — and a
    /// post whose source token names the listed bridge then classifies
    /// `Bridged` with the declared label and glyph.
    #[test]
    fn refresh_available_bridges_feeds_the_bridge_roster() {
        use fauna_core::source_glyph::{BridgeIdentitySnapshot, SourceGlyph};
        let nest = MockNest::arc();
        let mut matrix = bridge_status("matrix", "Matrix", false);
        matrix.glyph = Some("globe".into());
        nest.inner.lock().unwrap().bridges =
            vec![bridge_status("bluesky", "Bluesky", true), matrix];
        let m = mgr(nest.clone());
        block_on(m.refresh_available_bridges());
        let roster = m.snapshot().bridge_roster;
        assert_eq!(
            roster,
            vec![BridgeIdentitySnapshot {
                id: "matrix".into(),
                label: "Matrix".into(),
                glyph: SourceGlyph::Globe,
            }]
        );
        assert_eq!(
            crate::classify_sources("matrix", &roster),
            vec![crate::SourceKind::Bridged {
                id: "matrix".into(),
                label: "Matrix".into(),
                glyph: SourceGlyph::Globe,
            }]
        );
    }

    #[test]
    fn resolve_media_fills_blob_hash_from_decoded_post() {
        let nest = MockNest::arc();
        // A loaded post flagging media; the feed item carries no blob hash.
        let mut p = post("aa", 1);
        p.has_media = true;
        nest.push_page(vec![p], None);
        // Seed posts.get with a real signed TextWithMedia post carrying the blob.
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let media = fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([0xab; 32]),
            media_type: "image/png".into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let bytes = build_post_with_media(&kp, "hi", vec![media], &[], None).unwrap();
        nest.inner.lock().unwrap().posts_get_body = bytes;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.snapshot().posts[0].media_hash, None);
        block_on(m.resolve_media("aa".into()));
        assert_eq!(m.snapshot().posts[0].media_hash, Some("ab".repeat(32)));
    }

    #[test]
    fn resolve_media_is_a_noop_for_a_post_without_media() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None); // has_media: false
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let before = nest.kinds().len();
        block_on(m.resolve_media("aa".into()));
        // No posts.get issued; the hash stays None.
        assert_eq!(nest.kinds().len(), before);
        assert_eq!(m.snapshot().posts[0].media_hash, None);
    }

    // ── The single-post deep link (`resolve_post`) ───────────────
    //
    // A search hit is the first caller that can name a post the feed query
    // never loaded; before this door a detail surface reading `snapshot.posts`
    // painted a blank dialog for one.

    /// The whole point: a post the timeline never loaded becomes renderable —
    /// through [`FeedSnapshot::find_post`], which is what every `post_detail`
    /// surface looks it up with. Note the body is the **full** decoded text, not
    /// the nest's 500-char list preview.
    #[test]
    fn resolve_post_makes_a_post_outside_the_timeline_renderable() {
        let nest = MockNest::arc();
        // The loaded page holds a DIFFERENT post — "zz" was never in the feed.
        nest.push_page(vec![post("aa", 1)], None);
        let kp = ActorKeypair::from_secret([9u8; 32]);
        let bytes = build_post(&kp, "the deep-linked body", &["Cats".into()], None).unwrap();
        // Requested under its real wire id: the fetch path binds the body to it.
        let zz = wire_post_id(&bytes);
        nest.inner.lock().unwrap().posts_get_body = bytes;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        // Red without the fix: the timeline lookup every caller used finds nothing.
        assert!(m.snapshot().posts.iter().all(|p| p.post_id != zz));

        assert_eq!(
            block_on(m.resolve_post(zz.clone())),
            PostResolution::Fetched
        );

        let snap = m.snapshot();
        let found = snap.find_post(&zz).expect("the deep-linked post renders");
        assert_eq!(found.body, "the deep-linked body");
        assert_eq!(found.author, hex::encode(kp.actor_id().0));
        // Tags come back in the feed index's own spelling (lowercased), so the
        // same post reads identically whichever door opened it.
        assert_eq!(found.tags, vec!["cats".to_string()]);
        // The deep-link path decodes the raw signed envelope, so unlike a list
        // card it carries a real verification answer.
        assert_eq!(found.verification, VerificationStatus::Verified);
        // The timeline itself is untouched — the post is NOT in the user's feed.
        assert!(snap.posts.iter().all(|p| p.post_id != zz));
    }

    /// A post the timeline already holds costs no round trip (the ordinary
    /// `post-card` click), and clears any stale slot.
    #[test]
    fn resolve_post_is_a_noop_for_a_post_the_timeline_already_holds() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        let kp = ActorKeypair::from_secret([9u8; 32]);
        nest.inner.lock().unwrap().posts_get_body =
            build_post(&kp, "elsewhere", &[], None).unwrap();

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        // Park a deep-linked post first, then open one the timeline holds.
        assert_eq!(
            block_on(m.resolve_post("zz".into())),
            PostResolution::Fetched
        );
        let before = nest.kinds().len();

        assert_eq!(
            block_on(m.resolve_post("aa".into())),
            PostResolution::Loaded
        );
        assert_eq!(nest.kinds().len(), before, "no fetch for a loaded post");
        assert_eq!(
            m.snapshot().deep_linked_post,
            None,
            "the stale slot is released"
        );
    }

    /// Re-opening the same deep-linked post is free — the slot already holds it.
    #[test]
    fn resolve_post_does_not_refetch_the_post_already_in_the_slot() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        let kp = ActorKeypair::from_secret([9u8; 32]);
        nest.inner.lock().unwrap().posts_get_body = build_post(&kp, "body", &[], None).unwrap();

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(
            block_on(m.resolve_post("zz".into())),
            PostResolution::Fetched
        );
        let after_first = nest.kinds().len();
        assert_eq!(
            block_on(m.resolve_post("zz".into())),
            PostResolution::Fetched
        );
        assert_eq!(
            nest.kinds().len(),
            after_first,
            "second open is a cache hit"
        );
    }

    /// A media-bearing deep-linked post resolves its blob hash from the body it
    /// already decoded — no second `fauna.posts.get`, unlike the list path's
    /// lazy `resolve_media`.
    #[test]
    fn resolve_post_fills_the_media_hash_without_a_second_fetch() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        let kp = ActorKeypair::from_secret([9u8; 32]);
        let media = fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([0xcd; 32]),
            media_type: "image/png".into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        nest.inner.lock().unwrap().posts_get_body =
            build_post_with_media(&kp, "with media", vec![media], &[], None).unwrap();

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let posts_gets = |n: &MockNest| {
            n.kinds()
                .iter()
                .filter(|k| k.as_str() == "fauna.posts.get")
                .count()
        };
        block_on(m.resolve_post("zz".into()));
        let snap = m.snapshot();
        let found = snap.find_post("zz").expect("resolved");
        assert!(found.has_media);
        assert_eq!(found.media_hash, Some("cd".repeat(32)));
        assert_eq!(posts_gets(&nest), 1, "exactly the one deep-link fetch");
    }

    /// A quote on a deep-linked post folds into its document exactly as it does
    /// for a timeline post — the embed-fold walks the rendered set, not the list.
    #[test]
    fn a_deep_linked_posts_quote_folds_into_its_document() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        let kp = ActorKeypair::from_secret([9u8; 32]);
        let quoted_digest = [0x5a; 32];
        let quoting = fauna_core::data::Post {
            author: kp.actor_id(),
            created_at: Timestamp::now(),
            body: fauna_core::data::PostBody::Text {
                content: "quoting body".into(),
                facets: vec![],
            },
            references: vec![fauna_core::data::Reference::Quote {
                post_id: PostId::from_digest_dag_cbor(quoted_digest),
            }],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        nest.inner.lock().unwrap().posts_get_body = sign_and_pack(&kp, &quoting).unwrap();

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_post("zz".into()));
        let quoted_hex = hex::encode(quoted_digest);
        assert_eq!(
            m.snapshot().find_post("zz").unwrap().quoted_post_id,
            Some(quoted_hex.clone()),
        );
        // Now answer the quote fetch with the quoted post's own body.
        nest.inner.lock().unwrap().posts_get_body =
            build_post(&kp, "the quoted body", &[], None).unwrap();
        block_on(m.resolve_quoted_post(quoted_hex));
        assert!(
            m.snapshot()
                .find_post("zz")
                .unwrap()
                .document
                .has_quoted_post(),
            "the quote block folded into the deep-linked post's document"
        );
    }

    /// A legally-taken-down post has its body withheld by the nest, so there is
    /// no post to project — but the detail surface still has something to
    /// render, and it belongs in the **body area**. The door answers `TakenDown`
    /// with the reference *and* parks a tombstone `PostSummary` in the slot, so
    /// `find_post` answers for the requested id and the app branches on
    /// `legal_takedown_ref` instead of standing its page error surface in for a
    /// body (`ui/feed.md` § The read model → *Opening a post the timeline never
    /// loaded*).
    #[test]
    fn resolve_post_answers_taken_down_and_parks_the_tombstone_in_the_slot() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        let kp = ActorKeypair::from_secret([9u8; 32]);
        nest.inner.lock().unwrap().posts_get_body = build_post(&kp, "fine", &[], None).unwrap();

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_post("zz".into()));
        assert!(m.snapshot().find_post("zz").is_some());

        nest.inner.lock().unwrap().posts_get_legal_takedown = Some("EU-DSA-2024/12345".into());
        assert_eq!(
            block_on(m.resolve_post("yy".into())),
            PostResolution::TakenDown {
                reference: "EU-DSA-2024/12345".into()
            }
        );

        // Red without the fix: the slot was emptied, so the body area had
        // nothing to paint and only `error-message` carried the notice.
        let snap = m.snapshot();
        let found = snap
            .find_post("yy")
            .expect("the tombstone renders in place");
        assert_eq!(
            found.legal_takedown_ref.as_deref(),
            Some("EU-DSA-2024/12345"),
            "the projection must carry the reference the app renders the tombstone from"
        );
        // Nothing of the withheld post leaks — there was never a body to decode.
        assert_eq!(found.body, "");
        assert_eq!(found.author, "");
        assert_eq!(found.verification, VerificationStatus::Unchecked);
        // And the *previous* deep link is gone: no earlier post may sit on
        // screen under a different post's takedown notice.
        assert!(
            snap.find_post("zz").is_none(),
            "the previous deep-linked post must not sit under another post's takedown notice"
        );
    }

    /// A post the nest cannot serve (not found / quarantine-gated) degrades to
    /// the surface's empty state rather than erroring the page.
    #[test]
    fn resolve_post_reports_unavailable_for_an_undecodable_body() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        // `posts_get_body` left empty ⇒ nothing decodes.
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(
            block_on(m.resolve_post("zz".into())),
            PostResolution::Unavailable
        );
        assert_eq!(m.snapshot().deep_linked_post, None);
    }

    /// Deleting the post you are viewing releases the slot too — `s.posts
    /// .retain` alone would have left it on screen.
    #[test]
    fn deleting_a_deep_linked_post_releases_the_slot() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        let kp = ActorKeypair::from_secret(TEST_SECRET);
        nest.inner.lock().unwrap().posts_get_body = build_post(&kp, "body", &[], None).unwrap();

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let target = "5a".repeat(32);
        block_on(m.resolve_post(target.clone()));
        assert!(m.snapshot().find_post(&target).is_some());
        block_on(m.delete_post(target.clone())).expect("delete");
        assert!(m.snapshot().find_post(&target).is_none());
    }

    // ── F-CL2/F-CL3: unverified-source indicator ─────────────────

    /// Flip a byte in a signed post's 64-byte Ed25519 signature so the envelope
    /// fails verification while the inner bytes (hence the CID + structural
    /// decode) stay intact — `decode_post` then returns `Ok((post, false))`, the
    /// "decodes but unverified" case the badge exists to surface.
    fn tamper_signature(body: Vec<u8>) -> Vec<u8> {
        use fauna_core::encoding::{EmbedAsBytes, canonical_decode, canonical_encode};
        let mut wire: EmbedAsBytes = canonical_decode(&body).expect("decode embed");
        // envelope = [36-byte CID || 64-byte sig]; flip the last sig byte.
        let last = wire.envelope.len() - 1;
        wire.envelope[last] ^= 0xff;
        canonical_encode(&wire).expect("re-encode embed")
    }

    /// A loaded `has_media` post whose raw signed body resolve_media fetches +
    /// decodes is marked `Verified` when the signature checks out.
    #[test]
    fn resolve_media_marks_a_valid_signed_body_verified() {
        let nest = MockNest::arc();
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let media = fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([0xab; 32]),
            media_type: "image/png".into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let bytes = build_post_with_media(&kp, "hi", vec![media], &[], None).unwrap();
        // The card is keyed by the body's real wire id — the id binding's
        // other half of `Verified`.
        let id = wire_post_id(&bytes);
        let mut p = post(&id, 1);
        p.has_media = true;
        nest.push_page(vec![p], None);
        nest.inner.lock().unwrap().posts_get_body = bytes;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        // The list card starts Unchecked (a trusted nest projection, no envelope).
        assert_eq!(
            m.snapshot().posts[0].verification,
            VerificationStatus::Unchecked
        );
        block_on(m.resolve_media(id));
        assert_eq!(
            m.snapshot().posts[0].verification,
            VerificationStatus::Verified
        );
    }

    /// A `has_media` post whose fetched body fails signature verification is
    /// marked `Failed` (drives the unverified-source badge) yet still renders —
    /// the media hash still resolves from the untampered inner bytes (a transient
    /// false-negative must not make a legit post vanish).
    #[test]
    fn resolve_media_marks_a_tampered_body_failed_but_still_renders() {
        let nest = MockNest::arc();
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let media = fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([0xab; 32]),
            media_type: "image/png".into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let bytes = build_post_with_media(&kp, "hi", vec![media], &[], None).unwrap();
        let tampered = tamper_signature(bytes);
        // Keyed by the tampered bytes' own wire id, so the id binding holds and
        // the signature alone is what fails.
        let id = wire_post_id(&tampered);
        let mut p = post(&id, 1);
        p.has_media = true;
        nest.push_page(vec![p], None);
        nest.inner.lock().unwrap().posts_get_body = tampered;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_media(id));
        assert_eq!(
            m.snapshot().posts[0].verification,
            VerificationStatus::Failed
        );
        // Still rendered: the media hash resolved from the (untouched) inner bytes.
        assert_eq!(m.snapshot().posts[0].media_hash, Some("ab".repeat(32)));
    }

    /// The quoted-post fallback (`fauna.posts.get` + decode for a quote outside
    /// the loaded page) carries its decode's verification into the returned
    /// `QuotedPostView` instead of discarding it (F-CL3) — a tampered quote is
    /// `Failed`.
    #[test]
    fn resolve_quoted_post_fallback_marks_a_tampered_quote_failed() {
        let nest = MockNest::arc();
        let kp = ActorKeypair::from_secret([9u8; 32]);
        let bytes = build_post(&kp, "the quoted body", &[], None).unwrap();
        let tampered = tamper_signature(bytes);
        // Quoted under the tampered bytes' own wire id (NOT in the loaded
        // page), so the id binding holds and the signature alone fails.
        let zz = wire_post_id(&tampered);
        let mut quoting = post("aa", 2);
        quoting.quoted_post_id = Some(zz.clone());
        nest.push_page(vec![quoting], None);
        nest.inner.lock().unwrap().posts_get_body = tampered;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let view = block_on(m.resolve_quoted_post(zz)).expect("fallback resolves");
        // The fallback fetched + decoded (no in-page projection).
        assert!(nest.kinds().contains(&"fauna.posts.get".to_string()));
        assert_eq!(view.verification, VerificationStatus::Failed);
        assert_eq!(view.body, "the quoted body");
    }

    // ── A fetched body must BE the post that was asked for ────
    //
    // A signature proves who wrote a body, not which post it is. A hostile or
    // lured nest answering `posts.get(X)` with another genuinely signed post `Y`
    // is a context substitution: each by-id fetch path renders `Y` (a failed
    // check never drops content — `security.md` § App display of unverified
    // content) but as `Failed`, with no authoring-origin claim.

    /// Two genuinely signed posts by one author: `(x_id, y_bytes)` — the id a
    /// caller asks for, and the different post a hostile nest serves under it.
    fn substituted_post() -> (String, Vec<u8>) {
        let kp = ActorKeypair::from_secret([9u8; 32]);
        let x = build_post(&kp, "the post that was asked for", &[], None).unwrap();
        let y = build_post(&kp, "a different signed post", &[], None).unwrap();
        (wire_post_id(&x), y)
    }

    #[test]
    fn resolve_quoted_post_fallback_marks_a_substituted_quote_failed() {
        let (x_id, y) = substituted_post();
        let nest = MockNest::arc();
        let mut quoting = post("aa", 2);
        quoting.quoted_post_id = Some(x_id.clone());
        nest.push_page(vec![quoting], None);
        nest.inner.lock().unwrap().posts_get_body = y;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let view = block_on(m.resolve_quoted_post(x_id)).expect("fallback resolves");
        assert_eq!(view.verification, VerificationStatus::Failed);
        assert_eq!(view.authoring_origin, AuthoringOriginStatus::Unknown);
        assert_eq!(view.body, "a different signed post");
    }

    #[test]
    fn resolve_post_marks_a_substituted_body_failed() {
        let (x_id, y) = substituted_post();
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        nest.inner.lock().unwrap().posts_get_body = y;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(
            block_on(m.resolve_post(x_id.clone())),
            PostResolution::Fetched
        );
        let snap = m.snapshot();
        let found = snap.find_post(&x_id).expect("still renders");
        assert_eq!(found.verification, VerificationStatus::Failed);
        assert_eq!(found.authoring_origin, AuthoringOriginStatus::Unknown);
        assert_eq!(found.body, "a different signed post");
    }

    #[test]
    fn resolve_media_marks_a_substituted_body_failed() {
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let media = |b: u8| fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([b; 32]),
            media_type: "image/png".into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let x = build_post_with_media(&kp, "x", vec![media(0xab)], &[], None).unwrap();
        let y = build_post_with_media(&kp, "y", vec![media(0xcd)], &[], None).unwrap();
        let nest = MockNest::arc();
        let mut p = post(&wire_post_id(&x), 1);
        p.has_media = true;
        nest.push_page(vec![p], None);
        nest.inner.lock().unwrap().posts_get_body = y;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_media(wire_post_id(&x)));
        let card = &m.snapshot().posts[0];
        assert_eq!(card.verification, VerificationStatus::Failed);
        assert_eq!(card.authoring_origin, AuthoringOriginStatus::Unknown);
        // Rendered, not dropped — with the media the nest actually served.
        assert_eq!(card.media_hash, Some("cd".repeat(32)));
    }

    /// The quoted-post fallback surfaces a **legal-takedown tombstone** when
    /// `fauna.posts.get` withholds the body (moderation.md § Categories &
    /// enforcement item 1). Instead of silently failing to decode the empty body
    /// (a blank/broken embed — the carve-out forbids that), the resolved
    /// `QuotedPostView` and the folded `RenderBlock::QuotedPost` carry the takedown
    /// `reference` so the client renders "Removed under legal obligation
    /// ({reference})" in place of the quoted content.
    #[test]
    fn resolve_quoted_post_fallback_surfaces_a_legal_takedown_tombstone() {
        use fauna_core::render::RenderBlock;
        let nest = MockNest::arc();
        let mut quoting = post("aa", 2);
        quoting.quoted_post_id = Some("zz".into()); // "zz" is NOT in the loaded page
        nest.push_page(vec![quoting], None);
        // The nest withholds the body and returns the takedown reference.
        nest.inner.lock().unwrap().posts_get_legal_takedown = Some("EU-DSA-2024/12345".into());

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let view = block_on(m.resolve_quoted_post("zz".into())).expect("tombstone resolves");
        assert!(nest.kinds().contains(&"fauna.posts.get".to_string()));
        // The tombstone view: reference carried, body withheld (empty), no
        // spurious verification claim.
        assert_eq!(
            view.legal_takedown_ref.as_deref(),
            Some("EU-DSA-2024/12345")
        );
        assert!(view.body.is_empty());
        assert_eq!(view.verification, VerificationStatus::Unchecked);

        // The folded document block carries the reference so every app walker
        // paints the tombstone in place of the quoted body.
        let aa = m
            .snapshot()
            .posts
            .into_iter()
            .find(|p| p.post_id == "aa")
            .unwrap();
        match aa.document.blocks.last() {
            Some(RenderBlock::QuotedPost {
                legal_takedown_ref,
                body,
                ..
            }) => {
                assert_eq!(legal_takedown_ref.as_deref(), Some("EU-DSA-2024/12345"));
                assert!(body.is_empty(), "the withheld body is empty on the block");
            }
            other => panic!("expected a trailing QuotedPost block, got {other:?}"),
        }
    }

    /// A quote of a post the nest no longer has (its author deleted it) folds
    /// the not-found embed — `ui/feed.md` § Post deletion: "their embedded
    /// target renders as the existing not-found state". Never nothing (a quote
    /// of no post), never a blank embed; and cached, so the dead target is
    /// asked for once.
    #[test]
    fn a_quote_of_a_post_the_nest_no_longer_has_folds_the_not_found_embed() {
        use fauna_core::render::RenderBlock;
        let nest = MockNest::arc();
        let mut quoting = post("aa", 2);
        quoting.quoted_post_id = Some("zz".into()); // "zz" is NOT in the loaded page
        nest.push_page(vec![quoting], None);
        // A seeded by-id map that lacks "zz": the mock answers it the way the
        // nest answers a deleted post — a `fauna.posts.not_found` rejection.
        nest.inner
            .lock()
            .unwrap()
            .posts_get_by_id
            .insert("some-other-post".into(), Vec::new());

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let view = block_on(m.resolve_quoted_post("zz".into())).expect("not-found resolves");
        assert!(view.not_found);
        assert!(view.body.is_empty() && view.author.is_empty());
        assert_eq!(view.legal_takedown_ref, None, "gone is not withheld");

        let aa = m
            .snapshot()
            .posts
            .into_iter()
            .find(|p| p.post_id == "aa")
            .unwrap();
        assert!(
            matches!(
                aa.document.blocks.last(),
                Some(RenderBlock::QuotedPost {
                    not_found: true,
                    ..
                })
            ),
            "the not-found embed folds into the quoting post's document, got {:?}",
            aa.document.blocks.last()
        );

        let gets = || {
            nest.kinds()
                .iter()
                .filter(|k| *k == "fauna.posts.get")
                .count()
        };
        let before = gets();
        let again = block_on(m.resolve_quoted_post("zz".into())).expect("cached");
        assert!(again.not_found);
        assert_eq!(gets(), before, "a dead target is fetched once, then cached");
    }

    /// A transport fault is NOT "the post is gone": it folds nothing, so the
    /// next resolve asks again — the wire code, never any error, is the signal.
    #[test]
    fn a_quote_fetch_that_fails_in_transport_folds_nothing() {
        let nest = MockNest::arc();
        let mut quoting = post("aa", 2);
        quoting.quoted_post_id = Some("zz".into());
        nest.push_page(vec![quoting], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        nest.inner.lock().unwrap().fail_kind =
            Some(("fauna.posts.get".into(), "socket closed".into()));

        assert_eq!(block_on(m.resolve_quoted_post("zz".into())), None);
        let aa = m
            .snapshot()
            .posts
            .into_iter()
            .find(|p| p.post_id == "aa")
            .unwrap();
        assert!(
            !aa.document.has_quoted_post(),
            "no embed folds on a transport fault"
        );
    }

    // ── D6: feed body → shared RenderDocument ────────────────────

    #[test]
    fn loaded_post_carries_the_body_as_a_render_document() {
        // D6 (render-model.md): the manager produces `PostSummary.document` from
        // the body once, so every app walks the shared semantic tree instead
        // of re-rendering the raw string. The document is the markdown projection
        // of the body, and the raw `body` is retained (source + quote projection).
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.body = "**bold** and a [link](https://example.com)".into();
        nest.push_page(vec![p], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let post = &m.snapshot().posts[0];
        assert_eq!(post.body, "**bold** and a [link](https://example.com)");
        assert_eq!(
            post.document,
            fauna_core::render::markdown_to_document("**bold** and a [link](https://example.com)"),
        );
        // Not flat: the markdown structure (a paragraph, not a raw string) is
        // honoured — the bold/link runs the per-app walker paints.
        assert!(matches!(
            post.document.blocks.first(),
            Some(fauna_core::render::RenderBlock::Paragraph { .. })
        ));
    }

    // ── D6 embed-fold: quoted post + media become document blocks ─────

    #[test]
    fn resolve_quoted_post_folds_a_quoted_post_block_into_the_quoting_document() {
        use fauna_core::render::RenderBlock;
        // "aa" quotes "bb"; both loaded. Before resolution aa's document is
        // body-only; resolve_quoted_post("bb") folds a `QuotedPost` block carrying
        // bb's author + truncated body into aa's document, after the body (D6).
        let nest = MockNest::arc();
        let mut quoting = post("aa", 2);
        quoting.quoted_post_id = Some("bb".into());
        let mut quoted = post("bb", 1);
        quoted.body = "the quoted body".into();
        nest.push_page(vec![quoting, quoted], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        let aa = m
            .snapshot()
            .posts
            .into_iter()
            .find(|p| p.post_id == "aa")
            .unwrap();
        assert!(
            !aa.document
                .blocks
                .iter()
                .any(|b| matches!(b, RenderBlock::QuotedPost { .. })),
            "body-only before resolution"
        );

        block_on(m.resolve_quoted_post("bb".into()));
        let aa = m
            .snapshot()
            .posts
            .into_iter()
            .find(|p| p.post_id == "aa")
            .unwrap();
        // The body paragraph stays first; the quoted post is folded in after it.
        assert!(matches!(
            aa.document.blocks.first(),
            Some(RenderBlock::Paragraph { .. })
        ));
        match aa.document.blocks.last() {
            Some(RenderBlock::QuotedPost { post_id, body, .. }) => {
                assert_eq!(post_id, "bb");
                assert_eq!(body, "the quoted body");
            }
            other => panic!("expected a trailing QuotedPost block, got {other:?}"),
        }
        // No fetch — bb was in the loaded page (the in-page projection path).
        assert!(!nest.kinds().contains(&"fauna.posts.get".to_string()));
    }

    #[test]
    fn folded_quoted_post_block_carries_the_quoted_verification() {
        use fauna_core::render::{RenderBlock, VerificationStatus};
        // Slice 2b: the folded `QuotedPost` block must carry the *quoted* post's
        // verification (security.md § App display of unverified content) so the
        // quoted-embed card paints the "unverified source" badge iff `Failed`.
        // "aa" quotes `zz` (NOT loaded) and the fetched quote body is tampered, so
        // the fallback decode marks it `Failed` — the folded block must reflect it,
        // not the default `Unchecked`.
        let nest = MockNest::arc();
        let kp = ActorKeypair::from_secret([9u8; 32]);
        let bytes = build_post(&kp, "the quoted body", &[], None).unwrap();
        let tampered = tamper_signature(bytes);
        // Quoted under the tampered bytes' own wire id (NOT in the loaded
        // page), so the id binding holds and the signature alone fails.
        let zz = wire_post_id(&tampered);
        let mut quoting = post("aa", 2);
        quoting.quoted_post_id = Some(zz.clone());
        nest.push_page(vec![quoting], None);
        nest.inner.lock().unwrap().posts_get_body = tampered;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_quoted_post(zz.clone()));

        let aa = m
            .snapshot()
            .posts
            .into_iter()
            .find(|p| p.post_id == "aa")
            .unwrap();
        match aa.document.blocks.last() {
            Some(RenderBlock::QuotedPost {
                post_id,
                verification,
                ..
            }) => {
                assert_eq!(*post_id, zz);
                assert_eq!(
                    *verification,
                    VerificationStatus::Failed,
                    "the folded block carries the tampered quote's Failed status, not Unchecked",
                );
            }
            other => panic!("expected a trailing QuotedPost block, got {other:?}"),
        }
    }

    #[test]
    fn resolve_quoted_post_notifies_once_then_is_idempotent() {
        // The fold notifies exactly once; a repeat resolution (e.g. a
        // notify-driven re-render in a non-fire-once client observer) is a no-op
        // with NO further notify — the discipline that stops a render loop.
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Counter(Arc<AtomicUsize>);
        impl FeedSnapshotObserver for Counter {
            fn on_changed(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let nest = MockNest::arc();
        let mut quoting = post("aa", 2);
        quoting.quoted_post_id = Some("bb".into());
        let mut quoted = post("bb", 1);
        quoted.body = "the quoted body".into();
        nest.push_page(vec![quoting, quoted], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        // Observe AFTER the initial load so we count only the quote resolutions.
        let count = Arc::new(AtomicUsize::new(0));
        m.add_observer(Arc::new(Counter(count.clone())));

        block_on(m.resolve_quoted_post("bb".into()));
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "the first fold re-emits exactly once"
        );

        block_on(m.resolve_quoted_post("bb".into()));
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "an idempotent re-resolution does not notify again (no render loop)"
        );

        // The retry is a cache hit — no second fetch either (bb was loaded, so
        // there is no fetch at all).
        let gets = nest
            .kinds()
            .iter()
            .filter(|k| k.as_str() == "fauna.posts.get")
            .count();
        assert_eq!(gets, 0, "cached view on retry; bb was in the loaded page");
    }

    #[test]
    fn resolve_media_folds_an_image_block_into_the_document() {
        use fauna_core::render::RenderBlock;
        // A loaded `has_media` post; resolve_media fills `media_hash` AND folds an
        // `Image` block carrying the resolved blob hash into the document (D6).
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.has_media = true;
        nest.push_page(vec![p], None);
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let media = fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([0xab; 32]),
            media_type: "image/png".into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let bytes = build_post_with_media(&kp, "hi", vec![media], &[], None).unwrap();
        nest.inner.lock().unwrap().posts_get_body = bytes;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_media("aa".into()));
        let aa = &m.snapshot().posts[0];
        assert_eq!(aa.media_hash.as_deref(), Some("ab".repeat(32).as_str()));
        match aa.document.blocks.last() {
            Some(RenderBlock::Image { hash, .. }) => assert_eq!(*hash, "ab".repeat(32)),
            other => panic!("expected a trailing Image block, got {other:?}"),
        }
    }

    /// The typed half of the D6 media fold: a `video/*` attachment must fold to a `Video`
    /// block, NOT an `Image` (render-model.md § Implementation status today — the gap that made
    /// `video-thumbnail` unbuildable on all 7 apps). `first_image_hash` must stay `None` so no
    /// app paints a video blob into its `post-image` element.
    #[test]
    fn resolve_media_folds_a_video_block_for_a_video_attachment() {
        use fauna_core::render::RenderBlock;
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.has_media = true;
        nest.push_page(vec![p], None);
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let media = fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([0xab; 32]),
            media_type: "video/mp4".into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let bytes = build_post_with_media(&kp, "hi", vec![media], &[], None).unwrap();
        nest.inner.lock().unwrap().posts_get_body = bytes;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_media("aa".into()));
        let aa = &m.snapshot().posts[0];
        match aa.document.blocks.last() {
            Some(RenderBlock::Video { hash, .. }) => assert_eq!(*hash, "ab".repeat(32)),
            other => panic!("expected a trailing Video block, got {other:?}"),
        }
        assert_eq!(
            aa.document.first_video_hash(),
            Some("ab".repeat(32).as_str())
        );
        assert_eq!(
            aa.document.first_image_hash(),
            None,
            "a video must never satisfy the `post-image` accessor"
        );
    }

    /// EVERY attachment folds, in body order, each typed by its own `media_type` — the
    /// richest-pattern half (priority #4). Before this the fold kept `items.first()` alone, so
    /// web had to render the extra items from a second, app-side `decode_post`.
    #[test]
    fn resolve_media_folds_every_attachment_in_body_order() {
        use fauna_core::render::RenderBlock;
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.has_media = true;
        nest.push_page(vec![p], None);
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let item = |byte: u8, ty: &str| fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([byte; 32]),
            media_type: ty.into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let items = vec![
            item(0x11, "image/png"),
            item(0x22, "video/mp4"),
            item(0x33, "image/jpeg"),
            // Neither image nor video: folds to nothing, matching the web path that has always
            // rendered only those two kinds.
            item(0x44, "application/pdf"),
        ];
        let bytes = build_post_with_media(&kp, "hi", items, &[], None).unwrap();
        nest.inner.lock().unwrap().posts_get_body = bytes;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_media("aa".into()));
        let aa = &m.snapshot().posts[0];
        let media = aa.document.media_blocks();
        let got: Vec<(&str, String)> = media
            .iter()
            .map(|b| match b {
                RenderBlock::Image { hash, .. } => ("image", hash.clone()),
                RenderBlock::Video { hash, .. } => ("video", hash.clone()),
                other => panic!("media_blocks yielded a non-media block: {other:?}"),
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("image", "11".repeat(32)),
                ("video", "22".repeat(32)),
                ("image", "33".repeat(32)),
            ],
            "every image/video item folds, in body order, and the pdf folds to nothing"
        );
    }

    /// render-model.md § D6c, the TDD cases: a bridged post's item carries no blob (a zero
    /// `blob_hash`) and a `remote_url`. Resolve it through the mock nest and return the
    /// folded post.
    fn resolve_bridged(
        items: Vec<fauna_core::data::MediaItem>,
    ) -> (Arc<MockNest>, FeedManager<Arc<MockNest>>) {
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.has_media = true;
        nest.push_page(vec![p], None);
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let bytes = build_post_with_media(&kp, "bridged", items, &[], None).unwrap();
        nest.inner.lock().unwrap().posts_get_body = bytes;
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_media("aa".into()));
        (nest, m)
    }

    fn remote_item(ty: &str, url: &str) -> fauna_core::data::MediaItem {
        fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([0u8; 32]),
            media_type: ty.into(),
            size_bytes: 0,
            dimensions: None,
            thumbnail: None,
            remote_url: Some(url.into()),
            ..Default::default()
        }
    }

    /// D6c case 1: a Bluesky item's nest-relative `remote_url` folds to `ProxiedImage` with
    /// that exact path — never to an `Image` naming the zero hash (the `/api/v1/blob/000…`
    /// fetch every app used to make).
    #[test]
    fn a_bridged_relative_remote_url_folds_to_a_proxied_image() {
        use fauna_core::render::RenderBlock;
        let path = "/api/v1/bluesky/media?url=https%3A%2F%2Fcdn.bsky.app%2Fimg%2Fa.jpg";
        let (_nest, m) = resolve_bridged(vec![remote_item("image/jpeg", path)]);
        let aa = &m.snapshot().posts[0];
        assert_eq!(
            aa.document.media_blocks(),
            vec![RenderBlock::ProxiedImage {
                path: path.into(),
                alt: String::new(),
            }],
        );
        assert_eq!(aa.document.first_image_hash(), None, "no zero-hash Image");
        assert!(
            !aa.document.has_blocked_remote_images(),
            "paints immediately"
        );
    }

    /// An item's own `alt` (what the two bridge ingest paths write from the remote
    /// attachment's description) is the `alt` of its media block, on all four media
    /// variants; an item with none folds an empty `alt`, as every item did before.
    #[test]
    fn a_media_items_alt_is_its_blocks_alt_on_every_media_variant() {
        use fauna_core::data::{ContentHash, MediaItem};
        use fauna_core::render::RenderBlock;
        let described = |item: MediaItem, alt: &str| MediaItem {
            alt: Some(alt.into()),
            ..item
        };
        let blob = |ty: &str, byte: u8| MediaItem {
            blob_hash: ContentHash::from_digest_raw([byte; 32]),
            media_type: ty.into(),
            size_bytes: 1,
            ..Default::default()
        };
        let image_path = "/api/v1/bluesky/media?url=https%3A%2F%2Fcdn.bsky.app%2Fimg%2Fa.jpg";
        let video_path = "/api/v1/media/proxy?url=https%3A%2F%2Ff.example%2Fv.mp4";
        let (_nest, m) = resolve_bridged(vec![
            described(remote_item("image/jpeg", image_path), "a cat"),
            described(remote_item("video/mp4", video_path), "a cat, moving"),
            described(blob("image/png", 0x11), "a dog"),
            described(blob("video/mp4", 0x22), "a dog, moving"),
            remote_item("image/jpeg", image_path),
        ]);
        let aa = &m.snapshot().posts[0];
        assert_eq!(
            aa.document.media_blocks(),
            vec![
                RenderBlock::ProxiedImage {
                    path: image_path.into(),
                    alt: "a cat".into(),
                },
                RenderBlock::ProxiedVideo {
                    path: video_path.into(),
                    alt: "a cat, moving".into(),
                },
                RenderBlock::Image {
                    hash: "11".repeat(32),
                    alt: "a dog".into(),
                },
                RenderBlock::Video {
                    hash: "22".repeat(32),
                    alt: "a dog, moving".into(),
                },
                RenderBlock::ProxiedImage {
                    path: image_path.into(),
                    alt: String::new(),
                },
            ],
        );
    }

    /// D6c case 2: an absolute `https://` `remote_url` (a row written before the ingest
    /// rewrite) is rewritten through the shared proxy form, so the snapshot never carries the
    /// remote origin; an unrewritable one (`http://`, a bare host) folds to nothing. (A
    /// zero-hash video folds to its own `ProxiedVideo` — the next test's subject.)
    #[test]
    fn a_bridged_absolute_remote_url_folds_to_the_proxied_path_never_the_origin() {
        use fauna_core::render::RenderBlock;
        let (_nest, m) = resolve_bridged(vec![
            remote_item("image/png", "https://files.example/a.png"),
            remote_item("image/png", "http://files.example/b.png"),
            remote_item(
                "video/mp4",
                "/api/v1/media/proxy?url=https%3A%2F%2Ff.example%2Fv.mp4",
            ),
            remote_item("image/png", "files.example/c.png"),
        ]);
        let aa = &m.snapshot().posts[0];
        assert_eq!(
            aa.document.media_blocks(),
            vec![
                RenderBlock::ProxiedImage {
                    path: "/api/v1/media/proxy?url=https%3A%2F%2Ffiles.example%2Fa.png".into(),
                    alt: String::new(),
                },
                RenderBlock::ProxiedVideo {
                    path: "/api/v1/media/proxy?url=https%3A%2F%2Ff.example%2Fv.mp4".into(),
                    alt: String::new(),
                },
            ],
        );
    }

    /// D6c → *Proxied video*, the ruling's TDD cases: a bridged `video/*` item folds to
    /// `ProxiedVideo` through the same `proxied_media_path` rewrite as its image twin — an
    /// absolute `https://` one to the proxy path (never the origin), a nest-relative one
    /// as-is — while a zero-hash `audio/*` still folds to nothing, a native blob `Video` is
    /// unchanged, and the post still reads `media_hash == Some("")` when every item is remote.
    #[test]
    fn a_bridged_video_folds_to_a_proxied_video_never_the_origin() {
        use fauna_core::render::RenderBlock;
        let relative = "/api/v1/media/proxy?url=https%3A%2F%2Ff.example%2Fv.webm";
        let (_nest, m) = resolve_bridged(vec![
            remote_item("video/mp4", "https://files.example/a.mp4"),
            remote_item("audio/mpeg", "https://files.example/b.mp3"),
            remote_item("video/webm", relative),
        ]);
        let aa = &m.snapshot().posts[0];
        assert_eq!(
            aa.document.media_blocks(),
            vec![
                RenderBlock::ProxiedVideo {
                    path: "/api/v1/media/proxy?url=https%3A%2F%2Ffiles.example%2Fa.mp4".into(),
                    alt: String::new(),
                },
                RenderBlock::ProxiedVideo {
                    path: relative.into(),
                    alt: String::new(),
                },
            ],
        );
        assert_eq!(aa.document.first_video_hash(), None, "no zero-hash Video");
        assert!(aa.document.proxied_images().is_empty());
        assert_eq!(aa.media_hash.as_deref(), Some(""));

        let native = fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([0x6b; 32]),
            media_type: "video/mp4".into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let (_nest, m) = resolve_bridged(vec![native]);
        assert_eq!(
            m.snapshot().posts[0].document.media_blocks(),
            vec![RenderBlock::Video {
                hash: "6b".repeat(32),
                alt: String::new(),
            }],
            "a native blob video is unchanged",
        );
    }

    /// The e2e finding behind D6c: the nest stores a bridged post as a BARE canonical
    /// `Post` (no signed envelope), so `resolve_media` must decode it — bound to its id
    /// — or no bridged post's picture ever folds. Nothing to verify, so it stays
    /// `Unchecked` (no badge) rather than `Failed`.
    #[test]
    fn resolve_media_folds_a_bare_bridged_body_unchecked() {
        use fauna_core::data::{PostBody, Timestamp};
        use fauna_core::identity::ActorId;
        use fauna_core::render::{RenderBlock, VerificationStatus};
        let path = "/api/v1/bluesky/media?url=y";
        let bare = fauna_core::data::Post {
            author: ActorId([9u8; 32]),
            created_at: Timestamp(5),
            body: PostBody::TextWithMedia {
                content: "from bluesky".into(),
                facets: vec![],
                items: vec![remote_item("image/jpeg", path)],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let bytes = fauna_core::encoding::canonical_encode(&bare).unwrap();
        let id = fauna_client_core::post::wire_post_id(&bytes);

        let nest = MockNest::arc();
        let mut p = post(&id, 1);
        p.has_media = true;
        nest.push_page(vec![p], None);
        nest.inner.lock().unwrap().posts_get_body = bytes;
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_media(id.clone()));
        let got = &m.snapshot().posts[0];
        assert_eq!(got.media_hash.as_deref(), Some(""));
        assert_eq!(
            got.document.media_blocks(),
            vec![RenderBlock::ProxiedImage {
                path: path.into(),
                alt: String::new(),
            }],
        );
        assert_eq!(got.verification, VerificationStatus::Unchecked);
    }

    /// D6c case 3: a native blob post is unchanged — `Image` by hash, `media_hash` the hash.
    #[test]
    fn a_native_blob_post_folds_unchanged_beside_the_proxied_arm() {
        use fauna_core::render::RenderBlock;
        let native = fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([0x5a; 32]),
            media_type: "image/png".into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let (_nest, m) = resolve_bridged(vec![native]);
        let aa = &m.snapshot().posts[0];
        assert_eq!(aa.media_hash.as_deref(), Some("5a".repeat(32).as_str()));
        assert_eq!(
            aa.document.media_blocks(),
            vec![RenderBlock::Image {
                hash: "5a".repeat(32),
                alt: String::new(),
            }],
        );
    }

    /// D6c's guard: `media_hash` is the fire-once resolve guard, so an all-remote post must
    /// rest `Some("")` after its resolve — never `None` (which re-fires the post fetch on
    /// every render pass) and never the zero hash's hex. A second `resolve_media` is a no-op:
    /// a different body served now changes nothing.
    #[test]
    fn an_all_remote_post_resolves_media_hash_to_empty_and_never_refires() {
        use fauna_core::render::RenderBlock;
        let path = "/api/v1/bluesky/media?url=x";
        let (nest, m) = resolve_bridged(vec![remote_item("image/jpeg", path)]);
        assert_eq!(m.snapshot().posts[0].media_hash.as_deref(), Some(""));

        let kp = ActorKeypair::from_secret([7u8; 32]);
        let other = fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([0x77; 32]),
            media_type: "image/png".into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        nest.inner.lock().unwrap().posts_get_body =
            build_post_with_media(&kp, "changed", vec![other], &[], None).unwrap();
        block_on(m.resolve_media("aa".into()));
        let aa = &m.snapshot().posts[0];
        assert_eq!(aa.media_hash.as_deref(), Some(""));
        assert_eq!(
            aa.document.media_blocks(),
            vec![RenderBlock::ProxiedImage {
                path: path.into(),
                alt: String::new(),
            }],
            "the second resolve must not have fetched",
        );
    }

    /// The multi-item fold must SURVIVE a later embed resolving. The rebuild paths recover the
    /// media from `document.media_blocks()`; recovering it from the single-hash `media_hash`
    /// instead would silently collapse this post back to one image the moment its quote landed.
    #[test]
    fn a_later_quote_resolve_does_not_collapse_multi_item_media() {
        use fauna_core::render::RenderBlock;
        let nest = MockNest::arc();
        let mut quoting = post("aa", 2);
        quoting.quoted_post_id = Some("bb".into());
        quoting.has_media = true;
        let mut quoted = post("bb", 1);
        quoted.body = "quoted body".into();
        nest.push_page(vec![quoting, quoted], None);
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let item = |byte: u8, ty: &str| fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([byte; 32]),
            media_type: ty.into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let bytes = build_post_with_media(
            &kp,
            "hi",
            vec![item(0x11, "image/png"), item(0x22, "video/mp4")],
            &[],
            None,
        )
        .unwrap();
        nest.inner.lock().unwrap().posts_get_body = bytes;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_media("aa".into()));
        block_on(m.resolve_quoted_post("bb".into()));

        let aa = m
            .snapshot()
            .posts
            .iter()
            .find(|p| p.post_id == "aa")
            .cloned()
            .expect("aa is loaded");
        assert!(
            aa.document.has_quoted_post(),
            "the quote must have folded in"
        );
        let kinds: Vec<&str> = aa
            .document
            .media_blocks()
            .iter()
            .map(|b| match b {
                RenderBlock::Image { .. } => "image",
                RenderBlock::Video { .. } => "video",
                other => panic!("non-media block: {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["image", "video"],
            "both media blocks survive the quote rebuild"
        );
    }

    #[test]
    fn quote_and_media_fold_in_body_order_independent_of_resolution_order() {
        use fauna_core::render::RenderBlock;
        // "aa" both quotes "bb" AND has media. Resolve media FIRST, then the
        // quote: the final document must end [..body.., QuotedPost, Image] — the
        // quote survives the media rebuild (resolved_quotes store) and the order
        // is body → quote → media regardless of which embed resolved first.
        let nest = MockNest::arc();
        let mut quoting = post("aa", 2);
        quoting.quoted_post_id = Some("bb".into());
        quoting.has_media = true;
        let mut quoted = post("bb", 1);
        quoted.body = "quoted body".into();
        nest.push_page(vec![quoting, quoted], None);
        let kp = ActorKeypair::from_secret([7u8; 32]);
        let media = fauna_core::data::MediaItem {
            blob_hash: fauna_core::data::ContentHash::from_digest_raw([0xcd; 32]),
            media_type: "image/png".into(),
            size_bytes: 10,
            dimensions: None,
            thumbnail: None,
            ..Default::default()
        };
        let bytes = build_post_with_media(&kp, "hi", vec![media], &[], None).unwrap();
        nest.inner.lock().unwrap().posts_get_body = bytes;

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_media("aa".into())); // media first
        block_on(m.resolve_quoted_post("bb".into())); // quote second

        let aa = m
            .snapshot()
            .posts
            .into_iter()
            .find(|p| p.post_id == "aa")
            .unwrap();
        let kinds: Vec<&str> = aa
            .document
            .blocks
            .iter()
            .map(|b| match b {
                RenderBlock::Paragraph { .. } => "para",
                RenderBlock::QuotedPost { .. } => "quote",
                RenderBlock::Image { .. } => "image",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["para", "quote", "image"]);
    }

    // ── The sealed-factor compose seam (topic-factors.md § Scoring) ──────────

    /// A feed with no composition stays chronological: no `order=score`, and the
    /// nest's order is handed through untouched. The seam must not promote a
    /// plain feed to score order just because it *could*.
    #[test]
    fn a_feed_without_a_composition_is_not_score_ordered() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 3), post("bb", 2)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        let req: FeedPostsRequest = nest.req("fauna.feed.posts");
        assert_eq!(req.order, None, "no composition ⇒ no score ordering");
        assert_eq!(ids(&m), vec!["aa", "bb"], "the nest order survives");
    }

    /// The load-bearing round-trip: a feed composing a trained topic is fetched
    /// `order=score`, and the loaded window is re-ranked by
    /// `FeedPostItem.score + Σ sealed contributions` — so an on-topic post with
    /// the *lower* nest key rises above an off-topic one with the higher key.
    /// The nest never sees the model, the match, or the resulting order.
    #[test]
    fn a_trained_topic_factor_reranks_the_loaded_window() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        nest.seed_model(CATS, &cats_model());
        // "fin" leads on the nest's key; "cat" is the on-topic post behind it.
        nest.push_page(
            vec![
                scored_post("fin", 2, 3_000_000, "quarterly earnings guidance"),
                scored_post("cat", 1, 1_000_000, "a fluffy cat purring"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        let req: FeedPostsRequest = nest.req("fauna.feed.posts");
        assert_eq!(req.order.as_deref(), Some("score"));
        assert_eq!(
            ids(&m),
            vec!["cat", "fin"],
            "the sealed topic contribution outweighs the nest's engagement gap",
        );
    }

    /// A **subscribed** `text-model` labeler re-ranks the loaded window — and
    /// does it for a post the publisher never saw.
    ///
    /// That generalization is the whole reason the kind exists: a List can only
    /// carry ids its publisher already scored, so it cannot move a post that
    /// was not in its corpus. Here the artifact is built from a cat/finance
    /// vocabulary and the on-topic post ("a small ginger kitten dozing") shares
    /// **no** words with any exemplar id — only the learned n-grams.
    #[test]
    fn a_subscribed_text_model_reranks_an_unseen_post() {
        let labeler_id = [0xABu8; 32];
        let factor =
            fauna_core::scoring::labeler_factor(&fauna_core::identity::ActorId(labeler_id));
        // The vocabulary a publisher's scrub over public cat/finance examples
        // would produce (counts clear the distinct-document prune floor).
        let artifact = fauna_core::scoring::build_text_model_artifact(
            Some("Small orange cats"),
            6,
            6,
            vec![
                ("cat".to_string(), 6, 0),
                ("kitten".to_string(), 5, 0),
                ("dozing".to_string(), 4, 0),
                ("earnings".to_string(), 0, 6),
                ("quarterly".to_string(), 0, 5),
                ("guidance".to_string(), 0, 4),
            ],
        )
        .unwrap();

        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(&factor, 1000)]);
        nest.seed_labeler_artifact(labeler_id, "text-model", artifact);
        // "fin" leads on the nest's key; the cat post trails it 3:1.
        nest.push_page(
            vec![
                scored_post("fin", 2, 3_000_000, "quarterly earnings guidance"),
                scored_post("cat", 1, 1_000_000, "a small ginger kitten dozing"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        assert_eq!(
            ids(&m),
            vec!["cat", "fin"],
            "a subscribed model must move a post its publisher never saw",
        );
    }

    /// ⚠ An artifact at a tokenizer `version` this build cannot score is left
    /// **INERT** — the nest's order stands exactly as sent.
    ///
    /// The alternative is the one outcome the version field exists to prevent:
    /// scoring n-grams with a tokenizer that would have split them differently,
    /// silently mis-ranking the user's feed against a model that says something
    /// else. Inert is not a failure mode here, it is the contract (frame
    /// § Tier-3 artifact kinds: "treats the factor as inert and says so") — and
    /// the saying-so is the catalog row's kind badge, not a feed banner, so the
    /// feed must also stay free of an error.
    #[test]
    fn a_text_model_at_an_unknown_tokenizer_version_is_inert() {
        let labeler_id = [0xCDu8; 32];
        let factor =
            fauna_core::scoring::labeler_factor(&fauna_core::identity::ActorId(labeler_id));
        // Same vocabulary as the test above — the ONLY difference is a version
        // this build does not implement, so any reordering here would be the
        // mis-score the contract forbids.
        let artifact = fauna_core::scoring::TextModelArtifact {
            version: fauna_core::scoring::TEXT_MODEL_ARTIFACT_VERSION + 7,
            more_docs: 6,
            less_docs: 6,
            ngrams: vec![
                fauna_core::scoring::TextModelNgram {
                    ngram: "cat".to_string(),
                    more: 6,
                    less: 0,
                },
                fauna_core::scoring::TextModelNgram {
                    ngram: "kitten".to_string(),
                    more: 5,
                    less: 0,
                },
            ],
            name: None,
        };
        let bytes = fauna_core::encoding::canonical_encode(&artifact).unwrap();

        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(&factor, 1000)]);
        nest.seed_labeler_artifact(labeler_id, "text-model", bytes);
        nest.push_page(
            vec![
                scored_post("fin", 2, 3_000_000, "quarterly earnings guidance"),
                scored_post("cat", 1, 1_000_000, "a small ginger kitten dozing"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        assert_eq!(
            ids(&m),
            vec!["fin", "cat"],
            "an unscoreable version must leave the nest's order untouched",
        );
        assert!(
            m.snapshot().error.is_none(),
            "inert is the contract, not an error — the badge says so, not a banner"
        );
    }

    /// ⚠⚠ THE corpus-read privacy step: the rebuild is built from the marked
    /// posts that are **publicly readable**, and a tier-gated one is dropped
    /// even though the publisher can fetch it perfectly well.
    ///
    /// That last clause is the whole point. "It fetched" proves nothing — the
    /// author is entitled to their own restricted content — so the gate is read
    /// off `Post.gated` in the post's own signed bytes. Here the factor has
    /// three markers: one public post, one tier-gated post whose fetch SUCCEEDS,
    /// and one that no longer exists. Only the first may teach the artifact, and
    /// the vocabulary must carry no word that appears solely in the other two.
    #[test]
    fn the_publish_corpus_read_drops_restricted_and_unfetchable_examples() {
        let kp = ActorKeypair::from_secret([9u8; 32]);
        let public_id = "aa".repeat(32);
        let gated_id = "bb".repeat(32);
        let missing_id = "cc".repeat(32);

        // The one public example. Repeated wording is what lets any n-gram clear
        // the 3-distinct-document floor from a single-document corpus... it
        // cannot, by design — so this test asserts the ABSENCES, and the
        // floor's own behaviour is pinned in `fauna-text-model`.
        let public_bytes = build_post(&kp, "an orange cat on a windowsill", &[], None).unwrap();

        // A tier-gated post. Its fetch SUCCEEDS (the publisher is the author),
        // so only `Post.gated` distinguishes it.
        let gated = fauna_core::data::Post {
            author: kp.actor_id(),
            created_at: Timestamp::now(),
            body: fauna_core::data::PostBody::Text {
                content: "confidential merger negotiations".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: Some(fauna_core::subscription::types::GatedInfo {
                encrypted_ref: fauna_core::data::ContentHash::from_digest_raw([0x11; 32]),
                key_access: fauna_core::subscription::types::KeyAccess::Broadcast {
                    key_blob_ref: fauna_core::data::ContentHash::from_digest_raw([0x33; 32]),
                },
                tier: "supporters".into(),
                tier_rank: 1,
                seal_id: fauna_core::data::ContentHash::from_digest_raw([0x5e; 32]),
                attachment_refs: vec![],
            }),
            content_warning: None,
            origin: None,
        };
        let gated_bytes = sign_and_pack(&kp, &gated).unwrap();

        let nest = MockNest::arc();
        {
            let mut g = nest.inner.lock().unwrap();
            g.posts_get_by_id.insert(public_id.clone(), public_bytes);
            g.posts_get_by_id.insert(gated_id.clone(), gated_bytes);
            // `missing_id` is deliberately NOT seeded ⇒ the fetch fails.
        }

        // A factor marked with all three.
        let mut model = TopicModel::new();
        model.train(
            &public_id,
            "an orange cat on a windowsill",
            ExampleLabel::MoreLikeThis,
        );
        model.train(
            &gated_id,
            "confidential merger negotiations",
            ExampleLabel::MoreLikeThis,
        );
        model.train(&missing_id, "a deleted post", ExampleLabel::MoreLikeThis);
        nest.seed_model(CATS, &model);

        let m = mgr(nest.clone());
        let review = block_on(m.scrub_corpus_for_factor(CATS)).expect("corpus read");

        assert_eq!(review.marked_examples, 3, "the factor marks three posts");
        assert_eq!(
            review.included_examples, 1,
            "only the publicly-readable one may teach the artifact"
        );
        assert_eq!(review.more_docs, 1);
        assert_eq!(review.less_docs, 0);

        let grams: Vec<&str> = review.ngrams.iter().map(|n| n.ngram.as_str()).collect();
        assert!(
            !grams
                .iter()
                .any(|g| g.contains("confidential") || g.contains("merger")),
            "restricted text must not reach the vocabulary: {grams:?}"
        );
        assert!(
            !grams.iter().any(|g| g.contains("deleted")),
            "an unfetchable example must not reach the vocabulary: {grams:?}"
        );
    }

    /// A subscribed **`list`** labeler does NOT ride the client-side seam: the
    /// nest materializes its rows, so it already arrives as a bus term, and
    /// scoring it again here would double-count it.
    #[test]
    fn a_subscribed_list_labeler_is_not_scored_client_side() {
        let labeler_id = [0xEFu8; 32];
        let factor =
            fauna_core::scoring::labeler_factor(&fauna_core::identity::ActorId(labeler_id));
        let list =
            fauna_core::scoring::build_list_artifact(None, vec![([0x01u8; 32], 1000)]).unwrap();

        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(&factor, 1000)]);
        nest.seed_labeler_artifact(labeler_id, "list", list);
        nest.push_page(
            vec![
                scored_post("fin", 2, 3_000_000, "quarterly earnings guidance"),
                scored_post("cat", 1, 1_000_000, "a small ginger kitten dozing"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        assert_eq!(ids(&m), vec!["fin", "cat"]);
        assert!(m.snapshot().error.is_none());
    }

    /// An untrained model is a flat 500 per-mille, which shifts every post
    /// equally — so it must leave the nest's order *exactly* as sent. This is
    /// what makes creating a topic factor safe before teaching it anything.
    #[test]
    fn an_untrained_topic_factor_leaves_the_nest_order_alone() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        nest.seed_model(CATS, &TopicModel::new());
        nest.push_page(
            vec![
                scored_post("aa", 3, 3_000_000, "a fluffy cat purring"),
                scored_post("bb", 2, 2_000_000, "quarterly earnings"),
                scored_post("cc", 1, 1_000_000, "something else"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));
        assert_eq!(ids(&m), vec!["aa", "bb", "cc"]);
    }

    /// The muted-keyword penalty rides the same seam, and it is **implicit**:
    /// the user muted a word, not a composition entry, yet the −1000 penalty
    /// sinks the matching post to the bottom of a score-ordered window.
    #[test]
    fn a_muted_keyword_sinks_a_post_without_any_composition_entry_for_it() {
        let nest = MockNest::arc();
        // The feed composes only engagement — nothing names `muted-keywords`.
        nest.set_feed_composition(vec![entry("engagement", 1000)]);
        nest.seed_muted_keywords(&["spoiler"]);
        nest.push_page(
            vec![
                scored_post("muted", 3, 9_000_000, "SPOILER: the butler did it"),
                scored_post("plain", 2, 1_000_000, "an ordinary post"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        assert_eq!(
            ids(&m),
            vec!["plain", "muted"],
            "the muted post sinks despite leading on engagement 9:1",
        );
        // …and it is flagged for the collapse-to-placeholder render treatment.
        assert!(m.is_muted("muted"));
        assert!(!m.is_muted("plain"));
    }

    /// A mute *collapses* everywhere but only *sinks* where there is an order to
    /// sink in. On a chronological feed the manager must not reorder anything —
    /// but `is_muted` still reports the match, which is what drives the collapse.
    #[test]
    fn a_mute_collapses_but_does_not_reorder_a_chronological_feed() {
        let nest = MockNest::arc();
        nest.seed_muted_keywords(&["spoiler"]);
        nest.push_page(vec![post("muted", 3), post("plain", 2)], None);
        {
            let mut g = nest.inner.lock().unwrap();
            g.pages[0].0[0].body = "SPOILER: the butler did it".into();
        }
        let m = mgr(nest.clone());
        block_on(m.select_feed(None)); // the local feed — always chronological

        assert_eq!(
            ids(&m),
            vec!["muted", "plain"],
            "a chronological feed keeps its order; the mute is a render signal here",
        );
        assert!(m.is_muted("muted"), "the collapse signal still fires");
    }

    /// Score-order pagination rides the **keyset** cursor: the manager echoes
    /// back both the key and its `created_at` tiebreak, and re-ranks the grown
    /// window as a whole (a second page's post can legitimately outrank a first
    /// page's one under a sealed factor).
    #[test]
    fn score_order_paginates_on_the_keyset_cursor_and_reranks_the_grown_window() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        nest.seed_model(CATS, &cats_model());
        nest.push_page(
            vec![scored_post(
                "p1",
                9,
                5_000_000,
                "quarterly earnings guidance",
            )],
            None,
        );
        // Page 1's reply carries the keyset cursor; page 2 must echo both halves.
        {
            let mut g = nest.inner.lock().unwrap();
            g.score_cursor = Some((5_000_000, 9));
        }
        nest.push_page(
            vec![scored_post("p2", 8, 1_000_000, "a fluffy cat purring")],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));
        assert_eq!(ids(&m), vec!["p1"]);
        assert!(m.snapshot().has_more, "a score cursor means another page");

        {
            let mut g = nest.inner.lock().unwrap();
            g.score_cursor = None; // page 2 is the last
        }
        block_on(m.load_more());

        let reqs: Vec<FeedPostsRequest> = nest
            .calls()
            .iter()
            .filter(|(k, _)| k == "fauna.feed.posts")
            .map(|(_, b)| fauna_protocol::decode_strict(b).unwrap())
            .collect();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[1].score_cursor, Some(5_000_000), "the key half");
        assert_eq!(
            reqs[1].score_cursor_created_at,
            Some(9),
            "the tiebreak half — without it a flat-keyed feed never advances",
        );
        assert_eq!(
            ids(&m),
            vec!["p2", "p1"],
            "the second page's on-topic post outranks the first page's off-topic one",
        );
    }

    /// The train gesture, end to end: fetch the post's FULL text, apply the
    /// forward delta, re-seal, put — and the feed re-ranks against the new model
    /// immediately, with no reload.
    #[test]
    fn training_a_post_reseals_the_model_and_reranks_the_feed_at_once() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        nest.seed_model(CATS, &cats_model());
        // Both previews are *neutral* to the seeded model (they share none of
        // its vocabulary), so it scores them both a flat 500 and the window
        // starts in exactly the nest's order — isolating what the train changes.
        nest.push_page(
            vec![
                scored_post("fin", 3, 5_000_000, "meeting notes from monday"),
                scored_post("kit", 2, 1_000_000, "tiny kitten spotted"),
            ],
            None,
        );
        // The full body the train fetches — richer than the 500-char preview it
        // is scored by, which is exactly why training refetches at all
        // (§ Training signals). It teaches the model "kitten"/"tiny"/"spotted",
        // which is what makes `kit`'s *preview* score above neutral afterwards.
        seed_posts_get_body(&nest, "tiny kitten spotted, soft fluffy cat purring away");

        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));
        assert_eq!(ids(&m), vec!["fin", "kit"], "before training, nest order");

        let outcome =
            block_on(m.train_post("kit".into(), CATS.to_string(), TrainVerb::MoreLikeThis))
                .expect("train succeeds");
        assert_eq!(outcome, TrainResult::Trained);

        // The model that reached the nest really carries the new example…
        let stored = nest.stored_model(CATS).expect("a model was put");
        assert_eq!(
            stored.example_label("kit"),
            Some(ExampleLabel::MoreLikeThis),
            "the marker round-tripped through the seal",
        );
        // …and the manager reads it back for the toggle state…
        assert_eq!(
            m.example_label_for("kit", CATS),
            Some(TrainVerb::MoreLikeThis),
        );
        // …and the loaded window re-ranked on the spot.
        assert_eq!(
            ids(&m),
            vec!["kit", "fin"],
            "the just-trained post rises without a reload",
        );
    }

    /// Re-tapping the SAME verb is a no-op in the model, so the manager must not
    /// issue a pointless put (and a pointless last-put-wins race with the user's
    /// other devices). The guard lives in the model, not the UI.
    #[test]
    fn re_tapping_the_same_verb_is_a_duplicate_signal_and_writes_nothing() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        nest.seed_model(CATS, &cats_model());
        nest.push_page(vec![scored_post("kit", 2, 1_000_000, "a kitten")], None);
        seed_posts_get_body(&nest, "a tiny kitten purring");
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        block_on(m.train_post("kit".into(), CATS.into(), TrainVerb::MoreLikeThis)).unwrap();
        let puts_after_first = put_count(&nest);

        let outcome =
            block_on(m.train_post("kit".into(), CATS.into(), TrainVerb::MoreLikeThis)).unwrap();
        assert_eq!(outcome, TrainResult::DuplicateSignal);
        assert_eq!(
            put_count(&nest),
            puts_after_first,
            "a duplicate signal must not write the model back",
        );
    }

    /// Flipping to the other verb applies the inverse of the old delta and then
    /// the forward of the new — never a double-count. The marker follows.
    #[test]
    fn flipping_the_verb_inverts_the_old_delta_then_applies_the_new() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        nest.seed_model(CATS, &cats_model());
        nest.push_page(vec![scored_post("kit", 2, 1_000_000, "a kitten")], None);
        seed_posts_get_body(&nest, "a tiny kitten purring");
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        block_on(m.train_post("kit".into(), CATS.into(), TrainVerb::MoreLikeThis)).unwrap();
        let outcome =
            block_on(m.train_post("kit".into(), CATS.into(), TrainVerb::LessLikeThis)).unwrap();
        assert_eq!(outcome, TrainResult::Flipped);
        assert_eq!(
            nest.stored_model(CATS).unwrap().example_label("kit"),
            Some(ExampleLabel::LessLikeThis),
        );
    }

    /// Un-marking applies the exact inverse and drops the marker — and the model
    /// returns **byte-for-byte** to what it was before the train. That exactness
    /// is the whole reason the primitive uses reversible deltas.
    #[test]
    fn untraining_restores_the_model_byte_for_byte() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        let before = cats_model();
        nest.seed_model(CATS, &before);
        nest.push_page(vec![scored_post("kit", 2, 1_000_000, "a kitten")], None);
        seed_posts_get_body(&nest, "a tiny kitten purring");
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        block_on(m.train_post("kit".into(), CATS.into(), TrainVerb::MoreLikeThis)).unwrap();
        assert_ne!(
            nest.stored_model(CATS).unwrap().to_bytes(),
            before.to_bytes()
        );

        block_on(m.untrain_post("kit".into(), CATS.into())).unwrap();
        assert_eq!(
            nest.stored_model(CATS).unwrap().to_bytes(),
            before.to_bytes(),
            "train → untrain is an exact round trip",
        );
        assert_eq!(m.example_label_for("kit", CATS), None, "the marker is gone");
    }

    /// A sealed blob that exists but will NOT open (corruption, or a newer
    /// client's layout) must never be mistaken for "never trained" — that would
    /// score as untrained *and* overwrite the user's real model on the next
    /// train. The feed still lists its posts; the failure surfaces as a
    /// non-fatal notice.
    #[test]
    fn an_unopenable_model_surfaces_an_error_instead_of_reading_as_untrained() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        nest.seed_unopenable_model(CATS);
        nest.push_page(vec![scored_post("aa", 2, 1_000_000, "a cat")], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        let snap = m.snapshot();
        assert_eq!(snap.status, FeedStatus::Loaded, "the posts still load");
        assert_eq!(snap.posts.len(), 1);
        assert!(
            snap.error.is_some(),
            "the unopenable trained factor is surfaced, not swallowed",
        );
    }

    /// A trained factor with no stored blob yet (created, never taught) is not
    /// an error — it is simply inert until its first example.
    #[test]
    fn a_never_trained_factor_is_inert_not_an_error() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        // No `seed_model` at all: `model.fetch` answers `sealed_blob: None`.
        nest.push_page(
            vec![
                scored_post("aa", 3, 3_000_000, "a cat"),
                scored_post("bb", 2, 1_000_000, "earnings"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        let snap = m.snapshot();
        assert!(snap.error.is_none(), "an untrained factor is not a failure");
        assert_eq!(ids(&m), vec!["aa", "bb"], "and it has no ordering effect");
    }

    /// Train-in-context: a feed dominated by exactly one trained topic tells the
    /// gesture which factor to train. Two (or zero) is ambiguous — the client
    /// opens the target sheet instead of guessing.
    #[test]
    fn the_train_target_is_the_feeds_single_topic_factor() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry("engagement", 1000), entry(CATS, 3000)]);
        nest.seed_model(CATS, &cats_model());
        nest.push_page(vec![scored_post("aa", 2, 1_000_000, "x")], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));
        assert_eq!(m.train_target_factor().as_deref(), Some(CATS));

        // A second trained topic makes the gesture ambiguous.
        let other = "topic:99887766554433221100ffeeddccbbaa";
        nest.set_feed_composition(vec![entry(CATS, 3000), entry(other, 1000)]);
        nest.seed_model(other, &TopicModel::new());
        nest.push_page(vec![scored_post("aa", 2, 1_000_000, "x")], None);
        block_on(m.select_feed(Some("feed-1".into())));
        assert_eq!(m.train_target_factor(), None, "two topics ⇒ target sheet");
    }

    /// The user's global factor set folds into the feed's own composition — and
    /// a topic factor promoted globally re-ranks a feed that never named it.
    #[test]
    fn a_globally_promoted_topic_factor_reranks_a_feed_that_does_not_name_it() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry("engagement", 1000)]);
        nest.inner.lock().unwrap().global_factors = vec![entry(CATS, 1000)];
        nest.seed_model(CATS, &cats_model());
        nest.push_page(
            vec![
                scored_post("fin", 3, 3_000_000, "quarterly earnings guidance"),
                scored_post("cat", 2, 1_000_000, "a fluffy cat purring"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));
        assert_eq!(
            ids(&m),
            vec!["cat", "fin"],
            "the global topic promotion reaches a feed with no topic of its own",
        );
    }

    /// Seed the body `fauna.posts.get` answers with — a real signed post, so the
    /// train path exercises the actual decode (`decode_post`), not a stub.
    fn seed_posts_get_body(nest: &Arc<MockNest>, text: &str) {
        let kp = ActorKeypair::from_secret(TEST_SECRET);
        let bytes = build_post(&kp, text, &["cat".to_string()], None).unwrap();
        nest.inner.lock().unwrap().posts_get_body = bytes;
    }

    fn put_count(nest: &Arc<MockNest>) -> usize {
        nest.kinds()
            .iter()
            .filter(|k| *k == "fauna.personalization.model.put")
            .count()
    }

    // Re-exports used by the shared harness live in the sibling test module;
    // this module covers the gate-to-tier compose + gated-unlock surface
    // (`ui/feed.md` § Encryption at rest; monetization.md § Pillars 2+3).

    const TIER: &str = "gold";
    const PERIOD_KEY: [u8; 32] = [0x42u8; 32];

    fn gated_item(id: &str, tier: &str, preview: &str) -> FeedPostItem {
        FeedPostItem {
            post_id: id.into(),
            author: "22".repeat(32),
            body: preview.into(),
            created_at: 1_000_000,
            source: "fauna".into(),
            gated_tier: Some(tier.into()),
            ..Default::default()
        }
    }

    #[test]
    fn prepare_gated_blob_is_none_for_a_public_compose() {
        let nest = MockNest::arc();
        let m = mgr(nest);
        m.update_compose("hello".into(), String::new(), None);
        let out = block_on(m.prepare_gated_blob()).expect("public compose is fine");
        assert!(out.is_none(), "no gate tier selected ⇒ no gated build");
    }

    #[test]
    fn gated_compose_stages_seals_and_creates() {
        let nest = MockNest::arc();
        nest.seed_tier(TIER, 2);
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.seed_key_blob(1, vec![0x11u8; 32], vec![0xAA]);
        let m = mgr(nest.clone());

        block_on(m.refresh_own_tiers());
        assert_eq!(
            m.snapshot().own_tiers,
            vec![crate::compose::GateTierOption {
                name: TIER.into(),
                rank: 2
            }],
            "tiers.list populates the gate select options"
        );

        m.update_compose("the full premium body".into(), String::new(), None);
        m.update_compose_gate(Some(TIER.into()), "public teaser".into());

        let blob = block_on(m.prepare_gated_blob())
            .expect("gated build succeeds")
            .expect("gated compose produces a sealed blob");
        assert!(!blob.is_empty());
        assert!(
            m.snapshot().compose.submitting,
            "staged build marks submitting"
        );

        // The upload echo check: a mangled hash refuses to create the post…
        let err =
            block_on(m.submit_gated_post("00".repeat(32))).expect_err("hash mismatch must refuse");
        assert!(err.contains("staged encrypted_ref"), "{err}");

        // …and the correct content address creates it and clears the composer.
        let blob2 = block_on(m.prepare_gated_blob()).unwrap().unwrap();
        let hash = hex::encode(blake3::hash(&blob2).as_bytes());
        block_on(m.submit_gated_post(hash)).expect("gated create lands");
        assert!(
            nest.kinds().contains(&"fauna.posts.create".to_string()),
            "the staged signed post reached fauna.posts.create"
        );
        let snap = m.snapshot();
        assert_eq!(snap.compose, Default::default(), "composer cleared");
    }

    /// The gated arm's in-flight window is the widest the manager sees: the app
    /// uploads the sealed blob BETWEEN `prepare_gated_blob` and
    /// `submit_gated_post`, and the composer stays editable through it. So the
    /// success clear runs against what the prepare built — carried on the stage
    /// as `PendingGatedPost::sent` — never the composer as it stands when the
    /// create confirms (`ui/feed.md` § User actions, `post-submit-button`). This
    /// is also the exact security-review case: the audience is
    /// sticky, so starting the next post during the upload must not silently
    /// widen the tier that is still in flight to Public.
    #[test]
    fn a_gated_submit_clears_what_it_staged_and_keeps_the_sticky_tier_when_content_changed() {
        let nest = MockNest::arc();
        nest.seed_tier(TIER, 2);
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.seed_key_blob(1, vec![0x11u8; 32], vec![0xAA]);
        let m = mgr(nest.clone());
        block_on(m.refresh_own_tiers());

        m.update_compose("the full premium body".into(), String::new(), None);
        m.update_compose_gate(Some(TIER.into()), "public teaser".into());
        let blob = block_on(m.prepare_gated_blob()).unwrap().unwrap();

        // The app is uploading the sealed blob; the user starts the next post.
        m.update_compose("the next post".into(), String::new(), None);

        let hash = hex::encode(blake3::hash(&blob).as_bytes());
        block_on(m.submit_gated_post(hash)).expect("gated create lands");
        let compose = m.snapshot().compose;
        assert_eq!(
            compose.text, "the next post",
            "typed during the upload — must not be erased by the create's clear"
        );
        assert_eq!(
            compose.gate_tier,
            Some(TIER.into()),
            "the audience is sticky: it stays with the next post since the text changed (owner ruling, dcxix)"
        );
        assert_eq!(
            compose.gate_preview, "public teaser",
            "the teaser stays with its sticky tier"
        );
        assert!(!compose.submitting, "the attempt that just landed is over");
    }

    /// A **real** PNG signature followed by bytes no PNG decoder can parse:
    /// `process_media` sniffs `image/png` off the signature (no decode) but
    /// renders no thumbnail, so the fixture exercises the real pipeline and
    /// still pins a deterministic single-blob payload. Same shape, and same
    /// reasoning, as `fauna_client::media_upload`'s own seal pin.
    fn png_signature_fixture() -> Vec<u8> {
        let mut raw = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        raw.extend_from_slice(b"not a decodable PNG body, just the signature");
        raw
    }

    /// The compose leg of `ui/media.md` § Encryption at rest — *one per-post
    /// key seals the post body and all attachments together*.
    ///
    /// Until 2026-09-07 a staged attachment on a gated compose was **silently
    /// dropped**: `prepare_gated_blob` read only `(text, gate_preview,
    /// gate_tier)` and `build_gated_post` hardcoded `PostBody::Text`, so a
    /// creator who attached a photo and then picked a tier published the
    /// caption alone with no error. This asserts the whole chain: the bytes
    /// leave as ciphertext, the sealed body is `TextWithMedia`, and the key
    /// derived from the post's *own* `seal_id` opens both halves — which is
    /// exactly what makes `unlock_gated_post`'s `sealed_media_keys`
    /// registration find a photo to open.
    #[test]
    fn a_gated_compose_seals_its_attachment_under_the_posts_own_key() {
        use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};

        let nest = MockNest::arc();
        nest.seed_tier(TIER, 2);
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.seed_key_blob(1, vec![0x11u8; 32], vec![0xAA]);
        let m = mgr(nest.clone());
        block_on(m.refresh_own_tiers());

        m.update_compose("the full premium body".into(), String::new(), None);
        m.update_compose_gate(Some(TIER.into()), "public teaser".into());

        // 1. The seal runs against the composer's CURRENT audience, and what
        //    the app is handed to POST is ciphertext — no plaintext copy of a
        //    restricted post's photo ever reaches the nest.
        let raw = png_signature_fixture();
        let sealed = block_on(m.seal_compose_attachment(raw.clone())).expect("gated seal succeeds");
        assert!(sealed.sealed, "a gated compose AEAD-seals its attachment");
        assert_ne!(sealed.primary.bytes, raw, "the POSTed bytes are ciphertext");
        assert_eq!(
            sealed.media_type, "image/png",
            "the real MIME comes back for the MediaItem, not the sealed \
             sidecar's application/octet-stream"
        );

        // 2. The app stages the hash the nest will assign. An AEAD-sealed blob
        //    is stored verbatim, so blake3(sealed bytes) IS that hash.
        let blob_hash = hex::encode(blake3::hash(&sealed.primary.bytes).as_bytes());
        m.update_compose(
            "the full premium body".into(),
            String::new(),
            Some(AttachedFile {
                name: "photo.png".into(),
                size: raw.len() as u64,
                blob_hash: Some(blob_hash.clone()),
                media_type: Some(sealed.media_type.clone()),
            }),
        );

        // 3. Submit through the unchanged upload+create pair.
        let body_blob = block_on(m.prepare_gated_blob())
            .expect("gated build succeeds")
            .expect("a gated compose produces a sealed blob");
        let hash = hex::encode(blake3::hash(&body_blob).as_bytes());
        block_on(m.submit_gated_post(hash)).expect("gated create lands");

        // 4. The post's own seal id derives the one key.
        let created: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        let (post, _) =
            fauna_client_core::post::decode_post(created.body.as_ref()).expect("decode post");
        let gated = post.gated.expect("the post is gated");
        let seal_id = gated.seal_id;
        let key = derive_post_key(&PERIOD_KEY, &seal_id);

        // 5. …and it opens the BODY, which carries the item.
        let plain = decrypt_content(&key, &body_blob).expect("the body opens under the post key");
        let full: fauna_core::data::PostBody =
            fauna_core::encoding::canonical_decode(&plain).expect("decode full body");
        let items = match &full {
            fauna_core::data::PostBody::TextWithMedia { content, items, .. } => {
                assert_eq!(content, "the full premium body");
                items.clone()
            }
            other => {
                panic!("a gated compose with an attachment must seal TextWithMedia: {other:?}")
            }
        };
        assert_eq!(items.len(), 1);
        assert_eq!(hex::encode(items[0].blob_hash.digest()), blob_hash);
        assert_eq!(items[0].media_type, "image/png");

        // 6. …and the SAME key opens the attachment. This is the property the
        //    whole design exists for: a reader who can open the body can open
        //    its photos, with no second key and no second grant.
        let opened =
            decrypt_content(&key, &sealed.primary.bytes).expect("the photo opens under it too");
        assert_eq!(opened, raw);
    }

    /// A gated compose whose gate was TOUCHED after the attachment was sealed
    /// must not publish: the photo is sealed under the seal id minted then, and
    /// a body sealed under a fresh one would name an item nobody — the author
    /// included — can ever open. `update_compose_gate` drops the stash on any
    /// gate edit (even a re-select of the same tier, which is what this drives),
    /// and `prepare_gated_blob` refuses rather than stranding the photo: the
    /// manager no longer holds the plaintext to re-seal.
    #[test]
    fn a_gate_edit_after_the_attachment_seal_refuses_rather_than_stranding_the_photo() {
        let nest = MockNest::arc();
        nest.seed_tier(TIER, 2);
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.seed_key_blob(1, vec![0x11u8; 32], vec![0xAA]);
        let m = mgr(nest.clone());
        block_on(m.refresh_own_tiers());

        m.update_compose("body".into(), String::new(), None);
        m.update_compose_gate(Some(TIER.into()), "teaser".into());
        let sealed = block_on(m.seal_compose_attachment(png_signature_fixture())).expect("seals");
        let blob_hash = hex::encode(blake3::hash(&sealed.primary.bytes).as_bytes());

        // The author edits the gate — here, retyping the teaser.
        m.update_compose_gate(Some(TIER.into()), "a better teaser".into());
        m.update_compose(
            "body".into(),
            String::new(),
            Some(AttachedFile {
                name: "photo.png".into(),
                size: 8,
                blob_hash: Some(blob_hash),
                media_type: Some(sealed.media_type),
            }),
        );

        let err = block_on(m.prepare_gated_blob()).expect_err("must refuse");
        assert!(err.contains("re-attach"), "{err}");
        assert!(
            !nest.kinds().contains(&"fauna.posts.create".to_string()),
            "nothing is published"
        );
    }

    /// A **public** compose still takes the plaintext path byte-for-byte — the
    /// audience branch must not have quietly sealed every attachment, which
    /// would break every public photo post on every app.
    #[test]
    fn a_public_compose_attachment_stays_plaintext() {
        let nest = MockNest::arc();
        let m = mgr(nest);
        m.update_compose("hello".into(), String::new(), None);

        let raw = png_signature_fixture();
        let sealed = block_on(m.seal_compose_attachment(raw.clone())).expect("public seal is fine");
        assert!(!sealed.sealed);
        assert_eq!(
            sealed.primary.bytes, raw,
            "a public attachment is signed plaintext, byte-identical"
        );
        assert_eq!(sealed.media_type, "image/png");
    }

    /// The sell twin of `a_gated_compose_seals_its_attachment_under_the_posts_own_key`:
    /// a sold post's photo seals under the tier the sale itself mints.
    ///
    /// The whole difficulty is that the tier does not exist when the author
    /// picks the file, so the flow is two-phase —
    /// [`stage_sell_tier`](FeedManager::stage_sell_tier) mints and persists the
    /// period key, the seal runs against it, and `prepare_sell_post` finishes
    /// on the stage it finds rather than minting a second tier.
    #[test]
    fn a_sell_compose_seals_its_attachment_under_the_tier_it_mints() {
        use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};

        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        m.update_compose("the body you are selling".into(), String::new(), None);
        m.update_compose_sell(
            Some(SellComposeState {
                price: "5 EUR".into(),
                subscribers_get_it_free: true,
                asking_price: "2100".into(),
            }),
            "buy this to read it".into(),
        );

        // 1. Phase one: mint + persist the tier the photo will seal under.
        block_on(m.stage_sell_tier(true, Some(2100))).expect("the sale's tier stages");

        // 2. The seal now has a period key to reach, and produces ciphertext.
        let raw = png_signature_fixture();
        let sealed = block_on(m.seal_compose_attachment(raw.clone())).expect("a staged sale seals");
        assert!(sealed.sealed, "a sold post's attachment is AEAD-sealed");
        assert_ne!(sealed.primary.bytes, raw, "the POSTed bytes are ciphertext");
        assert_eq!(sealed.media_type, "image/png");

        let blob_hash = hex::encode(blake3::hash(&sealed.primary.bytes).as_bytes());
        m.update_compose(
            "the body you are selling".into(),
            String::new(),
            Some(AttachedFile {
                name: "photo.png".into(),
                size: raw.len() as u64,
                blob_hash: Some(blob_hash.clone()),
                media_type: Some(sealed.media_type.clone()),
            }),
        );

        // 3. Phase two finishes on the SAME stage — one tier, not two.
        let body_blob = block_on(m.prepare_sell_post(Some("5 EUR".into()), true, Some(2100)))
            .expect("sell-post build succeeds");
        let hash = hex::encode(blake3::hash(&body_blob).as_bytes());
        block_on(m.submit_gated_post(hash)).expect("gated create lands");
        assert_eq!(
            nest.kinds()
                .iter()
                .filter(|k| *k == "fauna.subscriptions.tiers.create")
                .count(),
            1,
            "staging then preparing must mint ONE tier, not one per call"
        );

        // 4. One key opens the body and the photo, exactly as for a gated post.
        let created: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        let (post, _) =
            fauna_client_core::post::decode_post(created.body.as_ref()).expect("decode post");
        let gated = post.gated.expect("the sold post is gated");
        let seal_id = gated.seal_id;
        let tier_created: fauna_protocol::subscriptions::TierCreateRequest =
            nest.req("fauna.subscriptions.tiers.create");
        assert_eq!(gated.tier, tier_created.name, "gated to the minted tier");

        let period_key = block_on(m.compose_period_key(&tier_created.name))
            .expect("the staged tier's period key is in custody");
        let key = derive_post_key(&<[u8; 32]>::from(period_key.key.clone()), &seal_id);
        let plain = decrypt_content(&key, &body_blob).expect("the body opens under the post key");
        let full: fauna_core::data::PostBody =
            fauna_core::encoding::canonical_decode(&plain).expect("decode full body");
        let items = match &full {
            fauna_core::data::PostBody::TextWithMedia { content, items, .. } => {
                assert_eq!(content, "the body you are selling");
                items.clone()
            }
            other => panic!("a sold post with an attachment must seal TextWithMedia: {other:?}"),
        };
        assert_eq!(items.len(), 1);
        assert_eq!(hex::encode(items[0].blob_hash.digest()), blob_hash);
        assert_eq!(items[0].media_type, "image/png");
        let opened =
            decrypt_content(&key, &sealed.primary.bytes).expect("the photo opens under it too");
        assert_eq!(opened, raw);
    }

    /// Editing the sale after the seal drops the stage, exactly as a gate edit
    /// does — the photo is sealed under the tier that stage minted, and a fresh
    /// mint would name an item nobody can open. Refuse rather than strand it.
    #[test]
    fn a_sell_edit_after_the_attachment_seal_refuses_rather_than_stranding_the_photo() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        m.update_compose("the body you are selling".into(), String::new(), None);
        m.update_compose_sell(
            Some(SellComposeState {
                price: "5 EUR".into(),
                subscribers_get_it_free: true,
                asking_price: String::new(),
            }),
            "buy this to read it".into(),
        );
        block_on(m.stage_sell_tier(true, None)).expect("stages");
        let sealed = block_on(m.seal_compose_attachment(png_signature_fixture())).expect("seals");
        let blob_hash = hex::encode(blake3::hash(&sealed.primary.bytes).as_bytes());

        // The author changes the price after the photo was sealed.
        m.update_compose_sell(
            Some(SellComposeState {
                price: "9 EUR".into(),
                subscribers_get_it_free: true,
                asking_price: String::new(),
            }),
            "buy this to read it".into(),
        );
        m.update_compose(
            "the body you are selling".into(),
            String::new(),
            Some(AttachedFile {
                name: "photo.png".into(),
                size: 8,
                blob_hash: Some(blob_hash),
                media_type: Some(sealed.media_type),
            }),
        );

        let err = block_on(m.prepare_sell_post(Some("9 EUR".into()), true, None))
            .expect_err("must refuse");
        assert!(err.contains("re-attach"), "{err}");
        assert!(
            !nest.kinds().contains(&"fauna.posts.create".to_string()),
            "nothing is published"
        );
    }

    /// "Sell this post…" mints its own tier *inside* `prepare_sell_post`, so
    /// until [`stage_sell_tier`](FeedManager::stage_sell_tier) has run there is
    /// no period key to seal against. Refuse — never fall through to the public
    /// (plaintext) arm, which would upload a readable copy of the very photo
    /// the author is charging for.
    #[test]
    fn a_sell_compose_refuses_to_seal_an_attachment_rather_than_uploading_it_public() {
        let nest = MockNest::arc();
        let m = mgr(nest);
        m.update_compose("the body you are selling".into(), String::new(), None);
        m.update_compose_sell(
            Some(SellComposeState {
                price: "5 EUR".into(),
                subscribers_get_it_free: false,
                asking_price: String::new(),
            }),
            "buy this to read it".into(),
        );

        let err = block_on(m.seal_compose_attachment(png_signature_fixture()))
            .expect_err("a sell compose has no tier yet");
        assert!(err.contains("not supported yet"), "{err}");
    }

    /// The reserved owner-only tier is minted by the archive import, not
    /// chosen by the author at compose time: the gate picker never offers a
    /// hidden tier (`monetization.md` § The unifying model — *A tier may be
    /// hidden*), the same way it never offers a per-post unlock tier.
    #[test]
    fn a_hidden_tier_never_enters_the_gate_picker() {
        let nest = MockNest::arc();
        nest.seed_tier(TIER, 2);
        nest.seed_hidden_tier(fauna_core::subscription::OWNER_ONLY_TIER, u32::MAX);
        let m = mgr(nest);

        block_on(m.refresh_own_tiers());
        assert_eq!(
            m.snapshot().own_tiers,
            vec![crate::compose::GateTierOption {
                name: TIER.into(),
                rank: 2
            }],
        );
    }

    /// The whole "sell this post" ordering, end to end
    /// (`monetization.md` § Per-post pay-to-unlock): one call must auto-mint a
    /// designated tier and gate the composed post to it, in the only order the
    /// constraints permit — tier before post, post id before tier.
    #[test]
    fn sell_post_mints_a_designated_tier_and_gates_the_post_to_it() {
        let nest = MockNest::arc();
        // No key_blob.get seeding: the sell flow derives the birth blob's
        // content address locally. Seeding one would hide a regression.
        let m = mgr(nest.clone());
        m.update_compose("the body you are selling".into(), String::new(), None);
        m.update_compose_gate(None, "buy this to read it".into());

        let blob = block_on(m.prepare_sell_post(Some("5 EUR".into()), false, None))
            .expect("sell-post build succeeds");
        assert!(!blob.is_empty(), "a sealed blob is produced for upload");

        // The tier landed BEFORE the post, carrying the designation.
        let created: fauna_protocol::subscriptions::TierCreateRequest =
            nest.req("fauna.subscriptions.tiers.create");
        let post_id = created
            .unlocks_post
            .clone()
            .expect("the minted tier designates a post");
        assert!(
            created
                .name
                .starts_with(fauna_client_subscriptions::UNLOCK_TIER_PREFIX),
            "minted name: {}",
            created.name
        );
        assert!(
            !created.auto_approve,
            "a sold post must NOT grant to anyone who merely asks — a paid \
             entitlement drains through the payment_entitled path instead"
        );
        assert_eq!(created.price_hint.as_deref(), Some("5 EUR"));
        assert!(
            !nest.kinds().contains(&"fauna.posts.create".to_string()),
            "the post is created only after the blob upload echo"
        );

        // Finish through the SAME submit the ordinary gated flow uses.
        let hash = hex::encode(blake3::hash(&blob).as_bytes());
        block_on(m.submit_gated_post(hash)).expect("gated create lands");

        // The designation names the post the nest will actually store:
        // `post_id = blake3(wire bytes)` (bins/fauna-nest/src/routes.rs:2168).
        let created_post: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        assert_eq!(
            post_id,
            hex::encode(blake3::hash(created_post.body.as_ref()).as_bytes()),
            "the tier sells the post that was actually created"
        );

        // …and the post is really gated to that tier.
        let (post, _) =
            fauna_client_core::post::decode_post(created_post.body.as_ref()).expect("decode");
        let gated = post.gated.expect("the sold post is gated");
        assert_eq!(gated.tier, created.name, "gated to the minted tier");
        assert_eq!(gated.tier_rank, created.rank);
        assert_eq!(
            post.body,
            fauna_core::data::PostBody::Text {
                content: "buy this to read it".into(),
                facets: vec![],
            },
            "the public body is the teaser, never the sold text"
        );
    }

    /// The mint's custody write — the sold tier's fresh period key, recorded
    /// inside `stage_tier` — lands in the period-key store
    /// (`fauna.state.subscriptions`, plane-only).
    #[test]
    fn sell_post_mint_records_custody_in_the_period_key_store() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        m.update_compose("the body you are selling".into(), String::new(), None);
        m.update_compose_gate(None, "buy this to read it".into());

        let blob = block_on(m.prepare_sell_post(Some("5 EUR".into()), false, None))
            .expect("the mint's custody write lands");
        assert!(!blob.is_empty(), "a sealed blob is produced for upload");

        assert!(
            nest.period_keys.folded().tiers.iter().any(|t| t
                .tier_name
                .starts_with(fauna_client_subscriptions::UNLOCK_TIER_PREFIX)),
            "the minted tier's period key is in the period-key store"
        );
    }

    /// Rank is the "do subscribers get it free?" knob (`monetization.md:126`):
    /// rank 1 ⇒ inside every paid subscription; above the author's highest
    /// regular tier ⇒ pure pay-per-view. Nothing else in the model changes.
    #[test]
    fn sell_post_rank_follows_the_subscribers_free_toggle() {
        for (free, want) in [(true, 1u32), (false, 4u32)] {
            let nest = MockNest::arc();
            nest.seed_tier("silver", 1);
            nest.seed_tier("gold", 3);
            let m = mgr(nest.clone());
            m.update_compose("body".into(), String::new(), None);
            m.update_compose_gate(None, "teaser".into());

            block_on(m.prepare_sell_post(None, free, None)).expect("build");
            let created: fauna_protocol::subscriptions::TierCreateRequest =
                nest.req("fauna.subscriptions.tiers.create");
            assert_eq!(
                created.rank, want,
                "subscribers_get_it_free={free} ⇒ rank {want}"
            );
        }
    }

    /// The pay-per-view arm's rank derivation FAILS CLOSED (`monetization.md`
    /// § Implementation status (2d), obligation (iii)): reaching the
    /// `max().unwrap_or(0) + 1` fallback through a swallowed tiers-read error
    /// would mint at rank 1 — "subscribers get it free", the very arm the
    /// author declined. A failed read refuses the sell and mints nothing.
    #[test]
    fn sell_post_pay_per_view_refuses_on_a_failed_tiers_read() {
        let nest = MockNest::arc();
        nest.seed_tier("gold", 3);
        nest.inner.lock().unwrap().fail_kind =
            Some(("fauna.subscriptions.tiers.list".into(), "nest down".into()));
        let m = mgr(nest.clone());
        m.update_compose("body".into(), String::new(), None);
        m.update_compose_gate(None, "teaser".into());

        let err = block_on(m.prepare_sell_post(None, false, None))
            .expect_err("a failed rank read refuses");
        assert!(err.contains("rank"), "{err}");
        assert!(
            !nest
                .kinds()
                .contains(&"fauna.subscriptions.tiers.create".to_string()),
            "no tier is minted at a rank derived from a failed read"
        );
    }

    /// The asymmetry is deliberate: the subscribers-free arm's rank is the
    /// CONSTANT 1 — it derives nothing from the tiers read, so a failed read
    /// must not block it (fail-closed applies exactly to the arm whose answer
    /// the read determines).
    #[test]
    fn sell_post_subscribers_free_needs_no_tiers_read() {
        let nest = MockNest::arc();
        nest.inner.lock().unwrap().fail_kind =
            Some(("fauna.subscriptions.tiers.list".into(), "nest down".into()));
        let m = mgr(nest.clone());
        m.update_compose("body".into(), String::new(), None);
        m.update_compose_gate(None, "teaser".into());

        block_on(m.prepare_sell_post(None, true, None)).expect("rank 1 is constant");
        let created: fauna_protocol::subscriptions::TierCreateRequest =
            nest.req("fauna.subscriptions.tiers.create");
        assert_eq!(created.rank, 1);
    }

    /// A sold post is a gated post, so it needs the same public teaser — and the
    /// designated tier must NOT be minted when the composer is invalid, or a
    /// failed compose would litter the author's tier list with orphans.
    #[test]
    fn sell_post_refuses_a_blank_teaser_without_minting() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        m.update_compose("body".into(), String::new(), None);
        m.update_compose_gate(None, "   ".into());

        let err =
            block_on(m.prepare_sell_post(None, false, None)).expect_err("blank teaser refused");
        assert!(err.contains("preview"), "{err}");
        assert!(
            !nest
                .kinds()
                .contains(&"fauna.subscriptions.tiers.create".to_string()),
            "no tier is minted for a compose that never becomes a post"
        );
        assert_eq!(
            m.snapshot().compose.error.as_ref().map(|e| e.key.as_str()),
            Some("feed.compose_gate_preview_empty")
        );
    }

    // ── Buyer's price read (gap (2c), monetization.md § Per-post ─────────────
    // pay-to-unlock → the buyer's price read is post-addressed) ─────────────

    /// A sold post's teaser price resolves off `fauna.subscriptions.post_unlock.get`,
    /// keyed by the post's own `(author, post_id)` — the read a real sale's
    /// `unlocks_post` designation makes answerable.
    #[test]
    fn resolve_post_unlock_offer_fills_the_price_read_for_a_sold_post() {
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.gated_tier = Some("post-unlock-abc123".into());
        nest.push_page(vec![p], None);
        // The designated tier a real sale would have minted — the mock derives
        // the offer straight from this, the same (author, post_id) lookup the
        // real nest's handler does.
        nest.inner
            .lock()
            .unwrap()
            .tiers
            .push(fauna_protocol::subscriptions::TierItem {
                name: "post-unlock-abc123".into(),
                rank: 5,
                description: None,
                price_hint: Some("$3".into()),
                payment_url: None,
                auto_approve: false,
                created_at: fauna_core::data::Timestamp(0),
                unlocks_post: Some("aa".into()),
                asking_price: None,
                hidden: false,
                extra: Default::default(),
            });

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.snapshot().posts[0].unlock_offer, None);
        block_on(m.resolve_post_unlock_offer("aa".into()));
        let offer = m.snapshot().posts[0]
            .unlock_offer
            .clone()
            .expect("resolved");
        assert_eq!(offer.tier_name, "post-unlock-abc123");
        assert_eq!(offer.price_hint, Some("$3".into()));
        assert_eq!(offer.payment_url, None);
    }

    /// Build a sold post authored by THIS manager's own actor, plus the
    /// designated tier a real sale would have minted — the author's-own-post
    /// fixture the two invariants below share.
    fn own_sold_post(nest: &Arc<MockNest>) -> String {
        let me = hex::encode(ActorKeypair::from_secret(TEST_SECRET).actor_id().0);
        let mut p = post("aa", 1);
        p.author = me.clone();
        p.gated_tier = Some("post-unlock-abc123".into());
        nest.push_page(vec![p], None);
        nest.inner
            .lock()
            .unwrap()
            .tiers
            .push(fauna_protocol::subscriptions::TierItem {
                name: "post-unlock-abc123".into(),
                rank: 5,
                description: None,
                price_hint: Some("$3".into()),
                payment_url: None,
                auto_approve: false,
                created_at: fauna_core::data::Timestamp(0),
                unlocks_post: Some("aa".into()),
                asking_price: None,
                hidden: false,
                extra: Default::default(),
            });
        me
    }

    /// The price read is scoped to a **prospective buyer's** client
    /// (`monetization.md` § Per-post pay-to-unlock → *the buyer's price read is
    /// post-addressed*). An author is never a prospective buyer of their own
    /// post: their access to it is custody, never a purchase
    /// ([`FeedManager::unlock_gated_post`]'s `is_author` branch).
    ///
    /// This is not cosmetic. Resolving the offer paints a `gated-post-buy-button`
    /// on the author's own card, and clicking it subscribes the author to their
    /// own unlock tier — a roster the model has no place for, which then makes
    /// the real buyer's `key_blob.get` answer `not_subscribed`. Measured
    /// 2026-08-25 on macOS: the e2e teaser-buy completion leg had the SELLER in
    /// the roster and the buyer entitled to nothing.
    #[test]
    fn resolve_post_unlock_offer_ignores_the_authors_own_post() {
        let nest = MockNest::arc();
        own_sold_post(&nest);

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_post_unlock_offer("aa".into()));
        assert_eq!(
            m.snapshot().posts[0].unlock_offer,
            None,
            "the author must not be offered a purchase of their own sold post"
        );
    }

    /// The refusal is enforced at the mutation too, not only at the render
    /// trigger above: an offer already resolved (a stale snapshot from before an
    /// actor switch, which is exactly how the defect was reached on apple) must
    /// still not be buyable by the post's own author.
    #[test]
    fn buy_unlock_offer_refuses_the_authors_own_post() {
        let nest = MockNest::arc();
        own_sold_post(&nest);

        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        // Plant the offer directly — the render trigger now refuses to, so this
        // stands in for the stale-snapshot path.
        {
            let mut s = m.state.write().unwrap();
            if let Some(p) = rendered_posts_mut(&mut s).find(|p| p.post_id == "aa") {
                p.unlock_offer = Some(UnlockOfferView {
                    tier_name: "post-unlock-abc123".into(),
                    price_hint: Some("$3".into()),
                    payment_url: None,
                });
            }
        }
        assert!(
            block_on(m.buy_unlock_offer("aa".into())).is_none(),
            "the author must not be able to buy their own sold post"
        );
    }

    /// The read triggers only for a `post-unlock-*` tier — an ordinary gated
    /// post (gated to a ordinary author-managed tier) never fires it, since no
    /// generic tier read applies here and there is nothing to buy.
    #[test]
    fn resolve_post_unlock_offer_is_a_noop_for_an_ordinary_gated_post() {
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.gated_tier = Some("gold".into());
        nest.push_page(vec![p], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        let before = nest.kinds().len();
        block_on(m.resolve_post_unlock_offer("aa".into()));
        assert_eq!(
            nest.kinds().len(),
            before,
            "no post_unlock.get issued for a non-sold gated post"
        );
        assert_eq!(m.snapshot().posts[0].unlock_offer, None);
    }

    /// A refusal, a foreign/undesignated id, or any other
    /// empty reply all fold to `None` — the teaser then shows no price, and
    /// claim-code redemption stays the fallback purchase path
    /// (additive-everywhere).
    #[test]
    fn resolve_post_unlock_offer_stays_none_when_the_nest_answers_no_offer() {
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.gated_tier = Some("post-unlock-abc123".into());
        nest.push_page(vec![p], None);
        // No tier seeded: the mock's lookup answers `None`, the same as an
        // undesignated/foreign id or a refusal would.
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_post_unlock_offer("aa".into()));
        assert_eq!(m.snapshot().posts[0].unlock_offer, None);
    }

    /// The happy path: the post's totals and its attribution window land on
    /// `PostSummary.tips`, which is what `post-tip-total` / `post-tip-count` /
    /// `post-tip-item` render.
    #[cfg(feature = "payments")]
    #[test]
    fn resolve_post_tips_folds_the_totals_and_the_attribution_window() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        nest.set_tips_reply(fauna_protocol::tips::TipsListReply {
            total_msats: 21_000,
            tip_count: 3,
            tips: vec![
                fauna_protocol::tips::TipItem {
                    sender: Some(fauna_core::identity::ActorId([7u8; 32])),
                    sender_ref: None,
                    amount_msats: Some(21_000),
                    mechanism: "nostr_zap".into(),
                    received_at: 1_700_000_000,
                    extra: Default::default(),
                },
                // An outside tipper: no local actor, but a mechanism-native id
                // and no parseable amount — it still counts and still displays.
                fauna_protocol::tips::TipItem {
                    sender: None,
                    sender_ref: Some("npub1example".into()),
                    amount_msats: None,
                    mechanism: "nostr_zap".into(),
                    received_at: 1_699_999_000,
                    extra: Default::default(),
                },
            ],
            has_more: true,
            extra: Default::default(),
        });
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert_eq!(m.snapshot().posts[0].tips, None, "not resolved yet");
        block_on(m.resolve_post_tips("aa".into()));

        let tips = m.snapshot().posts[0].tips.clone().expect("resolved");
        assert_eq!(tips.total_msats, 21_000);
        assert_eq!(
            tips.tip_count, 3,
            "the UNBOUNDED count, not the window's len"
        );
        assert!(
            tips.has_more,
            "the nest said so — never inferred from len()"
        );
        assert_eq!(tips.senders.len(), 2);
        assert_eq!(tips.senders[0].sender.as_deref(), Some(&*"07".repeat(32)));
        assert_eq!(tips.senders[0].amount_msats, Some(21_000));
        assert_eq!(tips.senders[1].sender, None);
        assert_eq!(tips.senders[1].sender_ref.as_deref(), Some("npub1example"));
        assert_eq!(
            tips.senders[1].amount_msats, None,
            "a missing amount stays missing — never coerced to 0"
        );
    }

    /// `tip_count` deliberately exceeds the number of summable tips when a
    /// receipt carried no parseable invoice, so a post can honestly be
    /// "3 tips" with **no** amount at all. That is exactly why `post-tip-count`
    /// and `post-tip-total` are separate elements: the count renders, the total
    /// does not (`monetization.md` § Tips).
    #[cfg(feature = "payments")]
    #[test]
    fn a_tipped_post_with_no_parseable_amount_keeps_its_count_and_zero_total() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        nest.set_tips_reply(fauna_protocol::tips::TipsListReply {
            total_msats: 0,
            tip_count: 3,
            tips: vec![],
            has_more: false,
            extra: Default::default(),
        });
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_post_tips("aa".into()));

        let tips = m.snapshot().posts[0].tips.clone().expect("resolved");
        assert_eq!(tips.tip_count, 3, "post-tip-count renders");
        assert_eq!(tips.total_msats, 0, "post-tip-total does NOT render");
    }

    /// **The property the whole `Some`-on-every-outcome design exists for.**
    /// Nothing in the feed-index projection says whether a post has tips, so
    /// the caller's pump re-fires this resolve for any post still at `None` —
    /// and the resolve itself notifies. A `None`-on-empty resolver would
    /// therefore issue one `fauna.tips.list` per rendered post per notify,
    /// forever. Writing the empty view is what makes the guard close.
    #[cfg(feature = "payments")]
    #[test]
    fn an_untipped_post_resolves_once_and_never_asks_again() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        // No reply armed: the default zero/zero, which is what an untipped post
        // and a nest with no tip mechanism compiled in both genuinely answer.
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));

        block_on(m.resolve_post_tips("aa".into()));
        let after_first = nest
            .kinds()
            .iter()
            .filter(|k| *k == "fauna.tips.list")
            .count();
        assert_eq!(after_first, 1);
        assert_eq!(
            m.snapshot().posts[0].tips,
            Some(crate::snapshot::TipView::default()),
            "the empty answer is a VALUE — an untipped post is resolved, not unresolved"
        );

        // The pump fires again on the next notify, exactly as it would in an app.
        block_on(m.resolve_post_tips("aa".into()));
        block_on(m.resolve_post_tips("aa".into()));
        assert_eq!(
            nest.kinds()
                .iter()
                .filter(|k| *k == "fauna.tips.list")
                .count(),
            1,
            "re-firing the pump must not re-ask: that is the unbounded RPC loop"
        );
    }

    /// A refusal (`unknown_kind` included) and a transport error fold to the SAME
    /// empty surface as "no tips" — the ratified single degradation, so an app
    /// has one empty render rather than three states (`monetization.md` §
    /// Implementation status today, the Tips bullet). And it must still close
    /// the guard, or a nest that can't answer becomes the RPC loop above.
    #[cfg(feature = "payments")]
    #[test]
    fn a_refusal_or_a_transport_error_folds_to_the_same_empty_surface() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        nest.inner.lock().unwrap().fail_kind =
            Some(("fauna.tips.list".into(), "unknown_kind".into()));

        block_on(m.resolve_post_tips("aa".into()));
        assert_eq!(
            m.snapshot().posts[0].tips,
            Some(crate::snapshot::TipView::default()),
            "an unanswerable read renders as an empty tip surface, not as unresolved"
        );
        block_on(m.resolve_post_tips("aa".into()));
        assert_eq!(
            nest.kinds()
                .iter()
                .filter(|k| *k == "fauna.tips.list")
                .count(),
            1,
            "a failing nest must not be re-asked on every notify either"
        );
    }

    /// The tip surface belongs to the RENDERED set, so a deep-linked post — one
    /// the feed never loaded — resolves exactly like a list post.
    #[cfg(feature = "payments")]
    #[test]
    fn resolve_post_tips_is_a_noop_for_a_post_that_is_not_rendered() {
        let nest = MockNest::arc();
        nest.push_page(vec![post("aa", 1)], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_post_tips("ffff".into()));
        assert!(
            !nest.kinds().iter().any(|k| k == "fauna.tips.list"),
            "no read for a post this client isn't showing"
        );
    }

    /// Clicking `gated-post-buy-button` subscribes against the resolved
    /// offer's tier — the existing subscribe flow, no claim code needed. A
    /// client-minted unlock tier is never `auto_approve`, so it queues
    /// (pending the author's own §2 approve — the same path the buyer's
    /// claim-code redemption already proves end to end in `test_sell_post.py`).
    /// A successful buy also clears `unlock_offer` off the post's rendered
    /// state — the only observable signal a caller has that the (fire-and-
    /// forget, from the UI's view) click actually completed, rather than
    /// still being in flight.
    #[test]
    fn buy_unlock_offer_subscribes_against_the_resolved_tier() {
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.gated_tier = Some("post-unlock-abc123".into());
        nest.push_page(vec![p], None);
        nest.inner
            .lock()
            .unwrap()
            .tiers
            .push(fauna_protocol::subscriptions::TierItem {
                name: "post-unlock-abc123".into(),
                rank: 5,
                description: None,
                price_hint: Some("$3".into()),
                payment_url: None,
                auto_approve: false,
                created_at: fauna_core::data::Timestamp(0),
                unlocks_post: Some("aa".into()),
                asking_price: None,
                hidden: false,
                extra: Default::default(),
            });
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_post_unlock_offer("aa".into()));
        let queued = block_on(m.buy_unlock_offer("aa".into()))
            .expect("offer resolved")
            .expect("subscribed");
        assert!(queued, "a client-minted unlock tier is never auto_approve");
        assert!(
            nest.kinds()
                .contains(&"fauna.subscriptions.subscribe".to_string())
        );
        assert_eq!(
            m.snapshot().posts[0].unlock_offer,
            None,
            "a successful buy should clear unlock_offer, so the buy affordance \
             disappears and a caller has a real signal the click settled"
        );
    }

    /// A bought post's offer stays cleared when the render trigger fires again.
    ///
    /// `buy_unlock_offer` clears `unlock_offer` and calls `notify()`, and every
    /// app's render trigger re-runs `resolve_post_unlock_offer` on exactly that
    /// notification — with the blind `unlock_offer.is_none()` gate now true. The
    /// nest read is *post*-addressed and carries no caller axis by design (it
    /// answers "iff a tier genuinely sells this post" —
    /// `behavior/monetization.md` § Per-post pay-to-unlock → *The buyer's price
    /// read is post-addressed*, and § *No rotation* is why it must keep
    /// answering for an un-entitled buyer), so it re-offers, the teaser
    /// reappears, and the buy's only completion signal is destroyed.
    ///
    /// The guard therefore belongs *here*, client-side and actor-scoped —
    /// `FeedManager` is built per actor (`new(nest, actor_secret)`), the same
    /// reason the sibling `is_local_actor` refusal lives in shared Rust rather
    /// than in seven apps (`monetization.md:204`).
    #[test]
    fn a_bought_posts_offer_is_not_re_offered_by_a_second_resolve() {
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.gated_tier = Some("post-unlock-abc123".into());
        nest.push_page(vec![p], None);
        nest.inner
            .lock()
            .unwrap()
            .tiers
            .push(fauna_protocol::subscriptions::TierItem {
                name: "post-unlock-abc123".into(),
                rank: 5,
                description: None,
                price_hint: Some("$3".into()),
                payment_url: None,
                auto_approve: false,
                created_at: fauna_core::data::Timestamp(0),
                unlocks_post: Some("aa".into()),
                asking_price: None,
                hidden: false,
                extra: Default::default(),
            });
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        block_on(m.resolve_post_unlock_offer("aa".into()));
        block_on(m.buy_unlock_offer("aa".into()))
            .expect("offer resolved")
            .expect("subscribed");
        assert_eq!(m.snapshot().posts[0].unlock_offer, None);

        // The notification the buy itself emitted drives every app's trigger
        // straight back into this call.
        block_on(m.resolve_post_unlock_offer("aa".into()));
        assert_eq!(
            m.snapshot().posts[0].unlock_offer,
            None,
            "a second resolve after a successful buy must not re-offer the \
             post — the teaser would reappear and the buy's completion signal \
             (a cleared unlock_offer) would never durably hold"
        );
    }

    /// The button isn't reachable before the offer resolves — `buy_unlock_offer`
    /// on an unresolved post answers `None`, never a panicking unwrap on
    /// missing state.
    #[test]
    fn buy_unlock_offer_is_none_before_the_offer_resolves() {
        let nest = MockNest::arc();
        let mut p = post("aa", 1);
        p.gated_tier = Some("post-unlock-abc123".into());
        nest.push_page(vec![p], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(None));
        assert!(block_on(m.buy_unlock_offer("aa".into())).is_none());
    }

    /// The composer's three answers are mutually exclusive **by construction**
    /// — the invariant `FeedComposeState::sell` exists to make structural.
    /// Asserted in both directions, because a one-way clear would still leave
    /// "gated to a tier AND selling" reachable by the other ordering, and
    /// `prepare_sell_post` never reads `gate_tier` so that state has no
    /// defined meaning.
    #[test]
    fn the_gate_select_answers_are_mutually_exclusive() {
        let m = mgr(MockNest::arc());

        // tier → sell clears the tier.
        m.update_compose_gate(Some(TIER.into()), "teaser".into());
        m.update_compose_sell(Some(SellComposeState::default()), "teaser".into());
        let snap = m.snapshot();
        assert_eq!(snap.compose.gate_tier, None, "sell cleared the gate tier");
        assert!(snap.compose.sell.is_some());

        // sell → tier clears the sell.
        m.update_compose_gate(Some(TIER.into()), "teaser".into());
        let snap = m.snapshot();
        assert_eq!(snap.compose.gate_tier.as_deref(), Some(TIER));
        assert!(
            snap.compose.sell.is_none(),
            "the gate tier cleared the sell"
        );

        // sell → Public clears the sell too: "Public" is an answer to the same
        // select, not a no-op.
        m.update_compose_sell(Some(SellComposeState::default()), "teaser".into());
        m.update_compose_gate(None, "teaser".into());
        let snap = m.snapshot();
        assert_eq!(snap.compose.gate_tier, None);
        assert!(snap.compose.sell.is_none(), "Public cleared the sell");
    }

    /// The rank knob defaults ON (user-ratified 2026-07-29): an existing paying
    /// subscriber is not charged twice for a post their subscription would
    /// reasonably cover, so pay-per-view is the deliberate opt-in.
    #[test]
    fn entering_sell_mode_defaults_subscribers_to_free() {
        assert!(SellComposeState::default().subscribers_get_it_free);
        assert_eq!(SellComposeState::default().price, "");
    }

    #[test]
    fn gated_compose_requires_a_preview() {
        let nest = MockNest::arc();
        nest.seed_tier(TIER, 2);
        let m = mgr(nest);
        m.update_compose("body".into(), String::new(), None);
        m.update_compose_gate(Some(TIER.into()), "   ".into());
        let err = block_on(m.prepare_gated_blob()).expect_err("blank teaser refused");
        assert!(err.contains("preview"), "{err}");
        let snap = m.snapshot();
        assert_eq!(
            snap.compose.error.as_ref().map(|e| e.key.as_str()),
            Some("feed.compose_gate_preview_empty")
        );
    }

    #[test]
    fn gated_compose_requires_custody() {
        let nest = MockNest::arc();
        nest.seed_tier(TIER, 2);
        // Custody blob exists but holds NO key for this tier.
        nest.seed_muted_keywords(&[]);
        let m = mgr(nest);
        block_on(m.refresh_own_tiers());
        m.update_compose("body".into(), String::new(), None);
        m.update_compose_gate(Some(TIER.into()), "teaser".into());
        let err = block_on(m.prepare_gated_blob()).expect_err("no period key refused");
        assert!(err.contains("no period key"), "{err}");
        let snap = m.snapshot();
        assert_eq!(
            snap.compose.error.as_ref().map(|e| e.key.as_str()),
            Some("feed.compose_gate_no_key")
        );
    }

    #[test]
    fn unlock_gated_post_author_custody_path() {
        // The author built + sealed the post; their custody period key unseals
        // it with no KeyBlob fetch.
        let kp = ActorKeypair::from_secret(TEST_SECRET);
        let build = fauna_client_core::post::build_gated_post(
            &kp,
            "public teaser",
            "the-full-premium-body",
            TIER,
            2,
            [0x11u8; 32],
            &PERIOD_KEY,
        )
        .unwrap();

        let nest = MockNest::arc();
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.inner.lock().unwrap().posts_get_body = build.post_bytes.clone();
        nest.push_page(vec![gated_item("aa", TIER, "public teaser")], None);
        let m = mgr(nest);
        block_on(m.reload());

        let hash = block_on(m.gated_blob_hash("aa".into())).expect("gated resolve");
        assert_eq!(hash, hex::encode(build.encrypted_ref));

        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("author custody unlock");
        let snap = m.snapshot();
        let post = &snap.posts[0];
        assert!(post.gated_unlocked);
        assert_eq!(post.body, "the-full-premium-body");
    }

    /// A room-post key seam that knows exactly one (room, seal) and the base
    /// it opens under — the conversations plane's answer, reduced to a table.
    struct OneRoomKey {
        room: [u8; 32],
        seal: fauna_core::room_post::RoomPostSeal,
        base: [u8; 32],
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::room_post::RoomPostKeys for OneRoomKey {
        async fn room_post_seal_key(
            &self,
            room: [u8; 32],
        ) -> Result<
            (
                fauna_core::room_post::RoomPostSeal,
                zeroize::Zeroizing<[u8; 32]>,
            ),
            String,
        > {
            if room == self.room {
                Ok((self.seal, zeroize::Zeroizing::new(self.base)))
            } else {
                Err("not keyed into that room".into())
            }
        }

        async fn room_post_base_key(
            &self,
            room: [u8; 32],
            seal: fauna_core::room_post::RoomPostSeal,
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
            if room == self.room && seal == self.seal {
                Ok(zeroize::Zeroizing::new(self.base))
            } else {
                Err("not keyed into that room's key".into())
            }
        }

        async fn room_post_rooms(&self) -> Vec<fauna_core::room_post::RoomPostRoom> {
            vec![fauna_core::room_post::RoomPostRoom {
                room: self.room,
                label: "the room".into(),
            }]
        }
    }

    /// The same seam, for a room whose canonical plane lives on ANOTHER nest —
    /// what `ConversationsSession` answers off the channel's recorded
    /// `ChannelHome` once a member joined a foreign-homed room. The key answers
    /// are unchanged: a foreign home changes where the *derived views* are
    /// read, never who holds the key.
    struct HomedRoomKey {
        keys: OneRoomKey,
        home: String,
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::room_post::RoomPostKeys for HomedRoomKey {
        async fn room_post_seal_key(
            &self,
            room: [u8; 32],
        ) -> Result<
            (
                fauna_core::room_post::RoomPostSeal,
                zeroize::Zeroizing<[u8; 32]>,
            ),
            String,
        > {
            self.keys.room_post_seal_key(room).await
        }

        async fn room_post_base_key(
            &self,
            room: [u8; 32],
            seal: fauna_core::room_post::RoomPostSeal,
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
            self.keys.room_post_base_key(room, seal).await
        }

        async fn room_post_rooms(&self) -> Vec<fauna_core::room_post::RoomPostRoom> {
            self.keys.room_post_rooms().await
        }

        async fn room_home_nest_url(&self, room: [u8; 32]) -> Option<String> {
            (room == self.keys.room).then(|| self.home.clone())
        }
    }

    /// A room post by SOMEONE ELSE, loaded into a reader's feed — so the only
    /// route to its body is the room's key, never the author's custody.
    fn loaded_room_post(
        seal: fauna_core::room_post::RoomPostSeal,
        base: [u8; 32],
    ) -> (
        FeedManager<Arc<MockNest>>,
        fauna_client_core::post::GatedPostBuild,
    ) {
        let author = ActorKeypair::from_secret([0x5Au8; 32]);
        let build = fauna_client_core::post::build_room_post_at(
            &author,
            "a post for the room",
            fauna_core::data::PostBody::Text {
                content: "only the room reads this".into(),
                facets: vec![],
            },
            [0xC7u8; 32],
            seal,
            &base,
            [0x0Du8; 32],
            &fauna_client_core::post::PostAuthoring::now(),
        )
        .unwrap();
        let nest = MockNest::arc();
        nest.inner.lock().unwrap().posts_get_body = build.post_bytes.clone();
        nest.push_page(
            vec![gated_item(
                "aa",
                fauna_core::subscription::ROOM_POST_TIER,
                "a post for the room",
            )],
            None,
        );
        let m = mgr(nest);
        block_on(m.reload());
        block_on(m.gated_blob_hash("aa".into())).expect("gated resolve");
        (m, build)
    }

    #[test]
    fn a_community_room_post_opens_under_the_members_generation() {
        use fauna_core::room_post::RoomPostSeal;
        let seal = RoomPostSeal::Community {
            generation: [0x9Au8; 32],
        };
        let base = [0x33u8; 32];
        let (m, build) = loaded_room_post(seal, base);
        m.set_room_post_keys(Arc::new(OneRoomKey {
            room: [0xC7u8; 32],
            seal,
            base,
        }));
        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("a member of the room opens it");
        let snap = m.snapshot();
        assert!(snap.posts[0].gated_unlocked);
        assert_eq!(snap.posts[0].body, "only the room reads this");
    }

    #[test]
    fn a_room_posts_served_verdicts_merge_into_the_cards_own_labels() {
        // Ruling 7's client half: the verdicts the room's nest serves a member
        // at unlock reach the card's `labels` — what `content-label-badge`
        // paints — by the same superset rule a room message's do, and survive
        // a reload, which always arrives with the envelope's labels alone.
        use fauna_core::content_category::ContentLabelEntry;
        use fauna_core::room_post::RoomPostSeal;
        let seal = RoomPostSeal::Community {
            generation: [0x9Au8; 32],
        };
        let base = [0x33u8; 32];
        let (m, build) = loaded_room_post(seal, base);
        m.set_room_post_keys(Arc::new(OneRoomKey {
            room: [0xC7u8; 32],
            seal,
            base,
        }));
        let served = ContentLabelEntry {
            category: "spam".into(),
            confidence_per_mille: 900,
        };
        m.nest.inner.lock().unwrap().room_post_labels_reply =
            Some(fauna_protocol::posts::PostRoomLabelsReply {
                posts: vec![fauna_protocol::posts::PostRoomLabelsEntry {
                    post_id: "aa".into(),
                    labels: vec![served.clone()],
                    ..Default::default()
                }],
                extra: Default::default(),
            });
        assert!(
            m.snapshot().posts[0].labels.is_empty(),
            "the envelope read carries no verdict"
        );

        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("a member of the room opens it");
        let snap = m.snapshot();
        assert!(snap.posts[0].gated_unlocked);
        assert_eq!(snap.posts[0].labels, vec![served.clone()]);

        // A reload re-delivers the sealed item, label-less; the re-fold that
        // restores the body restores the verdicts with it.
        m.nest.push_page(
            vec![gated_item(
                "aa",
                fauna_core::subscription::ROOM_POST_TIER,
                "a post for the room",
            )],
            None,
        );
        block_on(m.reload());
        let snap = m.snapshot();
        assert!(snap.posts[0].gated_unlocked, "the unlock survives a reload");
        assert_eq!(snap.posts[0].labels, vec![served]);
    }

    #[test]
    fn a_foreign_homed_rooms_verdict_read_goes_to_the_rooms_home() {
        // The kind pick, which lives in the manager and nowhere per-app
        // (`ui/feed.md` § Encryption at rest → *Built* detail (v)): a room
        // whose plane is homed on another nest rides the distinct relay kind,
        // because only that home ran the reception pass that derived the
        // verdicts — this reader's own nest indexes no post of it and would
        // answer an empty reply the card cannot tell from "nobody labelled
        // it". The request names the room, which is what the home's
        // foreign-member gate is checked against.
        use fauna_core::content_category::ContentLabelEntry;
        use fauna_core::room_post::RoomPostSeal;
        let seal = RoomPostSeal::Community {
            generation: [0x9Au8; 32],
        };
        let base = [0x33u8; 32];
        let room = [0xC7u8; 32];
        let (m, build) = loaded_room_post(seal, base);
        m.set_room_post_keys(Arc::new(HomedRoomKey {
            keys: OneRoomKey { room, seal, base },
            home: "https://home.example".into(),
        }));
        let served = ContentLabelEntry {
            category: "spam".into(),
            confidence_per_mille: 900,
        };
        m.nest.inner.lock().unwrap().room_post_labels_reply =
            Some(fauna_protocol::posts::PostRoomLabelsReply {
                posts: vec![fauna_protocol::posts::PostRoomLabelsEntry {
                    post_id: "aa".into(),
                    labels: vec![served.clone()],
                    ..Default::default()
                }],
                extra: Default::default(),
            });

        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("a member of the room opens it");
        assert_eq!(m.snapshot().posts[0].labels, vec![served]);

        let calls = m.nest.inner.lock().unwrap().calls.clone();
        let (_, payload) = calls
            .iter()
            .find(|(kind, _)| kind == "fauna.posts.room_labels_remote")
            .expect("the relayed kind, never the same-nest one");
        assert!(
            !calls
                .iter()
                .any(|(kind, _)| kind == "fauna.posts.room_labels"),
            "and not both: an old own-nest answering the plain read would \
             answer empty, indistinguishably from 'nobody labelled it'"
        );
        let req: fauna_protocol::posts::PostRoomLabelsRemoteRequest =
            fauna_protocol::decode_strict(payload).expect("decodes");
        assert_eq!(req.nest_url, "https://home.example");
        assert_eq!(req.room_id, hex::encode(room), "gated on the room's id");
        assert_eq!(req.post_ids, vec!["aa".to_string()]);
    }

    #[test]
    fn a_same_nest_rooms_verdict_read_stays_on_the_plain_kind() {
        // The other half of the pick: a seam that names no foreign home — a
        // same-nest room, or a seam with no channel routing at all — keeps
        // every reader on the read it made before the relay existed.
        use fauna_core::room_post::RoomPostSeal;
        let seal = RoomPostSeal::Community {
            generation: [0x9Au8; 32],
        };
        let base = [0x33u8; 32];
        let (m, build) = loaded_room_post(seal, base);
        m.set_room_post_keys(Arc::new(OneRoomKey {
            room: [0xC7u8; 32],
            seal,
            base,
        }));
        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("a member of the room opens it");
        let calls = m.nest.inner.lock().unwrap().calls.clone();
        assert!(
            calls
                .iter()
                .any(|(kind, _)| kind == "fauna.posts.room_labels"),
            "the same-nest door"
        );
        assert!(
            !calls
                .iter()
                .any(|(kind, _)| kind == "fauna.posts.room_labels_remote"),
            "no relay leg for a room this nest homes"
        );
    }

    #[test]
    fn a_room_post_the_nest_serves_no_verdicts_for_keeps_the_cards_own_labels() {
        use fauna_core::room_post::RoomPostSeal;
        let seal = RoomPostSeal::Community {
            generation: [0x9Au8; 32],
        };
        let base = [0x33u8; 32];
        let (m, build) = loaded_room_post(seal, base);
        m.set_room_post_keys(Arc::new(OneRoomKey {
            room: [0xC7u8; 32],
            seal,
            base,
        }));
        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("a member of the room opens it");
        assert!(m.snapshot().posts[0].labels.is_empty());
    }

    #[test]
    fn an_end_to_end_room_post_opens_under_the_epoch_secret() {
        use fauna_core::room_post::RoomPostSeal;
        let seal = RoomPostSeal::EndToEnd { epoch: 4 };
        let base = [0x44u8; 32];
        let (m, build) = loaded_room_post(seal, base);
        m.set_room_post_keys(Arc::new(OneRoomKey {
            room: [0xC7u8; 32],
            seal,
            base,
        }));
        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("a member at that epoch opens it");
        assert_eq!(m.snapshot().posts[0].body, "only the room reads this");
    }

    #[test]
    fn a_room_post_this_device_holds_no_key_for_stays_locked() {
        // Not a member — the seam answers for a different room. The post must
        // stay the preview, and nothing else may be tried in the key's place:
        // a room post's readers are the floor, never a subscription, so the
        // tier's KeyBlob is the wrong question to ask.
        use fauna_core::room_post::RoomPostSeal;
        let seal = RoomPostSeal::Community {
            generation: [0x9Au8; 32],
        };
        let (m, build) = loaded_room_post(seal, [0x33u8; 32]);
        m.set_room_post_keys(Arc::new(OneRoomKey {
            room: [0x01u8; 32],
            seal,
            base: [0x33u8; 32],
        }));
        let err = block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect_err("a non-member stays locked");
        assert!(err.contains("not keyed"), "{err}");
        assert!(!m.snapshot().posts[0].gated_unlocked);
    }

    #[test]
    fn a_room_post_stays_locked_on_a_device_with_no_room_keys() {
        use fauna_core::room_post::RoomPostSeal;
        let (m, build) = loaded_room_post(RoomPostSeal::EndToEnd { epoch: 1 }, [0x44u8; 32]);
        let err = block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect_err("no seam, no key");
        assert!(err.contains("room"), "{err}");
        assert!(!m.snapshot().posts[0].gated_unlocked);
    }

    /// A seam that holds no room at all — the honest device with a
    /// conversations plane but no floor seat anywhere.
    struct NoRoomKeys;

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::room_post::RoomPostKeys for NoRoomKeys {
        async fn room_post_seal_key(
            &self,
            _room: [u8; 32],
        ) -> Result<
            (
                fauna_core::room_post::RoomPostSeal,
                zeroize::Zeroizing<[u8; 32]>,
            ),
            String,
        > {
            Err("no room".into())
        }

        async fn room_post_base_key(
            &self,
            _room: [u8; 32],
            _seal: fauna_core::room_post::RoomPostSeal,
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
            Err("no room".into())
        }
    }

    /// The card's room reading (`ui/feed.md` § Encryption at rest →
    /// *Room-restricted — the app half*, the card bullet): a room post whose
    /// projected `gated_room` names a room this reader sits on the floor of
    /// carries that room's label — the reader's own, from the seam — on every
    /// snapshot read, the deep-link slot included; a room post of a room the
    /// reader is not in carries none, so its card shows the reserved tier;
    /// and the label FOLLOWS the seam rather than being stored — once the
    /// room is no longer offered (the device lost its seat), the same post
    /// drops back to the reserved tier on the next read.
    #[test]
    fn a_room_posts_card_names_the_room_for_a_member_only_and_follows_the_seam() {
        use crate::test_support::{TestPostSpec, feed_snapshot_with_posts};
        let room = [0xC7u8; 32];
        let room_post = |id: &str, room_hex: String| TestPostSpec {
            post_id: id.repeat(32),
            body: "a teaser".into(),
            gated_tier: Some(fauna_core::subscription::ROOM_POST_TIER.into()),
            gated_room: Some(room_hex),
            ..Default::default()
        };
        let mut snapshot = feed_snapshot_with_posts(vec![
            room_post("aa", hex::encode(room)),
            room_post("bb", "d9".repeat(32)),
            TestPostSpec {
                post_id: "cc".repeat(32),
                body: "just a post".into(),
                ..Default::default()
            },
        ]);
        snapshot.deep_linked_post = Some(room_post("dd", hex::encode(room)).into_summary());
        let nest = MockNest::arc();
        let m = mgr(nest);
        m.set_feed_snapshot_for_test(snapshot);

        // No seam installed: no room is the reader's, so no card names one.
        assert!(
            m.snapshot()
                .rendered_posts()
                .all(|p| p.room_label.is_none()),
            "without a conversations plane every room post shows the reserved tier"
        );

        m.set_room_post_keys(Arc::new(OneRoomKey {
            room,
            seal: fauna_core::room_post::RoomPostSeal::EndToEnd { epoch: 1 },
            base: [0u8; 32],
        }));
        block_on(m.refresh_own_rooms());
        let snap = m.snapshot();
        assert_eq!(
            snap.posts[0].room_label.as_deref(),
            Some("the room"),
            "a member's card names the room by the member's own label"
        );
        assert_eq!(
            snap.posts[1].room_label, None,
            "a room the reader is not in stays the reserved tier"
        );
        assert_eq!(
            snap.posts[2].room_label, None,
            "a public post names no room"
        );
        assert_eq!(
            snap.deep_linked_post
                .as_ref()
                .and_then(|p| p.room_label.as_deref()),
            Some("the room"),
            "the deep-link slot is read on the same terms as a list post"
        );

        // The seam stops offering the room: the label goes with it.
        m.set_room_post_keys(Arc::new(NoRoomKeys));
        block_on(m.refresh_own_rooms());
        assert!(
            m.snapshot()
                .rendered_posts()
                .all(|p| p.room_label.is_none()),
            "a lost seat drops the card back to the reserved tier"
        );
    }

    /// The composer's room arm end to end in shared Rust: a room offered from
    /// the seam, a photo sealed first, the body built over the SAME seal the
    /// photo's base was resolved under, uploaded under the room arm's sidecar
    /// class, created by the unchanged `submit_gated_post` — and one per-post
    /// key, from the room's base, opening body and photo alike (`ui/feed.md`
    /// § Encryption at rest → *Room-restricted — the ruling*, rulings 3 and 4).
    #[test]
    fn a_room_compose_seals_body_and_photo_under_the_rooms_one_key() {
        use fauna_core::room_post::{RoomPostSeal, room_post_of};
        use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};
        use fauna_media::audience::AudienceClass;
        use fauna_media::sidecar::UploadSidecar;

        let room = [0xC7u8; 32];
        let seal = RoomPostSeal::EndToEnd { epoch: 7 };
        let base = [0x44u8; 32];
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        m.set_room_post_keys(Arc::new(OneRoomKey { room, seal, base }));
        block_on(m.refresh_own_rooms());
        assert_eq!(
            m.snapshot().own_rooms,
            vec![crate::compose::GateRoomOption {
                room: hex::encode(room),
                label: "the room".into(),
            }]
        );

        m.update_compose("only the room reads this".into(), String::new(), None);
        m.update_compose_room(Some(hex::encode(room)), "a post for the room".into());

        let raw = png_signature_fixture();
        let sealed = block_on(m.seal_compose_attachment(raw.clone())).expect("room seal succeeds");
        assert!(sealed.sealed, "a room compose AEAD-seals its attachment");
        assert_eq!(
            UploadSidecar::from_dag_cbor(&sealed.primary.sidecar_cbor)
                .expect("sidecar decodes")
                .class,
            AudienceClass::GroupRestrictedPost,
            "ruling 4: a room post's photo declares the Group class"
        );
        let blob_hash = hex::encode(blake3::hash(&sealed.primary.bytes).as_bytes());
        m.update_compose(
            "only the room reads this".into(),
            String::new(),
            Some(AttachedFile {
                name: "photo.png".into(),
                size: raw.len() as u64,
                blob_hash: Some(blob_hash.clone()),
                media_type: Some(sealed.media_type.clone()),
            }),
        );

        let body_blob = block_on(m.prepare_gated_blob())
            .expect("room build succeeds")
            .expect("a room compose produces a sealed blob");
        assert_eq!(
            m.gated_upload_sidecar().class,
            AudienceClass::GroupRestrictedPost,
            "the staged post names its own upload class — no app chooses it"
        );
        let hash = hex::encode(blake3::hash(&body_blob).as_bytes());
        block_on(m.submit_gated_post(hash)).expect("room create lands");

        let created: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        let (post, _) =
            fauna_client_core::post::decode_post(created.body.as_ref()).expect("decode post");
        let gated = post.gated.expect("the post is gated");
        assert_eq!(gated.tier, fauna_core::subscription::ROOM_POST_TIER);
        assert_eq!(room_post_of(&gated.key_access), Some((room, seal)));
        let key = derive_post_key(&base, &gated.seal_id);
        let plain = decrypt_content(&key, &body_blob).expect("the body opens under the room's key");
        let full: fauna_core::data::PostBody =
            fauna_core::encoding::canonical_decode(&plain).expect("decode full body");
        match &full {
            fauna_core::data::PostBody::TextWithMedia { content, items, .. } => {
                assert_eq!(content, "only the room reads this");
                assert_eq!(hex::encode(items[0].blob_hash.digest()), blob_hash);
            }
            other => panic!("a room compose with a photo seals TextWithMedia: {other:?}"),
        }
        let opened =
            decrypt_content(&key, &sealed.primary.bytes).expect("the photo opens under it too");
        assert_eq!(opened, raw);
    }

    /// A seam whose room advances an epoch on every "what seals a new post
    /// right now" — a membership change landing between the photo's seal and
    /// the body's, which is the ordinary case in a live room.
    struct AdvancingRoomKey {
        room: [u8; 32],
        asked: std::sync::atomic::AtomicU64,
    }

    fn base_for(epoch: u64) -> [u8; 32] {
        let mut base = [0u8; 32];
        base[0] = epoch as u8;
        base
    }

    #[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
    #[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
    impl fauna_core::room_post::RoomPostKeys for AdvancingRoomKey {
        async fn room_post_seal_key(
            &self,
            room: [u8; 32],
        ) -> Result<
            (
                fauna_core::room_post::RoomPostSeal,
                zeroize::Zeroizing<[u8; 32]>,
            ),
            String,
        > {
            assert_eq!(room, self.room);
            let epoch = self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            Ok((
                fauna_core::room_post::RoomPostSeal::EndToEnd { epoch },
                zeroize::Zeroizing::new(base_for(epoch)),
            ))
        }

        async fn room_post_base_key(
            &self,
            room: [u8; 32],
            seal: fauna_core::room_post::RoomPostSeal,
        ) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
            assert_eq!(room, self.room);
            match seal {
                fauna_core::room_post::RoomPostSeal::EndToEnd { epoch } => {
                    Ok(zeroize::Zeroizing::new(base_for(epoch)))
                }
                other => Err(format!("no key for {other:?}")),
            }
        }
    }

    /// The body seals under the seal the PHOTO's base was resolved under, not
    /// under whatever the room would hand out at submit time. Asking the room
    /// for a key twice would put the body at the later epoch and leave a photo
    /// nobody — the author included — could open beside it.
    #[test]
    fn a_photo_and_its_body_share_one_key_even_as_the_room_moves_under_them() {
        use fauna_core::room_post::{RoomPostSeal, room_post_of};
        use fauna_core::subscription::crypto::{decrypt_content, derive_post_key};

        let room = [0xC7u8; 32];
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        m.set_room_post_keys(Arc::new(AdvancingRoomKey {
            room,
            asked: std::sync::atomic::AtomicU64::new(0),
        }));
        m.update_compose("only the room reads this".into(), String::new(), None);
        m.update_compose_room(Some(hex::encode(room)), "a post for the room".into());

        let raw = png_signature_fixture();
        let sealed = block_on(m.seal_compose_attachment(raw.clone())).expect("room seal succeeds");
        let blob_hash = hex::encode(blake3::hash(&sealed.primary.bytes).as_bytes());
        m.update_compose(
            "only the room reads this".into(),
            String::new(),
            Some(AttachedFile {
                name: "photo.png".into(),
                size: raw.len() as u64,
                blob_hash: Some(blob_hash),
                media_type: Some(sealed.media_type.clone()),
            }),
        );
        let body_blob = block_on(m.prepare_gated_blob())
            .expect("room build succeeds")
            .expect("a room compose produces a sealed blob");
        let hash = hex::encode(blake3::hash(&body_blob).as_bytes());
        block_on(m.submit_gated_post(hash)).expect("room create lands");

        let created: fauna_protocol::posts::PostCreateRequest = nest.req("fauna.posts.create");
        let (post, _) =
            fauna_client_core::post::decode_post(created.body.as_ref()).expect("decode post");
        let gated = post.gated.expect("the post is gated");
        assert_eq!(
            room_post_of(&gated.key_access),
            Some((room, RoomPostSeal::EndToEnd { epoch: 1 })),
            "the post names the epoch its photo was sealed under, not a later one"
        );
        let key = derive_post_key(&base_for(1), &gated.seal_id);
        assert!(
            decrypt_content(&key, &body_blob).is_ok(),
            "the body opens under it"
        );
        assert_eq!(
            decrypt_content(&key, &sealed.primary.bytes).expect("the photo opens under it too"),
            raw
        );
    }

    /// The four answers to `compose-gate-tier-select` are mutually exclusive
    /// by construction: each setter clears the others, so "gated to a tier
    /// AND to a room" can never be staged.
    #[test]
    fn a_room_answer_and_the_other_audience_answers_clear_each_other() {
        let m = mgr(MockNest::arc());
        let room = hex::encode([0xC7u8; 32]);
        m.update_compose_gate(Some(TIER.into()), "teaser".into());
        m.update_compose_room(Some(room.clone()), "teaser".into());
        let c = m.snapshot().compose;
        assert_eq!(
            (c.gate_room.as_deref(), c.gate_tier, c.sell),
            (Some(room.as_str()), None, None)
        );

        m.update_compose_sell(Some(SellComposeState::default()), "teaser".into());
        assert_eq!(
            m.snapshot().compose.gate_room,
            None,
            "selling clears the room"
        );

        m.update_compose_room(Some(room.clone()), "teaser".into());
        assert_eq!(m.snapshot().compose.sell, None, "a room clears the sale");
        m.update_compose_gate(Some(TIER.into()), "teaser".into());
        assert_eq!(
            m.snapshot().compose.gate_room,
            None,
            "a tier clears the room"
        );
        m.update_compose_room(Some(room), "teaser".into());
        m.update_compose_gate(None, "teaser".into());
        assert_eq!(
            m.snapshot().compose.gate_room,
            None,
            "Public clears the room"
        );
    }

    /// A device with no conversations plane offers no room, and a room compose
    /// that reaches submit anyway is refused on `compose-error` — never sealed
    /// under anything else, and never published public.
    #[test]
    fn a_room_compose_on_a_device_with_no_room_keys_is_refused() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        block_on(m.refresh_own_rooms());
        assert!(m.snapshot().own_rooms.is_empty());

        m.update_compose("only the room reads this".into(), String::new(), None);
        m.update_compose_room(Some(hex::encode([0xC7u8; 32])), "teaser".into());
        let err = block_on(m.prepare_gated_blob()).expect_err("no seam, no seal");
        assert!(err.contains("room"), "{err}");
        assert_eq!(
            m.snapshot().compose.error,
            Some(LocalizedText::key("feed.compose_room_no_key"))
        );
        assert!(m.pending_gated.read().unwrap().is_none(), "nothing staged");
    }

    /// A photo sealed for one room must not ride a body addressed to another:
    /// the audience moved between the two calls, and a body sealed under the
    /// second room's key would name a photo no member of it can open.
    #[test]
    fn a_room_edit_after_the_photo_seal_refuses_rather_than_stranding_it() {
        use fauna_core::room_post::RoomPostSeal;
        let room = [0xC7u8; 32];
        let m = mgr(MockNest::arc());
        m.set_room_post_keys(Arc::new(OneRoomKey {
            room,
            seal: RoomPostSeal::EndToEnd { epoch: 1 },
            base: [0x44u8; 32],
        }));
        m.update_compose("body".into(), String::new(), None);
        m.update_compose_room(Some(hex::encode(room)), "teaser".into());
        let raw = png_signature_fixture();
        let sealed = block_on(m.seal_compose_attachment(raw.clone())).expect("room seal succeeds");
        let blob_hash = hex::encode(blake3::hash(&sealed.primary.bytes).as_bytes());
        m.update_compose(
            "body".into(),
            String::new(),
            Some(AttachedFile {
                name: "photo.png".into(),
                size: raw.len() as u64,
                blob_hash: Some(blob_hash),
                media_type: Some(sealed.media_type),
            }),
        );
        // Re-selecting the audience drops the stash, exactly as a gate edit does.
        m.update_compose_room(Some(hex::encode(room)), "teaser".into());
        let err = block_on(m.prepare_gated_blob()).expect_err("the photo is stale");
        assert!(err.contains("re-attach"), "{err}");
        assert_eq!(
            m.snapshot().compose.error,
            Some(LocalizedText::key("feed.compose_attachment_stale"))
        );
    }

    #[test]
    fn unlock_gated_post_survives_reload() {
        // A reader who unsealed a gated post keeps the full body across a feed
        // reload: the freshly-fetched list always arrives SEALED, and
        // `reapply_unlocked` re-folds the decrypted body so a refresh never
        // reverts the reader to the teaser. Without the re-fold the second
        // reload's `s.posts = fresh` would snap the post back to the teaser —
        // the same-manager face of the iOS re-login manager churn that stranded
        // the `test_gated_post_compose` subscriber leg on the teaser.
        let kp = ActorKeypair::from_secret(TEST_SECRET);
        let build = fauna_client_core::post::build_gated_post(
            &kp,
            "public teaser",
            "the-full-premium-body",
            TIER,
            2,
            [0x11u8; 32],
            &PERIOD_KEY,
        )
        .unwrap();

        let nest = MockNest::arc();
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.inner.lock().unwrap().posts_get_body = build.post_bytes.clone();
        nest.push_page(vec![gated_item("aa", TIER, "public teaser")], None);
        let m = mgr(nest.clone());
        block_on(m.reload());
        block_on(m.gated_blob_hash("aa".into())).expect("gated resolve");
        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("author custody unlock");
        assert!(m.snapshot().posts[0].gated_unlocked, "unlock applied");

        // A second reload re-fetches the same post, sealed.
        nest.push_page(vec![gated_item("aa", TIER, "public teaser")], None);
        block_on(m.reload());
        let snap = m.snapshot();
        let post = &snap.posts[0];
        assert!(post.gated_unlocked, "unlock must survive a feed reload");
        assert_eq!(post.body, "the-full-premium-body");
    }

    /// Build a gated post carrying one sealed attachment, and return
    /// `(build, sealed_bytes, plaintext, blob_hash_hex)`.
    ///
    /// The seal here is the writer's own expression —
    /// `encrypt_content(derive_post_key(period_key, seal_id), plaintext)`, the
    /// arm `fauna_media::seal::seal_for_audience` evaluates for
    /// `RestrictedPostAudience::Period` — rather than a call into `fauna-media`,
    /// which is not (and should not become) a dependency of this crate for one
    /// AEAD open. If that arm ever seals differently, this fixture is the second
    /// place to change; `libs/fauna-media/src/seal.rs`'s Period arm is the first.
    ///
    /// The blob is content-addressed on the CIPHERTEXT — the nest hashes what it
    /// stores — which is what the item carries and what an app fetches by.
    fn gated_post_with_sealed_photo(
        caption: &str,
        plaintext: &[u8],
    ) -> (
        fauna_client_core::post::GatedPostBuild,
        Vec<u8>,
        Vec<u8>,
        String,
    ) {
        use fauna_core::data::{ContentHash, MediaItem, PostBody};
        use fauna_core::subscription::crypto::{derive_post_key, encrypt_content};

        let kp = ActorKeypair::from_secret(TEST_SECRET);
        let seal_id = [0x5au8; 32];
        let per_post_key = derive_post_key(&PERIOD_KEY, &ContentHash::from_digest_raw(seal_id));
        let sealed = encrypt_content(&per_post_key, plaintext);
        let blob_hash = ContentHash::from_digest_raw(*blake3::hash(&sealed).as_bytes());
        let blob_hex = hex::encode(blob_hash.digest());

        let build = fauna_client_core::post::build_gated_post_at(
            &kp,
            "public teaser",
            PostBody::TextWithMedia {
                content: caption.to_string(),
                facets: vec![],
                items: vec![MediaItem {
                    blob_hash,
                    media_type: "image/png".into(),
                    size_bytes: sealed.len() as u64,
                    dimensions: None,
                    thumbnail: None,
                    ..Default::default()
                }],
            },
            TIER,
            2,
            [0x11u8; 32],
            &PERIOD_KEY,
            seal_id,
            &fauna_client_core::post::PostAuthoring::now(),
        )
        .unwrap();

        (build, sealed, plaintext.to_vec(), blob_hex)
    }

    /// A gated post's PHOTOS, not only its caption — the reader-side half of
    /// `media.md` § Encryption at rest ("recipients who can decrypt the body can
    /// decrypt the attachments by construction").
    ///
    /// The writer has sealed a gated post's media items since the archive import
    /// shipped (`fauna_client::media_upload::upload_period_sealed_media`), and no
    /// reader opened them: `unlock_gated_post` reduced the opened body to its
    /// text on arrival, so the items were decoded and dropped in one expression
    /// and a gated photo post rendered its caption over a blank card on all 7 apps.
    #[test]
    fn unlock_gated_post_opens_its_media_items() {
        use fauna_core::render::RenderBlock;

        let (build, sealed, plaintext, blob_hex) = gated_post_with_sealed_photo(
            "the-full-premium-body",
            b"\x89PNG\r\n\x1a\n-the-subscriber-only-photo",
        );

        let nest = MockNest::arc();
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.inner.lock().unwrap().posts_get_body = build.post_bytes.clone();
        nest.push_page(vec![gated_item("aa", TIER, "public teaser")], None);
        let m = mgr(nest);
        block_on(m.reload());
        block_on(m.gated_blob_hash("aa".into())).expect("gated resolve");

        // Before the unlock the card is the teaser and flags no media: the index
        // projection reads the PREVIEW body, which carries no items.
        assert!(
            !m.snapshot().posts[0].has_media,
            "the teaser flags no media"
        );

        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("author custody unlock");

        let snap = m.snapshot();
        let post = &snap.posts[0];
        assert!(post.gated_unlocked);
        assert_eq!(post.body, "the-full-premium-body");
        // The three the fold owes: the flag, the fire-once resolve guard, and
        // the typed block every app paints from.
        assert!(post.has_media, "the opened body carries an item");
        assert_eq!(post.media_hash.as_deref(), Some(blob_hex.as_str()));
        assert!(
            post.document
                .blocks
                .iter()
                .any(|b| matches!(b, RenderBlock::Image { hash, .. } if hash == &blob_hex)),
            "the unlocked body's item folds into the document: {:?}",
            post.document.blocks
        );

        // And the bytes an app fetches by that hash open, byte-for-byte.
        assert_eq!(
            m.open_media_bytes(&blob_hex, sealed),
            Some(plaintext),
            "the item opens under the same per-post key the body did"
        );
    }

    /// The pass-through half of the same call, which is what lets all 7 apps
    /// route every post image through one seam instead of branching on gating: a
    /// hash this reader holds no seal key for is public-post media, already
    /// plaintext on the wire, and comes back untouched.
    #[test]
    fn open_media_bytes_passes_public_post_bytes_through() {
        let m = mgr(MockNest::arc());
        let bytes = b"plaintext-public-post-image".to_vec();
        assert_eq!(
            m.open_media_bytes(&hex::encode([0x77u8; 32]), bytes.clone()),
            Some(bytes)
        );
    }

    /// A sealed item that does not open is `None`, never the ciphertext: an app
    /// degrades to its existing placeholder rather than decoding AEAD bytes as an
    /// image. (Reached when the fetched blob is truncated, or is not the blob the
    /// item named.)
    #[test]
    fn open_media_bytes_refuses_a_sealed_item_it_cannot_open() {
        let (build, sealed, _plaintext, blob_hex) =
            gated_post_with_sealed_photo("body", b"the-photo");

        let nest = MockNest::arc();
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.inner.lock().unwrap().posts_get_body = build.post_bytes.clone();
        nest.push_page(vec![gated_item("aa", TIER, "public teaser")], None);
        let m = mgr(nest);
        block_on(m.reload());
        block_on(m.gated_blob_hash("aa".into())).expect("gated resolve");
        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("author custody unlock");

        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 0xff;
        assert_eq!(m.open_media_bytes(&blob_hex, tampered), None);
    }

    /// The predicate web and apple need, because their image path is a URL the
    /// browser/SwiftUI fetches natively — no app code ever holds the bytes, so
    /// those two must know BEFORE rendering whether this hash can be served as a
    /// plain URL or has to be fetched and opened. The four apps that hold the bytes
    /// never ask; they route everything through `open_media_bytes`.
    ///
    /// Both arms in one test, because the two answers are one decision: registered
    /// (the unlocked post's own item) → must open; anything else — public media, an
    /// avatar, a link preview — → the URL is the image.
    #[test]
    fn is_sealed_media_marks_only_an_unlocked_posts_own_items() {
        let (build, _sealed, _plaintext, blob_hex) =
            gated_post_with_sealed_photo("body", b"the-photo");

        let nest = MockNest::arc();
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.inner.lock().unwrap().posts_get_body = build.post_bytes.clone();
        nest.push_page(vec![gated_item("aa", TIER, "public teaser")], None);
        let m = mgr(nest);
        block_on(m.reload());

        // Before the unlock the reader holds no key for it, so the card takes the
        // ordinary URL path and paints its placeholder — asking earlier must not
        // claim otherwise.
        assert!(
            !m.is_sealed_media(&blob_hex),
            "a still-sealed post's item is not openable yet"
        );

        block_on(m.gated_blob_hash("aa".into())).expect("gated resolve");
        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("author custody unlock");

        assert!(
            m.is_sealed_media(&blob_hex),
            "the unlock registers the item, so its bytes must be opened, not linked"
        );
        assert!(
            !m.is_sealed_media(&hex::encode([0x77u8; 32])),
            "public-post media, avatars and link previews stay on the URL path"
        );
    }

    /// Inline playback's shared decision (render-model.md § D6c → *Inline playback*):
    /// a public `Video` plays straight off the unauthenticated blob route,
    /// nest-relative so each app prefixes its own nest origin.
    #[test]
    fn playback_source_plays_a_public_video_from_the_blob_route() {
        let m = mgr(MockNest::arc());
        let hash = hex::encode([0x42u8; 32]);
        let block = RenderBlock::Video {
            hash: hash.clone(),
            alt: String::new(),
        };
        assert_eq!(
            block_on(m.playback_source(&block)),
            PlaybackSource::Url {
                url: format!("/api/v1/blob/{hash}")
            }
        );
    }

    /// A `Video` item of a post this reader unlocked is ciphertext on the blob
    /// route — no URL can play it, so the app is told to open it client-side. The
    /// SAME hash asked before the unlock is the URL arm (the reader holds no key;
    /// the player's own error is the honest surface, as the image card's
    /// placeholder is).
    #[test]
    fn playback_source_hands_an_unlocked_posts_video_to_the_sealed_path() {
        let (build, _sealed, _plaintext, blob_hex) =
            gated_post_with_sealed_photo("body", b"the-video-bytes");

        let nest = MockNest::arc();
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.inner.lock().unwrap().posts_get_body = build.post_bytes.clone();
        nest.push_page(vec![gated_item("aa", TIER, "public teaser")], None);
        let m = mgr(nest);
        block_on(m.reload());
        let block = RenderBlock::Video {
            hash: blob_hex.clone(),
            alt: String::new(),
        };

        assert_eq!(
            block_on(m.playback_source(&block)),
            PlaybackSource::Url {
                url: format!("/api/v1/blob/{blob_hex}")
            },
            "before the unlock nothing is registered — the URL arm"
        );

        block_on(m.gated_blob_hash("aa".into())).expect("gated resolve");
        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("author custody unlock");

        assert_eq!(
            block_on(m.playback_source(&block)),
            PlaybackSource::Sealed { hash: blob_hex },
            "an unlocked post's item must be opened, never linked"
        );
    }

    /// Everything that is not a video plays nothing — an image, and a text block.
    #[test]
    fn playback_source_refuses_a_non_video_block() {
        let m = mgr(MockNest::arc());
        for block in [
            RenderBlock::Image {
                hash: hex::encode([0x42u8; 32]),
                alt: String::new(),
            },
            RenderBlock::Paragraph { inlines: vec![] },
        ] {
            assert!(
                matches!(
                    block_on(m.playback_source(&block)),
                    PlaybackSource::Unplayable { .. }
                ),
                "{block:?} must be Unplayable"
            );
        }
    }

    /// The re-fold owes the photos too. A `reload` replaces the list with
    /// freshly-fetched SEALED posts; `reapply_unlocked` restores the reader's
    /// body from `unlocked_bodies`, and while that cache held a bare `String` it
    /// could not have restored the items whatever it did — there was no room for
    /// them.
    #[test]
    fn unlocked_media_survives_a_reload() {
        use fauna_core::render::RenderBlock;

        let (build, _sealed, _plaintext, blob_hex) =
            gated_post_with_sealed_photo("the-full-premium-body", b"the-photo");

        let nest = MockNest::arc();
        nest.seed_custody(TIER, PERIOD_KEY);
        nest.inner.lock().unwrap().posts_get_body = build.post_bytes.clone();
        nest.push_page(vec![gated_item("aa", TIER, "public teaser")], None);
        let m = mgr(nest.clone());
        block_on(m.reload());
        block_on(m.gated_blob_hash("aa".into())).expect("gated resolve");
        block_on(m.unlock_gated_post("aa".into(), build.encrypted_blob.clone()))
            .expect("author custody unlock");

        // A second reload re-fetches the same post, sealed.
        nest.push_page(vec![gated_item("aa", TIER, "public teaser")], None);
        block_on(m.reload());

        let snap = m.snapshot();
        let post = &snap.posts[0];
        assert!(post.gated_unlocked, "unlock survives the reload");
        assert!(post.has_media, "and so does the media flag");
        assert_eq!(post.media_hash.as_deref(), Some(blob_hex.as_str()));
        assert!(
            post.document
                .blocks
                .iter()
                .any(|b| matches!(b, RenderBlock::Image { hash, .. } if hash == &blob_hex)),
            "the re-fold restores the photo, not only the caption: {:?}",
            post.document.blocks
        );
    }

    #[test]
    fn unlock_gated_post_subscriber_keyblob_path() {
        use fauna_core::encoding::{EmbedAsBytes, sign_envelope};
        use fauna_core::subscription::crypto::create_key_blob_entry;
        use fauna_core::subscription::types::KeyBlob;

        // A DIFFERENT author sealed the post; the local actor (TEST_SECRET)
        // reads it through their wrap entry in the tier's KeyBlob.
        let author_kp = ActorKeypair::from_secret([9u8; 32]);
        let reader_kp = ActorKeypair::from_secret(TEST_SECRET);
        let build = fauna_client_core::post::build_gated_post(
            &author_kp,
            "public teaser",
            "subscriber-visible-full-body",
            TIER,
            2,
            [0x11u8; 32],
            &PERIOD_KEY,
        )
        .unwrap();

        // Mint the reader's wrap entry + a signed KeyBlob around it, encoded
        // exactly as the nest stores it (EmbedAsBytes over canonical bytes).
        let entry = create_key_blob_entry(&reader_kp.actor_id(), &PERIOD_KEY);
        let blob = KeyBlob {
            author: author_kp.actor_id(),
            tier: TIER.into(),
            rotated_at: fauna_core::data::Timestamp(2),
            entries: vec![entry],
            signer: author_kp.actor_id().0,
            key_commitment: [0x6b; 32],
        };
        let (bytes, envelope) = sign_envelope(&author_kp, &blob).unwrap();
        let wire = EmbedAsBytes::from_signed(bytes, envelope);
        let blob_data = fauna_protocol::encode_canonical(&wire).unwrap().to_vec();

        let nest = MockNest::arc();
        nest.seed_key_blob(1, vec![0x11u8; 32], blob_data);
        nest.inner.lock().unwrap().posts_get_body = build.post_bytes.clone();
        nest.push_page(vec![gated_item("bb", TIER, "public teaser")], None);
        let m = mgr(nest);
        block_on(m.reload());

        block_on(m.gated_blob_hash("bb".into())).expect("gated resolve");
        block_on(m.unlock_gated_post("bb".into(), build.encrypted_blob.clone()))
            .expect("subscriber KeyBlob unlock");
        let snap = m.snapshot();
        assert!(snap.posts[0].gated_unlocked);
        assert_eq!(snap.posts[0].body, "subscriber-visible-full-body");
    }

    // ── Engagement-cue capture (engagement-cues.md §§ Cue vocabulary / At rest) ──

    /// A non-media observation held past `CUE_DWELL_LONG_MS` visible — the
    /// derivation's `WatchComplete` gate (no clock is read; `at_ms` is the
    /// shell-supplied observation time driving the debounce).
    fn watch_complete_obs(content_id: &str, at_ms: u64) -> CueObservation {
        CueObservation {
            content_id: content_id.into(),
            is_media: false,
            media_played_pm: None,
            dwell_ms_at_skip_visibility: 0,
            dwell_ms_at_long_visibility: 10_000,
            observed_at_ms: at_ms,
        }
    }

    /// Fetch-on-session-start loads the nest's sealed rollup as the live engine —
    /// proven by a later put carrying BOTH the fetched item and a freshly
    /// observed one, which is impossible if hydration had started empty.
    #[test]
    fn hydrate_cues_installs_the_fetched_rollup() {
        let nest = MockNest::arc();
        let mut seeded = CueRollup::new();
        seeded.record("aa", CueVerdict::WatchComplete, 9_000, 1_000);
        nest.seed_cue_rollup(&seeded);

        let m = mgr(nest.clone());
        block_on(m.hydrate_cues()).expect("hydrate");
        block_on(m.record_observation(watch_complete_obs("bb", 5_000))).expect("record");
        block_on(m.flush_cues()).expect("flush");

        let stored = nest.stored_cue_rollup().expect("a rollup was put");
        assert_eq!(
            stored.verdict("aa"),
            Some(CueVerdict::WatchComplete),
            "fetched item survived"
        );
        assert_eq!(
            stored.verdict("bb"),
            Some(CueVerdict::WatchComplete),
            "new item captured"
        );
    }

    /// An unopenable rollup (corruption / a newer layout) is a **surfaced error**,
    /// never a silent fresh rollup — starting fresh would erase every cue the
    /// user's other devices recorded on the next put.
    #[test]
    fn hydrate_cues_surfaces_an_unopenable_rollup() {
        let nest = MockNest::arc();
        nest.seed_unopenable_cue_rollup();
        let m = mgr(nest.clone());
        block_on(m.hydrate_cues()).expect_err("an unopenable rollup must surface, not start fresh");
    }

    /// A recorded verdict does not put immediately (it accumulates), and a flush
    /// seals the current rollup to the nest under `cues:v1`.
    #[test]
    fn record_then_flush_puts_the_sealed_rollup() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        block_on(m.hydrate_cues()).expect("hydrate"); // absent ⇒ fresh engine

        let verdict =
            block_on(m.record_observation(watch_complete_obs("aa", 5_000))).expect("record");
        assert_eq!(verdict, Some(CueVerdict::WatchComplete));
        assert_eq!(
            put_count(&nest),
            0,
            "a single observation does not put — it accumulates"
        );

        block_on(m.flush_cues()).expect("flush");
        assert_eq!(put_count(&nest), 1, "flush seals the accumulated rollup");
        let stored = nest.stored_cue_rollup().expect("a rollup was put");
        assert_eq!(stored.verdict("aa"), Some(CueVerdict::WatchComplete));
    }

    /// Observations inside the debounce window accumulate without putting; the
    /// batch is sealed on the first observation past `CUE_PUT_DEBOUNCE_S`.
    #[test]
    fn debounce_batches_puts_within_the_window() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        block_on(m.hydrate_cues()).expect("hydrate");

        let window_ms = CUE_PUT_DEBOUNCE_S * 1_000;
        block_on(m.record_observation(watch_complete_obs("aa", 1_000))).expect("record aa"); // anchors
        block_on(m.record_observation(watch_complete_obs("bb", 2_000))).expect("record bb"); // in window
        assert_eq!(
            put_count(&nest),
            0,
            "two observations inside the window issue no put"
        );

        block_on(m.record_observation(watch_complete_obs("cc", 1_000 + window_ms)))
            .expect("record cc");
        assert_eq!(
            put_count(&nest),
            1,
            "the first observation past the window flushes the batch"
        );
        let stored = nest.stored_cue_rollup().expect("a rollup was put");
        assert_eq!(
            stored.len(),
            3,
            "the batched put carries every accumulated item"
        );
    }

    /// The user's delete affordance drops the nest row and resets the live engine,
    /// so a later capture starts fresh rather than resurrecting deleted cues.
    #[test]
    fn delete_cue_rollup_drops_the_row_and_resets_the_engine() {
        let nest = MockNest::arc();
        let mut seeded = CueRollup::new();
        seeded.record("aa", CueVerdict::WatchComplete, 9_000, 1_000);
        nest.seed_cue_rollup(&seeded);

        let m = mgr(nest.clone());
        block_on(m.hydrate_cues()).expect("hydrate");
        block_on(m.delete_cue_rollup()).expect("delete");

        assert!(
            nest.kinds()
                .iter()
                .any(|k| k == "fauna.personalization.model.delete"),
            "a delete was issued",
        );
        assert!(nest.stored_cue_rollup().is_none(), "the nest row is gone");

        block_on(m.record_observation(watch_complete_obs("bb", 5_000))).expect("record");
        block_on(m.flush_cues()).expect("flush");
        let stored = nest.stored_cue_rollup().expect("a fresh rollup was put");
        assert_eq!(
            stored.verdict("aa"),
            None,
            "the deleted item did not resurrect"
        );
        assert_eq!(stored.verdict("bb"), Some(CueVerdict::WatchComplete));
    }

    // ── Layer-B producer: opt-in k-anon contribution (engagement-cues.md § Layer B) ──

    const PUB_A: &str = "1a"; // two public posts + one gated, in the loaded window
    const PUB_B: &str = "1b";
    const GATED: &str = "2c";

    /// A manager whose loaded window holds two PUBLIC posts and one GATED post, so
    /// the producer's public-only gate has both cases to discriminate.
    fn mgr_with_mixed_window(nest: Arc<MockNest>) -> FeedManager<Arc<MockNest>> {
        use crate::test_support::{TestPostSpec, feed_snapshot_with_posts};
        let m = mgr(nest);
        let snap = feed_snapshot_with_posts(vec![
            TestPostSpec {
                post_id: PUB_A.into(),
                ..Default::default()
            },
            TestPostSpec {
                post_id: PUB_B.into(),
                ..Default::default()
            },
            TestPostSpec {
                post_id: GATED.into(),
                gated_tier: Some("gold".into()),
                ..Default::default()
            },
        ]);
        m.set_feed_snapshot_for_test(snap);
        m
    }

    fn contribute_count(nest: &Arc<MockNest>) -> usize {
        nest.kinds()
            .iter()
            .filter(|k| *k == "fauna.moderation.signal_contribute")
            .count()
    }

    /// An opted-in user's shareable verdict on a PUBLIC post fires the producer:
    /// `signal_contribute` reaches the nest carrying the post id + the verdict wire
    /// string.
    #[test]
    fn producer_contributes_a_public_watch_complete_when_opted_in() {
        let nest = MockNest::arc();
        let m = mgr_with_mixed_window(nest.clone());
        block_on(m.hydrate_cues()).expect("hydrate");
        // Opt in — nest-confirmed, so the producer's cache flips on.
        assert!(
            block_on(m.set_signal_sharing(true)).expect("opt in").share,
            "the nest-confirmed opt-in is on"
        );

        block_on(m.record_observation(watch_complete_obs(PUB_A, 5_000))).expect("record");

        let req: fauna_protocol::moderation::ModerationSignalContributeRequest =
            nest.req("fauna.moderation.signal_contribute");
        assert_eq!(
            req.content_id, PUB_A,
            "the contribution names the watched post"
        );
        assert_eq!(
            req.signal, "watch-complete",
            "the wire string is the derived verdict"
        );
    }

    /// The default-off producer never contributes: a shareable verdict with no
    /// opt-in submits nothing (the nest never learns the user even viewed the post).
    #[test]
    fn producer_is_silent_when_not_opted_in() {
        let nest = MockNest::arc();
        let m = mgr_with_mixed_window(nest.clone());
        block_on(m.hydrate_cues()).expect("hydrate");
        assert!(
            !block_on(m.hydrate_signal_optin()).expect("hydrate opt-in"),
            "opt-in defaults off"
        );

        block_on(m.record_observation(watch_complete_obs(PUB_A, 5_000))).expect("record");

        assert_eq!(contribute_count(&nest), 0, "no contribution without opt-in");
    }

    /// Public-posts-only: an opted-in verdict on a GATED post is not contributed —
    /// a `signal:*` aggregate on restricted content would leak readership.
    #[test]
    fn producer_excludes_gated_posts_even_when_opted_in() {
        let nest = MockNest::arc();
        let m = mgr_with_mixed_window(nest.clone());
        block_on(m.hydrate_cues()).expect("hydrate");
        block_on(m.set_signal_sharing(true)).expect("opt in");

        block_on(m.record_observation(watch_complete_obs(GATED, 5_000))).expect("record");

        assert_eq!(
            contribute_count(&nest),
            0,
            "a verdict about a gated post is never contributed",
        );
    }

    /// Opting back out re-silences the producer immediately (the cache follows the
    /// nest-confirmed set): a fresh verdict on a second public post after opt-out
    /// contributes nothing.
    #[test]
    fn producer_falls_silent_after_opt_out() {
        let nest = MockNest::arc();
        let m = mgr_with_mixed_window(nest.clone());
        block_on(m.hydrate_cues()).expect("hydrate");
        block_on(m.set_signal_sharing(true)).expect("opt in");
        block_on(m.record_observation(watch_complete_obs(PUB_A, 5_000))).expect("record A");
        assert_eq!(
            contribute_count(&nest),
            1,
            "the opted-in verdict contributed"
        );

        block_on(m.set_signal_sharing(false)).expect("opt out");
        block_on(m.record_observation(watch_complete_obs(PUB_B, 6_000))).expect("record B");
        assert_eq!(
            contribute_count(&nest),
            1,
            "opting out suppresses the next verdict's contribution",
        );
    }

    /// Puts are suppressed before hydration: capturing against an un-fetched
    /// engine would overwrite another device's rollup with this device's partial
    /// state on the first put.
    #[test]
    fn record_before_hydration_issues_no_put() {
        let nest = MockNest::arc();
        let m = mgr(nest.clone());
        // No hydrate_cues.
        let window_ms = CUE_PUT_DEBOUNCE_S * 1_000;
        block_on(m.record_observation(watch_complete_obs("aa", 1_000))).expect("record");
        block_on(m.record_observation(watch_complete_obs("bb", 1_000 + window_ms)))
            .expect("record");
        assert_eq!(
            put_count(&nest),
            0,
            "no put before hydration, even past the window"
        );
    }

    // ── Layer A: engagement training (topic-factors.md § Training signals) ──────

    /// A brief-glance observation deriving `Skip` — skip-visible but held under
    /// `CUE_SKIP_MS`, and (with a ≥ `CUE_BURST_MIN_GAP_MS` gap) not a flick.
    fn skip_obs(content_id: &str, at_ms: u64) -> CueObservation {
        CueObservation {
            content_id: content_id.into(),
            is_media: false,
            media_played_pm: None,
            dwell_ms_at_skip_visibility: 800,
            dwell_ms_at_long_visibility: 0,
            observed_at_ms: at_ms,
        }
    }

    /// The publish sheet's corpus read: public posts, scored by the factor, best
    /// first. The cats model ranks the on-topic post over the finance one.
    #[test]
    fn score_corpus_for_factor_ranks_public_posts_best_first() {
        let nest = MockNest::arc();
        nest.seed_model(CATS, &cats_model());
        nest.push_page(
            vec![
                scored_post("fin", 2, 0, "quarterly budget spreadsheet rows"),
                scored_post("cat", 1, 0, "a fluffy cat purring"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        let got = block_on(m.score_corpus_for_factor(CATS, 10)).expect("scores the window");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].post_id, "cat", "the on-topic post leads");
        assert!(
            got[0].score > got[1].score,
            "cat {} must outscore finance {}",
            got[0].score,
            got[1].score
        );
        assert!(
            (0..=1000).contains(&got[0].score),
            "scores are per-mille, the ListEntry range"
        );
        assert_eq!(
            got[0].preview, "a fluffy cat purring",
            "the preview is what the card renders, so the user prunes against what they read"
        );
    }

    /// A **gated** post must never reach the review sheet: a published List is
    /// public (frame § Tier-3 artifact kinds), so endorsing restricted content
    /// into one would leak what only the curator could see.
    #[test]
    fn score_corpus_for_factor_excludes_gated_posts() {
        let nest = MockNest::arc();
        nest.seed_model(CATS, &cats_model());
        let mut gated = scored_post("cat-gated", 2, 0, "a fluffy cat purring");
        gated.gated_tier = Some("gold".into());
        nest.push_page(
            vec![
                gated,
                scored_post("cat-public", 1, 0, "a fluffy cat purring"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        let got = block_on(m.score_corpus_for_factor(CATS, 10)).expect("scores the window");
        assert_eq!(
            got.iter().map(|e| e.post_id.as_str()).collect::<Vec<_>>(),
            vec!["cat-public"],
            "the gated post is excluded even though it scores identically"
        );
    }

    /// `top_n` bounds the sheet, keeping the best.
    #[test]
    fn score_corpus_for_factor_truncates_to_top_n() {
        let nest = MockNest::arc();
        nest.seed_model(CATS, &cats_model());
        nest.push_page(
            vec![
                scored_post("fin", 3, 0, "quarterly budget spreadsheet rows"),
                scored_post("cat", 2, 0, "a fluffy cat purring"),
                scored_post("fin2", 1, 0, "invoice ledger reconciliation"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        let got = block_on(m.score_corpus_for_factor(CATS, 1)).expect("scores the window");
        assert_eq!(got.len(), 1);
        assert_eq!(
            got[0].post_id, "cat",
            "truncation keeps the best, not the first"
        );
    }

    /// Unlike `example_label_for`, this answers for a factor the current feed
    /// does **not** compose — the user publishes from the Personalization home,
    /// whose factor need not be the feed they happen to be looking at.
    #[test]
    fn score_corpus_for_factor_answers_for_a_factor_the_feed_does_not_compose() {
        let nest = MockNest::arc();
        nest.seed_model(CATS, &cats_model());
        // The feed composes nothing at all.
        nest.push_page(vec![scored_post("cat", 1, 0, "a fluffy cat purring")], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        assert!(
            m.example_label_for("cat", CATS).is_none(),
            "precondition: the factor is not composed, so the per-post face abstains"
        );
        let got = block_on(m.score_corpus_for_factor(CATS, 10)).expect("scores anyway");
        assert_eq!(got.len(), 1, "the corpus read is not composed-restricted");
        assert!(got[0].score > 500, "and it used the real trained model");
    }

    /// An empty window is an empty sheet, not an error — the sheet renders its
    /// `-exemplar-empty` arm.
    #[test]
    fn score_corpus_for_factor_on_an_empty_window_is_empty_not_an_error() {
        let nest = MockNest::arc();
        nest.seed_model(CATS, &cats_model());
        nest.push_page(vec![], None);
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));

        assert!(
            block_on(m.score_corpus_for_factor(CATS, 10))
                .expect("empty is not an error")
                .is_empty()
        );
    }

    /// The Layer-A payoff: a cue-verdict transition on an in-window post weakly
    /// trains each composed `learn_from_engagement` factor, re-seals it, and puts
    /// it — with no explicit gesture and no `posts.get`.
    #[test]
    fn record_observation_trains_a_learn_from_engagement_factor() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        nest.seed_trained_factor(CATS_ID, true); // learn_from_engagement ON
        // No seeded model ⇒ a fresh one loads; the watch-complete trains it up.
        nest.push_page(
            vec![scored_post("cat1", 1, 1_000_000, "a fluffy cat purring")],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));
        block_on(m.hydrate_cues()).expect("hydrate");

        // A watch-complete for the in-window post: a None→WatchComplete transition.
        block_on(m.record_observation(watch_complete_obs("cat1", 5_000))).expect("record");

        let stored = nest.stored_model(CATS).expect("the trained model was put");
        assert_eq!(stored.example_count(), 0, "no EXPLICIT examples were added");
        assert!(
            stored.raw_score(
                "a fluffy cat purring",
                fauna_core::scoring::cues::CUE_ENGAGEMENT_WEIGHT_PM
            ) > 500,
            "the watch-complete folded a weak positive into the sealed model",
        );
    }

    /// The whole Layer-A loop, through to the ORDER a shell renders: a single
    /// dwell-derived watch-complete on a mid-window post re-ranks the loaded
    /// window so that post leads. This is the display-side link the
    /// train-and-put assertions above don't cover (and the exact shape the
    /// linux tier_3 `test_engagement_cues.py` drives through the real UI).
    #[test]
    fn a_single_watch_complete_reranks_the_loaded_window() {
        const CAT_TEXT: &str = "tabby kitten purring whiskers softly by the warm window";
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 5000)]);
        nest.seed_trained_factor(CATS_ID, true); // learn_from_engagement ON
        // Flat nest keys (the all-sealed composition's zero-term seam): order
        // arrives as created_at DESC, the on-topic post mid-window.
        nest.push_page(
            vec![
                scored_post("off-new", 4, 0, "commuter rail timetable stop"),
                scored_post("off-mid", 3, 0, "garage door spring note"),
                scored_post("cat", 2, 0, CAT_TEXT),
                scored_post("off-old", 1, 0, "quarterly budget spreadsheet rows"),
            ],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));
        block_on(m.hydrate_cues()).expect("hydrate");
        assert_eq!(
            m.snapshot().posts[2].post_id,
            "cat",
            "pre-train the on-topic post must sit mid-window"
        );

        block_on(m.record_observation(watch_complete_obs("cat", 9_000))).expect("record");

        let order: Vec<String> = m
            .snapshot()
            .posts
            .iter()
            .map(|p| p.post_id.clone())
            .collect();
        assert_eq!(
            order[0], "cat",
            "one weak watch-complete must re-rank the loaded window: {order:?}"
        );
    }

    /// A factor whose registry meta has `learn_from_engagement = off` is never
    /// touched by a cue — the opt-in is the gate (default off).
    #[test]
    fn record_observation_leaves_an_engagement_off_factor_untouched() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        nest.seed_trained_factor(CATS_ID, false); // learn_from_engagement OFF
        nest.push_page(
            vec![scored_post("cat1", 1, 1_000_000, "a fluffy cat purring")],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));
        block_on(m.hydrate_cues()).expect("hydrate");

        block_on(m.record_observation(watch_complete_obs("cat1", 5_000))).expect("record");

        assert!(
            nest.stored_model(CATS).is_none(),
            "an engagement-off factor issues no model.put — it is never trained",
        );
    }

    /// An item no longer in the loaded window trains no model (its text is not
    /// cheaply fetchable — the "text still fetchable" clause), yet its verdict is
    /// still captured in the rollup.
    #[test]
    fn record_observation_skips_training_for_an_item_not_in_the_window() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        nest.seed_trained_factor(CATS_ID, true);
        nest.push_page(
            vec![scored_post("cat1", 1, 1_000_000, "a fluffy cat purring")],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));
        block_on(m.hydrate_cues()).expect("hydrate");

        block_on(m.record_observation(watch_complete_obs("ghost", 5_000))).expect("record");
        assert!(
            nest.stored_model(CATS).is_none(),
            "an out-of-window item trains no model",
        );
        block_on(m.flush_cues()).expect("flush");
        let rollup = nest
            .stored_cue_rollup()
            .expect("the rollup still captured the verdict");
        assert_eq!(rollup.verdict("ghost"), Some(CueVerdict::WatchComplete));
    }

    /// A cue verdict flip reverses the training end to end: a watch-complete lifts
    /// the sealed model above neutral, a later skip of the same item takes it
    /// below — the transition-driven inverse, driven from the rollup's before /
    /// after verdict.
    #[test]
    fn a_cue_flip_reverses_the_engagement_training() {
        let nest = MockNest::arc();
        nest.set_feed_composition(vec![entry(CATS, 1000)]);
        nest.seed_trained_factor(CATS_ID, true);
        nest.push_page(
            vec![scored_post("cat1", 1, 1_000_000, "a fluffy cat purring")],
            None,
        );
        let m = mgr(nest.clone());
        block_on(m.select_feed(Some("feed-1".into())));
        block_on(m.hydrate_cues()).expect("hydrate");
        let w = fauna_core::scoring::cues::CUE_ENGAGEMENT_WEIGHT_PM;

        block_on(m.record_observation(watch_complete_obs("cat1", 1_000))).expect("watch");
        let after_watch = nest
            .stored_model(CATS)
            .expect("put")
            .raw_score("a fluffy cat purring", w);
        assert!(after_watch > 500, "watch-complete lifts: {after_watch}");

        block_on(m.record_observation(skip_obs("cat1", 5_000))).expect("skip");
        let after_skip = nest
            .stored_model(CATS)
            .expect("put")
            .raw_score("a fluffy cat purring", w);
        assert!(after_skip < 500, "the flip to skip reverses: {after_skip}");
    }

    // ── Draft persistence (posts rail) ───────────────────────────
    //
    // The manager half of `reserved-folders.md` § Drafts Sync for `"posts"`.
    // Drives the REAL compose doors (`update_compose` / `update_compose_gate` /
    // `update_compose_sell` — the ones the UI calls), then replays the relaunch
    // journey against a FRESH manager, which is the only order that proves a
    // draft actually crossed the at-rest boundary rather than surviving in
    // memory.

    /// The cross-device journey in miniature: what device A typed is what
    /// device B's composer shows. A *value* assertion, not a non-empty one —
    /// a stale draft, an empty composer and a placeholder all satisfy
    /// "non-empty", and this queue has been bitten by that class repeatedly.
    #[test]
    fn a_draft_typed_on_one_device_restores_onto_another() {
        let a = mgr(Arc::new(MockNest::default()));
        a.update_compose(
            "half a thought".into(),
            "walrus, quarterly".into(),
            Some(AttachedFile {
                name: "a.png".into(),
                size: 42,
                blob_hash: Some("abc123".into()),
                media_type: Some("image/png".into()),
            }),
        );
        a.update_compose_gate(Some("supporters".into()), "the public teaser".into());

        let bytes = a.drafts_snapshot_bytes();

        let b = mgr(Arc::new(MockNest::default()));
        b.restore_drafts(bytes);

        let compose = b.snapshot().compose;
        assert_eq!(compose.text, "half a thought");
        assert_eq!(compose.tags, "walrus, quarterly");
        assert_eq!(
            compose.attached_file.as_ref().unwrap().blob_hash.as_deref(),
            Some("abc123"),
        );
        assert_eq!(compose.gate_tier.as_deref(), Some("supporters"));
        assert_eq!(compose.gate_preview, "the public teaser");
    }

    /// A restore has to reach the shell, or the composer keeps rendering the
    /// pre-restore snapshot until the user's next keystroke — the same
    /// catch-up-notify property the conversations rail's restore carries.
    #[test]
    fn a_restore_notifies_observers() {
        let a = mgr(Arc::new(MockNest::default()));
        a.update_compose("something".into(), String::new(), None);
        let bytes = a.drafts_snapshot_bytes();

        let b = mgr(Arc::new(MockNest::default()));
        let spy = Arc::new(CountingObserver::default());
        b.add_observer(Arc::clone(&spy) as Arc<dyn FeedSnapshotObserver>);
        b.restore_drafts(bytes);

        assert_eq!(spy.count(), 1, "the restored draft must reach the shell");
    }

    /// An unreadable blob must cost the user nothing but their draft. The
    /// composer stays usable and empty — never a crash, never a wedged surface.
    #[test]
    fn an_unreadable_blob_leaves_a_usable_empty_composer() {
        let m = mgr(Arc::new(MockNest::default()));
        let spy = Arc::new(CountingObserver::default());
        m.add_observer(Arc::clone(&spy) as Arc<dyn FeedSnapshotObserver>);

        m.restore_drafts(vec![0xFF, 0xFF, 0xFF]);

        assert_eq!(m.snapshot().compose.text, "");
        assert_eq!(spy.count(), 0, "nothing changed, so nothing to notify");
        // Still fully usable afterwards.
        m.update_compose("typed anyway".into(), String::new(), None);
        assert_eq!(m.snapshot().compose.text, "typed anyway");
    }

    /// First run (`load` → `Ok(None)`) and an all-empty blob are the same
    /// state, and neither may notify: the autosave observer ticks on notify, so
    /// an empty restore that fired one would push a pointless upload on every
    /// launch.
    #[test]
    fn an_empty_blob_is_a_no_op() {
        let m = mgr(Arc::new(MockNest::default()));
        let empty = m.drafts_snapshot_bytes();
        let spy = Arc::new(CountingObserver::default());
        m.add_observer(Arc::clone(&spy) as Arc<dyn FeedSnapshotObserver>);

        m.restore_drafts(empty);

        assert_eq!(spy.count(), 0);
    }

    /// Sell mode is a compose answer like any other and rides the same rail —
    /// including its non-default rank knob, which a restore that rebuilt a
    /// `SellComposeState::default()` would silently flip back.
    #[test]
    fn sell_mode_survives_the_rail() {
        let a = mgr(Arc::new(MockNest::default()));
        a.update_compose("for sale".into(), String::new(), None);
        a.update_compose_sell(
            Some(SellComposeState {
                price: "5 EUR".into(),
                asking_price: String::new(),
                subscribers_get_it_free: false,
            }),
            "the teaser".into(),
        );

        let b = mgr(Arc::new(MockNest::default()));
        b.restore_drafts(a.drafts_snapshot_bytes());

        let sell = b.snapshot().compose.sell.expect("sell mode restores");
        assert_eq!(sell.price, "5 EUR");
        assert!(!sell.subscribers_get_it_free);
    }

    /// The dedup baseline `DraftsSync::save_if_changed` compares is these
    /// bytes, so equal composer state MUST produce equal bytes — otherwise
    /// every launch would re-upload an unchanged draft to the user's fleet.
    #[test]
    fn equal_composer_state_produces_equal_bytes_across_managers() {
        let a = mgr(Arc::new(MockNest::default()));
        a.update_compose("same".into(), "tag".into(), None);
        let b = mgr(Arc::new(MockNest::default()));
        b.restore_drafts(a.drafts_snapshot_bytes());

        assert_eq!(a.drafts_snapshot_bytes(), b.drafts_snapshot_bytes());
    }

    #[derive(Default)]
    struct CountingObserver(std::sync::atomic::AtomicUsize);

    impl CountingObserver {
        fn count(&self) -> usize {
            self.0.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl FeedSnapshotObserver for CountingObserver {
        fn on_changed(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// `feed_posts_json` must emit every field an e2e state-backed reader
    /// keys off (`_feed_posts_from_state` / the id-keyed helpers in
    /// `actions/feed.py`), with `is_muted` threaded from the caller-supplied
    /// predicate rather than read off the (mute-unaware) `PostSummary`, and
    /// absent optional ids as JSON `null` (linux's/windows' shape), not `""`.
    #[test]
    fn feed_posts_json_emits_the_full_state_contract() {
        let posts = vec![
            PostSummary {
                post_id: "p1".into(),
                author: "a1".into(),
                body: "hello".into(),
                timestamp: 1000,
                tags: vec!["tag1".into()],
                has_media: true,
                media_hash: Some("deadbeef".into()),
                is_reply: false,
                like_count: 3,
                reply_count: 1,
                repost_count: 2,
                quote_count: 0,
                viewer_liked: true,
                reposted_post_id: Some("orig".into()),
                viewer_repost_id: None,
                document: fauna_core::render::RenderDocument {
                    blocks: vec![
                        fauna_core::render::RenderBlock::LinkPreview {
                            url: "https://a.example/".into(),
                            state: fauna_core::render::PreviewState::Failed,
                        },
                        fauna_core::render::RenderBlock::LinkPreview {
                            url: "https://b.example/".into(),
                            state: fauna_core::render::PreviewState::Resolving,
                        },
                    ],
                },
                ..Default::default()
            },
            PostSummary {
                post_id: "p2".into(),
                author: "a2".into(),
                body: "muted post".into(),
                ..Default::default()
            },
        ];

        let json = feed_posts_json(&posts, |post_id| post_id == "p2");

        assert_eq!(
            json,
            serde_json::json!([
                {
                    "post_id": "p1",
                    "author": "a1",
                    "body": "hello",
                    "timestamp": 1000,
                    "tags": ["tag1"],
                    "has_media": true,
                    "media_hash": "deadbeef",
                    "is_reply": false,
                    "is_muted": false,
                    "like_count": 3,
                    "reply_count": 1,
                    "repost_count": 2,
                    "quote_count": 0,
                    "viewer_liked": true,
                    "reposted_post_id": "orig",
                    "viewer_repost_id": null,
                    "link_previews": [
                        {"url": "https://a.example/", "state": "failed"},
                        {"url": "https://b.example/", "state": "resolving"},
                    ],
                },
                {
                    "post_id": "p2",
                    "author": "a2",
                    "body": "muted post",
                    "timestamp": 0,
                    "tags": [],
                    "has_media": false,
                    "media_hash": "",
                    "is_reply": false,
                    "is_muted": true,
                    "like_count": 0,
                    "reply_count": 0,
                    "repost_count": 0,
                    "quote_count": 0,
                    "viewer_liked": false,
                    "reposted_post_id": null,
                    "viewer_repost_id": null,
                    "link_previews": [],
                },
            ])
        );
    }
}
